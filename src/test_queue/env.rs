//! Isolated child-environment building, cache-key derivation, ledger writing,
//! and job-view formatting for `test_queue`.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};

use super::{
    cache_path, job_path, list_jobs, read_job, write_json, CacheEntry, Job, PASS_KEYS, PORT_BASE,
    PORT_SPAN,
};

pub struct IsoSpec<'a> {
    pub daemon_state: &'a Path,
    pub isolation: &'a Path,
    pub jobs: usize,
    pub port: u16,
    pub pass: &'a BTreeMap<String, String>,
    /// Allowlisted env that was hashed into the cache key. Copied into
    /// the child as-is. Isolation paths are applied around it and do
    /// not replace these keys, except `CADENCE_STATE_DIR`, which is
    /// always the job directory.
    pub bound: &'a BTreeMap<String, String>,
    pub rustflags: &'a str,
}

/// The environment a job process actually receives. Built from a
/// pass-list plus isolation paths — never a copy of the daemon env.
pub fn child_env(spec: &IsoSpec<'_>) -> Result<BTreeMap<String, String>> {
    let state = spec.isolation.join("state");
    let daemon =
        fs::canonicalize(spec.daemon_state).unwrap_or_else(|_| spec.daemon_state.to_path_buf());
    // The isolation tree lives under the daemon state as bookkeeping.
    // The child's CADENCE_STATE_DIR is its own empty directory, never
    // the daemon root and never the production state dir.
    if state == daemon {
        return Err(Error::rejected(
            "test job isolation would use the daemon state dir",
        ));
    }
    if let Ok(production) = crate::client::default_state_dir() {
        if state == production {
            return Err(Error::rejected(
                "test job isolation would use the production state dir",
            ));
        }
    }
    let mut env = BTreeMap::new();
    for key in PASS_KEYS {
        if let Some(value) = spec.pass.get(*key) {
            if !value.is_empty() {
                env.insert((*key).to_string(), value.clone());
            }
        }
    }
    // The hashed contract. Copied before isolation overrides so a
    // caller-supplied CADENCE_SUITE_LOCK survives; CADENCE_STATE_DIR
    // is forced afterwards because it is not part of that contract.
    for (key, value) in spec.bound {
        env.insert(key.clone(), value.clone());
    }
    env.insert("RUSTFLAGS".into(), spec.rustflags.to_string());
    env.insert(
        "HOME".into(),
        spec.isolation.join("home").display().to_string(),
    );
    env.insert(
        "TMPDIR".into(),
        spec.isolation.join("tmp").display().to_string(),
    );
    env.insert(
        "CARGO_TARGET_DIR".into(),
        spec.isolation.join("target").display().to_string(),
    );
    env.insert("CARGO_BUILD_JOBS".into(), spec.jobs.to_string());
    env.insert("CADENCE_STATE_DIR".into(), state.display().to_string());
    env.insert("CADENCE_PORT".into(), spec.port.to_string());
    env.insert("CADENCE_TEST_PORT".into(), spec.port.to_string());
    if !spec.bound.contains_key("CADENCE_SUITE_LOCK") {
        env.insert(
            "CADENCE_SUITE_LOCK".into(),
            spec.isolation.join("suite.lock").display().to_string(),
        );
    }
    env.insert(
        "CADENCE_TEST_ISOLATION".into(),
        spec.isolation.display().to_string(),
    );
    for (key, value) in spec.bound {
        if key == "CADENCE_STATE_DIR" {
            continue;
        }
        if env.get(key).map(String::as_str) != Some(value.as_str()) {
            return Err(Error::rejected(format!(
                "test job env dropped hashed key {key}"
            )));
        }
    }
    if env.get("RUSTFLAGS").map(String::as_str) != Some(spec.rustflags) {
        return Err(Error::rejected(
            "test job env dropped the RUSTFLAGS the cache key recorded",
        ));
    }
    let recorded = env.get("CADENCE_STATE_DIR").map(String::as_str);
    if recorded == Some(daemon.to_str().unwrap_or_default()) {
        return Err(Error::rejected(
            "refusing to start a test job pointed at the daemon state dir",
        ));
    }
    Ok(env)
}

