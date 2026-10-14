//! Independent CAD-1365 creator-capture acceptance, written by the
//! acceptance author from the ticket and
//! `design-plans/designer-generation-provenance-v1.md` — NOT by the
//! run-creation implementer, who may not edit or weaken it.
//!
//! Frozen contract being checked: `app_run_create`/`app_run_start` may
//! receive trusted-relayed native session credentials `token,key,origin`
//! alongside their existing closed fields. If ANY credential field is
//! supplied, a complete valid credential must classify through the
//! existing `tool_session` seam; invalid or partial credentials refuse
//! and NEVER fall back to operator. A credential-less native operator
//! connection freezes `{kind:"operator"}`; a verified named public
//! session freezes `{kind:"user",id:BoardUser.sub}`. Body-selected
//! creator/user/actor/owner/workspace fields are never attribution —
//! the closed field set refuses them. The frozen `creator` lives INSIDE
//! the immutable snapshot BEFORE `snapshot_digest`, so the digest
//! covers it; snapshots are never mutated after creation.
//!
//! Fixture honesty — two real daemons, one per surface:
//!   * `SockFx` — `serve_with` on its own temp state dir: real socket
//!     `client::rpc` and a real `ui::serve` board that relays over the
//!     SAME socket. Operator sessions are minted through the real
//!     `operator_link_mint`/`operator_session_open` RPC pair.
//!   * `LocalFx` — `Shared::new` in-memory: needed for the NAMED
//!     session, because the running daemon holds `Auth` in memory
//!     (`Auth::load` once at start) — a side-loaded session is
//!     invisible to it. `LocalFx` mints the `BoardUser` row through
//!     the SAME daemon's own `Shared::operator_auth().open_public`,
//!     the exact primitive `rpc_board_session_open` calls AFTER its
//!     Ed25519/JWKS verification (that upstream trust boundary is the
//!     separately-tested CAD-526 surface and is not re-proven here).
//!     This is a TRUSTED native session fixture — a real session row
//!     the request's `tool_session`/`check` resolves natively — never
//!     an invented identity, a body-supplied sub, or a new auth API.
//!     The named session cannot be seeded on `SockFx` without a real
//!     board-identity config + signed JWT + JWKS fetch, an ABI this
//!     fixture does not have; the HTTP relay of a NAMED session is the
//!     reported gap, not faked. The HTTP checks below prove the closed
//!     body shape and the operator relay end-to-end on the real board.
//!
//! Baseline honesty: the current `rpc_app_local` field lists admit NO
//! `token`/`key`/`origin` on create/start, and the Store snapshot has
//! no `creator` member — a valid positive run reaching real creation
//! is the honest RED proof today, never a stub-guard PASS.

#![cfg(all(test, feature = "test-seam"))]

use super::cad1365_mount_authority_acceptance::Stop;
use super::*;
use crate::operator_auth::BoardUser;
use crate::test_seam::{scoped, Asserted, Seam};
use serde_json::{json, Value};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The no-capability local workflow both fixtures ship: one agent step,
/// no capability slots, so creation needs an approved install and a
/// registered owner group — never a provider binding or paid quote.
const WORKFLOW: &str = r#"---
title: "Creator note: {{topic}}"
goal: "One short note about the topic"
label: Creator note
inputs:
  topic: { ask: "Topic", example: "Release note" }
  writer: { ask: "Registered writer in the owner PM group" }
---

One short text note.

## Draft the note: {{topic}}
agent: {{writer}}
size: S
action: local.text.produce

Write one short plain-text note about the topic.

### Acceptance
- [ ] one short plain-text note
"#;

const MANIFEST: &str = r#"---
app: creator-fixture
title: Creator fixture
version: '1.0.0'
summary: One no-capability local text workflow for creator-freeze acceptance.
needs:
  connections: []
---

# Creator fixture

A run-creation fixture: one local text workflow, no capabilities.
"#;

