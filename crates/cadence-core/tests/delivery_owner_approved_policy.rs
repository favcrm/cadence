//! CAD-2417: acceptance tests for `policy_owner_approved` on the core op
//! `delivery.requirements.evaluate`.
//!
//! Spec: a host that knows the `delivery:` policy text is already
//! owner-approved sets `"policy_owner_approved": true`. When the text parses
//! to a policy, the core resolves it as approved at its own digest, exactly as
//! an `approval` for that policy would. `true` with an `approval` refuses as
//! contradictory; `true` with no policy text changes nothing (default); `true`
//! with a malformed text changes nothing (strict floor, fail closed). Absent
//! or `false` keeps today's answers byte-for-byte.
//!
//! Every case drives the public `cadence_core::evaluate` entry point with a
//! JSON request and asserts on the JSON answer, so the file compiles before the
//! field exists (today `deny_unknown_fields` refuses it, so the flag-true cases
//! fail until the change lands).
use cadence_core::delivery::{self, digest};
use cadence_core::evaluate;
use serde_json::{json, Value};

// ---- fixtures --------------------------------------------------------

/// A `PROJECT.md` carrying a `delivery:` section.
fn project_md(section: &str) -> String {
    format!("---\ndelivery:\n{section}\n---\n# P\n")
}

