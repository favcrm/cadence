//! Shared hosted CI-only contract fixture, exercised through the JSON ABI.
use serde_json::{json, Value};

fn evaluate(input: &Value) -> Value {
    let request = json!({"v": 1, "op": "delivery.requirements.evaluate", "input": input});
    let answer: Value = serde_json::from_slice(&cadence_core::evaluate(
        &serde_json::to_vec(&request).unwrap(),
    ))
    .unwrap();
    assert_eq!(answer["ok"], true, "{answer}");
    answer["value"].clone()
}

fn fixture() -> Value {
    serde_json::from_str(include_str!("fixtures/delivery_ci_only.json")).unwrap()
}

fn input(f: &Value, changes: Value) -> Value {
    json!({"policy_section": f["policy_section"], "policy_owner_approved": true,
        "changes": changes, "lines": 10, "risk_paths": null, "one_review_paths": null,
        "hosted_ci_only": f["hosted_ci_only"]})
}

#[test]
fn shared_cases_preserve_the_legacy_fallback() {
    let f = fixture();
    for case in f["cases"].as_array().unwrap() {
        let mut request = input(&f, case["changes"].clone());
        let result = evaluate(&request);
        assert_eq!(
            result.get("ci_only").is_some(),
            case["eligible"].as_bool().unwrap(),
            "{case}"
        );
        if case["eligible"] == true {
            assert_eq!(result["requirements"]["reviews"], 0);
            assert_eq!(result["requirements"]["review_kind"], "none");
            assert_eq!(result["requirements"]["full_checks"], true);
            assert_eq!(result["requirements"]["operator_approval"], false);
        } else {
            request.as_object_mut().unwrap().remove("hosted_ci_only");
            assert_eq!(result, evaluate(&request), "{case}");
        }
    }
}

#[test]
fn missing_proof_wrong_repo_and_unreadable_lists_do_not_enable_lane() {
    let f = fixture();
    let changes = f["cases"][0]["changes"].clone();
    let mut request = input(&f, changes);
    request.as_object_mut().unwrap().remove("hosted_ci_only");
    let fallback = evaluate(&request);
    request["hosted_ci_only"] =
        json!({"selected_repo": "other/repo", "both_role_lists_absent": true});
    assert_eq!(evaluate(&request), fallback);
    request["hosted_ci_only"] =
        json!({"selected_repo": "agenticos-stack/marketing-v2", "both_role_lists_absent": false});
    assert_eq!(evaluate(&request), fallback);
    request["hosted_ci_only"] = f["hosted_ci_only"].clone();
    request["policy_owner_approved"] = json!(false);
    assert!(evaluate(&request).get("ci_only").is_none());
}

#[test]
fn matching_conditional_review_and_duplicate_paths_block_lane() {
    let f = fixture();
    let mut request = input(&f, f["cases"][0]["changes"].clone());
    request["policy_section"] = json!("reviews:\n  review: {kind: agent, focus: general}\nrisk:\n  - require: [review]\n  - when: {paths: [src/content/**]}\n    require: [review]\nci_only:\n  repo: agenticos-stack/marketing-v2\n  paths: [src/content/editorial/blog-posts.json]");
    let result = evaluate(&request);
    assert_eq!(result["policy_source"], "approved");
    assert!(result.get("ci_only").is_none());
    request = input(
        &f,
        json!([f["cases"][0]["changes"][0], f["cases"][0]["changes"][0]]),
    );
    assert!(evaluate(&request).get("ci_only").is_none());
}

#[test]
fn ci_only_keeps_required_checks_and_protects_governance_files() {
    let f = fixture();
    let mut request = input(&f, f["cases"][0]["changes"].clone());
    request["policy_section"] = json!("reviews:\n  review: {kind: agent, focus: general}\n  ci: {kind: check, app: ci-app, mode: required}\n  lint: {kind: check, app: lint-app, mode: advisory}\nrisk:\n  - require: [review, ci]\nci_only:\n  repo: agenticos-stack/marketing-v2\n  paths: [src/content/editorial/blog-posts.json, .github/**, CODEOWNERS]");
    let result = evaluate(&request);
    assert_eq!(result["ci_only"], true, "{result}");
    assert_eq!(
        result["requirements"]["required_checks"],
        json!([{"name":"ci","app":"ci-app"}])
    );
    request["changes"] = json!([{"status":"M", "old_mode":"100644", "new_mode":"100644", "path":".github/workflows/verify.yml"}]);
    assert!(evaluate(&request).get("ci_only").is_none());
    request["changes"] =
        json!([{"status":"M", "old_mode":"100644", "new_mode":"100644", "path":"CODEOWNERS"}]);
    assert!(evaluate(&request).get("ci_only").is_none());
}

#[test]
fn malformed_scope_never_enables_lane() {
    let f = fixture();
    let mut request = input(&f, f["cases"][0]["changes"].clone());
    for scope in [
        "ci_only:\n  repo: Agenticos-stack/marketing-v2\n  paths: [src/content/editorial/blog-posts.json]",
        "ci_only:\n  repo: agenticos-stack/marketing-v2\n  paths: []",
        "ci_only:\n  repo: agenticos-stack/marketing-v2\n  paths: [../escape]",
    ] {
        request["policy_section"] = json!(format!("reviews:\n  review: {{kind: agent, focus: general}}\nrisk:\n  - require: [review]\n{scope}"));
        assert!(evaluate(&request).get("ci_only").is_none(), "{scope}");
    }
}

#[test]
fn parsed_trusted_lists_preserve_sensitive_and_qa_floors() {
    let f = fixture();
    let mut request = input(&f, f["cases"][0]["changes"].clone());
    request["hosted_ci_only"]["both_role_lists_absent"] = json!(false);
    request["risk_paths"] = json!("[schema]\npaths=[]\n[trigger1]\npaths=[]\n[trigger3]\npaths=[]\n[trigger4]\npaths=[]\n[trigger6]\npaths=[]\n[trigger7]\npaths=[]\n[operator]\npaths=[]\n[browser_qa]\npaths=[]\n[routine]\npaths=[]");
    request["one_review_paths"] = json!("one_review_include = []\none_review_exclude = []");
    assert_eq!(evaluate(&request)["ci_only"], true);
    request["risk_paths"] = json!("[schema]\npaths=[]\n[trigger1]\npaths=[]\n[trigger3]\npaths=[]\n[trigger4]\npaths=[]\n[trigger6]\npaths=[]\n[trigger7]\npaths=[]\n[operator]\npaths=[]\n[browser_qa]\npaths=[\"src/content/**\"]\n[routine]\npaths=[]");
    assert!(evaluate(&request).get("ci_only").is_none());
    request["one_review_paths"] = Value::Null;
    assert!(evaluate(&request).get("ci_only").is_none());
}