fn write_package(dir: &std::path::Path) -> std::path::PathBuf {
    let app = dir.join("app");
    std::fs::create_dir_all(app.join("workflows")).unwrap();
    std::fs::write(app.join("app.md"), MANIFEST).unwrap();
    std::fs::write(app.join("workflows/note.md"), WORKFLOW).unwrap();
    app
}

fn register_team(store: &crate::store::Store, cwd: &str) {
    for (alias, role) in [("lead", "pm"), ("writer", "worker")] {
        store
            .register_agent(&crate::store::NewAgent {
                alias,
                provider: "fake",
                endpoint_kind: "fake",
                role,
                cwd,
                sandbox: "read-only",
                instructions: None,
                params: Some("{\"upstream\":\"lead\"}"),
                team_role: None,
                model_policy: None,
            })
            .unwrap();
        store
            .set_identity(
                alias,
                &crate::adapter::Identity {
                    thread_id: "t".into(),
                    session_id: "s".into(),
                    model: None,
                    effort: None,
                    pid: std::process::id(),
                    endpoint: None,
                    generation: Some("g1".into()),
                    attach: None,
                },
            )
            .unwrap();
    }
}

/// Assert a refusal by ANY of several guard texts — the baseline
/// closed-field refusal ("unsupported fields") and the frozen
/// contract's session/credential refusals are all honest refusals of
/// the same bad call. A wrong-guard refusal (e.g. a workflow or
/// binding error, or silent operator fallback) still fails here.
fn refused_any(result: Result<Value>, needles: &[&str], what: &str) {
    let text = result
        .expect_err(&format!("{what} must be refused"))
        .to_string();
    assert!(
        needles.iter().any(|n| text.contains(n)),
        "{what} refused by the wrong guard — none of {needles:?}: {text}"
    );
}

fn refused(result: Result<Value>, needle: &str, what: &str) {
    let text = result
        .expect_err(&format!("{what} must be refused"))
        .to_string();
    assert!(
        text.contains(needle),
        "{what} refused by the wrong guard: {text}"
    );
}

/// `material_digest` over the persisted snapshot — the frozen digest
/// must cover the creator-bearing snapshot byte for byte.
fn snapshot_digest(snapshot: &Value) -> String {
    crate::store::app_runs::material_digest(snapshot)
}

/// The real socket daemon (`serve_with` on its own temp state) with the
/// no-capability package installed and approved — the same daemon the
/// board below relays to, so no handle is stale or in-memory-only.
struct SockFx {
    _stop: Stop,
    _dir: tempfile::TempDir,
    state: std::path::PathBuf,
    pm: std::path::PathBuf,
    install: String,
}

