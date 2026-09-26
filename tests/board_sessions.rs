//! board_sessions: area tests split from tests/board.rs (CAD-537).
//! Board e2e: the `cadence issue` CLI against a temp PM dir, and the
//! `cadence ui` HTTP server in-process.
// A test binary never runs the CAD-308 reaper (only `daemon run` does),
// so its own spawns need not go through `cadence_agent::reaper`.
#![allow(clippy::disallowed_methods)]
mod board_common;
use board_common::*;

use cadence_agent::client;
use cadence_agent::issue::time;
use cadence_agent::store::Store;
use cadence_agent::ui;
use serde_json::json;
use serde_json::Value;
use std::io::Read;
use std::io::Write;
use std::net::TcpListener;
use std::net::TcpStream;
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::Duration;
use std::time::Instant;
use tempfile::TempDir;

/// `ui run` with the seam envs stripped — an unarmed board even on a
/// `test-seam` build, for the test that proves assertion headers are
/// refused outright when no fixture credential exists.
#[cfg(feature = "test-seam")]
fn spawn_ui_unarmed(pm: &Path, state: &Path) -> (u16, UiProc) {
    spawn_ui_seam(pm, state, &[], false)
}

// ---- CAD-482: the board's assertion headers bind the fixture credential ----

/// The board-path twin of `forged_token_and_half_assertions_are_refused`
/// in tests/test_seam.rs: `scope_headers` (src/test_seam.rs) is the only
/// check that `X-Cadence-Test-As` needs the fixture's minted token, so
/// each refusal is exercised on the wire — an As header alone, a token
/// alone, a wrong token — each must 403, never fall back to ambient.
#[cfg(feature = "test-seam")]
#[test]
fn seam_board_headers_require_the_fixture_token() {
    let (_t, pm, state, _repo) = start_fx();
    let _d = UiDaemon::start_on(state.clone()); // armed: mints the token
    let (port, _ui) = spawn_ui(&pm, &state); // armed board
    let host = format!("127.0.0.1:{port}");
    let get = |headers: &[&str]| http_write(port, "GET", "/api/health", &host, headers, b"");

    // `X-Cadence-Test-As` alone — a half assertion.
    let (status, _, body) = get(&["X-Cadence-Test-As: operator"]);
    assert_eq!(status, 403, "{body}");
    assert!(body.contains("travel together"), "{body}");

    // `X-Cadence-Test-Token` alone — the other half.
    let (status, _, body) = get(&["X-Cadence-Test-Token: some-token"]);
    assert_eq!(status, 403, "{body}");
    assert!(body.contains("travel together"), "{body}");

    // Both halves present, but the token is not the fixture's.
    let (status, _, body) = get(&[
        "X-Cadence-Test-As: operator",
        "X-Cadence-Test-Token: not-the-fixtures-token",
    ]);
    assert_eq!(status, 403, "{body}");
    assert!(body.contains("seam token"), "{body}");

    // And the real pair is honored — the refusals above came from the
    // credential check, not from the headers merely being present.
    let token = cadence_agent::test_seam::Seam::token_at(&state).unwrap();
    let as_h = "X-Cadence-Test-As: operator".to_string();
    let tok_h = format!("X-Cadence-Test-Token: {token}");
    let (status, _, body) = get(&[&as_h, &tok_h]);
    assert_eq!(status, 200, "a correctly-bound assertion must pass: {body}");
}

