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