impl SockFx {
    fn new() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("c1365rs")
            .tempdir()
            .unwrap();
        let (state, pm) = (dir.path().join("s"), dir.path().join("pm"));
        std::fs::create_dir_all(&state).unwrap();
        crate::issue::Pm::init(&pm).unwrap();
        register_team(
            &crate::store::Store::open(&state.join("cadence.sqlite3")).unwrap(),
            state.to_str().unwrap(),
        );
        let app = write_package(dir.path());
        let stop = Stop(Arc::new(AtomicBool::new(false)), Vec::new());
        let opts = ServeOptions {
            test_seam: true,
            stop: Some(stop.0.clone()),
            ..Default::default()
        };
        opts.provider_env
            .set("CADENCE_PM_DIR", pm.to_str().unwrap());
        let daemon_state = state.clone();
        let mut stop = stop;
        stop.1.push(std::thread::spawn(move || {
            crate::daemon::serve_with(&daemon_state, opts).unwrap()
        }));
        let deadline = Instant::now() + Duration::from_secs(30);
        while !state.join("cadence.sock").exists() || Seam::token_at(&state).is_none() {
            assert!(Instant::now() < deadline, "fixture daemon never started");
            std::thread::sleep(Duration::from_millis(20));
        }
        let mut fx = Self {
            state,
            pm,
            install: String::new(),
            _dir: dir,
            _stop: stop,
        };
        let installed = fx
            .operator(
                "app_workspace_install",
                json!({"source": app.to_str().unwrap()}),
            )
            .expect("install");
        fx.operator(
            "app_local_install_approve",
            json!({"install_id": installed["install_id"], "digest": installed["digest"]}),
        )
        .expect("approve");
        fx.install = installed["install_id"].as_str().unwrap().to_string();
        fx
    }

    fn call(&self, who: Asserted, method: &str, params: Value) -> Result<Value> {
        scoped(who, || crate::client::rpc(&self.state, method, params))
    }
    fn operator(&self, method: &str, params: Value) -> Result<Value> {
        self.call(Asserted::Operator, method, params)
    }
    /// A real operator session through the native link mint + open.
    fn session(&self) -> (String, String) {
        crate::operator_auth::ensure_secret(&self.state).unwrap();
        let secret = crate::operator_auth::read_secret(&self.state).unwrap();
        let nonce = self
            .operator(
                "operator_link_mint",
                json!({"secret": secret, "origin": "loopback"}),
            )
            .unwrap()["nonce"]
            .clone();
        let opened = self
            .operator(
                "operator_session_open",
                json!({"nonce": nonce, "origin": "loopback"}),
            )
            .unwrap();
        (
            opened["token"].as_str().unwrap().to_string(),
            opened["key"].as_str().unwrap().to_string(),
        )
    }
}

/// The in-memory daemon for the named-session path: `Shared::new`
/// exposes `operator_auth()` so the `BoardUser` session row lives in
/// the SAME `Auth` the run-create request resolves natively.
struct LocalFx {
    shared: Arc<Shared>,
    _dir: tempfile::TempDir,
    install: String,
}

impl LocalFx {
    fn new() -> Self {
        let dir = tempfile::Builder::new()
            .prefix("c1365rl")
            .tempdir()
            .unwrap();
        let pm = dir.path().join("pm");
        crate::issue::Pm::init(&pm).unwrap();
        let opts = ServeOptions::default();
        opts.provider_env
            .set("CADENCE_PM_DIR", pm.to_str().unwrap());
        crate::board_identity::write_config(
            dir.path(),
            &crate::board_identity::Config {
                host: "cad1365.example.test".to_string(),
                issuer: "https://platform.example.test".to_string(),
                company: "cad1365-test-company".to_string(),
            },
        )
        .unwrap();
        let shared = Shared::new(dir.path(), &opts).unwrap();
        register_team(&shared.store, dir.path().to_str().unwrap());
        let app = write_package(dir.path());
        let fx = Self {
            _dir: dir,
            shared,
            install: String::new(),
        };
        let installed = fx
            .operator(
                "app_workspace_install",
                json!({"source": app.to_str().unwrap()}),
            )
            .expect("install");
        fx.operator(
            "app_local_install_approve",
            json!({"install_id": installed["install_id"], "digest": installed["digest"]}),
        )
        .expect("approve");
        Self {
            install: installed["install_id"].as_str().unwrap().to_string(),
            ..fx
        }
    }

    fn call(&self, who: Asserted, method: &str, params: Value) -> Result<Value> {
        scoped(who, || {
            self.shared.dispatch(method, &params, std::process::id())
        })
    }
    fn operator(&self, method: &str, params: Value) -> Result<Value> {
        self.call(Asserted::Operator, method, params)
    }

