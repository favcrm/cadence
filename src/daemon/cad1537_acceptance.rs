//! CAD-1537 independent refusal acceptance. Reviewer-written from the
//! ticket's operator-approved design note (claude-opus-spec-reviewer), not
//! by the implementer. Implementers must not edit or weaken this file.
//!
//! The one refusal check for the member read-only screen rule: a public
//! `member` session (a real `Auth::open_public_bound` row, role `member`)
//! mints through the real `app_screen_mint`, consumes through the real
//! `app_screen_consume`, and calls the real screen-service RPCs over the
//! daemon socket and the real public board over HTTP. No guard, permission,
//! record store or session is faked; the test seam only asserts the local
//! process identity of the RPC/HTTP peer inside this disposable state.
//!
//! Refused: (a) a member write (`records.create`, `segments.save`) through
//! the service call, RPC and HTTP; (b) a member SLOT prepare in the daemon,
//! and slot-prepare / slot-confirm / screen-tools over HTTP; (c) a mint with
//! a forged `read_only` value or an extra authority field, and a member
//! mint that tries to come out write-capable; an agent or detached caller.
//! Positive: the member reads the operator's record (RPC and HTTP).
//! The same allowlist holds a `--read-only` board's mount (an operator
//! session minting with `read_only: true`) over RPC.
#![cfg(feature = "test-seam")]

use crate::daemon::{serve, ServeOptions, Shared};
use crate::operator_auth::{BoardUser, Origin, PublicRealm};
use crate::store::app_records::RecordStore;
use crate::test_seam::{scoped, Asserted, Seam};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::os::unix::net::UnixListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

const COMPANY: &str = "co_c1537";
const SEEDED: &str = "c1537-operator-seeded";
const SEEDED_NAME: &str = "CAD1537 operator seeded customer";
const GUARD: &str = "read-only screen admits only read operations";
/// Declared by the package, so only the read-only rule can refuse them —
/// never "not declared by current package".
const OPERATIONS: &[&str] = &[
    "records.list",
    "records.show",
    "records.create",
    "records.update",
    "records.csv.preview",
    "records.csv.import",
    "records.csv.confirm",
    "segments.list",
    "segments.save",
];
const WRITES: &[&str] = &[
    "records.create",
    "records.update",
    "records.csv.preview",
    "records.csv.import",
    "records.csv.confirm",
    "segments.save",
];

struct Servers {
    stop: Arc<AtomicBool>,
    threads: Vec<std::thread::JoinHandle<()>>,
}

impl Drop for Servers {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        for thread in self.threads.drain(..).rev() {
            let _ = thread.join();
        }
    }
}

// No Debug: synthetic credentials must not enter failure output.
struct Session {
    token: String,
    key: String,
    origin: &'static str,
}

struct Fx {
    // Field order: stop the servers before the temp state is removed.
    _servers: Servers,
    shared: Arc<Shared>,
    root: tempfile::TempDir,
    agent: ureq::Agent,
    base: String,
    host: String,
    seam: String,
    issuer: String,
    install: String,
    context: String,
}

