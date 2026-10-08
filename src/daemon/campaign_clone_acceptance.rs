//! CAD-1182 ticket-authored acceptance of the real campaign clone guard.
//!
//! This check proves an operator can clone saved content, while both a
//! registered agent and an unproven daemon caller are refused without
//! mutation. It repeats the operator proof through the real board HTTP peer
//! and proves source approval, recipient freeze, accepted-test evidence,
//! prepared-send state, and proposal state do not become clone authority.
//!
//! Guard exercised: `Shared::operator_connection` in
//! `rpc_app_content` (`app_content_rpc.rs`) plus the `app_content_clone`
//! caller rule/dispatch arm; HTTP additionally exercises the clone write
//! route's `operator::admit` identity/session gate before relaying to that
//! same daemon operation.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use super::Shared;
use crate::store::app_audiences::AudienceBase;
use crate::store::app_records::{CustomerProfile, RecordStore};
use crate::store::app_sends::SendDraft;
use crate::store::NewAgent;
use crate::test_seam::{scoped, Asserted, Seam};
use serde_json::{json, Value};

fn pid() -> u32 {
    std::process::id()
}

struct Fixture {
    dir: tempfile::TempDir,
    shared: Option<Arc<Shared>>,
    install: String,
    context: String,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::Builder::new().prefix("c1182").tempdir().unwrap();
        let pm = dir.path().join("pm");
        crate::issue::Pm::init(&pm).unwrap();
        let opts = crate::daemon::ServeOptions::default();
        opts.provider_env
            .set("CADENCE_PM_DIR", pm.to_str().unwrap());
        let shared = Shared::new(dir.path(), &opts).unwrap();
        let cwd = dir.path().to_str().unwrap().to_string();
        for (alias, provider, kind) in [("master", "pi", "managed"), ("worker", "fake", "fake")] {
            shared
                .store
                .register_agent(&NewAgent {
                    alias,
                    provider,
                    endpoint_kind: kind,
                    role: "worker",
                    cwd: &cwd,
                    sandbox: "read-only",
                    instructions: None,
                    params: None,
                    team_role: None,
                    model_policy: None,
                })
                .unwrap();
        }
        let source = format!("{}/workspace-apps/crm", env!("CARGO_MANIFEST_DIR"));
        let install = scoped(Asserted::Operator, || {
            shared.dispatch("app_workspace_install", &json!({"source": source}), pid())
        })
        .unwrap()["install_id"]
            .as_str()
            .unwrap()
            .to_string();
        let config =
            crate::store::app_contexts::ContextConfig::new("Clone acceptance", BTreeMap::new())
                .unwrap();
        let context = shared
            .store
            .app_context_create(&install, &config, "cad-1182-context")
            .unwrap()["context"]["id"]
            .as_str()
            .unwrap()
            .to_string();
        Self {
            dir,
            shared: Some(shared),
            install,
            context,
        }
    }

    fn rpc(&self, who: Asserted, method: &str, params: Value) -> crate::Result<Value> {
        let shared = self
            .shared
            .as_ref()
            .expect("fixture daemon already stopped");
        scoped(who, || shared.dispatch(method, &params, pid()))
    }

    fn operator(&self, method: &str, params: Value) -> Value {
        self.rpc(Asserted::Operator, method, params).unwrap()
    }

    fn scope(&self) -> Value {
        json!({"install_id": self.install, "context_id": self.context})
    }

    fn db(&self) -> rusqlite::Connection {
        let path =
            crate::store::app_records::record_db_path(self.dir.path(), &self.install).unwrap();
        rusqlite::Connection::open(path).unwrap()
    }

    fn count(&self, sql: &str, id: &str) -> i64 {
        self.db()
            .query_row(sql, [self.context.as_str(), id], |row| row.get(0))
            .unwrap()
    }
}

