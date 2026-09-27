use super::app_runs::runtime_fixture;
use super::*;

#[test]
fn cad690_context_receipt_is_rechecked_in_create_and_execution_transactions() {
    use crate::store::app_contexts::ContextConfig;
    use crate::store::app_runs::{LocalRunRequest, LocalWorkflow};
    let (_dir, s, legacy) = runtime_fixture();
    let config = ContextConfig::new("Client", std::collections::BTreeMap::new()).unwrap();
    let context = s.app_context_create("install-1", &config, "context-1").unwrap();
    let id = context["context"]["id"].as_str().unwrap();
    let (_, proof) = s.app_context_proof("install-1", id).unwrap();
    let workflow: LocalWorkflow = serde_json::from_value(legacy["snapshot"]["workflow"].clone()).unwrap();
    let inputs = std::collections::BTreeMap::new();
    let request = |key| LocalRunRequest { install_id: "install-1", bundle_digest: "sha256:bundle", workflow: &workflow, inputs: &inputs, request_id: key, owner_pm: "lead", project_link: None };
    let contextual = s.app_run_create_with_context(request("contextual"), Some(&proof)).unwrap();
    assert_eq!(contextual["snapshot"]["context"]["id"], id);
    // Corrupt only the revision fixture, deliberately bypassing proactive
    // invalidation, so this assertion isolates the current-receipt gate.
    s.conn().execute("UPDATE app_contexts SET revision=revision+1 WHERE id=?", [id]).unwrap();
    assert!(s.app_run_create_with_context(request("stale-create"), Some(&proof)).is_err(), "stale context proof reached a run commit");
    assert!(s.app_run_decide(contextual["id"].as_str().unwrap(), contextual["snapshot_digest"].as_str(), false, Some("sha256:bundle")).is_err(), "stale context receipt reached execution approval");
    assert_eq!(s.app_run_show(contextual["id"].as_str().unwrap()).unwrap()["state"], "awaiting_approval");
    assert_eq!(s.app_run_show(legacy["id"].as_str().unwrap()).unwrap()["snapshot"], legacy["snapshot"]);
}
