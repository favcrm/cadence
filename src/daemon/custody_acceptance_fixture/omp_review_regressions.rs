//! Independent OMP Standards regression oracles for approved CAD-1128 r2.
//! Production writers must not edit or weaken these assertions. Real daemon,
//! RPC, core/vault and TLS fixture; simulated operator setup is NOT native
//! authority evidence. Author has not run builds, tests or formatters.
//! Encrypted token enrollment is unsupported: token records below are legacy;
//! token-shaped rotation of an encrypted SMTP record must remain refused.
#![cfg(all(test, target_os = "linux", feature = "test-seam"))]

use super::{Rig, SmtpCounts};
use crate::error::Result;
use crate::platform::encrypted_sqlite::RowMetadata;
use crate::store::NewAgent;
use serde_json::{json, Value};
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

const A: &str = "YAwS5AyAesmZzdAuOBbBykufIyVNQQunwIqfTTWJlu4";
const B: &str = "Lc6BitPRSkgpooTCxJPhIuQaLtngFLkWw5MpFl5l4HY";
const AGENT: &str = "omp-regression-agent";
const PROJECT: &str = "omp-regression-project";

fn rpc(rig: &Rig, method: &str, params: Value) -> Result<Value> {
    let result = crate::test_seam::scoped(crate::test_seam::Asserted::Operator, || {
        crate::client::rpc_private_timeout(&rig.state_dir, method, params, Duration::from_secs(30))
    });
    let text = match &result {
        Ok(value) => value.to_string(),
        Err(error) => error.to_string(),
    };
    assert!(
        !text.contains(A) && !text.contains(B),
        "public result exposed synthetic secret"
    );
    result
}

#[track_caller]
fn op(rig: &Rig, method: &str, params: Value) -> Value {
    rpc(rig, method, params).unwrap_or_else(|error| {
        panic!(
            "fixture {method} refused: kind={} code={}",
            error.kind(),
            error.code().unwrap_or("none")
        )
    })
}

