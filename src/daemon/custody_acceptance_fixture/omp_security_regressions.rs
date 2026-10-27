//! Independently authored OMP Spec/security regression for CAD-1128.
//! Mount as a test-only child of custody_acceptance_fixture on Linux with
//! test-seam. Uses real daemon RPC, real encrypted storage, real TLS SMTP,
//! read-only core observation and the real ordinary backup entry point.
//! The operator assertion is explicit fixture setup, not native identity proof.
//! Original r2: covered management/test-send paths never bind secret values
//! into core/app SQLite; new metadata is screened before core persistence.
//! No frozen driver, existing assertion or production guard is modified.

use super::Rig;
use crate::error::Result;
use rusqlite::{params, Connection, OpenFlags};
use serde_json::{json, Value};
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

const SECRET: &str = "q9x1k2v3z4n5m6r7";
const OTHER: &str = "J8w2c3f4h5p6t7y9";
const BENIGN_REQUEST: &str = "omp-safe-binding-control";

type LinkSnapshot = (String, i64, i64, String, String, String, f64, f64);

fn must<T, E>(result: std::result::Result<T, E>, stage: &str) -> T {
    result.unwrap_or_else(|_| panic!("{stage}"))
}

fn rpc(rig: &Rig, method: &str, params: Value) -> Result<Value> {
    crate::test_seam::scoped(crate::test_seam::Asserted::Operator, || {
        crate::client::rpc_private_timeout(&rig.state_dir, method, params, Duration::from_secs(30))
    })
}

fn core(rig: &Rig) -> Connection {
    must(
        Connection::open_with_flags(&rig.core_db_path, OpenFlags::SQLITE_OPEN_READ_ONLY),
        "read-only core observation setup failed",
    )
}

fn link(rig: &Rig) -> LinkSnapshot {
    must(
        core(rig).query_row(
            "SELECT connection_id,auth_revision,link_revision,state,digest,request_id,created,updated
             FROM crm_smtp_links WHERE install_id=?1 AND context_id=?2",
            params![rig.install_id, rig.context_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?,
                    r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?)),
        ),
        "real SMTP link observation failed",
    )
}

fn secret_events(rig: &Rig) -> i64 {
    must(
        core(rig).query_row(
            "SELECT count(*) FROM events WHERE instr(payload,?1)>0",
            [SECRET],
            |r| r.get(0),
        ),
        "real event confidentiality observation failed",
    )
}

fn bind(rig: &Rig, id: &str, request: &str) -> Result<Value> {
    rpc(
        rig,
        "crm_smtp_bind",
        json!({
            "install_id": rig.install_id, "context_id": rig.context_id,
            "connection_id": id, "request_id": request,
        }),
    )
}

