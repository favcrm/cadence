//! `cadence review <PR>` — the reviewer's mechanical routine as one
//! command: resolve the PR through `gh`, gate a detached checkout
//! (the merge result when the base moved), stress the new tests that
//! wait on daemon state, run the full suite once, and rerun every
//! failure in isolation on the gated tree and on the base head before
//! calling it a regression. The output is a Markdown report plus JSON
//! under the state dir — the hands-on check and the verdict stay with
//! the reviewer. The command never posts a status, never merges,
//! never pushes.
//!
//! The steps are data, not code: `cadence-review.toml` declares
//! `prepare`, `gates`, `full_suite`, `test_globs`, `stress_pattern`
//! and `test_command`. It is read from the base branch head
//! (`git show <base>:cadence-review.toml`), never from the PR tree or
//! the reviewer's working tree, so a PR cannot weaken the gates it is
//! judged by. A PR that changes the file is flagged in the report and
//! its suggested verdict is never `pass`. Every subprocess goes through
//! [`crate::proc::run_bounded`]; timeouts come from `[timeouts]`
//! (defaults until the config is loaded).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::issue::time;
use crate::proc::{run_bounded, BoundedError};
use crate::worktree;

/// How many trailing lines of a failing step's output the report keeps.
const TAIL_LINES: usize = 40;
/// Config file `run` reads from the base branch head (never the PR tree
/// or the reviewer's working tree).
pub const CONFIG_FILE: &str = "cadence-review.toml";
/// Required and optional keys, named when the config file is absent.
const CONFIG_KEYS: &str = "Required keys: prepare, gates, full_suite, test_globs, \
     test_command, stress_pattern; optional: [timeouts] prepare_secs gate_secs \
     stress_secs full_secs test_secs git_secs gh_secs";

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

/// `stress_pattern` accepts a single string or a list of strings; a new
/// test is stressed when its name or added body contains any of them.
#[derive(Clone, Debug, Default)]
pub struct Patterns(pub Vec<String>);

impl<'de> Deserialize<'de> for Patterns {
    fn deserialize<D>(d: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum OneOrMany {
            One(String),
            Many(Vec<String>),
        }
        Ok(match OneOrMany::deserialize(d)? {
            OneOrMany::One(s) => Patterns(vec![s]),
            OneOrMany::Many(v) => Patterns(v),
        })
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct Timeouts {
    /// Per `prepare` command [default 900].
    #[serde(default = "t_prepare")]
    pub prepare_secs: u64,
    /// Per `gates` command [default 1800].
    #[serde(default = "t_gate")]
    pub gate_secs: u64,
    /// Per single stress run [default 900].
    #[serde(default = "t_stress")]
    pub stress_secs: u64,
    /// The `full_suite` command [default 3600].
    #[serde(default = "t_full")]
    pub full_secs: u64,
    /// Per isolated `test_command` run [default 900].
    #[serde(default = "t_test")]
    pub test_secs: u64,
    /// Per git operation [default 300].
    #[serde(default = "t_git")]
    pub git_secs: u64,
    /// Per `gh` call [default 60].
    #[serde(default = "t_gh")]
    pub gh_secs: u64,
}

/// The subprocess backend used for the full suite and isolated reruns.
/// Keeping this explicit prevents a nextest suite from being adjudicated by
/// a cargo child with different process and retry semantics.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ReviewBackend {
    #[default]
    Cargo,
    Nextest,
}

/// Machine-readable result format emitted by the configured backend.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ResultFormat {
    #[default]
    Cargo,
    Junit,
}

fn default_result_path() -> String {
    "target/nextest/cadence/junit.xml".into()
}

#[derive(Clone, Debug, Deserialize)]
pub struct RunnerConfig {
    /// Backend used by both `full_suite` and `test_command`.
    #[serde(default)]
    pub backend: ReviewBackend,
    /// How an individual command proves what ran and what failed.
    #[serde(default)]
    pub result_format: ResultFormat,
    /// Repo-relative report path written by a structured backend.
    #[serde(default = "default_result_path")]
    pub result_path: String,
}

impl Default for RunnerConfig {
    fn default() -> Self {
        Self {
            backend: ReviewBackend::Cargo,
            result_format: ResultFormat::Cargo,
            result_path: default_result_path(),
        }
    }
}

fn t_prepare() -> u64 {
    900
}
fn t_gate() -> u64 {
    1800
}
fn t_stress() -> u64 {
    900
}
fn t_full() -> u64 {
    3600
}
fn t_test() -> u64 {
    900
}
fn t_git() -> u64 {
    300
}
fn t_gh() -> u64 {
    60
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            prepare_secs: t_prepare(),
            gate_secs: t_gate(),
            stress_secs: t_stress(),
            full_secs: t_full(),
            test_secs: t_test(),
            git_secs: t_git(),
            gh_secs: t_gh(),
        }
    }
}

/// `cadence-review.toml` — every step the verb performs is data here.
#[derive(Clone, Debug, Deserialize)]
pub struct ReviewConfig {
    /// Commands run once in the review checkout before gating (build
    /// steps, dependency links).
    pub prepare: Vec<String>,
    /// Ordered quality gates; the first failure stops the sequence.
    pub gates: Vec<String>,
    /// The full test suite, run once.
    pub full_suite: String,
    /// Diff paths that count as test files (`*`/`**`/`?` globs).
    pub test_globs: Vec<String>,
    /// How one test runs alone; `{test}` = fn name, `{file}` = diff
    /// path, `{target}` = file stem (cargo `--test <target>`).
    pub test_command: String,
    /// Substrings marking a new test as "waits on daemon state" —
    /// matched tests are stressed `--stress` times each.
    #[serde(default)]
    pub stress_pattern: Patterns,
    #[serde(default)]
    pub timeouts: Timeouts,
    /// Optional runner contract. The default preserves the historical cargo
    /// text path; nextest activation must opt into a structured report.
    #[serde(default)]
    pub runner: RunnerConfig,
}

impl ReviewConfig {
    /// Load `<root>/cadence-review.toml` from disk; a missing file names
    /// the required keys so a fresh repo can write one without guessing.
    /// `run` never reads the working tree — see `config_at_base`.
    pub fn load(root: &Path) -> Result<Self> {
        let path = root.join(CONFIG_FILE);
        if !path.is_file() {
            return Err(Error::rejected(format!(
                "no {CONFIG_FILE} at {} — `cadence review` reads its steps \
                 from that file. {CONFIG_KEYS}",
                root.display()
            )));
        }
        let text = std::fs::read_to_string(&path)?;
        Self::parse(&text, &path.display().to_string())
    }

    /// Parse and validate config text; `origin` (a path, or
    /// `<sha>:cadence-review.toml`) names the source in every error.
    pub fn parse(text: &str, origin: &str) -> Result<Self> {
        let cfg: ReviewConfig = toml::from_str(text)
            .map_err(|e| Error::rejected(format!("{origin} is not valid TOML: {e}")))?;
        if cfg.gates.is_empty() {
            return Err(Error::rejected(format!(
                "{origin}: `gates` must name at least one command"
            )));
        }
        if cfg.test_globs.is_empty() {
            return Err(Error::rejected(format!(
                "{origin}: `test_globs` must name at least one pattern"
            )));
        }
        if !cfg.test_command.contains("{test}") {
            return Err(Error::rejected(format!(
                "{origin}: `test_command` must contain a {{test}} placeholder"
            )));
        }
        if cfg.runner.result_format == ResultFormat::Junit
            && !safe_rel_path(&cfg.runner.result_path)
        {
            return Err(Error::rejected(format!(
                "{origin}: runner.result_path must be a safe repo-relative path"
            )));
        }
        if cfg.runner.backend == ReviewBackend::Nextest {
            if cfg.runner.result_format != ResultFormat::Junit {
                return Err(Error::rejected(format!(
                    "{origin}: nextest requires runner.result_format = 'junit'"
                )));
            }
            if !command_mentions_nextest(&cfg.full_suite)
                || !command_mentions_nextest(&cfg.test_command)
            {
                return Err(Error::rejected(format!(
                    "{origin}: nextest backend requires both full_suite and test_command to use scripts/cadence-nextest"
                )));
            }
            if !cfg.test_command.contains("--exact") || !cfg.test_command.contains("--") {
                return Err(Error::rejected(format!(
                    "{origin}: nextest test_command must use an exact libtest filter after '--'"
                )));
            }
        }
        Ok(cfg)
    }
}

fn command_mentions_nextest(command: &str) -> bool {
    command.split_whitespace().any(|part| {
        part.trim_matches(|c| c == '\'' || c == '"')
            .ends_with("cadence-nextest")
    })
}

fn backend_label(backend: ReviewBackend) -> &'static str {
    match backend {
        ReviewBackend::Cargo => "cargo",
        ReviewBackend::Nextest => "nextest",
    }
}

fn result_format_label(format: ResultFormat) -> &'static str {
    match format {
        ResultFormat::Cargo => "cargo-text",
        ResultFormat::Junit => "junit",
    }
}

// ---------------------------------------------------------------------------
// Flake ledger and host load
// ---------------------------------------------------------------------------

/// `<state>/reviews/flakes.jsonl` — one line per sighting of a test that
/// failed in the full run but passed alone on the gated tree and on the
/// base. The ledger is the quarantine: no attribute in the code.
pub const FLAKE_LEDGER: &str = "flakes.jsonl";
/// Distinct PR heads a flake needs sightings on before it stops
/// blocking — repeated reviews of one head never qualify on their own.
pub const KNOWN_FLAKE_HEADS: usize = 3;

/// What the ledger holds for one `(repo, test)` after a sighting.
#[derive(Debug, PartialEq)]
pub struct Sightings {
    /// Every sighting, this one included.
    pub total: u64,
    /// Distinct heads among them — the current head counts once.
    pub heads: usize,
}

impl Sightings {
    pub fn known_flake(&self) -> bool {
        self.heads >= KNOWN_FLAKE_HEADS
    }
}

/// Append `entry` (`repo`, `test`, `head`, …) and return the sightings
/// of its `(repo, test)` now on record. The read and the append happen
/// under an exclusive `flock` on the ledger, so concurrent reviews see
/// exact counts; each line is one `write_all`, so appends never fuse.
/// Malformed lines are skipped.
pub fn record_flake(ledger: &Path, entry: &Value) -> Result<Sightings> {
    use std::io::{Read, Write};
    use std::os::unix::io::AsRawFd;
    if let Some(dir) = ledger.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .open(ledger)?;
    // Released when `f` closes at return.
    if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let mut text = String::new();
    f.read_to_string(&mut text)?;
    let prior: Vec<Value> = text
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| v["repo"] == entry["repo"] && v["test"] == entry["test"])
        .collect();
    let mut heads: std::collections::HashSet<&str> = prior
        .iter()
        .map(|v| v["head"].as_str().unwrap_or_default())
        .collect();
    heads.insert(entry["head"].as_str().unwrap_or_default());
    let seen = Sightings {
        total: prior.len() as u64 + 1,
        heads: heads.len(),
    };
    f.write_all(format!("{}\n", serde_json::to_string(entry)?).as_bytes())?;
    Ok(seen)
}

/// The first lines of `test`'s panic in a full-suite output — enough to
/// tell two sightings apart without storing the whole log.
fn panic_head(output: &str, test: &str) -> String {
    let marker = format!("thread '{test}'");
    let mut lines = output.lines().skip_while(|l| !l.starts_with(&marker));
    lines.by_ref().take(4).collect::<Vec<_>>().join("\n")
}

/// Host facts at suite start, so a load flake is explainable later:
/// cores, the 1-minute load average, and live `cargo test` processes.
fn host_load() -> Value {
    let nproc = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(0);
    let load1 = std::fs::read_to_string("/proc/loadavg").ok().and_then(|s| {
        s.split_whitespace()
            .next()
            .and_then(|v| v.parse::<f64>().ok())
    });
    // This review and its ancestors (a `cargo test` that launched it)
    // are not load on the suite about to run.
    use std::os::unix::ffi::OsStrExt;
    let mut own = vec![std::process::id()];
    while let Some(ppid) = std::fs::read_to_string(format!("/proc/{}/stat", own[own.len() - 1]))
        .ok()
        .and_then(|s| {
            // `pid (comm) state ppid …` — comm may hold spaces; split after `)`.
            s.rsplit_once(')')
                .and_then(|(_, rest)| rest.split_whitespace().nth(1)?.parse::<u32>().ok())
        })
        .filter(|p| *p > 1 && !own.contains(p))
    {
        own.push(ppid);
    }
    let cargo_tests = std::fs::read_dir("/proc")
        .map(|dir| {
            dir.filter_map(|e| e.ok())
                .filter(|e| {
                    e.file_name()
                        .to_str()
                        .and_then(|p| p.parse::<u32>().ok())
                        .is_some_and(|pid| !own.contains(&pid))
                })
                .filter_map(|e| std::fs::read(e.path().join("cmdline")).ok())
                .filter(|raw| {
                    let argv: Vec<&[u8]> = raw.split(|b| *b == 0).collect();
                    let is_cargo = argv.first().is_some_and(|a0| {
                        Path::new(std::ffi::OsStr::from_bytes(a0)).file_name()
                            == Some(std::ffi::OsStr::new("cargo"))
                    });
                    is_cargo && argv.iter().skip(1).any(|a| *a == b"test")
                })
                .count()
        })
        .unwrap_or(0);
    json!({"nproc": nproc, "load1": load1, "cargo_test_processes": cargo_tests})
}

// ---------------------------------------------------------------------------
// Locks
// ---------------------------------------------------------------------------

/// An exclusive `flock` on a file — released by the kernel on drop/exit,
/// so a crashed review never leaves a stale lock.
struct Flock {
    _file: std::fs::File,
}

impl Flock {
    fn open(path: &Path) -> Result<std::fs::File> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        Ok(std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?)
    }

    /// Exclusive, non-blocking: `None` while another holder has it.
    fn try_lock(path: &Path) -> Result<Option<Flock>> {
        use std::os::unix::io::AsRawFd;
        let file = Self::open(path)?;
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        Ok(if rc == 0 {
            Some(Flock { _file: file })
        } else {
            None
        })
    }

    /// Exclusive with a deadline — the host-wide suite slot must not
    /// wait forever behind a wedged holder.
    fn lock_deadline(path: &Path, secs: u64) -> Result<Flock> {
        let deadline = Instant::now() + Duration::from_secs(secs);
        loop {
            if let Some(l) = Self::try_lock(path)? {
                return Ok(l);
            }
            if Instant::now() >= deadline {
                return Err(Error::rejected(format!(
                    "timed out after {secs}s waiting for the suite lock {}",
                    path.display()
                )));
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }
}

// ---------------------------------------------------------------------------
// Subprocess helpers — everything through run_bounded
// ---------------------------------------------------------------------------

struct StepOut {
    status: Option<i32>,
    stdout: String,
    stderr: String,
    timed_out: bool,
}

fn run_cmd(cmd: &mut Command, timeout: Duration) -> Result<StepOut> {
    match run_bounded(cmd, timeout) {
        Ok(out) => Ok(StepOut {
            status: out.status.code(),
            stdout: String::from_utf8_lossy(&out.stdout).to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).to_string(),
            timed_out: false,
        }),
        Err(BoundedError::TimedOut { stdout, stderr }) => Ok(StepOut {
            status: None,
            stdout: String::from_utf8_lossy(&stdout).to_string(),
            stderr: String::from_utf8_lossy(&stderr).to_string(),
            timed_out: true,
        }),
        Err(BoundedError::Spawn(e)) => Err(Error::internal(format!("spawn: {e}"))),
        Err(BoundedError::Wait(e)) => Err(Error::internal(format!("wait: {e}"))),
    }
}

fn git(repo: &Path, args: &[&str], secs: u64) -> Result<String> {
    let out = git_status(repo, args, secs)?;
    if out.timed_out {
        return Err(Error::internal(format!(
            "git {} timed out in {}",
            args.join(" "),
            repo.display()
        )));
    }
    if out.status != Some(0) {
        return Err(Error::rejected(format!(
            "git {} failed in {}: {}",
            args.join(" "),
            repo.display(),
            out.stderr.trim()
        )));
    }
    Ok(out.stdout.trim().to_string())
}

/// `git` where a non-zero exit is data, not an error.
/// Fetch `src` from origin straight into the local ref `dst` and return
/// the commit it names. Never reads or writes `FETCH_HEAD`: that file
/// is shared by every process fetching in this checkout, so a
/// concurrent fetch between ours and a `rev-parse FETCH_HEAD` would
/// resolve someone else's commit (CAD-261).
fn fetch_ref(repo: &Path, src: &str, dst: &str, secs: u64) -> Result<String> {
    git(
        repo,
        &[
            "fetch",
            "-q",
            "--no-write-fetch-head",
            "origin",
            &format!("+{src}:{dst}"),
        ],
        secs,
    )?;
    git(
        repo,
        &["rev-parse", "--verify", &format!("{dst}^{{commit}}")],
        secs,
    )
}

