//! One input_summary boundary counterexample plus its clean-title control.
//! Actual stage RPC/material/default custody; all identities/metadata synthetic.
#![cfg(all(test, unix, feature = "test-seam"))]
#[path = "cad1156_summary_adapter.rs"]
mod adapter;
#[path = "cad1156_summary_fixture.rs"]
mod fixture;
use adapter::OutcomeAdapter;
use fixture::Fx;
use rusqlite::OptionalExtension;
use serde_json::{json, Value};

const PASSWORD: &str = "cad1156-smtp-a";
const CLEAN_TITLE: &str = "Clean caller caption";
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
struct Observation {
    effects: i64,
    authorities: i64,
    events: i64,
    requested: i64,
    summary: Option<zeroize::Zeroizing<String>>,
}
fn observe(fx: &Fx, request: &str) -> Observation {
    let db = checked(
        rusqlite::Connection::open_with_flags(
            fx.dir.path().join("cadence.sqlite3"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        ),
        "read-only summary observation failed",
    );
    let (effects, authorities) = checked(db.query_row(
        "SELECT (SELECT COUNT(*) FROM platform_effects),(SELECT COUNT(*) FROM app_effect_authorizations)",
        [], |row| Ok((row.get(0)?, row.get(1)?))), "release counts failed");
    let (events, requested) = checked(db.query_row(
        "SELECT COUNT(*),COALESCE(SUM(kind=?3),0) FROM events WHERE alias=?1 AND json_extract(payload,'$.request')=?2",
        rusqlite::params![crate::store::PLATFORM_STREAM, request, crate::store::EFFECT_REQUESTED_EVENT],
        |row| Ok((row.get(0)?, row.get(1)?))), "release event counts failed");
    let summary: Option<String> = checked(
        db.query_row(
            "SELECT input_summary FROM platform_effects WHERE request=?1",
            [request],
            |row| row.get(0),
        )
        .optional(),
        "persisted summary observation failed",
    );
    Observation {
        effects,
        authorities,
        events,
        requested,
        summary: summary.map(zeroize::Zeroizing::new),
    }
}
fn setup(title: &str) -> (Fx, String, Value, String) {
    let fx = Fx::new(OutcomeAdapter::new());
    let connection = fx.enroll_smtp("summary", PASSWORD);
    let (install, bundle) = fx.install_social(&connection);
    let (run, artifact) = fx.publication_run(&install, &bundle);
    let request = format!(
        "app-release-{}",
        uuid::Uuid::new_v5(
            &uuid::Uuid::NAMESPACE_OID,
            format!("{install}:release-summary").as_bytes()
        )
        .simple()
    );
    let params = json!({"run_id":run,"artifact_id":artifact,"slot":"publication",
        "request_id":"release-summary","title":title});
    // Establish real default-loader/authenticated selected-secret truth BEFORE RPC.
    fx.with_secret(&connection, PASSWORD, |_| ());
    let before = observe(&fx, &request);
    assert!(
        before.effects == 0
            && before.authorities == 0
            && before.events == 0
            && before.summary.is_none(),
        "isolated eligible material must begin without release work or summary"
    );
    (fx, connection, params, request)
}
fn assert_no_execution(fx: &Fx) {
    assert!(
        fx.adapter.count() == 0 && fx.adapter.stage.execute_count() == 0,
        "staging must not call either provider execution hook"
    );
    let claims = fx.claims.lock().unwrap_or_else(|p| p.into_inner());
    assert!(
        claims.decisions == 0
            && claims.claims == 0
            && claims.decided.is_none()
            && claims.claimed.is_none(),
        "staging must not make an acceptance decision or execution claim"
    );
}
fn assert_clean_callback_surfaces(fx: &Fx, connection: &str, require_both: bool) {
    let prepared = fx.adapter.stage.prepare_count();
    let previewed = fx.adapter.stage.preview_count();
    assert!(
        prepared <= 1 && previewed <= prepared,
        "stage callbacks must be bounded and preparation must precede preview"
    );
    if require_both {
        assert!(
            prepared == 1 && previewed == 1,
            "clean-title control must reach actual eligible preparation and complete preview"
        );
    }
    let (input, preview) = fx.adapter.stage.last_prepared();
    assert!(
        input.is_some() == (prepared == 1) && preview.is_some() == (previewed == 1),
        "captured surfaces must correspond exactly to actual callbacks"
    );
    // A corrected guard may refuse BEFORE preparation. Baseline qualification
    // must separately report both actual callbacks reached to exclude input masks.
    fx.with_secret(connection, PASSWORD, |secret| {
        if let Some(input) = input {
            assert!(
                input["title"] == "Renderer-owned safe heading",
                "private wrapper must omit caller raw title from actual prepared input"
            );
            let serialized = zeroize::Zeroizing::new(input.to_string());
            assert!(
                crate::platform::refuse_leak("diagnostic", &serialized, secret).is_ok(),
                "actual prepared input must be independently clean against selected secret"
            );
        }
        if let Some(preview) = preview {
            let preview = zeroize::Zeroizing::new(preview);
            assert!(
                crate::platform::refuse_leak("diagnostic", &preview, secret).is_ok(),
                "actual complete preview must be independently clean against selected secret"
            );
        }
    });
}
#[test]
fn smtp_caller_title_secret_refuses_before_input_summary_persistence() {
    let (fx, connection, params, request) = setup(PASSWORD);
    fx.with_secret(&connection, PASSWORD, |secret| {
        assert!(params["title"].as_str().is_some_and(|title| title.as_bytes() == secret),
            "RPC title must contain the actual complete enrolled secret, not unrelated fixture text");
    });
    let result = fx.call("app_effect_stage", params);
    let observed = observe(&fx, &request);
    assert_no_execution(&fx);
    assert_clean_callback_surfaces(&fx, &connection, observed.effects > 0);
    // Privately inspect ACTUAL persisted summary before classifying RPC result.
    // This assertion is the intended genuine RED on defective current source.
    fx.with_secret(&connection, PASSWORD, |secret| {
        assert!(observed.summary.as_ref().is_none_or(|summary|
            crate::platform::refuse_leak("diagnostic", summary, secret).is_ok()),
            "pre-stage boundary must never persist actual enrolled secret in caller-derived input_summary");
    });
    assert!(observed.effects == 0 && observed.authorities == 0 && observed.events == 0
        && observed.requested == 0 && observed.summary.is_none(),
        "unsafe caller summary must refuse before effect, authority, summary or request-event persistence");
    match result {
        Err(error) => {
            let text = zeroize::Zeroizing::new(error.to_string());
            fx.with_secret(&connection, PASSWORD, |secret| {
                assert!(
                    crate::platform::refuse_leak("diagnostic", &text, secret).is_ok(),
                    "summary refusal must not echo actual enrolled secret or windows"
                );
            });
            // Exact passed supplement S5; keep no-echo before taxonomy/message.
            let expected = crate::Error::internal(
                "app release input would carry the enrolled credential — withheld",
            );
            assert!(
                error.kind() == expected.kind(),
                "summary refusal must retain the exact reviewed wire error kind"
            );
            let expected = zeroize::Zeroizing::new(expected.to_string());
            assert!(
                *text == *expected,
                "summary refusal must retain the exact reviewed fixed message"
            );
        }
        Ok(_) => panic!("unsafe caller summary unexpectedly returned success"),
    }
}
#[test]
fn smtp_clean_caller_title_stages_waiting_and_preserves_summary() {
    let (fx, connection, params, request) = setup(CLEAN_TITLE);
    fx.with_secret(&connection, PASSWORD, |secret| {
        assert!(
            crate::platform::refuse_leak("diagnostic", CLEAN_TITLE, secret).is_ok(),
            "clean title control must be independently eligible"
        );
    });
    let result = fx.call("app_effect_stage", params);
    let observed = observe(&fx, &request);
    assert_no_execution(&fx);
    assert_clean_callback_surfaces(&fx, &connection, true);
    assert!(observed.effects == 1 && observed.authorities == 1 && observed.events == 1
        && observed.requested == 1 && observed.summary.as_ref().is_some_and(|summary| summary.as_str() == CLEAN_TITLE),
        "clean caller title must retain exact summary with one waiting release authority and request event");
    let value = checked(result, "otherwise eligible clean caller summary must stage");
    assert!(
        value["effect"]["state"] == "waiting",
        "clean summary control must stage waiting"
    );
}
