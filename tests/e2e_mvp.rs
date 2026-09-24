//! CAD-433: E2E acceptance tier A — the whole MVP journey in one run,
//! with fake providers and a headless board.
//!
//! Entry point: `scripts/e2e/mvp.sh`. It builds the release binary with
//! the embedded UI, packs it exactly as the release workflow does and
//! runs this test (`--features e2e`; the target is not built without it,
//! so the PR suite never sees it) with `CADENCE_E2E_DIST` pointing at
//! that local release. From there, in a sandbox under a short `/tmp`
//! root — its own HOME, XDG dirs, TMPDIR, tracker, state dir and a board
//! port in 3110-3199 — the operator:
//!
//!  1. installs the tarball with `scripts/install.sh` from `file://`;
//!  2. runs `cadence setup`, then `cadence project new demo --repo …`;
//!  3. signs the operator's browser tab in with a `cadence ui login`
//!     link (CAD-313) — the cookie plus the per-tab session key every
//!     later board write rides on;
//!  4. starts the master (`cadence master start`, confined by Landlock)
//!     and registers a worker `w1` and a reviewer `r1`;
//!  5. asks the master for work in the board's chat (Playwright);
//!  6. approves the master's plan on its plan card; the daemon's own
//!     wake (CAD-445 — no nudge) has the master dispatch to `w1`, whose
//!     question the master escalates and the operator answers on the
//!     board — routed back to `w1` (CAD-447), which commits and files
//!     `done` with a sha and a PR;
//!  7. `r1` passes it; the board's own delivery sync (CAD-446 — no
//!     `delivery sync` call) sees the green head, Merge enqueues it
//!     through the fake `gh`, and a later pass settles the merged
//!     ticket done (CAD-449) — the epic's stage and weighted progress
//!     track all of it (CAD-432);
//!  8. restarts the daemon, then comes back two hours "later" to the
//!     since-you-left card; along the way the master's provider
//!     compacts its context, so the routed question's turn opens with
//!     a continuity pack carrying the active plan (CAD-324).
//!
//! Every agent is `tests/e2e/fake-claude.py` (a scripted stream-json
//! stand-in for the Claude CLI) and `gh` is `tests/fixtures/fake-gh.py`
//! (shared with the CAD-431 integration test): no network, no model, no
//! credentials. The production daemon, `~/pm`, port 3010, the installed
//! binary and `~/.local/share/cadence` are never touched — every path is
//! under the sandbox root, and every command runs with a cleared env.
//!
//! Each MVP use case (design-plans/20260923-onboarding-master-agent/
//! PLAN.md) is at least one assertion; what is not built yet is an
//! explicit expected-skip naming its ticket, printed and written to
//! `acceptance.json`. On any outcome the daemon's event log, its logs,
//! the fakes' logs and the board screenshots land in
//! `CADENCE_E2E_ARTIFACTS`.

// A test binary never runs the CAD-308 reaper (only `daemon run` does),
// so its own spawns need not go through `cadence_agent::reaper`.
#![allow(clippy::disallowed_methods)]

use std::fs;
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tempfile::TempDir;

/// The whole journey's budget (the acceptance: under 10 minutes).
const BUDGET: Duration = Duration::from_secs(600);
/// The operator's first chat and the master's answer (fake-claude.py).
const ASK: &str = "plan a CSV export";
const PR: &str = "https://github.com/acme/demo/pull/1";

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn required_env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| {
        panic!("{name} is not set — run the journey through scripts/e2e/mvp.sh, which builds the release it installs")
    })
}

/// The sandbox board range — never 3010.
const PORTS: std::ops::RangeInclusive<u16> = 3110..=3199;
/// Setup attempts on a new port when the one picked was taken meanwhile.
const PORT_ATTEMPTS: usize = 8;

/// A port in [`PORTS`] that binds right now, not in `tried`. The scan
/// starts at a pid-derived offset, so concurrent journeys (and the other
/// lanes' boards in the same range) rarely race for one port; the race
/// that remains between this probe and `setup` binding is retried by
/// [`Journey::setup`].
fn free_port(tried: &[u16]) -> u16 {
    let n = PORTS.len() as u64;
    let start = (u64::from(std::process::id()) * 7919 + tried.len() as u64 * 13) % n;
    (0..n)
        .map(|i| *PORTS.start() + ((start + i) % n) as u16)
        .find(|p| !tried.contains(p) && TcpListener::bind(("127.0.0.1", *p)).is_ok())
        .expect("no free port in 3110-3199")
}

/// A program's absolute path on this process's own PATH — the board
/// steps run with the sandbox's PATH, which does not hold Node.
fn host_program(name: &str) -> PathBuf {
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .map(|d| d.join(name))
        .find(|p| p.is_file())
        .unwrap_or_else(|| panic!("{name} is not on PATH"))
}

/// One MVP use case's outcome, as `acceptance.json` records it.
struct Case {
    use_case: u8,
    what: &'static str,
    /// `None` is a pass; `Some(ticket)` an expected skip.
    skip: Option<&'static str>,
}

