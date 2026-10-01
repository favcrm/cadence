//! app_source: CAD-970 R3 tests-first — a selected public Git bundle
//! resolves to an owned, descriptor-anchored UTF-8 snapshot plus its
//! source provenance, never a caller-reopened path.
//!
//! API under test (the contract the implementation lands):
//!
//!   SelectedGitSource::new(url: &str, commit: &str, dir: &str)
//!       -> Result<SelectedGitSource>
//!   app_source::resolve(&SelectedGitSource) -> Result<ResolvedGitBundle>
//!
//! `ResolvedGitBundle` carries `files: BTreeMap<String, String>`
//! (bundle-relative path → UTF-8 text) plus `url`, `commit` and `dir`
//! provenance, owning the clone TempDir internally — no PathBuf for a
//! caller to reopen.
//!
//! Test seam: a TEST-OWNED `git` wrapper lives at `<fixture>/bin/git`,
//! prepended to this test's own PATH inside an `in_own_process` child.
//! It logs the exact argv and the child env production handed it, then
//! delegates to the real git — substituting the fixture's https URL for
//! the owned local repo ONLY inside its own subprocess, where file
//! transport is allowed. Production argv and env are observed verbatim;
//! production never learns the local path exists.
// A test binary never runs the CAD-308 reaper (only `daemon run` does),
// so its own spawns need not go through `cadence_agent::reaper`.
#![allow(clippy::disallowed_methods)]
mod common;
use common::in_own_process;

use cadence_agent::issue::app_source::{resolve, SelectedGitSource};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

/// The one HTTPS repository URL the tests select. It never resolves —
/// the wrapper translates it to the owned local fixture inside the
/// wrapper's own subprocess; production validation still sees the real
/// https URL and the production transport list never gains file.
const FIXTURE_URL: &str = "https://fixture.invalid/apps-repo";

/// The bundle directory the fixture publishes: `bundles/app`.
const FIXTURE_DIR: &str = "bundles/app";

/// The deadline the private `timeout` wrapper pins into every git
/// step: `0.75` for the step under test and `0.1` for its TERM→KILL
/// escalation. The 0.75 bound must be applied ONLY at the matched
/// step's own call frame — blanket-rewriting every step's deadline
/// misattributes an earlier healthy phase's stall (under host load a
/// `clone` takes well over 0.75 s) to the phase actually under test.
const SHORT_STEP_DEADLINE: &str = "0.75";
const SHORT_KILL_AFTER: &str = "0.1";