#[test]
fn encrypted_smtp_binding_request_never_persists_plaintext_secret() {
    let root = must(
        tempfile::Builder::new()
            .prefix("omp-c1128-leak-")
            .tempdir_in(std::env::temp_dir()),
        "private fixture root creation failed",
    );
    must(
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)),
        "private fixture root permissions failed",
    );
    let rig = must(
        Rig::start(root.path(), SECRET.as_bytes(), OTHER.as_bytes()),
        "real encrypted fixture setup failed",
    );
    for method in ["connection_custody_initialize", "connection_custody_unlock"] {
        must(
            rpc(&rig, method, json!({"accept_same_uid_risk": true})),
            "explicit custody setup failed",
        );
    }
    must(
        crate::proto::identifier(SECRET, "synthetic binding request"),
        "synthetic secret must also be a valid request identifier",
    );
    let created = must(
        rpc(
            &rig,
            "connection_create",
            json!({
                "provider": "smtp", "account": "omp-metadata", "shape": "smtp",
                "host": "localhost", "port": rig.smtp_port, "tls_mode": "implicit",
                "username": "fixture-user", "secret": SECRET,
                "sender": "sender@example.test", "sender_name": "Fixture Sender",
                "scopes": ["email:send"], "accept_same_uid_risk": true,
            }),
        ),
        "real encrypted SMTP enrollment failed",
    );
    let id = created["connection"]["id"]
        .as_str()
        .expect("enrollment omitted Connection ID")
        .to_owned();
    assert!(
        must(rig.core_record(&id), "core credential observation failed")
            .custody
            .starts_with("encrypted-sqlite-v1:"),
        "positive control must use genuine encrypted custody"
    );

    // Working same-path control, including actual credential authentication.
    let bound = must(
        bind(&rig, &id, BENIGN_REQUEST),
        "benign binding control failed",
    );
    let revision = bound["binding"]["link_revision"]
        .as_i64()
        .expect("binding control omitted link revision");
    let counts_before = must(rig.smtp_barrier(), "pre-control SMTP barrier failed");
    let sent = must(
        rpc(
            &rig,
            "crm_smtp_test_send",
            json!({
                "install_id": rig.install_id, "context_id": rig.context_id,
                "campaign_id": rig.campaign_id, "to_email": "recipient@example.test",
            }),
        ),
        "genuine TLS SMTP control failed",
    );
    assert!(
        sent["receipt"]["accepted"] == true,
        "SMTP control was not accepted"
    );
    let counts = must(rig.smtp_barrier(), "post-control SMTP barrier failed");
    assert!(
        counts.tcp_sessions == counts_before.tcp_sessions + 1
            && counts.auth_a == counts_before.auth_a + 1
            && counts.accepted_messages == counts_before.accepted_messages + 1
            && counts.auth_b == counts_before.auth_b
            && counts.auth_other == counts_before.auth_other,
        "working control did not observe exactly one A-authenticated real send"
    );
    must(
        rpc(
            &rig,
            "crm_smtp_revoke",
            json!({
                "install_id": rig.install_id, "context_id": rig.context_id,
                "expected_revision": revision,
            }),
        ),
        "link retirement before fresh-bind case failed",
    );
    let prior = link(&rig);
    assert!(
        prior.3 == "revoked" && prior.5 == BENIGN_REQUEST,
        "negative setup must leave a genuine revoked link with a benign request"
    );
    assert!(
        secret_events(&rig) == 0,
        "setup already leaked the synthetic secret"
    );

    // The request is syntactically valid. Only secret-aware admission may
    // refuse it. No production screening function is used as the oracle.
    let attempted = bind(&rig, &id, SECRET);
    let after = link(&rig);
    let leaked_in_events = secret_events(&rig) != 0;
    let public_text = match &attempted {
        Ok(value) => value.to_string(),
        Err(error) => error.to_string(),
    };
    let traffic_after = must(rig.smtp_barrier(), "negative SMTP barrier failed");
    let backup = must(
        crate::backup::backup(&rig.state_dir, &root.path().join("artifact"), 2, "manual"),
        "real ordinary backup observation failed",
    );
    let artifact_db = backup["db"]
        .as_str()
        .expect("backup omitted actual database path");
    let artifact = must(
        std::fs::read(artifact_db),
        "produced backup byte observation failed",
    );
    let leaked_in_artifact = artifact
        .windows(SECRET.len())
        .any(|window| window == SECRET.as_bytes());
    must(rig.finish(), "owned fixture shutdown failed");

    assert!(
        after == prior,
        "secret-bearing request changed the real durable SMTP binding row"
    );
    assert!(
        !leaked_in_artifact,
        "plaintext SMTP secret reached the actual ordinary backup database"
    );
    assert!(
        !leaked_in_events,
        "plaintext SMTP secret reached a durable public event"
    );
    assert!(
        !public_text.contains(SECRET),
        "public binding response echoed the secret"
    );
    assert!(
        attempted.is_err(),
        "secret-bearing binding request was accepted"
    );
    assert!(
        traffic_after == counts,
        "binding admission started unexpected SMTP traffic"
    );
}

