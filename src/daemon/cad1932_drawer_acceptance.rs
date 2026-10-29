//! CAD-1932 independent refusal acceptance for the native drawer pairing
//! of a NON-records screen. Written by the Spec/security reviewer from
//! the ticket — NOT by the implementer, who must not edit or weaken this
//! file. Registered in daemon.rs under `all(test, feature = "test-seam")`.
//!
//! CAD-1932 removed only the "records screens only" check from
//! `app_screen_mint`. Every other pairing proof must still hold for any
//! screen contract. Over a real fixture daemon (real socket RPC, real
//! install consent, real session, real action contexts) this check uses a
//! `screen-actions.v1` tools screen, which is not a records screen:
//! - positive control: a consumed non-records primary admits one paired
//!   drawer child (this is what CAD-1932 newly allows, so the refusals
//!   below run on the widened path, not on the removed records check);
//! - a read-only parent, a parent from another session, a parent of
//!   another installation, and a drawer child named as a parent are each
//!   refused by the daemon's own parent proof, with that proof's reason;
//! - a read-only child mint naming a live parent is refused.
//!
//! The board relay (`/api/app-installations/<id>/screens/<tag>/mount`) is
//! unchanged by CAD-1932 and already exercised by the CAD-1777 acceptance,
//! so no board is started here. Disposable temp state only; credentials
//! and action tokens are never printed.
#![cfg(all(test, feature = "test-seam"))]

use crate::test_seam::{scoped, Asserted, Seam};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const TAG: &str = "main";

struct Daemon {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            assert!(thread.join().is_ok(), "owned acceptance daemon panicked");
        }
    }
}

/// Deliberately no Debug: synthetic credentials must not enter failure
/// output.
struct Session {
    token: String,
    key: String,
}

struct Scope {
    install: String,
    context: String,
}

struct Fixture {
    _daemon: Daemon,
    root: tempfile::TempDir,
}