/// The sandbox and everything the journey learned so far.
struct Journey {
    root: TempDir,
    port: u16,
    artifacts: PathBuf,
    started: Instant,
    /// The operator's browser, spawned on the first board step.
    driver: Option<Board>,
    cases: Vec<Case>,
    steps: Vec<(String, f64)>,
}

impl Journey {
    fn new() -> Journey {
        let root = tempfile::Builder::new()
            .prefix("c433-")
            .tempdir_in("/tmp")
            .unwrap();
        for d in [
            "home", "state", "config", "data", "cache", "tmp", "bin", "gh", "fake", "logs", "repo",
        ] {
            fs::create_dir_all(root.path().join(d)).unwrap();
        }
        let artifacts = PathBuf::from(required_env("CADENCE_E2E_ARTIFACTS"));
        fs::create_dir_all(&artifacts).unwrap();
        let j = Journey {
            // CADENCE_E2E_PORT pins the first attempt (debugging, and
            // proving the taken-port retry); the default probes the range.
            port: std::env::var("CADENCE_E2E_PORT")
                .ok()
                .and_then(|p| p.parse().ok())
                .filter(|p| PORTS.contains(p))
                .unwrap_or_else(|| free_port(&[])),
            root,
            artifacts,
            started: Instant::now(),
            driver: None,
            cases: Vec::new(),
            steps: Vec::new(),
        };
        // The socket path must fit sun_path (107 bytes).
        let sock = j.state_dir().join("cadence.sock");
        assert!(sock.as_os_str().len() <= 107, "{}", sock.display());
        j.install_fakes();
        j
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.root.path().join(rel)
    }

    fn state_dir(&self) -> PathBuf {
        self.path("state/cadence")
    }