fn refusal(result: crate::Result<Value>, reason: &str) {
    let error = result.expect_err("unauthorized clone unexpectedly succeeded");
    let text = error.to_string();
    assert!(
        text.contains(reason),
        "refusal did not identify {reason:?}: {text}"
    );
    assert!(
        !text.contains("unknown app content method"),
        "fixture did not reach clone guard: {text}"
    );
}

struct BoardStop(Arc<AtomicBool>, Vec<std::thread::JoinHandle<()>>);

impl Drop for BoardStop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
        for thread in self.1.drain(..).rev() {
            let _ = thread.join();
        }
    }
}

struct Board {
    agent: ureq::Agent,
    base: String,
    host: String,
    token: String,
    cookie: String,
    session_key: String,
    _stop: BoardStop,
}

impl Board {
    fn start(state: &std::path::Path) -> Self {
        let pm = state.join("pm");
        let mut stop = BoardStop(Arc::new(AtomicBool::new(false)), Vec::new());
        let opts = crate::daemon::ServeOptions {
            test_seam: true,
            stop: Some(stop.0.clone()),
            ..Default::default()
        };
        opts.provider_env
            .set("CADENCE_PM_DIR", pm.to_str().unwrap());
        let daemon_state = state.to_path_buf();
        stop.1.push(std::thread::spawn(move || {
            crate::daemon::serve_with(&daemon_state, opts).unwrap()
        }));
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while !state.join("cadence.sock").exists() || Seam::token_at(state).is_none() {
            assert!(
                std::time::Instant::now() < deadline,
                "test daemon did not start"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        let token = Seam::token_at(state).unwrap();
        let mut port = 3110 + (pid() % 80) as u16;
        loop {
            let (startup, ready) = std::sync::mpsc::channel();
            let opts = crate::ui::ServeOpts {
                host: "127.0.0.1".into(),
                port,
                stop: Some(stop.0.clone()),
                startup: Some(startup),
                test_seam: true,
                ..Default::default()
            };
            let (state, pm) = (state.to_path_buf(), pm.clone());
            let thread = std::thread::spawn(move || drop(crate::ui::serve(&state, &pm, &opts)));
            match ready.recv_timeout(Duration::from_secs(30)).unwrap() {
                Ok(()) => {
                    stop.1.push(thread);
                    break;
                }
                Err(_) if port < 3199 => {
                    port += 1;
                    thread.join().unwrap();
                }
                Err(kind) => panic!("board could not bind: {kind:?}"),
            }
        }
        crate::operator_auth::ensure_secret(state).unwrap();
        let secret = crate::operator_auth::read_secret(state).unwrap();
        let nonce = scoped(Asserted::Operator, || {
            crate::client::rpc(
                state,
                "operator_link_mint",
                json!({"secret": secret, "origin": "loopback"}),
            )
        })
        .unwrap()["nonce"]
            .clone();
        let host = format!("cadence-{port}.localhost:{port}");
        let base = format!("http://127.0.0.1:{port}");
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .build()
            .into();
        let session = agent
            .post(format!("{base}/api/session"))
            .header("Host", &host)
            .header("X-Cadence-Board", "1")
            .header("Origin", format!("http://{host}"))
            .header(crate::test_seam::AS_HEADER, "operator")
            .header(crate::test_seam::TOKEN_HEADER, &token)
            .header("Content-Type", "application/json")
            .send(json!({"nonce": nonce}).to_string())
            .unwrap();
        let set_cookie = session.headers()["set-cookie"].to_str().unwrap();
        let cookie = set_cookie[..set_cookie.find(';').unwrap()].to_owned();
        let body: Value = session.into_body().read_json().unwrap();
        Self {
            agent,
            base,
            host,
            token,
            cookie,
            session_key: body["session_key"].as_str().unwrap().to_string(),
            _stop: stop,
        }
    }

    fn clone(&self, who: &str, path: &str, body: &Value) -> (u16, Value) {
        let mut request = self
            .agent
            .post(format!("{}{path}", self.base))
            .header("Host", &self.host)
            .header("X-Cadence-Board", "1")
            .header("Origin", format!("http://{}", self.host))
            .header(crate::test_seam::AS_HEADER, who)
            .header(crate::test_seam::TOKEN_HEADER, &self.token)
            .header("Content-Type", "application/json");
        if who == "operator" {
            request = request
                .header("Cookie", &self.cookie)
                .header("X-Cadence-Session", &self.session_key);
        }
        let response = request.send(body.to_string()).unwrap();
        let status = response.status().as_u16();
        let reply = response.into_body().read_json().unwrap_or(Value::Null);
        (status, reply)
    }
}

/// The exact `app_content_clone` operator-connection guard refuses both a
/// registered agent and an unproven caller before any write. The positive
/// control proves the method is dispatched and its source-fixture evidence
/// is real; the same allowed/refused behavior is then checked over HTTP.
#[test]
fn campaign_clone_is_operator_only_and_starts_without_source_authority() {
    let mut fixture = Fixture::new();
    let source = "spring-source";
    let mut save = fixture.scope();
    save["campaign_id"] = json!(source);
    save["subject"] = json!("Saved source subject");
    save["preheader"] = json!("Saved source preheader");
    save["blocks"] = json!([{"type":"paragraph","text":"Saved source body"}]);
    let saved = fixture.operator("app_content_save", save);
    let source_revision = saved["content"]["revision"].as_i64().unwrap();
    let source_digest = saved["content"]["content_digest"]
        .as_str()
        .unwrap()
        .to_owned();

    let mut binding = fixture.scope();
    binding["binding_id"] = json!("sender-source");
    binding["sender_name"] = json!("Campaign Team");
    binding["sender_address"] = json!("team@example.test");
    binding["unsubscribe_base"] = json!("https://example.test/unsubscribe");
    fixture.operator("app_sender_binding_save", binding);
    let mut approve = fixture.scope();
    approve["campaign_id"] = json!(source);
    approve["expected_revision"] = json!(source_revision);
    assert_eq!(
        fixture.operator("app_content_approve", approve)["content"]["approval"]["valid"],
        json!(true)
    );
    let mut test_prepare = fixture.scope();
    test_prepare["campaign_id"] = json!(source);
    test_prepare["to_email"] = json!("operator@example.test");
    test_prepare["binding_id"] = json!("sender-source");
    assert_eq!(
        fixture.operator("app_content_test_prepare", test_prepare)["test_send"]["preview_only"],
        json!(true)
    );

    let records = RecordStore::open(fixture.dir.path(), &fixture.install).unwrap();
    let profile = CustomerProfile::parse(&json!({
        "schema": 1,
        "display_name": "Eligible source recipient",
        "email": "eligible@example.test",
        "tags": [],
        "consent": {"email": "granted"}
    }))
    .unwrap();
    records
        .app_record_create(&fixture.context, "customer-source", &profile)
        .unwrap();
    let freeze = records
        .app_audience_prepare(
            &fixture.context,
            "freeze-source",
            &AudienceBase::All,
            None,
            10,
        )
        .unwrap();
    assert_eq!(freeze["freeze"]["final_count"], json!(1));
    let binding_view = records
        .app_sender_binding_show(&fixture.context, "sender-source")
        .unwrap();
    records
        .app_campaign_test_send_record(
            &fixture.context,
            source,
            &source_digest,
            binding_view["binding"]["binding_digest"].as_str().unwrap(),
        )
        .unwrap();
    let mut proposal = fixture.scope();
    proposal["campaign_id"] = json!(source);
    proposal["proposal_id"] = json!("proposal-source");
    proposal["subject"] = json!("Suggested source subject");
    proposal["preheader"] = json!("Suggested source preheader");
    proposal["blocks"] = json!([{"type":"paragraph","text":"Suggested source body"}]);
    fixture.operator("app_content_propose", proposal);
    let prepared = SendDraft {
        send_id: "send-source".into(),
        campaign_id: source.into(),
        request_id: "request-source".into(),
        content_revision: source_revision,
        content_digest: source_digest.clone(),
        audience_freeze_id: "freeze-source".into(),
        audience_digest: freeze["freeze"]["digest"].as_str().unwrap().into(),
        connection_id: "fixture-connection".into(),
        auth_revision: 1,
        link_revision: 1,
        link_digest: "fixture-link-digest".into(),
        max_recipients: 10,
        unsubscribe_origin: "https://example.test".into(),
        send_digest: "fixture-prepared-send-digest".into(),
    };
    assert_eq!(
        records
            .app_campaign_send_prepare(&fixture.context, &prepared)
            .unwrap()
            .state,
        "prepared"
    );
    // Positive persisted send-authorization fixture: it creates one queued
    // row in this isolated DB, but no worker or SMTP transport is invoked.
    records
        .app_campaign_send_approve(
            &fixture.context,
            "send-source",
            &[(
                "customer-source".into(),
                "eligible@example.test".into(),
                "fixture-idempotency-key".into(),
            )],
        )
        .unwrap();

    assert_eq!(
        fixture.count(
            "SELECT count(*) FROM app_content_proposals WHERE context_id=? AND campaign_id=?",
            source
        ),
        1
    );
    assert_eq!(
        fixture.count(
            "SELECT count(*) FROM app_campaign_test_sends WHERE context_id=? AND campaign_id=?",
            source
        ),
        1
    );
    assert_eq!(fixture.count("SELECT count(*) FROM app_campaign_sends WHERE context_id=? AND campaign_id=? AND state='sending' AND approved_at IS NOT NULL", source), 1);
    assert_eq!(
        fixture.count(
            "SELECT count(*) FROM app_campaign_deliveries WHERE context_id=? AND send_id=?",
            "send-source"
        ),
        1
    );
    assert_eq!(
        fixture.count(
            "SELECT count(*) FROM app_audience_freezes WHERE context_id=? AND freeze_id=?",
            "freeze-source"
        ),
        1
    );

    let mut clone = fixture.scope();
    clone["campaign_id"] = json!(source);
    clone["expected_revision"] = json!(source_revision);
    clone["name"] = json!("Source fresh copy");
    clone["copy_audience"] = json!(true);
    clone["source_freeze_id"] = json!("freeze-source");
    clone["copy_sender"] = json!(true);
    clone["source_binding_id"] = json!("sender-source");
    // Positive operator control: this must be the real clone implementation,
    // not an unknown-method fixture failure.
    let first = fixture.operator("app_content_clone", clone.clone());
    let clone_id = first["content"]["campaign_id"].as_str().expect("clone id");
    assert_ne!(clone_id, source);
    assert_eq!(first["content"]["revision"], json!(1));
    assert_eq!(first["content"]["subject"], json!("Saved source subject"));
    assert_eq!(
        first["starter_selection"]["audience"]["base"]["mode"],
        json!("all")
    );
    assert_eq!(
        first["starter_selection"]["sender_binding_id"],
        json!("sender-source")
    );
    assert!(first["starter_selection"]["audience"]
        .get("freeze_id")
        .is_none());
    assert!(first["starter_selection"]["audience"]
        .get("member_ids")
        .is_none());
    assert_eq!(first["content"]["approval"]["valid"], json!(false));
    assert_eq!(first["content"]["approval"]["revision"], Value::Null);

    let clones_before = fixture.count(
        "SELECT count(*) FROM app_content_docs WHERE context_id=? AND campaign_id != ?",
        "",
    );
    let target_sends_before = fixture.count(
        "SELECT count(*) FROM app_campaign_sends WHERE context_id=? AND campaign_id=?",
        clone_id,
    );
    for who in [Asserted::Agent("worker".into()), Asserted::Unproven] {
        refusal(
            fixture.rpc(who, "app_content_clone", clone.clone()),
            "operator",
        );
        assert_eq!(
            fixture.count(
                "SELECT count(*) FROM app_content_docs WHERE context_id=? AND campaign_id=?",
                clone_id
            ),
            1
        );
        assert_eq!(
            fixture.count(
                "SELECT count(*) FROM app_content_docs WHERE context_id=? AND campaign_id != ?",
                ""
            ),
            clones_before
        );
        assert_eq!(
            fixture.count(
                "SELECT count(*) FROM app_campaign_sends WHERE context_id=? AND campaign_id=?",
                clone_id
            ),
            target_sends_before
        );
    }

    // New clone has content only: the source's evidence remains attached to
    // its source campaign; none of it appears under the host-minted target.
    assert_eq!(
        fixture.count(
            "SELECT count(*) FROM app_content_proposals WHERE context_id=? AND campaign_id=?",
            clone_id
        ),
        0
    );
    assert_eq!(
        fixture.count(
            "SELECT count(*) FROM app_campaign_test_sends WHERE context_id=? AND campaign_id=?",
            clone_id
        ),
        0
    );
    assert_eq!(
        fixture.count(
            "SELECT count(*) FROM app_campaign_sends WHERE context_id=? AND campaign_id=?",
            clone_id
        ),
        0
    );
    assert_eq!(
        fixture.count(
            "SELECT count(*) FROM app_campaign_deliveries WHERE context_id=? AND send_id IN (SELECT send_id FROM app_campaign_sends WHERE campaign_id=?)",
            clone_id
        ),
        0
    );
    let shown = fixture.operator("app_content_show", json!({"install_id": fixture.install, "context_id": fixture.context, "campaign_id": clone_id}));
    assert_eq!(shown["content"]["approval"]["valid"], json!(false));
    assert_eq!(shown["content"]["approval"]["digest"], Value::Null);

    let source_unchanged = fixture.operator("app_content_show", json!({"install_id": fixture.install, "context_id": fixture.context, "campaign_id": source}));
    assert_eq!(
        source_unchanged["content"]["content_digest"],
        json!(source_digest)
    );
    assert_eq!(
        source_unchanged["content"]["approval"]["valid"],
        json!(true)
    );

    let path = format!(
        "/api/app-installations/{}/contexts/{}/content/campaigns/{}/clone",
        fixture.install, fixture.context, source
    );
    let request = json!({
        "expected_revision": source_revision,
        "name": "HTTP fresh copy",
        "copy_audience": true,
        "source_freeze_id": "freeze-source",
        "copy_sender": false
    });
    let state = fixture.dir.path().to_path_buf();
    drop(fixture.shared.take());
    let board = Board::start(&state);
    let (status, reply) = board.clone("operator", &path, &request);
    assert_eq!(status, 200, "operator HTTP control: {reply}");
    let http_clone_id = reply["content"]["campaign_id"]
        .as_str()
        .expect("HTTP clone id");
    assert_ne!(http_clone_id, source);
    assert_eq!(reply["content"]["approval"]["valid"], json!(false));
    let before_agent = fixture.count(
        "SELECT count(*) FROM app_content_docs WHERE context_id=? AND campaign_id != ?",
        "",
    );
    assert_eq!(
        fixture.count(
            "SELECT count(*) FROM app_content_docs WHERE context_id=? AND campaign_id=?",
            http_clone_id,
        ),
        1
    );
    let (status, reply) = board.clone("agent:worker", &path, &request);
    assert_eq!(status, 403, "agent HTTP refusal: {reply}");
    assert_eq!(reply["check"], json!("operator_only"));
    assert_eq!(
        fixture.count(
            "SELECT count(*) FROM app_content_docs WHERE context_id=? AND campaign_id != ?",
            "",
        ),
        before_agent
    );
}
