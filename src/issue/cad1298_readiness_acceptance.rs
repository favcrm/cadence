//! Independent acceptance checks for native no-review readiness receipts.
//! Expected outcomes are fixed in `/tmp/cad1297-advisory-01a11/acceptance-spec.md`.

use crate::delivery::{Observed, ReadyBinding, ReadyEvidence, Record, State};

const ISSUE: &str = "CAD-1298";
const REPO: &str = "acme/cadence";
const PR: u64 = 42;
const HEAD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const BASE: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const POLICY: &str = "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
const RUN_ID: u64 = 123456;
const REPORT: &str = "CAD-1298/reports/worker-done.md";

fn evidence() -> ReadyEvidence {
    ReadyEvidence {
        repo: REPO.into(),
        pr: PR,
        head: HEAD.into(),
        base: BASE.into(),
        policy_digest: POLICY.into(),
        ci_run_id: RUN_ID,
        ci_run_url: format!("https://github.com/{REPO}/actions/runs/{RUN_ID}"),
        outcome_report: REPORT.into(),
    }
}

fn binding() -> ReadyBinding {
    let e = evidence();
    ReadyBinding {
        repo: e.repo,
        pr: e.pr,
        head: e.head,
        base: e.base,
        policy_digest: e.policy_digest,
        ci_run_id: e.ci_run_id,
        ci_run_url: e.ci_run_url,
        outcome_report: e.outcome_report,
    }
}

fn ready_record() -> Record {
    let mut record = Record::new(ISSUE, "cadence", "worker", 0);
    record.state = State::Ready;
    record.pr = Some(format!("https://github.com/{REPO}/pull/{PR}"));
    record.head = Some(HEAD.into());
    record.outcome_report = Some(REPORT.into());
    record.ready_evidence = Some(evidence());
    record.ready_binding = Some(binding());
    record.observed = Some(Observed {
        head: HEAD.into(),
        pr_state: "OPEN".into(),
        ci_green: true,
        ..Observed::default()
    });
    record
}

#[test]
fn cad1298_ready_acceptance_native_receipt_is_distinct_from_a_verdict() {
    let record = ready_record();
    assert!(record.merge_ready());
    assert!(
        record.passed_sha().is_none(),
        "readiness must not fabricate PASS"
    );
    let key = record
        .ready_key()
        .expect("fresh receipt should have a wake key");
    assert!(key.contains(REPO));
    assert!(key.contains(&PR.to_string()));
    assert!(key.contains(HEAD));
    assert!(key.contains(BASE));
    assert!(key.contains(POLICY));
    assert!(key.contains(&RUN_ID.to_string()));
    assert!(key.contains(REPORT));

    // A new current authority binding changes the wake key without requiring
    // or manufacturing a reviewer verdict.
    let mut moved = ready_record();
    let next = ReadyEvidence {
        base: "dddddddddddddddddddddddddddddddddddddddd".into(),
        ..evidence()
    };
    let next_binding = ReadyBinding {
        repo: next.repo.clone(),
        pr: next.pr,
        head: next.head.clone(),
        base: next.base.clone(),
        policy_digest: next.policy_digest.clone(),
        ci_run_id: next.ci_run_id,
        ci_run_url: next.ci_run_url.clone(),
        outcome_report: next.outcome_report.clone(),
    };
    moved.ready_evidence = Some(next);
    moved.ready_binding = Some(next_binding);
    assert!(moved.merge_ready());
    assert_ne!(moved.ready_key(), Some(key));
    assert!(moved.passed_sha().is_none());
}

#[test]
fn cad1298_ready_acceptance_stale_receipt_or_binding_refuses() {
    // Every binding coordinate is part of the receipt equality check.
    for mutate in [
        0_u8, // repo
        1,    // PR
        2,    // head
        3,    // base
        4,    // policy digest
        5,    // workflow run id
        6,    // workflow run URL
        7,    // worker outcome report
    ] {
        let mut record = ready_record();
        let b = record.ready_binding.as_mut().unwrap();
        match mutate {
            0 => b.repo = "other/cadence".into(),
            1 => b.pr += 1,
            2 => b.head = "dddddddddddddddddddddddddddddddddddddddd".into(),
            3 => b.base = "dddddddddddddddddddddddddddddddddddddddd".into(),
            4 => {
                b.policy_digest =
                    "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd".into()
            }
            5 => b.ci_run_id += 1,
            6 => b.ci_run_url = "https://github.com/acme/cadence/actions/runs/7".into(),
            7 => b.outcome_report = "CAD-1298/reports/other.md".into(),
            _ => unreachable!(),
        }
        assert!(!record.merge_ready(), "changed binding coordinate {mutate}");
        assert_eq!(record.ready_key(), None);
    }

    // Historical receipt cannot outlive movement in the current record or
    // observation, even if the historical evidence and binding still agree.
    let mut moved_head = ready_record();
    moved_head.head = Some("dddddddddddddddddddddddddddddddddddddddd".into());
    assert!(!moved_head.merge_ready());

    let mut moved_pr = ready_record();
    moved_pr.pr = Some(format!("https://github.com/{REPO}/pull/43"));
    assert!(!moved_pr.merge_ready());

    let mut moved_report = ready_record();
    moved_report.outcome_report = Some("CAD-1298/reports/other.md".into());
    assert!(!moved_report.merge_ready());
}

