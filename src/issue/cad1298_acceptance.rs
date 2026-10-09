//! Independent acceptance checks for CAD-1298's shared requirements evaluator.
//! Expected outcomes are fixed in `/tmp/cad1297-advisory-01a11/acceptance-spec.md`.

use super::{delivery_policy as dp, delivery_requirements as dr};
use serde_json::json;

const TRUSTED: dr::TrustedLists<'static> = dr::TrustedLists {
    risk_paths: Some(include_str!("../../docs/roles/risk-paths.toml")),
    one_review: Some(include_str!("../../docs/roles/one-review-paths.toml")),
};

fn active() -> dp::Resolved {
    let mut policy = dp::default_policy();
    policy.solo_operator = Some(dp::SoloOperatorProfile { version: 1 });
    policy
        .validate()
        .expect("default-equivalent profile is valid");
    let digest = dp::digest(&policy);
    let approval = dp::approved_from(&json!({
        "delivery_digest": digest,
        "delivery": policy,
    }))
    .expect("approval must pass the production decoder");
    let resolved = dp::effective(
        "cad1298",
        Ok(Some(approval.policy.clone().unwrap())),
        Some(&approval),
    );
    assert_eq!(resolved.source, "approved");
    resolved
}

fn changed(path: &str) -> dr::Change {
    dr::change("M", "100644", "100644", path)
}

fn assert_legacy_strict(req: &dr::Requirements, change: &dr::Change, size: u64) {
    assert_legacy_strict_with_lists(req, change, size, &TRUSTED);
}

fn assert_legacy_strict_with_lists(
    req: &dr::Requirements,
    change: &dr::Change,
    size: u64,
    lists: &dr::TrustedLists<'_>,
) {
    // A valid approved custom policy stays on the legacy strict path and
    // provides the reference requirements independently of the new defaults.
    let mut policy = dp::default_policy();
    policy.max_revise += 1;
    policy.validate().expect("custom policy is valid");
    let digest = dp::digest(&policy);
    let approval = dp::approved_from(&json!({
        "delivery_digest": digest,
        "delivery": policy,
    }))
    .expect("custom policy approval passes the production decoder");
    let resolved = dp::effective(
        "cad1298",
        Ok(Some(approval.policy.clone().unwrap())),
        Some(&approval),
    );
    let legacy = dr::classify(&resolved, std::slice::from_ref(change), size, lists);
    assert_eq!(req.class, dr::DeliveryClass::Strict);
    assert_eq!(req.reviews, legacy.reviews);
    assert_eq!(req.review_kind, legacy.review_kind);
    assert_eq!(req.security_capable, legacy.security_capable);
    assert_eq!(req.operator_approval, legacy.operator_approval);
    assert_eq!(req.browser_qa, legacy.browser_qa);
    assert_eq!(req.full_checks, legacy.full_checks);
}

#[test]
fn cad1298_acceptance_routine_consequential_sensitive_and_mixed() {
    let approved = active();

    let routine = dr::classify(&approved, &[changed("docs/guide.md")], 10, &TRUSTED);
    assert_eq!(routine.class, dr::DeliveryClass::Routine);
    assert_eq!(routine.reviews, 0);
    assert_eq!(routine.review_kind, dr::ReviewKind::None);
    assert!(!routine.operator_approval);
    assert!(!routine.full_checks);

    let ordinary = dr::classify(&approved, &[changed("src/issue/history.rs")], 10, &TRUSTED);
    assert_eq!(ordinary.class, dr::DeliveryClass::Consequential);
    assert_eq!(ordinary.reviews, 1);
    assert_eq!(ordinary.review_kind, dr::ReviewKind::Combined);
    assert!(!ordinary.security_capable);
    assert!(!ordinary.operator_approval);

    for path in ["src/audit/approval.rs", "src/issue/delivery_policy.rs"] {
        let protected = changed(path);
        let req = dr::classify(&approved, std::slice::from_ref(&protected), 10, &TRUSTED);
        assert_legacy_strict(&req, &protected, 10);
    }

    let mixed = dr::classify(
        &approved,
        &[
            changed("docs/guide.md"),
            changed("src/daemon/caller_rule.rs"),
        ],
        10,
        &TRUSTED,
    );
    assert_eq!(mixed.class, dr::DeliveryClass::Strict);
    assert_legacy_strict(&mixed, &changed("src/daemon/caller_rule.rs"), 10);
}