fn start(encrypted: bool, token: bool) -> (tempfile::TempDir, Rig) {
    let root = tempfile::Builder::new()
        .prefix("omp1128-")
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in(std::env::temp_dir())
        .expect("private regression root");
    let mut opts = crate::daemon::ServeOptions::default();
    if token {
        crate::platform::agenticos_external::register_with_deployment_pin(
            &mut opts,
            "https://api.example.test",
            None,
        )
        .expect("token fixture registration");
    }
    let metadata =
        crate::platform::deployments::DeploymentMetadata::parse(br#"{"schema":1,"providers":[]}"#)
            .expect("isolated deployment metadata");
    let rig = Rig::start_with_platforms(
        root.path(),
        A.as_bytes(),
        B.as_bytes(),
        encrypted,
        opts.platforms,
        Some(metadata),
    )
    .expect("real regression Rig");
    if encrypted {
        for method in ["connection_custody_initialize", "connection_custody_unlock"] {
            op(&rig, method, json!({"accept_same_uid_risk":true}));
        }
    }
    (root, rig)
}

fn create(rig: &Rig, token: bool, account: &str) -> String {
    let params = if token {
        json!({"provider":"agenticos_external","account":account,"shape":"token",
            "token":A,"scopes":["provider.read"],"accept_same_uid_risk":true})
    } else {
        json!({"provider":"smtp","account":account,"shape":"smtp",
            "host":"localhost","port":rig.smtp_port,"tls_mode":"implicit",
            "username":"fixture-user","secret":A,"sender":"sender@example.test",
            "sender_name":"Regression Sender","scopes":["email:send"],
            "accept_same_uid_risk":true})
    };
    op(rig, "connection_create", params)["connection"]["id"]
        .as_str()
        .expect("created identity")
        .to_string()
}

fn grant(rig: &Rig, id: &str) {
    let shared = rig.shared();
    if shared
        .store
        .agent_opt(AGENT)
        .expect("agent lookup")
        .is_none()
    {
        shared
            .store
            .register_agent(&NewAgent {
                alias: AGENT,
                provider: "fake",
                endpoint_kind: "fake",
                role: "worker",
                cwd: rig.state_dir.to_str().expect("fixture path"),
                sandbox: "read-only",
                instructions: None,
                params: None,
                team_role: None,
                model_policy: None,
            })
            .expect("fixture requester");
    }
    let record = rig.core_record(id).expect("live credential");
    if record.platform == crate::platform::agenticos_external::PLATFORM {
        // Existing generic platform_grant RPC uses proto::identifier, which
        // cannot spell agenticos_external or its ws_ workspace accounts.
        // Seed this fixture grant through the real typed Store facade, as
        // existing external-provider fixtures do; all Connection mutations
        // and every no-change/retirement observation still use real state.
        shared
            .store
            .platform_grant_add(
                AGENT,
                &record.platform,
                &record.account,
                &record.scopes,
                "operator",
            )
            .expect("token fixture grant");
    } else {
        op(
            rig,
            "platform_grant",
            json!({"agent":AGENT,"platform":record.platform,
            "account":record.account,"scopes":record.scopes}),
        );
    }
    shared
        .store
        .platform_default_set(PROJECT, &record.platform, &record.account, "operator")
        .expect("fixture project default");
}

fn rotate(id: &str, token: bool, secret: &str) -> Value {
    if token {
        json!({"connection_id":id,"token":secret})
    } else {
        // Password-only inheritance is part of the approved protocol.
        json!({"connection_id":id,"secret":secret})
    }
}

fn expected(mut params: Value, id: &str, revision: u64) -> Value {
    params["expected_connection_id"] = json!(id);
    params["expected_revision"] = json!(revision);
    params
}

#[derive(PartialEq)]
struct Snapshot {
    authority: Value,
    grants: Vec<Value>,
    defaults: Vec<Value>,
    rows: Option<Vec<RowMetadata>>,
    smtp: SmtpCounts,
}

fn snapshot(rig: &Rig, id: &str, encrypted: bool) -> Snapshot {
    let shared = rig.shared();
    // Absence is data, not a failed observation: an erroneous revoke must
    // fail the unchanged-state assertion rather than a fixture unwrap.
    let authority = shared
        .store
        .connection_credential(id)
        .expect("snapshot credential")
        .map(|r| r.to_json())
        .unwrap_or(Value::Null);
    let grants = shared
        .store
        .platform_grants(Some(AGENT))
        .expect("snapshot grants")
        .iter()
        .map(|g| g.to_json())
        .collect();
    let defaults = shared
        .store
        .platform_defaults()
        .expect("snapshot defaults")
        .iter()
        .map(|d| {
            json!({"project":d.project,"platform":d.platform,
            "account":d.account,"set_at":d.set_at,"by":d.by})
        })
        .collect();
    Snapshot {
        authority,
        grants,
        defaults,
        rows: encrypted.then(|| rig.vault_rows(id).expect("snapshot immutable vault rows")),
        smtp: rig.smtp_barrier().expect("snapshot provider barrier"),
    }
}

fn refused_unchanged(rig: &Rig, id: &str, encrypted: bool, method: &str, params: Value) {
    let before = snapshot(rig, id, encrypted);
    let result = rpc(rig, method, params);
    let after = snapshot(rig, id, encrypted);
    assert!(
        after == before,
        "refused mutation changed authority, grants, defaults, vault or provider counts"
    );
    let error = result.expect_err("malformed or stale expectation was accepted");
    assert_eq!(
        error.kind(),
        "rejected",
        "transport, internal, busy or unknown outcome is not an expectation refusal"
    );
}

fn send(rig: &Rig) -> Result<Value> {
    rpc(
        rig,
        "crm_smtp_test_send",
        json!({"install_id":rig.install_id,
        "context_id":rig.context_id,"campaign_id":rig.campaign_id,
        "to_email":"recipient@example.test"}),
    )
}

fn sent_once(before: SmtpCounts, after: SmtpCounts, use_b: bool) {
    assert_eq!(after.tcp_sessions, before.tcp_sessions + 1);
    assert_eq!(after.accepted_messages, before.accepted_messages + 1);
    assert_eq!(after.auth_a, before.auth_a + u64::from(!use_b));
    assert_eq!(after.auth_b, before.auth_b + u64::from(use_b));
    assert_eq!(after.auth_other, before.auth_other);
}

#[test]
fn encrypted_rotation_show_retains_stale_binding_and_explicit_rebind_uses_new_secret() {
    let (_root, rig) = start(true, false);
    let id = create(&rig, false, "omp-show");
    grant(&rig, &id);
    let bound = op(
        &rig,
        "crm_smtp_bind",
        json!({"install_id":rig.install_id,
        "context_id":rig.context_id,"connection_id":id,"request_id":"omp-show-bind"}),
    );
    let link_revision = bound["binding"]["link_revision"]
        .as_i64()
        .expect("link revision");
    let before = rig.smtp_barrier().expect("positive send barrier");
    assert_eq!(
        send(&rig).expect("positive A send")["receipt"]["accepted"],
        true
    );
    sent_once(before, rig.smtp_barrier().expect("A completed"), false);

    op(
        &rig,
        "connection_rotate",
        expected(rotate(&id, false, B), &id, 1),
    );
    assert_eq!(
        rig.core_record(&id)
            .expect("rotated record")
            .credential_revision,
        2
    );
    let rotated = snapshot(&rig, &id, true);
    let denied = send(&rig);
    assert!(
        snapshot(&rig, &id, true) == rotated,
        "stale send changed state or reached SMTP"
    );
    let error = denied.expect_err("stale binding sent with the new credential");
    assert_eq!(error.kind(), "rejected", "not a stale-authority refusal");

    // Read the rebind CAS from show itself; a pre-rotation cached link
    // revision must not substitute for the management result under test.
    let shown = rpc(
        &rig,
        "crm_smtp_show",
        json!({"install_id":rig.install_id,
        "context_id":rig.context_id}),
    );
    assert!(
        snapshot(&rig, &id, true) == rotated,
        "show mutated authority or used provider"
    );
    let shown = shown.expect("rotated encrypted sender must remain inspectable for rebind");
    let binding = &shown["binding"];
    assert_eq!(binding["state"], "stale");
    assert_eq!(binding["connection_id"], id);
    assert_eq!(binding["auth_revision"], 1);
    assert_eq!(binding["link_revision"], link_revision);
    assert_eq!(binding["connection"]["revision"], 2);
    assert_eq!(binding["sender"]["address"], "sender@example.test");
    assert_eq!(binding["transport"]["port"], rig.smtp_port);
    let rebound = op(
        &rig,
        "crm_smtp_rebind",
        json!({"install_id":rig.install_id,
        "context_id":rig.context_id,"connection_id":id,
        "expected_revision":binding["link_revision"]}),
    );
    assert_eq!(rebound["binding"]["auth_revision"], 2);
    assert_eq!(rebound["binding"]["link_revision"], link_revision + 1);
    let before = rig.smtp_barrier().expect("B before");
    let receipt = send(&rig).expect("explicit rebind must use B");
    assert_eq!(receipt["receipt"]["accepted"], true);
    assert_eq!(receipt["receipt"]["auth_revision"], 2);
    sent_once(before, rig.smtp_barrier().expect("B completed"), true);
    rig.finish().expect("owned regression teardown");
}

fn expectation_matrix(encrypted: bool, token: bool) {
    let (_root, rig) = start(encrypted, token);
    let account = if token {
        "ws_550e8400-e29b-41d4-a716-446655440000"
    } else {
        "omp-cas"
    };
    let id = create(&rig, token, account);
    grant(&rig, &id);
    let fresh = rig.core_record(&id).expect("fresh record");
    assert_eq!(fresh.custody.starts_with("encrypted-sqlite-v1:"), encrypted);
    let before = rig.smtp_barrier().expect("rotation before");
    let original_grants = snapshot(&rig, &id, encrypted).grants;
    assert!(
        !original_grants.is_empty(),
        "positive control has no grant to preserve"
    );
    op(
        &rig,
        "connection_rotate",
        expected(rotate(&id, token, B), &id, 1),
    );
    assert_eq!(
        rig.core_record(&id)
            .expect("rotated record")
            .credential_revision,
        2
    );
    assert!(
        snapshot(&rig, &id, encrypted).grants == original_grants,
        "valid rotation changed existing grants"
    );
    assert_eq!(rig.smtp_barrier().expect("rotation after"), before);

    for method in ["connection_rotate", "connection_revoke"] {
        let base = if method == "connection_rotate" {
            rotate(&id, token, A)
        } else {
            json!({"connection_id":id})
        };
        refused_unchanged(&rig, &id, encrypted, method, expected(base.clone(), &id, 1));
        for value in [
            Value::Null,
            json!(true),
            json!(17),
            json!([id]),
            json!({"id":id}),
        ] {
            let mut params = expected(base.clone(), &id, 2);
            params["expected_connection_id"] = value;
            refused_unchanged(&rig, &id, encrypted, method, params);
        }
        for value in [
            Value::Null,
            json!("1"),
            json!(true),
            json!(-1),
            json!(0),
            json!(1.5),
            json!([1]),
            json!({"revision":1}),
            json!(i64::MAX as u64 + 1),
        ] {
            let mut params = expected(base.clone(), &id, 2);
            params["expected_revision"] = value;
            refused_unchanged(&rig, &id, encrypted, method, params);
        }
    }
    let revision = if encrypted {
        for (method, params) in [
            ("connection_rotate", rotate(&id, false, A)),
            ("connection_revoke", json!({"connection_id":id})),
        ] {
            refused_unchanged(&rig, &id, true, method, params);
        }
        // There is no supported encrypted token record to invent here.
        for value in [json!(2), json!("1"), Value::Null] {
            let mut params = expected(rotate(&id, true, A), &id, 2);
            params["expected_revision"] = value;
            refused_unchanged(&rig, &id, true, "connection_rotate", params);
        }
        2
    } else {
        op(&rig, "connection_rotate", rotate(&id, token, A));
        assert_eq!(
            rig.core_record(&id)
                .expect("omitted legacy rotate")
                .credential_revision,
            3
        );
        assert!(
            snapshot(&rig, &id, false).grants == original_grants,
            "omitted legacy rotation changed grants"
        );
        3
    };
    op(
        &rig,
        "connection_revoke",
        expected(json!({"connection_id":id}), &id, revision),
    );
    let shared = rig.shared();
    assert!(shared
        .store
        .connection_credential(&id)
        .expect("revoked identity")
        .is_none());
    assert!(shared
        .store
        .platform_grants(Some(AGENT))
        .expect("retired grants")
        .is_empty());
    assert!(shared
        .store
        .platform_default(PROJECT, &fresh.platform)
        .expect("retired default")
        .is_none());
    if !encrypted {
        let id = create(&rig, token, account);
        grant(&rig, &id);
        op(&rig, "connection_revoke", json!({"connection_id":id}));
        assert!(shared
            .store
            .connection_credential(&id)
            .expect("omitted legacy revoke")
            .is_none());
        assert!(shared
            .store
            .platform_grants(Some(AGENT))
            .expect("omitted revoke grants")
            .is_empty());
    }
    assert_eq!(
        rig.smtp_barrier()
            .expect("management-only provider barrier"),
        before
    );
    drop(shared);
    rig.finish().expect("owned regression teardown");
}

#[test]
fn legacy_smtp_expectations_reject_malformed_without_dropping_cas() {
    expectation_matrix(false, false);
}

#[test]
fn legacy_token_expectations_reject_malformed_without_dropping_cas() {
    expectation_matrix(false, true);
}

#[test]
fn encrypted_smtp_expectations_refuse_malformed_omitted_and_token_shape() {
    expectation_matrix(true, false);
}

#[test]
fn encrypted_smtp_raw_escaped_subject_refuses_before_provider_or_receipt_effects() {
    // Every eight-character window crosses a JSON-escaped quote or backslash.
    // Literal matching of serialized JSON therefore cannot stand in for
    // screening the raw subject that the SMTP encoder actually transmits.
    let secret = r#"q9"x1\k2"v3\p4"t5\z6"n7\h8"#;
    let characters: Vec<_> = secret.chars().collect();
    assert!(
        characters.len() >= 8,
        "fixture password must exercise fragment policy"
    );
    assert!(
        characters
            .windows(8)
            .all(|window| { window.iter().any(|ch| matches!(*ch, '\"' | '\\')) }),
        "every fixture password window must require JSON escaping"
    );
    let root = tempfile::Builder::new()
        .prefix("omp1128-raw-")
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in(std::env::temp_dir())
        .expect("private raw-input regression root");
    let rig = Rig::start(root.path(), secret.as_bytes(), B.as_bytes())
        .expect("real encrypted raw-input Rig");
    for method in ["connection_custody_initialize", "connection_custody_unlock"] {
        op(&rig, method, json!({"accept_same_uid_risk": true}));
    }
    let created = op(
        &rig,
        "connection_create",
        json!({"provider": "smtp", "account": "omp-raw-subject", "shape": "smtp",
            "host": "localhost", "port": rig.smtp_port, "tls_mode": "implicit",
            "username": "fixture-user", "secret": secret, "sender": "sender@example.test",
            "sender_name": "Regression Sender", "scopes": ["email:send"],
            "accept_same_uid_risk": true}),
    );
    let id = created["connection"]["id"]
        .as_str()
        .expect("created identity");
    assert!(
        rig.core_record(id)
            .expect("genuine encrypted credential")
            .custody
            .starts_with("encrypted-sqlite-v1:"),
        "raw-input control must use genuine encrypted custody"
    );
    op(
        &rig,
        "crm_smtp_bind",
        json!({"install_id": rig.install_id, "context_id": rig.context_id,
            "connection_id": id, "request_id": "omp-raw-subject-bind"}),
    );
    let before_control = rig.smtp_barrier().expect("safe-content provider barrier");
    let control = send(&rig).unwrap_or_else(|_| panic!("safe-content TLS control refused"));
    assert!(
        control["receipt"]["accepted"] == true,
        "safe-content control was not accepted"
    );
    let after_control = rig.smtp_barrier().expect("safe-content TLS completion");
    sent_once(before_control, after_control, false);

    // Fixture-owned content is seeded through the real typed RecordStore API,
    // not a fake adapter or a write into the renderer output. The real renderer
    // must return the raw password as its subject before the refusal is tested.
    let records = crate::store::app_records::RecordStore::open(&rig.state_dir, &rig.install_id)
        .expect("real content fixture store");
    let current = records
        .app_content_show(&rig.context_id, &rig.campaign_id)
        .unwrap_or_else(|_| panic!("current content fixture unavailable"));
    let revision = current["content"]["revision"]
        .as_i64()
        .expect("content fixture revision");
    let draft = crate::store::app_content::Draft::parse(
        secret,
        "Safe preheader",
        &[json!({"type": "paragraph", "text": "Safe body."})],
    )
    .unwrap_or_else(|_| panic!("raw-subject fixture must be valid content"));
    records
        .app_content_save(&rig.context_id, &rig.campaign_id, Some(revision), &draft)
        .unwrap_or_else(|_| panic!("typed raw-subject fixture save failed"));
    let rendered = records
        .app_content_verified_test_bytes(
            &rig.context_id,
            &rig.campaign_id,
            "Regression Sender",
            "sender@example.test",
        )
        .unwrap_or_else(|_| panic!("real raw-subject rendering failed"));
    assert!(
        rendered["subject"].as_str() == Some(secret),
        "real renderer must preserve the raw subject used by this counterexample"
    );
    let content_digest = rendered["content_digest"]
        .as_str()
        .expect("rendered content digest");
    let link_digest = control["receipt"]["link_digest"]
        .as_str()
        .expect("control link digest");
    assert!(
        !records
            .app_campaign_test_send_accepted(
                &rig.context_id,
                &rig.campaign_id,
                content_digest,
                link_digest,
            )
            .expect("pre-attempt receipt observation"),
        "new raw-subject content must have no prior accepted receipt"
    );
    let receipts = || -> i64 {
        records
            .conn()
            .query_row("SELECT count(*) FROM app_campaign_test_sends", [], |r| {
                r.get(0)
            })
            .expect("real receipt count")
    };
    let core = rusqlite::Connection::open_with_flags(
        &rig.core_db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .expect("read-only event observation");
    let events = || -> i64 {
        core.query_row("SELECT count(*) FROM events", [], |r| r.get(0))
            .expect("real core event count")
    };
    let receipts_before = receipts();
    let events_before = events();
    let provider_before = rig.smtp_barrier().expect("raw-subject pre-attempt barrier");
    let attempted = send(&rig);
    let provider_after = rig
        .smtp_barrier()
        .expect("raw-subject post-attempt barrier");
    let receipts_after = receipts();
    let events_after = events();
    let accepted_after = records
        .app_campaign_test_send_accepted(
            &rig.context_id,
            &rig.campaign_id,
            content_digest,
            link_digest,
        )
        .expect("post-attempt receipt observation");
    drop(core);
    drop(records);
    rig.finish().expect("owned raw-input regression teardown");

    assert!(
        provider_after == provider_before,
        "raw secret-bearing subject reached TCP, AUTH or DATA before refusal"
    );
    assert!(
        receipts_after == receipts_before && !accepted_after,
        "raw secret-bearing subject created accepted-send receipt evidence"
    );
    assert!(
        events_after == events_before,
        "raw secret-bearing subject emitted a new core event"
    );
    assert!(
        attempted.is_err(),
        "raw secret-bearing subject was accepted"
    );
}