#[test]
fn cad1298_ready_acceptance_missing_or_stale_live_observation_refuses() {
    for observation in [
        None,
        Some(Observed {
            head: HEAD.into(),
            pr_state: "CLOSED".into(),
            ci_green: true,
            ..Observed::default()
        }),
        Some(Observed {
            head: HEAD.into(),
            pr_state: "OPEN".into(),
            ci_green: false,
            ..Observed::default()
        }),
        Some(Observed {
            head: "dddddddddddddddddddddddddddddddddddddddd".into(),
            pr_state: "OPEN".into(),
            ci_green: true,
            ..Observed::default()
        }),
    ] {
        let mut record = ready_record();
        record.observed = observation;
        assert!(!record.merge_ready());
    }

    let mut no_receipt = ready_record();
    no_receipt.ready_evidence = None;
    assert!(!no_receipt.merge_ready());

    let mut no_fresh_binding = ready_record();
    no_fresh_binding.ready_binding = None;
    assert!(!no_fresh_binding.merge_ready());

    let mut wrong_state = ready_record();
    wrong_state.state = State::Working;
    assert!(!wrong_state.merge_ready());
}

#[test]
fn cad1298_ready_acceptance_requires_canonical_receipt_fields() {
    let mut bad_repo = ready_record();
    bad_repo.ready_evidence.as_mut().unwrap().repo = "Acme/cadence".into();
    bad_repo.ready_binding.as_mut().unwrap().repo = "Acme/cadence".into();
    assert!(!bad_repo.merge_ready());

    let mut bad_head = ready_record();
    bad_head.ready_evidence.as_mut().unwrap().head = "A".repeat(40);
    bad_head.ready_binding.as_mut().unwrap().head = "A".repeat(40);
    bad_head.head = Some("A".repeat(40));
    bad_head.observed.as_mut().unwrap().head = "A".repeat(40);
    assert!(!bad_head.merge_ready());

    let mut bad_base = ready_record();
    bad_base.ready_evidence.as_mut().unwrap().base = "z".repeat(40);
    bad_base.ready_binding.as_mut().unwrap().base = "z".repeat(40);
    assert!(!bad_base.merge_ready());

    let mut bad_policy = ready_record();
    bad_policy.ready_evidence.as_mut().unwrap().policy_digest = "sha256:Z".to_string();
    bad_policy.ready_binding.as_mut().unwrap().policy_digest = "sha256:Z".to_string();
    assert!(!bad_policy.merge_ready());

    let mut bad_run = ready_record();
    bad_run.ready_evidence.as_mut().unwrap().ci_run_id = 0;
    bad_run.ready_binding.as_mut().unwrap().ci_run_id = 0;
    bad_run.ready_evidence.as_mut().unwrap().ci_run_url =
        format!("https://github.com/{REPO}/actions/runs/0");
    bad_run.ready_binding.as_mut().unwrap().ci_run_url =
        format!("https://github.com/{REPO}/actions/runs/0");
    assert!(!bad_run.merge_ready());

    let mut bad_url = ready_record();
    bad_url.ready_evidence.as_mut().unwrap().ci_run_url =
        "https://github.com/acme/cadence/actions/runs/999".into();
    bad_url.ready_binding.as_mut().unwrap().ci_run_url =
        "https://github.com/acme/cadence/actions/runs/999".into();
    assert!(!bad_url.merge_ready());

    for report in [
        "OTHER/reports/worker-done.md",
        "CAD-1298/reports/../worker-done.md",
        "CAD-1298/reports/nested/worker-done.md",
        "CAD-1298/reports/worker-done.txt",
    ] {
        let mut bad_report = ready_record();
        bad_report.ready_evidence.as_mut().unwrap().outcome_report = report.into();
        bad_report.ready_binding.as_mut().unwrap().outcome_report = report.into();
        bad_report.outcome_report = Some(report.into());
        assert!(
            !bad_report.merge_ready(),
            "accepted invalid report {report}"
        );
    }
}

#[test]
fn cad1298_ready_acceptance_legacy_passed_state_still_requires_exact_green_head() {
    let mut record = ready_record();
    record.state = State::Passed;
    record.ready_evidence = None;
    record.ready_binding = None;
    record.verdict = Some(crate::delivery::VerdictRec {
        verdict: "pass".into(),
        sha: HEAD.into(),
        reviewer: "independent-reviewer".into(),
        summary: "pass".into(),
        report: format!("{ISSUE}/reports/review.md"),
        at: 0,
    });

    assert!(
        record.merge_ready(),
        "legacy PASS behavior remains supported"
    );
    record.observed.as_mut().unwrap().head = "dddddddddddddddddddddddddddddddddddddddd".into();
    assert!(
        !record.merge_ready(),
        "a moved head invalidates the old PASS"
    );
}