fn fixture_git(dir: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgsign=false",
        ])
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn fixture_git_sha(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgsign=false",
        ])
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// One bundle file: `bundles/app/<rel>` = `text`.
fn put(repo: &Path, rel: &str, text: &str) {
    let p = repo.join(FIXTURE_DIR).join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

/// The minimal valid bundle: `app.md` (frontmatter + guide) and one
/// `workflows/<tag>.md`. Content mirrors the shape `app::validate`
/// accepts; the resolver itself only snapshots bytes.
fn put_minimal_bundle(repo: &Path, wf_text: &str) {
    put(
        repo,
        "app.md",
        "---\napp: git-app\ntitle: Git app\nversion: 0.1.0\n---\nGuide v1\n",
    );
    put(repo, "workflows/triage.md", wf_text);
}

/// A real, owned two-commit repo behind a test-owned `git` wrapper:
///   sha1 — bundle v1 (`wf-v1` in workflows/triage.md)
///   sha2 — whatever `edit2` plants (HEAD)
/// PATH is untouched until the test calls [`use_bin`].
struct GitFixture {
    _dir: TempDir,
    repo: PathBuf,
    bin: PathBuf,
    sha1: String,
    sha2: String,
    /// Base name of the wrapper's logs: `<log>.argv`, `<log>.env`.
    log: PathBuf,
}

impl GitFixture {
    fn new(edit2: impl FnOnce(&Path)) -> Self {
        let dir = TempDir::new().unwrap();
        let repo = dir.path().join("fixture-repo");
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&bin).unwrap();
        fixture_git(&repo, &["init", "-q", "-b", "main"]);
        fixture_git(&repo, &["config", "user.email", "t@t"]);
        fixture_git(&repo, &["config", "user.name", "t"]);
        put_minimal_bundle(&repo, "# triage\nwf-v1\n");
        fixture_git(&repo, &["add", "-A"]);
        fixture_git(&repo, &["commit", "-qm", "v1"]);
        let sha1 = fixture_git_sha(&repo, &["rev-parse", "HEAD"]);
        edit2(&repo);
        fixture_git(&repo, &["add", "-A"]);
        fixture_git(&repo, &["commit", "-qm", "v2"]);
        let sha2 = fixture_git_sha(&repo, &["rev-parse", "HEAD"]);
        let f = Self {
            log: dir.path().join("git-calls"),
            repo,
            bin,
            sha1,
            sha2,
            _dir: dir,
        };
        f.install_git_wrapper();
        f
    }

    /// The default second commit: same bundle shape, new content, plus
    /// a rubric that exists only at HEAD.
    fn basic() -> Self {
        Self::new(|repo| {
            put(repo, "workflows/triage.md", "# triage\nwf-v2\n");
            put(repo, "rubrics/quality.md", "rubric v2\n");
        })
    }

    /// The test-owned `git`: a Python script with an ABSOLUTE
    /// interpreter shebang (the resolver's env_clear'd child carries a
    /// minimal env, so `#!/usr/bin/env` cannot be relied on). It
    /// appends the verbatim argv as a JSON line and the observed env
    /// keys as a JSON line, then execs the real git — substituting the
    /// fixture https URL for `file://<owned repo>` only on the command
    /// that carries it, and only there widening the transport allowlist
    /// inside this private delegation.
    fn install_git_wrapper(&self) {
        let real_git = which("git");
        let python = which("python3");
        let script = format!(
            concat!(
                "#!{python}\n",
                "import json, os, subprocess, sys\n",
                "LOG = {log:?}\n",
                "URL = {url:?}\n",
                "REPO = {repo:?}\n",
                "REAL = {real:?}\n",
                "argv = sys.argv[1:]\n",
                "with open(LOG + '.argv', 'a') as fh:\n",
                "    fh.write(json.dumps(argv) + '\\n')\n",
                "keys = ['HOME', 'PATH', 'GIT_CONFIG_NOSYSTEM',\n",
                "        'GIT_CONFIG_GLOBAL', 'GIT_CONFIG_COUNT',\n",
                "        'GIT_CONFIG_PARAMETERS', 'GIT_TEMPLATE_DIR',\n",
                "        'GIT_TERMINAL_PROMPT', 'GIT_ALLOW_PROTOCOL',\n",
                "        'GIT_DIR', 'GIT_WORK_TREE', 'GIT_OBJECT_DIRECTORY',\n",
                "        'GIT_ALTERNATE_OBJECT_DIRECTORIES',\n",
                "        'GIT_SSH_COMMAND', 'GIT_PROXY_COMMAND']\n",
                "with open(LOG + '.env', 'a') as fh:\n",
                "    fh.write(json.dumps({{k: os.environ.get(k) for k in keys}}) + '\\n')\n",
                "delegated = 'clone' in argv and URL in argv\n",
                "sub = [('file://' + REPO) if a == URL else a for a in argv]\n",
                "env = dict(os.environ)\n",
                "if delegated:\n",
                "    # Owned-subprocess substitution ONLY: file transport\n",
                "    # is permitted here and nowhere else. The argv\n",
                "    # production sent — the https URL — is logged above.\n",
                "    env['GIT_ALLOW_PROTOCOL'] = 'https:file'\n",
                "sys.exit(subprocess.call([REAL] + sub, env=env))\n",
            ),
            python = python,
            log = self.log.display().to_string(),
            url = FIXTURE_URL,
            repo = self.repo.display().to_string(),
            real = real_git,
        );
        let git = self.bin.join("git");
        std::fs::write(&git, script).unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&git, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    /// Every argv the resolver handed `git`, one JSON array per line.
    fn argv_log(&self) -> String {
        std::fs::read_to_string(format!("{}.argv", self.log.display())).unwrap_or_default()
    }

    /// Every child env the resolver handed `git`, one JSON object per
    /// line — the verbatim observation of the scrubbed environment.
    fn env_log(&self) -> String {
        std::fs::read_to_string(format!("{}.env", self.log.display())).unwrap_or_default()
    }

    fn source(&self, commit: &str) -> SelectedGitSource {
        SelectedGitSource::new(FIXTURE_URL, commit, FIXTURE_DIR)
            .unwrap_or_else(|e| panic!("fixture source rejected: {e}"))
    }
}

fn which(cmd: &str) -> String {
    let out = std::process::Command::new("sh")
        .args(["-c", &format!("command -v {cmd}")])
        .output()
        .unwrap();
    let path = String::from_utf8(out.stdout).unwrap().trim().to_string();
    assert!(out.status.success() && !path.is_empty(), "no {cmd} on PATH");
    path
}

/// Run the resolver's `git` through this fixture's wrapper: prepend
/// `f.bin` to `orig` — the PATH this test inherited — so the wrapper
/// wins while every other executable still resolves. The resolver's
/// contract is that `git` is found through the caller-visible PATH;
/// a hardcoded child PATH would defeat the seam by design.
fn use_bin(f: &GitFixture, orig: &OsString) {
    let mut paths = vec![f.bin.clone()];
    paths.extend(std::env::split_paths(orig));
    std::env::set_var("PATH", std::env::join_paths(paths).unwrap());
}

/// Every recorded child-env line must show the scrub the plan pins:
/// none of the GIT_* inheritables, no foreign HOME, prompt off.
fn assert_env_scrubbed(log: &str, poison: &[&Path]) {
    assert!(!log.is_empty(), "git was never invoked");
    for line in log.lines() {
        let env: serde_json::Value = serde_json::from_str(line).unwrap();
        for key in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_OBJECT_DIRECTORY",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            "GIT_CONFIG_COUNT",
            "GIT_CONFIG_PARAMETERS",
            "GIT_SSH_COMMAND",
            "GIT_PROXY_COMMAND",
        ] {
            assert_eq!(
                env.get(key),
                Some(&serde_json::Value::Null),
                "{key}: {line}"
            );
        }
        for (key, value) in [
            ("GIT_TERMINAL_PROMPT", "0"),
            ("GIT_CONFIG_NOSYSTEM", "1"),
            ("GIT_ALLOW_PROTOCOL", "https"),
        ] {
            assert_eq!(env[key].as_str(), Some(value), "{key}: {line}");
        }
        for p in poison {
            let s = p.display().to_string();
            assert!(
                !line.contains(&s),
                "poisoned path {s} reached the child: {line}"
            );
        }
    }
}

/// The git argv fragment that uniquely selects one resolver phase.
/// `clone` is the only clone argv; `verify` is the
/// `rev-parse --verify <sha>^{commit}`; `checkout` is the detached
/// checkout; `head` is the bare `rev-parse HEAD`. `in argv` is
/// exact-element membership, so `--no-checkout` never matches
/// `'checkout'`, and the `head` classifier excludes `--verify` so the
/// verify step — whose argv also carries `rev-parse` — is never
/// selected as `head`.
fn phase_predicate(phase: &str) -> &'static str {
    match phase {
        "clone" => "'clone' in argv",
        "verify" => "'rev-parse' in argv and '--verify' in argv",
        "checkout" => "'checkout' in argv",
        "head" => "'rev-parse' in argv and 'HEAD' in argv and '--verify' not in argv",
        _ => unreachable!("{phase}"),
    }
}

/// Patch the fixture's `git` wrapper so invocations matching `phase`
/// stall `seconds` while ignoring SIGTERM — the production TERM must
/// escalate to a finite KILL. The stall is deterministic: it does not
/// depend on wall-clock jitter, only on the wrapper observing the
/// matched argv.
fn stall_phase(f: &GitFixture, phase: &str, seconds: u64) {
    let git = f.bin.join("git");
    let script = std::fs::read_to_string(&git).unwrap();
    let predicate = phase_predicate(phase);
    let stall = format!(
        "argv = sys.argv[1:]\nif {predicate}:\n    import signal, time\n    signal.signal(signal.SIGTERM, signal.SIG_IGN)\n    time.sleep({seconds})"
    );
    let patched = script.replace("argv = sys.argv[1:]", &stall);
    assert_ne!(patched, script, "git stall injection anchor missing");
    std::fs::write(&git, patched).unwrap();
}

