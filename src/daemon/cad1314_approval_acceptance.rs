//! Independent CAD-1314 acceptance for the retained native merge-approval guard.
//!
//! The RPC and classifier are real. The isolated GitHub executable supplies
//! deterministic transport responses only; it cannot authorize a caller or
//! classify the change itself.

use crate::daemon::{ServeOptions, Shared};
use crate::test_seam::{scoped, Asserted};
use serde_json::json;
use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use tempfile::tempdir;

const ISSUE: &str = "CAD-1314";
const REPO: &str = "acme/widgets";
const PR: u64 = 914;
const HEAD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const BASE: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const MERGE_BASE: &str = "cccccccccccccccccccccccccccccccccccccccc";

fn fixture() -> (tempfile::TempDir, std::sync::Arc<Shared>) {
    let root = tempdir().expect("isolated CAD-1314 root");
    let pm_dir = root.path().join("pm");
    let pm = crate::issue::Pm::init(&pm_dir).expect("initialize isolated PM");
    let checkout = root.path().join("widgets");
    std::fs::create_dir_all(&checkout).unwrap();
    for args in [
        &["init", "-q"][..],
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/acme/widgets.git",
        ][..],
    ] {
        assert!(std::process::Command::new("git")
            .arg("-C")
            .arg(&checkout)
            .args(args)
            .status()
            .expect("run fixture git")
            .success());
    }
    crate::issue::write::project_add(
        &pm,
        "widgets",
        "CAD",
        &[checkout.to_str().unwrap().to_string()],
        &[],
        &[],
        None,
    )
    .expect("register project with the default policy");
    crate::issue::write::new_issue(
        &pm,
        &checkout,
        Some("widgets"),
        "Native approval strict-floor acceptance",
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
if [ "$1" = pr ] && [ "$2" = merge ]; then echo "$*" >> "$here/merges"; exit 0; fi
if [ "$1" = pr ] && [ "$2" = view ]; then
  case "$*" in
    *headRefOid,baseRefOid,statusCheckRollup*) printf '{{"headRefOid":"{HEAD}","baseRefOid":"{BASE}","statusCheckRollup":[{{"context":"ci","state":"SUCCESS"}}]}}\n' ;;
    *) printf '{{"number":{PR},"state":"OPEN","title":"CAD-1314: fixture","headRefOid":"{HEAD}","headRefName":"cad-1314","baseRefName":"main","baseRefOid":"{BASE}","statusCheckRollup":[{{"context":"ci","state":"SUCCESS"}}],"additions":1,"deletions":0,"changedFiles":1,"autoMergeRequest":null}}\n' ;;
  esac
  exit 0
fi
if [ "$1" = repo ] && [ "$2" = view ]; then printf '{{"defaultBranchRef":{{"name":"main"}}}}\n'; exit 0; fi
if [ "$1" = api ]; then
  endpoint=$2
  case "$endpoint" in
    repos/{REPO}/compare/{BASE}...{HEAD}) printf '{{"merge_base_commit":{{"sha":"{MERGE_BASE}"}},"files":[{{"filename":"src/daemon/caller_rule.rs","status":"modified","additions":1,"deletions":0}}]}}\n' ;;
    repos/{REPO}/git/trees/{MERGE_BASE}?recursive=1|repos/{REPO}/git/trees/{HEAD}?recursive=1) printf '{{"truncated":false,"tree":[{{"path":"src/daemon/caller_rule.rs","mode":"100644","type":"blob"}}]}}\n' ;;
    repos/{REPO}/contents/docs/roles/risk-paths.toml\?ref={BASE}) printf '{{"encoding":"base64","content":"{risk_content}"}}\n' ;;
    repos/{REPO}/contents/docs/roles/one-review-paths.toml\?ref={BASE}) printf '{{"encoding":"base64","content":"{review_content}"}}\n' ;;
    *) echo "unhandled GitHub API fixture: $endpoint" >&2; exit 1 ;;
  esac
  exit 0
fi
echo "unhandled gh fixture: $*" >&2
exit 1
"##
    );
    std::fs::write(&gh, script).unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();

    let mut records = BTreeMap::new();
    let mut rec = crate::delivery::Record::new(ISSUE, "widgets", "fixture-worker", 1);
    rec.state = crate::delivery::State::Passed;
    rec.pr = Some(format!("https://github.com/{REPO}/pull/{PR}"));
    rec.head = Some(HEAD.into());
    rec.verdict = Some(crate::delivery::VerdictRec {
        verdict: "pass".into(),
        sha: HEAD.into(),
        reviewer: "fixture-reviewer".into(),
        summary: "independent review passed".into(),
        report: format!("{ISSUE}/reports/fixture.md"),
        at: 1,
    });
    rec.observed = Some(crate::delivery::Observed {
        head: HEAD.into(),
        pr_state: "OPEN".into(),
        ci_green: true,
        ..Default::default()
    });
    records.insert(ISSUE.to_string(), rec);
    crate::delivery::save(root.path(), &records).expect("save merge-ready fixture");

    let opts = ServeOptions {
        test_seam: true,
        delivery_gh: Some(gh),
        ..Default::default()
    };
    opts.provider_env
        .set("CADENCE_PM_DIR", pm_dir.to_str().unwrap());
    let shared = Shared::new(root.path(), &opts).expect("initialize isolated daemon");
    (root, shared)
}

#[test]
fn cad1314_native_approval_refuses_default_policy_sensitive_change() {
    let (root, shared) = fixture();
    let response = scoped(Asserted::Operator, || {
        shared.rpc_delivery_approve(&json!({"issue": ISSUE, "sha": HEAD}), std::process::id())
    });
    let error = response.expect_err("native approval must not bypass strict default requirements");
    assert!(
        error.to_string().contains("native approval cannot satisfy current review or Browser QA requirements; use the reviewed-enqueue"),
        "expected real classifier-backed strict-floor refusal, got {error}"
    );
    assert!(
        !root.path().join("merges").exists(),
        "refusal must not invoke GitHub merge"
    );
    assert!(
        shared
            .store
            .last_event_of(
                crate::store::APPROVAL_STREAM,
                &[crate::store::APPROVAL_RECORDED_EVENT],
            )
            .expect("read native approval audit stream")
            .is_none(),
        "refusal must not write a native merge approval audit event"
    );
}
