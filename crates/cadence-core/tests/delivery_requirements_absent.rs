//! CAD-2386: a repository that declares no trusted lists. `classify_without_lists`
//! answers the lean default (one combined review, operator approval, full checks)
//! for any path; a custom policy or incomplete evidence keeps the strict floor.
use cadence_core::delivery::{default_policy, digest, Resolved};
use cadence_core::delivery_requirements::{
    change, classify, classify_without_lists, default_resolved, Activation, DeliveryClass,
    ReviewKind, TrustedLists,
};

fn m(path: &str) -> cadence_core::delivery_requirements::Change {
    change("M", "100644", "100644", path)
}

fn custom() -> Resolved {
    let mut p = default_policy();
    p.max_revise += 1;
    Resolved {
        digest: digest(&p),
        policy: p,
        source: "default",
        note: None,
    }
}

#[test]
fn default_policy_is_consequential_for_any_path() {
    let paths = [
        "ui/App.tsx",
        "Cargo.toml",
        "package.json",
        "scripts/x.sh",
        ".github/workflows/ci.yml",
        "src/delivery/x.rs",
        "docs/a.md",
        "README.md",
        "a/.cargo/config.toml",
        "weird/path.bin",
    ];
    for lines in [0u64, 10, 1500, 1501, 100_000] {
        for p in paths {
            let r = classify_without_lists(&default_resolved(), &[m(p)], lines);
            assert_eq!(r.class, DeliveryClass::Consequential, "{p} {lines}: {r:?}");
            assert_eq!(r.reviews, 1, "{p} {lines}");
            assert_eq!(r.review_kind, ReviewKind::Combined, "{p} {lines}");
            assert!(r.operator_approval, "{p} {lines}");
            assert!(!r.browser_qa, "{p} {lines}");
            assert!(r.full_checks, "{p} {lines}");
            assert!(!r.security_capable, "{p} {lines}");
            assert_eq!(r.activation, Activation::Active, "{p} {lines}");
            assert_eq!(r.policy_digest, default_resolved().digest);
        }
    }
}

#[test]
fn custom_policy_is_the_strict_floor() {
    for p in ["docs/a.md", "src/x.rs", "ui/App.tsx", "Cargo.toml"] {
        let r = classify_without_lists(&custom(), &[m(p)], 10);
        assert_eq!(r.class, DeliveryClass::Strict, "{p}: {r:?}");
        assert!(r.operator_approval, "{p}");
        assert_eq!(r.reviews, 2, "{p}");
        assert_eq!(r.review_kind, ReviewKind::StandardsAndSpec, "{p}");
        assert!(r.full_checks, "{p}");
    }
}

#[test]
fn incomplete_evidence_is_strict() {
    let cases: Vec<Vec<cadence_core::delivery_requirements::Change>> = vec![
        vec![],
        vec![change("Z", "100644", "100644", "src/x.rs")],
        vec![m("../x")],
        vec![m("/abs/x")],
        vec![change("M", "10064", "10064", "src/x.rs")],
    ];
    for changes in cases {
        let r = classify_without_lists(&default_resolved(), &changes, 10);
        assert_eq!(r.class, DeliveryClass::Strict, "{changes:?}: {r:?}");
        assert!(r.operator_approval, "{changes:?}");
        assert_eq!(r.reviews, 2, "{changes:?}");
    }
}

// Unreadable lists (not proven absent) still fail closed, and browser_qa stays
// false for a ui/ path exactly as before.
#[test]
fn unreadable_lists_fail_closed_without_browser_qa() {
    let r = classify(
        &default_resolved(),
        &[m("ui/App.tsx")],
        10,
        &TrustedLists::default(),
    );
    assert_eq!(r.class, DeliveryClass::Strict, "{r:?}");
    assert_eq!(r.reviews, 2);
    assert!(r.operator_approval);
    assert!(!r.browser_qa, "{r:?}");
}
