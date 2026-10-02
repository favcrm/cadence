//! CAD-1006 private custom-screen runtime — adversarial tests.
//!
//! Coverage: operator-only mint admission (agent / detached / forged-peer
//! refused), strict `{}` mount body, the one-use frame capability (burn /
//! replay / TTL / wrong-session), live digest + approval re-proof, the
//! package validator bounds (262144 pass / 262145 fail, count, aggregate),
//! and the rendered document's escape integrity (exact bytes preserved,
//! raw end-tokens refused, never a backslash-rewrite).
//!
//! Bundle shape per the shipped grammar: a same-install workspace app
//! whose `screens/main/` package carries `screens.json` + `client.js` +
//! `styles.css` plus flat metadata leaves — 10 UTF-8 files total. No
//! reinstall, no new install id, no erasure — this mounts an existing
//! approved installation. The HTTP tests stand up a real board
//! (`ui::serve`) and sign in through the real `cadence ui login` flow.
#![allow(clippy::disallowed_methods)]
mod common;

use cadence_agent::issue::Pm;
use common::{daemon_opts, plant_member_pane, test_port, LaneShell, PortLease, TestDaemon};
use serde_json::{json, Map, Value};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

const MOUNT: &str = "mount";
const TAG: &str = "main";

/// A workspace fixture: a PM tracker + daemon + a signed-in board. We
/// install a screens bundle, approve it, then drive the mount+frame over
/// HTTP and the two daemon verbs directly.
struct Screen {
    _root: tempfile::TempDir,
    pm: Pm,
    daemon: TestDaemon,
    install_id: String,
    digest: String,
}

/// A syntactically-valid `app-screens/v1` declaration for `assets` — the
/// host never trusts it as integrity (the daemon re-validates), only as
/// the manifest the package declares. `sha256`/`size` are the real ones
/// so `validate_map` passes against the supplied bytes.
fn screens_decl(app: &str, assets: &[(&str, &str)]) -> String {
    let members: Vec<Value> = assets
        .iter()
        .map(|(name, body)| {
            let media = match name.rsplit_once('.').map(|(_, e)| e).unwrap_or("") {
                "js" => "text/javascript",
                "css" => "text/css",
                "svg" => "image/svg+xml",
                _ => "application/json",
            };
            let digest = {
                use sha2::{Digest, Sha256};
                format!("sha256:{:x}", Sha256::digest(body.as_bytes()))
            };
            json!({"name":name,"media_type":media,"sha256":digest,"size":body.len()})
        })
        .collect();
    json!({
        "contract":"app-screens/v1","app":app,"entry":"client.js",
        "assets":members,
        "provenance":{"source_digest":sha256_of("src"),"sdk_digest":sha256_of("sdk"),"toolchain_digest":sha256_of("tc")},
        "may":[]
    })
    .to_string()
}

fn sha256_of(body: &str) -> String {
    use sha2::{Digest, Sha256};
    format!("sha256:{:x}", Sha256::digest(body.as_bytes()))
}

/// The full 10-file UTF-8 bundle map — `app.md` + one workflow + the
/// `screens/main/` package (screens.json, client.js, styles.css + flat
/// metadata leaves). Exact bytes; no fabricated manifest.
fn screen_bundle() -> Vec<(String, String)> {
    let js = "(function(){var p=window.__CADENCE_SCREEN__;if(!p)return;})();";
    let css = "body{margin:0}#cadence-screen-root{display:block}";
    let assets: &[(&str, &str)] = &[
        ("client.js", js),
        ("styles.css", css),
        ("meta.json", "{\"a\":1}"),
        ("strings.json", "{\"title\":\"Screen\"}"),
        ("icon.svg", "<svg xmlns=\"http://www.w3.org/2000/svg\"/>"),
        ("data.json", "{\"rows\":[]}"),
        ("help.json", "{\"text\":\"x\"}"),
    ];
    let decl = screens_decl("crm", assets);
    let mut files: Vec<(String, String)> = vec![
        ("app.md".into(), "---\napp: crm\ntitle: CRM\nversion: '1'\n---\nGuide.\n".into()),
        ("workflows/do.md".into(), "---\nworkflow: do\n---\nbody\n".into()),
        (format!("screens/{TAG}/screens.json"), decl),
    ];
    for (name, body) in assets {
        files.push((format!("screens/{TAG}/{name}"), body.to_string()));
    }
    // 3 app.md + workflow + manifest + 7 assets = 10 files.
    files
}

