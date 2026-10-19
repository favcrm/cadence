//! Independent CAD-225 negative acceptance.
//!
//! Written from the ticket and the bounded-preauthorization contract, not
//! from an implementer's tests. These are deterministic Shared RPC checks
//! against an isolated PM, a real git checkout, the CAD-482 identity seam,
//! and a scripted `gh`. They are not the bounded real-provider smoke and
//! they are not evidence that a real merge or postmerge settle ran.
//!
//! Each case names the guard it has to cross:
//!
//! | case | real guard |
//! |---|---|
//! | agent, unproven, or forged field issues a grant | `operator_connection` on `rpc_workflow_approve` |
//! | forged `grant_id`, wildcard, traversal, ttl, flags without `--unattended` | strict `unattended` object on `rpc_workflow_approve` |
//! | changed template or input | `rpc_plan_propose` admission, which must bind the full template and rendered text, not only `workflow::gate_digest` |
//! | repository moved, worker removed | the same admission re-reads the registered remote and the worker row |
//! | agent proposes the exact render under a live grant | admission consumes the grant on the exact render — issuance is operator-bound, consumption is not; caller `grant_id`/`approved`/`unattended` keys on propose are refused |
//! | ordinary workflow, `--file`, app workflow | existing propose path stays `proposed`; `unattended_authorization_for_issue` is `None` |
//! | duplicate propose and reopen | admission dedupe across a new `Shared` on the same state |
//! | expiry, revoke, replayed audit payload, edited ticket body | `unattended_authorization_for_issue` returns `Err` (not `None`); `plan_gate_task` refuses while `plan::gate_id` still passes |
//! | superseded renewal replayed after a newer one, current record replayed verbatim | `unattended_grant_current` history scan: an older revision is withheld (expiry stays the newest renewal's, no credit); an identical current replay is the only idempotent write |
//! | out-of-scope, deleted, or mode-changed PR | `try_unattended_enqueue` -> `check_scope` over the real `gh api .../compare` file list |
//! | renew by agent or under drifted bounds | `workflow_approve` `renew` re-derives every immutable bound and stays operator-only |
//! | live head moved before enqueue | `try_unattended_enqueue` -> `check_view` on a fresh `gh pr view`; `rpc_delivery_approve` live-head compare |
//! | worker PASSes its own head | `try_unattended_enqueue` verdict independence plus `store.review_export_recorded` |
//! | reviewer-declared risk `human (1)` or missing | `try_unattended_enqueue` verdict check plus `store.unattended_review_recorded` — the semantic risk floor path class cannot see |
//! | in-scope green PR | the full `try_unattended_enqueue` path records a durable intent, runs `gh pr merge --auto --match-head-commit`, then settles the merge |
//! | done report filed before its dispatch turn completed | `route_delivery`/`on_done` worker-turn gate: only the latest `source='dispatch'` row for (issue, worker) at `completed` with a result sha equal to the reported head may reach `start_review` |
//! | merged head with no intent, expired grant at settle | `unattended_merge_done_refusal` holds ticket completion inside `settle_merge` |
//! | bound issue attached to a caller-authored job task | `plan_gate_task` `unattended_job_scope`, exercised through the real `rpc_task_dispatch` and the automatic monitor dispatch path — a grant authorizes canonical issue dispatch, never general job/monitor authority |
//! | grant TTL expires while admission waits on the tracker lock | `revalidate_grant` under the PM write lock inside `create_granted_plan`'s `on_intent` — nothing committed, no issue ids allocated |
//! | pending done-write retry after revoke | `settle_ticket_done` must re-derive `unattended_merge_done_refusal` on the retry — a `TicketDone::Pending` is retried under live authority, never replayed over a dead grant |
//! | same-grant renewal while a reconcile waits | `try_unattended_enqueue` reconcile: the recorded intent's own `grant_expires_at` is the bound expiry — the renewed grant must not widen it, re-run `gh pr merge`, or set `disable_auto` |
//! | queue ruleset absent, bypassable, or its checks unbound; protected check set missing a gate or the aggregate; trusted workflow without `merge_group` | `full_ci_green` -> `require_queue_authority` holds with zero mutation — no `pr merge`, no intent, the queue-entry probe is never reached |
//! | `autoMergeRequest` alone, intent+auto+null entry, intent+auto gone+null entry | `try_unattended_enqueue` -> `unattended_queue_entry`: an unbound request refuses and flags `disable_auto`; a bound pending request stays `Passed`/`unattended_pending` with no receipt and no replay; an unproven post-mutation outcome holds the intent, never replays |
//!
//! | expired/revoked grant drives a checkup CI-fix | `checkup_idle_lane` -> `fix_turn` must read `unattended_authorization_for_issue` before queueing — no `checkup-fix-*` row, no message, no task mutation |
//! | live grant's CI-fix turn and its repaired head | `send_issue_followup` writes a native `source='review'` row carrying the owned `issue`/`worktree` tags; `worker_issue_turn` correlates it latest, so the H2 done report passes `route_delivery`/`on_done` to independent review once |
//! | REVISE hand-back turn ends `failed` or `unknown`, then a fresh done report and repeated router passes | `unattended_worker_complete` refuses replay of a dead turn: `route_record` leaves the record `Escalated` with `unattended_hold` on the dead row's id, sends nothing to the worker, and only a native reconcile to `completed` at the exact head (or a new owned completed turn — a `failed` row is never relabeled) permits `start_review` once more |
//! | live `autoMergeRequest`/queue entry/unreadable remote over a durable intent, or armed metadata with no intent | `checkup_idle_lane` -> `fix_turn` must hold for the operator with zero new follow-up, zero mutation, never auto-cancel; a proven-removed request permits the bound fix and the historical intent stays durable |
//! | PASS without a latest owned turn `completed` at the reviewed head — queued/running, `failed`/`unknown`, sha-mismatched, absent | `try_unattended_enqueue` latest-turn barrier: a live turn is a normal WAIT (Working, the premature PASS clears, no refusal, no hold); dead or missing proof refuses and holds — either way no intent, no `pr merge` |
//!
//! `master_dispatch` is not called: `dispatch::run` opens a daemon socket
//! and can launch a worker. `checkup_idle_lane` is called on the shared
//! handle directly — the same internal entry the scheduler drives — with
//! a genuinely idle registered lane. The issue-bound automatic gates this
//! module crosses are `plan_gate_task` — directly, through
//! `rpc_task_dispatch`, and through `monitor_dispatch_task(automatic)` —
//! and `full_ci_green` inside `try_unattended_enqueue`. There is no
//! public seam to plant a torn pre-commit intent, so restart coverage is
//! a dropped `Shared` plus the same propose, not a crash between the
//! intent write and the tracker commit. The enqueue-vs-fix race is
//! covered as ordering, not threads: `fix_turn` re-reads the fresh record
//! under the delivery lock, so a fix seeded before the enqueue's save
//! cannot interleave — single-flight calls exercise the same lock.

#[path = "cad225_unattended_chain_acceptance.rs"]
mod cad225_unattended_chain_acceptance;
#[path = "cad225_unattended_lessons_acceptance.rs"]
mod cad225_unattended_lessons_acceptance;
#[path = "cad225_unattended_recovery_acceptance.rs"]
mod cad225_unattended_recovery_acceptance;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::{ServeOptions, Shared};
use crate::delivery::{self, Observed, Record, State, VerdictRec};
use crate::error::Result;
use crate::store::{NewAgent, Sender};
use crate::test_seam::{scoped, Asserted};

const PROJECT: &str = "widgets";
const PREFIX: &str = "CAD";
const WORKFLOW: &str = "ship";
const REPO: &str = "acme/widgets";
const PATH: &str = "src/ship.rs";
const TOPIC: &str = "rust";
const PR: u64 = 225;
const HEAD: &str = "2250000000000000000000000000000000000000";
const BASE: &str = "225bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const MOVED: &str = "225ddddddddddddddddddddddddddddddddddddd";
const MERGE: &str = "225ccccccccccccccccccccccccccccccccccccc";

const WORKFLOW_TEXT: &str = "---\ntitle: \"Ship {{topic}}\"\ngoal: \"Land {{topic}} through the granted path\"\ninputs:\n  topic:\n    ask: \"What ships?\"\n---\n\nContext for {{topic}}.\n\n## Land {{topic}}\nagent: worker\nsize: S\n\nDo the landing work.\n\n### Acceptance\n- [ ] diff stays on src/ship.rs\n";

const FILE_PLAN: &str = "---\ntitle: Manual file plan\ngoal: Stay proposed\n---\n\n## Manual\nagent: worker\nsize: S\n\nA free-text plan.\n\n### Acceptance\n- [ ] operator still approves\n";

#[derive(Clone, Debug, PartialEq, Eq)]
struct Snap {
    issues: BTreeSet<String>,
    approvals: Vec<(String, String)>,
    merges: String,
    delivery: String,
    commits: usize,
}

struct World {
    root: tempfile::TempDir,
    shared: std::sync::Arc<Shared>,
}

struct Admitted {
    epic: String,
    ticket: String,
    grant_id: String,
}

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn find_field(value: &Value, key: &str) -> Option<String> {
    match value {
        Value::Object(map) => {
            if let Some(found) = map.get(key).and_then(Value::as_str) {
                return Some(found.to_string());
            }
            map.values().find_map(|child| find_field(child, key))
        }
        Value::Array(items) => items.iter().find_map(|child| find_field(child, key)),
        _ => None,
    }
}

fn options(root: &Path) -> ServeOptions {
    let opts = ServeOptions {
        test_seam: true,
        delivery_gh: Some(root.join("gh")),
        checkup: Some(0),
        report_router: Some(0),
        ..Default::default()
    };
    opts.provider_env
        .set("CADENCE_PM_DIR", root.join("pm").to_str().unwrap());
    opts
}

/// A scripted `gh`. It answers only what the real delivery evidence chain
/// asks for: `pr view` (the three call shapes), `repo view`, `api graphql`
/// for the queue-entry probe, and `api` for compare, trees, the two
/// trusted TOML lists at base, branch rules + ruleset detail + protection,
/// the trusted ci.yml at base, and ci.yml runs. A `pr merge` is recorded
/// in `merges`, never authorized by this script — and it does NOT create a
/// queue entry: admission is observed later through `gh-queued` (the real
/// delayed-admission shape). `gh-auto` alone turns `autoMergeRequest` on.
/// `gh-case` picks the diff shape; `live-head` is the head `gh` reports;
/// `issue-id` names the ticket in the PR title so `classify`'s exact-title
/// check binds this PR to the admitted ticket. `latemove` moves only the
/// enqueue-path `pr view`, leaving classification heads intact, so
/// `check_view` is the guard that fires. `gh-queue`, `gh-protect` and
/// `gh-wf` select the enforcement fixtures: the queue ruleset, the
/// protection bindings, and the trusted workflow the base pins.
fn write_gh(root: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let risk = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        include_str!("../../docs/roles/risk-paths.toml"),
    );
    let review = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        include_str!("../../docs/roles/one-review-paths.toml"),
    );

    // The enforcement fixtures the queue-authority check parses. Each is a
    // file the script `cat`s verbatim, so a case swaps authority, never
    // script logic. The protected floor is the trusted workflow's five
    // integration gates plus its cross-build aggregate — every context a
    // GitHub-Actions-bound check (app_id 15368), mirrored by the ruleset's
    // required_status_checks entries (integration_id 15368).
    let contexts: &str = "\"fmt\",\"clippy\",\"test\",\"build\",\"ui\",\"cross-build-required\"";
    let checks: &str = concat!(
        "{\"context\":\"fmt\",\"app_id\":15368},{\"context\":\"clippy\",\"app_id\":15368},",
        "{\"context\":\"test\",\"app_id\":15368},{\"context\":\"build\",\"app_id\":15368},",
        "{\"context\":\"ui\",\"app_id\":15368},{\"context\":\"cross-build-required\",\"app_id\":15368}"
    );
    let rules_ok = "[{\"type\":\"merge_queue\",\"ruleset_id\":23887779},{\"type\":\"required_status_checks\",\"parameters\":{\"required_status_checks\":[{\"context\":\"fmt\",\"integration_id\":15368},{\"context\":\"clippy\",\"integration_id\":15368},{\"context\":\"test\",\"integration_id\":15368},{\"context\":\"build\",\"integration_id\":15368},{\"context\":\"ui\",\"integration_id\":15368},{\"context\":\"cross-build-required\",\"integration_id\":15368}]}}]";
    let ruleset_bypass = "{\"id\":23887779,\"target\":\"branch\",\"enforcement\":\"active\",\"current_user_can_bypass\":\"always\",\"bypass_actors\":[{\"actor_id\":5,\"actor_type\":\"RepositoryRole\",\"bypass_mode\":\"always\"}],\"rules\":[{\"type\":\"merge_queue\"}]}";
    let ruleset_ok = "{\"id\":23887779,\"target\":\"branch\",\"enforcement\":\"active\",\"current_user_can_bypass\":\"never\",\"bypass_actors\":[],\"rules\":[{\"type\":\"merge_queue\"}]}";
    let protect_ok = format!(
        "{{\"enforce_admins\":{{\"enabled\":true}},\"required_status_checks\":{{\"strict\":true,\"contexts\":[{contexts}],\"checks\":[{checks}]}}}}"
    );
    let wf_full = "name: ci\non:\n  push:\n  pull_request:\n  merge_group:\njobs:\n  fmt:\n    runs-on: ubuntu-latest\n    steps: []\n  clippy:\n    runs-on: ubuntu-latest\n    steps: []\n  test:\n    runs-on: ubuntu-latest\n    steps: []\n  build:\n    runs-on: ubuntu-latest\n    steps: []\n  ui:\n    runs-on: ubuntu-latest\n    steps: []\n  cross-build-required:\n    name: cross-build-required\n    needs: [fmt, clippy, test, build, ui]\n    runs-on: ubuntu-latest\n    steps: []\n";
    let wf_five = "name: ci\non:\n  push:\n  pull_request:\n  merge_group:\njobs:\n  fmt:\n    runs-on: ubuntu-latest\n    steps: []\n  clippy:\n    runs-on: ubuntu-latest\n    steps: []\n  test:\n    runs-on: ubuntu-latest\n    steps: []\n  build:\n    runs-on: ubuntu-latest\n    steps: []\n  ui:\n    runs-on: ubuntu-latest\n    steps: []\n";
    // Refusal variants — each differs from the enforced baseline in exactly
    // the authority it removes.
    let rules_noqueue = "[{\"type\":\"required_status_checks\",\"parameters\":{\"required_status_checks\":[{\"context\":\"ci\"}]}}]";
    let rules_unbound = "[{\"type\":\"merge_queue\",\"ruleset_id\":23887779},{\"type\":\"required_status_checks\",\"parameters\":{\"required_status_checks\":[{\"context\":\"test\",\"integration_id\":15368},{\"context\":\"ui\",\"integration_id\":1}]}}]";
    let protect_notest = "{\"enforce_admins\":{\"enabled\":true},\"required_status_checks\":{\"strict\":true,\"contexts\":[\"fmt\",\"clippy\",\"build\",\"ui\",\"cross-build-required\"],\"checks\":[{\"context\":\"fmt\",\"app_id\":15368},{\"context\":\"clippy\",\"app_id\":15368},{\"context\":\"build\",\"app_id\":15368},{\"context\":\"ui\",\"app_id\":15368},{\"context\":\"cross-build-required\",\"app_id\":15368}]}}";
    let protect_nocross = "{\"enforce_admins\":{\"enabled\":true},\"required_status_checks\":{\"strict\":true,\"contexts\":[\"fmt\",\"clippy\",\"test\",\"build\",\"ui\"],\"checks\":[{\"context\":\"fmt\",\"app_id\":15368},{\"context\":\"clippy\",\"app_id\":15368},{\"context\":\"test\",\"app_id\":15368},{\"context\":\"build\",\"app_id\":15368},{\"context\":\"ui\",\"app_id\":15368}]}}";
    let protect_unbound = "{\"enforce_admins\":{\"enabled\":true},\"required_status_checks\":{\"strict\":true,\"contexts\":[\"fmt\",\"clippy\",\"test\",\"build\",\"ui\",\"cross-build-required\",\"external/sonar\"],\"checks\":[{\"context\":\"fmt\",\"app_id\":15368},{\"context\":\"clippy\",\"app_id\":15368},{\"context\":\"test\",\"app_id\":15368},{\"context\":\"build\",\"app_id\":15368},{\"context\":\"ui\",\"app_id\":15368},{\"context\":\"cross-build-required\",\"app_id\":15368}]}}";
    let wf_nogroup = "name: ci\non:\n  push:\n  pull_request:\njobs:\n  fmt:\n    runs-on: ubuntu-latest\n    steps: []\n  clippy:\n    runs-on: ubuntu-latest\n    steps: []\n  test:\n    runs-on: ubuntu-latest\n    steps: []\n  build:\n    runs-on: ubuntu-latest\n    steps: []\n  ui:\n    runs-on: ubuntu-latest\n    steps: []\n  cross-build-required:\n    name: cross-build-required\n    runs-on: ubuntu-latest\n    steps: []\n";
    let wf_notest = "name: ci\non:\n  push:\n  pull_request:\n  merge_group:\njobs:\n  fmt:\n    runs-on: ubuntu-latest\n    steps: []\n  clippy:\n    runs-on: ubuntu-latest\n    steps: []\n  build:\n    runs-on: ubuntu-latest\n    steps: []\n  ui:\n    runs-on: ubuntu-latest\n    steps: []\n  cross-build-required:\n    name: cross-build-required\n    runs-on: ubuntu-latest\n    steps: []\n";

    for (name, body) in [
        ("rules-ok.json", rules_ok),
        ("rules-noqueue.json", rules_noqueue),
        ("rules-unbound.json", rules_unbound),
        ("ruleset-ok.json", ruleset_ok),
        ("ruleset-bypass.json", ruleset_bypass),
        ("protect-ok.json", protect_ok.as_str()),
        ("protect-notest.json", protect_notest),
        ("protect-nocross.json", protect_nocross),
        ("protect-unbound.json", protect_unbound),
        ("wf-full.yml", wf_full),
        ("wf-five.yml", wf_five),
        ("wf-nogroup.yml", wf_nogroup),
        ("wf-notest.yml", wf_notest),
    ] {
        std::fs::write(root.join(name), body).unwrap();
    }

    let script = format!(
        r##"#!/bin/sh
here=$(dirname "$0")
want=in
if [ -f "$here/gh-case" ]; then want=$(cat "$here/gh-case"); fi
head=$(cat "$here/live-head")
issue=$(cat "$here/issue-id" 2>/dev/null || echo {PREFIX}-225)
runs=$(cat "$here/gh-runs" 2>/dev/null || echo green)
queue=ok
if [ -f "$here/gh-queue" ]; then queue=$(cat "$here/gh-queue"); fi
base=main
if [ -f "$here/gh-base" ]; then base=$(cat "$here/gh-base"); fi
protect=ok
if [ -f "$here/gh-protect" ]; then protect=$(cat "$here/gh-protect"); fi
wf=full
if [ -f "$here/gh-wf" ]; then wf=$(cat "$here/gh-wf"); fi
if [ -f "$here/gh-queued" ]; then entry='{{"id":"queue-225"}}'; automr='{{"enabledAt":"2026-01-01T00:00:00Z"}}'; else entry=null; automr=null; fi
if [ -f "$here/gh-auto" ]; then automr='{{"enabledAt":"2026-01-01T00:00:00Z"}}'; fi
echo "$*" >> "$here/gh-calls"
if [ -f "$here/gh-down" ]; then echo "gh: remote unavailable (fixture)" >&2; exit 1; fi
if [ "$1" = pr ] && [ "$2" = merge ]; then echo "$*" >> "$here/merges"; touch "$here/gh-auto"; exit 0; fi
if [ "$1" = pr ] && [ "$2" = view ]; then
  if [ -f "$here/rv-arm" ] && [ ! -f "$here/rv-done" ]; then touch "$here/rv-call" "$here/rv-done"; i=0; while [ ! -f "$here/rv-go" ] && [ $i -lt 3000 ]; do sleep 0.05; i=$((i+1)); done; fi
  case "$*" in
    *autoMergeRequest*)
      vhead=$head
      if [ "$want" = latemove ]; then vhead={MOVED}; fi
      printf '%s\n' "{{\"number\":{PR},\"state\":\"OPEN\",\"headRefOid\":\"$vhead\",\"headRefName\":\"cad-225\",\"baseRefName\":\"$base\",\"baseRefOid\":\"{BASE}\",\"statusCheckRollup\":[{{\"context\":\"fmt\",\"state\":\"SUCCESS\"}},{{\"context\":\"clippy\",\"state\":\"SUCCESS\"}},{{\"context\":\"test\",\"state\":\"SUCCESS\"}},{{\"context\":\"build\",\"state\":\"SUCCESS\"}},{{\"context\":\"ui\",\"state\":\"SUCCESS\"}},{{\"context\":\"cross-build-required\",\"state\":\"SUCCESS\"}},{{\"context\":\"ci\",\"state\":\"SUCCESS\"}},{{\"context\":\"external/sonar\",\"state\":\"SUCCESS\"}}],\"additions\":1,\"deletions\":0,\"changedFiles\":1,\"autoMergeRequest\":$automr}}"
      ;;
    *headRefOid,baseRefOid,statusCheckRollup*)
      printf '%s\n' "{{\"headRefOid\":\"$head\",\"baseRefOid\":\"{BASE}\",\"statusCheckRollup\":[{{\"context\":\"fmt\",\"state\":\"SUCCESS\"}},{{\"context\":\"clippy\",\"state\":\"SUCCESS\"}},{{\"context\":\"test\",\"state\":\"SUCCESS\"}},{{\"context\":\"build\",\"state\":\"SUCCESS\"}},{{\"context\":\"ui\",\"state\":\"SUCCESS\"}},{{\"context\":\"cross-build-required\",\"state\":\"SUCCESS\"}},{{\"context\":\"ci\",\"state\":\"SUCCESS\"}},{{\"context\":\"external/sonar\",\"state\":\"SUCCESS\"}}]}}"
      ;;
    *)
      printf '%s\n' "{{\"number\":{PR},\"state\":\"OPEN\",\"title\":\"$issue: fixture\",\"headRefOid\":\"$head\",\"headRefName\":\"cad-225\",\"baseRefName\":\"$base\",\"baseRefOid\":\"{BASE}\",\"author\":{{\"login\":\"worker\"}},\"statusCheckRollup\":[{{\"context\":\"fmt\",\"state\":\"SUCCESS\"}},{{\"context\":\"clippy\",\"state\":\"SUCCESS\"}},{{\"context\":\"test\",\"state\":\"SUCCESS\"}},{{\"context\":\"build\",\"state\":\"SUCCESS\"}},{{\"context\":\"ui\",\"state\":\"SUCCESS\"}},{{\"context\":\"cross-build-required\",\"state\":\"SUCCESS\"}},{{\"context\":\"ci\",\"state\":\"SUCCESS\"}},{{\"context\":\"external/sonar\",\"state\":\"SUCCESS\"}}],\"additions\":1,\"deletions\":0,\"changedFiles\":1,\"autoMergeRequest\":$automr}}"
      ;;
  esac
  exit 0