    fn pm_dir(&self) -> PathBuf {
        self.path("home/pm")
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// The board as the operator's browser must reach it: the session
    /// cookie lives on this name (CAD-313), and a request on
    /// 127.0.0.1 is outside every session.
    fn board_url(&self) -> String {
        format!("http://cadence-{}.localhost:{}", self.port, self.port)
    }

    /// The fake `claude` the setup check detects (version and a signed-in
    /// status exit code only), the scripted provider the daemon launches
    /// in its place, and the fake `gh` on the operator's PATH.
    fn install_fakes(&self) {
        let exe = |p: &Path, body: &str| {
            fs::write(p, body).unwrap();
            fs::set_permissions(p, fs::Permissions::from_mode(0o755)).unwrap();
        };
        exe(
            &self.path("bin/claude"),
            "#!/bin/sh\ncase \"$1 $2\" in\n'--version ') echo '2.0.0 (Claude Code)' ;;\n\
             'auth status') exit 0 ;;\n*) exit 2 ;;\nesac\n",
        );
        let fake = manifest_dir().join("tests/e2e/fake-claude.py");
        fs::copy(&fake, self.path("fake/fake-claude.py")).unwrap();
        let gh = manifest_dir().join("tests/fixtures/fake-gh.py");
        exe(&self.path("gh/gh"), &fs::read_to_string(gh).unwrap());
        self.set_gh(false);
    }

    /// The fake GitHub's checks flag. The PR's head is never stored:
    /// `head_from` has the fake `gh` ask the real repo for the ticket
    /// branch's tip, so the board's sync can never observe a stale head
    /// (the worker's commit IS the PR head — CAD-446).
    fn set_gh(&self, green: bool) {
        fs::write(
            self.path("gh/gh-state.json"),
            json!({
                "head_from": {"repo": self.path("repo"), "glob": "cadence/dem-2*"},
                "head": "0".repeat(40),
                "state": "OPEN", "green": green, "auto": false,
            })
            .to_string(),
        )
        .unwrap();
    }

    fn gh_log(&self) -> String {
        fs::read_to_string(self.path("gh/gh.log")).unwrap_or_default()
    }

    /// A command in the sandbox's clean environment — nothing of the
    /// host's cadence env, HOME or PATH reaches it.
    fn command(&self, program: &str) -> Command {
        let mut cmd = Command::new(program);
        cmd.env_clear()
            .current_dir(self.root.path())
            .env("HOME", self.path("home"))
            .env("XDG_STATE_HOME", self.path("state"))
            .env("XDG_CONFIG_HOME", self.path("config"))
            .env("XDG_DATA_HOME", self.path("data"))
            .env("XDG_CACHE_HOME", self.path("cache"))
            .env("TMPDIR", self.path("tmp"))
            .env("LANG", "C.UTF-8")
            .env(
                "PATH",
                format!(
                    "{}:{}:{}:/usr/local/bin:/usr/bin:/bin",
                    self.path("gh").display(),
                    self.path("home/.local/bin").display(),
                    self.path("bin").display()
                ),
            )
            // The daemon (started by `setup`) launches this for every
            // claude agent; it is a documented launch override, and the
            // master's confinement gets exactly the fake's dir to read
            // and its log dir to write — no `--unconfined`.
            .env(
                "CADENCE_CLAUDE_COMMAND",
                format!(
                    "python3 {} {}",
                    self.path("fake/fake-claude.py").display(),
                    self.path("logs").display()
                ),
            )
            .env("CADENCE_MASTER_CONFINE_READ", self.path("fake"))
            .env("CADENCE_MASTER_CONFINE_WRITE", self.path("logs"));
        cmd
    }

    /// `cadence <args>` as the operator: (ok, stdout JSON or the text).
    fn cadence(&self, args: &[&str]) -> (bool, Value) {
        let out = self.command("cadence").args(args).output().unwrap();
        let text = if out.stdout.is_empty() {
            String::from_utf8_lossy(&out.stderr).to_string()
        } else {
            String::from_utf8_lossy(&out.stdout).to_string()
        };
        let value = serde_json::from_str(text.trim()).unwrap_or(Value::String(text));
        (out.status.success(), value)
    }

    fn ok(&self, args: &[&str]) -> Value {
        let (ok, out) = self.cadence(args);
        assert!(ok, "cadence {args:?} failed: {out}");
        out
    }

    fn git(&self, args: &[&str]) -> String {
        let out = self
            .command("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// `cadence ui login --json`: the single-use link that signs one
    /// browser tab in (CAD-313), on the board's own name.
    fn ui_login(&self) -> String {
        let out = self.ok(&["ui", "login", "--json"]);
        let link = out["link"].as_str().unwrap_or_default().to_string();
        assert!(
            link.starts_with(&self.board_url()) && link.contains("/login#n="),
            "ui login --json: {out}"
        );
        link
    }

    /// One headless board step (tests/e2e/board.mjs). The browser is the
    /// operator's: started from this process, outside every agent — one
    /// Chrome, one context, one signed-in tab for the whole journey.
    fn board(&mut self, step: &str, args: Value) -> Value {
        let t = Instant::now();
        if self.driver.is_none() {
            self.driver = Some(Board::spawn(self));
        }
        let out = self.driver.as_mut().unwrap().step(step, &args);
        self.steps
            .push((format!("board {step}"), t.elapsed().as_secs_f64()));
        out
    }

    fn pass(&mut self, use_case: u8, what: &'static str) {
        println!("PASS  use case {use_case}: {what}");
        self.cases.push(Case {
            use_case,
            what,
            skip: None,
        });
    }

    fn expected_skip(&mut self, use_case: u8, what: &'static str, ticket: &'static str) {
        println!("SKIP  use case {use_case}: {what} — expected, not built yet ({ticket})");
        self.cases.push(Case {
            use_case,
            what,
            skip: Some(ticket),
        });
    }

    fn step_done(&mut self, name: &str, since: Instant) {
        self.steps
            .push((name.to_string(), since.elapsed().as_secs_f64()));
    }

    /// Poll `probe` until it answers `Some`, or fail naming `what`.
    fn wait<T>(&self, what: &str, secs: u64, mut probe: impl FnMut() -> Option<T>) -> T {
        let deadline = Instant::now() + Duration::from_secs(secs);
        loop {
            if let Some(v) = probe() {
                return v;
            }
            assert!(Instant::now() < deadline, "timed out after {secs}s: {what}");
            thread::sleep(Duration::from_millis(250));
        }
    }

    /// `cadence setup` on the journey's port. When that port was taken
    /// between the probe and the bind (another journey, another lane's
    /// board), the `ui` check fails naming it: pick the next free port
    /// and run setup again — bounded. Returns each check's status from
    /// the first attempt where it did not fail (setup is idempotent, so a
    /// retry reports what the failed attempt created as `ok`).
    fn setup(&mut self) -> (Vec<(String, String)>, String) {
        let mut tried = Vec::new();
        let mut merged: Vec<(String, String)> = Vec::new();
        let mut all = String::new();
        for _ in 0..PORT_ATTEMPTS {
            tried.push(self.port);
            let port = self.port.to_string();
            let (ok, out) = self.cadence(&["setup", "--json", "--no-open", "--port", &port]);
            let text = out.as_str().map(str::to_string).unwrap_or(out.to_string());
            all.push_str(&text);
            for c in text
                .lines()
                .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            {
                let name = c["check"].as_str().unwrap_or_default().to_string();
                let status = c["status"].as_str().unwrap_or_default().to_string();
                match merged.iter_mut().find(|(n, _)| *n == name) {
                    Some(slot) if slot.1 == "failed" => slot.1 = status,
                    Some(_) => {}
                    None => merged.push((name, status)),
                }
            }
            if ok {
                return (merged, all);
            }
            assert!(
                text.contains("is taken"),
                "setup failed, not over a taken port:\n{text}"
            );
            println!(
                "port {} was taken meanwhile — setup again on another",
                self.port
            );
            self.port = free_port(&tried);
        }
        panic!("setup found no free board port in {PORT_ATTEMPTS} attempts:\n{all}");
    }

    fn delivery(&self, issue: &str) -> Value {
        let out = self.ok(&["delivery", "ls"]);
        out["records"]
            .as_array()
            .and_then(|r| r.iter().find(|r| r["issue"] == issue).cloned())
            .unwrap_or(Value::Null)
    }

    fn thread_text(&self) -> String {
        self.thread_of("master")
    }

    fn thread_of(&self, alias: &str) -> String {
        let (_, out) = self.cadence(&["thread", "show", alias, "--limit", "500"]);
        out.to_string()
    }

    /// Everything a failed (or passed) run is judged by, copied out of
    /// the sandbox before it is deleted.
    fn collect(&self) {
        let copy = |from: PathBuf, name: &str| {
            if from.exists() {
                let _ = fs::copy(&from, self.artifacts.join(name));
            }
        };
        let state = self.state_dir();
        copy(state.join("daemon.log"), "daemon.log");
        copy(state.join("ui.log"), "ui.log");
        copy(state.join("delivery.json"), "delivery.json");
        copy(self.path("gh/gh.log"), "gh.log");
        if let Ok(entries) = fs::read_dir(self.path("logs")) {
            for e in entries.flatten() {
                let name = format!("fake-{}", e.file_name().to_string_lossy());
                copy(e.path(), &name);
            }
        }
        // The daemon's event log, read-only (never a recovery pass).
        let db = state.join("cadence.sqlite3");
        if let Ok(conn) =
            rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        {
            if let Ok(mut stmt) =
                conn.prepare("SELECT seq, at, alias, kind, payload FROM events ORDER BY seq")
            {
                let rows = stmt.query_map([], |r| {
                    Ok(json!({
                        "seq": r.get::<_, i64>(0)?,
                        "at": r.get::<_, f64>(1).unwrap_or_default(),
                        "alias": r.get::<_, String>(2)?,
                        "kind": r.get::<_, String>(3)?,
                        "payload": serde_json::from_str::<Value>(
                            &r.get::<_, String>(4).unwrap_or_default()
                        ).unwrap_or(Value::Null),
                    }))
                });
                if let Ok(rows) = rows {
                    let lines: Vec<String> = rows.flatten().map(|v| v.to_string()).collect();
                    let _ = fs::write(self.artifacts.join("events.jsonl"), lines.join("\n"));
                }
            }
        }
        let cases: Vec<Value> = self
            .cases
            .iter()
            .map(|c| {
                json!({"use_case": c.use_case, "what": c.what,
                       "status": if c.skip.is_some() { "expected-skip" } else { "pass" },
                       "ticket": c.skip})
            })
            .collect();
        let steps: Vec<Value> = self
            .steps
            .iter()
            .map(|(s, secs)| json!({"step": s, "secs": (secs * 10.0).round() / 10.0}))
            .collect();
        let _ = fs::write(
            self.artifacts.join("acceptance.json"),
            serde_json::to_string_pretty(&json!({
                "cases": cases, "steps": steps,
                "total_secs": self.started.elapsed().as_secs(),
            }))
            .unwrap_or_default(),
        );
    }
}

impl Drop for Journey {
    /// Collect the evidence, then stop the sandbox's board and daemon
    /// (and with it every agent) — pass or fail.
    fn drop(&mut self) {
        drop(self.driver.take());
        self.collect();
        let _ = self.command("cadence").args(["ui", "stop"]).output();
        let _ = self.command("cadence").args(["daemon", "stop"]).output();
    }
}

/// The operator's browser (tests/e2e/board.mjs), kept alive for the
/// whole journey: one Chrome, one context, one page — the signed-in tab
/// every board write rides on (CAD-313). Steps go in as one JSON line
/// each; each answers one JSON line. Anything it prints to stderr lands
/// in `board-driver.log` under the artifacts.
struct Board {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    stdout: std::io::BufReader<std::process::ChildStdout>,
    log: PathBuf,
}

impl Board {
    fn spawn(j: &Journey) -> Board {
        let dir = manifest_dir().join("tests/e2e");
        // The sandbox's clean env (HOME, XDG, TMPDIR): the browser's
        // profile and caches stay under the sandbox root.
        let mut cmd = j.command(host_program("node").to_str().unwrap());
        if let Some(chrome) = std::env::var_os("E2E_CHROME") {
            cmd.env("E2E_CHROME", chrome);
        }
        let log_path = j.artifacts.join("board-driver.log");
        let log = fs::File::create(&log_path).unwrap();
        let mut child = cmd
            .arg(dir.join("board.mjs"))
            .current_dir(&dir)
            .env("E2E_URL", j.board_url())
            .env("E2E_ARTIFACTS", j.artifacts.join("screens"))
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::from(log))
            .spawn()
            .unwrap();
        Board {
            stdin: child.stdin.take().unwrap(),
            stdout: std::io::BufReader::new(child.stdout.take().unwrap()),
            child,
            log: log_path,
        }
    }

    fn log_tail(&self) -> String {
        let text = fs::read_to_string(&self.log).unwrap_or_default();
        let tail: Vec<&str> = text.lines().rev().take(40).collect();
        tail.into_iter().rev().collect::<Vec<_>>().join("\n")
    }

    fn step(&mut self, step: &str, args: &Value) -> Value {
        use std::io::{BufRead, Write};
        let req = format!("{}\n", json!({"step": step, "args": args}));
        let sent = self
            .stdin
            .write_all(req.as_bytes())
            .and_then(|()| self.stdin.flush());
        assert!(
            sent.is_ok(),
            "board driver is gone before {step}:\n{}",
            self.log_tail()
        );
        let mut line = String::new();
        let read = self.stdout.read_line(&mut line);
        assert!(
            matches!(read, Ok(n) if n > 0),
            "board driver is gone at {step}:\n{}",
            self.log_tail()
        );
        let out: Value = serde_json::from_str(line.trim())
            .unwrap_or_else(|_| panic!("board driver answered badly at {step}: {line:?}"));
        assert_eq!(
            out["ok"],
            true,
            "board step {step} failed:\n{}",
            out["error"].as_str().unwrap_or_default()
        );
        out
    }
}

impl Drop for Board {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn mvp_journey_end_to_end() {
    let mut j = Journey::new();
    let dist = required_env("CADENCE_E2E_DIST");
    let tag = required_env("CADENCE_E2E_TAG");
    let version = required_env("CADENCE_E2E_VERSION");

    // ---- 1. Install: the local release through install.sh, file:// ----
    let t = Instant::now();
    let out = j
        .command("sh")
        .arg(manifest_dir().join("scripts/install.sh"))
        .args(["--version", &tag, "--base-url", &format!("file://{dist}")])
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "install.sh failed:\n{text}");
    assert!(text.contains("checksum ok"), "{text}");
    assert!(text.contains(&format!("ok: {version}")), "{text}");
    let link = j.path("home/.local/bin/cadence");
    let target = fs::read_link(&link).unwrap();
    assert_eq!(
        target,
        j.path(&format!("data/cadence/releases/{tag}/cadence")),
        "the release layout under the sandbox's XDG_DATA_HOME"
    );
    let (ok, v) = j.cadence(&["--version"]);
    assert!(
        ok && v.as_str().unwrap_or_default().trim() == version,
        "{v}"
    );
    j.step_done("install", t);
    j.pass(
        1,
        "install.sh installs the local release from file:// and verifies it",
    );

    // ---- 2. Set up: setup, then the project ----
    let t = Instant::now();
    let (checks, text) = j.setup();
    let port = j.port.to_string();
    let status = |name: &str| -> String {
        checks
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, s)| s.clone())
            .unwrap_or_else(|| panic!("no {name} check in:\n{text}"))
    };
    for created in ["state_dir", "tracker", "daemon", "ui"] {
        assert_eq!(status(created), "created", "{created}:\n{text}");
    }
    assert_eq!(
        status("claude"),
        "ok",
        "the fake claude is detected:\n{text}"
    );
    assert_eq!(status("master"), "missing", "{text}");
    assert!(
        text.contains(&j.url()),
        "setup names the sandbox board:\n{text}"
    );
    // Idempotent: a second run creates nothing.
    let (ok, again) = j.cadence(&["setup", "--json", "--no-open", "--port", &port]);
    let again = again.as_str().map(str::to_string).unwrap_or_default();
    assert!(ok && !again.contains("\"created\""), "{again}");
    // Questions reach the master at once (the default window a PM has is
    // 15 minutes): the operator's own tracker setting, one commit.
    let pm_yaml = j.pm_dir().join("pm.yaml");
    let mut yaml = fs::read_to_string(&pm_yaml).unwrap();
    yaml.push_str("host:\n  question_escalate_after_secs: 0\n");
    fs::write(&pm_yaml, yaml).unwrap();
    let pm = j.pm_dir().to_str().unwrap().to_string();
    j.git(&[
        "-C",
        &pm,
        "commit",
        "-qam",
        "host: route open questions at once",
    ]);
    // The repo the work lands in, with a GitHub remote for the PR.
    let repo = j.path("repo").to_str().unwrap().to_string();
    j.git(&["-C", &repo, "init", "-q", "-b", "main"]);
    fs::write(j.path("repo/README.md"), "# demo\n").unwrap();
    j.git(&["-C", &repo, "add", "-A"]);
    j.git(&["-C", &repo, "commit", "-qm", "init"]);
    j.git(&[
        "-C",
        &repo,
        "remote",
        "add",
        "origin",
        "https://github.com/acme/demo.git",
    ]);
    let project = j.ok(&["project", "new", "demo", "--repo", &repo]);
    assert_eq!(project["prefix"], "DEM", "{project}");
    assert_eq!(project["remote"], "github.com/acme/demo", "{project}");
    assert_eq!(project["committed"], true, "{project}");
    j.board("setup", json!({}));
    j.step_done("setup", t);
    j.pass(
        2,
        "cadence setup creates state, tracker, daemon and board, detects claude, is idempotent",
    );
    j.pass(
        2,
        "the /setup page lists its steps and the signed-in claude CLI",
    );
    j.pass(
        2,
        "cadence project new registers the repo (prefix DEM, GitHub remote)",
    );

