//! Independent CAD-1561 acceptance for the operator-approved
//! integration-branch delivery target.
//!
//! Every check goes through the real production path —
//! `classify`, `routine_readiness` and
//! `Shared::rpc_delivery_requirements` — against an isolated state
//! dir, PM root and `gh` executable (the cad1403 fixture shape). The
//! fake `gh` supplies deterministic transport facts only: it cannot
//! approve a policy, classify a diff or green a check the production
//! reader did not actually request. The approvals recorded here are
//! isolated-store test input — no live operator approval is claimed.
//!
//! Proven by the real guards, not by reading source or echoing values:
//! - the repository-default route is unchanged (accepts, `base_ref`
//!   binds `main`, no approval needed);
//! - a non-default PR base is admitted only while the EXACT current
//!   `delivery:` policy is operator-approved (digest/provenance match)
//!   and names that branch as `integration_branch` — then the
//!   response and the readiness evidence bind the actual target name
//!   and the routine floor reads THAT branch's rules/protection;
//! - an arbitrary target, an unapproved or digest-stale policy, a
//!   drifted file (last-approved `delivery_unapproved` fallback),
//!   a malformed section or a non-canonical branch name all refuse;
//! - the actual target's required checks cannot be substituted by the
//!   default branch's green checks;
//! - the legacy `solo_operator` v1 profile stays orthogonal to the
//!   new routing option (approved profile+target still admits and
//!   keeps routine reachable on the actual target's checks).

#[cfg(all(unix, feature = "test-seam"))]
mod cad1561 {
    use super::super::{classify, routine_readiness, RoutineReadiness};
    use crate::daemon::{ServeOptions, Shared};
    use crate::delivery::{self, Record};
    use crate::issue::{self, delivery_policy};
    use serde_json::json;
    use std::collections::BTreeMap;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    const ISSUE: &str = "CAD-1561";
    const REPO: &str = "acme/widgets";
    const PR: u64 = 1561;
    const HEAD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const BASE: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const MERGE_BASE: &str = "cccccccccccccccccccccccccccccccccccccccc";
    const REPORT: &str = "CAD-1561/reports/19700101T000000Z-worker.md";
    const DEFAULT_BRANCH: &str = "main";
    const INTEGRATION_BRANCH: &str = "staging";

    struct Fixture {
        _root: tempfile::TempDir,
        shared: std::sync::Arc<Shared>,
        pm_dir: std::path::PathBuf,
    }

    /// `<pm>/widgets/PROJECT.md` holding one `delivery:` section.
    fn project_md(delivery_yaml: &str) -> String {
        format!("---\ndelivery:\n{delivery_yaml}---\n# Widgets\n")
    }

    /// Write the PROJECT.md a classify call will read on its next pass.
    fn write_policy(pm_dir: &std::path::Path, delivery_yaml: &str) {
        std::fs::write(
            pm_dir.join("widgets").join("PROJECT.md"),
            project_md(delivery_yaml),
        )
        .unwrap();
    }

    /// A valid section identical to the defaults plus
    /// `integration_branch: <name>` — the only admissible non-default
    /// target declaration.
    fn staging_delivery(branch: &str) -> String {
        format!(
            "  integration_branch: {branch}\n  reviews:\n    review:\n      kind: agent\n      focus: general\n  risk:\n    - require: [review]\n"
        )
    }

    /// The same approved-target section plus the legacy solo-operator
    /// profile — `integration_branch` is a routing option and must stay
    /// orthogonal to the review profile.
    fn staging_solo_delivery() -> String {
        format!(
            "  integration_branch: {INTEGRATION_BRANCH}\n  solo_operator:\n    version: 1\n  reviews:\n    review:\n      kind: agent\n      focus: general\n  risk:\n    - require: [review]\n"
        )
    }

    /// Record a `project_work_approved` event in the ISOLATED store —
    /// test input only, standing in for a real `project_work_approve`
    /// by the operator. `policy` is the validated canonical policy
    /// half the recorded digest covers.
    fn record_work_approval(shared: &Shared, policy: Option<&delivery_policy::DeliveryPolicy>) {
        let delivery_json = match policy {
            Some(p) => serde_json::to_value(p).expect("policy serializes"),
            None => serde_json::Value::Null,
        };
        let policy_for_digest = policy
            .cloned()
            .unwrap_or_else(delivery_policy::default_policy);
        shared
            .store
            .record_work_approval(json!({
                "project": "widgets",
                "digest": issue::work::gate_digest(&issue::work::WorkConfig::default()),
                "delivery": delivery_json,
                "delivery_digest": delivery_policy::digest(&policy_for_digest),
                "by": "operator",
                "at": issue::time::iso(0),
            }))
            .expect("seed isolated approval store");
    }

