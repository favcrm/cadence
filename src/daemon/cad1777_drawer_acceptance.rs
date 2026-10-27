//! CAD-1777 independent ticket-derived acceptance for the host drawer's
//! paired PRIMARY/DRAWER mount admission. Written by the acceptance owner
//! from the approved contract — NOT by the native implementer, who must
//! not edit or weaken this file. Register in daemon.rs under
//! `all(test, feature = "test-seam")`.
//!
//! The chain under test (real socket RPC -> governed native handler):
//! - `app_screen_mint` gains an OPTIONAL `parent_action_token`; without
//!   it the existing PRIMARY mount behavior is unchanged.
//! - A parent token admits exactly one DRAWER child tied to an actually
//!   consumed, live, non-read-only PRIMARY action context of the SAME
//!   verified session, install, screen tag, active context, digest and
//!   approval epoch. Forged, foreign-session, foreign-context,
//!   foreign-tag, stale/superseded, pending, nested (child-as-parent)
//!   and read-only parents are refused.
//! - Consume re-proves the parent and requires the cap's OWN still-pending
//!   action context: consuming a cap whose context was superseded or
//!   explicitly retired refuses, never activates a sibling token and never
//!   disturbs the live child slot or primary. Child consume supersedes
//!   only that parent's prior child; child retire revokes only the child.
//! - Parent replacement/revoke invalidates the live child AND a pending
//!   child cap, while the existing old-primary supersession guard holds.
//!
//! The same mount admission is exercised through the real board POST
//! (`/api/app-installations/<install>/screens/<tag>/mount`): the proven
//! session credential/peer guards, the closed body, the daemon parent
//! proof, and the guarantee that the parent token never enters the child
//! frame bootstrap. Every handle — session, install, consent, context,
//! mount, consumed action context, record store — is minted through the
//! live fixture daemon; the existing test seam supplies synthetic
//! connection identities ONLY inside this disposable state. No fake
//! admission predicate, service dispatcher, permissions or record
//! storage; no token-sharing bypass. Credentials and action tokens are
//! never printed.
//!
//! Disposable state/PM root, own board port from 3110..=3187 or
//! 3190..=3199 (3188/3189 are other lanes' services). Execution requires
//! build-slot admission and the suite lock, and is intentionally not run
//! by the acceptance author.
#![cfg(all(test, feature = "test-seam"))]

use crate::store::app_records::RecordStore;
use crate::test_seam::{scoped, Asserted, Seam};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const SERVICE_RPC: &str = "app_screen_service_call";
const APP: &str = "crm-drawer-acceptance";
const MAIN: &str = "main";
const DETAIL: &str = "detail";
const OPERATIONS: &[&str] = &[
    "records.list",
    "records.show",
    "records.create",
    "records.update",
];

struct Servers {
    stop: Arc<AtomicBool>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

impl Drop for Servers {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        for thread in self.threads.drain(..).rev() {
            assert!(thread.join().is_ok(), "owned acceptance server panicked");
        }
    }
}

/// Deliberately no Debug: synthetic credentials must not enter failure
/// output.
struct Session {
    cookie: String,
    key: String,
    token: String,
}

struct Scope {
    install: String,
    context: String,
}

/// A fixture daemon (`serve_with` on its own temp state) plus the board
/// bound to the same state/PM root, one records install with two screen
/// tags, and a registered agent for the HTTP caller gate. Every handle is
/// minted over the real socket the board relays to.
struct Fixture {
    servers: Servers,
    root: tempfile::TempDir,
    agent: ureq::Agent,
    base: String,
    host: String,
    port: u16,
    seam: String,
}