/// Headers sent to a board that never armed — `ui run` without the
/// seam envs on a state dir carrying no minted token — refuse
/// outright: an assertion cannot smuggle onto a board that did not
/// opt in.
#[cfg(feature = "test-seam")]
#[test]
fn unarmed_board_refuses_assertion_headers() {
    let (_t, pm, state, _repo) = start_fx();
    // No daemon, so no minted token: the board cannot attach even if it
    // tried (`ui run` re-attaches on the token's presence alone).
    let (port, _ui) = spawn_ui_unarmed(&pm, &state);
    let host = format!("127.0.0.1:{port}");
    let (status, _, body) = http_write(
        port,
        "GET",
        "/api/health",
        &host,
        &[
            "X-Cadence-Test-As: operator",
            "X-Cadence-Test-Token: anything",
        ],
        b"",
    );
    assert_eq!(status, 403, "{body}");
    assert!(body.contains("test seam armed"), "{body}");
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
    // nothing is written. Under the seam the write asserts
    // `agent:board-agent` on the wire — an agent presenting the
    // operator's session trips CAD-313's stolen-session check on the
    // board itself, which revokes it before the relay runs.
    let (agent_port, _agent_ui) =
        spawn_ui_env(pm.path(), &d.state(), &[("CADENCE_ALIAS", "board-agent")]);
    let agent_host = format!("127.0.0.1:{agent_port}");
    let mut agent_op = sign_in(&d.state(), agent_port);
    if !agent_op.seam.is_empty() {
        agent_op.seam = op::seam_headers(&d.state(), "agent:board-agent");
    }
    let (code, _, body) = op_write_json(
        &agent_op,
        agent_port,
        "POST",
        "/api/settings/model-defaults",
        &agent_host,
        doc,
    );
    if agent_op.seam.is_empty() {
        assert_eq!(code, 400, "{body}");
        assert!(
            body.contains("not provably the operator") && body.contains("carries CADENCE_ALIAS"),
            "{body}"
        );
    } else {
        assert_eq!(code, 403, "{body}");
        assert!(
            body.contains("session_from_agent") && body.contains("board-agent"),
            "{body}"
        );
    }
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

    let _ = d.operator_rpc(
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
        seam: op::seam_headers(&d.state(), "operator"),
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

    // An agent's environment, however detached — on an armed fixture
    // the child asserts `agent:pane-l` outright; ambiently the alias
    // on its ancestry is what refuses it.
    let (ok, out, err) = op::cli_as(
        bin(),
        &d.state(),
        &["ui", "login", "--json", "--port", &port.to_string()],
        &[("CADENCE_ALIAS", "pane-l")],
        "agent:pane-l",
    );
    assert!(!ok, "{out}");
    assert!(
        err.contains("pane-l") || err.contains("CADENCE_ALIAS"),
        "{err}"
    );

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
    // The replay is presented BY the agent, not the operator: on a
    // seam-armed board the request asserts `agent:pane-t` (the agent a
    // planted pane would be); without the seam it asserts nothing and
    // the pane ancestry does the same job.
    let request = s.request_as(
        "POST",
        "/api/issues/CAD-3/comments",
        r#"{"body":"stolen"}"#,
        &op::seam_headers(&d.state(), "agent:pane-t"),
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
        // The replaying process is a planted pane, not the operator:
        // assert no seam identity so the pane's own (agent) caller
        // stands — in a pane and in CI alike.
        let req = s.request_as(
            "POST",
            "/api/issues/CAD-3/comments",
            &format!(r#"{{"body":"hit and run {n}"}}"#),
            "",
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
        // CAD-561: the Update button and its check are the same class —
        // an agent's process can never start an update, and the refusal
        // lands before the route reads anything.
        ("/api/update", "{}"),
        ("/api/update/check", "{}"),
    ] {
        let out = Command::new("bash")
            .args([
                "-c",
                r#"exec 3<>"/dev/tcp/127.0.0.1/$PORT"; printf '%s' "$REQ" >&3; cat <&3"#,
            ])
            .env("CADENCE_ALIAS", "some-agent")
            .env("PORT", port.to_string())
            // The caller fails the process proof, so it must not carry
            // the operator's seam assertion — ambient identity stands.
            .env("REQ", s.request_as("POST", path, body, ""))
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

/// CAD-561 r2: the Update button starts a detached helper process that
/// outlives the board — the switch restarts the board, so a pipeline
/// running inside it would SIGTERM its own process before `ui start`.
/// This proves the board half: the spawn contract (own session, the log
/// on argv, no agent identity), the run-log protocol, and that the
/// board replacing it keeps reading the same log to the end. The
/// pipeline's health check, rollback and lease release are the fake
/// host's in tests/update.rs.
#[test]
fn cad561_a_board_initiated_update_survives_the_board_being_replaced() {
    let pm = TempDir::new().unwrap();
    let state = TempDir::new().unwrap();
    seed(pm.path(), state.path());
    let d = UiDaemon::start_on(state.path().to_path_buf());
    let helper = write_fake_update_helper(state.path());
    let log = state.path().join(cadence_agent::update::PROGRESS_FILE);
    let (port, board) = start_ui_opts(
        pm.path().to_path_buf(),
        state.path().to_path_buf(),
        move |opts| opts.update_helper = Some(helper.clone()),
    );
    let s = sign_in(&d.state(), port);
    let host = op::board_host(port);
    let (code, _, body) = op_write_json(&s, port, "POST", "/api/update", &host, "{}");
    assert_eq!(code, 200, "{body}");
    assert!(body.contains("\"started\": true"), "{body}");

    // The helper is the board's own `cadence update`: the state dir and
    // the log on argv, its own session leader (the restart stops the
    // board by pid — a helper in the board's session would be a
    // candidate for a group signal), and no agent identity in its env.
    let contract = wait_json(&state.path().join("helper-contract.json"));
    let argv: Vec<String> = contract["argv"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert_eq!(argv[0], "--state-dir", "{argv:?}");
    assert_eq!(argv[1], state.path().to_str().unwrap(), "{argv:?}");
    assert_eq!(argv[2], "update", "{argv:?}");
    assert_eq!(argv[3], "--as", "{argv:?}");
    assert_eq!(argv[4], "operator (ui)", "{argv:?}");
    assert_eq!(argv[5], "--progress", "{argv:?}");
    assert_eq!(argv[6], log.to_str().unwrap(), "{argv:?}");
    assert_eq!(contract["alias"], Value::Null, "CADENCE_ALIAS rode in");
    assert_eq!(contract["as_env"], Value::Null, "a seam assertion rode in");
    assert_eq!(
        contract["sid"], contract["pid"],
        "the helper must lead its own session: {contract}"
    );
    let helper_pid = contract["pid"].as_u64().unwrap() as u32;

    // The card shows the run in flight, from the log.
    let status = wait_update_running(port, &host);
    assert_eq!(status["result"], Value::Null, "{status}");
    assert!(has_line(&status, "draining: swe-554 (12m)"), "{status}");

    // The switch replaces the board: this one stops (a restart SIGTERMs
    // exactly its pid), the helper keeps going, and the board that
    // replaces it reads the same log.
    drop(board);
    wait_port_closed(port);
    assert!(
        pid_alive(helper_pid),
        "the helper must outlive the board that started it"
    );
    let (port2, _board2) = start_ui_opts(pm.path().to_path_buf(), state.path().to_path_buf(), |_| {});
    let host2 = op::board_host(port2);
    let status = wait_update_running(port2, &host2);
    assert!(has_line(&status, "draining: swe-554 (12m)"), "{status}");

    // The run ends: the replacement board shows the result, and the
    // helper is gone.
    std::fs::write(log.with_extension("jsonl.go"), "").unwrap();
    let status = wait_update_done(port2, &host2);
    assert_eq!(status["running"], json!(false), "{status}");
    assert_eq!(status["result"]["rolled_back"], json!(false), "{status}");
    assert_eq!(status["error"], Value::Null, "{status}");
    wait_pid_gone(helper_pid);
}

/// The board's detached update helper, faked: a process that speaks the
/// run-log protocol the real `cadence update --progress` writes,
/// records its spawn contract, and waits for a go file so the test
/// controls when the run ends.
fn write_fake_update_helper(state: &Path) -> std::path::PathBuf {
    let path = state.join("fake-update-helper.py");
    let script = r#"#!/usr/bin/env python3
import json, os, sys, time

args = sys.argv[1:]
progress = args[args.index("--progress") + 1]
state = args[args.index("--state-dir") + 1]
with open(os.path.join(state, "helper-contract.json"), "w") as f:
    json.dump({"argv": args, "pid": os.getpid(), "sid": os.getsid(0),
               "alias": os.environ.get("CADENCE_ALIAS"),
               "as_env": os.environ.get("CADENCE_TEST_AS")}, f)
with open(progress, "w") as f:
    f.write(json.dumps({"update_run": "running", "pid": os.getpid(),
                        "by": "operator (ui)", "at": time.time()}) + "\n")
    f.write("draining: swe-554 (12m)\n")
    f.write("drained: quiet after 3s\n")
go = progress + ".go"
while not os.path.exists(go):
    time.sleep(0.02)
with open(progress, "a") as f:
    f.write("switching: restarting the daemon on " + "b" * 40 + "\n")
    f.write(json.dumps({"update_run": "finished", "at": time.time(),
                        "report": {"rolled_back": False,
                                   "check": {"target": "b" * 40}}}) + "\n")
"#;
    std::fs::write(&path, script).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn has_line(status: &Value, want: &str) -> bool {
    status["lines"]
        .as_array()
        .is_some_and(|lines| lines.iter().any(|l| l.as_str() == Some(want)))
}

fn wait_update_running(port: u16, host: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let status: Value =
            serde_json::from_str(&http(port, "GET", "/api/update", host).1).unwrap();
        if status["running"] == json!(true) {
            return status;
        }
        assert!(Instant::now() < deadline, "the update never read as running: {status}");
        thread::sleep(Duration::from_millis(50));
    }
}

fn wait_update_done(port: u16, host: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let status: Value =
            serde_json::from_str(&http(port, "GET", "/api/update", host).1).unwrap();
        if status["running"] == json!(false) && status["result"].is_object() {
            return status;
        }
        assert!(Instant::now() < deadline, "the update never finished: {status}");
        thread::sleep(Duration::from_millis(50));
    }
}

fn wait_json(path: &Path) -> Value {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(text) = std::fs::read_to_string(path) {
            if let Ok(value) = serde_json::from_str(&text) {
                return value;
            }
        }
        assert!(Instant::now() < deadline, "{} never appeared", path.display());
        thread::sleep(Duration::from_millis(20));
    }
}

/// Is `pid` a live process — not gone, not a zombie?
fn pid_alive(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
        stat.rsplit_once(')')
            .is_some_and(|(_, rest)| !rest.trim_start().starts_with('Z'))
    })
}

fn wait_pid_gone(pid: u32) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while pid_alive(pid) {
        assert!(Instant::now() < deadline, "pid {pid} never exited");
        thread::sleep(Duration::from_millis(20));
    }
}

/// The board's port stops answering (the in-process owner's stop).
fn wait_port_closed(port: u16) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while TcpStream::connect(("127.0.0.1", port)).is_ok() {
        assert!(Instant::now() < deadline, "port {port} never closed");
        thread::sleep(Duration::from_millis(20));
    }
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
        // Replayed by an unattributable process: no seam assertion, the
        // caller is whatever the socket says — nothing, here.
        let req = s.request_as(
            "POST",
            "/api/issues/CAD-3/comments",
            &format!(r#"{{"body":"no agent, hit and run {n}"}}"#),
            "",
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
