//! Board e2e: the `cadence issue` CLI against a temp PM dir, and the
//! `cadence ui` HTTP server in-process — routes, host/method/id/traversal
//! rejection, and daemon-unreachable honesty.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use cadence_agent::issue::board;
use cadence_agent::ui;
use serde_json::Value;
use tempfile::TempDir;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_cadence")
}

fn cli(pm: &Path, state: &Path, args: &[&str]) -> (bool, Value) {
    let out = Command::new(bin())
        .arg("--state-dir")
        .arg(state)
        .args(args)
        .env("CADENCE_PM_DIR", pm)
        .env("HOME", std::env::var("HOME").unwrap())
        .output()
        .unwrap();
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
    let mut buf = String::new();
    s.read_to_string(&mut buf).unwrap();
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

/// Spawn `ui::serve` on a free port and wait for health. The caller owns
/// the TempDirs keeping the pm/state dirs alive. `free_port` is a
/// bind-release race — a parallel test may grab the port first, so a
/// failed start retries on a fresh port.
fn start_ui(pm_dir: PathBuf, state_dir: PathBuf) -> u16 {
    let overall = Instant::now() + Duration::from_secs(20);
    loop {
        let port = free_port();
        let (sd, pd) = (state_dir.clone(), pm_dir.clone());
        thread::spawn(move || {
            let _ = ui::serve(&sd, &pd, "127.0.0.1", port, None, &[]);
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
                    return port;
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
    let port = start_ui(pm.path().to_path_buf(), state.path().to_path_buf());
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
    let (code, _) = http(port, "POST", "/api/issues", &ok_host);
    assert_eq!(code, 405);
    let (code, _) = http(port, "DELETE", "/api/issues/CAD-1", &ok_host);
    assert_eq!(code, 405);
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
fn ui_pm_absent_is_honest() {
    let pm = TempDir::new().unwrap();
    let empty = pm.path().join("nope");
    let state = TempDir::new().unwrap();
    let port = start_ui(empty.clone(), state.path().to_path_buf());
    let host = format!("127.0.0.1:{port}");
    let (code, body) = http(port, "GET", "/api/health", &host);
    assert_eq!(code, 200);
    let h: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(h["pm_present"], false);
    let (code, _) = http(port, "GET", "/api/issues", &host);
    assert_eq!(code, 503);
}
