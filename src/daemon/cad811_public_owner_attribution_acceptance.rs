// Independent CAD-811 S1 acceptance; author 010412b8-daad-4fa.
// Frozen ticket contract: Native verified_owner_human / relay_owner_proof
// attribution survives delegated writes; absence remains valid only locally.
// Included as a CHILD of the unchanged cad811_screen_services_acceptance module.
// No new product export, copied verifier, JWT bypass, actor, provider or send.
// Trusted session fixture scope: existing Native Auth::open_public_bound plus
// membership_lookup_for_test. This does NOT re-prove issuer/JWKS/login security.

use super::{call, profile, Fixture, Scope, Servers, Session};
use crate::board_identity::Role;
use crate::daemon::{serve, ServeOptions, Shared};
use crate::operator_auth::{BoardUser, PublicRealm};
use crate::test_seam::{Asserted, Seam};
use rusqlite::{params, Connection, OpenFlags};
use serde_json::{json, Value};
use std::os::unix::net::UnixListener;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

const COMPANY: &str = "co_c811_owner";
const INSTANCE: &str = "c811owner";
const OWNER: &str = "synthetic-c811-owner";
const OPERATIONS: &[&str] = &[
    "records.create",
    "records.update",
    "records.csv.preview",
    "records.csv.confirm",
    "records.csv.import",
    "content.save",
    "chat.campaign.brief",
];

fn must<T, E>(result: std::result::Result<T, E>, what: &str) -> T {
    match result {
        Ok(value) => value,
        Err(_) => panic!("fixture BLOCKED: {what}"),
    }
}

pub(super) struct Native {
    // Drop the owned board/socket threads before the temp state or Shared.
    pub(super) fx: Fixture,
    pub(super) shared: Arc<Shared>,
    member_live: Arc<AtomicBool>,
    lookups: Arc<AtomicUsize>,
    pub(super) issuer: String,
}

impl Native {
    pub(super) fn new() -> Self {
        Self::new_with_subs(&[OWNER])
    }