/// `head` as it would land on `base`: a commit object (no ref) whose tree
/// is the clean merge of the two, or `None` when they conflict. Comparing
/// two PRs as landed puts the current base on both sides, so what the
/// base changed between their cut points cannot read as an overlap, and
/// git's own rename detection sees a moved file on both sides (CAD-277).
fn landed(repo: &Path, base: &str, head: &str, secs: u64) -> Result<Option<String>> {
    let mt = git_status(repo, &["merge-tree", "--write-tree", base, head], secs)?;
    match mt.status {
        Some(0) => {}
        Some(1) if !mt.timed_out => return Ok(None),
        _ => {
            return Err(Error::internal(format!(
                "merge-tree {base} {head}: {}",
                if mt.timed_out {
                    "timed out".to_string()
                } else {
                    format!("exited {:?}: {}", mt.status, mt.stderr.trim())
                }
            )))
        }
    }
    let tree = mt.stdout.lines().next().unwrap_or("").trim().to_string();
    // Explicit identity: a review host or CI runner may have none.
    let commit = git(
        repo,
        &[
            "-c",
            "user.name=cadence-review",
            "-c",
            "user.email=cadence-review@localhost",
            "commit-tree",
            &tree,
            "-p",
            base,
            "-p",
            head,
            "-m",
            "cadence review: as landed",
        ],
        secs,
    )?;
    Ok(Some(commit))
}

/// Every path `head` changes itself, `merge-base(base, head)..head`. A
/// rename lists both its old and its new path — plumbing `diff-tree`
/// without `-M` reports it as a delete plus an add, where porcelain
/// `git diff --name-only` would name only the new path — so a file one
/// PR moves still meets the other PR's edit of the old name (CAD-297).
fn changed_paths(repo: &Path, base: &str, head: &str, secs: u64) -> Result<BTreeSet<String>> {
    let mb = git(repo, &["merge-base", base, head], secs)?;
    let out = git_status(
        repo,
        &["diff-tree", "-r", "--name-only", "-z", &mb, head],
        secs,
    )?;
    if out.timed_out || out.status != Some(0) {
        return Err(Error::internal(format!(
            "diff-tree {mb} {head}: {}",
            if out.timed_out {
                "timed out".to_string()
            } else {
                format!("exited {:?}: {}", out.status, out.stderr.trim())
            }
        )));
    }
    Ok(out
        .stdout
        .split('\0')
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .collect())
}

/// The review's private ref namespace, `refs/cadence/review/<pr>/`,
/// deleted when the run ends however it ends.
struct ReviewRefs {
    root: PathBuf,
    prefix: String,
    git_secs: u64,
}

impl ReviewRefs {
    fn new(root: &Path, pr: i64, git_secs: u64) -> Self {
        Self {
            root: root.to_path_buf(),
            prefix: format!("refs/cadence/review/{pr}/"),
            git_secs,
        }
    }

    fn name(&self, leaf: &str) -> String {
        format!("{}{leaf}", self.prefix)
    }
}

impl Drop for ReviewRefs {
    fn drop(&mut self) {
        let Ok(list) = git(
            &self.root,
            &["for-each-ref", "--format=%(refname)", &self.prefix],
            self.git_secs,
        ) else {
            return;
        };
        for r in list.lines().filter(|l| !l.is_empty()) {
            let _ = git(&self.root, &["update-ref", "-d", r], self.git_secs);
        }
    }
}

fn git_status(repo: &Path, args: &[&str], secs: u64) -> Result<StepOut> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(repo).args(args);
    run_cmd(&mut cmd, Duration::from_secs(secs))
}

/// The blob id of `cadence-review.toml` at `sha`, or `None` when the
/// file does not exist in that commit.
fn config_blob(repo: &Path, sha: &str, secs: u64) -> Result<Option<String>> {
    let spec = format!("{sha}:{CONFIG_FILE}");
    let out = git_status(repo, &["rev-parse", "--verify", "-q", &spec], secs)?;
    if out.timed_out {
        return Err(Error::internal(format!(
            "git rev-parse {spec} timed out in {}",
            repo.display()
        )));
    }
    let blob = out.stdout.trim();
    Ok((out.status == Some(0) && !blob.is_empty()).then(|| blob.to_string()))
}

/// The review config as committed at the base head — never the PR's
/// copy, never the reviewer's working tree, so a PR cannot weaken the
/// gates it is judged by (risk class 7). A base without the file is
/// refused; there is no fallback.
fn config_at_base(repo: &Path, base_ref: &str, base_sha: &str, secs: u64) -> Result<ReviewConfig> {
    if config_blob(repo, base_sha, secs)?.is_none() {
        return Err(Error::rejected(format!(
            "no {CONFIG_FILE} at the base head {base_sha} ({base_ref}) — \
             `cadence review` reads its steps from the file committed on \
             the base branch, never from the PR or the working tree. \
             {CONFIG_KEYS}"
        )));
    }
    let spec = format!("{base_sha}:{CONFIG_FILE}");
    let text = git(repo, &["show", &spec], secs)?;
    ReviewConfig::parse(&text, &spec)
}

/// Whether the PR itself changes `cadence-review.toml`: its blob at the
/// merge-base differs from the one at the PR head (absent on one side
/// counts). Compared against the merge-base, not the base head, so a
/// config change that landed on the base after the branch was cut is
/// not blamed on the PR.
fn config_changed_by_pr(repo: &Path, merge_base: &str, head_sha: &str, secs: u64) -> Result<bool> {
    Ok(config_blob(repo, merge_base, secs)? != config_blob(repo, head_sha, secs)?)
}

/// Every `gh` invocation goes through here so tests put a fake `gh`
/// first on PATH.
fn gh(cwd: &Path, args: &[String], secs: u64) -> Result<String> {
    let mut cmd = Command::new("gh");
    cmd.args(args).current_dir(cwd);
    let out = run_cmd(&mut cmd, Duration::from_secs(secs))?;
    if out.timed_out {
        return Err(Error::internal(format!("gh {} timed out", args.join(" "))));
    }
    if out.status != Some(0) {
        return Err(Error::rejected(format!(
            "gh {} failed: {}",
            args.join(" "),
            out.stderr.trim()
        )));
    }
    Ok(out.stdout.trim().to_string())
}

fn tail(text: &str, n: usize) -> Vec<String> {
    let lines: Vec<&str> = text.lines().collect();
    lines
        .iter()
        .skip(lines.len().saturating_sub(n))
        .map(|s| s.to_string())
        .collect()
}

/// One testcase from a structured backend report. JUnit keeps names in the
/// same bare form accepted by libtest, which lets the isolated command use
/// the exact filter without guessing at package or binary prefixes.
#[derive(Clone, Debug)]
struct TestCaseResult {
    name: String,
    outcome: &'static str,
    duration_s: Option<f64>,
}

/// The smallest structured evidence needed by the review path: non-empty
/// testcase coverage, failure names, and per-test timings. Missing or empty
/// evidence is deliberately invalid so a successful process cannot launder a
/// zero-test or missing-report run into a pass.
#[derive(Clone, Debug)]
struct TestRunSummary {
    valid: bool,
    test_count: u64,
    passed: u64,
    failed: u64,
    skipped: u64,
    tests: Vec<TestCaseResult>,
    failed_tests: Vec<String>,
    reason: Option<String>,
}

impl TestRunSummary {
    fn invalid(reason: impl Into<String>) -> Self {
        Self {
            valid: false,
            test_count: 0,
            passed: 0,
            failed: 0,
            skipped: 0,
            tests: Vec::new(),
            failed_tests: Vec::new(),
            reason: Some(reason.into()),
        }
    }

    fn to_json(&self) -> Value {
        let tests: Vec<Value> = self
            .tests
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "outcome": t.outcome,
                    "duration_s": t.duration_s,
                })
            })
            .collect();
        json!({
            "format": "junit",
            "valid": self.valid,
            "test_count": self.test_count,
            "passed": self.passed,
            "failed": self.failed,
            "skipped": self.skipped,
            "tests": tests,
            "failed_tests": self.failed_tests,
            "reason": self.reason,
        })
    }

    fn executed_count(&self) -> u64 {
        self.passed + self.failed
    }
}

/// Parse the Jenkins XML emitted by cargo-nextest's configured JUnit
/// profile. This intentionally accepts only the small generated subset we
/// need instead of adding an XML dependency to the CLI; malformed, missing,
/// and empty documents remain invalid and therefore fail closed.
fn parse_junit(text: &str) -> TestRunSummary {
    let mut root_seen = false;
    let mut tests = Vec::new();
    let mut current: Option<usize> = None;
    let mut stack: Vec<String> = Vec::new();
    let mut cursor = 0usize;
    while let Some((end, raw)) = next_xml_tag(text, cursor) {
        cursor = end;
        let tag = raw.trim();
        if tag.starts_with("?") || tag.starts_with('!') {
            continue;
        }
        let closing = tag.starts_with('/');
        let self_closing = tag.ends_with('/');
        let name = xml_tag_name(tag);
        if name.is_empty() {
            return TestRunSummary::invalid("JUnit report has an empty tag name");
        }
        if closing {
            if stack.pop().as_deref() != Some(name) {
                return TestRunSummary::invalid("JUnit report has mismatched closing tags");
            }
        } else if !self_closing {
            stack.push(name.to_string());
        }
        if name == "testsuites" && !closing {
            root_seen = true;
        } else if name == "testcase" && !closing {
            let Some(raw_name) = xml_attr(tag, "name") else {
                return TestRunSummary::invalid("JUnit testcase has no name");
            };
            let name = xml_unescape(&raw_name);
            if name.is_empty() {
                return TestRunSummary::invalid("JUnit testcase has an empty name");
            }
            let duration_s = xml_attr(tag, "time").and_then(|v| v.parse::<f64>().ok());
            tests.push(TestCaseResult {
                name,
                outcome: "pass",
                duration_s,
            });
            current = Some(tests.len() - 1);
            if self_closing {
                current = None;
            }
        } else if !closing && matches!(name, "failure" | "error" | "flakyFailure") {
            if let Some(i) = current {
                tests[i].outcome = "fail";
            }
        } else if !closing && name == "skipped" {
            if let Some(i) = current {
                if tests[i].outcome == "pass" {
                    tests[i].outcome = "skipped";
                }
            }
        } else if closing && name == "testcase" {
            current = None;
        }
    }

    if !stack.is_empty() {
        return TestRunSummary::invalid("JUnit report ended before closing all tags");
    }
    if !root_seen {
        return TestRunSummary::invalid("JUnit report has no <testsuites> root");
    }
    if tests.is_empty() {
        return TestRunSummary::invalid("JUnit report contained zero testcases");
    }

    let mut passed = 0;
    let mut failed = 0;
    let mut skipped = 0;
    let mut failed_tests = Vec::new();
    for test in &tests {
        match test.outcome {
            "pass" => passed += 1,
            "fail" => {
                failed += 1;
                failed_tests.push(test.name.clone());
            }
            "skipped" => skipped += 1,
            _ => {}
        }
    }
    failed_tests.sort();
    failed_tests.dedup();
    let reason =
        (passed + failed == 0).then(|| "JUnit report contained no executed testcases".to_string());
    TestRunSummary {
        valid: true,
        test_count: tests.len() as u64,
        passed,
        failed,
        skipped,
        tests,
        failed_tests,
        reason,
    }
}

fn parse_junit_file(path: &Path) -> TestRunSummary {
    match std::fs::read_to_string(path) {
        Ok(text) => parse_junit(&text),
        Err(e) => {
            TestRunSummary::invalid(format!("JUnit report {} unavailable: {e}", path.display()))
        }
    }
}

fn next_xml_tag(text: &str, from: usize) -> Option<(usize, &str)> {
    let start = from + text[from..].find('<')?;
    let end = start + text[start..].find('>')? + 1;
    Some((end, &text[start + 1..end - 1]))
}

fn xml_tag_name(tag: &str) -> &str {
    tag.trim_start_matches('/')
        .trim_end_matches('/')
        .split_whitespace()
        .next()
        .unwrap_or("")
}

fn xml_attr(tag: &str, wanted: &str) -> Option<String> {
    let mut rest = tag.trim_start_matches('/').trim();
    let name = xml_tag_name(rest);
    rest = rest.get(name.len()..)?.trim_start();
    while !rest.is_empty() {
        let key_end = rest
            .find(|c: char| c.is_ascii_whitespace() || c == '=')
            .unwrap_or(rest.len());
        let key = &rest[..key_end];
        rest = rest[key_end..].trim_start();
        if !rest.starts_with('=') {
            rest = rest
                .find(char::is_whitespace)
                .map(|i| rest[i..].trim_start())
                .unwrap_or("");
            continue;
        }
        rest = rest[1..].trim_start();
        let quote = rest.chars().next()?;
        if quote != '\'' && quote != '"' {
            return None;
        }
        let value = &rest[quote.len_utf8()..];
        let end = value.find(quote)?;
        let parsed = value[..end].to_string();
        rest = rest[quote.len_utf8() + end + quote.len_utf8()..].trim_start();
        if key == wanted {
            return Some(parsed);
        }
    }
    None
}

