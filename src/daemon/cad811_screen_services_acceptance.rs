//! CAD-811 / CRM-1 independent ticket-derived refusal acceptance.
//! Written before the screen-records.v1 service guard. Implementers must not
//! edit/weaken this file. Register in daemon.rs under all(test, test-seam).
//!
//! Real socket RPC -> rpc_app_screen_service_call -> governed native handler;
//! real board POST -> credential/peer relay -> the same daemon guard. No fake
//! service dispatcher, permissions or record storage. The existing test-seam
//! supplies synthetic connection identities ONLY inside this disposable state;
//! sessions, install consent, consumed mounts and record CAS remain real.
//! Execution requires separate build admission and isolated runtime ownership.
#![cfg(feature = "test-seam")]

use crate::store::app_records::{record_db_path, RecordStore};
use crate::test_seam::{scoped, Asserted, Seam};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const RPC: &str = "app_screen_service_call";
const HTTP: &str = "/api/app-screen-services/call";
const FOREIGN_NAME: &str = "CAD811 FOREIGN PRIVATE SENTINEL";
const FOREIGN_EMAIL: &str = "foreign-private@example.test";
const FOREIGN_ONLY_EMAIL: &str = "foreign-only-private@example.test";
const CAMPAIGN: &str = "approval-sentinel";

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

// Deliberately no Debug: synthetic credentials must not enter failure output.
struct Session {
    cookie: String,
    key: String,
    token: String,
}

struct Scope {
    install: String,
    context: String,
    digest: Value,
}

struct Fixture {
    servers: Servers,
    root: tempfile::TempDir,
    agent: ureq::Agent,
    base: String,
    host: String,
    seam: String,
}

