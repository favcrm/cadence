//! CAD-2413: acceptance tests for evaluating an approved custom delivery
//! policy's `risk` rules in `delivery_requirements::classify`.
//!
//! Spec: an approved (`source: "approved"`) valid non-default policy is
//! evaluated (union of the reviews named by every applying rule), with the
//! trusted-list floor of today's strict requirements always applied on top
//! (D1), while default, unapproved and malformed policies keep today's
//! behaviour (D2).
//!
//! The new `Requirements` fields (`required_reviews`, `required_checks`) are
//! read through `serde_json::to_value`, so this file compiles before the
//! engine change lands. Fields that exist today are asserted directly.
use cadence_core::delivery::{self, default_policy, digest, DeliveryPolicy, Resolved};
use cadence_core::delivery_requirements::{
    change, classify, default_resolved, Activation, Change, DeliveryClass, Requirements,
    ReviewKind, TrustedLists,
};
use serde_json::Value;

// ---- fixtures --------------------------------------------------------

/// Parse a `delivery:` section the way a PROJECT.md carries it.
fn policy(yaml: &str) -> DeliveryPolicy {
    delivery::parse(&format!("---\ndelivery:\n{yaml}\n---\n# P\n"))
        .expect("the synthetic delivery section parses and validates")
        .expect("a delivery section is present")
}

fn approved(p: DeliveryPolicy) -> Resolved {
    Resolved {
        digest: digest(&p),
        policy: p,
        source: "approved",
        note: None,
    }
}