#[test]
fn cad1298_acceptance_legacy_and_refusal_inputs_never_relax() {
    let approved = active();
    let protected = changed("docs/roles/risk-classes.md");
    let req = dr::classify(
        &dr::default_resolved(),
        std::slice::from_ref(&protected),
        1,
        &TRUSTED,
    );
    assert_legacy_strict(&req, &protected, 1);

    // This path is an ordinary source change covered by the real one-review
    // base list, not an unknown top-level path or malformed diff evidence.
    let ordinary = dr::classify(
        &dr::default_resolved(),
        &[changed("src/issue/unknown_module.rs")],
        1,
        &TRUSTED,
    );
    assert_eq!(ordinary.class, dr::DeliveryClass::Consequential);
    assert_eq!(ordinary.reviews, 1);
    assert_eq!(ordinary.review_kind, dr::ReviewKind::Combined);
    assert!(ordinary.full_checks);

    let missing_lists = dr::TrustedLists::default();
    let missing_change = changed("docs/guide.md");
    let req = dr::classify(
        &approved,
        std::slice::from_ref(&missing_change),
        1,
        &missing_lists,
    );
    assert_legacy_strict_with_lists(&req, &missing_change, 1, &missing_lists);

    let malformed_risk = dr::TrustedLists {
        risk_paths: Some("[trigger1\npaths = [\"src/**\"]\n"),
        one_review: TRUSTED.one_review,
    };
    let guide = changed("docs/guide.md");
    let req = dr::classify(&approved, std::slice::from_ref(&guide), 1, &malformed_risk);
    assert_legacy_strict_with_lists(&req, &guide, 1, &malformed_risk);

    let malformed_glob = dr::TrustedLists {
        risk_paths: TRUSTED.risk_paths,
        one_review: Some("one_review_include = [\"../**\"]\none_review_exclude = []\n"),
    };
    let req = dr::classify(&approved, std::slice::from_ref(&guide), 1, &malformed_glob);
    assert_legacy_strict_with_lists(&req, &guide, 1, &malformed_glob);

    let incomplete = [
        dr::change("D", "100644", "000000", "docs/guide.md"),
        dr::change("R", "100644", "100644", "docs/guide.md"),
        dr::change("M", "100644", "100755", "docs/guide.md"),
        dr::change("M", "120000", "120000", "docs/guide.md"),
        dr::change("D", "100644", "000000", "src/audit/approval.rs"),
    ];
    for change in &incomplete {
        let req = dr::classify(&approved, std::slice::from_ref(change), 1, &TRUSTED);
        assert_legacy_strict(&req, change, 1);
    }
    let empty = dr::classify(&approved, &[], 0, &TRUSTED);
    assert_eq!(empty.class, dr::DeliveryClass::Strict);
    assert!(empty.reviews > 0);
    assert!(empty.full_checks);

    let unsafe_change = changed("docs/../guide.md");
    let unsafe_path = dr::classify(&approved, std::slice::from_ref(&unsafe_change), 1, &TRUSTED);
    assert_legacy_strict(&unsafe_path, &unsafe_change, 1);

    let empty_one_review = dr::TrustedLists {
        risk_paths: TRUSTED.risk_paths,
        one_review: Some("one_review_include = []\none_review_exclude = []\n"),
    };
    let req = dr::classify(
        &dr::default_resolved(),
        &[changed("src/issue/history.rs")],
        1,
        &empty_one_review,
    );
    assert_eq!(req.class, dr::DeliveryClass::Strict);
    assert_eq!(req.reviews, 2);
}

#[test]
fn cad1298_acceptance_approved_valid_default_profile_remains_compatible() {
    let active = active();
    assert_eq!(
        dr::classify(&active, &[changed("docs/guide.md")], 1, &TRUSTED).class,
        dr::DeliveryClass::Routine
    );

    let mut invalid = active.clone();
    invalid.policy.solo_operator = Some(dp::SoloOperatorProfile { version: 2 });
    invalid.digest = dp::digest(&invalid.policy);
    let req = dr::classify(&invalid, &[changed("docs/guide.md")], 1, &TRUSTED);
    assert_legacy_strict(&req, &changed("docs/guide.md"), 1);
    assert!(dp::approved_from(&json!({
        "delivery_digest": invalid.digest,
        "delivery": invalid.policy,
    }))
    .is_none());

    assert!(dp::approved_from(&json!({
        "delivery_digest": "sha256:not-the-policy-digest",
        "delivery": active.policy.clone(),
    }))
    .is_none());

    let mut custom = active.clone();
    custom.policy.max_revise += 1;
    custom.digest = dp::digest(&custom.policy);
    let req = dr::classify(&custom, &[changed("docs/guide.md")], 1, &TRUSTED);
    assert_legacy_strict(&req, &changed("docs/guide.md"), 1);
    assert!(dp::approved_from(&json!({
        "delivery_digest": custom.digest,
        "delivery": custom.policy,
    }))
    .is_none());

    let unapproved_profile = dp::effective("cad1298", Ok(Some(active.policy.clone())), None);
    assert_eq!(unapproved_profile.source, "default");
    assert!(unapproved_profile
        .note
        .as_deref()
        .is_some_and(|n| n.starts_with("delivery_unapproved")));
    let req = dr::classify(
        &unapproved_profile,
        &[changed("docs/guide.md")],
        1,
        &TRUSTED,
    );
    assert_eq!(req.activation, dr::Activation::NotApproved);
    assert_legacy_strict(&req, &changed("docs/guide.md"), 1);

    let mutated = dp::default_policy();
    let effective = dp::effective(
        "cad1298",
        Ok(Some(mutated)),
        Some(&dp::Approved {
            digest: active.digest.clone(),
            policy: Some(active.policy.clone()),
        }),
    );
    assert_eq!(effective.source, "approved");
    assert!(effective
        .note
        .as_deref()
        .is_some_and(|n| n.starts_with("delivery_unapproved")));
    let req = dr::classify(&effective, &[changed("docs/guide.md")], 1, &TRUSTED);
    assert_legacy_strict(&req, &changed("docs/guide.md"), 1);
}

#[test]
fn cad1298_acceptance_policy_evaluator_path_is_sensitive() {
    let req = dr::classify(
        &active(),
        &[changed("src/issue/delivery_requirements.rs")],
        1,
        &TRUSTED,
    );
    assert_legacy_strict(&req, &changed("src/issue/delivery_requirements.rs"), 1);
}