fn hash(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

impl Fx {
    fn start() -> Self {
        assert!(
            std::env::var_os("CADENCE_SOCKET").is_none(),
            "fixture BLOCKED: inherited socket override"
        );
        let root = tempfile::Builder::new().prefix("c1537").tempdir().unwrap();
        let state = root.path().to_path_buf();
        let pm = state.join("pm");
        crate::issue::Pm::init(&pm).unwrap();
        let (port, reservation) = crate::test_ports::reserve_board_port();
        let host = format!("c1537.board.localhost:{port}");
        let issuer = format!("http://127.0.0.1:{port}");
        crate::board_identity::write_config(
            &state,
            &crate::board_identity::Config {
                host: host.clone(),
                issuer: issuer.clone(),
                company: COMPANY.into(),
            },
        )
        .unwrap();
        let opts = ServeOptions {
            provider_env: crate::adapter::ProviderEnv::refusing_providers(),
            test_seam: true,
            ..Default::default()
        };
        opts.provider_env
            .set("CADENCE_PM_DIR", pm.to_str().unwrap());
        let shared = Shared::new(&state, &opts).unwrap();
        shared
            .store
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
        let mut servers = Servers {
            stop: Arc::new(AtomicBool::new(false)),
            threads: Vec::new(),
        };
        let listener = UnixListener::bind(state.join("cadence.sock")).unwrap();
        listener.set_nonblocking(true).unwrap();
        let (stop, dispatch) = (servers.stop.clone(), shared.clone());
        servers.threads.push(std::thread::spawn(move || {
            let mut connections = Vec::new();
            while !stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let dispatch = dispatch.clone();
                        connections.push(std::thread::spawn(move || {
                            serve::handle_conn(dispatch, stream)
                        }));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5))
                    }
                    Err(_) => panic!("owned RPC listener failed"),
                }
            }
            for connection in connections {
                let _ = connection.join();
            }
        }));
        let seam = Seam::token_at(&state).expect("fixture BLOCKED: seam not armed");
        let (ready, receipt) = std::sync::mpsc::channel();
        let board_opts = crate::ui::ServeOpts {
            host: "127.0.0.1".into(),
            port,
            stop: Some(servers.stop.clone()),
            startup: Some(ready),
            test_seam: true,
            allow_hosts: vec![host.clone()],
            allow_origins: vec![format!("http://{host}")],
            public: Some(crate::ui::PublicBoard {
                host: host.clone(),
                authorize_url: format!("{issuer}/unused-authorize"),
                issuer: issuer.clone(),
                company: COMPANY.into(),
            }),
            ..Default::default()
        };
        drop(reservation);
        let (board_state, board_pm) = (state.clone(), pm.clone());
        servers.threads.push(std::thread::spawn(move || {
            drop(crate::ui::serve(&board_state, &board_pm, &board_opts))
        }));
        let bound = receipt
            .recv_timeout(Duration::from_secs(10))
            .expect("fixture BLOCKED: board startup receipt");
        assert!(bound.is_ok(), "fixture BLOCKED: board could not bind");
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(10)))
            .build()
            .into();
        let mut fx = Self {
            _servers: servers,
            shared,
            root,
            agent,
            base: format!("http://127.0.0.1:{port}"),
            host,
            seam,
            issuer,
            install: String::new(),
            context: String::new(),
        };
        fx.install_crm();
        fx
    }

    fn rpc(&self, who: Asserted, method: &str, params: Value) -> crate::Result<Value> {
        scoped(who, || {
            crate::client::rpc_timeout(self.root.path(), method, params, Duration::from_secs(10))
        })
    }

    fn op(&self, method: &str, params: Value) -> Value {
        self.rpc(Asserted::Operator, method, params)
            .unwrap_or_else(|error| panic!("operator RPC {method} failed: {error}"))
    }

    /// The operator's real install (consent at these exact bytes) and one
    /// active context — General setup stays the operator's.
    fn install_crm(&mut self) {
        let source = self.root.path().join("source");
        let screen = source.join("screens/main");
        std::fs::create_dir_all(&screen).unwrap();
        let manifest = "---\napp: crm-c1537\ntitle: Synthetic CRM\nversion: '0.1.0'\nsummary: Isolated CAD1537 refusal fixture\nneeds:\n  connections: []\n  requires:\n    contracts:\n      app-screens: [1]\n      screen-records: [1]\n---\n\nSynthetic only.\n";
        std::fs::write(source.join("app.md"), manifest).unwrap();
        std::fs::create_dir_all(source.join("workflows")).unwrap();
        std::fs::write(source.join("workflows/synthetic.md"), "---\ntitle: Synthetic refusal fixture\ngoal: Synthetic text only\ninputs:\n  writer: { ask: Synthetic writer }\n  reviewer: { ask: Synthetic reviewer }\ndistinct: [writer, reviewer]\n---\n\n## Synthetic draft\nagent: {{writer}}\nsize: S\naction: local.text.produce\n\nProduce synthetic text only.\n\n### Acceptance\n- [ ] synthetic text only\n\n## Synthetic review\nagent: {{reviewer}}\nsize: S\ndepends_on: 1\naction: local.text.review\n\nReview the synthetic text only.\n\n### Acceptance\n- [ ] synthetic text only\n").unwrap();
        let js = "(()=>{document.getElementById('root').textContent='c1537';})();";
        std::fs::write(screen.join("client.js"), js).unwrap();
        let declaration = json!({
            "contract": "app-screens/v2", "app": "crm-c1537", "entry": "client.js",
            "host_contract": "screen-records.v1", "may": ["records.call", "records.slot"],
            "operations": OPERATIONS, "tools": {},
            "assets": [{"name": "client.js", "media_type": "text/javascript", "size": js.len(), "sha256": hash(js.as_bytes())}],
            "provenance": {"source_digest": hash(b"c1537"), "sdk_digest": hash(b"synthetic-sdk"), "toolchain_digest": hash(b"synthetic-toolchain")}
        });
        std::fs::write(screen.join("screens.json"), declaration.to_string()).unwrap();
        let installed = self.op(
            "app_workspace_install",
            json!({"source": source.to_str().unwrap()}),
        );
        assert_eq!(
            installed["approved"], true,
            "fixture BLOCKED: install consent"
        );
        self.install = installed["install_id"].as_str().unwrap().to_owned();
        self.context = self.op(
            "app_context_create",
            json!({"install_id": self.install, "label": "general",
                "input_defaults": {}, "request_id": "c1537-context"}),
        )["context"]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        self.op(
            "app_record_create",
            json!({"install_id": self.install, "context_id": self.context,
                "record_id": SEEDED, "profile": profile(SEEDED_NAME, "seeded@example.test")}),
        );
    }

    /// A real public session row whose platform role is `member`.
    fn member(&self) -> Session {
        let now = self.shared.operator_now();
        let opened = self
            .shared
            .operator_auth()
            .open_public_bound(
                BoardUser {
                    sub: "synthetic-c1537-member".into(),
                    email: "c1537-member@example.test".into(),
                    name: "Synthetic member".into(),
                    role: "member".into(),
                    handle: "c1537-member".into(),
                },
                "c1537-member-jti",
                now + 120,
                "c1537-isolated-acceptance",
                PublicRealm {
                    issuer: self.issuer.clone(),
                    company: COMPANY.into(),
                    host: self.host.clone(),
                },
                now,
            )
            .unwrap()
            .expect("fixture BLOCKED: fresh member jti");
        Session {
            token: opened.token,
            key: String::new(),
            origin: "public",
        }
    }

    /// A real local operator session (positive control for the narrowing).
    fn local_operator(&self) -> Session {
        let now = self.shared.operator_now();
        let mut auth = self.shared.operator_auth();
        let nonce = auth.mint(Origin::Loopback, now).unwrap();
        let opened = auth
            .open(&nonce, Origin::Loopback, "", now)
            .unwrap()
            .expect("fixture BLOCKED: fresh local nonce");
        Session {
            token: opened.token,
            key: opened.key,
            origin: "loopback",
        }
    }

    fn creds(&self, session: &Session, mut body: Value) -> Value {
        body["token"] = json!(session.token);
        body["key"] = json!(session.key);
        body["origin"] = json!(session.origin);
        body
    }

    fn mint(
        &self,
        session: &Session,
        generation: u64,
        extra: Option<(&str, Value)>,
    ) -> crate::Result<Value> {
        let mut body = json!({"install_id": self.install, "context_id": self.context,
            "tag": "main", "generation": generation});
        if let Some((field, value)) = extra {
            body[field] = value;
        }
        self.rpc(
            Asserted::Operator,
            "app_screen_mint",
            self.creds(session, body),
        )
    }

    /// The real frame consume. A mount the viewer cannot consume is a mount
    /// the viewer cannot see.
    fn consume(&self, mint: &Value, who: &str) -> Value {
        let nonce = mint["mount"].as_str().unwrap().rsplit('/').next().unwrap();
        self.rpc(
            Asserted::Operator,
            "app_screen_consume",
            json!({"nonce": nonce}),
        )
        .unwrap_or_else(|error| panic!("{who} mount cannot be consumed: {error}"))
    }

    fn service(
        &self,
        method: &str,
        session: &Session,
        action: &str,
        operation: &str,
        args: Value,
    ) -> crate::Result<Value> {
        self.rpc(
            Asserted::Operator,
            method,
            self.creds(
                session,
                json!({"action_token": action, "operation": operation, "args": args}),
            ),
        )
    }

    fn post(&self, session: &Session, path: &str, body: &Value) -> (u16, Value) {
        let response = self
            .agent
            .post(format!("{}{path}", self.base))
            .header("Host", &self.host)
            .header("Origin", format!("http://{}", self.host))
            .header("X-Cadence-Board", "1")
            .header(crate::test_seam::AS_HEADER, "operator")
            .header(crate::test_seam::TOKEN_HEADER, &self.seam)
            .header("Content-Type", "application/json")
            .header(
                "Cookie",
                format!("__Host-aos-board-session={}", session.token),
            )
            .send(body.to_string())
            .expect("owned HTTP transport failed");
        let status = response.status().as_u16();
        let body = response.into_body().read_json().unwrap_or(Value::Null);
        (status, body)
    }

    /// The durable record set, as the native store reads it.
    fn records(&self) -> Value {
        RecordStore::open(self.root.path(), &self.install)
            .unwrap()
            .app_record_list(&self.context)
            .unwrap()
    }

    fn segment_count(&self) -> usize {
        let listed = RecordStore::open(self.root.path(), &self.install)
            .unwrap()
            .app_segment_list(&self.context)
            .unwrap();
        listed["segments"].as_array().map_or(0, Vec::len)
    }
}