    /// Same fixture, admitting exactly `allowed_subs` through the membership
    /// lookup. The Works two-human test passes the owner plus one second
    /// synthetic sub; every other caller stays on `new()`.
    pub(super) fn new_with_subs(allowed_subs: &[&str]) -> Self {
        let allowed_subs: Vec<String> = allowed_subs.iter().map(|sub| (*sub).to_owned()).collect();
        assert!(
            std::env::var_os("CADENCE_SOCKET").is_none(),
            "fixture BLOCKED: inherited socket override; do not clear caller identity"
        );
        let root = must(
            tempfile::Builder::new().prefix("c811owner").tempdir(),
            "temporary state",
        );
        let state = root.path().to_path_buf();
        let pm = state.join("pm");
        must(crate::issue::Pm::init(&pm), "isolated PM");

        // Reserve the board through its REAL startup acknowledgement, not a
        // racy health check or adoption of a neighbor. Only owned 3110-3199.
        // Build Shared after selecting a disjoint free candidate; the actual
        // board bind still must acknowledge success or this attempt is BLOCKED.
        let reservation = (3110..3200)
            .filter(|port| !matches!(*port, 3139 | 3140))
            .find_map(|port| {
                std::net::TcpListener::bind(("127.0.0.1", port))
                    .ok()
                    .map(|listener| (port, listener))
            })
            .expect("fixture BLOCKED: no isolated board port");
        let (port, reservation) = reservation;
        let host = format!("c811-owner.board.localhost:{port}");
        let issuer = format!("http://127.0.0.1:{port}");
        must(
            crate::board_identity::write_config(
                &state,
                &crate::board_identity::Config {
                    host: host.clone(),
                    issuer: issuer.clone(),
                    company: COMPANY.into(),
                },
            ),
            "synthetic board realm",
        );
        let opts = ServeOptions {
            provider_env: crate::adapter::ProviderEnv::refusing_providers(),
            test_seam: true,
            lease: Some(Default::default()),
            ..Default::default()
        };
        opts.provider_env
            .set("CADENCE_PM_DIR", pm.to_str().expect("fixture PM path"));
        // Exactly the supported Native fixture binding (not Standalone).
        // These markers belong only to this private ServeOptions, not the
        // caller environment, production org, installed binary or PM policy.
        for (name, value) in [
            ("AGENTICOS_COMPANY", COMPANY),
            ("AGENTICOS_INSTANCE", INSTANCE),
            ("AGENTICOS_BOARD_HOST", host.as_str()),
            ("AGENTICOS_BOARD_ISSUER", issuer.as_str()),
            ("AGENTICOS_BOARD_COMPANY", COMPANY),
        ] {
            opts.provider_env.set(name, value);
        }
        let mut shared = must(
            Shared::new(&state, &opts),
            "Native Shared construction/admission",
        );
        assert!(
            matches!(
                &shared.membership_mode,
                crate::daemon::public_membership::MembershipMode::Native(_)
            ),
            "fixture BLOCKED: expected Native, never Standalone"
        );
        let member_live = Arc::new(AtomicBool::new(true));
        let lookups = Arc::new(AtomicUsize::new(0));
        let (live, seen) = (member_live.clone(), lookups.clone());
        Arc::get_mut(&mut shared)
            .expect("fresh unique Shared")
            .membership_lookup_for_test = Some(Box::new(move |_hash, sub, company, instance| {
            assert!(
                allowed_subs.iter().any(|allowed| allowed.as_str() == sub)
                    && company == COMPANY
                    && instance == INSTANCE,
                "fixture BLOCKED: wrong synthetic membership binding"
            );
            seen.fetch_add(1, Ordering::SeqCst);
            Ok(if live.load(Ordering::SeqCst) {
                Some(Role::Operator)
            } else {
                None
            })
        }));
        // A registered inert mailbox, NOT a running master/provider. No actor
        // loop is started, so a brief can persist but cannot execute a model.
        must(
            shared.store.register_agent(&crate::store::NewAgent {
                alias: "master",
                provider: "fake",
                endpoint_kind: "fake",
                role: "pm",
                cwd: state.to_str().expect("fixture state path"),
                sandbox: "read-only",
                instructions: None,
                params: None,
                team_role: None,
                model_policy: None,
            }),
            "synthetic inert master registration",
        );

        let mut servers = Servers {
            stop: Arc::new(AtomicBool::new(false)),
            threads: Vec::new(),
        };
        let listener = must(
            UnixListener::bind(state.join("cadence.sock")),
            "owned RPC bind",
        );
        must(listener.set_nonblocking(true), "RPC listener mode");
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
                assert!(connection.join().is_ok(), "owned RPC connection panicked");
            }
        }));
        let seam = Seam::token_at(&state).expect("fixture BLOCKED: native seam was not armed");
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
        // A concurrent bind wins => BLOCKED, never kill/adopt it or change ports
        // outside the approved range. Servers RAII still joins our own socket.
        drop(reservation);
        servers.threads.push(std::thread::spawn(move || {
            drop(crate::ui::serve(&state, &pm, &board_opts))
        }));
        let bound = must(
            receipt.recv_timeout(Duration::from_secs(10)),
            "board startup receipt",
        );
        assert!(
            bound.is_ok(),
            "fixture BLOCKED: real board could not bind reserved port"
        );
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(10)))
            .build()
            .into();
        let fx = Fixture {
            servers,
            root,
            agent,
            base: format!("http://127.0.0.1:{port}"),
            host,
            seam,
        };
        Self {
            fx,
            shared,
            member_live,
            lookups,
            issuer,
        }
    }

    pub(super) fn rpc(&self, method: &str, params: Value) -> crate::Result<Value> {
        self.fx.rpc(Asserted::Operator, method, params)
    }

    pub(super) fn owner(&self) -> (Session, Value) {
        let user = BoardUser {
            sub: OWNER.into(),
            email: "c811-owner@example.test".into(),
            name: "Synthetic CRM owner".into(),
            role: "operator".into(),
            handle: OWNER.into(),
        };
        let display = user.actor();
        let now = self.shared.operator_now();
        let opened = must(
            self.shared.operator_auth().open_public_bound(
                user,
                "c811-owner-case-jti",
                now + 60,
                "c811-isolated-acceptance",
                PublicRealm {
                    issuer: self.issuer.clone(),
                    company: COMPANY.into(),
                    host: self.fx.host.clone(),
                },
                now,
            ),
            "supported Native session constructor",
        )
        .expect("fixture BLOCKED: synthetic JTI was not fresh");
        let expected = json!({"sub": OWNER, "company": COMPANY,
            "session": opened.session.id, "role": "operator", "display": display});
        let session = Session {
            cookie: format!("__Host-aos-board-session={}", opened.token),
            token: opened.token,
            key: String::new(),
        };
        (session, expected)
    }

    pub(super) fn local(&self) -> Session {
        // Same real Auth constructor used by the existing Native fence
        // fixture, not a public-host bootstrap with a local nonce.
        let now = self.shared.operator_now();
        let mut auth = self.shared.operator_auth();
        let nonce = must(
            auth.mint(crate::operator_auth::Origin::Loopback, now),
            "local Native nonce",
        );
        let opened = must(
            auth.open(&nonce, crate::operator_auth::Origin::Loopback, "", now),
            "local Native session",
        )
        .expect("fresh local nonce");
        // Local controls below use RPC; no public-host cookie downgrade.
        Session {
            cookie: String::new(),
            token: opened.token,
            key: opened.key,
        }
    }

    pub(super) fn credentials(&self, session: &Session, origin: &str, mut body: Value) -> Value {
        body["token"] = json!(session.token);
        body["key"] = json!(session.key);
        body["origin"] = json!(origin);
        body
    }

    pub(super) fn mount(
        &self,
        session: &Session,
        origin: &str,
        scope: &Scope,
        generation: u64,
    ) -> String {
        let mint = must(
            self.rpc(
                "app_screen_mint",
                self.credentials(
                    session,
                    origin,
                    json!({"install_id": scope.install, "context_id": scope.context,
                "tag": "main", "generation": generation}),
                ),
            ),
            "real consumed mount mint",
        );
        let action = mint["action_token"]
            .as_str()
            .expect("mount action context")
            .to_owned();
        let nonce = mint["mount"]
            .as_str()
            .expect("mount path")
            .rsplit('/')
            .next()
            .expect("mount nonce");
        let consumed = must(
            self.rpc("app_screen_consume", json!({"nonce": nonce})),
            "real mount consume",
        );
        assert!(
            consumed["install_id"] == scope.install
                && consumed["digest"] == scope.digest
                && consumed["generation"] == generation,
            "consumed mount scope mismatch"
        );
        action
    }

    pub(super) fn service(
        &self,
        session: &Session,
        origin: &str,
        action: &str,
        operation: &str,
        args: Value,
        http: bool,
    ) -> Value {
        let body = call(action, operation, args);
        if http {
            self.http(session, "/api/app-screen-services/call", body)
        } else {
            must(
                self.rpc(
                    "app_screen_service_call",
                    self.credentials(session, origin, body),
                ),
                "valid real service CALL",
            )
        }
    }

    fn http(&self, session: &Session, path: &str, body: Value) -> Value {
        let response = self.fx.post("operator", Some(session), path, &body);
        assert!(
            response.status().as_u16() == 200,
            "valid real HTTP service was refused"
        );
        must(response.into_body().read_json(), "HTTP JSON result")
    }

    pub(super) fn slot(
        &self,
        session: &Session,
        origin: &str,
        action: &str,
        operation: &str,
        inputs: (Value, Value),
        http: bool,
    ) -> Value {
        let (args, confirmation) = inputs;
        let body = call(action, operation, args);
        let prepared = if http {
            self.http(session, "/api/app-screen-services/slot-prepare", body)
        } else {
            must(
                self.rpc(
                    "app_screen_service_slot_prepare",
                    self.credentials(session, origin, body),
                ),
                "real protected SLOT prepare",
            )
        };
        let body = json!({"action_token": action,
            "confirmation_token": prepared["confirmation_token"],
            "confirmation": confirmation});
        if http {
            self.http(session, "/api/app-screen-services/slot-confirm", body)
        } else {
            must(
                self.rpc(
                    "app_screen_service_slot_confirm",
                    self.credentials(session, origin, body),
                ),
                "real protected SLOT confirm",
            )
        }
    }

    fn record_db(&self, scope: &Scope) -> Connection {
        let path = must(
            crate::store::app_records::record_db_path(self.fx.root.path(), &scope.install),
            "installation record path",
        );
        must(
            Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY),
            "read-only durable database",
        )
    }

    fn revision_human(&self, scope: &Scope, kind: &str, id: &str, revision: i64) -> Option<Value> {
        let db = self.record_db(scope);
        let sql = match kind {
            "record" => "SELECT human FROM app_record_revisions WHERE context_id=?1 AND record_id=?2 AND revision=?3",
            "content" => "SELECT human FROM app_content_revisions WHERE context_id=?1 AND campaign_id=?2 AND revision=?3",
            _ => panic!("invalid independent history selector"),
        };
        let text: Option<String> = must(
            db.query_row(sql, params![scope.context, id, revision], |row| row.get(0)),
            "durable revision row",
        );
        text.map(|text| must(serde_json::from_str(&text), "durable human JSON"))
    }

    fn import_human(&self, scope: &Scope, request: &str) -> Option<Value> {
        let db = self.record_db(scope);
        let (state, text): (String, Option<String>) = must(db.query_row(
            "SELECT state,human FROM app_record_csv_imports WHERE context_id=?1 AND request_id=?2",
            params![scope.context, request], |row| Ok((row.get(0)?, row.get(1)?))),
            "durable CSV receipt");
        assert!(
            state == "complete",
            "CSV must actually settle before attribution is judged"
        );
        text.map(|text| must(serde_json::from_str(&text), "durable CSV human JSON"))
    }

    fn brief_humans(&self, receipt: &Value) -> (Option<Value>, Option<Value>) {
        let message = receipt["receipt"]["message"]
            .as_str()
            .expect("actual brief message");
        let conversation = receipt["host_command"]["conversation_id"]
            .as_str()
            .expect("actual campaign conversation");
        assert!(
            must(self.shared.store.message(message), "stored brief message").is_some(),
            "brief must persist a genuine message, not only an acknowledgement"
        );
        let entries = must(
            self.shared.store.thread_entries_of(conversation, 0, 100),
            "durable conversation entries",
        );
        let entry = entries
            .iter()
            .find(|entry| entry.message_id.as_deref() == Some(message))
            .expect("brief must persist its actual linked thread entry");
        let events = must(
            self.shared.store.events_tail("master", 100),
            "durable queued events",
        );
        let queued = events
            .iter()
            .find(|event| {
                event.kind == "queued" && event.payload["message"].as_str() == Some(message)
            })
            .expect("brief must persist its queued event");
        let human = |value: Option<&Value>| value.filter(|value| !value.is_null()).cloned();
        (
            human(
                entry
                    .payload
                    .as_ref()
                    .and_then(|payload| payload.get("human")),
            ),
            human(queued.payload.get("human")),
        )
    }

    pub(super) fn durable_snapshot(&self, scope: &Scope) -> Value {
        let db = self.record_db(scope);
        let mut tables = Vec::new();
        for table in [
            "app_records",
            "app_record_revisions",
            "app_record_csv_imports",
            "app_content_docs",
            "app_content_revisions",
        ] {
            let mut stmt = must(
                db.prepare(&format!("SELECT * FROM {table} ORDER BY rowid")),
                "read-only durable snapshot",
            );
            let columns = stmt.column_count();
            let rows = must(
                stmt.query_map([], |row| {
                    (0..columns)
                        .map(|column| {
                            use rusqlite::types::ValueRef;
                            Ok(match row.get_ref(column)? {
                                ValueRef::Null => "null".into(),
                                ValueRef::Integer(value) => format!("i:{value}"),
                                ValueRef::Real(value) => format!("r:{value}"),
                                ValueRef::Text(value) => {
                                    format!("t:{}", String::from_utf8_lossy(value))
                                }
                                ValueRef::Blob(_) => {
                                    panic!("unexpected blob in synthetic CRM table")
                                }
                            })
                        })
                        .collect::<rusqlite::Result<Vec<String>>>()
                }),
                "snapshot query",
            );
            let rows = must(rows.collect::<rusqlite::Result<Vec<_>>>(), "snapshot rows");
            tables.push(json!({"table": table, "rows": rows}));
        }
        let messages = must(
            self.shared.store.messages("master"),
            "master mailbox snapshot",
        )
        .into_iter()
        .map(|message| message.id)
        .collect::<Vec<_>>();
        let conversations = must(
            self.shared
                .store
                .conversation_list("master", &scope.install),
            "conversation snapshot",
        )
        .into_iter()
        .map(|thread| thread.id)
        .collect::<Vec<_>>();
        let queued = must(
            self.shared.store.events_tail("master", 100),
            "queued snapshot",
        )
        .into_iter()
        .filter(|event| event.kind == "queued")
        .map(|event| event.payload)
        .collect::<Vec<_>>();
        json!({"tables": tables, "messages": messages,
            "conversations": conversations, "queued": queued})
    }
}

