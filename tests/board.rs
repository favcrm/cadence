//! Board e2e: the `cadence issue` CLI against a temp PM dir, and the
//! `cadence ui` HTTP server in-process — routes, host/method/id/traversal
//! rejection, and daemon-unreachable honesty.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use cadence_agent::issue::{board, time};
use cadence_agent::store::Store;
use cadence_agent::ui;
use cadence_agent::{client, daemon};
use serde_json::{json, Value};
use tempfile::TempDir;

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
    start_ui_opts(pm_dir, state_dir, |_| {})
}

/// `start_ui` with `ServeOpts` overrides (`f` runs after the free port
/// is chosen — the port itself is always the probe-verified one).
fn start_ui_opts(
    pm_dir: PathBuf,
    state_dir: PathBuf,
    f: impl Fn(&mut ui::ServeOpts) + Send + Sync + 'static,
) -> u16 {
    let f = std::sync::Arc::new(f);
    let overall = Instant::now() + Duration::from_secs(20);
    loop {
        let port = free_port();
        let (sd, pd, f) = (state_dir.clone(), pm_dir.clone(), f.clone());
        thread::spawn(move || {
            let mut opts = ui::ServeOpts {
                host: "127.0.0.1".to_string(),
                port,
                ..Default::default()
            };
            f(&mut opts);
            opts.port = port;
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
    state: PathBuf,
    _tmp: Option<TempDir>,
    handle: Option<thread::JoinHandle<()>>,
}

impl UiDaemon {
    fn start() -> Self {
        let tmp = TempDir::new().unwrap();
        Self::serve(tmp.path().to_path_buf(), Some(tmp))
    }

    /// Serve on a caller-owned state dir — for fixtures whose `state`
    /// the cli-under-test already points at.
    fn start_on(state: PathBuf) -> Self {
        Self::serve(state, None)
    }

    fn serve(state: PathBuf, tmp: Option<TempDir>) -> Self {
        let owned = state.clone();
        let handle = thread::spawn(move || {
            let _ = daemon::serve(&owned);
        });
        let d = Self {
            state,
            _tmp: tmp,
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
        client::rpc(&self.state, method, params)
    }

    fn rpc(&self, method: &str, params: Value) -> Value {
        self.rpc_opt(method, params).unwrap()
    }

    fn state(&self) -> PathBuf {
        self.state.clone()
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

    let port = start_ui(pm.path().to_path_buf(), d.state());
    let host = format!("127.0.0.1:{port}");
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
    let read_only_port = start_ui_opts(pm.path().to_path_buf(), d.state(), |opts| {
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

// ---------- CAD-41: issue history from git plumbing ----------

/// A tracker whose CAD-1 has a varied, deterministic write history:
/// created → two CLI sets → link → comment → attach → one HTTP PATCH
/// (the ` (operator (ui))` actor) last, so `diff`'s default lands on a
/// field change. Shas per step are captured for blame/diff asserts.
struct HistFx {
    pm: TempDir,
    state: TempDir,
    port: u16,
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
    let port = start_ui(pm.path().to_path_buf(), state.path().to_path_buf());
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
    // ` (operator (ui))`.
    let (_, detail) = http(port, "GET", "/api/issues/CAD-1", &host);
    let rev = serde_json::from_str::<Value>(&detail).unwrap()["rev"]
        .as_str()
        .unwrap()
        .to_string();
    let (code, _, body) = write_json(
        port,
        "PATCH",
        "/api/issues/CAD-1",
        &host,
        &format!(r#"{{"status":"review","if_rev":"{rev}"}}"#),
    );
    assert_eq!(code, 200, "{body}");
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
    // The route is read-only — POST has no write route to reach.
    let (code, _) = http(fx.port, "POST", "/api/issues/CAD-1/history", &host);
    assert_eq!(code, 404);
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
fn commits_fixture() -> (TempDir, TempDir, TempDir, u16) {
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
    let port = start_ui(pm.path().to_path_buf(), state.path().to_path_buf());
    (pm, state, repo, port)
}

#[test]
fn issue_detail_lists_code_commits() {
    let (pm, state, repo, port) = commits_fixture();
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
    let (pm, state, repo, _port) = commits_fixture();
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
    let port = start_ui(pm.to_path_buf(), state.to_path_buf());
    let host = format!("127.0.0.1:{port}");
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
    let port = start_ui(pm.to_path_buf(), state.to_path_buf());
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

#[test]
fn issue_trailer_prints_and_validates() {
    let (pm, state, _repo, _port) = commits_fixture();
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

    // cwd inside the repo resolves without --repo.
    let (ok, out) = cli_dir(
        &pm,
        &state,
        &repo,
        &["issue", "start", "D-1", "--name", "from-cwd"],
    );
    assert!(ok, "{out}");
    assert!(out["worktree"].as_str().unwrap().ends_with("d-1-from-cwd"));

    // Single-repo fallback (cwd is the test process — not a project repo).
    let (ok, out) = cli(&pm, &state, &["issue", "start", "D-1", "--name", "single"]);
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
    let (ok, _) = cli(&pm, &state, &["issue", "start", "D-2"]);
    assert!(ok);
    let front = std::fs::read_to_string(pm.join("demo/D-2/issue.md")).unwrap();
    assert!(
        front.contains("status: review") && front.contains("owner: bob"),
        "{front}"
    );
}

// ---- CAD-55: `cadence dispatch` + `cadence issue finish` ----

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

    // --keep-branch leaves the local branch but still closes the refs.
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
    // and never blocks — the branch tip is main's, so "ancestry".
    assert!(cli(&pm, &state, &["issue", "new", "Sy", "--project", "demo"]).0);
    assert!(cli(&pm, &state, &["issue", "start", "D-4"]).0);
    let wt = repo.join(".cadence/wt/d-4-sy");
    std::fs::create_dir_all(wt.join("ui")).unwrap();
    std::fs::create_dir_all(wt.join("real_nm")).unwrap();
    std::os::unix::fs::symlink("../real_nm", wt.join("ui/node_modules")).unwrap();
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
    // identity probe resolving to `<login> (tailscale)`.
    let (ok, text) = cli_raw_env(
        pm.path(),
        state.path(),
        &["ui", "tailscale", "status"],
        &env,
    );
    assert!(ok, "{text}");
    assert!(text.contains(&format!("https://{TS_DNS}:9450")), "{text}");
    assert!(text.contains("(live)"), "{text}");
    assert!(text.contains("(tailscale)"), "{text}");

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

fn tailnet_opts() -> impl Fn(&mut ui::ServeOpts) {
    |o| {
        o.tailnet = Some((TS_DNS.to_string(), 9450));
        o.allow_hosts = vec![TS_DNS.to_string(), format!("{TS_DNS}:9450")];
        o.allow_origins = vec![format!("https://{TS_DNS}:9450")];
    }
}

#[test]
fn tailnet_write_is_attributed() {
    let (pm, state) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    seed(pm.path(), state.path());
    let port = start_ui_opts(
        pm.path().to_path_buf(),
        state.path().to_path_buf(),
        tailnet_opts(),
    );
    let ts_host = format!("{TS_DNS}:9450");
    let origin = format!("https://{TS_DNS}:9450");

    // A write shaped exactly like the proxy's: tailnet Host, https
    // Origin, identity headers — attributed to the tailnet user.
    let headers = ts_write_headers(&origin, "fable@example.com");
    let href: Vec<&str> = headers.iter().map(String::as_str).collect();
    let (code, _, _) = http_write(
        port,
        "PATCH",
        "/api/issues/CAD-2",
        &ts_host,
        &href,
        br#"{"status":"done"}"#,
    );
    assert_eq!(code, 200);
    let sha = sha_of(pm.path(), "cadence/CAD-2", "status=done");
    assert!(
        trailers_of(pm.path(), &sha).contains("Actor: fable@example.com (tailscale)"),
        "{}",
        trailers_of(pm.path(), &sha)
    );

    // /api/meta reports the same identity + tailnet URL.
    let (code, _, body) = http_write(
        port,
        "GET",
        "/api/meta",
        &ts_host,
        &[
            "Tailscale-User-Login: fable@example.com",
            "Tailscale-User-Name: Some User",
        ],
        b"",
    );
    assert_eq!(code, 200);
    let meta: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(meta["actor"], "fable@example.com (tailscale)");
    assert_eq!(meta["tailnet_url"], origin);
    assert_eq!(meta["read_only"], false);
}

#[test]
fn forged_tailscale_headers_not_attributed() {
    let (pm, state) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    seed(pm.path(), state.path());
    let port = start_ui_opts(
        pm.path().to_path_buf(),
        state.path().to_path_buf(),
        tailnet_opts(),
    );
    let host = format!("127.0.0.1:{port}");

    // Same headers, but the Host is direct loopback — the identity
    // headers must be ignored (the proxy only sets them on the
    // tailnet name).
    let headers = ts_write_headers(&format!("http://127.0.0.1:{port}"), "mallory@evil.example");
    let href: Vec<&str> = headers.iter().map(String::as_str).collect();
    let (code, _, _) = http_write(
        port,
        "PATCH",
        "/api/issues/CAD-2",
        &host,
        &href,
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
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap()["actor"],
        "operator (ui)"
    );
}

#[test]
fn tailnet_write_wrong_origin_refused() {
    let (pm, state) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    seed(pm.path(), state.path());
    let port = start_ui_opts(
        pm.path().to_path_buf(),
        state.path().to_path_buf(),
        tailnet_opts(),
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
    let port = start_ui_opts(pm.path().to_path_buf(), state.path().to_path_buf(), |o| {
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
    let port = start_ui(pm.path().to_path_buf(), state.path().to_path_buf());
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

#[test]
fn memory_stale_flags_changed_paths() {
    let (_t, pm, state, repo) = mem_fx();
    let verified = time::iso(time::now_epoch());
    legacy_memory(
        &pm,
        "src-watch",
        "rule",
        &["--scope-path", "src/**"],
        "accepted",
        Some(&verified),
    );
    legacy_memory(
        &pm,
        "docs-watch",
        "rule",
        &["--scope-path", "docs/**"],
        "accepted",
        Some(&verified),
    );
    // verified_at == now; the change must land strictly after. Staleness is
    // an informational reader path and does not require trust in a legacy
    // acceptance receipt.
    std::thread::sleep(Duration::from_millis(1100));
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

    // An accepted memory whose verified_at predates the window is
    // stale even with no path scope at all — hand-age the file.
    let aged = pm.join("mem/memory/docs-watch.md");
    let text = std::fs::read_to_string(&aged)
        .unwrap()
        .replace("verified_at:", "verified_at: 2020-01-01 # was ");
    std::fs::write(&aged, text).unwrap();
    let (ok, out) = mem_cli(&pm, &state, &["ls", "--stale", "--days", "30", "--json"]);
    assert!(ok, "{out}");
    let stale = out["stale"].as_array().unwrap();
    let aged_entry = stale
        .iter()
        .find(|s| s["slug"] == "docs-watch")
        .expect("aged memory is stale");
    assert_eq!(
        aged_entry["reason"].as_str().unwrap(),
        "not verified within the window"
    );
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

#[allow(clippy::zombie_processes)] // UiProc's Drop kills + waits.
fn spawn_ui(pm: &Path, state: &Path) -> (u16, UiProc) {
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

/// A `verified_at` that isn't ASCII — e.g. `abcé-01-01` — must not
/// panic the stale scan (the old slicer cut mid-char).
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
    let file = pm.join("mem/memory/bad-date.md");
    let text = std::fs::read_to_string(&file)
        .unwrap()
        .replace("verified_at:", "verified_at: abc\u{e9}-01-01 # was ");
    std::fs::write(&file, text).unwrap();

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

/// A fake `gh` binary dir: `pr` calls answer $FAKE_GH_PRS, `api` calls
/// answer $FAKE_GH_CI, FAKE_GH_FAIL=1 makes every call exit 1. The call
/// log records argv lines.
const FAKE_GH: &str = r#"#!/bin/sh
echo "$*" >> "$FAKE_GH_LOG"
if [ "$FAKE_GH_FAIL" = "1" ]; then echo "gh: simulated outage" >&2; exit 1; fi
case "$1" in
  pr) printf '%s' "$FAKE_GH_PRS" ;;
  api) printf '%s' "$FAKE_GH_CI" ;;
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
    let port = start_ui(pm.path().to_path_buf(), state.path().to_path_buf());
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
            ("FAKE_GH_CI", r#"{"state":"success","statuses":[]}"#),
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
    // gh was asked exactly once per repo for each of the two queries.
    let calls = std::fs::read_to_string(&gh.log).unwrap();
    assert_eq!(calls.lines().count(), 2, "{calls}");
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
            ("FAKE_GH_CI", "{}"),
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
            ("FAKE_GH_CI", r#"{"state":"success","statuses":[]}"#),
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
            ("FAKE_GH_CI", r#"{"state":"success","statuses":[]}"#),
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
        "schema: 1\nproject: {project}\ndocuments:\n  - id: guide\n    kind: index\n    path: {document_path}\n    title: Guide\n    required: true\n    roles: [pm, dev, qa, ops]\n"
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
    let port = start_ui(pm.path().to_path_buf(), state.path().to_path_buf());
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
    let port = start_ui(pm.path().to_path_buf(), state.path().to_path_buf());
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
    let port = start_ui(pm.path().to_path_buf(), state.path().to_path_buf());
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
    let port = start_ui(pm.path().to_path_buf(), state.path().to_path_buf());
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

    let port = start_ui(pm_dir.path().to_path_buf(), state.path().to_path_buf());
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
    let port = start_ui(pm.path().to_path_buf(), state.path().to_path_buf());
    let host = format!("127.0.0.1:{port}");
    let (code, body) = http(port, "GET", "/api/settings/model-defaults", &host);
    assert_eq!(code, 503, "{body}");
    let missing: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(missing["code"], "daemon_unavailable");

    let d = UiDaemon::start();
    let pm = TempDir::new().unwrap();
    let port = start_ui(pm.path().to_path_buf(), d.state());
    let host = format!("127.0.0.1:{port}");
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

    let (code, _, _) = http_write(
        port,
        "DELETE",
        "/api/settings/model-defaults",
        &host,
        &[],
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

    let read_only = start_ui_opts(pm.path().to_path_buf(), d.state(), |opts| {
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