    // ---- CAD-313: the operator's browser signs in — one tab, one
    // session (cookie + per-tab key). Every board write from here rides
    // on it. The link works once, and a tab that holds only the cookie
    // — never the exchanged key — writes nothing.
    let t = Instant::now();
    let link = j.ui_login();
    j.board("login", json!({"link": link}));
    j.board("login-replay", json!({"link": link}));
    j.board("unsigned-write", json!({}));
    j.step_done("operator sign-in", t);
    j.pass(
        2,
        "cadence ui login signs this tab in — every later board write carries its session; a link works once; a cookie-only tab writes nothing (CAD-313)",
    );

    // ---- 3. The master and its team ----
    let t = Instant::now();
    let master = j.ok(&["master", "start"]);
    assert_eq!(
        master["provider"], "claude",
        "AGENT.md's preferred provider: {master}"
    );
    assert_eq!(
        master["confined"], true,
        "Landlock confines the master: {master}"
    );
    for alias in ["w1", "r1"] {
        j.ok(&[
            "agent",
            "register",
            alias,
            "--provider",
            "claude",
            "--endpoint",
            "managed",
            "--cwd",
            &repo,
        ]);
    }
    for alias in ["master", "w1", "r1"] {
        j.wait(&format!("{alias} idle"), 60, || {
            let (_, show) = j.cadence(&["agent", "show", alias]);
            (show["agent"]["state"] == "idle").then_some(())
        });
    }
    j.step_done("master start + team", t);