fn xml_unescape(value: &str) -> String {
    value
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// One recorded command execution for the report.
struct Step {
    name: String,
    cmd: String,
    duration_ms: u128,
    /// ok | fail | timeout | skipped
    outcome: &'static str,
    exit: Option<i32>,
    tail: Vec<String>,
    /// Full stdout+stderr — kept for failure-name extraction, never
    /// serialized into the report.
    output: String,
    /// Optional structured result written by the configured backend.
    result: Option<TestRunSummary>,
}

impl Step {
    fn to_json(&self) -> Value {
        json!({
            "name": self.name,
            "cmd": self.cmd,
            "duration_ms": self.duration_ms,
            "outcome": self.outcome,
            "exit": self.exit,
            "tail": self.tail,
            "result": self.result.as_ref().map(TestRunSummary::to_json),
        })
    }
}

// ---------------------------------------------------------------------------
// The gate environment — CI's identity-less git (CAD-301)
// ---------------------------------------------------------------------------

/// Variables that hand git an identity (or a config file that may hold
/// one) ahead of `HOME`. GitHub CI sets none of them, so every step
/// command runs with them removed; `HOME` then decides alone.
const GATE_ENV_UNSET: &[&str] = &[
    "GIT_AUTHOR_NAME",
    "GIT_AUTHOR_EMAIL",
    "GIT_COMMITTER_NAME",
    "GIT_COMMITTER_EMAIL",
    "EMAIL",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_SYSTEM",
    "XDG_CONFIG_HOME",
];

/// Config keys that name an identity. A caller's `GIT_CONFIG_KEY_<n>`
/// entry for one of these is dropped; every other entry is kept.
const IDENTITY_KEYS: &[&str] = &[
    "user.name",
    "user.email",
    "author.name",
    "author.email",
    "committer.name",
    "committer.email",
    "user.useconfigonly",
];

/// CAD-301: the env every step command runs under — prepare, gates,
/// the full suite, stress runs and isolated reruns, on the gated tree
/// and the base tree alike. GitHub CI runners have no git identity, so
/// a test or tool that runs `git commit` without `-c user.name=… -c
/// user.email=…` fails there with "Author identity unknown"; this host
/// would otherwise lend it a global `~/.gitconfig` or git's own
/// user@hostname guess, and the review would pass what CI then fails.
///
/// - `HOME` is `home`, a fresh empty directory the review removes when
///   it ends: no `~/.gitconfig`, no `~/.config/git/config`
///   ([`GATE_ENV_UNSET`] clears the variables that would point past it).
/// - `GIT_CONFIG_NOSYSTEM=1`: no `/etc/gitconfig`.
/// - `user.useConfigOnly=true`, appended through `GIT_CONFIG_COUNT`:
///   git stops guessing an identity, so only an explicit one works — as
///   on CI. A caller's own `GIT_CONFIG_*` entries are kept (renumbered)
///   except identity keys.
/// - `CARGO_HOME`, `RUSTUP_HOME` and `XDG_DATA_HOME` stay at the
///   caller's real locations (derived from the real `HOME` when unset):
///   cargo's registry, rustup's toolchains and the pinned nextest under
///   `$XDG_DATA_HOME/cadence/tools` (`scripts/cadence-nextest`) still
///   resolve.
///
/// The review's own git calls (fetch, merge, worktree, merge-tree and
/// commit-tree) never take this env: fetch keeps the real `HOME` for its
/// credential helper, and commit-tree names its identity explicitly.
fn gate_env(home: &Path, caller: impl Fn(&str) -> Option<String>) -> Vec<(String, String)> {
    let caller = |k: &str| caller(k).filter(|v| !v.is_empty());
    let real_home = caller("HOME");
    let mut env = Vec::new();
    for (var, under_home) in [
        ("CARGO_HOME", ".cargo"),
        ("RUSTUP_HOME", ".rustup"),
        ("XDG_DATA_HOME", ".local/share"),
    ] {
        let real = caller(var).or_else(|| real_home.as_ref().map(|h| format!("{h}/{under_home}")));
        if let Some(real) = real {
            env.push((var.to_string(), real));
        }
    }
    env.push(("HOME".into(), home.to_string_lossy().into_owned()));
    env.push(("GIT_CONFIG_NOSYSTEM".into(), "1".into()));

    let count = caller("GIT_CONFIG_COUNT")
        .and_then(|c| c.trim().parse::<usize>().ok())
        .unwrap_or(0);
    let mut entries: Vec<(String, String)> = (0..count)
        .filter_map(|i| {
            let key = caller(&format!("GIT_CONFIG_KEY_{i}"))?;
            let value = caller(&format!("GIT_CONFIG_VALUE_{i}")).unwrap_or_default();
            (!IDENTITY_KEYS.contains(&key.to_ascii_lowercase().as_str())).then_some((key, value))
        })
        .collect();
    entries.push(("user.useConfigOnly".into(), "true".into()));
    env.push(("GIT_CONFIG_COUNT".into(), entries.len().to_string()));
    for (i, (key, value)) in entries.into_iter().enumerate() {
        env.push((format!("GIT_CONFIG_KEY_{i}"), key));
        env.push((format!("GIT_CONFIG_VALUE_{i}"), value));
    }
    env
}

/// Run `sh -c <cmd>` in `cwd` with the review's env, timing it.
fn run_step(
    name: &str,
    cmd: &str,
    cwd: &Path,
    env: &[(String, String)],
    secs: u64,
) -> Result<Step> {
    run_step_with_result(name, cmd, cwd, env, secs, None)
}

/// Run a step and, when configured, consume the report produced by the
/// backend. The report is removed before launch so a stale previous run can
/// never be mistaken for current evidence.
fn run_step_with_result(
    name: &str,
    cmd: &str,
    cwd: &Path,
    env: &[(String, String)],
    secs: u64,
    result_path: Option<&Path>,
) -> Result<Step> {
    if let Some(path) = result_path {
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(Error::rejected(format!(
                    "cannot clear prior structured result {}: {e}",
                    path.display()
                )))
            }
        }
    }
    let started = Instant::now();
    let mut sh = Command::new("sh");
    sh.arg("-c").arg(cmd).current_dir(cwd);
    for k in GATE_ENV_UNSET {
        sh.env_remove(k);
    }
    for (k, v) in env {
        sh.env(k, v);
    }
    let out = run_cmd(&mut sh, Duration::from_secs(secs))?;
    let duration_ms = started.elapsed().as_millis();
    let combined = format!("{}\n{}", out.stdout, out.stderr);
    let (mut outcome, mut tail) = if out.timed_out {
        ("timeout", tail(&combined, TAIL_LINES))
    } else if out.status == Some(0) {
        ("ok", Vec::new())
    } else {
        ("fail", tail(&combined, TAIL_LINES))
    };
    let result = result_path.map(parse_junit_file);
    if let Some(summary) = &result {
        if (!summary.valid || summary.executed_count() == 0) && outcome == "ok" {
            outcome = "fail";
            tail = summary
                .reason
                .as_deref()
                .map(|reason| vec![reason.to_string()])
                .unwrap_or_else(|| vec!["structured result was invalid".into()]);
        }
    }
    Ok(Step {
        name: name.to_string(),
        cmd: cmd.to_string(),
        duration_ms,
        outcome,
        exit: out.status,
        tail,
        output: combined,
        result,
    })
}

/// Pass the outer-slot contract to a full-suite child only when this review
/// actually owns the host flock. A `--no-suite-lock` review must not mint the
/// held marker because a future nextest command would otherwise bypass its
/// fail-closed lock check.
fn suite_child_env(
    mut suite_env: Vec<(String, String)>,
    outer_slot_held: bool,
) -> Vec<(String, String)> {
    if outer_slot_held {
        suite_env.push(("CADENCE_SUITE_LOCK".into(), String::new()));
        suite_env.push(("CADENCE_REVIEW_SUITE_LOCK_HELD".into(), "1".into()));
    }
    suite_env
}

fn runner_result_path(cwd: &Path, runner: &RunnerConfig) -> Option<PathBuf> {
    (runner.result_format == ResultFormat::Junit).then(|| cwd.join(&runner.result_path))
}

/// Render the full-suite object with the stable CAD-173 consumer fields at
/// its top level. Keep the nested step/result shape as well: review reports
/// already written by this branch use it, while the acceptance contract
/// intentionally addresses the concise `.full_suite.tests[]` path.
fn full_suite_report(step: Option<&Step>, command: &str, retries: u64) -> Value {
    let mut report = match step {
        Some(step) => step.to_json(),
        None => json!({
            "outcome": "skipped",
            "reason": "--no-full",
            "cmd": command,
        }),
    };
    // Keep the acceptance path in seconds while retaining duration_ms for
    // existing report consumers and markdown rendering. A skipped suite has
    // no measurement, so its schema value is explicit null rather than an
    // invented duration.
    report["duration_s"] = step
        .map(|step| json!(step.duration_ms as f64 / 1000.0))
        .unwrap_or(Value::Null);
    report["retries"] = json!(retries);
    let structured = step
        .and_then(|step| step.result.as_ref())
        .map(TestRunSummary::to_json);
    report["tests"] = structured
        .as_ref()
        .map(|result| result["tests"].clone())
        .unwrap_or_else(|| json!([]));
    if let Some(structured) = structured {
        for key in [
            "valid",
            "test_count",
            "passed",
            "failed",
            "skipped",
            "failed_tests",
            "reason",
        ] {
            report[key] = structured[key].clone();
        }
    }
    report
}

/// Publish both names for the equal-conditions rows. `failures` is retained
/// for existing report consumers; `isolated` is the CAD-173 contract path.
fn set_failure_reports(report: &mut Value, comparisons: &[Value]) {
    report["failures"] = json!(comparisons);
    report["isolated"] = json!(comparisons);
}

// ---------------------------------------------------------------------------
// Pure helpers — unit-tested without IO
// ---------------------------------------------------------------------------

/// `*`/`**`/`?` glob match: `*` stays inside a path component, `**`
/// crosses directories, `?` is one non-`/` character.
pub fn glob_match(pattern: &str, path: &str) -> bool {
    glob_rec(pattern.as_bytes(), path.as_bytes())
}

fn glob_rec(p: &[u8], s: &[u8]) -> bool {
    if p.is_empty() {
        return s.is_empty();
    }
    match p[0] {
        b'*' if p.get(1) == Some(&b'*') => {
            // `**/`: also try matching zero directories.
            if p.get(2) == Some(&b'/') && glob_rec(&p[3..], s) {
                return true;
            }
            for i in 0..=s.len() {
                if glob_rec(&p[2..], &s[i..]) {
                    return true;
                }
            }
            false
        }
        b'*' => {
            for i in 0..=s.len() {
                if glob_rec(&p[1..], &s[i..]) {
                    return true;
                }
                if i < s.len() && s[i] == b'/' {
                    break;
                }
            }
            false
        }
        b'?' => !s.is_empty() && s[0] != b'/' && glob_rec(&p[1..], &s[1..]),
        c => !s.is_empty() && s[0] == c && glob_rec(&p[1..], &s[1..]),
    }
}

/// A `fn` item added under `test_globs` in the PR diff, with the added
/// lines that follow it (its body, for the stress-pattern match).
#[derive(Clone, Debug)]
pub struct NewTest {
    pub name: String,
    pub file: String,
    pub body: String,
}

/// Parse a unified diff (`git diff --unified=0 <merge-base> <head>`)
/// for `fn` items added under files matching `globs`, then keep those
/// whose name or added body contains any `patterns` substring (an
/// empty pattern list keeps every new test). A `fn` only counts as a
/// test when an added `#[test]`/`#[tokio::test]` attribute precedes it —
/// helpers like `fn review_fixture` are not tests and stressing them
/// would run zero tests.
pub fn parse_new_tests(diff: &str, globs: &[String], patterns: &[String]) -> Vec<NewTest> {
    let mut file = String::new();
    let mut file_ok = false;
    let mut tests: Vec<NewTest> = Vec::new();
    let mut cur: Option<usize> = None;
    // Contiguous added `#[...]` lines immediately before the next `fn`.
    let mut attrs: Vec<String> = Vec::new();
    for line in diff.lines() {
        if let Some(rest) = line.strip_prefix("+++ b/") {
            file = rest.trim().to_string();
            file_ok = globs.iter().any(|g| glob_match(g, &file));
            cur = None;
            attrs.clear();
            continue;
        }
        if line.starts_with("+++")
            || line.starts_with("diff ")
            || line.starts_with("index ")
            || line.starts_with("Binary")
        {
            // `+++ /dev/null` (deleted file) and every other header
            // clears the file match too.
            file.clear();
            file_ok = false;
            cur = None;
            attrs.clear();
            continue;
        }
        if !file_ok {
            continue;
        }
        if let Some(added) = line.strip_prefix('+') {
            if let Some(name) = added_fn_name(added) {
                if attrs.iter().any(|a| is_test_attr(a)) {
                    tests.push(NewTest {
                        name,
                        file: file.clone(),
                        body: String::new(),
                    });
                    cur = Some(tests.len() - 1);
                } else {
                    cur = None;
                }
                attrs.clear();
            } else {
                if let Some(i) = cur {
                    tests[i].body.push_str(added);
                    tests[i].body.push('\n');
                }
                let t = added.trim();
                if t.starts_with("#[") {
                    attrs.push(t.to_string());
                } else if !t.is_empty() {
                    attrs.clear();
                }
            }
        } else {
            // Context/removal/hunk boundary: the fn's added block ends.
            cur = None;
            attrs.clear();
        }
    }
    tests.retain(|t| {
        patterns.is_empty()
            || patterns.iter().any(|p| {
                !p.is_empty() && (t.name.contains(p.as_str()) || t.body.contains(p.as_str()))
            })
    });
    tests
}

/// `#[test]` or `#[tokio::test]` (with or without arguments).
fn is_test_attr(line: &str) -> bool {
    let t = line.trim();
    t == "#[test]" || t.starts_with("#[tokio::test")
}

/// `fn name(` out of an added diff line — accepts `fn`, `pub fn`,
/// `pub(crate) fn`, `async fn` and leading whitespace.
fn added_fn_name(added: &str) -> Option<String> {
    let t = added.trim_start();
    let t = t
        .strip_prefix("pub(crate) ")
        .or_else(|| t.strip_prefix("pub "))
        .unwrap_or(t);
    let t = t.strip_prefix("async ").unwrap_or(t);
    let rest = t.strip_prefix("fn ")?;
    let name: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

/// Cargo-style failure extraction: `test <name> ... FAILED` lines plus
/// the indented `failures:` summary block.
pub fn extract_failed_tests(output: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let mut in_block = false;
    for line in output.lines() {
        let t = line.trim();
        if t == "failures:" {
            in_block = true;
            continue;
        }
        if in_block {
            if t.is_empty() {
                continue;
            }
            if line.starts_with("    ") {
                names.push(t.to_string());
                continue;
            }
            in_block = false;
        }
        if let Some(rest) = t.strip_prefix("test ") {
            if let Some((name, _)) = rest.split_once(" ... FAILED") {
                names.push(name.trim().to_string());
            }
        }
    }
    names.sort();
    names.dedup();
    names
}

/// Added-line heuristic for a schema migration: `ALTER TABLE <name>`
/// or a bump of the store's `schema_version` table. Word-boundary aware
/// so code that merely mentions the phrases — like this detector —
/// doesn't trip it.
pub fn schema_migration_hit(diff: &str) -> Vec<String> {
    // Built at runtime so this file's own diff doesn't read as a hit.
    let sv_ops: Vec<String> = ["update", "into", "from", "exists"]
        .iter()
        .map(|op| format!("{op} schema_version"))
        .collect();
    let mut hits = Vec::new();
    for line in diff.lines() {
        let Some(added) = line.strip_prefix('+') else {
            continue;
        };
        if added.starts_with('+') {
            continue; // the +++ header line
        }
        let low = added.to_lowercase();
        if phrase_then_word(&low, "alter table")
            || set_version_bump(&low)
            || sv_ops.iter().any(|p| low.contains(p))
        {
            hits.push(added.trim().to_string());
        }
    }
    hits.sort();
    hits.dedup();
    hits
}

/// `needle` appears followed by whitespace then an identifier char.
fn phrase_then_word(s: &str, needle: &str) -> bool {
    s.match_indices(needle).any(|(i, _)| {
        s[i + needle.len()..]
            .trim_start()
            .chars()
            .next()
            .map(|c| c.is_ascii_alphanumeric() || c == '_')
            .unwrap_or(false)
    })
}

/// `set version` then `=` then a digit — the store's version bump idiom.
fn set_version_bump(s: &str) -> bool {
    s.match_indices("set version").any(|(i, _)| {
        s[i + "set version".len()..]
            .trim_start()
            .strip_prefix('=')
            .and_then(|r| r.trim_start().chars().next())
            .map(|c| c.is_ascii_digit())
            .unwrap_or(false)
    })
}

/// The isolated-test command for one test: `{test}` `{file}` `{target}`
/// substitution (`{target}` is the file stem — cargo's `--test <stem>`).
/// Every value is single-quoted: the names come from diff and test
/// output the PR controls, so the template must not pre-quote.
pub fn test_command(template: &str, test: &NewTest) -> String {
    let stem = Path::new(&test.file)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    template
        .replace("{test}", &sh_quote(&test.name))
        .replace("{file}", &sh_quote(&test.file))
        .replace("{target}", &sh_quote(&stem))
}

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// `^[A-Za-z0-9_:]+$` — cargo test names, nothing else. A name that
/// fails validation is reported `unknown` and never executed.
fn valid_test_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':')
}

/// A repo-relative path with no traversal and no shell metacharacters.
fn safe_rel_path(p: &str) -> bool {
    !p.is_empty()
        && !p.starts_with('/')
        && !p.split('/').any(|c| c == "..")
        && p.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-' | '/'))
}

/// `(passed, failed)` totals from `test result:` summary lines, or
/// `None` when the output isn't a cargo-style run at all.
fn test_totals(output: &str) -> Option<(u64, u64)> {
    let mut found = false;
    let (mut passed, mut failed) = (0u64, 0u64);
    for line in output.lines() {
        let Some(rest) = line.trim().strip_prefix("test result:") else {
            continue;
        };
        found = true;
        if let Some(n) = count_before(rest, "passed") {
            passed += n;
        }
        if let Some(n) = count_before(rest, "failed") {
            failed += n;
        }
    }
    found.then_some((passed, failed))
}

fn count_before(s: &str, unit: &str) -> Option<u64> {
    let idx = s.find(unit)?;
    s[..idx].trim_end().rsplit(' ').next()?.parse().ok()
}

fn structured_failed_tests(step: &Step) -> Vec<String> {
    step.result
        .as_ref()
        .filter(|r| r.valid)
        .map(|r| r.failed_tests.clone())
        .unwrap_or_default()
}

