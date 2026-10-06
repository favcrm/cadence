//! CAD-1156 bounded additional I3/I4/I6/I7 real-release acceptance checks.
//! Retains sealed stage5 and nine-scenario downstream evidence unchanged.
#![cfg(all(test, unix, feature = "test-seam"))]
#[path = "cad1156_boundary_adapter.rs"]
mod adapter;
#[path = "cad1156_boundary_fixture.rs"]
mod fixture;
use adapter::BoundaryAdapter;
use fixture::Fx;
use serde_json::{json, Value};

const PASSWORD: &str = "cad1156-smtp-a";
// Non-JSON coherent corrupt custody. Unlike the canonical framing, a raw-byte
// fallback does NOT happen to refuse our clean publication on this value.
const BAD: &[u8] = b"zzvkrqptnxwls";
fn checked<T, E>(result: std::result::Result<T, E>, message: &'static str) -> T {
    match result {
        Ok(value) => value,
        Err(_) => panic!("{message}"),
    }
}
fn required<T>(value: Option<T>, message: &'static str) -> T {
    match value {
        Some(value) => value,
        None => panic!("{message}"),
    }
}
#[derive(Default)]
struct ClaimObservation {
    decisions: u64,
    claims: u64,
    decided: Option<Value>,
    claimed: Option<Value>,
}

/// Adversarial corruption ONLY in the private fixture, after the REAL persisted
/// decision and before execute's locks/rechecks. Fingerprint coherence isolates
/// the canonical decoder from an earlier fingerprint/binding refusal. No effect,
/// approval, material, revision or authority row is forged; no production seam.
fn corrupt_after_decision(shared: &crate::daemon::Shared, row: &crate::store::EffectRow) {
    let _guard = shared
        .platform_custody_lock
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let record = required(
        checked(
            shared
                .store
                .platform_credential(&row.platform, &row.account),
            "corruption fixture record read failed",
        ),
        "corruption fixture record missing",
    );
    assert!(
        record.platform == "smtp"
            && record.exchange == "smtp"
            && matches!(
                record.custody.as_str(),
                crate::platform::custody::FILE_TAG | crate::platform::custody::LIBSECRET_TAG
            ),
        "corruption fixture must target the actual legacy SMTP record"
    );
    checked(
        shared.platform_custody.put(
            &crate::platform::custody::Key {
                platform: &record.platform,
                account: &record.account,
            },
            BAD,
        ),
        "corruption fixture custody write failed",
    );
    let changed = checked(shared.store.fixture_write(|tx| tx.execute(
        "UPDATE platform_credentials SET fingerprint=?1 WHERE platform=?2 AND account=?3 AND connection_id=?4 AND credential_revision=?5",
        rusqlite::params![crate::secret::fingerprint(BAD), record.platform, record.account,
            record.connection_id, record.credential_revision],
    )), "coherent corruption fixture update failed");
    assert!(
        changed == 1,
        "coherent corruption must update only the targeted credential fingerprint"
    );
    let bytes = zeroize::Zeroizing::new(checked(
        crate::platform::load_credential(
            &shared.store,
            &shared.platform_custody,
            &row.platform,
            &row.account,
        ),
        "coherently corrupted credential must remain fingerprint-authenticated",
    ));
    assert!(
        bytes.as_slice() == BAD && crate::platform::smtp::custody_decode(&bytes).is_err(),
        "corruption must specifically break canonical SMTP validation, not the default loader"
    );
}

fn authenticated<T>(
    fx: &Fx,
    connection: &str,
    f: impl FnOnce(&crate::store::CredentialRecord, &[u8]) -> T,
) -> T {
    let guard = fx
        .shared
        .platform_custody_lock
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let record = required(
        checked(
            fx.shared.store.connection_credential(connection),
            "authenticated fixture record read failed",
        ),
        "authenticated fixture record missing",
    );
    let bytes = zeroize::Zeroizing::new(checked(
        crate::platform::load_credential(
            &fx.shared.store,
            &fx.shared.platform_custody,
            &record.platform,
            &record.account,
        ),
        "authenticated fixture custody load failed",
    ));
    let result = f(&record, &bytes);
    drop(bytes);
    drop(guard);
    result
}