    // ---- Use case 3: ask for work in chat → a plan card ----
    let t = Instant::now();
    j.board(
        "chat",
        json!({"text": ASK, "expect": "Proposed plan DEM-1"}),
    );
    let plan = j.ok(&["plan", "show", "DEM-1"]);
    assert_eq!(plan["plan"]["state"], "proposed", "{plan}");
    assert_eq!(plan["plan"]["proposed_by"], "master", "{plan}");
    // Adversarial: the master dispatched DEM-2 before the operator
    // approved. The plan gate refused it, by name, and nothing moved.
    let thread = j.thread_text();
    assert!(
        thread.contains("Early dispatch of DEM-2: REFUSED")
            && thread
                .contains("plan DEM-1 is proposed — approve it with `cadence plan approve DEM-1`"),
        "the plan gate must refuse a dispatch before approval: {thread}"
    );
    assert!(j.delivery("DEM-2").is_null(), "{}", j.delivery("DEM-2"));
    assert_eq!(
        j.ok(&["issue", "show", "DEM-2", "--json"])["status"],
        "backlog"
    );
    j.step_done("ask for work", t);
    j.pass(
        4,
        "the plan gate refuses the master's dispatch before approval, by name",
    );
    j.pass(
        3,
        "the operator's chat reaches the master; its reply and plan are in the thread",
    );

