//! CAD-2207 — a member's private personal assistant must not become an
//! operator settings/model-defaults observation: neither its alias, its
//! member subject, the model it runs, nor a live-use/used-by count hint.
//!
//! Independently authored acceptance, separate from the implementation and
//! from the shared `team_assistants_refusal` cases. This module is a CHILD of
//! that module only to reuse its already-real fixture: a real in-process
//! daemon (`Shared::dispatch`, the production caller rule), a real store, a
//! real `operator/board-identity.json` trust root, and a real bound assistant
//! registered by the production `ensure_assistant_runtime` (through the
//! production `member_assist_state` verb). Nothing here mirrors the guard.
//!
//! WHAT IS EXERCISED
//!   * `model_defaults_get` — the operator read whose observed-model
//!     suggestions are built from live agent rows. Both the private assistant
//!     and an ordinary worker carry a distinguishable synthetic configured
//!     model written through the real `Store::set_params` merge path.
//!   * `agent_settings_get` / `agent_settings_set` — the operator settings
//!     snapshot and its write. The overlay (provider/model/profile) is built
//!     from the product's own snapshot `config` shape — no hand-invented
//!     schema — so `parse_overlay`/`parse_profiles` accept exactly what
//!     `build_snapshot` publishes.
//!   * the refusal path of `agent_settings_set` when the update would remove a
//!     model an ACTIVE PRIVATE assistant still runs. `check_references` walks
//!     the full fleet (including private rows) and refuses; the refusal text
//!     must not name the private alias or the member subject, and the failed
//!     write must leave the settings revision, the snapshot and the assistant
//!     binding untouched.
//!
//! WHAT IS ASSERTED
//!   * `model_defaults_get`'s observed suggestions contain the ordinary
//!     worker's model (positive control) and never the private assistant's
//!     model, alias or member subject.
//!   * `agent_settings_get`'s live model count (`observations.models[id].live`)
//!     counts the ordinary worker and never the private assistant, while the
//!     operator-configured model row itself stays visible (it is operator
//!     data, not the assistant's).
//!   * `observations.profiles[id].usedBy` lists the ordinary profile-using
//!     worker and never the private alias; the whole snapshot JSON carries no
//!     private alias and no member subject.
//!   * a refused `agent_settings_set` is followed by a byte-identical
//!     snapshot (revision, config, observations) and an intact
//!     `assistants` binding; the SAME update succeeds once the private
//!     assistant no longer runs the removed model — proving the refusal came
//!     from the private reference, not from an invalid document.
//!
//! LIMITATION (reported, not simulated): `params.profile` is fixed at
//! registration (`Store::set_params` refuses any `profile` patch) and the
//! production `ensure_assistant_runtime` registers the assistant with
//! `{"confine": true, "assist": <sub>}`, so a private assistant cannot be made
//! to reference an OPERATOR settings profile through a supported path. The
//! private side of the profile `usedBy` hint is therefore covered by the
//! shared module's operator-settings cases and by the alias-absence assertion
//! here; only the model `live` hint is represented for the private assistant.
//!
//! Run: `cargo test --lib --features test-seam cad2207_settings_models_privacy`

use super::*;

/// Synthetic values only: distinguishable, never a real profile/model id.
const PRIVATE_MODEL: &str = "cad2207-private-model";
const ORDINARY_MODEL: &str = "cad2207-ordinary-model";
const OVERLAY_PRIVATE_MODEL: &str = "cad2207-overlay-private";
const OVERLAY_ORDINARY_MODEL: &str = "cad2207-overlay-ordinary";
const OVERLAY_PROFILE: &str = "cad2207-overlay-profile";
const PROFILE_WORKER: &str = "cad2207-profile-worker";