struct Counts {
    rows: i64,
    authorities: i64,
    events: i64,
    requested: i64,
    decided: i64,
}
fn counts(fx: &Fx, request: &str, effect: Option<&str>) -> Counts {
    let conn = checked(
        rusqlite::Connection::open_with_flags(
            fx.dir.path().join("cadence.sqlite3"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        ),
        "read-only refusal observation failed",
    );
    let (rows, authorities) = checked(conn.query_row(
        "SELECT (SELECT COUNT(*) FROM platform_effects),(SELECT COUNT(*) FROM app_effect_authorizations)",
        [], |row| Ok((row.get(0)?, row.get(1)?))), "release row counts failed");
    let (events, requested, decided) = checked(conn.query_row(
        "SELECT COUNT(*),COALESCE(SUM(kind=?4),0),COALESCE(SUM(kind=?5),0) FROM events WHERE alias=?1 AND (json_extract(payload,'$.request')=?2 OR json_extract(payload,'$.effect_id')=?3)",
        rusqlite::params![crate::store::PLATFORM_STREAM, request, effect,
            crate::store::EFFECT_REQUESTED_EVENT, crate::store::EFFECT_DECIDED_EVENT],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))), "release event counts failed");
    Counts {
        rows,
        authorities,
        events,
        requested,
        decided,
    }
}
fn request(install: &str) -> String {
    format!(
        "app-release-{}",
        uuid::Uuid::new_v5(
            &uuid::Uuid::NAMESPACE_OID,
            format!("{install}:release-boundary").as_bytes()
        )
        .simple()
    )
}
fn material(fx: &Fx, connection: &str, title: &str) -> (Value, String) {
    let (install, bundle) = fx.install_social(connection);
    let (run, artifact) = fx.publication_run(&install, &bundle);
    (
        json!({"run_id":run,"artifact_id":artifact,"slot":"publication",
        "request_id":"release-boundary","title":title}),
        request(&install),
    )
}
fn assert_empty_stage(fx: &Fx, request: &str) {
    // Check actual forbidden persistence/callbacks before error matching.
    let observed = counts(fx, request, None);
    assert!(
        observed.rows == 0 && observed.authorities == 0 && observed.events == 0,
        "stage refusal must leave no effect, publication authority or request audit event"
    );
    assert!(
        fx.adapter.count() == 0 && fx.adapter.stage.execute_count() == 0,
        "stage refusal must never call either provider execution hook"
    );
    let claims = fx.claims.lock().unwrap_or_else(|p| p.into_inner());
    assert!(
        claims.decisions == 0 && claims.claims == 0,
        "stage refusal must not create a decision or execution claim"
    );
}
fn assert_fixed_refusal(result: crate::Result<Value>, expected: crate::Error, secret: &[u8]) {
    match result {
        Err(error) => {
            let text = zeroize::Zeroizing::new(error.to_string());
            // No-echo before classification; do not apply this as a whole-final
            // outcome property to short passwords coincident with constants.
            assert!(
                crate::platform::refuse_leak("diagnostic", &text, secret).is_ok(),
                "refusal must not echo actual sensitive bytes or windows"
            );
            assert!(
                error.kind() == expected.kind(),
                "refusal must retain the exact reviewed wire error kind"
            );
            let expected = zeroize::Zeroizing::new(expected.to_string());
            assert!(
                *text == *expected,
                "refusal must be the exact reviewed fixed Error message"
            );
        }
        Ok(_) => panic!("forbidden release unexpectedly returned success"),
    }
}

#[test]
fn smtp_malformed_typed_stage_refuses_before_rows_without_raw_byte_fallback() {
    let fx = Fx::new(BoundaryAdapter::new(false));
    let connection = fx.enroll_bytes("boundary", BAD, "smtp");
    let (params, req) = material(&fx, &connection, "Reviewed caption");
    let result = fx.call("app_effect_stage", params);
    assert_empty_stage(&fx, &req);
    authenticated(&fx, &connection, |record, bytes| {
        assert!(
            record.platform == "smtp"
                && record.exchange == "smtp"
                && matches!(
                    record.custody.as_str(),
                    crate::platform::custody::FILE_TAG | crate::platform::custody::LIBSECRET_TAG
                )
                && bytes == BAD
                && crate::platform::smtp::custody_decode(bytes).is_err(),
            "malformed case must be an authenticated classified-SMTP decode failure"
        );
        // Preparation need not precede decode in every valid implementation.
        // If it was reached, observe the REAL input rather than fabricate it.
        let (input, preview) = fx.adapter.stage.last_prepared();
        if let (Some(input), Some(preview)) = (input, preview) {
            let serialized = zeroize::Zeroizing::new(input.to_string());
            assert!(
                crate::platform::refuse_leak("diagnostic", &serialized, bytes).is_ok()
                    && crate::platform::refuse_leak("diagnostic", &preview, bytes).is_ok(),
                "raw-byte fallback must not accidentally mask this malformed decode counterexample"
            );
        }
    });
    assert_fixed_refusal(
        result,
        crate::Error::rejected("app release credential is invalid — withheld"),
        BAD,
    );
}