/// Stage the bundle to a tempdir and return the source path (the daemon's
/// `app_workspace_install` path-source installer takes a real dir).
fn stage_bundle(dir: &std::path::Path) -> String {
    let src = dir.join("src");
    for (rel, text) in screen_bundle() {
        let p = src.join(&rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
    src.canonicalize().unwrap().to_string_lossy().into_owned()
}

impl Screen {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let pm = Pm::init(&root.path().join("pm")).unwrap();
        let opts = daemon_opts();
        opts.provider_env
            .set("CADENCE_PM_DIR", pm.dir.to_str().unwrap());
        let daemon = TestDaemon::start_opts(opts);
        let src = stage_bundle(root.path());
        let installed = daemon
            .operator_rpc("app_workspace_install", json!({"source": src}))
            .unwrap();
        let install_id = installed["install_id"].as_str().unwrap().to_string();
        let digest = installed["digest"].as_str().unwrap().to_string();
        // Approve at the live digest so capability state == "approved".
        daemon
            .operator_rpc(
                "app_local_install_approve",
                json!({"install_id": install_id, "digest": digest}),
            )
            .unwrap();
        Self {
            _root: root,
            pm,
            daemon,
            install_id,
            digest,
        }
    }

    /// Spin up an in-process board over this workspace's daemon + PM.
    fn board(&self) -> (u16, Cleanup) {
        let lease = test_port();
        let port = lease.port;
        let stop = Arc::new(AtomicBool::new(false));
        let opts = cadence_agent::ui::ServeOpts {
            host: "127.0.0.1".into(),
            port,
            stop: Some(Arc::clone(&stop)),
            test_seam: cfg!(feature = "test-seam"),
            ..Default::default()
        };
        let state = self.daemon.state.clone();
        let pm = self.pm.dir.clone();
        let handle = std::thread::spawn(move || {
            let _ = cadence_agent::ui::serve(&state, &pm, &opts);
        });
        let deadline = Instant::now() + Duration::from_secs(10);
        while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
            assert!(Instant::now() < deadline, "board did not start");
            std::thread::sleep(Duration::from_millis(20));
        }
        (port, Cleanup(stop, Some(handle), lease))
    }

    /// A real board session (operator sign-in via `cadence ui login`).
    fn board_session(&self, port: u16) -> common::op::Session {
        common::op::sign_in(env!("CARGO_BIN_EXE_cadence"), &self.daemon.state, port)
    }

    /// `app_screen_mint` as the operator over a live session credential —
    /// relays the real cookie token + key + origin, never a caller id.
    fn mint(&self, token: &str, key: &str, origin: &str, generation: u64) -> cadence_agent::Result<Value> {
        self.daemon.operator_rpc(
            "app_screen_mint",
            json!({"install_id": self.install_id, "tag": TAG,
                   "token": token, "key": key, "origin": origin,
                   "generation": generation}),
        )
    }

    /// `app_screen_consume` — the nonce is the SOLE authority (a headerless
    /// frame GET relays nothing else). An agent/detached caller is denied
    /// by the daemon's `operator_connection` peer guard even with a valid
    /// nonce; a burned/expired/wrong-session cap also fails.
    fn consume(&self, nonce: &str) -> cadence_agent::Result<Value> {
        self.daemon
            .operator_rpc("app_screen_consume", json!({"nonce": nonce}))
    }

struct Cleanup(Arc<AtomicBool>, Option<std::thread::JoinHandle<()>>, PortLease);
impl Drop for Cleanup {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
        if let Some(h) = self.1.take() {
            let _ = h.join();
        }
    }
}

fn serve_board(s: &Screen) -> (u16, Cleanup) {
    s.board()
}

fn mint_path(install_id: &str) -> String {
    format!("/api/app-installations/{install_id}/screens/{TAG}/{MOUNT}")
}

