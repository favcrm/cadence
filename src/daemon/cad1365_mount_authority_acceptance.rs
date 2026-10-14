//! Independent CAD-1365 refusal acceptance for the Studio media host's
//! mount authority seam, written by the acceptance author from the ticket
//! and `/tmp/designer-studio-plan-01a12a14/workspace-scope-approved.md` —
//! NOT by the host-guard implementer, who may not edit or weaken it.
//!
//! Scope honesty: this is the BASELINE mount-authority regression check
//! over the live `app_screen_mint` / `app_screen_consume` /
//! `app_tool_invoke` chain the admitted screen actions ride TODAY — the
//! chain a future media operation is PROPOSED to reuse, nothing more.
//! No media route, cross-workspace asset/ref custody, creator
//! attribution, publication CAS or original-download ABI exists yet and
//! none is guaranteed by a pass here; those questions need a PM decision
//! (see the ticket's closure note, not this file).
//!
//! The chain under test: `app_screen_mint` (operator_connection +
//! closed params + native session proof + live digest/approval pin),
//! `app_screen_consume` (operator peer guard + burned one-use cap +
//! live minting session + digest re-proof) and `app_tool_invoke`
//! (operator_connection + closed params + `tool_context_proven`:
//! session ownership, liveness, live consumed mount, declared alias).
//! Every handle — session, install, context, mount, consumed cap — is
//! minted through `client::rpc` against the SAME running fixture daemon
//! the board relays to, so no handle is stale or in-memory-only.
//! Refusals are proven BEFORE any binding/custody/provider work; no
//! provider door is needed or faked.
//!
//! What it proves: a forged caller (agent/detached), a forged session
//! or mount credential, forged authority fields (incl. creator/owner/
//! sub — caller-chosen identity is never authority), a stale/superseded
//! mount, a second operator session borrowing another's mount, a
//! replayed burned cap, and the board's HTTP peer at least as strict as
//! the RPC on the tool relay. Positive controls ride the same live
//! daemon so a blanket denial cannot masquerade as a pass.

#![cfg(all(test, feature = "test-seam"))]

use super::ServeOptions;
use crate::test_seam::{scoped, Asserted, Seam};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn copy_dir(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// Stops the fixture daemon and board threads even when an assertion
/// fails: sets the shared stop flag, then joins every owned thread
/// before the tempdir drops.
pub(super) struct Stop(
    pub(super) Arc<AtomicBool>,
    pub(super) Vec<std::thread::JoinHandle<()>>,
);

impl Drop for Stop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
        for thread in self.1.drain(..).rev() {
            let _ = thread.join();
        }
    }
}

/// A fixture daemon (`serve_with` on its own temp state) plus the
/// Social Content/tools-screen install, context and registered `worker`
/// agent — every handle minted over the real socket the board relays to.
pub(super) struct Fx {
    pub(super) _stop: Stop,
    pub(super) _dir: tempfile::TempDir,
    pub(super) state: std::path::PathBuf,
    pub(super) install: String,
    pub(super) context: String,
}