/// The ordinary worker's configured model must surface in the observed
/// suggestions (positive control); a private assistant's configured model must
/// not surface anywhere — suggestions are an operator read of the fleet.
#[test]
fn model_default_suggestions_withhold_a_private_assistant_model_but_keep_the_ordinary_one() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    let rt = fx.member_runtime("sub-cad2207-suggestions");
    fx.shared
        .store
        .set_params(&rt.alias, &json!({"model": PRIVATE_MODEL}))
        .unwrap_or_else(|e| panic!("configure the private assistant's model: {e}"));
    fx.shared
        .store
        .set_params(&rt.worker, &json!({"model": ORDINARY_MODEL}))
        .unwrap_or_else(|e| panic!("configure the ordinary worker's model: {e}"));

    let page = fx.op("model_defaults_get", json!({}));
    let text = page.to_string();
    assert!(
        !text.contains(rt.alias.as_str()),
        "the operator model-defaults read exposed the private assistant alias: {text}"
    );
    assert!(
        !text.contains("sub-cad2207-suggestions"),
        "the operator model-defaults read exposed the member subject: {text}"
    );
    assert!(
        !text.contains(PRIVATE_MODEL),
        "the private assistant's model became an operator suggestion: {text}"
    );

    let pi = page["providers"]
        .as_array()
        .expect("model-defaults providers is an array")
        .iter()
        .find(|row| row["id"] == json!("pi"))
        .expect("the registry publishes a pi provider row");
    assert_eq!(
        pi["eligible"],
        json!(true),
        "the pi row must be model-eligible for this control: {pi}"
    );
    let suggestions: Vec<&str> = pi["suggestions"]
        .as_array()
        .expect("suggestions is an array")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(
        suggestions.contains(&ORDINARY_MODEL),
        "the ordinary worker's model is missing from its provider's suggestions: {suggestions:?}"
    );
    assert!(
        !suggestions.contains(&PRIVATE_MODEL),
        "the private assistant's model leaked into the suggestions: {suggestions:?}"
    );
}