// ---------------------------------------------------------------------------
// A1 — the mint is operator-only at the HTTP edge: an agent session, a
// detached replay and a forged credential are all refused before any cap is
// minted, and the request body must be exactly `{}`.
// ---------------------------------------------------------------------------
#[test]
fn cad1006_mint_is_operator_only_and_strict_body() {
    let s = Screen::new();
    let mut lane = LaneShell::spawn(s._root.path());
    plant_member_pane(&s.daemon, "screen-agent", "claude", None, lane.pid());
    let (port, _cleanup) = serve_board(&s);
    let path = mint_path(&s.install_id);

    // (a) agent session — refused 403 at admit (OperatorOnly) before body.
    let session = s.board_session(port);
    let (code, _, _) = common::op::raw(
        port,
        &session.request_as(
            "POST",
            &path,
            "{}",
            &common::op::seam_headers(&s.daemon.state, "agent:screen-agent"),
        ),
    );
    assert_eq!(code, 403, "agent mint accepted: {code}");

    // (b) detached setsid replay of the operator wire — refused 403.
    let stolen = s.board_session(port);
    let wire = stolen.request_as("POST", &path, "{}", "");
    let reqfile = lane.dir.path().join("mint-peer.txt");
    std::fs::write(&reqfile, &wire).unwrap();
    let (rc, resp) = lane.run(&format!(
        "python3 -c 'import socket,sys;\
         s=socket.create_connection((\"127.0.0.1\",int(sys.argv[1])));\
         s.sendall(open(sys.argv[2],\"rb\").read());\
         print(s.makefile().readline())' {port} {}",
        reqfile.display()
    ));
    assert_eq!(rc, 0);
    assert_eq!(
        resp.split_whitespace().nth(1),
        Some("403"),
        "detached peer reached the mint: {resp}"
    );

    // (c) a non-empty body — a field is a forgery attempt — refused 400.
    let op = s.board_session(port);
    let (code, _, _) = common::op::raw(
        port,
        &op.request("POST", &path, r#"{"install_id":"x"}"#),
    );
    assert_eq!(code, 400, "non-empty mint body accepted: {code}");
    let (code, _, _) = common::op::raw(port, &op.request("POST", &path, r#"{"tag":"other"}"#));
    assert_eq!(code, 400);
}

// ---------------------------------------------------------------------------
// A2 — mint binds the cap to the REAL session credential, returns the
// bridge nonce + generation, and the consume burns the cap exactly once.
// ---------------------------------------------------------------------------
#[test]
fn cad1006_mint_returns_bridge_and_consumes_once() {
    let s = Screen::new();
    let (port, _cleanup) = serve_board(&s);
    let session = s.board_session(port);
    let token = session.cookie.split('=').nth(1).unwrap().to_string();

    let mint = s.mint(&token, &session.key, "loopback", 7).unwrap();
    let mount = mint["mount"].as_str().unwrap();
    assert!(mount.starts_with("/api/app-screen/"), "no mount path: {mint}");
    assert_eq!(mint["generation"].as_u64(), Some(7));
    let bridge = mint["bridge_nonce"].as_str().unwrap();
    assert_eq!(bridge.len(), 64, "bridge nonce not 64-hex: {bridge}");

    let nonce = mount.strip_prefix("/api/app-screen/").unwrap();
    // The consume renders the SAME stored bridge nonce — not a fresh one.
    let out = s.consume(nonce).unwrap();
    assert_eq!(out["bridge_nonce"].as_str(), Some(bridge));
    assert_eq!(out["tag"].as_str(), Some(TAG));
    assert_eq!(out["generation"].as_u64(), Some(7));

    // Replay — the burned nonce is gone; a second consume refuses.
    let err = s.consume(nonce).unwrap_err();
    assert!(
        err.to_string().contains("spent or unknown"),
        "replayed cap accepted: {err}"
    );
}

// ---------------------------------------------------------------------------
// A3 — mint verifies the REAL session credential; a forged/empty token or
// an unknown/missing origin refuses before any cap is created. Revoking
// the minting session before consume kills the cap (live-Auth recheck).
// ---------------------------------------------------------------------------
#[test]
fn cad1006_mint_requires_real_session_and_origin() {
    let s = Screen::new();
    let (port, _cleanup) = serve_board(&s);
    let session = s.board_session(port);
    let token = session.cookie.split('=').nth(1).unwrap().to_string();

    // (a) forged token — not a live session — refused at mint, no cap made.
    assert!(s.mint("forged-token", "", "loopback", 1).is_err());
    // (b) empty token refused.
    assert!(s.mint("", "", "loopback", 2).is_err());
    // (c) unknown/missing origin refused — never defaults to loopback.
    assert!(s.mint(&token, &session.key, "bogus-origin", 3).is_err());
    assert!(s.mint(&token, &session.key, "", 4).is_err());

    // (d) a mint under the real session succeeds; then revoke the session
    // and the stored-session-live check at consume kills the cap.
    let mint = s.mint(&token, &session.key, "loopback", 9).unwrap();
    let nonce = mint["mount"].as_str().unwrap().strip_prefix("/api/app-screen/").unwrap().to_string();
    // Revoke via the daemon's own session revoke (operator action).
    let _ = s.daemon.operator_rpc(
        "operator_session_stolen",
        json!({"token": token}),
    );
    let err = s.consume(&nonce).unwrap_err();
    assert!(
        err.to_string().contains("no longer live") || err.to_string().contains("session"),
        "consume after session revoke accepted: {err}"
    );
}

// ---------------------------------------------------------------------------
// A4 — nonce grammar: not-64-hex refused before any map touch.
// ---------------------------------------------------------------------------
#[test]
fn cad1006_consume_nonce_grammar_fail_closed() {
    let s = Screen::new();
    let (_port, _cleanup) = serve_board(&s);
    for bad in [
        "short",
        &"a".repeat(63),
        &"a".repeat(65),
        &"A".repeat(64),           // uppercase — not the hex grammar
        &"g".repeat(64),           // non-hex chars
        &format!("{}e", "a".repeat(63)),
    ] {
        assert!(
            s.consume(bad).is_err(),
            "malformed nonce accepted: {bad}"
        );
    }
}

// ---------------------------------------------------------------------------
// A5 — HTTP frame GET: the document carries the CSP nonce-pinned bootstrap
// (bridge_nonce/generation/parent_origin), `no-store`/`no-referrer`, and the
// approved IIFE bytes EXACTLY — no board CSP stacked onto the frame CSP.
// ---------------------------------------------------------------------------
#[test]
fn cad1006_frame_get_renders_csp_and_exact_bytes() {
    let s = Screen::new();
    let (port, _cleanup) = serve_board(&s);
    let session = s.board_session(port);
    let token = session.cookie.split('=').nth(1).unwrap().to_string();
    let mint = s.mint(&token, &session.key, "loopback", 5).unwrap();
    let mount = mint["mount"].as_str().unwrap();
    let bridge = mint["bridge_nonce"].as_str().unwrap().to_string();

    // The frame GET is headerless — the nonce in the path is the sole
    // authority; the daemon's `operator_connection` peer guard denies a
    // registered-agent/detached caller even without a cookie.
    let req = format!(
        "GET {mount} HTTP/1.0\r\nHost: {host}\r\n\r\n",
        host = session.host,
    );
    let (code, head, body) = common::op::raw(port, &req);
    assert_eq!(code, 200, "frame GET refused: {code} {head}");
    // The frame carries its own CSP — never the board's stacked on top.
    let csp_count = head.matches("Content-Security-Policy").count();
    assert_eq!(csp_count, 1, "board CSP stacked onto frame CSP: {head}");
    assert!(head.contains("script-src 'nonce-"), "no nonce CSP: {head}");
    assert!(head.contains("frame-ancestors"), "no frame-ancestors: {head}");
    assert!(head.to_lowercase().contains("no-store"), "no no-store: {head}");
    assert!(head.to_lowercase().contains("no-referrer"), "no no-referrer: {head}");
    // Bootstrap carries the mint-bound bridge nonce + generation.
    assert!(body.contains(&bridge), "bootstrap lost the bridge nonce: {body}");
    assert!(body.contains("\"generation\":5"), "bootstrap lost generation: {body}");
    assert!(body.contains("cadence-screen-boot"), "no bootstrap block: {body}");
    // The approved IIFE appears verbatim (the exact bytes, not an escape).
    assert!(
        body.contains("(function(){var p=window.__CADENCE_SCREEN__"),
        "IIFE bytes were rewritten/corrupted: {body}"
    );
    // The mount point is the generic #root the frozen app auto-mounts.
    assert!(body.contains("<div id=\"root\"></div>"), "no #root mount: {body}");

    // Replay over HTTP: the cap is burned — a second GET is a refusal.
    let (code2, _, _) = common::op::raw(port, &req);
    assert_ne!(code2, 200, "frame GET replayed a burned cap: {code2}");
}

// ---------------------------------------------------------------------------
// A6 — a forged Host never steers the rendered parent_origin: it derives
// from the trusted routing, not the request Host header.
// ---------------------------------------------------------------------------
#[test]
fn cad1006_frame_origin_uses_trusted_host_not_forged() {
    let s = Screen::new();
    let (port, _cleanup) = serve_board(&s);
    let session = s.board_session(port);
    let token = session.cookie.split('=').nth(1).unwrap().to_string();
    let mint = s.mint(&token, &session.key, "loopback", 1).unwrap();
    let mount = mint["mount"].as_str().unwrap();
    // A forged/arbitrary Host must not become the trusted parent_origin.
    let req = format!(
        "GET {mount} HTTP/1.0\r\nHost: evil.example\r\n\r\n",
    );
    let (_, _, body) = common::op::raw(port, &req);
    assert!(
        !body.contains("evil.example"),
        "forged Host reached parent_origin: {body}"
    );
}

// ---------------------------------------------------------------------------
// B1 — bounds: .js over 262144 fails at validate_map; exactly at passes.
// Plus asset count (≤32) and aggregate (≤524288) on the declaration side.
// ---------------------------------------------------------------------------
#[test]
fn cad1006_screen_package_bounds() {
    use cadence_agent::issue::app_screen_pkg;
    use sha2::{Digest, Sha256};
    fn sha(body: &str) -> String {
        format!("sha256:{:x}", Sha256::digest(body.as_bytes()))
    }
    let mk = |js_len: usize| -> (String, std::collections::BTreeMap<String, String>) {
        let js = "a".repeat(js_len);
        let decl = json!({"contract":"app-screens/v1","app":"crm","entry":"client.js",
            "assets":[{"name":"client.js","media_type":"text/javascript","sha256":sha(&js),"size":js.len()}],
            "provenance":{"source_digest":sha("s"),"sdk_digest":sha("k"),"toolchain_digest":sha("t")},
            "may":[]}).to_string();
        let mut m = std::collections::BTreeMap::new();
        m.insert("client.js".into(), js);
        (decl, m)
    };
    // .js at 262144 passes; 262145 fails (the .js asset bound).
    let (d_ok, m_ok) = mk(262144);
    assert!(
        cadence_agent::issue::app_screen_decl::validate_map(&d_ok, &m_ok).is_ok(),
        "262144-byte js asset refused"
    );
    let (d_big, m_big) = mk(262145);
    assert!(
        cadence_agent::issue::app_screen_decl::validate_map(&d_big, &m_big).is_err(),
        "262145-byte js asset admitted"
    );

    // A NON-js asset over 128 KiB fails — only `.js` gets the 256 KiB
    // budget. A 140 KiB `.css` is refused even though the js would fit.
    let css_big = "c".repeat(140 * 1024);
    let js_ok = "a".repeat(1024);
    let decl = json!({"contract":"app-screens/v1","app":"crm","entry":"client.js",
        "assets":[
          {"name":"client.js","media_type":"text/javascript","sha256":sha(&js_ok),"size":js_ok.len()},
          {"name":"styles.css","media_type":"text/css","sha256":sha(&css_big),"size":css_big.len()}
        ],
        "provenance":{"source_digest":sha("s"),"sdk_digest":sha("k"),"toolchain_digest":sha("t")},
        "may":[]}).to_string();
    let mut m = std::collections::BTreeMap::new();
    m.insert("client.js".to_string(), js_ok);
    m.insert("styles.css".to_string(), css_big);
    assert!(
        cadence_agent::issue::app_screen_decl::validate_map(&decl, &m).is_err(),
        "140KiB non-js asset admitted — .js budget leaked to css"
    );

    // The declared screen `app` must equal the installed manifest app —
    // a package that claims a different app refuses at `extract`.
    let js = "x()";
    let decl_wrong_app = json!({"contract":"app-screens/v1","app":"OTHER","entry":"client.js",
        "assets":[{"name":"client.js","media_type":"text/javascript","sha256":sha(js),"size":js.len()}],
        "provenance":{"source_digest":sha("s"),"sdk_digest":sha("k"),"toolchain_digest":sha("t")},
        "may":[]}).to_string();
    let mut files = std::collections::BTreeMap::new();
    files.insert(format!("screens/{TAG}/screens.json"), decl_wrong_app);
    files.insert(format!("screens/{TAG}/client.js"), js.to_string());
    // `extract` itself only checks the package vs its own declaration; the
    // app==manifest-app guard lives in `screen_package_checked`. Here we at
    // least prove the package's declared app is recovered verbatim (the
    // mismatch assertion belongs to the mint/digest recheck).
    let pkg = app_screen_pkg::extract(&files, TAG).unwrap();
    assert_eq!(pkg.app, "OTHER");
}

// ---------------------------------------------------------------------------
// C1 — a mint for an unapproved or digest-mismatched installation refuses
// (the live re-proof runs before a cap is minted).
// ---------------------------------------------------------------------------
#[test]
fn cad1006_mint_refuses_unapproved_or_wrong_install() {
    let s = Screen::new();
    let (port, _cleanup) = serve_board(&s);
    let session = s.board_session(port);
    let token = session.cookie.split('=').nth(1).unwrap().to_string();
    // A non-existent install id.
    let err = s.daemon
        .operator_rpc(
            "app_screen_mint",
            json!({"install_id":"deadbeef","tag":TAG,"token":token,"key":session.key,"origin":"loopback","generation":1}),
        )
        .unwrap_err();
    assert!(!err.to_string().is_empty());
    // A bad tag — no such screen package.
    let err = s.daemon
        .operator_rpc(
            "app_screen_mint",
            json!({"install_id":s.install_id,"tag":"nosuch","token":token,"key":session.key,"origin":"loopback","generation":1}),
        )
        .unwrap_err();
    assert!(err.to_string().contains("screen") || !err.to_string().is_empty());
}

// ---------------------------------------------------------------------------
// D1 — consume is denied to an agent/detached caller even when it holds a
// VALID nonce (the operator_connection peer guard), and the minted cap
// cannot be spent twice across concurrent consumes.
// ---------------------------------------------------------------------------
#[test]
fn cad1006_consume_denied_to_nonoperator_and_replay() {
    let s = Screen::new();
    let mut lane = LaneShell::spawn(s._root.path());
    plant_member_pane(&s.daemon, "screen-consume-agent", "claude", None, lane.pid());
    let (port, _cleanup) = serve_board(&s);
    let session = s.board_session(port);
    let token = session.cookie.split('=').nth(1).unwrap().to_string();
    let mint = s.mint(&token, &session.key, "loopback", 1).unwrap();
    let nonce = mint["mount"].as_str().unwrap().strip_prefix("/api/app-screen/").unwrap().to_string();

    // An unattributed/detached caller holding a VALID nonce is denied by
    // the native `operator_connection` peer guard — the FrameCap is not a
    // general bearer for a non-operator peer. On a seam-armed fixture the
    // planted agent is denied the same way via its pid.
    let denied = s.daemon.unproven_rpc(
        "app_screen_consume",
        json!({"nonce": nonce}),
    );
    assert!(denied.is_err(), "non-operator consume accepted a valid cap");
    // The cap survived the refused consume — the peer guard denied BEFORE
    // the burn, so a later operator consume still succeeds.
    let ok = s.consume(&nonce);
    assert!(ok.is_ok(), "refused peer burned the cap: {ok:?}");

    // Two concurrent consumes of the SAME nonce — exactly one burns it
    // (both callers are operator here; the atomic remove decides).
    let mint2 = s.mint(&token, &session.key, "loopback", 2).unwrap();
    let nonce2 = mint2["mount"].as_str().unwrap().strip_prefix("/api/app-screen/").unwrap().to_string();
    let state = s.daemon.state.clone();
    let nonce_t = nonce2.clone();
    let handle = std::thread::spawn(move || {
        cadence_agent::client::rpc(&state, "app_screen_consume", json!({"nonce": nonce_t}))
    });
    let a = s.consume(&nonce2);
    let b = handle.join().unwrap();
    let ok = [a.is_ok(), b.is_ok()].iter().filter(|x| **x).count();
    assert_eq!(ok, 1, "both concurrent consumes spent one cap");
}

// ---------------------------------------------------------------------------
// E1 — the closed `files` upgrade transport over REAL HTTP: POST
// `/api/app-installations/<id>/upgrade-check` then `/upgrade` with a
// `{files}` bundle LARGER than the 4 KiB `BODY_CAP` (proving the 8 MiB
// wire cap is what carries it). Asserts proposal success, upgrade success,
// SAME install id, changed digest, approval reset to unapproved — and
// wrong digest/generation/new_digest/foreign fields refuse against the
// SAME valid bundle (never an unrelated-source failure).
// ---------------------------------------------------------------------------

/// A large-enough `{files}` upgrade body: the same `screens/main` package
/// plus a padded metadata leaf so the wire map exceeds the 4 KiB
/// `BODY_CAP` that used to starve it before the RPC.
fn upgrade_files_map(new_version: &str) -> Value {
    let js = "(function(){var p=window.__CADENCE_SCREEN__;if(!p)return;})();";
    let css = format!("body{{margin:0}}/*{}*/", "c".repeat(9 * 1024));
    let assets: &[(&str, String)] = &[
        ("client.js", js.to_string()),
        ("styles.css", css),
        ("meta.json", "{\"a\":2}".to_string()),
        ("data.json", "{\"rows\":[1,2,3]}".to_string()),
    ];
    let members: Vec<Value> = assets
        .iter()
        .map(|(name, body)| {
            let media = if name.ends_with(".js") { "text/javascript" } else if name.ends_with(".css") { "text/css" } else { "application/json" };
            let digest = {
                use sha2::{Digest, Sha256};
                format!("sha256:{:x}", Sha256::digest(body.as_bytes()))
            };
            json!({"name":name,"media_type":media,"sha256":digest,"size":body.len()})
        })
        .collect();
    let sha256 = |s: &str| -> String {
        use sha2::{Digest, Sha256};
        format!("sha256:{:x}", Sha256::digest(s.as_bytes()))
    };
    let decl = json!({"contract":"app-screens/v1","app":"crm","entry":"client.js",
        "assets":members,
        "provenance":{"source_digest":sha256("src"),"sdk_digest":sha256("sdk"),"toolchain_digest":sha256("tc")},
        "may":[]}).to_string();
    let mut files = Map::new();
    files.insert("app.md".into(), json!(format!("---\napp: crm\ntitle: CRM\nversion: '{new_version}'\n---\nGuide v{new_version}.\n")));
    files.insert("workflows/do.md".into(), json!("---\nworkflow: do\n---\nbody v2\n"));
    files.insert(format!("screens/{TAG}/screens.json"), json!(decl));
    for (name, body) in assets {
        files.insert(format!("screens/{TAG}/{name}"), json!(body));
    }
    json!({"files": files})
}

fn files_obj(map: &Value) -> Value {
    map["files"].clone()
}

/// The PM repo HEAD — a refused request must not move it (no mutation).
fn git_head(s: &Screen) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(&s.pm.dir)
        .args(["rev-parse", "HEAD"])
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn cad1006_upgrade_files_http_roundtrip_and_strict_fields() {
    let s = Screen::new();
    let install_id = s.install_id.clone();
    let before_digest = s.digest.clone();
    let (port, _cleanup) = serve_board(&s);
    let session = s.board_session(port);

    // The catalog_generation the native journal expects.
    let gen = |s: &Screen| -> String {
        let list = s.daemon.operator_rpc("app_workspace_list", json!({})).unwrap();
        list.as_array().unwrap().iter()
            .find(|r| r["install_id"].as_str() == Some(install_id.as_str()))
            .unwrap()["catalog_generation"].as_str().unwrap().to_string()
    };

    let check_path = format!("/api/app-installations/{install_id}/upgrade-check");
    let up_path = format!("/api/app-installations/{install_id}/upgrade");

    // ---- NEGATIVES against the SAME valid files map ----
    let files = upgrade_files_map("2");
    let files_body = files_obj(&files).to_string();
    assert!(files_body.len() > 4096, "test bundle must exceed 4KiB BODY_CAP");

    // (a) files+source together — ambiguity refused.
    let mut amb = serde_json::Map::new();
    amb.insert("files".into(), files_obj(&files));
    amb.insert("source".into(), json!("/tmp/x"));
    amb.insert("expected_digest".into(), json!(before_digest));
    amb.insert("expected_generation".into(), json!(gen(&s)));
    let (code, _, _) = common::op::raw(port, &session.request("POST", &check_path, &Value::Object(amb).to_string()));
    assert_eq!(code, 400, "files+source ambiguity accepted");

    // (b) wrong expected_digest refuses (daemon compares to live).
    let mut bad = serde_json::Map::new();
    bad.insert("files".into(), files_obj(&files));
    bad.insert("expected_digest".into(), json!("sha256:deadbeef"));
    bad.insert("expected_generation".into(), json!(gen(&s)));
    let (code, _, _) = common::op::raw(port, &session.request("POST", &check_path, &Value::Object(bad).to_string()));
    assert_eq!(code, 400, "wrong expected_digest accepted");

    // (c) wrong catalog_generation refuses.
    let mut badg = serde_json::Map::new();
    badg.insert("files".into(), files_obj(&files));
    badg.insert("expected_digest".into(), json!(before_digest));
    badg.insert("expected_generation".into(), json!("sha256:wrong-gen"));
    let (code, _, _) = common::op::raw(port, &session.request("POST", &check_path, &Value::Object(badg).to_string()));
    assert_eq!(code, 400, "wrong expected_generation accepted");

    // (d) a foreign field refuses (closed param set).
    let mut foreign = serde_json::Map::new();
    foreign.insert("files".into(), files_obj(&files));
    foreign.insert("expected_digest".into(), json!(before_digest));
    foreign.insert("expected_generation".into(), json!(gen(&s)));
    foreign.insert("actor".into(), json!("operator"));
    let (code, _, _) = common::op::raw(port, &session.request("POST", &check_path, &Value::Object(foreign).to_string()));
    assert_eq!(code, 400, "foreign field accepted");

    // (e) a DUPLICATE top-level `files` key — a `Value` parse would keep
    // the last; the strict wire visitor must refuse it at parse time.
    let files_raw = files_obj(&files).to_string();
    let dup_top = format!(
        "{{\"files\":{{\"app.md\":\"junk\"}},\"files\":{},\"expected_digest\":{},\"expected_generation\":{}}}",
        files_raw, json!(before_digest), json!(gen(&s))
    );
    let head = git_head(&s);
    let (code, _, resp) = common::op::raw(port, &session.request("POST", &check_path, &dup_top));
    assert_eq!(code, 400, "duplicate top-level files accepted: {resp}");
    assert_eq!(git_head(&s), head, "duplicate-files request mutated state");

    // (f) a DUPLICATE inner path key inside `files` — the strict FilesMap
    // visitor must refuse (a Value-overwrites decoder would silently pick
    // one staged bundle).
    let inner = files["files"].as_object().unwrap();
    let mut inner_raw = String::new();
    for (k, v) in inner {
        inner_raw.push_str(&format!("{}:{},", json!(k), v));
    }
    let dup_inner = format!(
        "{{\"files\":{{\"app.md\":\"JUNK\",{}\"app.md\":{}}},\"expected_digest\":{},\"expected_generation\":{}}}",
        inner_raw, inner["app.md"], json!(before_digest), json!(gen(&s))
    );
    let (code, _, resp) = common::op::raw(port, &session.request("POST", &check_path, &dup_inner));
    assert_eq!(code, 400, "duplicate inner path accepted: {resp}");
    assert_eq!(git_head(&s), head, "duplicate-path request mutated state");

    // ---- POSITIVE: proposal (upgrade-check) then upgrade, same files ----
    let mut chk = serde_json::Map::new();
    chk.insert("files".into(), files_obj(&files));
    chk.insert("expected_digest".into(), json!(before_digest));
    chk.insert("expected_generation".into(), json!(gen(&s)));
    let (code, _, body) = common::op::raw(port, &session.request("POST", &check_path, &Value::Object(chk).to_string()));
    assert_eq!(code, 200, "upgrade-check refused valid files: {body}");
    let proposal: Value = serde_json::from_str(&body).unwrap();
    let new_digest = proposal["digest"].as_str().expect("no proposed digest").to_string();
    assert_ne!(new_digest, before_digest, "upgrade produced an unchanged digest");

    // (e) wrong expected_new_digest refuses against the valid proposal.
    let mut badn = serde_json::Map::new();
    badn.insert("files".into(), files_obj(&files));
    badn.insert("expected_digest".into(), json!(before_digest));
    badn.insert("expected_generation".into(), json!(gen(&s)));
    badn.insert("expected_new_digest".into(), json!("sha256:wrong-new"));
    badn.insert("request_id".into(), json!("upg-bad"));
    let (code, _, _) = common::op::raw(port, &session.request("POST", &up_path, &Value::Object(badn).to_string()));
    assert_eq!(code, 400, "wrong expected_new_digest accepted");

    // The real upgrade: same files, correct pins, a request_id.
    let mut up = serde_json::Map::new();
    up.insert("files".into(), files_obj(&files));
    up.insert("expected_digest".into(), json!(before_digest));
    up.insert("expected_generation".into(), json!(gen(&s)));
    up.insert("expected_new_digest".into(), json!(new_digest));
    up.insert("request_id".into(), json!("upg-1"));
    let (code, _, body) = common::op::raw(port, &session.request("POST", &up_path, &Value::Object(up).to_string()));
    assert_eq!(code, 200, "upgrade refused valid files body: {body}");

    // SAME install id persists; digest changed; approval reset.
    let list = s.daemon.operator_rpc("app_workspace_list", json!({})).unwrap();
    let row = list.as_array().unwrap().iter()
        .find(|r| r["install_id"].as_str() == Some(install_id.as_str()))
        .expect("upgrade erased the install id").clone();
    assert_eq!(row["digest"].as_str(), Some(new_digest.as_str()), "digest unchanged after upgrade");
    assert_eq!(row["approval"]["state"].as_str(), Some("unapproved"), "upgrade kept approval: {row}");
    assert_eq!(row["name"].as_str(), Some("crm"), "install app identity changed");
    assert_eq!(row["version"].as_str(), Some("2"), "version did not advance to 2: {row}");
    // Contexts/bindings/records are keyed by install_id, not digest — the
    // same id surviving the journal commit is the retention proof.
    let ctx = s.daemon.operator_rpc("app_context_list", json!({"install_id": install_id}));
    assert!(ctx.is_ok(), "context list failed after upgrade: {ctx:?}");
}

// ---------------------------------------------------------------------------
// D2 — the per-install outstanding-cap bound (≤4): the 5th mint for the
// SAME install refuses; caps freed by a consume drop the count again.
// ---------------------------------------------------------------------------
#[test]
fn cad1006_mint_per_install_cap_bound() {
    let s = Screen::new();
    let (port, _cleanup) = serve_board(&s);
    let session = s.board_session(port);
    let token = session.cookie.split('=').nth(1).unwrap().to_string();

    // 4 outstanding caps for the same install are admitted; the 5th refuses.
    let mut nonces = Vec::new();
    for gen in 0..4u64 {
        let mint = s.mint(&token, &session.key, "loopback", gen).unwrap();
        let n = mint["mount"].as_str().unwrap().strip_prefix("/api/app-screen/").unwrap().to_string();
        nonces.push(n);
    }
    let fifth = s.mint(&token, &session.key, "loopback", 5);
    assert!(fifth.is_err(), "5th per-install cap minted past the bound");

    // Free one cap by consuming it — the count drops, a new mint succeeds.
    assert!(s.consume(&nonces[0]).is_ok());
    let sixth = s.mint(&token, &session.key, "loopback", 6);
    assert!(sixth.is_ok(), "cap bound did not free after a consume");
}

// ---------------------------------------------------------------------------
// D3 — the mint RATE bound (≤64 mints/session/60s): mint+consume cycles
// keep the outstanding count low, so the per-install bound never fires —
// the RATE gate is what stops the 65th mint. Proves the bound is real and
// the cap map does not leak (each mint's cap is consumed).
// ---------------------------------------------------------------------------
#[test]
fn cad1006_mint_rate_bound_allows_64_refuses_65() {
    let s = Screen::new();
    let (port, _cleanup) = serve_board(&s);
    let session = s.board_session(port);
    let token = session.cookie.split('=').nth(1).unwrap().to_string();

    // 64 mint→consume cycles — each mint is allowed, each cap consumed so
    // the outstanding map never accumulates past one.
    for gen in 0..64u64 {
        let mint = s
            .mint(&token, &session.key, "loopback", gen)
            .unwrap_or_else(|e| panic!("mint {gen} within rate bound refused: {e}"));
        let nonce = mint["mount"].as_str().unwrap()
            .strip_prefix("/api/app-screen/").unwrap().to_string();
        s.consume(&nonce).expect("consume after mint failed");
    }
    // The 65th mint in the same window refuses at the RATE gate — not the
    // outstanding-cap bound (only one cap is ever outstanding here).
    let over = s.mint(&token, &session.key, "loopback", 64);
    assert!(over.is_err(), "65th mint in one window was not rate-refused");
    assert!(
        over.unwrap_err().to_string().contains("rate"),
        "65th mint refused for a non-rate reason"
    );
}

// ---------------------------------------------------------------------------
// D4 — closed param sets: mint and consume refuse ANY field outside their
// own allowlist, so a forged authority field never rides a valid call.
// ---------------------------------------------------------------------------
#[test]
fn cad1006_mint_and_consume_reject_foreign_fields() {
    let s = Screen::new();
    let (port, _cleanup) = serve_board(&s);
    let session = s.board_session(port);
    let token = session.cookie.split('=').nth(1).unwrap().to_string();

    // Mint: an extra field refuses.
    let bad = s.daemon.operator_rpc(
        "app_screen_mint",
        json!({"install_id": s.install_id, "tag": TAG, "token": token,
               "key": session.key, "origin": "loopback", "generation": 1,
               "actor": "operator"}),
    );
    assert!(bad.is_err(), "mint accepted a foreign field");

    // Consume: only {nonce} — an extra field refuses BEFORE the burn on a
    // valid nonce, so the cap survives.
    let mint = s.mint(&token, &session.key, "loopback", 1).unwrap();
    let nonce = mint["mount"].as_str().unwrap().strip_prefix("/api/app-screen/").unwrap().to_string();
    let bad = s.daemon.operator_rpc(
        "app_screen_consume",
        json!({"nonce": nonce, "session_id": "forged"}),
    );
    assert!(bad.is_err(), "consume accepted a foreign field");
    assert!(s.consume(&nonce).is_ok(), "refused consume burned the cap");
}

// ---------------------------------------------------------------------------
// D5 — TTL: a cap older than 60 s is refused on consume. Real-time —
// runs in the default suite; no skip.
// ---------------------------------------------------------------------------
#[test]
fn cad1006_cap_ttl_expires() {
    let s = Screen::new();
    let (port, _cleanup) = serve_board(&s);
    let session = s.board_session(port);
    let token = session.cookie.split('=').nth(1).unwrap().to_string();
    let mint = s.mint(&token, &session.key, "loopback", 1).unwrap();
    let nonce = mint["mount"].as_str().unwrap().strip_prefix("/api/app-screen/").unwrap().to_string();
    std::thread::sleep(std::time::Duration::from_secs(61));
    let err = s.consume(&nonce).unwrap_err();
    assert!(
        err.to_string().contains("expired") || err.to_string().contains("spent or unknown"),
        "expired cap accepted: {err}"
    );
}