    /// A REAL named public session minted through the SAME running
    /// daemon's `Auth::open_public` — the primitive `board_session_open`
    /// calls after JWT verification (that boundary is separately
    /// covered). The row lives in the SAME `Auth` the request's
    /// `tool_session`/`check` resolves, so `Origin::Public` verifies
    /// natively. Trusted native fixture, never a faked or
    /// body-selected identity.
    fn public_session(&self, sub: &str, role: &str) -> String {
        let now = self.shared.operator_now();
        let config = crate::board_identity::read_config(self._dir.path()).unwrap();
        let realm = crate::operator_auth::PublicRealm {
            issuer: config.issuer,
            company: config.company,
            host: config.host,
        };
        self.shared
            .operator_auth()
            .open_public_bound(
                BoardUser {
                    sub: sub.to_string(),
                    email: format!("{sub}@example.test"),
                    name: sub.to_string(),
                    role: role.to_string(),
                    handle: sub.to_string(),
                },
                &format!("jti-{sub}-{}", std::process::id()),
                now + 60,
                "cad1365-acceptance",
                realm,
                now,
            )
            .unwrap()
            .expect("fresh jti opens a session")
            .token
    }
}

/// Positive control: a credential-less native operator `app_run_create`
/// reaches real run creation and freezes `creator` INSIDE the snapshot
/// BEFORE the digest. A verified named public session on `token/
/// origin` freezes `{kind:"user",id:sub}` — the digest covers it.
#[test]
fn cad1365_run_creator_freezes_operator_and_named_subject_before_digest() {
    let fx = SockFx::new();
    let run = fx
        .operator(
            "app_run_create",
            json!({"install_id": fx.install, "workflow": "note",
                "inputs": {"topic": "release", "writer": "writer"},
                "request_id": "run-creator-op-1", "owner_pm": "lead"}),
        )
        .expect("operator create must reach real run creation");
    let shown = fx
        .operator("app_run_show", json!({"run_id": run["id"]}))
        .expect("show");
    assert_eq!(
        shown["snapshot"]["creator"],
        json!({"kind": "operator"}),
        "operator create must freeze creator=operator in the snapshot: {shown}"
    );
    assert_eq!(
        shown["snapshot_digest"].as_str().unwrap(),
        snapshot_digest(&shown["snapshot"]),
        "snapshot_digest must cover the creator-bearing snapshot"
    );

    // A verified NAMED public session freezes {kind:"user",id:sub}.
    // The session row is minted inside the SAME running daemon's Auth
    // (LocalFx), so tool_session resolves it natively.
    let lfx = LocalFx::new();
    let named_token = lfx.public_session("usr-verified-author", "operator");
    let named = lfx
        .operator(
            "app_run_create",
            json!({"install_id": lfx.install, "workflow": "note",
                "inputs": {"topic": "named", "writer": "writer"},
                "request_id": "run-creator-user-1", "owner_pm": "lead",
                "token": named_token, "origin": "public"}),
        )
        .expect("named session create must reach real run creation");
    let shown = lfx
        .operator("app_run_show", json!({"run_id": named["id"]}))
        .expect("show named");
    assert_eq!(
        shown["snapshot"]["creator"],
        json!({"kind": "user", "id": "usr-verified-author"}),
        "named session must freeze creator=user(sub): {shown}"
    );
    assert_eq!(
        shown["snapshot_digest"].as_str().unwrap(),
        snapshot_digest(&shown["snapshot"]),
        "named snapshot digest must cover creator"
    );
}