#[test]
fn smtp_malformed_typed_accept_retains_prior_decision_without_claim_call_or_outcome() {
    let fx = Fx::corrupting_after_decision(BoundaryAdapter::new(false));
    let connection = fx.enroll_smtp("boundary", PASSWORD);
    let (params, req) = material(&fx, &connection, "Reviewed caption");
    let staged = fx.op("app_effect_stage", params);
    assert!(
        staged["effect"]["state"] == "waiting",
        "accept-corruption setup must stage an eligible actual artifact"
    );
    let id = required(
        staged["effect"]["effect_id"].as_str(),
        "staged effect handle missing",
    );
    let result = fx.call(
        "app_effect_decide",
        json!({"effect_id":id,
        "digest":staged["effect"]["digest"],"decision":"accept"}),
    );
    // BEFORE matching Err: no provider/claim/outcome, but prior decide remains.
    assert!(
        fx.adapter.count() == 0 && fx.adapter.stage.execute_count() == 0,
        "execute decode failure must refuse before any provider execution"
    );
    let row = required(
        checked(
            fx.shared.store.effect_by_id(id),
            "accepted effect observation failed",
        ),
        "accepted effect missing",
    );
    let observed = counts(&fx, &req, Some(id));
    assert!(observed.rows == 1 && observed.authorities == 1 && observed.events == 2
        && observed.requested == 1 && observed.decided == 1,
        "execute decode refusal must retain only the real staged row, authority and earlier decision event");
    assert!(row.state == "decided" && row.outcome.is_none() && !row.needs_you,
        "execute decode failure must neither acquire execution nor persist an outcome or uncertainty");
    let claims = fx.claims.lock().unwrap_or_else(|p| p.into_inner());
    assert!(claims.decisions == 1 && claims.claims == 0 && claims.claimed.is_none()
        && claims.decided.is_some() && row.decision == claims.decided,
        "decode refusal must retain the exact earlier durable operator decision without a later claim");
    drop(claims);
    let shown = fx.op("app_effect_show", json!({"effect_id":id}));
    assert!(
        shown["effect"]["authority"] == staged["effect"]["authority"]
            && shown["effect"]["digest"] == staged["effect"]["digest"]
            && row.input == staged["effect"]["record"]["input"],
        "decode refusal must not rewrite the historical approved authority or artifact input"
    );
    authenticated(&fx, &connection, |record, bytes| {
        assert!(record.credential_revision == 1 && bytes == BAD && crate::platform::smtp::custody_decode(bytes).is_err(),
            "accept counterexample must specifically reach coherent canonical decode failure, not rotation staleness");
    });
    assert_fixed_refusal(
        result,
        crate::Error::rejected("app release credential is invalid — withheld"),
        BAD,
    );
}

#[test]
fn smtp_public_projection_overlap_refuses_even_when_publication_input_is_clean() {
    let fx = Fx::new(BoundaryAdapter::new(false));
    // The actually enrolled password equals the real transport username.
    let connection = fx.enroll_smtp("boundary", "fixture-user");
    let (params, req) = material(&fx, &connection, "Reviewed caption");
    let result = fx.call("app_effect_stage", params);
    assert_empty_stage(&fx, &req);
    authenticated(&fx, &connection, |record, bytes| {
        assert!(
            record.platform == "smtp"
                && record.exchange == "smtp"
                && matches!(
                    record.custody.as_str(),
                    crate::platform::custody::FILE_TAG | crate::platform::custody::LIBSECRET_TAG
                ),
            "projection case must use the actual classified legacy SMTP record"
        );
        let (envelope, projection) = checked(
            crate::platform::smtp::custody_decode(bytes),
            "overlap credential must decode canonically",
        );
        assert!(
            envelope.secret() == b"fixture-user",
            "projection overlap must use the actual enrolled password"
        );
        let serialized = zeroize::Zeroizing::new(projection.to_json().to_string());
        assert!(
            crate::platform::refuse_leak("diagnostic", &serialized, envelope.secret()).is_err(),
            "real public transport projection must overlap the actual password"
        );
        let (input, preview) = fx.adapter.stage.last_prepared();
        if let (Some(input), Some(preview)) = (input, preview) {
            let serialized = zeroize::Zeroizing::new(input.to_string());
            assert!(crate::platform::refuse_leak("diagnostic", &serialized, envelope.secret()).is_ok()
                && crate::platform::refuse_leak("diagnostic", &preview, envelope.secret()).is_ok(),
                "projection refusal must not be masked by a password-bearing publication input or preview");
        }
        drop(envelope);
    });
    assert_fixed_refusal(
        result,
        crate::Error::internal(
            "smtp publication projection would carry the enrolled credential — withheld",
        ),
        b"fixture-user",
    );
}

