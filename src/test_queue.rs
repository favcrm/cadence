//! CAD-129: a daemon-side test queue. Agents submit a worktree and get a
//! job id back; the daemon runs `cargo test` itself (not an LLM), one
//! FIFO at a time up to the CAD-113 build-slot cap, and reuses a
//! successful result when the cache key matches.
//!
//! The key is a canonical record — git tree (not branch, not commit),
//! filter, toolchain, feature set, Cargo.lock / manifest hashes, the
//! cargo argv, and an allowlisted `CADENCE_*` env. A failed, interrupted
//! or cancelled run is never a hit. `--no-cache` skips the hit and still
//! joins an in-flight run of the same key, so a rerun is never hidden by
//! a stale success and two concurrent submits still share one execution.
//!
//! Each run gets its own state dir, temp, target dir and port. The child
//! environment is built from a pass-list, never copied from the daemon
//! process, so the job cannot see the daemon's `CADENCE_STATE_DIR`.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};

/// How many jobs may run at once, and the `CARGO_BUILD_JOBS` each one
/// receives. Taken from the CAD-113 slot config at daemon start.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Limits {
    pub build_slots: usize,
    pub jobs_per_lane: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            build_slots: 3,
            jobs_per_lane: 4,
        }
    }
}

impl Limits {
    pub fn from_slot_config(config: &crate::slots::SlotConfig) -> Self {
        Self {
            build_slots: config.build_slots.max(1),
            jobs_per_lane: config.jobs_per_lane.max(1),
        }
    }
}

/// What the daemon executes. `Shell` exists so tests can hold a job
/// open without invoking cargo; a production daemon uses it only when
/// the CAD-482 seam is armed and `<state>/test-queue/runner.sh` is
/// present.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Runner {
    Cargo,
    Shell(String),
}

impl Runner {
    fn label(&self) -> &'static str {
        match self {
            Runner::Cargo => "cargo",
            Runner::Shell(_) => "shell",
        }
    }
}