impl Fixture {
    fn start() -> Self {
        assert!(
            std::env::var_os("CADENCE_SOCKET").is_none(),
            "inherited socket override is not an isolated fixture; do not clear identity/environment to run"
        );
        let root = tempfile::Builder::new()
            .prefix("c1777drawer")
            .tempdir()
            .unwrap();
        let state = root.path().to_path_buf();
        let pm = state.join("pm");
        crate::issue::Pm::init(&pm).unwrap();
        let store = crate::store::Store::open(&crate::rollout::db_file(&state)).unwrap();
        // The test seam never invents a registered agent; the HTTP caller
        // gate resolves this real registry row.
        store
            .register_agent(&crate::store::NewAgent {
                alias: "drawer-agent",
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
        drop(store);
        let mut servers = Servers {
            stop: Arc::new(AtomicBool::new(false)),
            threads: Vec::new(),
        };
        let opts = crate::daemon::ServeOptions {
            provider_env: crate::adapter::ProviderEnv::refusing_providers(),
            test_seam: true,
            stop: Some(servers.stop.clone()),
            report_router: Some(0),
            checkup: Some(0),
            ..Default::default()
        };
        opts.provider_env
            .set("CADENCE_PM_DIR", pm.to_str().unwrap());
        let daemon_state = state.clone();
        servers.threads.push(std::thread::spawn(move || {
            crate::daemon::serve_with(&daemon_state, opts).unwrap();
        }));
        let deadline = Instant::now() + Duration::from_secs(30);
        while !state.join("cadence.sock").exists() || Seam::token_at(&state).is_none() {
            assert!(Instant::now() < deadline, "owned test daemon did not start");
            assert!(
                !servers.threads[0].is_finished(),
                "owned test daemon exited"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
        // Bind proof comes from this board's startup channel. Never adopt a
        // neighbor answering a health request; retry only AddrInUse.
        let deadline = Instant::now() + Duration::from_secs(20);
        // Own lanes only: 3188 (CRM preview) and 3189 (core proof) are
        // another lane's live services.
        let candidates: Vec<u16> = (3110..=3187).chain(3190..=3199).collect();
        let first = (std::process::id() as usize) % candidates.len();
        let mut port = None;
        for offset in 0..candidates.len() {
            assert!(Instant::now() < deadline, "board bind deadline reached");
            let candidate = candidates[(first + offset) % candidates.len()];
            let (ready, rx) = std::sync::mpsc::channel();
            let opts = crate::ui::ServeOpts {
                host: "127.0.0.1".into(),
                port: candidate,
                stop: Some(servers.stop.clone()),
                startup: Some(ready),
                test_seam: true,
                ..Default::default()
            };
            let (state, pm) = (state.clone(), pm.clone());
            let board = std::thread::spawn(move || {
                drop(crate::ui::serve(&state, &pm, &opts));
            });
            match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(Ok(())) => {
                    servers.threads.push(board);
                    port = Some(candidate);
                    break;
                }
                Ok(Err(std::io::ErrorKind::AddrInUse)) => {
                    assert!(board.join().is_ok(), "own failed bind thread panicked");
                }
                other => {
                    servers.threads.push(board);
                    panic!("owned board startup failed: {other:?}");
                }
            }
        }
        let port = port.expect("no free isolated board port in 3110-3187/3190-3199");
        let seam = Seam::token_at(&state).unwrap();
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(10)))
            .build()
            .into();
        Self {
            servers,
            root,
            agent,
            base: format!("http://127.0.0.1:{port}"),
            host: format!("cadence-{port}.localhost:{port}"),
            port,
            seam,
        }
    }

    fn rpc(&self, who: Asserted, method: &str, params: Value) -> crate::Result<Value> {
        scoped(who, || {
            crate::client::rpc_timeout(self.root.path(), method, params, Duration::from_secs(10))
        })
    }

    fn op(&self, method: &str, params: Value) -> Value {
        self.rpc(Asserted::Operator, method, params)
            .unwrap_or_else(|error| panic!("synthetic operator RPC failed: {method}: {error}"))
    }

    fn post(
        &self,
        who: &str,
        session: Option<&Session>,
        path: &str,
        body: &Value,
    ) -> ureq::http::Response<ureq::Body> {
        let mut request = self
            .agent
            .post(format!("{}{path}", self.base))
            .header("Host", &self.host)
            .header("Origin", format!("http://{}", self.host))
            .header("X-Cadence-Board", "1")
            .header(crate::test_seam::AS_HEADER, who)
            .header(crate::test_seam::TOKEN_HEADER, &self.seam)
            .header("Content-Type", "application/json");
        if let Some(session) = session {
            request = request
                .header("Cookie", &session.cookie)
                .header("X-Cadence-Session", &session.key);
        }
        request
            .send(body.to_string())
            .expect("owned HTTP transport failed")
    }

    fn post_json(
        &self,
        who: &str,
        session: Option<&Session>,
        path: &str,
        body: &Value,
    ) -> (u16, Value) {
        let response = self.post(who, session, path, body);
        let status = response.status().as_u16();
        let reply = response.into_body().read_json().unwrap_or(Value::Null);
        (status, reply)
    }

    /// The headerless frame navigation the child iframe performs.
    fn frame(&self, path: &str) -> (u16, String) {
        let mut response = self
            .agent
            .get(format!("{}{path}", self.base))
            .header("Host", &self.host)
            .call()
            .expect("owned frame navigation failed");
        let status = response.status().as_u16();
        let text = response.body_mut().read_to_string().unwrap_or_default();
        (status, text)
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
        let token = opened["token"]
            .as_str()
            .expect("native session token missing")
            .to_owned();
        let key = opened["key"]
            .as_str()
            .expect("native session key missing")
            .to_owned();
        Session {
            cookie: format!("cadence_operator_{}={token}", self.port),
            key,
            token,
        }
    }

    fn context(&self, install: &str, label: &str) -> String {
        self.op(
            "app_context_create",
            json!({"install_id": install, "label": label,
            "input_defaults": {}, "request_id": format!("context-{label}")}),
        )["context"]["id"]
            .as_str()
            .expect("context receipt missing")
            .to_owned()
    }

    fn install(&self) -> Scope {
        let source = self.root.path().join("source");
        std::fs::create_dir_all(source.join("workflows")).unwrap();
        std::fs::write(
            source.join("app.md"),
            format!(
                "---\napp: {APP}\ntitle: Synthetic CRM drawer acceptance\n\
                 version: '0.1.0'\nsummary: Isolated CAD-1777 paired drawer fixture\n\
                 needs:\n  connections: []\n  requires:\n    contracts:\n      \
                 app-screens: [1]\n      screen-records: [1]\n---\n\n\
                 Synthetic only; no provider or effect.\n"
            ),
        )
        .unwrap();
        // Existing local consent requires a supported workflow. This authored
        // inert template is admitted but NEVER dispatched; no provider/effect.
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
        // Two independently valid records screens. A drawer child must use
        // the parent's EXACT tag; the second tag proves the same-tag proof
        // rather than a missing-package accident.
        for tag in [MAIN, DETAIL] {
            let dir = source.join("screens").join(tag);
            std::fs::create_dir_all(&dir).unwrap();
            let js =
                format!("(()=>{{document.getElementById('root').textContent='drawer-{tag}';}})();");
            std::fs::write(dir.join("client.js"), &js).unwrap();
            let declaration = json!({
                "contract": "app-screens/v2",
                "app": APP,
                "entry": "client.js",
                "host_contract": "screen-records.v1",
                "may": ["records.call"],
                "operations": OPERATIONS,
                "tools": {},
                "assets": [{
                    "name": "client.js",
                    "media_type": "text/javascript",
                    "size": js.len(),
                    "sha256": hash(js.as_bytes()),
                }],
                "provenance": {
                    "source_digest": hash(tag.as_bytes()),
                    "sdk_digest": hash(b"synthetic-sdk"),
                    "toolchain_digest": hash(b"synthetic-toolchain"),
                }
            });
            std::fs::write(dir.join("screens.json"), declaration.to_string()).unwrap();
        }
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
        let context = self.context(&install, "drawer");
        Scope { install, context }
    }
}

/// One mount request's scope: the verified session, installation, active
/// context, screen tag, host generation and optional drawer pairing. It is
/// grouped (rather than passed as seven positional arguments) so the
/// request builder stays readable; every call site still names the scope
/// fields it varies.
struct MountScope<'a> {
    session: &'a Session,
    install: &'a str,
    context: Option<&'a str>,
    tag: &'a str,
    generation: u64,
    parent: Option<&'a str>,
    read_only: bool,
}