/// Patch the fixture's `git` wrapper so invocations matching `phase`
/// take at least `seconds` of real work (a plain sleep, TERM
/// untouched) — a HEALTHY step slowed past the private test deadline,
/// never past the production one. Unlike [`stall_phase`] this does
/// not ignore SIGTERM; it models a well-behaved step under host load.
fn slow_phase(f: &GitFixture, phase: &str, seconds: u64) {
    let git = f.bin.join("git");
    let script = std::fs::read_to_string(&git).unwrap();
    let predicate = phase_predicate(phase);
    let slow =
        format!("argv = sys.argv[1:]\nif {predicate}:\n    import time\n    time.sleep({seconds})");
    let patched = script.replace("argv = sys.argv[1:]", &slow);
    assert_ne!(patched, script, "git slow injection anchor missing");
    std::fs::write(&git, patched).unwrap();
}

/// Install the private `timeout` wrapper in `f.bin`, bound to `phase`.
/// It logs the verbatim argv production sent (`timeout-argv.jsonl`
/// under the fixture dir), asserts the production `--kill-after=10 120
/// git` shape, then — at the matched step's own call frame ONLY —
/// execs the real `timeout` with the shortened `SHORT_STEP_DEADLINE`/
/// `SHORT_KILL_AFTER` pair. Every other phase's `timeout` call passes
/// through with its argv untouched: healthy steps keep the full
/// production 120 s deadline and 10 s kill-after, so an earlier
/// phase's slowness under host load can never be misreported as this
/// phase's timeout. Rewriting EVERY call's deadline —
/// [`install_timeout_wrapper_all_phases`] — is exactly the bug
/// `a_slow_earlier_phase_is_not_blamed_for_a_later_stall` demonstrates.
fn install_timeout_wrapper(f: &GitFixture, real_timeout: &str, python: &str, phase: &str) {
    let timeout_log = f._dir.path().join("timeout-argv.jsonl");
    let predicate = phase_predicate(phase);
    let timeout_script = format!(
        "#!{python}\nimport json, os, sys\nargv = sys.argv[1:]\nwith open({log}, 'a') as fh:\n    fh.write(json.dumps(argv) + '\\n')\nassert argv[:3] == ['--kill-after=10', '120', 'git'], argv\nif {predicate}:\n    # The selected step's own frame ONLY: shorten the deadline so the\n    # TERM->KILL escalation is exercised without waiting out the\n    # production bound. Other phases keep argv exactly as written.\n    argv = ['--kill-after={kill}', '{deadline}'] + argv[2:]\nos.execv({real}, [{real}] + argv)\n",
        log = serde_json::to_string(&timeout_log.to_str().unwrap()).unwrap(),
        real = serde_json::to_string(&real_timeout).unwrap(),
        kill = SHORT_KILL_AFTER,
        deadline = SHORT_STEP_DEADLINE,
        predicate = predicate,
    );
    let timeout = f.bin.join("timeout");
    std::fs::write(&timeout, timeout_script).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&timeout, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

/// The pre-fix `timeout` wrapper shape, kept ONLY as the witness's
/// counterexample: it shortens the deadline on EVERY git step, so a
/// healthy earlier phase slower than `SHORT_STEP_DEADLINE` eats the
/// kill meant for the selected phase and the error names the wrong
/// step. `a_slow_earlier_phase_is_not_blamed_for_a_later_stall` runs it
/// to prove that misattribution deterministically, then runs the fixed
/// wrapper to prove the selected phase is the one reported.
fn install_timeout_wrapper_all_phases(f: &GitFixture, real_timeout: &str, python: &str) {
    let timeout_log = f._dir.path().join("timeout-argv.jsonl");
    let timeout_script = format!(
        "#!{python}\nimport json, os, sys\nargv = sys.argv[1:]\nwith open({log}, 'a') as fh:\n    fh.write(json.dumps(argv) + '\\n')\nassert argv[:3] == ['--kill-after=10', '120', 'git'], argv\nos.execv({real}, [{real}, '--kill-after={kill}', '{deadline}'] + argv[2:])\n",
        log = serde_json::to_string(&timeout_log.to_str().unwrap()).unwrap(),
        real = serde_json::to_string(&real_timeout).unwrap(),
        kill = SHORT_KILL_AFTER,
        deadline = SHORT_STEP_DEADLINE,
    );
    let timeout = f.bin.join("timeout");
    std::fs::write(&timeout, timeout_script).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&timeout, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

// ---------------------------------------------------------------------
// Field validation — every refusal before git is ever invoked.
// ---------------------------------------------------------------------

/// Every invalid URL shape refuses at `new`; the wrapper's argv log
/// proves no process ever ran for it.
#[test]
fn invalid_url_refuses_before_any_git_invocation() {
    if !in_own_process("invalid_url_refuses_before_any_git_invocation", &[]) {
        return;
    }
    let orig_path = std::env::var_os("PATH").unwrap();
    let f = GitFixture::basic();
    use_bin(&f, &orig_path);
    let bad = [
        "",                                     // empty
        "http://fixture.invalid/repo",          // not https
        "ssh://git@fixture.invalid/repo",       // wrong scheme
        "fixture.invalid/repo",                 // no scheme
        "https://",                             // no host
        "https:///repo",                        // empty host
        "https://fixture.invalid",              // no repo path
        "https://fixture.invalid/",             // empty repo path
        "https://user@fixture.invalid/repo",    // userinfo
        "https://user:pw@fixture.invalid/repo", // userinfo+pw
        "https://fixture.invalid/repo?x=1",     // query
        "https://fixture.invalid/repo#frag",    // fragment
        "https://fixture.invalid/re%70o",       // percent in path
        "https://fixture.invalid/%2e%2e/x",     // percent alias
        "https://fix%74ure.invalid/repo",       // percent in host
        "https://fixture.invalid\\repo",        // backslash
        "https://fixture.invalid/re po",        // space
        "https://fixture.invalid/repo\n",       // control
        "HTTPS://fixture.invalid/repo",         // scheme is exact
        "git@fixture.invalid:repo",             // scp-style
        "file:///repo",                         // file transport
        "ext::sh -c 'touch /tmp/pwn'",          // executable transport
    ];
    for url in bad {
        assert!(
            SelectedGitSource::new(url, &f.sha1, FIXTURE_DIR).is_err(),
            "url accepted: {url:?}"
        );
    }
    // Over the exact 2048-byte cap refuses; at the cap is legal.
    let mut long = String::from("https://fixture.invalid/");
    while long.len() <= 2048 {
        long.push('r');
    }
    assert!(long.len() > 2048);
    assert!(SelectedGitSource::new(&long, &f.sha1, FIXTURE_DIR).is_err());
    let at = format!("https://fixture.invalid/{}", "r".repeat(2048 - 24));
    assert_eq!(at.len(), 2048);
    assert!(SelectedGitSource::new(&at, &f.sha1, FIXTURE_DIR).is_ok());
    assert_eq!(
        f.argv_log(),
        "",
        "git ran for a rejected url: {}",
        f.argv_log()
    );
}

/// Commit is exactly 40 lowercase hex — anything else refuses at `new`.
/// Bad values are fixed literals: no dependence on fixture content.
#[test]
fn invalid_commit_refuses_before_any_git_invocation() {
    if !in_own_process("invalid_commit_refuses_before_any_git_invocation", &[]) {
        return;
    }
    let orig_path = std::env::var_os("PATH").unwrap();
    let f = GitFixture::basic();
    use_bin(&f, &orig_path);
    let bad = [
        "".to_string(),
        "main".to_string(),
        "HEAD".to_string(),
        "HEAD~1".to_string(),
        "0123456789abcdef0123456789abcdef0123456".to_string(), // 39
        "0123456789abcdef0123456789abcdef0123456789".to_string(), // 41
        "0123456789abcdef0123456789abcdef0123456g".to_string(), // non-hex last
        "0123456789ABCDEF0123456789ABCDEF01234567".to_string(), // uppercase
        format!("{}\n", "0".repeat(40)),                       // trailing control
        " g23456789abcdef0123456789abcdef01234567".to_string(), // leading space
    ];
    for commit in bad {
        assert!(
            SelectedGitSource::new(FIXTURE_URL, &commit, FIXTURE_DIR).is_err(),
            "commit accepted: {commit:?}"
        );
    }
    assert_eq!(f.argv_log(), "", "git ran for a rejected commit");
}

/// The directory is a canonical relative bundle path: bounded
/// `[A-Za-z0-9_-]+` components; no dot/empty/absolute/backslash/
/// percent/control/unicode-alias components; ≤256 bytes total.
#[test]
fn invalid_dir_refuses_before_any_git_invocation() {
    if !in_own_process("invalid_dir_refuses_before_any_git_invocation", &[]) {
        return;
    }
    let orig_path = std::env::var_os("PATH").unwrap();
    let f = GitFixture::basic();
    use_bin(&f, &orig_path);
    let bad = [
        "", ".", "..", "/", "app/", "/app", "a//b", "a/./b", "a/../b", "x/..", "./app", "../app",
        "a\\b", "a%20b", "a b", "a.b", "a\tb", "app\n", "app\x01", "äpp", "app/./x", "app//x",
    ];
    for dir in bad {
        assert!(
            SelectedGitSource::new(FIXTURE_URL, &f.sha1, dir).is_err(),
            "dir accepted: {dir:?}"
        );
    }
    // Over the 256-byte cap refuses; at the cap is legal.
    let over = "a".repeat(257);
    assert!(SelectedGitSource::new(FIXTURE_URL, &f.sha1, &over).is_err());
    let at = "a".repeat(256);
    assert!(SelectedGitSource::new(FIXTURE_URL, &f.sha1, &at).is_ok());
    for ok in ["app", "bundles/app", "a-b_c/9", "a"] {
        assert!(
            SelectedGitSource::new(FIXTURE_URL, &f.sha1, ok).is_ok(),
            "dir refused: {ok:?}"
        );
    }
    assert_eq!(f.argv_log(), "", "git ran for a rejected dir");
}

// ---------------------------------------------------------------------
// Real resolution through the hermetic wrapper.
// ---------------------------------------------------------------------

/// Selecting commit1 while HEAD sits at commit2 must resolve commit1's
/// bytes — never whatever HEAD says — and the wrapper must observe the
/// production command shape: `git -c core.hooksPath=/dev/null -c
/// core.fsmonitor=false clone --no-checkout --no-tags -- <url> <dest>`,
/// `rev-parse --verify <sha>^{commit}`, `checkout --detach <sha>` and
/// an independent `rev-parse HEAD`. The https URL is what git was
/// asked for; the owned local path appears in NO argv line.
#[test]
fn selected_commit_resolves_its_own_bytes_and_provenance() {
    if !in_own_process("selected_commit_resolves_its_own_bytes_and_provenance", &[]) {
        return;
    }
    let orig_path = std::env::var_os("PATH").unwrap();
    let f = GitFixture::basic();
    use_bin(&f, &orig_path);

    let src = f.source(&f.sha1);
    let bundle = resolve(&src).unwrap_or_else(|e| panic!("resolve failed: {e}"));

    // Snapshot bytes are commit1's — HEAD (commit2) never consulted.
    let files: &BTreeMap<String, String> = &bundle.files;
    assert_eq!(files["workflows/triage.md"], "# triage\nwf-v1\n");
    assert!(files["app.md"].contains("version: 0.1.0"));
    assert!(
        !files.contains_key("rubrics/quality.md"),
        "v2 file leaked into the v1 snapshot"
    );
    assert!(files.keys().all(|k| !k.starts_with(".git")));
    // Provenance is the selected source, verbatim.
    assert_eq!(bundle.url, FIXTURE_URL);
    assert_eq!(bundle.commit, f.sha1);
    assert_eq!(bundle.dir, FIXTURE_DIR);
    assert_ne!(f.sha1, f.sha2, "fixture must have distinct commits");

    // Production argv, verbatim from the wrapper's log.
    let argv = f.argv_log();
    assert!(argv.contains("clone"), "{argv}");
    assert!(argv.contains("--no-checkout"), "{argv}");
    assert!(argv.contains("--no-tags"), "{argv}");
    assert!(!argv.contains("--depth"), "shallow clone: {argv}");
    assert!(argv.contains("\"--\""), "{argv}");
    assert!(argv.contains(FIXTURE_URL), "{argv}");
    assert!(argv.contains("core.hooksPath=/dev/null"), "{argv}");
    assert!(argv.contains("core.fsmonitor=false"), "{argv}");
    assert!(
        argv.contains(&format!("{}^{{commit}}", f.sha1)),
        "no rev-parse --verify of the selected commit: {argv}"
    );
    assert!(argv.contains("--detach"), "{argv}");
    assert!(argv.contains("checkout"), "{argv}");
    assert!(argv.contains("HEAD"), "{argv}");
    // The owned local path is wrapper-private — no argv names it.
    assert!(
        !argv.contains(&f.repo.display().to_string()),
        "the fixture's local path reached a production argv: {argv}"
    );
    assert_env_scrubbed(&f.env_log(), &[]);
}

/// A selected commit that no clone carries must fail clean — never
/// fall back to HEAD, never checkout a near-match.
#[test]
fn a_missing_commit_never_falls_back_to_head() {
    if !in_own_process("a_missing_commit_never_falls_back_to_head", &[]) {
        return;
    }
    let orig_path = std::env::var_os("PATH").unwrap();
    let f = GitFixture::basic();
    use_bin(&f, &orig_path);
    // A real-looking 40-hex SHA the fixture does not contain.
    let missing = "1".repeat(40);
    assert_ne!(missing, f.sha1);
    assert_ne!(missing, f.sha2);
    let src = f.source(&missing);
    let err = resolve(&src).unwrap_err();
    assert!(!err.to_string().is_empty());
    // The clone ran (the repo is real); nothing returned HEAD bytes.
    assert!(f.argv_log().contains("clone"), "{}", f.argv_log());
}

/// Inherited git state must never reach the child. The load-bearing
/// poison is live-fire: the fixture commits a root `.gitattributes`
/// (`*.md filter=pwn`) and the inherited env declares
/// `filter.pwn.smudge` = `touch <marker>` via BOTH env config channels.
/// If either `GIT_CONFIG_PARAMETERS` or the `GIT_CONFIG_COUNT` triplet
/// leaked, checkout runs the smudge on every `*.md` and the marker
/// appears. Scrubbed, the filter is undefined and non-required —
/// checkout passes clean. `.gitattributes` sits at the clone root,
/// never inside `bundles/app`.
#[test]
fn poisoned_inherited_git_state_never_reaches_the_child() {
    if !in_own_process("poisoned_inherited_git_state_never_reaches_the_child", &[]) {
        return;
    }
    let orig_path = std::env::var_os("PATH").unwrap();
    let poison = TempDir::new().unwrap();
    let marker = poison.path().join("SMUDGE-FIRED");

    let f = GitFixture::new(|repo| {
        put(repo, "workflows/triage.md", "# triage\nwf-v2\n");
        std::fs::write(repo.join(".gitattributes"), "*.md filter=pwn\n").unwrap();
    });
    use_bin(&f, &orig_path);

    // Live-fire poison through BOTH env config channels.
    let smudge = format!("touch {}", marker.display());
    std::env::set_var(
        "GIT_CONFIG_PARAMETERS",
        format!("'filter.pwn.smudge'='{smudge}'"),
    );
    std::env::set_var("GIT_CONFIG_COUNT", "1");
    std::env::set_var("GIT_CONFIG_KEY_0", "filter.pwn.required");
    std::env::set_var("GIT_CONFIG_VALUE_0", "true");

    // A poisoned global config file: url.insteadOf rewriting the
    // fixture URL to file:// (which the https-only allowlist refuses —
    // a leak here fails the clone, caught by the Ok assertion below).
    let global = poison.path().join("gitconfig");
    std::fs::write(
        &global,
        format!(
            "[url \"file://{}/\"]\n\tinsteadOf = https://fixture.invalid/\n\
             [credential]\n\thelper = !{}\n",
            poison.path().display(),
            smudge,
        ),
    )
    .unwrap();
    std::env::set_var("GIT_CONFIG_GLOBAL", &global);

    // A poisoned template dir carrying an executable post-checkout
    // hook. Defence in depth only: production's own
    // `-c core.hooksPath=/dev/null` pins hooks off even if the dir
    // leaked, so the marker assertion is secondary — the env log is
    // the proof the leak itself never happened.
    let template = poison.path().join("template");
    let hook = template.join("hooks/post-checkout");
    std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
    std::fs::write(&hook, format!("#!/bin/sh\n{smudge}\n")).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    std::env::set_var("GIT_TEMPLATE_DIR", &template);

    // Repo-context env vars: if leaked they misdirect every git call —
    // the clone and the checkouts fail, caught by the Ok assertion.
    std::env::set_var("GIT_DIR", &template);
    std::env::set_var("GIT_WORK_TREE", &template);
    std::env::set_var("GIT_OBJECT_DIRECTORY", &template);
    std::env::set_var("GIT_ALTERNATE_OBJECT_DIRECTORIES", &template);

    // Credentials/prompt/transports that must be pinned, not inherited.
    let foreign_home = poison.path().join("home");
    std::fs::create_dir_all(&foreign_home).unwrap();
    std::fs::write(
        foreign_home.join(".gitconfig"),
        "[filter \"pwn\"]\n\tsmudge = touch /tmp/home-smudge-pwn\n",
    )
    .unwrap();
    std::env::set_var("HOME", &foreign_home);
    std::env::set_var("GIT_SSH_COMMAND", &hook);
    std::env::set_var("GIT_PROXY_COMMAND", &hook);
    std::env::set_var("GIT_ALLOW_PROTOCOL", "file:ext");
    std::env::set_var("GIT_TERMINAL_PROMPT", "1");

    let src = f.source(&f.sha2);
    let bundle = resolve(&src).unwrap_or_else(|e| panic!("resolve under poison failed: {e}"));
    assert_eq!(bundle.commit, f.sha2);
    assert_eq!(bundle.files["workflows/triage.md"], "# triage\nwf-v2\n");
    assert!(
        !marker.exists(),
        "the poisoned smudge filter fired — inherited config leaked: {}",
        marker.display()
    );

    // The verbatim child env proves the scrub, every invocation.
    assert_env_scrubbed(
        &f.env_log(),
        &[poison.path(), &global, &template, &foreign_home],
    );
}

// ---------------------------------------------------------------------
// Snapshot containment — the bundle directory's own integrity.
// ---------------------------------------------------------------------

/// `bundles/app` itself a symlink → `repo/outside` (committed, carrying
/// sentinel content): refuse; the sentinel bytes are never returned.
#[test]
fn a_symlinked_selected_directory_refuses() {
    if !in_own_process("a_symlinked_selected_directory_refuses", &[]) {
        return;
    }
    let orig_path = std::env::var_os("PATH").unwrap();
    let f = GitFixture::new(|repo| {
        let outside = repo.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), "sentinel-outside\n").unwrap();
        // `bundles/app` -> `repo/outside`: ../../ from bundles/app.
        std::fs::remove_dir_all(repo.join(FIXTURE_DIR)).unwrap();
        std::os::unix::fs::symlink("../../outside", repo.join(FIXTURE_DIR)).unwrap();
    });
    use_bin(&f, &orig_path);
    let err = resolve(&f.source(&f.sha2)).unwrap_err();
    assert!(!err.to_string().contains("sentinel-outside"));
}

