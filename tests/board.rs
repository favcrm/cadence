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
use cadence_agent::{client, daemon};
use serde_json::{json, Value};
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
    // POST on the create route exists in I2 — without the write headers
    // it stops at the cross-site guards (403), never reaching the writer.
    let (code, body) = http(port, "POST", "/api/issues", &ok_host);
    assert_eq!(code, 403);
    assert!(body.contains("content_type"));
    let (code, _) = http(port, "DELETE", "/api/issues/CAD-1", &ok_host);
    assert_eq!(code, 405);
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
    let port = start_ui(pm.path().to_path_buf(), state.path().to_path_buf());
    let host = format!("127.0.0.1:{port}");
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
    let port = start_ui(empty.clone(), state.path().to_path_buf());
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
    let port = start_ui(pm.path().to_path_buf(), state.path().to_path_buf());
    let host = format!("127.0.0.1:{port}");
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
/// integration.rs wraps in TestDaemon, pared down to what the board
/// routes need.
struct UiDaemon {
    state: TempDir,
    handle: Option<thread::JoinHandle<()>>,
}

impl UiDaemon {
    fn start() -> Self {
        let state = TempDir::new().unwrap();
        let owned = state.path().to_path_buf();
        let handle = thread::spawn(move || {
            let _ = daemon::serve(&owned);
        });
        let d = Self {
            state,
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

    fn rpc_opt(&self, method: &str, params: Value) -> cadence_agent::Result<Value> {
        client::rpc(self.state.path(), method, params)
    }

    fn rpc(&self, method: &str, params: Value) -> Value {
        self.rpc_opt(method, params).unwrap()
    }

    fn state(&self) -> PathBuf {
        self.state.path().to_path_buf()
    }
}

impl Drop for UiDaemon {
    fn drop(&mut self) {
        let _ = self.rpc_opt("shutdown", json!({}));
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
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
    let port = start_ui(pm.path().to_path_buf(), d.state());
    let host = format!("127.0.0.1:{port}");

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
fn ui_agent_detail_route_and_guards() {
    let pm = TempDir::new().unwrap();
    let d = UiDaemon::start();
    seed(pm.path(), &d.state());
    d.rpc(
        "agent_register",
        json!({"alias": "wk", "provider": "fake",
               "endpoint_kind": "fake", "cwd": pm.path().to_str().unwrap()}),
    );
    let port = start_ui(pm.path().to_path_buf(), d.state());
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
    let port = start_ui(pm.path().to_path_buf(), d.state());
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

    // A tracker write moves the mtime fingerprint → `event: issues`.
    std::fs::write(pm.path().join("poke.txt"), "x").unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut got_issues = false;
    while Instant::now() < deadline {
        match s.read(&mut tmp) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                raw.extend_from_slice(&tmp[..n]);
                if String::from_utf8_lossy(&raw).contains("event: issues") {
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
                if String::from_utf8_lossy(&raw).contains("event: agents") {
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
                if String::from_utf8_lossy(&raw).contains("event: jobs") {
                    got_jobs = true;
                    break;
                }
            }
        }
    }
    assert!(got_jobs, "no jobs event within 8s");
}

/// Poll `agent_show` until `pred` holds or the deadline passes — the
/// board-test equivalent of integration's wait_agent.
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

    let port = start_ui(pm.path().to_path_buf(), d.state());
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
