//! Board e2e: the `cadence issue` CLI against a temp PM dir, and the
//! `cadence ui` HTTP server in-process — routes, host/method/id/traversal
//! rejection, and daemon-unreachable honesty.

// A test binary never runs the CAD-308 reaper (only `daemon run` does),
// so its own spawns need not go through `cadence_agent::reaper`.
#![allow(clippy::disallowed_methods)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use cadence_agent::issue::{board, plan, time, write, Pm};
use cadence_agent::store::Store;
use cadence_agent::ui;
use cadence_agent::{client, daemon};
use serde_json::{json, Value};
use tempfile::TempDir;

#[path = "support/operator.rs"]
mod op;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_cadence")
}

fn cli(pm: &Path, state: &Path, args: &[&str]) -> (bool, Value) {
    cli_env::<&str>(pm, state, args, &[])
}

fn cli_env<S: AsRef<str>>(
    pm: &Path,
    state: &Path,
    args: &[&str],
    env: &[(&str, S)],
) -> (bool, Value) {
    cli_run(pm, state, None, args, env)
}

/// `cli_env` run from `cwd` — `issue start` resolves the repo from it.
fn cli_dir(pm: &Path, state: &Path, cwd: &Path, args: &[&str]) -> (bool, Value) {
    cli_run::<&str>(pm, state, Some(cwd), args, &[])
}

fn cli_run<S: AsRef<str>>(
    pm: &Path,
    state: &Path,
    cwd: Option<&Path>,
    args: &[&str],
    env: &[(&str, S)],
) -> (bool, Value) {
    let mut cmd = Command::new(bin());
    cmd.arg("--state-dir")
        .arg(state)
        .args(args)
        .env("CADENCE_PM_DIR", pm)
        // The tracker's pre-commit hook runs `cadence` from PATH —
        // put the just-built binary first so its lint sees the ref
        // kinds this build writes.
        .env(
            "PATH",
            format!(
                "{}:{}",
                Path::new(bin()).parent().unwrap().display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        // Ambient aliases would leak into Actor: trailers and comment
        // authors — remove it so every env resolves to `operator`.
        .env_remove("CADENCE_ALIAS");
    if let Ok(home) = std::env::var("HOME") {
        cmd.env("HOME", home);
    }
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }
    for (k, v) in env {
        cmd.env(k, v.as_ref());
    }
    let out = cmd.output().unwrap();
    // Errors print their JSON to stderr; successes to stdout.
    let text = if out.stdout.is_empty() {
        String::from_utf8_lossy(&out.stderr).to_string()
    } else {
        String::from_utf8_lossy(&out.stdout).to_string()
    };
    let json: Value = serde_json::from_str(text.trim()).unwrap_or_else(|_| {
        panic!(
            "not json: {text} (stderr: {})",
            String::from_utf8_lossy(&out.stderr)
        )
    });
    (out.status.success(), json)
}

/// `cadence …` for verbs that print plain text instead of JSON
/// (`issue trailer`). Returns (ok, stdout-or-stderr).
fn cli_raw(pm: &Path, state: &Path, args: &[&str]) -> (bool, String) {
    cli_raw_env(pm, state, args, &[])
}

fn cli_raw_env(pm: &Path, state: &Path, args: &[&str], env: &[(&str, String)]) -> (bool, String) {
    let mut cmd = Command::new(bin());
    cmd.arg("--state-dir")
        .arg(state)
        .args(args)
        .env("CADENCE_PM_DIR", pm)
        .env_remove("CADENCE_ALIAS");
    if let Ok(home) = std::env::var("HOME") {
        cmd.env("HOME", home);
    }
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    let text = if out.stdout.is_empty() {
        String::from_utf8_lossy(&out.stderr).to_string()
    } else {
        String::from_utf8_lossy(&out.stdout).to_string()
    };
    (out.status.success(), text.trim().to_string())
}

fn commits(pm: &Path) -> usize {
    let out = Command::new("git")
        .arg("-C")
        .arg(pm)
        .args(["rev-list", "--count", "HEAD"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .unwrap_or(0)
}

/// `git -C <pm> <args>` stdout — the tracker assertion helper. Raw
/// bytes: `status --porcelain` lines start with a space for unstaged
/// entries, so no trimming.
fn pm_git(pm: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(pm)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// `git status --porcelain` as one line per entry — XY codes intact.
fn status_lines(pm: &Path) -> Vec<String> {
    pm_git(pm, &["status", "--porcelain"])
        .lines()
        .map(str::to_string)
        .collect()
}

/// The repo-relative paths HEAD's commit changed, sorted.
fn head_paths(pm: &Path) -> Vec<String> {
    let out = pm_git(pm, &["show", "--pretty=format:", "--name-only", "HEAD"]);
    let mut paths: Vec<String> = out
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    paths.sort();
    paths
}

/// HEAD's commit touched exactly `want` (repo-relative).
fn assert_head_paths(pm: &Path, want: &[&str], what: &str) {
    let got = head_paths(pm);
    let mut want: Vec<String> = want.iter().map(|s| s.to_string()).collect();
    want.sort();
    assert_eq!(got, want, "{what}");
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Minimal blocking HTTP/1.0 client — enough for assertions without a
/// client dependency.
fn http_full(port: u16, method: &str, path: &str, host: &str) -> (u16, String, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(s, "{method} {path} HTTP/1.0\r\nHost: {host}\r\n\r\n").unwrap();
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).unwrap();
    // Bodies may be binary (png artifacts) — lossy-decode for asserts.
    let buf = String::from_utf8_lossy(&raw).to_string();
    let status = buf
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    let mut parts = buf.splitn(2, "\r\n\r\n");
    let headers = parts.next().unwrap_or("").to_string();
    let body = parts.next().unwrap_or("").to_string();
    (status, headers, body)
}

fn http(port: u16, method: &str, path: &str, host: &str) -> (u16, String) {
    let (status, _, body) = http_full(port, method, path, host);
    (status, body)
}

/// A write request: extra headers plus a raw body. `(status, headers,
/// body)` — headers joined so tests can assert on what is (not) sent.
fn http_write(
    port: u16,
    method: &str,
    path: &str,
    host: &str,
    headers: &[&str],
    body: &[u8],
) -> (u16, String, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    let mut req = format!("{method} {path} HTTP/1.0\r\nHost: {host}\r\n");
    for h in headers {
        req.push_str(h);
        req.push_str("\r\n");
    }
    req.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
    s.write_all(req.as_bytes()).unwrap();
    s.write_all(body).unwrap();
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).unwrap();
    let buf = String::from_utf8_lossy(&raw).to_string();
    let status = buf
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    let mut parts = buf.splitn(2, "\r\n\r\n");
    let head = parts.next().unwrap_or("").to_string();
    (status, head, parts.next().unwrap_or("").to_string())
}

/// The headers a legitimate board write always carries.
const WRITE_HEADERS: &[&str] = &[
    "Content-Type: application/json",
    "X-Cadence-Board: 1",
    "Sec-Fetch-Site: same-origin",
];

fn write_json(
    port: u16,
    method: &str,
    path: &str,
    host: &str,
    body: &str,
) -> (u16, String, String) {
    http_write(port, method, path, host, WRITE_HEADERS, body.as_bytes())
}

/// Stops an in-process board when dropped (CAD-471): the board closes
/// its port instead of serving for the rest of the run — a leaked one
/// outlives its temp dirs, and any client that finds its port pins one
/// server thread per idle connection until the runner exits.
struct BoardStop(std::sync::Arc<std::sync::atomic::AtomicBool>);

impl Drop for BoardStop {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Sign in to the board on `port` as the operator (CAD-313) — the real
/// `cadence ui login` link exchanged at `POST /api/session`. The daemon
/// on `state` must be up: it is the session authority.
fn sign_in(state: &Path, port: u16) -> op::Session {
    op::sign_in(bin(), state, port)
}

/// [`http_write`] as the signed-in operator `op`: to `port`'s own board
/// Host with the session cookie, plus that Host's `Origin` unless
/// `headers` set one (a cookie-bearing write must say where it comes
/// from). `host` is replaced: sessions live on the board's name only.
fn op_http_write(
    op: &op::Session,
    port: u16,
    method: &str,
    path: &str,
    host: &str,
    headers: &[&str],
    body: &[u8],
) -> (u16, String, String) {
    // A session lives on the board's own Host only (CAD-313): the
    // signed-in write goes to `port`'s own name, whatever `host` the
    // caller used; an `Origin` the caller set on purpose is kept.
    let _ = host;
    let own = op::board_host(port);
    let cookie = format!("Cookie: {}", op.cookie);
    let key = op.key_header();
    let origin = format!("Origin: http://{own}");
    let mut all: Vec<&str> = headers.to_vec();
    if !headers
        .iter()
        .any(|h| h.to_ascii_lowercase().starts_with("origin:"))
    {
        all.push(&origin);
    }
    all.push(&cookie);
    all.push(&key);
    http_write(port, method, path, &own, &all, body)
}

/// [`write_json`] as the signed-in operator `op`.
fn op_write_json(
    op: &op::Session,
    port: u16,
    method: &str,
    path: &str,
    host: &str,
    body: &str,
) -> (u16, String, String) {
    op_http_write(op, port, method, path, host, WRITE_HEADERS, body.as_bytes())
}

/// Spawn `ui::serve` on a free port and wait for health. The caller owns
/// the TempDirs keeping the pm/state dirs alive, and the returned
/// [`BoardStop`] keeping the board up. `free_port` is a bind-release
/// race — a parallel test may grab the port first, so a failed start
/// retries on a fresh port.
fn start_ui(pm_dir: PathBuf, state_dir: PathBuf) -> (u16, BoardStop) {
    start_ui_opts(pm_dir, state_dir, |_| {})
}

/// `start_ui` with `ServeOpts` overrides (`f` runs after the free port
/// is chosen — the port itself is always the probe-verified one).
fn start_ui_opts(
    pm_dir: PathBuf,
    state_dir: PathBuf,
    f: impl Fn(&mut ui::ServeOpts) + Send + Sync + 'static,
) -> (u16, BoardStop) {
    let f = std::sync::Arc::new(f);
    let overall = Instant::now() + Duration::from_secs(20);
    loop {
        let port = free_port();
        let (sd, pd, f) = (state_dir.clone(), pm_dir.clone(), f.clone());
        // A start that loses its port race is stopped too.
        let board = BoardStop(Default::default());
        let stop = board.0.clone();
        thread::spawn(move || {
            let mut opts = ui::ServeOpts {
                host: "127.0.0.1".to_string(),
                port,
                ..Default::default()
            };
            f(&mut opts);
            opts.port = port;
            opts.stop = Some(stop);
            let _ = ui::serve(&sd, &pd, &opts);
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        // Host carries the port, so a stolen port's foreign server
        // rejects this probe (its allowlist names ITS port) — 200
        // only ever comes from OUR server.
        loop {
            if let Ok(mut s) = TcpStream::connect(("127.0.0.1", port)) {
                s.set_read_timeout(Some(Duration::from_secs(2))).ok();
                let probe = format!("GET /api/health HTTP/1.0\r\nHost: 127.0.0.1:{port}\r\n\r\n");
                let _ = s.write_all(probe.as_bytes());
                let mut buf = String::new();
                if s.read_to_string(&mut buf).is_ok() && buf.contains("200") {
                    return (port, board);
                }
            }
            if Instant::now() >= deadline {
                break; // port was likely stolen — retry on another
            }
            assert!(Instant::now() < overall, "ui server did not start");
            thread::sleep(Duration::from_millis(50));
        }
    }
}

fn seed(pm: &Path, state: &Path) {
    assert!(cli(pm, state, &["issue", "init"]).0);
    assert!(
        cli(
            pm,
            state,
            &["issue", "project", "add", "cadence", "--prefix", "CAD"]
        )
        .0
    );
    assert!(
        cli(
            pm,
            state,
            &["issue", "new", "root task", "--project", "cadence"]
        )
        .0
    );
    assert!(
        cli(
            pm,
            state,
            &[
                "issue",
                "new",
                "child task",
                "--project",
                "cadence",
                "--parent",
                "CAD-1"
            ]
        )
        .0
    );
    assert!(
        cli(
            pm,
            state,
            &["issue", "new", "sibling", "--project", "cadence"]
        )
        .0
    );
    assert!(
        cli(
            pm,
            state,
            &["issue", "set", "CAD-2", "status=doing", "owner=me"]
        )
        .0
    );
}

#[test]
fn issue_cli_end_to_end() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();

    // Missing pm dir fails closed.
    let (ok, err) = cli(pm.path(), state.path(), &["issue", "ls"]);
    assert!(!ok);
    assert!(err["error"].as_str().unwrap().contains("issue init"));

    seed(pm.path(), state.path());
    let base = commits(pm.path());

    // Every write is exactly one commit.
    for args in [
        vec!["issue", "set", "CAD-2", "status=doing", "owner=you"],
        vec!["issue", "link", "CAD-3", "blocked_by", "CAD-2"],
        vec!["issue", "comment", "CAD-3", "-m", "hi", "--author", "t"],
        vec!["issue", "ref", "CAD-3", "commit", "abc123"],
        vec!["issue", "unlink", "CAD-3", "blocked_by", "CAD-2"],
    ] {
        let before = commits(pm.path());
        let (ok, _) = cli(pm.path(), state.path(), &args);
        assert!(ok, "{args:?} failed");
        assert_eq!(commits(pm.path()), before + 1, "{args:?} != 1 commit");
    }
    assert!(commits(pm.path()) > base);

    // attach honours the size cap.
    let big = pm.path().join("big.bin");
    std::fs::write(&big, vec![0u8; 1_048_577]).unwrap();
    let (ok, err) = cli(
        pm.path(),
        state.path(),
        &["issue", "attach", "CAD-3", big.to_str().unwrap()],
    );
    assert!(!ok);
    assert!(err["error"].as_str().unwrap().contains("cap"));

    // Link validation: dangling, cycle, depth-3.
    let (ok, err) = cli(
        pm.path(),
        state.path(),
        &["issue", "link", "CAD-3", "blocked_by", "CAD-99"],
    );
    assert!(!ok && err["error"].as_str().unwrap().contains("CAD-99"));
    // Re-link CAD-3 → CAD-2 (the loop above unlinked it), then the
    // reverse edge must be rejected as a cycle.
    assert!(
        cli(
            pm.path(),
            state.path(),
            &["issue", "link", "CAD-3", "blocked_by", "CAD-2"]
        )
        .0
    );
    let (ok, err) = cli(
        pm.path(),
        state.path(),
        &["issue", "link", "CAD-2", "blocked_by", "CAD-3"],
    );
    assert!(!ok && err["error"].as_str().unwrap().contains("cycle"));
    let (ok, err) = cli(
        pm.path(),
        state.path(),
        &[
            "issue",
            "new",
            "grandchild",
            "--project",
            "cadence",
            "--parent",
            "CAD-2",
        ],
    );
    assert!(!ok && err["error"].as_str().unwrap().contains("depth"));

    // Views: rollup, blocked, inverse.
    let issues = board::load_all(pm.path(), None).unwrap();
    let views = board::views(Path::new("/no-notes"), issues);
    let v = |id: &str| views.iter().find(|v| v.issue.front.id == id).unwrap();
    assert_eq!(v("CAD-1").status, "doing"); // child doing → rollup
    assert_eq!(v("CAD-1").status_source, "rollup");
    assert!(v("CAD-1").container);
    assert!(v("CAD-3").blocked); // blocked_by CAD-2 (doing)
    assert_eq!(v("CAD-2").blocks, vec!["CAD-3"]);

    // lint is clean.
    let (ok, lint) = cli(pm.path(), state.path(), &["issue", "lint"]);
    assert!(ok);
    assert_eq!(lint["ok"], true);

    // status=ready while a blocker is open → warning, not an error.
    assert!(
        cli(
            pm.path(),
            state.path(),
            &["issue", "set", "CAD-3", "status=ready"]
        )
        .0
    );
    let (ok, lint) = cli(pm.path(), state.path(), &["issue", "lint"]);
    assert!(ok);
    assert_eq!(lint["ok"], true);
    assert!(lint["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|w| w.as_str().unwrap().contains("CAD-3")));
}

#[test]
fn issue_acceptance_round_trip_and_refusals() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    seed(pm.path(), state.path());
    let source_dir = TempDir::new().unwrap();
    let source = source_dir.path().join("acceptance.md");
    std::fs::write(
        &source,
        "- [X] first user outcome\r\n\r\n- [ ] second user outcome\r\n",
    )
    .unwrap();

    let before = commits(pm.path());
    let (ok, out) = cli(
        pm.path(),
        state.path(),
        &[
            "issue",
            "acceptance",
            "CAD-3",
            "--from",
            source.to_str().unwrap(),
        ],
    );
    assert!(ok, "{out}");
    assert_eq!(commits(pm.path()), before + 1);
    assert_eq!(out["acceptance"][0]["text"], "first user outcome");
    assert_eq!(out["acceptance"][0]["checked"], true);
    assert_eq!(out["acceptance"][1]["done"], false);
    assert!(head_message(pm.path()).contains("CAD-3: acceptance replaced"));

    let (ok, shown) = cli(
        pm.path(),
        state.path(),
        &["issue", "show", "CAD-3", "--json"],
    );
    assert!(ok, "{shown}");
    assert_eq!(
        shown["acceptance"],
        json!([
            {
                "text": "first user outcome",
                "checked": true,
                "done": true
            },
            {
                "text": "second user outcome",
                "checked": false,
                "done": false
            }
        ])
    );

    // Readback stays scoped to the unique section while legacy checks remain
    // the global compatibility count.
    let issue_md = pm.path().join("cadence/CAD-3/issue.md");
    let original = std::fs::read_to_string(&issue_md).unwrap();
    let marker = "\n---\n\n";
    let body_start = original.find(marker).unwrap() + marker.len();
    let body = concat!(
        "Before acceptance\n",
        "- [ ] unrelated checkbox\n",
        "## Acceptance\n",
        "- [x] scoped outcome\n",
        "\x60\x60\x60markdown\n",
        "- [x] fenced example\n",
        "\x60\x60\x60\n",
        "## Notes\n",
        "Unrelated notes stay intact.\n",
        "- [ ] unrelated after\n",
    );
    std::fs::write(&issue_md, format!("{}{}", &original[..body_start], body)).unwrap();
    let (ok, shown) = cli(
        pm.path(),
        state.path(),
        &["issue", "show", "CAD-3", "--json"],
    );
    assert!(ok, "{shown}");
    assert_eq!(
        shown["acceptance"],
        json!([{
            "text": "scoped outcome",
            "checked": true,
            "done": true
        }])
    );
    assert_eq!(shown["checks"], json!({"done": 2, "total": 4}));

    // Duplicate sections refuse before save or commit.
    let duplicate = concat!(
        "## Acceptance\n",
        "- [ ] first\n",
        "## Acceptance\n",
        "- [ ] duplicate\n",
    );
    std::fs::write(
        &issue_md,
        format!("{}{}", &original[..body_start], duplicate),
    )
    .unwrap();
    let duplicate_bytes = std::fs::read(&issue_md).unwrap();
    let before = commits(pm.path());
    let (ok, err) = cli(
        pm.path(),
        state.path(),
        &[
            "issue",
            "acceptance",
            "CAD-3",
            "--from",
            source.to_str().unwrap(),
        ],
    );
    assert!(
        !ok && err["error"].as_str().unwrap().contains("duplicate"),
        "{err}"
    );
    assert_eq!(commits(pm.path()), before);
    assert_eq!(std::fs::read(&issue_md).unwrap(), duplicate_bytes);

    // Empty and malformed sources are rejected without touching the issue.
    let before = commits(pm.path());
    for invalid in ["", "- [] missing state\n", "not a checklist\n"] {
        std::fs::write(&source, invalid).unwrap();
        let (ok, err) = cli(
            pm.path(),
            state.path(),
            &[
                "issue",
                "acceptance",
                "CAD-3",
                "--from",
                source.to_str().unwrap(),
            ],
        );
        assert!(!ok, "{invalid:?}: {err}");
    }
    assert_eq!(commits(pm.path()), before);
    assert_eq!(std::fs::read(&issue_md).unwrap(), duplicate_bytes);

    // Missing issue and missing input both fail closed without creating
    // folders or commits.
    std::fs::write(&source, "- [ ] valid input for missing issue\n").unwrap();
    let missing = source_dir.path().join("missing.md");
    let (ok, err) = cli(
        pm.path(),
        state.path(),
        &[
            "issue",
            "acceptance",
            "CAD-99",
            "--from",
            source.to_str().unwrap(),
        ],
    );
    assert!(
        !ok && err["error"].as_str().unwrap().contains("Unknown"),
        "{err}"
    );
    let (ok, err) = cli(
        pm.path(),
        state.path(),
        &[
            "issue",
            "acceptance",
            "CAD-99",
            "--from",
            missing.to_str().unwrap(),
        ],
    );
    assert!(
        !ok && err["error"].as_str().unwrap().contains("Cannot read"),
        "{err}"
    );
    assert!(!pm.path().join("cadence/CAD-99").exists());
}

#[test]
fn symlinks_are_never_followed() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    seed(pm.path(), state.path());

    // CAD-2/issue.md → symlink to a file outside the PM dir.
    let outside = TempDir::new().unwrap();
    let loot = outside.path().join("loot.md");
    std::fs::write(
        &loot,
        "---\nid: CAD-2\ntitle: escaped\nstatus: done\npriority: P0\ncreated: 2026-01-01T00:00:00Z\n---\n\noutside\n",
    )
    .unwrap();
    let issue_md = pm.path().join("cadence/CAD-2/issue.md");
    std::fs::remove_file(&issue_md).unwrap();
    std::os::unix::fs::symlink(&loot, &issue_md).unwrap();

    // The loader and the writer both pretend the issue is absent.
    assert!(board::load_all(pm.path(), None)
        .unwrap()
        .iter()
        .all(|i| i.front.id != "CAD-2"));
    let (ok, err) = cli(pm.path(), state.path(), &["issue", "show", "CAD-2"]);
    assert!(!ok && err["error"].as_str().unwrap().contains("Unknown"));

    // lint names the link instead of following it.
    let (ok, lint) = cli(pm.path(), state.path(), &["issue", "lint"]);
    assert!(!ok);
    let errors = lint["errors"].as_array().unwrap();
    assert!(errors
        .iter()
        .any(|e| e.as_str().unwrap().contains("symlink")));

    // A symlinked whole issue folder disappears the same way.
    let dir = pm.path().join("cadence/CAD-3");
    let real = pm.path().join("cadence/CAD-3-real");
    std::fs::rename(&dir, &real).unwrap();
    std::os::unix::fs::symlink(&real, &dir).unwrap();
    assert!(board::load_all(pm.path(), None)
        .unwrap()
        .iter()
        .all(|i| i.front.id != "CAD-3"));
}

#[test]
fn ui_routes_and_rejections() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    seed(pm.path(), state.path());
    let (port, _board) = start_ui(pm.path().to_path_buf(), state.path().to_path_buf());
    let ok_host = format!("127.0.0.1:{port}");

    let (code, body) = http(port, "GET", "/api/health", &ok_host);
    assert_eq!(code, 200);
    let h: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(h["pm_present"], true);
    assert_eq!(h["daemon"], "unreachable"); // honest: no daemon socket here

    let (code, body) = http(port, "GET", "/api/issues", &ok_host);
    assert_eq!(code, 200);
    let list: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(list["issues"].as_array().unwrap().len(), 3);

    let (code, body) = http(port, "GET", "/api/issues/CAD-1", &ok_host);
    assert_eq!(code, 200);
    let d: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(d["status"], "doing");
    assert_eq!(d["status_source"], "rollup");
    assert!(d["links"]["children"].as_array().unwrap().len() == 1);

    let (code, body) = http(port, "GET", "/api/issues/CAD-1/file", &ok_host);
    assert_eq!(code, 200);
    assert!(body.starts_with("---\nid: CAD-1"));

    let (code, body) = http(port, "GET", "/api/issues/CAD-1/activity", &ok_host);
    assert_eq!(code, 200);
    let a: Value = serde_json::from_str(&body).unwrap();
    assert!(!a["activity"].as_array().unwrap().is_empty());

    let (code, body) = http(port, "GET", "/api/agents", &ok_host);
    assert_eq!(code, 200);
    let ag: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(ag["daemon"], "unreachable");

    // --- rejections ---
    let (code, _) = http(port, "GET", "/api/issues", "evil.example");
    assert_eq!(code, 421);
    // POST on the create route exists in I2 — without the write headers
    // it stops at the cross-site guards (403), never reaching the writer.
    let (code, body) = http(port, "POST", "/api/issues", &ok_host);
    assert_eq!(code, 403);
    assert!(body.contains("content_type"));
    // CAD-313: an unlisted write (DELETE on an issue) is operator-only
    // and fails closed before any route — 403, not 405.
    let (code, body) = http(port, "DELETE", "/api/issues/CAD-1", &ok_host);
    assert_eq!(code, 403, "{body}");
    // OPTIONS is never a preflight — 405, and no Access-Control-* header
    // is ever sent on any response.
    let (code, headers, _) = http_full(port, "OPTIONS", "/api/issues", &ok_host);
    assert_eq!(code, 405);
    assert!(!headers.to_lowercase().contains("access-control"));
    let (code, _) = http(port, "GET", "/api/issues/nope", &ok_host);
    assert_eq!(code, 400);
    let (code, _) = http(port, "GET", "/api/issues/CAD-1%2F..%2Fsecret", &ok_host);
    assert_eq!(code, 400);
    let (code, _) = http(port, "GET", "/api/issues/../../etc/passwd", &ok_host);
    assert_eq!(code, 400);
    let (code, _) = http(port, "GET", "/api/issues/CAD-99", &ok_host);
    assert_eq!(code, 404);
    let (code, _) = http(port, "GET", "/api/issues/CAD-1/nope", &ok_host);
    assert_eq!(code, 404);
    let (code, _) = http(port, "GET", "/api/nope", &ok_host);
    assert_eq!(code, 404);

    // Security headers ride every response; HEAD mirrors GET's headers
    // without the body. (No dist was passed, so `/` is the 503 path —
    // headers still apply.)
    let (code, headers, _) = http_full(port, "GET", "/api/health", &ok_host);
    assert_eq!(code, 200);
    let headers = headers.to_lowercase();
    assert!(headers.contains("x-content-type-options: nosniff"));
    assert!(headers.contains("referrer-policy: no-referrer"));
    assert!(!headers.contains("content-security-policy")); // JSON, not HTML
    let (code, headers, body) = http_full(port, "HEAD", "/api/health", &ok_host);
    assert_eq!(code, 200);
    assert!(body.is_empty());
    assert!(headers
        .to_lowercase()
        .contains("x-content-type-options: nosniff"));
}

#[test]
fn ui_write_path() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    seed(pm.path(), state.path());
    let (port, _board) = start_ui(pm.path().to_path_buf(), state.path().to_path_buf());
    let host = format!("127.0.0.1:{port}");
    // CAD-313: a board write is the operator's only with a session —
    // this test process signs in and writes as `operator (ui)`.
    let _d = UiDaemon::start_on(state.path().to_path_buf());
    let op = sign_in(state.path(), port);
    let write_json = |port: u16, method: &str, path: &str, host: &str, body: &str| {
        op_write_json(&op, port, method, path, host, body)
    };
    let http_write =
        |port: u16, method: &str, path: &str, host: &str, headers: &[&str], body: &[u8]| {
            op_http_write(&op, port, method, path, host, headers, body)
        };
    let before = commits(pm.path());

    // --- cross-site guards, each failing separately ---
    // 1. Simulated cross-site form post: simple content type + foreign
    //    origin — the first guard that fails is named.
    let (code, _, body) = http_write(
        port,
        "PATCH",
        "/api/issues/CAD-2",
        &host,
        &[
            "Content-Type: application/x-www-form-urlencoded",
            "X-Cadence-Board: 1",
            "Origin: http://evil.example",
        ],
        b"status=done",
    );
    assert_eq!(code, 403);
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap()["check"],
        "content_type"
    );

    // 2. Right content type, no board marker.
    let (code, _, body) = http_write(
        port,
        "PATCH",
        "/api/issues/CAD-2",
        &host,
        &["Content-Type: application/json"],
        b"{}",
    );
    assert_eq!(code, 403);
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap()["check"],
        "x_cadence_board"
    );

    // 3. Foreign Origin.
    let (code, _, body) = http_write(
        port,
        "PATCH",
        "/api/issues/CAD-2",
        &host,
        &[
            "Content-Type: application/json",
            "X-Cadence-Board: 1",
            "Origin: http://evil.example",
        ],
        b"{}",
    );
    assert_eq!(code, 403);
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap()["check"],
        "origin"
    );

    // 4. Cross-site fetch metadata.
    let (code, _, body) = http_write(
        port,
        "PATCH",
        "/api/issues/CAD-2",
        &host,
        &[
            "Content-Type: application/json",
            "X-Cadence-Board: 1",
            "Sec-Fetch-Site: cross-site",
        ],
        b"{}",
    );
    assert_eq!(code, 403);
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap()["check"],
        "sec_fetch_site"
    );

    // 5. A wrong marker value is the same refusal as a missing one.
    let (code, _, body) = http_write(
        port,
        "PATCH",
        "/api/issues/CAD-2",
        &host,
        &["Content-Type: application/json", "X-Cadence-Board: yes"],
        b"{}",
    );
    assert_eq!(code, 403);
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap()["check"],
        "x_cadence_board"
    );
    assert_eq!(commits(pm.path()), before, "guards ran before any write");

    // --- the happy path: every write route through the same writer ---
    let (code, _, body) = write_json(
        port,
        "POST",
        "/api/issues",
        &host,
        r#"{"project":"cadence","title":"via api","priority":"P1"}"#,
    );
    assert_eq!(code, 201);
    let v: Value = serde_json::from_str(&body).unwrap();
    let new_id = v["card"]["id"].as_str().unwrap().to_string();
    assert!(new_id.starts_with("CAD-"));
    assert_eq!(v["card"]["status"], "backlog");
    assert_eq!(v["issue"]["title"], "via api");

    // PATCH fields + body; rev moves; commit names the ui actor.
    let rev = v["card"]["rev"].as_str().unwrap().to_string();
    let (code, _, body) = write_json(
        port,
        "PATCH",
        &format!("/api/issues/{new_id}"),
        &host,
        &format!(
            r##"{{"status":"ready","owner":"operator","body":"# new body","if_rev":"{rev}"}}"##
        ),
    );
    assert_eq!(code, 200);
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["card"]["status"], "ready");
    assert_eq!(v["card"]["owner"], "operator");
    assert_eq!(v["issue"]["body"], "# new body");
    assert_ne!(v["card"]["rev"].as_str().unwrap(), rev);

    // PATCH with the same (now stale) if_rev → 409 + current rev.
    let (code, _, body) = write_json(
        port,
        "PATCH",
        &format!("/api/issues/{new_id}"),
        &host,
        &format!(r#"{{"status":"doing","if_rev":"{rev}"}}"#),
    );
    assert_eq!(code, 409);
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["conflict"], "if_rev");
    assert!(v["current_rev"].as_str().unwrap().starts_with("fnv1a:"));

    // Empty strings clear owner and component.
    let (code, _, body) = write_json(
        port,
        "PATCH",
        &format!("/api/issues/{new_id}"),
        &host,
        r#"{"owner":"","component":""}"#,
    );
    assert_eq!(code, 200);
    let card = serde_json::from_str::<Value>(&body).unwrap()["card"].clone();
    assert!(card["owner"].is_null());
    assert!(card["component"].is_null());

    // Derived status refuses: CAD-1 is a rollup container.
    let (code, _, body) = write_json(
        port,
        "PATCH",
        "/api/issues/CAD-1",
        &host,
        r#"{"status":"done"}"#,
    );
    assert_eq!(code, 409);
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap()["conflict"],
        "status_derived"
    );

    // ready while still blocked → succeeds, warns. CAD-3 waits on CAD-2
    // (seeded link); move CAD-3 to ready and expect the warning.
    assert!(
        cli(
            pm.path(),
            state.path(),
            &["issue", "link", "CAD-3", "blocked_by", "CAD-2"]
        )
        .0
    );
    let (code, _, body) = write_json(
        port,
        "PATCH",
        "/api/issues/CAD-3",
        &host,
        r#"{"status":"ready"}"#,
    );
    assert_eq!(code, 200);
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["card"]["status"], "ready");
    assert!(v["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|w| w.as_str().unwrap().contains("CAD-2")));

    // links: add relates + delete it; self-link rejected.
    let (code, _, body) = write_json(
        port,
        "POST",
        "/api/issues/CAD-3/links",
        &host,
        r#"{"type":"relates","target":"CAD-1"}"#,
    );
    assert_eq!(code, 200);
    assert!(
        serde_json::from_str::<Value>(&body).unwrap()["issue"]["links"]["relates"]
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l["id"] == "CAD-1")
    );
    let (code, _, _) = write_json(
        port,
        "POST",
        "/api/issues/CAD-3/links",
        &host,
        r#"{"type":"blocked_by","target":"CAD-3"}"#,
    );
    assert_eq!(code, 400);
    let (code, _, body) = write_json(
        port,
        "DELETE",
        "/api/issues/CAD-3/links",
        &host,
        r#"{"type":"relates","target":"CAD-1"}"#,
    );
    assert_eq!(code, 200);
    assert!(
        serde_json::from_str::<Value>(&body).unwrap()["issue"]["links"]["relates"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    // refs: url form lands as a url ref.
    let (code, _, body) = write_json(
        port,
        "POST",
        "/api/issues/CAD-3/refs",
        &host,
        r#"{"kind":"url","url":"https://example.com/doc","label":"doc"}"#,
    );
    assert_eq!(code, 200);
    assert!(
        serde_json::from_str::<Value>(&body).unwrap()["issue"]["refs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["label"] == "doc")
    );

    // comments: author operator, kind ui, markdown body stored verbatim.
    let (code, _, body) = write_json(
        port,
        "POST",
        "/api/issues/CAD-3/comments",
        &host,
        r#"{"body":"hello **bold** <script>x</script>"}"#,
    );
    assert_eq!(code, 200);
    let v: Value = serde_json::from_str(&body).unwrap();
    let comment = v["issue"]["comments"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["body"].as_str().unwrap().contains("**bold**"))
        .unwrap()
        .clone();
    assert_eq!(comment["author"], "operator");
    assert_eq!(comment["kind"], "ui");

    // --- artifacts ---
    // upload: octet-stream body, ?name= grammar.
    let (code, _, body) = http_write(
        port,
        "POST",
        "/api/issues/CAD-3/artifacts?name=notes.md",
        &host,
        &[
            "Content-Type: application/octet-stream",
            "X-Cadence-Board: 1",
        ],
        b"# report\n",
    );
    assert_eq!(code, 200);
    assert!(
        serde_json::from_str::<Value>(&body).unwrap()["issue"]["artifacts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["name"] == "notes.md")
    );

    // json content-type on the upload route is refused.
    let (code, _, _) = write_json(
        port,
        "POST",
        "/api/issues/CAD-3/artifacts?name=x.md",
        &host,
        "{}",
    );
    assert_eq!(code, 403);

    // same name again → create-only conflict.
    let (code, _, body) = http_write(
        port,
        "POST",
        "/api/issues/CAD-3/artifacts?name=notes.md",
        &host,
        &[
            "Content-Type: application/octet-stream",
            "X-Cadence-Board: 1",
        ],
        b"v2",
    );
    assert_eq!(code, 409);
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap()["conflict"],
        "exists"
    );

    // over-cap upload refused without buffering (cap is 1 MiB).
    let big = vec![b'x'; 1_048_577];
    let (code, _, body) = http_write(
        port,
        "POST",
        "/api/issues/CAD-3/artifacts?name=big.bin",
        &host,
        &[
            "Content-Type: application/octet-stream",
            "X-Cadence-Board: 1",
        ],
        &big,
    );
    assert_eq!(code, 413);
    assert!(body.contains("cap"));

    // bad names.
    for bad in ["a/b.txt", ".hidden", ""] {
        let (code, _, _) = http_write(
            port,
            "POST",
            &format!("/api/issues/CAD-3/artifacts?name={bad}"),
            &host,
            &[
                "Content-Type: application/octet-stream",
                "X-Cadence-Board: 1",
            ],
            b"x",
        );
        assert_eq!(code, 400, "name {bad:?}");
    }

    // The constrained read: text inline, html/svg/pdf as attachments.
    std::fs::write(
        pm.path().join("cadence/CAD-3/artifacts/report.html"),
        "<script>alert(1)</script>",
    )
    .unwrap();
    std::fs::write(pm.path().join("cadence/CAD-3/artifacts/scan.svg"), "<svg/>").unwrap();
    let (code, headers, body) =
        http_full(port, "GET", "/api/issues/CAD-3/artifacts/notes.md", &host);
    assert_eq!(code, 200);
    let h = headers.to_lowercase();
    assert!(h.contains("text/plain"));
    assert!(h.contains("content-security-policy: sandbox; default-src 'none'"));
    assert!(!h.contains("content-disposition"));
    assert!(body.contains("# report"));

    // Images open inline too — the drawer previews them.
    std::fs::write(
        pm.path().join("cadence/CAD-3/artifacts/shot.png"),
        b"\x89PNG",
    )
    .unwrap();
    let (code, headers, _) = http_full(port, "GET", "/api/issues/CAD-3/artifacts/shot.png", &host);
    assert_eq!(code, 200);
    let h = headers.to_lowercase();
    assert!(h.contains("image/png"));
    assert!(!h.contains("content-disposition"));

    // Active formats never render: html/svg/xml/js/pdf download as
    // octet-stream attachments under the sandbox CSP.
    for (name, bytes) in [
        ("x.pdf", b"%PDF".as_slice()),
        ("feed.xml", b"<x/>".as_slice()),
        ("app.js", b"alert(1)".as_slice()),
    ] {
        std::fs::write(pm.path().join("cadence/CAD-3/artifacts").join(name), bytes).unwrap();
    }
    for name in ["report.html", "scan.svg", "x.pdf", "feed.xml", "app.js"] {
        let (code, headers, _) = http_full(
            port,
            "GET",
            &format!("/api/issues/CAD-3/artifacts/{name}"),
            &host,
        );
        assert_eq!(code, 200, "{name}");
        let h = headers.to_lowercase();
        assert!(h.contains("content-disposition: attachment"), "{name}: {h}");
        assert!(h.contains("application/octet-stream"), "{name}: {h}");
        assert!(h.contains("sandbox"), "{name}: {h}");
    }

    // A nested name is not a traversal escape — the name grammar
    // refuses "/" outright.
    let (code, _) = http(
        port,
        "GET",
        "/api/issues/CAD-3/artifacts/../issue.md",
        &host,
    );
    assert_eq!(code, 400);
    // a symlinked artifact is refused.
    let outside = pm.path().join("outside.txt");
    std::fs::write(&outside, "secret").unwrap();
    std::os::unix::fs::symlink(&outside, pm.path().join("cadence/CAD-3/artifacts/leak.txt"))
        .unwrap();
    let (code, _) = http(port, "GET", "/api/issues/CAD-3/artifacts/leak.txt", &host);
    assert_eq!(code, 404);

    // Every write produced exactly one commit naming the ui actor.
    let log = Command::new("git")
        .arg("-C")
        .arg(pm.path())
        .args(["log", "--format=%s"])
        .output()
        .unwrap();
    let subjects = String::from_utf8_lossy(&log.stdout);
    assert!(subjects.contains("(operator (ui))"), "{subjects}");
    assert!(subjects.contains("attach notes.md (operator (ui))"));

    // unknown field rejected; bad json rejected.
    let (code, _, _) = write_json(port, "PATCH", "/api/issues/CAD-2", &host, r#"{"bogus":1}"#);
    assert_eq!(code, 400);
    let (code, _, _) = write_json(port, "PATCH", "/api/issues/CAD-2", &host, "{");
    assert_eq!(code, 400);

    // One commit per successful write: 9 HTTP writes + the 1 CLI link.
    assert_eq!(
        commits(pm.path()),
        before + 10,
        "each write is exactly one commit"
    );
}

#[test]
fn ui_pm_absent_is_honest() {
    let pm = TempDir::new().unwrap();
    let empty = pm.path().join("nope");
    let state = TempDir::new().unwrap();
    let (port, _board) = start_ui(empty.clone(), state.path().to_path_buf());
    let host = format!("127.0.0.1:{port}");
    let (code, body) = http(port, "GET", "/api/health", &host);
    assert_eq!(code, 200);
    let h: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(h["pm_present"], false);
    let (code, _) = http(port, "GET", "/api/issues", &host);
    assert_eq!(code, 503);
}

#[test]
fn writes_validate_fields() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    seed(pm.path(), state.path());
    // A project that declares components, so membership is checkable.
    assert!(
        cli(
            pm.path(),
            state.path(),
            &[
                "issue",
                "project",
                "add",
                "ops",
                "--prefix",
                "OPS",
                "--component",
                "api",
                "--component",
                "cli"
            ]
        )
        .0
    );
    assert!(
        cli(
            pm.path(),
            state.path(),
            &["issue", "new", "task one", "--project", "ops"]
        )
        .0
    );

    // --- CLI rejections: each names the allowed values, no commit ---
    let base = commits(pm.path());
    for (args, want) in [
        (vec!["issue", "set", "OPS-1", "component=bogus"], "api, cli"),
        (vec!["issue", "set", "OPS-1", "status=flying"], "backlog"),
        (vec!["issue", "set", "OPS-1", "priority=P9"], "P0"),
        (
            vec![
                "issue",
                "new",
                "bad comp",
                "--project",
                "ops",
                "--component",
                "bogus",
            ],
            "api, cli",
        ),
        (
            vec![
                "issue",
                "new",
                "bad prio",
                "--project",
                "ops",
                "--priority",
                "P9",
            ],
            "P0",
        ),
        // Field-path link targets must exist too.
        (
            vec![
                "issue",
                "new",
                "bad dep",
                "--project",
                "ops",
                "--blocked-by",
                "OPS-99",
            ],
            "OPS-99",
        ),
        (
            vec![
                "issue",
                "new",
                "bad parent",
                "--project",
                "ops",
                "--parent",
                "OPS-99",
            ],
            "OPS-99",
        ),
        (
            vec!["issue", "link", "OPS-1", "blocked_by", "OPS-99"],
            "OPS-99",
        ),
        (vec!["issue", "link", "OPS-1", "parent", "OPS-99"], "OPS-99"),
        (
            vec!["issue", "link", "OPS-1", "relates", "OPS-99"],
            "OPS-99",
        ),
        (
            vec!["issue", "link", "OPS-1", "duplicate_of", "OPS-99"],
            "OPS-99",
        ),
    ] {
        let (ok, err) = cli(pm.path(), state.path(), &args);
        assert!(!ok, "{args:?} unexpectedly succeeded");
        let msg = err["error"].as_str().unwrap_or_default();
        assert!(msg.contains(want), "{args:?}: '{msg}' lacks '{want}'");
    }
    assert_eq!(commits(pm.path()), base, "a rejection still committed");

    // A declared component and an empty clear still work.
    assert!(
        cli(
            pm.path(),
            state.path(),
            &["issue", "set", "OPS-1", "component=api"]
        )
        .0
    );
    assert!(
        cli(
            pm.path(),
            state.path(),
            &["issue", "set", "OPS-1", "component="]
        )
        .0
    );

    // --- HTTP: the same writer, the same rejections (400) ---
    let (port, _board) = start_ui(pm.path().to_path_buf(), state.path().to_path_buf());
    let host = format!("127.0.0.1:{port}");
    // CAD-313: a board write is the operator's only with a session —
    // this test process signs in and writes as `operator (ui)`.
    let _d = UiDaemon::start_on(state.path().to_path_buf());
    let op = sign_in(state.path(), port);
    let write_json = |port: u16, method: &str, path: &str, host: &str, body: &str| {
        op_write_json(&op, port, method, path, host, body)
    };
    let before = commits(pm.path());
    for (method, path, body, want) in [
        (
            "POST",
            "/api/issues".to_string(),
            r#"{"project":"ops","title":"bad comp","component":"bogus"}"#.to_string(),
            "api, cli",
        ),
        (
            "POST",
            "/api/issues".to_string(),
            r#"{"project":"ops","title":"bad prio","priority":"P9"}"#.to_string(),
            "P0",
        ),
        (
            "POST",
            "/api/issues".to_string(),
            r#"{"project":"ops","title":"bad dep","blocked_by":["OPS-99"]}"#.to_string(),
            "OPS-99",
        ),
        (
            "POST",
            "/api/issues".to_string(),
            r#"{"project":"ops","title":"bad parent","parent":"OPS-99"}"#.to_string(),
            "OPS-99",
        ),
        (
            "PATCH",
            "/api/issues/OPS-1".to_string(),
            r#"{"component":"bogus"}"#.to_string(),
            "api, cli",
        ),
        (
            "PATCH",
            "/api/issues/OPS-1".to_string(),
            r#"{"status":"flying"}"#.to_string(),
            "backlog",
        ),
        (
            "PATCH",
            "/api/issues/OPS-1".to_string(),
            r#"{"priority":"P9"}"#.to_string(),
            "P0",
        ),
        (
            "POST",
            "/api/issues/OPS-1/links".to_string(),
            r#"{"type":"blocked_by","target":"OPS-99"}"#.to_string(),
            "OPS-99",
        ),
    ] {
        let (code, _, body) = write_json(port, method, &path, &host, &body);
        // Field validation rejects 400; an unknown link target is a
        // 404 through write_err's "Unknown issue" mapping.
        assert!(
            code == 400 || code == 404,
            "{method} {path} returned {code}: {body}"
        );
        assert!(
            body.contains(want),
            "{method} {path}: '{body}' lacks '{want}'"
        );
    }
    assert_eq!(
        commits(pm.path()),
        before,
        "an HTTP rejection still committed"
    );

    // And the happy path is unchanged through HTTP.
    let (code, _, body) = write_json(
        port,
        "PATCH",
        "/api/issues/OPS-1",
        &host,
        r#"{"component":"api"}"#,
    );
    assert_eq!(code, 200, "{body}");
}

#[test]
fn init_hooks_and_doctor() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();

    // Plant a foreign pre-commit before init: git repo first, then the
    // hook, so `issue init` sees it as pre-existing and must keep it.
    let hooks_dir = pm.path().join(".git/hooks");
    Command::new("git")
        .arg("-C")
        .arg(pm.path())
        .args(["init", "-q"])
        .output()
        .unwrap();
    std::fs::create_dir_all(&hooks_dir).unwrap();
    std::fs::write(hooks_dir.join("pre-commit"), "#!/bin/sh\necho foreign\n").unwrap();

    let (ok, out) = cli(pm.path(), state.path(), &["issue", "init"]);
    assert!(ok, "init failed: {out}");
    let pre = hooks_dir.join("pre-commit");
    let post = hooks_dir.join("post-commit");
    // Foreign hook preserved; our post-commit installed and executable.
    assert_eq!(
        std::fs::read_to_string(&pre).unwrap(),
        "#!/bin/sh\necho foreign\n"
    );
    assert_eq!(out["hooks"]["pre-commit"]["action"], "kept_foreign");
    assert_eq!(out["hooks"]["pre-commit"]["owner"], "foreign");
    assert_eq!(out["hooks"]["post-commit"]["action"], "installed");
    let post_text = std::fs::read_to_string(&post).unwrap();
    assert!(post_text.contains("cadence board tracker"));
    use std::os::unix::fs::PermissionsExt;
    assert!(std::fs::metadata(&post).unwrap().permissions().mode() & 0o111 != 0);

    // Second init is a no-op on disk and reports both hooks.
    let before_pre = std::fs::read(&pre).unwrap();
    let before_post = std::fs::read(&post).unwrap();
    let (ok, out) = cli(pm.path(), state.path(), &["issue", "init"]);
    assert!(ok);
    assert_eq!(out["hooks"]["pre-commit"]["action"], "kept_foreign");
    assert_eq!(out["hooks"]["post-commit"]["action"], "present");
    assert_eq!(std::fs::read(&pre).unwrap(), before_pre);
    assert_eq!(std::fs::read(&post).unwrap(), before_post);

    // Doctor reports every field; a foreign hook makes it not-ok.
    let (ok, report) = cli(pm.path(), state.path(), &["issue", "doctor"]);
    assert!(!ok, "doctor should fail with a foreign hook: {report}");
    assert_eq!(report["ok"], false);
    assert_eq!(report["git"], true);
    assert_eq!(report["remote"], Value::Null);
    assert_eq!(report["hooks"]["pre-commit"]["owner"], "foreign");
    assert_eq!(report["hooks"]["post-commit"]["owner"], "cadence");
    assert_eq!(report["hooks"]["post-commit"]["executable"], true);
    assert_eq!(report["lint"]["ok"], true);
    assert!(report["push"].is_null());
    assert!(report["push_failures"].is_null());

    // Restore our hook — a drifted cadence-owned file is refreshed.
    std::fs::write(&pre, "#!/bin/sh\n# cadence board tracker: stale\nexit 0\n").unwrap();
    let (ok, out) = cli(pm.path(), state.path(), &["issue", "init"]);
    assert!(ok);
    assert_eq!(out["hooks"]["pre-commit"]["action"], "updated");
    let pre_text = std::fs::read_to_string(&pre).unwrap();
    assert!(pre_text.contains("cadence issue lint"));

    // The failure-log tail shows up verbatim.
    std::fs::write(
        pm.path().join(".git/push-failures.log"),
        "2026-09-18T00:00:00Z push failed\n2026-09-18T01:00:00Z push failed\n",
    )
    .unwrap();
    let (ok, report) = cli(pm.path(), state.path(), &["issue", "doctor"]);
    assert!(ok, "clean tracker should pass: {report}");
    assert_eq!(report["ok"], true);
    let tail = report["push_failures"]["tail"].as_array().unwrap();
    assert_eq!(tail.len(), 2);
    assert!(tail[1].as_str().unwrap().contains("01:00:00Z"));

    // A missing hook fails the check and names which.
    std::fs::remove_file(&post).unwrap();
    let (ok, report) = cli(pm.path(), state.path(), &["issue", "doctor"]);
    assert!(!ok);
    assert_eq!(report["hooks"]["post-commit"]["present"], false);
}

// ---------- CAD-39: issue sync — fetch, rebase, lint, push ----------

/// A bare remote plus two clones with `issue init` run in each — the
/// multi-host shape. `user.email`/`user.name` are set per clone so
/// tests can also commit by hand. The post-commit hook pushes in the
/// background, so remote state is only ever asserted through
/// `wait_remote`.
struct TrackerPair {
    _root: TempDir,
    remote: PathBuf,
    a: PathBuf,
    b: PathBuf,
    state: TempDir,
}

fn git(dir: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
    )
}

fn head(dir: &Path) -> String {
    git(dir, &["rev-parse", "HEAD"]).1
}

fn porcelain(dir: &Path) -> String {
    git(dir, &["status", "--porcelain"]).1
}

/// Poll `ls-remote` until the remote's branch tip equals `want` —
/// pushing `from` on each round. The winner's post-commit hook usually
/// lands it first; the explicit push covers the race. Diverged pushes
/// simply fail every round until the timeout.
fn wait_remote(remote: &Path, from: &Path, want: &str, branch: &str) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let (ok, out) = Command::new("git")
            .arg("ls-remote")
            .arg(remote)
            .arg(format!("refs/heads/{branch}"))
            .output()
            .map(|o| {
                (
                    o.status.success(),
                    String::from_utf8_lossy(&o.stdout).to_string(),
                )
            })
            .unwrap_or((false, String::new()));
        if ok && out.split_whitespace().next() == Some(want) {
            return;
        }
        let _ = git(from, &["push", "-q", "origin", branch]);
        assert!(
            Instant::now() < deadline,
            "remote never reached {want} from {}",
            from.display()
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn branch(dir: &Path) -> String {
    git(dir, &["rev-parse", "--abbrev-ref", "HEAD"]).1
}

/// The remote's current tip for `branch`, or empty when unreadable.
fn remote_tip(remote: &Path, branch: &str) -> String {
    Command::new("git")
        .arg("ls-remote")
        .arg(remote)
        .arg(format!("refs/heads/{branch}"))
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .split_whitespace()
                .next()
                .map(str::to_string)
        })
        .unwrap_or_default()
}

/// Fail if the remote's branch tip leaves `tip` inside `window` — a
/// leaked post-commit push lands within a couple of seconds of the
/// replayed commit, so watching briefly after sync returns covers the
/// hook's in-flight window.
fn assert_remote_stable(remote: &Path, branch: &str, tip: &str, window: Duration) {
    let deadline = Instant::now() + window;
    while Instant::now() < deadline {
        assert_eq!(
            remote_tip(remote, branch),
            tip,
            "remote moved during the post-commit window"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

impl TrackerPair {
    fn new() -> TrackerPair {
        let root = TempDir::new().unwrap();
        let remote = root.path().join("remote.git");
        let a = root.path().join("a");
        let b = root.path().join("b");
        let state = TempDir::new().unwrap();
        // Pin the branch name — host `init.defaultBranch` differs
        // (CI has none and would mint `master`, breaking every
        // `origin/main`/`"main"` reference below).
        Command::new("git")
            .args(["init", "-q", "--bare", "-b", "main"])
            .arg(&remote)
            .output()
            .unwrap();
        // A inits and publishes first — B clones the bootstrapped
        // remote so both share the same init commit.
        Command::new("git")
            .args(["clone", "-q"])
            .arg(&remote)
            .arg(&a)
            .output()
            .unwrap();
        git(&a, &["config", "user.email", "a@x"]);
        git(&a, &["config", "user.name", "a"]);
        let (ok, out) = cli(&a, state.path(), &["issue", "init"]);
        assert!(ok, "init on {}: {out}", a.display());
        wait_remote(&remote, &a, &head(&a), &branch(&a));
        Command::new("git")
            .args(["clone", "-q"])
            .arg(&remote)
            .arg(&b)
            .output()
            .unwrap();
        git(&b, &["config", "user.email", "b@x"]);
        git(&b, &["config", "user.name", "b"]);
        // Init on B is idempotent — the skeleton exists; only the
        // hooks land in B's .git/hooks.
        let (ok, out) = cli(&b, state.path(), &["issue", "init"]);
        assert!(ok, "init on {}: {out}", b.display());
        TrackerPair {
            _root: root,
            remote,
            a,
            b,
            state,
        }
    }

    fn cli(&self, clone: &Path, args: &[&str]) -> (bool, Value) {
        cli(clone, self.state.path(), args)
    }

    /// Push `clone`'s HEAD to the remote and wait until it lands.
    fn publish(&self, clone: &Path) {
        wait_remote(&self.remote, clone, &head(clone), &branch(clone));
    }
}

/// A writes, B writes a different issue (in its own project — same-id
/// minting is inherent while behind), B syncs → rebased + pushed;
/// A syncs → level, both trees identical, no conflict.
#[test]
fn sync_rebases_and_levels_two_clones() {
    let t = TrackerPair::new();
    // A: project + first issue, pushed.
    let (ok, out) = t.cli(&t.a, &["issue", "project", "add", "cad", "--prefix", "CAD"]);
    assert!(ok, "{out}");
    t.publish(&t.a);
    // B picks the project up via a strict-behind sync first — a pure
    // fast-forward replays and pushes nothing.
    let (ok, out) = t.cli(&t.b, &["issue", "sync"]);
    assert!(ok, "B first sync: {out}");
    assert_eq!(out["rebased"], false);
    assert_eq!(out["pushed"], false);
    assert_eq!(out["behind"], 0);
    // A moves the remote again — B is behind from here.
    let (ok, out) = t.cli(&t.a, &["issue", "new", "alpha", "--project", "cad"]);
    assert!(ok, "{out}");
    t.publish(&t.a);
    // B writes a different issue in its own project — disjoint paths,
    // so the rebase merges cleanly. B's hook push fails non-ff.
    let (ok, out) = t.cli(&t.b, &["issue", "project", "add", "ops", "--prefix", "OPS"]);
    assert!(ok, "{out}");
    let (ok, out) = t.cli(&t.b, &["issue", "new", "beta", "--project", "ops"]);
    assert!(ok, "{out}");
    assert_eq!(out["id"].as_str().unwrap(), "OPS-1");
    let (ok, out) = t.cli(&t.b, &["issue", "sync"]);
    assert!(ok, "B sync: {out}");
    assert_eq!(out["rebased"], true);
    assert_eq!(out["pushed"], true);
    // A syncs: strictly behind now → rebase fast-forwards.
    let (ok, out) = t.cli(&t.a, &["issue", "sync"]);
    assert!(ok, "A sync: {out}");
    // Identical tracked trees: same HEAD, same file list.
    assert_eq!(head(&t.a), head(&t.b));
    assert_eq!(
        git(&t.a, &["ls-files"]).1,
        git(&t.b, &["ls-files"]).1,
        "tracked trees differ"
    );
}

/// Same `issue.md` edited on both hosts: sync without `--resolve`
/// aborts naming the path and both subjects, and the tree is exactly
/// as found; `--resolve theirs` then `--resolve ours` on a fresh
/// divergence take the named side whole.
#[test]
fn sync_conflict_aborts_then_resolves() {
    let t = TrackerPair::new();
    let (ok, out) = t.cli(&t.a, &["issue", "project", "add", "cad", "--prefix", "CAD"]);
    assert!(ok, "{out}");
    let (ok, out) = t.cli(&t.a, &["issue", "new", "alpha", "--project", "cad"]);
    assert!(ok, "{out}");
    let id = out["id"].as_str().unwrap().to_string();
    t.publish(&t.a);
    let (ok, out) = t.cli(&t.b, &["issue", "sync"]);
    assert!(ok, "B level sync: {out}");

    // B edits the issue first and lands it on the remote.
    let (ok, out) = t.cli(&t.b, &["issue", "set", &id, "status=doing"]);
    assert!(ok, "{out}");
    t.publish(&t.b);
    // A edits the same file — its hook push fails non-ff.
    let (ok, out) = t.cli(&t.a, &["issue", "set", &id, "status=review"]);
    assert!(ok, "{out}");
    let pre_head = head(&t.a);

    let (ok, out) = t.cli(&t.a, &["issue", "sync"]);
    assert!(!ok, "conflicting sync must fail: {out}");
    assert_eq!(out["ok"], false);
    assert_eq!(out["aborted"], "conflict");
    let conflicts = out["conflicts"].as_array().unwrap();
    assert_eq!(conflicts.len(), 1, "{conflicts:?}");
    assert_eq!(
        conflicts[0]["path"].as_str().unwrap(),
        format!("cad/{id}/issue.md")
    );
    assert!(conflicts[0]["local"].as_str().unwrap().contains("set"));
    assert!(conflicts[0]["remote"].as_str().unwrap().contains("set"));
    assert_eq!(out["tree_restored"], true);
    // The tree is exactly as found: same HEAD, clean status, no
    // rebase marker left behind.
    assert_eq!(head(&t.a), pre_head);
    assert_eq!(porcelain(&t.a), "", "sync left the tree dirty");
    assert!(!t.a.join(".git/rebase-merge").exists());

    // --resolve theirs: the remote's value wins wholesale.
    let (ok, out) = t.cli(&t.a, &["issue", "sync", "--resolve", "theirs"]);
    assert!(ok, "resolve theirs: {out}");
    let body = std::fs::read_to_string(t.a.join(format!("cad/{id}/issue.md"))).unwrap();
    assert!(body.contains("status: doing"), "{body}");
    assert_eq!(porcelain(&t.a), "");

    // Fresh divergence — --resolve ours keeps the local value. B must
    // publish first: whichever `issue set` commits first wins the
    // remote via its own post-commit hook, so A writes only after B's
    // push has landed (A's hook push then fails non-ff, as intended).
    let (ok, out) = t.cli(&t.b, &["issue", "set", &id, "status=done"]);
    assert!(ok, "{out}");
    t.publish(&t.b);
    let (ok, out) = t.cli(&t.a, &["issue", "set", &id, "status=backlog"]);
    assert!(ok, "{out}");
    let (ok, out) = t.cli(&t.a, &["issue", "sync", "--resolve", "ours"]);
    assert!(ok, "resolve ours: {out}");
    let body = std::fs::read_to_string(t.a.join(format!("cad/{id}/issue.md"))).unwrap();
    assert!(body.contains("status: backlog"), "{body}");
}

/// A clean rebase into a lint failure aborts the same way: the local
/// host added an issue on a component the remote side removed — the
/// merge is disjoint, lint rejects it, the tree is restored and
/// nothing is pushed.
#[test]
fn sync_lint_failure_aborts() {
    let t = TrackerPair::new();
    let (ok, out) = t.cli(
        &t.a,
        &[
            "issue",
            "project",
            "add",
            "cad",
            "--prefix",
            "CAD",
            "--component",
            "board",
            "--component",
            "gone",
        ],
    );
    assert!(ok, "{out}");
    t.publish(&t.a);
    let (ok, out) = t.cli(&t.b, &["issue", "sync"]);
    assert!(ok, "B level sync: {out}");

    // B removes `gone` from project.yaml by hand and pushes it.
    let py = t.b.join("cad/project.yaml");
    std::fs::write(
        &py,
        std::fs::read_to_string(&py)
            .unwrap()
            .replace("\n- gone", ""),
    )
    .unwrap();
    let (ok, _) = git(&t.b, &["add", "-A"]);
    assert!(ok);
    let (ok, _) = git(&t.b, &["commit", "-qm", "drop gone component"]);
    assert!(ok);
    t.publish(&t.b);

    // A's local project.yaml still declares `gone` — the write is
    // valid there and its background push fails non-ff.
    let (ok, out) = t.cli(
        &t.a,
        &[
            "issue",
            "new",
            "uses gone",
            "--project",
            "cad",
            "--component",
            "gone",
        ],
    );
    assert!(ok, "{out}");
    let id = out["id"].as_str().unwrap().to_string();
    let pre_head = head(&t.a);
    // Baseline before sync — a mid-rebase hook leak lands inside the
    // sync call itself, so the remote tip must be captured first.
    let tip = remote_tip(&t.remote, "main");
    assert!(!tip.is_empty());

    let (ok, out) = t.cli(&t.a, &["issue", "sync"]);
    assert!(!ok, "lint-broken merge must fail: {out}");
    assert_eq!(out["aborted"], "lint");
    assert_eq!(out["rebased"], true);
    assert_eq!(out["pushed"], false);
    assert_eq!(out["tree_restored"], true);
    let errors = out["lint"]["errors"].as_array().unwrap();
    assert!(
        errors
            .iter()
            .any(|e| e.as_str().unwrap().contains(&id) && e.as_str().unwrap().contains("gone")),
        "lint errors: {errors:?}"
    );
    assert_eq!(head(&t.a), pre_head);
    assert_eq!(porcelain(&t.a), "");
    // Nothing was pushed — the remote tip still lacks A's commit, and
    // it must stay that way through the post-commit hook's window: the
    // rebase's replayed commits once leaked out mid-abort.
    assert_remote_stable(&t.remote, "main", &tip, Duration::from_secs(6));
    // A fresh clone of the remote still lints clean — the bad commit
    // never escaped this clone's rebase.
    let fresh = TempDir::new().unwrap();
    let (ok, _) = {
        let out = Command::new("git")
            .args(["clone", "-q"])
            .arg(&t.remote)
            .arg(fresh.path())
            .output()
            .unwrap();
        (out.status.success(), ())
    };
    assert!(ok, "fresh clone failed");
    let (ok, out) = cli(fresh.path(), t.state.path(), &["issue", "lint"]);
    assert!(ok && out["ok"] == true, "fresh clone lint: {out}");
}

/// `--dry-run` fetches and reports divergence + would-conflict paths
/// without touching the tree; `--no-push` rebases and lints but
/// leaves the remote alone.
#[test]
fn sync_dry_run_and_no_push() {
    let t = TrackerPair::new();
    let (ok, out) = t.cli(&t.a, &["issue", "project", "add", "cad", "--prefix", "CAD"]);
    assert!(ok, "{out}");
    let (ok, out) = t.cli(&t.a, &["issue", "new", "alpha", "--project", "cad"]);
    assert!(ok, "{out}");
    let id = out["id"].as_str().unwrap().to_string();
    t.publish(&t.a);
    let (ok, out) = t.cli(&t.b, &["issue", "sync"]);
    assert!(ok, "{out}");

    // Same-file divergence: B pushes its edit, A's two edits stay
    // local — ahead=2/behind=1 is asymmetric, so a swapped count
    // would fail these asserts.
    let (ok, out) = t.cli(&t.b, &["issue", "set", &id, "status=doing"]);
    assert!(ok, "{out}");
    t.publish(&t.b);
    let (ok, out) = t.cli(&t.a, &["issue", "set", &id, "status=review"]);
    assert!(ok, "{out}");
    let (ok, out) = t.cli(&t.a, &["issue", "set", &id, "priority=P1"]);
    assert!(ok, "{out}");
    let pre_head = head(&t.a);

    let (ok, out) = t.cli(&t.a, &["issue", "sync", "--dry-run"]);
    assert!(ok, "dry-run: {out}");
    assert_eq!(out["dry_run"], true);
    assert_eq!(out["changed"], false);
    assert_eq!(out["behind"], 1);
    assert_eq!(out["ahead"], 2);
    assert_eq!(
        out["would_conflict"].as_array().unwrap(),
        &vec![json!(format!("cad/{id}/issue.md"))]
    );
    assert_eq!(head(&t.a), pre_head);
    assert_eq!(porcelain(&t.a), "");

    // --no-push rebases and lints but leaves the remote untouched —
    // and stays untouched through the post-commit hook's window: the
    // rebase's replayed commits once leaked out mid-sync. `--resolve
    // ours` keeps A's two commits so the follow-up push has real work.
    let remote_before = git(&t.a, &["rev-parse", "origin/main"]).1;
    let (ok, out) = t.cli(&t.a, &["issue", "sync", "--no-push", "--resolve", "ours"]);
    assert!(ok, "no-push: {out}");
    assert_eq!(out["rebased"], true);
    assert_eq!(out["pushed"], false);
    assert_eq!(git(&t.a, &["rev-parse", "origin/main"]).1, remote_before);
    assert_remote_stable(&t.remote, "main", &remote_before, Duration::from_secs(6));
    // The rebased commits exist locally — a later plain sync pushes
    // them, and `pushed` reports the remote actually moving.
    let (ok, out) = t.cli(&t.a, &["issue", "sync"]);
    assert!(ok, "follow-up push: {out}");
    assert_eq!(out["pushed"], true);
}

/// Preconditions refuse with the offending paths: a dirty tree, no
/// `origin`, and a rebase or merge already in progress.
#[test]
fn sync_precondition_refusals() {
    let t = TrackerPair::new();

    // Dirty tree — the porcelain listing names the path.
    std::fs::write(t.a.join("scratch.txt"), "x").unwrap();
    let (ok, out) = t.cli(&t.a, &["issue", "sync"]);
    assert!(!ok);
    assert!(
        out["error"].as_str().unwrap().contains("scratch.txt"),
        "{out}"
    );
    std::fs::remove_file(t.a.join("scratch.txt")).unwrap();

    // A rebase in progress — the marker path is named.
    std::fs::create_dir(t.a.join(".git/rebase-merge")).unwrap();
    let (ok, out) = t.cli(&t.a, &["issue", "sync"]);
    assert!(!ok);
    assert!(
        out["error"].as_str().unwrap().contains("rebase-merge"),
        "{out}"
    );
    std::fs::remove_dir(t.a.join(".git/rebase-merge")).unwrap();

    // A merge in progress — MERGE_HEAD is named.
    std::fs::write(t.a.join(".git/MERGE_HEAD"), "0".repeat(40)).unwrap();
    let (ok, out) = t.cli(&t.a, &["issue", "sync"]);
    assert!(!ok);
    assert!(
        out["error"].as_str().unwrap().contains("MERGE_HEAD"),
        "{out}"
    );
    std::fs::remove_file(t.a.join(".git/MERGE_HEAD")).unwrap();

    // No origin: an init'd tracker without a remote.
    let solo = TempDir::new().unwrap();
    let (ok, out) = cli(solo.path(), t.state.path(), &["issue", "init"]);
    assert!(ok, "{out}");
    let (ok, out) = cli(solo.path(), t.state.path(), &["issue", "sync"]);
    assert!(!ok);
    assert!(out["error"].as_str().unwrap().contains("origin"), "{out}");

    // Not a git repo: pm.yaml with no .git at all.
    let bare = TempDir::new().unwrap();
    std::fs::write(
        bare.path().join("pm.yaml"),
        "schema: 1\nnotes_dir: /var/www/agent-notes\n",
    )
    .unwrap();
    let (ok, out) = cli(bare.path(), t.state.path(), &["issue", "sync"]);
    assert!(!ok);
    assert!(
        out["error"]
            .as_str()
            .unwrap()
            .contains("not a git repository"),
        "{out}"
    );
}

/// Doctor reports ahead/behind against `origin/<branch>` and points
/// at `issue sync` while the local side is behind.
#[test]
fn doctor_reports_divergence_and_sync_hint() {
    let t = TrackerPair::new();
    let (ok, out) = t.cli(&t.a, &["issue", "project", "add", "cad", "--prefix", "CAD"]);
    assert!(ok, "{out}");
    t.publish(&t.a);
    // A is level — doctor is healthy and carries no sync hint.
    let (ok, out) = t.cli(&t.a, &["issue", "doctor"]);
    assert!(ok, "level doctor: {out}");
    assert_eq!(out["push"]["ahead"], 0);
    assert_eq!(out["push"]["behind"], 0);
    assert!(out["push"]["sync"].is_null());

    // B falls behind: fetch alone moves `origin/<branch>` forward.
    let (ok, _) = git(&t.b, &["fetch", "origin"]);
    assert!(ok);
    let (_, report) = t.cli(&t.b, &["issue", "doctor"]);
    assert!(report["push"]["behind"].as_u64().unwrap() >= 1, "{report}");
    assert_eq!(
        report["push"]["sync"].as_str().unwrap(),
        "cadence issue sync"
    );
}

/// The hardened post-commit hook itself: a commit made while a rebase
/// marker stands is not pushed, and neither is one on a detached HEAD —
/// even when the hook is invoked directly. Commits under the marker
/// stay local, which is what makes the whole class safe.
#[test]
fn post_commit_hook_refuses_mid_sequence_and_detached() {
    let t = TrackerPair::new();
    let tip0 = remote_tip(&t.remote, "main");
    assert!(!tip0.is_empty());

    // A rebase marker: commits created now (which fire the hook) must
    // stay local — replayed commits are not settled state.
    std::fs::create_dir(t.a.join(".git/rebase-merge")).unwrap();
    let (ok, out) = t.cli(&t.a, &["issue", "project", "add", "cad", "--prefix", "CAD"]);
    assert!(ok, "{out}");
    let (ok, out) = t.cli(&t.a, &["issue", "new", "alpha", "--project", "cad"]);
    assert!(ok, "{out}");
    assert_ne!(head(&t.a), tip0, "commits did not happen locally");
    // Invoke the hook directly — it still exits without pushing.
    let rc = Command::new("sh")
        .arg(".git/hooks/post-commit")
        .current_dir(&t.a)
        .status()
        .unwrap();
    assert!(rc.success());
    assert_remote_stable(&t.remote, "main", &tip0, Duration::from_secs(3));

    // A merge marker covers the same branch of the guard.
    std::fs::remove_dir(t.a.join(".git/rebase-merge")).unwrap();
    std::fs::write(t.a.join(".git/MERGE_HEAD"), "0".repeat(40)).unwrap();
    let rc = Command::new("sh")
        .arg(".git/hooks/post-commit")
        .current_dir(&t.a)
        .status()
        .unwrap();
    assert!(rc.success());
    std::fs::remove_file(t.a.join(".git/MERGE_HEAD")).unwrap();

    // Detached HEAD — no branch means nothing to push.
    let (ok, _) = git(&t.a, &["checkout", "-q", "--detach", "HEAD"]);
    assert!(ok);
    let rc = Command::new("sh")
        .arg(".git/hooks/post-commit")
        .current_dir(&t.a)
        .status()
        .unwrap();
    assert!(rc.success());
    assert_remote_stable(&t.remote, "main", &tip0, Duration::from_secs(3));
}

// ---------- I3: job-derived status, exact binding, SSE, agent detail ----------

/// In-process daemon for the runtime strip — the same `daemon::serve`
/// common/mod.rs wraps in TestDaemon, pared down to what the board
/// routes need.
struct UiDaemon {
    state: PathBuf,
    _tmp: Option<TempDir>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl UiDaemon {
    fn start() -> Self {
        let tmp = TempDir::new().unwrap();
        Self::serve(tmp.path().to_path_buf(), Some(tmp), None)
    }

    /// Serve on a caller-owned state dir — for fixtures whose `state`
    /// the cli-under-test already points at.
    fn start_on(state: PathBuf) -> Self {
        Self::serve(state, None, None)
    }

    /// `start_on` with the daemon's `CADENCE_PM_DIR` bound — a dispatch
    /// reads the tracker daemon-side (`dispatch_send`), so the daemon
    /// must see the same pm dir the cli calls do.
    fn start_on_pm(state: PathBuf, pm: &Path) -> Self {
        Self::serve(state, None, Some(pm))
    }

    fn serve(state: PathBuf, tmp: Option<TempDir>, pm: Option<&Path>) -> Self {
        let owned = state.clone();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let provider_env = cadence_agent::adapter::ProviderEnv::default();
        if let Some(pm) = pm {
            provider_env.set("CADENCE_PM_DIR", pm.to_str().unwrap());
        }
        let opts = daemon::ServeOptions {
            stop: Some(stop.clone()),
            provider_env,
            ..Default::default()
        };
        let handle = thread::spawn(move || {
            let _ = daemon::serve_with(&owned, opts);
        });
        let d = Self {
            state,
            _tmp: tmp,
            stop,
            handle: Some(handle),
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if d.rpc_opt("health", json!({})).is_ok() {
                return d;
            }
            assert!(Instant::now() < deadline, "daemon did not become healthy");
            thread::sleep(Duration::from_millis(50));
        }
    }

    /// Serve with the operator-auth clock `clock` (CAD-313): a test
    /// advances it past a login link's TTL instead of sleeping.
    fn start_with_clock(
        state: PathBuf,
        clock: std::sync::Arc<dyn Fn() -> i64 + Send + Sync>,
    ) -> Self {
        let owned = state.clone();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let opts = daemon::ServeOptions {
            operator_clock: Some(clock),
            stop: Some(stop.clone()),
            ..Default::default()
        };
        let handle = thread::spawn(move || {
            let _ = daemon::serve_with(&owned, opts);
        });
        let d = Self {
            state,
            _tmp: None,
            stop,
            handle: Some(handle),
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        while d.rpc_opt("health", json!({})).is_err() {
            assert!(Instant::now() < deadline, "daemon did not become healthy");
            thread::sleep(Duration::from_millis(50));
        }
        d
    }

    fn rpc_opt(&self, method: &str, params: Value) -> cadence_agent::Result<Value> {
        client::rpc(&self.state, method, params)
    }

    /// A fixture call — the operator's act — that must succeed. When the
    /// suite runs in an agent pane, this process's ancestry carries
    /// `CADENCE_ALIAS` and operator-only methods (`agent_register`,
    /// `task_dispatch`, …) refuse it, correctly; the refused call is
    /// made again the way an operator shell outside every pane looks to
    /// the daemon ([`Self::operator_rpc`], CAD-471). Gate assertions use
    /// [`Self::rpc_opt`], which never retries.
    fn rpc(&self, method: &str, params: Value) -> Value {
        match self.rpc_opt(method, params.clone()) {
            Err(e) if e.to_string().contains("not provably the operator") => {
                self.operator_rpc(method, params)
            }
            r => r,
        }
        .unwrap_or_else(|e| panic!("{method}: {e}"))
    }

    /// `rpc` from a caller that is provably the operator however the
    /// suite is run — `TestDaemon::operator_rpc` in tests/common/mod.rs
    /// (CAD-291): `setsid -f` hands the call to a fresh session leader
    /// that waits until it has left this process's ancestry,
    /// `env_clear` leaves no `CADENCE_ALIAS`, and stdio is not a pane
    /// tty — the residual `peer::operator_proof` accepts. The gate
    /// itself is untouched.
    fn operator_rpc(&self, method: &str, params: Value) -> cadence_agent::Result<Value> {
        let script = self.state.join("operator-rpc.py");
        if !script.exists() {
            std::fs::write(&script, OPERATOR_RPC_PY).unwrap();
        }
        let out = self.state.join(format!(
            "operator-rpc-{}.json",
            uuid::Uuid::new_v4().simple()
        ));
        let frame = json!({"method": method, "params": params}).to_string();
        let status = Command::new("setsid")
            .arg("-f")
            .arg("python3")
            .arg(&script)
            .arg(client::socket_path(&self.state))
            .arg(&frame)
            .arg(&out)
            .arg(std::process::id().to_string())
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "setsid -f failed: {status}");
        let deadline = Instant::now() + Duration::from_secs(20);
        while !out.exists() {
            assert!(
                Instant::now() < deadline,
                "operator rpc {method} never answered"
            );
            thread::sleep(Duration::from_millis(20));
        }
        let frame: Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
        let _ = std::fs::remove_file(&out);
        cadence_agent::proto::unwrap(frame)
    }

    fn state(&self) -> PathBuf {
        self.state.clone()
    }
}

impl Drop for UiDaemon {
    /// Not the `shutdown` RPC: when the suite runs in an agent pane,
    /// this process's ancestry carries `CADENCE_ALIAS`, and the caller
    /// rule refuses it `shutdown` (CAD-384) — the join then waited
    /// forever (CAD-471). The in-process stop flag needs no connection.
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            join_within(handle, DAEMON_STOP_BOUND, "the in-process daemon");
        }
    }
}

/// [`UiDaemon::operator_rpc`]'s caller, as in tests/common/mod.rs: it
/// waits until it has left the test runner's ancestry, then sends one
/// frame and lands the reply line atomically.
const OPERATOR_RPC_PY: &str = r#"
import json, os, socket, sys, time

sock_path, frame, out, runner = sys.argv[1:5]

def on_lineage(pid):
    p = os.getpid()
    while p > 1:
        if p == pid:
            return True
        with open("/proc/%d/status" % p) as f:
            p = int([l for l in f if l.startswith("PPid:")][0].split()[1])
    return False

while on_lineage(int(runner)):
    time.sleep(0.02)
s = socket.socket(socket.AF_UNIX)
s.connect(sock_path)
s.sendall((frame + "\n").encode())
line = s.makefile().readline()
s.close()
with open(out + ".tmp", "w") as f:
    f.write(line)
os.rename(out + ".tmp", out)
"#;

/// `OPERATOR_RPC_PY`'s sibling for process exec, as in
/// tests/common/mod.rs: it waits until it has left the test runner's
/// ancestry, then runs the argv and lands the result atomically — so a
/// cli child presents as an operator shell outside every pane, not a
/// process the daemon launched (CAD-467: `dispatch`'s lane-provenance
/// send needs that proof).
const OPERATOR_EXEC_PY: &str = r#"
import json, os, subprocess, sys, time

spec_path, out, runner = sys.argv[1:4]
spec = json.load(open(spec_path))

def on_lineage(pid):
    p = os.getpid()
    while p > 1:
        if p == pid:
            return True
        with open("/proc/%d/status" % p) as f:
            p = int([l for l in f if l.startswith("PPid:")][0].split()[1])
    return False

while on_lineage(int(runner)):
    time.sleep(0.02)
r = subprocess.run(spec["argv"], env=spec["env"], cwd=spec["cwd"],
                   stdin=subprocess.DEVNULL, capture_output=True)
open(out + ".stdout", "wb").write(r.stdout)
open(out + ".stderr", "wb").write(r.stderr)
with open(out + ".tmp", "w") as f:
    json.dump({"rc": r.returncode}, f)
os.rename(out + ".tmp", out)
"#;

/// `cli` detached so the daemon sees a provably-operator caller (the
/// CAD-291/431 seam, `OperatorOutput::operator_output` in
/// tests/common/mod.rs): `setsid -f` reparents it off this process's
/// ancestry and the env carries no agent identity.
fn cli_op(pm: &Path, state: &Path, args: &[&str]) -> (bool, Value) {
    let dir = state.join(format!("opx-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    let (script, spec, out) = (dir.join("run.py"), dir.join("spec.json"), dir.join("out"));
    std::fs::write(&script, OPERATOR_EXEC_PY).unwrap();
    let mut env: std::collections::BTreeMap<String, String> = std::env::vars()
        .filter(|(k, _)| k != "CADENCE_ALIAS" && k != "CADENCE_ROLLOUT_AS")
        .collect();
    env.insert("CADENCE_PM_DIR".into(), pm.to_str().unwrap().into());
    env.insert(
        "PATH".into(),
        format!(
            "{}:{}",
            Path::new(bin()).parent().unwrap().display(),
            std::env::var("PATH").unwrap_or_default()
        ),
    );
    let mut argv = vec![
        bin().to_string(),
        "--state-dir".into(),
        state.to_str().unwrap().into(),
    ];
    argv.extend(args.iter().map(|a| a.to_string()));
    std::fs::write(
        &spec,
        json!({"argv": argv, "env": env,
               "cwd": std::env::current_dir().unwrap()})
        .to_string(),
    )
    .unwrap();
    let status = Command::new("setsid")
        .arg("-f")
        .arg("python3")
        .arg(&script)
        .arg(&spec)
        .arg(&out)
        .arg(std::process::id().to_string())
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "setsid -f failed: {status}");
    let deadline = Instant::now() + Duration::from_secs(120);
    while !out.exists() {
        assert!(
            Instant::now() < deadline,
            "operator cli {args:?} never finished"
        );
        thread::sleep(Duration::from_millis(20));
    }
    let rc: Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
    let text = {
        let stdout = std::fs::read_to_string(dir.join("out.stdout")).unwrap();
        if stdout.is_empty() {
            std::fs::read_to_string(dir.join("out.stderr")).unwrap()
        } else {
            stdout
        }
    };
    let _ = std::fs::remove_dir_all(&dir);
    (
        rc["rc"].as_i64() == Some(0),
        serde_json::from_str(text.trim()).unwrap_or(Value::String(text)),
    )
}

/// How long a stopped in-process daemon may take to return: its accept
/// poll plus `Shared::shutdown` joining the actors.
const DAEMON_STOP_BOUND: Duration = Duration::from_secs(60);

/// Join `handle`, failing the test — never hanging it — when the thread
/// is still running after `bound` (CAD-471).
fn join_within(handle: thread::JoinHandle<()>, bound: Duration, what: &str) {
    let deadline = Instant::now() + bound;
    while !handle.is_finished() {
        if Instant::now() >= deadline {
            // A second panic while unwinding would abort the runner.
            if !thread::panicking() {
                panic!("{what} did not stop within {bound:?} of its stop flag");
            }
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
    let _ = handle.join();
}

/// CAD-471: CI runs outside every pane, so the suite from an agent pane
/// is only covered here. Re-run the probe below in a child whose
/// environment carries an agent's alias, the shape of a run from a
/// pane: its in-process daemon and board must stop, and the child must
/// finish, instead of the drop's join waiting forever.
#[test]
fn in_process_daemon_stops_from_an_agent_runner() {
    let mut child = Command::new(std::env::current_exe().unwrap());
    child
        .args([
            "--exact",
            "in_process_daemon_stops_from_an_agent_runner_probe",
            "--ignored",
        ])
        .env("CADENCE_ALIAS", "cad471-runner")
        .env("CAD471_PROBE", "1")
        // A plain libtest child: the outer run's suite lock and
        // nextest markers are not its to honour.
        .env_remove("CADENCE_SUITE_LOCK")
        .env_remove("CADENCE_REVIEW_SUITE_LOCK_HELD")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("NEXTEST") {
            child.env_remove(key);
        }
    }
    let mut child = child.spawn().unwrap();
    let deadline = Instant::now() + DAEMON_STOP_BOUND + Duration::from_secs(30);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            // Our own child: killing it can never touch another process.
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "the agent-shaped probe hung: its in-process daemon or board did not stop \
                 (a drop waiting on a `shutdown` the caller rule refuses, CAD-471)"
            );
        }
        thread::sleep(Duration::from_millis(50));
    }
    let out = child.wait_with_output().unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "{text}");
    assert!(
        text.contains("1 passed"),
        "the probe must actually run: {text}"
    );
}

#[test]
#[ignore = "run by in_process_daemon_stops_from_an_agent_runner as an agent-shaped child"]
fn in_process_daemon_stops_from_an_agent_runner_probe() {
    if std::env::var("CAD471_PROBE").as_deref() != Ok("1") {
        return;
    }
    assert!(std::env::var("CADENCE_ALIAS").is_ok());
    let d = UiDaemon::start();
    let pm = TempDir::new().unwrap();
    seed(pm.path(), &d.state());
    let (port, board) = start_ui(pm.path().to_path_buf(), d.state());
    // The gate is untouched: this process carries an agent's
    // environment, so it may not stop the daemon over its socket.
    let err = d.rpc_opt("shutdown", json!({})).unwrap_err();
    assert!(err.to_string().contains("carries CADENCE_ALIAS"), "{err}");
    assert_eq!(d.rpc("health", json!({}))["state"], "ready");
    // The in-process stops need no connection: both return.
    drop(board);
    wait_port_closed(port);
    drop(d);
}

/// A dropped [`BoardStop`] closes its board's port within its accept
/// poll (CAD-471) — the board does not serve on for the rest of the run.
#[test]
fn board_stops_when_its_guard_drops() {
    let (pm, state) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    seed(pm.path(), state.path());
    let (port, board) = start_ui(pm.path().to_path_buf(), state.path().to_path_buf());
    let host = format!("127.0.0.1:{port}");
    assert_eq!(http(port, "GET", "/api/health", &host).0, 200);
    drop(board);
    wait_port_closed(port);
}

/// Wait until nothing accepts on `port`; fail after 10 s.
fn wait_port_closed(port: u16) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while TcpStream::connect(("127.0.0.1", port)).is_ok() {
        assert!(
            Instant::now() < deadline,
            "the board on {port} still accepts after its stop"
        );
        thread::sleep(Duration::from_millis(50));
    }
}

/// Plant `pid` as `alias`'s live pty pane — the row shape the daemon's
/// pane map resolves callers by (a `pty` endpoint with a pid and a
/// generation). Registered as an actorless `inbox` pair first and kept
/// `enabled=0`, so no actor ever opens it and overwrites the plant —
/// the same recipe common/mod.rs's `plant_pane` uses.
fn plant_pane(d: &UiDaemon, alias: &str, pid: u32) {
    d.rpc(
        "agent_register",
        json!({"alias": alias, "provider": "inbox", "endpoint_kind": "inbox",
               "cwd": d.state().to_str().unwrap()}),
    );
    let conn = rusqlite::Connection::open(d.state().join("cadence.sqlite3")).unwrap();
    conn.execute(
        "UPDATE agents SET endpoint_kind='pty', pid=?1, pid_start=?3, enabled=0, \
            generation='planted', session_id='planted' WHERE alias=?2",
        rusqlite::params![pid as i64, alias, proc_start(pid)],
    )
    .unwrap();
}

/// `/proc/<pid>/stat` field 22 — what the daemon records as a pid's
/// `pid_start` (CAD-385), so a planted row names exactly that process.
fn proc_start(pid: u32) -> Option<i64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    stat.rsplit_once(')')?
        .1
        .split_whitespace()
        .nth(19)?
        .parse()
        .ok()
}

/// CAD-254: the write guards stop browsers, not local processes. A
/// board write from a process that descends from a registered pane is
/// that agent's write — its alias lands in the commit and as the
/// comment author, never `operator` — while a peer on no pane's lineage
/// (this test process, standing in for the operator's browser) writes
/// as `operator (ui)` only with the operator's session (CAD-313) and is
/// refused without one. With the store present but the daemon gone the
/// panes are unknowable, so a write is refused, not guessed.
#[test]
fn ui_write_caller_derives_from_pane_ancestry() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    seed(pm.path(), state.path());
    let d = UiDaemon::start_on(state.path().to_path_buf());
    let (port, _board) = start_ui(pm.path().to_path_buf(), state.path().to_path_buf());
    let host = format!("127.0.0.1:{port}");

    // The pane: a bash that waits for a go line, then runs the client
    // as its CHILD — a plain HTTP write over bash's /dev/tcp, so the
    // peer holding the socket descends from the planted pane pid.
    let body = r#"{"body":"from the pane"}"#;
    let request = format!(
        "POST /api/issues/CAD-3/comments HTTP/1.0\r\nHost: {host}\r\n\
         Content-Type: application/json\r\nX-Cadence-Board: 1\r\n\
         Origin: http://{host}\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let mut pane = Command::new("bash")
        .args(["-c", r#"read -r _; bash -c "$CLIENT"; true"#])
        .env(
            "CLIENT",
            r#"exec 3<>"/dev/tcp/127.0.0.1/$PORT"; printf '%s' "$REQ" >&3; cat <&3"#,
        )
        .env("PORT", port.to_string())
        .env("REQ", &request)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    plant_pane(&d, "pane-w", pane.id());
    pane.stdin.take().unwrap().write_all(b"go\n").unwrap();
    let mut response = String::new();
    pane.stdout
        .take()
        .unwrap()
        .read_to_string(&mut response)
        .unwrap();
    assert!(pane.wait().unwrap().success());
    assert!(
        response.starts_with("HTTP/1.1 200") || response.starts_with("HTTP/1.0 200"),
        "{response}"
    );
    let json_body = response.split_once("\r\n\r\n").unwrap().1;
    let v: Value = serde_json::from_str(json_body).unwrap();
    let comment = v["issue"]["comments"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["body"] == "from the pane")
        .unwrap()
        .clone();
    assert_eq!(comment["author"], "pane-w", "{comment}");
    let (_, last) = git(pm.path(), &["log", "-1", "--format=%B"]);
    assert!(last.contains("(pane-w)"), "{last}");
    assert!(last.contains("Actor: pane-w"), "{last}");
    assert!(!last.contains("operator"), "{last}");

    // On no pane's lineage is not the operator (CAD-313, F1): without
    // a session this test process is refused and nothing is written.
    let commits_before = commits(pm.path());
    let (code, _, body) = write_json(
        port,
        "PATCH",
        "/api/issues/CAD-3",
        &host,
        r#"{"priority":"P1"}"#,
    );
    assert_eq!(code, 403, "{body}");
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["check"], "operator_session_required", "{v}");
    assert_eq!(commits(pm.path()), commits_before);
    // The operator: this test process, signed in.
    let op = sign_in(state.path(), port);
    let (code, _, _) = op_write_json(
        &op,
        port,
        "PATCH",
        "/api/issues/CAD-3",
        &host,
        r#"{"priority":"P1"}"#,
    );
    assert_eq!(code, 200);
    let (_, last) = git(pm.path(), &["log", "-1", "--format=%B"]);
    assert!(last.contains("Actor: operator (ui)"), "{last}");

    // Fail closed: the store exists but no daemon can name the panes.
    let commits_before = commits(pm.path());
    drop(d);
    let (code, _, body) = write_json(
        port,
        "PATCH",
        "/api/issues/CAD-3",
        &host,
        r#"{"priority":"P2"}"#,
    );
    assert_eq!(code, 403, "{body}");
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["check"], "caller_identity", "{v}");
    assert_eq!(commits(pm.path()), commits_before);
}

/// CAD-337: the board relays a model-defaults write to the daemon over
/// its OWN connection, so the daemon's operator gate sees the board
/// process, not the HTTP caller. A caller the board attributes to a
/// pane is an agent and is refused here, before any relay, naming the
/// rule — else an agent could launder the write through the board and
/// land it as the operator's. Nothing changes.
#[test]
fn ui_model_defaults_refuses_pane_agent() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    seed(pm.path(), state.path());
    let d = UiDaemon::start_on(state.path().to_path_buf());
    let (port, _board) = start_ui(pm.path().to_path_buf(), state.path().to_path_buf());
    let host = format!("127.0.0.1:{port}");
    let doc = r#"{"expected_revision":0,"config":{"schema":1,"providers":{"claude":{"default":{"mode":"model","model":"forged-model"},"roles":{}}}}}"#;
    let request = format!(
        "POST /api/settings/model-defaults HTTP/1.0\r\nHost: {host}\r\n\
         Content-Type: application/json\r\nX-Cadence-Board: 1\r\n\
         Origin: http://{host}\r\nContent-Length: {}\r\n\r\n{doc}",
        doc.len()
    );
    let mut pane = Command::new("bash")
        .args(["-c", r#"read -r _; bash -c "$CLIENT"; true"#])
        .env(
            "CLIENT",
            r#"exec 3<>"/dev/tcp/127.0.0.1/$PORT"; printf '%s' "$REQ" >&3; cat <&3"#,
        )
        .env("PORT", port.to_string())
        .env("REQ", &request)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    plant_pane(&d, "pane-m", pane.id());
    pane.stdin.take().unwrap().write_all(b"go\n").unwrap();
    let mut response = String::new();
    pane.stdout
        .take()
        .unwrap()
        .read_to_string(&mut response)
        .unwrap();
    assert!(pane.wait().unwrap().success());
    assert!(
        response.starts_with("HTTP/1.1 403") || response.starts_with("HTTP/1.0 403"),
        "{response}"
    );
    let v: Value = serde_json::from_str(response.split_once("\r\n\r\n").unwrap().1).unwrap();
    assert_eq!(v["check"], "operator_only", "{v}");
    let msg = v["error"].as_str().unwrap_or_default();
    assert!(msg.contains("pane-m") && msg.contains("operator"), "{v}");
    let (code, body) = http(port, "GET", "/api/settings/model-defaults", &host);
    assert_eq!(code, 200, "{body}");
    let current: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(current["revision"], 0, "{current}");
}

/// A POST of `body` as a comment on CAD-3 — the raw request a pane-side
/// client writes over bash's /dev/tcp.
fn comment_request(host: &str, body: &str) -> String {
    let body = format!(r#"{{"body":"{body}"}}"#);
    format!(
        "POST /api/issues/CAD-3/comments HTTP/1.0\r\nHost: {host}\r\n\
         Content-Type: application/json\r\nX-Cadence-Board: 1\r\n\
         Origin: http://{host}\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
}

/// The comment `body` in a board write's HTTP reply.
fn replied_comment(response: &str, body: &str) -> Value {
    assert!(
        response.starts_with("HTTP/1.1 200") || response.starts_with("HTTP/1.0 200"),
        "{response}"
    );
    let v: Value = serde_json::from_str(response.split_once("\r\n\r\n").unwrap().1).unwrap();
    v["issue"]["comments"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["body"] == body)
        .unwrap()
        .clone()
}

/// Runs a bash script as a "pane" whose stdio is a real pty, relaying
/// the test's stdin lines to it; prints the pane's pid first.
const PTY_PANE_PY: &str = r#"
import os, subprocess, sys, threading
m, s = os.openpty()
p = subprocess.Popen(["bash", "-c", sys.argv[1]], stdin=s, stdout=s, stderr=s)
os.close(s)
print(p.pid, flush=True)
def drain():
    try:
        while os.read(m, 4096):
            pass
    except OSError:
        pass
threading.Thread(target=drain, daemon=True).start()
for line in sys.stdin:
    os.write(m, line.encode())
p.wait()
"#;

/// CAD-263: a `setsid`'d child of a registered pane has no pane on its
/// `/proc` ancestry, but a detach keeps stdio — it still holds the
/// pane's pty, the process signal the board shares with the daemon, so
/// the write is the agent's, never `operator`'s. The client
/// double-forks (`setsid -f`) and connects only once the pane is
/// provably off its ancestry, so ancestry cannot carry it.
#[test]
fn ui_write_caller_attributes_a_setsid_child_of_a_pane() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let out_dir = TempDir::new().unwrap();
    seed(pm.path(), state.path());
    let d = UiDaemon::start_on(state.path().to_path_buf());
    let (port, _board) = start_ui(pm.path().to_path_buf(), state.path().to_path_buf());
    let host = format!("127.0.0.1:{port}");
    let out = out_dir.path().join("response");
    // The client waits until the pane pid is off its ancestry (the
    // `setsid -f` intermediate has exited and it was reparented), then
    // writes over bash's /dev/tcp and lands the reply atomically. Its
    // stdio stays the pane's pty.
    let client = r#"
        on_pane_lineage() {
            p=$$
            while [ "$p" -gt 1 ]; do
                [ "$p" = "$PANE" ] && return 0
                p=$(awk '/^PPid:/{print $2}' "/proc/$p/status") || return 0
                [ -n "$p" ] || return 0
            done
            return 1
        }
        while on_pane_lineage; do sleep 0.02; done
        exec 3<>"/dev/tcp/127.0.0.1/$PORT"
        printf '%s' "$REQ" >&3
        cat <&3 >"$OUT.tmp" && mv "$OUT.tmp" "$OUT"
    "#;
    let mut pane = Command::new("python3")
        .args([
            "-c",
            PTY_PANE_PY,
            r#"read -r _; PANE=$$ setsid -f bash -c "$CLIENT"; read -r _; true"#,
        ])
        .env("CADENCE_ALIAS", "pane-s")
        .env("CLIENT", client)
        .env("PORT", port.to_string())
        .env("REQ", comment_request(&host, "from a detached child"))
        .env("OUT", &out)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut first = String::new();
    BufReader::new(pane.stdout.take().unwrap())
        .read_line(&mut first)
        .unwrap();
    let pane_pid: u32 = first.trim().parse().unwrap();
    plant_pane(&d, "pane-s", pane_pid);
    let mut stdin = pane.stdin.take().unwrap();
    stdin.write_all(b"go\n").unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !out.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "the detached client never answered"
        );
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    stdin.write_all(b"done\n").unwrap();
    drop(stdin);
    assert!(pane.wait().unwrap().success());
    let response = std::fs::read_to_string(&out).unwrap();
    let comment = replied_comment(&response, "from a detached child");
    assert_eq!(comment["author"], "pane-s", "{comment}");
    let (_, last) = git(pm.path(), &["log", "-1", "--format=%B"]);
    assert!(last.contains("Actor: pane-s"), "{last}");
    assert!(!last.contains("operator"), "{last}");
}

/// CAD-263 review: `CADENCE_ALIAS` is caller-chosen, so on the board it
/// never attributes by itself. A pane-less process exporting a
/// registered pane's alias — no ancestry, no pane pty — is not that
/// agent, and (CAD-313, flipped deliberately per ADR 0004 §6) not the
/// operator either: without a session it is refused and writes nothing.
#[test]
fn ui_write_caller_ignores_an_uncorroborated_env_alias() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    seed(pm.path(), state.path());
    let d = UiDaemon::start_on(state.path().to_path_buf());
    let (port, _board) = start_ui(pm.path().to_path_buf(), state.path().to_path_buf());
    let host = format!("127.0.0.1:{port}");
    // pane-b is a registered, live pane the client is unrelated to.
    let mut pane_b = Command::new("sleep")
        .arg("600")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    plant_pane(&d, "pane-b", pane_b.id());
    let client = Command::new("bash")
        .args([
            "-c",
            r#"exec 3<>"/dev/tcp/127.0.0.1/$PORT"; printf '%s' "$REQ" >&3; cat <&3"#,
        ])
        .env("CADENCE_ALIAS", "pane-b")
        .env("PORT", port.to_string())
        .env("REQ", comment_request(&host, "forged alias"))
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .unwrap();
    let _ = pane_b.kill();
    let _ = pane_b.wait();
    let response = String::from_utf8(client.stdout).unwrap();
    assert!(
        response.starts_with("HTTP/1.1 403") || response.starts_with("HTTP/1.0 403"),
        "{response}"
    );
    assert!(response.contains("operator_session_required"), "{response}");
    let (_, last) = git(pm.path(), &["log", "-1", "--format=%B"]);
    assert!(!last.contains("forged alias"), "{last}");
    assert!(!last.contains("pane-b"), "{last}");
}

/// CAD-276 PINS AN ACCEPTED RESIDUAL — see the PM decision on CAD-276
/// and the `src/peer.rs` module doc. The pty-on-stdio tie is
/// caller-choosable: a same-uid process on NO pane's ancestry (a child
/// of this test, no `CADENCE_ALIAS`) opens a registered pane's
/// `/dev/pts/N` onto its stderr and is attributed as that pane's agent
/// — lateral authorship forgery, no privilege over `operator (ui)`.
/// The tie stays because dropping it sends `setsid` children of panes
/// back to `operator (ui)` (an escalation). A future fix
/// (operator-by-positive-proof, or a second signal) must flip this
/// test DELIBERATELY: the expected author then stops being `pane-v`.
#[test]
fn ui_write_caller_pty_tie_is_forgeable_residual_pinned() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    seed(pm.path(), state.path());
    let d = UiDaemon::start_on(state.path().to_path_buf());
    let (port, _board) = start_ui(pm.path().to_path_buf(), state.path().to_path_buf());
    let host = format!("127.0.0.1:{port}");
    // The victim pane: a bash whose stdio is a real pty, idling.
    let mut pane = Command::new("python3")
        .args(["-c", PTY_PANE_PY, "read -r _; true"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut first = String::new();
    BufReader::new(pane.stdout.take().unwrap())
        .read_line(&mut first)
        .unwrap();
    let pane_pid: u32 = first.trim().parse().unwrap();
    plant_pane(&d, "pane-v", pane_pid);
    let pts = std::fs::read_link(format!("/proc/{pane_pid}/fd/0")).unwrap();
    assert!(pts.to_string_lossy().starts_with("/dev/pts/"), "{pts:?}");
    // The forger: this test's child — never on the pane's ancestry —
    // with the pane's pts opened onto its stderr, nothing else.
    let client = Command::new("bash")
        .args([
            "-c",
            r#"exec 2>"$PTS"; exec 3<>"/dev/tcp/127.0.0.1/$PORT"; printf '%s' "$REQ" >&3; cat <&3"#,
        ])
        .env_remove("CADENCE_ALIAS")
        .env("PTS", &pts)
        .env("PORT", port.to_string())
        .env("REQ", comment_request(&host, "forged via pty"))
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    let mut stdin = pane.stdin.take().unwrap();
    stdin.write_all(b"done\n").unwrap();
    drop(stdin);
    let _ = pane.wait();
    let response = String::from_utf8(client.stdout).unwrap();
    let comment = replied_comment(&response, "forged via pty");
    assert_eq!(
        comment["author"], "pane-v",
        "CAD-276 residual changed — if deliberate, flip this pin: {comment}"
    );
    let (_, last) = git(pm.path(), &["log", "-1", "--format=%B"]);
    assert!(last.contains("Actor: pane-v"), "{last}");
}

/// Seed the tracker and daemon-side world for the binding tests:
/// pm + wk fake agents, a job bound to `issue`, one task for `wk`
/// dispatched so the task is live. Returns (job_id, task_id).
fn bound_job(pm: &Path, d: &UiDaemon, issue: &str) -> (String, String) {
    let cwd = pm.to_str().unwrap();
    d.rpc(
        "agent_register",
        json!({"alias": "pm", "provider": "fake",
               "endpoint_kind": "fake", "cwd": cwd}),
    );
    // The worker must sit in the pm's group or dispatch refuses it.
    d.rpc(
        "agent_register",
        json!({"alias": "wk", "provider": "fake",
               "endpoint_kind": "fake", "cwd": cwd,
               "params": "{\"upstream\":\"pm\"}"}),
    );
    let spec = pm.join("spec.md");
    std::fs::write(&spec, "# spec\n").unwrap();
    let job = d.rpc(
        "job_new",
        json!({"pm": "pm", "spec": spec, "spec_sha256": "test",
               "issue": issue, "title": "bound job"}),
    );
    let job_id = job["job"]["id"].as_str().unwrap().to_string();
    // The acceptance text names a DIFFERENT issue on purpose — the
    // kickoff body embeds it ahead of the real "tracks issue <id>"
    // line, so a message-text scan would bind `wk` to CAD-1 while the
    // task join binds the job's real issue.
    let task = d.rpc(
        "task_new",
        json!({"job": job_id, "assignee": "wk", "title": "worker task",
               "acceptance": "verify against CAD-1"}),
    );
    let task_id = task["task"]["id"].as_str().unwrap().to_string();
    d.rpc("task_dispatch", json!({"task": task_id, "by": "operator"}));
    (job_id, task_id)
}

#[test]
fn ui_job_state_drives_status_and_binding() {
    let pm = TempDir::new().unwrap();
    let d = UiDaemon::start();
    seed(pm.path(), &d.state());
    // CAD-3 is the leaf — CAD-1 is a container whose roll-up legitimately
    // outranks any job.
    let (_, task_id) = bound_job(pm.path(), &d, "CAD-3");
    let (port, _board) = start_ui(pm.path().to_path_buf(), d.state());
    let host = format!("127.0.0.1:{port}");
    // CAD-313: a board write is the operator's only with a session —
    // this test process signs in and writes as `operator (ui)`.
    let op = sign_in(&d.state(), port);
    let write_json = |port: u16, method: &str, path: &str, host: &str, body: &str| {
        op_write_json(&op, port, method, path, host, body)
    };

    // Card: job state wins over notes/file and binds the agent.
    let (code, body) = http(port, "GET", "/api/issues", &host);
    assert_eq!(code, 200);
    let issues: Value = serde_json::from_str(&body).unwrap();
    let card = issues["issues"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == "CAD-3")
        .cloned()
        .expect("CAD-3 card");
    assert_eq!(card["status_source"], "job");
    assert!(
        matches!(card["status"].as_str(), Some("doing" | "review" | "done")),
        "job-derived status: {}",
        card["status"]
    );
    let bound = &card["agents"];
    assert!(
        bound
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["alias"] == "wk" && a["task"] == task_id),
        "card agents strip: {bound}"
    );

    // Detail: same strip on the drawer payload.
    let (code, body) = http(port, "GET", "/api/issues/CAD-3", &host);
    assert_eq!(code, 200);
    let detail: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(detail["status_source"], "job");
    assert!(
        detail["agents"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["alias"] == "wk" && a["task"] == task_id),
        "detail agents strip: {}",
        detail["agents"]
    );

    // Agents payload: the exact join — wk is `on` CAD-3 through its
    // task, and by_issue carries the strip for the card.
    let (code, body) = http(port, "GET", "/api/agents", &host);
    assert_eq!(code, 200);
    let agents: Value = serde_json::from_str(&body).unwrap();
    let wk = agents["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["alias"] == "wk")
        .cloned()
        .expect("wk row");
    assert!(
        wk["on"].as_array().unwrap().iter().any(|i| i == "CAD-3"),
        "wk.on: {}",
        wk["on"]
    );
    assert!(
        wk["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["task"] == task_id && t["issue"] == "CAD-3"),
        "wk.tasks: {}",
        wk["tasks"]
    );
    assert!(
        agents["by_issue"]["CAD-3"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["alias"] == "wk" && a["task"] == task_id),
        "by_issue.CAD-3: {}",
        agents["by_issue"]["CAD-3"]
    );
    // The kickoff body names CAD-1 (acceptance text) ahead of the real
    // issue line — a message-text scan would bind `wk` there. The join
    // must not.
    let binds_cad1 = agents["by_issue"]["CAD-1"]
        .as_array()
        .map(|v| v.iter().any(|a| a["alias"] == "wk"))
        .unwrap_or(false)
        || wk["on"].as_array().unwrap().iter().any(|i| i == "CAD-1");
    assert!(!binds_cad1, "decoy issue id in the message must not bind");

    // A manual status write against a job-derived status is refused —
    // the same 409 the CLI enforces on notes/rollup-derived statuses.
    let (code, _, body) = write_json(
        port,
        "PATCH",
        "/api/issues/CAD-3",
        &host,
        r#"{"status":"done"}"#,
    );
    assert_eq!(code, 409, "derived status write must conflict: {body}");
    assert!(body.contains("derived"), "409 names the cause: {body}");
}

#[test]
fn ui_overview_surfaces_durable_monitor_alert_and_acknowledges_it() {
    let pm = TempDir::new().unwrap();
    let d = UiDaemon::start();
    seed(pm.path(), &d.state());
    let cwd = pm.path().to_str().unwrap();
    d.rpc(
        "agent_register",
        json!({"alias": "pm", "provider": "fake",
               "endpoint_kind": "fake", "cwd": cwd}),
    );
    d.rpc(
        "agent_register",
        json!({"alias": "wk", "provider": "fake",
               "endpoint_kind": "fake", "cwd": cwd,
               "params": "{\"upstream\":\"pm\"}"}),
    );
    let spec = pm.path().join("monitor-ui.md");
    std::fs::write(&spec, "# monitor ui\n").unwrap();
    d.rpc(
        "job_new",
        json!({"pm": "pm", "job": "ui-monitor-job", "spec": spec,
               "spec_sha256": "synthetic", "repo": "cadence", "issue": "CAD-3"}),
    );
    d.rpc(
        "task_new",
        json!({"job": "ui-monitor-job", "task": "ui-monitor-task",
               "assignee": "wk", "acceptance": "observe the monitor"}),
    );
    d.rpc(
        "monitor_register",
        json!({"monitor": "ui-monitor", "project": "cadence",
               "owner": "watchdog", "tasks": ["ui-monitor-task"],
               "interval_secs": 1}),
    );
    let active_deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let monitor = d.rpc("monitor_show", json!({"monitor": "ui-monitor"}))["monitor"].clone();
        if monitor["monitoring"] == "active" {
            assert!(monitor["last_success_at"].is_number(), "{monitor}");
            break;
        }
        assert!(
            Instant::now() < active_deadline,
            "monitor never became active: {monitor}"
        );
        thread::sleep(Duration::from_millis(50));
    }
    let store = Store::open(&d.state.join("cadence.sqlite3")).unwrap();
    store
        .event_public_scoped(
            "wk",
            "turn_stalled",
            json!({"message": "synthetic-stall", "episode": 1}),
            Some("ui-monitor-job"),
            Some("ui-monitor-task"),
        )
        .unwrap();
    let alert = loop {
        let page = d.rpc("monitor_alerts", json!({"monitor": "ui-monitor"}));
        if let Some(alert) = page["alerts"].as_array().and_then(|a| a.first()) {
            break alert.clone();
        }
        assert!(
            Instant::now() < active_deadline + Duration::from_secs(5),
            "alert not observed: {page}"
        );
        thread::sleep(Duration::from_millis(50));
    };

    // The ack relays through the board's own daemon connection, which
    // the caller rule proves (CAD-384) — an operator-shaped board, so
    // this passes from an agent pane too (CAD-471, as CAD-380).
    let (port, _board) = start_operator_ui(pm.path(), &d.state());
    let host = format!("127.0.0.1:{port}");
    // CAD-313: a board write is the operator's only with a session —
    // this test process signs in and writes as `operator (ui)`.
    let op = sign_in(&d.state(), port);
    let write_json = |port: u16, method: &str, path: &str, host: &str, body: &str| {
        op_write_json(&op, port, method, path, host, body)
    };
    let (code, body) = http(port, "GET", "/api/overview", &host);
    assert_eq!(code, 200, "{body}");
    let overview: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(overview["monitoring"]["state"], "active", "{overview}");
    assert!(
        overview["monitoring"]["last_success_at"].is_number(),
        "{overview}"
    );
    let monitor = &overview["monitoring"]["monitors"][0];
    assert!(monitor["heartbeat_at"].is_number(), "{overview}");
    assert!(monitor["last_check_at"].is_number(), "{overview}");
    assert_eq!(
        monitor["coverage"],
        json!(["ui-monitor-task"]),
        "{overview}"
    );
    assert_eq!(overview["monitoring"]["open_alerts"], 1, "{overview}");
    assert_eq!(overview["monitoring"]["alerts"][0]["project"], "cadence");
    assert_eq!(
        overview["monitoring"]["alerts"][0]["next_owner"],
        "watchdog"
    );
    assert_eq!(
        overview["monitoring"]["alerts"][0]["evidence"]["event_seq"],
        alert["event_seq"]
    );

    let seq = alert["seq"].as_i64().unwrap();
    let (code, _, body) = write_json(
        port,
        "POST",
        &format!("/api/monitors/ui-monitor/alerts/{seq}/ack"),
        &host,
        "{}",
    );
    assert_eq!(code, 200, "{body}");
    let ack: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(ack["alert"]["state"], "acknowledged", "{ack}");

    let (code, body) = http(port, "GET", "/api/overview", &host);
    assert_eq!(code, 200, "{body}");
    let overview: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(overview["monitoring"]["open_alerts"], 0, "{overview}");
    assert_eq!(
        overview["monitoring"]["alerts"][0]["state"], "acknowledged",
        "{overview}"
    );

    // The durable acknowledgement remains behind the board's existing
    // read-only guard; a shared browse-only board cannot claim it handled
    // an operator alert.
    let (read_only_port, _ro_board) = start_ui_opts(pm.path().to_path_buf(), d.state(), |opts| {
        opts.read_only = true;
    });
    let read_only_host = format!("127.0.0.1:{read_only_port}");
    let (code, _, body) = write_json(
        read_only_port,
        "POST",
        &format!("/api/monitors/ui-monitor/alerts/{seq}/ack"),
        &read_only_host,
        "{}",
    );
    assert_eq!(code, 403, "read-only board must refuse monitor ack: {body}");
}

#[test]
fn ui_agent_detail_route_and_guards() {
    let pm = TempDir::new().unwrap();
    let d = UiDaemon::start();
    seed(pm.path(), &d.state());
    d.rpc(
        "agent_register",
        json!({"alias": "wk", "provider": "fake",
               "endpoint_kind": "fake", "cwd": pm.path().to_str().unwrap()}),
    );
    let (port, _board) = start_ui(pm.path().to_path_buf(), d.state());
    let host = format!("127.0.0.1:{port}");

    let (code, body) = http(port, "GET", "/api/agents/wk", &host);
    assert_eq!(code, 200);
    let detail: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(detail["agent"]["alias"], "wk");
    assert_eq!(detail["fenced"], false);
    assert!(detail["events"].is_array());
    assert!(detail["agent"]["capabilities"].is_object());

    // Alias grammar is enforced before the daemon is asked.
    let (code, _) = http(port, "GET", "/api/agents/bad%20alias", &host);
    assert_eq!(code, 400);
    let (code, _) = http(port, "GET", "/api/agents/..%2Fetc", &host);
    assert!(matches!(code, 400 | 404));
    // A well-formed but unknown alias is a daemon-level 404.
    let (code, _) = http(port, "GET", "/api/agents/ghost-1", &host);
    assert_eq!(code, 404);
}

#[test]
fn ui_stream_sse_and_guards() {
    let pm = TempDir::new().unwrap();
    let d = UiDaemon::start();
    seed(pm.path(), &d.state());
    let (port, _board) = start_ui(pm.path().to_path_buf(), d.state());
    let host = format!("127.0.0.1:{port}");

    // Method guard: HEAD on the stream is refused, not hung.
    let (code, _, _) = http_full(port, "HEAD", "/api/stream", &host);
    assert_eq!(code, 405);
    // Host guard applies to the stream exactly like any other route.
    let (code, _, _) = http_full(port, "GET", "/api/stream", "evil.example");
    assert_eq!(code, 421);

    // GET streams SSE: headers first, then frames. The reader emits a
    // `: ping` immediately, so first bytes arrive fast.
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(8))).unwrap();
    write!(s, "GET /api/stream HTTP/1.0\r\nHost: {host}\r\n\r\n").unwrap();
    let mut raw = Vec::new();
    let mut tmp = [0u8; 2048];
    let deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < deadline {
        match s.read(&mut tmp) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                raw.extend_from_slice(&tmp[..n]);
                let text = String::from_utf8_lossy(&raw);
                if text.contains(": ping") {
                    break;
                }
            }
        }
    }
    let text = String::from_utf8_lossy(&raw).to_string();
    assert!(text.contains("text/event-stream"), "headers: {text}");
    assert!(
        text.contains(": ping"),
        "first frame is the keepalive: {text}"
    );

    // CAD-258: each frame names the resources it invalidates, so the
    // client refetches only those. The event name stays the source.
    const ISSUES_FRAME: &str =
        "event: issues\ndata: {\"resources\":[\"issues\",\"projects\",\"issue\",\"overview\",\"workflows\"]}\n\n";
    const AGENTS_FRAME: &str =
        "event: agents\ndata: {\"resources\":[\"agents\",\"issue\",\"overview\"]}\n\n";
    const JOBS_FRAME: &str =
        "event: jobs\ndata: {\"resources\":[\"issues\",\"agents\",\"issue\",\"overview\"]}\n\n";

    // A tracker write moves the mtime fingerprint → `event: issues`.
    std::fs::write(pm.path().join("poke.txt"), "x").unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut got_issues = false;
    while Instant::now() < deadline {
        match s.read(&mut tmp) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                raw.extend_from_slice(&tmp[..n]);
                if String::from_utf8_lossy(&raw).contains(ISSUES_FRAME) {
                    got_issues = true;
                    break;
                }
            }
        }
    }
    assert!(
        got_issues,
        "no issues event within 8s: {}",
        String::from_utf8_lossy(&raw)
    );

    // An agent change moves the agent fingerprint → `event: agents`.
    d.rpc(
        "agent_register",
        json!({"alias": "late", "provider": "fake",
               "endpoint_kind": "fake", "cwd": pm.path().to_str().unwrap()}),
    );
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut got_agents = false;
    while Instant::now() < deadline {
        match s.read(&mut tmp) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                raw.extend_from_slice(&tmp[..n]);
                if String::from_utf8_lossy(&raw).contains(AGENTS_FRAME) {
                    got_agents = true;
                    break;
                }
            }
        }
    }
    assert!(got_agents, "no agents event within 8s");

    // A dispatch moves the job fingerprint → `event: jobs`. `bound_job`
    // creates + dispatches, both of which change `job_list`.
    bound_job(pm.path(), &d, "CAD-3");
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut got_jobs = false;
    while Instant::now() < deadline {
        match s.read(&mut tmp) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                raw.extend_from_slice(&tmp[..n]);
                if String::from_utf8_lossy(&raw).contains(JOBS_FRAME) {
                    got_jobs = true;
                    break;
                }
            }
        }
    }
    assert!(got_jobs, "no jobs event within 8s");
}

/// Poll `agent_show` until `pred` holds or the deadline passes — the
/// board-test equivalent of the common harness's wait_agent.
fn wait_agent_pred(d: &UiDaemon, alias: &str, secs: u64, pred: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        let show = d.rpc("agent_show", json!({"alias": alias}));
        if pred(&show) {
            return show;
        }
        assert!(
            Instant::now() < deadline,
            "agent {alias} never reached condition: {show}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn ui_agents_payload_covers_all_kinds() {
    let pm = TempDir::new().unwrap();
    let d = UiDaemon::start();
    seed(pm.path(), &d.state());
    let cwd = pm.path().to_str().unwrap();

    // Mailbox — inbox endpoints are counted separately, never fenced.
    d.rpc(
        "agent_register",
        json!({"alias": "obs", "provider": "inbox",
               "endpoint_kind": "inbox", "cwd": cwd}),
    );
    // Idle worker — registered, no turn in flight.
    d.rpc(
        "agent_register",
        json!({"alias": "idle1", "provider": "fake",
               "endpoint_kind": "fake", "cwd": cwd}),
    );
    // Busy worker — SLEEP holds the turn so `running` stays up.
    d.rpc(
        "agent_register",
        json!({"alias": "busy1", "provider": "fake",
               "endpoint_kind": "fake", "cwd": cwd}),
    );
    d.rpc(
        "agent_send",
        json!({"alias": "busy1", "text": "SLEEP:30", "message": "b1"}),
    );
    wait_agent_pred(&d, "busy1", 10, |s| {
        s["messages"]
            .as_array()
            .map(|ms| ms.iter().any(|m| m["state"] == "running"))
            .unwrap_or(false)
    });
    // Stopped worker — registered then stopped.
    d.rpc(
        "agent_register",
        json!({"alias": "stop1", "provider": "fake",
               "endpoint_kind": "fake", "cwd": cwd}),
    );
    d.rpc("agent_stop", json!({"alias": "stop1"}));
    // Fenced worker — DISCONNECT drops mid-turn → unknown → attention.
    d.rpc(
        "agent_register",
        json!({"alias": "fenced1", "provider": "fake",
               "endpoint_kind": "fake", "cwd": cwd}),
    );
    d.rpc(
        "agent_send",
        json!({"alias": "fenced1", "text": "DISCONNECT", "message": "f1"}),
    );
    wait_agent_pred(&d, "fenced1", 15, |s| {
        s["unknown"].as_i64().unwrap_or(0) > 0 || s["agent"]["state"].as_str() == Some("attention")
    });

    let (port, _board) = start_ui(pm.path().to_path_buf(), d.state());
    let host = format!("127.0.0.1:{port}");
    let (code, body) = http(port, "GET", "/api/agents", &host);
    assert_eq!(code, 200);
    let payload: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(payload["daemon"], "reachable");
    let row = |alias: &str| {
        payload["agents"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["alias"] == alias)
            .cloned()
            .unwrap_or_else(|| panic!("{alias} row: {}", payload["agents"]))
    };

    let obs = row("obs");
    assert_eq!(obs["inbox"], true);
    assert_eq!(obs["state"], "inbox");
    assert_eq!(obs["fenced"], false);

    let idle = row("idle1");
    assert_eq!(idle["fenced"], false);
    assert!(matches!(idle["state"].as_str(), Some("idle" | "stopped")));

    let busy = row("busy1");
    assert!(
        busy["running"].as_i64().unwrap_or(0) >= 1,
        "busy row: {busy}"
    );
    assert_eq!(busy["message"]["id"], "b1");

    let fenced = row("fenced1");
    assert_eq!(fenced["fenced"], true, "fenced row: {fenced}");
    assert!(
        fenced["recovery"].as_str().is_some_and(|t| !t.is_empty()),
        "fenced row carries the daemon recovery text: {fenced}"
    );

    let totals = &payload["totals"];
    assert!(totals["running"].as_i64().unwrap_or(0) >= 1);
    assert!(totals["fenced"].as_i64().unwrap_or(0) >= 1);
    assert_eq!(totals["inboxes"], 1);
}

// ---------- CAD-41: issue history from git plumbing ----------

/// A tracker whose CAD-1 has a varied, deterministic write history:
/// created → two CLI sets → link → comment → attach → one HTTP PATCH
/// (the ` (operator (ui))` actor) last, so `diff`'s default lands on a
/// field change. Shas per step are captured for blame/diff asserts.
struct HistFx {
    pm: TempDir,
    state: TempDir,
    port: u16,
    _board: BoardStop,
    created_sha: String,
    set2_sha: String,
    patch_sha: String,
    link_sha: String,
}

fn history_fixture() -> HistFx {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    assert!(cli(pm.path(), state.path(), &["issue", "init"]).0);
    assert!(
        cli(
            pm.path(),
            state.path(),
            &["issue", "project", "add", "cadence", "--prefix", "CAD"]
        )
        .0
    );
    assert!(
        cli(
            pm.path(),
            state.path(),
            &["issue", "new", "alpha", "--project", "cadence"]
        )
        .0
    );
    assert!(
        cli(
            pm.path(),
            state.path(),
            &["issue", "new", "beta", "--project", "cadence"]
        )
        .0
    );
    let (port, board) = start_ui(pm.path().to_path_buf(), state.path().to_path_buf());
    let host = format!("127.0.0.1:{port}");
    assert!(
        cli(
            pm.path(),
            state.path(),
            &["issue", "set", "CAD-1", "status=doing"]
        )
        .0
    );
    assert!(
        cli(
            pm.path(),
            state.path(),
            &["issue", "set", "CAD-1", "priority=P1", "owner=alice"]
        )
        .0
    );
    assert!(
        cli(
            pm.path(),
            state.path(),
            &["issue", "link", "CAD-1", "relates", "CAD-2"]
        )
        .0
    );
    assert!(
        cli(
            pm.path(),
            state.path(),
            &["issue", "comment", "CAD-1", "-m", "hello", "--author", "fable-cc"]
        )
        .0
    );
    // The attach source lives outside the PM dir so `git add -A` does
    // not drag it into the tracker commit.
    let src = state.path().join("note.txt");
    std::fs::write(&src, b"artifact body").unwrap();
    assert!(
        cli(
            pm.path(),
            state.path(),
            &["issue", "attach", "CAD-1", src.to_str().unwrap()]
        )
        .0
    );
    // One set through the HTTP write path — the commit subject ends in
    // ` (operator (ui))`. It needs the operator's session (CAD-313), so
    // a daemon runs just for the sign-in and the write.
    let d = UiDaemon::start_on(state.path().to_path_buf());
    let op = sign_in(state.path(), port);
    let (_, detail) = http(port, "GET", "/api/issues/CAD-1", &host);
    let rev = serde_json::from_str::<Value>(&detail).unwrap()["rev"]
        .as_str()
        .unwrap()
        .to_string();
    let (code, _, body) = op_write_json(
        &op,
        port,
        "PATCH",
        "/api/issues/CAD-1",
        &host,
        &format!(r#"{{"status":"review","if_rev":"{rev}"}}"#),
    );
    assert_eq!(code, 200, "{body}");
    drop(d);
    let log = git(pm.path(), &["log", "--format=%H %s", "--", "cadence/CAD-1"]).1;
    let sha_of = |needle: &str| -> String {
        log.lines()
            .find(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("no commit containing '{needle}': {log}"))
            .split_whitespace()
            .next()
            .unwrap()
            .to_string()
    };
    HistFx {
        created_sha: sha_of("created"),
        set2_sha: sha_of("priority=P1"),
        patch_sha: sha_of("set status=review"),
        link_sha: sha_of("link relates"),
        pm,
        state,
        port,
        _board: board,
    }
}

#[test]
fn issue_log_kinds_actors_limit() {
    let fx = history_fixture();
    let (ok, out) = cli(fx.pm.path(), fx.state.path(), &["issue", "log", "CAD-1"]);
    assert!(ok, "{out}");
    let hist = out["history"].as_array().unwrap();
    let kinds: Vec<&str> = hist.iter().map(|e| e["kind"].as_str().unwrap()).collect();
    // Newest-first: the UI patch lands on top.
    assert_eq!(
        kinds,
        ["set", "attach", "comment", "link", "set", "set", "created"]
    );
    // `by`: the `Actor:` trailer — `operator (ui)` for the HTTP patch,
    // `operator` for CLI writes (no alias in the test env).
    assert_eq!(hist[0]["by"], "operator (ui)");
    assert_eq!(hist[1]["by"], "operator");
    assert_eq!(hist[2]["by"], "fable-cc");
    // Summaries drop the id prefix and the actor suffix.
    assert_eq!(hist[0]["summary"], "set status=review");
    assert_eq!(hist[1]["summary"], "attach note.txt");
    assert_eq!(hist[2]["summary"], "comment by fable-cc");
    // `fields` only on set entries; bare patch words map to null.
    assert_eq!(hist[0]["fields"]["status"], "review");
    assert_eq!(hist[4]["fields"]["owner"], "alice");
    assert!(hist[1].get("fields").is_none());
    // `sha` is short, `at` is RFC 3339 UTC.
    let sha = hist[0]["sha"].as_str().unwrap();
    assert!(sha.len() >= 7 && fx.patch_sha.starts_with(sha));
    assert!(hist[0]["at"].as_str().unwrap().ends_with('Z'));
    // --limit trims.
    let (ok, out) = cli(
        fx.pm.path(),
        fx.state.path(),
        &["issue", "log", "CAD-1", "--limit", "2"],
    );
    assert!(ok);
    assert_eq!(out["history"].as_array().unwrap().len(), 2);
}

#[test]
fn issue_diff_fields_and_files() {
    let fx = history_fixture();
    // Default: the issue's newest change (the UI patch) vs its parent —
    // the last field change shows as from/to.
    let (ok, out) = cli(fx.pm.path(), fx.state.path(), &["issue", "diff", "CAD-1"]);
    assert!(ok, "{out}");
    assert_eq!(out["to"]["sha"], fx.patch_sha);
    let fields = out["fields"].as_array().unwrap();
    assert_eq!(fields.len(), 1);
    assert_eq!(fields[0]["field"], "status");
    assert_eq!(fields[0]["from"], "doing");
    assert_eq!(fields[0]["to"], "review");

    // <first-sha> --to HEAD: every field changed since creation, plus
    // the added comment and artifact files.
    let (ok, out) = cli(
        fx.pm.path(),
        fx.state.path(),
        &["issue", "diff", "CAD-1", &fx.created_sha, "--to", "HEAD"],
    );
    assert!(ok, "{out}");
    let fields = out["fields"].as_array().unwrap();
    let get = |name: &str| {
        fields
            .iter()
            .find(|f| f["field"] == name)
            .unwrap_or_else(|| panic!("no {name} field in {fields:?}"))
    };
    assert_eq!(get("status")["from"], "backlog");
    assert_eq!(get("status")["to"], "review");
    assert_eq!(get("priority")["to"], "P1");
    assert!(get("owner")["from"].is_null());
    assert_eq!(get("owner")["to"], "alice");
    assert_eq!(get("relates")["to"], json!(["CAD-2"]));
    assert_eq!(
        out["comments"]["added"].as_array().unwrap().len(),
        1,
        "one comment file added"
    );
    assert!(out["comments"]["added"][0]
        .as_str()
        .unwrap()
        .ends_with("-fable-cc.md"));
    assert_eq!(out["artifacts"]["added"], json!(["note.txt"]));

    // Unknown and unrelated revs are refused with a clear message.
    let (ok, err) = cli(
        fx.pm.path(),
        fx.state.path(),
        &["issue", "diff", "CAD-1", "notasha"],
    );
    assert!(!ok);
    assert!(
        err["error"].as_str().unwrap().contains("Unknown revision"),
        "{err}"
    );
    // A commit from a foreign repo resolves nowhere in this history.
    let foreign = TempDir::new().unwrap();
    assert!(git(foreign.path(), &["init", "-q"]).0);
    let f = foreign.path().join("f");
    std::fs::write(&f, b"x").unwrap();
    assert!(git(foreign.path(), &["add", "f"]).0);
    assert!(
        git(
            foreign.path(),
            &[
                "-c",
                "user.name=x",
                "-c",
                "user.email=x@x",
                "commit",
                "-q",
                "-m",
                "x"
            ]
        )
        .0
    );
    let foreign_sha = head(foreign.path());
    let (ok, err) = cli(
        fx.pm.path(),
        fx.state.path(),
        &["issue", "diff", "CAD-1", &foreign_sha],
    );
    assert!(!ok);
    assert!(
        err["error"].as_str().unwrap().contains("Unknown revision"),
        "{err}"
    );
    // A commit that resolves but is off HEAD's history — an orphan
    // side-branch — is "unrelated" and refused by name.
    let main_branch = branch(fx.pm.path());
    assert!(git(fx.pm.path(), &["checkout", "-q", "--orphan", "side"]).0);
    let side = fx.pm.path().join("side.txt");
    std::fs::write(&side, b"x").unwrap();
    assert!(git(fx.pm.path(), &["add", "side.txt"]).0);
    assert!(
        git(
            fx.pm.path(),
            &[
                "-c",
                "user.name=x",
                "-c",
                "user.email=x@x",
                "commit",
                "-q",
                "-m",
                "side"
            ]
        )
        .0
    );
    let side_sha = head(fx.pm.path());
    assert!(git(fx.pm.path(), &["checkout", "-q", &main_branch]).0);
    let (ok, err) = cli(
        fx.pm.path(),
        fx.state.path(),
        &["issue", "diff", "CAD-1", &side_sha],
    );
    assert!(!ok);
    assert!(
        err["error"]
            .as_str()
            .unwrap()
            .contains("not part of this tracker's history"),
        "{err}"
    );
}

#[test]
fn issue_blame_attributes_fields() {
    let fx = history_fixture();
    let (ok, out) = cli(fx.pm.path(), fx.state.path(), &["issue", "blame", "CAD-1"]);
    assert!(ok, "{out}");
    let fields = out["fields"].as_array().unwrap();
    let get = |name: &str| {
        fields
            .iter()
            .find(|f| f["field"] == name)
            .unwrap_or_else(|| panic!("no {name} in {fields:?}"))
    };
    // status last changed by the UI patch — actor, not author.
    assert_eq!(get("status")["value"], "review");
    assert!(fx
        .patch_sha
        .starts_with(get("status")["sha"].as_str().unwrap()));
    assert_eq!(get("status")["by"], "operator (ui)");
    // priority and owner both came from the second set.
    assert_eq!(get("priority")["value"], "P1");
    assert!(fx
        .set2_sha
        .starts_with(get("priority")["sha"].as_str().unwrap()));
    assert_eq!(get("owner")["value"], "alice");
    assert!(fx
        .set2_sha
        .starts_with(get("owner")["sha"].as_str().unwrap()));
    // relates from the link commit; title/id/created from creation.
    assert!(fx
        .link_sha
        .starts_with(get("relates")["sha"].as_str().unwrap()));
    assert!(fx
        .created_sha
        .starts_with(get("title")["sha"].as_str().unwrap()));
}

#[test]
fn issue_ls_at_historical_board() {
    let fx = history_fixture();
    // No `cadence-issue-at-*` temp dirs before — compare the set after.
    let tmp_entries = || -> Vec<String> {
        std::fs::read_dir(std::env::temp_dir())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.starts_with("cadence-issue-at-"))
            .collect()
    };
    let before = tmp_entries();
    let (ok, out) = cli(
        fx.pm.path(),
        fx.state.path(),
        &["issue", "ls", "--at", &fx.created_sha, "--json"],
    );
    assert!(ok, "{out}");
    let issues = out["issues"].as_array().unwrap();
    let cad1 = issues
        .iter()
        .find(|i| i["id"] == "CAD-1")
        .expect("CAD-1 card");
    assert_eq!(cad1["status"], "backlog", "status at the creation sha");
    assert_eq!(cad1["status_source"], "file");
    assert_eq!(out["at"]["sha"], fx.created_sha);
    assert!(out["at"]["time"].as_str().unwrap().ends_with('Z'));
    // The temp export is gone afterwards.
    assert_eq!(tmp_entries(), before);
    // --project still filters on the historical tree.
    let (ok, out) = cli(
        fx.pm.path(),
        fx.state.path(),
        &[
            "issue",
            "ls",
            "--at",
            &fx.created_sha,
            "--project",
            "cadence",
            "--json",
        ],
    );
    assert!(ok);
    assert!(out["issues"]
        .as_array()
        .unwrap()
        .iter()
        .all(|i| i["project"] == "cadence"));
    // Current ls shows the current status — the historical read moved
    // nothing.
    let (ok, out) = cli(fx.pm.path(), fx.state.path(), &["issue", "ls", "--json"]);
    assert!(ok);
    let cur = out["issues"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == "CAD-1")
        .unwrap()
        .clone();
    assert_eq!(cur["status"], "review");
}

#[test]
fn issue_log_other_for_hand_and_revert() {
    let fx = history_fixture();
    // A hand-made commit: body append on CAD-1's issue.md under a
    // foreign author — `other`, raw subject, author name as `by`.
    let md = fx.pm.path().join("cadence/CAD-1/issue.md");
    let mut text = std::fs::read_to_string(&md).unwrap();
    text.push_str("\nhand edit\n");
    std::fs::write(&md, text).unwrap();
    assert!(git(fx.pm.path(), &["add", "-A"]).0);
    assert!(
        git(
            fx.pm.path(),
            &[
                "-c",
                "user.name=hand",
                "-c",
                "user.email=hand@h",
                "commit",
                "-q",
                "-m",
                "wip manual edit"
            ]
        )
        .0
    );
    // A revert of the UI patch — subject `Revert "…"` is `other` too.
    assert!(
        git(
            fx.pm.path(),
            &[
                "-c",
                "user.name=hand",
                "-c",
                "user.email=hand@h",
                "revert",
                "--no-edit",
                &fx.patch_sha
            ]
        )
        .0
    );
    let (ok, out) = cli(fx.pm.path(), fx.state.path(), &["issue", "log", "CAD-1"]);
    assert!(ok, "{out}");
    let hist = out["history"].as_array().unwrap();
    assert_eq!(hist[0]["kind"], "other");
    assert!(hist[0]["summary"].as_str().unwrap().starts_with("Revert"));
    assert_eq!(hist[0]["by"], "hand", "author name, never paren-parsed");
    assert_eq!(hist[1]["kind"], "other");
    assert_eq!(hist[1]["summary"], "wip manual edit");
    // The cadence entries still parse underneath.
    assert_eq!(hist[2]["kind"], "set");
    assert!(hist.iter().any(|e| e["kind"] == "created"));
}

#[test]
fn issue_history_api_matches_cli_and_guards() {
    let fx = history_fixture();
    let host = format!("127.0.0.1:{}", fx.port);
    let (code, body) = http(fx.port, "GET", "/api/issues/CAD-1/history?limit=50", &host);
    assert_eq!(code, 200);
    let api: Value = serde_json::from_str(&body).unwrap();
    let (ok, cli_out) = cli(fx.pm.path(), fx.state.path(), &["issue", "log", "CAD-1"]);
    assert!(ok);
    assert_eq!(api["history"], cli_out["history"]);
    // `?limit` honoured; a bad one is a 400.
    let (code, body) = http(fx.port, "GET", "/api/issues/CAD-1/history?limit=2", &host);
    assert_eq!(code, 200);
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap()["history"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let (code, _) = http(fx.port, "GET", "/api/issues/CAD-1/history?limit=x", &host);
    assert_eq!(code, 400);
    // The route is read-only — POST has no write route to reach; as an
    // unlisted write it is operator-only and fails closed (CAD-313).
    let (code, _) = http(fx.port, "POST", "/api/issues/CAD-1/history", &host);
    assert_eq!(code, 403);
}

#[test]
fn issue_history_refuses_non_git() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    assert!(cli(pm.path(), state.path(), &["issue", "init"]).0);
    assert!(
        cli(
            pm.path(),
            state.path(),
            &["issue", "project", "add", "cadence", "--prefix", "CAD"]
        )
        .0
    );
    assert!(
        cli(
            pm.path(),
            state.path(),
            &["issue", "new", "alpha", "--project", "cadence"]
        )
        .0
    );
    // Detach the repo — pm.yaml stays, `.git` moves aside.
    std::fs::rename(pm.path().join(".git"), pm.path().join("git-aside")).unwrap();
    for args in [
        vec!["issue", "log", "CAD-1"],
        vec!["issue", "diff", "CAD-1"],
        vec!["issue", "blame", "CAD-1"],
        vec!["issue", "ls", "--at", "HEAD"],
    ] {
        let (ok, err) = cli(pm.path(), state.path(), &args);
        assert!(!ok, "{args:?} unexpectedly ok");
        assert!(
            err["error"]
                .as_str()
                .unwrap()
                .contains("not a git repository"),
            "{args:?}: {err}"
        );
    }
}

// ---------- CAD-42: Issue:/Actor: trailers, truthful `by`, code commits ----------

/// `git interpret-trailers --parse` on one commit's message — the
/// acceptance criterion checks trailers through git's own parser.
fn trailers_of(dir: &Path, sha: &str) -> String {
    let (_, msg) = git(dir, &["show", "-s", "--format=%B", sha]);
    let mut child = Command::new("git")
        .args(["interpret-trailers", "--parse"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(msg.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// Find the sha of the issue-folder commit whose subject contains
/// `needle` — same lookup shape as `history_fixture`'s `sha_of`.
fn sha_of(pm: &Path, rel: &str, needle: &str) -> String {
    let (_, log) = git(pm, &["log", "--format=%H %s", "--", rel]);
    log.lines()
        .find(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("no commit containing '{needle}': {log}"))
        .split_whitespace()
        .next()
        .unwrap()
        .to_string()
}

#[test]
fn issue_commits_carry_trailers() {
    let fx = history_fixture();
    let rel = "cadence/CAD-1";
    // Every write kind lands `Issue:` + `Actor:` trailers that
    // `git interpret-trailers --parse` reads back.
    for (needle, issue, actor) in [
        ("created", "CAD-1", "operator"),
        ("set status=doing", "CAD-1", "operator"),
        ("set priority=P1", "CAD-1", "operator"),
        ("link relates", "CAD-1", "operator"),
        ("comment by fable-cc", "CAD-1", "fable-cc"),
        ("attach note.txt", "CAD-1", "operator"),
        ("set status=review", "CAD-1", "operator (ui)"),
    ] {
        let sha = sha_of(fx.pm.path(), rel, needle);
        let trailers = trailers_of(fx.pm.path(), &sha);
        assert!(
            trailers.contains(&format!("Issue: {issue}")),
            "{needle}: {trailers}"
        );
        assert!(
            trailers.contains(&format!("Actor: {actor}")),
            "{needle}: {trailers}"
        );
    }
    // The link commit carries both ends, own id first.
    let sha = sha_of(fx.pm.path(), rel, "link relates");
    let trailers = trailers_of(fx.pm.path(), &sha);
    let ids: Vec<&str> = trailers
        .lines()
        .filter_map(|l| l.strip_prefix("Issue: "))
        .collect();
    assert_eq!(ids, ["CAD-1", "CAD-2"], "{trailers}");
    // CADENCE_ALIAS resolves before the `operator` fallback.
    assert!(
        cli_env(
            fx.pm.path(),
            fx.state.path(),
            &["issue", "set", "CAD-1", "priority=P2"],
            &[("CADENCE_ALIAS", "agent-x")],
        )
        .0
    );
    let sha = sha_of(fx.pm.path(), rel, "priority=P2");
    assert!(trailers_of(fx.pm.path(), &sha).contains("Actor: agent-x"));
    // Non-issue commits carry Actor only (project add), init too.
    let proj = sha_of(fx.pm.path(), "cadence/project.yaml", "project cadence");
    let t = trailers_of(fx.pm.path(), &proj);
    assert!(
        t.contains("Actor: operator") && !t.contains("Issue:"),
        "{t}"
    );
    let (_, first) = git(fx.pm.path(), &["log", "--format=%H", "--reverse"]);
    let init_sha = first.lines().next().unwrap().to_string();
    assert!(
        trailers_of(fx.pm.path(), &init_sha).contains("Actor:"),
        "init commit carries Actor"
    );
}

#[test]
fn issue_log_by_from_trailers() {
    let fx = history_fixture();
    let (ok, out) = cli(fx.pm.path(), fx.state.path(), &["issue", "log", "CAD-1"]);
    assert!(ok, "{out}");
    let hist = out["history"].as_array().unwrap();
    let by_for = |summary: &str| {
        hist.iter()
            .find(|e| e["summary"].as_str().unwrap_or("").contains(summary))
            .unwrap_or_else(|| panic!("no entry '{summary}' in {hist:?}"))["by"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert_eq!(by_for("set status=review"), "operator (ui)");
    assert_eq!(by_for("comment by fable-cc"), "fable-cc");
    assert_eq!(by_for("attach note.txt"), "operator");
    assert_eq!(by_for("link relates"), "operator");
    assert_eq!(by_for("created"), "operator");
    // A trailer-less `comment by` commit still resolves its author
    // from the subject; a plain hand commit falls to the git author.
    // Each touches the issue folder so the path-filtered log sees it.
    let md = fx.pm.path().join("cadence/CAD-1/issue.md");
    for (name, subject) in [
        ("ghost", "CAD-1: comment by ghost"),
        ("hand", "wip manual edit"),
    ] {
        let text = std::fs::read_to_string(&md).unwrap();
        std::fs::write(&md, format!("{text}\n{name}\n")).unwrap();
        assert!(git(fx.pm.path(), &["add", "-A"]).0);
        assert!(
            git(
                fx.pm.path(),
                &[
                    "-c",
                    &format!("user.name={name}"),
                    "-c",
                    "user.email=h@h",
                    "commit",
                    "-q",
                    "-m",
                    subject
                ]
            )
            .0
        );
    }
    let (ok, out) = cli(fx.pm.path(), fx.state.path(), &["issue", "log", "CAD-1"]);
    assert!(ok);
    let hist = out["history"].as_array().unwrap();
    assert_eq!(hist[0]["kind"], "other");
    assert_eq!(hist[0]["by"], "hand");
    assert_eq!(hist[1]["kind"], "comment");
    assert_eq!(hist[1]["by"], "ghost", "subject fallback without trailer");
}

/// A tracker whose `x` project declares two repos: `repo` (a real
/// git dir the test fills) and `/definitely/missing` (skip target).
fn commits_fixture() -> (TempDir, TempDir, TempDir, u16, BoardStop) {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    assert!(git(repo.path(), &["init", "-q"]).0);
    assert!(cli(pm.path(), state.path(), &["issue", "init"]).0);
    assert!(
        cli(
            pm.path(),
            state.path(),
            &[
                "issue",
                "project",
                "add",
                "x",
                "--prefix",
                "X",
                "--repo",
                repo.path().to_str().unwrap(),
                "--repo",
                "/definitely/missing",
            ]
        )
        .0
    );
    assert!(
        cli(
            pm.path(),
            state.path(),
            &["issue", "new", "feat", "--project", "x"]
        )
        .0
    );
    let commit = |subject: &str, trailer: Option<&str>| {
        let f = repo
            .path()
            .join(format!("f{}", repo.path().read_dir().unwrap().count()));
        std::fs::write(&f, b"x").unwrap();
        assert!(git(repo.path(), &["add", "."]).0);
        let msg = match trailer {
            Some(t) => format!("{subject}\n\n{t}"),
            None => subject.to_string(),
        };
        assert!(
            git(
                repo.path(),
                &[
                    "-c",
                    "user.name=dev",
                    "-c",
                    "user.email=d@d",
                    "commit",
                    "-q",
                    "-m",
                    &msg
                ]
            )
            .0
        );
    };
    commit("feat: wire it", Some("Issue: X-1"));
    commit("fix (X-1) edge case", None);
    commit("wip X-12 unrelated", None);
    commit("unrelated refactor", None);
    let (port, board) = start_ui(pm.path().to_path_buf(), state.path().to_path_buf());
    (pm, state, repo, port, board)
}

#[test]
fn issue_detail_lists_code_commits() {
    let (pm, state, repo, port, _board) = commits_fixture();
    let (ok, out) = cli(pm.path(), state.path(), &["issue", "show", "X-1", "--json"]);
    assert!(ok, "{out}");
    let commits = out["commits"].as_array().unwrap();
    let subjects: Vec<&str> = commits
        .iter()
        .map(|c| c["subject"].as_str().unwrap())
        .collect();
    assert_eq!(
        subjects,
        ["fix (X-1) edge case", "feat: wire it"],
        "trailer + whole-word matches, newest first: {subjects:?}"
    );
    for c in commits {
        assert_eq!(c["repo"], repo.path().to_str().unwrap());
        assert_eq!(c["author"], "dev");
        assert!(c["at"].as_str().unwrap().ends_with('Z'));
        assert_eq!(c["sha"].as_str().unwrap().len(), 7);
    }
    let skipped = out["commits_skipped"].as_array().unwrap();
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0]["repo"], "/definitely/missing");
    // The API detail carries the identical payload.
    let host = format!("127.0.0.1:{port}");
    let (code, body) = http(port, "GET", "/api/issues/X-1", &host);
    assert_eq!(code, 200);
    let api: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(api["commits"], out["commits"]);
    assert_eq!(api["commits_skipped"], out["commits_skipped"]);
}

/// CAD-60: `--all` walks stale remote-tracking refs, so a squash-merged
/// branch would list its work twice. The default-branch twin wins; a
/// branch-only commit stays, tagged `on_default: false`, after the
/// default-branch commits even when it is the newest.
#[test]
fn issue_detail_dedupes_stale_branch_commits() {
    let (pm, state, repo, _port, _board) = commits_fixture();
    let commit = |subject: &str, date: &str| {
        assert!(
            git(
                repo.path(),
                &[
                    "-c",
                    "user.name=dev",
                    "-c",
                    "user.email=d@d",
                    "commit",
                    "-q",
                    "--allow-empty",
                    "--date",
                    date,
                    "-m",
                    subject
                ]
            )
            .0
        );
    };
    assert!(git(repo.path(), &["checkout", "-q", "-b", "feat"]).0);
    commit("land it (X-1)", "2030-01-01T00:00:00Z");
    commit("wip (X-1) branch only", "2030-01-03T00:00:00Z");
    assert!(git(repo.path(), &["checkout", "-q", "-"]).0);
    commit("land it (X-1) (#7)", "2030-01-02T00:00:00Z");
    // The merged branch is gone locally; only the stale remote ref holds it.
    assert!(
        git(
            repo.path(),
            &["update-ref", "refs/remotes/origin/feat", "feat"]
        )
        .0
    );
    assert!(git(repo.path(), &["branch", "-q", "-D", "feat"]).0);

    let (ok, out) = cli(pm.path(), state.path(), &["issue", "show", "X-1", "--json"]);
    assert!(ok, "{out}");
    let listed: Vec<(&str, bool)> = out["commits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["subject"].as_str().unwrap(),
                c["on_default"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        listed,
        [
            ("land it (X-1) (#7)", true),
            ("fix (X-1) edge case", true),
            ("feat: wire it", true),
            ("wip (X-1) branch only", false),
        ],
        "twin listed once from the default branch; default first, then newest"
    );
}

// ---------- CAD-81: tags, epics, filters, bulk edits ----------

/// Tracker with project `x` (declares tags `ui api infra`) and project
/// `y` (declares none), two epics and a loose issue:
///
/// ```text
/// X-1 epic A ─ X-3 done    P1 ann  [ui]
///            ├ X-4 doing      bob  [api ui]
///            └ X-5 backlog    ann  [api]      blocked_by X-4
/// X-2 epic B ─ X-6 dropped         [ui]
///            └ X-7 ready      cy   []         component core
/// X-8 review                       [infra]
/// ```
fn tags_fixture() -> (TempDir, TempDir) {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let run = |args: &[&str]| {
        let (ok, out) = cli(pm.path(), state.path(), args);
        assert!(ok, "{args:?}: {out}");
    };
    run(&["issue", "init"]);
    run(&[
        "issue", "project", "add", "x", "--prefix", "X", "--tag", "ui", "--tag", "api", "--tag",
        "infra",
    ]);
    run(&["issue", "project", "add", "y", "--prefix", "Y"]);
    run(&["issue", "new", "epic A", "--project", "x"]);
    run(&["issue", "new", "epic B", "--project", "x"]);
    run(&[
        "issue",
        "new",
        "a1",
        "--project",
        "x",
        "--epic",
        "X-1",
        "--tag",
        "ui",
        "--owner",
        "ann",
        "--priority",
        "P1",
    ]);
    // Tags arrive unsorted and repeated; they are stored sorted, once.
    run(&[
        "issue",
        "new",
        "a2",
        "--project",
        "x",
        "--epic",
        "X-1",
        "--tag",
        "ui",
        "--tag",
        "api",
        "--tag",
        "ui",
        "--owner",
        "bob",
    ]);
    run(&[
        "issue",
        "new",
        "a3",
        "--project",
        "x",
        "--epic",
        "X-1",
        "--tag",
        "api",
        "--owner",
        "ann",
        "--blocked-by",
        "X-4",
    ]);
    run(&[
        "issue",
        "new",
        "b1",
        "--project",
        "x",
        "--parent",
        "X-2",
        "--tag",
        "ui",
    ]);
    run(&[
        "issue",
        "new",
        "b2",
        "--project",
        "x",
        "--epic",
        "X-2",
        "--owner",
        "cy",
        "--component",
        "core",
    ]);
    run(&["issue", "new", "loose", "--project", "x", "--tag", "infra"]);
    for (id, status) in [
        ("X-3", "done"),
        ("X-4", "doing"),
        ("X-6", "dropped"),
        ("X-7", "ready"),
        ("X-8", "review"),
    ] {
        run(&["issue", "set", id, &format!("status={status}")]);
    }
    (pm, state)
}

fn tags_of(pm: &Path, state: &Path, id: &str) -> Vec<String> {
    let (ok, out) = cli(pm, state, &["issue", "show", id, "--json"]);
    assert!(ok, "{out}");
    out["tags"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t.as_str().unwrap().to_string())
        .collect()
}

fn head_message(pm: &Path) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(pm)
        .args(["log", "-1", "--format=%B"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn tree_is_clean(pm: &Path) -> bool {
    let out = Command::new("git")
        .arg("-C")
        .arg(pm)
        .args(["status", "--porcelain"])
        .output()
        .unwrap();
    out.stdout.is_empty()
}

#[test]
fn issue_tags_round_trip_and_declared_list() {
    let (pm, state) = tags_fixture();
    let (pm, state) = (pm.path(), state.path());
    assert_eq!(tags_of(pm, state, "X-4"), ["api", "ui"]);
    assert!(cli(pm, state, &["issue", "tag", "X-4", "add", "infra"]).0);
    assert_eq!(tags_of(pm, state, "X-4"), ["api", "infra", "ui"]);
    assert!(cli(pm, state, &["issue", "tag", "X-4", "rm", "api", "infra"]).0);
    assert_eq!(tags_of(pm, state, "X-4"), ["ui"]);
    assert!(cli(pm, state, &["issue", "set", "X-4", "tags=infra,api"]).0);
    assert_eq!(tags_of(pm, state, "X-4"), ["api", "infra"]);
    // Empty clears; an issue with no tags stores no `tags:` key.
    assert!(cli(pm, state, &["issue", "set", "X-4", "tags="]).0);
    assert!(tags_of(pm, state, "X-4").is_empty());
    let file = std::fs::read_to_string(pm.join("x/X-4/issue.md")).unwrap();
    assert!(!file.contains("tags"), "{file}");
    assert!(cli(pm, state, &["issue", "set", "X-4", "tags=ui,api"]).0);

    // The declared list and the grammar reject through every CLI write,
    // and a rejection commits nothing.
    let before = commits(pm);
    for args in [
        &["issue", "new", "n", "--project", "x", "--tag", "nope"][..],
        &["issue", "tag", "X-4", "add", "nope"],
        &["issue", "set", "X-4", "tags=ui,nope"],
    ] {
        let (ok, out) = cli(pm, state, args);
        assert!(!ok, "{args:?}");
        let msg = out.to_string();
        assert!(
            msg.contains("Unknown tag 'nope'") && msg.contains("api, infra, ui"),
            "{msg}"
        );
    }
    let (ok, out) = cli(pm, state, &["issue", "tag", "X-4", "add", "Not-A-Tag"]);
    assert!(!ok && out.to_string().contains("Invalid tag"), "{out}");
    let (ok, out) = cli(pm, state, &["issue", "tag", "X-4", "add", "ui"]);
    assert!(!ok && out.to_string().contains("changes nothing"), "{out}");
    assert_eq!(commits(pm), before);
    assert!(tree_is_clean(pm));
    assert!(
        !pm.join("x/X-9").exists(),
        "a rejected new leaves no folder"
    );
    // A project that declares no tags accepts any well-formed one.
    let (ok, out) = cli(
        pm,
        state,
        &[
            "issue",
            "new",
            "free",
            "--project",
            "y",
            "--tag",
            "whatever-2",
        ],
    );
    assert!(ok, "{out}");
    assert_eq!(tags_of(pm, state, "Y-1"), ["whatever-2"]);

    // The HTTP write path: same validation, same if_rev rule.
    let (port, _board) = start_ui(pm.to_path_buf(), state.to_path_buf());
    let host = format!("127.0.0.1:{port}");
    // CAD-313: a board write is the operator's only with a session —
    // this test process signs in and writes as `operator (ui)`.
    let _d = UiDaemon::start_on(state.to_path_buf());
    let op = sign_in(state, port);
    let write_json = |port: u16, method: &str, path: &str, host: &str, body: &str| {
        op_write_json(&op, port, method, path, host, body)
    };
    let detail = |id: &str| -> Value {
        let (code, body) = http(port, "GET", &format!("/api/issues/{id}"), &host);
        assert_eq!(code, 200, "{body}");
        serde_json::from_str(&body).unwrap()
    };
    let rev = detail("X-4")["rev"].as_str().unwrap().to_string();
    let before = commits(pm);
    let (code, _, body) = write_json(
        port,
        "PATCH",
        "/api/issues/X-4",
        &host,
        &json!({"tags": ["ui", "nope"], "if_rev": rev}).to_string(),
    );
    assert_eq!(code, 400, "{body}");
    assert!(body.contains("Unknown tag 'nope'"), "{body}");
    assert_eq!(commits(pm), before, "a rejected patch commits nothing");
    assert_eq!(detail("X-4")["rev"], rev.as_str());
    let (code, _, body) = write_json(
        port,
        "PATCH",
        "/api/issues/X-4",
        &host,
        &json!({"tags": ["infra", "api", "infra"], "if_rev": rev}).to_string(),
    );
    assert_eq!(code, 200, "{body}");
    assert_eq!(commits(pm), before + 1);
    assert_eq!(detail("X-4")["tags"], json!(["api", "infra"]));
    assert!(head_message(pm).contains("X-4: set tags=api,infra (operator (ui))"));
    // The rev moved, so the old one is now a conflict.
    let (code, _, body) = write_json(
        port,
        "PATCH",
        "/api/issues/X-4",
        &host,
        &json!({"tags": [], "if_rev": rev}).to_string(),
    );
    assert_eq!(code, 409, "{body}");
    assert_eq!(detail("X-4")["tags"], json!(["api", "infra"]));
    let (code, _, body) = write_json(
        port,
        "POST",
        "/api/issues",
        &host,
        &json!({"project": "x", "title": "via api", "tags": ["nope"]}).to_string(),
    );
    assert_eq!(code, 400, "{body}");
    let (code, _, body) = write_json(
        port,
        "POST",
        "/api/issues",
        &host,
        &json!({"project": "x", "title": "via api", "tags": ["ui"]}).to_string(),
    );
    assert_eq!(code, 201, "{body}");
    let created: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(created["card"]["tags"], json!(["ui"]));
    assert_eq!(
        tags_of(pm, state, created["card"]["id"].as_str().unwrap()),
        ["ui"]
    );
    // The declared list reaches the board through /api/projects.
    let (_, body) = http(port, "GET", "/api/projects", &host);
    let projects: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(
        projects["projects"][0]["tags"],
        json!(["api", "infra", "ui"])
    );

    // Lint: clean now; a hand edit trips grammar, duplicate and the
    // declared list.
    let (ok, out) = cli(pm, state, &["issue", "lint"]);
    assert!(ok, "{out}");
    let path = pm.join("x/X-8/issue.md");
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("tags:\n- infra\n"), "{text}");
    std::fs::write(
        &path,
        text.replace(
            "tags:\n- infra\n",
            "tags:\n- infra\n- infra\n- Bad\n- nope\n",
        ),
    )
    .unwrap();
    let (ok, out) = cli(pm, state, &["issue", "lint"]);
    assert!(!ok);
    let errors = out["errors"].to_string();
    for want in [
        "X-8: duplicated tag 'infra'",
        "X-8: bad tag grammar 'Bad'",
        "X-8: unknown tag 'nope'",
    ] {
        assert!(errors.contains(want), "{want} missing from {errors}");
    }
}

#[test]
fn issue_bulk_edits_are_one_atomic_commit() {
    let (pm, state) = tags_fixture();
    let (pm, state) = (pm.path(), state.path());
    let status_of = |id: &str| {
        cli(pm, state, &["issue", "show", id, "--json"]).1["frontmatter"]["status"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let before = commits(pm);
    let (ok, out) = cli(
        pm,
        state,
        &[
            "issue",
            "set",
            "X-5",
            "X-7",
            "X-5",
            "status=ready",
            "priority=P1",
        ],
    );
    assert!(ok, "{out}");
    assert_eq!(
        out["ids"],
        json!(["X-5", "X-7"]),
        "a repeated id counts once"
    );
    assert_eq!(commits(pm), before + 1, "one commit for the whole batch");
    let msg = head_message(pm);
    assert!(
        msg.starts_with("X-5, X-7: set status=ready priority=P1\n"),
        "{msg}"
    );
    for trailer in ["Issue: X-5\n", "Issue: X-7\n", "Actor: operator\n"] {
        assert!(msg.contains(trailer), "{trailer:?} missing from {msg}");
    }
    assert_eq!(
        (status_of("X-5"), status_of("X-7")),
        ("ready".into(), "ready".into())
    );

    // One bad id, one bad value, one undeclared tag: nothing is written.
    let before = commits(pm);
    for args in [
        &["issue", "set", "X-5", "X-99", "status=doing"][..],
        &["issue", "set", "X-5", "X-7", "status=nonsense"],
        &["issue", "set", "X-5", "X-7", "status=doing", "tags=nope"],
        &["issue", "tag", "X-5", "X-99", "add", "ui"],
        &["issue", "tag", "X-5", "X-7", "add", "nope"],
        &["issue", "set", "X-5", "status=doing", "X-7"],
    ] {
        let (ok, out) = cli(pm, state, args);
        assert!(!ok, "{args:?}: {out}");
    }
    assert_eq!(commits(pm), before);
    assert!(tree_is_clean(pm));
    assert_eq!(status_of("X-5"), "ready");
    assert_eq!(tags_of(pm, state, "X-5"), ["api"]);

    // Bulk tag: one commit; an issue the edit does not change stays out
    // of it.
    let (ok, out) = cli(
        pm,
        state,
        &["issue", "tag", "X-3", "X-5", "X-7", "add", "ui"],
    );
    assert!(ok, "{out}");
    assert_eq!(commits(pm), before + 1);
    assert_eq!(out["ids"], json!(["X-5", "X-7"]), "X-3 already had ui");
    let msg = head_message(pm);
    assert!(msg.starts_with("X-5, X-7: tag add ui\n"), "{msg}");
    assert!(
        msg.contains("Issue: X-5\n") && msg.contains("Issue: X-7\n"),
        "{msg}"
    );
    assert!(!msg.contains("Issue: X-3"), "{msg}");
    assert_eq!(tags_of(pm, state, "X-5"), ["api", "ui"]);
    assert_eq!(tags_of(pm, state, "X-7"), ["ui"]);

    // Each issue's history reads the shared commits as its own.
    let (ok, log) = cli(pm, state, &["issue", "log", "X-7"]);
    assert!(ok, "{log}");
    let kinds: Vec<&str> = log["history"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .collect();
    assert_eq!(&kinds[..2], ["tag", "set"], "{log}");
    assert_eq!(log["history"][1]["fields"]["status"], "ready", "{log}");
    let (ok, blame) = cli(pm, state, &["issue", "blame", "X-7"]);
    assert!(ok, "{blame}");
    assert!(blame.to_string().contains("\"field\":\"tags\""), "{blame}");
}

#[test]
fn issue_ls_unknown_project_is_an_error() {
    let (pm, state) = tags_fixture();
    let (pm, state) = (pm.path(), state.path());
    let (ok, out, err) = cli_out_err(pm, state, &["issue", "ls", "--project", "nope", "--json"]);
    assert!(!ok, "unknown project must fail: {out}");
    assert!(err.contains("unknown project 'nope'"), "{err}");
    let known = err.split("known:").nth(1).unwrap_or("");
    assert!(
        known.contains('x') && known.contains('y'),
        "error lists known keys: {err}"
    );
    let (ok, out) = cli(pm, state, &["issue", "ls", "--project", "x", "--json"]);
    assert!(ok, "{out}");
}

#[test]
fn issue_ls_filters_and_epics_match_the_api() {
    let (pm, state) = tags_fixture();
    let (pm, state) = (pm.path(), state.path());
    let (port, _board) = start_ui(pm.to_path_buf(), state.to_path_buf());
    let host = format!("127.0.0.1:{port}");
    let ids = |v: &Value| -> Vec<String> {
        v["issues"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["id"].as_str().unwrap().to_string())
            .collect()
    };
    // (CLI flags, API query, expected ids)
    let cases: &[(&[&str], &str, &[&str])] = &[
        (&["--tag", "ui"], "tag=ui", &["X-3", "X-4", "X-6"]),
        (&["--tag", "ui", "--tag", "api"], "tag=ui&tag=api", &["X-4"]),
        (&["--tag", "ui", "--tag", "api"], "tag=ui,api", &["X-4"]),
        (&["--epic", "X-1"], "epic=X-1", &["X-3", "X-4", "X-5"]),
        (&["--owner", "ann"], "owner=ann", &["X-3", "X-5"]),
        // X-1 rolls up to doing from X-4.
        (
            &["--status", "doing", "--status", "review"],
            "status=doing&status=review",
            &["X-1", "X-4", "X-8"],
        ),
        (&["--status", "dropped"], "status=dropped", &["X-6"]),
        (&["--component", "core"], "component=core", &["X-7"]),
        (&["--priority", "P1"], "priority=P1", &["X-3"]),
        (
            &["--open"],
            "open=1",
            &["X-1", "X-2", "X-4", "X-5", "X-7", "X-8"],
        ),
        (
            &["--epic", "X-1", "--open", "--tag", "api"],
            "epic=X-1&open=1&tag=api",
            &["X-4", "X-5"],
        ),
        (&["--owner", "ann", "--open"], "owner=ann&open=1", &["X-5"]),
        (
            &["--tag", "infra", "--epic", "X-1"],
            "tag=infra&epic=X-1",
            &[],
        ),
    ];
    for (flags, query, want) in cases {
        let mut args = vec!["issue", "ls", "--project", "x", "--json"];
        args.extend_from_slice(flags);
        let (ok, out) = cli(pm, state, &args);
        assert!(ok, "{flags:?}: {out}");
        assert_eq!(ids(&out), *want, "cli {flags:?}");
        let (code, body) = http(
            port,
            "GET",
            &format!("/api/issues?project=x&{query}"),
            &host,
        );
        assert_eq!(code, 200, "{query}: {body}");
        let api: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(ids(&api), *want, "api {query}");
    }
    // Cards carry the tags; a typo in a filter is an error, not an
    // empty list.
    let (_, out) = cli(pm, state, &["issue", "ls", "--tag", "api", "--json"]);
    assert_eq!(out["issues"][0]["tags"], json!(["api", "ui"]));
    assert!(
        !cli(
            pm,
            state,
            &["issue", "ls", "--status", "nonsense", "--json"]
        )
        .0
    );
    for bad in ["status=nonsense", "tag=Bad", "priority=P9", "epic=nope"] {
        let (code, body) = http(port, "GET", &format!("/api/issues?{bad}"), &host);
        assert_eq!(code, 400, "{bad}: {body}");
    }

    // Epics: numbers from the fixture's diagram.
    let (ok, out) = cli(
        pm,
        state,
        &["issue", "epic", "ls", "--project", "x", "--json"],
    );
    assert!(ok, "{out}");
    let epics = out["epics"].as_array().unwrap();
    assert_eq!(epics.len(), 2, "{out}");
    let (a, b) = (&epics[0], &epics[1]);
    assert_eq!(
        (a["id"].as_str(), a["status"].as_str()),
        (Some("X-1"), Some("doing"))
    );
    assert_eq!(a["total"], 3);
    assert_eq!(
        a["counts"],
        json!({"backlog": 1, "ready": 0, "doing": 1, "review": 0, "done": 1, "dropped": 0})
    );
    assert_eq!(a["done_ratio"], 0.33);
    assert_eq!(a["blocked"], 1, "X-5 waits on X-4");
    assert_eq!(a["owners"], json!(["ann", "bob"]));
    assert_eq!(a["children"], json!(["X-3", "X-4", "X-5"]));
    assert_eq!(b["id"], "X-2");
    assert_eq!(b["total"], 2);
    assert_eq!(b["counts"]["dropped"], 1);
    assert_eq!(b["counts"]["ready"], 1);
    assert_eq!(
        b["done_ratio"], 0.0,
        "dropped children leave the ratio's base"
    );
    assert_eq!(b["blocked"], 0);
    assert_eq!(b["owners"], json!(["cy"]));
    // Finishing the only live child completes epic B.
    assert!(cli(pm, state, &["issue", "set", "X-7", "status=done"]).0);
    let (_, out) = cli(pm, state, &["issue", "epic", "ls", "--json"]);
    assert_eq!(out["epics"][1]["done_ratio"], 1.0);
    assert_eq!(out["epics"][1]["status"], "done");
    // The API serves the identical payload; another project has none.
    let (code, body) = http(port, "GET", "/api/epics?project=x", &host);
    assert_eq!(code, 200, "{body}");
    let api: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(api["epics"], out["epics"]);
    let (_, body) = http(port, "GET", "/api/epics?project=y", &host);
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap()["epics"],
        json!([])
    );

    // epic show: the children, as JSON and as an aligned table.
    let (ok, out) = cli(pm, state, &["issue", "epic", "show", "X-1", "--json"]);
    assert!(ok, "{out}");
    assert_eq!(ids(&out), ["X-3", "X-4", "X-5"]);
    assert_eq!(out["total"], 3);
    assert_eq!(out["issues"][1]["tags"], json!(["api", "ui"]));
    let (ok, table) = cli_raw(pm, state, &["issue", "epic", "show", "X-1"]);
    assert!(ok, "{table}");
    let row = table.lines().find(|l| l.starts_with("X-4")).unwrap();
    let header = table.lines().find(|l| l.starts_with("ID")).unwrap();
    assert_eq!(
        header.find("TAGS"),
        row.find("api,ui"),
        "columns align:\n{table}"
    );
    assert!(row.contains("doing") && row.contains("bob"), "{row}");
    let (ok, err) = cli_raw(pm, state, &["issue", "epic", "show", "X-8"]);
    assert!(!ok && err.contains("has no children"), "{err}");
    // --epic is --parent: same rules, and the two cannot be combined.
    let (ok, err) = cli_raw(
        pm,
        state,
        &["issue", "new", "deep", "--project", "x", "--epic", "X-3"],
    );
    assert!(!ok && err.contains("depth"), "{err}");
    let (ok, _) = cli_raw(
        pm,
        state,
        &[
            "issue",
            "new",
            "both",
            "--project",
            "x",
            "--epic",
            "X-1",
            "--parent",
            "X-2",
        ],
    );
    assert!(!ok);
}

/// CAD-437: the shared list grammar on `issue ls` — repeatable any-of
/// value flags, AND across flags, unknown values are errors, and the
/// sort/limit/fields tail.
#[test]
fn issue_ls_cad437_grammar() {
    let (pm, state) = tags_fixture();
    let (pm, state) = (pm.path(), state.path());
    let run = |args: &[&str]| {
        let (ok, out) = cli(pm, state, args);
        assert!(ok, "{args:?}: {out}");
        out
    };
    let ids = |v: &Value| -> Vec<String> {
        let mut ids: Vec<String> = v["issues"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["id"].as_str().unwrap().to_string())
            .collect();
        ids.sort();
        ids
    };

    // --type: X-1/X-2 are epics by the has-children rule; the rest are
    // tasks. Any-of across a comma-joined value.
    let out = run(&["issue", "ls", "--type", "epic", "--json"]);
    assert_eq!(ids(&out), ["X-1", "X-2"]);
    let out = run(&["issue", "ls", "--type", "task,bug", "--json"]);
    assert_eq!(ids(&out).len(), 6, "{out}");

    // --milestone: any-of over the field value (the `m<n>-…` tag
    // mapping is unit-tested in board.rs).
    cli(pm, state, &["issue", "set", "X-7", "milestone=m2"]);
    cli(pm, state, &["issue", "set", "X-8", "milestone=m3"]);
    let out = run(&["issue", "ls", "--milestone", "m2", "--json"]);
    assert_eq!(ids(&out), ["X-7"]);
    let out = run(&[
        "issue",
        "ls",
        "--milestone",
        "m3",
        "--milestone",
        "m2",
        "--json",
    ]);
    assert_eq!(ids(&out), ["X-7", "X-8"]);

    // --plan on a board without plans: `any` selects none; a bad
    // state is an error, not an empty list.
    let out = run(&["issue", "ls", "--plan", "any", "--json"]);
    assert_eq!(ids(&out), Vec::<String>::new());
    let (ok, _, err) = cli_out_err(pm, state, &["issue", "ls", "--plan", "bogus", "--json"]);
    assert!(!ok && err.contains("--plan"), "{err}");

    // --since/--until bound the last-update time (commit clock here).
    let out = run(&["issue", "ls", "--since", "0", "--json"]);
    assert_eq!(ids(&out).len(), 8);
    let out = run(&["issue", "ls", "--since", "9999999999", "--json"]);
    assert_eq!(ids(&out), Vec::<String>::new());
    let out = run(&["issue", "ls", "--until", "0", "--json"]);
    assert_eq!(ids(&out), Vec::<String>::new());
    let (ok, _, err) = cli_out_err(pm, state, &["issue", "ls", "--since", "whenever"]);
    assert!(!ok && err.contains("--since"), "{err}");

    // --sort descends on `-KEY`; the id breaks ties. --limit caps.
    let (ok, out) = cli(pm, state, &["issue", "ls", "--sort", "-id", "--json"]);
    assert!(ok, "{out}");
    let first = out["issues"][0]["id"].as_str().unwrap();
    assert_eq!(first, "X-8", "{out}");
    let out = run(&[
        "issue", "ls", "--sort", "priority", "--limit", "2", "--json",
    ]);
    assert_eq!(ids(&out).len(), 2, "{out}");
    let (ok, _, err) = cli_out_err(pm, state, &["issue", "ls", "--sort", "bogus"]);
    assert!(!ok && err.contains("--sort"), "{err}");

    // --fields keeps only the named keys — and needs --json.
    let out = run(&[
        "issue",
        "ls",
        "--status",
        "doing",
        "--fields",
        "id,status",
        "--json",
    ]);
    assert_eq!(
        out["issues"][0]
            .as_object()
            .unwrap()
            .keys()
            .collect::<Vec<_>>(),
        ["id", "status"]
    );
    let (ok, _, err) = cli_out_err(pm, state, &["issue", "ls", "--fields", "nope", "--json"]);
    assert!(!ok && err.contains("--fields"), "{err}");
    let (ok, _, err) = cli_out_err(pm, state, &["issue", "ls", "--fields", "id"]);
    assert!(!ok && err.contains("--json"), "{err}");

    // Unknown values across the new flags are errors.
    for args in [
        &["issue", "ls", "--type", "widget"][..],
        &["issue", "ls", "--milestone", "BAD TAG"][..],
        &["issue", "ls", "--health", "sunny"][..],
        &["issue", "ls", "--stage", "nonsense"][..],
    ] {
        let (ok, _, err) = cli_out_err(pm, state, args);
        assert!(!ok, "{args:?} must fail: {err}");
    }
    // --stage/--health are live-computed — refused under --at.
    let (ok, _, err) = cli_out_err(
        pm,
        state,
        &["issue", "ls", "--at", "HEAD", "--health", "on_track"],
    );
    assert!(!ok && err.contains("--at"), "{err}");
}

/// CAD-437: `epic ls`/`milestone ls` share the grammar — any-of flags,
/// sort/limit/fields, unknown values error.
#[test]
fn epic_and_milestone_ls_cad437_grammar() {
    let (pm, state) = tags_fixture();
    let (pm, state) = (pm.path(), state.path());
    let ids = |v: &Value, key: &str| -> Vec<String> {
        let mut ids: Vec<String> = v[key]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["id"].as_str().unwrap().to_string())
            .collect();
        ids.sort();
        ids
    };

    // Every epic carries a work block with a stage and health.
    let (ok, out) = cli(pm, state, &["issue", "epic", "ls", "--json"]);
    assert!(ok, "{out}");
    let epics = out["epics"].as_array().unwrap();
    assert_eq!(epics.len(), 2);
    let stage = epics[0]["work"]["stage"]["id"].as_str().unwrap_or("?");
    assert!(!stage.is_empty() && stage != "?", "{epics:?}");

    // --stage/--health/--milestone are any-of; AND across flags.
    let (ok, out) = cli(
        pm,
        state,
        &[
            "issue",
            "epic",
            "ls",
            "--health",
            "on_track,at_risk",
            "--json",
        ],
    );
    assert!(ok, "{out}");
    assert_eq!(ids(&out, "epics").len(), 2);
    let (ok, out) = cli(
        pm,
        state,
        &["issue", "epic", "ls", "--health", "stalled", "--json"],
    );
    assert!(ok);
    assert_eq!(ids(&out, "epics"), Vec::<String>::new());
    let (ok, out) = cli(
        pm,
        state,
        &[
            "issue", "epic", "ls", "--sort", "-id", "--limit", "1", "--json",
        ],
    );
    assert!(ok, "{out}");
    assert_eq!(ids(&out, "epics"), ["X-2"]);
    let (ok, _, err) = cli_out_err(
        pm,
        state,
        &["issue", "epic", "ls", "--health", "sunny", "--json"],
    );
    assert!(!ok && err.contains("--health"), "{err}");
    let (ok, _, err) = cli_out_err(
        pm,
        state,
        &["issue", "epic", "ls", "--fields", "nope", "--json"],
    );
    assert!(!ok && err.contains("--fields"), "{err}");

    // Milestones: name one via the field, filter + shape rows.
    cli(pm, state, &["issue", "set", "X-7", "milestone=m9"]);
    let (ok, out) = cli(pm, state, &["milestone", "ls", "--json"]);
    assert!(ok, "{out}");
    let ms = out["milestones"].as_array().unwrap();
    assert!(!ms.is_empty(), "{out}");
    let (ok, out) = cli(
        pm,
        state,
        &["milestone", "ls", "--milestone", "m9", "--json"],
    );
    assert!(ok, "{out}");
    assert_eq!(ids(&out, "milestones"), ["m9"]);
    let (ok, _, err) = cli_out_err(
        pm,
        state,
        &["milestone", "ls", "--health", "sunny", "--json"],
    );
    assert!(!ok && err.contains("--health"), "{err}");
}

#[test]
fn issue_trailer_prints_and_validates() {
    let (pm, state, _repo, _port, _board) = commits_fixture();
    let (ok, out) = cli_raw(pm.path(), state.path(), &["issue", "trailer", "X-1"]);
    assert!(ok, "{out}");
    assert_eq!(out, "Issue: X-1");
    let (ok, err) = cli_raw(pm.path(), state.path(), &["issue", "trailer", "X-9"]);
    assert!(!ok);
    assert!(err.contains("Unknown issue"), "{err}");
    let (ok, err) = cli_raw(pm.path(), state.path(), &["issue", "trailer", "bad id"]);
    assert!(!ok);
    assert!(err.contains("issue id") || err.contains("Unknown"), "{err}");
}

#[test]
fn issue_doctor_reports_trailer_share() {
    let fx = history_fixture();
    // Doctor exits non-zero when checks fail; the report prints
    // either way, so assert the payload not the status.
    let (_, out) = cli(fx.pm.path(), fx.state.path(), &["issue", "doctor"]);
    let trailers = &out["trailers"];
    let (window, with) = (
        trailers["window"].as_u64().unwrap_or(0),
        trailers["with_trailers"].as_u64().unwrap_or(0),
    );
    // 7 issue writes + project add + init all carry `Actor:` now.
    assert!(window > 0 && with >= 9, "{trailers}");
}

// ---------- CAD-43: issue start ----------

/// Temp project repo (`main`, one commit) + tracker with a `demo`/`D`
/// project pointing at it. Returns the tmp guard plus the paths.
fn start_fx() -> (TempDir, PathBuf, PathBuf, PathBuf) {
    let tmp = TempDir::new().unwrap();
    let pm = tmp.path().join("pm");
    let state = tmp.path().join("state");
    let repo = tmp.path().join("repo");
    for d in [&pm, &state, &repo] {
        std::fs::create_dir_all(d).unwrap();
    }
    assert!(git(&repo, &["init", "-b", "main"]).0);
    git(&repo, &["config", "user.email", "t@t"]);
    git(&repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join("f"), "x").unwrap();
    git(&repo, &["add", "-A"]);
    assert!(git(&repo, &["commit", "-qm", "init"]).0);
    assert!(cli(&pm, &state, &["issue", "init"]).0);
    let repo_s = repo.to_str().unwrap().to_string();
    assert!(
        cli(
            &pm,
            &state,
            &["issue", "project", "add", "demo", "--prefix", "D", "--repo", &repo_s]
        )
        .0
    );
    (tmp, pm, state, repo)
}

#[test]
fn issue_start_creates_records_and_is_idempotent() {
    let (_tmp, pm, state, repo) = start_fx();
    assert!(
        cli(
            &pm,
            &state,
            &["issue", "new", "Start Work", "--project", "demo"]
        )
        .0
    );
    let base_commits = commits(&pm);

    let (ok, out) = cli(&pm, &state, &["issue", "start", "D-1"]);
    assert!(ok, "{out}");
    let wt = repo.join(".cadence/wt/d-1-start-work");
    assert_eq!(out["issue"], "D-1");
    assert_eq!(out["worktree"].as_str().unwrap(), wt.to_str().unwrap());
    assert_eq!(out["branch"], "cadence/d-1-start-work");
    assert_eq!(
        out["repo"].as_str().unwrap(),
        repo.canonicalize().unwrap().to_str().unwrap()
    );
    assert_eq!(out["trailer"], "Issue: D-1");
    assert_eq!(out["created"], true);
    assert_eq!(out["base"]["sha"].as_str().unwrap().len(), 40);
    assert!(wt.is_dir());
    // The worktree is checked out on the new branch.
    assert_eq!(
        git(&wt, &["symbolic-ref", "--short", "HEAD"]).1,
        "cadence/d-1-start-work"
    );
    // `.cadence/` ignore line added exactly once.
    let ignore = std::fs::read_to_string(repo.join(".gitignore")).unwrap();
    assert_eq!(
        ignore.lines().filter(|l| l.trim() == ".cadence/").count(),
        1
    );

    // One tracker commit: `<ID>: start <branch>` + CAD-42 trailers.
    assert_eq!(commits(&pm), base_commits + 1);
    let (_, body) = git(&pm, &["log", "-1", "--format=%B"]);
    assert!(body.contains("D-1: start cadence/d-1-start-work"), "{body}");
    assert!(body.contains("Issue: D-1"), "{body}");
    assert!(body.contains("Actor: operator"), "{body}");

    // Front: both refs, status doing, owner = resolved actor.
    let front = std::fs::read_to_string(pm.join("demo/D-1/issue.md")).unwrap();
    assert!(front.contains("kind: branch"), "{front}");
    assert!(front.contains("cadence/d-1-start-work"), "{front}");
    assert!(front.contains("kind: worktree"), "{front}");
    assert!(front.contains("status: doing"), "{front}");
    assert!(front.contains("owner: operator"), "{front}");

    // Second run: idempotent — same answer, created:false, no commit.
    let (ok, out2) = cli(&pm, &state, &["issue", "start", "D-1"]);
    assert!(ok, "{out2}");
    assert_eq!(out2["created"], false);
    assert_eq!(out2["worktree"], out["worktree"]);
    assert_eq!(commits(&pm), base_commits + 1);
}

#[test]
fn issue_start_repo_resolution() {
    let (_tmp, pm, state, repo) = start_fx();
    assert!(
        cli(
            &pm,
            &state,
            &["issue", "new", "Repo Pick", "--project", "demo"]
        )
        .0
    );
    let repo_s = repo.to_str().unwrap().to_string();

    // --repo explicit.
    let (ok, out) = cli(
        &pm,
        &state,
        &[
            "issue", "start", "D-1", "--repo", &repo_s, "--name", "explicit",
        ],
    );
    assert!(ok, "{out}");
    assert!(out["worktree"].as_str().unwrap().ends_with("d-1-explicit"));

    // cwd inside the repo resolves without --repo. Each case starts
    // its own issue: D-1 already has an open lane, and a second
    // `--name` on it is refused (CAD-274).
    for id in ["D-2", "D-3"] {
        assert!(cli(&pm, &state, &["issue", "new", id, "--project", "demo"]).0);
    }
    let (ok, out) = cli_dir(
        &pm,
        &state,
        &repo,
        &["issue", "start", "D-2", "--name", "from-cwd"],
    );
    assert!(ok, "{out}");
    assert!(out["worktree"].as_str().unwrap().ends_with("d-2-from-cwd"));

    // Single-repo fallback (cwd is the test process — not a project repo).
    let (ok, out) = cli(&pm, &state, &["issue", "start", "D-3", "--name", "single"]);
    assert!(ok, "{out}");

    // Ambiguous: a second repo on a second project refuses with the list.
    let repo2 = _tmp.path().join("repo2");
    std::fs::create_dir_all(&repo2).unwrap();
    assert!(git(&repo2, &["init", "-b", "main"]).0);
    git(&repo2, &["config", "user.email", "t@t"]);
    git(&repo2, &["config", "user.name", "t"]);
    std::fs::write(repo2.join("g"), "y").unwrap();
    git(&repo2, &["add", "-A"]);
    assert!(git(&repo2, &["commit", "-qm", "init"]).0);
    let repo2_s = repo2.to_str().unwrap().to_string();
    assert!(
        cli(
            &pm,
            &state,
            &[
                "issue", "project", "add", "two", "--prefix", "T", "--repo", &repo_s, "--repo",
                &repo2_s
            ]
        )
        .0
    );
    assert!(cli(&pm, &state, &["issue", "new", "Ambig", "--project", "two"]).0);
    let (ok, err) = cli(&pm, &state, &["issue", "start", "T-1"]);
    assert!(!ok);
    let msg = err["error"].as_str().unwrap();
    assert!(msg.contains("--repo"), "{msg}");
    assert!(msg.contains(&repo_s) && msg.contains(&repo2_s), "{msg}");

    // A real repo undeclared on the issue's project refuses, naming
    // the declared repos — code-commit discovery only walks those.
    let before = git(&pm, &["rev-list", "--count", "HEAD"]).1;
    let (ok, err) = cli(
        &pm,
        &state,
        &[
            "issue",
            "start",
            "D-1",
            "--repo",
            &repo2_s,
            "--name",
            "undeclared",
        ],
    );
    assert!(!ok);
    let msg = err["error"].as_str().unwrap();
    assert!(msg.contains("not declared"), "{msg}");
    assert!(msg.contains(&repo_s), "{msg}");
    assert!(msg.contains("project.yaml"), "{msg}");
    assert!(!repo2.join(".cadence/wt/d-1-undeclared").exists());
    assert!(!git(&repo2, &["rev-parse", "--verify", "cadence/d-1-undeclared"]).0);
    assert_eq!(git(&pm, &["rev-list", "--count", "HEAD"]).1, before);

    // Bad --repo path refuses; bad --base refuses.
    let (ok, err) = cli(
        &pm,
        &state,
        &["issue", "start", "D-1", "--repo", "/nonexistent"],
    );
    assert!(!ok && err["error"].as_str().unwrap().contains("git repository"));
    let (ok, err) = cli(
        &pm,
        &state,
        &[
            "issue", "start", "D-1", "--name", "bad-base", "--base", "nope-ref",
        ],
    );
    assert!(!ok && err["error"].as_str().unwrap().contains("does not resolve"));

    // --job without --pm/--spec is a clap error, not a start (usage
    // text, not JSON — plain-text read).
    let (ok, usage) = cli_raw(&pm, &state, &["issue", "start", "D-1", "--job"]);
    assert!(!ok && usage.contains("--pm"), "{usage}");
}

#[test]
fn issue_start_conflicting_branch_refused_and_status_owner() {
    let (_tmp, pm, state, repo) = start_fx();
    assert!(cli(&pm, &state, &["issue", "new", "Clash", "--project", "demo"]).0);
    assert!(cli(&pm, &state, &["issue", "set", "D-1", "status=ready"]).0);

    // A pre-existing branch under the same name refuses, naming the
    // branch and the recorded worktree — nothing created, no commit.
    assert!(git(&repo, &["branch", "cadence/d-1-clash"]).0);
    let before = commits(&pm);
    let (ok, err) = cli(&pm, &state, &["issue", "start", "D-1", "--name", "clash"]);
    assert!(!ok);
    let msg = err["error"].as_str().unwrap();
    assert!(
        msg.contains("cadence/d-1-clash") && msg.contains("worktree"),
        "{msg}"
    );
    assert!(!repo.join(".cadence/wt/d-1-clash").exists());
    assert_eq!(commits(&pm), before);
    git(&repo, &["branch", "-D", "cadence/d-1-clash"]);

    // ready -> doing, --owner wins when empty.
    let (ok, _) = cli(&pm, &state, &["issue", "start", "D-1", "--owner", "alice"]);
    assert!(ok);
    let front = std::fs::read_to_string(pm.join("demo/D-1/issue.md")).unwrap();
    assert!(
        front.contains("status: doing") && front.contains("owner: alice"),
        "{front}"
    );

    // review stays review; an existing owner is kept.
    assert!(
        cli(
            &pm,
            &state,
            &["issue", "new", "In Review", "--project", "demo"]
        )
        .0
    );
    assert!(
        cli(
            &pm,
            &state,
            &["issue", "set", "D-2", "status=review", "owner=bob"]
        )
        .0
    );
    // CAD-383: bob holds the review issue — another requester is
    // refused; bob's own start keeps the status and the owner.
    let (ok, err) = cli(&pm, &state, &["issue", "start", "D-2"]);
    assert!(
        !ok && err["error"].as_str().unwrap().contains("bob"),
        "{err}"
    );
    let (ok, out) = cli(&pm, &state, &["issue", "start", "D-2", "--by", "bob"]);
    assert!(ok, "{out}");
    let front = std::fs::read_to_string(pm.join("demo/D-2/issue.md")).unwrap();
    assert!(
        front.contains("status: review") && front.contains("owner: bob"),
        "{front}"
    );
}

// ---- CAD-55: `cadence dispatch` + `cadence issue finish` ----

#[test]
fn issue_finish_pairs_branch_when_dir_name_differs() {
    // CAD-166: a worktree whose dir name is not its branch name (moved,
    // hand-recorded or adopted) must still finish its branch.
    let (_tmp, pm, state, repo) = start_fx();
    assert!(cli(&pm, &state, &["issue", "new", "Moved", "--project", "demo"]).0);
    let (ok, out) = cli(&pm, &state, &["issue", "start", "D-1"]);
    assert!(ok, "{out}");
    let old = repo.join(".cadence/wt/d-1-moved");
    let new = repo.join(".cadence/wt/elsewhere");
    let moved = git(
        &repo,
        &[
            "worktree",
            "move",
            old.to_str().unwrap(),
            new.to_str().unwrap(),
        ],
    );
    assert!(moved.0, "{}", moved.1);
    let file = pm.join("demo/D-1/issue.md");
    let front = std::fs::read_to_string(&file).unwrap();
    let old_s = old.canonicalize().unwrap_or(old.clone());
    let recorded = if front.contains(old.to_str().unwrap()) {
        old.to_str().unwrap().to_string()
    } else {
        old_s.to_str().unwrap().to_string()
    };
    assert!(front.contains(&recorded), "{front}");
    std::fs::write(&file, front.replace(&recorded, new.to_str().unwrap())).unwrap();
    // Explicit identity: CI runners have no global git user.
    let committed = git(
        &pm,
        &[
            "-c",
            "user.name=hand",
            "-c",
            "user.email=hand@h",
            "commit",
            "-qam",
            "D-1: hand-move worktree ref",
        ],
    );
    assert!(committed.0, "{}", committed.1);
    land(&repo, &new, "moved.txt");

    let (ok, out) = cli(&pm, &state, &["issue", "finish", "D-1"]);
    assert!(ok, "{out}");
    assert_eq!(out["removed_worktree"], true, "{out}");
    assert_eq!(out["deleted_branch"], true, "branch must not leak: {out}");
    assert!(!new.exists());
    assert!(
        !git(
            &repo,
            &["rev-parse", "--verify", "--quiet", "cadence/d-1-moved"]
        )
        .0
    );
}

/// Rewrite `id`'s issue.md by hand and commit it — the tracker is plain
/// markdown any agent can edit, bypassing every CLI write check.
fn hand_edit(pm: &Path, id: &str, edit: impl Fn(String) -> String) {
    let file = pm.join(format!("demo/{id}/issue.md"));
    let before = std::fs::read_to_string(&file).unwrap();
    let after = edit(before.clone());
    assert_ne!(before, after, "hand edit changed nothing");
    std::fs::write(&file, after).unwrap();
    let committed = git(
        pm,
        &[
            "-c",
            "user.name=hand",
            "-c",
            "user.email=hand@h",
            "commit",
            "-qam",
            &format!("{id}: hand edit"),
        ],
    );
    assert!(committed.0, "{}", committed.1);
}

/// The recorded spelling of `path` in `id`'s issue.md — as given or
/// canonicalized, whichever the writer stored.
fn recorded_path(pm: &Path, id: &str, path: &Path) -> String {
    let front = std::fs::read_to_string(pm.join(format!("demo/{id}/issue.md"))).unwrap();
    let canon = path.canonicalize().unwrap_or(path.to_path_buf());
    [path, canon.as_path()]
        .iter()
        .map(|p| p.to_str().unwrap().to_string())
        .find(|p| front.contains(&format!("path: {p}\n")))
        .unwrap_or_else(|| panic!("{} not recorded: {front}", path.display()))
}

/// CAD-144: a ref value beginning with `-` reads as a git option
/// (`--upload-pack=<cmd>` executes over ssh/file remotes). `issue ref`
/// and `issue start` refuse to write one, and nothing is committed.
#[test]
fn issue_ref_and_start_refuse_option_like_values() {
    let (_tmp, pm, state, repo) = start_fx();
    assert!(cli(&pm, &state, &["issue", "new", "Lane", "--project", "demo"]).0);
    let (ok, out) = cli(&pm, &state, &["issue", "start", "D-1"]);
    assert!(ok, "{out}");
    let before = commits(&pm);
    for value in ["-x", "--upload-pack=touch /tmp/cad144-never"] {
        for kind in ["branch", "worktree", "note"] {
            let (ok, err) = cli(&pm, &state, &["issue", "ref", "D-1", kind, "--", value]);
            assert!(!ok, "{kind} {value} was written: {err}");
            assert!(
                err["error"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("begins with '-'"),
                "{err}"
            );
        }
    }
    assert_eq!(commits(&pm), before, "a refused ref commits nothing");

    // `issue start` re-records the lane's branch from the tracker and
    // the checkout: a hand-recorded `-x` checked out in the lane must
    // not be written back.
    let wt = repo.join(".cadence/wt/d-1-lane");
    assert!(git(&wt, &["update-ref", "refs/heads/-x", "HEAD"]).0);
    assert!(git(&wt, &["symbolic-ref", "HEAD", "refs/heads/-x"]).0);
    hand_edit(&pm, "D-1", |f| {
        f.replace("path: cadence/d-1-lane\n", "path: '-x'\n")
    });
    let before = commits(&pm);
    let (ok, err) = cli(&pm, &state, &["issue", "start", "D-1"]);
    assert!(!ok, "start re-recorded '-x': {err}");
    assert!(
        err["error"]
            .as_str()
            .unwrap_or_default()
            .contains("begins with '-'"),
        "{err}"
    );
    assert_eq!(commits(&pm), before, "a refused start commits nothing");
}

/// CAD-144: finish refuses an option-like ref it reads from a
/// hand-edited tracker before any git command sees it — with a file
/// remote, `fetch origin --upload-pack=<cmd>` would run the command.
#[test]
fn issue_finish_refuses_option_like_refs() {
    let (tmp, pm, state, repo) = start_fx();
    let bare = tmp.path().join("remote.git");
    assert!(git(tmp.path(), &["init", "-q", "--bare", "remote.git"]).0);
    assert!(git(&repo, &["remote", "add", "origin", bare.to_str().unwrap()]).0);
    assert!(
        cli(
            &pm,
            &state,
            &["issue", "new", "Hostile", "--project", "demo"]
        )
        .0
    );
    let (ok, out) = cli(&pm, &state, &["issue", "start", "D-1"]);
    assert!(ok, "{out}");
    let wt = repo.join(".cadence/wt/d-1-hostile");
    let wt_s = recorded_path(&pm, "D-1", &wt);
    let marker = tmp.path().join("upload-pack-ran");
    // The worktree ref closed, the branch ref rewritten: finish reads
    // the lone open branch ref as its target.
    let hostile = format!("--upload-pack=touch {}", marker.display());
    hand_edit(&pm, "D-1", |f| {
        f.replace(
            "path: cadence/d-1-hostile\n",
            &format!("path: '{hostile}'\n"),
        )
        .replace(
            &format!("path: {wt_s}\n"),
            &format!("path: {wt_s}\n  closed: true\n"),
        )
    });
    let before = commits(&pm);
    let (ok, err) = cli(
        &pm,
        &state,
        &["issue", "finish", "D-1", "--remote", "--force"],
    );
    assert!(!ok, "finish ran with '{hostile}': {err}");
    assert!(
        err["error"]
            .as_str()
            .unwrap_or_default()
            .contains("begins with '-'"),
        "{err}"
    );
    assert!(!marker.exists(), "--upload-pack reached git");
    assert_eq!(commits(&pm), before, "a refused finish commits nothing");
    // The same refusal for a plain `-x`, and for a worktree ref.
    hand_edit(&pm, "D-1", |f| f.replace(&format!("'{hostile}'"), "'-x'"));
    let (ok, err) = cli(&pm, &state, &["issue", "finish", "D-1", "--force"]);
    assert!(
        !ok && err["error"].as_str().unwrap_or_default().contains("'-x'"),
        "{err}"
    );
    hand_edit(&pm, "D-1", |f| {
        f.replace(&format!("path: {wt_s}\n  closed: true\n"), "path: '-wt'\n")
    });
    let (ok, err) = cli(&pm, &state, &["issue", "finish", "D-1", "--force"]);
    assert!(
        !ok && err["error"].as_str().unwrap_or_default().contains("'-wt'"),
        "{err}"
    );
    assert!(wt.is_dir(), "nothing was removed");
}

/// CAD-145: a branch kept with `--keep-branch` keeps its branch ref
/// open — the surviving work stays on the board and finishable — while
/// the worktree ref closes. A later finish deletes the merged branch
/// and closes the ref.
#[test]
fn issue_finish_keep_branch_leaves_its_ref_open() {
    let (_tmp, pm, state, repo) = start_fx();
    assert!(cli(&pm, &state, &["issue", "new", "Keep", "--project", "demo"]).0);
    let (ok, out) = cli(&pm, &state, &["issue", "start", "D-1"]);
    assert!(ok, "{out}");
    let wt = repo.join(".cadence/wt/d-1-keep");
    land(&repo, &wt, "keep.txt");
    let (ok, out) = cli(&pm, &state, &["issue", "finish", "D-1", "--keep-branch"]);
    assert!(ok, "{out}");
    assert_eq!(out["removed_worktree"], true, "{out}");
    assert_eq!(out["deleted_branch"], false, "{out}");
    assert!(
        git(
            &repo,
            &["rev-parse", "--verify", "--quiet", "cadence/d-1-keep"]
        )
        .0
    );
    let refs = refs_of(&pm, &state, "D-1");
    let closed = |kind: &str| {
        refs.iter()
            .find(|r| r["kind"] == kind)
            .map(|r| r["closed"] == true)
            .unwrap_or_else(|| panic!("no {kind} ref: {refs:?}"))
    };
    assert!(closed("worktree"), "the worktree ref closes: {refs:?}");
    assert!(
        !closed("branch"),
        "the kept branch's ref stays open: {refs:?}"
    );

    let (ok, out) = cli(&pm, &state, &["issue", "finish", "D-1"]);
    assert!(ok, "{out}");
    assert_eq!(out["finished"], true, "{out}");
    assert_eq!(out["deleted_branch"], true, "{out}");
    let refs = refs_of(&pm, &state, "D-1");
    assert!(
        refs.iter()
            .filter(|r| r["kind"] == "branch" || r["kind"] == "worktree")
            .all(|r| r["closed"] == true),
        "{refs:?}"
    );
}

/// CAD-265: the safety half of CAD-166's pairing — a worktree whose dir
/// differs from its branch, with a checked-out branch that is NOT a
/// recorded ref, finishes without deleting that branch, locally or on
/// the remote. It is foreign work (an adopted checkout, ADR-0003).
#[test]
fn issue_finish_never_deletes_an_unrecorded_checked_out_branch() {
    let (tmp, pm, state, repo) = start_fx();
    let bare = tmp.path().join("remote.git");
    assert!(git(tmp.path(), &["init", "-q", "--bare", "remote.git"]).0);
    assert!(git(&repo, &["remote", "add", "origin", bare.to_str().unwrap()]).0);
    assert!(cli(&pm, &state, &["issue", "new", "Moved", "--project", "demo"]).0);
    let (ok, out) = cli(&pm, &state, &["issue", "start", "D-1"]);
    assert!(ok, "{out}");
    let old = repo.join(".cadence/wt/d-1-moved");
    let new = repo.join(".cadence/wt/elsewhere");
    let old_s = recorded_path(&pm, "D-1", &old);
    let moved = git(&repo, &["worktree", "move", &old_s, new.to_str().unwrap()]);
    assert!(moved.0, "{}", moved.1);
    hand_edit(&pm, "D-1", |f| f.replace(&old_s, new.to_str().unwrap()));
    // Foreign work: an unrecorded branch with a commit of its own,
    // merged and pushed — every rule would call it deletable.
    assert!(git(&new, &["checkout", "-q", "-b", "foreign"]).0);
    land(&repo, &new, "foreign.txt");
    assert!(git(&repo, &["push", "-q", "origin", "foreign"]).0);
    let tip = git(&repo, &["rev-parse", "foreign"]).1;

    let (ok, out) = cli(&pm, &state, &["issue", "finish", "D-1", "--remote"]);
    assert!(ok, "{out}");
    assert_eq!(out["branch"], "", "the foreign branch never pairs: {out}");
    assert_eq!(out["deleted_branch"], false, "{out}");
    assert_eq!(out["remote_deleted"], false, "{out}");
    assert_eq!(
        git(&repo, &["rev-parse", "--verify", "--quiet", "foreign"]).1,
        tip
    );
    assert_eq!(
        git(
            &bare,
            &["rev-parse", "--verify", "--quiet", "refs/heads/foreign"]
        )
        .1,
        tip,
        "the remote copy survives"
    );
}

/// `issue finish` without a reachable daemon refuses rather than
/// guesses; `--force` overrides and is recorded; a second finish is a
/// no-op; an issue without refs has nothing to finish.
#[test]
fn issue_finish_daemon_down_force_and_idempotent() {
    let (_tmp, pm, state, repo) = start_fx();
    assert!(cli(&pm, &state, &["issue", "new", "Done", "--project", "demo"]).0);
    let (ok, out) = cli(&pm, &state, &["issue", "start", "D-1"]);
    assert!(ok, "{out}");
    let wt = repo.join(".cadence/wt/d-1-done");
    assert!(wt.is_dir());

    // A stale socket is a daemon that was there and stopped answering
    // — owner 'operator' can't be checked and finish refuses rather
    // than guessing.
    std::fs::write(state.join("cadence.sock"), "stale").unwrap();
    let (ok, err) = cli(&pm, &state, &["issue", "finish", "D-1"]);
    assert!(!ok, "{err}");
    let msg = err["error"].as_str().unwrap();
    assert!(
        msg.contains("operator") && msg.contains("reachable"),
        "{msg}"
    );
    assert!(wt.is_dir(), "refused finish must not remove the worktree");

    // --force overrides and records it; the tracker commit carries
    // the Forced trailer and closes both refs as history.
    let (ok, out) = cli(&pm, &state, &["issue", "finish", "D-1", "--force"]);
    assert!(ok, "{out}");
    assert_eq!(out["finished"], true);
    assert_eq!(out["removed_worktree"], true);
    assert_eq!(out["deleted_branch"], true);
    assert!(
        out["overrode"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o.as_str().unwrap().contains("unreachable")),
        "{out}"
    );
    assert!(!wt.exists());
    assert!(
        !git(
            &repo,
            &["rev-parse", "--verify", "--quiet", "cadence/d-1-done"]
        )
        .0
    );
    let body = git(&pm, &["log", "-1", "--format=%B"]).1;
    assert!(
        body.contains("D-1: finish cadence/d-1-done") && body.contains("Forced: true"),
        "{body}"
    );
    let front = std::fs::read_to_string(pm.join("demo/D-1/issue.md")).unwrap();
    assert!(front.contains("closed: true"), "{front}");
    assert!(front.contains("status: doing"), "status untouched: {front}");

    // A cleanly stopped daemon removes its socket — that means "no
    // agents", not "unreachable": the /proc and pane scans carry the
    // check and a clean merged worktree finishes without --force.
    std::fs::remove_file(state.join("cadence.sock")).unwrap();
    assert!(cli(&pm, &state, &["issue", "new", "Idle", "--project", "demo"]).0);
    assert!(cli(&pm, &state, &["issue", "start", "D-2"]).0);
    land(&repo, &repo.join(".cadence/wt/d-2-idle"), "idle.txt");
    let (ok, out) = cli(&pm, &state, &["issue", "finish", "D-2"]);
    assert!(
        ok && out["finished"] == true && out["overrode"] == json!([]),
        "no daemon at all must not block a clean finish: {out}"
    );

    // Second finish is a no-op, not an error.
    let (ok, out) = cli(&pm, &state, &["issue", "finish", "D-1"]);
    assert!(ok, "{out}");
    assert_eq!(out["finished"], false);

    // An issue never started has nothing to finish.
    assert!(cli(&pm, &state, &["issue", "new", "Never", "--project", "demo"]).0);
    let (ok, err) = cli(&pm, &state, &["issue", "finish", "D-3"]);
    assert!(!ok && err["error"].as_str().unwrap().contains("nothing to finish"));

    // --keep-branch leaves the local branch; the worktree ref closes.
    assert!(cli(&pm, &state, &["issue", "new", "Keep", "--project", "demo"]).0);
    assert!(cli(&pm, &state, &["issue", "start", "D-4"]).0);
    let (ok, out) = cli(
        &pm,
        &state,
        &["issue", "finish", "D-4", "--force", "--keep-branch"],
    );
    assert!(
        ok && out["kept_branch"] == true && out["deleted_branch"] == false,
        "{out}"
    );
    assert!(
        git(
            &repo,
            &["rev-parse", "--verify", "--quiet", "cadence/d-4-keep"]
        )
        .0
    );
}

/// With no owner recorded (hand-edited history), the owner check is
/// skipped; the bound-message enumeration still needs a daemon that
/// answers (an unanswerable enumeration refuses — a task-bound
/// kickoff could hide anywhere). The worktree-side guards stay
/// observable: a dirty worktree refuses listing the files, an
/// unmerged+unpushed branch refuses, and finish succeeds once the
/// branch is merged.
#[test]
fn issue_finish_dirty_and_unmerged_refusals() {
    let (_tmp, pm, state, repo) = start_fx();
    let _d = UiDaemon::start_on(state.clone());
    assert!(
        cli(
            &pm,
            &state,
            &["issue", "new", "Guards", "--project", "demo"]
        )
        .0
    );
    assert!(cli(&pm, &state, &["issue", "start", "D-1"]).0);
    let wt = repo.join(".cadence/wt/d-1-guards");

    // Strip the owner so the daemon check is skipped — a tracker
    // commit of its own so finish commits nothing extra.
    let md = pm.join("demo/D-1/issue.md");
    let front = std::fs::read_to_string(&md).unwrap();
    std::fs::write(&md, front.replace("owner: operator\n", "")).unwrap();
    git(&pm, &["add", "-A"]);
    git(
        &pm,
        &[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-qm",
            "setup",
        ],
    );
    let before = commits(&pm);

    // Dirty: the refusal lists the uncommitted paths.
    std::fs::write(wt.join("scratch.txt"), "wip").unwrap();
    let (ok, err) = cli(&pm, &state, &["issue", "finish", "D-1"]);
    assert!(!ok, "{err}");
    let msg = err["error"].as_str().unwrap();
    assert!(
        msg.contains("uncommitted") && msg.contains("scratch.txt"),
        "{msg}"
    );
    assert!(wt.is_dir());
    assert_eq!(commits(&pm), before);

    // Committed but unmerged and unpushed: the work would be lost.
    git(&wt, &["add", "-A"]);
    git(&wt, &["commit", "-qm", "wip"]);
    idle(&wt);
    let (ok, err) = cli(&pm, &state, &["issue", "finish", "D-1"]);
    assert!(!ok, "{err}");
    assert!(
        err["error"].as_str().unwrap().contains("neither merged"),
        "{err}"
    );
    assert!(wt.is_dir());

    // Merge into the repo's default branch → the guards pass and the
    // cleanup lands in one tracker commit.
    git(&repo, &["merge", "-q", "cadence/d-1-guards"]);
    let (ok, out) = cli(&pm, &state, &["issue", "finish", "D-1"]);
    assert!(ok, "{out}");
    assert_eq!(out["finished"], true);
    assert_eq!(out["overrode"], json!([]));
    assert!(!wt.exists());
    assert!(
        !git(
            &repo,
            &["rev-parse", "--verify", "--quiet", "cadence/d-1-guards"]
        )
        .0
    );
    assert_eq!(commits(&pm), before + 1);

    // Unmerged but pushed: a plain finish (no --remote) leaves the
    // remote alone, so the pushed copy IS the survivability evidence
    // — the local branch is deleted, not kept.
    assert!(cli(&pm, &state, &["issue", "new", "Pushd", "--project", "demo"]).0);
    assert!(cli(&pm, &state, &["issue", "start", "D-2"]).0);
    assert!(cli(&pm, &state, &["issue", "set", "D-2", "owner="]).0);
    let wt2 = repo.join(".cadence/wt/d-2-pushd");
    std::fs::write(wt2.join("p.txt"), "x").unwrap();
    git(&wt2, &["add", "-A"]);
    git(&wt2, &["commit", "-qm", "pushed work"]);
    idle(&wt2);
    let tip = git(&repo, &["rev-parse", "cadence/d-2-pushd"]).1;
    git(
        &repo,
        &[
            "update-ref",
            "refs/remotes/origin/cadence/d-2-pushd",
            tip.trim(),
        ],
    );
    let (ok, out) = cli(&pm, &state, &["issue", "finish", "D-2"]);
    assert!(
        ok && out["finished"] == true && out["deleted_branch"] == true,
        "pushed evidence alone still deletes the local on a plain finish: {out}"
    );
    assert!(!wt2.exists());
}

/// Drop the recorded owner (and commit the edit) so `issue finish`
/// skips the daemon-owner check entirely — board tests run with no
/// daemon, and an owner would make finish refuse "unreachable".
fn strip_owner(pm: &Path, id: &str) {
    let md = pm.join(format!("demo/{id}/issue.md"));
    let front = std::fs::read_to_string(&md).unwrap();
    std::fs::write(&md, front.replace("owner: operator\n", "")).unwrap();
    git(pm, &["add", "-A"]);
    git(
        pm,
        &[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-qm",
            "strip owner",
        ],
    );
}

/// CAD-275: age every file under `dir` past `issue finish`'s
/// 30-minute active window — the state of a lane nobody has touched
/// since. Without it a lane written seconds ago is "in use".
fn idle(dir: &Path) {
    let st = Command::new("find")
        .arg(dir)
        .args(["-exec", "touch", "-h", "-d", "2 hours ago", "{}", "+"])
        .status()
        .unwrap();
    assert!(st.success(), "backdate {}", dir.display());
}

/// CAD-275: give a fresh lane real work — one commit, fast-forward
/// merged into the repo's default branch — then idle it, so finish
/// sees a started, merged, untouched lane.
fn land(repo: &Path, wt: &Path, file: &str) {
    std::fs::write(wt.join(file), format!("{file}\n")).unwrap();
    assert!(git(wt, &["add", "-A"]).0);
    assert!(git(wt, &["commit", "-qm", &format!("work {file}")]).0);
    let branch = git(wt, &["symbolic-ref", "--short", "HEAD"]).1;
    assert!(
        git(repo, &["merge", "-q", "--ff-only", &branch]).0,
        "merge {branch}"
    );
    idle(wt);
}

/// CAD-64: a branch is "merged" when its work is on the default
/// branch however it got there — a squash merge (combined patch) or
/// cherry-picked commits both count, while an extra unmerged commit
/// still refuses. Ignored paths (the ui/node_modules build symlink)
/// never count as dirty; real untracked files do and are listed.
#[test]
fn issue_finish_squash_cherry_and_ignored() {
    let (_tmp, pm, state, repo) = start_fx();
    let _d = UiDaemon::start_on(state.clone());
    // The build-symlink rule lives in the repo's .gitignore, like the
    // real cadence repo.
    std::fs::write(repo.join(".gitignore"), "/ui/node_modules\n").unwrap();
    git(&repo, &["add", ".gitignore"]);
    git(&repo, &["commit", "-qm", "ignore rules"]);

    // D-1: two branch commits squash-merged into one → "patch".
    assert!(cli(&pm, &state, &["issue", "new", "Sq", "--project", "demo"]).0);
    assert!(cli(&pm, &state, &["issue", "start", "D-1"]).0);
    let wt = repo.join(".cadence/wt/d-1-sq");
    // Newline-terminated, multi-line, one file edited twice — the
    // shape of real code, whose diff ends in a newline.
    std::fs::write(wt.join("a.txt"), "1\n").unwrap();
    git(&wt, &["add", "-A"]);
    git(&wt, &["commit", "-qm", "part a"]);
    std::fs::write(wt.join("a.txt"), "1\n2\n").unwrap();
    std::fs::write(wt.join("b.txt"), "b1\nb2\n").unwrap();
    git(&wt, &["add", "-A"]);
    git(&wt, &["commit", "-qm", "part b"]);
    git(&repo, &["merge", "--squash", "-q", "cadence/d-1-sq"]);
    git(&repo, &["commit", "-qm", "D-1: sq (#1)"]);
    idle(&wt);
    strip_owner(&pm, "D-1");
    let (ok, out) = cli(&pm, &state, &["issue", "finish", "D-1"]);
    assert!(ok, "{out}");
    assert_eq!(out["finished"], true);
    assert_eq!(out["merged_by"], "patch", "{out}");
    assert_eq!(out["overrode"], json!([]));
    assert!(!wt.exists());

    // D-2: the single branch commit cherry-picked onto main →
    // "cherry" (patch-equivalent, different sha).
    assert!(cli(&pm, &state, &["issue", "new", "Ch", "--project", "demo"]).0);
    assert!(cli(&pm, &state, &["issue", "start", "D-2"]).0);
    let wt = repo.join(".cadence/wt/d-2-ch");
    std::fs::write(wt.join("c.txt"), "c1\nc2\n").unwrap();
    git(&wt, &["add", "-A"]);
    git(&wt, &["commit", "-qm", "cherry work"]);
    // -x records the source sha in the message — a different commit
    // object with the same patch-id, like a real cherry-pick merge.
    git(&repo, &["cherry-pick", "-x", "cadence/d-2-ch"]);
    idle(&wt);
    strip_owner(&pm, "D-2");
    let (ok, out) = cli(&pm, &state, &["issue", "finish", "D-2"]);
    assert!(ok && out["merged_by"] == "cherry", "{out}");

    // D-3: squash-merged, then an extra commit only on the branch —
    // the work isn't all upstream, so finish still refuses.
    assert!(cli(&pm, &state, &["issue", "new", "Ex", "--project", "demo"]).0);
    assert!(cli(&pm, &state, &["issue", "start", "D-3"]).0);
    let wt = repo.join(".cadence/wt/d-3-ex");
    std::fs::write(wt.join("d.txt"), "d1\nd2\n").unwrap();
    git(&wt, &["add", "-A"]);
    git(&wt, &["commit", "-qm", "part d"]);
    git(&repo, &["merge", "--squash", "-q", "cadence/d-3-ex"]);
    git(&repo, &["commit", "-qm", "D-3 part (#3)"]);
    std::fs::write(wt.join("late.txt"), "late\n").unwrap();
    git(&wt, &["add", "-A"]);
    git(&wt, &["commit", "-qm", "unmerged late work"]);
    idle(&wt);
    strip_owner(&pm, "D-3");
    let (ok, err) = cli(&pm, &state, &["issue", "finish", "D-3"]);
    assert!(!ok, "{err}");
    assert!(
        err["error"].as_str().unwrap().contains("neither merged"),
        "{err}"
    );
    assert!(wt.is_dir());
    git(&repo, &["branch", "-D", "cadence/d-3-ex"]);
    let wts = wt.display().to_string();
    git(&repo, &["worktree", "remove", "--force", &wts]);

    // D-4: the ui/node_modules build symlink alone is ignored (`!!`)
    // and never blocks — the branch was fast-forwarded into main, so
    // "ancestry". (An unstarted lane is never merged — CAD-275 — so it
    // lands one commit first.)
    assert!(cli(&pm, &state, &["issue", "new", "Sy", "--project", "demo"]).0);
    assert!(cli(&pm, &state, &["issue", "start", "D-4"]).0);
    let wt = repo.join(".cadence/wt/d-4-sy");
    land(&repo, &wt, "sy.txt");
    std::fs::create_dir_all(wt.join("ui")).unwrap();
    std::fs::create_dir_all(wt.join("real_nm")).unwrap();
    std::os::unix::fs::symlink("../real_nm", wt.join("ui/node_modules")).unwrap();
    idle(&wt);
    let status = git(&wt, &["status", "--porcelain", "--ignored"]).1;
    assert!(status.contains("!!"), "{status}");
    strip_owner(&pm, "D-4");
    let (ok, out) = cli(&pm, &state, &["issue", "finish", "D-4"]);
    assert!(ok && out["finished"] == true, "{out}");
    assert_eq!(out["merged_by"], "ancestry");
    assert!(!wt.exists());

    // D-5: a real untracked file still refuses and is listed — while
    // the ignored symlink alongside it is not.
    assert!(cli(&pm, &state, &["issue", "new", "Re", "--project", "demo"]).0);
    assert!(cli(&pm, &state, &["issue", "start", "D-5"]).0);
    let wt = repo.join(".cadence/wt/d-5-re");
    land(&repo, &wt, "re.txt");
    std::fs::create_dir_all(wt.join("ui")).unwrap();
    std::fs::create_dir_all(wt.join("real_nm")).unwrap();
    std::os::unix::fs::symlink("../real_nm", wt.join("ui/node_modules")).unwrap();
    std::fs::write(wt.join("real.txt"), "x").unwrap();
    strip_owner(&pm, "D-5");
    let (ok, err) = cli(&pm, &state, &["issue", "finish", "D-5"]);
    assert!(!ok, "{err}");
    let msg = err["error"].as_str().unwrap();
    assert!(msg.contains("real.txt"), "{msg}");
    assert!(
        !msg.contains("node_modules") && !msg.contains("!!"),
        "{msg}"
    );
    assert!(wt.is_dir());
    std::fs::remove_file(wt.join("real.txt")).unwrap();
    idle(&wt);
    let (ok, out) = cli(&pm, &state, &["issue", "finish", "D-5"]);
    assert!(ok && out["finished"] == true, "{out}");

    // D-6 / D-7: two-commit squashes of a binary file and of a file
    // with no trailing newline — both diff shapes still apply → "patch".
    for (id, name, first, second) in [
        (
            "D-6",
            "Bi",
            &b"\x00\x01\xff\n\x00"[..],
            &b"\x00\x02\xfe\x00"[..],
        ),
        ("D-7", "Nn", &b"x\ny"[..], &b"x\ny\nz"[..]),
    ] {
        assert!(cli(&pm, &state, &["issue", "new", name, "--project", "demo"]).0);
        assert!(cli(&pm, &state, &["issue", "start", id]).0);
        let branch = format!("cadence/{}-{}", id.to_lowercase(), name.to_lowercase());
        let wt = repo.join(format!(
            ".cadence/wt/{}-{}",
            id.to_lowercase(),
            name.to_lowercase()
        ));
        std::fs::write(wt.join("f.bin"), first).unwrap();
        git(&wt, &["add", "-A"]);
        git(&wt, &["commit", "-qm", "first"]);
        std::fs::write(wt.join("f.bin"), second).unwrap();
        git(&wt, &["add", "-A"]);
        git(&wt, &["commit", "-qm", "second"]);
        git(&repo, &["merge", "--squash", "-q", &branch]);
        git(&repo, &["commit", "-qm", &format!("{id}: squash")]);
        idle(&wt);
        strip_owner(&pm, id);
        let (ok, out) = cli(&pm, &state, &["issue", "finish", id]);
        assert!(ok, "{id}: {out}");
        assert_eq!(out["merged_by"], "patch", "{id}: {out}");
        assert!(!wt.exists(), "{id}");
    }
}

/// CAD-64: with a GitHub origin and `gh` on PATH, a merged PR with
/// the branch as head counts as merged (`merged_by: "pr"`); a `gh`
/// that reports nothing merged leaves the branch refused.
#[test]
fn issue_finish_pr_merge_via_gh() {
    let (_tmp, pm, state, repo) = start_fx();
    let _d = UiDaemon::start_on(state.clone());
    git(
        &repo,
        &["remote", "add", "origin", "https://github.com/o/r.git"],
    );
    // Fake gh — controlled JSON on stdout, ignores its arguments.
    let fakebin = _tmp.path().join("fakebin");
    std::fs::create_dir_all(&fakebin).unwrap();
    let gh = fakebin.join("gh");
    let set_gh = |body: &str| {
        std::fs::write(&gh, format!("#!/bin/sh\nprintf '%s' '{body}'\n")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    };
    let path = format!(
        "{}:{}:{}",
        fakebin.display(),
        Path::new(bin()).parent().unwrap().display(),
        std::env::var("PATH").unwrap_or_default()
    );

    // D-1: branch commits not on main, nothing pushed — but gh says
    // a PR whose recorded head IS this tip is MERGED → finished,
    // merged_by "pr". A bare name match proves nothing (CAD-106):
    // first answer with a head oid that does not cover the tip.
    assert!(cli(&pm, &state, &["issue", "new", "Pr", "--project", "demo"]).0);
    assert!(cli(&pm, &state, &["issue", "start", "D-1"]).0);
    let wt = repo.join(".cadence/wt/d-1-pr");
    std::fs::write(wt.join("p.txt"), "p").unwrap();
    git(&wt, &["add", "-A"]);
    git(&wt, &["commit", "-qm", "pr work"]);
    idle(&wt);
    let tip = git(&repo, &["rev-parse", "cadence/d-1-pr"]).1;
    strip_owner(&pm, "D-1");
    set_gh("[{\"number\":7,\"headRefOid\":\"0000000000000000000000000000000000000000\",\"baseRefName\":\"main\"}]");
    let (ok, err) = cli_env(
        &pm,
        &state,
        &["issue", "finish", "D-1"],
        &[("PATH", path.as_str())],
    );
    assert!(!ok, "{err}");
    assert!(
        err["error"].as_str().unwrap().contains("neither merged"),
        "a stale headRefOid must not prove the merge: {err}"
    );
    assert!(wt.is_dir());
    set_gh(&format!(
        "[{{\"number\":7,\"headRefOid\":\"{tip}\",\"baseRefName\":\"main\"}}]"
    ));
    let (ok, out) = cli_env(
        &pm,
        &state,
        &["issue", "finish", "D-1"],
        &[("PATH", path.as_str())],
    );
    assert!(ok, "{out}");
    assert_eq!(out["merged_by"], "pr", "{out}");
    assert!(!wt.exists());

    // D-2: gh reports nothing merged → the branch is still refused.
    assert!(cli(&pm, &state, &["issue", "new", "No", "--project", "demo"]).0);
    assert!(cli(&pm, &state, &["issue", "start", "D-2"]).0);
    let wt = repo.join(".cadence/wt/d-2-no");
    std::fs::write(wt.join("n.txt"), "n").unwrap();
    git(&wt, &["add", "-A"]);
    git(&wt, &["commit", "-qm", "unmerged"]);
    idle(&wt);
    strip_owner(&pm, "D-2");
    set_gh("[]");
    let (ok, err) = cli_env(
        &pm,
        &state,
        &["issue", "finish", "D-2"],
        &[("PATH", path.as_str())],
    );
    assert!(!ok, "{err}");
    assert!(
        err["error"].as_str().unwrap().contains("neither merged"),
        "{err}"
    );
    assert!(wt.is_dir());
}

// ---- CAD-274: one issue, one open lane ----

/// The refs of `id` from `issue show --json`.
fn refs_of(pm: &Path, state: &Path, id: &str) -> Vec<Value> {
    let (ok, show) = cli(pm, state, &["issue", "show", id, "--json"]);
    assert!(ok, "{show}");
    show["refs"].as_array().cloned().unwrap_or_default()
}

/// The `cadence/*` branches in `repo`.
fn lane_branches(repo: &Path) -> Vec<String> {
    git(
        repo,
        &[
            "for-each-ref",
            "--format=%(refname:short)",
            "refs/heads/cadence/",
        ],
    )
    .1
    .lines()
    .map(str::to_string)
    .collect()
}

/// CAD-274: with exactly one open worktree ref, `issue start` reuses
/// that lane whether or not `--name` is given — even after the title
/// (and so the default slug) changed, the CAD-270 repro. It re-applies
/// the cargo target, fixes a stale recorded one in one commit, and
/// mints no worktree, branch or ref. A `--name` for a different slug
/// is refused, naming the open lane.
#[test]
fn issue_start_reuses_the_one_open_lane() {
    let (_tmp, pm, state, repo) = start_fx();
    assert!(
        cli(
            &pm,
            &state,
            &["issue", "new", "Skill Kickoff Lessons", "--project", "demo"]
        )
        .0
    );
    let (ok, out) = cli(&pm, &state, &["issue", "start", "D-1"]);
    assert!(ok, "{out}");
    let wt = out["worktree"].as_str().unwrap().to_string();
    assert!(wt.ends_with("d-1-skill-kickoff-lessons"), "{out}");
    assert!(
        cli(
            &pm,
            &state,
            &[
                "issue",
                "set",
                "D-1",
                "title=Cadence skill kickoff checklist"
            ]
        )
        .0
    );
    // A stale recorded cargo target — the re-start's job is to fix it.
    let md = pm.join("demo/D-1/issue.md");
    let front = std::fs::read_to_string(&md).unwrap();
    let planted = front.replace(
        &format!("path: {wt}\n"),
        &format!("path: {wt}\n  cargo_target: /stale/target\n"),
    );
    assert_ne!(planted, front, "fixture: {front}");
    std::fs::write(&md, planted).unwrap();
    assert!(
        git(
            &pm,
            &[
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "-qam",
                "stale target"
            ]
        )
        .0
    );
    let refs_before = refs_of(&pm, &state, "D-1").len();

    let before = commits(&pm);
    let (ok, again) = cli(&pm, &state, &["issue", "start", "D-1"]);
    assert!(ok, "{again}");
    assert_eq!(again["created"], false, "{again}");
    assert_eq!(again["worktree"], out["worktree"], "{again}");
    assert_eq!(again["branch"], out["branch"], "{again}");
    assert_eq!(commits(&pm), before + 1, "one commit fixes the target");
    let (_, body) = git(&pm, &["log", "-1", "--format=%B"]);
    assert!(body.contains("(refs refreshed)"), "{body}");
    let refs = refs_of(&pm, &state, "D-1");
    assert_eq!(refs.len(), refs_before, "no new ref: {refs:?}");
    assert!(
        refs.iter().all(|r| r["cargo_target"].is_null()),
        "the stale target is corrected (not a cargo repo): {refs:?}"
    );
    assert_eq!(
        lane_branches(&repo),
        vec!["cadence/d-1-skill-kickoff-lessons"]
    );
    let lanes = std::fs::read_dir(repo.join(".cadence/wt")).unwrap().count();
    assert_eq!(lanes, 1, "no second worktree");

    // Idempotent now; `--name` naming the open slug reuses it too.
    let before = commits(&pm);
    for args in [
        &["issue", "start", "D-1"][..],
        &["issue", "start", "D-1", "--name", "skill-kickoff-lessons"][..],
    ] {
        let (ok, out) = cli(&pm, &state, args);
        assert!(ok && out["created"] == false, "{args:?}: {out}");
        assert_eq!(out["worktree"].as_str().unwrap(), wt, "{args:?}");
    }
    assert_eq!(commits(&pm), before);

    // A different --name would fork the work — refused, nothing made.
    let (ok, err) = cli(&pm, &state, &["issue", "start", "D-1", "--name", "other"]);
    assert!(!ok, "{err}");
    let msg = err["error"].as_str().unwrap();
    assert!(
        msg.contains("'skill-kickoff-lessons'") && msg.contains(&wt) && msg.contains("--worktree"),
        "{msg}"
    );
    assert!(!repo.join(".cadence/wt/d-1-other").exists());
    assert_eq!(
        lane_branches(&repo),
        vec!["cadence/d-1-skill-kickoff-lessons"]
    );
    assert_eq!(commits(&pm), before);
}

/// CAD-274: two or more open worktree refs make `issue start` and a
/// bare `issue finish` refuse, listing them; `issue finish <ID>
/// --worktree <path>` closes exactly one — for a lane whose dir (and
/// branch) are already gone it only marks the refs closed, in one
/// tracker commit, keeping any surviving branch. The remaining lane
/// is then reused by `issue start`.
#[test]
fn issue_several_open_lanes_refuse_and_finish_one_by_path() {
    let (_tmp, pm, state, repo) = start_fx();
    assert!(cli(&pm, &state, &["issue", "new", "Lanes", "--project", "demo"]).0);
    let (ok, out) = cli(&pm, &state, &["issue", "start", "D-1"]);
    assert!(ok, "{out}");
    let live = out["worktree"].as_str().unwrap().to_string();
    // What the pre-CAD-274 start left behind: an extra pair whose dir
    // and branch were removed by hand, and one whose dir is gone but
    // whose branch still holds unmerged work.
    let ghost = repo.join(".cadence/wt/d-1-ghost");
    let ghost_s = ghost.to_str().unwrap().to_string();
    let stale = repo.join(".cadence/wt/d-1-stale");
    let stale_s = stale.to_str().unwrap().to_string();
    assert!(
        git(
            &repo,
            &["worktree", "add", "-q", "-b", "cadence/d-1-stale", &stale_s]
        )
        .0
    );
    std::fs::write(stale.join("s.txt"), "s\n").unwrap();
    assert!(git(&stale, &["add", "-A"]).0);
    assert!(git(&stale, &["commit", "-qm", "stale work"]).0);
    std::fs::remove_dir_all(&stale).unwrap();
    assert!(git(&repo, &["worktree", "prune"]).0);
    for (kind, target) in [
        ("branch", "cadence/d-1-ghost"),
        ("worktree", ghost_s.as_str()),
        ("branch", "cadence/d-1-stale"),
        ("worktree", stale_s.as_str()),
    ] {
        let (ok, out) = cli(&pm, &state, &["issue", "ref", "D-1", kind, target]);
        assert!(ok, "{out}");
    }

    let before = commits(&pm);
    for args in [
        &["issue", "start", "D-1"][..],
        &["issue", "finish", "D-1"][..],
    ] {
        let (ok, err) = cli(&pm, &state, args);
        assert!(!ok, "{args:?}: {err}");
        let msg = err["error"].as_str().unwrap();
        assert!(
            msg.contains("3 open worktree refs")
                && msg.contains(&live)
                && msg.contains(&ghost_s)
                && msg.contains(&stale_s)
                && msg.contains("--worktree"),
            "{args:?}: {msg}"
        );
    }
    assert_eq!(commits(&pm), before);

    // Dir and branch both gone: the refs close, nothing else moves.
    let (ok, out) = cli(
        &pm,
        &state,
        &["issue", "finish", "D-1", "--worktree", &ghost_s],
    );
    assert!(ok, "{out}");
    assert!(
        out["finished"] == true
            && out["refs_only"] == true
            && out["removed_worktree"] == false
            && out["deleted_branch"] == false
            && out["overrode"] == json!([]),
        "{out}"
    );
    assert_eq!(commits(&pm), before + 1, "one tracker commit");
    let (_, body) = git(&pm, &["log", "-1", "--format=%B"]);
    assert!(body.contains("D-1: finish cadence/d-1-ghost"), "{body}");
    // Dir gone, branch alive with unmerged work: refs close, the
    // branch is kept — nothing is deleted, so nothing is lost.
    let (ok, out) = cli(
        &pm,
        &state,
        &["issue", "finish", "D-1", "--worktree", &stale_s],
    );
    assert!(ok, "{out}");
    assert!(
        out["finished"] == true && out["deleted_branch"] == false,
        "{out}"
    );
    assert!(
        out["branch_note"]
            .as_str()
            .unwrap_or_default()
            .contains("refs closed only"),
        "{out}"
    );
    assert!(
        git(
            &repo,
            &["rev-parse", "--verify", "--quiet", "cadence/d-1-stale"]
        )
        .0
    );
    assert_eq!(commits(&pm), before + 2);
    let refs = refs_of(&pm, &state, "D-1");
    for r in &refs {
        let path = r["path"].as_str().unwrap_or_default();
        let closed = r["closed"] == true;
        let extra = path.contains("d-1-ghost") || path.contains("d-1-stale");
        assert_eq!(closed, extra, "{path}: {refs:?}");
    }
    assert!(Path::new(&live).is_dir(), "the live lane is untouched");

    // Finishing a closed lane again is the idempotent no-op; a path
    // the issue never recorded refuses.
    let (ok, out) = cli(
        &pm,
        &state,
        &["issue", "finish", "D-1", "--worktree", &ghost_s],
    );
    assert!(ok && out["finished"] == false, "{out}");
    let (ok, err) = cli(
        &pm,
        &state,
        &["issue", "finish", "D-1", "--worktree", "/nope/wt"],
    );
    assert!(!ok, "{err}");
    assert!(
        err["error"]
            .as_str()
            .unwrap()
            .contains("records no open worktree"),
        "{err}"
    );

    // One open lane left — start reuses it.
    let (ok, out) = cli(&pm, &state, &["issue", "start", "D-1"]);
    assert!(ok && out["created"] == false, "{out}");
    assert_eq!(out["worktree"].as_str().unwrap(), live);
}

// ---- CAD-275: an unstarted or active lane is never finished as merged ----

/// A fresh `issue start` lane with no commits is `not started`, never
/// merged: the `--merged` sweep skips it (dry run and real, the text
/// output naming the reason), an explicit finish refuses — first as
/// recently active (the checkout itself is fresh), then, once idle, as
/// not started — and `--force` finishes an abandoned lane, recorded.
#[test]
fn issue_finish_unstarted_lane_is_never_merged() {
    let (_tmp, pm, state, repo) = start_fx();
    assert!(cli(&pm, &state, &["issue", "new", "Fresh", "--project", "demo"]).0);
    assert!(cli(&pm, &state, &["issue", "start", "D-1"]).0);
    let wt = repo.join(".cadence/wt/d-1-fresh");

    let (ok, plan) = cli(
        &pm,
        &state,
        &["issue", "finish", "--merged", "--dry-run", "--json"],
    );
    assert!(ok, "{plan}");
    let row = &plan["rows"][0];
    assert!(
        row["outcome"] == "skipped" && row["reason"] == "not started" && row["merged_by"].is_null(),
        "{plan}"
    );
    let (ok, text) = cli_raw(&pm, &state, &["issue", "finish", "--merged", "--dry-run"]);
    assert!(ok, "{text}");
    assert!(text.contains("D-1: skipped(not started)"), "{text}");

    let before = commits(&pm);
    let (ok, err) = cli(&pm, &state, &["issue", "finish", "D-1"]);
    assert!(!ok, "{err}");
    let msg = err["error"].as_str().unwrap();
    assert!(
        msg.contains("was modified") && msg.contains("s ago (") && msg.contains("idle 30m"),
        "a fresh checkout is recent activity, named: {msg}"
    );
    idle(&wt);
    let (ok, err) = cli(&pm, &state, &["issue", "finish", "D-1"]);
    assert!(!ok, "{err}");
    assert!(
        err["error"].as_str().unwrap().contains("has not started"),
        "{err}"
    );
    let (ok, out) = cli(&pm, &state, &["issue", "finish", "--merged", "--json"]);
    assert!(ok, "{out}");
    assert_eq!(out["rows"][0]["reason"], "not started", "{out}");
    assert!(wt.is_dir());
    assert!(
        git(
            &repo,
            &["rev-parse", "--verify", "--quiet", "cadence/d-1-fresh"]
        )
        .0
    );
    assert_eq!(commits(&pm), before, "nothing finished");

    let (ok, out) = cli(&pm, &state, &["issue", "finish", "D-1", "--force"]);
    assert!(ok, "{out}");
    assert!(
        out["finished"] == true
            && out["not_started"] == true
            && out["merged_by"].is_null()
            && out["deleted_branch"] == true,
        "{out}"
    );
    assert!(
        out["overrode"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o == "not-started"),
        "{out}"
    );
    assert!(!wt.exists());
}

/// CAD-275: a merged, clean lane with a file modified inside the
/// active window is refused as in use — the refusal and the dry-run
/// row name the file and its age — and `--force` overrides, recorded.
#[test]
fn issue_finish_refuses_recent_activity() {
    let (_tmp, pm, state, repo) = start_fx();
    assert!(cli(&pm, &state, &["issue", "new", "Busy", "--project", "demo"]).0);
    assert!(cli(&pm, &state, &["issue", "start", "D-1"]).0);
    let wt = repo.join(".cadence/wt/d-1-busy");
    land(&repo, &wt, "b.txt");
    // Touched, not changed: the tree stays clean.
    assert!(Command::new("touch")
        .arg(wt.join("f"))
        .status()
        .unwrap()
        .success());
    assert_eq!(git(&wt, &["status", "--porcelain"]).1, "");

    let (ok, text) = cli_raw(&pm, &state, &["issue", "finish", "--merged", "--dry-run"]);
    assert!(!ok, "a refused row exits 1: {text}");
    assert!(
        text.contains("D-1: refused(Worktree")
            && text.contains("was modified")
            && text.contains("(f)"),
        "{text}"
    );
    let (ok, err) = cli(&pm, &state, &["issue", "finish", "D-1"]);
    assert!(!ok, "{err}");
    let msg = err["error"].as_str().unwrap();
    assert!(msg.contains("(f)") && msg.contains("s ago"), "{msg}");
    assert!(wt.is_dir());

    let (ok, out) = cli(&pm, &state, &["issue", "finish", "D-1", "--force"]);
    assert!(ok, "{out}");
    assert!(
        out["merged_by"] == "ancestry" && out["not_started"] == false,
        "a landed lane is merged, not unstarted: {out}"
    );
    assert!(
        out["overrode"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o == "recent-activity"),
        "{out}"
    );
    let (_, body) = git(&pm, &["log", "-1", "--format=%B"]);
    assert!(body.contains("Forced: true"), "{body}");
}

/// Dispatch pre-flight refuses before anything is created: no return
/// address, an unreadable note, or an unreachable daemon each leave
/// no worktree, branch or tracker commit behind.
#[test]
fn dispatch_refusals_leave_nothing() {
    let (_tmp, pm, state, repo) = start_fx();
    assert!(cli(&pm, &state, &["issue", "new", "Disp", "--project", "demo"]).0);
    let before = commits(&pm);
    let wt = repo.join(".cadence/wt/d-1-disp");
    let note = _tmp.path().join("note.md");
    std::fs::write(&note, "# kickoff").unwrap();
    let note_s = note.to_str().unwrap().to_string();

    // No --reply-to and no CADENCE_ALIAS (the cli helper strips it).
    let (ok, err) = cli(
        &pm,
        &state,
        &["dispatch", "D-1", "--to", "w1", "--note", &note_s],
    );
    assert!(
        !ok && err["error"].as_str().unwrap().contains("return address"),
        "{err}"
    );

    // Unreadable note.
    let (ok, err) = cli(
        &pm,
        &state,
        &[
            "dispatch",
            "D-1",
            "--to",
            "w1",
            "--note",
            "/nonexistent/kickoff.md",
            "--reply-to",
            "pm",
        ],
    );
    assert!(
        !ok && err["error"].as_str().unwrap().contains("unreadable"),
        "{err}"
    );

    // Daemon down: the worker can't be verified — refused, nothing.
    let (ok, err) = cli(
        &pm,
        &state,
        &[
            "dispatch",
            "D-1",
            "--to",
            "w1",
            "--note",
            &note_s,
            "--reply-to",
            "pm",
        ],
    );
    assert!(!ok, "{err}");
    assert!(
        err["error"].as_str().unwrap().contains("not reachable"),
        "{err}"
    );

    assert!(!wt.exists());
    assert!(
        !git(
            &repo,
            &["rev-parse", "--verify", "--quiet", "cadence/d-1-disp"]
        )
        .0
    );
    assert_eq!(commits(&pm), before);
}

// ---------- CAD-383: claims guard dispatch and issue start ----------

fn show_issue(pm: &Path, state: &Path, id: &str) -> Value {
    let (ok, out) = cli(pm, state, &["issue", "show", id, "--json"]);
    assert!(ok, "{out}");
    out
}

fn has_comment(issue: &Value, needles: &[&str]) -> bool {
    issue["comments"].as_array().unwrap().iter().any(|c| {
        let body = c["body"].as_str().unwrap_or_default();
        needles.iter().all(|n| body.contains(n))
    })
}

/// A claim recorded outside cadence (`issue claim`) guards `issue
/// start` on a doing issue: a foreign requester is refused naming the
/// holder and the claim age, the holder is let through, a take-over
/// needs a reason and is recorded, and backlog/unowned issues start
/// exactly as before (an owned backlog issue only warns).
#[test]
fn issue_claim_guards_start_and_take_over_is_recorded() {
    let (_tmp, pm, state, repo) = start_fx();
    for title in ["Claimed", "Taken", "Fresh", "Soft"] {
        assert!(cli(&pm, &state, &["issue", "new", title, "--project", "demo"]).0);
    }

    // A PM whose lane runs outside cadence records a claim: owner,
    // claim and a comment in one tracker commit; backlog → doing.
    let before = commits(&pm);
    let (ok, out) = cli(
        &pm,
        &state,
        &[
            "issue",
            "claim",
            "D-1",
            "--by",
            "pm-a",
            "--note",
            "claude subagent lane",
        ],
    );
    assert!(ok, "{out}");
    assert_eq!(out["claim"]["by"], "pm-a", "{out}");
    assert_eq!(commits(&pm), before + 1);
    let d1 = show_issue(&pm, &state, "D-1");
    assert_eq!(d1["status"], "doing", "{d1}");
    // The claim is the PM's; `owner` stays for the lane.
    assert!(d1["owner"].is_null(), "{d1}");
    assert_eq!(d1["claim"]["by"], "pm-a", "{d1}");
    assert!(d1["claim"]["at"].as_str().is_some(), "{d1}");
    assert!(d1["claim"]["age_secs"].as_i64().is_some(), "{d1}");
    assert!(
        has_comment(&d1, &["Claimed by pm-a", "claude subagent lane"]),
        "{d1}"
    );
    // The card view (issue ls / board) carries the claim too.
    let (_, ls) = cli(&pm, &state, &["issue", "ls", "--json"]);
    let card = ls["issues"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == "D-1")
        .unwrap()
        .clone();
    assert_eq!(card["claim"]["by"], "pm-a", "{card}");
    // `overview` lists the in-flight claim with its age.
    let (ok, ov) = cli(&pm, &state, &["overview", "--json"]);
    assert!(ok, "{ov}");
    let claim = ov["projects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["key"] == "demo")
        .and_then(|p| p["claims"].as_array())
        .and_then(|cs| cs.iter().find(|c| c["issue"] == "D-1"))
        .unwrap_or_else(|| panic!("no D-1 claim row: {ov}"))
        .clone();
    assert_eq!(claim["by"], "pm-a", "{claim}");
    assert!(claim["age_secs"].as_i64().is_some(), "{claim}");

    // A foreign requester is refused before anything is created: the
    // refusal names the holder, the claim age and the take-over flag.
    let before = commits(&pm);
    for args in [
        &["issue", "start", "D-1"][..],
        &["issue", "start", "D-1", "--by", "pm-b"][..],
    ] {
        let (ok, err) = cli(&pm, &state, args);
        assert!(!ok, "{err}");
        let msg = err["error"].as_str().unwrap();
        assert!(
            msg.contains("pm-a") && msg.contains("ago") && msg.contains("--take-over"),
            "{msg}"
        );
    }
    assert!(!repo.join(".cadence/wt/d-1-claimed").exists());
    assert_eq!(commits(&pm), before);
    // Another requester's claim is refused the same way.
    let (ok, err) = cli(&pm, &state, &["issue", "claim", "D-1", "--by", "pm-b"]);
    assert!(
        !ok && err["error"].as_str().unwrap().contains("pm-a"),
        "{err}"
    );
    assert_eq!(commits(&pm), before);

    // The holder is let through — by CADENCE_ALIAS or --by — and the
    // claim is kept as it was.
    let (ok, out) = cli_env(
        &pm,
        &state,
        &["issue", "start", "D-1"],
        &[("CADENCE_ALIAS", "pm-a")],
    );
    assert!(ok, "{out}");
    assert_eq!(out["created"], true);
    let (ok, out) = cli(&pm, &state, &["issue", "start", "D-1", "--by", "pm-a"]);
    assert!(ok && out["created"] == false, "{out}");
    let d1 = show_issue(&pm, &state, "D-1");
    assert_eq!(d1["claim"]["by"], "pm-a", "{d1}");
    assert_eq!(d1["owner"], "pm-a", "{d1}");

    // Take-over: an empty reason is refused, a real one is recorded as
    // the new claim, a comment and a `claim` history entry.
    assert!(cli(&pm, &state, &["issue", "claim", "D-2", "--by", "pm-a"]).0);
    let before = commits(&pm);
    let (ok, err) = cli(
        &pm,
        &state,
        &["issue", "start", "D-2", "--by", "pm-b", "--take-over", "  "],
    );
    assert!(
        !ok && err["error"].as_str().unwrap().contains("reason"),
        "{err}"
    );
    assert_eq!(commits(&pm), before);
    let (ok, out) = cli(
        &pm,
        &state,
        &[
            "issue",
            "start",
            "D-2",
            "--by",
            "pm-b",
            "--take-over",
            "pm-a lane died at 09:00",
        ],
    );
    assert!(ok, "{out}");
    assert_eq!(out["claim"]["take_over"]["from"], "pm-a", "{out}");
    let d2 = show_issue(&pm, &state, "D-2");
    assert_eq!(d2["claim"]["by"], "pm-b", "{d2}");
    assert_eq!(d2["owner"], "pm-b", "{d2}");
    assert!(
        has_comment(
            &d2,
            &["Take-over by pm-b", "pm-a", "pm-a lane died at 09:00"]
        ),
        "{d2}"
    );
    let (ok, log) = cli(&pm, &state, &["issue", "log", "D-2"]);
    assert!(ok, "{log}");
    assert!(
        log["history"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["kind"] == "claim" && e["summary"].as_str().unwrap().contains("take-over")),
        "{log}"
    );

    // Backlog/unowned is unchanged: no warning, owner = the actor.
    let (ok, out) = cli(&pm, &state, &["issue", "start", "D-3"]);
    assert!(ok, "{out}");
    assert!(out["claim"]["warning"].is_null(), "{out}");
    let d3 = show_issue(&pm, &state, "D-3");
    assert_eq!(d3["owner"], "operator", "{d3}");
    assert_eq!(d3["claim"]["by"], "operator", "{d3}");
    // An owned backlog issue only warns, naming the owner, and keeps it.
    assert!(cli(&pm, &state, &["issue", "set", "D-4", "owner=bob"]).0);
    let (ok, out) = cli(&pm, &state, &["issue", "start", "D-4"]);
    assert!(ok, "{out}");
    assert!(
        out["claim"]["warning"].as_str().unwrap().contains("bob"),
        "{out}"
    );
    assert_eq!(show_issue(&pm, &state, "D-4")["owner"], "bob");

    // Release: only a holder may release; it clears the claim.
    let (ok, err) = cli(&pm, &state, &["issue", "release", "D-1", "--by", "pm-b"]);
    assert!(
        !ok && err["error"].as_str().unwrap().contains("pm-a"),
        "{err}"
    );
    let (ok, out) = cli(
        &pm,
        &state,
        &[
            "issue",
            "release",
            "D-1",
            "--by",
            "pm-a",
            "--note",
            "done here",
        ],
    );
    assert!(ok, "{out}");
    let d1 = show_issue(&pm, &state, "D-1");
    assert!(d1["claim"].is_null(), "{d1}");
    assert!(d1["owner"].is_null(), "{d1}");
    assert!(has_comment(&d1, &["Released by pm-a", "done here"]), "{d1}");
    // Unclaimed now: anyone may start it again.
    let (ok, out) = cli(&pm, &state, &["issue", "start", "D-1", "--by", "pm-b"]);
    assert!(ok, "{out}");
}

/// `dispatch` (plain and `--job`) reads the claim: re-dispatching to
/// the same worker and the claiming PM dispatching another worker go
/// through; a different PM is refused before anything is created
/// unless it takes over with a reason; a claim recorded with `issue
/// claim` by a PM whose lanes run outside cadence is seen too.
#[test]
fn dispatch_respects_claims() {
    let (tmp, pm, state, repo) = start_fx();
    // `dispatch_send` reads the tracker daemon-side (claim check +
    // lane resolution) — the daemon must see the same pm dir the
    // cli calls do.
    let d = UiDaemon::start_on_pm(state.clone(), &pm);
    let cwd = pm.to_str().unwrap();
    for (alias, upstream) in [
        ("pm-a", None),
        ("pm-b", None),
        ("w1", Some("pm-a")),
        ("w2", Some("pm-a")),
        ("w3", Some("pm-b")),
    ] {
        // Mailboxes: kickoffs stay queued, so counts are exact.
        let mut req = json!({"alias": alias, "provider": "inbox",
                             "endpoint_kind": "inbox", "cwd": cwd});
        if let Some(up) = upstream {
            req["params"] = json!(format!("{{\"upstream\":\"{up}\"}}"));
        }
        d.rpc("agent_register", req);
    }
    for title in ["Lane", "Outside"] {
        assert!(cli(&pm, &state, &["issue", "new", title, "--project", "demo"]).0);
    }
    let note = tmp.path().join("note.md");
    std::fs::write(&note, "# kickoff").unwrap();
    let note_s = note.to_str().unwrap().to_string();
    let spec = tmp.path().join("spec.md");
    std::fs::write(&spec, "# spec").unwrap();
    let spec_s = spec.to_str().unwrap().to_string();
    let dispatch = |id: &str, to: &str, by: &str, extra: &[&str]| {
        let mut args = vec![
            "dispatch",
            id,
            "--to",
            to,
            "--note",
            &note_s,
            "--reply-to",
            by,
        ];
        args.extend_from_slice(extra);
        cli_op(&pm, &state, &args)
    };
    let messages = |alias: &str| {
        d.rpc("agent_show", json!({"alias": alias}))["messages"]
            .as_array()
            .unwrap()
            .len()
    };

    // pm-a dispatches w1: the worker owns the lane, pm-a holds the claim.
    let (ok, out) = dispatch("D-1", "w1", "pm-a", &[]);
    assert!(ok && out["dispatched"] == true, "{out}");
    let d1 = show_issue(&pm, &state, "D-1");
    assert_eq!(d1["owner"], "w1", "{d1}");
    assert_eq!(d1["claim"]["by"], "pm-a", "{d1}");
    // `status` shows the claim, with its age, on the worker's row and
    // in the footer.
    let (ok, st) = cli(&pm, &state, &["status", "--json"]);
    assert!(ok, "{st}");
    let footer = st["footer"]["claims"].as_array().unwrap();
    let c = footer.iter().find(|c| c["issue"] == "D-1").unwrap();
    assert!(
        c["by"] == "pm-a" && c["owner"] == "w1" && c["age_secs"].as_i64().is_some(),
        "{c}"
    );
    let w1 = st["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["alias"] == "w1")
        .unwrap();
    assert!(
        w1["claims"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["issue"] == "D-1"),
        "{w1}"
    );
    // (a) re-dispatching to the same worker still works.
    let (ok, out) = dispatch("D-1", "w1", "pm-a", &[]);
    assert!(ok, "{out}");
    // (b) the claiming PM may hand the issue to another of its workers.
    let (ok, out) = dispatch("D-1", "w2", "pm-a", &[]);
    assert!(ok, "{out}");

    // (c) a different PM is refused, plain and --job, naming the holder
    // and the claim age — no commit, no message, no job.
    let before = commits(&pm);
    let (ok, err) = dispatch("D-1", "w3", "pm-b", &[]);
    assert!(!ok, "{err}");
    let msg = err["error"].as_str().unwrap();
    assert!(
        msg.contains("pm-a") && msg.contains("ago") && msg.contains("--take-over"),
        "{msg}"
    );
    let (ok, err) = dispatch("D-1", "w3", "pm-b", &["--job", "--spec", &spec_s]);
    assert!(
        !ok && err["error"].as_str().unwrap().contains("pm-a"),
        "{err}"
    );
    assert_eq!(commits(&pm), before);
    assert_eq!(messages("w3"), 0);
    let jobs = d.rpc("job_list", json!({"all": true}));
    assert_eq!(jobs["jobs"].as_array().unwrap().len(), 0, "{jobs}");

    // A claim recorded outside cadence is seen by dispatch.
    assert!(cli(&pm, &state, &["issue", "claim", "D-2", "--by", "pm-x"]).0);
    let (ok, err) = dispatch("D-2", "w1", "pm-a", &[]);
    assert!(
        !ok && err["error"].as_str().unwrap().contains("pm-x"),
        "{err}"
    );
    assert!(!repo.join(".cadence/wt/d-2-outside").exists());
    assert_eq!(messages("w1"), 1);

    // Take-over with a reason goes through and is recorded: the new
    // claim, the new owner and a comment naming the old holder.
    let (ok, out) = dispatch(
        "D-2",
        "w1",
        "pm-a",
        &["--take-over", "pm-x asked pm-a to finish it"],
    );
    assert!(ok && out["dispatched"] == true, "{out}");
    let d2 = show_issue(&pm, &state, "D-2");
    assert_eq!(d2["claim"]["by"], "pm-a", "{d2}");
    assert_eq!(d2["owner"], "w1", "{d2}");
    assert!(
        has_comment(
            &d2,
            &["Take-over by pm-a", "pm-x", "pm-x asked pm-a to finish it"]
        ),
        "{d2}"
    );
    assert_eq!(messages("w1"), 2);
}

// ---------- CAD-69: board over Tailscale ----------

const TS_DNS: &str = "node.tail1234.ts.net";

/// A fake `tailscale` first on PATH: `status` answers from
/// `status.json` (+ optional `status.rc`/`status.err`), `serve` keeps a
/// `key→target` map in `serve.map` and reports it as
/// `serve status --json`. Every argv lands in `calls.log`. The map is
/// keyed `<dns>:<port>` like the real `Web` object.
fn fake_ts() -> (TempDir, PathBuf) {
    let dir = TempDir::new().unwrap();
    let d = &dir.path().to_path_buf();
    std::fs::write(d.join("dns"), TS_DNS).unwrap();
    std::fs::write(d.join("calls.log"), "").unwrap();
    std::fs::write(
        d.join("status.json"),
        format!(
            r#"{{"BackendState":"Running","Self":{{"DNSName":"{TS_DNS}."}},"CertDomains":["{TS_DNS}"]}}"#
        ),
    )
    .unwrap();
    let script = r#"#!/usr/bin/env bash
d="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
echo "$*" >> "$d/calls.log"
case "${1:-}" in
status)
  [ -f "$d/status.err" ] && cat "$d/status.err" >&2
  [ -f "$d/status.json" ] && cat "$d/status.json"
  exit "$(cat "$d/status.rc" 2>/dev/null || echo 0)"
  ;;
serve)
  case "${2:-}" in
  status)
    printf '{"Web":{'
    first=1
    if [ -f "$d/serve.map" ]; then
      while IFS=$'\t' read -r key target; do
        [ -n "$key" ] || continue
        [ "$first" -eq 0 ] && printf ','
        first=0
        printf '"%s":{"Handlers":{"/":{"Proxy":"%s"}}}' "$key" "$target"
      done < "$d/serve.map"
    fi
    printf '}}'
    ;;
  --bg)
    port="${3#--https=}"
    printf '%s:%s\t%s\n' "$(cat "$d/dns")" "$port" "$4" >> "$d/serve.map"
    ;;
  --https=*)
    if [ "${3:-}" = "off" ]; then
      key="$(cat "$d/dns"):${2#--https=}"
      grep -v "^$key" "$d/serve.map" > "$d/.sm" 2>/dev/null || true
      mv "$d/.sm" "$d/serve.map"
    fi
    ;;
  esac
  ;;
esac
exit 0
"#;
    let path = d.join("tailscale");
    std::fs::write(&path, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    (dir, d.to_path_buf())
}

/// PATH with the fake `tailscale` dir first, then the built binary,
/// then ambient — `cli_env`'s env overrides its own PATH default.
fn ts_env(fake: &Path) -> [(&'static str, String); 1] {
    [(
        "PATH",
        format!(
            "{}:{}:{}",
            fake.display(),
            Path::new(bin()).parent().unwrap().display(),
            std::env::var("PATH").unwrap_or_default()
        ),
    )]
}

fn calls(fake: &Path) -> String {
    std::fs::read_to_string(fake.join("calls.log")).unwrap_or_default()
}

/// Stop a detached `cadence ui` for `state` — best effort, ignores
/// output. Tests that spawn the detached server hold this so a
/// panicking assert doesn't leak a setsid'd server.
struct DetachedUi(PathBuf);

impl Drop for DetachedUi {
    fn drop(&mut self) {
        let _ = Command::new(bin())
            .arg("--state-dir")
            .arg(&self.0)
            .args(["ui", "stop"])
            .env("CADENCE_PM_DIR", &self.0)
            .output();
    }
}

/// Write a `ui.json` carrying just the port — the "board not running"
/// path of `ui tailscale start` picks its target from it.
fn seed_ui_port(state: &Path, port: u16) {
    std::fs::write(state.join("ui.json"), format!(r#"{{"port":{port}}}"#)).unwrap();
}

fn ui_opts(state: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(state.join("ui.json")).unwrap()).unwrap()
}

#[test]
fn ui_tailscale_start_shares_and_persists() {
    let (pm, state) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    seed(pm.path(), state.path());
    let (_ts_dir, fake) = fake_ts();
    let env = ts_env(&fake);
    let port = free_port();
    seed_ui_port(state.path(), port);
    let _ui = DetachedUi(state.path().to_path_buf());

    let (ok, out) = cli_env(
        pm.path(),
        state.path(),
        &["ui", "tailscale", "start", "--port", "9450"],
        &env,
    );
    assert!(ok, "{out}");
    assert_eq!(out["state"], "sharing");
    assert_eq!(out["tailnet_url"], format!("https://{TS_DNS}:9450"));
    assert_eq!(out["board"], "started");
    assert_eq!(out["mapping_created"], true);

    // The mapping was created exactly once; funnel never invoked.
    let c = calls(&fake);
    assert_eq!(c.matches("serve --bg --https=9450").count(), 1, "{c}");
    assert!(!c.contains("funnel"), "{c}");

    // Options persisted: tailnet block + derived names resolvable.
    let o = ui_opts(state.path());
    assert_eq!(o["tailscale"]["dns_name"], TS_DNS);
    assert_eq!(o["tailscale"]["https_port"], 9450);
    assert_eq!(o["tailscale"]["target"], format!("http://127.0.0.1:{port}"));

    // `ui status` reports the tailnet URL and the running board.
    let (ok, out) = cli_env(pm.path(), state.path(), &["ui", "status"], &env);
    assert!(ok, "{out}");
    assert_eq!(out["state"], "running");
    assert_eq!(out["tailnet_url"], format!("https://{TS_DNS}:9450"));

    // `ui tailscale status` prints the URL, the live mapping, and the
    // identity probe: a local process posing as the proxy is ignored
    // (CAD-336) — the probe is exactly the forgery it must refuse.
    let (ok, text) = cli_raw_env(
        pm.path(),
        state.path(),
        &["ui", "tailscale", "status"],
        &env,
    );
    assert!(ok, "{text}");
    assert!(text.contains(&format!("https://{TS_DNS}:9450")), "{text}");
    assert!(text.contains("(live)"), "{text}");
    assert!(
        text.contains("identity: local forged login ignored (operator (ui); refused by check"),
        "{text}"
    );
    assert!(!text.contains("FORGEABLE"), "{text}");

    // Second start is idempotent: no new mapping, board restarted.
    let (ok, out) = cli_env(
        pm.path(),
        state.path(),
        &["ui", "tailscale", "start", "--port", "9450"],
        &env,
    );
    assert!(ok, "{out}");
    assert_eq!(out["mapping_created"], false);
    assert_eq!(out["board"], "restarted");
    assert_eq!(
        calls(&fake).matches("serve --bg").count(),
        1,
        "{}",
        calls(&fake)
    );
}

#[test]
fn ui_tailscale_start_while_board_runs_restarts() {
    let (pm, state) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    seed(pm.path(), state.path());
    let (_ts_dir, fake) = fake_ts();
    let env = ts_env(&fake);
    let _ui = DetachedUi(state.path().to_path_buf());

    // Plain `ui start --port` first — the board is already running
    // local-only when the operator shares it.
    let port = free_port();
    let (ok, out) = cli_env(
        pm.path(),
        state.path(),
        &["ui", "start", "--port", &port.to_string()],
        &env,
    );
    assert!(ok, "{out}");
    assert_eq!(out["state"], "started");

    let (ok, out) = cli_env(pm.path(), state.path(), &["ui", "tailscale", "start"], &env);
    assert!(ok, "{out}");
    assert_eq!(out["board"], "restarted");
    // Default https port is 9450; the target uses the persisted port.
    let o = ui_opts(state.path());
    assert_eq!(o["tailscale"]["https_port"], 9450);
    assert_eq!(o["tailscale"]["target"], format!("http://127.0.0.1:{port}"));
}

#[test]
fn ui_tailscale_conflicting_mapping_refused() {
    let (pm, state) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let (_ts_dir, fake) = fake_ts();
    let env = ts_env(&fake);
    // A foreign mapping already owns :9450.
    std::fs::write(
        fake.join("serve.map"),
        format!("{TS_DNS}:9450\thttp://127.0.0.1:9999\n"),
    )
    .unwrap();

    let (ok, err) = cli_env(pm.path(), state.path(), &["ui", "tailscale", "start"], &env);
    assert!(!ok, "{err}");
    let msg = err["error"].as_str().unwrap();
    assert!(msg.contains("refusing to overwrite"), "{msg}");
    assert!(msg.contains("http://127.0.0.1:9999"), "{msg}");
    // The foreign mapping is untouched; no ui.json was written.
    assert_eq!(
        std::fs::read_to_string(fake.join("serve.map")).unwrap(),
        format!("{TS_DNS}:9450\thttp://127.0.0.1:9999\n")
    );
    assert!(!state.path().join("ui.json").exists());
    // And no board got started.
    let (_, out) = cli_env(pm.path(), state.path(), &["ui", "status"], &env);
    assert_eq!(out["state"], "stopped");
}

#[test]
fn ui_tailscale_logged_out_refused() {
    let (pm, state) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let (_ts_dir, fake) = fake_ts();
    let env = ts_env(&fake);
    std::fs::write(fake.join("status.rc"), "1").unwrap();
    std::fs::write(fake.join("status.err"), "Logged out.\n").unwrap();
    std::fs::remove_file(fake.join("status.json")).unwrap();

    let (ok, err) = cli_env(pm.path(), state.path(), &["ui", "tailscale", "start"], &env);
    assert!(!ok, "{err}");
    assert!(
        err["error"].as_str().unwrap().contains("logged out"),
        "{err}"
    );
    assert!(!state.path().join("ui.json").exists());
}

#[test]
fn ui_tailscale_no_https_certs_refused() {
    let (pm, state) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let (_ts_dir, fake) = fake_ts();
    let env = ts_env(&fake);
    std::fs::write(
        fake.join("status.json"),
        format!(
            r#"{{"BackendState":"Running","Self":{{"DNSName":"{TS_DNS}."}},"CertDomains":[]}}"#
        ),
    )
    .unwrap();

    let (ok, err) = cli_env(pm.path(), state.path(), &["ui", "tailscale", "start"], &env);
    assert!(!ok, "{err}");
    assert!(
        err["error"]
            .as_str()
            .unwrap()
            .contains("HTTPS certificates"),
        "{err}"
    );
}

#[test]
fn ui_tailscale_not_running_refused() {
    let (pm, state) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let (_ts_dir, fake) = fake_ts();
    let env = ts_env(&fake);
    std::fs::write(
        fake.join("status.json"),
        r#"{"BackendState":"Stopped","Self":{"DNSName":"node.tail1234.ts.net."},"CertDomains":["node.tail1234.ts.net"]}"#,
    )
    .unwrap();

    let (ok, err) = cli_env(pm.path(), state.path(), &["ui", "tailscale", "start"], &env);
    assert!(!ok, "{err}");
    assert!(err["error"].as_str().unwrap().contains("not up"), "{err}");
}

#[test]
fn ui_start_tailscale_requires_loopback() {
    let (pm, state) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let (_ts_dir, fake) = fake_ts();
    let env = ts_env(&fake);

    // The refusal must precede any tailscale subprocess call.
    let (ok, err) = cli_env(
        pm.path(),
        state.path(),
        &["ui", "start", "--host", "0.0.0.0", "--tailscale"],
        &env,
    );
    assert!(!ok, "{err}");
    assert!(
        err["error"].as_str().unwrap().contains("not loopback"),
        "{err}"
    );
    assert!(
        calls(&fake).is_empty(),
        "tailscale never ran: {}",
        calls(&fake)
    );
}

#[test]
fn ui_start_tailscale_flag_alias() {
    let (pm, state) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    seed(pm.path(), state.path());
    let (_ts_dir, fake) = fake_ts();
    let env = ts_env(&fake);
    let _ui = DetachedUi(state.path().to_path_buf());
    let port = free_port();

    let (ok, out) = cli_env(
        pm.path(),
        state.path(),
        &[
            "ui",
            "start",
            "--port",
            &port.to_string(),
            "--tailscale",
            "--read-only",
        ],
        &env,
    );
    assert!(ok, "{out}");
    assert_eq!(out["tailnet_url"], format!("https://{TS_DNS}:9450"));
    let o = ui_opts(state.path());
    assert_eq!(o["tailscale"]["https_port"], 9450);
    assert_eq!(o["read_only"], true);
}

#[test]
fn ui_tailscale_survives_stop_start_cycle() {
    let (pm, state) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    seed(pm.path(), state.path());
    let (_ts_dir, fake) = fake_ts();
    let env = ts_env(&fake);
    let _ui = DetachedUi(state.path().to_path_buf());
    let port = free_port();
    seed_ui_port(state.path(), port);

    assert!(cli_env(pm.path(), state.path(), &["ui", "tailscale", "start"], &env).0);

    // `ui stop` leaves the mapping; `ui start` re-ensures (no new
    // --bg) and the board comes back shared.
    assert!(cli_env(pm.path(), state.path(), &["ui", "stop"], &env).0);
    let (ok, out) = cli_env(pm.path(), state.path(), &["ui", "start"], &env);
    assert!(ok, "{out}");
    assert_eq!(out["tailnet_url"], format!("https://{TS_DNS}:9450"));
    assert_eq!(
        calls(&fake).matches("serve --bg").count(),
        1,
        "{}",
        calls(&fake)
    );
    let o = ui_opts(state.path());
    assert_eq!(o["tailscale"]["https_port"], 9450);
}

#[test]
fn ui_stop_tailscale_off_removes_only_cadences_mapping() {
    let (pm, state) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    seed(pm.path(), state.path());
    let (_ts_dir, fake) = fake_ts();
    let env = ts_env(&fake);
    let _ui = DetachedUi(state.path().to_path_buf());
    let port = free_port();
    seed_ui_port(state.path(), port);
    assert!(cli_env(pm.path(), state.path(), &["ui", "tailscale", "start"], &env).0);
    // A foreign mapping on another port must survive --tailscale-off.
    std::fs::write(
        fake.join("serve.map"),
        format!(
            "{TS_DNS}:9450\thttp://127.0.0.1:{port}\nother.host.ts.net:8443\thttp://127.0.0.1:9999\n"
        ),
    )
    .unwrap();

    let (ok, out) = cli_env(
        pm.path(),
        state.path(),
        &["ui", "stop", "--tailscale-off"],
        &env,
    );
    assert!(ok, "{out}");
    let map = std::fs::read_to_string(fake.join("serve.map")).unwrap();
    assert!(!map.contains(":9450"), "{map}");
    assert!(map.contains("other.host.ts.net:8443"), "{map}");
    assert_eq!(ui_opts(state.path())["tailscale"], Value::Null);
    assert!(
        calls(&fake).contains("serve --https=9450 off"),
        "{}",
        calls(&fake)
    );
}

#[test]
fn ui_tailscale_stop_foreign_mapping_left_alone() {
    let (pm, state) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let (_ts_dir, fake) = fake_ts();
    let env = ts_env(&fake);
    let _ui = DetachedUi(state.path().to_path_buf());
    seed_ui_port(state.path(), free_port());
    assert!(cli_env(pm.path(), state.path(), &["ui", "tailscale", "start"], &env).0);
    // Someone else claimed :9450 after us — stop must not remove it.
    std::fs::write(
        fake.join("serve.map"),
        format!("{TS_DNS}:9450\thttp://127.0.0.1:9999\n"),
    )
    .unwrap();

    let (ok, out) = cli_env(pm.path(), state.path(), &["ui", "tailscale", "stop"], &env);
    assert!(ok, "{out}");
    assert_eq!(out["mapping_removed"], false);
    let map = std::fs::read_to_string(fake.join("serve.map")).unwrap();
    assert!(map.contains("http://127.0.0.1:9999"), "{map}");
    assert!(!calls(&fake).contains("off"), "{}", calls(&fake));
    assert_eq!(ui_opts(state.path())["tailscale"], Value::Null);
}

#[test]
fn ui_tailscale_stop_when_not_sharing() {
    let (pm, state) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let (_ts_dir, fake) = fake_ts();
    let env = ts_env(&fake);
    let (ok, out) = cli_env(pm.path(), state.path(), &["ui", "tailscale", "stop"], &env);
    assert!(ok, "{out}");
    assert_eq!(out["state"], "not_sharing");
}

// --- request-level: identity, origins, read-only ---

/// The headers a proxied tailnet write carries — the tailnet Host,
/// the https Origin, and Tailscale's identity headers.
fn ts_write_headers(origin: &str, login: &str) -> Vec<String> {
    vec![
        "Content-Type: application/json".to_string(),
        "X-Cadence-Board: 1".to_string(),
        "Sec-Fetch-Site: same-origin".to_string(),
        format!("Origin: {origin}"),
        format!("Tailscale-User-Login: {login}"),
        "Tailscale-User-Name: Some User".to_string(),
    ]
}

/// Tailscale sharing armed, with the tailnet proof reading tailscaled's
/// LocalAPI at `socket` (a [`fake_localapi`], or a path with nothing).
fn tailnet_opts(socket: &Path) -> impl Fn(&mut ui::ServeOpts) + Send + Sync + 'static {
    let socket = socket.to_path_buf();
    move |o| {
        o.tailnet = Some((TS_DNS.to_string(), 9450));
        o.allow_hosts = vec![TS_DNS.to_string(), format!("{TS_DNS}:9450")];
        o.allow_origins = vec![format!("https://{TS_DNS}:9450")];
        o.tailscaled_socket = Some(socket.clone());
    }
}

/// A fake tailscaled LocalAPI (CAD-336): a unix socket, owned by this
/// test's uid, answering `/localapi/v0/status` with `status.json`,
/// `/localapi/v0/prefs` with `prefs.json` and `/localapi/v0/serve-config`
/// with `serve.json` from its directory, read per request — a missing
/// file answers `500`. It starts with no operator user, so a board's
/// startup read ([`ui::serve`]'s operator latch) finds none. Under `/tmp`: a
/// long TMPDIR would overflow `sun_path`.
fn fake_localapi() -> (TempDir, PathBuf) {
    let dir = tempfile::Builder::new()
        .prefix("ts")
        .tempdir_in("/tmp")
        .unwrap();
    let sock = dir.path().join("ts.sock");
    localapi_operator(dir.path(), "");
    let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
    let root = dir.path().to_path_buf();
    thread::spawn(move || {
        for mut conn in listener.incoming().flatten() {
            // Read through the end of the request head.
            let mut raw = Vec::new();
            let mut buf = [0u8; 512];
            while !raw.windows(4).any(|w| w == b"\r\n\r\n") {
                match conn.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => raw.extend_from_slice(&buf[..n]),
                }
            }
            let req = String::from_utf8_lossy(&raw).to_string();
            let path = req.split_whitespace().nth(1).unwrap_or_default();
            let file = if path.starts_with("/localapi/v0/status") {
                Some("status.json")
            } else if path == "/localapi/v0/prefs" {
                Some("prefs.json")
            } else if path == "/localapi/v0/serve-config" {
                Some("serve.json")
            } else {
                None
            };
            let resp = match file.and_then(|f| std::fs::read(root.join(f)).ok()) {
                Some(body) => [
                    b"HTTP/1.0 200 OK\r\nContent-Type: application/json\r\n\r\n".as_slice(),
                    &body,
                ]
                .concat(),
                None => b"HTTP/1.0 500 Internal Server Error\r\n\r\n".to_vec(),
            };
            let _ = conn.write_all(&resp);
        }
    });
    (dir, sock)
}

/// Write the fake LocalAPI's answers: `TUN` (None omits the field), no
/// operator user, and the serve config.
fn localapi_says(dir: &Path, tun: Option<bool>, serve: Value) {
    let status = match tun {
        Some(t) => json!({"TUN": t, "BackendState": "Running"}),
        None => json!({"BackendState": "Running"}),
    };
    std::fs::write(dir.join("status.json"), status.to_string()).unwrap();
    localapi_operator(dir, "");
    std::fs::write(dir.join("serve.json"), serve.to_string()).unwrap();
}

/// The fake LocalAPI's `prefs.OperatorUser` (`""` is none).
fn localapi_operator(dir: &Path, user: &str) {
    let prefs = json!({"OperatorUser": user, "WantRunning": true});
    std::fs::write(dir.join("prefs.json"), prefs.to_string()).unwrap();
}

/// This test process's user name — the board's user in board tests.
fn own_user_name() -> String {
    let out = Command::new("id").arg("-un").output().unwrap();
    assert!(out.status.success());
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// The serve config `ui tailscale start` makes: https:9450 proxied to
/// the board, plus an unrelated TCP forwarder (ssh).
fn serve_https_only(board_port: u16) -> Value {
    json!({
        "TCP": {"9450": {"HTTPS": true}, "2222": {"TCPForward": "127.0.0.1:22"}},
        "Web": {format!("{TS_DNS}:9450"): {"Handlers": {"/": {"Proxy": format!("http://127.0.0.1:{board_port}")}}}}
    })
}

/// `/api/meta` for a tailnet-shaped request carrying a forged login:
/// `(actor, tailnet_proof)`.
fn tailnet_meta(port: u16) -> (String, Value) {
    let (code, _, body) = http_write(
        port,
        "GET",
        "/api/meta",
        &format!("{TS_DNS}:9450"),
        &["Tailscale-User-Login: mallory@evil.example"],
        b"",
    );
    assert_eq!(code, 200, "{body}");
    let meta: Value = serde_json::from_str(&body).unwrap();
    (
        meta["actor"].as_str().unwrap_or_default().to_string(),
        meta["tailnet_proof"].clone(),
    )
}

/// CAD-336: a local process sending exactly the proxy's shape —
/// tailnet Host, https Origin, identity headers, loopback peer — against
/// a tailscaled that passes every config check (kernel networking, no
/// TCP forwarder to the board) is still not the proxy: here the fake
/// tailscaled is this test's own uid, so the last check, `foreign_uid`,
/// refuses (on a real host the local caller fails `socket_owner`, which
/// `tailnet_proof`'s unit tests cover — a test cannot open a socket as
/// another uid). It writes as itself, `operator (ui)`, and the forged
/// login is never recorded.
#[test]
fn tailnet_shaped_local_write_is_not_the_proxy() {
    let (pm, state) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    seed(pm.path(), state.path());
    let (ts_dir, sock) = fake_localapi();
    let (port, _board) = start_ui_opts(
        pm.path().to_path_buf(),
        state.path().to_path_buf(),
        tailnet_opts(&sock),
    );
    localapi_says(ts_dir.path(), Some(true), serve_https_only(port));
    let ts_host = format!("{TS_DNS}:9450");
    let origin = format!("https://{TS_DNS}:9450");

    // CAD-313/CAD-428: an unproven tailnet request can use no session —
    // not even the operator's loopback one replayed onto it — so the
    // local caller is refused and nothing is written, under no name.
    let _d = UiDaemon::start_on(state.path().to_path_buf());
    let op = sign_in(state.path(), port);
    let before = commits(pm.path());
    let mut headers = ts_write_headers(&origin, "fable@example.com");
    for cookie in [None, Some(format!("Cookie: {}", op.cookie))] {
        headers.retain(|h| !h.starts_with("Cookie:") && !h.starts_with("X-Cadence-Session:"));
        if cookie.is_some() {
            headers.push(op.key_header());
        }
        headers.extend(cookie);
        let href: Vec<&str> = headers.iter().map(String::as_str).collect();
        let (code, _, body) = http_write(
            port,
            "PATCH",
            "/api/issues/CAD-2",
            &ts_host,
            &href,
            br#"{"status":"done"}"#,
        );
        assert_eq!(code, 403, "{body}");
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["check"], "operator_session_required", "{v}");
        assert!(!body.contains("fable"), "{body}");
    }
    assert_eq!(commits(pm.path()), before, "nothing is written");

    // /api/meta agrees, names the refusing check, and still reports
    // the tailnet URL.
    let (actor, proof) = tailnet_meta(port);
    assert_eq!(actor, "operator (ui)");
    assert_eq!(proof["proven"], false, "{proof}");
    assert_eq!(proof["check"], "foreign_uid", "{proof}");
    let (_, _, body) = http_write(port, "GET", "/api/meta", &ts_host, &[], b"");
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap()["tailnet_url"],
        origin
    );
}

/// CAD-336: each fail-closed check of the tailnet proof refuses on its
/// own condition and `/api/meta` names it — so a refusal can never pass
/// for a different reason (on a host without tailscaled every request
/// would otherwise fail at `tailscaled_socket`). One board per case: the
/// LocalAPI facts are cached per socket.
#[test]
fn tailnet_proof_refusals_name_their_check() {
    let (pm, state) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    seed(pm.path(), state.path());
    type Setup = fn(&Path, u16);
    let cases: [(&str, Setup); 9] = [
        ("localapi", |d, p| {
            // No TUN field: the status cannot be read as either mode.
            localapi_says(d, None, serve_https_only(p))
        }),
        ("localapi", |d, _| {
            // The serve config does not answer.
            localapi_says(d, Some(true), json!({}));
            std::fs::remove_file(d.join("serve.json")).unwrap();
        }),
        ("localapi", |d, p| {
            // The prefs do not answer: the operator user is unknown.
            localapi_says(d, Some(true), serve_https_only(p));
            std::fs::remove_file(d.join("prefs.json")).unwrap();
        }),
        ("kernel_networking", |d, p| {
            localapi_says(d, Some(false), serve_https_only(p))
        }),
        ("not_operator_user", |d, p| {
            // The board's user is tailscaled's operator: it could make
            // tailscaled dial the board at will (qa-1 round 2).
            localapi_says(d, Some(true), serve_https_only(p));
            localapi_operator(d, &own_user_name());
        }),
        ("not_operator_user", |d, p| {
            // An operator name that resolves to no user: fail closed.
            localapi_says(d, Some(true), serve_https_only(p));
            localapi_operator(d, "no-such-user-cad336");
        }),
        ("foreign_uid", |d, p| {
            // CAD-509: tailscaled omits an empty OperatorUser
            // (omitempty) — a prefs object without the field is "no
            // operator", so the rungs pass through to the fixture's
            // own-uid refusal.
            localapi_says(d, Some(true), serve_https_only(p));
            std::fs::write(d.join("prefs.json"), r#"{"WantRunning": true}"#).unwrap();
        }),
        ("no_tcp_forwarder", |d, p| {
            // qa-1's attack: `tailscale serve --tcp=N tcp://127.0.0.1:<board>`
            // in a foreground session — raw TCP passes forged headers.
            let mut serve = serve_https_only(p);
            serve["Foreground"] = json!({"sess1": {"TCP": {"7777": {
                "TCPForward": format!("127.0.0.1:{p}")
            }}}});
            localapi_says(d, Some(true), serve)
        }),
        ("foreign_uid", |d, p| {
            // Another user (root) is the operator: every config check
            // passes, and the fake tailscaled's own uid refuses last.
            localapi_says(d, Some(true), serve_https_only(p));
            localapi_operator(d, "root");
        }),
    ];
    for (want, setup) in cases {
        let (ts_dir, sock) = fake_localapi();
        let (port, _board) = start_ui_opts(
            pm.path().to_path_buf(),
            state.path().to_path_buf(),
            tailnet_opts(&sock),
        );
        setup(ts_dir.path(), port);
        let (actor, proof) = tailnet_meta(port);
        assert_eq!(actor, "operator (ui)", "{want}");
        assert_eq!(proof["proven"], false, "{want}: {proof}");
        assert_eq!(proof["check"], want, "{proof}");
    }

    // No LocalAPI socket at all: tailscaled's uid is unknown.
    let nowhere = TempDir::new().unwrap().path().join("absent.sock");
    let (port, _board) = start_ui_opts(
        pm.path().to_path_buf(),
        state.path().to_path_buf(),
        tailnet_opts(&nowhere),
    );
    let (actor, proof) = tailnet_meta(port);
    assert_eq!(actor, "operator (ui)");
    assert_eq!(proof["check"], "tailscaled_socket", "{proof}");
}

/// CAD-336 r4 (qa-1 round 3): the operator check latches for the
/// board process's life. A board whose user was tailscaled's operator
/// at startup stays refused after the operator is cleared — a
/// connection set up through a since-removed forwarder would outlive
/// the clear — and so does a board whose startup read failed.
#[test]
fn tailnet_operator_latch_outlives_a_clear() {
    let (pm, state) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    seed(pm.path(), state.path());

    // The board's user is the operator at startup, then clears itself.
    let (ts_dir, sock) = fake_localapi();
    localapi_operator(ts_dir.path(), &own_user_name());
    let (port, _board) = start_ui_opts(
        pm.path().to_path_buf(),
        state.path().to_path_buf(),
        tailnet_opts(&sock),
    );
    localapi_says(ts_dir.path(), Some(true), serve_https_only(port));
    let (actor, proof) = tailnet_meta(port);
    assert_eq!(actor, "operator (ui)");
    assert_eq!(proof["check"], "operator_latched", "{proof}");

    // The startup read fails (no prefs), then the LocalAPI recovers.
    let (ts_dir, sock) = fake_localapi();
    std::fs::remove_file(ts_dir.path().join("prefs.json")).unwrap();
    let (port, _board) = start_ui_opts(
        pm.path().to_path_buf(),
        state.path().to_path_buf(),
        tailnet_opts(&sock),
    );
    localapi_says(ts_dir.path(), Some(true), serve_https_only(port));
    let (actor, proof) = tailnet_meta(port);
    assert_eq!(actor, "operator (ui)");
    assert_eq!(proof["check"], "operator_latched", "{proof}");

    // Sighted as operator AFTER startup: latched from then on too.
    let (ts_dir, sock) = fake_localapi();
    let (port, _board) = start_ui_opts(
        pm.path().to_path_buf(),
        state.path().to_path_buf(),
        tailnet_opts(&sock),
    );
    localapi_says(ts_dir.path(), Some(true), serve_https_only(port));
    localapi_operator(ts_dir.path(), &own_user_name());
    assert_eq!(tailnet_meta(port).1["check"], "not_operator_user");
    localapi_operator(ts_dir.path(), "");
    // Past the LocalAPI cache: the fresh read shows no operator.
    thread::sleep(Duration::from_millis(2100));
    assert_eq!(tailnet_meta(port).1["check"], "operator_latched");
}

/// The TCP-forwarder attack end to end: a tailnet-shaped write through
/// a board whose serve config forwards raw TCP to it is not the proxy,
/// so it names nobody — and, holding no session, writes nothing.
#[test]
fn tailnet_write_with_a_tcp_forwarder_is_not_attributed() {
    let (pm, state) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    seed(pm.path(), state.path());
    let (ts_dir, sock) = fake_localapi();
    let (port, _board) = start_ui_opts(
        pm.path().to_path_buf(),
        state.path().to_path_buf(),
        tailnet_opts(&sock),
    );
    let mut serve = serve_https_only(port);
    serve["TCP"]["7777"] = json!({"TCPForward": format!("127.0.0.1:{port}")});
    localapi_says(ts_dir.path(), Some(true), serve);
    let origin = format!("https://{TS_DNS}:9450");
    let before = commits(pm.path());
    let headers = ts_write_headers(&origin, "mallory@evil.example");
    let href: Vec<&str> = headers.iter().map(String::as_str).collect();
    let (code, _, body) = http_write(
        port,
        "PATCH",
        "/api/issues/CAD-2",
        &format!("{TS_DNS}:9450"),
        &href,
        br#"{"status":"done"}"#,
    );
    // CAD-313: unproven, so no session and no operator — refused.
    assert_eq!(code, 403, "{body}");
    assert!(body.contains("operator_session_required"), "{body}");
    assert!(!body.contains("mallory"), "{body}");
    assert_eq!(commits(pm.path()), before, "nothing is written");
    assert_eq!(tailnet_meta(port).1["check"], "no_tcp_forwarder");
}

#[test]
fn forged_tailscale_headers_not_attributed() {
    let (pm, state) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    seed(pm.path(), state.path());
    let (port, _board) = start_ui_opts(
        pm.path().to_path_buf(),
        state.path().to_path_buf(),
        tailnet_opts(Path::new("/nonexistent/tailscaled.sock")),
    );
    let host = format!("127.0.0.1:{port}");

    // Same headers, but the Host is direct loopback — the identity
    // headers must be ignored (the proxy only sets them on the
    // tailnet name). Without a session that is a refusal (CAD-313);
    // with the operator's, `operator (ui)` — never the forged login.
    let headers = ts_write_headers(&format!("http://127.0.0.1:{port}"), "mallory@evil.example");
    let href: Vec<&str> = headers.iter().map(String::as_str).collect();
    let (code, _, body) = http_write(
        port,
        "PATCH",
        "/api/issues/CAD-2",
        &host,
        &href,
        br#"{"status":"done"}"#,
    );
    assert_eq!(code, 403, "{body}");
    assert!(body.contains("operator_session_required"), "{body}");
    let _d = UiDaemon::start_on(state.path().to_path_buf());
    let op = sign_in(state.path(), port);
    // On the board's own Host (where the session lives) with its Origin.
    let signed: Vec<&str> = href
        .iter()
        .copied()
        .filter(|h| !h.starts_with("Origin:"))
        .collect();
    let (code, _, _) = op_http_write(
        &op,
        port,
        "PATCH",
        "/api/issues/CAD-2",
        &host,
        &signed,
        br#"{"status":"done"}"#,
    );
    assert_eq!(code, 200);
    let sha = sha_of(pm.path(), "cadence/CAD-2", "status=done");
    let t = trailers_of(pm.path(), &sha);
    assert!(t.contains("Actor: operator (ui)"), "{t}");
    assert!(!t.contains("mallory"), "{t}");

    // And meta agrees: the forged header never resolves.
    let (code, _, body) = http_write(
        port,
        "GET",
        "/api/meta",
        &host,
        &["Tailscale-User-Login: mallory@evil.example"],
        b"",
    );
    assert_eq!(code, 200);
    let meta = serde_json::from_str::<Value>(&body).unwrap();
    assert_eq!(meta["actor"], "operator (ui)");
    // Not tailnet-shaped at all: no proof was even attempted.
    assert_eq!(meta["tailnet_proof"], Value::Null);
}

#[test]
fn tailnet_write_wrong_origin_refused() {
    let (pm, state) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    seed(pm.path(), state.path());
    let (port, _board) = start_ui_opts(
        pm.path().to_path_buf(),
        state.path().to_path_buf(),
        tailnet_opts(Path::new("/nonexistent/tailscaled.sock")),
    );
    // Tailnet Host but a foreign Origin — still refused.
    let headers = ts_write_headers("https://evil.example", "fable@example.com");
    let href: Vec<&str> = headers.iter().map(String::as_str).collect();
    let (code, _, body) = http_write(
        port,
        "PATCH",
        "/api/issues/CAD-2",
        &format!("{TS_DNS}:9450"),
        &href,
        br#"{"status":"done"}"#,
    );
    assert_eq!(code, 403);
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap()["check"],
        "origin"
    );
}

#[test]
fn read_only_board_refuses_every_write() {
    let (pm, state) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    seed(pm.path(), state.path());
    let (port, _board) = start_ui_opts(pm.path().to_path_buf(), state.path().to_path_buf(), |o| {
        o.read_only = true
    });
    let host = format!("127.0.0.1:{port}");
    let before = commits(pm.path());

    // Every write shape answers 403 with the read-only marker.
    for (method, path, body) in [
        (
            "POST",
            "/api/issues",
            r#"{"project":"cadence","title":"x"}"#,
        ),
        ("PATCH", "/api/issues/CAD-2", r#"{"status":"done"}"#),
        ("POST", "/api/issues/CAD-2/comments", r#"{"body":"hi"}"#),
        (
            "POST",
            "/api/issues/CAD-2/links",
            r#"{"kind":"relates","target":"CAD-1"}"#,
        ),
        (
            "POST",
            "/api/issues/CAD-2/refs",
            r#"{"kind":"commit","value":"abc"}"#,
        ),
        // Memory curation routes refuse identically — plain, and
        // carrying the accept-time body edit.
        ("POST", "/api/memories/cadence/foo/accept", r#"{}"#),
        (
            "POST",
            "/api/memories/cadence/foo/accept",
            r#"{"body":"x"}"#,
        ),
        ("POST", "/api/memories/cadence/foo/reject", r#"{}"#),
    ] {
        let (code, _, body) = write_json(port, method, path, &host, body);
        assert_eq!(code, 403, "{method} {path}");
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["check"], "read_only", "{method} {path}: {v}");
        assert!(v["error"].as_str().unwrap().contains("read-only"), "{v}");
    }
    // Artifact upload (octet-stream) too.
    let (code, _, body) = http_write(
        port,
        "POST",
        "/api/issues/CAD-2/artifacts?name=note.txt",
        &host,
        &[
            "Content-Type: application/octet-stream",
            "X-Cadence-Board: 1",
            "Sec-Fetch-Site: same-origin",
        ],
        b"data",
    );
    assert_eq!(code, 403);
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap()["check"],
        "read_only"
    );

    // Reads still work; meta reports the mode + local actor.
    let (code, body) = http(port, "GET", "/api/issues", &host);
    assert_eq!(code, 200, "{body}");
    let (code, body) = http(port, "GET", "/api/meta", &host);
    assert_eq!(code, 200);
    let meta: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(meta["read_only"], true);
    assert_eq!(meta["actor"], "operator (ui)");
    assert_eq!(meta["tailnet_url"], Value::Null);
    assert_eq!(commits(pm.path()), before, "no write landed");
}

/// A dumb TCP relay — what tailscaled is, once TLS is stripped: bytes
/// in, bytes out, no buffering policy. Proves the SSE stream survives
/// a proxy hop (frames flush per write, `: ping` keeps it alive).
fn tcp_relay(listen: u16, target: u16) {
    // Bound in the caller so the port is live before the test dials it.
    let listener = TcpListener::bind(("127.0.0.1", listen)).unwrap();
    thread::spawn(move || {
        for c in listener.incoming().flatten() {
            let Ok(u) = TcpStream::connect(("127.0.0.1", target)) else {
                continue;
            };
            let (mut c2, mut u2) = (c.try_clone().unwrap(), u.try_clone().unwrap());
            thread::spawn(move || {
                let _ = std::io::copy(&mut c2, &mut u.try_clone().unwrap());
            });
            thread::spawn(move || {
                let _ = std::io::copy(&mut u2, &mut c.try_clone().unwrap());
            });
        }
    });
}

#[test]
fn sse_stream_survives_a_tcp_proxy() {
    let (pm, state) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    seed(pm.path(), state.path());
    let (port, _board) = start_ui(pm.path().to_path_buf(), state.path().to_path_buf());
    let relay_port = free_port();
    tcp_relay(relay_port, port);

    // Connect to the stream THROUGH the relay, not directly.
    let mut s = TcpStream::connect(("127.0.0.1", relay_port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(6))).unwrap();
    write!(
        s,
        "GET /api/stream HTTP/1.0\r\nHost: 127.0.0.1:{port}\r\n\r\n"
    )
    .unwrap();

    // The head + the first `: ping` arrive through the proxy.
    let mut got = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(6);
    while !String::from_utf8_lossy(&got).contains(": ping") {
        assert!(
            Instant::now() < deadline,
            "no : ping through proxy: {got:?}"
        );
        let mut buf = [0u8; 4096];
        match s.read(&mut buf) {
            Ok(0) => panic!("proxy closed stream: {got:?}"),
            Ok(n) => got.extend_from_slice(&buf[..n]),
            Err(e) => panic!("read: {e} — got {got:?}"),
        }
    }
    assert!(String::from_utf8_lossy(&got).contains("text/event-stream"));

    // A write → `event: issues` arrives through the same proxy hop.
    let host = format!("127.0.0.1:{port}");
    // CAD-313: a board write is the operator's only with a session —
    // this test process signs in and writes as `operator (ui)`.
    let _d = UiDaemon::start_on(state.path().to_path_buf());
    let op = sign_in(state.path(), port);
    let write_json = |port: u16, method: &str, path: &str, host: &str, body: &str| {
        op_write_json(&op, port, method, path, host, body)
    };
    let (code, _, _) = write_json(
        port,
        "PATCH",
        "/api/issues/CAD-2",
        &host,
        r#"{"status":"review"}"#,
    );
    assert_eq!(code, 200);
    let deadline = Instant::now() + Duration::from_secs(6);
    while !String::from_utf8_lossy(&got).contains("event: issues") {
        assert!(
            Instant::now() < deadline,
            "no issues event through proxy: {got:?}"
        );
        let mut buf = [0u8; 4096];
        match s.read(&mut buf) {
            Ok(0) => panic!("proxy closed stream: {got:?}"),
            Ok(n) => got.extend_from_slice(&buf[..n]),
            Err(e) => panic!("read: {e} — got {got:?}"),
        }
    }
}
// ==== project memory (CAD-68) ====

/// A fixture with a project repo + components for memory tests.
fn mem_fx() -> (TempDir, PathBuf, PathBuf, PathBuf) {
    let (_t, pm, state, repo) = start_fx();
    let repo_s = repo.to_str().unwrap().to_string();
    // start_fx's project is `demo` with no components — add the
    // component-bearing project `mem` alongside it.
    assert!(
        cli(
            &pm,
            &state,
            &[
                "issue",
                "project",
                "add",
                "mem",
                "--prefix",
                "M",
                "--repo",
                &repo_s,
                "--component",
                "daemon",
                "--component",
                "other",
            ]
        )
        .0
    );
    (_t, pm, state, repo)
}

fn mem_body(fact: &str) -> String {
    format!("{fact}\n\n**Why:** the test reason.\n\n**How to apply:** apply the fact.\n")
}

/// `git` with the just-built cadence first on PATH — needed for `git
/// commit` inside a pm that has a `memory/` dir, where the tracker's
/// pre-commit lint hook shells out to `cadence` and an older release
/// binary on PATH would refuse the memory dir.
fn git_hooked(dir: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        // Same identity pm.commit injects — CI runners have no global
        // git config, so a plain `git commit` refuses without it.
        .args(["-c", "user.name=test", "-c", "user.email=t@t"])
        .args(args)
        .env(
            "PATH",
            format!(
                "{}:{}",
                Path::new(bin()).parent().unwrap().display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .output()
        .unwrap();
    (
        out.status.success(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout).trim(),
            String::from_utf8_lossy(&out.stderr).trim()
        ),
    )
}

/// `memory <verb>` against the fixture.
fn mem_cli(pm: &Path, state: &Path, args: &[&str]) -> (bool, Value) {
    let mut full = vec!["memory"];
    full.extend_from_slice(args);
    cli(pm, state, &full)
}

fn mem_cli_env(pm: &Path, state: &Path, args: &[&str], env: &[(&str, &str)]) -> (bool, Value) {
    let mut full = vec!["memory"];
    full.extend_from_slice(args);
    cli_env(pm, state, &full, env)
}

fn propose(pm: &Path, state: &Path, slug: &str, kind: &str, extra: &[&str]) -> (bool, Value) {
    let body = mem_body(&format!("fact for {slug}"));
    let mut args = vec![
        "propose",
        "--project",
        "mem",
        "--type",
        kind,
        "--id",
        slug,
        "-m",
        &body,
    ];
    args.extend_from_slice(extra);
    mem_cli(pm, state, &args)
}

/// Write an explicitly legacy/unverified fixture for reader tests.  It is
/// intentionally not an authority-bearing setup: accepted records without
/// native proposer/review/finalization receipts must remain visible but
/// blocked from retrieval.  Native write coverage belongs to the daemon
/// integration tests, where the socket can prove a live PTY endpoint.
fn legacy_memory(
    pm: &Path,
    slug: &str,
    kind: &str,
    extra: &[&str],
    status: &str,
    verified_at: Option<&str>,
) {
    let mut project = false;
    let mut components = Vec::new();
    let mut paths = Vec::new();
    let mut providers = Vec::new();
    let mut tags = Vec::new();
    let mut confidence = "medium";
    let mut source = None;
    let mut i = 0;
    while i < extra.len() {
        match extra[i] {
            "--scope-project" => {
                project = true;
                i += 1;
            }
            "--scope-component" => {
                components.push(extra[i + 1]);
                i += 2;
            }
            "--scope-path" => {
                paths.push(extra[i + 1]);
                i += 2;
            }
            "--scope-provider" => {
                providers.push(extra[i + 1]);
                i += 2;
            }
            "--scope-tag" => {
                tags.push(extra[i + 1]);
                i += 2;
            }
            "--confidence" => {
                confidence = extra[i + 1];
                i += 2;
            }
            "--source" => {
                source = Some(extra[i + 1]);
                i += 2;
            }
            _ => i += 1,
        }
    }
    let mut yaml = format!(
        "id: {slug}\ntype: {kind}\nstatus: {status}\nconfidence: {confidence}\ncreated: 2026-01-01T00:00:00Z\n"
    );
    if let Some(source) = source {
        yaml.push_str(&format!("source: {source}\n"));
    }
    if let Some(verified_at) = verified_at {
        yaml.push_str(&format!("verified_at: {verified_at}\n"));
    }
    if project
        || !components.is_empty()
        || !paths.is_empty()
        || !providers.is_empty()
        || !tags.is_empty()
    {
        yaml.push_str("scope:\n");
        if project {
            yaml.push_str("  project: true\n");
        }
        if !components.is_empty() {
            yaml.push_str("  components:\n");
            for value in components {
                yaml.push_str(&format!("    - {value}\n"));
            }
        }
        if !paths.is_empty() {
            yaml.push_str("  paths:\n");
            for value in paths {
                yaml.push_str(&format!("    - \"{value}\"\n"));
            }
        }
        if !providers.is_empty() {
            yaml.push_str("  providers:\n");
            for value in providers {
                yaml.push_str(&format!("    - {value}\n"));
            }
        }
        if !tags.is_empty() {
            yaml.push_str("  tags:\n");
            for value in tags {
                yaml.push_str(&format!("    - {value}\n"));
            }
        }
    } else {
        yaml.push_str("scope: {}\n");
    }
    std::fs::create_dir_all(pm.join("mem/memory")).unwrap();
    std::fs::write(
        pm.join(format!("mem/memory/{slug}.md")),
        format!(
            "---\n{yaml}---\n\n{}",
            mem_body(&format!("fact for {slug}"))
        ),
    )
    .unwrap();
}

#[test]
fn memory_writes_require_native_identity() {
    let (_t, pm, state, _repo) = mem_fx();
    let before = commits(&pm);

    // A CLI process is not itself an enrolled native endpoint.  Every
    // authority-bearing write therefore fails before creating a file or
    // tracker commit; the native daemon integration owns the positive path.
    let (ok, err) = propose(
        &pm,
        &state,
        "pipe-drain",
        "gotcha",
        &["--scope-path", "src/**"],
    );
    assert!(!ok, "{err}");
    assert!(
        err["error"]
            .as_str()
            .unwrap()
            .contains("Daemon is not reachable"),
        "{err}"
    );
    assert!(!pm.join("mem/memory/pipe-drain.md").exists());
    assert_eq!(
        commits(&pm),
        before,
        "failed native write changed the tracker"
    );

    legacy_memory(
        &pm,
        "pipe-drain",
        "gotcha",
        &["--scope-path", "src/**"],
        "proposed",
        None,
    );
    for args in [
        vec!["accept", "pipe-drain", "--project", "mem"],
        vec!["reject", "pipe-drain", "--project", "mem"],
        vec!["verify", "pipe-drain", "--project", "mem"],
        vec![
            "supersede",
            "pipe-drain",
            "pipe-drain-new",
            "--project",
            "mem",
        ],
    ] {
        let (ok, err) = mem_cli(&pm, &state, &args);
        assert!(!ok, "{args:?} unexpectedly wrote: {err}");
        assert!(
            err["error"]
                .as_str()
                .unwrap()
                .contains("Daemon is not reachable"),
            "{args:?}: {err}"
        );
    }
    let text = std::fs::read_to_string(pm.join("mem/memory/pipe-drain.md")).unwrap();
    assert!(text.contains("status: proposed"), "{text}");
}

#[test]
fn memory_write_guards() {
    let (_t, pm, state, _repo) = mem_fx();
    legacy_memory(
        &pm,
        "curated",
        "rule",
        &["--scope-project"],
        "proposed",
        None,
    );

    // An operator or environment alias is not a native endpoint proof.
    // Every authority-bearing action fails closed while the daemon is down.
    for args in [
        vec!["accept", "curated", "--project", "mem"],
        vec!["reject", "curated", "--project", "mem"],
        vec!["verify", "curated", "--project", "mem"],
    ] {
        let (ok, err) = mem_cli_env(&pm, &state, &args, &[("CADENCE_ALIAS", "w1")]);
        assert!(!ok, "{args:?} should refuse: {err}");
        assert!(
            err["error"]
                .as_str()
                .unwrap()
                .contains("Daemon is not reachable"),
            "{err}"
        );
    }
    let text = std::fs::read_to_string(pm.join("mem/memory/curated.md")).unwrap();
    assert!(text.contains("status: proposed"), "{text}");

    // …and the same alias cannot turn a CLI process into a proposer.
    let (ok, out) = mem_cli_env(
        &pm,
        &state,
        &[
            "propose",
            "--project",
            "mem",
            "--type",
            "gotcha",
            "--id",
            "w-lesson",
            "--scope-project",
            "-m",
            &mem_body("worker learned a thing"),
        ],
        &[("CADENCE_ALIAS", "w1")],
    );
    assert!(!ok, "{out}");
    assert!(
        out["error"]
            .as_str()
            .unwrap()
            .contains("Daemon is not reachable"),
        "{out}"
    );
    assert!(!pm.join("mem/memory/w-lesson.md").exists());

    // The daemon is consulted before write-time validation, so malformed
    // request content cannot use the CLI as a local validation bypass.
    let (ok, err) = mem_cli(
        &pm,
        &state,
        &[
            "propose",
            "--project",
            "mem",
            "--type",
            "rule",
            "--id",
            "no-why",
            "--scope-project",
            "-m",
            "just a fact, no sections",
        ],
    );
    assert!(
        !ok && err["error"]
            .as_str()
            .unwrap()
            .contains("Daemon is not reachable"),
        "{err}"
    );
    let (ok, err) = mem_cli(
        &pm,
        &state,
        &[
            "propose",
            "--project",
            "mem",
            "--type",
            "rule",
            "--id",
            "bad-comp",
            "--scope-component",
            "nope",
            "-m",
            &mem_body("x"),
        ],
    );
    assert!(
        !ok && err["error"]
            .as_str()
            .unwrap()
            .contains("Daemon is not reachable"),
        "{err}"
    );
    // Lint flags a dangling supersedes on a hand-edited file.
    let bad = pm.join("mem/memory/dangling.md");
    std::fs::write(
        &bad,
        format!(
            "---\nid: dangling\ntype: rule\nstatus: accepted\nconfidence: high\ncreated: 2026-01-01T00:00:00Z\nsupersedes: ghost\nscope:\n  project: true\n---\n\n{}",
            mem_body("dangling link")
        ),
    )
    .unwrap();
    let (ok, out) = mem_cli(&pm, &state, &["lint"]);
    assert!(!ok, "{out}");
    assert!(
        out["errors"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e.as_str().unwrap().contains("supersedes 'ghost'")),
        "{out}"
    );
    std::fs::remove_file(&bad).unwrap();
}

#[test]
fn memory_match_blocks_legacy_records() {
    let (_t, pm, state, repo) = mem_fx();

    // The ten-memory fixture across every scope axis. These are deliberately
    // legacy accepted records: their scope metadata remains readable, but
    // retrieval must refuse them because they have no native proposer and
    // PM finalization proof.
    let specs: [(&str, &str, &[&str]); 10] = [
        (
            "r-project",
            "rule",
            &["--scope-project", "--confidence", "high"],
        ),
        ("r-comp", "rule", &["--scope-component", "daemon"]),
        ("r-low", "rule", &["--scope-project", "--confidence", "low"]),
        (
            "g-comp-hi",
            "gotcha",
            &["--scope-component", "daemon", "--confidence", "high"],
        ),
        ("g-tag", "gotcha", &["--scope-tag", "flaky"]),
        (
            "g-comp-lo",
            "gotcha",
            &["--scope-component", "daemon", "--confidence", "low"],
        ),
        ("c-path", "recipe", &["--scope-path", "src/**"]),
        ("c-prov", "recipe", &["--scope-provider", "claude"]),
        (
            "d-prov",
            "decision",
            &["--scope-provider", "claude", "--confidence", "high"],
        ),
        ("x-other", "gotcha", &["--scope-component", "other"]),
    ];
    for (slug, kind, extra) in &specs {
        legacy_memory(
            &pm,
            slug,
            kind,
            extra,
            "accepted",
            Some("2026-01-01T00:00:00Z"),
        );
    }
    // A proposed (not yet accepted) twin must never match.
    legacy_memory(
        &pm,
        "pending",
        "rule",
        &["--scope-project"],
        "proposed",
        None,
    );

    // Issue M-1: component daemon + tags [flaky] + a code commit
    // touching src/adapter/x.rs — path/tag/component/provider axes
    // all live.
    assert!(
        cli(
            &pm,
            &state,
            &[
                "issue",
                "new",
                "Scoped Work",
                "--project",
                "mem",
                "--component",
                "daemon"
            ]
        )
        .0
    );
    // tags isn't a writable field on this base — hand-edit it in;
    // the loose reader picks it up.
    let issue_md = pm.join("mem/M-1/issue.md");
    let text = std::fs::read_to_string(&issue_md).unwrap();
    std::fs::write(
        &issue_md,
        text.replace("created:", "tags: [flaky]\ncreated:"),
    )
    .unwrap();
    git_hooked(&pm, &["add", "-A"]);
    git_hooked(&pm, &["commit", "-qm", "tag fixture"]);
    // A code commit the issue discovery will pick up (subject names M-1).
    std::fs::create_dir_all(repo.join("src/adapter")).unwrap();
    std::fs::write(repo.join("src/adapter/x.rs"), "x").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "M-1 adapter work"]);
    // D-2-equivalent: M-2 has no component/tags/commits.
    assert!(cli(&pm, &state, &["issue", "new", "Bare", "--project", "mem"]).0);

    let (ok, out) = mem_cli(
        &pm,
        &state,
        &["match", "--issue", "M-1", "--provider", "claude", "--json"],
    );
    assert!(ok, "{out}");
    let slugs: Vec<&str> = out["matched"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["slug"].as_str().unwrap())
        .collect();
    assert!(
        slugs.is_empty(),
        "legacy records must stay blocked: {slugs:?}"
    );

    // M-2 also has no eligible native records.
    let (ok, out) = mem_cli(&pm, &state, &["match", "--issue", "M-2", "--json"]);
    assert!(ok, "{out}");
    let slugs: Vec<&str> = out["matched"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["slug"].as_str().unwrap())
        .collect();
    assert!(slugs.is_empty(), "{slugs:?}");

    // Explicit-axis match without an issue.
    let (ok, out) = mem_cli(
        &pm,
        &state,
        &[
            "match",
            "--project",
            "mem",
            "--component",
            "other",
            "--json",
        ],
    );
    assert!(ok, "{out}");
    let slugs: Vec<&str> = out["matched"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["slug"].as_str().unwrap())
        .collect();
    assert!(slugs.is_empty(), "{slugs:?}");
}

/// Give a `legacy_memory` fixture a PM-finalized verify cycle at `at` —
/// the receipt retrieval reads as "last verified". Freshness readers do
/// not require quorum; the raw `verified_at` field is not the receipt.
fn verify_fixture(pm: &Path, slug: &str, at: &str) {
    let path = pm.join(format!("mem/memory/{slug}.md"));
    let text = std::fs::read_to_string(&path).unwrap();
    let (mut front, body) = cadence_agent::memory::parse_memory(&text).unwrap();
    front.review_cycle = 2;
    front.finalizations.retain(|r| r.operation != "verify");
    front
        .finalizations
        .push(cadence_agent::memory::FinalizationReceipt {
            operation: "verify".to_string(),
            cycle: 2,
            digest: "fixture".to_string(),
            finalizer: cadence_agent::memory::IdentityProof {
                alias: "fixture-pm".to_string(),
                registration: 4,
                generation: "fixture-pm-4".to_string(),
                process_start: 104,
                role: "pm".to_string(),
            },
            finalized_at: at.to_string(),
        });
    std::fs::write(
        &path,
        cadence_agent::issue::parse::render(&front, &body).unwrap(),
    )
    .unwrap();
}

/// Set (or clear) a fixture's explicit `stale:` mark.
fn mark_stale_fixture(pm: &Path, slug: &str, why: Option<&str>) {
    let path = pm.join(format!("mem/memory/{slug}.md"));
    let text = std::fs::read_to_string(&path).unwrap();
    let (mut front, body) = cadence_agent::memory::parse_memory(&text).unwrap();
    front.stale = why.map(str::to_string);
    std::fs::write(
        &path,
        cadence_agent::issue::parse::render(&front, &body).unwrap(),
    )
    .unwrap();
}

fn stale_entry<'a>(out: &'a Value, slug: &str) -> Option<&'a Value> {
    out["stale"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["slug"] == slug)
}

#[test]
fn memory_stale_flags_changed_paths() {
    let (_t, pm, state, repo) = mem_fx();
    let verified_epoch = time::now_epoch();
    let verified = time::iso(verified_epoch);
    for (slug, glob) in [("src-watch", "src/**"), ("docs-watch", "docs/**")] {
        legacy_memory(
            &pm,
            slug,
            "rule",
            &["--scope-path", glob],
            "accepted",
            Some(&verified),
        );
        verify_fixture(&pm, slug, &verified);
    }
    // The change must land strictly after the verify. Both stamps are
    // whole seconds: wait for the clock to pass the verified second
    // (often already true after the CLI calls).
    let deadline = Instant::now() + Duration::from_secs(5);
    while time::now_epoch() <= verified_epoch {
        assert!(Instant::now() < deadline, "clock never passed {verified}");
        std::thread::sleep(Duration::from_millis(20));
    }
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/changed.rs"), "x").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "late src change"]);

    let (ok, out) = mem_cli(&pm, &state, &["ls", "--stale", "--json"]);
    assert!(ok, "{out}");
    let slugs: Vec<&str> = out["stale"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["slug"].as_str().unwrap())
        .collect();
    assert_eq!(slugs, vec!["src-watch"], "{out}");
    assert_eq!(
        out["stale"][0]["reason"].as_str().unwrap(),
        "paths changed after verified_at"
    );
    assert_eq!(out["stale"][0]["evidence"]["state"], "verified", "{out}");
    assert_eq!(
        out["stale"][0]["changed"],
        json!(["src/changed.rs"]),
        "{out}"
    );
}

/// CAD-395: `ls --stale` reads freshness through the same `Freshness`
/// retrieval uses — past the window reads "unverified (last verified
/// <date>)", a stale mark reads withheld with its reason, a raw
/// `verified_at` with no verify receipt reads unverified, and none of
/// them is ever "verified".
#[test]
fn memory_stale_reads_retrieval_freshness() {
    let (_t, pm, state, _repo) = mem_fx();
    let now = time::iso(time::now_epoch());
    for slug in ["fresh", "aged", "marked", "raw-stamp"] {
        legacy_memory(
            &pm,
            slug,
            "rule",
            &["--scope-project"],
            "accepted",
            Some(&now),
        );
    }
    verify_fixture(&pm, "fresh", &now);
    verify_fixture(&pm, "aged", "2020-01-01T00:00:00Z");
    verify_fixture(&pm, "marked", &now);
    mark_stale_fixture(&pm, "marked", Some("M-9 reverted the cited fix"));

    let (ok, out) = mem_cli(&pm, &state, &["ls", "--stale", "--json"]);
    assert!(ok, "{out}");
    assert!(stale_entry(&out, "fresh").is_none(), "{out}");
    let aged = stale_entry(&out, "aged").expect("past-window is stale");
    assert_eq!(aged["reason"], "not verified within the window");
    assert_eq!(aged["verified_at"], "2020-01-01T00:00:00Z");
    assert_eq!(aged["evidence"]["state"], "unverified");
    assert_eq!(
        aged["evidence"]["label"],
        "unverified (last verified 2020-01-01)"
    );
    let marked = stale_entry(&out, "marked").expect("stale mark is stale");
    assert_eq!(
        marked["reason"],
        "evidence marked stale: M-9 reverted the cited fix"
    );
    assert_eq!(marked["evidence"]["state"], "withheld");
    assert_eq!(
        marked["evidence"]["reason"],
        "evidence marked stale: M-9 reverted the cited fix"
    );
    // A raw verified_at of now is not a verify receipt.
    let raw = stale_entry(&out, "raw-stamp").expect("raw stamp is not a verify");
    assert_eq!(raw["verified_at"], Value::Null);
    assert_eq!(raw["evidence"]["label"], "unverified");

    // The text view carries the same labels, never "verified".
    let (ok, text, _) = cli_out_err(&pm, &state, &["memory", "ls", "--stale"]);
    assert!(ok, "{text}");
    assert!(
        text.contains(
            "mem/aged\tunverified (last verified 2020-01-01)\tnot verified within the window"
        ),
        "{text}"
    );
    assert!(
        text.contains("mem/marked\twithheld\tevidence marked stale: M-9 reverted the cited fix"),
        "{text}"
    );
    assert!(text.contains("mem/raw-stamp\tunverified\t"), "{text}");
    assert!(!text.contains("\tverified"), "{text}");

    // The window is the project's `memory.stale_days`; `--days` overrides.
    let yaml = pm.join("mem/project.yaml");
    let mut conf = std::fs::read_to_string(&yaml).unwrap();
    conf.push_str("memory:\n  stale_days: 100000\n");
    std::fs::write(&yaml, conf).unwrap();
    let (ok, out) = mem_cli(&pm, &state, &["ls", "--stale", "--json"]);
    assert!(ok, "{out}");
    assert!(stale_entry(&out, "aged").is_none(), "{out}");
    let (ok, out) = mem_cli(&pm, &state, &["ls", "--stale", "--days", "30", "--json"]);
    assert!(ok, "{out}");
    assert!(stale_entry(&out, "aged").is_some(), "{out}");
}

#[test]
fn memory_seed_lessons_require_native_identity() {
    let (_t, pm, state, _repo) = mem_fx();
    // The kickoff's seed list — ten lessons, verbatim.
    let lessons: [(&str, &str); 10] = [
        ("detached-reviews", "Reviews happen on detached checkouts only."),
        ("isolate-before-blame", "Compare a failure in isolation on the PR head and on the base before blaming a PR."),
        ("stress-state-waits", "Stress new state-waiting tests before trusting them."),
        ("gate-moved-base", "Gate the merge result when the base moved under the PR."),
        ("pipe-draining", "4 KB pipes require draining while waiting on a child process."),
        ("devin-prompts", "The Devin busy prompt is `Guide Devin while it works` vs idle `Ask Devin to build…`."),
        ("worktree-idle-probe", "Never remove a worktree until the owner probes idle."),
        ("restart-quiet-queue", "Queue nothing immediately before a daemon restart."),
        ("claude-default-model", "Managed Claude workers inherit the host default model unless --model is provided."),
        ("rebase-closing-brace", "After a keep-both rebase at end of file, re-insert the closing brace Git matched as context."),
    ];
    for (slug, fact) in &lessons {
        let (ok, out) = mem_cli(
            &pm,
            &state,
            &[
                "propose", "--project", "mem", "--type", "rule", "--id", slug,
                "--scope-project", "--source", "CAD-68", "-m",
                &format!(
                    "{fact}\n\n**Why:** learned the hard way in cadence development.\n\n**How to apply:** check this before the relevant step.\n"
                ),
            ],
        );
        assert!(
            !ok,
            "{slug} unexpectedly imported without native identity: {out}"
        );
        assert!(
            out["error"]
                .as_str()
                .unwrap()
                .contains("Daemon is not reachable"),
            "{slug}: {out}"
        );
    }
    let (ok, out) = mem_cli(&pm, &state, &["lint"]);
    assert!(ok && out["ok"] == true, "{out}");
    let (ok, out) = cli(&pm, &state, &["issue", "lint"]);
    assert!(ok && out["ok"] == true, "{out}");
    let (ok, out) = mem_cli(&pm, &state, &["ls", "--project", "mem", "--json"]);
    assert!(ok);
    assert_eq!(out["memories"].as_array().unwrap().len(), 0, "{out}");
}

/// `ui run` as a subprocess with a sanitized env. Kills the child on drop
/// so a failed assert leaves no listener behind.
struct UiProc(std::process::Child);
impl Drop for UiProc {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn spawn_ui(pm: &Path, state: &Path) -> (u16, UiProc) {
    spawn_ui_env(pm, state, &[])
}

/// `spawn_ui` with `env` set on the server after the sanitizing.
#[allow(clippy::zombie_processes)] // UiProc's Drop kills + waits.
fn spawn_ui_env(pm: &Path, state: &Path, env: &[(&str, &str)]) -> (u16, UiProc) {
    let port = free_port();
    let mut cmd = Command::new(bin());
    cmd.arg("--state-dir")
        .arg(state)
        .args(["ui", "run", "--port", &port.to_string()])
        .env("CADENCE_PM_DIR", pm)
        // The tracker's pre-commit hook runs `cadence` from PATH — same
        // PATH fix as cli_run so the hook lints with THIS build.
        .env(
            "PATH",
            format!(
                "{}:{}",
                Path::new(bin()).parent().unwrap().display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env_remove("CADENCE_ALIAS")
        .env_remove("CADENCE_STATE_DIR");
    if let Ok(home) = std::env::var("HOME") {
        cmd.env("HOME", home);
    }
    cmd.envs(env.iter().copied());
    let child = cmd.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        // Connect-refused while the listener binds is expected — the
        // http helper unwraps the connect, so probe it raw here.
        if let Ok(mut s) = TcpStream::connect(("127.0.0.1", port)) {
            s.set_read_timeout(Some(Duration::from_secs(2))).ok();
            let probe = format!("GET /api/health HTTP/1.0\r\nHost: 127.0.0.1:{port}\r\n\r\n");
            let _ = s.write_all(probe.as_bytes());
            let mut buf = String::new();
            if s.read_to_string(&mut buf).is_ok() && buf.contains("200") {
                return (port, UiProc(child));
            }
        }
        assert!(Instant::now() < deadline, "ui subprocess did not start");
        thread::sleep(Duration::from_millis(50));
    }
}

/// The board as an operator runs it, however the suite is run: a
/// detached `cadence ui start`. Operator-only relays (`model_defaults_set`,
/// CAD-337) refuse a board whose ancestry carries an agent, and when the
/// suite itself runs in an agent pane this test process is one — so
/// `start_ui`'s in-process board is (CAD-380). `ui start` hands the
/// server to a fresh session leader (`setsid`) that is reparented off
/// this process's ancestry once `start` exits, `env_clear` leaves no
/// `CADENCE_ALIAS`, and stdio is a log file, not a pane tty — the shape
/// `peer::operator_proof` accepts, as `TestDaemon::operator_rpc` does in
/// tests/common/mod.rs (CAD-291). The gate itself is untouched.
/// `free_port` is a bind-release race, so a failed start retries.
fn start_operator_ui(pm: &Path, state: &Path) -> (u16, DetachedUi) {
    let guard = DetachedUi(state.to_path_buf());
    let overall = Instant::now() + Duration::from_secs(30);
    loop {
        let port = free_port();
        let out = Command::new(bin())
            .arg("--state-dir")
            .arg(state)
            .args(["ui", "start", "--port", &port.to_string()])
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", pm)
            .env("CADENCE_PM_DIR", pm)
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        if out.status.success() {
            return (port, guard);
        }
        // A start that timed out leaves its pid file — clear it, or the
        // retry answers `already_running` on the old port.
        drop(DetachedUi(state.to_path_buf()));
        assert!(
            Instant::now() < overall,
            "operator ui did not start: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test]
fn memory_ui_lists_detail_and_refuses_memory_write() {
    let (_t, pm, state, _repo) = mem_fx();
    legacy_memory(
        &pm,
        "ui-mem",
        "rule",
        &["--scope-project"],
        "proposed",
        None,
    );
    let (port, _ui) = spawn_ui(&pm, &state);
    let host = format!("127.0.0.1:{port}");

    // List + filter.
    let (status, body) = http(port, "GET", "/api/memories", &host);
    assert_eq!(status, 200, "{body}");
    let list: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(list["memories"].as_array().unwrap().len(), 1);
    let (status, body) = http(port, "GET", "/api/memories?status=accepted", &host);
    assert_eq!(status, 200);
    let list: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(list["memories"].as_array().unwrap().len(), 0);

    // Detail.
    let (status, body) = http(port, "GET", "/api/memories/mem/ui-mem", &host);
    assert_eq!(status, 200, "{body}");
    let detail: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(detail["slug"], "ui-mem");
    assert!(detail["body"].as_str().unwrap().contains("**Why:**"));

    // Unguarded write refused (no X-Cadence-Board header).
    let (status, _, _) = http_write(
        port,
        "POST",
        "/api/memories/mem/ui-mem/accept",
        &host,
        &["Content-Type: application/json"],
        b"{}",
    );
    assert_eq!(status, 403);

    // The CSRF guard permits the request shape, but HTTP still cannot prove
    // the native PTY identity required for memory authority. The route must
    // refuse without changing the legacy record.
    let before = std::fs::read_to_string(pm.join("mem/memory/ui-mem.md")).unwrap();
    let (status, _, body) =
        write_json(port, "POST", "/api/memories/mem/ui-mem/accept", &host, "{}");
    assert_eq!(status, 400, "{body}");
    assert!(body.contains("native agent endpoint"), "{body}");
    let text = std::fs::read_to_string(pm.join("mem/memory/ui-mem.md")).unwrap();
    assert_eq!(text, before);

    // Reject is equally authority-bearing and therefore equally refused.
    legacy_memory(
        &pm,
        "ui-no",
        "gotcha",
        &["--scope-project"],
        "proposed",
        None,
    );
    let before = std::fs::read_to_string(pm.join("mem/memory/ui-no.md")).unwrap();
    let (status, _, body) = write_json(port, "POST", "/api/memories/mem/ui-no/reject", &host, "{}");
    assert_eq!(status, 400, "{body}");
    assert!(body.contains("native agent endpoint"), "{body}");
    assert_eq!(
        std::fs::read_to_string(pm.join("mem/memory/ui-no.md")).unwrap(),
        before
    );
}

/// CAD-395: the board's memory view reads a lesson's freshness through
/// the same `Freshness` retrieval uses, per the lesson's project window:
/// past-window → "unverified (last verified <date>)", stale-marked →
/// withheld with its reason, raw `verified_at` alone → unverified.
#[test]
fn memory_board_reads_retrieval_freshness() {
    let (_t, pm, state, _repo) = mem_fx();
    let now = time::iso(time::now_epoch());
    for slug in ["fresh", "aged", "marked", "raw-stamp"] {
        legacy_memory(
            &pm,
            slug,
            "rule",
            &["--scope-project"],
            "accepted",
            Some(&now),
        );
    }
    verify_fixture(&pm, "fresh", &now);
    verify_fixture(&pm, "aged", "2020-01-01T00:00:00Z");
    verify_fixture(&pm, "marked", &now);
    mark_stale_fixture(&pm, "marked", Some("M-9 reverted the cited fix"));
    let (port, _ui) = spawn_ui(&pm, &state);
    let host = format!("127.0.0.1:{port}");

    let (status, body) = http(port, "GET", "/api/memories", &host);
    assert_eq!(status, 200, "{body}");
    let list: Value = serde_json::from_str(&body).unwrap();
    let evidence = |slug: &str| -> Value {
        list["memories"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["slug"] == slug)
            .unwrap()["evidence"]
            .clone()
    };
    let fresh = evidence("fresh");
    assert_eq!(fresh["state"], "verified", "{fresh}");
    assert_eq!(fresh["label"], format!("verified {}", &now[..10]));
    let aged = evidence("aged");
    assert_eq!(aged["state"], "unverified", "{aged}");
    assert_eq!(aged["label"], "unverified (last verified 2020-01-01)");
    assert_eq!(aged["window_days"], 30);
    let marked = evidence("marked");
    assert_eq!(marked["state"], "withheld", "{marked}");
    assert_eq!(marked["label"], "withheld");
    assert_eq!(
        marked["reason"],
        "evidence marked stale: M-9 reverted the cited fix"
    );
    let raw = evidence("raw-stamp");
    assert_eq!(raw["state"], "unverified", "{raw}");
    assert_eq!(raw["label"], "unverified");
    assert_eq!(raw["last_verified"], Value::Null);

    // The detail and `memory ls --json` carry the same reading.
    let (status, body) = http(port, "GET", "/api/memories/mem/aged", &host);
    assert_eq!(status, 200, "{body}");
    let detail: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(detail["evidence"], aged);
    let (ok, out) = mem_cli(&pm, &state, &["ls", "--json"]);
    assert!(ok, "{out}");
    let card = out["memories"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["slug"] == "marked")
        .unwrap();
    assert_eq!(card["evidence"], marked);

    // The window is the lesson's project's.
    let yaml = pm.join("mem/project.yaml");
    let mut conf = std::fs::read_to_string(&yaml).unwrap();
    conf.push_str("memory:\n  stale_days: 100000\n");
    std::fs::write(&yaml, conf).unwrap();
    let (status, body) = http(port, "GET", "/api/memories/mem/aged", &host);
    assert_eq!(status, 200, "{body}");
    let detail: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(detail["evidence"]["label"], "verified 2020-01-01");
}

/// `cadence …` returning (ok, stdout, stderr) — load warnings go to
/// stderr while JSON stays on stdout, and cli_run merges them away.
fn cli_out_err(pm: &Path, state: &Path, args: &[&str]) -> (bool, String, String) {
    let mut cmd = Command::new(bin());
    cmd.arg("--state-dir")
        .arg(state)
        .args(args)
        .env("CADENCE_PM_DIR", pm)
        .env(
            "PATH",
            format!(
                "{}:{}",
                Path::new(bin()).parent().unwrap().display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env_remove("CADENCE_ALIAS");
    if let Ok(home) = std::env::var("HOME") {
        cmd.env("HOME", home);
    }
    let out = cmd.output().unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

/// A memory file that fails to load does not sink the readers:
/// `memory ls` still lists the good ones, warns once on stderr and
/// reports `load_errors`; `/api/memories` carries `memory_errors`.
#[test]
fn memory_load_errors_surface_in_cli_and_api() {
    let (_t, pm, state, _repo) = mem_fx();
    legacy_memory(
        &pm,
        "good-mem",
        "rule",
        &["--scope-project"],
        "proposed",
        None,
    );
    std::fs::write(
        pm.join("mem/memory/broken.md"),
        "---\nid: [unclosed\n---\nbody\n",
    )
    .unwrap();

    // CLI: one good memory lists, stderr names the broken file.
    let (ok, out, err) = cli_out_err(&pm, &state, &["memory", "ls", "--project", "mem", "--json"]);
    assert!(ok, "{err}");
    let v: Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(v["memories"].as_array().unwrap().len(), 1, "{v}");
    assert_eq!(v["load_errors"].as_array().unwrap().len(), 1, "{v}");
    assert!(err.contains("1 file(s) failed to load"), "{err}");
    assert!(err.contains("broken.md"), "{err}");

    // `ls --stale` takes the same path.
    let (ok, out, err) = cli_out_err(&pm, &state, &["memory", "ls", "--stale", "--json"]);
    assert!(ok, "{err}");
    let v: Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(v["load_errors"].as_array().unwrap().len(), 1, "{v}");

    // API: same split — good payload, errors alongside.
    let (port, _ui) = spawn_ui(&pm, &state);
    let host = format!("127.0.0.1:{port}");
    let (status, body) = http(port, "GET", "/api/memories?project=mem", &host);
    assert_eq!(status, 200, "{body}");
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["memories"].as_array().unwrap().len(), 1, "{v}");
    assert_eq!(v["memory_errors"].as_array().unwrap().len(), 1, "{v}");
    assert!(
        v["memory_errors"][0]
            .as_str()
            .unwrap()
            .contains("broken.md"),
        "{v}"
    );
}

/// An absent memory dir is a valid empty store; a path that exists
/// but cannot be enumerated (here: a file where the dir should be)
/// is an explicit load error — never silently empty.
#[test]
fn memory_absent_dir_empty_unreadable_dir_errors() {
    let (_t, pm, state, _repo) = mem_fx();

    // Absent: `mem` has no memory/ dir yet — clean empty, no errors.
    let (ok, out, err) = cli_out_err(&pm, &state, &["memory", "ls", "--project", "mem", "--json"]);
    assert!(ok, "{err}");
    let v: Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(v["memories"].as_array().unwrap().len(), 0, "{v}");
    assert_eq!(v["load_errors"].as_array().unwrap().len(), 0, "{v}");

    // A file where the dir should be: read_dir fails ENOTDIR → error.
    std::fs::write(pm.join("mem/memory"), "not a dir").unwrap();
    let (ok, out, err) = cli_out_err(&pm, &state, &["memory", "ls", "--project", "mem", "--json"]);
    assert!(ok, "{err}");
    let v: Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(v["memories"].as_array().unwrap().len(), 0, "{v}");
    let errs = v["load_errors"].as_array().unwrap();
    assert_eq!(errs.len(), 1, "{v}");
    assert!(errs[0].as_str().unwrap().contains("cannot list"), "{v}");
    assert!(err.contains("failed to load"), "{err}");

    // The API surfaces the same split — HTTP stays 200, the error is
    // in-band next to the (empty) records.
    let (port, _ui) = spawn_ui(&pm, &state);
    let host = format!("127.0.0.1:{port}");
    let (status, body) = http(port, "GET", "/api/memories?project=mem", &host);
    assert_eq!(status, 200, "{body}");
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["memories"].as_array().unwrap().len(), 0, "{v}");
    assert!(
        v["memory_errors"][0]
            .as_str()
            .unwrap()
            .contains("cannot list"),
        "{v}"
    );
}

/// CAD-437: `memory ls` shares the grammar — repeatable any-of value
/// flags, AND across them (with component/path scope semantics kept),
/// unknown values error, sort/limit/fields tail.
#[test]
fn memory_ls_cad437_grammar() {
    let (_t, pm, state, _repo) = mem_fx();
    legacy_memory(
        &pm,
        "a-rule",
        "rule",
        &["--scope-project"],
        "accepted",
        None,
    );
    legacy_memory(
        &pm,
        "a-gotcha",
        "gotcha",
        &["--scope-component", "daemon"],
        "accepted",
        None,
    );
    legacy_memory(
        &pm,
        "old-rule",
        "rule",
        &["--scope-component", "other"],
        "superseded",
        None,
    );
    let slugs = |v: &Value| -> Vec<String> {
        let mut s: Vec<String> = v["memories"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["slug"].as_str().unwrap().to_string())
            .collect();
        s.sort();
        s
    };

    // Any-of within a flag (repeat or comma-join); AND across flags.
    let (ok, out) = mem_cli(&pm, &state, &["ls", "--type", "rule,gotcha", "--json"]);
    assert!(ok, "{out}");
    assert_eq!(slugs(&out).len(), 3, "{out}");
    let (ok, out) = mem_cli(
        &pm,
        &state,
        &["ls", "--type", "rule", "--status", "accepted", "--json"],
    );
    assert!(ok, "{out}");
    assert_eq!(slugs(&out), ["a-rule"], "{out}");
    // Scoped axes keep their retrieval meaning: a component filter
    // still passes project-wide memories.
    let (ok, out) = mem_cli(&pm, &state, &["ls", "--component", "daemon", "--json"]);
    assert!(ok, "{out}");
    assert_eq!(slugs(&out), ["a-gotcha", "a-rule"], "{out}");
    let (ok, out) = mem_cli(
        &pm,
        &state,
        &[
            "ls",
            "--component",
            "daemon",
            "--status",
            "superseded",
            "--json",
        ],
    );
    assert!(ok, "{out}");
    assert_eq!(slugs(&out), Vec::<String>::new(), "{out}");

    // Unknown vocabularies error; so do unknown sorts and fields.
    let (ok, _, err) = cli_out_err(&pm, &state, &["memory", "ls", "--type", "zzz"]);
    assert!(!ok && err.contains("gotcha"), "{err}");
    let (ok, _, err) = cli_out_err(&pm, &state, &["memory", "ls", "--status", "zzz"]);
    assert!(!ok && err.contains("accepted"), "{err}");
    let (ok, _, err) = cli_out_err(&pm, &state, &["memory", "ls", "--sort", "zzz", "--json"]);
    assert!(!ok && err.contains("--sort"), "{err}");
    let (ok, _, err) = cli_out_err(&pm, &state, &["memory", "ls", "--fields", "id"]);
    assert!(!ok && err.contains("--json"), "{err}");

    // The tail: sort, limit, fields.
    let (ok, out) = mem_cli(
        &pm,
        &state,
        &["ls", "--sort", "-slug", "--limit", "1", "--json"],
    );
    assert!(ok, "{out}");
    assert_eq!(slugs(&out), ["old-rule"], "{out}");
    let (ok, out) = mem_cli(&pm, &state, &["ls", "--fields", "slug,type", "--json"]);
    assert!(ok, "{out}");
    let keys: Vec<&String> = out["memories"][0].as_object().unwrap().keys().collect();
    assert_eq!(keys, ["slug", "type"], "{out}");
}

/// A hand-edited over-complex path glob bypasses write-time
/// validation — so load quarantines it with an error before matching
/// can ever run it. Valid siblings still list and match.
#[test]
fn memory_overcomplex_glob_quarantined_at_load() {
    let (_t, pm, state, _repo) = mem_fx();
    legacy_memory(
        &pm,
        "good-rule",
        "rule",
        &["--scope-project"],
        "accepted",
        Some("2026-01-01T00:00:00Z"),
    );
    // The evil file is accepted and project-scoped — it would match
    // everything; its hand-edited glob quarantines it instead. The valid
    // sibling remains readable, but both legacy records stay ineligible for
    // retrieval without native receipts.
    std::fs::write(
        pm.join("mem/memory/evil-glob.md"),
        "---\nid: evil-glob\ntype: rule\nstatus: accepted\nconfidence: medium\ncreated: 2026-01-01T00:00:00Z\nscope:\n  project: true\n  paths:\n    - \"**a**a**a**\"\n---\nfact\n\n**Why:** w\n\n**How to apply:** h\n",
    )
    .unwrap();

    let (ok, out) = mem_cli(&pm, &state, &["ls", "--project", "mem", "--json"]);
    assert!(ok, "{out}");
    let slugs: Vec<&str> = out["memories"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["slug"].as_str().unwrap())
        .collect();
    assert_eq!(slugs, vec!["good-rule"], "{out}");
    let errs = out["load_errors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e.as_str().unwrap().to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(errs.contains("evil-glob"), "{errs}");
    assert!(errs.contains("too complex"), "{errs}");
    assert!(errs.contains("quarantined"), "{errs}");

    // Match — `evil-glob` never reaches glob_match. The valid legacy rule
    // is visible but blocked by retrieval proof, so the result is empty;
    // the test still proves the adversarial glob is quarantined before any
    // matcher can execute it.
    let (ok, out) = mem_cli(
        &pm,
        &state,
        &["match", "--project", "mem", "--path", "src/x.rs", "--json"],
    );
    assert!(ok, "{out}");
    let slugs: Vec<&str> = out["matched"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["slug"].as_str().unwrap())
        .collect();
    assert!(slugs.is_empty(), "{out}");
    assert!(
        out["load_errors"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e.as_str().unwrap().contains("evil-glob")),
        "{out}"
    );
}

/// Lint bounds the fact block in bytes, not just lines, and refuses
/// path scopes whose `**` recursion could backtrack exponentially.
#[test]
fn memory_lint_bounds_fact_bytes_and_glob() {
    let (_t, pm, state, _repo) = mem_fx();
    let dir = pm.join("mem/memory");
    std::fs::create_dir_all(&dir).unwrap();
    let fat = "x".repeat(600);
    std::fs::write(
        dir.join("fat-fact.md"),
        format!(
            "---\nid: fat-fact\ntype: rule\nstatus: proposed\nconfidence: medium\ncreated: 2026-01-01T00:00:00Z\n---\n{fat}\n\n**Why:** w\n\n**How to apply:** h\n"
        ),
    )
    .unwrap();
    std::fs::write(
        dir.join("evil-glob.md"),
        "---\nid: evil-glob\ntype: rule\nstatus: proposed\nconfidence: medium\ncreated: 2026-01-01T00:00:00Z\nscope:\n  paths:\n    - \"**a**a**a**\"\n---\nfact\n\n**Why:** w\n\n**How to apply:** h\n",
    )
    .unwrap();

    let (ok, out) = mem_cli(&pm, &state, &["lint"]);
    assert!(!ok && out["ok"] == false, "{out}");
    let errs = out["errors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e.as_str().unwrap().to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(errs.contains("fat-fact: fact is 600 bytes"), "{errs}");
    assert!(errs.contains("evil-glob"), "{errs}");
    assert!(errs.contains("too complex"), "{errs}");
}

/// A verify time that isn't ASCII — e.g. `abcé-01-01` — must not
/// panic the stale scan (the old slicer cut mid-char); it reads as not
/// current.
#[test]
fn memory_malformed_timestamp_is_safe() {
    let (_t, pm, state, _repo) = mem_fx();
    legacy_memory(
        &pm,
        "bad-date",
        "rule",
        &["--scope-project"],
        "accepted",
        Some("2026-01-01T00:00:00Z"),
    );
    verify_fixture(&pm, "bad-date", "abc\u{e9}-01-01");

    let (ok, out) = mem_cli(&pm, &state, &["ls", "--stale", "--json"]);
    assert!(ok, "{out}");
    let stale = out["stale"].as_array().unwrap();
    assert_eq!(stale.len(), 1, "{out}");
    assert_eq!(stale[0]["slug"], "bad-date");
    assert_eq!(
        stale[0]["reason"].as_str().unwrap(),
        "not verified within the window"
    );
}
// ==== CAD-83: `cadence overview` + /api/meta + /api/overview ====

/// A fake `gh` binary dir: `pr` calls answer $FAKE_GH_PRS, the
/// `ci.yml` runs listing answers $FAKE_GH_RUNS (default: no runs), the
/// repo read answers default branch `main`; FAKE_GH_FAIL=1 makes every
/// call exit 1. The call log records argv lines.
const FAKE_GH: &str = r#"#!/bin/sh
echo "$*" >> "$FAKE_GH_LOG"
if [ "$FAKE_GH_FAIL" = "1" ]; then echo "gh: simulated outage" >&2; exit 1; fi
no_runs='{"total_count": 0, "workflow_runs": []}'
case "$1 $2" in
  pr\ *) printf '%s' "$FAKE_GH_PRS" ;;
  "api repos/"*/actions/workflows/*) printf '%s' "${FAKE_GH_RUNS:-$no_runs}" ;;
  "api repos/"*) printf '%s' '{"default_branch": "main"}' ;;
  *) exit 1 ;;
esac
"#;

/// Fake-gh dir + call log; drop keeps the tempdir alive for the test.
struct FakeGh {
    _tmp: TempDir,
    bin: PathBuf,
    log: PathBuf,
}

fn fake_gh() -> FakeGh {
    let tmp = TempDir::new().unwrap();
    let bin = tmp.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let gh = bin.join("gh");
    std::fs::write(&gh, FAKE_GH).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let log = tmp.path().join("gh.log");
    std::fs::write(&log, "").unwrap();
    FakeGh {
        _tmp: tmp,
        bin,
        log,
    }
}

/// PATH overlay: fake gh first, the just-built cadence second (the
/// tracker's pre-commit hook resolves `cadence` from PATH).
fn gh_path(gh: &FakeGh) -> String {
    format!(
        "{}:{}:{}",
        gh.bin.display(),
        Path::new(bin()).parent().unwrap().display(),
        std::env::var("PATH").unwrap_or_default()
    )
}

/// A temp git repo carrying `remote.origin.url` — `issue project add`
/// records the remote for cwd/drift matching.
fn repo_with_remote(remote: &str) -> TempDir {
    let dir = TempDir::new().unwrap();
    assert!(git(dir.path(), &["init", "-q"]).0);
    assert!(git(dir.path(), &["config", "user.email", "t@t"]).0);
    assert!(git(dir.path(), &["config", "user.name", "t"]).0);
    std::fs::write(dir.path().join("f"), "x").unwrap();
    assert!(git(dir.path(), &["add", "f"]).0);
    assert!(git(dir.path(), &["commit", "-qm", "init"]).0);
    assert!(git(dir.path(), &["remote", "add", "origin", remote]).0);
    dir
}

/// ISO `<n> seconds ago` — for `updatedAt` fixtures.
fn iso_ago(secs: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    cadence_agent::issue::time::iso(now - secs)
}

#[test]
fn overview_meta_and_shell_routes() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    seed(pm.path(), state.path());
    let (port, _board) = start_ui(pm.path().to_path_buf(), state.path().to_path_buf());
    let host = format!("127.0.0.1:{port}");

    // /api/meta carries the serving binary's build identity.
    let (code, body) = http(port, "GET", "/api/meta", &host);
    assert_eq!(code, 200, "{body}");
    let meta: Value = serde_json::from_str(&body).unwrap();
    assert!(!meta["build_commit"].as_str().unwrap_or("").is_empty());
    assert!(!meta["build_time"].as_str().unwrap_or("").is_empty());
    assert!(!meta["version"].as_str().unwrap_or("").is_empty());

    // /api/overview — the whole derived screen, daemon unreachable is
    // honest but never fatal.
    let (code, body) = http(port, "GET", "/api/overview", &host);
    assert_eq!(code, 200, "{body}");
    let v: Value = serde_json::from_str(&body).unwrap();
    assert!(v["needs_me"].is_array(), "{v}");
    assert!(v["drift"].is_object(), "{v}");
    assert!(v["projects"].is_array(), "{v}");
    assert!(v["generated_at"].as_i64().unwrap_or(0) > 0, "{v}");
    assert_eq!(v["daemon"]["reachable"], false, "{v}");
    assert_eq!(v["drift"]["matched"], false, "{v}");
    assert!(
        v["drift"]["reason"]
            .as_str()
            .unwrap_or("")
            .contains("unreachable"),
        "{v}"
    );
}

#[test]
fn overview_merge_ready_pr_first_with_exact_command() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let repo = repo_with_remote("https://github.com/acme/widgets.git");
    assert!(cli(pm.path(), state.path(), &["issue", "init"]).0);
    let repo_s = repo.path().to_str().unwrap().to_string();
    assert!(
        cli(
            pm.path(),
            state.path(),
            &["issue", "project", "add", "cadence", "--prefix", "CAD", "--repo", &repo_s]
        )
        .0
    );
    let gh = fake_gh();
    let prs = format!(
        r#"[
        {{"number": 7, "title": "widgets: the fix",
          "url": "https://github.com/acme/widgets/pull/7",
          "headRefOid": "abc", "headRefName": "fix",
          "updatedAt": "{}",
          "statusCheckRollup": [
            {{"__typename":"CheckRun","name":"test","status":"COMPLETED","conclusion":"SUCCESS"}},
            {{"__typename":"StatusContext","context":"qa-verdict","state":"SUCCESS"}}
          ]}},
        {{"number": 9, "title": "wip thing",
          "url": "https://github.com/acme/widgets/pull/9",
          "headRefOid": "def", "headRefName": "wip",
          "updatedAt": "{}",
          "statusCheckRollup": [
            {{"__typename":"CheckRun","name":"test","status":"COMPLETED","conclusion":"SUCCESS"}}
          ]}}
        ]"#,
        iso_ago(7200),
        iso_ago(3 * 3600)
    );
    let path = gh_path(&gh);
    let log = gh.log.to_str().unwrap().to_string();
    let (ok, v) = cli_env(
        pm.path(),
        state.path(),
        &["overview", "--json"],
        &[
            ("PATH", path.as_str()),
            ("FAKE_GH_LOG", log.as_str()),
            ("FAKE_GH_PRS", prs.as_str()),
        ],
    );
    assert!(ok, "{v}");
    let needs = v["needs_me"].as_array().unwrap();
    // The merge-ready PR leads: exact command, link, project, age.
    assert_eq!(needs[0]["kind"], "merge", "{needs:?}");
    assert_eq!(
        needs[0]["command"],
        "gh pr merge 7 --repo acme/widgets --squash --admin --match-head-commit abc",
        "{needs:?}"
    );
    assert_eq!(needs[0]["link"], "https://github.com/acme/widgets/pull/7");
    assert_eq!(needs[0]["project"], "cadence");
    assert!(needs[0]["age"].as_i64().unwrap_or(0) >= 7000);
    // The verdict-less PR follows with its age and the view command.
    assert_eq!(needs[1]["kind"], "pr_no_verdict", "{needs:?}");
    assert_eq!(needs[1]["command"], "gh pr view 9 --repo acme/widgets");
    assert!(needs[1]["age"].as_i64().unwrap_or(0) >= 10000);
    assert_eq!(v["github"]["state"], "ok");
    // gh was asked exactly once per repo for each of the three queries
    // (PRs, default branch, ci.yml runs) — never the legacy status API.
    let calls = std::fs::read_to_string(&gh.log).unwrap();
    assert_eq!(calls.lines().count(), 3, "{calls}");
    assert!(
        calls.lines().any(|c| c
            == "api repos/acme/widgets/actions/workflows/ci.yml/runs?branch=main&event=push&per_page=30"),
        "{calls}"
    );
    assert!(!calls.contains("/status"), "{calls}");
}

#[test]
fn overview_github_failure_degrades_not_fails() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let repo = repo_with_remote("https://github.com/acme/widgets.git");
    assert!(cli(pm.path(), state.path(), &["issue", "init"]).0);
    let repo_s = repo.path().to_str().unwrap().to_string();
    assert!(
        cli(
            pm.path(),
            state.path(),
            &["issue", "project", "add", "cadence", "--prefix", "CAD", "--repo", &repo_s]
        )
        .0
    );
    // A review issue still surfaces when GitHub is out.
    assert!(
        cli(
            pm.path(),
            state.path(),
            &["issue", "new", "review me", "--project", "cadence"]
        )
        .0
    );
    assert!(
        cli(
            pm.path(),
            state.path(),
            &["issue", "set", "CAD-1", "status=review"]
        )
        .0
    );
    let gh = fake_gh();
    let path = gh_path(&gh);
    let log = gh.log.to_str().unwrap().to_string();
    let (ok, v) = cli_env(
        pm.path(),
        state.path(),
        &["overview", "--json"],
        &[
            ("PATH", path.as_str()),
            ("FAKE_GH_LOG", log.as_str()),
            ("FAKE_GH_FAIL", "1"),
            ("FAKE_GH_PRS", "[]"),
        ],
    );
    assert!(ok, "{v}");
    assert_eq!(v["github"]["state"], "unavailable", "{v}");
    let needs = v["needs_me"].as_array().unwrap();
    assert!(!needs.iter().any(|n| n["kind"] == "merge"));
    assert!(
        needs.iter().any(|n| n["kind"] == "review_no_pr"),
        "{needs:?}"
    );
}

#[test]
fn overview_tracker_items_and_tracker_behind() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    seed(pm.path(), state.path());
    // seed: CAD-1 container (derived doing via child CAD-2 doing),
    // CAD-3 sibling leaf. Review must go on a leaf — container status
    // rolls up from children.
    assert!(
        cli(
            pm.path(),
            state.path(),
            &["issue", "set", "CAD-3", "status=review"]
        )
        .0
    );
    // blocked_ready: CAD-5 blocked_by CAD-4, and CAD-4 is done.
    assert!(
        cli(
            pm.path(),
            state.path(),
            &["issue", "new", "blocker", "--project", "cadence"]
        )
        .0
    );
    assert!(
        cli(
            pm.path(),
            state.path(),
            &["issue", "new", "blocked work", "--project", "cadence"]
        )
        .0
    );
    assert!(
        cli(
            pm.path(),
            state.path(),
            &["issue", "link", "CAD-5", "blocked_by", "CAD-4"]
        )
        .0
    );
    assert!(
        cli(
            pm.path(),
            state.path(),
            &["issue", "set", "CAD-4", "status=done"]
        )
        .0
    );
    // tracker_behind: an upstream clone with one extra commit.
    let upstream = TempDir::new().unwrap();
    let (ok, _) = git(
        upstream.path(),
        &["clone", "-q", pm.path().to_str().unwrap(), "."],
    );
    assert!(ok);
    assert!(git(upstream.path(), &["config", "user.email", "t@t"]).0);
    assert!(git(upstream.path(), &["config", "user.name", "t"]).0);
    std::fs::write(upstream.path().join("extra.md"), "x").unwrap();
    assert!(git(upstream.path(), &["add", "extra.md"]).0);
    assert!(git(upstream.path(), &["commit", "-qm", "upstream commit"]).0);
    let branch = git(pm.path(), &["rev-parse", "--abbrev-ref", "HEAD"]).1;
    assert!(
        git(
            pm.path(),
            &["remote", "add", "origin", upstream.path().to_str().unwrap()]
        )
        .0
    );
    assert!(git(pm.path(), &["fetch", "-q", "origin"]).0);
    assert!(
        git(
            pm.path(),
            &[
                "branch",
                "--set-upstream-to",
                &format!("origin/{branch}"),
                &branch
            ]
        )
        .0
    );

    let (ok, v) = cli(pm.path(), state.path(), &["overview", "--json"]);
    assert!(ok, "{v}");
    let needs = v["needs_me"].as_array().unwrap();
    let kind = |k: &str| needs.iter().find(|n| n["kind"] == k);
    let review = kind("review_no_pr").expect("review item");
    assert_eq!(review["command"], "cadence issue show CAD-3");
    let unblocked = kind("blocked_ready").expect("unblocked item");
    assert_eq!(unblocked["command"], "cadence issue set CAD-5 status=ready");
    let behind = kind("tracker_behind").expect("behind item");
    assert_eq!(behind["command"], "cadence issue sync");
    // No github remotes declared → no gh work attempted, state "ok".
    assert_eq!(v["github"]["state"], "ok");
    // projects summary counts + oldest review age.
    let proj = v["projects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["key"] == "cadence")
        .expect("cadence project row");
    assert!(proj["oldest_review_age"].as_i64().is_some(), "{proj}");
    assert_eq!(proj["open_by_status"]["review"], 1, "{proj}");
}

#[test]
fn overview_plain_render_shows_sections() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    seed(pm.path(), state.path());
    assert!(
        cli(
            pm.path(),
            state.path(),
            &["issue", "set", "CAD-3", "status=review"]
        )
        .0
    );
    let (ok, out) = cli_raw(pm.path(), state.path(), &["overview"]);
    assert!(ok, "{out}");
    assert!(out.contains("NEEDS ME"), "{out}");
    assert!(out.contains("DRIFT"), "{out}");
    assert!(out.contains("PROJECTS"), "{out}");
    assert!(out.contains("review_no_pr"), "{out}");
    assert!(out.contains("cadence issue show CAD-3"), "{out}");
}

#[test]
fn version_reports_build_identity() {
    let out = Command::new(bin()).arg("--version").output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert_eq!(
        text,
        format!(
            "cadence {}+{}",
            env!("CARGO_PKG_VERSION"),
            env!("CADENCE_BUILD_COMMIT")
        ),
        "{text}"
    );
}

/// A fake daemon on `<state>/cadence.sock` — answers `health`,
/// `agent_list`, `agent_show`, `agent_requests`, `agent_probe`, and
/// rejects `daemon_info` like a build that predates the RPC. Returns
/// the listener thread's stop flag + join handle.
fn fake_daemon_no_info(
    state: &Path,
    agents: Value,
) -> (
    std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread::JoinHandle<()>,
) {
    use std::io::BufRead;
    use std::os::unix::net::UnixListener;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    std::fs::create_dir_all(state).unwrap();
    let listener = UnixListener::bind(state.join("cadence.sock")).unwrap();
    listener.set_nonblocking(true).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let handle = thread::spawn(move || {
        while !flag.load(Ordering::Relaxed) {
            let Ok((mut conn, _)) = listener.accept() else {
                thread::sleep(Duration::from_millis(5));
                continue;
            };
            let mut line = String::new();
            std::io::BufReader::new(&conn)
                .read_line(&mut line)
                .unwrap_or_default();
            let method = serde_json::from_str::<Value>(&line)
                .ok()
                .and_then(|r| r["method"].as_str().map(str::to_string))
                .unwrap_or_default();
            let result = match method.as_str() {
                "health" => json!({"ok": true, "result": {"pid": 1}}),
                "daemon_info" => json!({
                    "ok": false,
                    "error": {"kind": "rejected", "message": "Unknown method 'daemon_info'"}
                }),
                "agent_list" => {
                    json!({"ok": true, "result": {"agents": agents.clone()}})
                }
                "agent_show" => json!({"ok": true, "result": {"messages": [], "queued": 0}}),
                "agent_requests" => json!({"ok": true, "result": {"requests": []}}),
                "agent_probe" => json!({"ok": true, "result": {"idle": true}}),
                _ => json!({
                    "ok": false,
                    "error": {"kind": "rejected", "message": "Unknown method"}
                }),
            };
            let _ = writeln!(conn, "{result}");
        }
    });
    (stop, handle)
}

#[test]
fn overview_daemon_without_daemon_info_stays_reachable() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    // A fenced worker agent — reachable daemon rows must still appear
    // even though `daemon_info` is unknown to this old build.
    let epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
        - 300.0;
    let agents = json!([{
        "alias": "w1", "state": "attention", "provider": "devin",
        "endpoint_kind": "ws", "updated": epoch
    }]);
    let (stop, handle) = fake_daemon_no_info(state.path(), agents);
    let (ok, v) = cli(pm.path(), state.path(), &["overview", "--json"]);
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = handle.join();
    assert!(ok, "{v}");
    assert_eq!(v["daemon"]["reachable"], true, "{v}");
    let needs = v["needs_me"].as_array().unwrap();
    let fenced = needs
        .iter()
        .find(|n| n["kind"] == "fenced")
        .expect("fenced row from a reachable old daemon");
    assert_eq!(fenced["command"], "cadence agent unfence w1");
    // Build identity unreadable → drift explains instead of guessing.
    let reason = v["drift"]["reason"].as_str().unwrap_or("");
    assert!(reason.contains("predates daemon_info"), "{v}");
    assert_eq!(v["drift"]["matched"], false, "{v}");
}

#[test]
fn overview_review_suppressed_by_branch_match() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let repo = repo_with_remote("https://github.com/acme/widgets.git");
    assert!(cli(pm.path(), state.path(), &["issue", "init"]).0);
    let repo_s = repo.path().to_str().unwrap().to_string();
    assert!(
        cli(
            pm.path(),
            state.path(),
            &["issue", "project", "add", "cadence", "--prefix", "CAD", "--repo", &repo_s]
        )
        .0
    );
    assert!(
        cli(
            pm.path(),
            state.path(),
            &["issue", "new", "review me", "--project", "cadence"]
        )
        .0
    );
    assert!(
        cli(
            pm.path(),
            state.path(),
            &["issue", "set", "CAD-1", "status=review"]
        )
        .0
    );
    // CAD-1 has no `pr` ref — but an open PR on `cadence/cad-1-…`
    // branch counts as its PR.
    let gh = fake_gh();
    let prs = format!(
        r#"[{{"number": 9, "title": "cad-1 work",
           "url": "https://github.com/acme/widgets/pull/9",
           "headRefOid": "def", "headRefName": "cadence/cad-1-review",
           "updatedAt": "{}", "statusCheckRollup": []}}]"#,
        iso_ago(300)
    );
    let path = gh_path(&gh);
    let log = gh.log.to_str().unwrap().to_string();
    let (ok, v) = cli_env(
        pm.path(),
        state.path(),
        &["overview", "--json"],
        &[
            ("PATH", path.as_str()),
            ("FAKE_GH_LOG", log.as_str()),
            ("FAKE_GH_PRS", prs.as_str()),
        ],
    );
    assert!(ok, "{v}");
    let needs = v["needs_me"].as_array().unwrap();
    assert!(
        !needs.iter().any(|n| n["kind"] == "review_no_pr"),
        "{needs:?}"
    );
}

#[test]
fn overview_empty_slug_set_keeps_cached_rows() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let repo = repo_with_remote("https://github.com/acme/widgets.git");
    assert!(cli(pm.path(), state.path(), &["issue", "init"]).0);
    let repo_s = repo.path().to_str().unwrap().to_string();
    assert!(
        cli(
            pm.path(),
            state.path(),
            &["issue", "project", "add", "cadence", "--prefix", "CAD", "--repo", &repo_s]
        )
        .0
    );
    // Run 1: a good fetch writes the slug-keyed cache.
    let gh = fake_gh();
    let prs = format!(
        r#"[{{"number": 9, "title": "wip thing",
           "url": "https://github.com/acme/widgets/pull/9",
           "headRefOid": "def", "headRefName": "wip",
           "updatedAt": "{}", "statusCheckRollup": []}}]"#,
        iso_ago(300)
    );
    let path = gh_path(&gh);
    let log = gh.log.to_str().unwrap().to_string();
    let (ok, v) = cli_env(
        pm.path(),
        state.path(),
        &["overview", "--json"],
        &[
            ("PATH", path.as_str()),
            ("FAKE_GH_LOG", log.as_str()),
            ("FAKE_GH_PRS", prs.as_str()),
        ],
    );
    assert!(ok, "{v}");
    assert_eq!(v["github"]["state"], "ok");
    let cache = state.path().join("overview-gh.json");
    let body: Value = serde_json::from_str(&std::fs::read_to_string(&cache).unwrap()).unwrap();
    assert_eq!(body["slugs"], json!(["acme/widgets"]), "{body}");
    // Run 2: a tracker with no remotes must not stamp over the cache.
    let empty_pm = TempDir::new().unwrap();
    assert!(cli(empty_pm.path(), state.path(), &["issue", "init"]).0);
    let (ok, v) = cli_env(
        empty_pm.path(),
        state.path(),
        &["overview", "--json"],
        &[("PATH", path.as_str()), ("FAKE_GH_LOG", log.as_str())],
    );
    assert!(ok, "{v}");
    assert_eq!(v["github"]["state"], "ok", "{v}");
    let body2: Value = serde_json::from_str(&std::fs::read_to_string(&cache).unwrap()).unwrap();
    assert_eq!(body2["slugs"], json!(["acme/widgets"]), "{body2}");
    assert!(body2["repos"]["acme/widgets"]["prs"].is_array(), "{body2}");
}

// ---------- issue retro ----------

/// Point pm.yaml's `notes_dir` at a scratch dir so the retro never
/// reads the host's real agent-notes.
fn set_notes_dir(pm: &Path, notes: &Path) {
    let file = pm.join("pm.yaml");
    let text = std::fs::read_to_string(&file).unwrap();
    let mut out = String::new();
    for line in text.lines() {
        if line.starts_with("notes_dir:") {
            out.push_str(&format!("notes_dir: {}\n", notes.display()));
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    std::fs::write(&file, out).unwrap();
}

fn write_note(notes: &Path, name: &str, text: &str) {
    std::fs::write(notes.join(name), text).unwrap();
}

#[test]
fn retro_reports_rounds_defects_flakes_and_unknowns() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let notes = TempDir::new().unwrap();
    assert!(cli(pm.path(), state.path(), &["issue", "init"]).0);
    assert!(
        cli(
            pm.path(),
            state.path(),
            &["issue", "project", "add", "cadence", "--prefix", "CAD"]
        )
        .0
    );
    assert!(
        cli(
            pm.path(),
            state.path(),
            &["issue", "new", "the work", "--project", "cadence"]
        )
        .0
    );
    for s in ["ready", "doing", "review"] {
        assert!(
            cli(
                pm.path(),
                state.path(),
                &["issue", "set", "CAD-1", &format!("status={s}")]
            )
            .0
        );
    }
    // A comment mentioning a flake becomes flake evidence.
    assert!(
        cli(
            pm.path(),
            state.path(),
            &[
                "issue",
                "comment",
                "CAD-1",
                "-m",
                "one transient failure on the lock probe, green on rerun",
                "--author",
                "qa-1"
            ]
        )
        .0
    );
    set_notes_dir(pm.path(), notes.path());

    // Header-tagged qa + verdict notes; a kickoff that only matches
    // by filename (no `Issue:` header) — the weaker evidence class.
    write_note(
        notes.path(),
        "20260920-100000-60747-cad-1-the-work-kickoff.md",
        "# CAD-1 kickoff\n\nNo Issue header — filename match only.\n",
    );
    write_note(
        notes.path(),
        "20260920-110000-60747-x-qa.md",
        "# QA report\n> Issue: `CAD-1`\n\nRound 1 done; gates green.\n",
    );
    write_note(
        notes.path(),
        "20260920-120000-60747-x-verdict.md",
        "# Verdict: CAD-1\n> Issue: `CAD-1`\n\n## Verdict\n**Blocked.**\n\n## Blocking finding\nThe join matched pid alone — stale rows could manufacture ownership.\n",
    );
    write_note(
        notes.path(),
        "20260920-130000-60747-x-verdict.md",
        "# Verdict: CAD-1 round 2\n> Issue: `CAD-1`\n\n## Verdict\n**Pass.** Ship it.\n",
    );

    let before = commits(pm.path());
    let (ok, v) = cli(
        pm.path(),
        state.path(),
        &["issue", "retro", "CAD-1", "--json"],
    );
    assert!(ok, "{v}");
    // Read-only: the tracker gained no commits.
    assert_eq!(commits(pm.path()), before, "retro must not write");

    assert_eq!(v["schema"], "cadence.retro/1");
    assert_eq!(v["id"], "CAD-1");
    // Two verdict notes = two review rounds; round 1 blocked.
    assert_eq!(v["review"]["rounds"], 2, "{v}");
    let verdicts = v["review"]["verdict_notes"].as_array().unwrap();
    assert_eq!(verdicts[0]["outcome"], "not-pass");
    assert_eq!(verdicts[1]["outcome"], "pass");
    assert_eq!(v["review"]["qa_reports"], 1);
    assert_eq!(v["review"]["kickoffs"], 1);
    assert_eq!(v["review"]["comments"], 1);
    // The filename-only kickoff is labelled with the weaker match.
    let fnote = v["review"]["verdict_notes"]
        .as_array()
        .unwrap()
        .iter()
        .all(|n| n["match"] == "header");
    assert!(fnote, "verdict notes here are all header-tagged: {v}");

    // The blocked round surfaces as a defect with its finding text.
    let defects = v["defects"].as_array().unwrap();
    assert_eq!(defects.len(), 1, "{v}");
    assert!(defects[0]["summary"]
        .as_str()
        .unwrap()
        .contains("join matched pid alone"));
    assert!(defects[0]["source"]
        .as_str()
        .unwrap()
        .contains("verdict.md"));

    // Flake keyword hit from the comment.
    let flakes = v["flakes"].as_array().unwrap();
    assert!(!flakes.is_empty());
    assert!(flakes
        .iter()
        .any(|f| f["text"].as_str().unwrap().contains("transient")));

    // Timings from set-transitions; done_at falls back to the
    // passing-verdict note while no `set status=done` exists.
    assert!(v["timings"]["ready_at"].is_string());
    assert!(v["timings"]["doing_at"].is_string());
    assert!(v["timings"]["review_at"].is_string());
    assert_eq!(v["timings"]["done_at"], "2026-09-20T13:00:00Z");

    // With status=done the tracker commit supplies done_at and the
    // lead time becomes a real (small) number.
    assert!(
        cli(
            pm.path(),
            state.path(),
            &["issue", "set", "CAD-1", "status=done"]
        )
        .0
    );
    let (ok, v) = cli(
        pm.path(),
        state.path(),
        &["issue", "retro", "CAD-1", "--json"],
    );
    assert!(ok, "{v}");
    assert!(v["timings"]["done_at"].is_string());
    assert_ne!(v["timings"]["done_at"], "2026-09-20T13:00:00Z");
    assert!(v["timings"]["lead_hours_ready_to_done"].is_number());

    // Explicit unknowns: merged_at (no code commits), human_minutes,
    // defect classes, and the absent store.
    let unknown_fields: Vec<&str> = v["unknowns"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|u| u["field"].as_str())
        .collect();
    for f in [
        "merged_at",
        "human_minutes",
        "defect_classes",
        "store_verdicts",
    ] {
        assert!(unknown_fields.contains(&f), "missing unknown {f}: {v}");
    }
    assert_eq!(v["sources_state"]["store"], "absent");

    // Lessons are proposed only — promotion text is pinned.
    let lessons = v["proposed_lessons"].as_array().unwrap();
    assert!(!lessons.is_empty());
    assert!(lessons
        .iter()
        .all(|l| l["promotion"].as_str().unwrap().contains("manual")));
}

#[test]
fn retro_minimal_issue_marks_everything_unknown() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let notes = TempDir::new().unwrap();
    assert!(cli(pm.path(), state.path(), &["issue", "init"]).0);
    assert!(
        cli(
            pm.path(),
            state.path(),
            &["issue", "project", "add", "cadence", "--prefix", "CAD"]
        )
        .0
    );
    assert!(
        cli(
            pm.path(),
            state.path(),
            &["issue", "new", "bare", "--project", "cadence"]
        )
        .0
    );
    set_notes_dir(pm.path(), notes.path());
    let (ok, v) = cli(
        pm.path(),
        state.path(),
        &["issue", "retro", "CAD-1", "--json"],
    );
    assert!(ok, "{v}");
    assert_eq!(v["review"]["rounds"], 0);
    assert!(v["defects"].as_array().unwrap().is_empty());
    assert!(v["flakes"].as_array().unwrap().is_empty());
    assert!(v["proposed_lessons"].as_array().unwrap().is_empty());
    // Never invented: every unproven field is a named unknown.
    let fields: Vec<&str> = v["unknowns"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|u| u["field"].as_str())
        .collect();
    for f in [
        "ready_at",
        "doing_at",
        "review_at",
        "done_at",
        "merged_at",
        "human_minutes",
    ] {
        assert!(fields.contains(&f), "missing unknown {f}: {v}");
    }
}

#[test]
fn retro_rejects_unknown_issue_and_stays_read_only() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let notes = TempDir::new().unwrap();
    assert!(cli(pm.path(), state.path(), &["issue", "init"]).0);
    set_notes_dir(pm.path(), notes.path());
    let (ok, err) = cli(
        pm.path(),
        state.path(),
        &["issue", "retro", "CAD-9", "--json"],
    );
    assert!(!ok);
    assert!(err["error"].as_str().unwrap().contains("CAD-9"));
}

#[test]
fn retro_joins_store_verdicts_for_the_issue() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let notes = TempDir::new().unwrap();
    assert!(cli(pm.path(), state.path(), &["issue", "init"]).0);
    assert!(
        cli(
            pm.path(),
            state.path(),
            &["issue", "project", "add", "cadence", "--prefix", "CAD"]
        )
        .0
    );
    assert!(
        cli(
            pm.path(),
            state.path(),
            &["issue", "new", "stored", "--project", "cadence"]
        )
        .0
    );
    set_notes_dir(pm.path(), notes.path());

    // Minimal daemon store: one job bound to CAD-1, one task, two
    // verdicts (a not-pass then a pass). Another job's verdict on a
    // different issue must NOT join.
    let db = state.path().join("cadence.sqlite3");
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch(
        "CREATE TABLE jobs(id TEXT PRIMARY KEY, issue_id TEXT);
         CREATE TABLE tasks(id TEXT PRIMARY KEY, job_id TEXT NOT NULL);
         CREATE TABLE verdicts(seq INTEGER PRIMARY KEY AUTOINCREMENT,
             task_id TEXT NOT NULL, revision INTEGER NOT NULL,
             sha TEXT NOT NULL, verdict TEXT NOT NULL,
             reviewer TEXT NOT NULL, created REAL NOT NULL);
         INSERT INTO jobs VALUES('j1','CAD-1'),('j2','CAD-99');
         INSERT INTO tasks VALUES('t1','j1'),('t2','j2');
         INSERT INTO verdicts(task_id,revision,sha,verdict,reviewer,created)
             VALUES('t1',0,'aaa111','changes-requested','qa-1',1758400000.0),
                   ('t1',1,'bbb222','pass','qa-1',1758403600.0),
                   ('t2',0,'ccc333','fail','qa-1',1758400000.0);",
    )
    .unwrap();
    drop(conn);

    let (ok, v) = cli(
        pm.path(),
        state.path(),
        &["issue", "retro", "CAD-1", "--json"],
    );
    assert!(ok, "{v}");
    assert_eq!(v["sources_state"]["store"], "ok");
    let sv = v["review"]["store_verdicts"].as_array().unwrap();
    assert_eq!(sv.len(), 2, "{sv:?}"); // t2/CAD-99 must not join
    assert_eq!(sv[0]["sha"], "aaa111");
    assert_eq!(sv[0]["verdict"], "changes-requested");
    assert_eq!(sv[1]["verdict"], "pass");
    assert!(sv[0]["at"].as_str().unwrap().starts_with("2025-09-20"));
    assert_eq!(v["review"]["store_failed"], 1);
    // The non-pass store verdict surfaces as a defect with its sha.
    assert!(v["defects"]
        .as_array()
        .unwrap()
        .iter()
        .any(|d| d["summary"].as_str().unwrap().contains("aaa111")));
    // Store verdicts corroborate review rounds too.
    assert!(v["review"]["store_verdicts"]
        .as_array()
        .unwrap()
        .iter()
        .all(|x| x["task"] == "t1"));
}

fn context_repo(repo: &Path, project: &str, document_path: &str, document: &str) -> String {
    assert!(git(repo, &["init", "-q"]).0);
    assert!(git(repo, &["config", "user.email", "context@test"]).0);
    assert!(git(repo, &["config", "user.name", "context-test"]).0);
    let manifest = format!(
        "schema: 1\nproject: {project}\ndocuments:\n  - id: guide\n    kind: index\n    path: {document_path}\n    title: Guide\n    required: true\n    roles: [pm, dev, qa, devops]\n"
    );
    let manifest_path = repo.join("docs/cadence/project-context.yaml");
    std::fs::create_dir_all(manifest_path.parent().unwrap()).unwrap();
    std::fs::write(manifest_path, manifest).unwrap();
    let document_path = repo.join(document_path);
    if let Some(parent) = document_path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(document_path, document).unwrap();
    assert!(git(repo, &["add", "-A"]).0);
    assert!(git(repo, &["commit", "-qm", "context fixture"]).0);
    head(repo)
}

fn add_context_project(pm: &Path, state: &Path, key: &str, prefix: &str, repos: &[&Path]) {
    let mut args = vec!["issue", "project", "add", key, "--prefix", prefix];
    let paths: Vec<String> = repos
        .iter()
        .map(|repo| repo.to_str().unwrap().to_string())
        .collect();
    for path in &paths {
        args.extend(["--repo", path.as_str()]);
    }
    assert!(
        cli(pm, state, &args).0,
        "could not add context project {key}"
    );
}

#[test]
fn project_context_api_scopes_projects_and_pins_revision() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let alpha = TempDir::new().unwrap();
    let beta = TempDir::new().unwrap();
    let alpha_old = context_repo(alpha.path(), "alpha", "docs/alpha.md", "alpha HEAD text\n");
    let beta_head = context_repo(beta.path(), "beta", "docs/beta.md", "beta only text\n");
    assert!(cli(pm.path(), state.path(), &["issue", "init"]).0);
    add_context_project(pm.path(), state.path(), "alpha", "A", &[alpha.path()]);
    add_context_project(pm.path(), state.path(), "beta", "B", &[beta.path()]);
    let (port, _board) = start_ui(pm.path().to_path_buf(), state.path().to_path_buf());
    let host = format!("127.0.0.1:{port}");

    let (code, body) = http(port, "GET", "/api/projects/alpha/context?role=pm", &host);
    assert_eq!(code, 200, "{body}");
    let first: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(first["project"], "alpha");
    assert_eq!(first["state"], "ready");
    assert_eq!(first["snapshot"]["head_revision"], alpha_old);
    assert_eq!(first["snapshot"]["revision_state"], "uncompared");
    assert!(!first["snapshot"]["dirty"].as_bool().unwrap());
    assert!(first["documents"][0]["excerpt"]
        .as_str()
        .unwrap()
        .contains("alpha HEAD"));

    // Untracked bytes do not make dirty=true and a tracked edit is still
    // excluded from the pinned HEAD blob.
    std::fs::write(
        alpha.path().join("docs/alpha.md"),
        "alpha working tree only\n",
    )
    .unwrap();
    std::fs::write(alpha.path().join("secret.txt"), "never served\n").unwrap();
    let query = format!("/api/projects/alpha/context?role=pm&expected_revision={alpha_old}");
    let (code, body) = http(port, "GET", &query, &host);
    assert_eq!(code, 200, "{body}");
    let dirty: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(dirty["snapshot"]["head_revision"], alpha_old);
    assert_eq!(dirty["snapshot"]["revision_state"], "current");
    assert!(dirty["snapshot"]["dirty"].as_bool().unwrap());
    assert!(dirty["documents"][0]["excerpt"]
        .as_str()
        .unwrap()
        .contains("alpha HEAD"));
    assert!(!body.contains("secret.txt"));

    assert!(git(alpha.path(), &["add", "docs/alpha.md"]).0);
    assert!(git(alpha.path(), &["commit", "-qm", "advance context"]).0);
    let alpha_new = head(alpha.path());
    assert_ne!(alpha_new, alpha_old);
    let query = format!("/api/projects/alpha/context?role=pm&expected_revision={alpha_old}");
    let (code, body) = http(port, "GET", &query, &host);
    assert_eq!(code, 200, "{body}");
    let stale: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(stale["snapshot"]["revision_state"], "stale");
    assert_eq!(stale["snapshot"]["expected_revision"], alpha_old);
    assert_eq!(stale["snapshot"]["head_revision"], alpha_new);
    assert!(stale["documents"][0]["excerpt"]
        .as_str()
        .unwrap()
        .contains("alpha working"));

    let (code, body) = http(port, "GET", "/api/projects/beta/context?role=pm", &host);
    assert_eq!(code, 200, "{body}");
    let second: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(second["project"], "beta");
    assert_eq!(second["snapshot"]["head_revision"], beta_head);
    assert!(second.to_string().contains("beta only"));
    assert!(!second.to_string().contains("alpha HEAD"));
    assert!(!second.to_string().contains(&alpha_new));

    let (code, _) = http(port, "GET", "/api/projects/unknown/context?role=pm", &host);
    assert_eq!(code, 404);
    let (code, _) = http(
        port,
        "GET",
        "/api/projects/alpha/context?role=pm&unexpected=1",
        &host,
    );
    assert_eq!(code, 400);
    let (code, _) = http(
        port,
        "GET",
        "/api/projects/alpha/context?expected_revision=bad",
        &host,
    );
    assert_eq!(code, 400);
}

/// `devops` is the stored context role. A manifest or caller written
/// before the rename still says `ops`; both spellings select the same
/// documents and the response only ever names `devops`.
#[test]
fn project_context_accepts_ops_and_stores_devops() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    let _ = context_repo(repo.path(), "roles", "docs/guide.md", "guide text\n");
    let manifest = "schema: 1\nproject: roles\ndocuments:\n  - id: guide\n    kind: index\n    path: docs/guide.md\n    title: Guide\n    required: true\n  - id: legacy\n    kind: release\n    path: docs/legacy.md\n    title: Legacy ops document\n    required: false\n    roles: [ops]\n  - id: current\n    kind: validation\n    path: docs/current.md\n    title: Current devops document\n    required: false\n    roles: [devops]\n";
    std::fs::write(
        repo.path().join("docs/cadence/project-context.yaml"),
        manifest,
    )
    .unwrap();
    std::fs::write(repo.path().join("docs/legacy.md"), "legacy text\n").unwrap();
    std::fs::write(repo.path().join("docs/current.md"), "current text\n").unwrap();
    assert!(git(repo.path(), &["add", "-A"]).0);
    assert!(git(repo.path(), &["commit", "-qm", "role fixture"]).0);
    assert!(cli(pm.path(), state.path(), &["issue", "init"]).0);
    add_context_project(pm.path(), state.path(), "roles", "R", &[repo.path()]);
    let (port, _board) = start_ui(pm.path().to_path_buf(), state.path().to_path_buf());
    let host = format!("127.0.0.1:{port}");

    for role in ["devops", "ops"] {
        let query = format!("/api/projects/roles/context?role={role}");
        let (code, body) = http(port, "GET", &query, &host);
        assert_eq!(code, 200, "{body}");
        let value: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(value["state"], "ready", "role={role}: {body}");
        for index in [1, 2] {
            let document = &value["documents"][index];
            assert_eq!(document["selected"], true, "role={role}: {document}");
            assert_eq!(document["selection_reason"], "role:devops", "role={role}");
        }
        assert!(!body.contains("role:ops"), "role={role}: {body}");
    }

    let (code, body) = http(port, "GET", "/api/projects/roles/context?role=dev", &host);
    assert_eq!(code, 200, "{body}");
    let value: Value = serde_json::from_str(&body).unwrap();
    for index in [1, 2] {
        assert_eq!(value["documents"][index]["selected"], false);
        assert_eq!(
            value["documents"][index]["selection_reason"],
            "excluded:role-mismatch"
        );
    }
    let (code, _) = http(
        port,
        "GET",
        "/api/projects/roles/context?role=operations",
        &host,
    );
    assert_eq!(code, 400);
}

#[test]
fn project_context_dirty_probe_failure_keeps_pinned_documents() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    let head = context_repo(
        repo.path(),
        "dirty-probe",
        "docs/probe.md",
        "probe HEAD remains readable\n",
    );

    // status needs the index; immutable HEAD/tree/blob readers do not.
    std::fs::write(repo.path().join(".git/index"), b"not a git index\n").unwrap();
    let (status_ok, status_output) = git(
        repo.path(),
        &["status", "--porcelain=v1", "--untracked-files=no"],
    );
    assert!(
        !status_ok,
        "corrupted index unexpectedly passed status: {status_output}"
    );
    let (head_ok, observed_head) = git(repo.path(), &["rev-parse", "--verify", "HEAD"]);
    assert!(head_ok);
    assert_eq!(observed_head.trim(), head);
    let (tree_ok, tree) = git(repo.path(), &["ls-tree", &head, "--", "docs/probe.md"]);
    assert!(tree_ok);
    assert!(tree.contains("docs/probe.md"));
    let (blob_ok, blob) = git(repo.path(), &["cat-file", "blob", "HEAD:docs/probe.md"]);
    assert!(blob_ok);
    assert!(blob.contains("probe HEAD remains readable"));

    assert!(cli(pm.path(), state.path(), &["issue", "init"]).0);
    add_context_project(pm.path(), state.path(), "dirty-probe", "D", &[repo.path()]);
    let (port, _board) = start_ui(pm.path().to_path_buf(), state.path().to_path_buf());
    let host = format!("127.0.0.1:{port}");
    let (code, body) = http(port, "GET", "/api/projects/dirty-probe/context", &host);
    assert_eq!(code, 200, "{body}");
    let value: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(value["snapshot"]["dirty"], Value::Null);
    assert!(!value["snapshot"]["error"]
        .as_str()
        .unwrap_or_default()
        .is_empty());
    assert_eq!(value["snapshot"]["head_revision"], head);
    assert_ne!(value["state"], "unavailable_repository");
    assert!(value["documents"][0]["excerpt"]
        .as_str()
        .unwrap()
        .contains("probe HEAD remains readable"));
}

#[test]
fn project_context_rejects_invalid_paths_and_reports_repository_states() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    let no_manifest = TempDir::new().unwrap();
    let _ = context_repo(repo.path(), "bad", "docs/guide.md", "safe text\n");
    assert!(git(no_manifest.path(), &["init", "-q"]).0);
    assert!(
        git(
            no_manifest.path(),
            &["config", "user.email", "context@test"]
        )
        .0
    );
    assert!(git(no_manifest.path(), &["config", "user.name", "context-test"]).0);
    std::fs::write(no_manifest.path().join("README.md"), "no manifest\n").unwrap();
    assert!(git(no_manifest.path(), &["add", "-A"]).0);
    assert!(git(no_manifest.path(), &["commit", "-qm", "without manifest"]).0);
    assert!(cli(pm.path(), state.path(), &["issue", "init"]).0);
    add_context_project(pm.path(), state.path(), "bad", "BAD", &[repo.path()]);
    add_context_project(
        pm.path(),
        state.path(),
        "no-manifest",
        "N",
        &[no_manifest.path()],
    );
    add_context_project(pm.path(), state.path(), "remote", "REM", &[]);
    add_context_project(
        pm.path(),
        state.path(),
        "multi",
        "M",
        &[repo.path(), outside.path()],
    );
    add_context_project(
        pm.path(),
        state.path(),
        "unavailable",
        "U",
        &[Path::new("/definitely/missing/cadence-context")],
    );
    // `outside` is deliberately not a git repository; the multi-repo state
    // is decided from declarations before either path is opened.
    let (port, _board) = start_ui(pm.path().to_path_buf(), state.path().to_path_buf());
    let host = format!("127.0.0.1:{port}");

    for (key, state_name) in [
        ("no-manifest", "missing"),
        ("remote", "missing_repository"),
        ("multi", "ambiguous_repository"),
        ("unavailable", "unavailable_repository"),
    ] {
        let (code, body) = http(port, "GET", &format!("/api/projects/{key}/context"), &host);
        assert_eq!(code, 200, "{body}");
        let value: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(value["state"], state_name, "{value}");
    }

    // A manifest path escape is retained as metadata and blocked before any
    // Git blob lookup.
    let manifest = "schema: 1\nproject: bad\ndocuments:\n  - id: escape\n    kind: index\n    path: ../outside.md\n    title: Escape\n    required: true\n";
    std::fs::write(
        repo.path().join("docs/cadence/project-context.yaml"),
        manifest,
    )
    .unwrap();
    assert!(git(repo.path(), &["add", "-A"]).0);
    assert!(git(repo.path(), &["commit", "-qm", "escape path"]).0);
    let (code, body) = http(port, "GET", "/api/projects/bad/context", &host);
    assert_eq!(code, 200, "{body}");
    let escaped: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(escaped["state"], "conflict");
    assert_eq!(escaped["documents"][0]["state"], "invalid_path");
    assert!(escaped["documents"][0].get("excerpt").is_none());

    // A valid manifest still reports each missing required document exactly.
    std::fs::write(
        repo.path().join("docs/cadence/project-context.yaml"),
        "schema: 1\nproject: bad\ndocuments:\n  - id: architecture\n    kind: architecture\n    path: docs/ARCHITECTURE.md\n    title: Architecture\n    required: true\n",
    )
    .unwrap();
    std::fs::remove_file(repo.path().join("docs/link.md")).ok();
    assert!(git(repo.path(), &["add", "-A"]).0);
    assert!(git(repo.path(), &["commit", "-qm", "missing architecture"]).0);
    let (code, body) = http(port, "GET", "/api/projects/bad/context", &host);
    assert_eq!(code, 200, "{body}");
    let missing: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(missing["state"], "missing");
    assert_eq!(missing["documents"][0]["state"], "missing");

    // A tracked document over the blob bound is reported without serving a
    // prefix of its bytes.
    std::fs::write(
        repo.path().join("docs/cadence/project-context.yaml"),
        "schema: 1\nproject: bad\ndocuments:\n  - id: huge\n    kind: spec\n    path: docs/huge.md\n    title: Huge\n    required: true\n",
    )
    .unwrap();
    std::fs::write(repo.path().join("docs/huge.md"), vec![b'x'; 65 * 1024]).unwrap();
    assert!(git(repo.path(), &["add", "-A"]).0);
    assert!(git(repo.path(), &["commit", "-qm", "oversized document"]).0);
    let (code, body) = http(port, "GET", "/api/projects/bad/context", &host);
    assert_eq!(code, 200, "{body}");
    let huge: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(huge["state"], "too_large");
    assert_eq!(huge["documents"][0]["state"], "too_large");
    assert!(huge["documents"][0].get("excerpt").is_none());

    // A tracked symlink is rejected even though its target is a document.
    std::fs::write(
        repo.path().join("docs/cadence/project-context.yaml"),
        "schema: 1\nproject: bad\ndocuments:\n  - id: link\n    kind: index\n    path: docs/link.md\n    title: Link\n    required: true\n",
    )
    .unwrap();
    std::fs::write(
        repo.path().join("docs/real.md"),
        "target bytes must not appear\n",
    )
    .unwrap();
    std::os::unix::fs::symlink("real.md", repo.path().join("docs/link.md")).unwrap();
    assert!(git(repo.path(), &["add", "-A"]).0);
    assert!(git(repo.path(), &["commit", "-qm", "symlink path"]).0);
    let (code, body) = http(port, "GET", "/api/projects/bad/context", &host);
    assert_eq!(code, 200, "{body}");
    let linked: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(linked["state"], "unreadable");
    assert_eq!(linked["documents"][0]["state"], "unreadable");
    assert!(!body.contains("target bytes must not appear"));
}

#[test]
fn project_context_manifest_conflicts_are_bounded_and_select_nothing() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    let _ = context_repo(repo.path(), "conflicts", "docs/guide.md", "guide\n");
    assert!(cli(pm.path(), state.path(), &["issue", "init"]).0);
    add_context_project(pm.path(), state.path(), "conflicts", "C", &[repo.path()]);
    let (port, _board) = start_ui(pm.path().to_path_buf(), state.path().to_path_buf());
    let host = format!("127.0.0.1:{port}");
    let replace_manifest = |manifest: &str, message: &str| -> Value {
        std::fs::write(
            repo.path().join("docs/cadence/project-context.yaml"),
            manifest,
        )
        .unwrap();
        assert!(git(repo.path(), &["add", "-A"]).0);
        assert!(git(repo.path(), &["commit", "-qm", message]).0);
        let (code, body) = http(port, "GET", "/api/projects/conflicts/context", &host);
        assert_eq!(code, 200, "{body}");
        serde_json::from_str(&body).unwrap()
    };

    let malformed = replace_manifest("schema: [", "malformed context manifest");
    assert_eq!(malformed["state"], "conflict");
    assert_eq!(malformed["documents"].as_array().unwrap().len(), 0);

    let mismatched = replace_manifest(
        "schema: 1\nproject: another\ndocuments:\n  - id: guide\n    kind: index\n    path: docs/guide.md\n    title: Guide\n    required: true\n",
        "mismatched context manifest",
    );
    assert_eq!(mismatched["state"], "conflict");
    assert_eq!(mismatched["documents"][0]["selected"], false);
    assert!(mismatched["documents"][0].get("excerpt").is_none());

    let duplicate = replace_manifest(
        "schema: 1\nproject: conflicts\ndocuments:\n  - id: guide\n    kind: index\n    path: docs/guide.md\n    title: Guide\n    required: true\n  - id: guide\n    kind: index\n    path: docs/guide.md\n    title: Duplicate\n    required: false\n",
        "duplicate context manifest",
    );
    assert_eq!(duplicate["state"], "conflict");
    assert!(duplicate["manifest"]["errors"]
        .as_array()
        .unwrap()
        .iter()
        .any(|error| error.as_str().unwrap().contains("duplicate")));
    assert!(duplicate["documents"]
        .as_array()
        .unwrap()
        .iter()
        .all(|document| document["selected"] == false));

    let mut oversized = String::from("schema: 1\nproject: conflicts\ndocuments:\n");
    for index in 0..33 {
        oversized.push_str(&format!(
            "  - id: doc-{index}\n    kind: spec\n    path: docs/doc-{index}.md\n    title: Document {index}\n    required: false\n"
        ));
    }
    let oversized = replace_manifest(&oversized, "oversized context manifest");
    assert_eq!(oversized["state"], "conflict");
    assert_eq!(oversized["manifest"]["entry_count"], 33);
    assert_eq!(oversized["manifest"]["entries_omitted"], 1);
    assert_eq!(oversized["documents"].as_array().unwrap().len(), 32);
    assert!(oversized["documents"]
        .as_array()
        .unwrap()
        .iter()
        .all(|document| document["selected"] == false));
    assert!(!oversized.to_string().contains("doc-32.md"));
}

#[test]
fn project_context_memories_include_only_verified_lessons_and_bound_withheld() {
    let pm_dir = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    let _ = context_repo(repo.path(), "memory", "docs/guide.md", "memory context\n");
    assert!(cli(pm_dir.path(), state.path(), &["issue", "init"]).0);
    add_context_project(pm_dir.path(), state.path(), "memory", "M", &[repo.path()]);
    let pm = cadence_agent::issue::Pm::at(pm_dir.path()).unwrap();
    let identity =
        |alias: &str, registration: u64, role: &str| cadence_agent::memory::NativeIdentity {
            proof: cadence_agent::memory::IdentityProof {
                alias: alias.to_string(),
                registration,
                generation: format!("{alias}-generation"),
                process_start: registration,
                role: role.to_string(),
            },
        };
    let author = identity("author", 1, "worker");
    let reviewer_a = identity("reviewer-a", 2, "worker");
    let reviewer_b = identity("reviewer-b", 3, "worker");
    let finalizer = identity("pm", 4, "pm");
    let scope = cadence_agent::memory::Scope {
        paths: vec!["docs/**".to_string()],
        ..Default::default()
    };
    let proposed = cadence_agent::memory::propose_native(
        &pm,
        "memory",
        "rule",
        &scope,
        Some("CAD-224"),
        Some("high"),
        None,
        Some("verified memory fact\n\n**Why:** fixture\n\n**How to apply:** use it\n"),
        Some("verified"),
        &author,
    )
    .unwrap();
    let digest = proposed["digest"].as_str().unwrap().to_string();
    for reviewer in [&reviewer_a, &reviewer_b] {
        cadence_agent::memory::submit_review(
            &pm,
            Some("memory"),
            "verified",
            &cadence_agent::memory::ReviewRequest {
                operation: "accept",
                verdict: "pass",
                evidence: "reviewed fixture",
                expected_digest: &digest,
            },
            reviewer,
        )
        .unwrap();
    }
    cadence_agent::memory::finalize_native(
        &pm,
        Some("memory"),
        "verified",
        "accept",
        &digest,
        &finalizer,
    )
    .unwrap();
    cadence_agent::memory::propose_native(
        &pm,
        "memory",
        "rule",
        &scope,
        Some("CAD-224"),
        Some("high"),
        None,
        Some("proposed claim must be withheld\n\n**Why:** fixture\n\n**How to apply:** never serve\n"),
        Some("proposed"),
        &author,
    )
    .unwrap();
    std::fs::create_dir_all(pm_dir.path().join("memory/memory")).unwrap();
    std::fs::write(
        pm_dir.path().join("memory/memory/broken.md"),
        "this is not a memory document\n",
    )
    .unwrap();

    let (port, _board) = start_ui(pm_dir.path().to_path_buf(), state.path().to_path_buf());
    let host = format!("127.0.0.1:{port}");
    let (code, body) = http(port, "GET", "/api/projects/memory/context", &host);
    assert_eq!(code, 200, "{body}");
    let value: Value = serde_json::from_str(&body).unwrap();
    let ids: Vec<&str> = value["memories"]["included"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item["id"].as_str())
        .collect();
    assert!(ids.contains(&"verified"), "{value}");
    assert!(!ids.contains(&"proposed"));
    assert!(value["memories"]["withheld"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["id"] == "proposed"));
    assert!(value["memories"]["load_errors_total"].as_u64().unwrap() >= 1);
    assert!(value["memories"]["load_errors"]
        .as_array()
        .unwrap()
        .iter()
        .any(|error| error.as_str().unwrap().contains("broken.md")));
    assert!(value["memories"]["lessons"].as_str().unwrap().len() <= 4 * 1024);
}

#[test]
fn model_defaults_http_round_trip_guards_and_conflict() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let (port, _board) = start_ui(pm.path().to_path_buf(), state.path().to_path_buf());
    let host = format!("127.0.0.1:{port}");
    let (code, body) = http(port, "GET", "/api/settings/model-defaults", &host);
    assert_eq!(code, 503, "{body}");
    let missing: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(missing["code"], "daemon_unavailable");

    let d = UiDaemon::start();
    let pm = TempDir::new().unwrap();
    // The write relays through the board's own daemon connection, which
    // the operator gate proves (CAD-337) — an operator-shaped board, so
    // this passes from an agent pane too (CAD-380).
    let (port, _ui) = start_operator_ui(pm.path(), &d.state());
    let host = format!("127.0.0.1:{port}");
    // CAD-313: the operator's writes carry a session.
    let op = sign_in(&d.state(), port);
    let write_json = |port: u16, method: &str, path: &str, host: &str, body: &str| {
        op_write_json(&op, port, method, path, host, body)
    };
    let (code, body) = http(port, "GET", "/api/settings/model-defaults", &host);
    assert_eq!(code, 200, "{body}");
    let snap: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(snap["revision"], 0);
    assert_eq!(snap["read_only"], false);
    assert!(snap["config"]["providers"].as_object().unwrap().is_empty());
    let devin = snap["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == "devin")
        .unwrap();
    assert_eq!(devin["eligible"], false);
    assert!(devin["limitation"].as_str().unwrap().contains("Devin"));
    assert!(snap["providers"]
        .as_array()
        .unwrap()
        .iter()
        .all(|row| row["id"] != "inbox" && row["id"] != "fake"));

    // CAD-313: an unlisted write fails closed as operator-only — 403;
    // with the operator's session it reaches the route's 405.
    let (code, _, _) = http_write(
        port,
        "DELETE",
        "/api/settings/model-defaults",
        &host,
        &[],
        b"{}",
    );
    assert_eq!(code, 403);
    let (code, _, _) = op_http_write(
        &op,
        port,
        "DELETE",
        "/api/settings/model-defaults",
        &host,
        WRITE_HEADERS,
        b"{}",
    );
    assert_eq!(code, 405);
    let (code, _, _) = http_write(
        port,
        "POST",
        "/api/settings/model-defaults",
        "evil.example",
        WRITE_HEADERS,
        b"{}",
    );
    assert_eq!(code, 421);
    let (code, _, body) = http_write(
        port,
        "POST",
        "/api/settings/model-defaults",
        &host,
        &["Content-Type: text/plain", "X-Cadence-Board: 1"],
        b"{}",
    );
    assert_eq!(code, 403, "{body}");
    let (code, _, body) = http_write(
        port,
        "POST",
        "/api/settings/model-defaults",
        &host,
        &["Content-Type: application/json"],
        b"{}",
    );
    assert_eq!(code, 403, "{body}");

    let doc = r#"{"expected_revision":0,"config":{"schema":1,"providers":{"claude":{"default":{"mode":"model","model":"baseline-a"},"roles":{"qa":{"mode":"provider_default"}}}}}}"#;
    // An agent-shaped board — its own environment carries CADENCE_ALIAS
    // — is refused by the daemon's gate, however the suite is run, and
    // nothing is written.
    let (agent_port, _agent_ui) =
        spawn_ui_env(pm.path(), &d.state(), &[("CADENCE_ALIAS", "board-agent")]);
    let agent_host = format!("127.0.0.1:{agent_port}");
    let agent_op = sign_in(&d.state(), agent_port);
    let (code, _, body) = op_write_json(
        &agent_op,
        agent_port,
        "POST",
        "/api/settings/model-defaults",
        &agent_host,
        doc,
    );
    assert_eq!(code, 400, "{body}");
    assert!(
        body.contains("not provably the operator") && body.contains("carries CADENCE_ALIAS"),
        "{body}"
    );
    let (code, body) = http(port, "GET", "/api/settings/model-defaults", &host);
    assert_eq!(code, 200, "{body}");
    assert_eq!(serde_json::from_str::<Value>(&body).unwrap()["revision"], 0);

    let (code, _, body) = write_json(port, "POST", "/api/settings/model-defaults", &host, doc);
    assert_eq!(code, 200, "{body}");
    let saved: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(saved["revision"], 1);
    assert_eq!(
        saved["config"]["providers"]["claude"]["default"]["model"],
        "baseline-a"
    );
    assert_eq!(saved["read_only"], false);

    let (code, _, body) = write_json(port, "POST", "/api/settings/model-defaults", &host, doc);
    assert_eq!(code, 409, "{body}");
    let conflict: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(conflict["code"], "revision_conflict");
    assert_eq!(conflict["revision"], 1);
    let (code, body) = http(port, "GET", "/api/settings/model-defaults", &host);
    let current: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(code, 200);
    assert_eq!(current["revision"], 1);
    assert_eq!(
        current["config"]["providers"]["claude"]["default"]["model"],
        "baseline-a"
    );

    let duplicate = r#"{"expected_revision":1,"config":{"schema":1,"providers":{}},"config":{"schema":1,"providers":{"claude":{"default":{"mode":"provider_default"},"roles":{}}}}}"#;
    let (code, _, body) = write_json(
        port,
        "POST",
        "/api/settings/model-defaults",
        &host,
        duplicate,
    );
    assert_eq!(code, 400, "{body}");
    let rejected: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(rejected["code"], "invalid_config");
    assert_eq!(
        serde_json::from_str::<Value>(&http(port, "GET", "/api/settings/model-defaults", &host).1)
            .unwrap()["revision"],
        1
    );

    let mut huge = vec![b' '; cadence_agent::model_defaults::MAX_HTTP_BODY_BYTES + 1];
    huge[0] = b'{';
    let (code, _, body) = write_json(
        port,
        "POST",
        "/api/settings/model-defaults",
        &host,
        std::str::from_utf8(&huge).unwrap(),
    );
    assert_eq!(code, 400, "{body}");
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap()["code"],
        "invalid_request"
    );

    d.rpc(
        "agent_register",
        json!({"alias": "box", "provider": "inbox", "endpoint_kind": "inbox", "team_role": "ops", "role": "worker"}),
    );
    let (code, body) = http(port, "GET", "/api/agents", &host);
    assert_eq!(code, 200, "{body}");
    let agents: Value = serde_json::from_str(&body).unwrap();
    let row = agents["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|agent| agent["alias"] == "box")
        .unwrap();
    assert_eq!(row["role"], "worker");
    assert_eq!(row["team_role"], "devops");
    assert!(row["model_selection"].is_null());
    assert!(row.get("model_lookup_role").is_some());

    let (read_only, _ro_board) = start_ui_opts(pm.path().to_path_buf(), d.state(), |opts| {
        opts.read_only = true;
    });
    let read_host = format!("127.0.0.1:{read_only}");
    let (code, body) = http(read_only, "GET", "/api/settings/model-defaults", &read_host);
    assert_eq!(code, 200, "{body}");
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap()["read_only"],
        true
    );
    let (code, _, body) = write_json(
        read_only,
        "POST",
        "/api/settings/model-defaults",
        &read_host,
        doc,
    );
    assert_eq!(code, 403, "{body}");
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap()["check"],
        "read_only"
    );
}

// --- CAD-327: `/api/setup` is the operator's, on the host ---

/// A read-only board and a request through the tailnet (proven or not)
/// are refused with 403 before anything runs — no provider probe is
/// spawned for a viewer.
#[test]
fn setup_is_refused_to_read_only_and_tailnet_viewers() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let runs = ui::setup_runs();

    let (ro, _ro_board) = start_ui_opts(pm.path().to_path_buf(), state.path().to_path_buf(), |o| {
        o.read_only = true;
    });
    let (code, _, body) = http_write(
        ro,
        "GET",
        "/api/setup",
        &format!("127.0.0.1:{ro}"),
        &[],
        b"",
    );
    assert_eq!(code, 403, "{body}");
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap()["check"],
        "read_only"
    );

    let nowhere = TempDir::new().unwrap().path().join("absent.sock");
    let (ts, _ts_board) = start_ui_opts(
        pm.path().to_path_buf(),
        state.path().to_path_buf(),
        tailnet_opts(&nowhere),
    );
    for headers in [vec![], vec!["Tailscale-User-Login: operator@example.com"]] {
        let (code, _, body) = http_write(
            ts,
            "GET",
            "/api/setup?fresh=1",
            &format!("{TS_DNS}:9450"),
            &headers,
            b"",
        );
        assert_eq!(code, 403, "{headers:?}: {body}");
        assert_eq!(
            serde_json::from_str::<Value>(&body).unwrap()["check"],
            "tailnet"
        );
    }
    assert_eq!(ui::setup_runs(), runs, "a refused request ran the checks");
}

// --- CAD-313 / CAD-428: operator sessions — adversarial first ---
//
// Each test below names the guard it pins; each was run against the
// code with that guard removed and failed (the PR body lists the
// mutations).

/// The daemon's events of `kind` on its own stream.
fn daemon_events(state: &Path, kind: &str) -> Vec<Value> {
    let conn = rusqlite::Connection::open_with_flags(
        state.join("cadence.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let mut stmt = conn
        .prepare("SELECT payload FROM events WHERE alias=?1 AND kind=?2 ORDER BY seq")
        .unwrap();
    let rows = stmt
        .query_map(rusqlite::params![Store::DAEMON_STREAM, kind], |r| {
            r.get::<_, String>(0)
        })
        .unwrap();
    rows.map(|r| serde_json::from_str(&r.unwrap()).unwrap())
        .collect()
}

/// `/api/meta` as `headers` see it.
fn meta_with(port: u16, host: &str, headers: &[&str]) -> Value {
    let (code, _, body) = http_write(port, "GET", "/api/meta", host, headers, b"");
    assert_eq!(code, 200, "{body}");
    serde_json::from_str(&body).unwrap()
}

fn check_of(body: &str) -> String {
    serde_json::from_str::<Value>(body).unwrap()["check"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// Every byte of every file under `dir`, for "never persisted" checks.
fn all_bytes(dir: &Path) -> Vec<u8> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        let ft = entry.file_type().unwrap();
        if ft.is_dir() {
            out.extend(all_bytes(&path));
        } else if ft.is_file() {
            out.extend(std::fs::read(&path).unwrap_or_default());
        }
    }
    out
}

fn contains(hay: &[u8], needle: &str) -> bool {
    hay.windows(needle.len()).any(|w| w == needle.as_bytes())
}

/// L1, L4, L7, O1, O2 and "exactly once" under concurrency: a login
/// link opens ONE session however many callers race for it; a replay
/// is refused `already_used` and recorded as an alert; the cookie is
/// HttpOnly, SameSite=Strict, host-only, no-store; the session writes
/// as `operator (ui)` and logout ends it. Neither the nonce nor the
/// token is written anywhere under the state dir — the board's own
/// `ui.log` and the daemon store included.
#[test]
fn a_login_link_opens_exactly_one_session_and_leaves_no_trace() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    seed(pm.path(), state.path());
    let d = UiDaemon::start_on(state.path().to_path_buf());
    let (port, _ui) = start_operator_ui(pm.path(), &d.state());
    // Sessions live on the board's own name (CAD-313).
    let host = op::board_host(port);

    let meta = meta_with(port, &host, &[]);
    assert_eq!(meta["signed_in"], false, "{meta}");
    assert_eq!(meta["login_hint"], "cadence ui login", "{meta}");

    let link = op::login_link(bin(), &d.state(), port, &[]).unwrap();
    assert!(
        link.starts_with(&format!("http://cadence-{port}.localhost:{port}/login#n=")),
        "{link}"
    );
    let nonce = op::nonce_of(&link);
    // Eight racers for one link: exactly one session.
    let wins: Vec<(u16, String)> = (0..8)
        .map(|_| {
            let (nonce, host) = (nonce.clone(), host.clone());
            thread::spawn(move || {
                let (code, head, body) = op::exchange(port, &host, &nonce);
                (code, format!("{head}\r\n\r\n{body}"))
            })
        })
        .collect::<Vec<_>>()
        .into_iter()
        .map(|h| h.join().unwrap())
        .collect();
    let won: Vec<&(u16, String)> = wins.iter().filter(|(c, _)| *c == 200).collect();
    assert_eq!(won.len(), 1, "{wins:?}");
    assert!(wins.iter().all(|(c, _)| *c == 200 || *c == 403), "{wins:?}");
    let (head, body) = won[0].1.split_once("\r\n\r\n").unwrap();
    assert!(
        head.to_ascii_lowercase()
            .contains("cache-control: no-store"),
        "{head}"
    );
    let set = op::set_cookie(head).unwrap();
    assert!(
        set.starts_with(&format!("cadence_operator_{port}=")),
        "{set}"
    );
    for attr in ["HttpOnly", "SameSite=Strict", "Path=/", "Max-Age="] {
        assert!(set.contains(attr), "{attr}: {set}");
    }
    assert!(!set.contains("Domain") && !set.contains("Secure"), "{set}");
    let cookie = set.split(';').next().unwrap().to_string();
    let token = cookie.split_once('=').unwrap().1.to_string();
    // The page's second credential rides in the body, never a cookie.
    let key = serde_json::from_str::<Value>(body).unwrap()["session_key"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(key.len(), 64, "{body}");
    assert!(!head.contains(&key), "the key must not be in a header");

    // A replay is refused loudly, and the daemon records the alert.
    let (code, _, body) = op::exchange(port, &host, &nonce);
    assert_eq!(code, 403, "{body}");
    assert_eq!(check_of(&body), "login_link");
    assert!(body.contains("already_used"), "{body}");
    let rejected = daemon_events(&d.state(), "operator_link_rejected");
    assert!(
        rejected
            .iter()
            .any(|e| e["reason"] == "already_used" && e["alert"] == true),
        "{rejected:?}"
    );

    // Signed in: meta says so, and a write is the operator's.
    let session = op::Session {
        host: host.clone(),
        origin: format!("http://{host}"),
        cookie: cookie.clone(),
        set_cookie: set.clone(),
        key: key.clone(),
    };
    let cookie_h = format!("Cookie: {cookie}");
    let key_h = session.key_header();
    // The cookie alone is no session: the page's key is required too.
    assert_eq!(meta_with(port, &host, &[&cookie_h])["signed_in"], false);
    let meta = meta_with(port, &host, &[&cookie_h, &key_h]);
    assert_eq!(meta["signed_in"], true, "{meta}");
    assert_eq!(meta["session"]["origin"], "loopback", "{meta}");
    let (code, _, body) = op_write_json(
        &session,
        port,
        "PATCH",
        "/api/issues/CAD-3",
        &host,
        r#"{"priority":"P1"}"#,
    );
    assert_eq!(code, 200, "{body}");
    let (_, last) = git(pm.path(), &["log", "-1", "--format=%B"]);
    assert!(last.contains("Actor: operator (ui)"), "{last}");

    // Logout ends it, and clears the cookie.
    let (code, head, _) = op::raw(port, &session.request("POST", "/api/session/logout", "{}"));
    assert_eq!(code, 204, "{head}");
    assert!(head.contains("Max-Age=0"), "{head}");
    assert_eq!(
        meta_with(port, &host, &[&cookie_h, &key_h])["signed_in"],
        false
    );
    let (code, _, body) = op_write_json(
        &session,
        port,
        "PATCH",
        "/api/issues/CAD-3",
        &host,
        r#"{"priority":"P2"}"#,
    );
    assert_eq!(code, 403, "{body}");
    assert_eq!(check_of(&body), "operator_session_required");

    // L7: the log is not empty, and holds neither credential; nor does
    // anything else under the state dir.
    let log = std::fs::read_to_string(d.state().join("ui.log")).unwrap();
    assert!(!log.trim().is_empty(), "the board log must be non-empty");
    assert!(!daemon_events(&d.state(), "operator_link_minted").is_empty());
    let everything = all_bytes(&d.state());
    assert!(!contains(&everything, &nonce), "the nonce was persisted");
    assert!(
        !contains(&everything, &token),
        "the session token was persisted"
    );
    assert!(
        !contains(&everything, &key),
        "the session key was persisted"
    );
    assert!(
        !contains(log.as_bytes(), &key),
        "the session key reached the log"
    );
    let secret = std::fs::read_to_string(d.state().join("operator/secret")).unwrap();
    assert!(
        !contains(log.as_bytes(), secret.trim()),
        "the operator secret reached the log"
    );
}

/// L2: a link is good for 120 s, not a second more — by the daemon's
/// clock, advanced instead of slept.
#[test]
fn a_login_link_expires_after_its_ttl() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    seed(pm.path(), state.path());
    let skew = std::sync::Arc::new(std::sync::atomic::AtomicI64::new(0));
    let clock = {
        let skew = skew.clone();
        std::sync::Arc::new(move || {
            time::now_epoch() + skew.load(std::sync::atomic::Ordering::SeqCst)
        })
    };
    let d = UiDaemon::start_with_clock(state.path().to_path_buf(), clock);
    let (port, _board) = start_ui(pm.path().to_path_buf(), d.state());
    let host = op::board_host(port);
    let late = op::nonce_of(&op::login_link(bin(), &d.state(), port, &[]).unwrap());
    let on_time = op::nonce_of(&op::login_link(bin(), &d.state(), port, &[]).unwrap());
    skew.store(121, std::sync::atomic::Ordering::SeqCst);
    let (code, _, body) = op::exchange(port, &host, &late);
    assert_eq!(code, 403, "{body}");
    assert!(body.contains("expired"), "{body}");
    skew.store(119, std::sync::atomic::Ordering::SeqCst);
    let (code, _, body) = op::exchange(port, &host, &on_time);
    assert_eq!(code, 200, "{body}");
}

/// L3 and L5 — a link and a session belong to one origin. A tailnet
/// link is refused on loopback; a loopback link cannot be opened on an
/// unproven tailnet request. A session cookie is honoured only with
/// this request's own `Origin`: none, a foreign one, another allowed
/// board origin, or the tailnet Host all fail; another board's cookie
/// (another port's name) is no session here.
#[test]
fn links_and_sessions_are_bound_to_their_origin() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    seed(pm.path(), state.path());
    let d = UiDaemon::start_on(state.path().to_path_buf());
    let (ts_dir, sock) = fake_localapi();
    let (port, _board) = start_ui_opts(pm.path().to_path_buf(), d.state(), tailnet_opts(&sock));
    localapi_says(ts_dir.path(), Some(true), serve_https_only(port));
    let host = op::board_host(port);
    let plain = format!("127.0.0.1:{port}");
    let ts_host = format!("{TS_DNS}:9450");
    // `ui login --tailnet` reads the persisted sharing block.
    std::fs::write(
        d.state().join("ui.json"),
        json!({"tailscale": {"dns_name": TS_DNS, "https_port": 9450,
                             "target": format!("http://127.0.0.1:{port}")}})
        .to_string(),
    )
    .unwrap();
    let before = commits(pm.path());

    let tailnet = op::login_link(bin(), &d.state(), port, &["--tailnet"]).unwrap();
    assert!(
        tailnet.starts_with(&format!("https://{TS_DNS}:9450/login#n=")),
        "{tailnet}"
    );
    let (code, _, body) = op::exchange(port, &host, &op::nonce_of(&tailnet));
    assert_eq!(code, 403, "{body}");
    assert!(body.contains("wrong_origin"), "{body}");

    let loopback = op::login_link(bin(), &d.state(), port, &[]).unwrap();
    let (code, _, body) = op::raw(
        port,
        &op::request(
            "POST",
            "/api/session",
            &ts_host,
            Some(&format!("https://{TS_DNS}:9450")),
            None,
            &format!(r#"{{"nonce":"{}"}}"#, op::nonce_of(&loopback)),
        ),
    );
    assert_eq!(code, 403, "{body}");
    assert_eq!(check_of(&body), "session_origin");

    let s = sign_in(&d.state(), port);
    let patch = r#"{"priority":"P0"}"#;
    let with = |host: &str, origin: Option<&str>, cookie: &str| {
        // The page's key rides along: only the cookie's origin varies.
        let req = op::request(
            "PATCH",
            "/api/issues/CAD-3",
            host,
            origin,
            Some(cookie),
            patch,
        )
        .replacen(
            "X-Cadence-Board: 1\r\n",
            &format!("X-Cadence-Board: 1\r\n{}\r\n", s.key_header()),
            1,
        );
        op::raw(port, &req)
    };
    let other = format!("http://localhost:{port}");
    for (h, origin, cookie, check) in [
        (host.as_str(), None, s.cookie.as_str(), "origin"),
        (
            host.as_str(),
            Some("http://evil.example"),
            s.cookie.as_str(),
            "origin",
        ),
        (
            host.as_str(),
            Some(other.as_str()),
            s.cookie.as_str(),
            "origin",
        ),
        (
            ts_host.as_str(),
            Some("https://node.tail1234.ts.net:9450"),
            s.cookie.as_str(),
            "operator_session_required",
        ),
    ] {
        let (code, _, body) = with(h, origin, cookie);
        assert_eq!(code, 403, "{h} {origin:?}: {body}");
        assert_eq!(check_of(&body), check, "{h} {origin:?}: {body}");
    }
    // The cookie presented on another loopback Host of this very board
    // (127.0.0.1, where every port's cookies meet) is no session.
    let (code, _, body) = with(&plain, Some(&format!("http://{plain}")), &s.cookie);
    assert_eq!(code, 403, "{body}");
    assert_eq!(check_of(&body), "operator_session_required");
    // Nor can a link be exchanged there.
    let spare = op::login_link(bin(), &d.state(), port, &[]).unwrap();
    let (code, _, body) = op::exchange(port, &plain, &op::nonce_of(&spare));
    assert_eq!(code, 403, "{body}");
    assert_eq!(check_of(&body), "session_origin");
    let foreign = s.cookie.replacen(
        &format!("cadence_operator_{port}="),
        &format!("cadence_operator_{}=", port.wrapping_add(1)),
        1,
    );
    let (code, _, body) = with(&host, Some(&s.origin), &foreign);
    assert_eq!(code, 403, "{body}");
    assert_eq!(check_of(&body), "operator_session_required");
    assert_eq!(commits(pm.path()), before, "no refused write landed");
    // The same session, from its own origin, writes.
    let (code, _, body) = with(&host, Some(&s.origin), &s.cookie);
    assert_eq!(code, 200, "{body}");
}

/// A10, A11 and S2 — only the operator mints. `ui login` from a pane's
/// child, or from a shell carrying an agent's `CADENCE_ALIAS`, is
/// refused by the daemon; so is an operator-shaped caller with a wrong
/// or absent secret; and a loose secret file is refused — by the CLI
/// and by the daemon, even when the right secret is presented — and
/// never repaired. No link is minted by any of them.
#[test]
fn only_the_operator_with_the_secret_mints_a_login_link() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    seed(pm.path(), state.path());
    let d = UiDaemon::start_on(state.path().to_path_buf());
    let (port, _board) = start_ui(pm.path().to_path_buf(), d.state());
    let minted = || daemon_events(&d.state(), "operator_link_minted").len();

    // A pane's child.
    let mut pane = Command::new("bash")
        .args([
            "-c",
            r#"read -r _; "$BIN" --state-dir "$STATE" ui login --json --port "$PORT" 2>&1; true"#,
        ])
        .env("BIN", bin())
        .env("STATE", d.state())
        .env("PORT", port.to_string())
        .env_remove("CADENCE_ALIAS")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    plant_pane(&d, "pane-l", pane.id());
    pane.stdin.take().unwrap().write_all(b"go\n").unwrap();
    let mut said = String::new();
    pane.stdout
        .take()
        .unwrap()
        .read_to_string(&mut said)
        .unwrap();
    assert!(pane.wait().unwrap().success());
    assert!(!said.contains("#n="), "a pane minted a link: {said}");
    assert!(said.contains("pane-l"), "{said}");

    // An agent's environment, however detached.
    let (ok, out, err) = op::operator_cli_env(
        bin(),
        &d.state(),
        &["ui", "login", "--json", "--port", &port.to_string()],
        &[("CADENCE_ALIAS", "pane-l")],
    );
    assert!(!ok, "{out}");
    assert!(err.contains("CADENCE_ALIAS"), "{err}");

    // The right shape with a wrong secret, or none.
    let sock = client::socket_path(&d.state());
    let made_up = format!("{:064x}", 0x5eed_u128 ^ u128::from(std::process::id()));
    for params in [
        json!({"origin": "loopback", "secret": made_up}),
        json!({"origin": "loopback"}),
    ] {
        let frame = op::operator_rpc(&sock, "operator_link_mint", params);
        assert_eq!(frame["ok"], false, "{frame}");
        assert_eq!(frame["error"]["code"], "operator_secret", "{frame}");
    }
    assert_eq!(minted(), 0, "a refused caller minted");

    // Loose modes: refused by the CLI and the daemon; never repaired.
    use std::os::unix::fs::PermissionsExt;
    let secret_path = d.state().join("operator/secret");
    let secret = std::fs::read_to_string(&secret_path).unwrap();
    for (path, loose, fix) in [
        (secret_path.clone(), 0o640, "chmod 600"),
        (secret_path.clone(), 0o604, "chmod 600"),
        (d.state().join("operator"), 0o755, "chmod 700"),
    ] {
        let tight = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(loose)).unwrap();
        let (ok, _, err) = op::operator_cli(
            bin(),
            &d.state(),
            &["ui", "login", "--json", "--port", &port.to_string()],
        );
        assert!(!ok && err.contains(fix), "{loose:o}: {err}");
        let frame = op::operator_rpc(
            &sock,
            "operator_link_mint",
            json!({"origin": "loopback", "secret": secret.trim()}),
        );
        assert_eq!(
            frame["error"]["code"], "operator_secret",
            "{loose:o}: {frame}"
        );
        let now = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(now, loose, "the refusal repaired the mode");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(tight)).unwrap();
    }
    assert_eq!(minted(), 0, "a loose secret minted");
    // Restored, the operator mints.
    assert!(op::login_link(bin(), &d.state(), port, &[]).is_ok());
    assert_eq!(minted(), 1);
}

/// A9 — a live session presented by a process tied to an agent is
/// evidence of theft: refused `session_from_agent`, the session is
/// revoked (the operator's next request with it fails too), and the
/// daemon records it. Nothing is written.
#[test]
fn a_session_presented_by_an_agent_is_revoked() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    seed(pm.path(), state.path());
    let d = UiDaemon::start_on(state.path().to_path_buf());
    let (port, _board) = start_ui(pm.path().to_path_buf(), d.state());
    let host = format!("127.0.0.1:{port}");
    let s = sign_in(&d.state(), port);
    let before = commits(pm.path());
    let request = s.request("POST", "/api/issues/CAD-3/comments", r#"{"body":"stolen"}"#);
    let mut pane = Command::new("bash")
        .args(["-c", r#"read -r _; bash -c "$CLIENT"; true"#])
        .env(
            "CLIENT",
            r#"exec 3<>"/dev/tcp/127.0.0.1/$PORT"; printf '%s' "$REQ" >&3; cat <&3"#,
        )
        .env("PORT", port.to_string())
        .env("REQ", &request)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    plant_pane(&d, "pane-t", pane.id());
    pane.stdin.take().unwrap().write_all(b"go\n").unwrap();
    let mut response = String::new();
    pane.stdout
        .take()
        .unwrap()
        .read_to_string(&mut response)
        .unwrap();
    assert!(pane.wait().unwrap().success());
    assert!(response.contains(" 403 "), "{response}");
    assert!(response.contains("session_from_agent"), "{response}");
    let seen = daemon_events(&d.state(), "operator_session_from_agent");
    assert!(
        seen.iter()
            .any(|e| e["agent"] == "pane-t" && e["revoked"] == true),
        "{seen:?}"
    );
    let (code, _, body) = op_write_json(
        &s,
        port,
        "POST",
        "/api/issues/CAD-3/comments",
        &host,
        r#"{"body":"after"}"#,
    );
    assert_eq!(code, 403, "{body}");
    assert_eq!(check_of(&body), "operator_session_required");
    assert_eq!(commits(pm.path()), before, "nothing is written");
}

/// F1 (A2, A5, A6) — without a session, no process shape is the
/// operator, on any route: a `setsid -f` child of a pane with a
/// scrubbed env and stdio (tied to no pane any more), this test process
/// with every guard header forged, and one exporting a registered
/// pane's alias. Each is refused `operator_session_required` on an
/// agent-allowed route (a comment) and an operator-only one (model
/// defaults), and nothing is written.
#[test]
fn without_a_session_no_process_shape_is_the_operator() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let out_dir = TempDir::new().unwrap();
    seed(pm.path(), state.path());
    let d = UiDaemon::start_on(state.path().to_path_buf());
    let (port, _board) = start_ui(pm.path().to_path_buf(), d.state());
    let host = format!("127.0.0.1:{port}");
    let before = commits(pm.path());
    let doc = r#"{"expected_revision":0,"config":{"schema":1,"providers":{}}}"#;
    let routes = [
        ("/api/issues/CAD-3/comments", r#"{"body":"f1"}"#),
        ("/api/settings/model-defaults", doc),
    ];
    let forged = |path: &str, body: &str| -> String {
        format!(
            "POST {path} HTTP/1.0\r\nHost: {host}\r\nContent-Type: application/json\r\n\
             X-Cadence-Board: 1\r\nOrigin: http://{host}\r\nSec-Fetch-Site: same-origin\r\n\
             Content-Length: {}\r\n\r\n{body}",
            body.len()
        )
    };

    // A detached, scrubbed child of a pane — off its ancestry and stdio.
    let client = r#"
        on_pane_lineage() {
            p=$$
            while [ "$p" -gt 1 ]; do
                [ "$p" = "$PANE" ] && return 0
                p=$(awk '/^PPid:/{print $2}' "/proc/$p/status") || return 0
                [ -n "$p" ] || return 0
            done
            return 1
        }
        while on_pane_lineage; do sleep 0.02; done
        exec 3<>"/dev/tcp/127.0.0.1/$PORT"
        printf '%s' "$REQ" >&3
        cat <&3 >"$OUT.tmp" && mv "$OUT.tmp" "$OUT"
    "#;
    for (n, (path, body)) in routes.iter().enumerate() {
        let out = out_dir.path().join(format!("reply-{n}"));
        let mut pane = Command::new("bash")
            .args([
                "-c",
                r#"read -r _; PANE=$$ setsid -f env -i PATH="$PATH" PANE=$$ PORT="$PORT" REQ="$REQ" OUT="$OUT" bash -c "$CLIENT" </dev/null >/dev/null 2>&1; true"#,
            ])
            .env("CADENCE_ALIAS", "pane-d")
            .env("CLIENT", client)
            .env("PORT", port.to_string())
            .env("REQ", forged(path, body))
            .env("OUT", &out)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        plant_pane(&d, &format!("pane-d{n}"), pane.id());
        pane.stdin.take().unwrap().write_all(b"go\n").unwrap();
        assert!(pane.wait().unwrap().success());
        let deadline = Instant::now() + Duration::from_secs(20);
        while !out.exists() {
            assert!(
                Instant::now() < deadline,
                "the detached child never answered"
            );
            thread::sleep(Duration::from_millis(25));
        }
        let reply = std::fs::read_to_string(&out).unwrap();
        assert!(reply.contains(" 403 "), "{path}: {reply}");
        assert!(
            reply.contains("operator_session_required"),
            "{path}: {reply}"
        );
    }

    // This test process, every guard header forged; then with a
    // registered pane's alias exported.
    for (path, body) in routes {
        let (code, _, reply) = op::raw(port, &forged(path, body));
        assert_eq!(code, 403, "{path}: {reply}");
        assert_eq!(check_of(&reply), "operator_session_required", "{path}");
        let out = Command::new("bash")
            .args([
                "-c",
                r#"exec 3<>"/dev/tcp/127.0.0.1/$PORT"; printf '%s' "$REQ" >&3; cat <&3"#,
            ])
            .env("CADENCE_ALIAS", "pane-d0")
            .env("PORT", port.to_string())
            .env("REQ", forged(path, body))
            .output()
            .unwrap();
        let reply = String::from_utf8_lossy(&out.stdout);
        assert!(
            reply.contains("operator_session_required"),
            "{path}: {reply}"
        );
    }
    assert_eq!(commits(pm.path()), before, "nothing is written");
    let current: Value =
        serde_json::from_str(&http(port, "GET", "/api/settings/model-defaults", &host).1).unwrap();
    assert_eq!(current["revision"], 0, "{current}");
}

/// CAD-428 / the #233 review probe: a board started with `--allow-host
/// <tailnet name>` and NO armed tailnet answers a loopback request with
/// that Host and a `Tailscale-User-Login` as a local caller — which,
/// without a session, is refused; with the operator's session it writes
/// as `operator (ui)` and never as the forged login.
#[test]
fn an_allowed_tailnet_host_without_armed_sharing_trusts_no_header() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    seed(pm.path(), state.path());
    let d = UiDaemon::start_on(state.path().to_path_buf());
    let (port, _board) = start_ui_opts(pm.path().to_path_buf(), d.state(), |o| {
        o.allow_hosts = vec![TS_DNS.to_string(), format!("{TS_DNS}:9450")];
    });
    let ts_host = format!("{TS_DNS}:9450");
    let origin = format!("http://{ts_host}");
    let before = commits(pm.path());
    let headers = ts_write_headers(&origin, "mallory@evil.example");
    let href: Vec<&str> = headers.iter().map(String::as_str).collect();
    let (code, _, body) = http_write(
        port,
        "PATCH",
        "/api/issues/CAD-3",
        &ts_host,
        &href,
        br#"{"priority":"P0"}"#,
    );
    assert_eq!(code, 403, "{body}");
    assert_eq!(check_of(&body), "operator_session_required");
    assert!(!body.contains("mallory"), "{body}");
    assert_eq!(commits(pm.path()), before);
    let meta = meta_with(
        port,
        &ts_host,
        &["Tailscale-User-Login: mallory@evil.example"],
    );
    assert_eq!(meta["actor"], "operator (ui)", "{meta}");
    assert_eq!(meta["signed_in"], false, "{meta}");

    // The operator's session does not travel to that Host (sessions live
    // on the board's own name only) …
    let s = sign_in(&d.state(), port);
    let with_cookie: Vec<String> = headers
        .iter()
        .cloned()
        .chain([format!("Cookie: {}", s.cookie), s.key_header()])
        .collect();
    let wref: Vec<&str> = with_cookie.iter().map(String::as_str).collect();
    let (code, _, body) = http_write(
        port,
        "PATCH",
        "/api/issues/CAD-3",
        &ts_host,
        &wref,
        br#"{"priority":"P0"}"#,
    );
    assert_eq!(code, 403, "{body}");
    assert_eq!(check_of(&body), "operator_session_required");
    assert_eq!(commits(pm.path()), before);
    // … and where it lives, the forged login header still names nobody.
    let no_origin: Vec<&str> = href
        .iter()
        .copied()
        .filter(|h| !h.starts_with("Origin:"))
        .collect();
    let (code, _, body) = op_http_write(
        &s,
        port,
        "PATCH",
        "/api/issues/CAD-3",
        &ts_host,
        &no_origin,
        br#"{"priority":"P0"}"#,
    );
    assert_eq!(code, 200, "{body}");
    let sha = sha_of(pm.path(), "cadence/CAD-3", "priority=P0");
    let t = trailers_of(pm.path(), &sha);
    assert!(t.contains("Actor: operator (ui)"), "{t}");
    assert!(!t.contains("mallory"), "{t}");
}

/// T-A over the route table itself (`ui::WRITE_ROUTES`), so a new route
/// cannot skip it: the table has operator-only routes (an empty table
/// fails), and every operator-only and agent-allowed write refuses a
/// caller that holds no session and is tied to no agent — with nothing
/// written. An unlisted write is operator-only.
#[test]
fn every_classified_write_refuses_a_caller_without_a_session() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    seed(pm.path(), state.path());
    let d = UiDaemon::start_on(state.path().to_path_buf());
    let (port, _board) = start_ui(pm.path().to_path_buf(), d.state());
    let host = format!("127.0.0.1:{port}");
    let operator_only = ui::WRITE_ROUTES
        .iter()
        .filter(|r| r.class == ui::RouteClass::OperatorOnly)
        .count();
    assert!(
        operator_only >= 7,
        "the route table lost its operator-only routes"
    );
    assert_eq!(
        ui::route_class("POST", "/api/some/new/route"),
        ui::RouteClass::OperatorOnly
    );
    let before = commits(pm.path());
    let mut checked = 0;
    for r in ui::WRITE_ROUTES {
        if !matches!(
            r.class,
            ui::RouteClass::OperatorOnly | ui::RouteClass::AgentAllowed
        ) {
            continue;
        }
        let mut path = String::new();
        let segs: Vec<&str> = r.pattern.split('/').collect();
        for (i, seg) in segs.iter().enumerate() {
            if i > 0 {
                path.push('/');
            }
            path.push_str(match (*seg, segs.get(i.wrapping_sub(1)).copied()) {
                ("*", Some("threads")) => "lead",
                ("*", Some("monitors")) => "m1",
                ("*", Some("alerts")) => "1",
                ("*", _) => "CAD-3",
                (s, _) => s,
            });
        }
        let artifacts = path.ends_with("/artifacts");
        if artifacts {
            path.push_str("?name=x.md");
        }
        let ct = if artifacts {
            "Content-Type: application/octet-stream"
        } else {
            "Content-Type: application/json"
        };
        let origin = format!("Origin: http://{host}");
        let (code, _, body) = http_write(
            port,
            r.method,
            &path,
            &host,
            &[ct, "X-Cadence-Board: 1", &origin],
            b"{}",
        );
        assert_eq!(code, 403, "{} {path}: {body}", r.method);
        assert_eq!(
            check_of(&body),
            "operator_session_required",
            "{} {path}",
            r.method
        );
        checked += 1;
    }
    assert!(checked > operator_only, "the loop checked {checked} routes");
    assert_eq!(commits(pm.path()), before, "a refused write landed");
}

/// ADR 0004 §7 — PINS AN ACCEPTED RESIDUAL (phase 1). A same-uid process
/// that deliberately reads the operator secret AND evades
/// `operator_proof` — a `setsid -f` child of a pane, env scrubbed, stdio
/// off the pane's pty — mints a login link. Phase 1 makes this a
/// deliberate, multi-step act (the daemon records `operator_link_minted`
/// with the pid); it does not prevent it. Phase 2 (CAD-280: make
/// `operator/` unreadable from agent process trees) must flip this test
/// DELIBERATELY: the mint must then fail.
#[test]
fn operator_secret_theft_residual_pinned() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let out_dir = TempDir::new().unwrap();
    seed(pm.path(), state.path());
    let d = UiDaemon::start_on(state.path().to_path_buf());
    let (port, _board) = start_ui(pm.path().to_path_buf(), d.state());
    let out = out_dir.path().join("minted");
    let client = r#"
        on_pane_lineage() {
            p=$$
            while [ "$p" -gt 1 ]; do
                [ "$p" = "$PANE" ] && return 0
                p=$(awk '/^PPid:/{print $2}' "/proc/$p/status") || return 0
                [ -n "$p" ] || return 0
            done
            return 1
        }
        while on_pane_lineage; do sleep 0.02; done
        "$BIN" --state-dir "$STATE" ui login --json --port "$PORT" >"$OUT.tmp" 2>&1
        mv "$OUT.tmp" "$OUT"
    "#;
    let mut pane = Command::new("bash")
        .args([
            "-c",
            r#"read -r _; setsid -f env -i PATH="$PATH" PANE=$$ BIN="$BIN" STATE="$STATE" PORT="$PORT" OUT="$OUT" bash -c "$CLIENT" </dev/null >/dev/null 2>&1; true"#,
        ])
        .env("CADENCE_ALIAS", "pane-r")
        .env("CLIENT", client)
        .env("BIN", bin())
        .env("STATE", d.state())
        .env("PORT", port.to_string())
        .env("OUT", &out)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    plant_pane(&d, "pane-r", pane.id());
    pane.stdin.take().unwrap().write_all(b"go\n").unwrap();
    assert!(pane.wait().unwrap().success());
    let deadline = Instant::now() + Duration::from_secs(30);
    while !out.exists() {
        assert!(
            Instant::now() < deadline,
            "the detached child never finished"
        );
        thread::sleep(Duration::from_millis(25));
    }
    let said = std::fs::read_to_string(&out).unwrap();
    assert!(
        said.contains("#n="),
        "CAD-313 residual changed — if phase 2 landed, flip this pin: {said}"
    );
    assert_eq!(daemon_events(&d.state(), "operator_link_minted").len(), 1);
}

// --- CAD-313 review round 1 (PR #249): the probes, as tests ---

/// Run `script` (bash) as the CHILD of a freshly planted pane `alias` —
/// the pane waits for a go line first, so the plant lands before the
/// child connects. `env` is passed to both. Returns the child's stdout.
fn as_pane_child(d: &UiDaemon, alias: &str, script: &str, env: &[(&str, String)]) -> String {
    let mut cmd = Command::new("bash");
    cmd.args(["-c", r#"read -r _; bash -c "$CLIENT"; true"#])
        .env("CLIENT", script)
        .env_remove("CADENCE_ALIAS")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut pane = cmd.spawn().unwrap();
    plant_pane(d, alias, pane.id());
    pane.stdin.take().unwrap().write_all(b"go\n").unwrap();
    let mut out = String::new();
    pane.stdout
        .take()
        .unwrap()
        .read_to_string(&mut out)
        .unwrap();
    assert!(pane.wait().unwrap().success());
    out
}

/// MUST-FIX 1: a pane replays the operator's stolen cookie and closes
/// its end of the socket at once (`exec 3>&-`), so the board can no
/// longer attribute the connection. An unattributable peer is the
/// operator only when its socket is alive and another uid's; this one
/// is neither — refused, nothing written, three times out of three.
/// The same early close on the login exchange spends the link and opens
/// nothing.
#[test]
fn an_early_closed_replay_of_a_stolen_session_writes_nothing() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    seed(pm.path(), state.path());
    let d = UiDaemon::start_on(state.path().to_path_buf());
    let (port, _board) = start_ui(pm.path().to_path_buf(), d.state());
    let before = commits(pm.path());
    const HIT_AND_RUN: &str =
        r#"exec 3<>"/dev/tcp/127.0.0.1/$PORT"; printf '%s' "$REQ" >&3; exec 3>&-; sleep 1"#;
    for n in 0..3 {
        let s = sign_in(&d.state(), port);
        let req = s.request(
            "POST",
            "/api/issues/CAD-3/comments",
            &format!(r#"{{"body":"hit and run {n}"}}"#),
        );
        as_pane_child(
            &d,
            &format!("pane-e{n}"),
            HIT_AND_RUN,
            &[("PORT", port.to_string()), ("REQ", req)],
        );
        // Let the board finish the request it read.
        thread::sleep(Duration::from_millis(500));
        assert_eq!(commits(pm.path()), before, "attempt {n} wrote");
    }
    let (_, last) = git(pm.path(), &["log", "-1", "--format=%B"]);
    assert!(!last.contains("hit and run"), "{last}");

    // The login exchange, hit and run: the link is spent, and the
    // exchange is refused as a stolen one (recorded), not opened.
    let spent_before = daemon_events(&d.state(), "operator_session_from_agent").len();
    let link = op::login_link(bin(), &d.state(), port, &[]).unwrap();
    let nonce = op::nonce_of(&link);
    let host = op::board_host(port);
    let req = op::request(
        "POST",
        "/api/session",
        &host,
        Some(&format!("http://{host}")),
        None,
        &format!(r#"{{"nonce":"{nonce}"}}"#),
    );
    as_pane_child(
        &d,
        "pane-ex",
        HIT_AND_RUN,
        &[("PORT", port.to_string()), ("REQ", req)],
    );
    thread::sleep(Duration::from_millis(500));
    let (code, _, body) = op::exchange(port, &host, &nonce);
    assert_eq!(code, 403, "{body}");
    assert!(body.contains("already_used"), "{body}");
    assert_eq!(
        daemon_events(&d.state(), "operator_session_from_agent").len(),
        spent_before + 1,
        "the hit-and-run exchange is refused and recorded"
    );
}

/// MUST-FIX 2: model defaults and thread messages are operator-only
/// routes like the plan decisions: a process tied to no pane that holds
/// the operator's cookie but carries an agent's `CADENCE_ALIAS` fails
/// the process proof on the peer — `403 operator_proof`, nothing set.
#[test]
fn every_operator_only_route_runs_the_process_proof() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    seed(pm.path(), state.path());
    let d = UiDaemon::start_on(state.path().to_path_buf());
    let (port, _board) = start_ui(pm.path().to_path_buf(), d.state());
    let s = sign_in(&d.state(), port);
    let doc = r#"{"expected_revision":0,"config":{"schema":1,"providers":{}}}"#;
    for (path, body) in [
        ("/api/settings/model-defaults", doc),
        ("/api/threads/lead/messages", r#"{"text":"hi"}"#),
    ] {
        let out = Command::new("bash")
            .args([
                "-c",
                r#"exec 3<>"/dev/tcp/127.0.0.1/$PORT"; printf '%s' "$REQ" >&3; cat <&3"#,
            ])
            .env("CADENCE_ALIAS", "some-agent")
            .env("PORT", port.to_string())
            .env("REQ", s.request("POST", path, body))
            .output()
            .unwrap();
        let reply = String::from_utf8_lossy(&out.stdout);
        assert!(reply.contains(" 403 "), "{path}: {reply}");
        assert!(reply.contains("operator_proof"), "{path}: {reply}");
        assert!(reply.contains("CADENCE_ALIAS"), "{path}: {reply}");
    }
    let current: Value = serde_json::from_str(
        &http(
            port,
            "GET",
            "/api/settings/model-defaults",
            &op::board_host(port),
        )
        .1,
    )
    .unwrap();
    assert_eq!(current["revision"], 0, "{current}");
}

/// MUST-FIX 3: `WRITE_ROUTES` is enforced, not documentation. A caller
/// tied to a pane (no session) is refused `operator_only` on every
/// operator-only route in the table and ACCEPTED (never refused by a
/// caller check) on every agent-allowed one; an unlisted write is
/// operator-only for it too.
#[test]
fn route_classes_are_enforced_for_an_agent_caller() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    seed(pm.path(), state.path());
    let d = UiDaemon::start_on(state.path().to_path_buf());
    let (port, _board) = start_ui(pm.path().to_path_buf(), d.state());
    let host = format!("127.0.0.1:{port}");
    let client = r#"exec 3<>"/dev/tcp/127.0.0.1/$PORT"; printf '%s' "$REQ" >&3; cat <&3"#;
    let mut n = 0;
    let mut as_agent = |method: &str, path: &str, ct: &str, body: &str| -> String {
        n += 1;
        let req = format!(
            "{method} {path} HTTP/1.0\r\nHost: {host}\r\nContent-Type: {ct}\r\n\
             X-Cadence-Board: 1\r\nOrigin: http://{host}\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        as_pane_child(
            &d,
            &format!("pane-c{n}"),
            client,
            &[("PORT", port.to_string()), ("REQ", req)],
        )
    };
    // The table itself is pinned: exactly these writes are agent-allowed.
    let mut agent_allowed: Vec<String> = ui::WRITE_ROUTES
        .iter()
        .filter(|r| r.class == ui::RouteClass::AgentAllowed)
        .map(|r| format!("{} {}", r.method, r.pattern))
        .collect();
    agent_allowed.sort();
    assert_eq!(
        agent_allowed,
        [
            "DELETE /api/issues/*/links",
            "PATCH /api/issues/*",
            "POST /api/issues",
            "POST /api/issues/*/artifacts",
            "POST /api/issues/*/comments",
            "POST /api/issues/*/links",
            "POST /api/issues/*/refs",
            "POST /api/monitors/*/alerts/*/ack",
        ]
    );
    let mut classes = (0, 0);
    for r in ui::WRITE_ROUTES {
        let segs: Vec<&str> = r.pattern.split('/').collect();
        let mut path = segs
            .iter()
            .enumerate()
            .map(|(i, seg)| {
                let prev = i.checked_sub(1).map(|p| segs[p]);
                match (*seg, prev) {
                    ("*", Some("threads")) => "lead".to_string(),
                    ("*", Some("monitors")) => "m1".to_string(),
                    ("*", Some("alerts")) => "1".to_string(),
                    ("*", Some("epics")) => "CAD-1".to_string(),
                    ("*", _) => "CAD-3".to_string(),
                    (s, _) => s.to_string(),
                }
            })
            .collect::<Vec<_>>()
            .join("/");
        let (ct, body) = if path.ends_with("/artifacts") {
            path.push_str("?name=agent.md");
            ("application/octet-stream", "# notes".to_string())
        } else {
            let body = match (r.method, r.pattern) {
                ("POST", "/api/issues") => r#"{"project":"cadence","title":"from an agent"}"#,
                ("PATCH", _) => r#"{"priority":"P1"}"#,
                (_, p) if p.ends_with("/links") => r#"{"type":"relates","target":"CAD-1"}"#,
                (_, p) if p.ends_with("/refs") => r#"{"kind":"url","url":"https://example.com/x"}"#,
                (_, p) if p.ends_with("/comments") => r#"{"body":"agent note"}"#,
                _ => "{}",
            };
            ("application/json", body.to_string())
        };
        match r.class {
            ui::RouteClass::OperatorOnly => {
                let reply = as_agent(r.method, &path, ct, &body);
                assert!(reply.contains(" 403 "), "{} {path}: {reply}", r.method);
                assert!(
                    reply.contains("operator_only"),
                    "{} {path}: {reply}",
                    r.method
                );
                classes.0 += 1;
            }
            ui::RouteClass::AgentAllowed => {
                let reply = as_agent(r.method, &path, ct, &body);
                assert!(!reply.contains("\"check\""), "{} {path}: {reply}", r.method);
                assert!(!reply.contains(" 403 "), "{} {path}: {reply}", r.method);
                if path.starts_with("/api/issues") {
                    assert!(
                        reply.contains(" 200 ") || reply.contains(" 201 "),
                        "{} {path}: {reply}",
                        r.method
                    );
                }
                classes.1 += 1;
            }
            _ => {}
        }
    }
    assert!(classes.0 >= 8 && classes.1 >= 7, "{classes:?}");
    // Unlisted: operator-only.
    let reply = as_agent("POST", "/api/launch", "application/json", "{}");
    assert!(reply.contains("operator_only"), "{reply}");
}

/// MUST-FIX 5: there is no shared failure budget — a flood of bogus
/// nonces over HTTP never locks the operator's fresh link out.
#[test]
fn bogus_sign_ins_never_lock_the_operator_out() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    seed(pm.path(), state.path());
    let d = UiDaemon::start_on(state.path().to_path_buf());
    let (port, _board) = start_ui(pm.path().to_path_buf(), d.state());
    let host = op::board_host(port);
    for i in 0..25u32 {
        let bogus = format!("{:064x}", u128::from(i) * 7919 + 1);
        let (code, _, body) = op::exchange(port, &host, &bogus);
        assert_eq!(code, 403, "{body}");
    }
    let link = op::login_link(bin(), &d.state(), port, &[]).unwrap();
    let (code, _, body) = op::exchange(port, &host, &op::nonce_of(&link));
    assert_eq!(code, 200, "{body}");
}

/// The daemon's own session checks, end to end over the socket: a
/// session is honoured only on the origin it was opened for, a link
/// only on the origin it was minted for, and a connection that derives
/// an agent cannot open a session with a nonce it holds — it spends it.
#[test]
fn the_daemon_binds_sessions_to_their_origin_and_refuses_agents() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    seed(pm.path(), state.path());
    let d = UiDaemon::start_on(state.path().to_path_buf());
    let (port, _board) = start_ui(pm.path().to_path_buf(), d.state());
    let s = sign_in(&d.state(), port);
    let token = s.cookie.split_once('=').unwrap().1.to_string();
    let check = |origin: &str, key: &str| {
        d.rpc(
            "operator_session_check",
            json!({"token": token, "key": key, "origin": origin}),
        )["valid"]
            .clone()
    };
    assert_eq!(check("loopback", &s.key), true);
    assert_eq!(check("tailnet", &s.key), false);
    assert_eq!(
        check("loopback", ""),
        false,
        "the token alone is no session"
    );
    // A tailnet link, exchanged directly as loopback.
    std::fs::write(
        d.state().join("ui.json"),
        json!({"tailscale": {"dns_name": TS_DNS, "https_port": 9450,
                             "target": format!("http://127.0.0.1:{port}")}})
        .to_string(),
    )
    .unwrap();
    let tailnet = op::login_link(bin(), &d.state(), port, &["--tailnet"]).unwrap();
    let err = d
        .rpc_opt(
            "operator_session_open",
            json!({"nonce": op::nonce_of(&tailnet), "origin": "loopback"}),
        )
        .unwrap_err();
    assert!(err.to_string().contains("wrong_origin"), "{err}");
    // An agent with a leaked nonce skips the board and calls the verb.
    let link = op::login_link(bin(), &d.state(), port, &[]).unwrap();
    let nonce = op::nonce_of(&link);
    let frame = json!({"method": "operator_session_open",
                       "params": {"nonce": nonce, "origin": "loopback"}})
    .to_string();
    let reply = as_pane_child(
        &d,
        "pane-leak",
        r#"python3 -c 'import socket,sys; s=socket.socket(socket.AF_UNIX); s.connect(sys.argv[1]); s.sendall((sys.argv[2]+"\n").encode()); print(s.makefile().readline())' "$SOCK" "$FRAME""#,
        &[
            (
                "SOCK",
                client::socket_path(&d.state()).display().to_string(),
            ),
            ("FRAME", frame),
        ],
    );
    assert!(reply.contains("session_from_agent"), "{reply}");
    assert!(!reply.contains("\"token\""), "{reply}");
    let (code, _, body) = op::exchange(port, &op::board_host(port), &nonce);
    assert_eq!(code, 403, "{body}");
    assert!(body.contains("already_used"), "{body}");
}

// --- CAD-313 review round 2 (PR #249): the second credential ---

/// A session is the cookie AND the page's `X-Cadence-Session` key,
/// together: either alone is refused, both are the operator.
#[test]
fn a_session_needs_the_cookie_and_the_page_key() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    seed(pm.path(), state.path());
    let d = UiDaemon::start_on(state.path().to_path_buf());
    let (port, _board) = start_ui(pm.path().to_path_buf(), d.state());
    let s = sign_in(&d.state(), port);
    let before = commits(pm.path());
    let full = s.request("PATCH", "/api/issues/CAD-3", r#"{"priority":"P0"}"#);
    let cookie_line = format!("Cookie: {}\r\n", s.cookie);
    let key_line = format!("{}\r\n", s.key_header());
    for (what, req) in [
        ("cookie without key", full.replacen(&key_line, "", 1)),
        ("key without cookie", full.replacen(&cookie_line, "", 1)),
        (
            "cookie with another session's key",
            full.replacen(&s.key, &"ab".repeat(32), 1),
        ),
    ] {
        assert_ne!(req, full, "{what}: the request did not change");
        let (code, _, body) = op::raw(port, &req);
        assert_eq!(code, 403, "{what}: {body}");
        assert_eq!(check_of(&body), "operator_session_required", "{what}");
    }
    assert_eq!(commits(pm.path()), before, "a half session wrote");
    let (code, _, body) = op::raw(port, &full);
    assert_eq!(code, 200, "both: {body}");
    // meta agrees: signed in only with both.
    let host = op::board_host(port);
    let cookie_h = format!("Cookie: {}", s.cookie);
    assert_eq!(meta_with(port, &host, &[&cookie_h])["signed_in"], false);
    assert_eq!(
        meta_with(port, &host, &[&cookie_h, &s.key_header()])["signed_in"],
        true
    );
}

/// The round-2 probe: a worker's comment links to
/// `http://cadence-<board port>.localhost:<its port>/…`; the operator's
/// browser follows it and sends the board's cookie there (cookies
/// ignore ports). The agent's listener captures what the browser sends —
/// the cookie, never the page's key, which lives in the board origin's
/// `sessionStorage` — and hands it to a `setsid -f env -i` child of its
/// pane. The replay is refused on an agent-allowed route (a comment) and
/// an operator-only one (model defaults), and nothing is written.
#[test]
fn a_cookie_leaked_to_another_port_is_worthless_alone() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    seed(pm.path(), state.path());
    let d = UiDaemon::start_on(state.path().to_path_buf());
    let (port, _board) = start_ui(pm.path().to_path_buf(), d.state());
    let s = sign_in(&d.state(), port);
    // The agent's listener on another port, and what the browser sends
    // it for `http://cadence-<port>.localhost:<other>/preview`.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let other = listener.local_addr().unwrap().port();
    let browser = thread::spawn(move || {
        let mut c = TcpStream::connect(("127.0.0.1", other)).unwrap();
        write!(
            c,
            "GET /preview HTTP/1.1\r\nHost: cadence-{port}.localhost:{other}\r\n\
             Sec-Fetch-Site: same-site\r\nCookie: {}\r\n\r\n",
            s.cookie
        )
        .unwrap();
    });
    let (mut conn, _) = listener.accept().unwrap();
    let mut seen = Vec::new();
    let mut buf = [0u8; 1024];
    while !seen.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = conn.read(&mut buf).unwrap();
        assert!(n > 0, "the browser hung up early");
        seen.extend_from_slice(&buf[..n]);
    }
    browser.join().unwrap();
    let captured = String::from_utf8_lossy(&seen).to_string();
    let stolen = captured
        .lines()
        .find_map(|l| l.strip_prefix("Cookie: "))
        .unwrap()
        .to_string();
    assert!(
        !captured.contains(&s.key),
        "the listener must never see the key: {captured}"
    );

    let host = op::board_host(port);
    let doc = r#"{"expected_revision":0,"config":{"schema":1,"providers":{}}}"#;
    let before = commits(pm.path());
    for (n, (path, body)) in [
        (
            "/api/issues/CAD-3/comments",
            r#"{"body":"as the operator"}"#,
        ),
        ("/api/settings/model-defaults", doc),
    ]
    .into_iter()
    .enumerate()
    {
        let out = work.path().join(format!("reply-{n}"));
        let req = op::request(
            "POST",
            path,
            &host,
            Some(&format!("http://{host}")),
            Some(&stolen),
            body,
        );
        let script = r#"PANE=$$ setsid -f env -i PATH="$PATH" PANE=$$ PORT="$PORT" REQ="$REQ" OUT="$OUT" bash -c '
            while p=$$; true; do
                on=0; while [ "$p" -gt 1 ]; do [ "$p" = "$PANE" ] && on=1 && break
                    p=$(awk "/^PPid:/{print \$2}" /proc/$p/status) || break; done
                [ $on = 0 ] && break; sleep 0.02; done
            exec 3<>"/dev/tcp/127.0.0.1/$PORT"; printf "%s" "$REQ" >&3
            cat <&3 >"$OUT.tmp" && mv "$OUT.tmp" "$OUT"' </dev/null >/dev/null 2>&1"#;
        as_pane_child(
            &d,
            &format!("pane-l{n}"),
            script,
            &[
                ("PORT", port.to_string()),
                ("REQ", req),
                ("OUT", out.display().to_string()),
            ],
        );
        let deadline = Instant::now() + Duration::from_secs(20);
        while !out.exists() {
            assert!(
                Instant::now() < deadline,
                "the detached child never answered"
            );
            thread::sleep(Duration::from_millis(25));
        }
        let reply = std::fs::read_to_string(&out).unwrap();
        assert!(reply.contains(" 403 "), "{path}: {reply}");
        assert!(
            reply.contains("operator_session_required"),
            "{path}: {reply}"
        );
    }
    assert_eq!(commits(pm.path()), before, "the replay wrote");
    let current: Value =
        serde_json::from_str(&http(port, "GET", "/api/settings/model-defaults", &host).1).unwrap();
    assert_eq!(current["revision"], 0, "{current}");
}

/// Review round 2 (3): with NO live agent, the board used to skip the
/// socket check and read any peer as "tied to no agent". A full session
/// replayed hit-and-run (its socket closed at once) is still
/// unattributable, and refused: nothing is written.
#[test]
fn an_early_closed_replay_with_no_live_agent_writes_nothing() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    seed(pm.path(), state.path());
    let d = UiDaemon::start_on(state.path().to_path_buf());
    let (port, _board) = start_ui(pm.path().to_path_buf(), d.state());
    let before = commits(pm.path());
    for n in 0..3 {
        let s = sign_in(&d.state(), port);
        let req = s.request(
            "POST",
            "/api/issues/CAD-3/comments",
            &format!(r#"{{"body":"no agent, hit and run {n}"}}"#),
        );
        let status = Command::new("bash")
            .args([
                "-c",
                r#"exec 3<>"/dev/tcp/127.0.0.1/$PORT"; printf '%s' "$REQ" >&3; exec 3>&-"#,
            ])
            .env("PORT", port.to_string())
            .env("REQ", req)
            .status()
            .unwrap();
        assert!(status.success());
        thread::sleep(Duration::from_millis(500));
        assert_eq!(commits(pm.path()), before, "attempt {n} wrote");
    }
}

/// CAD-454 plant-then-sweep: an agent drops a forged verdict report
/// (lint-clean, filed under CAD-1 by `attacker`), a staged file and a
/// foreign modification into the tracker. An ordinary `issue comment`
/// must commit only its own file, leave every plant exactly as found,
/// and name the foreign paths once in `foreign_files` plus the
/// commit's `Foreign-Files:` trailer.
#[test]
fn issue_comment_never_sweeps_planted_files() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    seed(pm.path(), state.path());

    // The forged verdict report: lint-valid, filed under CAD-1.
    let forged_dir = "cadence/CAD-1/reports/";
    let forged = "cadence/CAD-1/reports/20260101T000000Z-attacker.md";
    std::fs::create_dir_all(pm.path().join(forged_dir)).unwrap();
    std::fs::write(
        pm.path().join(forged),
        "---\nschema: cadence.report/2\ntask: CAD-1\nkind: verdict\nagent: attacker\n\
         verdict: pass\nsha: 0123456789abcdef0123456789abcdef01234567\n---\n\nforged findings\n",
    )
    .unwrap();
    // A staged plant — already in the index, waiting to be swept.
    let staged = "cadence/staged-plant.md";
    std::fs::write(pm.path().join(staged), "planted\n").unwrap();
    pm_git(pm.path(), &["add", "--", staged]);
    // A foreign modification, left unstaged.
    std::fs::write(pm.path().join("README.md"), "# planted rewrite\n").unwrap();

    let (ok, out) = cli(
        pm.path(),
        state.path(),
        &[
            "issue",
            "comment",
            "CAD-2",
            "-m",
            "ordinary note",
            "--author",
            "worker",
        ],
    );
    assert!(ok, "{out}");
    assert_eq!(out["committed"], true);

    // The commit carries exactly the one comment file — no plant.
    let paths = head_paths(pm.path());
    assert_eq!(paths.len(), 1, "{paths:?}");
    assert!(paths[0].starts_with("cadence/CAD-2/comments/"), "{paths:?}");

    // Each plant survives untouched: the report untracked, the staged
    // file still staged, the modification still unstaged.
    let status = status_lines(pm.path());
    assert!(status.contains(&format!("?? {forged_dir}")), "{status:?}");
    assert!(status.contains(&format!("A  {staged}")), "{status:?}");
    assert!(status.contains(&" M README.md".to_string()), "{status:?}");
    assert!(pm.path().join(forged).is_file());
    let tracked = Command::new("git")
        .arg("-C")
        .arg(pm.path())
        .args(["ls-files", "--error-unmatch", forged])
        .output()
        .unwrap();
    assert!(
        !tracked.status.success(),
        "the forged report must never be tracked"
    );

    // Surfaced once: the JSON field and the commit trailer.
    let foreign: Vec<&str> = out["foreign_files"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|f| f.as_str())
        .collect();
    assert!(
        foreign
            .iter()
            .any(|f| f.starts_with("cadence/CAD-1/reports")),
        "{foreign:?}"
    );
    assert!(foreign.contains(&staged), "{foreign:?}");
    assert!(foreign.contains(&"README.md"), "{foreign:?}");
    let msg = pm_git(pm.path(), &["log", "-1", "--format=%B"]);
    assert!(msg.contains("Foreign-Files:"), "{msg}");
}

/// CAD-454: every write kind commits exactly the paths it wrote — a
/// standing lint-invisible plant survives all of them.
#[test]
fn tracker_writes_commit_only_their_own_paths() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    seed(pm.path(), state.path());
    // A file under the project dir is not an issue folder — lint
    // ignores it, but `git add -A` would sweep it.
    std::fs::write(pm.path().join("cadence/planted.md"), "planted\n").unwrap();
    let foreign_ok = |out: &Value, what: &str| {
        let foreign: Vec<&str> = out["foreign_files"]
            .as_array()
            .unwrap_or_else(|| panic!("{what}: no foreign_files in {out}"))
            .iter()
            .filter_map(|f| f.as_str())
            .collect();
        assert!(
            foreign.contains(&"cadence/planted.md"),
            "{what}: {foreign:?}"
        );
    };

    let (ok, out) = cli(
        pm.path(),
        state.path(),
        &["issue", "new", "extra task", "--project", "cadence"],
    );
    assert!(ok, "{out}");
    let new_id = out["id"].as_str().unwrap().to_string();
    assert_head_paths(
        pm.path(),
        &[&format!("cadence/{new_id}/issue.md")],
        "issue new",
    );
    foreign_ok(&out, "issue new");

    for (args, path, what) in [
        (
            vec!["issue", "set", "CAD-2", "owner=you"],
            "cadence/CAD-2/issue.md",
            "issue set",
        ),
        (
            vec!["issue", "tag", "CAD-2", "add", "ui"],
            "cadence/CAD-2/issue.md",
            "issue tag",
        ),
        (
            vec!["issue", "link", "CAD-3", "relates", "CAD-2"],
            "cadence/CAD-3/issue.md",
            "issue link",
        ),
        (
            vec!["issue", "unlink", "CAD-3", "relates", "CAD-2"],
            "cadence/CAD-3/issue.md",
            "issue unlink",
        ),
        (
            vec!["issue", "ref", "CAD-2", "commit", "abc123"],
            "cadence/CAD-2/issue.md",
            "issue ref",
        ),
    ] {
        let (ok, out) = cli(pm.path(), state.path(), &args);
        assert!(ok, "{what}: {out}");
        assert_head_paths(pm.path(), &[path], what);
        foreign_ok(&out, what);
    }

    let (ok, out) = cli(
        pm.path(),
        state.path(),
        &["issue", "comment", "CAD-2", "-m", "hi", "--author", "t"],
    );
    assert!(ok, "{out}");
    let paths = head_paths(pm.path());
    assert_eq!(paths.len(), 1, "issue comment: {paths:?}");
    assert!(
        paths[0].starts_with("cadence/CAD-2/comments/"),
        "issue comment: {paths:?}"
    );
    foreign_ok(&out, "issue comment");

    let note = state.path().join("note.txt");
    std::fs::write(&note, "pinned\n").unwrap();
    let (ok, out) = cli(
        pm.path(),
        state.path(),
        &["issue", "attach", "CAD-2", note.to_str().unwrap()],
    );
    assert!(ok, "{out}");
    assert_head_paths(pm.path(), &["cadence/CAD-2/artifacts/note.txt"], "attach");
    foreign_ok(&out, "attach");

    let acc = state.path().join("acc.md");
    std::fs::write(&acc, "- [ ] first\n- [x] second\n").unwrap();
    let (ok, out) = cli(
        pm.path(),
        state.path(),
        &[
            "issue",
            "acceptance",
            "CAD-2",
            "--from",
            acc.to_str().unwrap(),
        ],
    );
    assert!(ok, "{out}");
    assert_head_paths(pm.path(), &["cadence/CAD-2/issue.md"], "acceptance");
    foreign_ok(&out, "acceptance");

    let (ok, out) = cli(
        pm.path(),
        state.path(),
        &["issue", "project", "add", "ops", "--prefix", "OPS"],
    );
    assert!(ok, "{out}");
    assert_head_paths(pm.path(), &["ops/project.yaml"], "project add");
    foreign_ok(&out, "project add");

    // `report` — the intake writer — creates one issue file.
    let (ok, out) = cli(
        pm.path(),
        state.path(),
        &[
            "report",
            "--kind",
            "bug",
            "--project",
            "cadence",
            "-m",
            "a bug\n\nit broke",
        ],
    );
    assert!(ok, "{out}");
    let report_id = out["id"].as_str().unwrap().to_string();
    assert_head_paths(
        pm.path(),
        &[&format!("cadence/{report_id}/issue.md")],
        "report",
    );
    foreign_ok(&out, "report");

    // `report file` — the task-report writer — adds one file under
    // reports/ on the ticket it names.
    let rep = state.path().join("blocked.md");
    std::fs::write(
        &rep,
        "---\nschema: cadence.report/2\ntask: CAD-1\nkind: blocked\n---\n\
         intro\n\n## Expected\n\nx\n\n## Evidence\n\nx\n\n## Cause\n\nx\n\n\
         ## Correction\n\nx\n\n## Lesson\n\nx\n\n## Next\n\nx\n",
    )
    .unwrap();
    let (ok, out) = cli(
        pm.path(),
        state.path(),
        &[
            "report",
            "file",
            "--task",
            "CAD-1",
            "--kind",
            "blocked",
            "--file",
            rep.to_str().unwrap(),
        ],
    );
    assert!(ok, "{out}");
    let paths = head_paths(pm.path());
    assert_eq!(paths.len(), 1, "report file: {paths:?}");
    assert!(
        paths[0].starts_with("cadence/CAD-1/reports/"),
        "report file: {paths:?}"
    );
    foreign_ok(&out, "report file");

    // The plant was never committed by any of the writes above.
    let status = status_lines(pm.path());
    assert_eq!(status, ["?? cadence/planted.md"], "{status:?}");
}

/// CAD-454: `plan propose`'s one commit carries the epic's and every
/// ticket's issue.md — nothing else. Driven in-process (the CLI routes
/// through the daemon); `Pm::init` never installs hooks, so the commit
/// path is exercised without lint.
#[test]
fn plan_commit_stages_only_its_issue_files() {
    let dir = TempDir::new().unwrap();
    let pm = Pm::init(dir.path()).unwrap();
    let out = write::project_add(&pm, "cadence", "CAD", &[], &[], &[], None).unwrap();
    assert_eq!(out["committed"], true);
    assert_head_paths(dir.path(), &["cadence/project.yaml"], "project add");

    std::fs::write(dir.path().join("cadence/planted.md"), "planted\n").unwrap();
    let doc = plan::parse_plan(
        "---\ntitle: ship it\ngoal: the goal\n---\n\nintro\n\n\
         ## first ticket\n\ndo it\n\n### Acceptance\n\n- [ ] done\n",
    )
    .unwrap();
    let out = write::create_plan(&pm, "cadence", &doc, "tester").unwrap();
    assert_eq!(out["committed"], true);
    let mut want: Vec<String> = std::iter::once(out["epic"].as_str().unwrap().to_string())
        .chain(
            out["tickets"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|t| t.as_str().map(str::to_string)),
        )
        .map(|id| format!("cadence/{id}/issue.md"))
        .collect();
    want.sort();
    assert_eq!(head_paths(dir.path()), want);
    let foreign: Vec<&str> = out["foreign_files"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|f| f.as_str())
        .collect();
    assert!(foreign.contains(&"cadence/planted.md"), "{foreign:?}");
    let status = status_lines(dir.path());
    assert_eq!(status, ["?? cadence/planted.md"], "{status:?}");
}

/// CAD-454: a commit refused at the hook leaves nothing staged and no
/// half-written file — and the next writer's commit carries only its
/// own files, never the failed write's residue.
#[test]
fn tracker_failed_commit_leaves_nothing_staged() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    seed(pm.path(), state.path());
    let hook = pm.path().join(".git/hooks/pre-commit");
    let saved = std::fs::read(&hook).unwrap();
    std::fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();

    let before = commits(pm.path());
    let (ok, err) = cli(
        pm.path(),
        state.path(),
        &["issue", "comment", "CAD-2", "-m", "doomed"],
    );
    assert!(!ok, "{err}");
    assert_eq!(commits(pm.path()), before);
    // No staged residue and no orphan comment file — the write's own
    // paths were unstaged and removed.
    let status = status_lines(pm.path());
    assert!(status.is_empty(), "{status:?}");

    std::fs::write(&hook, &saved).unwrap();
    let (ok, out) = cli(
        pm.path(),
        state.path(),
        &["issue", "set", "CAD-2", "owner=you"],
    );
    assert!(ok, "{out}");
    assert_head_paths(pm.path(), &["cadence/CAD-2/issue.md"], "set after failure");
    assert!(status_lines(pm.path()).is_empty());
}

/// CAD-454 at the `Pm::commit` level: a deletion and a rename are
/// staged by path like any other write, a foreign staged entry is
/// never carried into the commit, and a no-op write returns without
/// committing. `Pm::init` installs no hooks — this needs none.
#[test]
fn pm_commit_stages_only_the_named_paths() {
    let dir = TempDir::new().unwrap();
    let pm = Pm::init(dir.path()).unwrap();

    let note = dir.path().join("note.txt");
    std::fs::write(&note, "v1\n").unwrap();
    let foreign = pm
        .commit(std::slice::from_ref(&note), "add note\n\nActor: t\n")
        .unwrap();
    assert!(foreign.is_empty(), "{foreign:?}");
    assert_head_paths(dir.path(), &["note.txt"], "add");

    // The plant: one untracked file, one staged file — both foreign.
    std::fs::write(dir.path().join("planted.txt"), "x\n").unwrap();
    std::fs::write(dir.path().join("staged.txt"), "x\n").unwrap();
    pm_git(dir.path(), &["add", "--", "staged.txt"]);

    // A deletion is staged by naming the removed path.
    std::fs::remove_file(&note).unwrap();
    let foreign = pm
        .commit(std::slice::from_ref(&note), "drop note\n\nActor: t\n")
        .unwrap();
    assert_eq!(foreign, ["planted.txt", "staged.txt"]);
    let ns = pm_git(
        dir.path(),
        &["show", "--pretty=format:", "--name-status", "HEAD"],
    );
    assert_eq!(ns.trim_end(), "D\tnote.txt", "{ns}");

    // A rename is the pair: the old path's delete plus the new file.
    std::fs::write(&note, "v2\n").unwrap();
    pm.commit(std::slice::from_ref(&note), "re-add note\n\nActor: t\n")
        .unwrap();
    let moved = dir.path().join("renamed.txt");
    std::fs::rename(&note, &moved).unwrap();
    pm.commit(&[note.clone(), moved], "rename\n\nActor: t\n")
        .unwrap();
    let ns = pm_git(
        dir.path(),
        &["show", "--pretty=format:", "--name-status", "HEAD"],
    );
    assert!(
        ns.lines()
            .any(|l| l.starts_with('R') && l.contains("renamed.txt")),
        "{ns}"
    );

    // Both plants survived every commit untouched.
    let status = status_lines(dir.path());
    assert_eq!(status, ["A  staged.txt", "?? planted.txt"], "{status:?}");

    // A write that changed nothing still stages nothing: no commit.
    let before = commits(dir.path());
    let foreign = pm
        .commit(&[dir.path().join("renamed.txt")], "no-op\n\nActor: t\n")
        .unwrap();
    assert_eq!(foreign, ["planted.txt", "staged.txt"]);
    assert_eq!(commits(dir.path()), before);

    // A plant inside a fresh untracked directory is named exactly —
    // porcelain's default collapsing would report only `nest/`.
    let nested = dir.path().join("nest/deep/planted-in-dir.txt");
    std::fs::create_dir_all(nested.parent().unwrap()).unwrap();
    std::fs::write(&nested, "x\n").unwrap();
    let foreign = pm
        .commit(&[dir.path().join("renamed.txt")], "no-op\n\nActor: t\n")
        .unwrap();
    assert_eq!(
        foreign,
        ["nest/deep/planted-in-dir.txt", "planted.txt", "staged.txt"],
        "{foreign:?}"
    );

    // The scan is capped: past 64 foreign paths the list ends with a
    // "(+N more)" marker instead of naming them all.
    let big = dir.path().join("big");
    std::fs::create_dir_all(&big).unwrap();
    for i in 0..70 {
        std::fs::write(big.join(format!("f{i:03}.txt")), "x\n").unwrap();
    }
    let foreign = pm
        .commit(&[dir.path().join("renamed.txt")], "no-op\n\nActor: t\n")
        .unwrap();
    assert_eq!(foreign.len(), 65, "{foreign:?}");
    assert_eq!(foreign.last().unwrap(), "(+9 more)");
}