/// pass | fail | unknown — `unknown` means "could not run": timeout,
/// missing file, prepare failure, or a filter that matched zero tests.
/// A non-zero exit IS a test failure; the command ran.
fn classify_isolated(step: &Step) -> &'static str {
    match step.outcome {
        "ok" => match &step.result {
            Some(result)
                if !result.valid || result.test_count == 0 || result.executed_count() == 0 =>
            {
                "unknown"
            }
            Some(result) if result.failed > 0 => "fail",
            Some(_) => "pass",
            None => match test_totals(&step.output) {
                Some((0, 0)) => "unknown",
                _ => "pass",
            },
        },
        "fail" => {
            if step.result.as_ref().is_some_and(|result| {
                !result.valid || result.test_count == 0 || result.executed_count() == 0
            }) {
                "unknown"
            } else {
                "fail"
            }
        }
        _ => "unknown",
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// `favcrm/cadence` → `favcrm_cadence`; no remote → `repo-<sha12>`.
fn lock_slug(slug: &str, root: &Path) -> String {
    let clean: String = slug
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let clean = clean.trim_matches('_');
    if !clean.is_empty() {
        return clean.to_string();
    }
    use sha2::Digest;
    let h = sha2::Sha256::digest(root.to_string_lossy().as_bytes());
    format!("repo-{}", hex(&h[..6]))
}

/// `git grep -l 'fn <name>'` in the tree, filtered to `globs` — locates
/// a failing test that did not come from the PR's own new tests (a
/// pre-existing test the suite saw fail).
fn find_test_file(dir: &Path, name: &str, globs: &[String], git_secs: u64) -> Option<String> {
    let out = git_status(dir, &["grep", "-l", &format!("fn {name}")], git_secs).ok()?;
    out.stdout
        .lines()
        .map(str::trim)
        .filter(|f| !f.is_empty())
        .find(|f| globs.iter().any(|g| glob_match(g, f)))
        .map(str::to_string)
}

/// An `{unknown}` isolated result for the report — the test was never
/// executed for `reason`.
fn unknown_side(reason: &str) -> Value {
    json!({"outcome": "unknown", "reason": reason})
}

/// Preserve a failed full-suite observation when the backend did not emit a
/// testcase name. The row carries a null `test` field: a synthetic name would
/// turn missing evidence into a false attribution. `result` is the gated
/// isolated classification and remains `unknown`, so the verdict stays
/// blocking until a named testcase can be compared.
fn unknown_full_suite_row(step: &Step) -> Value {
    let reason = step
        .result
        .as_ref()
        .and_then(|result| result.reason.as_deref())
        .unwrap_or("full suite failed without a named testcase");
    let gated_reason = format!("full suite testcase evidence unavailable: {reason}");
    let base_reason = "full suite emitted no testcase name for base comparison";
    let gated = unknown_side(&gated_reason);
    let base = unknown_side(base_reason);
    json!({
        "source": "full_suite",
        "test": null,
        "in_run": step.outcome,
        "result": "unknown",
        "gated": gated.clone(),
        "base": base.clone(),
        "isolated_gated": gated,
        "isolated_base": base,
        "verdict": "inconclusive",
        "reason": reason,
    })
}

// ---------------------------------------------------------------------------
// Review worktree
// ---------------------------------------------------------------------------

/// Written into every worktree the review creates, checked before any
/// destructive call — the tool only ever mutates or removes a tree it
/// created in THIS run, so a crash-recovery path cannot destroy a
/// reviewer's own checkout that happens to sit at the same path.
const REVIEW_MARKER: &str = ".cadence-review-tree";

/// A detached git checkout under `<root>/.cadence/wt/` that this run
/// created — removed on drop unless `keep`.
struct ReviewTree {
    root: PathBuf,
    dir: PathBuf,
    keep: bool,
    git_secs: u64,
}

impl ReviewTree {
    /// `git worktree add --detach <dir> <sha>`; refuses when the path
    /// already exists (a `--keep` leftover counts — the operator
    /// clears it), then writes the ownership marker.
    fn checkout(root: &Path, name: &str, sha: &str, keep: bool, git_secs: u64) -> Result<Self> {
        let dir = root.join(".cadence").join("wt").join(name);
        if dir.exists() {
            return Err(Error::rejected(format!(
                "review checkout {} already exists — refusing to touch a \
                 tree the review did not create; inspect it, then clear \
                 it with `git worktree remove --force {}` or delete the \
                 directory",
                dir.display(),
                dir.display()
            )));
        }
        git(
            root,
            &["worktree", "add", "--detach", &dir.to_string_lossy(), sha],
            git_secs,
        )?;
        if let Err(e) = std::fs::write(
            dir.join(REVIEW_MARKER),
            format!(
                "created by `cadence review` at {} — this file marks the \
                 tree as tool-owned\n",
                time::iso(time::now_epoch())
            ),
        ) {
            // No marker ⇒ not ours: deregister rather than leave a
            // tree the drop guard would refuse to touch.
            let mut rm = Command::new("git");
            rm.arg("-C")
                .arg(root)
                .args(["worktree", "remove", "--force"])
                .arg(&dir);
            let _ = run_bounded(&mut rm, Duration::from_secs(git_secs));
            return Err(e.into());
        }
        Ok(Self {
            root: root.to_path_buf(),
            dir,
            keep,
            git_secs,
        })
    }

    /// Marker present ⇒ this run created the tree and may destroy it.
    fn owns(&self) -> bool {
        self.dir.join(REVIEW_MARKER).is_file()
    }
}

impl Drop for ReviewTree {
    fn drop(&mut self) {
        if self.keep || !self.owns() {
            return;
        }
        let mut cmd = Command::new("git");
        cmd.arg("-C")
            .arg(&self.root)
            .args(["worktree", "remove", "--force"])
            .arg(&self.dir);
        let _ = run_bounded(&mut cmd, Duration::from_secs(self.git_secs));
        if self.dir.exists() {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

// ---------------------------------------------------------------------------
// The pipeline
// ---------------------------------------------------------------------------

/// `gh pr view` → the bits the review needs.
struct PrInfo {
    number: i64,
    title: String,
    url: String,
    head_sha: String,
    head_ref: String,
    base_ref: String,
    /// `gh` lifecycle state (OPEN / MERGED / CLOSED) — a review on a
    /// merged or closed PR is informational only.
    state: String,
    files: Vec<String>,
}

fn gh_pr_view(cwd: &Path, slug: &str, pr: &str, secs: u64) -> Result<PrInfo> {
    let out = gh(
        cwd,
        &[
            "pr".into(),
            "view".into(),
            pr.into(),
            "--repo".into(),
            slug.into(),
            "--json".into(),
            "number,title,url,headRefName,headRefOid,baseRefName,files,state".into(),
        ],
        secs,
    )?;
    let v: Value = serde_json::from_str(&out)
        .map_err(|e| Error::internal(format!("gh pr view {pr}: bad JSON: {e}")))?;
    let files = v["files"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|f| f["path"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    Ok(PrInfo {
        number: v["number"].as_i64().unwrap_or(0),
        title: v["title"].as_str().unwrap_or("").to_string(),
        url: v["url"].as_str().unwrap_or("").to_string(),
        head_sha: v["headRefOid"].as_str().unwrap_or("").to_string(),
        head_ref: v["headRefName"].as_str().unwrap_or("").to_string(),
        base_ref: v["baseRefName"].as_str().unwrap_or("").to_string(),
        state: v["state"].as_str().unwrap_or("").to_string(),
        files,
    })
}

pub struct Options {
    pub pr: String,
    pub repo: Option<String>,
    /// Run the full suite (`--no-full` clears this).
    pub full: bool,
    /// Allow the full suite without `CADENCE_SUITE_LOCK` set.
    pub no_suite_lock: bool,
    /// Stress repetitions per matched new test [default 5].
    pub stress: u32,
    /// Keep the review worktree(s) for inspection.
    pub keep: bool,
    /// Print the JSON report on stdout.
    pub json: bool,
    /// The directory the command was invoked from.
    pub cwd: PathBuf,
    /// Resolved state dir (`--state-dir` or `client::state_dir`).
    pub state_dir: PathBuf,
}

/// Run the review; returns the process exit code (0 = a report was
/// written, whatever the verdict).
pub fn run(opts: &Options) -> Result<i32> {
    let started = Instant::now();
    // Concurrent ten-minute suites on one host starve each other into
    // load flakes — the full run takes the host-wide slot or says why not.
    let suite_lock_path = std::env::var("CADENCE_SUITE_LOCK")
        .ok()
        .filter(|p| !p.is_empty());
    if opts.full && suite_lock_path.is_none() && !opts.no_suite_lock {
        return Err(Error::rejected(
            "CADENCE_SUITE_LOCK is unset — the full suite would run beside \
             every other suite on this host. Set it (see docs/SESSION.md: \
             export CADENCE_SUITE_LOCK=~/.local/state/cadence/suite.lock), \
             or pass --no-full or --no-suite-lock",
        ));
    }
    let root = worktree::main_root(&opts.cwd)?;
    // The config comes from the base head, which is only known once the
    // PR is resolved — every call up to there runs on default timeouts.
    let pre = Timeouts::default();

    // Repo slug: --repo wins, else the origin remote's `gh repo view`.
    let slug = match &opts.repo {
        Some(r) => r.clone(),
        None => gh(
            &root,
            &[
                "repo".into(),
                "view".into(),
                "--json".into(),
                "nameWithOwner".into(),
                "--jq".into(),
                ".nameWithOwner".into(),
            ],
            pre.gh_secs,
        )?,
    };

    // One review at a time per repo, host-wide.
    let reviews_dir = opts.state_dir.join("reviews");
    std::fs::create_dir_all(&reviews_dir)?;
    let lock_path = reviews_dir.join(format!("{}.review.lock", lock_slug(&slug, &root)));
    let _review_lock = Flock::try_lock(&lock_path)?.ok_or_else(|| {
        Error::rejected(format!(
            "another `cadence review` is already running for {slug} \
             (lock: {})",
            lock_path.display()
        ))
    })?;

    let pr = gh_pr_view(&root, &slug, &opts.pr, pre.gh_secs)?;
    if pr.head_sha.is_empty() || pr.base_ref.is_empty() {
        return Err(Error::rejected(format!(
            "gh pr view {} returned no head/base — is it an open PR?",
            opts.pr
        )));
    }

    // Fetch both ends into the main object store, each into this run's
    // own ref — never through the shared FETCH_HEAD (CAD-261).
    let refs = ReviewRefs::new(&root, pr.number, pre.git_secs);
    let base_sha = fetch_ref(
        &root,
        &format!("refs/heads/{}", pr.base_ref),
        &refs.name("base"),
        pre.git_secs,
    )?;
    let head_sha = fetch_ref(
        &root,
        &format!("pull/{}/head", pr.number),
        &refs.name("head"),
        pre.git_secs,
    )?;
    if head_sha != pr.head_sha {
        return Err(Error::rejected(format!(
            "PR #{} head moved while resolving: gh saw {}, fetch got {} \
             — rerun `cadence review`",
            pr.number, pr.head_sha, head_sha
        )));
    }
    let merge_base = git(&root, &["merge-base", &base_sha, &head_sha], pre.git_secs)?;
    let base_moved = merge_base != base_sha;

    // Gates, full_suite and test_command come from the base head; a PR
    // that edits the file is gated by the base copy and flagged.
    let cfg = config_at_base(&root, &pr.base_ref, &base_sha, pre.git_secs)?;
    let t = &cfg.timeouts;
    let config_changed = config_changed_by_pr(&root, &merge_base, &head_sha, t.git_secs)?;

    // The review checkout — detached, never the author's worktree.
    let wt_name = format!("review-{}", pr.number);
    let mut tree = ReviewTree::checkout(&root, &wt_name, &head_sha, opts.keep, t.git_secs)?;

    // Env every step command sees: the review's own variables plus
    // CI's identity-less git (`gate_env`) under a scratch HOME that
    // lives exactly as long as this run.
    let gate_home = tempfile::Builder::new()
        .prefix("cadence-review-home-")
        .tempdir()?;
    let gate_env = gate_env(gate_home.path(), |k| std::env::var(k).ok());
    let env = |tree_kind: &str| -> Vec<(String, String)> {
        let mut env = vec![
            ("CADENCE_REVIEW_PR".into(), pr.number.to_string()),
            ("CADENCE_REVIEW_HEAD".into(), head_sha.clone()),
            ("CADENCE_REVIEW_BASE".into(), base_sha.clone()),
            ("CADENCE_REVIEW_MERGE_BASE".into(), merge_base.clone()),
            ("CADENCE_REVIEW_TREE".into(), tree_kind.into()),
            (
                "CADENCE_REVIEW_ROOT".into(),
                root.to_string_lossy().into_owned(),
            ),
        ];
        env.extend(gate_env.iter().cloned());
        env
    };

    // When the base moved, gate the merge result instead of the bare
    // PR head: detach at the base head and `git merge --no-commit`.
    let mut gated_tree = "pr-head";
    let mut merge = json!({"attempted": false});
    if base_moved {
        git(&tree.dir, &["checkout", "--detach", &base_sha], t.git_secs)?;
        let m = git_status(
            &tree.dir,
            &["merge", "--no-commit", "--no-ff", &head_sha],
            t.git_secs,
        )?;
        if m.status == Some(0) {
            gated_tree = "merge-result";
            merge = json!({"attempted": true, "result": "clean"});
        } else {
            let conflicts = git(
                &tree.dir,
                &["diff", "--name-only", "--diff-filter=U"],
                t.git_secs,
            )?;
            let files: Vec<String> = conflicts
                .lines()
                .map(|s| s.to_string())
                .filter(|s| !s.is_empty())
                .collect();
            let _ = git_status(&tree.dir, &["merge", "--abort"], t.git_secs);
            git(&tree.dir, &["checkout", "--detach", &head_sha], t.git_secs)?;
            git(&tree.dir, &["reset", "--hard", &head_sha], t.git_secs)?;
            merge = json!({"attempted": true, "result": "conflict",
                "conflict_files": files});
        }
    }

    let diff = git(
        &root,
        &["diff", "--unified=0", &merge_base, &head_sha],
        t.git_secs,
    )?;
    let schema_hits = schema_migration_hit(&diff);
    let new_tests = parse_new_tests(&diff, &cfg.test_globs, &cfg.stress_pattern.0);

    let mut report = json!({
        "pr": pr.number,
        "title": pr.title,
        "url": pr.url,
        "state": pr.state,
        "repo": slug,
        "head": head_sha,
        "head_ref": pr.head_ref,
        "base": {"ref": pr.base_ref, "sha": base_sha,
                 "moved_since_merge_base": base_moved},
        "merge_base": merge_base,
        "changed_files": pr.files,
        "config": {"source": "base", "base_sha": base_sha,
                   "changed_by_pr": config_changed},
        "gated_tree": gated_tree,
        "merge": merge,
        "worktree": tree.dir.to_string_lossy(),
        "no_full": !opts.full,
        "runner": {
            "backend": backend_label(cfg.runner.backend),
            "result_format": result_format_label(cfg.runner.result_format),
            "result_path": cfg.runner.result_path,
            "retries": 0,
        },
        "schema_migration": !schema_hits.is_empty(),
        "schema_hits": schema_hits,
        "started_at": time::iso(time::now_epoch()),
    });

    // Prepare → gates → stress → suite. A prepare failure stops the
    // pipeline: nothing after it can run.
    let mut prepare_steps = Vec::new();
    let mut prepare_failed = false;
    for cmd in &cfg.prepare {
        let s = run_step("prepare", cmd, &tree.dir, &env(gated_tree), t.prepare_secs)?;
        prepare_failed |= s.outcome != "ok";
        prepare_steps.push(s);
        if prepare_failed {
            break;
        }
    }
    report["prepare"] = json!(prepare_steps.iter().map(Step::to_json).collect::<Vec<_>>());

    let mut gate_steps: Vec<Step> = Vec::new();
    let mut stress_results: Vec<Value> = Vec::new();
    let mut suite_step: Option<Step> = None;
    let mut suite_lock = Value::Null;
    let gated_result_path = runner_result_path(&tree.dir, &cfg.runner);

    if !prepare_failed {
        let mut gates_failed = false;
        for (i, cmd) in cfg.gates.iter().enumerate() {
            if gates_failed {
                gate_steps.push(Step {
                    name: format!("gate-{}", i + 1),
                    cmd: cmd.clone(),
                    duration_ms: 0,
                    outcome: "skipped",
                    exit: None,
                    tail: Vec::new(),
                    output: String::new(),
                    result: None,
                });
                continue;
            }
            let s = run_step(
                &format!("gate-{}", i + 1),
                cmd,
                &tree.dir,
                &env(gated_tree),
                t.gate_secs,
            )?;
            gates_failed |= s.outcome != "ok";
            gate_steps.push(s);
        }

        // Stress each matched new test N times in isolation.
        for nt in &new_tests {
            let cmd = test_command(&cfg.test_command, nt);
            let mut failures = 0u32;
            let mut runs = Vec::new();
            let mut unknown = 0u32;
            for i in 0..opts.stress {
                let s = run_step_with_result(
                    &format!("stress-{}-{}", nt.name, i + 1),
                    &cmd,
                    &tree.dir,
                    &env(gated_tree),
                    t.stress_secs,
                    gated_result_path.as_deref(),
                )?;
                // A successful process with no structured testcase means the
                // filter missed (or the report disappeared) — unknown, not
                // an okay stress run.
                let outcome = classify_isolated(&s);
                match outcome {
                    "unknown" => unknown += 1,
                    "fail" => failures += 1,
                    _ => {}
                }
                runs.push(json!({"run": i + 1, "outcome": outcome,
                    "duration_ms": s.duration_ms,
                    "tail": s.tail}));
            }
            stress_results.push(json!({
                "test": nt.name, "file": nt.file, "cmd": cmd,
                "runs": opts.stress, "failures": failures,
                "unknown": unknown,
                "detail": runs,
            }));
        }

        // The full suite, once — optionally serialized host-wide.
        if opts.full {
            let mut _suite_guard = None;
            if let Some(path) = &suite_lock_path {
                let path = PathBuf::from(path);
                let wait = Instant::now();
                _suite_guard = Some(Flock::lock_deadline(&path, t.full_secs)?);
                suite_lock = json!({"path": path,
                    "waited_ms": wait.elapsed().as_millis(),
                    "ownership": "outer-review"});
            }
            report["host_load"] = host_load();
            // The slot is already held here: the suite's own harness prelude
            // must not queue behind its parent, so it sees the variable empty.
            // The explicit marker is consumed by the pinned nextest wrapper;
            // it prevents a nested flock while preserving a fail-closed
            // direct invocation.
            let suite_env = suite_child_env(env(gated_tree), _suite_guard.is_some());
            suite_step = Some(run_step_with_result(
                "full-suite",
                &cfg.full_suite,
                &tree.dir,
                &suite_env,
                t.full_secs,
                gated_result_path.as_deref(),
            )?);
        }
    }
    report["gates"] = json!(gate_steps.iter().map(Step::to_json).collect::<Vec<_>>());
    report["stress"] = json!(stress_results);
    report["suite_lock"] = suite_lock;
    report["full_suite"] = full_suite_report(suite_step.as_ref(), &cfg.full_suite, 0);

    // Equal-conditions compare: every failing test name, rerun alone
    // on the gated tree and alone on the base head.
    let mut failures_to_check: Vec<String> = Vec::new();
    for s in gate_steps.iter().chain(suite_step.iter()) {
        if s.outcome == "ok" || s.outcome == "skipped" {
            continue;
        }
        failures_to_check.extend(if s.result.is_some() {
            structured_failed_tests(s)
        } else {
            extract_failed_tests(&s.output)
        });
    }
    failures_to_check.extend(
        stress_results
            .iter()
            .filter(|r| r["failures"].as_u64().unwrap_or(0) > 0)
            .filter_map(|r| r["test"].as_str().map(str::to_string)),
    );
    failures_to_check.sort();
    failures_to_check.dedup();

    let mut comparisons = Vec::new();
    let mut base_prepare_steps: Vec<Step> = Vec::new();
    if !failures_to_check.is_empty() {
        // Base-head checkout, prepared like the gated tree. A prepare
        // failure here is its own recorded step — it must not quietly
        // turn every base run into the same error.
        let base_name = format!("review-{}-base", pr.number);
        let base_tree = ReviewTree::checkout(&root, &base_name, &base_sha, opts.keep, t.git_secs)?;
        let mut base_ready = true;
        for cmd in &cfg.prepare {
            let s = run_step(
                "prepare-base",
                cmd,
                &base_tree.dir,
                &env("base"),
                t.prepare_secs,
            )?;
            base_ready &= s.outcome == "ok";
            base_prepare_steps.push(s);
            if !base_ready {
                break;
            }
        }
        let base_result_path = runner_result_path(&base_tree.dir, &cfg.runner);
        for name in &failures_to_check {
            // Names come from test output the PR controls — validate
            // before they reach a shell, and never execute a bad one.
            if !valid_test_name(name) {
                comparisons.push(json!({
                    "test": name, "in_run": "fail",
                    "result": "unknown",
                    "gated": unknown_side("test name failed validation"),
                    "base": unknown_side("test name failed validation"),
                    "isolated_gated": unknown_side("test name failed validation"),
                    "isolated_base": unknown_side("test name failed validation"),
                    "verdict": "inconclusive",
                }));
                continue;
            }
            let file = new_tests
                .iter()
                .find(|t| &t.name == name)
                .map(|t| t.file.clone())
                .or_else(|| find_test_file(&tree.dir, name, &cfg.test_globs, t.git_secs));
            let Some(file) = file.filter(|f| safe_rel_path(f)) else {
                comparisons.push(json!({
                    "test": name, "in_run": "fail",
                    "result": "unknown",
                    "gated": unknown_side("test file not found under test_globs"),
                    "base": unknown_side("test file not found under test_globs"),
                    "isolated_gated": unknown_side("test file not found under test_globs"),
                    "isolated_base": unknown_side("test file not found under test_globs"),
                    "verdict": "inconclusive",
                }));
                continue;
            };
            let nt = NewTest {
                name: name.clone(),
                file,
                body: String::new(),
            };
            let cmd = test_command(&cfg.test_command, &nt);
            let on_gated = run_step_with_result(
                "compare-gated",
                &cmd,
                &tree.dir,
                &env(gated_tree),
                t.test_secs,
                gated_result_path.as_deref(),
            )?;
            let gated = classify_isolated(&on_gated);
            let (base, base_tail, base_reason) = if !base_ready {
                ("unknown", Vec::new(), Some("base prepare failed"))
            } else {
                let on_base = run_step_with_result(
                    "compare-base",
                    &cmd,
                    &base_tree.dir,
                    &env("base"),
                    t.test_secs,
                    base_result_path.as_deref(),
                )?;
                let c = classify_isolated(&on_base);
                (
                    c,
                    on_base.tail.clone(),
                    if c == "unknown" {
                        Some("could not run on the base tree")
                    } else {
                        None
                    },
                )
            };
            let gated_reason = if gated == "unknown" {
                Some("could not run on the gated tree")
            } else {
                None
            };
            let verdict = match (gated, base) {
                ("fail", "pass") => "regression",
                ("fail", "fail") => "pre-existing",
                ("unknown", _) | (_, "unknown") => "inconclusive",
                _ => "flake-under-load",
            };
            // Passing alone on both trees is a flake sighting: record it
            // once, with evidence, so the next review reads the count
            // instead of re-diagnosing by hand.
            let mut sighting = Value::Null;
            if (gated, base) == ("pass", "pass") {
                let entry = json!({
                    "at": time::iso(time::now_epoch()),
                    "repo": slug,
                    "test": name,
                    "pr": pr.number,
                    "head": head_sha,
                    "base": base_sha,
                    "panic_head": suite_step
                        .as_ref()
                        .map(|s| panic_head(&s.output, name))
                        .unwrap_or_default(),
                    "host_load": report["host_load"].clone(),
                });
                let seen = record_flake(&reviews_dir.join(FLAKE_LEDGER), &entry)?;
                sighting = json!({"sightings": seen.total, "heads": seen.heads,
                    "known_flake": seen.known_flake()});
            }
            let mut gated_side = json!({"outcome": gated, "tail": on_gated.tail});
            if let Some(r) = gated_reason {
                gated_side["reason"] = json!(r);
            }
            let mut base_side = json!({"outcome": base, "tail": base_tail});
            if let Some(r) = base_reason {
                base_side["reason"] = json!(r);
            }
            let mut row = json!({
                "test": name, "cmd": cmd,
                "in_run": "fail",
                "result": gated,
                "gated": gated_side.clone(),
                "base": base_side.clone(),
                "isolated_gated": gated_side,
                "isolated_base": base_side,
                "verdict": verdict,
            });
            if !sighting.is_null() {
                row["flake"] = sighting;
            }
            comparisons.push(row);
        }
        drop(base_tree);
    }
    // A failed full suite without a named testcase is still evidence that
    // blocks the review. Keep one explicit unknown row so the report cannot
    // look like a successful empty comparison set; never invent a testcase
    // name to satisfy the shape.
    let suite_without_named_failures = suite_step.as_ref().is_some_and(|step| {
        matches!(step.outcome, "fail" | "timeout")
            && if step.result.is_some() {
                structured_failed_tests(step).is_empty()
            } else {
                extract_failed_tests(&step.output).is_empty()
            }
    });
    if suite_without_named_failures {
        if let Some(step) = suite_step.as_ref() {
            comparisons.push(unknown_full_suite_row(step));
        }
    }
    set_failure_reports(&mut report, &comparisons);
    report["base_prepare"] = json!(base_prepare_steps
        .iter()
        .map(Step::to_json)
        .collect::<Vec<_>>());

    // Pairwise conflicts with the other open PRs (files only), compared
    // as both would land on the current base: each PR is merged onto the
    // base first, then the two results are merged with the base as their
    // merge-base. A head-vs-head merge-tree instead "conflicted" on what
    // the base changed between two PRs' cut points (CAD-277). A PR that
    // does not merge into the base cannot be compared that way; it is
    // listed as not assessed, never counted as a conflict or dropped.
    let mut pr_conflicts = Vec::new();
    let mut not_assessed = Vec::new();
    report["open_pr_conflicts_base"] = json!(base_sha);
    let mine = landed(&root, &base_sha, &head_sha, t.git_secs);
    let mine_changed = changed_paths(&root, &base_sha, &head_sha, t.git_secs);
    let open = gh(
        &root,
        &[
            "pr".into(),
            "list".into(),
            "--repo".into(),
            slug.clone(),
            "--state".into(),
            "open".into(),
            "--json".into(),
            "number,title,headRefOid".into(),
            "--limit".into(),
            "100".into(),
        ],
        t.gh_secs,
    )
    .and_then(|s| {
        serde_json::from_str::<Value>(&s)
            .map_err(|e| Error::internal(format!("gh pr list: bad JSON: {e}")))
    });
    match open {
        Ok(list) => {
            for other in list.as_array().cloned().unwrap_or_default() {
                let num = other["number"].as_i64().unwrap_or(0);
                if num == pr.number {
                    continue;
                }
                let fetch = fetch_ref(
                    &root,
                    &format!("pull/{num}/head"),
                    &refs.name(&format!("other/{num}")),
                    t.git_secs,
                );
                let Ok(theirs) = fetch else {
                    pr_conflicts.push(json!({"pr": num,
                        "title": other["title"],
                        "error": "could not fetch head"}));
                    continue;
                };
                let pair = mine.as_ref().map_err(|e| e.to_string()).and_then(|m| {
                    landed(&root, &base_sha, &theirs, t.git_secs)
                        .map(|t| (m.clone(), t))
                        .map_err(|e| e.to_string())
                });
                let unassessed = |reason: &str| {
                    let mut entry = json!({"pr": num, "title": other["title"],
                        "reason": reason});
                    // Advisory only: the files both PRs change themselves.
                    // A shared file is not a conflict and a disjoint pair
                    // is not proof of none — but it keeps a stale PR that
                    // edits the same files from being invisible (CAD-297).
                    let theirs_changed = changed_paths(&root, &base_sha, &theirs, t.git_secs);
                    match (&mine_changed, &theirs_changed) {
                        (Ok(m), Ok(th)) => {
                            let both: Vec<&String> = m.intersection(th).collect();
                            if !both.is_empty() {
                                entry["advisory_overlap"] = json!(both);
                            }
                        }
                        (Err(e), _) | (_, Err(e)) => {
                            entry["advisory_overlap_error"] = json!(e.to_string());
                        }
                    }
                    entry
                };
                let (mine_landed, theirs_landed) = match pair {
                    Ok((Some(m), Some(t))) => (m, t),
                    Ok((None, _)) => {
                        // This PR itself does not merge into the base —
                        // already a blocking reason; no pair can be judged.
                        not_assessed.push(unassessed(NOT_ASSESSED_THIS_PR));
                        continue;
                    }
                    Ok((_, None)) => {
                        not_assessed.push(unassessed(NOT_ASSESSED_OTHER_PR));
                        continue;
                    }
                    Err(e) => {
                        pr_conflicts.push(json!({"pr": num,
                            "title": other["title"],
                            "error": format!("as-landed scan failed: {e}")}));
                        continue;
                    }
                };
                let merge_base = format!("--merge-base={base_sha}");
                let mt = git_status(
                    &root,
                    &[
                        "merge-tree",
                        "--write-tree",
                        "--name-only",
                        &merge_base,
                        &mine_landed,
                        &theirs_landed,
                    ],
                    t.git_secs,
                )?;
                if mt.status == Some(0) {
                    continue;
                }
                // --name-only output: tree OID, conflicted names, blank
                // line, then the conflict messages. Only exit 1 with a
                // file list is a conflict — any other non-zero is a
                // scan error, surfaced instead of dropped.
                let files: Vec<String> = mt
                    .stdout
                    .lines()
                    .skip(1)
                    .take_while(|l| !l.trim().is_empty())
                    .map(|s| s.to_string())
                    .collect();
                if mt.status == Some(1) && !files.is_empty() {
                    pr_conflicts.push(json!({"pr": num, "title": other["title"],
                        "files": files}));
                } else {
                    let why = if mt.timed_out {
                        "merge-tree timed out".to_string()
                    } else {
                        format!("merge-tree exited {:?}: {}", mt.status, mt.stderr.trim())
                    };
                    pr_conflicts.push(json!({"pr": num,
                        "title": other["title"],
                        "error": why}));
                }
            }
        }
        Err(e) => {
            report["open_pr_conflicts_error"] = json!(e.to_string());
        }
    }
    report["open_pr_conflicts"] = json!(pr_conflicts);
    report["open_pr_not_assessed"] = json!(not_assessed);

    // Suggested verdict — the reviewer still does the hands-on check;
    // `pass` only means the mechanical part found nothing.
    let (verdict, reasons) = suggest(&report, prepare_failed);
    report["suggested_verdict"] = json!(verdict);
    report["verdict_reasons"] = json!(reasons);
    report["duration_ms"] = json!(started.elapsed().as_millis());

    // Reports under the state dir — never inside the repo.
    let stamp = time::basic(time::now_epoch());
    let base_name = format!(
        "review-{}-pr{}-{}",
        lock_slug(&slug, &root),
        pr.number,
        stamp
    );
    let md_path = reviews_dir.join(format!("{base_name}.md"));
    let json_path = reviews_dir.join(format!("{base_name}.json"));
    report["report_md"] = json!(md_path.to_string_lossy());
    std::fs::write(&md_path, render_markdown(&report))?;
    std::fs::write(&json_path, serde_json::to_string_pretty(&report)?)?;

    if opts.keep {
        tree.keep = true;
    }

    if opts.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("report: {}", md_path.display());
        println!("verdict: {verdict} — {}", reasons.join("; "));
    }
    Ok(match verdict {
        "pass" => 0,
        "needs-hands-on" => 1,
        _ => 2,
    })
}

// ---------------------------------------------------------------------------
// Verdict
// ---------------------------------------------------------------------------

/// `open_pr_not_assessed` reasons. Only the second one is a verdict
/// reason of its own: the first means this PR does not merge, which the
/// merge-conflict reason already blocks on.
const NOT_ASSESSED_THIS_PR: &str = "this PR does not merge into the current base";
const NOT_ASSESSED_OTHER_PR: &str = "it does not merge into the current base";

fn push_reason(level: &mut u8, reasons: &mut Vec<String>, l: u8, r: String) {
    *level = (*level).max(l);
    reasons.push(r);
}

/// The wrapper's own refusal line (`cadence-nextest: …`, its `die`
/// prefix) when the full suite exited before running anything.
fn runner_refusal(suite: &Value) -> Option<String> {
    suite["tail"]
        .as_array()?
        .iter()
        .filter_map(Value::as_str)
        .map(str::trim)
        .rfind(|l| !l.is_empty())
        .filter(|l| l.starts_with("cadence-nextest: "))
        .map(str::to_string)
}

/// A not-assessed entry's advisory overlap in words, `None` when the two
/// PRs share no changed file (CAD-297).
fn advisory_note(c: &Value) -> Option<String> {
    if let Some(files) = c["advisory_overlap"].as_array() {
        Some(format!(
            "advisory, not a conflict: both PRs change {}",
            files
                .iter()
                .filter_map(|f| f.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))
    } else {
        c["advisory_overlap_error"]
            .as_str()
            .map(|e| format!("advisory overlap unavailable: {e}"))
    }
}

/// `pass|needs-hands-on|blocked` with reasons — computed from the
/// report so the logic is unit-testable.
pub fn suggest(report: &Value, prepare_failed: bool) -> (&'static str, Vec<String>) {
    let mut level = 0u8; // 0 pass, 1 needs-hands-on, 2 blocked
    let mut reasons: Vec<String> = Vec::new();

    if let Some(state) = report["state"].as_str() {
        if !state.eq_ignore_ascii_case("open") {
            push_reason(
                &mut level,
                &mut reasons,
                1,
                format!("pr state is {state} — results are informational only"),
            );
        }
    }
    if prepare_failed {
        push_reason(
            &mut level,
            &mut reasons,
            2,
            "prepare step failed — the tree never built".into(),
        );
    }
    if report["merge"]["result"].as_str() == Some("conflict") {
        let files = report["merge"]["conflict_files"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|f| f.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        push_reason(
            &mut level,
            &mut reasons,
            2,
            format!("does not merge into the current base — {files}"),
        );
    }
    for s in report["gates"].as_array().cloned().unwrap_or_default() {
        if matches!(s["outcome"].as_str(), Some("fail") | Some("timeout")) {
            push_reason(
                &mut level,
                &mut reasons,
                2,
                format!(
                    "gate `{}` {}",
                    s["cmd"].as_str().unwrap_or(""),
                    s["outcome"].as_str().unwrap_or("")
                ),
            );
        }
    }
    let suite_failed = matches!(
        report["full_suite"]["outcome"].as_str(),
        Some("fail") | Some("timeout")
    );
    // The pinned runner's wrapper refused before any test ran (missing or
    // untrusted binary, a policy override): one named reason, not a
    // failed suite plus stress runs that "matched nothing" (CAD-273).
    let runner_refusal = suite_failed
        .then(|| runner_refusal(&report["full_suite"]))
        .flatten();
    if let Some(line) = &runner_refusal {
        push_reason(
            &mut level,
            &mut reasons,
            2,
            format!(
                "test runner refused to start, so no suite or stress test ran — {line} \
                 (install with scripts/install-cadence-nextest or set CADENCE_NEXTTEST_BIN)"
            ),
        );
    }
    let comparisons = report["failures"].as_array().cloned().unwrap_or_default();
    for f in &comparisons {
        if runner_refusal.is_some() && f["source"] == "full_suite" {
            continue;
        }
        if f["source"] == "full_suite" && f["result"] == "unknown" {
            push_reason(
                &mut level,
                &mut reasons,
                2,
                format!(
                    "full suite produced no executable testcase evidence — {}",
                    f["reason"].as_str().unwrap_or("reason unavailable")
                ),
            );
            continue;
        }
        let name = f["test"].as_str().unwrap_or("?");
        match f["verdict"].as_str() {
            Some("regression") => push_reason(
                &mut level,
                &mut reasons,
                2,
                format!(
                    "`{name}` fails alone on the gated tree but passes on the base — regression"
                ),
            ),
            Some("pre-existing") => push_reason(
                &mut level,
                &mut reasons,
                1,
                format!("`{name}` fails on the base too — pre-existing, not this PR"),
            ),
            // A ledger-known flake is listed, not blocking.
            Some("flake-under-load") if f["flake"]["known_flake"] == true => push_reason(
                &mut level,
                &mut reasons,
                0,
                format!(
                    "`{name}` is a known flake ({} sightings on {} heads) — not blocking",
                    f["flake"]["sightings"], f["flake"]["heads"]
                ),
            ),
            Some("flake-under-load") => push_reason(
                &mut level,
                &mut reasons,
                1,
                format!("`{name}` only fails under the parallel run — flake"),
            ),
            Some("inconclusive") => push_reason(
                &mut level,
                &mut reasons,
                2,
                format!("`{name}` could not be rerun cleanly — inconclusive"),
            ),
            _ => {}
        }
    }
    for s in report["base_prepare"]
        .as_array()
        .cloned()
        .unwrap_or_default()
    {
        if s["outcome"].as_str() != Some("ok") {
            push_reason(
                &mut level,
                &mut reasons,
                2,
                "base-tree prepare failed — base-side comparison degraded".into(),
            );
            break;
        }
    }
    if suite_failed && comparisons.is_empty() && runner_refusal.is_none() {
        push_reason(
            &mut level,
            &mut reasons,
            2,
            "full suite failed with no test name to compare — see the tail".into(),
        );
    }
    for s in report["stress"].as_array().cloned().unwrap_or_default() {
        if s["failures"].as_u64().unwrap_or(0) > 0 {
            push_reason(
                &mut level,
                &mut reasons,
                1,
                format!(
                    "`{}` failed {}/{} isolated stress runs",
                    s["test"].as_str().unwrap_or("?"),
                    s["failures"].as_u64().unwrap_or(0),
                    s["runs"].as_u64().unwrap_or(0)
                ),
            );
        }
        if s["unknown"].as_u64().unwrap_or(0) > 0 && runner_refusal.is_none() {
            push_reason(
                &mut level,
                &mut reasons,
                1,
                format!(
                    "`{}` stress ran zero tests {}/{} times — the filter matched nothing",
                    s["test"].as_str().unwrap_or("?"),
                    s["unknown"].as_u64().unwrap_or(0),
                    s["runs"].as_u64().unwrap_or(0)
                ),
            );
        }
    }
    // Risk class 7: a PR that rewrites its own gates never passes on
    // the mechanical check alone, even though the base config gated it.
    if report["config"]["changed_by_pr"].as_bool().unwrap_or(false) {
        push_reason(
            &mut level,
            &mut reasons,
            1,
            format!(
                "PR changes {CONFIG_FILE} — gated with the base head's config; \
                 risk class 7 needs operator review"
            ),
        );
    }
    if report["schema_migration"].as_bool().unwrap_or(false) {
        push_reason(
            &mut level,
            &mut reasons,
            1,
            "schema-migration heuristic hit — rehearse the migration by hand".into(),
        );
    }
    for c in report["open_pr_conflicts"]
        .as_array()
        .cloned()
        .unwrap_or_default()
    {
        if c["files"].is_array() {
            push_reason(
                &mut level,
                &mut reasons,
                1,
                format!(
                    "overlaps open PR #{} ({})",
                    c["pr"].as_i64().unwrap_or(0),
                    c["files"]
                        .as_array()
                        .map(|a| a
                            .iter()
                            .filter_map(|f| f.as_str())
                            .collect::<Vec<_>>()
                            .join(", "))
                        .unwrap_or_default()
                ),
            );
        } else if c["error"].is_string() {
            push_reason(
                &mut level,
                &mut reasons,
                1,
                format!(
                    "conflict scan incomplete for #{} ({})",
                    c["pr"].as_i64().unwrap_or(0),
                    c["error"].as_str().unwrap_or("?")
                ),
            );
        }
    }
    // An open PR that does not merge into the base has no as-landed
    // tree, so an overlap with it is unknown until it rebases. Named
    // here so it is not missed; needs hands-on, never blocking (CAD-295).
    // Its advisory file overlap rides inside the same reason — one
    // reason per PR, never a level of its own (CAD-297).
    let not_assessed = report["open_pr_not_assessed"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let unassessed: Vec<String> = not_assessed
        .iter()
        .filter(|c| c["reason"] == NOT_ASSESSED_OTHER_PR)
        .map(|c| {
            let pr = c["pr"].as_i64().unwrap_or(0);
            match advisory_note(c) {
                Some(note) => format!("#{pr} ({note})"),
                None => format!("#{pr}"),
            }
        })
        .collect();
    if !unassessed.is_empty() {
        let they = if unassessed.len() == 1 {
            "it does"
        } else {
            "they do"
        };
        push_reason(
            &mut level,
            &mut reasons,
            1,
            format!(
                "overlap not assessed with {} — {they} not merge into the current base",
                unassessed.join(", ")
            ),
        );
    }
    // When this PR is the one that does not merge, the merge-conflict
    // reason already blocks and adds nothing per PR (CAD-295); a PR
    // that changes the same files is still named, advisory only.
    for c in not_assessed
        .iter()
        .filter(|c| c["reason"] == NOT_ASSESSED_THIS_PR)
    {
        if let Some(note) = advisory_note(c) {
            push_reason(
                &mut level,
                &mut reasons,
                0,
                format!(
                    "#{} not assessed as this PR does not merge — {note}",
                    c["pr"].as_i64().unwrap_or(0)
                ),
            );
        }
    }
    if reasons.is_empty() {
        reasons.push("all mechanical checks green — the hands-on check remains".into());
    }
    let verdict = match level {
        2 => "blocked",
        1 => "needs-hands-on",
        _ => "pass",
    };
    (verdict, reasons)
}

// ---------------------------------------------------------------------------
// Markdown report
// ---------------------------------------------------------------------------

fn md_escape(s: &str) -> String {
    s.replace('|', "\\|")
}

fn short_sha(s: &str) -> &str {
    &s[..12.min(s.len())]
}

fn render_markdown(r: &Value) -> String {
    let mut md = String::new();
    let verdict = r["suggested_verdict"].as_str().unwrap_or("?");
    md.push_str(&format!(
        "# Review: PR #{} — {}\n\n",
        r["pr"].as_i64().unwrap_or(0),
        r["title"].as_str().unwrap_or("")
    ));
    md.push_str(&format!(
        "- repo `{}` · state `{}` · head `{}` (`{}`) · base `{}` @ `{}`\n",
        r["repo"].as_str().unwrap_or(""),
        r["state"].as_str().unwrap_or("?"),
        short_sha(r["head"].as_str().unwrap_or("")),
        r["head_ref"].as_str().unwrap_or(""),
        r["base"]["ref"].as_str().unwrap_or(""),
        short_sha(r["base"]["sha"].as_str().unwrap_or("")),
    ));
    let moved = r["base"]["moved_since_merge_base"]
        .as_bool()
        .unwrap_or(false);
    md.push_str(&format!(
        "- merge-base `{}` — base moved since merge-base: **{}**; gated tree: **{}**\n",
        short_sha(r["merge_base"].as_str().unwrap_or("")),
        if moved { "yes" } else { "no" },
        r["gated_tree"].as_str().unwrap_or("")
    ));
    if r["merge"]["attempted"].as_bool().unwrap_or(false) {
        if r["merge"]["result"].as_str() == Some("conflict") {
            let files = r["merge"]["conflict_files"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|f| f.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            md.push_str(&format!("- merge result: **conflict** — {files}\n"));
        } else {
            md.push_str("- merge result: clean — gates ran on the merged tree\n");
        }
    }
    if r["config"].is_object() {
        let changed = r["config"]["changed_by_pr"].as_bool().unwrap_or(false);
        md.push_str(&format!(
            "- config: `{CONFIG_FILE}` from the base head `{}` — changed by this PR: **{}**{}\n",
            short_sha(r["config"]["base_sha"].as_str().unwrap_or("")),
            if changed { "yes" } else { "no" },
            if changed {
                " (the PR's copy was not used; risk class 7)"
            } else {
                ""
            }
        ));
    }
    md.push_str(&format!(
        "- duration {}s · {}\n\n**suggested verdict: {}**\n\n",
        r["duration_ms"].as_u64().unwrap_or(0) / 1000,
        r["started_at"].as_str().unwrap_or(""),
        verdict
    ));
    for reason in r["verdict_reasons"].as_array().cloned().unwrap_or_default() {
        md.push_str(&format!("- {}\n", reason.as_str().unwrap_or("")));
    }
    md.push('\n');

    let section = |md: &mut String, title: &str, steps: &[Value]| {
        if steps.is_empty() {
            return;
        }
        md.push_str(&format!(
            "## {title}\n\n| step | command | dur | outcome |\n|---|---|---|---|\n"
        ));
        for s in steps {
            md.push_str(&format!(
                "| {} | `{}` | {}ms | {} |\n",
                s["name"].as_str().unwrap_or(""),
                md_escape(s["cmd"].as_str().unwrap_or("")),
                s["duration_ms"].as_u64().unwrap_or(0),
                s["outcome"].as_str().unwrap_or("")
            ));
        }
        md.push('\n');
        for s in steps {
            if let Some(tail) = s["tail"].as_array().filter(|t| !t.is_empty()) {
                md.push_str(&format!(
                    "### `{}` tail\n\n```\n{}\n```\n\n",
                    s["cmd"].as_str().unwrap_or(""),
                    tail.iter()
                        .filter_map(|l| l.as_str())
                        .collect::<Vec<_>>()
                        .join("\n")
                ));
            }
        }
    };
    section(
        &mut md,
        "Prepare",
        &r["prepare"].as_array().cloned().unwrap_or_default(),
    );
    section(
        &mut md,
        "Gates",
        &r["gates"].as_array().cloned().unwrap_or_default(),
    );
    section(&mut md, "Full suite", &[r["full_suite"].clone()]);

    let stress = r["stress"].as_array().cloned().unwrap_or_default();
    md.push_str("## New tests stressed\n\n");
    if stress.is_empty() {
        md.push_str("none — no new tests matched the stress pattern\n\n");
    } else {
        md.push_str("| test | file | runs | failures |\n|---|---|---|---|\n");
        for s in &stress {
            md.push_str(&format!(
                "| `{}` | {} | {} | {} |\n",
                s["test"].as_str().unwrap_or(""),
                s["file"].as_str().unwrap_or(""),
                s["runs"].as_u64().unwrap_or(0),
                s["failures"].as_u64().unwrap_or(0)
            ));
        }
        md.push('\n');
    }

    let failures = r["failures"].as_array().cloned().unwrap_or_default();
    md.push_str("## Failures — equal-conditions compare\n\n");
    if failures.is_empty() {
        md.push_str("none — nothing failed\n\n");
    } else {
        md.push_str("| test | in run | alone on gated tree | alone on base | verdict | flake sightings |\n|---|---|---|---|---|---|\n");
        for f in &failures {
            md.push_str(&format!(
                "| `{}` | {} | {} | {} | {} | {} |\n",
                f["test"].as_str().unwrap_or(""),
                f["in_run"].as_str().unwrap_or(""),
                f["isolated_gated"]["outcome"].as_str().unwrap_or(""),
                f["isolated_base"]["outcome"].as_str().unwrap_or(""),
                f["verdict"].as_str().unwrap_or(""),
                f["flake"]["sightings"]
                    .as_u64()
                    .map(|n| format!("{n} ({} heads)", f["flake"]["heads"]))
                    .unwrap_or_else(|| "-".into())
            ));
        }
        md.push('\n');
        let known: Vec<&Value> = failures
            .iter()
            .filter(|f| f["flake"]["known_flake"] == true)
            .collect();
        if !known.is_empty() {
            md.push_str(&format!(
                "## Known flakes\n\nLedger sightings on {KNOWN_FLAKE_HEADS}+ distinct heads — listed, not blocking:\n\n"
            ));
            for f in known {
                md.push_str(&format!(
                    "- `{}` — {} sightings on {} heads\n",
                    f["test"].as_str().unwrap_or(""),
                    f["flake"]["sightings"],
                    f["flake"]["heads"]
                ));
            }
            md.push('\n');
        }
    }
    if let Some(h) = r["host_load"].as_object() {
        md.push_str(&format!(
            "Host at suite start: nproc {}, load1 {}, {} `cargo test` processes\n\n",
            h.get("nproc").cloned().unwrap_or_default(),
            h.get("load1").cloned().unwrap_or_default(),
            h.get("cargo_test_processes").cloned().unwrap_or_default()
        ));
    }

    let base_prepare = r["base_prepare"].as_array().cloned().unwrap_or_default();
    if !base_prepare.is_empty() {
        section(&mut md, "Base-tree prepare", &base_prepare);
    }

    let conflicts = r["open_pr_conflicts"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let skipped = r["open_pr_not_assessed"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    md.push_str("## Other open PRs\n\n");
    if let Some(base) = r["open_pr_conflicts_base"].as_str() {
        md.push_str(&format!(
            "Compared as both PRs would land on base `{}`.\n\n",
            &base[..base.len().min(12)]
        ));
    }
    if let Some(err) = r["open_pr_conflicts_error"].as_str() {
        md.push_str(&format!("scan failed: {err}\n\n"));
    } else if conflicts.is_empty() && !skipped.is_empty() {
        // Not assessed is not "no conflict": the tool cannot say (CAD-297).
        md.push_str(&format!(
            "none among assessed PRs — {} not assessed, listed below\n\n",
            skipped.len()
        ));
    } else if conflicts.is_empty() {
        md.push_str("none — no conflicting open PRs\n\n");
    } else {
        for c in &conflicts {
            if let Some(files) = c["files"].as_array() {
                md.push_str(&format!(
                    "- #{} {} — conflicts on: {}\n",
                    c["pr"].as_i64().unwrap_or(0),
                    c["title"].as_str().unwrap_or(""),
                    files
                        .iter()
                        .filter_map(|f| f.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            } else {
                md.push_str(&format!(
                    "- #{} {} — {}\n",
                    c["pr"].as_i64().unwrap_or(0),
                    c["title"].as_str().unwrap_or(""),
                    c["error"].as_str().unwrap_or("no data")
                ));
            }
        }
        md.push('\n');
    }
    if !skipped.is_empty() {
        md.push_str("Not assessed (no clean as-landed tree to compare):\n\n");
        for c in &skipped {
            md.push_str(&format!(
                "- #{} {} — {}\n",
                c["pr"].as_i64().unwrap_or(0),
                c["title"].as_str().unwrap_or(""),
                c["reason"].as_str().unwrap_or("")
            ));
            if let Some(note) = advisory_note(c) {
                md.push_str(&format!("  - {note}\n"));
            }
        }
        md.push('\n');
    }

    md.push_str("## Schema migration\n\n");
    if r["schema_migration"].as_bool().unwrap_or(false) {
        md.push_str("Heuristic hit — rehearse the migration on a copied live DB:\n\n```\n");
        for h in r["schema_hits"].as_array().cloned().unwrap_or_default() {
            md.push_str(&format!("{}\n", h.as_str().unwrap_or("")));
        }
        md.push_str("```\n\n");
    } else {
        md.push_str("no schema-migration signal in the diff\n\n");
    }
    md
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIFF: &str = "\
diff --git a/src/main.rs b/src/main.rs
index 111..222 100644
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,2 +1,3 @@
+fn helper_not_a_test() {}
 fn old() {}
diff --git a/tests/integration.rs b/tests/integration.rs
index 333..444 100644
--- a/tests/integration.rs
+++ b/tests/integration.rs
@@ -10,0 +11,6 @@
+#[test]
+fn new_daemon_wait() {
+    let a = wait_agent(\"w\", \"idle\", 10);
+    drop(a);
+}
 fn existing_test() {}
@@ -40,0 +47,4 @@
+#[test]
+fn new_quiet_test() {
+    assert_eq!(1, 1);
+}
+fn helper_without_attr() {}
+#[cfg(test)]
+fn attr_but_cfg_only() {}
+#[tokio::test]
+fn tokio_wait() {
+    wait_agent(\"w\", \"idle\", 10);
+}
diff --git a/tests/deep/sub.rs b/tests/deep/sub.rs
index 555..666 100644
--- a/tests/deep/sub.rs
+++ b/tests/deep/sub.rs
@@ -1,0 +2,4 @@
+#[test]
+fn another_wait() {
+    wait_message(\"m\");
+}
";

    fn globs() -> Vec<String> {
        vec!["tests/**".into()]
    }

    #[test]
    fn glob_matching() {
        assert!(glob_match("tests/**", "tests/integration.rs"));
        assert!(glob_match("tests/**", "tests/deep/sub.rs"));
        assert!(!glob_match("tests/**", "src/tests/x.rs"));
        assert!(glob_match("tests/*.rs", "tests/a.rs"));
        assert!(!glob_match("tests/*.rs", "tests/deep/a.rs"));
        assert!(glob_match("a/**/b", "a/b"));
        assert!(glob_match("a/**/b", "a/x/y/b"));
        assert!(glob_match("a?c", "abc"));
        assert!(!glob_match("a?c", "a/c"));
        assert!(glob_match("*", "x"));
        assert!(!glob_match("*", "x/y"));
    }

    #[test]
    fn new_tests_only_under_globs_and_matching_pattern() {
        let pats = vec!["wait_".to_string()];
        let tests = parse_new_tests(DIFF, &globs(), &pats);
        let names: Vec<&str> = tests.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec!["new_daemon_wait", "tokio_wait", "another_wait"]);
        assert_eq!(tests[0].file, "tests/integration.rs");
        assert_eq!(tests[2].file, "tests/deep/sub.rs");
    }

    #[test]
    fn helpers_without_test_attr_are_not_tests() {
        let tests = parse_new_tests(DIFF, &globs(), &[]);
        let names: Vec<&str> = tests.iter().map(|t| t.name.as_str()).collect();
        // #[test] and #[tokio::test] count; bare helpers and #[cfg]-only
        // functions do not.
        assert_eq!(
            names,
            vec![
                "new_daemon_wait",
                "new_quiet_test",
                "tokio_wait",
                "another_wait"
            ]
        );
    }

    #[test]
    fn test_command_substitution() {
        let nt = NewTest {
            name: "x".into(),
            file: "tests/integration.rs".into(),
            body: String::new(),
        };
        // Substitutions arrive shell-quoted — the template must not
        // pre-quote.
        assert_eq!(
            test_command("cargo test --test {target} {test}", &nt),
            "cargo test --test 'integration' 'x'"
        );
    }

    #[test]
    fn validation_and_classification() {
        assert!(valid_test_name("tests::a::b"));
        assert!(valid_test_name("new_flaky"));
        assert!(!valid_test_name("bad;name"));
        assert!(!valid_test_name("$(evil)"));
        assert!(!valid_test_name("a b"));
        assert!(!valid_test_name(""));
        assert!(safe_rel_path("tests/deep/sub.rs"));
        assert!(!safe_rel_path("../x.rs"));
        assert!(!safe_rel_path("/abs/x.rs"));
        assert!(!safe_rel_path("a;b.rs"));

        let step = |outcome: &'static str, output: &str| Step {
            name: "t".into(),
            cmd: "c".into(),
            duration_ms: 0,
            outcome,
            exit: None,
            tail: vec![],
            output: output.to_string(),
            result: None,
        };
        assert_eq!(
            classify_isolated(&step("ok", "test result: ok. 3 passed; 0 failed")),
            "pass"
        );
        // A filter that matched nothing is `unknown`, not `ok`.
        assert_eq!(
            classify_isolated(&step("ok", "test result: ok. 0 passed; 0 failed")),
            "unknown"
        );
        // Non-cargo output still classifies by exit.
        assert_eq!(classify_isolated(&step("ok", "done")), "pass");
        assert_eq!(classify_isolated(&step("fail", "boom")), "fail");
        assert_eq!(classify_isolated(&step("timeout", "")), "unknown");
    }

    #[test]
    fn cargo_failure_extraction() {
        let out = "\
test a ... ok
test b ... FAILED
failures:

failures:
    b
    c

test result: FAILED. 1 passed; 2 failed
";
        assert_eq!(extract_failed_tests(out), vec!["b", "c"]);
    }

    #[test]
    fn schema_heuristic() {
        // Positive lines built at runtime — source literals reading as
        // migration SQL would trip the heuristic on this file's own diff.
        let mig = format!("+ UPDATE {} SET version={}", "schema_version", 4);
        let ddl = format!("+ {} {} ADD COLUMN x", "ALTER TABLE", "agents");
        assert_eq!(schema_migration_hit(&mig).len(), 1);
        assert_eq!(schema_migration_hit(&ddl).len(), 1);
        assert!(schema_migration_hit("+let version = 4;").is_empty());
        let quoted = format!("+ {}", "\"alter table\"");
        assert!(schema_migration_hit(&quoted).is_empty());
    }

    #[test]
    fn config_missing_names_keys() {
        let err = ReviewConfig::load(Path::new("/nonexistent-dir-xyz")).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("prepare"), "{msg}");
        assert!(msg.contains("test_globs"), "{msg}");
    }

    #[test]
    fn config_parses() {
        let toml = r#"
prepare = ["make deps"]
gates = ["cargo fmt --check", "cargo clippy"]
full_suite = "cargo test"
test_globs = ["tests/**"]
test_command = "cargo test --test {target} {test}"
stress_pattern = ["wait_", "sleep"]
[timeouts]
gate_secs = 42
"#;
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(CONFIG_FILE), toml).unwrap();
        let cfg = ReviewConfig::load(dir.path()).unwrap();
        assert_eq!(cfg.gates.len(), 2);
        assert_eq!(cfg.timeouts.gate_secs, 42);
        assert_eq!(cfg.timeouts.full_secs, 3600);
        assert_eq!(cfg.stress_pattern.0, vec!["wait_", "sleep"]);
        assert_eq!(cfg.runner.backend, ReviewBackend::Cargo);
        assert_eq!(cfg.runner.result_format, ResultFormat::Cargo);
    }

    #[test]
    fn config_parse_names_its_origin() {
        let origin = "abc123:cadence-review.toml";
        let err = ReviewConfig::parse(
            "prepare = []\ngates = []\nfull_suite = \"x\"\ntest_globs = [\"t/**\"]\ntest_command = \"{test}\"\n",
            origin,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains(origin) && err.contains("`gates`"), "{err}");
        let err = ReviewConfig::parse("gates = [", origin)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains(&format!("{origin} is not valid TOML")),
            "{err}"
        );
    }

    #[test]
    fn config_comes_from_the_base_commit_and_pr_changes_are_detected() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        let g = |args: &[&str]| git(repo, args, 60).unwrap();
        g(&["init", "-q"]);
        g(&["config", "user.email", "t@t"]);
        g(&["config", "user.name", "t"]);
        g(&["commit", "-q", "--allow-empty", "-m", "no config"]);
        let bare = g(&["rev-parse", "HEAD"]);
        let base_toml = "prepare = []\ngates = [\"false\"]\nfull_suite = \"x\"\n\
                         test_globs = [\"t/**\"]\ntest_command = \"{test}\"\n";
        std::fs::write(repo.join(CONFIG_FILE), base_toml).unwrap();
        g(&["add", "-A"]);
        g(&["commit", "-q", "-m", "config"]);
        let base = g(&["rev-parse", "HEAD"]);
        // The working tree says otherwise — it is never read.
        std::fs::write(repo.join(CONFIG_FILE), base_toml.replace("false", "true")).unwrap();

        let err = config_at_base(repo, "main", &bare, 60)
            .unwrap_err()
            .to_string();
        assert!(err.contains(&bare) && err.contains("base head"), "{err}");
        let cfg = config_at_base(repo, "main", &base, 60).unwrap();
        assert_eq!(cfg.gates, vec!["false"]);

        g(&["commit", "-q", "-am", "pr weakens gates"]);
        let pr = g(&["rev-parse", "HEAD"]);
        assert!(config_changed_by_pr(repo, &base, &pr, 60).unwrap());
        assert!(!config_changed_by_pr(repo, &base, &base, 60).unwrap());
        // Added where the merge-base had none counts as a change.
        assert!(config_changed_by_pr(repo, &bare, &base, 60).unwrap());
    }

    #[test]
    fn junit_results_name_failures_and_reject_zero_tests() {
        let xml = r#"<?xml version="1.0"?>
<testsuites tests="2" failures="1">
  <testsuite name="cadence-agent::integration" tests="2" failures="1">
    <testcase name="healthy_test" time="0.125"></testcase>
    <testcase name="broken_test" time="0.25"><failure type="test failure">boom</failure></testcase>
  </testsuite>
</testsuites>"#;
        let summary = parse_junit(xml);
        assert!(summary.valid);
        assert_eq!(summary.test_count, 2);
        assert_eq!(summary.passed, 1);
        assert_eq!(summary.failed, 1);
        assert_eq!(summary.failed_tests, vec!["broken_test"]);
        assert_eq!(summary.tests[0].duration_s, Some(0.125));

        let zero = parse_junit("<testsuites tests=\"0\"></testsuites>");
        assert!(!zero.valid);
        assert_eq!(zero.test_count, 0);
        assert!(zero.reason.unwrap().contains("zero testcases"));

        let missing_dir = tempfile::tempdir().unwrap();
        let missing = parse_junit_file(&missing_dir.path().join("junit.xml"));
        assert!(!missing.valid);
        assert!(missing.reason.unwrap().contains("unavailable"));

        let malformed = parse_junit("<testsuites><testsuite><testcase name=\"x\">");
        assert!(!malformed.valid);
        assert!(malformed.reason.unwrap().contains("closing all tags"));
    }

    #[test]
    fn junit_ignored_only_is_unknown_not_failure() {
        let ignored = parse_junit(
            r#"<testsuites tests="1" skipped="1" failures="0">
  <testsuite name="ignored" tests="1" skipped="1" failures="0">
    <testcase name="ignored_case"><skipped/></testcase>
  </testsuite>
</testsuites>"#,
        );
        assert!(ignored.valid);
        assert_eq!(ignored.test_count, 1);
        assert_eq!(ignored.executed_count(), 0);
        assert_eq!(
            ignored.reason.as_deref(),
            Some("JUnit report contained no executed testcases")
        );

        let step = |outcome: &'static str, result: TestRunSummary| Step {
            name: "ignored_case".into(),
            cmd: "scripts/cadence-nextest --test integration -- ignored_case --exact".into(),
            duration_ms: 0,
            outcome,
            exit: Some(4),
            tail: Vec::new(),
            output: String::new(),
            result: Some(result),
        };
        assert_eq!(classify_isolated(&step("fail", ignored.clone())), "unknown");
        assert_eq!(classify_isolated(&step("ok", ignored)), "unknown");

        let executed = parse_junit(
            r#"<testsuites tests="1" skipped="0" failures="0">
  <testsuite name="executed" tests="1" skipped="0" failures="0">
    <testcase name="executed_case"/>
  </testsuite>
</testsuites>"#,
        );
        assert_eq!(executed.executed_count(), 1);
        assert_eq!(classify_isolated(&step("ok", executed)), "pass");
    }

    #[test]
    fn report_exposes_cad173_schema_and_preserves_legacy_paths() {
        let summary = parse_junit(
            r#"<testsuites tests="1" skipped="0" failures="0">
  <testsuite name="contract" tests="1" skipped="0" failures="0">
    <testcase name="executed_case" time="0.125"/>
  </testsuite>
</testsuites>"#,
        );
        let step = Step {
            name: "full-suite".into(),
            cmd: "scripts/cadence-nextest --all-targets".into(),
            duration_ms: 125,
            outcome: "ok",
            exit: Some(0),
            tail: Vec::new(),
            output: String::new(),
            result: Some(summary),
        };
        let full = full_suite_report(Some(&step), "scripts/cadence-nextest", 0);

        // Canonical CAD-173 consumers read the concise top-level fields.
        assert_eq!(full["retries"], 0);
        assert_eq!(full["duration_s"], 0.125);
        assert_eq!(full["tests"].as_array().unwrap().len(), 1);
        assert_eq!(full["tests"][0]["name"], "executed_case");
        // Existing consumers retain the nested step/result representation.
        assert_eq!(full["result"]["tests"].as_array().unwrap().len(), 1);

        let gated = json!({"outcome": "pass"});
        let base = json!({"outcome": "pass"});
        let row = json!({
            "test": "executed_case",
            "result": "pass",
            "gated": gated.clone(),
            "base": base.clone(),
            "isolated_gated": gated,
            "isolated_base": base,
            "verdict": "flake-under-load",
        });
        let mut report = json!({});
        set_failure_reports(&mut report, &[row]);
        assert_eq!(report["isolated"][0]["gated"]["outcome"], "pass");
        assert_eq!(report["isolated"][0]["result"], "pass");
        assert_eq!(report["failures"][0]["isolated_base"]["outcome"], "pass");
    }

    #[test]
    fn no_execution_full_suite_evidence_is_unknown_and_blocks() {
        let all_skipped = parse_junit(
            r#"<testsuites tests="1" skipped="1" failures="0">
  <testsuite name="ignored" tests="1" skipped="1" failures="0">
    <testcase name="ignored_case"><skipped/></testcase>
  </testsuite>
</testsuites>"#,
        );
        assert!(all_skipped.valid);
        assert_eq!(all_skipped.executed_count(), 0);

        let missing_dir = tempfile::tempdir().unwrap();
        let missing = parse_junit_file(&missing_dir.path().join("junit.xml"));
        let malformed = parse_junit("<testsuites><testsuite><testcase name=\"x\">");
        let cases = [missing, malformed, all_skipped];
        for summary in cases {
            let step = Step {
                name: "full-suite".into(),
                cmd: "scripts/cadence-nextest --all-targets".into(),
                duration_ms: 250,
                outcome: "fail",
                exit: Some(0),
                tail: Vec::new(),
                output: String::new(),
                result: Some(summary),
            };
            let row = unknown_full_suite_row(&step);
            assert_eq!(row["source"], "full_suite");
            assert_eq!(row["result"], "unknown");
            assert_eq!(row["verdict"], "inconclusive");
            assert!(row["test"].is_null(), "must not invent a testcase name");
            assert_eq!(row["gated"]["outcome"], "unknown");
            assert_eq!(row["base"]["outcome"], "unknown");

            let mut report = json!({
                "merge": {}, "prepare": [], "gates": [], "stress": [],
                "open_pr_conflicts": [], "full_suite": {"outcome": "fail"}
            });
            set_failure_reports(&mut report, &[row]);
            assert_eq!(report["isolated"].as_array().unwrap().len(), 1);
            assert_eq!(suggest(&report, false).0, "blocked");
        }
    }

    #[test]
    fn nextest_config_requires_same_exact_backend_for_isolation() {
        let toml = r#"
prepare = ["make deps"]
gates = ["cargo fmt --check"]
full_suite = "scripts/cadence-nextest --test integration"
test_globs = ["tests/**"]
test_command = "scripts/cadence-nextest --test {target} -- {test} --exact"
[runner]
backend = "nextest"
result_format = "junit"
result_path = "target/nextest/cadence/junit.xml"
"#;
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(CONFIG_FILE), toml).unwrap();
        let cfg = ReviewConfig::load(dir.path()).unwrap();
        assert_eq!(cfg.runner.backend, ReviewBackend::Nextest);
        assert_eq!(cfg.runner.result_format, ResultFormat::Junit);

        let mismatched = toml.replace(
            "test_command = \"scripts/cadence-nextest --test {target} -- {test} --exact\"",
            "test_command = \"cargo test --test {target} {test}\"",
        );
        std::fs::write(dir.path().join(CONFIG_FILE), mismatched).unwrap();
        let err = ReviewConfig::load(dir.path()).unwrap_err().to_string();
        assert!(err.contains("both full_suite and test_command"), "{err}");
    }

    #[test]
    fn suite_child_marker_requires_outer_lock() {
        let unowned = suite_child_env(vec![("BASE".into(), "1".into())], false);
        assert_eq!(unowned, vec![("BASE".to_string(), "1".to_string())]);

        let owned = suite_child_env(vec![("BASE".into(), "1".into())], true);
        assert_eq!(owned[0], ("BASE".to_string(), "1".to_string()));
        assert!(owned.contains(&("CADENCE_SUITE_LOCK".into(), String::new())));
        assert!(owned.contains(&("CADENCE_REVIEW_SUITE_LOCK_HELD".into(), "1".into())));
    }

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: std::collections::HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k| map.get(k).cloned()
    }

    fn lookup<'a>(env: &'a [(String, String)], k: &str) -> Option<&'a str> {
        env.iter()
            .find(|(key, _)| key == k)
            .map(|(_, v)| v.as_str())
    }

    #[test]
    fn gate_env_scrubs_identity_and_keeps_tool_homes() {
        // Bare caller: tool homes derive from the real HOME, and
        // useConfigOnly is the only config entry.
        let env = gate_env(Path::new("/scratch"), env_of(&[("HOME", "/real")]));
        assert_eq!(lookup(&env, "HOME"), Some("/scratch"));
        assert_eq!(lookup(&env, "CARGO_HOME"), Some("/real/.cargo"));
        assert_eq!(lookup(&env, "RUSTUP_HOME"), Some("/real/.rustup"));
        assert_eq!(lookup(&env, "XDG_DATA_HOME"), Some("/real/.local/share"));
        assert_eq!(lookup(&env, "GIT_CONFIG_NOSYSTEM"), Some("1"));
        assert_eq!(lookup(&env, "GIT_CONFIG_COUNT"), Some("1"));
        assert_eq!(lookup(&env, "GIT_CONFIG_KEY_0"), Some("user.useConfigOnly"));
        assert_eq!(lookup(&env, "GIT_CONFIG_VALUE_0"), Some("true"));

        // Explicit tool homes win; the caller's GIT_CONFIG entries are
        // appended to, minus identity keys (any case), renumbered.
        let env = gate_env(
            Path::new("/scratch"),
            env_of(&[
                ("HOME", "/real"),
                ("CARGO_HOME", "/opt/cargo"),
                ("RUSTUP_HOME", "/opt/rustup"),
                ("XDG_DATA_HOME", "/opt/data"),
                ("GIT_CONFIG_COUNT", "4"),
                ("GIT_CONFIG_KEY_0", "safe.directory"),
                ("GIT_CONFIG_VALUE_0", "*"),
                ("GIT_CONFIG_KEY_1", "User.Email"),
                ("GIT_CONFIG_VALUE_1", "x@example.com"),
                ("GIT_CONFIG_KEY_2", "user.useConfigOnly"),
                ("GIT_CONFIG_VALUE_2", "false"),
                ("GIT_CONFIG_KEY_3", "core.autocrlf"),
                ("GIT_CONFIG_VALUE_3", ""),
            ]),
        );
        assert_eq!(lookup(&env, "CARGO_HOME"), Some("/opt/cargo"));
        assert_eq!(lookup(&env, "RUSTUP_HOME"), Some("/opt/rustup"));
        assert_eq!(lookup(&env, "XDG_DATA_HOME"), Some("/opt/data"));
        assert_eq!(lookup(&env, "GIT_CONFIG_COUNT"), Some("3"));
        assert_eq!(lookup(&env, "GIT_CONFIG_KEY_0"), Some("safe.directory"));
        assert_eq!(lookup(&env, "GIT_CONFIG_VALUE_0"), Some("*"));
        assert_eq!(lookup(&env, "GIT_CONFIG_KEY_1"), Some("core.autocrlf"));
        assert_eq!(lookup(&env, "GIT_CONFIG_VALUE_1"), Some(""));
        assert_eq!(lookup(&env, "GIT_CONFIG_KEY_2"), Some("user.useConfigOnly"));
        assert_eq!(lookup(&env, "GIT_CONFIG_VALUE_2"), Some("true"));

        // A malformed count is git's own error; the review starts over.
        let env = gate_env(Path::new("/s"), env_of(&[("GIT_CONFIG_COUNT", "x")]));
        assert_eq!(lookup(&env, "GIT_CONFIG_COUNT"), Some("1"));
        assert_eq!(lookup(&env, "CARGO_HOME"), None);
    }

    #[test]
    fn verdict_logic() {
        let mut r = json!({"merge": {}, "prepare": [], "gates": [], "failures": [],
            "stress": [], "open_pr_conflicts": []});
        assert_eq!(suggest(&r, false).0, "pass");
        r["gates"] = json!([{"cmd": "g", "outcome": "fail"}]);
        assert_eq!(suggest(&r, false).0, "blocked");
        r["gates"] = json!([]);
        r["failures"] = json!([{"test": "x", "verdict": "flake-under-load"}]);
        assert_eq!(suggest(&r, false).0, "needs-hands-on");
        r["failures"] = json!([{"test": "x", "verdict": "regression"}]);
        assert_eq!(suggest(&r, false).0, "blocked");
        r["failures"] = json!([]);
        r["full_suite"] = json!({"outcome": "fail"});
        assert_eq!(suggest(&r, false).0, "blocked");
        r["full_suite"] = json!({"outcome": "ok"});
        // An inconclusive compare blocks — never launders into pass.
        r["failures"] = json!([{"test": "x", "verdict": "inconclusive"}]);
        assert_eq!(suggest(&r, false).0, "blocked");
        // A conflict-scan error is a reason, not a conflict.
        r["failures"] = json!([]);
        r["open_pr_conflicts"] = json!([{"pr": 9, "error": "merge-tree exited 128"}]);
        assert_eq!(suggest(&r, false).0, "needs-hands-on");
        // A failed base prepare blocks too.
        r["open_pr_conflicts"] = json!([]);
        r["base_prepare"] = json!([{"outcome": "fail"}]);
        assert_eq!(suggest(&r, false).0, "blocked");
    }

    /// CAD-295: an open PR that does not merge into the base has no
    /// as-landed tree, so its overlap is unknown. That is named in the
    /// verdict — needs hands-on, never blocking. When this PR is the
    /// one that does not merge, the merge-conflict reason already
    /// blocks and no second reason is added.
    #[test]
    fn not_assessed_open_prs_are_a_verdict_reason() {
        let clean = json!({"merge": {}, "prepare": [], "gates": [], "failures": [],
            "stress": [], "open_pr_conflicts": []});

        let mut r = clean.clone();
        r["open_pr_not_assessed"] = json!([
            {"pr": 136, "title": "a", "reason": "it does not merge into the current base"},
            {"pr": 154, "title": "b", "reason": "it does not merge into the current base"},
        ]);
        let (verdict, reasons) = suggest(&r, false);
        assert_eq!(verdict, "needs-hands-on", "{reasons:?}");
        assert_eq!(
            reasons,
            vec!["overlap not assessed with #136, #154 — they do not merge into the current base"]
        );

        r["open_pr_not_assessed"] = json!([
            {"pr": 136, "title": "a", "reason": "it does not merge into the current base"},
        ]);
        let (verdict, reasons) = suggest(&r, false);
        assert_eq!(verdict, "needs-hands-on", "{reasons:?}");
        assert_eq!(
            reasons,
            vec!["overlap not assessed with #136 — it does not merge into the current base"]
        );

        // Never raised to blocking, and never lowers a block either.
        r["gates"] = json!([{"cmd": "g", "outcome": "fail"}]);
        let (verdict, reasons) = suggest(&r, false);
        assert_eq!(verdict, "blocked");
        assert!(
            reasons
                .iter()
                .any(|s| s.starts_with("overlap not assessed with #136")),
            "{reasons:?}"
        );

        // This PR does not merge: every other PR is listed, but the
        // blocking merge-conflict reason is the only one.
        let mut r = clean.clone();
        r["merge"] = json!({"result": "conflict", "conflict_files": ["x.txt"]});
        r["open_pr_not_assessed"] = json!([
            {"pr": 7, "title": "a", "reason": "this PR does not merge into the current base"},
            {"pr": 8, "title": "b", "reason": "this PR does not merge into the current base"},
        ]);
        let (verdict, reasons) = suggest(&r, false);
        assert_eq!(verdict, "blocked");
        assert_eq!(
            reasons,
            vec!["does not merge into the current base — x.txt"],
            "no duplicate reason for this PR's own conflict"
        );
    }

    /// CAD-297: a not-assessed PR's advisory overlap rides inside the
    /// CAD-295 reason — one reason per PR, never a level of its own —
    /// and an empty hint changes nothing.
    #[test]
    fn advisory_overlap_folds_into_the_not_assessed_reason() {
        let clean = json!({"merge": {}, "prepare": [], "gates": [], "failures": [],
            "stress": [], "open_pr_conflicts": []});
        let mut r = clean.clone();
        r["open_pr_not_assessed"] = json!([
            {"pr": 14, "title": "a", "reason": NOT_ASSESSED_OTHER_PR,
             "advisory_overlap": ["big.txt", "src/x.rs"]},
            {"pr": 15, "title": "b", "reason": NOT_ASSESSED_OTHER_PR},
            {"pr": 16, "title": "c", "reason": NOT_ASSESSED_OTHER_PR,
             "advisory_overlap_error": "diff-tree timed out"},
        ]);
        let (verdict, reasons) = suggest(&r, false);
        assert_eq!(verdict, "needs-hands-on", "{reasons:?}");
        assert_eq!(
            reasons,
            vec![
                "overlap not assessed with \
                 #14 (advisory, not a conflict: both PRs change big.txt, src/x.rs), #15, \
                 #16 (advisory overlap unavailable: diff-tree timed out) \
                 — they do not merge into the current base"
            ]
        );

        // This PR does not merge: CAD-295 adds nothing per PR, so a
        // shared file is named once, at level 0 — the verdict is the
        // merge conflict's alone. An empty hint adds nothing.
        let mut r = clean.clone();
        r["open_pr_not_assessed"] = json!([
            {"pr": 7, "title": "a", "reason": NOT_ASSESSED_THIS_PR,
             "advisory_overlap": ["big.txt"]},
            {"pr": 8, "title": "b", "reason": NOT_ASSESSED_THIS_PR},
        ]);
        let (verdict, reasons) = suggest(&r, false);
        assert_eq!(verdict, "pass", "{reasons:?}");
        assert_eq!(
            reasons,
            vec![
                "#7 not assessed as this PR does not merge — \
                 advisory, not a conflict: both PRs change big.txt"
            ]
        );
        r["merge"] = json!({"result": "conflict", "conflict_files": ["x.txt"]});
        let (verdict, reasons) = suggest(&r, false);
        assert_eq!(verdict, "blocked");
        assert_eq!(reasons.len(), 2, "{reasons:?}");
    }

    /// CAD-273: a missing or untrusted runner is named as such — one
    /// blocking reason, not a failed suite plus zero-test stress noise.
    #[test]
    fn runner_refusal_is_its_own_blocked_reason() {
        let r = json!({"merge": {}, "prepare": [], "gates": [], "open_pr_conflicts": [],
            "full_suite": {"outcome": "fail", "tail": ["",
                "cadence-nextest: pinned cargo-nextest 0.9.145 is not installed"]},
            "failures": [{"source": "full_suite", "result": "unknown",
                "reason": "JUnit report x unavailable"}],
            "stress": [{"test": "t", "runs": 5, "failures": 0, "unknown": 5}]});
        let (verdict, reasons) = suggest(&r, false);
        assert_eq!(verdict, "blocked");
        assert_eq!(reasons.len(), 1, "{reasons:?}");
        assert!(
            reasons[0].contains("test runner refused to start"),
            "{reasons:?}"
        );
        assert!(reasons[0].contains("is not installed"), "{reasons:?}");
        // A real suite failure whose tail merely mentions the wrapper
        // earlier is still a suite failure.
        let mut real = r.clone();
        real["full_suite"]["tail"] = json!(["cadence-nextest: note", "test result: FAILED"]);
        let (_, reasons) = suggest(&real, false);
        assert!(
            !reasons.iter().any(|m| m.contains("refused to start")),
            "{reasons:?}"
        );
        assert!(
            reasons
                .iter()
                .any(|m| m.contains("no executable testcase evidence")),
            "{reasons:?}"
        );
    }

    #[test]
    fn config_change_by_pr_is_never_a_pass() {
        let mut r = json!({"merge": {}, "prepare": [], "gates": [], "failures": [],
            "stress": [], "open_pr_conflicts": [],
            "config": {"source": "base", "base_sha": "b", "changed_by_pr": false}});
        assert_eq!(suggest(&r, false).0, "pass");
        r["config"]["changed_by_pr"] = json!(true);
        let (verdict, reasons) = suggest(&r, false);
        assert_eq!(verdict, "needs-hands-on", "{reasons:?}");
        assert!(
            reasons
                .iter()
                .any(|m| m.contains("changes cadence-review.toml") && m.contains("risk class 7")),
            "{reasons:?}"
        );
        // Already blocked stays blocked.
        r["gates"] = json!([{"cmd": "g", "outcome": "fail"}]);
        assert_eq!(suggest(&r, false).0, "blocked");
    }

    #[test]
    fn flake_ledger_needs_three_distinct_heads() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = dir.path().join("reviews").join(FLAKE_LEDGER);
        let entry = |repo: &str, test: &str, head: &str| json!({"at": "t", "repo": repo, "test": test, "pr": 1, "head": head});
        let seen = |e: Value| record_flake(&ledger, &e).unwrap();
        // First sighting: one head, not known.
        let first = seen(entry("o/r", "a", "h1"));
        assert_eq!(first, Sightings { total: 1, heads: 1 });
        assert!(!first.known_flake());
        // Three sightings on one head never qualify.
        seen(entry("o/r", "a", "h1"));
        let same_head = seen(entry("o/r", "a", "h1"));
        assert_eq!(same_head, Sightings { total: 3, heads: 1 });
        assert!(!same_head.known_flake());
        // Another repo and another test never count toward (o/r, a).
        assert_eq!(seen(entry("x/y", "a", "h2")).heads, 1);
        assert_eq!(seen(entry("o/r", "b", "h2")).heads, 1);
        // Malformed lines are skipped.
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&ledger)
            .unwrap();
        std::io::Write::write_all(&mut f, b"{not json\n\n{\"test\": 7}\n").unwrap();
        assert_eq!(seen(entry("o/r", "a", "h2")).heads, 2);
        let third = seen(entry("o/r", "a", "h3"));
        assert_eq!(third, Sightings { total: 5, heads: 3 });
        assert!(third.known_flake());
        // A fourth review of an already-seen head adds no qualifying head.
        let again = seen(entry("o/r", "a", "h3"));
        assert_eq!(again, Sightings { total: 6, heads: 3 });
    }

    #[test]
    fn flake_ledger_appends_stay_whole_under_concurrency() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = dir.path().join(FLAKE_LEDGER);
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let ledger = ledger.clone();
                std::thread::spawn(move || {
                    for n in 0..25 {
                        record_flake(
                            &ledger,
                            &json!({"repo": "o/r", "test": "t", "head": format!("h{i}-{n}"),
                                    "pad": "x".repeat(8000)}),
                        )
                        .unwrap();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let text = std::fs::read_to_string(&ledger).unwrap();
        assert_eq!(text.lines().count(), 200);
        assert!(text
            .lines()
            .all(|l| serde_json::from_str::<Value>(l).is_ok()));
        // The locked read makes the count exact.
        let last =
            record_flake(&ledger, &json!({"repo": "o/r", "test": "t", "head": "z"})).unwrap();
        assert_eq!(
            last,
            Sightings {
                total: 201,
                heads: 201
            }
        );
    }

    #[test]
    fn known_flake_does_not_block_the_verdict() {
        let mut r = json!({"merge": {}, "prepare": [], "gates": [], "stress": [],
            "open_pr_conflicts": [], "full_suite": {"outcome": "fail"}});
        r["failures"] = json!([{"test": "x", "verdict": "flake-under-load",
            "flake": {"sightings": 4, "heads": 2, "known_flake": false}}]);
        assert_eq!(suggest(&r, false).0, "needs-hands-on");
        r["failures"] = json!([{"test": "x", "verdict": "flake-under-load",
            "flake": {"sightings": 4, "heads": 3, "known_flake": true}}]);
        let (verdict, reasons) = suggest(&r, false);
        assert_eq!(verdict, "pass", "{reasons:?}");
        assert!(
            reasons.iter().any(|m| m.contains("known flake")),
            "{reasons:?}"
        );
    }

    #[test]
    fn panic_head_takes_the_named_threads_lines() {
        let out = "noise\nthread 'other' panicked\nx\nthread 'mine' (12) panicked at t.rs:1:2:\nboom\nmore\nand\ncut";
        assert_eq!(
            panic_head(out, "mine"),
            "thread 'mine' (12) panicked at t.rs:1:2:\nboom\nmore\nand"
        );
        assert_eq!(panic_head(out, "absent"), "");
    }
}
