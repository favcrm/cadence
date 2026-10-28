//! Independent CAD-1085 negative acceptance for the real persisted model-provider guard.
//!
//! Registered as a sibling test module under `store`. Uses an actual temporary
//! migrated Store and its real settings writer/read transaction. No production
//! state, provider traffic, keys, runtime permits, or copied guard implementation.

use cadence_core::connection_dispatch::{pi_model_decision, ModelId, RoleV1};
use serde_json::{json, Value};
use tempfile::TempDir;

use crate::error::Result;
use crate::pi_policy::PiPolicy;
use crate::store::connection_dispatch::{require_model_provider_in, role_model_policy_in};
use crate::store::Store;

const MODEL: &str = "devin/cad1085-metadata-fixture-high";
const DEVIN: &str = "devin";
const OTHER: &str = "alternate-fixture";

fn save_models(store: &Store, models: Value) {
    let current = store.settings().expect("read current settings authority");
    let mut doc = current.doc;
    doc.model_defaults.agent_settings["models"] = models;
    store
        .save_settings(
            current.revision,
            doc,
            "independent-cad1085-provider-acceptance",
        )
        .expect("persist metadata with the real settings writer");
}

fn check_real_provider_guard(store: &Store) -> Result<()> {
    let before = store.total_changes_for_test();
    let result = store.read_tx(|conn| {
        let policy = role_model_policy_in(&*conn)?;
        let model = ModelId::try_from(MODEL.to_string()).expect("valid fixture model ID");
        assert!(
            pi_model_decision(RoleV1::Worker, &model, policy.as_ref()).is_ok(),
            "the unchanged worker allowlist must still admit this exact model ID"
        );
        assert!(
            pi_model_decision(RoleV1::Master, &model, policy.as_ref()).is_err(),
            "the fixture remains worker-only rather than acquiring master scope"
        );
        require_model_provider_in(&*conn, MODEL, DEVIN)
    });
    assert_eq!(
        store.total_changes_for_test(),
        before,
        "provider admission/refusal must not mutate the real store"
    );
    result
}

fn expect_provider_refusal(store: &Store, case: &str) {
    let error = check_real_provider_guard(store).expect_err(case);
    assert_eq!(
        error.code(),
        Some("operation_not_granted"),
        "{case}: require the actual provider guard's closed refusal, not a setup/storage failure"
    );
}

#[test]
fn cad1085_native_provider_metadata_same_id_reassignment_refused() {
    let temp = TempDir::new().expect("temporary isolated store directory");
    let store = Store::open(&temp.path().join("provider-guard.sqlite3"))
        .expect("open a real migrated fixture Store");

    let current = store.settings().expect("read initial settings");
    let mut doc = current.doc;
    let mut policy = PiPolicy::default();
    policy.models.allow = vec![MODEL.to_string()];
    policy.models.master_allow = Some(Vec::new());
    policy.models.worker_allow = Some(vec![MODEL.to_string()]);
    doc.pi = Some(policy);
    doc.model_defaults.agent_settings = json!({
        "models": [{"id": MODEL, "provider": DEVIN}],
        "scope": {MODEL: "worker"},
        "providers": [
            {"id": DEVIN, "label": "Fixture Devin", "kind": "Pi extension",
             "pin": "", "gate": true, "paid": false, "note": "fixture only"},
            {"id": OTHER, "label": "Other fixture provider", "kind": "Pi extension",
             "pin": "", "gate": true, "paid": true, "note": "fixture only"}
        ],
        "harnesses": [],
        "roleDefaults": {},
        "pins": []
    });
    store
        .save_settings(
            current.revision,
            doc,
            "independent-cad1085-provider-acceptance",
        )
        .expect("persist initial real settings authority");

    // Positive control: a guard that refuses everything cannot pass this leaf.
    check_real_provider_guard(&store).expect("the exact model ID mapped to Devin must be accepted");
    let initial = store.settings().expect("read persisted positive control");
    let original_scope = initial.doc.model_defaults.agent_settings["scope"].clone();
    let original_policy = serde_json::to_value(&initial.doc.pi).expect("serialize policy");

    // Only the configured provider changes. ID, scope and role policy stay put.
    save_models(&store, json!([{"id": MODEL, "provider": OTHER}]));
    let reassigned = store.settings().expect("read actual reassigned metadata");
    assert_eq!(
        reassigned.doc.model_defaults.agent_settings["models"][0]["id"],
        MODEL
    );
    assert_eq!(
        reassigned.doc.model_defaults.agent_settings["models"][0]["provider"],
        OTHER
    );
    assert_eq!(
        reassigned.doc.model_defaults.agent_settings["scope"],
        original_scope
    );
    assert_eq!(
        serde_json::to_value(&reassigned.doc.pi).expect("serialize unchanged policy"),
        original_policy
    );
    expect_provider_refusal(
        &store,
        "same-ID reassignment must not retain Devin admission",
    );

    // Explicitly restoring the correct metadata restores this guard's admission.
    save_models(&store, json!([{"id": MODEL, "provider": DEVIN}]));
    check_real_provider_guard(&store).expect("restored unique Devin mapping is accepted");

    // The ID remains allowlisted, so missing metadata must not use an ID-prefix fallback.
    save_models(&store, json!([]));
    expect_provider_refusal(&store, "missing model metadata must refuse");

    // Even duplicate rows agreeing on Devin are ambiguous authority.
    save_models(
        &store,
        json!([
            {"id": MODEL, "provider": DEVIN},
            {"id": MODEL, "provider": DEVIN}
        ]),
    );
    expect_provider_refusal(&store, "duplicate agreeing metadata must refuse");

    save_models(
        &store,
        json!([
            {"id": MODEL, "provider": DEVIN},
            {"id": MODEL, "provider": OTHER}
        ]),
    );
    expect_provider_refusal(&store, "duplicate conflicting metadata must refuse");

    save_models(&store, json!([{"id": MODEL}]));
    expect_provider_refusal(&store, "metadata without a provider must refuse");
}