impl Fixture {
    fn start() -> Self {
        assert!(std::env::var_os("CADENCE_SOCKET").is_none(), "inherited socket override is not an isolated fixture; do not clear identity/environment to run");
        let root = tempfile::Builder::new()
            .prefix("c811svc")
            .tempdir()
            .unwrap();
        let state = root.path().to_path_buf();
        let pm = state.join("pm");
        crate::issue::Pm::init(&pm).unwrap();
        let store = crate::store::Store::open(&crate::rollout::db_file(&state)).unwrap();
        store
            .register_agent(&crate::store::NewAgent {
                alias: "synthetic-worker",
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
        let first = (std::process::id() % 90) as u16;
        let mut port = None;
        for offset in 0..90 {
            assert!(Instant::now() < deadline, "board bind deadline reached");
            let candidate = 3110 + (first + offset) % 90;
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
        let port = port.expect("no free isolated board port in 3110-3199");
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

    fn session(&self) -> Session {
        crate::operator_auth::ensure_secret(self.root.path()).unwrap();
        let secret = crate::operator_auth::read_secret(self.root.path()).unwrap();
        let nonce = self.op(
            "operator_link_mint",
            json!({"secret": secret, "origin": "loopback"}),
        )["nonce"]
            .clone();
        let response = self.post("operator", None, "/api/session", &json!({"nonce": nonce}));
        assert_eq!(
            response.status().as_u16(),
            200,
            "real session bootstrap failed"
        );
        let cookie = response.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let token = cookie.split_once('=').unwrap().1.to_owned();
        let body: Value = response.into_body().read_json().unwrap();
        let key = body["session_key"]
            .as_str()
            .expect("real page key missing")
            .to_owned();
        Session { cookie, key, token }
    }

    fn install(&self, label: &str, operations: &[&str]) -> Scope {
        let source = self.root.path().join(format!("source-{label}"));
        let screen = source.join("screens/main");
        std::fs::create_dir_all(&screen).unwrap();
        // Current workspace installer refuses duplicate app names. Use two
        // synthetic app identities with the SAME record schema, not a bypass
        // of that existing guard or a claim of two-CRM lifecycle acceptance.
        let app = format!("crm-acceptance-{label}");
        let manifest = format!("---\napp: {app}\ntitle: Synthetic CRM {label}\nversion: '0.1.0'\nsummary: Isolated CAD811 refusal fixture\nneeds:\n  connections: []\n  requires:\n    contracts:\n      app-screens: [1]\n      screen-records: [1]\n---\n\nSynthetic only.\n");
        std::fs::write(source.join("app.md"), manifest).unwrap();
        // Existing local consent requires a supported workflow. This authored
        // inert template is admitted but NEVER dispatched; no provider/effect.
        std::fs::create_dir_all(source.join("workflows")).unwrap();
        std::fs::write(source.join("workflows/synthetic.md"), "---\ntitle: Synthetic refusal fixture\ngoal: Synthetic text only\ninputs:\n  writer: { ask: Synthetic writer }\n  reviewer: { ask: Synthetic reviewer }\ndistinct: [writer, reviewer]\n---\n\n## Synthetic draft\nagent: {{writer}}\nsize: S\naction: local.text.produce\n\nProduce synthetic text only.\n\n### Acceptance\n- [ ] synthetic text only\n\n## Synthetic review\nagent: {{reviewer}}\nsize: S\ndepends_on: 1\naction: local.text.review\n\nReview the synthetic text only.\n\n### Acceptance\n- [ ] synthetic text only\n").unwrap();
        let js = format!(
            "(()=>{{document.getElementById('root').textContent='synthetic-{label}';}})();"
        );
        std::fs::write(screen.join("client.js"), &js).unwrap();
        let declaration = json!({
            "contract": "app-screens/v2", "app": app, "entry": "client.js",
            "host_contract": "screen-records.v1", "may": ["records.call", "records.slot"],
            "operations": operations, "tools": {},
            "assets": [{"name": "client.js", "media_type": "text/javascript", "size": js.len(), "sha256": hash(js.as_bytes())}],
            "provenance": {"source_digest": hash(label.as_bytes()), "sdk_digest": hash(b"synthetic-sdk"), "toolchain_digest": hash(b"synthetic-toolchain")}
        });
        std::fs::write(screen.join("screens.json"), declaration.to_string()).unwrap();
        let installed = self.op(
            "app_workspace_install",
            json!({"source": source.to_str().unwrap()}),
        );
        let install = installed["install_id"]
            .as_str()
            .expect("install receipt missing")
            .to_owned();
        let digest = installed["digest"].clone();
        // Existing operator install = consent at these exact synthetic bytes.
        // No direct permission-store writes or real audit merge approvals.
        assert_eq!(
            installed["approved"], true,
            "real synthetic install consent must succeed"
        );
        let context = self.context(&install, label);
        Scope {
            install,
            context,
            digest,
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

    fn mount(&self, session: &Session, scope: &Scope, generation: u64) -> String {
        let mint = self.op(
            "app_screen_mint",
            json!({"install_id": scope.install,
            "context_id": scope.context, "tag": "main", "generation": generation,
            "token": session.token, "key": session.key, "origin": "loopback"}),
        );
        let action = mint["action_token"]
            .as_str()
            .expect("tools-empty records mount needs action context")
            .to_owned();
        assert!(!action.is_empty(), "records action context is empty");
        let nonce = mint["mount"].as_str().unwrap().rsplit('/').next().unwrap();
        let consumed = self.op("app_screen_consume", json!({"nonce": nonce}));
        assert_eq!(consumed["install_id"], scope.install);
        assert_eq!(consumed["digest"], scope.digest);
        assert_eq!(consumed["generation"], generation);
        assert_eq!(consumed["contract"], "app-screens/v2");
        action
    }

    fn call(&self, who: Asserted, session: &Session, body: Value) -> crate::Result<Value> {
        let mut params = body;
        params["token"] = json!(session.token);
        params["key"] = json!(session.key);
        params["origin"] = json!("loopback");
        self.rpc(who, RPC, params)
    }

    fn http(&self, who: &str, session: &Session, body: &Value) -> (u16, Value) {
        let response = self.post(who, Some(session), HTTP, body);
        let status = response.status().as_u16();
        let body = response
            .into_body()
            .read_json()
            .expect("service must answer JSON");
        (status, body)
    }

    fn records(&self, install: &str, context: &str) -> Value {
        RecordStore::open(self.root.path(), install)
            .unwrap()
            .app_record_list(context)
            .unwrap()
    }

    fn native_create(&self, scope: &Scope, id: &str, profile: Value) {
        self.op(
            "app_record_create",
            json!({"install_id": scope.install, "context_id": scope.context,
            "record_id": id, "profile": profile}),
        );
    }

    fn content(&self, scope: &Scope) -> Value {
        self.op(
            "app_content_show",
            json!({"install_id": scope.install,
            "context_id": scope.context, "campaign_id": CAMPAIGN}),
        )
    }
}

fn hash(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn profile(name: &str, email: &str) -> Value {
    json!({"schema": 1, "display_name": name, "email": email, "tags": [], "consent": {"email": "unknown"}})
}

fn call(action: &str, operation: &str, args: Value) -> Value {
    json!({"action_token": action, "operation": operation, "args": args})
}

fn record(state: &Path, scope: &Scope, id: &str) -> Value {
    RecordStore::open(state, &scope.install)
        .unwrap()
        .app_record_show(&scope.context, id)
        .unwrap()
}

fn refused_rpc(result: crate::Result<Value>, case: &str) {
    let error = match result {
        Err(error) => error,
        Ok(_) => panic!("{case}: daemon accepted prohibited operation"),
    };
    assert!(
        matches!(error.kind(), "rejected" | "gate" | "conflict"),
        "{case}: transport/internal failures are not guard evidence"
    );
    no_foreign_data(&error.to_string(), case);
}

fn no_foreign_data(text: &str, case: &str) {
    assert!(
        !text.contains(FOREIGN_NAME)
            && !text.contains(FOREIGN_EMAIL)
            && !text.contains(FOREIGN_ONLY_EMAIL),
        "{case}: refused request disclosed foreign profile"
    );
}

/// ONE behavior check, not a restored legacy suite or a mock client.
#[test]
fn cad811_screen_services_real_mount_refusals_preserve_scoped_records() {
    let fx = Fixture::start();
    assert!(!fx.servers.stop.load(Ordering::SeqCst));
    let session = fx.session();
    let other_session = fx.session();
    let operations = [
        "records.list",
        "records.show",
        "records.create",
        "records.update",
        "content.approve",
    ];
    let a = fx.install("a", &operations);
    let mut b_ops = operations.to_vec();
    b_ops.push("segments.list"); // Known-valid operation, deliberately absent in A.
    let b = fx.install("b", &b_ops);
    assert_ne!(a.install, b.install, "fixture requires two installations");
    assert_ne!(
        record_db_path(fx.root.path(), &a.install).unwrap(),
        record_db_path(fx.root.path(), &b.install).unwrap()
    );
    let foreign_context = fx.context(&a.install, "other-a");
    fx.native_create(&a, "same-id", profile("A sentinel", "a@example.test"));
    fx.native_create(&b, "same-id", profile(FOREIGN_NAME, FOREIGN_EMAIL));
    fx.native_create(
        &b,
        "foreign-only",
        profile(FOREIGN_NAME, FOREIGN_ONLY_EMAIL),
    );
    fx.op(
        "app_record_create",
        json!({"install_id": a.install, "context_id": foreign_context,
        "record_id": "other-context-only", "profile": profile(FOREIGN_NAME, FOREIGN_EMAIL)}),
    );
    let saved = fx.op(
        "app_content_save",
        json!({"install_id": a.install, "context_id": a.context,
        "campaign_id": CAMPAIGN, "subject": "Protected synthetic content",
        "blocks": [{"type": "paragraph", "text": "Synthetic body"}]}),
    );
    let campaign_revision = saved["content"]["revision"].as_i64().unwrap();
    assert_ne!(saved["content"]["approval"]["valid"], json!(true));
    let old_action = fx.mount(&session, &a, 1);
    let b_action = fx.mount(&other_session, &b, 1);

    // Real positive controls on BOTH peers: requests must reach the real
    // app_record handler and persist to the actual per-installation DB.
    for (id, via_http) in [("rpc-created", false), ("http-created", true)] {
        let body = call(
            &old_action,
            "records.create",
            json!({"record_id": id,
            "profile": profile(id, &format!("{id}@example.test"))}),
        );
        let created = if via_http {
            let (status, body) = fx.http("operator", &session, &body);
            assert_eq!(status, 200, "HTTP valid create must work before refusals");
            body
        } else {
            fx.call(Asserted::Operator, &session, body)
                .expect("RPC valid create must work before refusals")
        };
        assert_eq!(
            created,
            record(fx.root.path(), &a, id),
            "real handler/storage readback mismatch"
        );
        assert_eq!(created["record"]["revision"], 1);
        assert!(created["record"]["digest"]
            .as_str()
            .is_some_and(|s| s.starts_with("sha256:")));
        let show = call(&old_action, "records.show", json!({"record_id": id}));
        assert_eq!(
            fx.call(Asserted::Operator, &session, show.clone()).unwrap(),
            created
        );
        let (status, shown) = fx.http("operator", &session, &show);
        assert_eq!(status, 200);
        assert_eq!(shown, created);
    }
    assert_eq!(
        fx.records(&a.install, &a.context)["records"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    let foreign = call(
        &b_action,
        "records.show",
        json!({"record_id": "foreign-only"}),
    );
    assert_eq!(
        fx.call(Asserted::Operator, &other_session, foreign)
            .unwrap(),
        record(fx.root.path(), &b, "foreign-only")
    );
    let listed = call(&b_action, "segments.list", json!({}));
    let expected = fx.op(
        "app_segment_list",
        json!({"install_id": b.install, "context_id": b.context}),
    );
    assert_eq!(
        fx.call(Asserted::Operator, &other_session, listed.clone())
            .unwrap(),
        expected
    );
    let (status, listed) = fx.http("operator", &other_session, &listed);
    assert_eq!(status, 200);
    assert_eq!(listed, expected);

    // A successful CAS proves stale refusals do not come from a blanket
    // update denial. The stale request below must retain this winner.
    let update = call(
        &old_action,
        "records.update",
        json!({"record_id": "rpc-created", "expected_revision": 1,
        "profile": profile("CAS winner", "winner@example.test")}),
    );
    let winner = fx.call(Asserted::Operator, &session, update).unwrap();
    assert_eq!(winner["record"]["revision"], 2);
    assert_eq!(winner, record(fx.root.path(), &a, "rpc-created"));
    let action = fx.mount(&session, &a, 2); // Real consume supersedes g1; no sleeps/TTL mutation.
    let show = call(&action, "records.show", json!({"record_id": "rpc-created"}));
    assert_eq!(
        fx.call(Asserted::Operator, &session, show.clone()).unwrap(),
        winner
    );
    let (status, shown) = fx.http("operator", &session, &show);
    assert_eq!(status, 200);
    assert_eq!(shown, winner);

    let snapshot = || {
        json!({"a": fx.records(&a.install, &a.context),
        "other_a": fx.records(&a.install, &foreign_context), "b": fx.records(&b.install, &b.context),
        "content": fx.content(&a)})
    };
    let before = snapshot();
    let before_digest = hash(before.to_string().as_bytes());
    let create = |token: &str, id: &str| {
        call(
            token,
            "records.create",
            json!({"record_id": id,
        "profile": profile("Forbidden write", "forbidden@example.test")}),
        )
    };
    let mut forged = create(&action, "forged-context-write");
    forged["args"]["context_id"] = json!(foreign_context);
    forged["args"]["install_id"] = json!(b.install);
    let cases = [
        (
            "superseded valid token",
            &session,
            Asserted::Operator,
            create(&old_action, "stale-mount-write"),
        ),
        (
            "different live session",
            &other_session,
            Asserted::Operator,
            create(&action, "wrong-session-write"),
        ),
        (
            "unlisted VALID operation",
            &session,
            Asserted::Operator,
            call(&action, "segments.list", json!({})),
        ),
        ("forged scope fields", &session, Asserted::Operator, forged),
        (
            "foreign record selector",
            &session,
            Asserted::Operator,
            call(
                &action,
                "records.show",
                json!({"record_id": "foreign-only"}),
            ),
        ),
        (
            "foreign context selector",
            &session,
            Asserted::Operator,
            call(
                &action,
                "records.show",
                json!({"record_id": "other-context-only"}),
            ),
        ),
        (
            "native agent holding real credentials",
            &session,
            Asserted::Agent("synthetic-worker".into()),
            create(&action, "agent-write"),
        ),
        (
            "stale record CAS",
            &session,
            Asserted::Operator,
            call(
                &action,
                "records.update",
                json!({"record_id": "rpc-created", "expected_revision": 1,
            "profile": profile("CAS loser", "loser@example.test")}),
            ),
        ),
        (
            "declared SLOT cannot become CALL approval",
            &session,
            Asserted::Operator,
            call(
                &action,
                "content.approve",
                json!({"campaign_id": CAMPAIGN, "expected_revision": campaign_revision}),
            ),
        ),
    ];
    let mut rpc_refusals = 0;
    let mut http_refusals = 0;
    for (case, session, who, body) in cases {
        refused_rpc(fx.call(who.clone(), session, body.clone()), case);
        rpc_refusals += 1;
        assert_eq!(
            snapshot(),
            before,
            "{case}: daemon refusal mutated real storage"
        );
        let (status, reply) = fx.http(&who.as_str(), session, &body);
        assert!(
            matches!(status, 400 | 401 | 403 | 404 | 409),
            "{case}: HTTP refusal must be a client refusal, not transport/server failure ({status})"
        );
        no_foreign_data(&reply.to_string(), case);
        assert!(
            reply.get("record").is_none() && reply.get("records").is_none(),
            "{case}: refusal returned record data"
        );
        http_refusals += 1;
        assert_eq!(
            snapshot(),
            before,
            "{case}: HTTP refusal mutated real storage"
        );
    }
    // Real approval revocation, not editing the guard's internal token map.
    fx.op(
        "app_local_install_revoke",
        json!({"install_id": a.install, "digest": a.digest}),
    );
    let body = create(&action, "revoked-digest-write");
    refused_rpc(
        fx.call(Asserted::Operator, &session, body.clone()),
        "revoked live approval",
    );
    rpc_refusals += 1;
    let (status, reply) = fx.http("operator", &session, &body);
    assert!(matches!(status, 400 | 401 | 403 | 404 | 409));
    no_foreign_data(&reply.to_string(), "revoked live approval");
    http_refusals += 1;
    assert_eq!(
        snapshot(),
        before,
        "live approval revocation allowed mutation"
    );
    assert_eq!(hash(snapshot().to_string().as_bytes()), before_digest);
    assert_eq!((rpc_refusals, http_refusals), (10, 10));
    // Prove the protected selector was genuine: native operator approval
    // still succeeds for these exact saved fields; CALL has not approved it.
    let approved = fx.op(
        "app_content_approve",
        json!({"install_id": a.install, "context_id": a.context,
        "campaign_id": CAMPAIGN, "expected_revision": campaign_revision}),
    );
    assert_eq!(approved["content"]["approval"]["valid"], true);
    eprintln!("CAD811 acceptance: named_checks=1 rpc_refusals={rpc_refusals} http_refusals={http_refusals} record_scopes=3 before_digest={before_digest}");
}

/// Independent sender recovery acceptance: real authorized empty read and
/// real bad-caller/scope refusal on both peers. No sender enrollment or send.
#[test]
fn cad811_sender_empty_read_is_typed_and_access_refusals_stay_refused() {
    let fx = Fixture::start();
    let session = fx.session();
    let scope = fx.install("sender-empty", &["smtp.show"]);
    let action = fx.mount(&session, &scope, 1);
    let body = call(&action, "smtp.show", json!({}));
    let native_args = json!({"install_id": scope.install, "context_id": scope.context});
    let empty_link = || {
        crate::store::Store::open(&crate::rollout::db_file(fx.root.path()))
            .unwrap()
            .crm_smtp_link(&scope.install, &scope.context)
            .unwrap()
            .is_none()
    };
    assert!(empty_link(), "positive control must be genuinely unbound");
    // Direct native read proves this code originates in the real SMTP handler,
    // not in the screen relay synthesizing an empty response for any refusal.
    for result in [
        fx.rpc(Asserted::Operator, "crm_smtp_show", native_args.clone()),
        fx.call(Asserted::Operator, &session, body.clone()),
    ] {
        let error = result.expect_err("unbound show must remain a typed refusal");
        assert_eq!(error.kind(), "rejected");
        assert_eq!(error.code(), Some("smtp_sender_unbound"));
    }
    let (status, reply) = fx.http("operator", &session, &body);
    assert_eq!(status, 400, "unbound is not success or an access failure");
    assert_eq!(reply["code"], "smtp_sender_unbound");
    assert!(reply.get("binding").is_none() && reply.get("receipt").is_none());

    let mut forged = body.clone();
    forged["args"]["context_id"] = json!("wrong-context");
    for (who, rejected_body) in [
        (Asserted::Agent("synthetic-worker".into()), body),
        (Asserted::Operator, forged),
    ] {
        let error = fx
            .call(who.clone(), &session, rejected_body.clone())
            .expect_err("sender access/context refusal must not become empty");
        assert!(matches!(error.kind(), "rejected" | "gate"));
        assert!(!matches!(
            error.code(),
            Some("smtp_sender_unbound" | "smtp_sender_revoked")
        ));
        let (status, reply) = fx.http(&who.as_str(), &session, &rejected_body);
        assert!(matches!(status, 400 | 401 | 403));
        assert_ne!(reply["code"], "smtp_sender_unbound");
        assert_ne!(reply["code"], "smtp_sender_revoked");
        assert!(reply.get("binding").is_none() && reply.get("receipt").is_none());
    }
    let error = fx
        .rpc(
            Asserted::Operator,
            "crm_smtp_show",
            json!({
                "install_id": scope.install, "context_id": "wrong-context"
            }),
        )
        .expect_err("native unknown context must refuse before empty read");
    assert_eq!(error.kind(), "rejected");
    assert_ne!(error.code(), Some("smtp_sender_unbound"));
    assert_ne!(error.code(), Some("smtp_sender_revoked"));
    assert!(
        empty_link(),
        "read/refusal paths must not create sender state"
    );
    eprintln!("CRM sender acceptance: genuine_unbound=1 native_rpc=1 screen_rpc=1 http_400_code=1 bad_caller=1 wrong_context=1 smtp_effects_requested=0");
}

#[path = "cad811_public_owner_attribution_acceptance.rs"]
mod independent_public_owner_attribution;

#[path = "cad811_works_acceptance.rs"]
mod independent_works;

#[path = "cad1827_campaign_work_acceptance.rs"]
mod campaign_work;
