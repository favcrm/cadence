//! Independent acceptance checks for the routine fresh-ready observer transition.
//!
//! These exercise the real pure transition extracted from the observer. They
//! do not model evidence collection, caller identity, GitHub responses, or the
//! daemon's decision to invoke the helper; the production call site must remain
//! limited to the routine fresh-ready branch.

use super::apply_routine_ready_observation;
use crate::delivery::Observed;
use crate::delivery::{ReadyBinding, ReadyEvidence, Record, State};

type EvidenceMutation = (&'static str, fn(&mut ReadyEvidence));

const ISSUE: &str = "CAD-1298";
const REPO: &str = "favcrm/cadence";
const PR_URL: &str = "https://github.com/favcrm/cadence/pull/42";
const HEAD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const BASE: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const POLICY_DIGEST: &str =
    "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
const REPORT: &str = "CAD-1298/reports/outcome.md";
const CI_RUN_ID: u64 = 1234;
const CI_RUN_URL: &str = "https://github.com/favcrm/cadence/actions/runs/1234";

fn evidence() -> ReadyEvidence {
    ReadyEvidence {
        repo: REPO.to_string(),
        pr: 42,
        head: HEAD.to_string(),
        base: BASE.to_string(),
        policy_digest: POLICY_DIGEST.to_string(),
        ci_run_id: CI_RUN_ID,
        ci_run_url: CI_RUN_URL.to_string(),
        outcome_report: REPORT.to_string(),
    }
}

fn binding(evidence: &ReadyEvidence) -> ReadyBinding {
    ReadyBinding {
        repo: evidence.repo.clone(),
        pr: evidence.pr,
        head: evidence.head.clone(),
        base: evidence.base.clone(),
        policy_digest: evidence.policy_digest.clone(),
        ci_run_id: evidence.ci_run_id,
        ci_run_url: evidence.ci_run_url.clone(),
        outcome_report: evidence.outcome_report.clone(),
    }
}

fn record(state: State) -> Record {
    let proof = evidence();
    let mut record = Record::new(ISSUE, "cadence", "worker", 1);
    record.state = state;
    record.pr = Some(PR_URL.to_string());
    record.head = Some(HEAD.to_string());
    record.outcome_report = Some(REPORT.to_string());
    record.ready_binding = Some(binding(&proof));
    record.ready_evidence = Some(proof);
    record.observed = Some(Observed {
        head: HEAD.to_string(),
        pr_state: "OPEN".to_string(),
        ci_green: true,
        auto_merge: true,
        ..Observed::default()
    });
    record
}

fn apply(record: &mut Record, current: ReadyEvidence, was_disable: bool, now: i64) {
    apply_routine_ready_observation(record, current, was_disable, now);
}

#[test]
fn matching_fresh_routine_proof_promotes_ready_without_a_review_pass() {
    let mut record = record(State::Ready);
    assert!(
        record.verdict.is_none(),
        "fixture must not carry a fabricated PASS"
    );

    apply(&mut record, evidence(), false, 2);

    assert_eq!(record.state, State::Enqueued);
    assert_eq!(record.ready_evidence, Some(evidence()));
    assert_eq!(record.ready_binding, Some(binding(&evidence())));
    assert!(
        !record.merge_ready(),
        "an already-enqueued record is not a merge decision"
    );
    assert!(
        record.ready_key().is_none(),
        "an already-enqueued record must not wake another decision"
    );
    assert!(
        record.verdict.is_none(),
        "routine promotion must not manufacture a PASS"
    );
}

#[test]
fn changed_binding_invalidates_an_existing_routine_enqueue() {
    // Each changed field is independently part of the native binding. Keeping
    // the old evidence while presenting a changed current read must never
    // leave the old enqueue intent valid or establish a replacement proof.
    let mutations: [EvidenceMutation; 7] = [
        ("repository", |e| e.repo = "other/project".to_string()),
        ("PR", |e| e.pr += 1),
        ("head", |e| {
            e.head = "dddddddddddddddddddddddddddddddddddddddd".to_string()
        }),
        ("base", |e| {
            e.base = "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee".to_string()
        }),
        ("policy digest", |e| {
            e.policy_digest =
                "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd"
                    .to_string()
        }),
        ("CI run", |e| {
            e.ci_run_id += 1;
            e.ci_run_url = "https://github.com/favcrm/cadence/actions/runs/1235".to_string();
        }),
        ("outcome report", |e| {
            e.outcome_report = "CAD-1298/reports/other.md".to_string()
        }),
    ];

    for (field, mutate) in mutations {
        let mut record = record(State::Enqueued);
        let mut changed = evidence();
        mutate(&mut changed);
        apply(&mut record, changed, false, 3);

        assert_ne!(
            record.state,
            State::Enqueued,
            "changed {field} kept stale enqueue"
        );
        assert!(
            !record.merge_ready(),
            "changed {field} retained merge readiness"
        );
        assert_ne!(
            record.ready_evidence,
            record.ready_binding.as_ref().map(|b| ReadyEvidence {
                repo: b.repo.clone(),
                pr: b.pr,
                head: b.head.clone(),
                base: b.base.clone(),
                policy_digest: b.policy_digest.clone(),
                ci_run_id: b.ci_run_id,
                ci_run_url: b.ci_run_url.clone(),
                outcome_report: b.outcome_report.clone(),
            }),
            "changed {field} must not be installed as replacement proof while preserving old binding"
        );
    }
}

#[test]
fn disable_auto_confirmation_requires_a_later_fresh_queue_observation() {
    let mut record = record(State::Ready);

    // The caller reports that auto-merge had been disabled, but the current
    // read still sees it enabled: this observation cannot restore legitimacy.
    apply(&mut record, evidence(), true, 4);
    assert_ne!(record.state, State::Enqueued);

    // An actual off observation confirms the disable. It does not promote.
    record.observed.as_mut().unwrap().auto_merge = false;
    apply(&mut record, evidence(), true, 5);
    assert_ne!(record.state, State::Enqueued);

    // Only a subsequent, fresh external queue observation can promote again.
    record.observed.as_mut().unwrap().auto_merge = true;
    apply(&mut record, evidence(), false, 6);
    assert_eq!(record.state, State::Enqueued);
    assert_eq!(record.ready_evidence, Some(evidence()));
    assert_eq!(record.ready_binding, Some(binding(&evidence())));
    assert!(
        !record.merge_ready(),
        "an already-enqueued record is not a merge decision"
    );
    assert!(
        record.ready_key().is_none(),
        "an already-enqueued record must not wake another decision"
    );
}

#[test]
fn unqualified_or_unproven_current_observations_never_promote() {
    struct Case {
        name: &'static str,
        prior_state: State,
        current_head: &'static str,
        pr_state: &'static str,
        ci_green: bool,
        auto_merge: bool,
        has_prior_proof: bool,
    }

    let cases = [
        Case {
            name: "open but red CI",
            prior_state: State::Ready,
            current_head: HEAD,
            pr_state: "OPEN",
            ci_green: false,
            auto_merge: true,
            has_prior_proof: true,
        },
        Case {
            name: "closed PR",
            prior_state: State::Ready,
            current_head: HEAD,
            pr_state: "CLOSED",
            ci_green: true,
            auto_merge: true,
            has_prior_proof: true,
        },
        Case {
            name: "foreign head",
            prior_state: State::Ready,
            current_head: "ffffffffffffffffffffffffffffffffffffffff",
            pr_state: "OPEN",
            ci_green: true,
            auto_merge: true,
            has_prior_proof: true,
        },
        Case {
            name: "auto-merge off",
            prior_state: State::Ready,
            current_head: HEAD,
            pr_state: "OPEN",
            ci_green: true,
            auto_merge: false,
            has_prior_proof: true,
        },
        Case {
            name: "missing prior proof",
            prior_state: State::Ready,
            current_head: HEAD,
            pr_state: "OPEN",
            ci_green: true,
            auto_merge: true,
            has_prior_proof: false,
        },
        Case {
            name: "already enqueued with missing proof",
            prior_state: State::Enqueued,
            current_head: HEAD,
            pr_state: "OPEN",
            ci_green: true,
            auto_merge: true,
            has_prior_proof: false,
        },
    ];

    for case in cases {
        let mut record = record(case.prior_state);
        record.observed.as_mut().unwrap().head = case.current_head.to_string();
        record.observed.as_mut().unwrap().pr_state = case.pr_state.to_string();
        record.observed.as_mut().unwrap().ci_green = case.ci_green;
        record.observed.as_mut().unwrap().auto_merge = case.auto_merge;
        if !case.has_prior_proof {
            record.ready_evidence = None;
            record.ready_binding = None;
        }
        apply(&mut record, evidence(), false, 7);

        assert_ne!(
            record.state,
            State::Enqueued,
            "{} promoted without qualifying evidence",
            case.name
        );
    }
}