/// An ANCESTOR component (`bundles`) a symlink escaping the clone
/// entirely → an owned dir outside the repo holding a valid-looking
/// bundle plus a sentinel: refuse at the ancestor, sentinel never
/// returned.
#[test]
fn a_symlinked_ancestor_component_refuses() {
    if !in_own_process("a_symlinked_ancestor_component_refuses", &[]) {
        return;
    }
    let orig_path = std::env::var_os("PATH").unwrap();
    let escape = TempDir::new().unwrap();
    let bait = escape.path().join("app");
    std::fs::create_dir_all(bait.join("workflows")).unwrap();
    std::fs::write(
        bait.join("app.md"),
        "---\napp: bait\ntitle: Bait\nversion: 1\n---\nBait\n",
    )
    .unwrap();
    std::fs::write(bait.join("workflows/w.md"), "sentinel-bait\n").unwrap();
    std::fs::write(bait.join("sentinel.txt"), "sentinel-bait\n").unwrap();
    let target = escape.path().to_path_buf();

    let f = GitFixture::new(|repo| {
        std::fs::remove_dir_all(repo.join("bundles")).unwrap();
        std::os::unix::fs::symlink(&target, repo.join("bundles")).unwrap();
    });
    use_bin(&f, &orig_path);
    let err = resolve(&f.source(&f.sha2)).unwrap_err();
    assert!(!err.to_string().contains("sentinel-bait"));
}