    /// Parse the `delivery:` section just written to the fixture
    /// PROJECT.md — the same canonical policy `classify` resolves —
    /// for building an approval's recorded halves.
    fn file_policy(pm_dir: &std::path::Path) -> delivery_policy::DeliveryPolicy {
        delivery_policy::load(pm_dir, "widgets")
            .expect("fixture delivery section must parse")
            .expect("fixture wrote a delivery section")
    }

    /// The isolated PM + daemon + fake-gh environment. `base_ref` is
    /// the PR's `baseRefName`; `base_rules`/`base_protection` are the
    /// rule and protection JSON the API returns for the PR's ACTUAL
    /// base branch — a reader that silently substitutes the default
    /// branch hits the distinct main-shaped arms instead. `rollup` is
    /// the PR's requested statusCheckRollup payload.
    fn fixture(base_ref: &str, rollup: &str, base_rules: &str, base_protection: &str) -> Fixture {
        let root = tempdir().expect("isolated CAD-1561 root");
        let pm_dir = root.path().join("pm");
        let pm = issue::Pm::init(&pm_dir).expect("initialize isolated PM");
        let checkout = root.path().join("widgets");
        std::fs::create_dir_all(&checkout).unwrap();
        for args in [
            vec!["init", "-q"],
            vec![
                "remote",
                "add",
                "origin",
                "https://github.com/acme/widgets.git",
            ],
        ] {
            assert!(std::process::Command::new("git")
                .arg("-C")
                .arg(&checkout)
                .args(&args)
                .status()
                .unwrap()
                .success());
        }
        issue::write::project_add(
            &pm,
            "widgets",
            "CAD",
            &[checkout.to_str().unwrap().into()],
            &[],
            &[],
            None,
        )
        .expect("register fixture project");
        issue::write::new_issue(
            &pm,
            &checkout,
            Some("widgets"),
            "Approved integration-branch target",
            None,
            None,
            &[],
            None,
            None,
            &[],
            Some(ISSUE),
            None,
            "operator",
        )
        .expect("create fixture issue");
        let report_path = pm_dir
            .join("widgets")
            .join(ISSUE)
            .join("reports")
            .join("19700101T000000Z-worker.md");
        std::fs::create_dir_all(report_path.parent().unwrap()).unwrap();
        std::fs::write(&report_path, format!("---\nschema: cadence.report/2\nkind: done\ntask: {ISSUE}\nagent: worker\nsha: {HEAD}\npr: https://github.com/{REPO}/pull/{PR}\n---\n## Expected\nready\n## Evidence\nchecks\n## Cause\n\n## Correction\n\n## Lesson\n\n## Next\n\n")).unwrap();

        let risk_paths = include_str!("../../docs/roles/risk-paths.toml");
        let one_review = include_str!("../../docs/roles/one-review-paths.toml");
        let risk_content =
            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, risk_paths);
        let review_content =
            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, one_review);
        let gh = root.path().join("gh");
        let script = r##"#!/bin/sh
if [ "$1" = pr ] && [ "$2" = view ]; then
  case "$*" in
    *statusCheckRollup*) rollup=',"statusCheckRollup":[%ROLLUP%]' ;;
    *) rollup='' ;;
  esac
  printf '{"number":%PR%,"state":"OPEN","title":"CAD-1561: fixture","headRefOid":"%HEAD%","baseRefName":"%BRANCH%","baseRefOid":"%OID%","changedFiles":1%s}\n' "$rollup"
  exit 0