    // ---- Use case 4: approve on the plan card ----
    let t = Instant::now();
    let approved = j.board(
        "approve",
        json!({"epic": "DEM-1", "tickets": ["DEM-2", "DEM-3"]}),
    );
    assert_eq!(approved["progress"], "0", "{approved}");
    let plan = j.ok(&["plan", "show", "DEM-1"]);
    assert_eq!(plan["plan"]["state"], "approved", "{plan}");
    assert_eq!(plan["plan"]["decided_by"], "operator", "{plan}");
    j.step_done("approve", t);
    j.pass(4, "Approve on the plan card: plan approved by the operator, tickets ready, progress bar at 0%");
    // CAD-432: the epic reads the approved plan's stage — build — with
    // size-weighted progress and health on the Projects screen. (No
    // children: the daemon's wake may have DEM-2 dispatched already —
    // the child rows are read once the dispatch has settled, below.)
    j.board(
        "epics",
        json!({"project": "demo", "epic": "DEM-1",
               "stage": "build 2/5", "percent": "0", "label": "0/2 weight",
               "health": "at risk"}),
    );
    j.pass(
        4,
        "the approved plan's epic reads stage build with size-weighted progress and health (CAD-432)",
    );

    // ---- Use case 5: work starts — dispatch, the dependency gate ----
    let t = Instant::now();
    // CAD-445: the daemon's own wake — "[wake] plan DEM-1 approved …" —
    // reaches the master, which dispatches on it. No operator nudge.
    j.wait("the plan-approved wake dispatches DEM-2", 120, || {
        j.thread_text().contains("DEM-2: dispatched").then_some(())
    });
    let thread = j.thread_text();
    assert!(
        thread.contains("[wake] plan DEM-1 approved"),
        "the daemon's own wake is in the master's thread: {thread}"
    );
    assert!(
        thread.contains("DEM-3 depends on DEM-2"),
        "the gate holds DEM-3 until DEM-2 is done: {thread}"
    );
    let rec = j.delivery("DEM-2");
    assert_eq!(rec["worker"], "w1", "{rec}");
    let show = j.ok(&["issue", "show", "DEM-2", "--json"]);
    assert_eq!(show["status"], "doing", "{show}");
    let show = j.ok(&["issue", "show", "DEM-3", "--json"]);
    assert_eq!(
        show["status"], "ready",
        "the blocked ticket is still ready, not dispatched: {show}"
    );
    j.board(
        "watch",
        json!({"project": "demo", "issue": "DEM-2", "agent": "w1"}),
    );
    j.board(
        "epics",
        json!({"project": "demo", "epic": "DEM-1",
               "stage": "build 2/5", "percent": "0", "label": "0/2 weight",
               "health": "at risk",
               "children": {"DEM-2": "doing", "DEM-3": "blocked"}}),
    );
    j.step_done("dispatch", t);
    j.pass(
        5,
        "the master dispatches DEM-2 to w1 on the daemon's wake; DEM-3 waits on its dependency",
    );
    j.pass(
        5,
        "the project board shows the ticket and the agents screen its worker; the epic's stage and progress track the work",
    );