fi
if [ "$1" = repo ] && [ "$2" = view ]; then printf '%s\n' "{{\"defaultBranchRef\":{{\"name\":\"main\"}}}}"; exit 0; fi
if [ "$1" = api ]; then
  file=src/ship.rs
  status=added
  mode=100644
  case "$want" in
    outside) file=src/cad225_outside.rs ;;
    delete) status=removed ;;
    mode) mode=100755 ;;
  esac
  case "$2" in
    graphql)
      # One-shot rendezvous at the actual final queue observation, after
      # classification/CI/final PR reads. Reload destination AFTER release:
      # a retarget effective now must not be hidden by an earlier snapshot.
      if [ -f "$here/qr-arm" ] && [ ! -f "$here/qr-done" ]; then
        touch "$here/qr-call" "$here/qr-done"
        i=0
        while [ ! -f "$here/qr-go" ] && [ $i -lt 600 ]; do sleep 0.05; i=$((i+1)); done
        if [ ! -f "$here/qr-go" ]; then echo "queue rendezvous timed out" >&2; exit 1; fi
      fi
      qbase=main
      if [ -f "$here/gh-base" ]; then qbase=$(cat "$here/gh-base"); fi
      if [ -f "$here/qr-done" ]; then
        printf '%s\n' "$head|$qbase|{BASE}" > "$here/qr-observed"
      fi
      # Respect the real GraphQL selection. Values come from remote fixture
      # state, never from client-supplied expected target or base values.
      target=
      missing=none
      if [ -f "$here/qr-missing" ]; then missing=$(cat "$here/qr-missing"); fi
      case "$*" in *baseRefName*)
        if [ "$missing" != name ]; then target="$target,\"baseRefName\":\"$qbase\""; fi ;;
      esac
      case "$*" in *baseRefOid*)
        if [ "$missing" != oid ]; then target="$target,\"baseRefOid\":\"{BASE}\""; fi ;;
      esac
      printf '%s\n' "{{\"data\":{{\"repository\":{{\"pullRequest\":{{\"headRefOid\":\"$head\",\"state\":\"OPEN\",\"mergeQueueEntry\":$entry$target}}}}}}}}"
      ;;
    repos/{REPO}/compare/{BASE}...{HEAD}|repos/{REPO}/compare/{BASE}...{MOVED}) printf '%s\n' "{{\"merge_base_commit\":{{\"sha\":\"{MERGE}\"}},\"files\":[{{\"filename\":\"$file\",\"status\":\"$status\",\"additions\":1,\"deletions\":0}}]}}" ;;
    repos/{REPO}/git/trees/{MERGE}?recursive=1|repos/{REPO}/git/trees/{BASE}?recursive=1) printf '%s\n' "{{\"truncated\":false,\"tree\":[{{\"path\":\"src/ship.rs\",\"mode\":\"100644\",\"type\":\"blob\"}}]}}" ;;
    repos/{REPO}/git/trees/{HEAD}?recursive=1|repos/{REPO}/git/trees/{MOVED}?recursive=1) printf '%s\n' "{{\"truncated\":false,\"tree\":[{{\"path\":\"$file\",\"mode\":\"$mode\",\"type\":\"blob\"}}]}}" ;;
    repos/{REPO}/contents/docs/roles/risk-paths.toml?ref={BASE}) printf '%s\n' "{{\"encoding\":\"base64\",\"content\":\"{risk}\"}}" ;;
    repos/{REPO}/contents/docs/roles/one-review-paths.toml?ref={BASE}) printf '%s\n' "{{\"encoding\":\"base64\",\"content\":\"{review}\"}}" ;;
    repos/{REPO}/rules/branches/$base)
      if [ -f "$here/rules-$queue.json" ]; then cat "$here/rules-$queue.json"; else cat "$here/rules-ok.json"; fi ;;
    repos/{REPO}/rulesets/*)
      if [ -f "$here/ruleset-$queue.json" ]; then cat "$here/ruleset-$queue.json"; else cat "$here/ruleset-ok.json"; fi ;;
    repos/{REPO}/branches/$base/protection)
      if [ -f "$here/protect-$protect.json" ]; then cat "$here/protect-$protect.json"; else cat "$here/protect-ok.json"; fi ;;
    repos/{REPO}/contents/.github/workflows/ci.yml?ref={BASE})
      wff=wf-full.yml
      if [ -f "$here/wf-$wf.yml" ]; then wff=wf-$wf.yml; fi
      printf '{{"encoding":"base64","content":"'; base64 -w0 "$here/$wff"; printf '"}}\n' ;;
    repos/{REPO}/actions/workflows/ci.yml/runs*)
      # The check answers GitHub's unfiltered canonical history for this
      # head — `total_count` must cover every `workflow_runs` entry, and
      # the newest (run_number, run_attempt) decides. `green` lands;
      # `failed-newer`/`pending-newer` wait; base trust lives in the
      # enforced queue, never in mutable run-sidecar data.
      okrun="\"head_sha\":\"$head\",\"event\":\"pull_request\""
      case "$runs" in
        failed-newer) printf '%s\n' "{{\"total_count\":2,\"workflow_runs\":[{{\"id\":102,\"run_number\":102,\"run_attempt\":2,$okrun,\"status\":\"completed\",\"conclusion\":\"failure\",\"html_url\":\"https://github.com/{REPO}/actions/runs/102\"}},{{\"id\":101,\"run_number\":101,\"run_attempt\":1,$okrun,\"status\":\"completed\",\"conclusion\":\"success\",\"html_url\":\"https://github.com/{REPO}/actions/runs/101\"}}]}}" ;;
        pending-newer) printf '%s\n' "{{\"total_count\":2,\"workflow_runs\":[{{\"id\":102,\"run_number\":102,\"run_attempt\":2,$okrun,\"status\":\"in_progress\",\"conclusion\":null,\"html_url\":\"https://github.com/{REPO}/actions/runs/102\"}},{{\"id\":99,\"run_number\":99,\"run_attempt\":1,$okrun,\"status\":\"completed\",\"conclusion\":\"success\",\"html_url\":\"https://github.com/{REPO}/actions/runs/99\"}}]}}" ;;
        *) printf '%s\n' "{{\"total_count\":1,\"workflow_runs\":[{{\"id\":99,\"run_number\":99,\"run_attempt\":1,$okrun,\"status\":\"completed\",\"conclusion\":\"success\",\"html_url\":\"https://github.com/{REPO}/actions/runs/99\"}}]}}" ;;
      esac
      ;;
    *) echo "unhandled GitHub API fixture: $2" >&2; exit 1 ;;
  esac
  exit 0
fi
echo "unhandled gh fixture: $*" >&2
exit 1
"##,
        risk = risk,
        review = review,
    );
    let gh = root.join("gh");
    std::fs::write(&gh, script).unwrap();
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(root.join("live-head"), HEAD).unwrap();
    std::fs::write(root.join("gh-case"), "in\n").unwrap();
    // Baseline: the enforced merge-queue authority the real check requires.
    std::fs::write(root.join("gh-queue"), "ok\n").unwrap();
    std::fs::write(root.join("gh-protect"), "ok\n").unwrap();
    std::fs::write(root.join("gh-wf"), "full\n").unwrap();
}

impl World {
    fn open() -> Self {
        for sha in [HEAD, BASE, MERGE, MOVED] {
            assert_eq!(sha.len(), 40, "{sha}");
            assert!(
                sha.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
                "{sha} must be lowercase hex"
            );
        }
        let root = tempfile::tempdir().expect("isolated CAD-225 root");
        write_gh(root.path());
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
            assert!(
                std::process::Command::new("git")
                    .arg("-C")
                    .arg(&checkout)
                    .args(args)
                    .status()
                    .expect("fixture git")
                    .success(),
                "git {args:?}"
            );
        }
        crate::issue::write::project_add(
            &pm,
            PROJECT,
            PREFIX,
            &[checkout.to_str().unwrap().to_string()],
            &[],
            &[],
            None,
        )
        .expect("register project");
        let opts = options(root.path());
        let shared = Shared::new(root.path(), &opts).expect("isolated daemon");
        let world = Self { root, shared };
        world.register("worker", "worker");
        world.register("reviewer", "reviewer");
        world.write_workflow(WORKFLOW_TEXT, true);
        world
    }

    fn reopen(root: PathBuf) -> std::sync::Arc<Shared> {
        Shared::new(&root, &options(&root)).expect("reopen isolated daemon on the same state")
    }

    fn pm(&self) -> PathBuf {
        self.root.path().join("pm")
    }

    fn checkout(&self) -> PathBuf {
        self.root.path().join("widgets")
    }

    fn register(&self, alias: &str, role: &str) {
        // `pick_reviewer` never routes a review to an `inbox` endpoint —
        // the mailbox has no actor to read it — so a `reviewer` row
        // stands on the fake endpoint the real adapter registry knows
        // (`has_actor`), like cad1298's fixture. Every other role stays
        // the durable mailbox the tests otherwise want.
        let (provider, endpoint_kind) = if role == "reviewer" {
            ("fake", "fake")
        } else {
            ("inbox", "inbox")
        };
        self.shared
            .store
            .register_agent(&NewAgent {
                alias,
                provider,
                endpoint_kind,
                role,
                cwd: self.root.path().to_str().unwrap(),
                sandbox: "read-only",
                instructions: None,
                params: None,
                team_role: None,
                model_policy: None,
            })
            .unwrap_or_else(|e| panic!("register {alias}: {e}"));
        // A real endpoint registers `starting`; its actor then reports
        // ready — the fixture stands in for that handshake so the lane is
        // `idle` exactly as the supervisor would leave it. Inbox lanes
        // are already durable mailboxes.
        if crate::adapter::registry::has_actor(provider, endpoint_kind) {
            self.shared
                .store
                .set_agent_state(alias, "idle", None)
                .expect("the registered endpoint reports ready");
        }
    }

    fn write_workflow(&self, text: &str, add: bool) {
        let pm = crate::issue::Pm::at(&self.pm()).expect("open PM");
        crate::issue::workflow::write_file(
            &pm,
            PROJECT,
            WORKFLOW,
            text,
            add,
            self.root.path(),
            "operator",
        )
        .unwrap_or_else(|e| panic!("workflow write: {e}"));
    }

    fn stored_workflow(&self) -> String {
        crate::issue::workflow::read_for(&self.pm(), PROJECT, WORKFLOW).unwrap()
    }

    fn rendered(&self, inputs: &Value) -> String {
        let mut provided = BTreeMap::new();
        for (key, value) in inputs.as_object().unwrap() {
            provided.insert(key.clone(), value.as_str().unwrap().to_string());
        }
        crate::issue::workflow::render(&self.stored_workflow(), &provided).unwrap()
    }

    fn call(&self, who: Asserted, run: impl FnOnce(&Shared) -> Result<Value>) -> Result<Value> {
        let shared = &self.shared;
        scoped(who, || run(shared))
    }

    fn approve(&self, who: Asserted, params: Value) -> Result<Value> {
        self.call(who, |shared| {
            shared.rpc_workflow_approve(&params, std::process::id())
        })
    }

    /// The contract's `workflow approve NAME --project P --unattended
    /// --repo R --allow-path X --input k=v [--ttl-secs T]`: exactly
    /// `{repo, paths, inputs, ttl_secs?}` inside `unattended`.
    fn unattended(
        &self,
        paths: &[&str],
        inputs: Value,
        ttl: Option<u64>,
        extra: Option<Value>,
    ) -> Value {
        let mut body = json!({
            "repo": REPO,
            "paths": paths,
            "inputs": inputs,
        });
        if let Some(ttl) = ttl {
            body["ttl_secs"] = json!(ttl);
        }
        if let Some(extra) = extra {
            for (key, value) in extra.as_object().unwrap() {
                body[key] = value.clone();
            }
        }
        json!({"project": PROJECT, "name": WORKFLOW, "unattended": body})
    }

    fn propose_workflow(
        &self,
        who: Asserted,
        inputs: Value,
        extra: Option<Value>,
    ) -> Result<Value> {
        let mut params = json!({
            "project": PROJECT,
            "workflow": WORKFLOW,
            "inputs": inputs,
        });
        if let Some(extra) = extra {
            for (key, value) in extra.as_object().unwrap() {
                params[key] = value.clone();
            }
        }
        let shared = self.shared.clone();
        scoped(who, || shared.rpc_plan_propose(&params, std::process::id()))
    }

    fn propose_value(&self, params: Value) -> Result<Value> {
        let shared = self.shared.clone();
        scoped(Asserted::Operator, || {
            shared.rpc_plan_propose(&params, std::process::id())
        })
    }

    fn issues(&self) -> BTreeSet<String> {
        let mut ids = BTreeSet::new();
        let project = self.pm().join(PROJECT);
        let Ok(entries) = std::fs::read_dir(project) else {
            return ids;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with(&format!("{PREFIX}-")) && entry.path().join("issue.md").is_file() {
                ids.insert(name);
            }
        }
        ids
    }

    fn issue(&self, id: &str) -> crate::issue::board::Issue {
        crate::issue::board::load_issue(&self.pm(), PROJECT, id).unwrap()
    }

    /// The `audit:approvals` stream's `(kind, payload)` rows, oldest
    /// first. `events_tail` refuses the stream alias — `audit:approvals`
    /// is a managed-stream name, never a registered agent — so the
    /// fixture reads the same rows it wrote through a read-only
    /// connection instead of registering a fake agent named like the
    /// stream (which would alter the alias namespace the guards match).
    fn audit_events(&self) -> Vec<(String, Value)> {
        let conn = crate::store::open_read_only(&self.root.path().join("cadence.sqlite3"))
            .expect("read-only audit view opens");
        let mut stmt = conn
            .prepare("SELECT kind, payload FROM events WHERE alias=?1 ORDER BY seq")
            .expect("audit read");
        stmt.query_map([crate::store::APPROVAL_STREAM], |row| {
            let kind: String = row.get(0)?;
            let payload: String = row.get(1)?;
            Ok((kind, serde_json::from_str(&payload).unwrap_or(Value::Null)))
        })
        .expect("audit read")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("audit read")
    }

    fn snap(&self) -> Snap {
        let approvals = self
            .audit_events()
            .into_iter()
            .map(|(kind, payload)| (kind, payload.to_string()))
            .collect();
        Snap {
            issues: self.issues(),
            approvals,
            merges: std::fs::read_to_string(self.root.path().join("merges")).unwrap_or_default(),
            delivery: std::fs::read_to_string(self.root.path().join("delivery.json"))
                .unwrap_or_default(),
            commits: self.commits(),
        }
    }

    fn commits(&self) -> usize {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(self.pm())
            .args(["rev-list", "--count", "HEAD"])
            .output()
            .unwrap();
        assert!(output.status.success());
        String::from_utf8(output.stdout)
            .unwrap()
            .trim()
            .parse()
            .unwrap()
    }

    fn recorded_merge_approvals(&self) -> usize {
        self.audit_events()
            .iter()
            .filter(|(kind, _)| kind == crate::store::APPROVAL_RECORDED_EVENT)
            .count()
    }

    fn held_events(&self) -> usize {
        self.audit_events()
            .iter()
            .filter(|(kind, _)| kind == "unattended_landing_held")
            .count()
    }

    /// The one actionable Needs-you row a held automatic landing must show,
    /// through the existing operator-owned overview read — a repeat refusal
    /// must not stack another row.
    /// The one actionable Needs-me row a held automatic landing must show,
    /// through the existing operator-owned overview read — a repeat refusal
    /// must not stack another row. `overview_cached` emits `needs_me` rows
    /// keyed by `kind`/`subject`, not a `needs_you`/`state` shape.
    fn held_rows(&self, ticket: &str) -> usize {
        let view = crate::overview::overview_cached(self.root.path(), &self.pm());
        view["needs_me"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|row| {
                row["kind"].as_str() == Some("unattended_held")
                    && row["subject"]["id"].as_str() == Some(ticket)
            })
            .count()
    }

    fn workflow_approval(&self) -> Value {
        self.shared
            .store
            .workflow_approvals()
            .unwrap()
            .remove(&format!("{PROJECT}/{WORKFLOW}"))
            .unwrap_or(Value::Null)
    }

    fn assert_unchanged(&self, before: &Snap) {
        assert_eq!(
            self.snap(),
            *before,
            "unauthorized tracker, queue, or approval effect"
        );
    }

    fn assert_refused(&self, result: Result<Value>, before: &Snap) {
        let err = result.expect_err("guard must refuse");
        assert!(!err.to_string().is_empty(), "refusal must name itself");
        self.assert_unchanged(before);
    }

    fn require_native_grant(&self, response: &Value) -> String {
        let stored = self.workflow_approval();
        let grant_id = find_field(response, "grant_id")
            .or_else(|| find_field(&stored, "grant_id"))
            .unwrap_or_else(|| panic!("native grant id missing from {response} / {stored}"));
        let template = sha256_hex(self.stored_workflow().as_bytes());
        let rendered = sha256_hex(self.rendered(&json!({"topic": TOPIC})).as_bytes());
        let blob = format!("{response}{stored}");
        assert!(
            blob.contains(&template),
            "approval payload must record the full template sha256 {template}, not only the gate digest; got {blob}"
        );
        assert!(
            blob.contains(&rendered),
            "approval payload must record the rendered sha256 {rendered}; got {blob}"
        );
        let gate = crate::issue::workflow::gate_digest(&self.stored_workflow()).unwrap();
        assert_ne!(
            gate.trim_start_matches("sha256:"),
            template,
            "fixture template hash collided with the gate digest"
        );
        assert_ne!(grant_id, "forged-grant");
        grant_id
    }

    /// Issue the grant, propose the exact bound render, and verify one
    /// commit produced an approved epic plus its ticket bound to the run.
    fn grant_and_admit(&self, ttl: Option<u64>) -> Admitted {
        let response = self
            .approve(
                Asserted::Operator,
                self.unattended(&[PATH], bound_inputs(), ttl, None),
            )
            .expect("operator unattended grant");
        let grant_id = self.require_native_grant(&response);
        self.assert_plan_admitted(Asserted::Operator, &grant_id)
    }

    /// Propose the grant's exact render on whatever grant is live. No
    /// re-approval: a live grant must admit; a dead one must not.
    fn assert_plan_admitted(&self, who: Asserted, grant_id: &str) -> Admitted {
        let caller = who.as_str().replace("agent:", "");
        let commits = self.commits();
        let proposed = self
            .propose_workflow(who.clone(), bound_inputs(), None)
            .expect("exact render must admit one plan");
        assert_eq!(
            proposed["state"].as_str(),
            Some("approved"),
            "exact grant must accept-at-create, got {proposed}"
        );
        assert_eq!(
            self.commits(),
            commits + 1,
            "admission is one tracker commit"
        );
        let epic = proposed["epic"].as_str().unwrap().to_string();
        let plan = self.issue(&epic).front.plan.expect("epic plan");
        assert_eq!(plan.state, "approved");
        let decided = plan.decided_by.expect("grant attribution");
        assert_ne!(
            decided, "operator",
            "attribution must not be a fabricated operator decision"
        );
        assert!(
            decided.to_lowercase().contains("grant"),
            "attribution must name the workflow grant, got {decided}"
        );
        assert_eq!(plan.tickets.len(), 1);
        let ticket = plan.tickets[0].clone();
        std::fs::write(self.root.path().join("issue-id"), &ticket).unwrap();
        assert_eq!(self.issue(&ticket).front.status, "ready");
        let run = self
            .shared
            .unattended_authorization_for_issue(&ticket)
            .expect("bound run reads")
            .unwrap_or_else(|| panic!("{ticket} looked unbound after admission"));
        assert_eq!(run.grant_id, grant_id);
        assert_eq!(run.project, PROJECT);
        assert_eq!(run.epic, epic);
        assert_eq!(run.repo, REPO);
        assert_eq!(run.paths, vec![PATH.to_string()]);
        assert_eq!(
            plan.proposed_by, caller,
            "admission attributes the proposing caller, not the operator"
        );
        Admitted {
            epic,
            ticket,
            grant_id: grant_id.to_string(),
        }
    }

    /// A propose that must not produce an approved, grant-bound plan: it
    /// either refuses (leaving the tracker untouched) or lands the ordinary
    /// `proposed` path with no grant decision, no ready tickets and no
    /// bound run.
    fn assert_not_admitted(&self, before: &BTreeSet<String>, result: Result<Value>) {
        match result {
            Err(_) => assert_eq!(
                &self.issues(),
                before,
                "refused admission must not write tracker issues"
            ),
            Ok(body) => {
                assert_ne!(
                    body["state"].as_str(),
                    Some("approved"),
                    "unattended admission of work the grant does not name: {body}"
                );
                let epic = body["epic"].as_str().expect("propose answers the epic");
                let plan = self.issue(epic).front.plan.expect("plan");
                assert_eq!(plan.state, "proposed");
                assert!(
                    plan.decided_by.is_none(),
                    "ordinary propose has no grant decision"
                );
                for ticket in &plan.tickets {
                    assert_eq!(self.issue(ticket).front.status, "backlog");
                    assert!(
                        self.shared
                            .unattended_authorization_for_issue(ticket)
                            .unwrap()
                            .is_none(),
                        "{ticket} must not carry a native unattended run"
                    );
                }
            }
        }
        assert!(self.snap().merges.is_empty(), "GitHub merge ran");
        assert_eq!(self.recorded_merge_approvals(), 0);
    }

    /// Revoked, expired or rebound authority reads as an error, never as
    /// "no binding" (which would fall back to the legacy path) and never
    /// as a live grant.
    fn assert_invalid_run(&self, issue: &str) {
        match self.shared.unattended_authorization_for_issue(issue) {
            Ok(None) => panic!("{issue} must not look unbound"),
            Ok(Some(run)) => panic!("{issue} is still authorized under {}", run.grant_id),
            Err(_) => {}
        }
    }

    fn bind_job(&self, ticket: &str) {
        self.shared
            .store
            .create_job(
                "cad225-job",
                Some("gate"),
                "spec.md",
                &sha256_hex(b"cad225"),
                "worker",
                Some(ticket),
                None,
                None,
                0,
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            )
            .expect("issue-bound job");
    }

    fn set_case(&self, case: &str) {
        std::fs::write(self.root.path().join("gh-case"), format!("{case}\n")).unwrap();
    }

    /// Which `workflow_runs` list the scripted `gh` serves for the head:
    /// `green` (accepted default), `failed-newer` or `pending-newer` —
    /// the staleness fixtures `full_ci_green` must consult instead of
    /// trusting any historical success.
    fn set_runs(&self, case: &str) {
        std::fs::write(self.root.path().join("gh-runs"), format!("{case}\n")).unwrap();
    }

    /// Which enforcement fixture `gh` serves: `gh-queue` picks the branch
    /// rules / ruleset detail pair (`noqueue`, `bypass`, `unbound`), and
    /// `gh-protect` the protection doc (`notest`, `nocross`, `unbound`).
    fn set_queue(&self, case: &str) {
        std::fs::write(self.root.path().join("gh-queue"), format!("{case}\n")).unwrap();
    }

    fn set_protect(&self, case: &str) {
        std::fs::write(self.root.path().join("gh-protect"), format!("{case}\n")).unwrap();
    }

    /// Which trusted workflow the base serves: `full` (five gates plus the
    /// cross-build aggregate), `five` (gates only), `nogroup` (no
    /// `merge_group` event), `notest` (the `test` gate missing).
    fn set_workflow(&self, case: &str) {
        std::fs::write(self.root.path().join("gh-wf"), format!("{case}\n")).unwrap();
    }

    /// The remote queue state: after the enqueue mutation GitHub reports
    /// `mergeQueueEntry.id` (and `autoMergeRequest`) — absent until the
    /// queue service admits the head, which the script never fakes on
    /// `pr merge` itself.
    fn queue_admitted(&self) {
        std::fs::write(self.root.path().join("gh-queued"), "1\n").unwrap();
    }

    /// Which branch the scripted `gh` reports as the PR's `baseRefName` —
    /// `main` by default (the repository default). A non-default name is
    /// also the branch whose `rules`/`protection` fixtures the same
    /// controls select, so the actual target's authority is read, never
    /// silently substituted for the default branch's.
    fn set_base(&self, branch: &str) {
        std::fs::write(self.root.path().join("gh-base"), format!("{branch}\n")).unwrap();
    }

    /// An `autoMergeRequest` exists without a queue entry — a request the
    /// queue service has not yet granted, or one enabled by hand.
    fn auto_merge_only(&self) {
        std::fs::write(self.root.path().join("gh-auto"), "1\n").unwrap();
    }

    /// Every scripted `gh` invocation fails — the remote is unreachable,
    /// so the queue-state proof cannot be read at all.
    fn set_down(&self) {
        std::fs::write(self.root.path().join("gh-down"), "1\n").unwrap();
    }

    /// Arm the scripted `gh`'s one-shot `pr view` rendezvous: the next
    /// view call writes `rv-call` and parks until `rv-go` exists.
    fn arm_rendezvous(&self) {
        std::fs::write(self.root.path().join("rv-arm"), "1\n").unwrap();
    }

    /// Wait for a fixture marker with a hard deadline — the rendezvous
    /// fires inside the handler under test, so a timeout is a bug, not
    /// a flake. Returns false only on the deadline.
    fn wait_for(&self, marker: &str, secs: u64) -> bool {
        let deadline = std::time::Instant::now() + Duration::from_secs(secs);
        while !self.root.path().join(marker).exists() {
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        true
    }

    /// Release the parked `pr view` call.
    fn release_rendezvous(&self) {
        std::fs::write(self.root.path().join("rv-go"), "1\n").unwrap();
    }

    fn set_head(&self, head: &str) {
        std::fs::write(self.root.path().join("live-head"), head).unwrap();
    }

    /// How many scripted `gh` invocations matched `needle` — proves which
    /// stage of the real evidence chain a refusal reached.
    fn gh_calls(&self, needle: &str) -> usize {
        std::fs::read_to_string(self.root.path().join("gh-calls"))
            .unwrap_or_default()
            .lines()
            .filter(|line| line.contains(needle))
            .count()
    }

    /// A worker `done` report filed at `at` on `ticket`, head `sha`; returns
    /// the filename, the `handled` key the record matches. The filename's UTC
    /// stamp is what `task_report::list` answers as `at` — the router's
    /// freshness check reads it, so `at` is the report's provenance clock,
    /// never a field the body could smuggle.
    fn done_report(&self, ticket: &str, at: i64, sha: &str) -> String {
        let iso = crate::issue::time::iso(at);
        let compact: String = iso.chars().filter(|c| *c != '-' && *c != ':').collect();
        let name = format!("{compact}-worker-{at}.md");
        let dir = self.pm().join(PROJECT).join(ticket).join("reports");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(&name),
            format!(
                "---\nschema: cadence.report/2\nkind: done\ntask: {ticket}\nagent: worker\n\
                 sha: {sha}\npr: https://github.com/{REPO}/pull/{PR}\n---\n\n\
                 ## Expected\n\nx\n\n## Evidence\n\nx\n\n## Cause\n\nx\n\n\
                 ## Correction\n\nx\n\n## Lesson\n\nx\n\n## Next\n\nx\n"
            ),
        )
        .unwrap();
        name
    }

    /// Plant the kickoff-turn row `dispatch_send`/`enqueue_steered` writes
    /// for the worker's lane: `source='dispatch'` with the daemon-resolved
    /// `issue`/`worktree` tags no caller can set. `seq` distinguishes the
    /// turns one worker accumulates across redispatch/restart. `running` is
    /// the provider ack (`false` leaves the row `queued`); `result` finishes
    /// the turn (`Some(Some(sha))` records a head, `Some(None)` completes
    /// without one), so the phases can stand the turn at each point of its
    /// real lifecycle.
    fn dispatch_turn(&self, ticket: &str, seq: usize, running: bool, result: Option<Option<&str>>) {
        let id = format!("cad225-dispatch-{seq}");
        let (duplicate, _state) = self
            .shared
            .store
            .enqueue_steered(
                "worker",
                "bound run kickoff",
                None,
                &id,
                "dispatch",
                None,
                Some(ticket),
                Some(self.checkout().to_str().expect("checkout path is utf-8")),
                &Sender::Unattributed,
                &crate::store::Steer::NONE,
                None,
                None,
                None,
            )
            .expect("the kickoff row is a store write, not a caller path");
        assert!(!duplicate, "kickoff seed {id} must not collide");
        if running || result.is_some() {
            self.shared
                .store
                .mark_running(&id, &format!("turn-{seq}"))
                .expect("provider ack");
        }
        if let Some(inner) = result {
            let message = self
                .shared
                .store
                .message(&id)
                .expect("turn reads")
                .expect("the seeded kickoff exists");
            self.shared
                .store
                .finish(
                    &message,
                    "completed",
                    &match inner {
                        Some(sha) => json!({"sha": sha}),
                        None => json!({"text": "done without a recorded head"}),
                    },
                    None,
                )
                .expect("turn completes");
        }
    }

    /// The owned dispatch turn dies: `failed` rows are terminal, never
    /// relabeled, so the latest-turn barrier must see the dead state.
    fn fail_turn(&self, ticket: &str, seq: usize) {
        self.dispatch_turn(ticket, seq, true, None);
        let id = format!("cad225-dispatch-{seq}");
        let message = self
            .shared
            .store
            .message(&id)
            .expect("turn reads")
            .expect("the seeded kickoff exists");
        self.shared
            .store
            .finish(
                &message,
                "failed",
                &json!({"text": "provider failure"}),
                Some("provider failure"),
            )
            .expect("the failed row lands");
    }

    /// The owned dispatch turn's endpoint dies mid-flight: the real
    /// `orphan_running` path fences the row `unknown` — dead evidence,
    /// reconcilable only by the operator.
    fn orphan_turn(&self, ticket: &str, seq: usize) {
        self.dispatch_turn(ticket, seq, true, None);
        self.shared
            .store
            .orphan_running("worker", "endpoint died")
            .expect("the dead endpoint fences its in-flight rows unknown");
    }

    /// `review_routed` daemon events naming `ticket` — one per real review
    /// kickoff `start_review` sends, never inferable from the record alone.
    fn routed(&self, ticket: &str) -> Vec<Value> {
        [
            crate::store::Store::DAEMON_STREAM,
            crate::store::APPROVAL_STREAM,
        ]
        .iter()
        .flat_map(|stream| {
            self.shared
                .store
                .events_tail(stream, 100)
                .unwrap_or_default()
        })
        .filter(|event| {
            event.kind == "review_routed" && event.payload["issue"].as_str() == Some(ticket)
        })
        .map(|event| event.payload)
        .collect()
    }

    /// The state an independent PASS leaves: a Passed record on the exact
    /// head with the daemon-attested verdict receipt `review_export_recorded`
    /// matches — latest per issue, scope fields included. `risk` is the
    /// reviewer's declared risk verdict: written into the report frontmatter
    /// and carried on the immutable receipt (`Some` only when declared);
    /// `unattended_review_recorded` requires `auto`.
    fn save_pass_risk(&self, ticket: &str, reviewer: &str, risk: Option<&str>) {
        let report = format!("{ticket}/reports/pass.md");
        let mut rec = Record::new(ticket, PROJECT, "worker", 1);
        rec.state = State::Passed;
        rec.pr = Some(format!("https://github.com/{REPO}/pull/{PR}"));
        rec.head = Some(HEAD.into());
        rec.reviewer = Some(reviewer.into());
        rec.outcome_report = Some(report.clone());
        rec.verdict = Some(VerdictRec {
            verdict: "pass".into(),
            sha: HEAD.into(),
            reviewer: reviewer.into(),
            summary: "fixture pass".into(),
            report: report.clone(),
            at: 1,
        });
        rec.observed = Some(Observed {
            head: HEAD.into(),
            pr_state: "OPEN".into(),
            ci_green: true,
            ..Default::default()
        });
        let mut all = BTreeMap::new();
        all.insert(ticket.to_string(), rec);
        delivery::save(self.root.path(), &all).unwrap();
        let dir = self.pm().join(PROJECT).join(ticket).join("reports");
        std::fs::create_dir_all(&dir).unwrap();
        let risk_line = risk.map(|r| format!("risk: {r}\n")).unwrap_or_default();
        std::fs::write(
            dir.join("pass.md"),
            format!("# Verdict: {ticket} combined \u{2014} pass\n{risk_line}From: {reviewer}\n"),
        )
        .unwrap();
        let mut receipt = json!({
            "issue": ticket,
            "verdict": "pass",
            "sha": HEAD,
            "reviewer": reviewer,
            "report": report,
            "pr": format!("https://github.com/{REPO}/pull/{PR}"),
            "project": PROJECT,
            "worker": "worker",
        });
        if let Some(r) = risk {
            receipt["risk"] = json!(r);
        }
        self.shared
            .store
            .record_review_verdict(receipt)
            .expect("daemon-attested PASS");
    }
    /// A full legitimate PASS: the verdict state plus the completed
    /// native dispatch turn at the reviewed head the enqueue barrier
    /// requires — a PASS with no owned completed turn is the refused
    /// case, never the seeded one.
    fn save_pass(&self, ticket: &str, reviewer: &str) {
        let seq = self
            .shared
            .store
            .messages("worker")
            .unwrap_or_default()
            .iter()
            .filter(|m| m.id.starts_with("cad225-dispatch-"))
            .count()
            + 1;
        self.dispatch_turn(ticket, seq, true, Some(Some(HEAD)));
        self.save_pass_risk(ticket, reviewer, Some("auto"));
    }

    /// A `working` record whose observed head carries red CI — the lane
    /// the checkup's `fix_turn` would send one fix note to. Unlike
    /// `save_pass` there is no verdict: the CI failure is the trigger.
    fn save_red(&self, ticket: &str) {
        let mut rec = Record::new(ticket, PROJECT, "worker", 1);
        rec.state = State::Working;
        rec.pr = Some(format!("https://github.com/{REPO}/pull/{PR}"));
        rec.head = Some(HEAD.into());
        rec.observed = Some(Observed {
            head: HEAD.into(),
            pr_state: "OPEN".into(),
            ci_green: false,
            ..Default::default()
        });
        let mut all = BTreeMap::new();
        all.insert(ticket.to_string(), rec);
        delivery::save(self.root.path(), &all).unwrap();
    }

    /// Flip only the recorded observation to red CI — the state, the
    /// PASS verdict and every other field stand. The fix trigger sees
    /// a genuinely landable record whose CI failed, so whatever moves
    /// the lane next is the handler under test, never this seed.
    fn set_observed_red(&self, ticket: &str) {
        let mut all = delivery::load(self.root.path()).unwrap();
        let rec = all
            .get_mut(ticket)
            .unwrap_or_else(|| panic!("{ticket} has no record to redden"));
        let obs = rec
            .observed
            .as_mut()
            .unwrap_or_else(|| panic!("{ticket} has no observation to redden"));
        obs.ci_green = false;
        obs.auto_merge = true;
        delivery::save(self.root.path(), &all).unwrap();
    }

    /// The `done`-report ref a checkup visit to the worker's idle lane
    /// sees for `ticket` — the newest filing, real statuses.
    fn lane_done_ref(
        &self,
        ticket: &str,
        at: i64,
        sha: &str,
    ) -> crate::daemon::next_action::DoneRef {
        crate::daemon::next_action::DoneRef {
            issue: ticket.to_string(),
            project: PROJECT.to_string(),
            name: self.done_report(ticket, at, sha),
            at,
            sha: Some(sha.to_string()),
            pr: Some(format!("https://github.com/{REPO}/pull/{PR}")),
            status: self.issue(ticket).front.status.clone(),
        }
    }

    /// The registered `worker` row the checkup iterates over.
    fn worker_agent(&self) -> crate::store::Agent {
        self.shared
            .store
            .agents()
            .unwrap()
            .into_iter()
            .find(|a| a.alias == "worker")
            .expect("the fixture registers worker")
    }

    /// Complete the turn row the checkup just queued, the way the
    /// provider reports it: running, then finished with the fixed head.
    fn complete_turn(&self, mid: &str, sha: &str) {
        self.shared
            .store
            .mark_running(mid, &format!("{mid}-turn"))
            .expect("provider ack");
        let message = self
            .shared
            .store
            .message(mid)
            .expect("turn reads")
            .expect("the queued fix turn exists");
        self.shared
            .store
            .finish(&message, "completed", &json!({"sha": sha}), None)
            .expect("turn completes");
    }

    fn observe_state(
        &self,
        who: Asserted,
        ticket: &str,
        head: &str,
        pr_state: &str,
    ) -> Result<Value> {
        self.observe_full(who, ticket, head, pr_state, false)
    }
    /// `delivery_observe` as the real sync path issues it: `auto_merge`
    /// reports what the fresh `pr view` just read — `true` once the
    /// remote PR has an auto-merge request the record did not create.
    fn observe_auto(
        &self,
        who: Asserted,
        ticket: &str,
        head: &str,
        pr_state: &str,
    ) -> Result<Value> {
        self.observe_full(who, ticket, head, pr_state, true)
    }

    fn observe_full(
        &self,
        who: Asserted,
        ticket: &str,
        head: &str,
        pr_state: &str,
        auto_merge: bool,
    ) -> Result<Value> {
        let params = json!({
            "issue": ticket,
            "head": head,
            "pr_state": pr_state,
            "ci_green": true,
            "auto_merge": auto_merge,
            "additions": 1,
            "deletions": 0,
            "files": 1
        });
        let shared = self.shared.clone();
        scoped(who, || {
            shared.rpc_delivery_observe(&params, std::process::id())
        })
    }

    fn observe(&self, who: Asserted, ticket: &str, head: &str) -> Result<Value> {
        self.observe_state(who, ticket, head, "OPEN")
    }

    fn merge_approve(&self, who: Asserted, ticket: &str, sha: &str) -> Result<Value> {
        let params = json!({"issue": ticket, "sha": sha});
        let shared = self.shared.clone();
        scoped(who, || {
            shared.rpc_delivery_approve(&params, std::process::id())
        })
    }

    /// The operator-visible account of a refusal: the RPC answer or error,
    /// the delivery record's state and note, and the held/notice events the
    /// loop wrote.
    fn refusal_surface(&self, ticket: &str, result: &Result<Value>) -> String {
        let mut text = match result {
            Ok(body) => body.to_string(),
            Err(err) => err.to_string(),
        };
        if let Ok(records) = delivery::load(self.root.path()) {
            if let Some(rec) = records.get(ticket) {
                text.push(' ');
                text.push_str(rec.state.as_str());
                if let Some(note) = &rec.note {
                    text.push(' ');
                    text.push_str(note);
                }
            }
        }
        for stream in [
            crate::store::Store::DAEMON_STREAM,
            crate::store::APPROVAL_STREAM,
        ] {
            if let Ok(events) = self.shared.store.events_tail(stream, 40) {
                for event in events {
                    text.push(' ');
                    text.push_str(&event.kind);
                    text.push(' ');
                    text.push_str(&event.payload.to_string());
                }
            }
        }
        text.to_lowercase()
    }

    /// A "wait, never refuse" outcome: a newer CI attempt still running
    /// or failing does not land and does not spam the operator with a
    /// human-refusal hold — the call answers `Ok`, no intent, no merge,
    /// no approval, no hold flag, the grant intact, and the record's
    /// observed CI reports not-green so no other path treats it as
    /// ready.
    fn assert_enqueue_waited(&self, ticket: &str, result: &Result<Value>) {
        let body = result
            .as_ref()
            .expect("a newest-attempt wait is an Ok answer, never an operator refusal");
        assert!(
            body["unattended_refusal"].is_null(),
            "a CI wait must not surface a refusal: {}",
            self.refusal_surface(ticket, result)
        );
        let rec = delivery::load(self.root.path())
            .unwrap()
            .get(ticket)
            .cloned()
            .expect("the waiting ticket still has a delivery record");
        assert_ne!(rec.state, State::Enqueued);
        assert_ne!(rec.state, State::Merged);
        assert!(
            !rec.unattended_hold,
            "a CI wait must not flag a human-refusal hold"
        );
        let observed = rec.observed.as_ref().expect("the observe persisted");
        assert!(
            !observed.ci_green,
            "a wait must overwrite the observe's claimed ci_green"
        );
        assert!(
            self.shared
                .store
                .unattended_merge_intent(ticket, HEAD)
                .unwrap()
                .is_none(),
            "a waited enqueue wrote a durable merge intent"
        );
        assert!(self.snap().merges.is_empty(), "scripted gh merge ran");
        assert_eq!(
            self.recorded_merge_approvals(),
            0,
            "a wait wrote an operator approval"
        );
        assert!(
            self.shared
                .unattended_authorization_for_issue(ticket)
                .unwrap()
                .is_some(),
            "a CI wait consumed the grant"
        );
    }

    /// A refused automatic enqueue: the observation carried a refusal, the
    /// record is held for the operator, no durable enqueue intent exists,
    /// the grant survives, no `gh pr merge` ran and no merge approval was
    /// recorded. Guard identity is proven by the `gh` call log at the case
    /// site, never by message text.
    fn assert_not_enqueued(&self, ticket: &str, result: &Result<Value>) {
        let refused = match result {
            Ok(body) => body["unattended_refusal"].as_str().map(str::to_string),
            Err(err) => Some(err.to_string()),
        };
        assert!(
            refused.is_some_and(|why| !why.is_empty()),
            "automatic land carried no refusal: {}",
            self.refusal_surface(ticket, result)
        );
        let rec = delivery::load(self.root.path())
            .unwrap()
            .get(ticket)
            .cloned()
            .expect("the refused ticket still has a delivery record");
        assert_ne!(rec.state, State::Enqueued);
        assert_ne!(rec.state, State::Merged);
        assert!(
            rec.unattended_hold,
            "a held automatic landing must flag the record for the operator"
        );
        assert!(
            rec.note.as_deref().is_some_and(|n| !n.is_empty()),
            "the held record must carry the refusal for the operator"
        );
        assert!(
            self.shared
                .store
                .unattended_merge_intent(ticket, HEAD)
                .unwrap()
                .is_none(),
            "a refused enqueue wrote a durable merge intent"
        );
        assert!(self.snap().merges.is_empty(), "scripted gh merge ran");
        assert_eq!(
            self.recorded_merge_approvals(),
            0,
            "land wrote an operator approval"
        );
        assert!(
            self.shared
                .unattended_authorization_for_issue(ticket)
                .unwrap()
                .is_some(),
            "a refused land consumed the grant"
        );
    }

    /// A settled-merge refusal: the merge happened but the ticket must stay
    /// open, the record's `ticket_done` carries the refusal, and nothing
    /// landed on the merge or approval surfaces.
    fn assert_merge_held(&self, ticket: &str, result: &Result<Value>) {
        let surface = self.refusal_surface(ticket, result);
        let rec = delivery::load(self.root.path())
            .unwrap()
            .get(ticket)
            .cloned()
            .expect("the merged ticket still has a delivery record");
        assert_eq!(rec.state, State::Merged);
        let why = rec
            .ticket_done
            .as_ref()
            .and_then(|done| done.open())
            .unwrap_or("");
        assert!(
            !why.is_empty(),
            "the merge must not mark {ticket} done: {surface}"
        );
        assert_ne!(
            self.issue(ticket).front.status,
            "done",
            "held postmerge settle marked the ticket done"
        );
        assert!(self.snap().merges.is_empty());
        assert_eq!(self.recorded_merge_approvals(), 0);
        assert!(
            self.shared
                .store
                .events_tail(crate::store::Store::DAEMON_STREAM, 40)
                .unwrap()
                .iter()
                .all(|event| event.kind != "ticket_done_on_merge"),
            "a held settle recorded a ticket-done notice"
        );
    }
}

fn bound_inputs() -> Value {
    json!({"topic": TOPIC})
}

#[test]
fn cad225_agent_cannot_issue_widen_forge_or_replay_unattended_authority() {
    let world = World::open();
    let body = world.unattended(&[PATH], bound_inputs(), None, None);

    let before = world.snap();
    let agent = world.approve(Asserted::Agent("worker".into()), body.clone());
    let agent_err = agent
        .expect_err("derived agent must not issue a grant")
        .to_string();
    assert!(agent_err.contains("operator"), "{agent_err}");
    world.assert_unchanged(&before);

    let unproven = world.approve(Asserted::Unproven, body.clone());
    assert!(unproven
        .expect_err("unproven caller")
        .to_string()
        .contains("operator"));
    world.assert_unchanged(&before);

    let mut forged_field = body.clone();
    forged_field["by"] = json!("operator");
    world.assert_refused(world.approve(Asserted::Operator, forged_field), &before);

    world.assert_refused(
        world.approve(
            Asserted::Operator,
            world.unattended(
                &[PATH],
                bound_inputs(),
                None,
                Some(json!({"grant_id": "forged-grant"})),
            ),
        ),
        &before,
    );

    let issued = world
        .approve(Asserted::Operator, body.clone())
        .expect("operator issues the only grant");
    let grant_id = world.require_native_grant(&issued);
    let live = world.snap();
    assert_ne!(live, before);

    world.assert_refused(world.approve(Asserted::Agent("worker".into()), body), &live);
    world.assert_refused(
        world.approve(
            Asserted::Agent("worker".into()),
            world.unattended(&[PATH, "src/evil.rs"], bound_inputs(), None, None),
        ),
        &live,
    );
    world.assert_refused(
        world.approve(
            Asserted::Operator,
            world.unattended(&["**"], bound_inputs(), None, None),
        ),
        &live,
    );
    assert!(
        world.workflow_approval().to_string().contains(&grant_id),
        "a rejected widen cleared the operator grant"
    );

    // The grant the operator issued covers the render, not the caller: a
    // derived agent proposing the byte-exact bound render consumes it —
    // this is the CAD-225 acceptance path (inactive user's goal lands
    // without the operator online for admission).
    let admitted = world.assert_plan_admitted(Asserted::Agent("worker".into()), &grant_id);
    assert_eq!(world.issue(&admitted.ticket).front.status, "ready");

    // An unproven caller is refused before the grant check runs.
    let unproven_propose = world.propose_workflow(Asserted::Unproven, bound_inputs(), None);
    assert!(
        unproven_propose.is_err(),
        "an unattributable caller proposed"
    );
}

#[test]
fn cad225_grant_shape_refuses_wildcard_traversal_ttl_and_loose_flags() {
    let world = World::open();
    let before = world.snap();
    let bad_paths: &[&[&str]] = &[
        &[],
        &["*"],
        &["**"],
        &["../x"],
        &["/etc/passwd"],
        &["src/ship.rs/../../x"],
    ];
    for paths in bad_paths {
        world.assert_refused(
            world.approve(
                Asserted::Operator,
                world.unattended(paths, bound_inputs(), None, None),
            ),
            &before,
        );
    }
    world.assert_refused(
        world.approve(
            Asserted::Operator,
            world.unattended(&[PATH], bound_inputs(), Some(0), None),
        ),
        &before,
    );
    world.assert_refused(
        world.approve(
            Asserted::Operator,
            world.unattended(&[PATH], bound_inputs(), Some(86_401), None),
        ),
        &before,
    );
    let mut foreign = world.unattended(&[PATH], bound_inputs(), None, None);
    foreign["unattended"]["repo"] = json!("evil/other");
    world.assert_refused(world.approve(Asserted::Operator, foreign), &before);

    // Loose flags are not the opt-in: repo/paths/ttl at the top level and a
    // bare boolean both must be refused rather than parsed as authority.
    world.assert_refused(
        world.approve(
            Asserted::Operator,
            json!({
                "project": PROJECT,
                "name": WORKFLOW,
                "repo": REPO,
                "paths": [PATH],
                "inputs": {"topic": TOPIC},
                "ttl_secs": 3600
            }),
        ),
        &before,
    );
    world.assert_refused(
        world.approve(
            Asserted::Operator,
            json!({"project": PROJECT, "name": WORKFLOW, "unattended": true}),
        ),
        &before,
    );
}

#[test]
fn cad225_changed_template_or_input_cannot_auto_admit() {
    let world = World::open();
    world
        .approve(
            Asserted::Operator,
            world.unattended(&[PATH], bound_inputs(), None, None),
        )
        .unwrap();
    let digest = crate::issue::workflow::gate_digest(WORKFLOW_TEXT).unwrap();
    let changed = WORKFLOW_TEXT.replace("Context for", "Background for");
    assert_eq!(
        crate::issue::workflow::gate_digest(&changed).unwrap(),
        digest,
        "this edit is wording only; a gate-digest check would still admit it"
    );
    world.write_workflow(&changed, false);
    let before = world.issues();
    world.assert_not_admitted(
        &before,
        world.propose_workflow(Asserted::Operator, bound_inputs(), None),
    );
    // The grant bound the old bytes. Restoring them must admit on that same
    // grant — if the refused proposal consumed it, this cannot pass.
    world.write_workflow(WORKFLOW_TEXT, false);
    let still_granted = world.workflow_approval().to_string().contains("grant_id");
    assert!(
        still_granted,
        "the refused proposal must not clear the grant"
    );
    let commits = world.commits();
    let proposed = world
        .propose_workflow(Asserted::Operator, bound_inputs(), None)
        .expect("restored template admits on the surviving grant");
    assert_eq!(proposed["state"].as_str(), Some("approved"));
    assert_eq!(world.commits(), commits + 1);
    let plan = world
        .issue(proposed["epic"].as_str().unwrap())
        .front
        .plan
        .unwrap();
    assert_eq!(plan.state, "approved");
    let ticket = plan.tickets[0].clone();
    assert!(
        world
            .shared
            .unattended_authorization_for_issue(&ticket)
            .unwrap()
            .is_some(),
        "the restored-render admission must bind the run"
    );

    // A different input renders different plan text; the grant does not
    // cover it. The original render still admits on the same grant.
    let world = World::open();
    world
        .approve(
            Asserted::Operator,
            world.unattended(&[PATH], bound_inputs(), None, None),
        )
        .unwrap();
    let before = world.issues();
    world.assert_not_admitted(
        &before,
        world.propose_workflow(Asserted::Operator, json!({"topic": "go"}), None),
    );
    let commits = world.commits();
    let proposed = world
        .propose_workflow(Asserted::Operator, bound_inputs(), None)
        .expect("the bound render admits");
    assert_eq!(proposed["state"].as_str(), Some("approved"));
    assert_eq!(world.commits(), commits + 1);
}

#[test]
fn cad225_moved_repo_or_missing_worker_cannot_auto_admit() {
    let world = World::open();
    world
        .approve(
            Asserted::Operator,
            world.unattended(&[PATH], bound_inputs(), None, None),
        )
        .unwrap();
    assert!(std::process::Command::new("git")
        .arg("-C")
        .arg(world.checkout())
        .args([
            "remote",
            "set-url",
            "origin",
            "https://github.com/evil/other.git"
        ])
        .status()
        .unwrap()
        .success());
    let before = world.issues();
    world.assert_not_admitted(
        &before,
        world.propose_workflow(Asserted::Operator, bound_inputs(), None),
    );
    assert!(std::process::Command::new("git")
        .arg("-C")
        .arg(world.checkout())
        .args([
            "remote",
            "set-url",
            "origin",
            "https://github.com/acme/widgets.git"
        ])
        .status()
        .unwrap()
        .success());
    let proposed = world
        .propose_workflow(Asserted::Operator, bound_inputs(), None)
        .expect("restored remote admits on the surviving grant");
    assert_eq!(proposed["state"].as_str(), Some("approved"));

    let world = World::open();
    world
        .approve(
            Asserted::Operator,
            world.unattended(&[PATH], bound_inputs(), None, None),
        )
        .unwrap();
    world
        .shared
        .store
        .remove_agent(
            "worker",
            true,
            &json!({"by": "operator", "by_kind": "operator"}),
        )
        .expect("remove the approved worker");
    let before = world.issues();
    world.assert_not_admitted(
        &before,
        world.propose_workflow(Asserted::Operator, bound_inputs(), None),
    );
    world.register("worker", "worker");
    let proposed = world
        .propose_workflow(Asserted::Operator, bound_inputs(), None)
        .expect("restored worker admits on the surviving grant");
    assert_eq!(proposed["state"].as_str(), Some("approved"));
}

#[test]
fn cad225_caller_grant_flags_are_not_authority() {
    let world = World::open();
    world
        .approve(
            Asserted::Operator,
            world.unattended(&[PATH], bound_inputs(), None, None),
        )
        .unwrap();
    let before = world.snap();
    world.assert_refused(
        world.propose_workflow(
            Asserted::Operator,
            bound_inputs(),
            Some(json!({
                "grant_id": "forged-grant",
                "approved": true,
                "unattended": {"repo": REPO, "paths": [PATH]}
            })),
        ),
        &before,
    );
    let admitted = world.grant_and_admit(None);
    assert_ne!(admitted.grant_id, "forged-grant");
}

#[test]
fn cad225_ordinary_workflow_file_and_app_stay_manual() {
    let world = World::open();
    world
        .approve(
            Asserted::Operator,
            json!({"project": PROJECT, "name": WORKFLOW}),
        )
        .expect("ordinary workflow approval");
    assert!(
        find_field(&world.workflow_approval(), "grant_id").is_none(),
        "ordinary approval minted unattended authority"
    );
    let proposed = world
        .propose_workflow(Asserted::Operator, bound_inputs(), None)
        .expect("ordinary propose");
    assert_eq!(proposed["state"].as_str(), Some("proposed"));
    let epic = proposed["epic"].as_str().unwrap();
    let plan = world.issue(epic).front.plan.unwrap();
    assert_eq!(plan.state, "proposed");
    let ticket = plan.tickets[0].clone();
    assert_eq!(world.issue(&ticket).front.status, "backlog");
    assert!(world
        .shared
        .unattended_authorization_for_issue(&ticket)
        .unwrap()
        .is_none());
    world.bind_job(&ticket);
    let gate = world
        .shared
        .plan_gate_task("cad225-job-t1")
        .unwrap_err()
        .to_string();
    assert!(
        gate.contains("proposed") || gate.contains("plan_not_approved"),
        "{gate}"
    );

    let world = World::open();
    world
        .approve(
            Asserted::Operator,
            world.unattended(&[PATH], bound_inputs(), None, None),
        )
        .unwrap();
    let file = world
        .propose_value(json!({"project": PROJECT, "text": FILE_PLAN}))
        .expect("file plan stays proposed");
    assert_eq!(file["state"].as_str(), Some("proposed"));
    let file_epic = file["epic"].as_str().unwrap();
    for ticket in world.issue(file_epic).front.plan.unwrap().tickets {
        assert!(world
            .shared
            .unattended_authorization_for_issue(&ticket)
            .unwrap()
            .is_none());
    }
    let before = world.issues();
    let app = world.propose_value(json!({"project": PROJECT, "workflow": "crm/ship"}));
    world.assert_not_admitted(&before, app);
    let _ = world.grant_and_admit(None);
}

#[test]
fn cad225_duplicate_propose_and_restart_return_the_same_plan() {
    let mut world = World::open();
    let first = world.grant_and_admit(None);
    let now = now_secs();
    let run = world
        .shared
        .unattended_authorization_for_issue(&first.ticket)
        .unwrap()
        .unwrap();
    assert!(
        run.expires_at > now + 3_000.0 && run.expires_at < now + 3_700.0,
        "default ttl is 3600s, got {}",
        run.expires_at
    );
    let commits = world.commits();
    let issues = world.issues();
    let again = world
        .propose_workflow(Asserted::Operator, bound_inputs(), None)
        .expect("duplicate propose");
    assert_eq!(again["epic"].as_str(), Some(first.epic.as_str()));
    if let Some(tickets) = again["tickets"].as_array() {
        assert_eq!(
            tickets.first().and_then(Value::as_str),
            Some(first.ticket.as_str()),
            "a repeated proposal must return the same epic and tickets"
        );
    }
    assert_eq!(
        world.commits(),
        commits,
        "duplicate propose committed again"
    );
    assert_eq!(world.issues(), issues);

    let root = world.root.path().to_path_buf();
    drop(world.shared);
    world.shared = World::reopen(root);
    let restarted = world
        .propose_workflow(Asserted::Operator, bound_inputs(), None)
        .expect("propose after restart");
    assert_eq!(restarted["epic"].as_str(), Some(first.epic.as_str()));
    assert_eq!(world.issues(), issues, "restart allocated a second plan");
    assert_eq!(world.commits(), commits);
    let run = world
        .shared
        .unattended_authorization_for_issue(&first.ticket)
        .unwrap()
        .unwrap();
    assert_eq!(run.grant_id, first.grant_id);
    assert_eq!(run.epic, first.epic);
}

#[test]
fn cad225_expiry_revocation_replay_and_semantic_change_drop_authority() {
    let world = World::open();
    let admitted = world.grant_and_admit(Some(8));
    let run = world
        .shared
        .unattended_authorization_for_issue(&admitted.ticket)
        .unwrap()
        .unwrap();
    let now = now_secs();
    assert!(
        run.expires_at > now && run.expires_at <= now + 12.0,
        "ttl 8 was not the server expiry ({})",
        run.expires_at
    );
    let wait = (run.expires_at - now + 1.0).clamp(0.0, 15.0);
    std::thread::sleep(Duration::from_secs_f64(wait));
    world.assert_invalid_run(&admitted.ticket);
    world.bind_job(&admitted.ticket);
    assert!(
        crate::issue::plan::gate_id(&world.pm(), &admitted.ticket).is_ok(),
        "revoked or expired authority must not be simulated by un-approving the plan"
    );
    assert!(
        world.shared.plan_gate_task("cad225-job-t1").is_err(),
        "issue-bound job gate fell back to legacy automatic authority"
    );
    let issues = world.issues();
    let again = world.propose_workflow(Asserted::Operator, bound_inputs(), None);
    if let Ok(body) = &again {
        assert_ne!(body["state"].as_str(), Some("approved"));
        assert_ne!(body["epic"].as_str(), Some(admitted.epic.as_str()));
    }
    let approved: Vec<_> = world
        .issues()
        .iter()
        .filter(|id| {
            world
                .issue(id)
                .front
                .plan
                .as_ref()
                .is_some_and(|plan| plan.state == "approved")
        })
        .cloned()
        .collect();
    assert_eq!(approved, vec![admitted.epic.clone()]);
    let _ = issues;

    // Expired authority also holds automatic postmerge settlement: the
    // grant is gone, so a merged head cannot finish the ticket.
    world.save_pass(&admitted.ticket, "reviewer");
    let merged = world.observe_state(Asserted::Operator, &admitted.ticket, HEAD, "MERGED");
    world.assert_merge_held(&admitted.ticket, &merged);

    // Renewal is re-issuance: an agent may not renew at all, and the
    // operator's renew must re-derive every immutable bound — a drifted
    // template must refuse, never silently widen the grant.
    world.assert_refused(
        world.approve(
            Asserted::Agent("worker".into()),
            json!({"project": PROJECT, "name": WORKFLOW, "renew": admitted.grant_id}),
        ),
        &world.snap(),
    );
    world.write_workflow(
        &WORKFLOW_TEXT.replace("Context for", "Background for"),
        false,
    );
    let renewed_drift = world.approve(
        Asserted::Operator,
        json!({"project": PROJECT, "name": WORKFLOW, "renew": admitted.grant_id}),
    );
    assert!(
        renewed_drift.is_err(),
        "renewal under a drifted template must refuse: {renewed_drift:?}"
    );
    world.write_workflow(WORKFLOW_TEXT, false);

    let world = World::open();
    let admitted = world.grant_and_admit(None);
    let captured = world.workflow_approval();
    world
        .approve(
            Asserted::Operator,
            json!({"project": PROJECT, "name": WORKFLOW}),
        )
        .expect("reapprove without unattended revokes the grant");
    world.assert_invalid_run(&admitted.ticket);
    assert_eq!(
        world.issue(&admitted.epic).front.plan.unwrap().state,
        "approved"
    );
    world
        .shared
        .store
        .record_workflow_approval(captured)
        .expect("replay the captured approval payload");
    world.assert_invalid_run(&admitted.ticket);
    let before = world.issues();
    world.assert_not_admitted(
        &before,
        world.propose_workflow(Asserted::Operator, bound_inputs(), None),
    );
    world.bind_job(&admitted.ticket);
    assert!(crate::issue::plan::gate_id(&world.pm(), &admitted.ticket).is_ok());
    assert!(world.shared.plan_gate_task("cad225-job-t1").is_err());

    let world = World::open();
    let admitted = world.grant_and_admit(None);
    let path = world
        .pm()
        .join(PROJECT)
        .join(&admitted.ticket)
        .join("issue.md");
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(
        path,
        text.replace("diff stays on src/ship.rs", "diff may touch any path"),
    )
    .unwrap();
    world.assert_invalid_run(&admitted.ticket);
}

#[test]
fn cad225_replayed_older_renewal_adds_no_credit() {
    // A captured approval payload that was once the CURRENT record —
    // an earlier renewal replayed after a newer one — is withheld the
    // same way a grant killed by an ordinary re-approve is: the daemon
    // owns the revision counter, the live expiry is the newest
    // renewal's, and replaying the still-current record verbatim is the
    // only idempotent write — never a widened one.
    let world = World::open();
    let admitted = world.grant_and_admit(None);
    let first = world
        .approve(
            Asserted::Operator,
            json!({"project": PROJECT, "name": WORKFLOW,
                   "renew": admitted.grant_id, "ttl_secs": 7200}),
        )
        .expect("the first renewal lands");
    assert_eq!(first["unattended"]["revision"].as_u64(), Some(1));
    let r1 = world.workflow_approval();
    let e1 = first["unattended"]["expires_at"].as_f64().unwrap();
    let second = world
        .approve(
            Asserted::Operator,
            json!({"project": PROJECT, "name": WORKFLOW,
                   "renew": admitted.grant_id, "ttl_secs": 5400}),
        )
        .expect("the second renewal lands");
    assert_eq!(second["unattended"]["revision"].as_u64(), Some(2));
    let e2 = second["unattended"]["expires_at"].as_f64().unwrap();
    assert_ne!(e1, e2, "the arms need distinct live expiries");

    // Replaying the superseded renewal makes it the newest stream event:
    // the run still reads authorized at the real renewal's expiry — the
    // replay neither rolls the expiry back nor mints credit past it.
    world
        .shared
        .store
        .record_workflow_approval(r1)
        .expect("replay the superseded renewal payload");
    let run = world
        .shared
        .unattended_authorization_for_issue(&admitted.ticket)
        .expect("bound run reads")
        .unwrap_or_else(|| panic!("an older replay must not revoke {}", admitted.grant_id));
    assert_eq!(
        run.expires_at, e2,
        "an older replayed renewal must not widen or rewind the live expiry"
    );

    // Replaying the still-current record is the idempotent case: nothing
    // moves, nothing widens, and the run's expiry is still exactly e2.
    let current = world.workflow_approval();
    let before = world.issues();
    world
        .shared
        .store
        .record_workflow_approval(current)
        .expect("replay the current record verbatim");
    let run = world
        .shared
        .unattended_authorization_for_issue(&admitted.ticket)
        .expect("bound run reads")
        .expect("the live renewal stays authorized");
    assert_eq!(run.expires_at, e2, "an idempotent replay must not widen");
    let dedupe = world
        .propose_workflow(Asserted::Operator, bound_inputs(), None)
        .expect("the live grant's dedupe answers");
    assert_eq!(
        dedupe["reused"].as_bool(),
        Some(true),
        "an idempotent replayed authority still admits only its own ids: {dedupe}"
    );
    assert_eq!(dedupe["epic"].as_str(), Some(admitted.epic.as_str()));
    assert_eq!(
        dedupe["tickets"].as_array(),
        Some(&vec![json!(admitted.ticket.clone())]),
        "the dedupe returns the bound ticket, never a new id"
    );
    assert_eq!(
        world.issues(),
        before,
        "the idempotent replay allocated a second plan"
    );

    // The next genuine renewal continues from the daemon-owned revision —
    // a replayed payload can never stand in for renewal credit.
    let third = world
        .approve(
            Asserted::Operator,
            json!({"project": PROJECT, "name": WORKFLOW,
                   "renew": admitted.grant_id}),
        )
        .expect("the third renewal lands");
    assert_eq!(
        third["unattended"]["revision"].as_u64(),
        Some(3),
        "replayed history must not stall or skip the daemon's revision"
    );
}

#[test]
fn cad225_out_of_scope_moved_head_and_self_review_cannot_enqueue() {
    let world = World::open();
    let admitted = world.grant_and_admit(None);
    world.save_pass(&admitted.ticket, "reviewer");
    let before = world.snap();

    let agent_observe = world.observe(Asserted::Agent("worker".into()), &admitted.ticket, HEAD);
    assert!(
        agent_observe.is_err(),
        "an agent cannot drive the observer RPC"
    );
    world.assert_unchanged(&before);
    let agent_approve =
        world.merge_approve(Asserted::Agent("worker".into()), &admitted.ticket, HEAD);
    assert!(
        agent_approve.is_err(),
        "an agent cannot approve a delivery head"
    );
    world.assert_unchanged(&before);

    // The real compare transport decides the diff: `outside` adds a path
    // the allowlist does not name, `delete` removes it, `mode` makes it
    // executable. Each observe must fail at the scope check — the compare
    // ran (the call log shows it), no enqueue intent was written.
    for case in ["outside", "delete", "mode"] {
        world.set_case(case);
        world.set_head(HEAD);
        world.save_pass(&admitted.ticket, "reviewer");
        let compares = world.gh_calls("compare/");
        let merges_seen = world.gh_calls("pr merge");
        let result = world.observe(Asserted::Operator, &admitted.ticket, HEAD);
        world.assert_not_enqueued(&admitted.ticket, &result);
        assert!(
            world.gh_calls("compare/") > compares,
            "the guard never read the actual diff — nothing real was scoped"
        );
        assert_eq!(world.gh_calls("pr merge"), merges_seen);
    }
    // A repeated identical hold is one actionable row and one event, not a
    // growing list.
    world.set_case("outside");
    world.save_pass(&admitted.ticket, "reviewer");
    let first = world.observe(Asserted::Operator, &admitted.ticket, HEAD);
    world.assert_not_enqueued(&admitted.ticket, &first);
    let held = world.held_events();
    let second = world.observe(Asserted::Operator, &admitted.ticket, HEAD);
    world.assert_not_enqueued(&admitted.ticket, &second);
    assert_eq!(
        world.held_events(),
        held,
        "identical refusal stacked a second held event"
    );
    assert_eq!(
        world.held_rows(&admitted.ticket),
        1,
        "repeated refusal must yield one actionable overview row"
    );

    // `latemove` makes only the enqueue-path `pr view` (the
    // `autoMergeRequest` field list) answer a moved head: classification
    // and the CI floor still see HEAD, so the refusal can only be the
    // pre-enqueue head check — proven by the stage log, not a message.
    world.set_case("latemove");
    world.set_head(HEAD);
    world.save_pass(&admitted.ticket, "reviewer");
    let api_stage = world.gh_calls("workflows/ci.yml/runs");
    let moved = world.observe(Asserted::Operator, &admitted.ticket, HEAD);
    world.assert_not_enqueued(&admitted.ticket, &moved);
    assert!(
        world.gh_calls("workflows/ci.yml/runs") > api_stage,
        "the refusal fired before the CI floor — wrong guard"
    );

    // `rpc_delivery_approve` re-reads the live head with the boot-fixed gh;
    // a head that moved after review is not approved by the old SHA.
    world.set_case("in");
    world.set_head(MOVED);
    world.save_pass(&admitted.ticket, "reviewer");
    let approve = world.merge_approve(Asserted::Operator, &admitted.ticket, HEAD);
    assert!(
        approve.is_err(),
        "approving SHA {HEAD} while the live head is {MOVED} must refuse"
    );
    assert!(world.snap().merges.is_empty());
    assert_eq!(world.recorded_merge_approvals(), 0);

    // The worker reviewing its own head is no independent PASS.
    world.set_head(HEAD);
    world.save_pass(&admitted.ticket, "worker");
    let self_review = world.observe(Asserted::Operator, &admitted.ticket, HEAD);
    world.assert_not_enqueued(&admitted.ticket, &self_review);

    // The reviewer's declared risk floor: the native receipt is real and
    // current in both cases — a `human` declaration or a missing `risk`
    // must withhold automatic landing even though the path class is
    // consequential and the PASS is independent. `review_export_recorded`
    // holds while `unattended_review_recorded` must not.
    for risk in [Some("human (1)"), None] {
        world.set_case("in");
        world.set_head(HEAD);
        world.save_pass_risk(&admitted.ticket, "reviewer", risk);
        let result = world.observe(Asserted::Operator, &admitted.ticket, HEAD);
        world.assert_not_enqueued(&admitted.ticket, &result);
        let rec = delivery::load(world.root.path())
            .unwrap()
            .get(&admitted.ticket)
            .cloned()
            .expect("delivery record");
        assert!(
            world.shared.store.review_export_recorded(&rec).unwrap(),
            "the PASS receipt must still exist — only the risk floor withholds"
        );
        assert!(
            !world.shared.store.unattended_review_recorded(&rec).unwrap(),
            "risk {risk:?} must not qualify for unattended landing"
        );
    }

    // Merged with no recorded enqueue intent: settle must hold, not mark
    // the ticket done.
    world.save_pass(&admitted.ticket, "reviewer");
    let merged = world.observe_state(Asserted::Operator, &admitted.ticket, HEAD, "MERGED");
    world.assert_merge_held(&admitted.ticket, &merged);
}
#[test]
fn cad225_done_report_cannot_review_before_its_completed_dispatch_turn() {
    // `route_delivery` regression: a bound run's `done` report is evidence
    // only when the daemon can correlate it with the kickoff turn that was
    // supposed to produce it — the latest `source='dispatch'` row for
    // (issue, worker), `completed`, whose recorded result sha IS the head
    // the report names. A report with no turn behind it or a completed
    // turn whose result is another head is evidence of corruption — the
    // record escalates for the operator; a still-queued/running turn only
    // waits. In every case `start_review` never runs: no review kickoff.
    let world = World::open();
    let admitted = world.grant_and_admit(None);
    let ticket = &admitted.ticket;
    let record = |state| {
        let mut all = delivery::load(world.root.path()).unwrap();
        let mut rec = Record::new(ticket, PROJECT, "worker", 1);
        rec.state = state;
        all.insert(ticket.clone(), rec);
        delivery::save(world.root.path(), &all).unwrap();
    };
    let rec_state = || {
        delivery::load(world.root.path())
            .unwrap()
            .get(ticket)
            .expect("the bound ticket still has a delivery record")
            .state
    };

    // Phase 1 — a `done` report exists but no dispatch turn row does: the
    // report predates any kickoff this lane ever ran.
    record(State::Working);
    world.done_report(ticket, now_secs() as i64, HEAD);
    world
        .shared
        .route_delivery()
        .expect("a router pass without a dispatch turn answers, never panics");
    assert_eq!(
        rec_state(),
        State::Escalated,
        "a done report with no dispatch turn must hold for the operator, not review"
    );
    assert!(
        world.routed(ticket).is_empty(),
        "no dispatch turn -> no review kickoff may be sent"
    );

    // Phase 2 — the kickoff exists but the provider never finished it:
    // `queued` (no ack) and `running` (acked, no completion) both mean the
    // report was filed ahead of the turn's lifecycle; the lane just waits.
    record(State::Working);
    world.dispatch_turn(ticket, 1, false, None);
    world.done_report(ticket, now_secs() as i64 + 1, HEAD);
    world
        .shared
        .route_delivery()
        .expect("a router pass on a queued turn answers");
    assert_eq!(
        rec_state(),
        State::Working,
        "a report filed while the kickoff is only queued routed review"
    );
    assert!(
        world.routed(ticket).is_empty(),
        "a queued turn -> no review kickoff may be sent"
    );

    // Phase 3 — the provider acknowledged the turn but it is still running.
    record(State::Working);
    world.dispatch_turn(ticket, 2, true, None);
    world.done_report(ticket, now_secs() as i64 + 2, HEAD);
    world
        .shared
        .route_delivery()
        .expect("a router pass on a running turn answers");
    assert_eq!(
        rec_state(),
        State::Working,
        "a report filed while the kickoff is running routed review"
    );
    assert!(
        world.routed(ticket).is_empty(),
        "a running turn -> no review kickoff may be sent"
    );
    // Phase 4 — the turn completed but recorded no head at all.
    record(State::Working);
    world.dispatch_turn(ticket, 3, false, Some(None));
    world.done_report(ticket, now_secs() as i64 + 3, HEAD);
    world
        .shared
        .route_delivery()
        .expect("a router pass on a headless turn answers");
    assert_eq!(
        rec_state(),
        State::Escalated,
        "a report riding a turn that recorded no sha must hold, not review"
    );
    assert!(
        world.routed(ticket).is_empty(),
        "a turn with no recorded sha -> no review kickoff may be sent"
    );

    // Phase 5 — the turn completed but its recorded result is another head:
    // the report's `sha` is not what this lane produced.
    record(State::Working);
    world.dispatch_turn(ticket, 4, false, Some(Some(MOVED)));
    world.done_report(ticket, now_secs() as i64 + 4, HEAD);
    world
        .shared
        .route_delivery()
        .expect("a router pass on a mismatched turn answers");
    assert_eq!(
        rec_state(),
        State::Escalated,
        "a report whose sha the kickoff never produced must hold, not review"
    );
    assert!(
        world.routed(ticket).is_empty(),
        "a mismatched turn result -> no review kickoff may be sent"
    );

    // Phase 6 — the real lifecycle: the latest dispatch turn completed and
    // its result sha is the report's head. `route_delivery` must now reach
    // `start_review` — one `review_routed`, record `Reviewing`, reviewer is
    // not the worker.
    record(State::Working);
    world.dispatch_turn(ticket, 5, false, Some(Some(HEAD)));
    world.done_report(ticket, now_secs() as i64 + 5, HEAD);
    world
        .shared
        .route_delivery()
        .expect("the completed matching turn must route");
    let routed = world.routed(ticket);
    assert_eq!(
        routed.len(),
        1,
        "exactly one review kickoff after the completed matching turn: {routed:?}"
    );
    assert_eq!(routed[0]["sha"].as_str(), Some(HEAD));
    assert_eq!(routed[0]["reviewer"].as_str(), Some("reviewer"));
    assert_eq!(
        rec_state(),
        State::Reviewing,
        "the record must enter review on the completed matching turn"
    );

    // A replayed router pass on the same turn + report is a no-op — one
    // kickoff, one round, no second `review_routed`.
    world
        .shared
        .route_delivery()
        .expect("the replayed pass answers");
    assert_eq!(
        world.routed(ticket).len(),
        1,
        "a replayed pass routed a second review kickoff"
    );
    assert_eq!(rec_state(), State::Reviewing);
}

#[test]
fn cad225_unattended_merge_lands_in_scope() {
    // The positive counterpart: everything the negative cases refuse, in
    // place — an in-scope diff, green required checks plus the canonical
    // ci.yml run, an enforced merge queue the operator cannot bypass, a
    // live grant, an independent exact-head native PASS. Enqueue must run
    // the real `gh pr merge` once after recording its durable intent;
    // `--auto` success is only a request — the record holds until the
    // queue's own entry proves admission.
    let world = World::open();
    let admitted = world.grant_and_admit(None);
    world.set_case("in");
    world.set_head(HEAD);
    world.save_pass(&admitted.ticket, "reviewer");
    let landed = world
        .observe(Asserted::Operator, &admitted.ticket, HEAD)
        .expect("in-scope authorized run must enqueue");
    let rec = delivery::load(world.root.path())
        .unwrap()
        .get(&admitted.ticket)
        .cloned()
        .expect("delivery record");
    assert_ne!(rec.state, State::Enqueued);
    assert_ne!(rec.state, State::Merged);
    let merges = world.snap().merges;
    assert!(
        merges.contains("pr merge")
            && merges.contains("--match-head-commit")
            && merges.contains(HEAD),
        "enqueue must run the real head-bound merge: {merges}"
    );
    let intent = world
        .shared
        .store
        .unattended_merge_intent(&admitted.ticket, HEAD)
        .expect("intent reads")
        .expect("the durable intent is written before gh runs");
    assert_eq!(
        intent["grant_id"].as_str(),
        Some(admitted.grant_id.as_str())
    );
    assert_eq!(intent["issue"].as_str(), Some(admitted.ticket.as_str()));
    assert_eq!(intent["head"].as_str(), Some(HEAD));
    assert_eq!(
        intent["ci_authority"].as_str(),
        Some("enforced_merge_queue"),
        "the intent must name the queue authority it ran under"
    );
    // `gh pr merge --auto` returned success and auto-merge is live, but
    // the queue has not admitted the head yet: the request is authorized
    // pending — the record stays `Passed` (never Enqueued), no receipt is
    // recorded, the intent is durable, and the answer is not a refusal.
    assert!(
        landed["unattended_refusal"].is_null(),
        "an authorized pending queue request is not a refusal: {landed}"
    );
    assert!(
        !rec.unattended_hold,
        "pending admission is a known queue state, not an operator hold"
    );
    assert!(
        rec.unattended_pending,
        "the bound pending request must be marked pending, never merge-ready"
    );
    assert!(
        !rec.merge_ready(),
        "a pending queue admission must not present a manual merge card"
    );
    assert!(
        landed["merge_ready"].as_bool() == Some(false),
        "the observe answer must not report merge_ready while pending: {landed}"
    );
    assert!(
        world
            .shared
            .store
            .unattended_merge_enqueued(&admitted.ticket, HEAD)
            .unwrap()
            .is_none(),
        "a pending request recorded an enqueue receipt"
    );

    // Repeated sync passes over the bound pending state: zero further
    // mutations, still no receipt, and the auto-merge request the enqueue
    // bound is never flagged for disabling.
    for _ in 0..3 {
        let again = world
            .observe_auto(Asserted::Operator, &admitted.ticket, HEAD, "OPEN")
            .expect("a sync pass over pending admission answers");
        assert!(
            !again["disable_auto"].as_bool().unwrap_or(true),
            "a bound pending request must never disable its own auto-merge: {again}"
        );
    }
    assert_eq!(
        world.gh_calls("pr merge"),
        1,
        "sync passes over pending admission replayed the mutation"
    );
    assert!(
        world
            .shared
            .store
            .unattended_merge_enqueued(&admitted.ticket, HEAD)
            .unwrap()
            .is_none(),
        "a still-null queue entry recorded a receipt"
    );

    // The queue service admits the head: the next observe reads the same
    // durable intent, proves the native entry, never replays the merge.
    world.queue_admitted();
    let second = world
        .observe_auto(Asserted::Operator, &admitted.ticket, HEAD, "OPEN")
        .expect("the reconciling observe answers");
    assert!(
        second["unattended_refusal"].is_null(),
        "an observed queue entry must admit: {second}"
    );
    assert_eq!(
        world.gh_calls("pr merge"),
        1,
        "reconcile must never replay the mutation"
    );
    let enqueued = world
        .shared
        .store
        .unattended_merge_enqueued(&admitted.ticket, HEAD)
        .expect("enqueue proof reads")
        .expect("the observed queue entry must be recorded");
    assert!(
        enqueued["queue_entry"]
            .as_str()
            .is_some_and(|id| !id.is_empty()),
        "the enqueue proof must carry the queue's own entry id"
    );
    let rec = delivery::load(world.root.path())
        .unwrap()
        .get(&admitted.ticket)
        .cloned()
        .expect("delivery record");
    assert_eq!(rec.state, State::Enqueued);
    assert!(!rec.unattended_hold);
    assert!(
        !rec.unattended_pending,
        "the native queue entry clears the pending marker"
    );

    // The observed merge then settles: a live grant, the durable intent,
    // the recorded queue entry and the same independent PASS mark the
    // ticket done — not held. The postmerge check re-reads the live
    // enforcement fixtures.
    let merged = world
        .observe_state(Asserted::Operator, &admitted.ticket, HEAD, "MERGED")
        .expect("settle answers");
    let rec = delivery::load(world.root.path())
        .unwrap()
        .get(&admitted.ticket)
        .cloned()
        .expect("delivery record");
    assert_eq!(rec.state, State::Merged, "{merged}");
    assert!(
        rec.ticket_done
            .as_ref()
            .is_some_and(|done| done.open().is_none()),
        "authorized settle was refused: {merged}"
    );
    assert_eq!(world.issue(&admitted.ticket).front.status, "done");
}

#[test]
fn cad225_pick_reviewer_never_selects_the_author() {
    // `pick_reviewer` is a pure check on the live roster: the worker who
    // authored the head can never be assigned its review, no matter which
    // candidates remain.
    use crate::delivery::{pick_reviewer, Candidate, ReviewRules, REVIEWER_ROLE};
    let author = Candidate {
        alias: "worker".into(),
        provider: "pi".into(),
        model: None,
        state: "idle".into(),
        enabled: true,
        upstream: None,
        role: REVIEWER_ROLE.into(),
    };
    assert!(
        pick_reviewer(
            "worker",
            Some("pi"),
            None,
            &[],
            std::slice::from_ref(&author),
            &ReviewRules::default(),
            &[]
        )
        .is_none(),
        "the author was eligible to review their own head"
    );
    let reviewer = Candidate {
        alias: "reviewer".into(),
        provider: "pi".into(),
        model: None,
        state: "idle".into(),
        enabled: true,
        upstream: None,
        role: REVIEWER_ROLE.into(),
    };
    let pick = pick_reviewer(
        "worker",
        Some("pi"),
        None,
        &[],
        &[author, reviewer],
        &ReviewRules::default(),
        &[],
    )
    .expect("an independent reviewer qualifies");
    assert_eq!(pick.alias, "reviewer");
}

#[test]
fn cad225_bound_issue_lends_no_job_authority() {
    // The grant authorizes ONE exact render's canonical issue dispatch.
    // A caller-authored job task bound to the same issue carries its own
    // spec/assignee/worktree semantics the grant never rendered — every
    // dispatch entry point must refuse it with `unattended_job_scope`,
    // not just the direct `plan_gate_task` call.
    let world = World::open();
    let admitted = world.grant_and_admit(None);
    let ticket = &admitted.ticket;
    world.bind_job(ticket);
    let task_id = "cad225-job-t1";
    let task = || world.shared.store.task(task_id).expect("bound task reads");
    let before_state = task().state;

    // The real `job dispatch` RPC: the deferred-commit gate fires inside
    // the handler before any store transaction writes.
    let shared = world.shared.clone();
    let err = scoped(Asserted::Agent("worker".into()), || {
        shared.rpc_task_dispatch(&json!({
            "task": task_id,
            "to": "worker",
            "by": "operator",
        }))
    })
    .expect_err("a bound issue's job task cannot dispatch");
    assert_eq!(
        err.code(),
        Some("unattended_job_scope"),
        "job dispatch must name its scope refusal: {err}"
    );
    // The automatic monitor enqueue crosses the same gate before its
    // pending-approval mutex and store transaction — an unattended ticket
    // cannot ride a monitor's automatic dispatch either.
    let err = world
        .shared
        .clone()
        .monitor_dispatch_task("mon-auto", task_id, true)
        .expect_err("automatic monitor dispatch cannot take a bound task");
    assert_eq!(
        err.code(),
        Some("unattended_job_scope"),
        "the automatic monitor path must name the same refusal: {err}"
    );

    // Zero task mutation, zero kickoff on either entry point: the task
    // row is untouched, no message reached the worker, and the issue
    // keeps its canonical `ready` shape for `master_dispatch` alone.
    let after = task();
    assert_eq!(after.state, before_state, "the refused task changed state");
    assert!(
        after.dispatch_message.is_none(),
        "a refused dispatch left a dispatch message"
    );
    assert!(
        world
            .shared
            .store
            .messages("worker")
            .unwrap_or_default()
            .iter()
            .all(|m| m.task_id.as_deref() != Some(task_id)),
        "a kickoff reached the worker for the refused task"
    );
    assert_eq!(world.issue(ticket).front.status, "ready");
    assert!(
        world
            .shared
            .unattended_authorization_for_issue(ticket)
            .unwrap()
            .is_some(),
        "the refused dispatches consumed the grant"
    );
}

#[test]
fn cad225_unfinished_ci_attempts_wait_and_queue_authority_gates_landing() {
    // A success row is not "CI green": GitHub keeps every attempt on the
    // head and the newest (run_number, run_attempt) decides — pending or
    // failed waits silently with zero side effects and an observed
    // ci_green of false. `green` is the control: the enforced-queue floor
    // then admits the head-pinned enqueue. Every case crosses
    // `try_unattended_enqueue` -> `full_ci_green` through the real
    // `delivery_observe` RPC.
    for case in ["failed-newer", "pending-newer", "green"] {
        let world = World::open();
        let admitted = world.grant_and_admit(None);
        world.set_case("in");
        world.set_head(HEAD);
        world.set_runs(case);
        world.save_pass(&admitted.ticket, "reviewer");
        let landed = world.observe(Asserted::Operator, &admitted.ticket, HEAD);
        match case {
            "green" => {
                let out = landed.expect("the current green floor still lands");
                assert!(
                    world.snap().merges.contains("--match-head-commit"),
                    "the green control must enqueue the head-pinned merge"
                );
                assert!(
                    world
                        .shared
                        .store
                        .unattended_merge_intent(&admitted.ticket, HEAD)
                        .unwrap()
                        .is_some(),
                    "the green control recorded its durable intent: {out}"
                );
            }
            _ => {
                // A newer pending or failed attempt is a wait, never an
                // operator-facing refusal and never a land.
                world.assert_enqueue_waited(&admitted.ticket, &landed);
            }
        }
        assert!(
            world.gh_calls("workflows/ci.yml/runs") >= 1,
            "{case}: the run evidence was never consulted"
        );
        assert_eq!(
            world.recorded_merge_approvals(),
            0,
            "{case}: a CI wait recorded an approval"
        );
    }
}

#[test]
fn cad225_without_enforced_queue_authority_no_landing() {
    // Automatic landing rides GitHub's enforced merge queue, never a
    // self-reported check. Each case removes exactly one piece of the
    // enforcement chain — the queue ruleset, its bypass closure, the
    // GitHub-Actions binding of a ruleset or protected check, a trusted
    // gate's protected context, the aggregate's, or the workflow's
    // merge_group handling — and the observe must hold for the operator
    // with zero mutation: no `pr merge`, no intent, no approval, grant
    // alive, and the queue-entry probe never reached (authority failed
    // before admission could be asked).
    for (queue, protect, workflow) in [
        ("noqueue", "ok", "full"),
        ("bypass", "ok", "full"),
        ("unbound", "ok", "full"),
        ("ok", "notest", "full"),
        ("ok", "nocross", "full"),
        ("ok", "unbound", "full"),
        ("ok", "ok", "nogroup"),
        ("ok", "notest", "five"),
        ("ok", "ok", "notest"),
    ] {
        let world = World::open();
        let admitted = world.grant_and_admit(None);
        world.set_case("in");
        world.set_head(HEAD);
        world.set_queue(queue);
        world.set_protect(protect);
        world.set_workflow(workflow);
        world.save_pass(&admitted.ticket, "reviewer");
        let landed = world.observe(Asserted::Operator, &admitted.ticket, HEAD);
        world.assert_not_enqueued(&admitted.ticket, &landed);
        assert_eq!(
            world.gh_calls("graphql"),
            0,
            "{queue}/{protect}/{workflow}: admission was probed before authority proved"
        );
        assert_eq!(
            world.gh_calls("pr merge"),
            0,
            "{queue}/{protect}/{workflow}: an unproven queue ran a mutation"
        );
    }
}

#[test]
fn cad225_grant_expiring_during_lock_wait_admits_nothing() {
    // The grant is proved twice: once inside `admit_unattended_workflow`
    // under `unattended_lock`, then by `revalidate_grant` under the PM
    // write lock immediately before `create_granted_plan` commits ids.
    // A grant that dies on the lock queue admits nothing — no epic, no
    // ticket files, no tracker commit, and the stored approval is not
    // consumed (the operator may renew it).
    let world = World::open();
    world
        .approve(
            Asserted::Operator,
            world.unattended(&[PATH], bound_inputs(), Some(6), None),
        )
        .expect("operator unattended grant");
    let pm = crate::issue::Pm::at(&world.pm()).expect("open PM");
    let hold = pm.lock().expect("test holds the tracker lock");
    let issues_before = world.issues();
    let commits_before = world.commits();
    let shared = world.shared.clone();
    let proposal = std::thread::spawn(move || {
        scoped(Asserted::Operator, || {
            shared.rpc_plan_propose(
                &json!({
                    "project": PROJECT,
                    "workflow": WORKFLOW,
                    "inputs": bound_inputs(),
                }),
                std::process::id(),
            )
        })
    });
    // The proposer is queued on the tracker lock; let it arrive there,
    // then let the grant expire before the lock frees.
    std::thread::sleep(Duration::from_secs(7));
    drop(hold);
    let result = proposal
        .join()
        .expect("the propose thread answered, never deadlocked");
    assert!(
        result.is_err(),
        "a grant that expired on the lock queue must refuse, never admit: {result:?}"
    );
    assert_eq!(
        world.issues(),
        issues_before,
        "a refused admission allocated no issue ids"
    );
    assert_eq!(
        world.commits(),
        commits_before,
        "a refused admission left no tracker commit"
    );
}

#[test]
fn cad225_auto_merge_request_is_not_queue_admission() {
    // `autoMergeRequest` is a request, never authority. Three shapes the
    // real `try_unattended_enqueue` -> `unattended_queue_entry` contract
    // must keep apart:
    //
    // 1. auto-merge enabled on a head that no durable intent owns:
    //    refused, and the answer flags the unbound request for disabling.
    //    The observe arrives with the caller's stale `auto_merge:false` —
    //    the fresh `pr view` is what must arm the refusal, never the
    //    incoming flag.
    let world = World::open();
    let admitted = world.grant_and_admit(None);
    world.set_case("in");
    world.set_head(HEAD);
    world.auto_merge_only();
    world.save_pass(&admitted.ticket, "reviewer");
    let refused = world
        .observe(Asserted::Operator, &admitted.ticket, HEAD)
        .expect("an unbound auto-merge refusal still answers the observe");
    assert!(
        refused["unattended_refusal"].as_str().is_some(),
        "an unbound auto-merge request must refuse: {refused}"
    );
    assert!(
        refused["disable_auto"].as_bool().unwrap_or(false),
        "an unbound auto-merge request must be flagged for disabling: {refused}"
    );
    world.assert_not_enqueued(&admitted.ticket, &Ok(refused));
    assert_eq!(world.gh_calls("pr merge"), 0, "an unbound request mutated");

    // 2. the intent exists and auto-merge is live, but the queue entry is
    //    still null after the mutation: the authorized pending state —
    //    Passed, no hold, no receipt, no replay. Removing the auto flag
    //    without an entry is the unknown outcome instead: held, intent
    //    kept, still never replayed.
    let world = World::open();
    let admitted = world.grant_and_admit(None);
    world.set_case("in");
    world.set_head(HEAD);
    world.save_pass(&admitted.ticket, "reviewer");
    world
        .observe(Asserted::Operator, &admitted.ticket, HEAD)
        .expect("the authorized enqueue request answers");
    assert_eq!(world.gh_calls("pr merge"), 1, "one enqueue mutation ran");
    let rec = delivery::load(world.root.path())
        .unwrap()
        .get(&admitted.ticket)
        .cloned()
        .expect("delivery record");
    assert_eq!(rec.state, State::Passed, "pending admission stays Passed");
    assert!(!rec.unattended_hold);
    assert!(
        rec.unattended_pending,
        "the bound pending request must carry the pending marker"
    );
    assert!(
        !rec.merge_ready(),
        "a pending admission never presents a merge card"
    );

    // The post-mutation unknown: entry still null and auto-merge now gone
    // — the enqueue's remote outcome is unproven, so the durable intent
    // holds for the operator instead of replaying or admitting.
    std::fs::remove_file(world.root.path().join("gh-auto")).unwrap();
    let held = world
        .observe(Asserted::Operator, &admitted.ticket, HEAD)
        .expect("the unknown post-mutation observe answers");
    assert!(
        held["unattended_refusal"].as_str().is_some(),
        "an unproven post-mutation outcome must hold: {held}"
    );
    let rec = delivery::load(world.root.path())
        .unwrap()
        .get(&admitted.ticket)
        .cloned()
        .expect("delivery record");
    assert!(rec.unattended_hold, "the unknown outcome is operator-held");
    assert!(
        !rec.unattended_pending,
        "an unresolved outcome is a hold, never a pending marker"
    );
    assert_ne!(rec.state, State::Enqueued);
    assert_eq!(
        world.gh_calls("pr merge"),
        1,
        "the held unknown replayed the mutation"
    );
    assert!(
        world
            .shared
            .store
            .unattended_merge_intent(&admitted.ticket, HEAD)
            .unwrap()
            .is_some(),
        "the held unknown dropped the durable intent"
    );
    assert!(
        world
            .shared
            .store
            .unattended_merge_enqueued(&admitted.ticket, HEAD)
            .unwrap()
            .is_none(),
        "the held unknown wrote an enqueue receipt"
    );

    // 3. the queue then admits the head: reconcile once, Enqueued, the
    //    same single mutation.
    world.queue_admitted();
    let admitted_now = world
        .observe(Asserted::Operator, &admitted.ticket, HEAD)
        .expect("the reconciling observe answers");
    assert!(
        admitted_now["unattended_refusal"].is_null(),
        "the observed entry must admit: {admitted_now}"
    );
    let rec = delivery::load(world.root.path())
        .unwrap()
        .get(&admitted.ticket)
        .cloned()
        .expect("delivery record");
    assert_eq!(rec.state, State::Enqueued);
    assert!(
        !rec.unattended_pending,
        "admission clears the pending marker"
    );
    assert_eq!(
        world.gh_calls("pr merge"),
        1,
        "admission reconciled the intent — never a second mutation"
    );

    // 4. an auto-merge flag that survives on a moved or revoked head is
    //    still refused and still flagged for disabling — admission is the
    //    only state the flag is safe on.
    let world = World::open();
    let admitted = world.grant_and_admit(None);
    world.set_case("in");
    world.set_head(HEAD);
    world.save_pass(&admitted.ticket, "reviewer");
    world
        .observe(Asserted::Operator, &admitted.ticket, HEAD)
        .expect("the authorized enqueue request answers");
    world.queue_admitted();
    world
        .observe_auto(Asserted::Operator, &admitted.ticket, HEAD, "OPEN")
        .expect("the reconciling observe answers");
    // The head actually moves while the remote auto-merge request is
    // still armed: the observe reports the real MOVED head, and the
    // ticket outcome — never the field's presence — is the contract:
    // the standing Enqueued admission is invalidated, the orphaned
    // request is flagged for disabling, and the moved head never gains
    // enqueue authority or a second mutation.
    world.set_head(MOVED);
    world.set_case("outside");
    let moved = world
        .observe_auto(Asserted::Operator, &admitted.ticket, MOVED, "OPEN")
        .expect("a moved-head observe still answers");
    let rec = delivery::load(world.root.path())
        .unwrap()
        .get(&admitted.ticket)
        .cloned()
        .expect("the delivery record stands");
    assert_ne!(
        rec.state,
        State::Enqueued,
        "the standing admission must not survive a moved head: {:?}",
        rec.state
    );
    assert!(
        rec.unattended_hold || moved["unattended_refusal"].as_str().is_some(),
        "a moved head under live auto-merge must hold or refuse: {moved}"
    );
    assert!(
        moved["disable_auto"].as_bool().unwrap_or(false) || rec.disable_auto,
        "the orphaned auto-merge request must stay flagged for disabling: {moved}"
    );
    assert!(
        world
            .shared
            .unattended_authorization_for_issue(&admitted.ticket)
            .is_ok(),
        "the grant itself is unchanged — only the head moved"
    );
    assert_eq!(
        world.gh_calls("pr merge"),
        1,
        "the moved-head observe ran no second mutation"
    );
}

#[test]
fn cad225_pending_done_retry_never_outlives_its_grant() {
    // The done write retries `TicketDone::Pending` from `settle_ticket_done`
    // — the router path — which must re-derive the merge-done refusal
    // under live authority, not replay the merge over a dead grant.
    // Revoking between the failed first write and the retry must leave
    // the ticket refused for the operator, never marked done.
    let world = World::open();
    let admitted = world.grant_and_admit(None);
    world.set_case("in");
    world.set_head(HEAD);
    world.set_runs("green");
    world.save_pass(&admitted.ticket, "reviewer");
    let first = world
        .observe(Asserted::Operator, &admitted.ticket, HEAD)
        .expect("the green enqueue request answers");
    assert!(
        first["unattended_refusal"].is_null(),
        "the in-scope enqueue request must run: {first}"
    );
    // The queue service admits the head: reconcile the durable intent into
    // the Enqueued record before the merge settles.
    world.queue_admitted();
    let landed = world
        .observe_auto(Asserted::Operator, &admitted.ticket, HEAD, "OPEN")
        .expect("the reconciling observe answers");
    assert!(
        landed["unattended_refusal"].is_null(),
        "the observed queue entry must land: {landed}"
    );
    assert!(
        world
            .shared
            .store
            .unattended_merge_intent(&admitted.ticket, HEAD)
            .unwrap()
            .is_some(),
        "the enqueue recorded its durable intent"
    );
    use std::os::unix::fs::PermissionsExt;
    // Fail only the tracker write: the issue directory is read-only,
    // `load_front` still reads the status (`from` is captured), and
    // `save_front` fails — the merge's done write goes `Pending`.
    let issue_dir = world.pm().join(PROJECT).join(&admitted.ticket);
    let original_mode = std::fs::metadata(&issue_dir).unwrap().permissions();
    std::fs::set_permissions(&issue_dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    let merged = world.observe_state(Asserted::Operator, &admitted.ticket, HEAD, "MERGED");
    std::fs::set_permissions(&issue_dir, original_mode).unwrap();
    merged.expect("the merge observation answers; the done write is what failed");
    let rec = delivery::load(world.root.path())
        .unwrap()
        .get(&admitted.ticket)
        .cloned()
        .expect("the merged ticket has a delivery record");
    assert!(
        matches!(rec.ticket_done, Some(delivery::TicketDone::Pending { .. })),
        "a failed done write must be pending for the router, not refused or marked: {:?}",
        rec.ticket_done
    );
    assert_ne!(
        world.issue(&admitted.ticket).front.status,
        "done",
        "a failed write marked the ticket done"
    );
    // The authority the merge ran under is gone before the retry.
    world
        .approve(
            Asserted::Operator,
            json!({"project": PROJECT, "name": WORKFLOW}),
        )
        .expect("reapprove without unattended revokes the grant");
    world.assert_invalid_run(&admitted.ticket);
    let _ = world
        .shared
        .settle_ticket_done()
        .expect("the router pass answers");
    let rec = delivery::load(world.root.path())
        .unwrap()
        .get(&admitted.ticket)
        .cloned()
        .expect("the merged ticket has a delivery record");
    assert!(
        !matches!(rec.ticket_done, Some(delivery::TicketDone::Marked { .. })),
        "the retry marked {0} done under a revoked grant: {1:?}",
        admitted.ticket,
        rec.ticket_done
    );
    assert!(
        !matches!(rec.ticket_done, Some(delivery::TicketDone::Pending { .. })),
        "the retry must not keep a dead grant's write pending forever: {:?}",
        rec.ticket_done
    );
    assert!(
        !matches!(rec.ticket_done, Some(delivery::TicketDone::Kept { .. })),
        "the write did not happen — the retry must not claim it kept a status: {:?}",
        rec.ticket_done
    );
    assert_ne!(
        world.issue(&admitted.ticket).front.status,
        "done",
        "a retry over dead authority marked the ticket done"
    );
}

#[test]
fn cad225_renewal_during_reconcile_keeps_intent_expiry_and_never_reenqueues() {
    // The reconcile branch: a durable intent exists, the record is still
    // `passed` (the enqueue request answered but the queue has not
    // admitted the head — the known-pending state), and the remote queue
    // then proves the head is enqueued. The reconcile admits the recorded
    // outcome without replaying `gh pr merge`. A renewal of the same
    // grant racing that reconcile extends the LIVE expiry — the recorded
    // intent's `grant_expires_at` is the bound value; the run must
    // reconcile from its own intent, never re-enqueue and never turn on
    // `disable_auto`.
    let world = World::open();
    let admitted = world.grant_and_admit(None);
    world.set_case("in");
    world.set_head(HEAD);
    world.set_runs("green");
    world.save_pass(&admitted.ticket, "reviewer");
    world
        .observe(Asserted::Operator, &admitted.ticket, HEAD)
        .expect("the authorized enqueue request answers");
    assert_eq!(
        world.gh_calls("pr merge"),
        1,
        "exactly one enqueue mutation ran"
    );
    let merges_before = world.snap().merges;
    // The reconcile precondition: intent recorded, record still `passed`
    // (the pending admission), remote queue now enabled for this head.
    world.queue_admitted();
    // The operator renews the same grant — same immutable bounds, only
    // `issued_at`/`expires_at` move — while the reconcile waits.
    world
        .approve(
            Asserted::Operator,
            json!({"project": PROJECT, "name": WORKFLOW, "renew": admitted.grant_id}),
        )
        .expect("an operator renewal of identical bounds succeeds");
    let renewed = world
        .shared
        .unattended_authorization_for_issue(&admitted.ticket)
        .unwrap()
        .expect("the renewed grant is live");
    assert_eq!(renewed.grant_id, admitted.grant_id, "renewal keeps the id");
    let reconciled = world.observe_auto(Asserted::Operator, &admitted.ticket, HEAD, "OPEN");
    let out =
        reconciled.expect("a same-grant renewal must reconcile the recorded intent, not hold it");
    assert!(
        out["unattended_refusal"].is_null(),
        "a renewal over identical bounds must not hold the reconcile: {out}"
    );
    assert_eq!(
        world.snap().merges,
        merges_before,
        "the reconcile replayed gh pr merge"
    );
    assert!(
        !out["disable_auto"].as_bool().unwrap_or(true),
        "the reconcile flagged the bound queue state for disable: {out}"
    );
    let rec = delivery::load(world.root.path())
        .unwrap()
        .get(&admitted.ticket)
        .cloned()
        .expect("the reconciled ticket has a delivery record");
    assert_eq!(rec.state, State::Enqueued);
    assert!(
        !rec.disable_auto,
        "the reconcile must not disable the queue it bound"
    );
    assert!(!rec.unattended_hold);
    assert_eq!(
        world.gh_calls("pr merge"),
        1,
        "still exactly one enqueue mutation"
    );
}

#[test]
fn cad225_retained_files_after_failed_commit_are_never_acknowledged() {
    use std::os::unix::fs::PermissionsExt;
    // A commit that fails inside `create_granted_plan` — a failing
    // pre-commit hook stands in for every structural validation/git
    // refusal — leaves whatever issue files were already written, but
    // byte-identical files are not commit proof. The next propose of
    // the identical render must re-run its admission write and refuse
    // while the blocker stands — never `committed`, never a second
    // plan beside the bound ids — and the reservations the intent
    // recorded are never reallocated even after the mark is held.
    let world = World::open();
    world
        .approve(
            Asserted::Operator,
            world.unattended(&[PATH], bound_inputs(), None, None),
        )
        .expect("operator unattended grant");
    let hooks = world.pm().join(".git").join("hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    let hook = hooks.join("pre-commit");
    std::fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();

    let commits = world.commits();
    let issues = world.issues();
    let first = world.propose_workflow(Asserted::Operator, bound_inputs(), None);
    assert!(
        first.is_err(),
        "the commit refusal must surface, never report an admission: {first:?}"
    );
    assert_eq!(world.commits(), commits, "the failed admission committed");
    // Whatever files the failed transaction left, no issue was
    // committed; the durable intent still binds the ids it named.
    let intents: Vec<Value> = world
        .shared
        .store
        .unattended_run_payloads()
        .unwrap()
        .into_iter()
        .filter(|p| p["status"].as_str() == Some("intent"))
        .collect();
    assert_eq!(intents.len(), 1, "one admission intent was recorded");
    let bound_epic = intents[0]["epic"]
        .as_str()
        .expect("the intent binds an epic")
        .to_string();
    let bound_tickets: Vec<String> = intents[0]["tickets"]
        .as_array()
        .expect("the intent binds tickets")
        .iter()
        .filter_map(|t| t.as_str().map(str::to_string))
        .collect();
    assert_eq!(bound_tickets.len(), 1, "the render binds one ticket");
    let reserved = world.shared.store.unattended_reserved_ids().unwrap();
    for id in std::iter::once(&bound_epic).chain(bound_tickets.iter()) {
        assert!(
            reserved.contains(id),
            "the torn admission's {id} stays reserved"
        );
    }
    // The failed transaction may have retained the issue files it wrote
    // — `issues_mid` is the honest mid-state the second propose sees.
    let retained: BTreeSet<String> = std::iter::once(bound_epic.clone())
        .chain(bound_tickets.iter().cloned())
        .collect();
    let issues_mid = world.issues();
    assert!(
        issues_mid
            .iter()
            .all(|id| issues.contains(id) || retained.contains(id)),
        "the failed admission wrote ids outside its intent: {issues_mid:?}"
    );

    // Retained byte-identical files plus a still-broken hook: identical
    // bytes are not commit proof — the second propose must refuse, no
    // further commit, no parallel plan, no approved admission.
    let second = world.propose_workflow(Asserted::Operator, bound_inputs(), None);
    if let Ok(body) = &second {
        assert_ne!(
            body["state"].as_str(),
            Some("approved"),
            "retained files acknowledged as a committed admission: {body}"
        );
        assert_ne!(
            body["committed"].as_bool(),
            Some(true),
            "a commit that never ran must never be reported committed: {body}"
        );
    }
    assert_eq!(world.commits(), commits, "the refused retry wrote a commit");
    assert_eq!(
        world.issues(),
        issues_mid,
        "the refused retry allocated a second plan beside the bound ids"
    );

    // Repair the blocker. The held/torn mark stays operator-gated: a
    // third propose may not silently re-admit beside a mark that needs
    // reconciliation, and the reserved ids are still protected — nothing
    // new is allocated and nothing commits outside the bound render.
    std::fs::write(&hook, "#!/bin/sh\nexit 0\n").unwrap();
    let third = world.propose_workflow(Asserted::Operator, bound_inputs(), None);
    if let Ok(body) = &third {
        assert_eq!(
            body["epic"].as_str(),
            Some(bound_epic.as_str()),
            "a later admit must land the intent's ids, never new ones: {body}"
        );
    }
    let now = world.issues();
    assert!(
        now.iter()
            .all(|id| id == &bound_epic || bound_tickets.contains(id) || issues.contains(id)),
        "recovery allocated ids outside the bound intent: {now:?}"
    );
    let reserved = world.shared.store.unattended_reserved_ids().unwrap();
    for id in std::iter::once(&bound_epic).chain(bound_tickets.iter()) {
        assert!(
            reserved.contains(id),
            "the reservations must survive every retry"
        );
    }
}

#[test]
fn cad225_dead_grant_authorizes_no_checkup_fix() {
    // The idle-lane checkup must read the live grant before queueing a
    // CI-fix follow-up: a revoked grant makes
    // `unattended_authorization_for_issue` an `Err`, and that refusal
    // must stop the fix turn — no `checkup-fix-*` row, no message to
    // the lane, no task mutation. The guard under test is the grant
    // read inside `fix_turn`'s path, exercised through the real
    // `checkup_idle_lane` on a genuinely idle registered lane.
    let world = World::open();
    let admitted = world.grant_and_admit(None);
    world.save_red(&admitted.ticket);
    // The authority dies before the checkup visits the lane.
    world
        .approve(
            Asserted::Operator,
            json!({"project": PROJECT, "name": WORKFLOW}),
        )
        .expect("reapprove without unattended revokes the grant");
    world.assert_invalid_run(&admitted.ticket);
    let report = world.lane_done_ref(&admitted.ticket, 2_000, HEAD);
    let _ = world
        .shared
        .checkup_idle_lane(&world.worker_agent(), &[report]);
    let mid = format!(
        "checkup-fix-{}-{}",
        admitted.ticket.to_lowercase(),
        &HEAD[..12]
    );
    assert!(
        world.shared.store.message(&mid).unwrap().is_none(),
        "a revoked grant queued a CI-fix turn: {mid} exists"
    );
    assert!(
        world
            .shared
            .store
            .messages("worker")
            .unwrap_or_default()
            .iter()
            .all(|m| m.id != mid && m.task_id.is_none()),
        "the dead grant's fix reached the worker lane"
    );
    assert!(
        world
            .shared
            .store
            .agents()
            .unwrap()
            .iter()
            .any(|a| a.alias == "worker" && a.enabled),
        "the lane itself stays registered and idle for the operator"
    );
}

#[test]
fn cad225_native_ci_fix_turn_carries_owned_dispatch_correlation() {
    // The live-grant path: `fix_turn`'s follow-up is a NATIVE turn —
    // daemon-written `source='review'` with the owned `issue` +
    // original `worktree` tags no caller may set — so
    // `worker_issue_turn` correlates it as the lane's latest scoped
    // turn. The fix turn's done report (a NEW head the initial
    // dispatch never produced) must therefore reach independent review
    // through the real `route_delivery`/`on_done` gate exactly once —
    // never `unattended_completion_revision` from comparing against
    // the stale initial-dispatch sha.
    let world = World::open();
    let admitted = world.grant_and_admit(None);
    // The original dispatch owned this lane and completed with H1 —
    // the row `send_issue_followup` must derive the native worktree
    // tag from, and the row a sha-comparing barrier would falsely pin.
    world.dispatch_turn(&admitted.ticket, 1, true, Some(Some(HEAD)));
    world.save_red(&admitted.ticket);
    let report = world.lane_done_ref(&admitted.ticket, 2_000, HEAD);
    let visit = world
        .shared
        .checkup_idle_lane(&world.worker_agent(), &[report]);
    assert!(visit.is_ok(), "the checkup visit answers");
    let mid = format!(
        "checkup-fix-{}-{}",
        admitted.ticket.to_lowercase(),
        &HEAD[..12]
    );
    let fix = world
        .shared
        .store
        .message(&mid)
        .expect("turn reads")
        .unwrap_or_else(|| panic!("the live grant's CI-fix turn must queue as {mid}"));
    assert_eq!(fix.alias, "worker");
    assert_eq!(fix.source, "review", "the fix turn is a review follow-up");
    assert_eq!(
        fix.issue.as_deref(),
        Some(admitted.ticket.as_str()),
        "the native fix turn must carry the owned issue tag the dispatch turn carries"
    );
    assert!(
        fix.worktree.as_deref().is_some_and(|w| !w.is_empty()),
        "the native fix turn must carry the original worktree tag — it is what makes it a scoped worker turn"
    );
    assert!(
        fix.task_id.is_none(),
        "a bound-issue fix is no caller-authored task delivery"
    );
    // The lane's fix completes with the repaired head H2 — the initial
    // dispatch never carried it, so only a latest-scoped-turn barrier
    // can accept this report.
    world.complete_turn(&mid, MOVED);
    world.done_report(&admitted.ticket, 3_000, MOVED);
    world
        .shared
        .route_delivery()
        .expect("the review router answers");
    let rec = delivery::load(world.root.path())
        .unwrap()
        .get(&admitted.ticket)
        .cloned()
        .expect("the delivery record stands");
    assert!(
        !rec.unattended_hold,
        "a completed native fix turn must not hold the loop: {:?}",
        rec.note
    );
    assert!(
        rec.unattended_turn.is_none(),
        "no unknown-turn correlation may stand on an accepted fix"
    );
    let routed = world.routed(&admitted.ticket);
    assert_eq!(
        routed.len(),
        1,
        "the fixed head reaches independent review exactly once: {routed:?}"
    );
    assert_eq!(
        rec.state,
        State::Reviewing,
        "the fix turn's done report lands the head under review"
    );
    assert!(
        rec.reviewer.as_deref().is_some_and(|r| r != "worker"),
        "the loop's own independence rule still applies to the fix round"
    );
}

#[test]
fn cad225_dead_revise_turn_holds_until_native_reconcile_resumes_once() {
    // A REVISE verdict hands the ticket back to the worker through the
    // daemon's own `send_issue_followup` (`revise-<hash>` — a native
    // `source='review'` row carrying the lane's `issue`/`worktree` tags).
    // That row IS the lane's scoped turn: `worker_issue_turn` names it.
    // If the provider reports it `failed` — or the endpoint dies and the
    // row is fenced `unknown` — the worker's next done report must NOT
    // replay it, must NOT send the worker anything, and must NOT flip the
    // record back to `Working`. The record holds `Escalated` on the dead
    // row's id for the operator. Both terminal shapes are exercised.
    for dead in ["failed", "unknown"] {
        let world = World::open();
        let admitted = world.grant_and_admit(None);
        let ticket = admitted.ticket;
        // The granted dispatch completes at HEAD and the done report
        // reaches independent review exactly once. The canonical
        // `issue start` opens the delivery record via `delivery_start`
        // — seed that same row so the router has a lane to route.
        world.dispatch_turn(&ticket, 1, true, Some(Some(HEAD)));
        world
            .shared
            .delivery_start(&ticket, PROJECT, "worker")
            .expect("the canonical issue start opens the delivery record");
        world.done_report(&ticket, now_secs() as i64, HEAD);
        world
            .shared
            .route_delivery()
            .expect("the first router pass answers");
        assert_eq!(
            world.routed(&ticket).len(),
            1,
            "{dead}: exactly one review kickoff after the completed turn"
        );

        // The assigned reviewer's REVISE verdict is the real
        // `report_verdict` RPC: it queues the daemon's own `revise-*`
        // follow-up (the lane's next owned turn) and leaves the record
        // Working, waiting on that turn.
        let shared = world.shared.clone();
        let verdict_body = format!(
            "---\nkind: verdict\ntask: {ticket}\nverdict: revise\nsha: {HEAD}\n---\n\n\
             Findings for {ticket} at {HEAD}.\n"
        );
        let verdict = scoped(Asserted::Agent("reviewer".into()), || {
            shared.rpc_report_verdict(
                &json!({"issue": ticket, "text": verdict_body}),
                std::process::id(),
            )
        })
        .expect("the assigned reviewer's REVISE verdict records");
        let report = verdict["path"]
            .as_str()
            .expect("the verdict filed a report");
        let revise_mid = format!("revise-{}", &sha256_hex(report.as_bytes())[..16]);
        let revise_row = world
            .shared
            .store
            .message(&revise_mid)
            .expect("the revise turn row reads")
            .expect("the REVISE verdict must queue the daemon's own revise turn");
        assert_eq!(
            revise_row.source, "review",
            "{dead}: revise must ride the native follow-up path, not raw mail"
        );

        // The turn dies: the provider answers `failed`, or the endpoint
        // drops and `orphan_running` fences the in-flight row `unknown`
        // (the real crash path — never retried).
        world
            .shared
            .store
            .mark_running(&revise_mid, "cad225")
            .expect("the revise turn acknowledges");
        if dead == "failed" {
            let row = world.shared.store.message(&revise_mid).unwrap().unwrap();
            world
                .shared
                .store
                .finish(
                    &row,
                    "failed",
                    &json!({"error": "provider declined"}),
                    Some("provider declined"),
                )
                .expect("the failed turn records its terminal state");
        } else {
            world
                .shared
                .store
                .orphan_running("worker", "endpoint died")
                .expect("the dead endpoint fences its in-flight rows unknown");
        }

        // The worker files a fresh done report on the same head. The
        // turn gate refuses: the lane's latest scoped turn is dead, so
        // the record holds `Escalated` on that row — no Working
        // oscillation, no worker ping, no review.
        world.done_report(&ticket, now_secs() as i64 + 1, HEAD);
        world
            .shared
            .route_delivery()
            .expect("a router pass on a dead revise turn answers");
        let rec = delivery::load(world.root.path())
            .unwrap()
            .get(&ticket)
            .cloned()
            .expect("the record persists");
        assert_eq!(
            rec.state,
            State::Escalated,
            "{dead}: a done report riding a {dead} revise turn must hold, not review"
        );
        assert!(
            rec.unattended_hold,
            "{dead}: the hold is the operator-visible flag"
        );
        assert_eq!(
            rec.unattended_turn.as_deref(),
            Some(revise_mid.as_str()),
            "{dead}: the hold must correlate the dead turn, not a bare flag"
        );
        assert_eq!(
            world.routed(&ticket).len(),
            1,
            "{dead}: a dead turn reached independent review again"
        );

        // Repeated router passes while the turn stays dead: nothing
        // requeues, nothing is sent, the same hold stands.
        let worker_queue = world
            .shared
            .store
            .messages("worker")
            .unwrap_or_default()
            .len();
        let note = rec.note.clone();
        for _ in 0..3 {
            world
                .shared
                .route_delivery()
                .expect("a replayed pass on the held record answers");
            let again = delivery::load(world.root.path())
                .unwrap()
                .get(&ticket)
                .cloned()
                .expect("the record persists");
            assert_eq!(again.state, State::Escalated, "{dead}: the hold oscillated");
            assert!(again.unattended_hold, "{dead}: the hold dropped");
            assert_eq!(
                again.unattended_turn.as_deref(),
                Some(revise_mid.as_str()),
                "{dead}: a replay re-correlated the hold to another row"
            );
            assert_eq!(again.note, note, "{dead}: a replay rewrote the held note");
        }
        assert_eq!(
            world
                .shared
                .store
                .messages("worker")
                .unwrap_or_default()
                .len(),
            worker_queue,
            "{dead}: replayed passes sent the worker a new message"
        );

        // Native reconciliation only: the operator restores the lane and
        // produces an owned completed outcome at the same head. For
        // `unknown`, `store.reconcile` — the real operator path — closes
        // the SAME row `completed` with the exact sha (the row's atomic
        // unknown→completed UPDATE is the fence). For `failed`, the row
        // is definitive: it stays `failed` forever and the owned turn is
        // a NEW dispatch row; the router's correlate check takes it.
        if dead == "unknown" {
            world
                .shared
                .store
                .reconcile(
                    &revise_mid,
                    "completed",
                    Some("endpoint recovered; provider confirmed the run finished"),
                    "operator",
                    Some(HEAD),
                )
                .expect("the operator reconciles the unknown revise turn at the exact head");
        } else {
            world.dispatch_turn(&ticket, 2, true, Some(Some(HEAD)));
        }
        world
            .shared
            .route_delivery()
            .expect("the reconciled pass answers");

        let rec = delivery::load(world.root.path())
            .unwrap()
            .get(&ticket)
            .cloned()
            .expect("the record persists");
        assert_eq!(
            rec.state,
            State::Reviewing,
            "{dead}: the reconciled completion must resume the held review exactly once"
        );
        assert!(!rec.unattended_hold, "{dead}: the resume cleared the hold");
        assert!(
            rec.unattended_turn.is_none(),
            "{dead}: no dead-turn correlation may stand on the resumed review"
        );
        assert_eq!(
            world.routed(&ticket).len(),
            2,
            "{dead}: the resumed head reached independent review exactly once more"
        );

        // A reconciled `unknown` row IS the durable completed result;
        // a `failed` row is never relabeled.
        let row_after = world.shared.store.message(&revise_mid).unwrap().unwrap();
        assert_eq!(
            row_after.state,
            if dead == "unknown" {
                "completed"
            } else {
                "failed"
            },
            "{dead}: reconciliation must preserve the row's truth"
        );

        // The resume consumed itself: one more pass routes nothing.
        world
            .shared
            .route_delivery()
            .expect("the post-resume pass answers");
        assert_eq!(
            world.routed(&ticket).len(),
            2,
            "{dead}: a second resume routed a third review"
        );
    }
}

#[test]
fn cad225_active_queue_state_blocks_the_bound_ci_fix() {
    // A bound ticket whose queue state is live — an armed
    // `autoMergeRequest`, an admitted `mergeQueueEntry`, or a remote
    // that cannot be read at all — must never receive a NEW CI-fix
    // follow-up: the fix could land on top of an in-flight queue
    // entry, and an unreadable proof is never permission to act.
    // `fix_turn` must hold for the operator instead of sending, and
    // must not cancel the request or re-run the merge itself.
    for armed in ["request", "entry", "down"] {
        let world = World::open();
        let admitted = world.grant_and_admit(None);
        world.set_case("in");
        world.set_head(HEAD);
        world.save_pass(&admitted.ticket, "reviewer");
        world
            .observe(Asserted::Operator, &admitted.ticket, HEAD)
            .expect("the enqueue mutation runs first — the intent is durable");
        assert_eq!(
            world.gh_calls("pr merge"),
            1,
            "{armed}: one enqueue mutation"
        );
        // Arm the live remote shape the fix guard must re-prove, then
        // turn the record's CI red — the checkup trigger.
        match armed {
            "entry" => world.queue_admitted(),
            "down" => world.set_down(),
            _ => {}
        }
        world.save_red(&admitted.ticket);
        let report = world.lane_done_ref(&admitted.ticket, 2_000, HEAD);
        let _ = world
            .shared
            .checkup_idle_lane(&world.worker_agent(), &[report]);
        let mid = format!(
            "checkup-fix-{}-{}",
            admitted.ticket.to_lowercase(),
            &HEAD[..12]
        );
        assert!(
            world.shared.store.message(&mid).unwrap().is_none(),
            "{armed}: a live queue state queued a CI-fix turn"
        );
        assert!(
            world
                .shared
                .store
                .messages("worker")
                .unwrap_or_default()
                .iter()
                .all(|m| m.id != mid && m.task_id.is_none()),
            "{armed}: the live queue state's fix reached the worker lane"
        );
        let rec = delivery::load(world.root.path())
            .unwrap()
            .get(&admitted.ticket)
            .cloned()
            .expect("the delivery record stands");
        assert!(
            rec.unattended_hold,
            "{armed}: an unreadable or active queue state must hold for the operator, never silently pass"
        );
        assert_eq!(
            world.gh_calls("pr merge"),
            1,
            "{armed}: the blocked fix ran no second merge mutation"
        );
        assert!(
            !world.snap().merges.contains("disable-auto-merge"),
            "{armed}: the daemon never cancels the armed request itself"
        );
    }

    // A claimed armed state with NO durable intent is forged provenance:
    // the metadata alone cannot excuse a fix — hold, zero send, zero
    // mutation. The request is armed by hand, no enqueue ever ran.
    let world = World::open();
    let admitted = world.grant_and_admit(None);
    world.set_case("in");
    world.set_head(HEAD);
    world.auto_merge_only();
    world.save_red(&admitted.ticket);
    let report = world.lane_done_ref(&admitted.ticket, 2_000, HEAD);
    let _ = world
        .shared
        .checkup_idle_lane(&world.worker_agent(), &[report]);
    let mid = format!(
        "checkup-fix-{}-{}",
        admitted.ticket.to_lowercase(),
        &HEAD[..12]
    );
    assert!(
        world.shared.store.message(&mid).unwrap().is_none(),
        "claimed queue metadata without a durable intent queued a fix"
    );
    let rec = delivery::load(world.root.path())
        .unwrap()
        .get(&admitted.ticket)
        .cloned()
        .expect("the delivery record stands");
    assert!(
        rec.unattended_hold,
        "metadata without intent must hold — it is never proof"
    );
    assert_eq!(world.gh_calls("pr merge"), 0, "the held fix mutated");

    // The request is removed and re-proven: the bound fix lane opens —
    // the turn queues as the daemon's own native review follow-up, the
    // historical intent stays durable, and the live grant is re-checked
    // only after the remote reads.
    let world = World::open();
    let admitted = world.grant_and_admit(None);
    world.set_case("in");
    world.set_head(HEAD);
    world.save_pass(&admitted.ticket, "reviewer");
    world
        .observe(Asserted::Operator, &admitted.ticket, HEAD)
        .expect("the enqueue mutation runs first — the intent is durable");
    // The operator removes the stale request out-of-band; the next fix
    // read proves OPEN + same head + request null + entry null.
    let _ = std::fs::remove_file(world.root.path().join("gh-auto"));
    world.save_red(&admitted.ticket);
    let report = world.lane_done_ref(&admitted.ticket, 2_000, HEAD);
    world
        .shared
        .checkup_idle_lane(&world.worker_agent(), &[report])
        .expect("the checkup visit answers");
    let mid = format!(
        "checkup-fix-{}-{}",
        admitted.ticket.to_lowercase(),
        &HEAD[..12]
    );
    let fix = world
        .shared
        .store
        .message(&mid)
        .expect("turn reads")
        .unwrap_or_else(|| panic!("a proven-clean queue state must permit the bound fix"));
    assert_eq!(
        fix.source, "review",
        "the fix rides the native follow-up path"
    );
    assert_eq!(fix.issue.as_deref(), Some(admitted.ticket.as_str()));
    assert!(
        fix.task_id.is_none(),
        "a bound-issue fix is no caller-authored task"
    );
    assert!(
        world
            .shared
            .store
            .unattended_merge_intent(&admitted.ticket, HEAD)
            .unwrap()
            .is_some(),
        "the permitted fix must keep the historical intent durable"
    );
    assert_eq!(
        world.gh_calls("pr merge"),
        1,
        "the permitted fix ran no new merge mutation"
    );
}

#[test]
fn cad225_enqueue_requires_the_latest_owned_turn_completed_at_the_reviewed_head() {
    // The enqueue barrier is a latest-native-owned-turn check: only a
    // `source`-owned row at `completed` whose result sha IS the
    // reviewed head proves the lane's work is the PASS's work. The
    // outcome splits two ways, and the ticket outcome — never a stray
    // merge, never a durable intent — is what each arm asserts:
    //
    // - a queued-or-running turn is a NORMAL WAIT: the observe
    //   answers, the premature PASS clears, the record re-enters
    //   Working — no `unattended_refusal`, no operator hold, the way
    //   a pending-CI wait already behaves;
    // - a `failed`/`unknown`, sha-mismatched, or missing turn is
    //   refused proof: the observe refuses or errors, the record
    //   holds for the operator, and nothing mutates.
    for dead in ["pending", "failed", "unknown", "mismatch", "absent"] {
        let world = World::open();
        let admitted = world.grant_and_admit(None);
        world.set_case("in");
        world.set_head(HEAD);
        match dead {
            // The provider never reported — the row is still queued.
            "pending" => world.dispatch_turn(&admitted.ticket, 1, false, None),
            // A dead row is terminal evidence, never relabeled.
            "failed" => world.fail_turn(&admitted.ticket, 1),
            // The endpoint died mid-turn — `orphan_running` fenced it `unknown`.
            "unknown" => world.orphan_turn(&admitted.ticket, 1),
            // Completed — but with a sha the PASS never reviewed.
            "mismatch" => world.dispatch_turn(&admitted.ticket, 1, true, Some(Some(MOVED))),
            _ => {}
        }
        // The seeded PASS is identical to the legitimate one minus its
        // completed owned turn at HEAD: `save_pass_risk` writes only the
        // verdict state, so the turn seeded above stays the lane's
        // latest — the arm owns its final state.
        world.save_pass_risk(&admitted.ticket, "reviewer", Some("auto"));
        let result = world.observe(Asserted::Operator, &admitted.ticket, HEAD);
        // Every arm: no merge mutation, no durable intent, no enqueued
        // receipt — the ticket can never land ahead of its own turn.
        let intent = world
            .shared
            .store
            .unattended_merge_intent(&admitted.ticket, HEAD)
            .unwrap();
        assert!(
            intent.is_none(),
            "{dead}: an unproven turn wrote a durable intent"
        );
        assert_eq!(
            world.gh_calls("pr merge"),
            0,
            "{dead}: an unproven turn ran the merge mutation"
        );
        let rec = delivery::load(world.root.path())
            .unwrap()
            .get(&admitted.ticket)
            .cloned()
            .expect("the delivery record stands");
        match dead {
            "pending" => {
                let out = result.expect("a wait observes normally");
                assert!(
                    out["unattended_refusal"].is_null(),
                    "pending: a queued turn is a wait, not a refusal: {out}"
                );
                assert_eq!(
                    rec.state,
                    State::Working,
                    "pending: the premature PASS clears and the lane resumes its turn"
                );
                assert!(
                    !rec.unattended_hold,
                    "pending: a wait must not escalate to the operator"
                );
            }
            _ => {
                let refused = match &result {
                    Ok(out) => out["unattended_refusal"].as_str().is_some(),
                    Err(_) => true,
                };
                assert!(
                    refused,
                    "{dead}: dead or missing turn proof must refuse, not wait: {result:?}"
                );
                assert!(
                    rec.unattended_hold,
                    "{dead}: dead or missing turn proof must hold for the operator"
                );
            }
        }
    }
}

#[test]
fn cad225_enqueue_and_ci_fix_never_decide_the_same_queue_state_twice() {
    // The real interleave the queue-state guard exists for: a handler
    // parked INSIDE its fresh `pr view` read (the one-shot rendezvous)
    // while a competing handler runs. Whatever the lock order, the
    // ticket can never end up with BOTH an armed queue request and a
    // new fix follow-up, and the losing side sees the winner's durable
    // state — never the stale snapshot it entered with. `World` stays
    // on the main thread (the env guard is !Send); each thread gets an
    // `Arc<Shared>` clone plus owned payloads and re-asserts its caller.
    // Order 1 — the enqueue handler holds the remote read when the
    // checkup arrives. The record is Passed with a standing PASS but
    // red observed CI, so the fix lane's claim is real; the enqueue's
    // fresh green view wins the lock. After release the queue intent is
    // armed, the record marks the pending admission, and the fix lane
    // sends NOTHING — there is no window where the armed request and a
    // new follow-up coexist.
    let world = World::open();
    let admitted = world.grant_and_admit(None);
    world.set_case("in");
    world.set_head(HEAD);
    world.save_pass(&admitted.ticket, "reviewer");
    world.set_observed_red(&admitted.ticket);
    world.arm_rendezvous();
    let shared = world.shared.clone();
    let ticket = admitted.ticket.clone();
    let enqueue_thread = std::thread::spawn(move || {
        let params = json!({
            "issue": ticket, "head": HEAD, "pr_state": "OPEN",
            "ci_green": true, "additions": 1, "deletions": 0, "files": 1,
        });
        scoped(Asserted::Operator, || {
            shared.rpc_delivery_observe(&params, std::process::id())
        })
    });
    assert!(
        world.wait_for("rv-call", 30),
        "the enqueue handler never reached the parked pr view"
    );
    // The competing fix lane sees the armed state only through the real
    // serialized path — launched while the enqueue handler is provably
    // mid-read.
    let shared = world.shared.clone();
    let report = world.lane_done_ref(&admitted.ticket, 2_000, HEAD);
    let agent = world.worker_agent();
    let fix_thread = std::thread::spawn(move || shared.checkup_idle_lane(&agent, &[report]));
    world.release_rendezvous();
    enqueue_thread
        .join()
        .expect("the enqueue thread answers")
        .expect("the enqueue observes");
    fix_thread.join().expect("the fix thread answers").ok();
    let mid = format!(
        "checkup-fix-{}-{}",
        admitted.ticket.to_lowercase(),
        &HEAD[..12]
    );
    assert!(
        world.shared.store.message(&mid).unwrap().is_none(),
        "an in-flight armed queue admission still queued a fix turn"
    );
    let rec = delivery::load(world.root.path())
        .unwrap()
        .get(&admitted.ticket)
        .cloned()
        .expect("the delivery record stands");
    assert!(
        rec.unattended_pending,
        "the enqueue's pending admission stands after the interleave"
    );
    assert_eq!(
        world.gh_calls("pr merge"),
        1,
        "exactly one enqueue mutation"
    );
    assert!(
        !world.snap().merges.contains("disable-auto-merge"),
        "the interleave never cancelled the armed request"
    );

    // Order 2 — the fix handler holds the remote read while a competing
    // observe arrives. The record is STILL Passed with its standing
    // PASS (only the observation reddened — the fixture never touches
    // state or verdict): the fix turn's own write is the only thing
    // that can move the lane to Working, so a late observe racing it
    // must land against the new truth — Working with a queued owned
    // turn — never against the stale PASS snapshot. No second intent,
    // no merge, no armed-plus-follow-up coexistence.
    let world = World::open();
    let admitted = world.grant_and_admit(None);
    world.set_case("in");
    world.set_head(HEAD);
    world.save_pass(&admitted.ticket, "reviewer");
    world
        .observe(Asserted::Operator, &admitted.ticket, HEAD)
        .expect("the historical enqueue runs first — the intent is durable");
    assert_eq!(
        world.gh_calls("pr merge"),
        1,
        "the original enqueue mutation ran"
    );
    // The stale request was removed out-of-band and CI went red: the
    // fix trigger stands on the SAME Passed record.
    let _ = std::fs::remove_file(world.root.path().join("gh-auto"));
    world.set_observed_red(&admitted.ticket);
    world.arm_rendezvous();
    let shared = world.shared.clone();
    let report = world.lane_done_ref(&admitted.ticket, 2_000, HEAD);
    let agent = world.worker_agent();
    let fix_thread = std::thread::spawn(move || shared.checkup_idle_lane(&agent, &[report]));
    assert!(
        world.wait_for("rv-call", 30),
        "the fix handler never reached the parked pr view"
    );
    let shared = world.shared.clone();
    let ticket = admitted.ticket.clone();
    let observe_thread = std::thread::spawn(move || {
        let params = json!({
            "issue": ticket, "head": HEAD, "pr_state": "OPEN",
            "ci_green": true, "additions": 1, "deletions": 0, "files": 1,
        });
        scoped(Asserted::Operator, || {
            shared.rpc_delivery_observe(&params, std::process::id())
        })
    });
    world.release_rendezvous();
    fix_thread.join().expect("the fix thread answers").ok();
    let _ = observe_thread.join().expect("the observe thread answers");
    let mid = format!(
        "checkup-fix-{}-{}",
        admitted.ticket.to_lowercase(),
        &HEAD[..12]
    );
    let fix = world
        .shared
        .store
        .message(&mid)
        .expect("turn reads")
        .unwrap_or_else(|| panic!("a clean remote state must permit the bound fix"));
    assert_eq!(
        fix.source, "review",
        "the fix follow-up rides the daemon's own native path"
    );
    let rec = delivery::load(world.root.path())
        .unwrap()
        .get(&admitted.ticket)
        .cloned()
        .expect("the delivery record stands");
    assert_eq!(
        rec.state,
        State::Working,
        "the lane resumes its fix turn — the stale PASS can no longer land"
    );
    assert_eq!(
        world.gh_calls("pr merge"),
        1,
        "the late observe ran no second merge over the resumed lane"
    );
    assert!(
        world
            .shared
            .store
            .unattended_merge_intent(&admitted.ticket, HEAD)
            .unwrap()
            .is_some(),
        "the historical intent stays durable through the interleave"
    );
    assert!(
        !world.snap().merges.contains("disable-auto-merge"),
        "the interleave never cancelled a request"
    );
}