fi
if [ "$1" = repo ] && [ "$2" = view ]; then printf '{"defaultBranchRef":{"name":"%DEF%"}}\n'; exit 0; fi
if [ "$1" = api ]; then
  endpoint=$2
  case "$endpoint" in
    repos/%REPO%/compare/%OID%...%HEAD%) printf '{"merge_base_commit":{"sha":"%MBASE%"},"files":[{"filename":"docs/guide.md","status":"modified","additions":1,"deletions":0}]}\n' ;;
    repos/%REPO%/git/trees/%MBASE%?recursive=1|repos/%REPO%/git/trees/%HEAD%?recursive=1) printf '{"truncated":false,"tree":[{"path":"docs/guide.md","mode":"100644","type":"blob"}]}\n' ;;
    repos/%REPO%/contents/docs/roles/risk-paths.toml\?ref=%OID%) printf '{"encoding":"base64","content":"%RISK%"}\n' ;;
    repos/%REPO%/contents/docs/roles/one-review-paths.toml\?ref=%OID%) printf '{"encoding":"base64","content":"%REVIEW%"}\n' ;;
    # The default branch's own rules/protection: required context "ci".
    # A reader binding the default branch instead of the actual target
    # reads THESE arms — exactly the substitution refused below.
    repos/%REPO%/rules/branches/%DEF%) printf '[{"type":"required_status_checks","parameters":{"required_status_checks":[{"context":"ci"}]}}]\n' ;;
    repos/%REPO%/branches/%DEF%/protection) printf '{"required_status_checks":{"contexts":["ci"],"checks":[]}}\n' ;;
    # The PR's actual target branch, whatever it is named.
    repos/%REPO%/rules/branches/%BRANCH%) printf '%s\n' '%BRULES%' ;;
    repos/%REPO%/branches/%BRANCH%/protection) printf '%s\n' '%BPROT%' ;;
    repos/%REPO%/actions/workflows/ci.yml/runs*) printf '{"workflow_runs":[{"id":1561,"head_sha":"%HEAD%","event":"pull_request","status":"completed","conclusion":"success","html_url":"https://github.com/%REPO%/actions/runs/1561"}]}\n' ;;
    *) echo "unhandled API fixture: $endpoint" >&2; exit 1 ;;
  esac
  exit 0