pub(super) fn pass_env() -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for key in PASS_KEYS {
        if let Ok(value) = std::env::var(key) {
            out.insert((*key).to_string(), value);
        }
    }
    out
}

pub(super) fn allocate_port(taken: &[u16]) -> Result<u16> {
    for offset in 0..PORT_SPAN {
        let port = PORT_BASE + offset;
        if !taken.contains(&port) {
            return Ok(port);
        }
    }
    Err(Error::rejected("no free port in 3110-3189 for a test job"))
}

pub(super) fn finish_job(state_dir: &Path, job: &mut Job, code: Option<i32>, outcome: &str) {
    let now = now_secs();
    job.state = outcome.into();
    job.outcome = Some(outcome.into());
    job.exit_code = code;
    job.finished_at = Some(now);
    if let Some(started) = job.started_at {
        job.execute_ms = Some(millis(now - started));
    }
    if outcome == "passed" {
        let _ = write_json(
            &cache_path(state_dir, &job.key),
            &json!({"job_id": job.id, "finished_at": now}),
        );
    }
    let _ = append_ledger(state_dir, job);
}

pub(super) fn append_ledger(state_dir: &Path, job: &Job) -> Result<()> {
    // `review::record_flake` is the quarantine (a test that failed in
    // the full run and passed alone, three distinct heads). Writing
    // ordinary results there would count them as flake sightings, so
    // the result shape lives beside it.
    let path = state_dir.join("reviews/test-runs.jsonl");
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
    let load1 = fs::read_to_string("/proc/loadavg").ok().and_then(|s| {
        s.split_whitespace()
            .next()
            .and_then(|v| v.parse::<f64>().ok())
    });
    let nproc = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(0);
    let line = json!({
        "test": if job.filter.is_empty() { if job.full { "full" } else { "default" } } else { job.filter.as_str() },
        "tree": job.tree,
        "outcome": job.outcome,
        "duration_ms": job.execute_ms,
        "load": {"load1": load1, "nproc": nproc},
        "job": job.id,
        "commit": job.commit,
    });
    writeln!(file, "{line}")?;
    Ok(())
}

pub(super) fn cache_hit(state_dir: &Path, key: &str, now: f64) -> Result<Option<Value>> {
    let path = cache_path(state_dir, key);
    if !path.exists() {
        return Ok(None);
    }
    let entry: CacheEntry = serde_json::from_slice(&fs::read(&path)?)
        .map_err(|e| Error::internal(format!("test cache entry: {e}")))?;
    let job = match read_job(&job_path(state_dir, &entry.job_id)) {
        Ok(job) if job.outcome.as_deref() == Some("passed") => job,
        _ => return Ok(None),
    };
    let age = (now - entry.finished_at).max(0.0) as u64;
    let mut view = job_view(&job, now);
    view["cache"] = json!("hit");
    view["joined"] = json!(false);
    view["age_secs"] = json!(age);
    view["source_job"] = json!(job.id);
    Ok(Some(view))
}

pub(super) fn inflight_with_key(state_dir: &Path, key: &str) -> Result<Option<Job>> {
    for job in list_jobs(state_dir)? {
        if job.key == key && matches!(job.state.as_str(), "queued" | "running") {
            return Ok(Some(job));
        }
    }
    Ok(None)
}

pub(super) struct KeyParts<'a> {
    pub(super) repo: &'a str,
    pub(super) tree: &'a str,
    pub(super) filter: &'a str,
    pub(super) full: bool,
    pub(super) features: &'a str,
    pub(super) env: &'a BTreeMap<String, String>,
    pub(super) rustflags: &'a str,
    pub(super) jobs: usize,
    pub(super) worktree: &'a Path,
    pub(super) argv: &'a [String],
    pub(super) runner: &'a str,
}