/// A file inside the bundle a symlink → `repo/loot.txt` (committed):
/// refuse the member, never follow to sentinel bytes.
#[test]
fn a_symlinked_bundle_member_refuses() {
    if !in_own_process("a_symlinked_bundle_member_refuses", &[]) {
        return;
    }
    let orig_path = std::env::var_os("PATH").unwrap();
    let f = GitFixture::new(|repo| {
        put(repo, "workflows/triage.md", "# triage\nwf-v2\n");
        std::fs::write(repo.join("loot.txt"), "sentinel-loot\n").unwrap();
        // workflows/pwn.md -> repo/loot.txt: ../../../ up out of
        // bundles/app/workflows.
        std::os::unix::fs::symlink(
            "../../../loot.txt",
            repo.join(FIXTURE_DIR).join("workflows/pwn.md"),
        )
        .unwrap();
    });
    use_bin(&f, &orig_path);
    let err = resolve(&f.source(&f.sha2)).unwrap_err();
    assert!(!err.to_string().contains("sentinel-loot"));
}

/// The selected directory must exist as a real directory: a missing
/// path, a path that is a FILE (`bundles/app/app.md`), and an existing
/// dir that is not a bundle (`bundles`, holding only the nested `app/`)
/// all refuse.
#[test]
fn a_missing_or_non_directory_selection_refuses() {
    if !in_own_process("a_missing_or_non_directory_selection_refuses", &[]) {
        return;
    }
    let orig_path = std::env::var_os("PATH").unwrap();
    let f = GitFixture::new(|repo| {
        put(repo, "workflows/triage.md", "# triage\nwf-v2\n");
        std::fs::write(repo.join("bundles/file"), "regular file\n").unwrap();
    });
    use_bin(&f, &orig_path);
    for dir in ["no/such/dir", "bundles/file", "bundles"] {
        let src = SelectedGitSource::new(FIXTURE_URL, &f.sha2, dir)
            .unwrap_or_else(|e| panic!("dir shape {dir:?} must pass validation: {e}"));
        assert!(
            resolve(&src).is_err(),
            "selection {dir:?} resolved against the clone"
        );
    }
}