fi
echo "unhandled gh fixture: $*" >&2
exit 1
"##
        .replace("%ROLLUP%", rollup)
        .replace("%BRULES%", base_rules)
        .replace("%BPROT%", base_protection)
        .replace("%BRANCH%", base_ref)
        .replace("%MBASE%", MERGE_BASE)
        .replace("%DEF%", DEFAULT_BRANCH)
        .replace("%REPO%", REPO)
        .replace("%RISK%", &risk_content)
        .replace("%REVIEW%", &review_content)
        .replace("%PR%", &PR.to_string())
        .replace("%HEAD%", HEAD)
        .replace("%OID%", BASE);
        std::fs::write(&gh, script).unwrap();
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();

        let mut record = Record::new(ISSUE, "widgets", "worker", 0);
        record.dispatched_at = 0;
        record.pr = Some(format!("https://github.com/{REPO}/pull/{PR}"));
        record.head = Some(HEAD.into());
        record.outcome_report = Some(REPORT.into());
        let mut records = BTreeMap::new();
        records.insert(ISSUE.into(), record);
        delivery::save(root.path(), &records).expect("save worker record");

        let opts = ServeOptions {
            test_seam: true,
            delivery_gh: Some(gh),
            ..Default::default()
        };
        opts.provider_env
            .set("CADENCE_PM_DIR", pm_dir.to_str().unwrap());
        let shared = Shared::new(root.path(), &opts).expect("initialize isolated daemon");
        Fixture {
            pm_dir,
            shared,
            _root: root,
        }
    }

    fn pr_url() -> String {
        format!("https://github.com/{REPO}/pull/{PR}")
    }

    fn request() -> serde_json::Value {
        json!({"issue": ISSUE, "pr": pr_url(), "head": HEAD})
    }

    /// `Err` or a non-`Ready` outcome both refuse; only `Ready` admits.
    fn readiness_refused(result: &crate::error::Result<RoutineReadiness>) -> bool {
        !matches!(result, Ok(RoutineReadiness::Ready(_)))
    }

    fn readiness_name(result: &crate::error::Result<RoutineReadiness>) -> &'static str {
        match result {
            Ok(RoutineReadiness::Ready(_)) => "Ready",
            Ok(RoutineReadiness::Waiting) => "Waiting",
            Ok(RoutineReadiness::NotRoutine) => "NotRoutine",
            Err(_) => "Err",
        }
    }

    /// Baseline: the repository-default route is unchanged. A `main`
    /// PR classifies with `base_ref` bound to `main` under no
    /// approval, and a routine docs diff readies on the default
    /// branch's own checks.
    #[test]
    fn cad1561_default_branch_route_is_unchanged() {
        let f = fixture(
            DEFAULT_BRANCH,
            r#"{"context":"ci","state":"SUCCESS"}"#,
            "[]",
            "{}",
        );
        let answer = classify(&f.shared, &request()).expect("default-branch PR must classify");
        assert_eq!(answer["base"], BASE, "base stays the exact base OID");
        assert_eq!(
            answer["base_ref"], DEFAULT_BRANCH,
            "the response must bind the actual target name: {answer}"
        );
        let ready = routine_readiness(&f.shared, ISSUE, &pr_url(), HEAD, REPORT);
        assert!(
            matches!(ready, Ok(RoutineReadiness::Ready(_))),
            "routine docs diff on the default branch readies on its own checks: {}",
            readiness_name(&ready)
        );
    }

    /// Admission + target binding: a PR to `staging` is admitted while
    /// the exact current policy — canonical digest equal to the
    /// recorded operator approval — names `integration_branch:
    /// staging`. The response binds `base_ref: staging` and the
    /// routine floor evaluates the STAGING branch's own required check
    /// `ci-staging`, not main's `ci`. `main` green checks are present
    /// in the rollup and remain irrelevant to the staging gate.
    #[test]
    fn cad1561_approved_integration_branch_binds_actual_target() {
        // `main` green, staging's required context green: only the
        // actual target's rules can produce Ready here.
        let f = fixture(
            INTEGRATION_BRANCH,
            r#"{"context":"ci","state":"SUCCESS"},{"context":"ci-staging","state":"SUCCESS"}"#,
            r#"[{"type":"required_status_checks","parameters":{"required_status_checks":[{"context":"ci-staging"}]}}]"#,
            r#"{"required_status_checks":{"contexts":[],"checks":[]}}"#,
        );
        write_policy(&f.pm_dir, &staging_delivery(INTEGRATION_BRANCH));
        let policy = file_policy(&f.pm_dir);
        record_work_approval(&f.shared, Some(&policy));

        let answer = classify(&f.shared, &request())
            .expect("freshly approved integration target must classify");
        assert_eq!(
            answer["base_ref"], INTEGRATION_BRANCH,
            "the response must name the actual approved target: {answer}"
        );
        assert_eq!(answer["base"], BASE);
        assert_eq!(
            answer["requirements"]["policy_digest"], answer["policy_digest"],
            "requirements bind the approved policy digest"
        );

        // A correct implementation reads staging's rules (requiring
        // `ci-staging`, which is green); one that substituted main's
        // `ci` would still see it green — the missing/failing rows
        // below disprove the substitution in the other direction.
        let ready = routine_readiness(&f.shared, ISSUE, &pr_url(), HEAD, REPORT);
        let evidence = match ready {
            Ok(RoutineReadiness::Ready(e)) => e,
            other => panic!(
                "staging's own green required check must ready the routine floor: {}",
                readiness_name(&other)
            ),
        };
        let evidence_json = serde_json::to_value(&evidence).unwrap();
        assert_eq!(
            evidence_json["base_ref"], INTEGRATION_BRANCH,
            "readiness evidence must bind the actual target name: {evidence_json}"
        );
        assert_eq!(evidence.base, BASE, "evidence binds the exact base OID");
        assert_eq!(
            evidence.policy_digest,
            answer["policy_digest"].as_str().unwrap(),
            "evidence binds the approved policy digest"
        );

        // The public RPC surface carries the same binding.
        let rpc = f
            .shared
            .rpc_delivery_requirements(&request())
            .expect("requirements RPC");
        assert_eq!(rpc["base_ref"], INTEGRATION_BRANCH, "wire binds base_ref");
        assert_eq!(
            rpc["readiness"]["base_ref"], INTEGRATION_BRANCH,
            "wire readiness binds the actual target name: {rpc}"
        );
    }

    /// Legacy profile compatibility: `solo_operator: {version: 1}` is
    /// orthogonal to the new routing option. The approved policy below
    /// is default-equivalent plus `integration_branch: staging` plus
    /// the supported profile — it must validate, admit the staging
    /// target and keep the routine class reachable on the staging
    /// branch's own checks, exactly as the profile does on `main`.
    #[test]
    fn cad1561_solo_operator_profile_stays_compatible_with_integration_target() {
        let f = fixture(
            INTEGRATION_BRANCH,
            r#"{"context":"ci","state":"SUCCESS"},{"context":"ci-staging","state":"SUCCESS"}"#,
            r#"[{"type":"required_status_checks","parameters":{"required_status_checks":[{"context":"ci-staging"}]}}]"#,
            r#"{"required_status_checks":{"contexts":[],"checks":[]}}"#,
        );
        write_policy(&f.pm_dir, &staging_solo_delivery());
        let policy = file_policy(&f.pm_dir);
        record_work_approval(&f.shared, Some(&policy));

        let answer =
            classify(&f.shared, &request()).expect("approved profile+target policy must classify");
        assert_eq!(answer["base_ref"], INTEGRATION_BRANCH);
        assert_eq!(
            answer["requirements"]["class"], "routine",
            "the supported profile keeps routine reachable on the approved target: {answer}"
        );
        let ready = routine_readiness(&f.shared, ISSUE, &pr_url(), HEAD, REPORT);
        assert!(
            matches!(ready, Ok(RoutineReadiness::Ready(_))),
            "profile+target policy readies on staging's own green check: {}",
            readiness_name(&ready)
        );
    }

    /// Refusal of every non-approved route to a non-default base:
    /// - arbitrary: the PR targets `release-candidate`, which no valid
    ///   current approved policy names;
    /// - unapproved: the file names `staging` with no recorded approval
    ///   (the section is not the default → `delivery_unapproved`, the
    ///   default applies);
    /// - digest-stale: an approval exists but for a DIFFERENT policy
    ///   (`integration_branch: release-candidate`) than the current
    ///   file;
    /// - removed: the approved section was deleted — the last-approved
    ///   `delivery_unapproved` fallback still reports
    ///   `source=approved` yet carries a note, and must NOT admit;
    /// - malformed section and non-canonical branch names: the file
    ///   cannot even be a valid current policy.
    ///
    /// In every case the resolved policy still requires the default
    /// branch, or the binding approval is absent/not-current, so
    /// `staging` must refuse.
    #[test]
    fn cad1561_unapproved_arbitrary_malformed_and_drifted_targets_refuse() {
        // The whole row set is refused on ONE fixture whose PR targets
        // staging with staging's own checks green — so every refusal is
        // a target-selection refusal, never a missing-check accident.
        let f = fixture(
            INTEGRATION_BRANCH,
            r#"{"context":"ci","state":"SUCCESS"},{"context":"ci-staging","state":"SUCCESS"}"#,
            r#"[{"type":"required_status_checks","parameters":{"required_status_checks":[{"context":"ci-staging"}]}}]"#,
            r#"{"required_status_checks":{"contexts":[],"checks":[]}}"#,
        );

        // Unapproved file: custom section, no approval at all.
        write_policy(&f.pm_dir, &staging_delivery(INTEGRATION_BRANCH));
        assert!(
            classify(&f.shared, &request()).is_err(),
            "an unapproved custom policy must not admit a non-default target"
        );
        assert!(
            readiness_refused(&routine_readiness(
                &f.shared,
                ISSUE,
                &pr_url(),
                HEAD,
                REPORT
            )),
            "an unapproved custom policy must not ready a non-default target"
        );

        // Digest-stale: approval names release-candidate; the file says
        // staging. The approved fallback stays in force with a note
        // and must not admit staging.
        write_policy(&f.pm_dir, &staging_delivery("release-candidate"));
        let stale_policy = file_policy(&f.pm_dir);
        record_work_approval(&f.shared, Some(&stale_policy));
        write_policy(&f.pm_dir, &staging_delivery(INTEGRATION_BRANCH));
        assert!(
            classify(&f.shared, &request()).is_err(),
            "a policy digest-stale against its approval must not admit"
        );

        // Removed section: the recorded approval IS the staging
        // policy and the file is deleted afterward, so the resolver
        // keeps the approved policy in force under
        // `delivery_unapproved` — the fallback would authorize staging
        // if a note were honored; it must not be.
        let staging_policy = file_policy(&f.pm_dir);
        record_work_approval(&f.shared, Some(&staging_policy));
        std::fs::remove_file(f.pm_dir.join("widgets").join("PROJECT.md")).unwrap();
        assert!(
            classify(&f.shared, &request()).is_err(),
            "the last-approved fallback (note present) must not admit a non-default target"
        );
        assert!(
            readiness_refused(&routine_readiness(
                &f.shared,
                ISSUE,
                &pr_url(),
                HEAD,
                REPORT
            )),
            "the fallback must not ready a non-default target"
        );

        // Malformed section: not valid YAML — with the staging
        // approval still on record even a resolving fallback must not
        // admit; the current file is not a valid approved policy.
        write_policy(&f.pm_dir, "  integration_branch: [unclosed\n");
        assert!(
            classify(&f.shared, &request()).is_err(),
            "a malformed delivery section must refuse a non-default target"
        );

        // Non-canonical branch names in a current file: ref syntax,
        // traversal, revision and glob forms are never valid targets.
        // `load` must refuse the NAME at parse — without that first
        // assertion a wrongly-accepted name could still be refused only
        // for lacking an approval and the parser defect would hide.
        for name in [
            "refs/heads/staging",
            "../staging",
            "staging@{1}",
            "feature*",
            "-staging",
            "staging.lock",
            "feature.lock/staging",
            "feature..x",
        ] {
            write_policy(&f.pm_dir, &staging_delivery(name));
            assert!(
                delivery_policy::load(&f.pm_dir, "widgets").is_err(),
                "the parser must refuse non-canonical branch name {name:?}"
            );
            assert!(
                classify(&f.shared, &request()).is_err(),
                "non-canonical branch name {name:?} must refuse"
            );
        }

        // Arbitrary target: the approved policy names staging, but a
        // PR to a different branch is not covered by it.
        let other = fixture(
            "release-candidate",
            r#"{"context":"ci","state":"SUCCESS"},{"context":"ci-staging","state":"SUCCESS"}"#,
            r#"[{"type":"required_status_checks","parameters":{"required_status_checks":[{"context":"ci-staging"}]}}]"#,
            r#"{"required_status_checks":{"contexts":[],"checks":[]}}"#,
        );
        write_policy(&other.pm_dir, &staging_delivery(INTEGRATION_BRANCH));
        let approved = file_policy(&other.pm_dir);
        record_work_approval(&other.shared, Some(&approved));
        assert!(
            classify(&other.shared, &request()).is_err(),
            "an approved staging policy must not admit an arbitrary other branch"
        );
        assert!(
            readiness_refused(&routine_readiness(
                &other.shared,
                ISSUE,
                &pr_url(),
                HEAD,
                REPORT
            )),
            "an arbitrary branch must not ready"
        );
    }

    /// No substitution of default-branch checks for actual-target
    /// checks. The PR targets staging; `main` requires `ci` and its
    /// rollup entry is SUCCESS — while staging's OWN required
    /// `ci-staging` is absent or failing. A reader that consulted the
    /// default branch would wrongly find a green floor; the real
    /// guard must not admit.
    #[test]
    fn cad1561_actual_target_checks_cannot_substitute_default_branch_checks() {
        // Missing: staging's required context never reported.
        let missing = fixture(
            INTEGRATION_BRANCH,
            r#"{"context":"ci","state":"SUCCESS"}"#,
            r#"[{"type":"required_status_checks","parameters":{"required_status_checks":[{"context":"ci-staging"}]}}]"#,
            r#"{"required_status_checks":{"contexts":[],"checks":[]}}"#,
        );
        write_policy(&missing.pm_dir, &staging_delivery(INTEGRATION_BRANCH));
        let policy = file_policy(&missing.pm_dir);
        record_work_approval(&missing.shared, Some(&policy));
        let ready = routine_readiness(&missing.shared, ISSUE, &pr_url(), HEAD, REPORT);
        assert!(
            readiness_refused(&ready),
            "missing staging required check must not ready on main's green ci: {}",
            readiness_name(&ready)
        );
        let rpc = missing
            .shared
            .rpc_delivery_requirements(&request())
            .expect("requirements RPC");
        assert!(
            rpc["readiness"].is_null(),
            "wire readiness must not fabricate a staging pass: {rpc}"
        );

        // Failing: staging's required context reported FAILURE.
        let failing = fixture(
            INTEGRATION_BRANCH,
            r#"{"context":"ci","state":"SUCCESS"},{"context":"ci-staging","state":"FAILURE"}"#,
            r#"[{"type":"required_status_checks","parameters":{"required_status_checks":[{"context":"ci-staging"}]}}]"#,
            r#"{"required_status_checks":{"contexts":[],"checks":[]}}"#,
        );
        write_policy(&failing.pm_dir, &staging_delivery(INTEGRATION_BRANCH));
        let policy = file_policy(&failing.pm_dir);
        record_work_approval(&failing.shared, Some(&policy));
        let ready = routine_readiness(&failing.shared, ISSUE, &pr_url(), HEAD, REPORT);
        assert!(
            readiness_refused(&ready),
            "failing staging required check must refuse: {}",
            readiness_name(&ready)
        );
    }
}
