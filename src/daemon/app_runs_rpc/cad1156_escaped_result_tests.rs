//! ONE actual escaped-result boundary plus SAME-password clean control.
//! Existing owner files remain untouched; fixture is exact retained driver.
#![cfg(all(test, unix, feature = "test-seam"))]
#[path = "cad1156_escaped_result_adapter.rs"]
mod adapter;
#[path = "cad1156_escaped_result_fixture.rs"]
mod fixture;
use adapter::OutcomeAdapter;
use fixture::Fx;
use serde_json::{json, Value};

const PASSWORD: &str = "abcdefg\"";
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

struct Release {
    fx: Fx,
    stage_params: Value,
    staged: Value,
    id: String,
    digest: String,
    request: String,
    connection: String,
    password: zeroize::Zeroizing<String>,
}
impl Release {
    fn new(leak_result: bool) -> Self {
        let password = PASSWORD;
        let fx = Fx::new(OutcomeAdapter::new(leak_result));
        assert!(
            fx.shared.platform_custody.tag() == crate::platform::custody::FILE_TAG,
            "SETUP/ISOLATION: escaped-result fixture requires private FILE custody before enrollment"
        );
        let connection = fx.enroll_smtp("downstream", password);
        let (install, bundle) = fx.install_social(&connection);
        let (run, artifact) = fx.publication_run(&install, &bundle);
        let stage_params = json!({"run_id":run,"artifact_id":artifact,"slot":"publication",
            "request_id":"release-downstream","title":"Reviewed caption"});
        let stage_result = fx.call("app_effect_stage", stage_params.clone());
        // Private post-dispatch observations under the serving C guard establish
        // quoted-password stage eligibility before any provider-result proof.
        let (input, preview) = fx.adapter.stage.last_prepared();
        let input = required(input, "stage did not prepare the real artifact input");
        let preview = required(preview, "stage did not render the complete preview");
        assert!(
            fx.adapter.count() == 0 && fx.adapter.stage.execute_count() == 0,
            "staging must not execute any provider hook"
        );
        fx.with_secret(&connection, password, |secret| {
            assert!(
                crate::platform::refuse_leak("diagnostic", &input.to_string(), secret).is_ok()
                    && crate::platform::refuse_leak("diagnostic", &preview, secret).is_ok(),
                "downstream stage input and complete preview must be otherwise eligible"
            );
        });
        let staged = match stage_result {
            Ok(value) => value,
            Err(_) => panic!(
                "eligible downstream stage failed: SETUP failure, not downstream behavioral RED"
            ),
        };
        assert!(
            staged["effect"]["state"] == "waiting"
                && staged["effect"]["record"]["outcome"].is_null()
                && staged["effect"]["record"]["decision"].is_null(),
            "real accepted material must stage waiting before the operator decision"
        );
        let id = required(
            staged["effect"]["effect_id"].as_str(),
            "effect handle missing",
        )
        .to_string();
        let digest = required(
            staged["effect"]["digest"].as_str(),
            "release digest missing",
        )
        .to_string();
        let request = required(
            staged["effect"]["request"].as_str(),
            "release request missing",
        )
        .to_string();
        assert!(
            input["body"] == "# Post\nReviewed fixture copy."
                && input["provenance"]["effect_id"] == id
                && input["provenance"] == staged["effect"]["authority"]["provenance"],
            "staged input must contain the exact accepted artifact and real server provenance"
        );
        let waiting = required(
            checked(
                fx.shared.store.effect_by_id(&id),
                "actual waiting row read failed",
            ),
            "actual waiting row missing",
        );
        fx.with_secret(&connection, password, |secret| {
            let utf8 = checked(std::str::from_utf8(secret), "canonical password UTF-8 invalid");
            let encoded = zeroize::Zeroizing::new(checked(serde_json::to_string(utf8),
                "selected-secret JSON calibration failed"));
            let content = required(encoded.get(1..encoded.len()-1), "JSON content bounds invalid");
            let input_json = zeroize::Zeroizing::new(input.to_string());
            let summary_json = zeroize::Zeroizing::new(checked(serde_json::to_string(&waiting.input_summary),
                "actual summary JSON calibration failed"));
            // This is the SAME C guard held by retained with_secret, not a second
            // Shared/lock. Read actual public projection through default loader.
            let record = required(checked(fx.shared.store.connection_credential(&connection),
                "actual stage credential read failed"), "actual stage credential missing");
            let bytes = zeroize::Zeroizing::new(checked(crate::platform::load_credential(
                &fx.shared.store, &fx.shared.platform_custody, &record.platform, &record.account),
                "actual projection credential load failed"));
            let (envelope, projection) = checked(crate::platform::smtp::custody_decode(&bytes),
                "actual projection canonical decode failed");
            assert!(envelope.secret() == secret, "stage projection must use SAME selected password");
            let projection_json = zeroize::Zeroizing::new(projection.to_json().to_string());
            assert!(waiting.input_summary == "Reviewed caption"
                && crate::platform::refuse_leak("diagnostic", &waiting.input_summary, secret).is_ok()
                && crate::platform::refuse_leak("diagnostic", &summary_json, secret).is_ok()
                && crate::platform::refuse_leak("diagnostic", &projection_json, secret).is_ok()
                && !input_json.contains(content) && !preview.contains(content)
                && !waiting.input_summary.contains(content) && !summary_json.contains(content)
                && !projection_json.contains(content),
                "ACTUAL stage input/preview/summary/projection must be clean in raw AND complete escaped-pattern views");
            drop(envelope);
            drop(bytes);
        });
        assert!(
            fx.adapter.stage.prepare_count() == 1 && fx.adapter.stage.preview_count() == 1,
            "unmasked result setup must reach actual preparation and complete preview once"
        );
        Self {
            fx,
            stage_params,
            staged,
            id,
            digest,
            request,
            connection,
            password: zeroize::Zeroizing::new(password.to_string()),
        }
    }
    fn accept(&self) -> crate::Result<Value> {
        self.fx.call(
            "app_effect_decide",
            json!({"effect_id":self.id,"digest":self.digest,"decision":"accept"}),
        )
    }
    fn row(&self) -> crate::store::EffectRow {
        required(
            checked(
                self.fx.shared.store.effect_by_id(&self.id),
                "durable effect observation failed",
            ),
            "durable effect missing",
        )
    }
    fn show(&self) -> Value {
        self.fx.op("app_effect_show", json!({"effect_id":self.id}))
    }
    fn events(&self) -> Vec<(String, Value)> {
        // Correct audit:platforms stream, without Store::open identity reset.
        let conn = checked(
            rusqlite::Connection::open_with_flags(
                self.fx.dir.path().join("cadence.sqlite3"),
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            ),
            "event observation connection failed",
        );
        let mut stmt = checked(conn.prepare("SELECT kind,payload FROM events WHERE alias=?1 AND (json_extract(payload,'$.request')=?2 OR json_extract(payload,'$.effect_id')=?3) ORDER BY seq"), "event observation prepare failed");
        let rows = checked(
            stmt.query_map(
                rusqlite::params![crate::store::PLATFORM_STREAM, self.request, self.id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            ),
            "event observation query failed",
        );
        rows.map(|row| {
            let (kind, payload) = checked(row, "event row observation failed");
            let payload = zeroize::Zeroizing::new(payload);
            (
                kind,
                checked(serde_json::from_str(&payload), "event payload parse failed"),
            )
        })
        .collect()
    }
    fn assert_claim_retained(&self, row: &crate::store::EffectRow) {
        let observed = self.fx.claims.lock().unwrap_or_else(|p| p.into_inner());
        assert!(
            observed.decisions == 1 && observed.claims == 1,
            "the actual durable decision and executing claim must each occur once"
        );
        assert!(observed.decided.is_some() && observed.claimed == observed.decided && row.decision == observed.decided,
            "safe outcome must retain the exact prior operator decision observed at the executing claim");
        let shown = self.show();
        assert!(
            shown["effect"]["authority"] == self.staged["effect"]["authority"]
                && shown["effect"]["digest"] == self.staged["effect"]["digest"]
                && row.input == self.staged["effect"]["record"]["input"],
            "fallback must retain the frozen authorization, release digest and accepted input"
        );
    }
    fn assert_fallback_before_result(&self, expected: &Value) -> Vec<(String, Value)> {
        // Forbidden persistence/events/counts are checked BEFORE examining RPC
        // Ok/Err. None of these assertions Debug-renders a row/Value/Error.
        assert!(
            self.fx.adapter.count() == 1 && self.fx.adapter.stage.execute_count() == 0,
            "the real app-artifact provider callback must execute exactly once"
        );
        let row = self.row();
        assert!(
            row.state == "reconcile" && row.needs_you,
            "unsafe dynamic output must durably reconcile with needs-you"
        );
        assert!(
            row.outcome.as_ref() == Some(expected),
            "unsafe dynamic output must persist ONLY the exact independent uncertainty fallback"
        );
        self.assert_claim_retained(&row);
        let events = self.events();
        let count = |kind: &str| events.iter().filter(|(actual, _)| actual == kind).count();
        assert!(count(crate::store::EFFECT_REQUESTED_EVENT) == 1
            && count(crate::store::EFFECT_DECIDED_EVENT) == 1
            && count(crate::store::EFFECT_NEEDS_YOU_EVENT) == 1
            && count(crate::store::EFFECT_EXECUTED_EVENT) == 0
            && count(crate::store::EFFECT_FAILED_EVENT) == 0
            && count(crate::store::EFFECT_CANCELLED_EVENT) == 0 && events.len() == 3,
            "unsafe output must add only the normal uncertainty event, never success/failure/cancellation events");
        let needs = required(
            events
                .iter()
                .find(|(kind, _)| kind == crate::store::EFFECT_NEEDS_YOU_EVENT),
            "uncertainty event missing",
        );
        assert!(needs.1 == json!({"effect_id":self.id,"authorization_kind":"app_artifact",
            "reason":"app artifact completion is uncertain — reconcile","verified":"unknown"}),
            "uncertainty event must contain only the normal independent reason and unknown verification");
        events
    }
    fn assert_returned_fallback(&self, result: crate::Result<Value>, expected: &Value) {
        match result {
            Ok(value) => assert!(
                value["effect"]["state"] == "reconcile"
                    && value["effect"]["needs_you"] == true
                    && value["effect"]["record"]["outcome"] == *expected
                    && value == self.show(),
                "execute RPC must expose the same exact safe persisted uncertainty"
            ),
            Err(_) => panic!("execute RPC did not return the observed safe uncertainty"),
        }
    }
    fn assert_no_replay(&self, events: &[(String, Value)]) {
        let stable = self.show();
        let retry_stage = self.fx.call("app_effect_stage", self.stage_params.clone());
        assert!(
            self.fx.adapter.count() == 1
                && self.show() == stable
                && self.events().as_slice() == events,
            "stage replay must preserve the claimed effect and add no execution/events"
        );
        match retry_stage {
            Ok(value) => assert!(
                value == stable,
                "stage replay must return the same historical effect"
            ),
            Err(_) => panic!("identical stage replay unexpectedly refused"),
        }
        for decision in ["accept", "decline"] {
            let retry = self.fx.call(
                "app_effect_decide",
                json!({"effect_id":self.id,"digest":self.digest,"decision":decision}),
            );
            assert!(self.fx.adapter.count() == 1 && self.fx.adapter.stage.execute_count() == 0
                && self.show() == stable && self.events().as_slice() == events,
                "repeat decisions must preserve the executing claim history and never replay the provider");
            assert!(
                retry.is_err(),
                "an already-claimed effect must refuse repeat decisions"
            );
        }
        self.assert_claim_retained(&self.row());
    }
}

#[test]
fn smtp_escaped_actual_password_result_reconciles_unknown_without_replay() {
    let release = Release::new(true);
    let result = release.accept();
    assert!(
        release.fx.adapter.count() == 1 && release.fx.adapter.stage.execute_count() == 0,
        "result counterexample must reach the actual app-artifact provider exactly once"
    );
    let probe = required(
        *release
            .fx
            .adapter
            .probe
            .lock()
            .unwrap_or_else(|p| p.into_inner()),
        "actual provider-result representation calibration missing",
    );
    assert!(probe.canonical_eight_quote && probe.raw_scalar_unsafe
        && probe.serialized_raw_pattern_clean && probe.decoded_is_selected_password,
        "ACTUAL provider JSON must hide raw full/only-original-eight-window while decoding recovers selected password");
    let row = release.row();
    release.assert_claim_retained(&row);
    // FIRST forbidden-persistence assertion: current source should really store
    // provider C as released, not fail earlier on unrelated setup or message.
    release
        .fx
        .with_secret(&release.connection, &release.password, |secret| {
            assert!(
                !row.outcome
                    .as_ref()
                    .and_then(|outcome| outcome["result"].as_str())
                    .is_some_and(|text| text.as_bytes() == secret),
                "durable provider result must NEVER recover the actually enrolled escaped password"
            );
        });
    let expected =
        json!({"kind":"uncertain","error":"provider outcome withheld","verified":"unknown"});
    let events = release.assert_fallback_before_result(&expected);
    release.assert_returned_fallback(result, &expected);
    release.assert_no_replay(&events);
}

#[test]
fn smtp_clean_result_with_same_quoted_password_releases_verified_without_replay() {
    let release = Release::new(false);
    let result = release.accept();
    assert!(
        release.fx.adapter.count() == 1 && release.fx.adapter.stage.execute_count() == 0,
        "same-password clean control must reach the actual app-artifact provider exactly once"
    );
    let probe = required(
        *release
            .fx
            .adapter
            .probe
            .lock()
            .unwrap_or_else(|p| p.into_inner()),
        "same-password clean result calibration missing",
    );
    assert!(
        probe.canonical_eight_quote
            && !probe.raw_scalar_unsafe
            && probe.serialized_raw_pattern_clean
            && !probe.decoded_is_selected_password,
        "same valid quoted credential must yield genuinely clean independent provider data"
    );
    let row = release.row();
    let expected = json!({"kind":"released","result":{"receipt":"safe"},"verified":true});
    assert!(
        row.state == "done" && !row.needs_you && row.outcome.as_ref() == Some(&expected),
        "same quoted password clean result must retain ordinary released/verified semantics"
    );
    release.assert_claim_retained(&row);
    let actual = required(row.outcome.as_ref(), "actual clean outcome missing");
    release.fx.with_secret(&release.connection, &release.password, |secret| {
        let serialized = zeroize::Zeroizing::new(actual["result"].to_string());
        let utf8 = checked(std::str::from_utf8(secret), "canonical clean-control password encoding invalid");
        let encoded = zeroize::Zeroizing::new(checked(serde_json::to_string(utf8), "clean-control password representation failed"));
        let content = required(encoded.get(1..encoded.len()-1), "clean-control JSON content bounds invalid");
        assert!(crate::platform::refuse_leak("diagnostic", &serialized, secret).is_ok()
            && !serialized.contains(content),
            "ACTUAL clean retained result must pass raw AND complete escaped-password pattern views");
    });
    let events = release.events();
    let count = |kind: &str| events.iter().filter(|(actual, _)| actual == kind).count();
    assert!(
        count(crate::store::EFFECT_REQUESTED_EVENT) == 1
            && count(crate::store::EFFECT_DECIDED_EVENT) == 1
            && count(crate::store::EFFECT_EXECUTED_EVENT) == 1
            && count(crate::store::EFFECT_FAILED_EVENT) == 0
            && count(crate::store::EFFECT_NEEDS_YOU_EVENT) == 0
            && count(crate::store::EFFECT_CANCELLED_EVENT) == 0
            && events.len() == 3,
        "clean result must produce only truthful requested/decided/executed history"
    );
    match result {
        Ok(value) => assert!(
            value == release.show() && value["effect"]["record"]["outcome"] == expected,
            "clean execute RPC must expose exact independently inspected released outcome"
        ),
        Err(_) => panic!("same-password clean execute failed after durable observation"),
    }
    release.assert_no_replay(&events);
}