fn check_human(
    missing: &mut Vec<String>,
    label: &str,
    actual: Option<Value>,
    expected: Option<&Value>,
) {
    if actual.as_ref() != expected {
        // Labels contain only authored synthetic operation names, never
        // raw proofs, requests, responses, cookie strings or credential hashes.
        missing.push(label.to_string());
    }
}

// ONE parameterized outcome/refusal check, not a restored/routine test suite.
#[test]
fn cad811_public_owner_attribution_survives_native_delegation_and_retirement_refuses() {
    let native = Native::new();
    let scope = native.fx.install("owner-attribution", OPERATIONS);
    let (owner, expected) = native.owner();

    // Existing native endpoint is a positive control independent of the
    // bridge implementation. No new export/field is required by this oracle.
    let baseline = json!({"install_id": scope.install, "context_id": scope.context,
        "record_id": "native-owner-control",
        "profile": profile("Native owner control", "native-control@example.test"),
        "public_session": owner.token});
    must(
        native.rpc("app_record_create", baseline),
        "existing Native owner write control",
    );
    assert!(
        native
            .revision_human(&scope, "record", "native-owner-control", 1)
            .as_ref()
            == Some(&expected),
        "fixture BLOCKED: existing Native owner attribution control"
    );
    assert!(
        native.lookups.load(Ordering::SeqCst) > 0,
        "fixture BLOCKED: actual Native membership path was not exercised"
    );

    let local = native.local();
    let mut missing = Vec::new();
    // Separate generations, same real installation/context. No cache or
    // previous session/mount is used as attribution authority.
    for (prefix, session, origin, human, generation) in [
        ("public", &owner, "public", Some(&expected), 1u64),
        ("local", &local, "loopback", None, 2u64),
    ] {
        let action = native.mount(session, origin, &scope, generation);
        let transports: &[(&str, bool)] = if origin == "public" {
            &[("rpc", false), ("http", true)]
        } else {
            &[("rpc", false)]
        };
        for &(suffix, http) in transports {
            // Public outcomes cover both real RPC and HTTP. Local controls
            // use native RPC; never send a local cookie on a public host.
            let id = format!("{prefix}-{suffix}");
            let email = format!("{id}@example.test");
            let via_http = http;
            let created = native.service(
                session,
                origin,
                &action,
                "records.create",
                json!({"record_id": id, "profile": profile(&id, &email)}),
                via_http,
            );
            assert!(
                created["record"]["revision"] == 1,
                "create outcome must be revision 1"
            );
            check_human(
                &mut missing,
                &format!("{prefix}:create:{suffix}"),
                native.revision_human(&scope, "record", &id, 1),
                human,
            );
            let updated = native.service(
                session,
                origin,
                &action,
                "records.update",
                json!({"record_id": id, "expected_revision": 1,
                    "profile": profile("Updated synthetic customer", &email)}),
                via_http,
            );
            assert!(
                updated["record"]["revision"] == 2,
                "update must actually commit CAS revision 2"
            );
            check_human(
                &mut missing,
                &format!("{prefix}:update:{suffix}"),
                native.revision_human(&scope, "record", &id, 2),
                human,
            );
        }
        let csv_id = format!("{prefix}-csv");
        let updated_id = format!("{prefix}-rpc");
        let csv = format!("record_id,display_name,email,consent_email,expected_revision\n{csv_id},CSV new,{csv_id}@example.test,unknown,\n{updated_id},CSV updated,{updated_id}@example.test,unknown,2\n");
        let preview = native.service(
            session,
            origin,
            &action,
            "records.csv.preview",
            json!({"csv_text": csv}),
            false,
        );
        let request = format!("{prefix}-csv-request");
        let args = json!({"csv_text": csv, "preview_token": preview["preview_token"],
            "request_id": request});
        let confirmed = native.slot(
            session,
            origin,
            &action,
            "records.csv.confirm",
            (args.clone(), json!({"confirmed": true})),
            origin == "public",
        );
        let imported = native.slot(
            session,
            origin,
            &action,
            "records.csv.import",
            (
                args,
                json!({"confirmed": true, "csv_confirm_token": confirmed["confirm_token"]}),
            ),
            origin == "public",
        );
        assert!(
            imported["summary"]["applied"] == 2 && imported["summary"]["failed"] == 0,
            "CSV must actually create and update synthetic rows"
        );
        check_human(
            &mut missing,
            &format!("{prefix}:csv-create"),
            native.revision_human(&scope, "record", &csv_id, 1),
            human,
        );
        check_human(
            &mut missing,
            &format!("{prefix}:csv-update"),
            native.revision_human(&scope, "record", &updated_id, 3),
            human,
        );
        check_human(
            &mut missing,
            &format!("{prefix}:csv-durable-receipt"),
            native.import_human(&scope, &request),
            human,
        );

        let campaign = format!("{prefix}-campaign");
        for revision in 1..=2 {
            let mut args = json!({"campaign_id": campaign,
                "subject": format!("Synthetic owner subject {revision}"),
                "blocks": [{"type": "paragraph", "text": "Synthetic body; no real send"}]});
            if revision > 1 {
                args["expected_revision"] = json!(revision - 1);
            }
            let saved = native.service(
                session,
                origin,
                &action,
                "content.save",
                args,
                origin == "public" && revision == 2,
            );
            assert!(
                saved["content"]["revision"] == revision,
                "content save must commit the requested revision"
            );
            check_human(
                &mut missing,
                &format!("{prefix}:content:{revision}"),
                native.revision_human(&scope, "content", &campaign, revision),
                human,
            );
        }
        let brief = "Synthetic acceptance brief only; persist without running an actor";
        let sent = native.slot(
            session,
            origin,
            &action,
            "chat.campaign.brief",
            (
                json!({"campaign_id": campaign, "brief": brief}),
                json!({"confirmed": true, "brief": brief}),
            ),
            origin == "public",
        );
        let (entry_human, queued_human) = native.brief_humans(&sent);
        check_human(
            &mut missing,
            &format!("{prefix}:brief-thread-entry"),
            entry_human,
            human,
        );
        check_human(
            &mut missing,
            &format!("{prefix}:brief-queued-event"),
            queued_human,
            human,
        );
    }

    // ONE decisive negative scenario, on BOTH real peers. It starts from a
    // freshly consumed/public-owner mount that succeeds immediately before
    // authoritative membership withdrawal; stale generations cannot explain it.
    let action = native.mount(&owner, "public", &scope, 3);
    native.service(
        &owner,
        "public",
        &action,
        "records.update",
        json!({"record_id": "public-http", "expected_revision": 2,
            "profile": profile("Pre-retirement live owner", "public-http@example.test")}),
        false,
    );
    check_human(
        &mut missing,
        "public:pre-retirement-update",
        native.revision_human(&scope, "record", "public-http", 3),
        Some(&expected),
    );
    let before = native.durable_snapshot(&scope);
    let checks = native.lookups.load(Ordering::SeqCst);
    native.member_live.store(false, Ordering::SeqCst);
    let forbidden = call(
        &action,
        "records.create",
        json!({
        "record_id": "retired-owner-must-not-exist",
        "profile": profile("Refused retired owner", "retired@example.test")}),
    );
    match native.rpc(
        "app_screen_service_call",
        native.credentials(&owner, "public", forbidden.clone()),
    ) {
        Err(error) => assert!(
            error.kind() == "rejected" && error.code() == Some("screen_access_denied"),
            "retired public owner must reach the real live-session guard"
        ),
        Ok(_) => panic!("retired public owner RPC was accepted"),
    }
    assert!(
        native.lookups.load(Ordering::SeqCst) > checks,
        "withdrawal must be consumed by the actual Native membership path"
    );
    let response = native.fx.post(
        "operator",
        Some(&owner),
        "/api/app-screen-services/call",
        &forbidden,
    );
    assert!(
        matches!(response.status().as_u16(), 401 | 403),
        "HTTP must refuse retired public authority, not fall back to local or fail transport"
    );
    drop(response);
    assert!(before == native.durable_snapshot(&scope),
        "retired-owner refusal must leave every CRM durable table, mailbox, conversation and queued event unchanged");

    // Do not stop at the first old NULL: run every bounded outcome and refusal
    // above, then fail with static operation labels (never credential values).
    assert!(
        missing.is_empty(),
        "PUBLIC_OWNER_ATTRIBUTION_MISSING: {missing:?}"
    );
}

#[path = "cad811_missing_state_acceptance.rs"]
mod independent_missing_state;