impl KeyParts<'_> {
    pub(super) fn record(&self) -> Result<Value> {
        let toolchain = toolchain_identity()?;
        Ok(json!({
            "argv": self.argv,
            "cargo_build_jobs": self.jobs.to_string(),
            "env": self.env,
            "features": self.features,
            "filter": self.filter,
            "full": self.full,
            "lock_sha256": file_sha256(&self.worktree.join("Cargo.lock")),
            "manifest_sha256": file_sha256(&self.worktree.join("Cargo.toml")),
            "profile": "dev",
            "repo": self.repo,
            "runner": self.runner,
            "rustflags": self.rustflags,
            "toolchain": toolchain,
            "tree": self.tree,
        }))
    }
}

pub(super) fn cache_key(record: &Value) -> String {
    // serde_json maps are BTreeMaps, so the bytes are stable.
    sha256_hex(record.to_string().as_bytes())
}

fn toolchain_identity() -> Result<Value> {
    let rustc = tool_text("rustc")?;
    let cargo = tool_text("cargo")?;
    let triple = rustc
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .unwrap_or("unknown")
        .to_string();
    Ok(json!({"rustc": rustc, "cargo": cargo, "triple": triple}))
}

fn tool_text(bin: &str) -> Result<String> {
    let mut cmd = Command::new(bin);
    cmd.arg("-vV");
    let out = crate::reaper::output(&mut cmd)?;
    if !out.status.success() {
        return Err(Error::rejected(format!(
            "{bin} -vV failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

pub(super) fn test_threads(env: &BTreeMap<String, String>) -> Result<String> {
    match env.get("CADENCE_TEST_THREADS") {
        None => Ok("2".to_string()),
        Some(value)
            if !value.is_empty()
                && value.len() <= 3
                && value.chars().all(|c| c.is_ascii_digit()) =>
        {
            Ok(value.clone())
        }
        Some(value) => Err(Error::rejected(format!(
            "CADENCE_TEST_THREADS must be an integer — got '{value}'"
        ))),
    }
}

pub(super) fn cargo_argv(filter: &str, full: bool, features: &str, threads: &str) -> Vec<String> {
    let mut args = vec!["test".to_string()];
    if full {
        args.push("--all-targets".into());
    }
    if !features.is_empty() {
        args.push("--features".into());
        args.push(features.to_string());
    }
    if !filter.is_empty() {
        args.push(filter.to_string());
    }
    args.push("--".into());
    args.push("--test-threads".into());
    args.push(threads.to_string());
    args
}

pub(super) fn job_view(job: &Job, now: f64) -> Value {
    let age = job.finished_at.map(|t| (now - t).max(0.0) as u64);
    json!({
        "id": job.id,
        "seq": job.seq,
        "state": job.state,
        "key": job.key,
        "worktree": job.worktree,
        "filter": job.filter,
        "full": job.full,
        "features": job.features,
        "priority": job.priority,
        "env": job.env,
        "rustflags": job.rustflags,
        "tree": job.tree,
        "commit": job.commit,
        "repo": job.repo,
        "by": job.by,
        "runner": job.runner,
        "argv": job.argv,
        "created_at": job.created_at,
        "started_at": job.started_at,
        "finished_at": job.finished_at,
        "exit_code": job.exit_code,
        "outcome": job.outcome,
        "port": job.port,
        "cargo_build_jobs": job.cargo_build_jobs,
        "isolation_dir": job.isolation_dir,
        "log_path": job.log_path,
        "queued_ms": job.queued_ms,
        "compile_ms": Value::Null,
        "execute_ms": job.execute_ms,
        "postprocess_ms": Value::Null,
        "age_secs": age,
        "cache": "miss",
        "joined": false,
    })
}

pub(super) fn git(cwd: &Path, args: &[&str]) -> Result<String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(cwd).args(args);
    let out = crate::reaper::output(&mut cmd)?;
    if !out.status.success() {
        return Err(Error::rejected(format!(
            "git {} failed in {}: {}",
            args.join(" "),
            cwd.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn file_sha256(path: &Path) -> String {
    match fs::read(path) {
        Ok(bytes) => sha256_hex(&bytes),
        Err(_) => String::new(),
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub(super) fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

pub(super) fn millis(secs: f64) -> u64 {
    if secs.is_sign_negative() {
        0
    } else {
        secs.mul_add(1000.0, 0.0) as u64
    }
}
