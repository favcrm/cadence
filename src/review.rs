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
//! The steps are data, not code: `<repo>/cadence-review.toml` declares
//! `prepare`, `gates`, `full_suite`, `test_globs`, `stress_pattern`
//! and `test_command`. Every subprocess goes through
//! [`crate::proc::run_bounded`]; timeouts come from `[timeouts]`.

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
/// Config file `run` looks for at the main checkout root.
pub const CONFIG_FILE: &str = "cadence-review.toml";
/// Required keys named when the config file is absent.
const REQUIRED_KEYS: &str = "prepare, gates, full_suite, test_globs, test_command, stress_pattern";

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
}

impl ReviewConfig {
    /// Load `<root>/cadence-review.toml`; a missing file names the
    /// required keys so a fresh repo can write one without guessing.
    pub fn load(root: &Path) -> Result<Self> {
        let path = root.join(CONFIG_FILE);
        if !path.is_file() {
            return Err(Error::rejected(format!(
                "no {CONFIG_FILE} at {} — `cadence review` reads its steps \
                 from that file. Required keys: {REQUIRED_KEYS}; optional: \
                 [timeouts] prepare_secs gate_secs stress_secs full_secs \
                 test_secs git_secs gh_secs",
                root.display()
            )));
        }
        let text = std::fs::read_to_string(&path)?;
        let cfg: ReviewConfig = toml::from_str(&text)
            .map_err(|e| Error::rejected(format!("{} is not valid TOML: {e}", path.display())))?;
        if cfg.gates.is_empty() {
            return Err(Error::rejected(format!(
                "{}: `gates` must name at least one command",
                path.display()
            )));
        }
        if cfg.test_globs.is_empty() {
            return Err(Error::rejected(format!(
                "{}: `test_globs` must name at least one pattern",
                path.display()
            )));
        }
        if !cfg.test_command.contains("{test}") {
            return Err(Error::rejected(format!(
                "{}: `test_command` must contain a {{test}} placeholder",
                path.display()
            )));
        }
        Ok(cfg)
    }
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
fn git_status(repo: &Path, args: &[&str], secs: u64) -> Result<StepOut> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(repo).args(args);
    run_cmd(&mut cmd, Duration::from_secs(secs))
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
        })
    }
}