/// Top-level members outside `app.md`, `workflows/`, `rubrics/`,
/// `templates/` refuse — an executable script and a hidden file alike.
#[test]
fn foreign_top_level_entries_refuse() {
    if !in_own_process("foreign_top_level_entries_refuse", &[]) {
        return;
    }
    let orig_path = std::env::var_os("PATH").unwrap();
    let f = GitFixture::new(|repo| {
        put(repo, "workflows/triage.md", "# triage\nwf-v2\n");
        std::fs::write(repo.join(FIXTURE_DIR).join("evil.sh"), "echo pwn\n").unwrap();
        std::fs::write(repo.join(FIXTURE_DIR).join(".hidden"), "x\n").unwrap();
    });
    use_bin(&f, &orig_path);
    assert!(resolve(&f.source(&f.sha2)).is_err());
}

/// v0 bundle dirs are flat — a nested directory inside `workflows/`
/// refuses rather than being walked into.
#[test]
fn nested_directories_refuse() {
    if !in_own_process("nested_directories_refuse", &[]) {
        return;
    }
    let orig_path = std::env::var_os("PATH").unwrap();
    let f = GitFixture::new(|repo| {
        put(repo, "workflows/triage.md", "# triage\nwf-v2\n");
        put(repo, "workflows/nested/deep.md", "deep\n");
    });
    use_bin(&f, &orig_path);
    assert!(resolve(&f.source(&f.sha2)).is_err());
}

/// A non-regular member (a fifo) refuses — the snapshot carries real
/// files only.
#[test]
fn non_regular_members_refuse() {
    if !in_own_process("non_regular_members_refuse", &[]) {
        return;
    }
    let orig_path = std::env::var_os("PATH").unwrap();
    // Git cannot store a FIFO. The owned wrapper injects it into the
    // owned clone after a real checkout, before the snapshot is read.
    let f = GitFixture::basic();
    let wrapper = f.bin.join("git");
    let script = std::fs::read_to_string(&wrapper).unwrap();
    let injected = script.replace(
        "sys.exit(subprocess.call([REAL] + sub, env=env))",
        concat!(
            "status = subprocess.call([REAL] + sub, env=env)\n",
            "if status == 0 and 'checkout' in argv:\n",
            "    clone = argv[argv.index('-C') + 1] if '-C' in argv else os.getcwd()\n",
            "    fifo = os.path.join(clone, 'bundles/app/rubrics/pipe')\n",
            "    os.makedirs(os.path.dirname(fifo), exist_ok=True)\n",
            "    os.mkfifo(fifo)\n",
            "sys.exit(status)",
        ),
    );
    assert_ne!(injected, script, "fixture injection anchor missing");
    std::fs::write(&wrapper, injected).unwrap();
    use_bin(&f, &orig_path);
    assert!(resolve(&f.source(&f.sha2)).is_err());
}

