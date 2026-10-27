//! Independent CAD-1314 acceptance checks for default lean classification.
//! Expectations are from the operator-approved ticket, not implementation output.

use super::{delivery_policy as dp, delivery_requirements as dr};
use serde_json::json;

const TRUSTED: dr::TrustedLists<'static> = dr::TrustedLists {
    risk_paths: Some(include_str!("../../docs/roles/risk-paths.toml")),
    one_review: Some(include_str!("../../docs/roles/one-review-paths.toml")),
};

fn changed(path: &str) -> dr::Change {
    dr::change("M", "100644", "100644", path)
}

#[test]
fn cad1314_default_outcomes_and_authority_refusal() {
    // This is the native no-project-file/no-approval resolution path, not a
    // manufactured profile or caller-supplied class.
    let default = dp::effective("cadence", Ok(None), None);
    assert_eq!(default.source, "default");
    assert_eq!(default.digest, dp::digest(&dp::default_policy()));

    let routine_change = changed("docs/guide.md");
    let routine = dr::classify(
        &default,
        std::slice::from_ref(&routine_change),
        10,
        &TRUSTED,
    );
    assert_eq!(routine.class, dr::DeliveryClass::Routine);
    assert_eq!(routine.reviews, 0);
    assert_eq!(routine.review_kind, dr::ReviewKind::None);
    assert!(!routine.operator_approval);
    assert!(!routine.full_checks);

    let ordinary = dr::classify(&default, &[changed("src/issue/history.rs")], 10, &TRUSTED);
    assert_eq!(ordinary.class, dr::DeliveryClass::Consequential);
    assert_eq!(ordinary.reviews, 1);
    assert_eq!(ordinary.review_kind, dr::ReviewKind::Combined);
    assert!(ordinary.full_checks);

    let protected_change = changed("src/audit/approval.rs");
    let protected = dr::classify(
        &default,
        std::slice::from_ref(&protected_change),
        10,
        &TRUSTED,
    );
    assert_eq!(protected.class, dr::DeliveryClass::Strict);
    assert!(protected.reviews >= 1);
    assert!(protected.operator_approval);
    assert!(protected.full_checks);

    // A real valid custom policy remains strict when approved; a forged
    // edit without matching approval also cannot inherit the default waiver.
    let mut custom = dp::default_policy();
    custom.max_revise += 1;
    custom.validate().expect("custom policy is valid");
    let digest = dp::digest(&custom);
    let approval = dp::approved_from(&json!({
        "delivery_digest": digest,
        "delivery": custom,
    }))
    .expect("production approval decoder accepts the exact valid policy");
    let approved_custom = dp::effective(
        "cadence",
        Ok(Some(
            approval
                .policy
                .clone()
                .expect("custom approval carries policy"),
        )),
        Some(&approval),
    );
    let custom_req = dr::classify(
        &approved_custom,
        std::slice::from_ref(&routine_change),
        10,
        &TRUSTED,
    );
    assert_eq!(custom_req.class, dr::DeliveryClass::Strict);
    assert!(custom_req.reviews > 0);
    assert!(custom_req.full_checks);

    let unapproved = dp::effective("cadence", Ok(Some(custom)), None);
    assert!(unapproved
        .note
        .as_deref()
        .is_some_and(|note| note.starts_with("delivery_unapproved")));
    let refused = dr::classify(
        &unapproved,
        std::slice::from_ref(&routine_change),
        10,
        &TRUSTED,
    );
    assert_eq!(refused.class, dr::DeliveryClass::Strict);
    assert!(refused.reviews > 0);
    assert!(refused.full_checks);
}