/// A submit from the CLI. `env` is filtered through [`allowlisted_env`]
/// again inside the daemon — a caller cannot put `CADENCE_ALIAS` or
/// `CADENCE_STATE_DIR` into the key.
#[derive(Clone, Debug)]
pub struct Submit {
    pub worktree: PathBuf,
    pub filter: String,
    pub full: bool,
    pub no_cache: bool,
    pub env: BTreeMap<String, String>,
    pub rustflags: String,
    pub by: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Job {
    id: String,
    seq: u64,
    state: String,
    key: String,
    worktree: String,
    filter: String,
    full: bool,
    tree: String,
    commit: String,
    repo: String,
    by: String,
    no_cache: bool,
    runner: String,
    argv: Vec<String>,
    created_at: f64,
    #[serde(default)]
    started_at: Option<f64>,
    #[serde(default)]
    finished_at: Option<f64>,
    #[serde(default)]
    exit_code: Option<i32>,
    #[serde(default)]
    outcome: Option<String>,
    #[serde(default)]
    pid: Option<u32>,
    #[serde(default)]
    port: Option<u16>,
    cargo_build_jobs: usize,
    isolation_dir: String,
    log_path: String,
    #[serde(default)]
    queued_ms: Option<u64>,
    #[serde(default)]
    execute_ms: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Meta {
    next_seq: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct CacheEntry {
    job_id: String,
    finished_at: f64,
}

/// Env keys that change what a test run means. Identity and transport
/// (`CADENCE_ALIAS`, `CADENCE_STATE_DIR`, seam vars) are excluded so two
/// agents submitting the same tree share a hit.
const ENV_ALLOW: &[&str] = &[
    "CADENCE_SUITE_LOCK",
    "CADENCE_TEST_THREADS",
    "CADENCE_PM_DIR",
    "CADENCE_HOME",
    "CADENCE_REVIEW_CONFIG",
];

/// Process env copied into a job for the toolchain only. Everything
/// else — including any `CADENCE_*` — is dropped.
const PASS_KEYS: &[&str] = &["PATH", "RUSTUP_HOME", "CARGO_HOME", "RUSTC_WRAPPER"];

const PORT_BASE: u16 = 3110;
const PORT_SPAN: u16 = 80;

pub fn allowlisted_env<I, K, V>(vars: I) -> BTreeMap<String, String>
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<str>,
    V: AsRef<str>,
{
    let mut out = BTreeMap::new();
    for (k, v) in vars {
        let k = k.as_ref();
        if ENV_ALLOW.contains(&k) {
            out.insert(k.to_string(), v.as_ref().to_string());
        }
    }
    out
}

/// The runner a daemon tick should use. Production is always cargo.
pub fn runner_for(state_dir: &Path) -> Runner {
    if crate::test_seam::armed(state_dir) {
        let path = state_dir.join("test-queue/runner.sh");
        if let Ok(script) = fs::read_to_string(&path) {
            if !script.is_empty() {
                return Runner::Shell(script);
            }
        }
    }
    Runner::Cargo
}

pub fn publish_limits(state_dir: &Path, limits: &Limits) -> Result<()> {
    let _lock = lock(state_dir)?;
    write_json(&limits_path(state_dir), &serde_json::to_value(limits)?)?;
    Ok(())
}

pub fn submit(state_dir: &Path, req: &Submit) -> Result<Value> {
    let limits = load_limits(state_dir);
    submit_with(state_dir, req, &limits, &runner_for(state_dir))
}

pub fn submit_with(
    state_dir: &Path,
    req: &Submit,
    limits: &Limits,
    runner: &Runner,
) -> Result<Value> {
    let _lock = lock(state_dir)?;
    let worktree = fs::canonicalize(&req.worktree).map_err(|_| {
        Error::rejected(format!(
            "worktree {} is not a directory",
            req.worktree.display()
        ))
    })?;
    let state_canon = fs::canonicalize(state_dir).unwrap_or_else(|_| state_dir.to_path_buf());
    if worktree.starts_with(&state_canon) {
        return Err(Error::rejected(
            "worktree must not sit inside the daemon state dir — a test job \
             cannot read or write the daemon's own state",
        ));
    }
    let tree = git(&worktree, &["rev-parse", "HEAD^{tree}"])?;
    let commit = git(&worktree, &["rev-parse", "HEAD"])?;
    let repo = git(&worktree, &["remote", "get-url", "origin"])
        .unwrap_or_else(|_| format!("path:{}", worktree.display()));
    let env = allowlisted_env(req.env.iter().map(|(k, v)| (k.as_str(), v.as_str())));
    let argv = cargo_argv(&req.filter, req.full);
    let record = KeyParts {
        repo: &repo,
        tree: &tree,
        filter: &req.filter,
        full: req.full,
        env: &env,
        rustflags: &req.rustflags,
        jobs: limits.jobs_per_lane,
        worktree: &worktree,
        argv: &argv,
        runner: runner.label(),
    }
    .record()?;
    let key = cache_key(&record);
    let now = now_secs();
    if !req.no_cache {
        if let Some(hit) = cache_hit(state_dir, &key, now)? {
            return Ok(hit);
        }
    }
    if let Some(existing) = inflight_with_key(state_dir, &key)? {
        let mut view = job_view(&existing, now);
        view["joined"] = json!(true);
        view["cache"] = json!("miss");
        return Ok(view);
    }
    let seq = next_seq(state_dir)?;
    let id = format!("tq-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
    let isolation = state_dir.join("test-queue/run").join(&id);
    let log_path = isolation.join("log");
    let job = Job {
        id: id.clone(),
        seq,
        state: "queued".into(),
        key,
        worktree: worktree.display().to_string(),
        filter: req.filter.clone(),
        full: req.full,
        tree,
        commit,
        repo,
        by: req.by.clone(),
        no_cache: req.no_cache,
        runner: runner.label().into(),
        argv,
        created_at: now,
        started_at: None,
        finished_at: None,
        exit_code: None,
        outcome: None,
        pid: None,
        port: None,
        cargo_build_jobs: limits.jobs_per_lane,
        isolation_dir: isolation.display().to_string(),
        log_path: log_path.display().to_string(),
        queued_ms: None,
        execute_ms: None,
    };
    write_job(state_dir, &job)?;
    let mut view = job_view(&job, now);
    view["joined"] = json!(false);
    view["cache"] = json!("miss");
    Ok(view)
}

pub fn status(state_dir: &Path, id: &str) -> Result<Value> {
    let _lock = lock(state_dir)?;
    let job = read_job(&job_path(state_dir, id))?;
    Ok(job_view(&job, now_secs()))
}

pub fn log(state_dir: &Path, id: &str) -> Result<Value> {
    let _lock = lock(state_dir)?;
    let job = read_job(&job_path(state_dir, id))?;
    let bytes = fs::read(&job.log_path).unwrap_or_default();
    const CAP: usize = 256 * 1024;
    let truncated = bytes.len() > CAP;
    let slice = if truncated {
        &bytes[bytes.len() - CAP..]
    } else {
        &bytes[..]
    };
    Ok(json!({
        "id": job.id,
        "state": job.state,
        "truncated": truncated,
        "log": String::from_utf8_lossy(slice),
    }))
}

pub fn queue_view(state_dir: &Path) -> Result<Value> {
    let _lock = lock(state_dir)?;
    let limits = load_limits_locked(state_dir);
    let jobs = list_jobs(state_dir)?;
    let mut running: Vec<&Job> = jobs.iter().filter(|j| j.state == "running").collect();
    running.sort_by_key(|j| j.seq);
    let queued = jobs.iter().filter(|j| j.state == "queued").count();
    Ok(json!({
        "capacity": limits.build_slots,
        "jobs_per_lane": limits.jobs_per_lane,
        "running": running.iter().map(|j| json!({"id": j.id, "full": j.full})).collect::<Vec<_>>(),
        "queued": queued,
        "holder": running.first().map(|j| j.id.as_str()),
    }))
}

/// Owns the live children. One daemon thread ticks it; tests tick it
/// directly. Dropping a child without waiting would leak a zombie the
/// subreaper will not collect (the child is owned), so [`Worker::shutdown`]
/// kills and waits.
pub struct Worker {
    state_dir: PathBuf,
    limits: Limits,
    runner: Runner,
    children: BTreeMap<String, Child>,
}

impl Worker {
    pub fn new(state_dir: &Path, limits: Limits, runner: Runner) -> Result<Self> {
        publish_limits(state_dir, &limits)?;
        Ok(Self {
            state_dir: state_dir.to_path_buf(),
            limits,
            runner,
            children: BTreeMap::new(),
        })
    }

    pub fn tick(&mut self) -> Result<()> {
        self.reap()?;
        self.launch()?;
        Ok(())
    }

    pub fn shutdown(&mut self) {
        let ids: Vec<String> = self.children.keys().cloned().collect();
        for id in ids {
            if let Some(mut child) = self.children.remove(&id) {
                let _ = child.kill();
                let _ = child.wait();
            }
            if let Ok(guard) = lock(&self.state_dir) {
                if let Ok(mut job) = read_job(&job_path(&self.state_dir, &id)) {
                    if job.state == "running" {
                        finish_job(&self.state_dir, &mut job, None, "interrupted");
                        let _ = write_job(&self.state_dir, &job);
                    }
                }
                drop(guard);
            }
        }
    }

    fn reap(&mut self) -> Result<()> {
        let _lock = lock(&self.state_dir)?;
        let mut done = Vec::new();
        for (id, child) in &mut self.children {
            match child.try_wait() {
                Ok(Some(status)) => done.push((id.clone(), status.code())),
                Ok(None) => {}
                Err(_) => done.push((id.clone(), None)),
            }
        }
        for (id, code) in &done {
            self.children.remove(id);
            if let Ok(mut job) = read_job(&job_path(&self.state_dir, id)) {
                if job.state == "running" {
                    let outcome = match code {
                        Some(0) => "passed",
                        Some(_) => "failed",
                        None => "interrupted",
                    };
                    finish_job(&self.state_dir, &mut job, *code, outcome);
                    write_job(&self.state_dir, &job)?;
                }
            }
        }
        // A restart has no Child handle. A pid that is gone did not
        // finish in this process, so the result is inconclusive.
        for job in list_jobs(&self.state_dir)? {
            if job.state != "running" || self.children.contains_key(&job.id) {
                continue;
            }
            let alive = job
                .pid
                .is_some_and(|pid| Path::new(&format!("/proc/{pid}")).exists());
            if !alive {
                let mut job = job;
                finish_job(&self.state_dir, &mut job, None, "interrupted");
                write_job(&self.state_dir, &job)?;
            }
        }
        Ok(())
    }

    fn launch(&mut self) -> Result<()> {
        let _lock = lock(&self.state_dir)?;
        let mut jobs = list_jobs(&self.state_dir)?;
        jobs.sort_by_key(|j| j.seq);
        let running: Vec<Job> = jobs
            .iter()
            .filter(|j| j.state == "running")
            .cloned()
            .collect();
        let queued: Vec<Job> = jobs
            .iter()
            .filter(|j| j.state == "queued")
            .cloned()
            .collect();
        let starting = select_next(&running, &queued, self.limits.build_slots);
        let pass = pass_env();
        let mut used_ports: Vec<u16> = running.iter().filter_map(|j| j.port).collect();
        for mut job in starting {
            let isolation = PathBuf::from(&job.isolation_dir);
            for sub in ["state", "tmp", "home", "target"] {
                fs::create_dir_all(isolation.join(sub))?;
            }
            let port = allocate_port(&used_ports)?;
            used_ports.push(port);
            let env = child_env(&IsoSpec {
                daemon_state: &self.state_dir,
                isolation: &isolation,
                jobs: job.cargo_build_jobs,
                port,
                pass: &pass,
            })?;
            let log_file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&job.log_path)?;
            let log_err = log_file.try_clone()?;
            let mut cmd = match &self.runner {
                Runner::Cargo => {
                    let mut c = Command::new("cargo");
                    c.args(&job.argv);
                    c
                }
                Runner::Shell(script) => {
                    let mut c = Command::new("sh");
                    c.arg("-c").arg(script);
                    c
                }
            };
            cmd.current_dir(&job.worktree)
                .env_clear()
                .envs(&env)
                .stdin(Stdio::null())
                .stdout(Stdio::from(log_file))
                .stderr(Stdio::from(log_err));
            let now = now_secs();
            match crate::reaper::spawn(&mut cmd) {
                Ok(child) => {
                    job.pid = Some(child.id());
                    job.port = Some(port);
                    job.state = "running".into();
                    job.started_at = Some(now);
                    job.queued_ms = Some(millis(now - job.created_at));
                    write_job(&self.state_dir, &job)?;
                    self.children.insert(job.id.clone(), child);
                }
                Err(e) => {
                    job.state = "failed".into();
                    job.outcome = Some("failed".into());
                    job.started_at = Some(now);
                    job.finished_at = Some(now);
                    job.exit_code = Some(127);
                    job.queued_ms = Some(millis(now - job.created_at));
                    job.execute_ms = Some(0);
                    write_job(&self.state_dir, &job)?;
                    let _ = fs::write(&job.log_path, format!("failed to spawn test runner: {e}\n"));
                    append_ledger(&self.state_dir, &job)?;
                }
            }
        }
        Ok(())
    }
}

/// FIFO: never skip a job that cannot start yet. A `--full` run holds
/// the queue alone (the suite slot is exclusive). Review-vs-dev priority
/// is deferred.
fn select_next(running: &[Job], queued: &[Job], capacity: usize) -> Vec<Job> {
    let mut start = Vec::new();
    if running.iter().any(|j| j.full) {
        return start;
    }
    for job in queued {
        if job.full {
            if running.is_empty() && start.is_empty() {
                start.push(job.clone());
            }
            break;
        }
        if running.len() + start.len() >= capacity.max(1) {
            break;
        }
        start.push(job.clone());
    }
    start
}

pub struct IsoSpec<'a> {
    pub daemon_state: &'a Path,
    pub isolation: &'a Path,
    pub jobs: usize,
    pub port: u16,
    pub pass: &'a BTreeMap<String, String>,
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
    env.insert(
        "CADENCE_SUITE_LOCK".into(),
        spec.isolation.join("suite.lock").display().to_string(),
    );
    env.insert(
        "CADENCE_TEST_ISOLATION".into(),
        spec.isolation.display().to_string(),
    );
    let recorded = env.get("CADENCE_STATE_DIR").map(String::as_str);
    if recorded == Some(daemon.to_str().unwrap_or_default()) {
        return Err(Error::rejected(
            "refusing to start a test job pointed at the daemon state dir",
        ));
    }
    Ok(env)
}

fn pass_env() -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for key in PASS_KEYS {
        if let Ok(value) = std::env::var(key) {
            out.insert((*key).to_string(), value);
        }
    }
    out
}

fn allocate_port(taken: &[u16]) -> Result<u16> {
    for offset in 0..PORT_SPAN {
        let port = PORT_BASE + offset;
        if !taken.contains(&port) {
            return Ok(port);
        }
    }
    Err(Error::rejected("no free port in 3110-3189 for a test job"))
}

fn finish_job(state_dir: &Path, job: &mut Job, code: Option<i32>, outcome: &str) {
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

fn append_ledger(state_dir: &Path, job: &Job) -> Result<()> {
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

fn cache_hit(state_dir: &Path, key: &str, now: f64) -> Result<Option<Value>> {
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

fn inflight_with_key(state_dir: &Path, key: &str) -> Result<Option<Job>> {
    for job in list_jobs(state_dir)? {
        if job.key == key && matches!(job.state.as_str(), "queued" | "running") {
            return Ok(Some(job));
        }
    }
    Ok(None)
}

struct KeyParts<'a> {
    repo: &'a str,
    tree: &'a str,
    filter: &'a str,
    full: bool,
    env: &'a BTreeMap<String, String>,
    rustflags: &'a str,
    jobs: usize,
    worktree: &'a Path,
    argv: &'a [String],
    runner: &'a str,
}

impl KeyParts<'_> {
    fn record(&self) -> Result<Value> {
        let toolchain = toolchain_identity()?;
        Ok(json!({
            "argv": self.argv,
            "cargo_build_jobs": self.jobs.to_string(),
            "env": self.env,
            "features": "",
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

fn cache_key(record: &Value) -> String {
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

fn cargo_argv(filter: &str, full: bool) -> Vec<String> {
    let mut args = vec!["test".to_string()];
    if full {
        args.push("--all-targets".into());
    }
    if !filter.is_empty() {
        args.push(filter.to_string());
    }
    args.push("--".into());
    args.push("--test-threads".into());
    args.push("2".into());
    args
}

fn job_view(job: &Job, now: f64) -> Value {
    let age = job.finished_at.map(|t| (now - t).max(0.0) as u64);
    json!({
        "id": job.id,
        "seq": job.seq,
        "state": job.state,
        "key": job.key,
        "worktree": job.worktree,
        "filter": job.filter,
        "full": job.full,
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

fn git(cwd: &Path, args: &[&str]) -> Result<String> {
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

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn millis(secs: f64) -> u64 {
    if secs.is_sign_negative() {
        0
    } else {
        secs.mul_add(1000.0, 0.0) as u64
    }
}

struct QueueLock {
    file: File,
}

impl Drop for QueueLock {
    fn drop(&mut self) {
        unsafe {
            libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

fn lock(state_dir: &Path) -> Result<QueueLock> {
    let dir = state_dir.join("test-queue");
    fs::create_dir_all(&dir)?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(dir.join("lock"))?;
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(QueueLock { file })
}

fn limits_path(state_dir: &Path) -> PathBuf {
    state_dir.join("test-queue/limits.json")
}

fn load_limits(state_dir: &Path) -> Limits {
    let _lock = lock(state_dir);
    load_limits_locked(state_dir)
}

fn load_limits_locked(state_dir: &Path) -> Limits {
    fs::read(limits_path(state_dir))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn next_seq(state_dir: &Path) -> Result<u64> {
    let path = state_dir.join("test-queue/meta.json");
    let mut meta: Meta = fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(Meta { next_seq: 1 });
    let seq = meta.next_seq;
    meta.next_seq = meta.next_seq.saturating_add(1);
    write_json(&path, &serde_json::to_value(&meta)?)?;
    Ok(seq)
}

fn job_path(state_dir: &Path, id: &str) -> PathBuf {
    state_dir.join("test-queue/jobs").join(format!("{id}.json"))
}

fn cache_path(state_dir: &Path, key: &str) -> PathBuf {
    state_dir
        .join("test-queue/cache")
        .join(format!("{key}.json"))
}

fn write_job(state_dir: &Path, job: &Job) -> Result<()> {
    write_json(&job_path(state_dir, &job.id), &serde_json::to_value(job)?)
}

fn read_job(path: &Path) -> Result<Job> {
    if !path.exists() {
        let id = path.file_stem().and_then(|s| s.to_str()).unwrap_or("?");
        return Err(Error::rejected(format!("unknown test job {id}")));
    }
    serde_json::from_slice(&fs::read(path)?)
        .map_err(|e| Error::internal(format!("test job {}: {e}", path.display())))
}

fn list_jobs(state_dir: &Path) -> Result<Vec<Job>> {
    let dir = state_dir.join("test-queue/jobs");
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut jobs = Vec::new();
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        jobs.push(read_job(&path)?);
    }
    Ok(jobs)
}

fn write_json(path: &Path, value: &Value) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    let mut file = File::create(&tmp)?;
    file.write_all(serde_json::to_string_pretty(value)?.as_bytes())?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};
    use std::time::Duration;

    fn init_repo(dir: &Path) {
        fs::create_dir_all(dir).unwrap();
        fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname = \"t\"\nversion = \"0.0.0\"\n",
        )
        .unwrap();
        fs::write(dir.join("Cargo.lock"), "# lock\n").unwrap();
        let git = |args: &[&str]| {
            let status = Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(args)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@example.com")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@example.com")
                .status()
                .unwrap();
            assert!(status.success(), "{args:?}");
        };
        git(&["init", "-q"]);
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "init"]);
    }

    fn limits_one() -> Limits {
        Limits {
            build_slots: 1,
            jobs_per_lane: 4,
        }
    }

    fn submit_repo(
        state: &Path,
        repo: &Path,
        filter: &str,
        no_cache: bool,
        env_lock: &str,
    ) -> Value {
        let mut env = BTreeMap::new();
        env.insert("CADENCE_SUITE_LOCK".into(), env_lock.into());
        env.insert("CADENCE_ALIAS".into(), "should-not-key".into());
        env.insert(
            "CADENCE_STATE_DIR".into(),
            "/home/ubuntu/.local/state/cadence".into(),
        );
        submit_with(
            state,
            &Submit {
                worktree: repo.to_path_buf(),
                filter: filter.into(),
                full: false,
                no_cache,
                env,
                rustflags: String::new(),
                by: "cur-129".into(),
            },
            &limits_one(),
            &Runner::Shell("exit 0\n".into()),
        )
        .unwrap()
    }

    fn settle(worker: &mut Worker) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            worker.tick().unwrap();
            let view = queue_view(&worker.state_dir).unwrap();
            let running = view["running"].as_array().map(|a| a.len()).unwrap_or(0);
            let queued = view["queued"].as_u64().unwrap_or(0);
            if running == 0 && queued == 0 {
                return;
            }
            assert!(std::time::Instant::now() < deadline, "job did not finish");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn allowlist_drops_identity_and_state_dir() {
        let env = allowlisted_env([
            ("CADENCE_ALIAS", "cur-129"),
            ("CADENCE_STATE_DIR", "/home/ubuntu/.local/state/cadence"),
            ("CADENCE_SUITE_LOCK", "/tmp/suite"),
            ("PATH", "/usr/bin"),
            ("CADENCE_TEST_SEAM", "1"),
        ]);
        assert_eq!(env.len(), 1);
        assert_eq!(
            env.get("CADENCE_SUITE_LOCK").map(String::as_str),
            Some("/tmp/suite")
        );
    }

    #[test]
    fn child_env_never_points_at_the_daemon_state() {
        let tmp = tempfile::tempdir().unwrap();
        let daemon = tmp.path().join("state");
        fs::create_dir_all(&daemon).unwrap();
        let isolation = daemon.join("test-queue/run/tq-1");
        fs::create_dir_all(isolation.join("state")).unwrap();
        let mut pass = BTreeMap::new();
        pass.insert("PATH".into(), "/usr/bin".into());
        pass.insert(
            "CADENCE_STATE_DIR".into(),
            "/home/ubuntu/.local/state/cadence".into(),
        );
        pass.insert("CADENCE_ALIAS".into(), "cur-129".into());
        pass.insert("SECRET".into(), "nope".into());
        pass.insert("RUSTUP_HOME".into(), "/opt/rustup".into());
        let env = child_env(&IsoSpec {
            daemon_state: &daemon,
            isolation: &isolation,
            jobs: 4,
            port: 3110,
            pass: &pass,
        })
        .unwrap();
        let state = env.get("CADENCE_STATE_DIR").unwrap();
        assert_ne!(state, daemon.to_str().unwrap());
        assert_ne!(state, "/home/ubuntu/.local/state/cadence");
        assert!(state.ends_with("test-queue/run/tq-1/state"), "{state}");
        assert_eq!(env.get("CARGO_BUILD_JOBS").map(String::as_str), Some("4"));
        assert_eq!(env.get("CADENCE_PORT").map(String::as_str), Some("3110"));
        assert_eq!(
            env.get("RUSTUP_HOME").map(String::as_str),
            Some("/opt/rustup")
        );
        assert!(!env.contains_key("CADENCE_ALIAS"));
        assert!(!env.contains_key("SECRET"));
        assert!(!env
            .values()
            .any(|v| v == "/home/ubuntu/.local/state/cadence"));
    }

    #[test]
    fn worktree_inside_the_daemon_state_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("state");
        let repo = state.join("repo");
        fs::create_dir_all(&state).unwrap();
        init_repo(&repo);
        let err = submit_with(
            &state,
            &Submit {
                worktree: repo,
                filter: String::new(),
                full: false,
                no_cache: false,
                env: BTreeMap::new(),
                rustflags: String::new(),
                by: "cur-129".into(),
            },
            &limits_one(),
            &Runner::Shell("exit 0\n".into()),
        )
        .unwrap_err();
        assert!(err.to_string().contains("daemon state"), "{err}");
    }

    #[test]
    fn cache_hit_returns_the_same_job_and_no_cache_reruns() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("state");
        let repo = tmp.path().join("repo");
        fs::create_dir_all(&state).unwrap();
        init_repo(&repo);
        let first = submit_repo(&state, &repo, "alpha", false, "/tmp/suite");
        assert_eq!(first["cache"], "miss");
        let mut worker =
            Worker::new(&state, limits_one(), Runner::Shell("exit 0\n".into())).unwrap();
        settle(&mut worker);
        let again = submit_repo(&state, &repo, "alpha", false, "/tmp/suite");
        assert_eq!(again["cache"], "hit");
        assert_eq!(again["id"], first["id"]);
        assert!(again["age_secs"].as_u64().is_some());
        let forced = submit_repo(&state, &repo, "alpha", true, "/tmp/suite");
        assert_eq!(forced["cache"], "miss");
        assert_ne!(forced["id"], first["id"]);
    }

    #[test]
    fn a_failure_is_not_a_cache_hit() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("state");
        let repo = tmp.path().join("repo");
        fs::create_dir_all(&state).unwrap();
        init_repo(&repo);
        let first = submit_repo(&state, &repo, "alpha", false, "/tmp/suite");
        let mut worker =
            Worker::new(&state, limits_one(), Runner::Shell("exit 3\n".into())).unwrap();
        settle(&mut worker);
        let view = status(&state, first["id"].as_str().unwrap()).unwrap();
        assert_eq!(view["state"], "failed");
        assert_eq!(view["exit_code"], 3);
        let again = submit_repo(&state, &repo, "alpha", false, "/tmp/suite");
        assert_eq!(again["cache"], "miss");
        assert_ne!(again["id"], first["id"]);
        let ledger = fs::read_to_string(state.join("reviews/test-runs.jsonl")).unwrap();
        assert!(ledger.contains("\"outcome\":\"failed\""), "{ledger}");
        assert!(ledger.contains("\"tree\""), "{ledger}");
    }

    #[test]
    fn a_different_filter_or_env_is_a_different_key() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("state");
        let repo = tmp.path().join("repo");
        fs::create_dir_all(&state).unwrap();
        init_repo(&repo);
        let a = submit_repo(&state, &repo, "alpha", false, "/tmp/suite");
        let b = submit_repo(&state, &repo, "beta", false, "/tmp/suite");
        let c = submit_repo(&state, &repo, "alpha", false, "/tmp/other");
        assert_ne!(a["id"], b["id"]);
        assert_ne!(a["key"], b["key"]);
        assert_ne!(a["key"], c["key"]);
    }

    #[test]
    fn concurrent_submits_of_the_same_tree_join_one_job() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("state");
        let repo = tmp.path().join("repo");
        fs::create_dir_all(&state).unwrap();
        init_repo(&repo);
        let state = Arc::new(state);
        let repo = Arc::new(repo);
        let barrier = Arc::new(Barrier::new(2));
        let mut handles = Vec::new();
        for _ in 0..2 {
            let (state, repo, barrier) =
                (Arc::clone(&state), Arc::clone(&repo), Arc::clone(&barrier));
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                submit_repo(&state, &repo, "alpha", false, "/tmp/suite")
            }));
        }
        let ids: Vec<String> = handles
            .into_iter()
            .map(|h| h.join().unwrap()["id"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(ids[0], ids[1], "{ids:?}");
    }

    #[test]
    fn no_cache_joins_an_inflight_run_but_not_a_finished_one() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("state");
        let repo = tmp.path().join("repo");
        fs::create_dir_all(&state).unwrap();
        init_repo(&repo);
        let hold = r#"
touch "$CADENCE_TEST_ISOLATION/started"
while [ ! -f "$CADENCE_TEST_ISOLATION/go" ]; do sleep 0.02; done
exit 0
"#;
        let first = submit_repo(&state, &repo, "alpha", true, "/tmp/suite");
        let mut worker = Worker::new(&state, limits_one(), Runner::Shell(hold.into())).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            worker.tick().unwrap();
            let marker = PathBuf::from(first["isolation_dir"].as_str().unwrap()).join("started");
            if marker.exists() {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "runner did not start");
            std::thread::sleep(Duration::from_millis(20));
        }
        let joined = submit_repo(&state, &repo, "alpha", true, "/tmp/suite");
        assert_eq!(joined["joined"], true);
        assert_eq!(joined["id"], first["id"]);
        fs::write(
            PathBuf::from(first["isolation_dir"].as_str().unwrap()).join("go"),
            "1",
        )
        .unwrap();
        settle(&mut worker);
        let rerun = submit_repo(&state, &repo, "alpha", true, "/tmp/suite");
        assert_eq!(rerun["cache"], "miss");
        assert_ne!(rerun["id"], first["id"]);
    }

    #[test]
    fn fifo_runs_one_at_a_time_in_submit_order() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("state");
        let repo = tmp.path().join("repo");
        fs::create_dir_all(&state).unwrap();
        init_repo(&repo);
        let hold = r#"
echo "$CADENCE_TEST_JOB_WAIT" >> "$CADENCE_TEST_ISOLATION/order" || true
touch "$CADENCE_TEST_ISOLATION/started"
while [ ! -f "$CADENCE_TEST_ISOLATION/go" ]; do sleep 0.02; done
exit 0
"#;
        // The shell runner does not receive the job id in argv. Record
        // order by which isolation dir starts first.
        let a = submit_repo(&state, &repo, "alpha", false, "/tmp/suite");
        let b = submit_repo(&state, &repo, "beta", false, "/tmp/suite");
        let mut worker = Worker::new(&state, limits_one(), Runner::Shell(hold.into())).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            worker.tick().unwrap();
            let started = PathBuf::from(a["isolation_dir"].as_str().unwrap()).join("started");
            if started.exists() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "first job did not start"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        let view = queue_view(&state).unwrap();
        assert_eq!(view["holder"], a["id"]);
        assert_eq!(view["queued"], 1);
        assert!(!PathBuf::from(b["isolation_dir"].as_str().unwrap())
            .join("started")
            .exists());
        fs::write(
            PathBuf::from(a["isolation_dir"].as_str().unwrap()).join("go"),
            "1",
        )
        .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            worker.tick().unwrap();
            let started = PathBuf::from(b["isolation_dir"].as_str().unwrap()).join("started");
            if started.exists() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "second job did not start"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        fs::write(
            PathBuf::from(b["isolation_dir"].as_str().unwrap()).join("go"),
            "1",
        )
        .unwrap();
        settle(&mut worker);
        let a_done = status(&state, a["id"].as_str().unwrap()).unwrap();
        assert_eq!(a_done["state"], "passed");
        assert!(a_done["seq"].as_u64() < b["seq"].as_u64());
    }

    #[test]
    fn a_full_run_does_not_share_the_queue() {
        let running = vec![Job {
            id: "tq-full".into(),
            seq: 1,
            state: "running".into(),
            key: "k".into(),
            worktree: "/w".into(),
            filter: String::new(),
            full: true,
            tree: "t".into(),
            commit: "c".into(),
            repo: "r".into(),
            by: "b".into(),
            no_cache: false,
            runner: "shell".into(),
            argv: vec![],
            created_at: 0.0,
            started_at: Some(0.0),
            finished_at: None,
            exit_code: None,
            outcome: None,
            pid: Some(1),
            port: Some(3110),
            cargo_build_jobs: 4,
            isolation_dir: "/i".into(),
            log_path: "/i/log".into(),
            queued_ms: Some(0),
            execute_ms: None,
        }];
        let queued = vec![Job {
            full: false,
            seq: 2,
            id: "tq-next".into(),
            state: "queued".into(),
            ..running[0].clone()
        }];
        assert!(select_next(&running, &queued, 3).is_empty());
    }
}