// ---------------------------------------------------------------------
// Snapshot bounds — values pinned by source inspection of app.rs:
// MAX_FILES = 128, MAX_FILE_BYTES = plan::MAX_PLAN_BYTES = 256 KiB,
// MAX_APP_BYTES = 2 MiB (all private; parity is behavioural).
// ---------------------------------------------------------------------

/// More than 128 bundle files refuses.
#[test]
fn over_the_file_count_bound_refuses() {
    if !in_own_process("over_the_file_count_bound_refuses", &[]) {
        return;
    }
    let orig_path = std::env::var_os("PATH").unwrap();
    let f = GitFixture::new(|repo| {
        put(repo, "workflows/triage.md", "# triage\nwf-v2\n");
        for i in 0..130 {
            put(repo, &format!("templates/t{i}.md"), "x\n");
        }
    });
    use_bin(&f, &orig_path);
    assert!(resolve(&f.source(&f.sha2)).is_err());
}

/// One file over 256 KiB refuses.
#[test]
fn an_oversize_file_refuses() {
    if !in_own_process("an_oversize_file_refuses", &[]) {
        return;
    }
    let orig_path = std::env::var_os("PATH").unwrap();
    let f = GitFixture::new(|repo| {
        put(repo, "workflows/triage.md", "# triage\nwf-v2\n");
        put(repo, "templates/big.md", &"x".repeat(300 * 1024));
    });
    use_bin(&f, &orig_path);
    assert!(resolve(&f.source(&f.sha2)).is_err());
}

/// A non-UTF-8 member refuses — never lossy-converted into the map.
#[test]
fn a_non_utf8_member_refuses() {
    if !in_own_process("a_non_utf8_member_refuses", &[]) {
        return;
    }
    let orig_path = std::env::var_os("PATH").unwrap();
    let f = GitFixture::new(|repo| {
        put(repo, "workflows/triage.md", "# triage\nwf-v2\n");
        let p = repo.join(FIXTURE_DIR).join("rubrics/bin.md");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, [0xff, 0xfe, 0x00, 0x01]).unwrap();
    });
    use_bin(&f, &orig_path);
    assert!(resolve(&f.source(&f.sha2)).is_err());
}

/// `resolve` is a standalone library call: it writes no tracker record
/// or journal and never mutates the fixture repository — no worktree,
/// no `refs/cadence/*`, no extra entries. (Own-tempdir cleanup proof is
/// a remaining-coverage gap, recorded in the ready marker.)
#[test]
fn resolve_writes_no_tracker_state() {
    if !in_own_process("resolve_writes_no_tracker_state", &[]) {
        return;
    }
    let orig_path = std::env::var_os("PATH").unwrap();
    let f = GitFixture::basic();
    use_bin(&f, &orig_path);
    let before: Vec<String> = std::fs::read_dir(&f.repo)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    let src = f.source(&f.sha2);
    let bundle = resolve(&src).unwrap_or_else(|e| panic!("resolve failed: {e}"));
    assert_eq!(bundle.commit, f.sha2);
    let refs = fixture_git_sha(&f.repo, &["for-each-ref", "--format=%(refname)"]);
    assert!(!refs.contains("cadence"), "{refs}");
    let after: Vec<String> = std::fs::read_dir(&f.repo)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(before, after, "resolver mutated the fixture repo");
    drop(bundle);
}

#[test]
fn aggregate_bundle_bytes_refuse() {
    if !in_own_process("aggregate_bundle_bytes_refuse", &[]) {
        return;
    }
    let orig_path = std::env::var_os("PATH").unwrap();
    // Each file is below 256 KiB and count is below 128, but their
    // aggregate exceeds the independent 2 MiB snapshot bound.
    let f = GitFixture::new(|repo| {
        let body = "x".repeat(240 * 1024);
        for i in 0..9 {
            put(repo, &format!("templates/large-{i}.txt"), &body);
        }
    });
    use_bin(&f, &orig_path);
    assert!(resolve(&f.source(&f.sha2)).is_err());
}

#[test]
fn an_existing_blob_sha_refuses_as_a_commit() {
    if !in_own_process("an_existing_blob_sha_refuses_as_a_commit", &[]) {
        return;
    }
    let orig_path = std::env::var_os("PATH").unwrap();
    let f = GitFixture::basic();
    // This blob is reachable from HEAD and is included in a full
    // clone. Its refusal therefore exercises object type verification,
    // rather than the missing-object path.
    let blob = fixture_git_sha(
        &f.repo,
        &["rev-parse", &format!("{}:bundles/app/app.md", f.sha2)],
    );
    assert_ne!(blob, f.sha1);
    assert_ne!(blob, f.sha2);
    use_bin(&f, &orig_path);
    let source = SelectedGitSource::new(FIXTURE_URL, &blob, FIXTURE_DIR).unwrap();
    assert!(resolve(&source).is_err());
    assert!(f.argv_log().contains("clone"));
}

#[test]
fn non_utf8_member_name_refuses() {
    if !in_own_process("non_utf8_member_name_refuses", &[]) {
        return;
    }
    let orig_path = std::env::var_os("PATH").unwrap();
    let f = GitFixture::new(|repo| {
        put(repo, "workflows/triage.md", "# triage\nwf-v2\n");
        let dir = repo.join(FIXTURE_DIR).join("templates");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(OsString::from_vec(vec![b'x', 0xff])), "text\n").unwrap();
    });
    use_bin(&f, &orig_path);
    assert!(resolve(&f.source(&f.sha2)).is_err());
}

#[test]
fn resolved_bundle_owns_and_cleans_its_temporary_files() {
    if !in_own_process("resolved_bundle_owns_and_cleans_its_temporary_files", &[]) {
        return;
    }
    let orig_path = std::env::var_os("PATH").unwrap();
    let f = GitFixture::basic();
    let resolver_temp = f._dir.path().join("resolver-temp");
    std::fs::create_dir(&resolver_temp).unwrap();
    std::env::set_var("TMPDIR", &resolver_temp);
    use_bin(&f, &orig_path);

    let bundle = resolve(&f.source(&f.sha1)).unwrap();
    assert!(std::fs::read_dir(&resolver_temp).unwrap().count() > 0);
    assert!(bundle.files["workflows/triage.md"].contains("wf-v1"));
    drop(bundle);
    assert_eq!(std::fs::read_dir(&resolver_temp).unwrap().count(), 0);

    assert!(resolve(&f.source(&"1".repeat(40))).is_err());
    assert_eq!(std::fs::read_dir(&resolver_temp).unwrap().count(), 0);
}