fn profile(name: &str, email: &str) -> Value {
    json!({"schema": 1, "display_name": name, "email": email, "tags": [], "consent": {"email": "unknown"}})
}

/// A refusal is guard evidence only when it is the daemon's own rejection
/// carrying the named reason — never a transport or internal failure.
fn refused_by(result: crate::Result<Value>, reason: &str, case: &str) {
    match result {
        Ok(_) => panic!("{case}: daemon accepted a prohibited request"),
        Err(error) => {
            assert!(
                matches!(error.kind(), "rejected" | "gate"),
                "{case}: not a guard refusal (kind {})",
                error.kind()
            );
            // `reason` may name alternatives (`a|b`): an authority field is
            // refused by the dispatcher's connection-bound rule or the
            // mint's closed param set, whichever runs first.
            assert!(
                reason.split('|').any(|r| error.to_string().contains(r)),
                "{case}: refused for the wrong reason: {error}"
            );
        }
    }
}

fn ops(value: &Value) -> Vec<String> {
    value
        .as_array()
        .expect("operations array")
        .iter()
        .map(|op| op.as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn cad1537_member_screen_is_read_only_and_every_write_slot_and_forged_mint_is_refused() {
    let fx = Fx::start();
    let member = fx.member();
    let before = fx.records();
    assert!(
        before.to_string().contains(SEEDED)
            && before["records"].as_array().map(Vec::len) == Some(1),
        "fixture BLOCKED: seeded record"
    );
    let segments_before = fx.segment_count();
    let csv = "record_id,display_name,email,consent_email,expected_revision\nc1537-csv,CSV new,csv@example.test,unknown,\n";
    let csv_args =
        json!({"csv_text": csv, "preview_token": "c1537-preview", "request_id": "c1537-csv"});

    // Positive control: an operator mount advertises the declared writes,
    // so the narrowing below is the read-only rule, not the package.
    let operator = fx.local_operator();
    let op_mint = fx.mint(&operator, 1, None).expect("operator mint");
    assert!(
        ops(&op_mint["operations"]).contains(&"records.create".to_owned()),
        "fixture BLOCKED: operator mount must advertise the declared write"
    );

    // (c) Forged mint fields. `read_only` can only narrow: false, a string
    // and a number refuse; an extra authority field refuses.
    for (field, value, reason) in [
        ("read_only", json!(false), "read_only can only be true"),
        ("read_only", json!("true"), "read_only can only be true"),
        ("read_only", json!(0), "read_only can only be true"),
        (
            "operator",
            json!(true),
            "unsupported fields|is not accepted",
        ),
        (
            "role",
            json!("operator"),
            "unsupported fields|is not accepted",
        ),
    ] {
        for (who, session) in [("member", &member), ("operator", &operator)] {
            refused_by(
                fx.mint(session, 2, Some((field, value.clone()))),
                reason,
                &format!("forged mint {field}={value} as {who}"),
            );
        }
    }

    // The `--read-only` board relays `read_only: true` on an operator
    // session's mint. The daemon holds that mount to the read allowlist
    // itself: a read passes, every write and SLOT refuses.
    let ro_mint = fx
        .mint(&operator, 2, Some(("read_only", json!(true))))
        .expect("read-only board mint");
    let ro_action = ro_mint["action_token"]
        .as_str()
        .expect("read-only mount needs its action context")
        .to_owned();
    let ro_consumed = fx.consume(&ro_mint, "read-only board");
    for write in WRITES {
        assert!(
            !ops(&ro_consumed["operations"]).contains(&write.to_string()),
            "read-only board mount advertises write {write}"
        );
    }
    assert_eq!(
        ro_consumed["tools"],
        json!({}),
        "read-only board mount advertises tools"
    );
    let read = fx
        .service(
            "app_screen_service_call",
            &operator,
            &ro_action,
            "records.list",
            json!({}),
        )
        .expect("read-only board read verb over RPC");
    assert!(
        read.to_string().contains(SEEDED),
        "read-only board read misses the record"
    );
    for (method, operation, args) in [
        (
            "app_screen_service_call",
            "records.create",
            json!({"record_id": "c1537-ro-forbidden", "profile": profile("Forbidden", "ro@example.test")}),
        ),
        (
            "app_screen_service_call",
            "segments.save",
            json!({"segment_id": "c1537-ro-segment", "name": "Forbidden", "predicates": []}),
        ),
        (
            "app_screen_service_slot_prepare",
            "records.csv.import",
            csv_args.clone(),
        ),
    ] {
        refused_by(
            fx.service(method, &operator, &ro_action, operation, args),
            GUARD,
            &format!("read-only board {operation} via {method}"),
        );
    }

    // Member over HTTP: the real public board mounts read-only (no write,
    // no tools in the receipt).
    let (status, http_mint) = fx.post(
        &member,
        &format!("/api/app-installations/{}/screens/main/mount", fx.install),
        &json!({"context_id": fx.context}),
    );
    assert_eq!(status, 200, "member HTTP mount: {http_mint}");
    for write in WRITES {
        assert!(
            !ops(&http_mint["operations"]).contains(&write.to_string()),
            "member HTTP mount advertises write {write}"
        );
    }
    assert_eq!(
        http_mint["tools"],
        json!([]),
        "member HTTP mount advertises tools"
    );

    // (c) A member RPC mint with no flag still comes out read-only.
    let mint = fx.mint(&member, 3, None).expect("member read-only mint");
    let action = mint["action_token"]
        .as_str()
        .expect("member mount needs its action context")
        .to_owned();
    let receipt_ops = ops(&mint["operations"]);
    assert!(
        receipt_ops.contains(&"records.list".to_owned()),
        "member receipt lost the read verb"
    );
    for write in WRITES {
        assert!(
            !receipt_ops.contains(&write.to_string()),
            "member receipt advertises write {write}"
        );
    }

    // (a) Member writes over HTTP, refused before any relay.
    for (operation, args) in [
        (
            "records.create",
            json!({"record_id": "c1537-member-http", "profile": profile("Forbidden", "http@example.test")}),
        ),
        (
            "segments.save",
            json!({"segment_id": "c1537-member-http", "name": "Forbidden", "predicates": []}),
        ),
    ] {
        let (status, body) = fx.post(
            &member,
            "/api/app-screen-services/call",
            &json!({"action_token": action, "operation": operation, "args": args}),
        );
        assert_eq!(
            status, 403,
            "member {operation} over HTTP must be refused: {body}"
        );
    }
    // (b) Over HTTP the slot and tool routes stay the operator's.
    for (path, body) in [
        (
            "/api/app-screen-services/slot-prepare",
            json!({"action_token": action, "operation": "records.csv.import", "args": csv_args}),
        ),
        (
            "/api/app-screen-services/slot-confirm",
            json!({"action_token": action,
            "confirmation_token": "0".repeat(64), "confirmation": {"confirmed": true}}),
        ),
        (
            "/api/app-screen-tools/invoke",
            json!({"action_token": action, "tool_alias": "any", "input": {}}),
        ),
    ] {
        let (status, answer) = fx.post(&member, path, &body);
        assert_eq!(status, 403, "member {path} must be refused: {answer}");
        assert_eq!(
            answer["check"], "member_role",
            "member {path} refused by the wrong guard: {answer}"
        );
    }
    // An agent or a detached caller is refused by operator_connection,
    // even carrying the member's live credentials and mount.
    for who in [
        Asserted::Agent("synthetic-worker".into()),
        Asserted::Unproven,
    ] {
        let result = fx.rpc(
            who,
            "app_screen_service_call",
            fx.creds(
                &member,
                json!({"action_token": action, "operation": "records.list", "args": {}}),
            ),
        );
        assert!(
            result.is_err(),
            "agent/detached caller read a member screen"
        );
    }

    // The member must be able to SEE its read-only screen: the real frame
    // consume of its mount, with no write and no tools in the projection.
    let consumed = fx.consume(&mint, "member");
    let consumed_ops = ops(&consumed["operations"]);
    assert!(
        consumed_ops.contains(&"records.list".to_owned()),
        "member consume lost the read verb"
    );
    for write in WRITES {
        assert!(
            !consumed_ops.contains(&write.to_string()),
            "member consume advertises write {write}"
        );
    }
    assert_eq!(
        consumed["tools"],
        json!({}),
        "member mount advertises tools"
    );

    // Positive: the member reads what the operator stored — RPC and HTTP.
    let read = fx
        .service(
            "app_screen_service_call",
            &member,
            &action,
            "records.list",
            json!({}),
        )
        .expect("member read verb over RPC");
    assert!(
        read.to_string().contains(SEEDED),
        "member RPC read misses the record"
    );
    let (status, body) = fx.post(
        &member,
        "/api/app-screen-services/call",
        &json!({"action_token": action, "operation": "records.list", "args": {}}),
    );
    assert_eq!(status, 200, "member read verb over HTTP: {body}");
    assert!(
        body.to_string().contains(SEEDED),
        "member HTTP read misses the record"
    );

    // (a)+(b) The daemon refuses a member's write and SLOT prepare on the
    // read-only rule itself (args valid, verbs declared, mount live).
    for (method, operation, args) in [
        (
            "app_screen_service_call",
            "records.create",
            json!({"record_id": "c1537-member-forbidden", "profile": profile("Forbidden", "forbidden@example.test")}),
        ),
        (
            "app_screen_service_call",
            "records.update",
            json!({"record_id": SEEDED, "expected_revision": 1, "profile": profile("Overwritten", "seeded@example.test")}),
        ),
        (
            "app_screen_service_call",
            "segments.save",
            json!({"segment_id": "c1537-member-segment", "name": "Forbidden", "predicates": []}),
        ),
        (
            "app_screen_service_slot_prepare",
            "records.csv.import",
            csv_args.clone(),
        ),
    ] {
        refused_by(
            fx.service(method, &member, &action, operation, args),
            GUARD,
            &format!("member {operation} via {method}"),
        );
    }
    // A member never holds a confirmation token (only prepare mints one),
    // so the daemon's confirm refuses it as spent/unknown.
    refused_by(
        fx.rpc(
            Asserted::Operator,
            "app_screen_service_slot_confirm",
            fx.creds(&member, json!({"action_token": action, "confirmation_token": "0".repeat(64), "confirmation": {"confirmed": true}})),
        ),
        "spent or unknown",
        "member slot-confirm over RPC",
    );

    // Nothing a read-only caller attempted reached durable state.
    assert_eq!(fx.records(), before, "a refused write changed the records");
    assert_eq!(
        fx.segment_count(),
        segments_before,
        "a refused write saved a segment"
    );
}
