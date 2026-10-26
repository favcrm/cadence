// Independently authored and owned by cad1085-security; see ownership.md.
// CAD-1128 contract sha256:
// 3b17bc61f057d992ed2a423887d458002b75b4c8f6a8fa54d9d49ba7794167e8
// Missing fixture/API is compilation pending, never behavioral red evidence.
// Include unchanged as crate::daemon::custody_security_r2 (unit-test scope).

#[cfg(not(all(unix, feature = "test-seam")))]
compile_error!("CAD-1128 independent acceptance requires Unix and test-seam; do not skip it");

use crate::daemon::custody_acceptance_fixture::{Rig, SmtpCounts};
use crate::error::{Error, Result};
use rusqlite::{params, Connection, OpenFlags};
use serde_json::{json, Value};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

const A: &str = "p7N4x9Q2v6R8m3K5t1W0y4H7c8L2z9F6";
const B: &str = "s5D8u2J9a4V7e1G6b3T0k8P5n9C2r4X7";

// Fixed diagnostics deliberately omit raw errors, credentials and JSON values.
fn must<T, E>(result: std::result::Result<T, E>, stage: &'static str) -> T {
    match result {
        Ok(value) => value,
        Err(_) => panic!("{stage}"),
    }
}

fn safe_json(value: &Value) {
    let rendered = value.to_string();
    assert!(
        !rendered.contains(A) && !rendered.contains(B),
        "a public RPC result disclosed a synthetic credential"
    );
}

fn rpc(rig: &Rig, method: &str, params: Value) -> Result<Value> {
    let answer = crate::test_seam::scoped(crate::test_seam::Asserted::Operator, || {
        // Never follow CADENCE_SOCKET or clear native identity variables.
        crate::client::rpc_private_timeout(&rig.state_dir, method, params, Duration::from_secs(30))
    });
    match &answer {
        Ok(value) => safe_json(value),
        Err(error) => {
            let rendered = error.to_string();
            assert!(
                !rendered.contains(A) && !rendered.contains(B),
                "a public RPC error disclosed a synthetic credential"
            );
        }
    }
    answer
}

fn refused(answer: Result<Value>, stage: &'static str) -> Error {
    match answer {
        Err(error) => error,
        Ok(_) => panic!("{stage}"),
    }
}

fn expected_integrity_refusal(error: &Error) {
    // Existing brokers may wrap a typed custody failure in safe Rejected.
    // Transport/setup/Internal/Provider/Unknown failures are never this proof.
    assert!(
        matches!(error.code(), Some("integrity_failure" | "stale_revision"))
            || (error.code().is_none() && error.kind() == "rejected"),
        "substitution did not produce an integrity/staleness refusal"
    );
}

fn core_writer(path: &Path) -> Connection {
    must(
        Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        ),
        "fixture core database could not be opened without CREATE",
    )
}

fn replace_reference(rig: &Rig, connection_id: &str, reference: &str) {
    let db = core_writer(&rig.core_db_path);
    must(
        db.busy_timeout(Duration::from_secs(5)),
        "fixture core corruption writer timeout setup failed",
    );
    let changed = must(
        db.execute(
            "UPDATE platform_credentials SET custody=?1 WHERE connection_id=?2",
            params![reference, connection_id],
        ),
        "fixture reference substitution failed",
    );
    assert!(changed == 1, "fixture must substitute exactly one core row");
    assert!(db.is_autocommit(), "fixture substitution did not commit");
}

fn rotation(rig: &Rig, id: &str, revision: u64) -> Value {
    json!({
        "connection_id": id,
        "expected_connection_id": id,
        "expected_revision": revision,
        "host": "localhost", "port": rig.smtp_port, "tls_mode": "implicit",
        "username": "fixture-user", "secret": B,
        "sender": "sender@example.test", "sender_name": "Fixture Sender",
        "scopes": ["email:send"], "accept_same_uid_risk": true
    })
}

fn send(rig: &Rig) -> Result<Value> {
    rpc(
        rig,
        "crm_smtp_test_send",
        json!({
            "install_id": rig.install_id, "context_id": rig.context_id,
            "campaign_id": rig.campaign_id, "to_email": "recipient@example.test"
        }),
    )
}