fn hash(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

impl Fixture {
    fn start() -> Self {
        assert!(
            std::env::var_os("CADENCE_SOCKET").is_none(),
            "inherited socket override is not an isolated fixture"
        );
        let root = tempfile::Builder::new()
            .prefix("c1932drawer")
            .tempdir()
            .unwrap();
        let state = root.path().to_path_buf();
        let pm = state.join("pm");
        crate::issue::Pm::init(&pm).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let opts = crate::daemon::ServeOptions {
            provider_env: crate::adapter::ProviderEnv::refusing_providers(),
            test_seam: true,
            stop: Some(stop.clone()),
            report_router: Some(0),
            checkup: Some(0),
            ..Default::default()
        };
        opts.provider_env
            .set("CADENCE_PM_DIR", pm.to_str().unwrap());
        let daemon_state = state.clone();
        let thread = std::thread::spawn(move || {
            crate::daemon::serve_with(&daemon_state, opts).unwrap();
        });
        let deadline = Instant::now() + Duration::from_secs(30);
        while !state.join("cadence.sock").exists() || Seam::token_at(&state).is_none() {
            assert!(Instant::now() < deadline, "owned test daemon did not start");
            assert!(!thread.is_finished(), "owned test daemon exited");
            std::thread::sleep(Duration::from_millis(25));
        }
        Self {
            _daemon: Daemon {
                stop,
                thread: Some(thread),
            },
            root,
        }
    }

    fn rpc(&self, method: &str, params: Value) -> crate::Result<Value> {
        scoped(Asserted::Operator, || {
            crate::client::rpc_timeout(self.root.path(), method, params, Duration::from_secs(10))
        })
    }

    fn op(&self, method: &str, params: Value) -> Value {
        self.rpc(method, params)
            .unwrap_or_else(|error| panic!("synthetic operator RPC failed: {method}: {error}"))
    }

    fn session(&self) -> Session {
        crate::operator_auth::ensure_secret(self.root.path()).unwrap();
        let secret = crate::operator_auth::read_secret(self.root.path()).unwrap();
        let nonce = self.op(
            "operator_link_mint",
            json!({"secret": secret, "origin": "loopback"}),
        )["nonce"]
            .clone();
        let opened = self.op(
            "operator_session_open",
            json!({"nonce": nonce, "origin": "loopback"}),
        );
        Session {
            token: opened["token"].as_str().expect("session token").to_owned(),
            key: opened["key"].as_str().expect("session key").to_owned(),
        }
    }

    /// One real, consented install of a NON-records tools screen
    /// (`screen-actions.v1`, `tools.invoke`). The tool is declared so the
    /// mint produces an action context; it is never invoked.
    fn install(&self, app: &str) -> Scope {
        let source = self.root.path().join(format!("source-{app}"));
        std::fs::create_dir_all(source.join("workflows")).unwrap();
        std::fs::write(
            source.join("app.md"),
            format!(
                "---\napp: {app}\ntitle: Synthetic universal drawer acceptance\n\
                 version: '0.1.0'\nsummary: Isolated CAD-1932 non-records drawer fixture\n\
                 needs:\n  connections: []\n  capabilities:\n    source: {{ schema: 1, \
                 capability: social.read, version: 1, action: posts, \
                 resource_kind: connection_account, effect: read }}\n  requires:\n    \
                 contracts:\n      app-screens: [1]\n---\n\n\
                 Synthetic only; no provider or effect.\n"
            ),
        )
        .unwrap();
        // Existing local consent requires a supported workflow. This inert
        // template is admitted but NEVER dispatched.
        std::fs::write(
            source.join("workflows/synthetic.md"),
            "---\ntitle: Synthetic refusal fixture\ngoal: Synthetic text only\n\
             inputs:\n  writer: { ask: Synthetic writer }\n  \
             reviewer: { ask: Synthetic reviewer }\n\
             distinct: [writer, reviewer]\n---\n\n\
             ## Synthetic draft\nagent: {{writer}}\nsize: S\naction: local.text.produce\n\n\
             Produce synthetic text only.\n\n\
             ### Acceptance\n- [ ] synthetic text only\n\n\
             ## Synthetic review\nagent: {{reviewer}}\nsize: S\n\
             depends_on: 1\naction: local.text.review\n\n\
             Review the synthetic text only.\n\n\
             ### Acceptance\n- [ ] synthetic text only\n",
        )
        .unwrap();
        let dir = source.join("screens").join(TAG);
        std::fs::create_dir_all(&dir).unwrap();
        let js = format!("(()=>{{document.getElementById('root').textContent='{app}';}})();");
        std::fs::write(dir.join("client.js"), &js).unwrap();
        let declaration = json!({
            "contract": "app-screens/v2",
            "app": app,
            "entry": "client.js",
            "host_contract": "screen-actions.v1",
            "may": ["tools.invoke"],
            "tools": {"social.read": "source"},
            "assets": [{
                "name": "client.js",
                "media_type": "text/javascript",
                "size": js.len(),
                "sha256": hash(js.as_bytes()),
            }],
            "provenance": {
                "source_digest": hash(app.as_bytes()),
                "sdk_digest": hash(b"synthetic-sdk"),
                "toolchain_digest": hash(b"synthetic-toolchain"),
            }
        });
        std::fs::write(dir.join("screens.json"), declaration.to_string()).unwrap();
        let installed = self.op(
            "app_workspace_install",
            json!({"source": source.to_str().unwrap()}),
        );
        let install = installed["install_id"]
            .as_str()
            .expect("install receipt missing")
            .to_owned();
        assert_eq!(
            installed["approved"], true,
            "real synthetic install consent must succeed"
        );
        let context = self.op(
            "app_context_create",
            json!({"install_id": install, "label": "drawer",
                "input_defaults": {}, "request_id": format!("context-{app}")}),
        )["context"]["id"]
            .as_str()
            .expect("context receipt missing")
            .to_owned();
        Scope { install, context }
    }
}

fn mint_params(
    session: &Session,
    scope: &Scope,
    generation: u64,
    parent: Option<&str>,
    read_only: bool,
) -> Value {
    let mut params = json!({
        "install_id": scope.install,
        "context_id": scope.context,
        "tag": TAG,
        "token": session.token,
        "key": session.key,
        "origin": "loopback",
        "generation": generation,
    });
    if let Some(parent) = parent {
        params["parent_action_token"] = json!(parent);
    }
    if read_only {
        params["read_only"] = json!(true);
    }
    params
}

fn nonce(receipt: &Value) -> String {
    receipt["mount"]
        .as_str()
        .expect("mint receipt must carry a mount path")
        .rsplit('/')
        .next()
        .unwrap()
        .to_owned()
}

fn action(receipt: &Value) -> String {
    receipt["action_token"]
        .as_str()
        .filter(|token| !token.is_empty())
        .expect("a tools screen mount must mint an action context")
        .to_owned()
}

/// Mint + consume one mount; returns its action token.
fn mount(fx: &Fixture, params: Value, what: &str) -> String {
    let receipt = fx
        .rpc("app_screen_mint", params)
        .unwrap_or_else(|error| panic!("{what}: mint refused — positive control: {error}"));
    assert!(
        receipt.get("host_contract").is_none(),
        "{what}: the fixture must be a non-records screen"
    );
    fx.rpc("app_screen_consume", json!({"nonce": nonce(&receipt)}))
        .unwrap_or_else(|error| panic!("{what}: consume refused — positive control: {error}"));
    action(&receipt)
}

/// The drawer child mint must be refused by the daemon's real parent
/// proof, with that proof's own reason — never the removed records-only
/// check, a wire-shape refusal, caller authentication or install state.
fn refused(fx: &Fixture, params: Value, reason: &str, what: &str) {
    let error = match fx.rpc("app_screen_mint", params) {
        Ok(_) => panic!("{what}: daemon admitted a drawer child"),
        Err(error) => error,
    };
    assert_eq!(
        error.kind(),
        "rejected",
        "{what}: not a typed guard refusal"
    );
    let text = error.to_string();
    assert!(
        !text.contains("only a records screen"),
        "{what}: refused by the records-only check CAD-1932 removed, not the parent proof: {text}"
    );
    assert!(
        text.contains(reason),
        "{what}: expected the parent proof's reason {reason:?}, got: {text}"
    );
}

/// ONE refusal check: the native drawer parent proof for a non-records
/// screen.
#[test]
fn cad1932_non_records_drawer_parent_refusal_acceptance() {
    let fx = Fixture::start();
    let session = fx.session();
    let social = fx.install("social-drawer-acceptance");
    let other = fx.install("other-drawer-acceptance");

    // Positive control — the widened path: a non-records primary admits
    // one paired drawer child.
    let primary = mount(
        &fx,
        mint_params(&session, &social, 1, None, false),
        "non-records primary",
    );
    let child = mount(
        &fx,
        mint_params(&session, &social, 2, Some(&primary), false),
        "non-records paired drawer child",
    );

    // A drawer child is never a parent (no drawer-opens-drawer natively).
    refused(
        &fx,
        mint_params(&session, &social, 3, Some(&child), false),
        "cannot be a parent",
        "child named as parent",
    );

    // Another session's live primary cannot parent this session's child.
    let session_b = fx.session();
    refused(
        &fx,
        mint_params(&session_b, &social, 3, Some(&primary), false),
        "does not match this mount",
        "foreign-session parent",
    );

    // Another installation's live primary cannot parent a child here.
    let other_primary = mount(
        &fx,
        mint_params(&session, &other, 1, None, false),
        "other install primary",
    );
    refused(
        &fx,
        mint_params(&session, &social, 3, Some(&other_primary), false),
        "does not match this mount",
        "foreign-install parent",
    );

    // A read-only child mint never hosts a drawer, even with a live parent.
    refused(
        &fx,
        mint_params(&session, &other, 2, Some(&other_primary), true),
        "read-only mount cannot host a drawer child",
        "read-only child mint",
    );

    // A read-only primary is never a drawer parent.
    let read_only = mount(
        &fx,
        mint_params(&session, &social, 4, None, true),
        "read-only primary",
    );
    refused(
        &fx,
        mint_params(&session, &social, 5, Some(&read_only), false),
        "read-only mount has no drawer",
        "read-only parent",
    );

    // The refusals leak nothing: a fresh primary + child still admit.
    let recovered = mount(
        &fx,
        mint_params(&session, &social, 6, None, false),
        "primary after refusals",
    );
    mount(
        &fx,
        mint_params(&session, &social, 7, Some(&recovered), false),
        "child after refusals",
    );
}
