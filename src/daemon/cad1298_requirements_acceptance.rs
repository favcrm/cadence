//! Independent refusal acceptance for the CAD-1298 read-only requirements RPC.

use super::request_fields;
use serde_json::json;

#[test]
fn cad1298_requirements_accepts_only_issue_pr_and_lowercase_full_head() {
    let params = json!({
        "issue": "CAD-1298",
        "pr": "https://github.com/favcrm/cadence/pull/1298",
        "head": "0123456789abcdef0123456789abcdef01234567",
    });
    assert_eq!(
        request_fields(&params).expect("valid request fields"),
        (
            "CAD-1298",
            "https://github.com/favcrm/cadence/pull/1298",
            "0123456789abcdef0123456789abcdef01234567"
        )
    );
}

#[test]
fn cad1298_requirements_refuses_caller_controlled_policy_and_class_fields() {
    let base = json!({
        "issue": "CAD-1298",
        "pr": "https://github.com/favcrm/cadence/pull/1298",
        "head": "0123456789abcdef0123456789abcdef01234567",
    });
    for (key, value) in [
        ("class", json!("routine")),
        ("policy", json!({"solo_operator": {"version": 1}})),
        ("solo_operator", json!({"version": 1})),
        ("approval", json!({"recorded_via": "operator-connection"})),
        ("changes", json!([])),
        ("requirements", json!({"reviews": 0})),
        ("project", json!("caller-selected-project")),
    ] {
        let mut forged = base.as_object().expect("object").clone();
        forged.insert(key.to_string(), value);
        assert!(
            request_fields(&json!(forged)).is_err(),
            "caller field {key:?} must be refused"
        );
    }
}

#[test]
fn cad1298_requirements_refuses_missing_malformed_and_non_object_requests() {
    let mut accepted_invalid = Vec::new();
    for params in [
        json!(null),
        json!([]),
        json!("request"),
        json!({}),
        json!({"issue": "CAD-1298", "pr": "https://github.com/favcrm/cadence/pull/1298"}),
        json!({"issue": "", "pr": "https://github.com/favcrm/cadence/pull/1298", "head": "0123456789abcdef0123456789abcdef01234567"}),
        json!({"issue": "CAD-1298", "pr": "", "head": "0123456789abcdef0123456789abcdef01234567"}),
    ] {
        if request_fields(&params).is_ok() {
            accepted_invalid.push(params);
        }
    }
    assert!(
        accepted_invalid.is_empty(),
        "accepted invalid requests: {accepted_invalid:?}"
    );
}

#[test]
fn cad1298_requirements_refuses_invalid_or_non_lowercase_full_shas() {
    let valid_fields = |head: &str| {
        json!({
            "issue": "CAD-1298",
            "pr": "https://github.com/favcrm/cadence/pull/1298",
            "head": head,
        })
    };
    for head in [
        "",
        "0123456789abcdef0123456789abcdef0123456",
        "0123456789abcdef0123456789abcdef012345678",
        "0123456789abcdef0123456789abcdef0123456g",
        "0123456789ABCDEF0123456789ABCDEF01234567",
    ] {
        assert!(
            request_fields(&valid_fields(head)).is_err(),
            "must refuse {head:?}"
        );
    }
}