fn accepted_send(rig: &Rig, id: &str, revision: u64, use_b: bool) {
    let before = must(rig.smtp_barrier(), "SMTP observation barrier failed");
    let reply = must(send(rig), "authorized real SMTP test-send failed");
    let receipt = &reply["receipt"];
    assert!(
        receipt["accepted"] == true && receipt["pending_approval"] == false,
        "SMTP happy control did not report acceptance"
    );
    assert!(
        receipt["connection_id"] == id && receipt["auth_revision"].as_u64() == Some(revision),
        "SMTP receipt did not describe the exact core authority"
    );
    let after = must(rig.smtp_barrier(), "SMTP completion barrier failed");
    assert!(
        after.tcp_sessions == before.tcp_sessions + 1
            && after.accepted_messages == before.accepted_messages + 1
            && after.auth_a == before.auth_a + u64::from(!use_b)
            && after.auth_b == before.auth_b + u64::from(use_b)
            && after.auth_other == before.auth_other,
        "SMTP fixture observed traffic under the wrong credential or an extra send"
    );
}

fn no_traffic(rig: &Rig, before: SmtpCounts) {
    let after = must(
        rig.smtp_barrier(),
        "SMTP refusal observation barrier failed",
    );
    assert!(
        after == before,
        "a denied operation started SMTP fixture traffic"
    );
}

fn explicit_reconciliation(rig: &Rig) {
    // Permit a manager to fence after failure. This is a fresh deliberate
    // inspection/unlock, never automatic recovery or publication of an orphan.
    must(
        rpc(rig, "connection_custody_lock", json!({})),
        "explicit custody lock before reconciliation failed",
    );
    must(
        rpc(
            rig,
            "connection_custody_unlock",
            json!({"accept_same_uid_risk": true}),
        ),
        "explicit custody reconciliation/unlock failed",
    );
}