    // ---- Use case 6: a question, escalated by the master, answered ----
    let t = Instant::now();
    j.wait("the master escalates w1's question", 120, || {
        j.thread_text()
            .contains("Escalated DEM-2 to you: ok")
            .then_some(())
    });
    // CAD-324: the master's provider reported a context compaction on
    // its dispatch turn, so the routed question that followed opened
    // with a continuity pack — the thread records it went, with the
    // active plan it carried (counts, never content). A first-ever
    // kickoff cannot carry one: the worker's thread does not exist
    // until its operator chats it into being.
    j.wait(
        "the compacted master's next turn carried a pack",
        60,
        || {
            let t = j.thread_of("master");
            (t.contains("Continuity pack delivered") && t.contains("1 plan")).then_some(())
        },
    );
    j.board(
        "answer",
        json!({"issue": "DEM-2", "option": "comma", "summary": "I recommend comma"}),
    );
    let show = j.ok(&["issue", "show", "DEM-2", "--json"]);
    let reports = show["reports"].as_array().cloned().unwrap_or_default();
    assert!(
        reports
            .iter()
            .any(|r| r["kind"] == "answer" && r["agent"] == "operator"),
        "the operator's answer is on the ticket: {show}"
    );
    // CAD-447: the answer reaches the worker who asked — one `answer`
    // message to w1 naming the ticket, with the answer's text.
    let told = j.wait("w1 is told the operator's answer", 60, || {
        let (_, show) = j.cadence(&["agent", "show", "w1"]);
        let answers: Vec<Value> = show["messages"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|m| m["source"] == "answer")
            .collect();
        (!answers.is_empty()).then_some(answers)
    });
    assert_eq!(told.len(), 1, "one answer message: {told:?}");
    let body = told[0]["body"].as_str().unwrap_or_default();
    assert!(
        body.contains("DEM-2") && body.contains("comma"),
        "the answer message carries the ticket and the answer: {body}"
    );
    j.step_done("question", t);
    j.pass(6, "w1's question reaches Needs-you with the master's summary; the operator answers it on the board");
    j.pass(
        6,
        "the answer is routed back to w1 as one message (CAD-447)",
    );
    j.pass(
        6,
        "the compacted session's next turn opened with a continuity pack carrying its plan (CAD-324)",
    );
    j.expected_skip(
        6,
        "permission cards (a provider's tool approval) in Needs-you",
        "CAD-363",
    );