/// Caller-selected identity is never attribution: creator/owner/user/
/// actor/sub/workspace/session_id fields on the create body refuse at
/// the closed field set; a forged or partial credential refuses and
/// NEVER falls back to operator.
#[test]
fn cad1365_run_creator_refuses_body_identity_and_partial_forged_credentials() {
    let fx = SockFx::new();
    let (token, key) = fx.session();
    let base = |req: &str| {
        json!({"install_id": fx.install, "workflow": "note",
            "inputs": {"topic": "x", "writer": "writer"},
            "request_id": req, "owner_pm": "lead"})
    };
    // `rpc_app_local`'s closed field list runs before
    // `operator_connection`, so EVERY forged field — identity or
    // credential — refuses at the same "unsupported fields" guard.
    // Body-supplied identity is never read far enough to attribute.
    for field in [
        "creator",
        "user",
        "actor",
        "sub",
        "workspace",
        "session_id",
        "by",
        "operator",
        "alias",
        "lane",
        "as",
    ] {
        let mut p = base(&format!("run-forge-{field}"));
        p[field] = json!("forged-identity");
        refused(
            fx.operator("app_run_create", p),
            "unsupported fields",
            &format!("forged body field {field}"),
        );
    }
    // A forged session credential never creates — and never falls back
    // to operator attribution. (Baseline: the closed field list refuses
    // any `token`/`key`/`origin` today — the same RED.)
    // The credential guards: today the closed field list refuses
    // outright; under the frozen contract a classified-but-invalid or
    // partial credential refuses at `tool_session` — never an operator
    // fallback. Either refusal text is honest; a run created anyway
    // would be the bug.
    let session_refusal = ["unsupported fields", "session", "credential", "origin"];
    let mut bad = base("run-forge-token");
    bad["token"] = json!("f".repeat(64));
    bad["key"] = json!("f".repeat(64));
    bad["origin"] = json!("loopback");
    refused_any(
        fx.operator("app_run_create", bad),
        &session_refusal,
        "forged session credential",
    );
    // PARTIAL credentials refuse — token without a classified origin,
    // or a classified origin without a token — never a silent operator
    // drop.
    let mut token_only = base("run-forge-tokonly");
    token_only["token"] = json!("f".repeat(64));
    refused_any(
        fx.operator("app_run_create", token_only),
        &session_refusal,
        "partial credential: token only",
    );
    let mut origin_only = base("run-forge-origonly");
    origin_only["origin"] = json!("loopback");
    refused_any(
        fx.operator("app_run_create", origin_only),
        &session_refusal,
        "partial credential: origin only",
    );
    // An unclassified origin refuses — the relay must name a real one.
    let mut bad_origin = base("run-forge-badorigin");
    bad_origin["token"] = json!(token);
    bad_origin["key"] = json!(key);
    bad_origin["origin"] = json!("forged-origin");
    refused_any(
        fx.operator("app_run_create", bad_origin),
        &session_refusal,
        "unclassified session origin",
    );
    // A credential field PRESENT but JSON-null is still a supplied
    // credential field: null token/key/origin refuses, never silently
    // counts as "no credential" and falls back to operator.
    for field in ["token", "key", "origin"] {
        let mut p = base(&format!("run-null-{field}"));
        p[field] = Value::Null;
        refused_any(
            fx.operator("app_run_create", p),
            &session_refusal,
            &format!("null credential field {field}"),
        );
    }
    let mut all_null = base("run-null-all");
    all_null["token"] = Value::Null;
    all_null["key"] = Value::Null;
    all_null["origin"] = Value::Null;
    refused_any(
        fx.operator("app_run_create", all_null),
        &session_refusal,
        "all credential fields null",
    );
    // A VALID public session token with `key` present-but-null still
    // refuses: a supplied `key` must be a string credential, never
    // treated as absent. The session row is minted in the SAME
    // daemon's Auth through the post-JWT `open_public` primitive — the
    // trusted native fixture, not a JWT-signature claim.
    let lfx = LocalFx::new();
    let null_key_token = lfx.public_session("usr-null-key-author", "operator");
    refused_any(
        lfx.operator(
            "app_run_create",
            json!({"install_id": lfx.install, "workflow": "note",
                "inputs": {"topic": "nullkey", "writer": "writer"},
                "request_id": "run-null-public-key", "owner_pm": "lead",
                "token": null_key_token, "origin": "public",
                "key": Value::Null}),
        ),
        &session_refusal,
        "valid public session with null key",
    );
    // Positive control: a clean operator create still works after the
    // refusals (a blanket deny cannot masquerade as a pass).
    let ok = fx
        .operator(
            "app_run_create",
            json!({"install_id": fx.install, "workflow": "note",
                "inputs": {"topic": "control", "writer": "writer"},
                "request_id": "run-forge-control", "owner_pm": "lead"}),
        )
        .expect("operator create control");
    assert_eq!(ok["state"], "awaiting_approval", "{ok}");
}

