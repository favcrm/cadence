//! CAD-1156 bounded independently owned downstream assertions (I8/I9).
//! Requires the EXACT sealed REV3 test-only SMTP adapter, never edits stage5.
//! In-process real Shared dispatch + Store material/decision/claim/persistence.
//! All replies/metadata are synthetic; no SMTP network or actors launch.
#![cfg(all(test, unix, feature = "test-seam"))]

#[path = "cad1156_downstream_fixture.rs"]
mod fixture;
use crate::contract_fixture::{ToolTable, Verified};
use crate::platform::connections::ProviderDescriptor;
use crate::platform::smtp::test_artifact_adapter::SmtpArtifactAdapter;
use crate::platform::{AppArtifactError, PlatformAdapter};
use fixture::Fx;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

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

#[derive(Clone, Copy)]
enum Reply {
    SecretResult,
    SecretRefused,
    SecretUncertain,
    ConstantResult,
    ConstantRefused,
    EscapedRefused,
    CleanResult,
    CleanRefused,
    CleanUncertain,
}
#[derive(Clone, Copy)]
struct Probe {
    selected_raw_unsafe: bool,
    selected_json_unsafe: bool,
    raw_document_unsafe: bool,
}

/// Wraps the sealed descriptor/prepare/preview/count seam without changing it.
/// Only the fixture's explicit app-artifact provider return/read-back differ.
struct OutcomeAdapter {
    stage: SmtpArtifactAdapter,
    reply: Reply,
    expected_secret: zeroize::Zeroizing<Vec<u8>>,
    calls: AtomicU64,
    probe: Mutex<Option<Probe>>,
}
impl OutcomeAdapter {
    fn new(reply: Reply, secret: &str) -> Self {
        Self {
            stage: SmtpArtifactAdapter::new(),
            reply,
            expected_secret: zeroize::Zeroizing::new(secret.as_bytes().to_vec()),
            calls: AtomicU64::new(0),
            probe: Mutex::new(None),
        }
    }
    fn count(&self) -> u64 {
        self.calls.load(Ordering::SeqCst)
    }
}
impl PlatformAdapter for OutcomeAdapter {
    fn table(&self) -> &ToolTable {
        self.stage.table()
    }
    fn connection_descriptor(&self) -> Option<ProviderDescriptor> {
        self.stage.connection_descriptor()
    }
    fn connection_registration(&self) -> Option<String> {
        self.stage.connection_registration()
    }
    fn reported_manifest_version(&self) -> Option<String> {
        self.stage.reported_manifest_version()
    }
    fn prepare_app_text(
        &self,
        title: &str,
        body: &str,
        provenance: &Value,
    ) -> std::result::Result<Value, String> {
        self.stage.prepare_app_text(title, body, provenance)
    }
    fn preview(&self, account: &str, tool: &str, input: &Value) -> String {
        self.stage.preview(account, tool, input)
    }
    fn execute(
        &self,
        credential: &[u8],
        tool: &str,
        input: &Value,
        key: &str,
        hash: Option<&str>,
    ) -> std::result::Result<Value, String> {
        self.stage.execute(credential, tool, input, key, hash)
    }
    fn execute_app_artifact(
        &self,
        credential: &[u8],
        _tool: &str,
        _input: &Value,
        _key: &str,
        _hash: Option<&str>,
    ) -> std::result::Result<Value, AppArtifactError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        // The gate passed real authenticated custody bytes while holding C;
        // decode/borrow that password, not an unrelated planted constant.
        let (envelope, _) = checked(
            crate::platform::smtp::custody_decode(credential),
            "provider fixture canonical decode failed",
        );
        assert!(
            envelope.secret() == self.expected_secret.as_slice(),
            "provider must receive the actually enrolled credential"
        );
        let password = checked(
            std::str::from_utf8(envelope.secret()),
            "provider fixture password encoding invalid",
        );
        let reply = match self.reply {
            Reply::SecretResult => Ok(Value::String(password.to_string())),
            Reply::SecretRefused => Err(AppArtifactError::Refused(password.to_string())),
            Reply::SecretUncertain => Err(AppArtifactError::Uncertain(password.to_string())),
            // Origin is THIS real provider callback, not inferred from text.
            Reply::ConstantResult => Ok(Value::String("unknown".into())),
            Reply::ConstantRefused => {
                Err(AppArtifactError::Refused("provider error withheld".into()))
            }
            Reply::EscapedRefused => Err(AppArtifactError::Refused("tab:\t".into())),
            Reply::CleanResult => Ok(json!({"receipt":"safe"})),
            Reply::CleanRefused => Err(AppArtifactError::Refused("clean provider refusal".into())),
            Reply::CleanUncertain => Err(AppArtifactError::Uncertain(
                "clean provider uncertainty".into(),
            )),
        };
        let probe = match &reply {
            Ok(value) => {
                let serialized = zeroize::Zeroizing::new(value.to_string());
                Probe {
                    selected_raw_unsafe: false, // not an error/raw-text arm
                    selected_json_unsafe: crate::platform::refuse_leak(
                        "diagnostic",
                        &serialized,
                        envelope.secret(),
                    )
                    .is_err(),
                    raw_document_unsafe: crate::platform::refuse_leak(
                        "diagnostic",
                        &serialized,
                        credential,
                    )
                    .is_err(),
                }
            }
            Err(AppArtifactError::Refused(error) | AppArtifactError::Uncertain(error)) => {
                let serialized = zeroize::Zeroizing::new(checked(
                    serde_json::to_string(error),
                    "provider diagnostic serialization failed",
                ));
                Probe {
                    selected_raw_unsafe: crate::platform::refuse_leak(
                        "diagnostic",
                        error,
                        envelope.secret(),
                    )
                    .is_err(),
                    selected_json_unsafe: crate::platform::refuse_leak(
                        "diagnostic",
                        &serialized,
                        envelope.secret(),
                    )
                    .is_err(),
                    raw_document_unsafe: crate::platform::refuse_leak(
                        "diagnostic",
                        error,
                        credential,
                    )
                    .is_err(),
                }
            }
        };
        *self.probe.lock().unwrap_or_else(|p| p.into_inner()) = Some(probe);
        drop(envelope);
        reply
    }
    // Deliberate discriminator: unsafe fallback MUST replace this with Unknown.
    fn read_back(&self, _tool: &str, _input: &Value) -> Verified {
        Verified::True
    }
    fn source_hash(&self, _agent: &str, _source: &str) -> Option<String> {
        None
    }
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
    fn new(reply: Reply, password: &str) -> Self {
        let fx = Fx::new(OutcomeAdapter::new(reply, password));
        let connection = fx.enroll_smtp("downstream", password);
        let (install, bundle) = fx.install_social(&connection);
        let (run, artifact) = fx.publication_run(&install, &bundle);
        let stage_params = json!({"run_id":run,"artifact_id":artifact,"slot":"publication",
            "request_id":"release-downstream","title":"Reviewed caption"});
        let stage_result = fx.call("app_effect_stage", stage_params.clone());
        // Private post-dispatch observations under the serving C guard establish
        // short-password eligibility even if the legacy framing blocks stage.
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

fn fallback(result: bool) -> Value {
    json!({"kind":"uncertain","error":if result { "provider outcome withheld" } else { "provider error withheld" },"verified":"unknown"})
}
fn exercise_unsafe(
    reply: Reply,
    password: &str,
    is_result: bool,
    raw_unsafe: bool,
    document_unsafe: Option<bool>,
) {
    let release = Release::new(reply, password);
    let result = release.accept();
    let expected = fallback(is_result);
    let events = release.assert_fallback_before_result(&expected);
    let probe = required(
        *release
            .fx
            .adapter
            .probe
            .lock()
            .unwrap_or_else(|p| p.into_inner()),
        "actual provider diagnostic missing",
    );
    assert!(
        probe.selected_json_unsafe && probe.selected_raw_unsafe == raw_unsafe,
        "actual callback must supply the intended selected-secret dynamic counterexample"
    );
    if let Some(expected) = document_unsafe {
        assert!(probe.raw_document_unsafe == expected,
            "actual callback counterexample must discriminate whole-document from selected-secret screening");
    }
    release.assert_returned_fallback(result, &expected);
    release.assert_no_replay(&events);
}

#[test]
fn smtp_short_secret_unsafe_result_and_both_error_classes_reconcile_without_replay() {
    // One REAL enrolled Unicode character, independently short of any 8-char
    // window. The original raw-document screen cannot detect these replies.
    let password = "☃";
    assert!(
        password.chars().count() == 1,
        "short-secret fixture must be one character"
    );
    for (reply, result) in [
        (Reply::SecretResult, true),
        (Reply::SecretRefused, false),
        (Reply::SecretUncertain, false),
    ] {
        exercise_unsafe(reply, password, result, !result, Some(false));
    }
}

#[test]
fn smtp_fixed_literal_coincidence_never_exempts_identical_provider_origin_text() {
    // Provider result "unknown" is dynamic and unsafe. Constructed verified
    // "unknown" must nevertheless be retained at its fixed enum position.
    exercise_unsafe(Reply::ConstantResult, "unknown", true, false, None);
    // Exactly the SAME error text occurs in provider Refused and constructed
    // fallback. Provenance, not text equality, determines its permission:
    // unsafe Refused must become uncertainty, NOT ordinary refused/failed.
    exercise_unsafe(Reply::ConstantRefused, "withheld", false, true, None);
}

#[test]
fn smtp_error_json_serialization_cannot_introduce_a_retained_short_secret() {
    // Actual password has TWO characters: backslash + t. Raw provider tab does
    // not contain it; JSON-string serialization DOES. Omitting the final JSON
    // error check must fail on durable reconcile/outcome, not parser setup.
    exercise_unsafe(Reply::EscapedRefused, "\\t", false, false, Some(false));
}

#[test]
fn smtp_clean_dynamic_controls_retain_released_refused_and_uncertain_semantics() {
    // Prevent an always-withhold implementation from satisfying only negatives.
    for (reply, state, kind, error) in [
        (Reply::CleanResult, "done", "released", None),
        (
            Reply::CleanRefused,
            "failed",
            "refused",
            Some("clean provider refusal"),
        ),
        (
            Reply::CleanUncertain,
            "reconcile",
            "uncertain",
            Some("clean provider uncertainty"),
        ),
    ] {
        let release = Release::new(reply, "☃");
        let result = release.accept();
        assert!(
            release.fx.adapter.count() == 1 && release.fx.adapter.stage.execute_count() == 0,
            "clean control must reach the real app-artifact callback once"
        );
        let row = release.row();
        let expected = match error {
            Some(error) => json!({"kind":kind,"error":error,"verified":true}),
            None => json!({"kind":kind,"result":{"receipt":"safe"},"verified":true}),
        };
        assert!(row.state == state && row.needs_you == (state == "reconcile") && row.outcome.as_ref() == Some(&expected),
            "clean provider values must retain ordinary documented semantics and closed verification");
        release.assert_claim_retained(&row);
        let probe = required(
            *release
                .fx
                .adapter
                .probe
                .lock()
                .unwrap_or_else(|p| p.into_inner()),
            "clean provider diagnostic missing",
        );
        assert!(
            !probe.selected_raw_unsafe && !probe.selected_json_unsafe,
            "clean controls must actually pass both applicable selected-secret checks"
        );
        // Check the ACTUAL final dynamic field, not only a temporary callback
        // copy; wrapper constants are deliberately not globally screened.
        let actual = required(row.outcome.as_ref(), "clean outcome missing");
        let dynamic = if error.is_some() {
            &actual["error"]
        } else {
            &actual["result"]
        };
        release
            .fx
            .with_secret(&release.connection, &release.password, |secret| {
                let serialized = zeroize::Zeroizing::new(dynamic.to_string());
                assert!(
                    crate::platform::refuse_leak("diagnostic", &serialized, secret).is_ok(),
                    "final retained dynamic field must pass complete JSON serialization screening"
                );
                if error.is_some() {
                    assert!(
                        crate::platform::refuse_leak(
                            "diagnostic",
                            required(dynamic.as_str(), "retained provider error must be text"),
                            secret
                        )
                        .is_ok(),
                        "final retained provider error must also pass raw-text screening"
                    );
                }
            });
        let events = release.events();
        let count = |kind: &str| events.iter().filter(|(actual, _)| actual == kind).count();
        assert!(
            count(crate::store::EFFECT_REQUESTED_EVENT) == 1
                && count(crate::store::EFFECT_DECIDED_EVENT) == 1
                && count(crate::store::EFFECT_EXECUTED_EVENT) == usize::from(state == "done")
                && count(crate::store::EFFECT_FAILED_EVENT) == usize::from(state == "failed")
                && count(crate::store::EFFECT_NEEDS_YOU_EVENT) == usize::from(state == "reconcile")
                && count(crate::store::EFFECT_CANCELLED_EVENT) == 0
                && events.len() == 3,
            "clean outcomes must produce only their truthful existing completion/uncertainty event"
        );
        match result {
            Ok(value) => assert!(
                value == release.show() && value["effect"]["record"]["outcome"] == expected,
                "clean execute RPC must agree with the privately inspected persisted outcome"
            ),
            Err(_) => panic!("clean execute RPC failed after durable observation"),
        }
        release.assert_no_replay(&events);
    }
}