/// A custom policy whose rules differ from the default: docs need the
/// general review only, web adds Browser QA, infra needs the operator.
const CUSTOM: &str = "  reviews:
    gen: {kind: agent, focus: general}
    qa: {kind: agent, focus: browser-qa}
    ops: {kind: operator}
  risk:
    - require: [gen]
    - when: {paths: [\"web/**\"]}
      require: [qa]
    - when: {paths: [\"infra/**\"]}
      require: [ops]";

/// Spelled out equal to the product default policy.
const DEFAULT_EQUIVALENT: &str = "  reviews:
    review: {kind: agent, focus: general}
  risk:
    - require: [review]";

/// Not a valid `delivery:` section.
const MALFORMED: &str = "  bogus: 1";

/// Synthetic trusted lists (shape as in `delivery_policy_rules.rs`).
const RISK: &str = r#"
[schema]
paths = ["sys/schema.rs"]
[trigger1]
paths = ["sys/auth.rs"]
[trigger3]
paths = ["secrets/**"]
[trigger4]
paths = ["sys/supply.lock"]
[trigger6]
paths = ["sys/rollout.rs"]
[trigger7]
paths = ["sys/rules/**"]
[operator]
paths = ["deploy/**", "src/ops/**"]
[browser_qa]
paths = ["ui/**"]
[routine]
paths = ["docs/**/*.md"]
"#;

const ONE: &str = r#"
one_review_include = ["src/**", "docs/**", "web/**", "infra/**", "notes/**", "secrets/**"]
one_review_exclude = ["secrets/**"]
"#;

fn parsed(section: &str) -> delivery::DeliveryPolicy {
    delivery::parse(&project_md(section))
        .expect("the synthetic section parses")
        .expect("a section is present")
}

/// The input object; `extra` is merged over it (flag, approval, policy...).
fn input(policy: Option<&str>, paths: &[&str], extra: Value) -> Value {
    let changes: Vec<Value> = paths
        .iter()
        .map(|p| json!({"status": "M", "old_mode": "100644", "new_mode": "100644", "path": p}))
        .collect();
    let mut v = json!({
        "policy": policy.map(project_md),
        "approval": null,
        "changes": changes,
        "lines": 10,
        "risk_paths": RISK,
        "one_review_paths": ONE,
    });
    for (k, val) in extra.as_object().unwrap() {
        v[k] = val.clone();
    }
    v
}

fn raw(input: &Value) -> Vec<u8> {
    let request = json!({"v": 1, "op": "delivery.requirements.evaluate", "input": input});
    evaluate(&serde_json::to_vec(&request).unwrap())
}

fn answer(input: &Value) -> Value {
    serde_json::from_slice(&raw(input)).unwrap()
}

/// A success answer's `value`; fails with the whole answer otherwise.
fn value(input: &Value) -> Value {
    let a = answer(input);
    assert_eq!(a["ok"], true, "{a}");
    a["value"].clone()
}

fn review_names(v: &Value) -> Vec<String> {
    v["requirements"]["required_reviews"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|e| e["name"].as_str().unwrap_or("?").to_string())
                .collect()
        })
        .unwrap_or_default()
}

fn flag() -> Value {
    json!({"policy_owner_approved": true})
}

// ---- flag true -------------------------------------------------------

#[test]
fn flag_true_custom_policy_resolves_approved_and_is_evaluated() {
    let paths = ["web/x.ts"];
    // Without the flag the unapproved custom policy is ignored (today).
    let before = value(&input(Some(CUSTOM), &paths, json!({})));
    assert_eq!(before["policy_source"], "default", "{before}");
    assert!(before["requirements"].get("required_reviews").is_none());

    let v = value(&input(Some(CUSTOM), &paths, flag()));
    assert_eq!(v["policy_source"], "approved", "{v}");
    assert_eq!(v["policy_note"], Value::Null, "{v}");
    assert_eq!(v["policy_digest"], json!(digest(&parsed(CUSTOM))), "{v}");
    assert_eq!(
        v["requirements"]["policy_digest"], v["policy_digest"],
        "{v}"
    );
    assert_eq!(review_names(&v), vec!["gen", "qa"], "{v}");
    assert_eq!(v["requirements"]["class"], "consequential", "{v}");
    assert_eq!(v["requirements"]["browser_qa"], true, "{v}");
}

#[test]
fn flag_true_matches_an_explicit_approval_of_the_same_policy() {
    let p = parsed(CUSTOM);
    let approval = json!({"digest": digest(&p), "policy": serde_json::to_value(&p).unwrap()});
    let by_approval = value(&input(
        Some(CUSTOM),
        &["web/x.ts"],
        json!({"approval": approval}),
    ));
    let by_flag = value(&input(Some(CUSTOM), &["web/x.ts"], flag()));
    assert_eq!(by_flag, by_approval, "the flag is exactly an approval");
}

#[test]
fn flag_true_default_equivalent_policy_is_unchanged() {
    // Spec ambiguity: an approval of a default-equivalent text resolves as
    // "approved" in `effective`, while no approval resolves as "file". The
    // spec says "unchanged", so only the requirements are compared and no
    // policy is evaluated (`required_reviews` stays absent).
    let paths = ["src/a.rs"];
    let before = value(&input(Some(DEFAULT_EQUIVALENT), &paths, json!({})));
    assert_eq!(before["policy_source"], "file", "precondition: {before}");
    let v = value(&input(Some(DEFAULT_EQUIVALENT), &paths, flag()));
    assert_eq!(v["requirements"], before["requirements"], "{v}");
    assert_eq!(v["policy"], before["policy"], "{v}");
    assert_eq!(v["policy_digest"], before["policy_digest"], "{v}");
    assert!(v["requirements"].get("required_reviews").is_none(), "{v}");
}

#[test]
fn flag_true_with_an_approval_is_refused_as_contradictory() {
    let p = parsed(CUSTOM);
    // A matching approval and a mismatching one: both contradict the flag.
    let approvals = [
        json!({"digest": digest(&p), "policy": serde_json::to_value(&p).unwrap()}),
        json!({"digest": "sha256:other", "policy": null}),
    ];
    for approval in approvals {
        let a = answer(&input(
            Some(CUSTOM),
            &["web/x.ts"],
            json!({"policy_owner_approved": true, "approval": approval}),
        ));
        assert_eq!(a["ok"], false, "{a}");
        assert_eq!(a["code"], "invalid_command", "{a}");
        assert!(a.get("value").is_none(), "{a}");
        // Refused for the contradiction, not for an unknown field.
        let message = a["message"].as_str().unwrap_or("");
        assert!(!message.contains("unknown field"), "{a}");
        assert!(message.contains("policy_owner_approved"), "{a}");
    }
}

#[test]
fn flag_true_with_a_malformed_policy_stays_strict() {
    let paths = ["src/a.rs"];
    let plain = answer(&input(Some(MALFORMED), &paths, json!({})));
    let v = value(&input(Some(MALFORMED), &paths, flag()));
    assert_eq!(v["policy_source"], "default", "{v}");
    assert!(
        v["policy_note"]
            .as_str()
            .unwrap_or("")
            .starts_with("delivery_error"),
        "{v}"
    );
    assert!(v["requirements"].get("required_reviews").is_none(), "{v}");
    // A malformed section is the strict floor (CAD-2417 spec), flag or not.
    assert_eq!(v["requirements"]["class"], "strict", "{v}");
    // The flag changes nothing at all.
    assert_eq!(answer(&input(Some(MALFORMED), &paths, flag())), plain);
}

#[test]
fn flag_true_with_no_policy_is_the_default() {
    let paths = ["src/a.rs"];
    let v = value(&input(None, &paths, flag()));
    assert_eq!(v["policy_source"], "default", "{v}");
    assert!(v["requirements"].get("required_reviews").is_none(), "{v}");
    assert_eq!(
        raw(&input(None, &paths, flag())),
        raw(&input(None, &paths, json!({}))),
        "byte-identical to the same request without the flag"
    );
}

// ---- flag absent or false: today's answers ----------------------------

#[test]
fn flag_absent_or_false_is_byte_identical() {
    let cases: [(Option<&str>, &[&str]); 5] = [
        (Some(CUSTOM), &["web/x.ts"]),
        (Some(CUSTOM), &["infra/a.tf", "docs/a.md"]),
        (Some(DEFAULT_EQUIVALENT), &["src/a.rs"]),
        (Some(MALFORMED), &["src/a.rs"]),
        (None, &["src/a.rs"]),
    ];
    for (policy, paths) in cases {
        let absent = raw(&input(policy, paths, json!({})));
        let off = raw(&input(
            policy,
            paths,
            json!({"policy_owner_approved": false}),
        ));
        assert_eq!(absent, off, "policy {policy:?} paths {paths:?}");
    }
}

#[test]
fn flag_absent_custom_policy_is_not_evaluated() {
    // Today's answer for an unapproved custom policy: default plus a note.
    let v = value(&input(Some(CUSTOM), &["web/x.ts"], json!({})));
    assert_eq!(v["policy_source"], "default", "{v}");
    assert!(
        v["policy_note"]
            .as_str()
            .unwrap_or("")
            .starts_with("delivery_unapproved"),
        "{v}"
    );
    assert!(v["requirements"].get("required_reviews").is_none(), "{v}");
}
