//! CAD-1793 independent acceptance check (written by the check author, not
//! the implementer; the implementer may not edit or weaken it).
//!
//! The real guards: the shared evaluator `delivery_requirements::classify`
//! (what `cadence delivery requirements` and so `scripts/enqueue-reviewed`
//! use for the review count) and the daemon's compiled-in
//! `delegation::RiskPaths::classify` (the trigger numbers), both fed the
//! repository's real `docs/roles` lists.
//!
//! 1. A diff touching only a split `src/remote_cli/**`,
//!    `src/remote_enrollment/**` or `src/remote_*_acceptance.rs` file is
//!    trigger 1 and keeps the two-review floor.
//! 2. `src/audit/**` stays trigger 7 / human-class after the dead
//!    `src/audit.rs` entry is dropped.

use super::{delivery_policy as dp, delivery_requirements as dr};
use crate::delegation::{PathClass, RiskPaths};
use serde_json::json;

const TRUSTED: dr::TrustedLists<'static> = dr::TrustedLists {
    risk_paths: Some(include_str!("../../docs/roles/risk-paths.toml")),
    one_review: Some(include_str!("../../docs/roles/one-review-paths.toml")),
};

/// The approved default-equivalent (lean, active) policy, built through the
/// production decoder exactly as `cad1298_acceptance` does.
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
    dp::effective(
        "cad1793",
        Ok(Some(approval.policy.clone().unwrap())),
        Some(&approval),
    )
}

fn triggers(path: &str) -> Vec<&'static str> {
    match RiskPaths::load().classify(path) {
        PathClass::Operator(t) => t,
        other => panic!("{path}: expected an operator-class path, got {other:?}"),
    }
}

#[test]
fn cad1793_acceptance_split_remote_tests_keep_trigger1_and_two_reviews() {
    let policies = [
        ("default", dr::default_resolved()),
        ("approved-lean", active()),
    ];
    let mut gaps = Vec::new();
    for path in [
        "src/remote_enrollment/tests.rs",
        "src/remote_cli/tests.rs",
        "src/remote_enrollment/portable_migration_gate_tests.rs",
        "src/remote_enrollment_tls_acceptance.rs",
    ] {
        let t = triggers(path);
        if !t.contains(&"1") {
            gaps.push(format!("{path}: risk-paths triggers {t:?}, want trigger 1"));
        }
        for (name, resolved) in &policies {
            let change = dr::change("M", "100644", "100644", path);
            let req = dr::classify(resolved, &[change], 10, &TRUSTED);
            if req.class != dr::DeliveryClass::Strict || req.reviews != 2 {
                gaps.push(format!(
                    "{path} ({name}): class {:?} with {} review(s), want Strict with 2; reasons {:?}",
                    req.class, req.reviews, req.reasons
                ));
            }
        }
    }
    assert!(
        gaps.is_empty(),
        "uncovered trigger 1 paths:\n{}",
        gaps.join("\n")
    );
}

#[test]
fn cad1793_acceptance_audit_tree_stays_trigger7_human() {
    let path = "src/audit/git.rs";
    let t = triggers(path);
    assert!(
        t.contains(&"7"),
        "{path}: risk-paths triggers {t:?}, want trigger 7"
    );
    for resolved in [dr::default_resolved(), active()] {
        let change = dr::change("M", "100644", "100644", path);
        let req = dr::classify(&resolved, &[change], 10, &TRUSTED);
        assert_eq!(
            req.class,
            dr::DeliveryClass::Strict,
            "{path}: {:?}",
            req.reasons
        );
        assert!(
            req.operator_approval,
            "{path} must stay human-class: {:?}",
            req.reasons
        );
    }
}