    // ---- Use case 7: done → independent PASS → merge ----
    let t = Instant::now();
    let rec = j.wait("r1's PASS", 120, || {
        let r = j.delivery("DEM-2");
        (r["state"] == "passed").then_some(r)
    });
    let sha = rec["head"].as_str().unwrap().to_string();
    assert_eq!(rec["pr"], PR, "{rec}");
    assert_eq!(rec["reviewer"], "r1", "{rec}");
    assert_ne!(
        rec["reviewer"], rec["worker"],
        "an independent reviewer: {rec}"
    );
    assert_eq!(
        rec["verdict"]["sha"], sha,
        "the verdict is pinned to the head: {rec}"
    );
    // The worker's sha is a real commit on the ticket's branch.
    let branch = j.git(&["-C", &repo, "branch", "--contains", &sha]);
    assert!(branch.contains("cadence/dem-2"), "{branch}");
    // CI goes green on that head; the board's own delivery sync reads
    // the PR on its timer or a page view — no `delivery sync` call —
    // and the observed head is the reviewed one (CAD-446).
    j.set_gh(true);
    let rec = j.wait("the board's sync sees the green head", 90, || {
        let r = j.delivery("DEM-2");
        (r["merge_ready"] == true).then_some(r)
    });
    assert_eq!(
        rec["observed"]["head"].as_str().unwrap_or_default(),
        sha,
        "the board observed the reviewed head: {rec}"
    );
    assert_eq!(rec["observed"]["ci_green"], true, "{rec}");
    assert!(
        !j.gh_log().contains("pr merge"),
        "nothing merged before the click"
    );
    // Adversarial: the worker presses the board's Merge itself, from a
    // child `curl` of its provider. The board refuses it as an agent's
    // request before any gh call; the loop stays `passed`.
    // `message ask` waits for the turn; the message record carries the
    // worker's reply — the daemon's evidence, not the fake's own log.
    let asked = j.ok(&[
        "message",
        "ask",
        "w1",
        "--text",
        &format!("MERGE_PROBE {} DEM-2", j.url()),
        "--wait",
        "60",
    ]);
    let probe = asked.to_string();
    assert!(probe.contains("MERGE_PROBE result"), "{probe}");
    assert!(
        probe.contains("HTTP 403") && probe.contains("operator_only"),
        "an agent's Merge must be refused 403 operator_only: {probe}"
    );
    assert!(
        !j.gh_log().contains("pr merge"),
        "an agent's Merge reached gh:\n{}",
        j.gh_log()
    );
    assert_eq!(j.delivery("DEM-2")["state"], "passed");
    j.board(
        "merge",
        json!({"issue": "DEM-2", "reviewer": "r1", "sha": sha, "pr": "acme/demo#1",
               "verdict": "PASS: the endpoint streams every record."}),
    );
    assert!(
        j.gh_log().contains(&format!(
            "pr merge 1 -R acme/demo --auto --squash --match-head-commit {sha}"
        )),
        "the board's Merge ran the operator's gh pinned to the reviewed head:\n{}",
        j.gh_log()
    );
    assert_eq!(j.delivery("DEM-2")["state"], "enqueued");
    // Auto-merge lands with checks green; the board's next sync pass
    // observes the PR merged and the ticket settles done (CAD-449) —
    // DEM-3's blocker lifts, the epic's progress moves.
    j.wait("the merge settles DEM-2 done", 90, || {
        (j.delivery("DEM-2")["state"] == "merged").then_some(())
    });
    assert_eq!(
        j.ok(&["issue", "show", "DEM-2", "--json"])["status"],
        "done",
        "the merged ticket settled done"
    );
    j.board(
        "epics",
        json!({"project": "demo", "epic": "DEM-1",
               "stage": "build 2/5", "percent": "50", "label": "1/2 weight",
               "health": "on track",
               "children": {"DEM-2": "done", "DEM-3": "ready"}}),
    );
    j.step_done("review + merge", t);
    j.pass(
        7,
        "the worker's own Merge on the board is refused 403 operator_only, before any gh call",
    );
    j.pass(7, "w1's done (sha + PR) goes to r1; PASS pinned to the head; Merge in Needs-you enqueues it via gh; the merged ticket settles done");

    // ---- Use case 8: come back ----
    let t = Instant::now();
    // The daemon restarts (a reboot, an upgrade): setup brings it back
    // and the master resumes its own session.
    let (_, before) = j.cadence(&["agent", "show", "master"]);
    let session = before["agent"]["session_id"].clone();
    j.ok(&["daemon", "stop"]);
    let (ok, out) = j.cadence(&["setup", "--json", "--no-open", "--port", &port]);
    assert!(ok, "{out}");
    j.wait("the master back after the restart", 60, || {
        let (_, show) = j.cadence(&["agent", "show", "master"]);
        (show["agent"]["state"] == "idle").then_some(())
    });
    let (_, after) = j.cadence(&["agent", "show", "master"]);
    assert_eq!(after["agent"]["session_id"], session, "{after}");
    j.board(
        "since",
        json!({
            "expect": ["Since you left", "DEM-1 CSV export — approved"],
            // question, answer, done, verdict: the card shows 3 rows and
            // "+1 more", in an order that is not stable within a second.
            "counts": {"Plans": 2, "Reports": 4},
            "thread": [ASK, "Proposed plan DEM-1", "DEM-2: dispatched"],
        }),
    );
    // Which reports the card counted, from the same summary it reads.
    let summary = j.ok(&["master", "summary", "--since", "24h"]);
    let reports = summary["reports"].as_array().cloned().unwrap_or_default();
    for (kind, agent) in [
        ("question", "w1"),
        ("answer", "operator"),
        ("done", "w1"),
        ("verdict", "r1"),
    ] {
        assert!(
            reports
                .iter()
                .any(|r| r["issue"] == "DEM-2" && r["kind"] == kind && r["agent"] == agent),
            "since-you-left lacks DEM-2 {kind} by {agent}: {summary}"
        );
    }
    j.step_done("come back", t);
    j.pass(
        8,
        "after a daemon restart the master resumes its session; the thread is intact",
    );
    j.pass(
        8,
        "a return two hours later shows since-you-left: the plan, the question, the done report",
    );

    // Every use case has at least one passing assertion.
    for uc in 1..=8u8 {
        assert!(
            j.cases.iter().any(|c| c.use_case == uc && c.skip.is_none()),
            "use case {uc} has no passing assertion"
        );
    }
    let total = j.started.elapsed();
    println!("journey: {:.0}s", total.as_secs_f64());
    assert!(
        total < BUDGET,
        "the journey took {total:?}, over {BUDGET:?}"
    );
}