impl<'a> MountScope<'a> {
    /// A normal PRIMARY mount of the active context.
    fn primary(session: &'a Session, scope: &'a Scope, tag: &'a str, generation: u64) -> Self {
        Self {
            session,
            install: &scope.install,
            context: Some(&scope.context),
            tag,
            generation,
            parent: None,
            read_only: false,
        }
    }

    /// A DRAWER child mount paired with `parent`.
    fn child(
        session: &'a Session,
        scope: &'a Scope,
        tag: &'a str,
        generation: u64,
        parent: &'a str,
    ) -> Self {
        Self {
            parent: Some(parent),
            ..Self::primary(session, scope, tag, generation)
        }
    }
}

fn mint_params(mount: MountScope<'_>) -> Value {
    let MountScope {
        session,
        install,
        context,
        tag,
        generation,
        parent,
        read_only,
    } = mount;
    let mut params = json!({
        "install_id": install,
        "context_id": context,
        "tag": tag,
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

impl Fixture {
    fn mint(&self, mount: MountScope<'_>) -> Value {
        self.op("app_screen_mint", mint_params(mount))
    }

    fn mount_nonce(receipt: &Value) -> String {
        receipt["mount"]
            .as_str()
            .expect("mint receipt must carry a mount path")
            .rsplit('/')
            .next()
            .expect("mount path must end in its nonce")
            .to_owned()
    }

    fn action(receipt: &Value) -> String {
        receipt["action_token"]
            .as_str()
            .filter(|token| !token.is_empty())
            .expect("a records mount must mint an action context")
            .to_owned()
    }

    fn consume(&self, receipt: &Value) -> crate::Result<Value> {
        self.rpc(
            Asserted::Operator,
            "app_screen_consume",
            json!({"nonce": Self::mount_nonce(receipt)}),
        )
    }

    fn primary(&self, session: &Session, scope: &Scope, generation: u64) -> String {
        let receipt = self.mint(MountScope::primary(session, scope, MAIN, generation));
        let consumed = self
            .consume(&receipt)
            .expect("primary consume must succeed — positive control");
        assert_eq!(consumed["install_id"], scope.install);
        assert_eq!(consumed["generation"], generation);
        Self::action(&receipt)
    }

    fn child(&self, session: &Session, scope: &Scope, parent: &str, generation: u64) -> String {
        let receipt = self.mint(MountScope::child(session, scope, MAIN, generation, parent));
        let consumed = self
            .consume(&receipt)
            .expect("paired child consume must succeed — positive control");
        assert_eq!(consumed["install_id"], scope.install);
        Self::action(&receipt)
    }

    fn service(
        &self,
        session: &Session,
        action: &str,
        operation: &str,
        args: Value,
    ) -> crate::Result<Value> {
        self.rpc(
            Asserted::Operator,
            SERVICE_RPC,
            json!({"action_token": action, "operation": operation, "args": args,
                "token": session.token, "key": session.key, "origin": "loopback"}),
        )
    }

    fn records(&self, scope: &Scope) -> Value {
        RecordStore::open(self.root.path(), &scope.install)
            .unwrap()
            .app_record_list(&scope.context)
            .unwrap()
    }

    fn record_ids(&self, scope: &Scope) -> Vec<String> {
        self.records(scope)["records"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|record| record["id"].as_str().map(str::to_owned))
            .collect()
    }

    fn native_create(&self, scope: &Scope, id: &str, profile: Value) {
        self.op(
            "app_record_create",
            json!({"install_id": scope.install, "context_id": scope.context,
                "record_id": id, "profile": profile}),
        );
    }

    fn usable(&self, session: &Session, action: &str, scope: &Scope, what: &str) {
        let listed = self
            .service(session, action, "records.list", json!({}))
            .unwrap_or_else(|error| panic!("{what}: live mount refused: {error}"));
        assert_eq!(
            listed,
            self.records(scope),
            "{what}: screen read did not reach the real record store"
        );
    }

    fn revoke(&self, session: &Session, action: &str) {
        let reply = self.op(
            "app_tool_revoke",
            json!({"action_token": action, "token": session.token,
                "key": session.key, "origin": "loopback"}),
        );
        assert_eq!(
            reply["revoked"], true,
            "host teardown must revoke the owned action context"
        );
    }
}

fn hash(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn profile(name: &str, email: &str) -> Value {
    json!({"schema": 1, "display_name": name, "email": email, "tags": [],
        "consent": {"email": "unknown"}})
}

/// A child mount refusal must be the daemon's real parent-admission guard:
/// a typed rejection, not a wire-shape refusal and not a blanket deny.
fn drawer_refused(fx: &Fixture, params: Value, what: &str) {
    let error = match fx.rpc(Asserted::Operator, "app_screen_mint", params) {
        Ok(_) => panic!("{what}: daemon admitted a drawer child"),
        Err(error) => error,
    };
    assert!(
        matches!(error.kind(), "rejected" | "gate" | "conflict"),
        "{what}: transport/internal failure is not guard evidence ({})",
        error.kind()
    );
    let text = error.to_string();
    let lower = text.to_ascii_lowercase();
    assert!(
        !lower.contains("unsupported field") && !lower.contains("is not accepted"),
        "{what}: the closed wire shape refused the field, not parent proof: {text}"
    );
    assert!(
        !lower.contains("operator action")
            && !lower.contains("not provably the operator")
            && !lower.contains("live operator session"),
        "{what}: caller authentication refused, not the parent proof: {text}"
    );
    assert!(
        !lower.contains("not approved")
            && !lower.contains("digest changed")
            && !lower.contains("approval epoch"),
        "{what}: fixture installation/approval state refused, not the parent proof: {text}"
    );
}

/// A retired/superseded mount's action must lose authority. The service
/// wrapper may collapse the reason, so this checks the typed guard only;
/// the differential (same call succeeded before retirement) establishes
/// the cause.
fn action_refused(fx: &Fixture, session: &Session, action: &str, what: &str) {
    match fx.service(session, action, "records.list", json!({})) {
        Ok(_) => panic!("{what}: retired/foreign mount still read records"),
        Err(error) => assert!(
            matches!(error.kind(), "rejected" | "gate" | "conflict"),
            "{what}: transport/internal failure is not guard evidence ({})",
            error.kind()
        ),
    }
}

/// A cap whose exact pending action context no longer exists (superseded
/// sibling, or an explicitly retired pending child) must fail consume and
/// must never resurrect authority.
fn consume_refused(fx: &Fixture, receipt: &Value, what: &str) {
    match fx.consume(receipt) {
        Ok(_) => panic!("{what}: cap without its exact pending context consumed"),
        Err(error) => assert!(
            matches!(error.kind(), "rejected" | "gate" | "conflict"),
            "{what}: transport/internal failure is not guard evidence ({})",
            error.kind()
        ),
    }
}

/// ONE behavior check: paired drawer admission, lifecycle and the real
/// HTTP mount guard over isolated disposable state.
#[test]
fn cad1777_drawer_parent_admission_acceptance() {
    // ---- Refusals: forged / foreign / stale / pending / nested / readonly.
    {
        let fx = Fixture::start();
        assert!(!fx.servers.stop.load(Ordering::SeqCst));
        let session = fx.session();
        let scope = fx.install();
        fx.native_create(
            &scope,
            "seed",
            profile("Seed Customer", "seed@example.test"),
        );

        let primary = fx.primary(&session, &scope, 1);
        let child = fx.child(&session, &scope, &primary, 2);
        fx.usable(&session, &primary, &scope, "primary with its live child");
        fx.usable(&session, &child, &scope, "valid paired child");
        let written = fx
            .service(
                &session,
                &child,
                "records.create",
                json!({"record_id": "child-created",
                    "profile": profile("Child Created", "child@example.test")}),
            )
            .expect("the valid child must reach the real records handler");
        assert_eq!(written["record"]["revision"], 1);
        assert!(fx
            .record_ids(&scope)
            .iter()
            .any(|id| id.as_str() == "child-created"));
        fx.usable(&session, &primary, &scope, "primary alongside live child");
        fx.revoke(&session, &child);
        action_refused(&fx, &session, &child, "retired child");
        fx.usable(&session, &primary, &scope, "primary after child retire");

        // Forged parent token.
        let forged_parent = "f".repeat(64);
        drawer_refused(
            &fx,
            mint_params(MountScope::child(
                &session,
                &scope,
                MAIN,
                3,
                forged_parent.as_str(),
            )),
            "forged parent token",
        );

        // An empty parent token is malformed, never "no parent": it must
        // not silently mint a primary.
        drawer_refused(
            &fx,
            mint_params(MountScope::child(&session, &scope, MAIN, 3, "")),
            "empty parent token",
        );

        // Foreign session: B's live primary cannot parent A's child.
        let session_b = fx.session();
        let primary_b = fx.primary(&session_b, &scope, 3);
        drawer_refused(
            &fx,
            mint_params(MountScope::child(&session, &scope, MAIN, 3, &primary_b)),
            "foreign-session parent",
        );
        let child_b = fx.child(&session_b, &scope, &primary_b, 4);
        fx.usable(&session_b, &child_b, &scope, "owner session's own child");
        fx.revoke(&session_b, &child_b);

        // Foreign context: a live parent for another context cannot parent.
        let other_context = fx.context(&scope.install, "other");
        drawer_refused(
            &fx,
            mint_params(MountScope {
                context: Some(other_context.as_str()),
                ..MountScope::child(&session, &scope, MAIN, 3, &primary)
            }),
            "foreign-context parent",
        );

        // Foreign tag: the second screens package is real and valid, but a
        // child must use the parent's exact tag.
        drawer_refused(
            &fx,
            mint_params(MountScope::child(&session, &scope, DETAIL, 3, &primary)),
            "foreign-tag parent",
        );

        // Pending parent: minted and not consumed is not an active parent.
        let pending = fx.mint(MountScope::primary(&session, &scope, MAIN, 4));
        let pending_action = Fixture::action(&pending);
        drawer_refused(
            &fx,
            mint_params(MountScope::child(
                &session,
                &scope,
                MAIN,
                4,
                &pending_action,
            )),
            "pending parent",
        );
        fx.consume(&pending)
            .expect("pending primary consume must succeed — positive control");

        // Stale parent: the new consumed primary superseded gen 1, so the
        // old action is no longer a parent, and the old primary's own
        // action authority stays retired (existing supersession guard).
        drawer_refused(
            &fx,
            mint_params(MountScope::child(&session, &scope, MAIN, 4, &primary)),
            "stale parent",
        );
        action_refused(&fx, &session, &primary, "superseded old primary");

        // Nested: a child is never a parent.
        let current_primary = pending_action;
        let live_child = fx.child(&session, &scope, &current_primary, 5);
        drawer_refused(
            &fx,
            mint_params(MountScope::child(&session, &scope, MAIN, 5, &live_child)),
            "child-as-parent",
        );

        // A child requires the primary's active context.
        drawer_refused(
            &fx,
            mint_params(MountScope {
                context: None,
                ..MountScope::child(&session, &scope, MAIN, 5, &current_primary)
            }),
            "contextless child",
        );

        // Read-only parent: a read-only mount can never host a drawer.
        let read_only = fx.mint(MountScope {
            read_only: true,
            ..MountScope::primary(&session, &scope, MAIN, 6)
        });
        let read_only_action = Fixture::action(&read_only);
        fx.consume(&read_only)
            .expect("read-only primary consume must succeed — positive control");
        drawer_refused(
            &fx,
            mint_params(MountScope::child(
                &session,
                &scope,
                MAIN,
                6,
                &read_only_action,
            )),
            "read-only parent",
        );
        action_refused(
            &fx,
            &session,
            &current_primary,
            "primary replaced by read-only mount",
        );
        action_refused(&fx, &session, &live_child, "child of replaced primary");

        // The refusals above must not leak a cap, context or child slot:
        // a fresh primary and child still admit and work afterwards.
        let recovered_primary = fx.primary(&session, &scope, 7);
        let recovered_child = fx.child(&session, &scope, &recovered_primary, 8);
        fx.usable(
            &session,
            &recovered_primary,
            &scope,
            "primary after refusals",
        );
        fx.usable(&session, &recovered_child, &scope, "child after refusals");
    }

    // ---- Concurrent usability, child retire, one-child supersession.
    {
        let fx = Fixture::start();
        let session = fx.session();
        let scope = fx.install();
        fx.native_create(&scope, "seed", profile("Seed", "seed@example.test"));
        let primary = fx.primary(&session, &scope, 1);
        let child = fx.child(&session, &scope, &primary, 2);
        fx.usable(&session, &primary, &scope, "primary with live child");
        fx.usable(&session, &child, &scope, "live child");
        let child_write = fx
            .service(
                &session,
                &child,
                "records.create",
                json!({"record_id": "from-child",
                    "profile": profile("From Child", "child@example.test")}),
            )
            .expect("child write must reach the real store");
        assert_eq!(child_write["record"]["revision"], 1);
        assert!(fx
            .record_ids(&scope)
            .iter()
            .any(|id| id.as_str() == "from-child"));
        fx.usable(&session, &primary, &scope, "primary sees child write");
        let primary_write = fx
            .service(
                &session,
                &primary,
                "records.create",
                json!({"record_id": "from-primary",
                    "profile": profile("From Primary", "primary@example.test")}),
            )
            .expect("primary write must reach the real store");
        assert_eq!(primary_write["record"]["revision"], 1);
        fx.usable(&session, &child, &scope, "child sees primary write");

        // Child close revokes the child only; the primary stays live.
        fx.revoke(&session, &child);
        action_refused(&fx, &session, &child, "retired child");
        fx.usable(&session, &primary, &scope, "primary after child retire");

        // A new child consume supersedes the prior child of that parent,
        // still leaving the primary live.
        let first_child = fx.child(&session, &scope, &primary, 3);
        let next = fx.mint(MountScope::child(&session, &scope, MAIN, 4, &primary));
        let next_child = Fixture::action(&next);
        fx.consume(&next)
            .expect("second child consume must succeed — one child slot");
        action_refused(&fx, &session, &first_child, "superseded prior child");
        fx.usable(&session, &next_child, &scope, "new live child");
        fx.usable(
            &session,
            &primary,
            &scope,
            "primary after child supersession",
        );
        fx.revoke(&session, &next_child);
        fx.usable(
            &session,
            &primary,
            &scope,
            "primary after second child retire",
        );

        // The child slot is reusable once the prior child is retired.
        let reusable = fx.child(&session, &scope, &primary, 5);
        fx.usable(&session, &reusable, &scope, "child slot after retire");
        fx.revoke(&session, &reusable);
    }

    // ---- Parent replacement/revoke denies the child AND a pending child.
    {
        let fx = Fixture::start();
        let session = fx.session();
        let scope = fx.install();
        fx.native_create(&scope, "seed", profile("Seed", "seed@example.test"));
        let old_primary = fx.primary(&session, &scope, 1);
        let old_child = fx.child(&session, &scope, &old_primary, 2);
        let pending_child = fx.mint(MountScope::child(&session, &scope, MAIN, 3, &old_primary));

        // Parent replacement: a new consumed primary supersedes the old
        // primary and ALL its drawer descendants.
        let new_primary = fx.primary(&session, &scope, 4);
        action_refused(&fx, &session, &old_primary, "old primary supersession");
        action_refused(&fx, &session, &old_child, "child of replaced parent");
        consume_refused(&fx, &pending_child, "pending child of a replaced parent");
        fx.usable(
            &session,
            &new_primary,
            &scope,
            "new primary after replacement",
        );

        // Parent revoke: the host teardown verb retires the primary, its
        // live child and a pending child cap.
        let live_child = fx.child(&session, &scope, &new_primary, 5);
        let pending = fx.mint(MountScope::child(&session, &scope, MAIN, 6, &new_primary));
        fx.revoke(&session, &new_primary);
        action_refused(&fx, &session, &new_primary, "revoked primary");
        action_refused(&fx, &session, &live_child, "child of revoked parent");
        consume_refused(&fx, &pending, "pending child of a revoked parent");
    }

    // ---- Exact consumed mount / one child slot: a cap may only consume
    // its OWN still-pending context, never a sibling's, and consuming one
    // must not activate an unconsumed sibling token.
    {
        let fx = Fixture::start();
        let session = fx.session();
        let scope = fx.install();
        fx.native_create(&scope, "seed", profile("Seed", "seed@example.test"));
        let primary = fx.primary(&session, &scope, 1);

        // Same caller-supplied generation: both children exist, but only
        // the cap actually consumed may become live. The sibling's token
        // must never be activated, and its still-outstanding cap must not
        // resurrect it or disturb the live child/primary.
        let first = fx.mint(MountScope::child(&session, &scope, MAIN, 10, &primary));
        let first_action = Fixture::action(&first);
        let sibling = fx.mint(MountScope::child(&session, &scope, MAIN, 10, &primary));
        let sibling_action = Fixture::action(&sibling);
        assert!(
            first_action != sibling_action,
            "two child mints reused one action context"
        );
        fx.consume(&first)
            .expect("the consumed same-generation child cap");
        fx.usable(
            &session,
            &first_action,
            &scope,
            "consumed same-generation child",
        );
        action_refused(
            &fx,
            &session,
            &sibling_action,
            "unconsumed same-generation sibling token",
        );
        consume_refused(&fx, &sibling, "unconsumed same-generation sibling cap");
        action_refused(
            &fx,
            &session,
            &sibling_action,
            "same-generation sibling after its cap was refused",
        );
        fx.usable(
            &session,
            &first_action,
            &scope,
            "live child after same-generation sibling refusal",
        );
        fx.usable(
            &session,
            &primary,
            &scope,
            "primary after same-generation sibling refusal",
        );

        // Distinct generations: consuming one child retires the other's
        // pending context. The retired sibling cap must refuse and must
        // not take over the live child slot.
        let live = fx.mint(MountScope::child(&session, &scope, MAIN, 11, &primary));
        let live_action = Fixture::action(&live);
        let pending = fx.mint(MountScope::child(&session, &scope, MAIN, 12, &primary));
        let pending_action = Fixture::action(&pending);
        fx.consume(&live)
            .expect("the consumed distinct-generation child cap");
        fx.usable(
            &session,
            &live_action,
            &scope,
            "consumed distinct-generation child",
        );
        action_refused(
            &fx,
            &session,
            &pending_action,
            "unconsumed distinct-generation sibling token",
        );
        consume_refused(&fx, &pending, "unconsumed distinct-generation sibling cap");
        fx.usable(
            &session,
            &live_action,
            &scope,
            "live child after retired sibling cap refusal",
        );
        fx.usable(
            &session,
            &primary,
            &scope,
            "primary after retired sibling cap refusal",
        );

        // An explicitly retired PENDING child is not resurrected by its
        // still-outstanding cap, and the refusal leaves the current child
        // and primary untouched.
        let revoked = fx.mint(MountScope::child(&session, &scope, MAIN, 13, &primary));
        let revoked_action = Fixture::action(&revoked);
        fx.revoke(&session, &revoked_action);
        action_refused(
            &fx,
            &session,
            &revoked_action,
            "explicitly revoked pending child token",
        );
        consume_refused(&fx, &revoked, "explicitly revoked pending child cap");
        fx.usable(
            &session,
            &live_action,
            &scope,
            "current child after revoked pending cap refusal",
        );
        fx.usable(
            &session,
            &primary,
            &scope,
            "primary after revoked pending cap refusal",
        );
    }

    // ---- Same-generation PRIMARY caps: consuming one must not make the
    // other unconsumed cap's action token look like a consumed drawer
    // parent (a false consumed alias).
    {
        let fx = Fixture::start();
        let session = fx.session();
        let scope = fx.install();
        let a = fx.mint(MountScope::primary(&session, &scope, MAIN, 20));
        let a_action = Fixture::action(&a);
        let b = fx.mint(MountScope::primary(&session, &scope, MAIN, 20));
        let b_action = Fixture::action(&b);
        assert!(
            a_action != b_action,
            "two primary mints reused one action context"
        );
        fx.consume(&a).expect("primary A consume");
        fx.usable(&session, &a_action, &scope, "consumed primary A");
        drawer_refused(
            &fx,
            mint_params(MountScope::child(&session, &scope, MAIN, 21, &b_action)),
            "unconsumed same-generation primary as drawer parent",
        );
        fx.usable(
            &session,
            &a_action,
            &scope,
            "primary A after refusing same-generation B as parent",
        );

        // B is still an outstanding one-use primary cap: consuming it must
        // make B the exact current primary and clear A's non-own alias
        // (eligibility is exact, never a historical ever-consumed flag).
        fx.consume(&b).expect("primary B consume");
        drawer_refused(
            &fx,
            mint_params(MountScope::child(&session, &scope, MAIN, 22, &a_action)),
            "superseded primary A as drawer parent",
        );
        let b_child = fx.child(&session, &scope, &b_action, 23);
        fx.usable(&session, &b_action, &scope, "primary B after B consume");
        fx.usable(&session, &b_child, &scope, "child of current primary B");
    }

    // ---- Real board HTTP mount guard and child-frame token isolation.
    {
        let fx = Fixture::start();
        let session = fx.session();
        let scope = fx.install();
        let mount_path = format!(
            "/api/app-installations/{}/screens/{MAIN}/mount",
            scope.install
        );

        // Primary body without a parent stays the existing behavior.
        let (status, primary_receipt) = fx.post_json(
            "operator",
            Some(&session),
            &mount_path,
            &json!({"context_id": scope.context}),
        );
        assert_eq!(status, 200, "operator primary mount over HTTP");
        assert!(primary_receipt["mount"]
            .as_str()
            .is_some_and(|path| path.starts_with("/api/app-screen/")));
        let primary = Fixture::action(&primary_receipt);
        // A second PRIMARY cap with the SAME generation, never consumed.
        // HTTP chooses the generation, so this peer reuses it for B through
        // the native mint (same install/tag/context); the HTTP child POST
        // below then proves B is not a consumed parent.
        let primary_generation = primary_receipt["generation"]
            .as_u64()
            .expect("HTTP primary receipt generation");
        let same_generation_primary = fx.mint(MountScope::primary(
            &session,
            &scope,
            MAIN,
            primary_generation,
        ));
        fx.consume(&primary_receipt)
            .expect("HTTP-minted primary consume must succeed");

        // Valid paired child over HTTP; the receipt must not echo the
        // parent token and the frame must not carry it either.
        let (status, child_receipt) = fx.post_json(
            "operator",
            Some(&session),
            &mount_path,
            &json!({"context_id": scope.context, "parent_action_token": primary}),
        );
        assert_eq!(status, 200, "valid paired child mount over HTTP");
        let child = Fixture::action(&child_receipt);
        assert!(
            !child_receipt.to_string().contains(primary.as_str()),
            "child mount receipt must not echo the parent token"
        );
        let child_mount = child_receipt["mount"]
            .as_str()
            .expect("child receipt mount path missing")
            .to_owned();
        let (status, html) = fx.frame(&child_mount);
        assert_eq!(status, 200, "consumed child frame navigation");
        assert!(!html.is_empty(), "child frame rendered empty");
        assert!(
            !html.contains(primary.as_str()),
            "parent token leaked into the child frame bootstrap"
        );
        assert!(
            !html.contains(child.as_str()),
            "child action token leaked into the child frame bootstrap"
        );
        assert!(
            !html.contains(session.token.as_str()),
            "session token leaked into the child frame bootstrap"
        );
        assert!(
            !html.contains(session.key.as_str()),
            "session key leaked into the child frame bootstrap"
        );
        fx.usable(
            &session,
            &child,
            &scope,
            "child mounted and consumed over HTTP",
        );
        fx.usable(&session, &primary, &scope, "primary after HTTP child");

        // Forged parent through the real board: the daemon guard refuses;
        // the closed body must not be what refused it.
        let (status, reply) = fx.post_json(
            "operator",
            Some(&session),
            &mount_path,
            &json!({"context_id": scope.context, "parent_action_token": "f".repeat(64)}),
        );
        assert!(status >= 400, "forged parent over HTTP was accepted");
        let text = reply.to_string().to_ascii_lowercase();
        assert!(
            !text.contains("unsupported field") && !text.contains("not accepted"),
            "forged parent hit the wire-shape guard, not parent proof: {reply}"
        );

        // An empty/malformed parent token must never silently downgrade to
        // a PRIMARY mint (which would mint a new mount and, on consume,
        // supersede the real primary). The RPC refuses a non-string/empty
        // parent; the HTTP boundary must be at least as strict.
        let (status, reply) = fx.post_json(
            "operator",
            Some(&session),
            &mount_path,
            &json!({"context_id": scope.context, "parent_action_token": ""}),
        );
        assert_eq!(status, 400, "empty parent token was not a client refusal");
        assert!(
            reply.get("mount").is_none(),
            "empty parent token minted a mount instead of refusing"
        );
        // The malformed attempt must not disturb the real paired mounts.
        fx.usable(
            &session,
            &primary,
            &scope,
            "primary after malformed parent attempt",
        );
        fx.usable(
            &session,
            &child,
            &scope,
            "child after malformed parent attempt",
        );

        // Same-generation PRIMARY alias over the real HTTP parent guard:
        // B was minted but never consumed, so its token is not a parent.
        let same_generation_action = Fixture::action(&same_generation_primary);
        let (status, reply) = fx.post_json(
            "operator",
            Some(&session),
            &mount_path,
            &json!({"context_id": scope.context,
                "parent_action_token": same_generation_action}),
        );
        assert_eq!(
            status, 400,
            "unconsumed same-generation primary admitted as drawer parent"
        );
        assert!(
            reply.get("mount").is_none(),
            "same-generation alias minted a child mount"
        );
        fx.usable(
            &session,
            &primary,
            &scope,
            "HTTP primary A after refusing same-generation B as parent",
        );

        // The board's ScreenRead caller gate: agent and unproven refused.
        // Deliberately NO operator cookie here: presenting a live operator
        // session from an agent is the separate stolen-session case that
        // revokes the session by design; reusing it would turn the later
        // operator body-shape probe into a 403 auth artifact instead of
        // the closed-body guard.
        for who in ["agent:drawer-agent", "unproven"] {
            let (status, _) = fx.post_json(
                who,
                None,
                &mount_path,
                &json!({"context_id": scope.context, "parent_action_token": primary}),
            );
            assert_eq!(status, 403, "{who} passed the HTTP screen admission gate");
        }

        // Closed body: the new field does not open the request shape.
        let (status, reply) = fx.post_json(
            "operator",
            Some(&session),
            &mount_path,
            &json!({"context_id": scope.context, "parent_action_token": primary,
                "creator": "forged"}),
        );
        assert_eq!(status, 400, "unknown mount body field accepted");
        assert!(reply.to_string().contains("unsupported fields"));

        // A child must name the primary's active context.
        let (status, _) = fx.post_json(
            "operator",
            Some(&session),
            &mount_path,
            &json!({"parent_action_token": primary}),
        );
        assert!(status >= 400, "contextless HTTP child was accepted");

        // Later step over the real HTTP guard: consuming B makes it the
        // exact current primary and clears A's non-own alias; A is not a
        // parent and A's legacy action is superseded, while B and a child
        // of B work.
        fx.consume(&same_generation_primary)
            .expect("same-generation primary B consume");
        let (status, reply) = fx.post_json(
            "operator",
            Some(&session),
            &mount_path,
            &json!({"context_id": scope.context, "parent_action_token": primary}),
        );
        assert_eq!(
            status, 400,
            "superseded primary A admitted as drawer parent over HTTP"
        );
        assert!(reply.get("mount").is_none());
        let (status, b_child_receipt) = fx.post_json(
            "operator",
            Some(&session),
            &mount_path,
            &json!({"context_id": scope.context,
                "parent_action_token": same_generation_action}),
        );
        assert_eq!(
            status, 200,
            "current primary B must parent a child over HTTP"
        );
        fx.consume(&b_child_receipt).expect("child of B consume");
        let b_child = Fixture::action(&b_child_receipt);
        fx.usable(
            &session,
            &same_generation_action,
            &scope,
            "primary B action after B consume",
        );
        fx.usable(
            &session,
            &b_child,
            &scope,
            "child of current primary B over HTTP",
        );
    }

    eprintln!(
        "CAD-1777 drawer acceptance: named_checks=1 refused=forged,empty-parent,foreign-session,\
         foreign-context,foreign-tag,pending,stale,nested,contextless,read-only rpc=1 http=1 \
         child_retire_preserves_primary=1 parent_replacement_pending_child=1 \
         parent_revoke_pending_child=1 old_primary_supersession=1 \
         exact_pending_context=1 same_generation_sibling=1 \
         distinct_generation_sibling_cap=1 revoked_pending_child=1 \
         same_generation_primary_alias=1 http_same_generation_primary_alias=1 \
         frame_token_isolation=1"
    );
}