impl Fx {
    pub(super) fn call(
        &self,
        who: Asserted,
        method: &str,
        params: Value,
    ) -> crate::error::Result<Value> {
        scoped(who, || crate::client::rpc(&self.state, method, params))
    }
    pub(super) fn operator(&self, method: &str, params: Value) -> crate::error::Result<Value> {
        self.call(Asserted::Operator, method, params)
    }
    /// A real operator session through the native link mint + open.
    pub(super) fn session(&self) -> (String, String) {
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
    /// Mint + consume a real mount; returns `(cap_nonce, action_token)`.
    pub(super) fn mount(&self, token: &str, key: &str, generation: u64) -> (String, String) {
        let mint = self
            .operator(
                "app_screen_mint",
                json!({"install_id": self.install, "context_id": self.context,
                    "tag": "feed", "token": token, "key": key,
                    "origin": "loopback", "generation": generation}),
            )
            .expect("operator mint must succeed — positive control");
        let nonce = mint["mount"]
            .as_str()
            .unwrap()
            .rsplit('/')
            .next()
            .unwrap()
            .to_string();
        self.operator("app_screen_consume", json!({"nonce": nonce}))
            .expect("operator consume must succeed — positive control");
        (nonce, mint["action_token"].as_str().unwrap().to_string())
    }
    fn invoke(
        &self,
        who: Asserted,
        action: &str,
        token: &str,
        key: &str,
    ) -> crate::error::Result<Value> {
        self.call(
            who,
            "app_tool_invoke",
            json!({"action_token": action, "tool_alias": "instagram.read",
                "input": {"handle": "x"}, "request_id": format!("req-{}", &action[..8]),
                "token": token, "key": key, "origin": "loopback"}),
        )
    }
    pub(super) fn new() -> Self {
        Self::with_declaration(None)
    }
    /// The fixture daemon with the declared screen's `host_contract`,
    /// `may` and `tools` replaced when a media-contract override is
    /// given — every other fixture step (native session, install,
    /// approval, context, real socket) is identical to [`Fx::new`].
    /// A declaration the current validator refuses keeps the override
    /// itself honest: `app_workspace_install` fails on it, the same
    /// RED an unsupported `screen-media.v1` declaration gives today.
    pub(super) fn with_declaration(declaration: Option<(String, Vec<String>)>) -> Self {
        let dir = tempfile::Builder::new().prefix("c1365").tempdir().unwrap();
        let (state, pm) = (dir.path().join("s"), dir.path().join("pm"));
        std::fs::create_dir_all(&state).unwrap();
        crate::issue::Pm::init(&pm).unwrap();
        // A registered agent BEFORE the daemon opens the store, so an
        // asserted agent identity resolves to a real registry row on the
        // live daemon (the seam never invents one). `agent_register` RPC
        // is master-gated; the store is written here, then `serve_with`
        // opens and owns it.
        crate::store::Store::open(&state.join("cadence.sqlite3"))
            .unwrap()
            .register_agent(&crate::store::NewAgent {
                alias: "worker",
                provider: "fake",
                endpoint_kind: "fake",
                role: "worker",
                cwd: state.to_str().unwrap(),
                sandbox: "read-only",
                instructions: None,
                params: None,
                team_role: None,
                model_policy: None,
            })
            .unwrap();
        let mut stop = Stop(Arc::new(AtomicBool::new(false)), Vec::new());
        let opts = ServeOptions {
            test_seam: true,
            stop: Some(stop.0.clone()),
            ..Default::default()
        };
        opts.provider_env
            .set("CADENCE_PM_DIR", pm.to_str().unwrap());
        let daemon_state = state.clone();
        stop.1.push(std::thread::spawn(move || {
            crate::daemon::serve_with(&daemon_state, opts).unwrap()
        }));
        let deadline = Instant::now() + Duration::from_secs(30);
        while !state.join("cadence.sock").exists() || Seam::token_at(&state).is_none() {
            assert!(Instant::now() < deadline, "fixture daemon never started");
            std::thread::sleep(Duration::from_millis(20));
        }
        let fx = Self {
            state,
            install: String::new(),
            context: String::new(),
            _stop: stop,
            _dir: dir,
        };
        // Generic synthetic consumer package: the smallest bundle the
        // install validator accepts — one declared read capability slot
        // (`source`), one workflow file, and the generic tools-screen stub.
        let app = fx._dir.path().join("app");
        let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        std::fs::create_dir_all(app.join("workflows")).unwrap();
        std::fs::write(
            app.join("app.md"),
            "---\napp: social-content\ntitle: Synthetic host consumer\nversion: '0.1.0'\nneeds:\n  connections: []\n  capabilities:\n    source:\n      schema: 1\n      capability: social.read\n      version: 1\n      action: list_posts\n      resource_kind: connection_account\n      effect: read\n---\nSynthetic host-contract fixture; no app behaviour.\n",
        )
        .unwrap();
        std::fs::write(
            app.join("workflows/source.md"),
            "---\ntitle: \"Synthetic source read\"\ngoal: \"Exercise the host capability path\"\n\
             label: Source read\ncapability_slots: [source]\nexecution: host\n---\n\n\
             ## Source read\nsize: S\naction: local.capability.call\n\n\
             Host-only synthetic step; no worker and no provider.\n\n\
             ### Acceptance\n- [ ] The synthetic host path is exercised.\n",
        )
        .unwrap();
        copy_dir(
            &manifest_dir.join("tests/fixtures/apps/ig-tools-fixture/screens"),
            &app.join("screens"),
        );
        let screens = app.join("screens/feed/screens.json");
        let text = std::fs::read_to_string(&screens).unwrap();
        let text = text.replace("ig-tools-fixture", "social-content");
        if let Some((host_contract, may)) = declaration {
            let mut decl: Value = serde_json::from_str(&text).unwrap();
            decl["host_contract"] = json!(host_contract);
            decl["may"] = json!(may);
            std::fs::write(&screens, serde_json::to_string_pretty(&decl).unwrap()).unwrap();
        } else {
            std::fs::write(&screens, text).unwrap();
        }
        let install = fx
            .operator(
                "app_workspace_install",
                json!({"source": app.to_str().unwrap()}),
            )
            .expect("install");
        fx.operator(
            "app_local_install_approve",
            json!({"install_id": install["install_id"], "digest": install["digest"]}),
        )
        .expect("approve");
        let ctx = fx
            .operator(
                "app_context_create",
                json!({"install_id": install["install_id"], "label": "ws",
                    "input_defaults": {}, "request_id": "ctx-1365"}),
            )
            .expect("context");
        let mut fx = fx;
        fx.install = install["install_id"].as_str().unwrap().to_string();
        fx.context = ctx["context"]["id"]
            .as_str()
            .or_else(|| ctx["id"].as_str())
            .unwrap()
            .to_string();
        fx
    }
}

fn refused(result: crate::error::Result<Value>, needle: &str, what: &str) {
    let text = result
        .expect_err(&format!("{what} must be refused"))
        .to_string();
    assert!(text.contains(needle), "{what} refused wrongly: {text}");
}

/// Forged caller and forged authority fields are refused before a cap or
/// action context exists; caller-supplied creator/owner/sub are never
/// authority. Control: the operator's real mount+consume succeeds.
#[test]
fn cad1365_mint_and_consume_refuse_forged_caller_fields_and_replay() {
    let fx = Fx::new();
    let (token, key) = fx.session();
    let mint_params = || {
        json!({"install_id": fx.install, "context_id": fx.context, "tag": "feed",
            "token": token, "key": key, "origin": "loopback", "generation": 1})
    };
    // Agent and unproven callers stop at operator_connection.
    for who in [Asserted::Agent("worker".into()), Asserted::Unproven] {
        refused(
            fx.call(who.clone(), "app_screen_mint", mint_params()),
            "operator",
            "non-operator screen mint",
        );
        refused(
            fx.call(who, "app_screen_consume", json!({"nonce": "a".repeat(64)})),
            "operator",
            "non-operator screen consume",
        );
    }
    // Forged authority/identity fields ride a valid credential nowhere:
    // the closed param set refuses each (workspace creator is server-derived).
    // Identity-shaped fields are refused by the connection-bound gate
    // (`OPERATOR_FIELDS`) before the closed param set; the rest by the
    // mint's ALLOWED list. Both prove caller-chosen identity is never
    // authority. (workspace creator is server-derived.)
    for (field, needle) in [
        ("creator", "unsupported fields"),
        ("owner", "unsupported fields"),
        ("sub", "request field 'sub' is not accepted"),
        ("user", "unsupported fields"),
        ("actor", "request field 'actor' is not accepted"),
        ("as", "request field 'as' is not accepted"),
        ("session_id", "unsupported fields"),
    ] {
        let mut p = mint_params();
        p[field] = json!("forged");
        refused(
            fx.operator("app_screen_mint", p),
            needle,
            &format!("forged mint field {field}"),
        );
    }
    // A forged session credential never mints.
    let mut bad = mint_params();
    bad["token"] = json!("f".repeat(64));
    refused(
        fx.operator("app_screen_mint", bad),
        "live operator session",
        "mint with forged session token",
    );
    // Positive control: real mount + consume.
    let (nonce, _action) = fx.mount(&token, &key, 1);
    // The burned cap cannot be replayed.
    refused(
        fx.operator("app_screen_consume", json!({"nonce": nonce})),
        "spent or unknown",
        "replayed screen capability",
    );
    // A well-formed but unknown nonce is refused the same way.
    refused(
        fx.operator("app_screen_consume", json!({"nonce": "b".repeat(64)})),
        "spent or unknown",
        "forged screen nonce",
    );
}

/// The action context is bound to its mount's verified session: a second
/// operator session cannot borrow it, a superseded mount's context dies,
/// a forged token is refused, and non-operator callers stop at the gate —
/// all before any provider work.
#[test]
fn cad1365_tool_invoke_refuses_foreign_session_stale_mount_and_forgery() {
    let fx = Fx::new();
    let (token_a, key_a) = fx.session();
    let (_nonce, action_a) = fx.mount(&token_a, &key_a, 1);
    // Non-operator callers are refused before the context is even read.
    for who in [Asserted::Agent("worker".into()), Asserted::Unproven] {
        refused(
            fx.invoke(who, &action_a, &token_a, &key_a),
            "operator",
            "non-operator tool invoke",
        );
    }
    // A forged well-formed action token under the real session is refused.
    refused(
        fx.invoke(Asserted::Operator, &"c".repeat(64), &token_a, &key_a),
        "stale or unknown",
        "forged action token",
    );
    // A different live operator session cannot spend mount A's context.
    let (token_b, key_b) = fx.session();
    refused(
        fx.invoke(Asserted::Operator, &action_a, &token_b, &key_b),
        "does not belong to this session",
        "second session borrowing a mount",
    );
    // Supersede mount A by consuming generation 2 for the same session
    // and install: the old action handle must refuse.
    let (_nonce2, _action2) = fx.mount(&token_a, &key_a, 2);
    refused(
        fx.invoke(Asserted::Operator, &action_a, &token_a, &key_a),
        "stale or unknown",
        "superseded mount's action token",
    );
    // Forged closed params on invoke are refused before context proof.
    let mut forged = json!({"action_token": _action2, "tool_alias": "instagram.read",
        "input": {}, "request_id": "req-f", "token": token_a, "key": key_a,
        "origin": "loopback", "creator": "someone-else"});
    refused(
        fx.operator("app_tool_invoke", forged.clone()),
        "unsupported fields",
        "forged creator field on invoke",
    );
    forged.as_object_mut().unwrap().remove("creator");
    forged["sub"] = json!("attacker-sub");
    refused(
        fx.operator("app_tool_invoke", forged),
        "request field 'sub' is not accepted",
        "forged sub field on invoke",
    );
    // An alias the package never declared is refused before binding work.
    let mut undeclared = json!({"action_token": _action2, "tool_alias": "media.video",
        "input": {}, "request_id": "req-u", "token": token_a, "key": key_a,
        "origin": "loopback"});
    refused(
        fx.operator("app_tool_invoke", undeclared.clone()),
        "not declared",
        "undeclared tool alias",
    );
    // Positive control: the live mount's declared alias reaches the
    // binding path (it fails on unsaved handle, proving it got past
    // every authority gate above rather than a blanket deny).
    undeclared["tool_alias"] = json!("instagram.read");
    undeclared["input"] = json!({"handle": "juicysuite_crm"});
    let err = fx
        .operator("app_tool_invoke", undeclared)
        .expect_err("unsaved handle is refused downstream")
        .to_string();
    assert!(
        err.contains("saved in this context") || err.contains("binding is absent"),
        "control reached binding/context check, not authority: {err}"
    );
}

/// The board's real HTTP relay (`POST /api/app-screen-tools/invoke`) is at
/// least as strict as the RPC: an agent-asserted or unproven request is
/// refused at the write gate (403) before any daemon relay, and the
/// operator's session relays a forged action token to a daemon refusal.
/// Every handle below — session, install, context, mount, consumed cap —
/// was minted over the live socket of the SAME running daemon the board
/// relays to, so the positive control exercises real authority, never an
/// in-memory handle a second Shared would not see.
#[test]
fn cad1365_tool_invoke_http_peer_is_at_least_as_strict() {
    let fx = Fx::new();
    // Every handle — session, install, context, mount, consumed cap —
    // was minted over the live socket of the SAME running daemon the
    // board below relays to.
    let (token, key) = fx.session();
    let (_nonce, action) = fx.mount(&token, &key, 1);
    let state = fx.state.clone();
    let pm = state.parent().unwrap().join("pm");
    // One guard owns the board thread under the daemon's stop flag so a
    // panic still stops and joins BOTH threads before the tempdir drops
    // (`fx._stop` keeps the daemon thread).
    let mut guard = Stop(fx._stop.0.clone(), Vec::new());
    let seam = Seam::token_at(&state).unwrap();
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
            let (s, p) = (state.clone(), pm.clone());
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
    let call = |who: &str, body: Value| -> (u16, Value) {
        let mut req = agent
            .post(format!(
                "http://127.0.0.1:{port}/api/app-screen-tools/invoke"
            ))
            .header("Host", &host)
            .header("X-Cadence-Board", "1")
            .header("Origin", format!("http://{host}"))
            .header(crate::test_seam::AS_HEADER, who)
            .header(crate::test_seam::TOKEN_HEADER, &seam)
            .header("Content-Type", "application/json");
        if who == "operator" {
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
    let invoke_body = json!({"action_token": action, "tool_alias": "instagram.read",
        "input": {"handle": "juicysuite_crm"}, "request_id": "req-http"});
    for who in ["agent:worker", "unproven"] {
        let (status, reply) = call(who, invoke_body.clone());
        assert_eq!(status, 403, "{who} relayed a tool invoke: {reply}");
        assert!(
            matches!(
                reply["check"].as_str(),
                Some("operator_only" | "caller_identity")
            ),
            "{who}: {reply}"
        );
    }
    // The operator's real session relays a forged action token; the
    // daemon refuses and the frame sees a non-2xx, never success.
    let (status, reply) = call(
        "operator",
        json!({"action_token": "d".repeat(64), "tool_alias": "instagram.read",
            "input": {}, "request_id": "req-forged"}),
    );
    assert!(status >= 400, "forged action token over HTTP: {reply}");
    // Positive control: the operator's body reaches the daemon's real
    // checks (downstream context refusal, not the 403 gate or blanket deny).
    let (status, reply) = call("operator", invoke_body);
    assert_ne!(status, 403, "operator hit the write gate: {reply}");
    assert!(status >= 400, "control: {reply}");
    let text = reply.to_string();
    assert!(
        text.contains("saved in this context") || text.contains("binding"),
        "control must reach downstream checks, not a gate: {reply}"
    );
}