#[test]
fn connection_custody_never_executes_orphan_or_substituted_revision() {
    let root = must(
        tempfile::Builder::new()
            .prefix("c1128-sec-")
            .tempdir_in(std::env::temp_dir()),
        "private acceptance root creation failed",
    );
    must(
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)),
        "private acceptance root permissions failed",
    );
    let rig = must(
        Rig::start(root.path(), A.as_bytes(), B.as_bytes()),
        "real isolated rig setup failed",
    );
    for path in [&rig.state_dir, &rig.core_db_path] {
        let canonical = must(path.canonicalize(), "fixture path validation failed");
        assert!(
            canonical.starts_with(root.path()),
            "fixture path escaped the owned root"
        );
    }

    // Owner-provisioned key/config are setup only; real RPC does init/unlock.
    must(
        rpc(
            &rig,
            "connection_custody_initialize",
            json!({"accept_same_uid_risk": true}),
        ),
        "real explicit custody initialization failed",
    );
    must(
        rpc(
            &rig,
            "connection_custody_unlock",
            json!({"accept_same_uid_risk": true}),
        ),
        "real explicit custody unlock failed",
    );
    let created = must(
        rpc(
            &rig,
            "connection_create",
            json!({
                "provider": "smtp", "account": "security-r2", "shape": "smtp",
                "host": "localhost", "port": rig.smtp_port, "tls_mode": "implicit",
                "username": "fixture-user", "secret": A,
                "sender": "sender@example.test", "sender_name": "Fixture Sender",
                "scopes": ["email:send"], "accept_same_uid_risk": true
            }),
        ),
        "real encrypted Connection enrollment failed",
    );
    let id = match created["connection"]["id"].as_str() {
        Some(id) => id.to_owned(),
        None => panic!("Connection create omitted its identity"),
    };
    let initial = must(rig.core_record(&id), "initial core authority read failed");
    assert!(
        initial.connection_id == id && initial.credential_revision == 1,
        "initial authority was wrong"
    );
    assert!(
        initial.custody.starts_with("encrypted-sqlite-v1:"),
        "happy path used legacy custody"
    );
    let rows = must(rig.vault_rows(&id), "initial vault inspection failed");
    assert!(
        rows.len() == 1 && rows[0].reference == initial.custody,
        "initial exact row was absent"
    );
    let bound = must(
        rpc(
            &rig,
            "crm_smtp_bind",
            json!({
                "install_id": rig.install_id, "context_id": rig.context_id,
                "connection_id": id, "request_id": "independent-r2-bind"
            }),
        ),
        "real encrypted SMTP binding failed",
    );
    let link_revision = match bound["binding"]["link_revision"].as_i64() {
        Some(revision) if revision > 0 => revision,
        _ => panic!("SMTP binding omitted its revision"),
    };
    accepted_send(&rig, &id, initial.credential_revision, false);

    // Core WAL readers remain available while this genuine writer prevents
    // the later publication BEGIN. Dedicated vault commit is independent.
    let blocker = core_writer(&rig.core_db_path);
    must(
        blocker.execute_batch("BEGIN IMMEDIATE"),
        "core publication blocker setup failed",
    );
    assert!(
        !blocker.is_autocommit(),
        "core publication blocker was not acquired"
    );
    let failed = rpc(
        &rig,
        "connection_rotate",
        rotation(&rig, &id, initial.credential_revision),
    );
    must(
        blocker.execute_batch("ROLLBACK"),
        "core publication blocker release failed",
    );
    assert!(
        blocker.is_autocommit(),
        "core publication blocker remained active"
    );
    drop(blocker);
    let failed = refused(
        failed,
        "rotation published despite the real core writer lock",
    );
    assert!(
        failed.kind() == "busy" || matches!(failed.code(), Some("busy" | "storage_unavailable")),
        "publication fixture did not produce a known storage/busy refusal"
    );
    let unchanged = must(
        rig.core_record(&id),
        "post-failure core authority read failed",
    );
    assert!(
        unchanged.to_json() == initial.to_json(),
        "failed publication changed core authority"
    );
    explicit_reconciliation(&rig);
    let rows = must(rig.vault_rows(&id), "post-failure vault inspection failed");
    assert!(
        rows.len() == 2,
        "fixture did not retain exactly one unpublished attempted row"
    );
    let orphan = match rows.iter().find(|row| row.reference != initial.custody) {
        Some(row) => row,
        None => panic!("authentic unpublished B row was not created"),
    };
    assert!(
        orphan.workspace_id == rig.workspace_id
            && orphan.connection_id == id
            && orphan.provider == initial.platform
            && orphan.account == initial.account
            && orphan.exchange == initial.exchange
            && orphan.credential_revision == initial.credential_revision + 1,
        "unpublished row did not describe the real proposed next identity"
    );
    {
        // Diagnostic ONLY: actual AEAD/storage path, stored identity, no use
        // witness/provider call. This establishes that B is not garbage data.
        let envelope = must(
            rig.read_row_using_its_stored_identity(orphan),
            "orphan B did not authenticate through the real vault",
        );
        assert!(
            envelope.secret() == B.as_bytes(),
            "orphan did not contain authentic B"
        );
    }
    // An authentic later row cannot replace core's exact authorized A.
    accepted_send(&rig, &id, initial.credential_revision, false);

    // Corrupt only the locator, leaving core identity/revision/fingerprint and
    // link/context intact. Real supported dispatch must reject the valid B.
    replace_reference(&rig, &id, &orphan.reference);
    let corrupted = must(rig.core_record(&id), "substituted core inspection failed");
    let mut expected_corruption = initial.to_json();
    expected_corruption["custody"] = json!(orphan.reference);
    assert!(
        corrupted.to_json() == expected_corruption,
        "fixture changed more than the reference"
    );
    let before = must(rig.smtp_barrier(), "pre-substitution SMTP barrier failed");
    let denied = refused(
        send(&rig),
        "substituted authentic B was accepted by SMTP dispatch",
    );
    expected_integrity_refusal(&denied);
    no_traffic(&rig, before);
    replace_reference(&rig, &id, &initial.custody);
    assert!(
        must(rig.core_record(&id), "restored core inspection failed").to_json()
            == initial.to_json(),
        "fixture did not restore exact original authority"
    );
    explicit_reconciliation(&rig);
    accepted_send(&rig, &id, initial.credential_revision, false);

    // Fresh explicit attempt gets a different immutable locator at the same
    // proposed revision. The old SMTP link must remain stale until rebind.
    must(
        rpc(
            &rig,
            "connection_rotate",
            rotation(&rig, &id, initial.credential_revision),
        ),
        "fresh explicit rotation after known publication rollback failed",
    );
    let current = must(rig.core_record(&id), "published B authority read failed");
    assert!(
        current.connection_id == id
            && current.credential_revision == initial.credential_revision + 1
            && current.custody != initial.custody
            && current.custody != orphan.reference,
        "fresh rotation reused an orphan or failed to publish the intended revision"
    );
    let still_present = must(
        rig.vault_rows(&id),
        "immutable orphan retention inspection failed",
    );
    assert!(
        still_present.iter().any(|row| row == orphan),
        "later rotation rewrote/deleted its orphan"
    );
    let before = must(rig.smtp_barrier(), "pre-stale-link SMTP barrier failed");
    let denied = refused(send(&rig), "stale SMTP authorization revision was accepted");
    expected_integrity_refusal(&denied);
    no_traffic(&rig, before);
    explicit_reconciliation(&rig);
    must(
        rpc(
            &rig,
            "crm_smtp_rebind",
            json!({
                "install_id": rig.install_id, "context_id": rig.context_id,
                "connection_id": id, "expected_revision": link_revision
            }),
        ),
        "explicit rebind to published B failed",
    );
    // B is genuinely accepted by the same TLS server once core AND link
    // authorize it; earlier negatives cannot be explained by rejecting B.
    accepted_send(&rig, &id, current.credential_revision, true);
    must(rig.finish(), "owned fixture shutdown/drain failed");
}