/// A non-operator caller is refused before any creator work: agent and
/// unproven callers stop at `operator_connection`; a public member
/// session (named but NOT operator role) is refused the operator-role
/// floor — never an operator fallback.
#[test]
fn cad1365_run_creator_refuses_nonoperator_and_public_member_callers() {
    let fx = SockFx::new();
    let params = |req: &str| {
        json!({"install_id": fx.install, "workflow": "note",
            "inputs": {"topic": "x", "writer": "writer"},
            "request_id": req, "owner_pm": "lead"})
    };
    for who in [Asserted::Agent("worker".into()), Asserted::Unproven] {
        refused(
            fx.call(who.clone(), "app_run_create", params("run-nonop")),
            "operator",
            "non-operator app_run_create",
        );
        // `app_run_start` derives owner from the install's team, so its
        // closed field list omits `owner_pm` — a start body carrying
        // one refuses at the field list before the caller gate. Use a
        // start-shaped body (expected_quotes, no owner_pm) so the
        // refusal is the caller gate, not the field list.
        let mut start = params("run-nonop-start");
        start.as_object_mut().unwrap().remove("owner_pm");
        start["expected_quotes"] = json!({});
        refused(
            fx.call(who, "app_run_start", start),
            "operator",
            "non-operator app_run_start",
        );
    }
    // A public member session on the credential fields refuses the
    // operator-role floor — a named non-operator cannot run-create.
    let lfx = LocalFx::new();
    let member_token = lfx.public_session("usr-member-viewer", "member");
    // A public member session on the credential fields refuses the
    // operator-role floor — a named non-operator cannot run-create,
    // and never falls back to operator attribution. (Today the closed
    // field list refuses `token`/`origin` outright — the same RED.)
    let mut member = json!({"install_id": lfx.install, "workflow": "note",
        "inputs": {"topic": "x", "writer": "writer"},
        "request_id": "run-member-1", "owner_pm": "lead"});
    member["token"] = json!(member_token);
    member["origin"] = json!("public");
    refused_any(
        lfx.operator("app_run_create", member),
        &["unsupported fields", "operator", "session"],
        "public member session create",
    );
    // Positive control: the operator path still creates.
    let ok = lfx
        .operator(
            "app_run_create",
            json!({"install_id": lfx.install, "workflow": "note",
                "inputs": {"topic": "control", "writer": "writer"},
                "request_id": "run-nonop-control", "owner_pm": "lead"}),
        )
        .expect("operator create control");
    assert_eq!(ok["state"], "awaiting_approval", "{ok}");
}