fn uncertainty_send(rig: &Rig) -> Result<Value> {
    rpc(
        rig,
        "crm_smtp_test_send",
        json!({
            "install_id": rig.install_id, "context_id": rig.context_id,
            "campaign_id": rig.campaign_id, "to_email": "recipient@example.test",
        }),
    )
}

fn smtp_rebound_events(rig: &Rig) -> i64 {
    must(
        core(rig).query_row(
            "SELECT count(*) FROM events WHERE alias=?1 AND kind=?2",
            params!["crm_smtp", "crm_smtp_rebound"],
            |row| row.get(0),
        ),
        "real SMTP rebound event observation failed",
    )
}

/// Real SQLite COMMIT refusal must fence subsequent encrypted consumer use.
/// The targeted test-only hook vetoes a genuine guarded transaction; it does
/// not manufacture an Error, replace the authorizer, alter custody flags or
/// provide a receipt. Native operator identity is explicitly simulated here.
#[test]
fn encrypted_smtp_uncertain_link_commit_fences_until_explicit_reconciliation() {
    let root = must(
        tempfile::Builder::new()
            .prefix("omp-c1128-commit-")
            .tempdir_in(std::env::temp_dir()),
        "private uncertainty fixture root creation failed",
    );
    must(
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)),
        "private uncertainty fixture root permissions failed",
    );
    let rig = must(
        Rig::start(root.path(), SECRET.as_bytes(), OTHER.as_bytes()),
        "real uncertainty fixture setup failed",
    );
    for method in ["connection_custody_initialize", "connection_custody_unlock"] {
        must(
            rpc(&rig, method, json!({"accept_same_uid_risk": true})),
            "explicit uncertainty custody setup failed",
        );
    }
    let created = must(
        rpc(
            &rig,
            "connection_create",
            json!({
                "provider": "smtp", "account": "omp-commit", "shape": "smtp",
                "host": "localhost", "port": rig.smtp_port, "tls_mode": "implicit",
                "username": "fixture-user", "secret": SECRET,
                "sender": "sender@example.test", "sender_name": "Fixture Sender",
                "scopes": ["email:send"], "accept_same_uid_risk": true,
            }),
        ),
        "real uncertainty SMTP enrollment failed",
    );
    let id = created["connection"]["id"]
        .as_str()
        .expect("uncertainty enrollment omitted Connection ID")
        .to_owned();
    let authority = must(
        rig.core_record(&id),
        "initial uncertainty authority read failed",
    );
    assert!(
        authority.custody.starts_with("encrypted-sqlite-v1:"),
        "uncertainty control must use genuine encrypted custody"
    );
    let bound = must(
        bind(&rig, &id, "omp-commit-safe-bind"),
        "real uncertainty binding control failed",
    );
    let revision = bound["binding"]["link_revision"]
        .as_i64()
        .expect("uncertainty binding omitted link revision");
    let first_before = must(
        rig.smtp_barrier(),
        "initial uncertainty SMTP barrier failed",
    );
    let accepted = must(
        uncertainty_send(&rig),
        "genuine initial A/TLS uncertainty control failed",
    );
    assert!(
        accepted["receipt"]["accepted"] == true,
        "initial uncertainty control was not accepted"
    );
    let first_after = must(rig.smtp_barrier(), "initial uncertainty SMTP drain failed");
    assert!(
        first_after.tcp_sessions == first_before.tcp_sessions + 1
            && first_after.auth_a == first_before.auth_a + 1
            && first_after.accepted_messages == first_before.accepted_messages + 1
            && first_after.auth_b == first_before.auth_b
            && first_after.auth_other == first_before.auth_other,
        "initial control must observe exactly one real A-authenticated accepted send"
    );

    let link_before = link(&rig);
    let events_before = smtp_rebound_events(&rig);
    let shared = rig.shared();
    let probe = must(
        shared
            .store
            .cad1128_refuse_next_smtp_link_commit(&rig.install_id, &rig.context_id),
        "targeted actual-core COMMIT hook setup failed",
    );
    let attempted = rpc(
        &rig,
        "crm_smtp_rebind",
        json!({
            "install_id": rig.install_id, "context_id": rig.context_id,
            "connection_id": id, "expected_revision": revision,
        }),
    );
    // Disarm before any assertion/next operation; never leave a delayed veto.
    must(
        shared.store.cad1128_clear_smtp_commit_probe(),
        "actual core handle did not recover after real COMMIT refusal",
    );
    let observed = probe.counts();
    assert!(
        observed.target_updates == 1
            && observed.refused_commits == 1
            && observed.rollbacks >= 1
            && !observed.armed,
        "fault did not reach exactly the target SMTP-link transaction and rollback"
    );
    let error = match attempted {
        Err(error) => error,
        Ok(_) => panic!("actual SQLite COMMIT veto did not refuse rebind"),
    };
    assert!(
        error.code() == Some("outcome_unknown"),
        "actual core COMMIT failure did not reach the typed uncertainty boundary"
    );
    assert!(
        link(&rig) == link_before && smtp_rebound_events(&rig) == events_before,
        "SQLite-refused rebind did not roll back actual link and event state"
    );
    assert!(
        must(rig.core_record(&id), "post-fault authority read failed").to_json()
            == authority.to_json(),
        "COMMIT fault changed credential authority"
    );
    // Exercise the same guarded writer after the one-shot fault, not merely
    // a fresh sibling connection. Safe core-only work remains possible.
    must(
        shared.store.event_public(
            "omp-core-control",
            "core_after_commit_refusal",
            json!({"control": true}),
        ),
        "actual guarded core writer is unusable after COMMIT refusal",
    );
    let blocked_before = must(rig.smtp_barrier(), "pre-fence traffic barrier failed");
    assert!(
        blocked_before == first_after,
        "SMTP rebind or core usability control unexpectedly contacted the provider"
    );
    let blocked = uncertainty_send(&rig);
    let blocked_after = must(rig.smtp_barrier(), "post-fence traffic barrier failed");
    let status = must(
        rpc(&rig, "connection_custody_status", json!({})),
        "post-fault custody status failed",
    );
    let remained_locked = status["custody"]["unlocked"] == false;
    let safely_refused = blocked.is_err();
    let public_text = match blocked {
        Ok(value) => value.to_string(),
        Err(error) => error.to_string(),
    };

    must(
        rpc(&rig, "connection_custody_lock", json!({})),
        "explicit lock before reconciliation failed",
    );
    must(
        rpc(
            &rig,
            "connection_custody_unlock",
            json!({"accept_same_uid_risk": true}),
        ),
        "explicit current-authority reconciliation/unlock failed",
    );
    let resumed = must(
        uncertainty_send(&rig),
        "real A/TLS send after reconciliation failed",
    );
    let resumed_after = must(rig.smtp_barrier(), "reconciled SMTP drain failed");
    assert!(
        resumed["receipt"]["accepted"] == true
            && resumed_after.tcp_sessions == blocked_after.tcp_sessions + 1
            && resumed_after.auth_a == blocked_after.auth_a + 1
            && resumed_after.accepted_messages == blocked_after.accepted_messages + 1
            && resumed_after.auth_b == blocked_after.auth_b
            && resumed_after.auth_other == blocked_after.auth_other,
        "explicit reconciliation did not restore exactly one genuine A/TLS send"
    );
    drop(shared);
    must(rig.finish(), "owned uncertainty fixture shutdown failed");

    assert!(
        blocked_after == blocked_before,
        "uncertain SMTP link commit admitted provider traffic before explicit reconciliation"
    );
    assert!(
        safely_refused && remained_locked,
        "uncertain SMTP link commit left encrypted consumer use admitted"
    );
    assert!(
        !public_text.contains(SECRET),
        "post-uncertainty public result disclosed synthetic credential"
    );
}
