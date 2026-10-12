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

// CAD-1403 acceptance: this transport projects the check rollup only when the
// real gh invocation requested it. An unrequested field in a permissive fake
// would hide the production query-contract defect.
#[cfg(unix)]
mod cad1403 {
    use super::super::{routine_readiness, RoutineReadiness};
    use crate::daemon::{ServeOptions, Shared};
    use crate::delivery::{self, Record};
    use crate::issue;
    use std::collections::BTreeMap;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    const ISSUE: &str = "CAD-1403";
    const REPO: &str = "acme/widgets";
    const PR: u64 = 1403;
    const HEAD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const BASE: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const MERGE_BASE: &str = "cccccccccccccccccccccccccccccccccccccccc";
    const REPORT: &str = "CAD-1403/reports/19700101T000000Z-worker.md";

    fn fixture(
        check_state: &str,
        omit_rollup: bool,
    ) -> (
        tempfile::TempDir,
        std::sync::Arc<Shared>,
        std::path::PathBuf,
    ) {
        let root = tempdir().expect("isolated CAD-1403 root");
        let pm_dir = root.path().join("pm");
        let pm = issue::Pm::init(&pm_dir).expect("initialize isolated PM");
        let checkout = root.path().join("widgets");
        std::fs::create_dir_all(&checkout).unwrap();
        assert!(std::process::Command::new("git")
            .arg("-C")
            .arg(&checkout)
            .args(["init", "-q"])
            .status()
            .unwrap()
            .success());
        assert!(std::process::Command::new("git")
            .arg("-C")
            .arg(&checkout)
            .args([
                "remote",
                "add",
                "origin",
                "https://github.com/acme/widgets.git"
            ])
            .status()
            .unwrap()
            .success());
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
            "Routine readiness query contract",
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
        let script = format!(
            r##"#!/bin/sh
here=$(dirname "$0")
if [ "$1" = pr ] && [ "$2" = view ]; then
  printf '%s\n' "$*" >> "$here/pr-view-args"
  case "$*" in
    *statusCheckRollup*)
      if [ "{omit_rollup}" = "true" ]; then rollup=''; else rollup=',"statusCheckRollup":[{{"context":"ci","state":"{check_state}"}}]'; fi ;;
    *) rollup='' ;;
  esac
  printf '{{"number":{PR},"state":"OPEN","title":"CAD-1403: fixture","headRefOid":"{HEAD}","baseRefName":"main","baseRefOid":"{BASE}","changedFiles":1%s}}\n' "$rollup"
  exit 0
fi
if [ "$1" = repo ] && [ "$2" = view ]; then printf '{{"defaultBranchRef":{{"name":"main"}}}}\n'; exit 0; fi
if [ "$1" = api ]; then
  endpoint=$2
  case "$endpoint" in
    repos/{REPO}/compare/{BASE}...{HEAD}) printf '{{"merge_base_commit":{{"sha":"{MERGE_BASE}"}},"files":[{{"filename":"docs/guide.md","status":"modified","additions":1,"deletions":0}}]}}\n' ;;
    repos/{REPO}/git/trees/{MERGE_BASE}?recursive=1|repos/{REPO}/git/trees/{HEAD}?recursive=1) printf '{{"truncated":false,"tree":[{{"path":"docs/guide.md","mode":"100644","type":"blob"}}]}}\n' ;;
    repos/{REPO}/contents/docs/roles/risk-paths.toml\?ref={BASE}) printf '{{"encoding":"base64","content":"{risk_content}"}}\n' ;;
    repos/{REPO}/contents/docs/roles/one-review-paths.toml\?ref={BASE}) printf '{{"encoding":"base64","content":"{review_content}"}}\n' ;;
    repos/{REPO}/rules/branches/main) printf '[{{"type":"required_status_checks","parameters":{{"required_status_checks":[{{"context":"ci"}}]}}}}]\n' ;;
    repos/{REPO}/branches/main/protection) printf '{{"required_status_checks":{{"contexts":["ci"],"checks":[]}}}}\n' ;;
    repos/{REPO}/actions/workflows/ci.yml/runs*) printf '{{"workflow_runs":[{{"id":1403,"head_sha":"{HEAD}","event":"pull_request","status":"completed","conclusion":"success","html_url":"https://github.com/{REPO}/actions/runs/1403"}}]}}\n' ;;
    *) echo "unhandled API fixture: $endpoint" >&2; exit 1 ;;
  esac
  exit 0
fi
echo "unhandled gh fixture: $*" >&2
exit 1
"##
        );
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
        let args = root.path().join("pr-view-args");
        (root, shared, args)
    }

    #[test]
    fn cad1403_routine_readiness_requires_requested_green_required_checks() {
        let pr = format!("https://github.com/{REPO}/pull/{PR}");
        let (root, shared, args) = fixture("SUCCESS", false);
        let classified = super::super::classify(
            &shared,
            &serde_json::json!({"issue": ISSUE, "pr": pr, "head": HEAD}),
        );
        assert!(
            classified
                .as_ref()
                .is_ok_and(|value| value["requirements"]["class"] == "routine"),
            "fixture must classify routine: {classified:?}"
        );
        let reports = issue::task_report::list(&root.path().join("pm/widgets").join(ISSUE), ISSUE);
        assert!(
            reports.iter().any(|row| row["path"] == REPORT
                && row["kind"] == "done"
                && row["agent"] == "worker"
                && row["sha"] == HEAD
                && row["pr"] == pr),
            "fixture done report did not match readiness binding: {reports:?}"
        );
        let result =
            routine_readiness(&shared, ISSUE, &pr, HEAD, REPORT).expect("readiness evaluation");
        let calls = std::fs::read_to_string(args).expect("recorded gh PR-view requests");
        assert!(
            matches!(&result, RoutineReadiness::Ready(_)),
            "green exact-head checks should ready when requested; outcome={}, gh requests={calls}",
            match result {
                RoutineReadiness::Ready(_) => "Ready",
                RoutineReadiness::Waiting => "Waiting",
                RoutineReadiness::NotRoutine => "NotRoutine",
            }
        );
        assert!(
            calls
                .lines()
                .any(|line| line.contains("--json") && line.contains("statusCheckRollup")),
            "readiness must request statusCheckRollup; requests were {calls}"
        );
        drop(root);
        for (state, omit) in [("FAILURE", false), ("SUCCESS", true)] {
            let (_root, shared, _) = fixture(state, omit);
            let result =
                routine_readiness(&shared, ISSUE, &pr, HEAD, REPORT).expect("refusal evaluation");
            assert!(
                !matches!(result, RoutineReadiness::Ready(_)),
                "failed or unrequested required-check evidence must not ready: {state}, omitted={omit}"
            );
        }
    }
}