/// The board's real HTTP relay (`POST /api/app-runs` and
/// `/api/app-runs/start`) is at least as strict as the RPC: a body
/// carrying creator/identity or credential fields refuses at the typed
/// closed shape (400) BEFORE any daemon relay — the frame never selects
/// creator and never smuggles a credential the handler did not add.
/// The operator's clean body relays to real run creation on the SAME
/// daemon the socket fixture drives.
#[test]
fn cad1365_run_creator_http_peer_is_at_least_as_strict() {
    let fx = SockFx::new();
    let (token, key) = fx.session();
    // One guard owns the board thread under the daemon's stop flag so a
    // panic still stops and joins BOTH threads before the tempdir drops.
    let mut guard = Stop(fx._stop.0.clone(), Vec::new());
    let seam = Seam::token_at(&fx.state).unwrap();
    let first = (std::process::id() % 80) as u16;
    let startup_deadline = Instant::now() + Duration::from_secs(20);
    let port = {
        let mut offset = 0u16;
        loop {
            assert!(Instant::now() < startup_deadline, "board startup deadline");
            let port = 3110 + (first + offset) % 80;
            let (startup, ready) = std::sync::mpsc::channel();
            let board = crate::ui::ServeOpts {
                host: "127.0.0.1".into(),
                port,
                stop: Some(guard.0.clone()),
                startup: Some(startup),
                test_seam: true,
                ..Default::default()
            };
            let (s, p) = (fx.state.clone(), fx.pm.clone());
            let thread = std::thread::spawn(move || drop(crate::ui::serve(&s, &p, &board)));
            let remaining = startup_deadline.saturating_duration_since(Instant::now());
            match ready.recv_timeout(remaining) {
                Ok(Ok(())) => {
                    guard.1.push(thread);
                    break port;
                }
                Ok(Err(std::io::ErrorKind::AddrInUse)) => {
                    assert!(thread.join().is_ok());
                    offset += 1;
                    assert!(offset < 80, "all fixture board candidates busy");
                }
                other => {
                    guard.1.push(thread);
                    panic!("fixture board startup failed on port {port}: {other:?}");
                }
            }
        }
    };
    let host = format!("cadence-{port}.localhost:{port}");
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .into();
    let call = |who: &str, with_session: bool, path: &str, body: Value| -> (u16, Value) {
        let mut req = agent
            .post(format!("http://127.0.0.1:{port}{path}"))
            .header("Host", &host)
            .header("X-Cadence-Board", "1")
            .header("Origin", format!("http://{host}"))
            .header(crate::test_seam::AS_HEADER, who)
            .header(crate::test_seam::TOKEN_HEADER, &seam)
            .header("Content-Type", "application/json");
        if with_session {
            req = req
                .header("Cookie", format!("cadence_operator_{port}={token}"))
                .header("X-Cadence-Session", &key);
        }
        let mut r = req.send(body.to_string()).unwrap();
        (
            r.status().as_u16(),
            r.body_mut().read_json().unwrap_or(Value::Null),
        )
    };
    let create_body = |field: Option<&str>, req: &str| {
        let mut b = json!({"install_id": fx.install, "workflow": "note",
            "inputs": {"topic": "http", "writer": "writer"},
            "request_id": req, "owner_pm": "lead"});
        if let Some(f) = field {
            b[f] = json!("forged");
        }
        b
    };
    // Body-supplied identity OR credential fields refuse at the closed
    // typed shape — the frame cannot select creator or smuggle a
    // credential the handler did not add itself.
    for field in [
        "creator",
        "user",
        "actor",
        "sub",
        "workspace",
        "token",
        "key",
        "origin",
    ] {
        let (status, reply) = call(
            "operator",
            true,
            "/api/app-runs",
            create_body(Some(field), &format!("run-http-{field}")),
        );
        assert!(
            status >= 400,
            "HTTP body field {field} was admitted on create: {reply}"
        );
        let (status, reply) = call(
            "operator",
            true,
            "/api/app-runs/start",
            json!({"install_id": fx.install, "workflow": "note",
                "inputs": {}, "request_id": "r", "expected_quotes": {},
                field: "forged"}),
        );
        assert!(
            status >= 400,
            "HTTP body field {field} was admitted on start: {reply}"
        );
    }
    // Non-operator board callers stop at the write gate before relay.
    for who in ["agent:worker", "unproven"] {
        let (status, reply) = call(
            who,
            false,
            "/api/app-runs",
            create_body(None, "run-http-nonop"),
        );
        assert_eq!(status, 403, "{who} relayed a run create: {reply}");
    }
    // Positive control: the operator's clean body reaches real run
    // creation through the relay on the SAME daemon.
    let (status, reply) = call(
        "operator",
        true,
        "/api/app-runs",
        create_body(None, "run-http-ok"),
    );
    assert!(
        status < 400,
        "operator HTTP create must reach real creation: {reply}"
    );
    assert_eq!(reply["state"], "awaiting_approval", "{reply}");
    // The relayed run freezes the operator creator in its snapshot.
    let shown = fx
        .operator("app_run_show", json!({"run_id": reply["id"]}))
        .expect("show http run");
    assert_eq!(
        shown["snapshot"]["creator"],
        json!({"kind": "operator"}),
        "HTTP-relayed create must freeze creator=operator: {shown}"
    );
}