#[test]
fn smtp_json_looking_opaque_exchange_preserves_whole_credential_screening() {
    let fx = Fx::new(BoundaryAdapter::new(false));
    let bytes = zeroize::Zeroizing::new(checked(
        crate::platform::smtp::custody_bytes(&crate::platform::smtp::SmtpEnrollment {
            host: "localhost".into(),
            port: 465,
            tls_mode: "implicit".into(),
            username: "fixture-user".into(),
            secret: PASSWORD.as_bytes().to_vec(),
            sender: "fixture@fixture.cadence".into(),
            sender_name: "Fixture".into(),
        }),
        "opaque-looking fixture encoding failed",
    ));
    // Authoritative enrolled EXCHANGE is token; provider name/JSON secret key
    // cannot turn this opaque token into classified typed SMTP.
    let connection = fx.enroll_bytes("boundary", &bytes, "token");
    let (params, req) = material(&fx, &connection, "Reviewed caption");
    let result = fx.call("app_effect_stage", params);
    assert_empty_stage(&fx, &req);
    let (input, preview) = fx.adapter.stage.last_prepared();
    let input = required(input, "opaque control must reach real preparation");
    let preview = required(preview, "opaque control must reach real complete preview");
    authenticated(&fx, &connection, |record, loaded| {
        assert!(record.platform == "smtp" && record.exchange == "token" && loaded == bytes.as_slice(),
            "opaque control must load the real token-exchange record, not classify from its JSON contents");
        let (envelope, _) = checked(
            crate::platform::smtp::custody_decode(loaded),
            "opaque token must genuinely look like a canonical SMTP document",
        );
        let serialized = zeroize::Zeroizing::new(input.to_string());
        assert!(crate::platform::refuse_leak("diagnostic", &serialized, loaded).is_err()
            && crate::platform::refuse_leak("diagnostic", &preview, loaded).is_err()
            && crate::platform::refuse_leak("diagnostic", &serialized, envelope.secret()).is_ok()
            && crate::platform::refuse_leak("diagnostic", &preview, envelope.secret()).is_ok(),
            "whole-token refusal must discriminate against unsafe generic JSON/secret-field selection");
        drop(envelope);
    });
    assert_fixed_refusal(
        result,
        crate::Error::internal("app release input would carry the enrolled credential — withheld"),
        &bytes,
    );
}

#[test]
fn smtp_prepare_error_echo_is_replaced_by_the_exact_fixed_nonrevealing_literal() {
    let fx = Fx::new(BoundaryAdapter::new(true));
    let connection = fx.enroll_smtp("boundary", PASSWORD);
    let (params, req) = material(&fx, &connection, PASSWORD);
    let result = fx.call("app_effect_stage", params);
    assert_empty_stage(&fx, &req);
    assert!(
        fx.adapter.stage.prepare_count() == 1 && fx.adapter.stage.preview_count() == 0,
        "preparation-error case must reach the real error callback and refuse before preview"
    );
    let (input, _) = fx.adapter.stage.last_prepared();
    let input = required(
        input,
        "real error preparation must capture its actual input",
    );
    authenticated(&fx, &connection, |record, bytes| {
        let (envelope, _) = checked(
            crate::platform::smtp::custody_decode(bytes),
            "preparation-error credential must be canonical",
        );
        assert!(
            record.platform == "smtp"
                && record.exchange == "smtp"
                && matches!(
                    record.custody.as_str(),
                    crate::platform::custody::FILE_TAG | crate::platform::custody::LIBSECRET_TAG
                )
                && envelope.secret() == PASSWORD.as_bytes()
                && input["title"]
                    .as_str()
                    .is_some_and(|title| title.as_bytes() == envelope.secret()),
            "real callback error must echo the actually enrolled password-bearing caller title"
        );
        drop(envelope);
    });
    assert_fixed_refusal(
        result,
        crate::Error::rejected("app release preparation failed — withheld"),
        PASSWORD.as_bytes(),
    );
}