/// Run `sh -c <cmd>` in `cwd` with the review's env, timing it.
fn run_step(
    name: &str,
    cmd: &str,
    cwd: &Path,
    env: &[(String, String)],
    secs: u64,
) -> Result<Step> {
    let started = Instant::now();
    let mut sh = Command::new("sh");
    sh.arg("-c").arg(cmd).current_dir(cwd);
    for (k, v) in env {
        sh.env(k, v);
    }
    let out = run_cmd(&mut sh, Duration::from_secs(secs))?;
    let duration_ms = started.elapsed().as_millis();
    let combined = format!("{}\n{}", out.stdout, out.stderr);
    let (outcome, tail) = if out.timed_out {
        ("timeout", tail(&combined, TAIL_LINES))
    } else if out.status == Some(0) {
        ("ok", Vec::new())
    } else {
        ("fail", tail(&combined, TAIL_LINES))
    };
    Ok(Step {
        name: name.to_string(),
        cmd: cmd.to_string(),
        duration_ms,
        outcome,
        exit: out.status,
        tail,
        output: combined,
    })
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

/// pass | fail | unknown — `unknown` means "could not run": timeout,
/// missing file, prepare failure, or a filter that matched zero tests.
/// A non-zero exit IS a test failure; the command ran.
fn classify_isolated(step: &Step) -> &'static str {
    match step.outcome {
        "ok" => match test_totals(&step.output) {
            Some((0, 0)) => "unknown",
            _ => "pass",
        },
        "fail" => "fail",
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
    let root = worktree::main_root(&opts.cwd)?;
    let cfg = ReviewConfig::load(&root)?;
    let t = &cfg.timeouts;

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
            t.gh_secs,
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

    let pr = gh_pr_view(&root, &slug, &opts.pr, t.gh_secs)?;
    if pr.head_sha.is_empty() || pr.base_ref.is_empty() {
        return Err(Error::rejected(format!(
            "gh pr view {} returned no head/base — is it an open PR?",
            opts.pr
        )));
    }

    // Fetch both ends into the main object store.
    git(&root, &["fetch", "-q", "origin", &pr.base_ref], t.git_secs)?;
    let base_sha = git(&root, &["rev-parse", "FETCH_HEAD"], t.git_secs)?;
    git(
        &root,
        &["fetch", "-q", "origin", &format!("pull/{}/head", pr.number)],
        t.git_secs,
    )?;
    let head_sha = git(&root, &["rev-parse", "FETCH_HEAD"], t.git_secs)?;
    if head_sha != pr.head_sha {
        return Err(Error::rejected(format!(
            "PR #{} head moved while resolving: gh saw {}, fetch got {} \
             — rerun `cadence review`",
            pr.number, pr.head_sha, head_sha
        )));
    }
    let merge_base = git(&root, &["merge-base", &base_sha, &head_sha], t.git_secs)?;
    let base_moved = merge_base != base_sha;

    // The review checkout — detached, never the author's worktree.
    let wt_name = format!("review-{}", pr.number);
    let mut tree = ReviewTree::checkout(&root, &wt_name, &head_sha, opts.keep, t.git_secs)?;

    // Env every step command sees.
    let env = |tree_kind: &str| -> Vec<(String, String)> {
        vec![
            ("CADENCE_REVIEW_PR".into(), pr.number.to_string()),
            ("CADENCE_REVIEW_HEAD".into(), head_sha.clone()),
            ("CADENCE_REVIEW_BASE".into(), base_sha.clone()),
            ("CADENCE_REVIEW_MERGE_BASE".into(), merge_base.clone()),
            ("CADENCE_REVIEW_TREE".into(), tree_kind.into()),
            (
                "CADENCE_REVIEW_ROOT".into(),
                root.to_string_lossy().into_owned(),
            ),
        ]
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
        "gated_tree": gated_tree,
        "merge": merge,
        "worktree": tree.dir.to_string_lossy(),
        "no_full": !opts.full,
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
                let s = run_step(
                    &format!("stress-{}-{}", nt.name, i + 1),
                    &cmd,
                    &tree.dir,
                    &env(gated_tree),
                    t.stress_secs,
                )?;
                // A "pass" that ran zero tests means the filter missed —
                // that is `unknown`, not `ok`.
                let outcome = if s.outcome == "ok" && test_totals(&s.output) == Some((0, 0)) {
                    "unknown"
                } else {
                    s.outcome
                };
                match outcome {
                    "unknown" => unknown += 1,
                    o if o != "ok" => failures += 1,
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
            if let Ok(path) = std::env::var("CADENCE_SUITE_LOCK") {
                if !path.is_empty() {
                    let path = PathBuf::from(path);
                    let wait = Instant::now();
                    _suite_guard = Some(Flock::lock_deadline(&path, t.full_secs)?);
                    suite_lock = json!({"path": path,
                        "waited_ms": wait.elapsed().as_millis()});
                }
            }
            suite_step = Some(run_step(
                "full-suite",
                &cfg.full_suite,
                &tree.dir,
                &env(gated_tree),
                t.full_secs,
            )?);
        }
    }
    report["gates"] = json!(gate_steps.iter().map(Step::to_json).collect::<Vec<_>>());
    report["stress"] = json!(stress_results);
    report["suite_lock"] = suite_lock;
    report["full_suite"] = match &suite_step {
        Some(s) => s.to_json(),
        None => json!({"outcome": "skipped", "reason": "--no-full",
            "cmd": cfg.full_suite}),
    };

    // Equal-conditions compare: every failing test name, rerun alone
    // on the gated tree and alone on the base head.
    let mut failures_to_check: Vec<String> = Vec::new();
    for s in gate_steps.iter().chain(suite_step.iter()) {
        if s.outcome == "ok" || s.outcome == "skipped" {
            continue;
        }
        failures_to_check.extend(extract_failed_tests(&s.output));
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
        for name in &failures_to_check {
            // Names come from test output the PR controls — validate
            // before they reach a shell, and never execute a bad one.
            if !valid_test_name(name) {
                comparisons.push(json!({
                    "test": name, "in_run": "fail",
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
            let on_gated = run_step(
                "compare-gated",
                &cmd,
                &tree.dir,
                &env(gated_tree),
                t.test_secs,
            )?;
            let gated = classify_isolated(&on_gated);
            let (base, base_tail, base_reason) = if !base_ready {
                ("unknown", Vec::new(), Some("base prepare failed"))
            } else {
                let on_base = run_step(
                    "compare-base",
                    &cmd,
                    &base_tree.dir,
                    &env("base"),
                    t.test_secs,
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
            let mut gated_side = json!({"outcome": gated, "tail": on_gated.tail});
            if let Some(r) = gated_reason {
                gated_side["reason"] = json!(r);
            }
            let mut base_side = json!({"outcome": base, "tail": base_tail});
            if let Some(r) = base_reason {
                base_side["reason"] = json!(r);
            }
            comparisons.push(json!({
                "test": name, "cmd": cmd,
                "in_run": "fail",
                "isolated_gated": gated_side,
                "isolated_base": base_side,
                "verdict": verdict,
            }));
        }
        drop(base_tree);
    }
    report["failures"] = json!(comparisons);
    report["base_prepare"] = json!(base_prepare_steps
        .iter()
        .map(Step::to_json)
        .collect::<Vec<_>>());

    // Pairwise conflicts with the other open PRs (files only).
    let mut pr_conflicts = Vec::new();
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
                let fetch = git(
                    &root,
                    &["fetch", "-q", "origin", &format!("pull/{num}/head")],
                    t.git_secs,
                )
                .and_then(|_| git(&root, &["rev-parse", "FETCH_HEAD"], t.git_secs));
                let Ok(theirs) = fetch else {
                    pr_conflicts.push(json!({"pr": num,
                        "title": other["title"],
                        "error": "could not fetch head"}));
                    continue;
                };
                let mt = git_status(
                    &root,
                    &[
                        "merge-tree",
                        "--write-tree",
                        "--name-only",
                        &head_sha,
                        &theirs,
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

fn push_reason(level: &mut u8, reasons: &mut Vec<String>, l: u8, r: String) {
    *level = (*level).max(l);
    reasons.push(r);
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
    let comparisons = report["failures"].as_array().cloned().unwrap_or_default();
    for f in &comparisons {
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
    if suite_failed && comparisons.is_empty() {
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
        if s["unknown"].as_u64().unwrap_or(0) > 0 {
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
        md.push_str("| test | in run | alone on gated tree | alone on base | verdict |\n|---|---|---|---|---|\n");
        for f in &failures {
            md.push_str(&format!(
                "| `{}` | {} | {} | {} | {} |\n",
                f["test"].as_str().unwrap_or(""),
                f["in_run"].as_str().unwrap_or(""),
                f["isolated_gated"]["outcome"].as_str().unwrap_or(""),
                f["isolated_base"]["outcome"].as_str().unwrap_or(""),
                f["verdict"].as_str().unwrap_or("")
            ));
        }
        md.push('\n');
    }

    let base_prepare = r["base_prepare"].as_array().cloned().unwrap_or_default();
    if !base_prepare.is_empty() {
        section(&mut md, "Base-tree prepare", &base_prepare);
    }

    let conflicts = r["open_pr_conflicts"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    md.push_str("## Other open PRs\n\n");
    if let Some(err) = r["open_pr_conflicts_error"].as_str() {
        md.push_str(&format!("scan failed: {err}\n\n"));
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
}