/// Live-use counts in the operator settings snapshot must count ordinary
/// workers only, and a write that removes a model the private assistant still
/// runs must refuse without naming it and without mutating anything.
#[test]
fn agent_settings_live_counts_withhold_a_private_assistant_and_a_failed_removal_does_not_mutate() {
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    let rt = fx.member_runtime("sub-cad2207-settings");
    fx.shared
        .store
        .set_params(&rt.alias, &json!({"model": OVERLAY_PRIVATE_MODEL}))
        .unwrap_or_else(|e| panic!("configure the private assistant's model: {e}"));
    fx.shared
        .store
        .set_params(&rt.worker, &json!({"model": OVERLAY_ORDINARY_MODEL}))
        .unwrap_or_else(|e| panic!("configure the ordinary worker's model: {e}"));

    // The initial operator snapshot publishes the exact `config` shape the
    // settings write parses back; extend it with one provider, two models and
    // one profile. The models are native-CLI provider models, so the overlay
    // needs no Pi scope and no Pi policy.
    let before = fx.op("agent_settings_get", json!({}));
    let mut config = before["config"].clone();
    {
        let obj = config
            .as_object_mut()
            .expect("snapshot config is an object");
        obj["providers"]
            .as_array_mut()
            .expect("config.providers is an array")
            .push(json!({
                "id": "codex",
                "label": "Codex",
                "kind": "native CLI",
                "pin": "",
                "gate": false,
                "paid": false,
                "note": "",
            }));
        for model in [OVERLAY_PRIVATE_MODEL, OVERLAY_ORDINARY_MODEL] {
            obj["models"]
                .as_array_mut()
                .expect("config.models is an array")
                .push(json!({
                    "id": model,
                    "provider": "codex",
                    "ctx": "",
                    "out": "",
                    "thinking": null,
                    "images": null,
                }));
        }
        obj["profiles"]
            .as_array_mut()
            .expect("config.profiles is an array")
            .push(json!({
                "id": OVERLAY_PROFILE,
                "role": "worker",
                "model": "inherit",
                "job": "cad2207 synthetic settings profile body",
            }));
    }
    let saved = fx.op(
        "agent_settings_set",
        json!({"expected_revision": before["revision"], "config": config}),
    );
    assert!(
        saved["config"]["models"]
            .as_array()
            .expect("saved config.models is an array")
            .iter()
            .any(|row| row["id"] == json!(OVERLAY_PRIVATE_MODEL)),
        "the operator overlay did not store the model the private assistant runs: {saved}"
    );

    // An ORDINARY agent that references the operator profile: `params.profile`
    // is fixed at registration, so this is the supported way to prove the
    // used-by projection still works for a non-private runtime.
    let profile_cwd = fx.dir.path().join("cad2207-profile-worker");
    std::fs::create_dir_all(&profile_cwd).unwrap();
    let profile_params = json!({"profile": OVERLAY_PROFILE}).to_string();
    fx.shared
        .store
        .register_agent(&crate::store::NewAgent {
            alias: PROFILE_WORKER,
            provider: "pi",
            endpoint_kind: "managed",
            role: "worker",
            cwd: profile_cwd.to_str().unwrap(),
            sandbox: "read-only",
            instructions: None,
            params: Some(profile_params.as_str()),
            team_role: None,
            model_policy: None,
        })
        .unwrap_or_else(|e| panic!("register the ordinary profile worker: {e}"));

    let page = fx.op("agent_settings_get", json!({}));
    let models = page["observations"]["models"]
        .as_object()
        .expect("observations.models is an object");
    assert_eq!(
        models
            .get(OVERLAY_PRIVATE_MODEL)
            .map(|row| row["live"].clone()),
        Some(json!(0)),
        "the private assistant counted as a live user of its model: {page}"
    );
    assert_eq!(
        models
            .get(OVERLAY_ORDINARY_MODEL)
            .map(|row| row["live"].clone()),
        Some(json!(1)),
        "the ordinary worker's live use is missing: {page}"
    );
    let used: Vec<&str> = page["observations"]["profiles"][OVERLAY_PROFILE]["usedBy"]
        .as_array()
        .expect("usedBy is an array")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(
        used,
        vec![PROFILE_WORKER],
        "the operator profile's used-by list is not the ordinary worker exactly: {page}"
    );
    let text = page.to_string();
    assert!(
        !text.contains(rt.alias.as_str()),
        "the settings snapshot exposed the private assistant alias: {text}"
    );
    assert!(
        !text.contains("sub-cad2207-settings"),
        "the settings snapshot exposed the member subject: {text}"
    );

    // Removing the model the private assistant runs must refuse. The private
    // row is the ONLY user of that model, so the refusal names no alias.
    let mut removal = page["config"].clone();
    removal["models"]
        .as_array_mut()
        .expect("config.models is an array")
        .retain(|row| row["id"] != json!(OVERLAY_PRIVATE_MODEL));
    let err = fx
        .op_err(
            "agent_settings_set",
            json!({"expected_revision": page["revision"], "config": removal}),
        )
        .expect_err("removing a model a private assistant still runs must refuse");
    let message = err.to_string();
    assert!(
        !message.contains(rt.alias.as_str()),
        "the refusal exposed the private assistant alias: {message}"
    );
    assert!(
        !message.contains("sub-cad2207-settings"),
        "the refusal exposed the member subject: {message}"
    );
    assert!(
        !message.contains(OVERLAY_PROFILE),
        "the refusal exposed an unrelated operator profile id: {message}"
    );

    // No mutation: the settings revision/config/observations and the private
    // binding are exactly as they were before the refused write.
    let unchanged = fx.op("agent_settings_get", json!({}));
    assert_eq!(unchanged["revision"], page["revision"]);
    assert_eq!(unchanged["config"], page["config"]);
    assert_eq!(unchanged["observations"], page["observations"]);
    assert!(
        fx.shared
            .store
            .assistant_for_alias(&rt.alias)
            .unwrap_or_else(|e| panic!("read the assistant binding: {e}"))
            .is_some(),
        "the refused settings write removed the private binding"
    );
    let private_row = fx
        .shared
        .store
        .agent_opt(&rt.alias)
        .unwrap_or_else(|e| panic!("read the private assistant row: {e}"))
        .expect("the private assistant row survived the refused write");
    assert_eq!(
        private_row
            .params
            .as_ref()
            .and_then(|params| params.get("model")),
        Some(&json!(OVERLAY_PRIVATE_MODEL)),
        "the refused settings write changed the private assistant's model"
    );

    // Positive control: once the private assistant no longer runs the model,
    // the SAME update is accepted — so the refusal above was the private
    // reference, not an unparseable document.
    fx.shared
        .store
        .set_params(&rt.alias, &json!({"model": null}))
        .unwrap_or_else(|e| panic!("clear the private assistant's model: {e}"));
    let removed = fx.op(
        "agent_settings_set",
        json!({"expected_revision": unchanged["revision"], "config": removal}),
    );
    assert!(
        removed["config"]["models"]
            .as_array()
            .expect("config.models is an array")
            .iter()
            .all(|row| row["id"] != json!(OVERLAY_PRIVATE_MODEL)),
        "the positive-control removal did not take effect: {removed}"
    );
    assert_eq!(
        removed["observations"]["models"][OVERLAY_ORDINARY_MODEL]["live"],
        json!(1),
        "the ordinary live-use count changed with the removal: {removed}"
    );
    assert!(
        !removed.to_string().contains(rt.alias.as_str()),
        "the post-removal snapshot exposed the private assistant alias: {removed}"
    );
}