/// A private timeout wrapper observes production's fixed deadline argv,
/// then shortens ONLY the selected phase's own call frame to the
/// TEST-ONLY deadline. Every other phase keeps the production
/// 120s/kill-after-10 argv untouched, so a healthy earlier phase that
/// is merely slow under host load can never be misreported as the
/// phase under test. The stalled git ignores TERM so the finite KILL
/// escalation fires inside the test deadline.
#[test]
fn every_git_step_has_a_finite_deadline() {
    if !in_own_process("every_git_step_has_a_finite_deadline", &[]) {
        return;
    }
    let orig_path = std::env::var_os("PATH").unwrap();
    let real_timeout = which("timeout");
    let python = which("python3");
    for phase in ["clone", "verify", "checkout", "head"] {
        std::env::set_var("PATH", &orig_path);
        let f = GitFixture::basic();
        stall_phase(&f, phase, 30);
        install_timeout_wrapper(&f, &real_timeout, &python, phase);
        use_bin(&f, &orig_path);
        let selected = SelectedGitSource::new(FIXTURE_URL, &f.sha1, FIXTURE_DIR).unwrap();
        let started = std::time::Instant::now();
        let error = resolve(&selected).unwrap_err().to_string();
        assert!(error.contains(&format!("git {phase} exceeded")), "{error}");
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
        let timeout_log = f._dir.path().join("timeout-argv.jsonl");
        let calls = std::fs::read_to_string(&timeout_log).unwrap();
        let expected_steps = match phase {
            "clone" => 1,
            "verify" => 2,
            "checkout" => 3,
            "head" => 4,
            _ => unreachable!(),
        };
        assert_eq!(calls.lines().count(), expected_steps);
        for call in calls.lines() {
            let args: Vec<String> = serde_json::from_str(call).unwrap();
            assert_eq!(&args[..3], &["--kill-after=10", "120", "git"]);
        }
    }
    std::env::set_var("PATH", orig_path);
}

/// Adversarial witness for the bounded deadline fixture, in two legs
/// over identical stalls: a HEALTHY earlier phase (`clone`, TERM-
/// respecting, only slowed — 2 s, well past the 0.75 s test deadline
/// yet far inside the production 120 s) precedes a `checkout` that
/// wedges ignoring TERM.
///
/// Leg A runs the pre-fix all-phase wrapper: the shortened deadline
/// hits the slowed clone first, so the error names `clone` — the
/// misattribution this correction exists to remove. Leg B runs the
/// fixed per-phase wrapper: the clone keeps its production 120 s
/// deadline and survives, checkout alone hits the short bound, and
/// the error names `checkout` — with the argv log proving every call
/// production made carried the untouched `--kill-after=10 120` pair.
#[test]
fn a_slow_earlier_phase_is_not_blamed_for_a_later_stall() {
    if !in_own_process("a_slow_earlier_phase_is_not_blamed_for_a_later_stall", &[]) {
        return;
    }
    let orig_path = std::env::var_os("PATH").unwrap();
    let real_timeout = which("timeout");
    let python = which("python3");

    // Leg A — the buggy all-phase wrapper reports the wrong phase.
    {
        let f = GitFixture::basic();
        slow_phase(&f, "clone", 2);
        stall_phase(&f, "checkout", 30);
        install_timeout_wrapper_all_phases(&f, &real_timeout, &python);
        use_bin(&f, &orig_path);
        let selected = SelectedGitSource::new(FIXTURE_URL, &f.sha1, FIXTURE_DIR).unwrap();
        let error = resolve(&selected).unwrap_err().to_string();
        // Deterministic misattribution: the healthy 2 s clone is what
        // hit the blanket 0.75 s deadline — never the wedged checkout.
        assert!(
            error.contains("git clone exceeded"),
            "the all-phase wrapper should have blamed clone: {error}"
        );
        assert!(
            !error.contains("git checkout exceeded"),
            "the buggy wrapper must not reach checkout: {error}"
        );
        let calls = std::fs::read_to_string(f._dir.path().join("timeout-argv.jsonl")).unwrap();
        assert_eq!(calls.lines().count(), 1, "clone died first: {calls}");
        let args: Vec<String> = serde_json::from_str(calls.lines().next().unwrap()).unwrap();
        assert_eq!(&args[..3], &["--kill-after=10", "120", "git"], "{calls}");
    }
    std::env::set_var("PATH", &orig_path);

    // Leg B — the fixed wrapper times out the selected phase only.
    {
        let f = GitFixture::basic();
        slow_phase(&f, "clone", 2);
        stall_phase(&f, "checkout", 30);
        install_timeout_wrapper(&f, &real_timeout, &python, "checkout");
        use_bin(&f, &orig_path);
        let selected = SelectedGitSource::new(FIXTURE_URL, &f.sha1, FIXTURE_DIR).unwrap();
        let started = std::time::Instant::now();
        let error = resolve(&selected).unwrap_err().to_string();
        // The blame lands on the stalled phase — never the slow one.
        assert!(
            error.contains("git checkout exceeded"),
            "blame misattributed: {error}"
        );
        assert!(
            !error.contains("git clone exceeded"),
            "the healthy slow clone took the blame: {error}"
        );
        // clone's 2 s + checkout's 0.75 s + escalation, inside 10 s.
        assert!(started.elapsed() < std::time::Duration::from_secs(10));

        // clone's production 120 s deadline is what let it survive its
        // 2 s: every recorded call carried the real argv.
        let calls = std::fs::read_to_string(f._dir.path().join("timeout-argv.jsonl")).unwrap();
        // clone, verify and checkout ran; checkout never returns, so
        // head is never reached.
        assert_eq!(calls.lines().count(), 3, "{calls}");
        for call in calls.lines() {
            let args: Vec<String> = serde_json::from_str(call).unwrap();
            assert_eq!(&args[..3], &["--kill-after=10", "120", "git"], "{call}");
        }
    }
    std::env::set_var("PATH", orig_path);
}
