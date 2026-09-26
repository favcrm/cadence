//! board_http: area tests split from tests/board.rs (CAD-537).
//! Board e2e: the `cadence issue` CLI against a temp PM dir, and the
//! `cadence ui` HTTP server in-process.
// A test binary never runs the CAD-308 reaper (only `daemon run` does),
// so its own spawns need not go through `cadence_agent::reaper`.
#![allow(clippy::disallowed_methods)]
mod board_common;
use board_common::*;

use serde_json::Value;
use std::io::Read;
use std::io::Write;
use std::net::TcpListener;
use std::net::TcpStream;
use std::process::Command;
use std::thread;
use std::time::Duration;
use std::time::Instant;
use tempfile::TempDir;

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