/// The spec's synthetic policy.
fn spec_policy() -> DeliveryPolicy {
    policy(
        "  reviews:
    std: {kind: agent, focus: standards}
    spec: {kind: agent, focus: spec-security}
    gen: {kind: agent, focus: general}
    qa: {kind: agent, focus: browser-qa}
    ops: {kind: operator}
    bot: {kind: check, app: acme-ci, mode: required}
    hint: {kind: check, app: lint-bot, mode: advisory}
  risk:
    - require: [gen]
    - when: {paths: [\"web/**\"]}
      require: [qa]
    - when: {paths: [\"infra/**\"]}
      require: [ops]
    - when: {paths: [\"src/**\"]}
      require: [std, spec, bot]
    - when: {lines_over: 400}
      require: [ops]",
    )
}

/// The only rule is the unconditional general review.
fn gen_only() -> DeliveryPolicy {
    policy(
        "  reviews:
    gen: {kind: agent, focus: general}
  risk:
    - require: [gen]",
    )
}

/// Synthetic trusted lists satisfying every required table. `secrets/**` is
/// trigger3 (and a one-review exclude), `deploy/**` and `src/ops/**` are in
/// `operator`; `src/ops/**` is also on the one-review include list, so
/// today's strict answer for it is ONE combined review plus the operator.
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

fn synth() -> TrustedLists<'static> {
    TrustedLists {
        risk_paths: Some(RISK),
        one_review: Some(ONE),
    }
}

fn unreadable() -> TrustedLists<'static> {
    TrustedLists {
        risk_paths: None,
        one_review: None,
    }
}

/// This repository's real trusted lists (read as the golden test does).
struct RepoLists {
    risk: String,
    one: String,
}

fn repo_lists() -> RepoLists {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    RepoLists {
        risk: std::fs::read_to_string(root.join("docs/roles/risk-paths.toml")).unwrap(),
        one: std::fs::read_to_string(root.join("docs/roles/one-review-paths.toml")).unwrap(),
    }
}

impl RepoLists {
    fn trusted(&self) -> TrustedLists<'_> {
        TrustedLists {
            risk_paths: Some(&self.risk),
            one_review: Some(&self.one),
        }
    }
}

fn m(path: &str) -> Change {
    change("M", "100644", "100644", path)
}

fn json(r: &Requirements) -> Value {
    serde_json::to_value(r).unwrap()
}

/// Names in `required_reviews`, in order (empty when the field is absent).
fn review_names(r: &Requirements) -> Vec<String> {
    json(r)["required_reviews"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|e| e["name"].as_str().unwrap_or("?").to_string())
                .collect()
        })
        .unwrap_or_default()
}

fn review_entry(r: &Requirements, name: &str) -> Value {
    json(r)["required_reviews"]
        .as_array()
        .and_then(|a| a.iter().find(|e| e["name"] == name).cloned())
        .unwrap_or(Value::Null)
}

/// `required_checks` as `(name, app)` pairs; `None` when the field is absent.
fn checks(r: &Requirements) -> Option<Vec<(String, String)>> {
    json(r)["required_checks"].as_array().map(|a| {
        a.iter()
            .map(|e| {
                (
                    e["name"].as_str().unwrap_or("?").to_string(),
                    e["app"].as_str().unwrap_or("?").to_string(),
                )
            })
            .collect()
    })
}

fn rule_reasons(r: &Requirements) -> usize {
    r.reasons
        .iter()
        .filter(|s| s.contains("policy rule"))
        .count()
}

fn mentions_floor(r: &Requirements) -> bool {
    r.reasons.iter().any(|s| s.to_lowercase().contains("floor"))
}

/// What today's engine answers for `changes` under the default policy and
/// the same lists: the `strict` floor the spec says an approved policy must
/// never go below.
fn today_strict(changes: &[Change], lines: u64, lists: &TrustedLists) -> Requirements {
    classify(&default_resolved(), changes, lines, lists)
}

// ---- evaluated rules (synthetic policy, synthetic lists) --------------

#[test]
fn docs_path_needs_one_combined_review_only() {
    let r = classify(&approved(spec_policy()), &[m("docs/a.md")], 10, &synth());
    assert_eq!(r.class, DeliveryClass::Consequential, "{r:?}");
    assert_eq!(r.reviews, 1, "{r:?}");
    assert_eq!(r.review_kind, ReviewKind::Combined, "{r:?}");
    assert!(!r.operator_approval, "{r:?}");
    assert!(!r.browser_qa, "{r:?}");
    assert!(r.full_checks, "{r:?}");
    assert!(!r.security_capable, "{r:?}");
    assert_eq!(r.activation, Activation::NoProfile);
    assert_eq!(r.policy_digest, digest(&spec_policy()));
    assert_eq!(checks(&r), Some(vec![]), "required_checks is present and empty: {r:?}");
    assert_eq!(review_names(&r), vec!["gen"], "{r:?}");
    assert_eq!(rule_reasons(&r), 1, "only the unconditional rule applies: {:?}", r.reasons);
}

#[test]
fn web_path_adds_browser_qa_to_the_combined_review() {
    let r = classify(&approved(spec_policy()), &[m("web/x.ts")], 10, &synth());
    assert_eq!(r.class, DeliveryClass::Consequential, "{r:?}");
    assert!(r.browser_qa, "{r:?}");
    assert_eq!(r.reviews, 1, "browser QA is not counted as a review: {r:?}");
    assert_eq!(r.review_kind, ReviewKind::Combined, "{r:?}");
    assert!(!r.operator_approval, "{r:?}");
    assert_eq!(review_names(&r), vec!["gen", "qa"], "{r:?}");
    assert_eq!(rule_reasons(&r), 2, "{:?}", r.reasons);
}

#[test]
fn infra_path_needs_the_operator() {
    let r = classify(&approved(spec_policy()), &[m("infra/x.tf")], 10, &synth());
    assert_eq!(r.class, DeliveryClass::Consequential, "{r:?}");
    assert!(r.operator_approval, "{r:?}");
    assert!(!r.browser_qa, "{r:?}");
    assert_eq!(r.reviews, 1, "the operator is not counted as a review: {r:?}");
    assert_eq!(review_entry(&r, "ops")["kind"], "operator", "{r:?}");
}

#[test]
fn src_path_needs_standards_and_spec_and_the_required_check() {
    let r = classify(&approved(spec_policy()), &[m("src/a.rs")], 10, &synth());
    assert_eq!(r.class, DeliveryClass::Consequential, "{r:?}");
    assert_eq!(r.reviews, 2, "{r:?}");
    assert_eq!(r.review_kind, ReviewKind::StandardsAndSpec, "{r:?}");
    assert!(!r.operator_approval, "{r:?}");
    assert_eq!(
        checks(&r),
        Some(vec![("bot".to_string(), "acme-ci".to_string())]),
        "{r:?}"
    );
    // Rule order, deduplicated; the advisory `hint` is never listed.
    assert_eq!(review_names(&r), vec!["gen", "std", "spec", "bot"], "{r:?}");
    assert_eq!(review_entry(&r, "std")["kind"], "agent");
    assert_eq!(review_entry(&r, "std")["focus"], "standards");
    assert_eq!(review_entry(&r, "spec")["focus"], "spec-security");
    let bot = review_entry(&r, "bot");
    assert_eq!(bot["kind"], "check");
    assert_eq!(bot["app"], "acme-ci");
    assert_eq!(bot["mode"], "required");
    assert!(!review_names(&r).contains(&"hint".to_string()));
}

#[test]
fn line_count_over_the_rule_threshold_needs_the_operator() {
    let at = classify(&approved(spec_policy()), &[m("docs/a.md")], 400, &synth());
    assert!(!at.operator_approval, "lines == lines_over does not apply: {at:?}");
    let over = classify(&approved(spec_policy()), &[m("docs/a.md")], 401, &synth());
    assert!(over.operator_approval, "{over:?}");
    assert_eq!(over.class, DeliveryClass::Consequential, "{over:?}");
}

#[test]
fn rules_union_across_every_changed_path() {
    let r = classify(
        &approved(spec_policy()),
        &[m("web/x.ts"), m("infra/x.tf"), m("src/a.rs")],
        10,
        &synth(),
    );
    assert!(r.browser_qa && r.operator_approval, "{r:?}");
    assert_eq!(r.review_kind, ReviewKind::StandardsAndSpec, "{r:?}");
    assert_eq!(r.reviews, 2, "{r:?}");
    assert_eq!(
        review_names(&r),
        vec!["gen", "qa", "ops", "std", "spec", "bot"],
        "rule order, deduplicated: {r:?}"
    );
}

#[test]
fn a_rule_applies_on_any_of_its_conditions() {
    let p = policy(
        "  reviews:
    gen: {kind: agent, focus: general}
    ops: {kind: operator}
  risk:
    - require: [gen]
    - when: {paths: [\"infra/**\"], lines_over: 50, files_over: 2}
      require: [ops]",
    );
    let rs = approved(p);
    let ops = |changes: &[Change], lines: u64| {
        classify(&rs, changes, lines, &synth()).operator_approval
    };
    assert!(!ops(&[m("docs/a.md")], 10), "no condition holds");
    assert!(ops(&[m("infra/x.tf")], 10), "path alone");
    assert!(ops(&[m("docs/a.md")], 51), "lines alone");
    assert!(!ops(&[m("docs/a.md")], 50), "lines at the limit");
    assert!(
        ops(&[m("docs/a.md"), m("docs/b.md"), m("docs/c.md")], 10),
        "files alone (3 > 2)"
    );
    assert!(
        !ops(&[m("docs/a.md"), m("docs/b.md")], 10),
        "files at the limit"
    );
}

#[test]
fn a_single_axis_review_counts_one_review_of_that_axis() {
    for (rev, focus) in [("std", "standards"), ("spec", "spec-security")] {
        let p = policy(&format!(
            "  reviews:\n    {rev}: {{kind: agent, focus: {focus}}}\n  risk:\n    - require: [{rev}]"
        ));
        let r = classify(&approved(p), &[m("docs/a.md")], 10, &synth());
        assert_eq!(r.class, DeliveryClass::Consequential, "{rev}: {r:?}");
        assert_eq!(r.reviews, 1, "{rev}: {r:?}");
        let kind = json(&r)["review_kind"].as_str().unwrap().to_string();
        assert!(
            kind != "none" && kind != "standards_and_spec",
            "{rev}: one review of that axis, got {kind}"
        );
    }
}

#[test]
fn standards_wins_over_general_when_both_are_required() {
    let p = policy(
        "  reviews:
    gen: {kind: agent, focus: general}
    std: {kind: agent, focus: standards}
  risk:
    - require: [gen, std]",
    );
    let r = classify(&approved(p), &[m("docs/a.md")], 10, &synth());
    assert_eq!(r.reviews, 1, "{r:?}");
    assert_ne!(r.review_kind, ReviewKind::Combined, "{r:?}");
    assert_ne!(r.review_kind, ReviewKind::StandardsAndSpec, "{r:?}");
}

#[test]
fn an_approved_policy_never_yields_routine() {
    // docs/a.md is on the routine allowlist; the default policy admits it as
    // routine, an approved custom policy does not.
    let routine = classify(&default_resolved(), &[m("docs/a.md")], 10, &synth());
    assert_eq!(routine.class, DeliveryClass::Routine, "{routine:?}");
    for p in [gen_only(), spec_policy()] {
        let r = classify(&approved(p), &[m("docs/a.md")], 10, &synth());
        assert_ne!(r.class, DeliveryClass::Routine, "{r:?}");
        assert!(r.reviews >= 1 || r.operator_approval, "{r:?}");
    }
}

#[test]
fn an_operator_only_policy_needs_the_operator() {
    let p = policy(
        "  reviews:
    ops: {kind: operator}
  risk:
    - require: [ops]",
    );
    let r = classify(&approved(p), &[m("docs/a.md")], 10, &synth());
    assert_eq!(r.class, DeliveryClass::Consequential, "{r:?}");
    assert!(r.operator_approval, "{r:?}");
}

// ---- the floor (D1) ---------------------------------------------------

#[test]
fn trigger3_path_is_strict_with_two_reviews_regardless_of_policy() {
    let r = classify(&approved(spec_policy()), &[m("secrets/k.rs")], 10, &synth());
    assert_eq!(r.class, DeliveryClass::Strict, "{r:?}");
    assert_eq!(r.reviews, 2, "{r:?}");
    assert_eq!(r.review_kind, ReviewKind::StandardsAndSpec, "{r:?}");
    // trigger3 is not human-class: the operator comes only via a Risk line.
    assert!(!r.operator_approval, "{r:?}");
    assert!(r.full_checks, "{r:?}");
    assert!(mentions_floor(&r), "reasons say the floor applied: {:?}", r.reasons);
    assert_eq!(r.activation, Activation::NoProfile);
}

#[test]
fn operator_table_path_is_strict_with_the_operator() {
    let r = classify(&approved(spec_policy()), &[m("deploy/run.sh")], 10, &synth());
    assert_eq!(r.class, DeliveryClass::Strict, "{r:?}");
    assert!(r.operator_approval, "{r:?}");
    assert!(r.full_checks, "{r:?}");
    assert!(mentions_floor(&r), "{:?}", r.reasons);
}

#[test]
fn unreadable_lists_are_strict_with_the_operator() {
    let r = classify(&approved(spec_policy()), &[m("docs/a.md")], 10, &unreadable());
    assert_eq!(r.class, DeliveryClass::Strict, "{r:?}");
    assert!(r.operator_approval, "{r:?}");
    assert_eq!(r.reviews, 2, "{r:?}");
    assert_eq!(r.review_kind, ReviewKind::StandardsAndSpec, "{r:?}");
    assert!(!r.browser_qa, "unreadable lists ask no Browser QA: {r:?}");
}

#[test]
fn policy_required_checks_are_still_listed_under_the_floor() {
    let r = classify(
        &approved(spec_policy()),
        &[m("src/a.rs"), m("secrets/k.rs")],
        10,
        &synth(),
    );
    assert_eq!(r.class, DeliveryClass::Strict, "{r:?}");
    assert_eq!(
        checks(&r),
        Some(vec![("bot".to_string(), "acme-ci".to_string())]),
        "{r:?}"
    );
}

#[test]
fn the_floor_takes_the_stricter_of_policy_and_strict() {
    // Today's strict answer for an operator-table path that is on the
    // one-review include list is ONE combined review plus the operator.
    let ops_path = [m("src/ops/run.sh")];
    let strict = today_strict(&ops_path, 10, &synth());
    assert_eq!(strict.reviews, 1, "fixture premise: {strict:?}");
    assert!(strict.operator_approval, "fixture premise: {strict:?}");
    // The policy's src/** rule needs standards AND spec: the stricter wins.
    let r = classify(&approved(spec_policy()), &ops_path, 10, &synth());
    assert_eq!(r.class, DeliveryClass::Strict, "{r:?}");
    assert_eq!(r.reviews, 2, "{r:?}");
    assert_eq!(r.review_kind, ReviewKind::StandardsAndSpec, "{r:?}");
    assert!(r.operator_approval, "{r:?}");

    // Conversely a policy needing less than strict never lowers it.
    let r = classify(&approved(gen_only()), &[m("secrets/k.rs")], 10, &synth());
    assert_eq!(r.reviews, 2, "{r:?}");
    assert_eq!(r.review_kind, ReviewKind::StandardsAndSpec, "{r:?}");
}

#[test]
fn the_floor_ors_operator_browser_qa_and_full_checks() {
    // Policy asks browser QA for web/**; the floor adds the operator for
    // deploy/**. Both must survive.
    let r = classify(
        &approved(spec_policy()),
        &[m("web/x.ts"), m("deploy/run.sh")],
        10,
        &synth(),
    );
    assert_eq!(r.class, DeliveryClass::Strict, "{r:?}");
    assert!(r.browser_qa, "policy browser QA is kept: {r:?}");
    assert!(r.operator_approval, "{r:?}");
    assert!(r.full_checks, "{r:?}");
}

#[test]
fn oversize_incomplete_and_unsafe_shapes_hit_the_floor() {
    let p = approved(gen_only());
    let lists = synth();
    // lines > 1500 (strictly): 1500 is still ordinary.
    let at = classify(&p, &[m("docs/a.md")], 1500, &lists);
    assert_eq!(at.class, DeliveryClass::Consequential, "{at:?}");
    assert!(!at.operator_approval, "{at:?}");
    let big = classify(&p, &[m("docs/a.md")], 1501, &lists);
    assert_eq!(big.class, DeliveryClass::Strict, "{big:?}");
    assert!(big.operator_approval, "{big:?}");

    let shapes: Vec<(&str, Vec<Change>)> = vec![
        ("empty diff", vec![]),
        (
            "delete",
            vec![change("D", "100644", "000000", "docs/a.md")],
        ),
        (
            "rename",
            vec![change("R", "100644", "100644", "docs/a.md")],
        ),
        (
            "mode change",
            vec![change("M", "100644", "100755", "docs/a.md")],
        ),
        (
            "symlink",
            vec![change("M", "120000", "120000", "docs/a.md")],
        ),
        ("unsafe path", vec![m("../etc/passwd")]),
    ];
    for (what, changes) in shapes {
        let r = classify(&p, &changes, 10, &lists);
        assert_eq!(r.class, DeliveryClass::Strict, "{what}: {r:?}");
        assert!(r.full_checks, "{what}");
        assert_eq!(r.reviews, 2, "{what}: {r:?}");
        let today = today_strict(&changes, 10, &lists);
        assert_eq!(
            (r.reviews, r.operator_approval, r.browser_qa),
            (today.reviews, today.operator_approval, today.browser_qa),
            "{what}: at least today's strict answer"
        );
    }
}

// ---- lowering (D1) with the repository's real lists -------------------

#[test]
fn ordinary_path_outside_the_include_list_is_lowered_by_an_approved_policy() {
    let lists = repo_lists();
    let notes = [m("notes/x.md")];

    // Today, default policy: outside the one-review include list → strict, 2.
    let d = classify(&default_resolved(), &notes, 10, &lists.trusted());
    assert_eq!(d.class, DeliveryClass::Strict, "{d:?}");
    assert_eq!(d.reviews, 2, "{d:?}");
    assert_eq!(d.review_kind, ReviewKind::StandardsAndSpec, "{d:?}");

    // Approved policy whose only rule is the unconditional general review.
    let r = classify(&approved(gen_only()), &notes, 10, &lists.trusted());
    assert_eq!(r.class, DeliveryClass::Consequential, "{r:?}");
    assert_eq!(r.reviews, 1, "{r:?}");
    assert_eq!(r.review_kind, ReviewKind::Combined, "{r:?}");
    assert!(!r.operator_approval, "{r:?}");
    assert!(r.full_checks, "{r:?}");
    assert_eq!(r.activation, Activation::NoProfile);
}

#[test]
fn human_class_paths_stay_strict_with_the_operator_under_a_lowering_policy() {
    let lists = repo_lists();
    for path in ["Cargo.toml", ".github/workflows/ci.yml", "docs/roles/x.md"] {
        let r = classify(&approved(gen_only()), &[m(path)], 10, &lists.trusted());
        assert_eq!(r.class, DeliveryClass::Strict, "{path}: {r:?}");
        assert!(r.operator_approval, "{path}: {r:?}");
        assert!(r.full_checks, "{path}");
    }
}

#[test]
fn trigger_paths_keep_todays_strict_requirements_under_a_lowering_policy() {
    let lists = repo_lists();
    for path in ["src/secret/x.rs", "src/daemon/identity.rs", "ui/App.tsx"] {
        let changes = [m(path)];
        let today = today_strict(&changes, 10, &lists.trusted());
        assert_eq!(today.class, DeliveryClass::Strict, "{path}: premise");
        let r = classify(&approved(gen_only()), &changes, 10, &lists.trusted());
        assert_eq!(r.class, DeliveryClass::Strict, "{path}: {r:?}");
        assert!(r.reviews >= today.reviews, "{path}: {r:?} vs {today:?}");
        assert_eq!(r.review_kind, today.review_kind, "{path}: {r:?}");
        assert_eq!(r.operator_approval, today.operator_approval, "{path}");
        assert_eq!(r.browser_qa, today.browser_qa, "{path}");
    }
}

#[test]
fn golden_custom_policy_evaluated_as_approved() {
    // The golden's `custom` variant (default + max_revise + 1) re-classified
    // under `source: "approved"`: its `risk` is the default one-general-review
    // rule, so it behaves like the lowering policy above.
    let lists = repo_lists();
    let mut p = default_policy();
    p.max_revise += 1;
    let custom = approved(p);
    let docs = classify(&custom, &[m("docs/a.md")], 10, &lists.trusted());
    assert_eq!(docs.class, DeliveryClass::Consequential, "never routine: {docs:?}");
    assert_eq!(docs.reviews, 1);
    assert_eq!(docs.review_kind, ReviewKind::Combined);
    let src = classify(&custom, &[m("src/foo.rs")], 10, &lists.trusted());
    assert_eq!(src.class, DeliveryClass::Consequential, "{src:?}");
    assert_eq!(src.reviews, 1);
    let cargo = classify(&custom, &[m("Cargo.toml")], 10, &lists.trusted());
    assert_eq!(cargo.class, DeliveryClass::Strict, "{cargo:?}");
    assert!(cargo.operator_approval);
}

// ---- unchanged behaviour (D2) ------------------------------------------

#[test]
fn custom_policy_in_the_file_but_not_approved_stays_strict() {
    let lists = repo_lists();
    let notes = [m("notes/x.md")];
    let docs = [m("docs/a.md")];
    let custom = gen_only();
    let from_file = Resolved {
        digest: digest(&custom),
        policy: custom.clone(),
        source: "file",
        note: None,
    };
    for (what, res) in [
        ("file source", from_file),
        (
            "unapproved note",
            Resolved {
                note: Some("delivery_unapproved: x".into()),
                ..approved(custom.clone())
            },
        ),
    ] {
        // Outside the one-review include list: strict with two reviews.
        let r = classify(&res, &notes, 10, &lists.trusted());
        assert_eq!(r.class, DeliveryClass::Strict, "{what}: {r:?}");
        assert_eq!(r.reviews, 2, "{what}: {r:?}");
        assert_eq!(r.review_kind, ReviewKind::StandardsAndSpec, "{what}");
        // On the include list: strict class, today's one combined review.
        let r = classify(&res, &docs, 10, &lists.trusted());
        assert_eq!(r.class, DeliveryClass::Strict, "{what}: {r:?}");
        assert_eq!(r.reviews, 1, "{what}: {r:?}");
    }
    // The product path: the file differs from the default and nothing is
    // approved, so `effective` falls back to the default policy with a note.
    let eff = delivery::effective("k", Ok(Some(custom)), None);
    assert_eq!(eff.source, "default");
    let r = classify(&eff, &docs, 10, &lists.trusted());
    assert_eq!(r.activation, Activation::NotApproved);
    assert_eq!(r.class, DeliveryClass::Strict, "{r:?}");
}

#[test]
fn an_approved_policy_changed_in_the_file_is_not_evaluated_until_reapproved() {
    let lists = repo_lists();
    let approved_policy = gen_only();
    let ap = delivery::Approved {
        digest: digest(&approved_policy),
        policy: Some(approved_policy.clone()),
    };
    // Matching file → evaluated.
    let same = delivery::effective("k", Ok(Some(approved_policy.clone())), Some(&ap));
    assert_eq!(same.source, "approved");
    assert!(same.note.is_none());
    let r = classify(&same, &[m("notes/x.md")], 10, &lists.trusted());
    assert_eq!(r.class, DeliveryClass::Consequential, "{r:?}");
    // File edited since approval → the approved policy stays, with a
    // `delivery_unapproved` note: strict, as before.
    let mut edited = approved_policy;
    edited.max_revise += 1;
    let drift = delivery::effective("k", Ok(Some(edited)), Some(&ap));
    assert_eq!(drift.source, "approved");
    assert!(drift.note.is_some());
    let r = classify(&drift, &[m("notes/x.md")], 10, &lists.trusted());
    assert_eq!(r.activation, Activation::NotApproved);
    assert_eq!(r.class, DeliveryClass::Strict, "{r:?}");
    assert_eq!(r.reviews, 2, "{r:?}");
}

#[test]
fn malformed_policy_stays_strict() {
    let lists = repo_lists();
    // A digest that does not match the policy, and a delivery_error note.
    let mut bad = approved(gen_only());
    bad.digest = "sha256:0".into();
    let mut err = approved(gen_only());
    err.note = Some("delivery_error: x".into());
    for res in [bad, err] {
        let r = classify(&res, &[m("notes/x.md")], 10, &lists.trusted());
        assert_eq!(r.activation, Activation::Malformed, "{r:?}");
        assert_eq!(r.class, DeliveryClass::Strict, "{r:?}");
        assert_eq!(r.reviews, 2, "{r:?}");
    }
}

#[test]
fn default_equivalent_approved_policy_keeps_todays_lean_classes() {
    let lists = repo_lists();
    let mut with_target = default_policy();
    with_target.integration_branch = Some("staging".into());
    for res in [approved(default_policy()), approved(with_target)] {
        let routine = classify(&res, &[m("docs/a.md")], 10, &lists.trusted());
        assert_eq!(routine.activation, Activation::Active);
        assert_eq!(routine.class, DeliveryClass::Routine, "{routine:?}");
        assert_eq!(routine.reviews, 0);
        let ord = classify(&res, &[m("src/foo.rs")], 10, &lists.trusted());
        assert_eq!(ord.class, DeliveryClass::Consequential, "{ord:?}");
        assert_eq!(ord.reviews, 1);
        let outside = classify(&res, &[m("notes/x.md")], 10, &lists.trusted());
        assert_eq!(outside.class, DeliveryClass::Strict, "{outside:?}");
        assert_eq!(outside.reviews, 2);
    }
}
