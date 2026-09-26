//! dispatch_jobs_monitor: area tests split from tests/integration.rs (CAD-426).
//! End-to-end tests: real socket daemon in-process, fake provider.
//! These exercise the observable contract — queue order, idempotency,
//! restart fencing, approval brokering, serialization — without model calls.
// A test binary never runs the CAD-308 reaper (only `daemon run` does),
// so its own spawns need not go through `cadence_agent::reaper`.
#![allow(clippy::disallowed_methods)]
mod common;
use common::*;

use cadence_agent::store::NewAgent;
use cadence_agent::store::Store;
use serde_json::json;
use serde_json::Value;
use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use std::sync::Barrier;
use std::thread;
use std::time::Duration;
use std::time::Instant;
use std::time::SystemTime;
use tempfile::TempDir;

/// Fixture-only provider evidence. The automatic coordinator must read this
/// provider-owned column; a caller-supplied `params.quota` is intentionally
/// not sufficient to pass admission.
fn seed_provider_quota(d: &TestDaemon, alias: &str, observed_epoch: i64) {
    let agent = d.rpc("agent_show", json!({"alias": alias})).unwrap()["agent"].clone();
    let thread_id = agent["thread_id"]
        .as_str()
        .unwrap_or_else(|| panic!("fake agent {alias} has no thread identity"));
    let account_id = format!("acct-{alias}");
    let quota = json!({
        "provider": agent["provider"],
        "assignee": alias,
        "account_id": account_id,
        "thread_id": thread_id,
        "state": "available",
        "source": "account/rateLimits/read",
        "observed_at": cadence_agent::issue::time::iso(observed_epoch),
        "updated_at": cadence_agent::issue::time::iso(observed_epoch),
        "data": {
            "accountId": account_id,
            "rateLimits": {"primary": {"usedPercent": 1}}
        }
    });
    let conn = rusqlite::Connection::open(d.state.join("cadence.sqlite3")).unwrap();
    conn.execute(
        "UPDATE agents SET quota=?1 WHERE alias=?2",
        rusqlite::params![quota.to_string(), alias],
    )
    .unwrap();
}

/// CAD-437: `job list` — `states` any-of beats the singular `state`,
/// an explicit state set implies terminal rows, unknown values error.
#[test]
fn job_list_cad437_filters() {
    let d = TestDaemon::start();
    d.register("pm");
    let (spec, sha) = d.spec_file("spec.md", "first job");
    d.job_new_issue("pm", "j1", &spec, &sha, "CAD-26").unwrap();
    let (spec2, sha2) = d.spec_file("spec2.md", "second job");
    d.job_new_issue("pm", "j2", &spec2, &sha2, "CAD-27")
        .unwrap();

    let ids = |v: &Value| -> Vec<String> {
        let mut ids: Vec<String> = v["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|j| j["id"].as_str().unwrap().to_string())
            .collect();
        ids.sort();
        ids
    };
    // Legacy singular and the new plural agree; any-of unions.
    let v = d.rpc("job_list", json!({"state": "open"})).unwrap();
    assert_eq!(ids(&v), ["j1", "j2"]);
    let v = d
        .rpc("job_list", json!({"states": ["open", "done"]}))
        .unwrap();
    assert_eq!(ids(&v), ["j1", "j2"]);
    // An explicit state shows terminal rows the default hides — none
    // here, so the answer is empty rather than an error.
    let v = d.rpc("job_list", json!({"states": ["done"]})).unwrap();
    assert_eq!(ids(&v), Vec::<String>::new());
    let err = d.rpc("job_list", json!({"states": ["zzz"]})).unwrap_err();
    assert!(err.to_string().contains("open"), "{err}");

    // CLI: repeatable flag, comma-joined, sort/limit/fields.
    let bin = env!("CARGO_BIN_EXE_cadence");
    let run = |args: &[&str]| -> (bool, String, String) {
        let out = std::process::Command::new(bin)
            .arg("--state-dir")
            .arg(&d.state)
            .args(args)
            .env_remove("CADENCE_ALIAS")
            .output()
            .unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    };
    let (ok, out, err) = run(&["job", "list", "--state", "open,done", "--json"]);
    assert!(ok, "{err}");
    assert_eq!(ids(&serde_json::from_str(&out).unwrap()), ["j1", "j2"]);
    let (ok, out, err) = run(&["job", "list", "--sort", "-id", "--limit", "1", "--json"]);
    assert!(ok, "{err}");
    let v: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["jobs"][0]["id"], "j2");
    let (ok, out, err) = run(&["job", "list", "--fields", "id,state", "--json"]);
    assert!(ok, "{err}");
    let v: Value = serde_json::from_str(&out).unwrap();
    let keys: Vec<&String> = v["jobs"][0].as_object().unwrap().keys().collect();
    assert_eq!(keys, ["id", "state"], "{v}");
    let (ok, _, err) = run(&["job", "list", "--state", "zzz"]);
    assert!(!ok && err.contains("open"), "{err}");
}

#[test]
fn job_new_creates_default_task_and_validates_issue() {
    let d = TestDaemon::start();
    d.register("pm");
    let (spec, sha) = d.spec_file("spec.md", "do the seeded-bug fix");

    let r = d.rpc(
        "job_new",
        json!({"pm": "pm", "job": "j1", "spec": spec, "spec_sha256": sha,
               "title": "fix it", "issue": "CAD-26", "repo": "/tmp/x",
               "base_ref": "main", "max_revisions": 3}),
    );
    let job = r.unwrap()["job"].clone();
    assert_eq!(job["state"], "open");
    assert_eq!(job["issue"], "CAD-26");
    assert_eq!(job["pm"], "pm");
    assert_eq!(job["spec_sha256"], sha.as_str());
    assert_eq!(job["max_revisions"], 3);

    // The default task <job>-t1 is created draft.
    let show = d.rpc("job_show", json!({"job": "j1"})).unwrap();
    let tasks = show["job"]["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0]["id"], "j1-t1");
    assert_eq!(tasks[0]["state"], "draft");
    assert_eq!(tasks[0]["revision"], 0);

    // job list shows the issue id + task counts.
    let list = d.rpc("job_list", json!({})).unwrap()["jobs"].clone();
    assert_eq!(list[0]["issue"], "CAD-26");
    assert_eq!(list[0]["tasks"]["draft"], 1);

    // Idempotent re-create with identical params.
    let dup = d.job_new_issue("pm", "j1", &spec, &sha, "CAD-26");
    assert_eq!(dup.unwrap()["duplicate"], true);

    // Same id, different content → rejected.
    assert!(d
        .job_new_issue("pm", "j1", "/other.md", &"0".repeat(64), "CAD-26")
        .is_err());

    // Issue grammar is validated — not the filesystem.
    assert!(d
        .job_new_issue("pm", "j2", &spec, &sha, "not-an-issue")
        .is_err());

    // One leaf issue → one open job.
    assert!(d.job_new_issue("pm", "j2", &spec, &sha, "CAD-26").is_err());

    // Unknown PM rejected.
    assert!(d
        .rpc(
            "job_new",
            json!({"pm": "nobody", "job": "j3", "spec": spec,
                   "spec_sha256": sha}),
        )
        .is_err());
}

#[test]
fn job_seeded_bug_loop_end_to_end() {
    let d = TestDaemon::start();
    d.register("pm");
    d.register_member("w1", "pm");
    d.wait_agent("pm", "idle", 10);
    d.wait_agent("w1", "idle", 10);
    let (spec, sha) = d.spec_file("spec.md", "seeded bug: off-by-one");
    d.job_new("pm", "j1", &spec, &sha);

    let wt = d.dir.path().join("wt-t1");
    std::fs::create_dir_all(&wt).unwrap();
    d.rpc(
        "task_new",
        json!({"job": "j1", "task": "j1-fix", "title": "fix the bug",
               "assignee": "w1", "worktree": wt.to_str().unwrap(),
               "branch": "cadence/fix", "base_sha": SHA_B,
               "acceptance": format!("tests pass REPORT_SHA:{SHA_A}")}),
    )
    .unwrap();

    // r1: dispatch → running → completed with the SHA trailer → review.
    let r1 = d.job_dispatch("j1-fix", json!({})).unwrap();
    let kickoff1 = r1["message"].as_str().unwrap().to_string();
    assert_eq!(r1["task"]["state"], "dispatched");
    assert_eq!(r1["task"]["revision"], 1);
    let m1 = d.wait_message("w1", &kickoff1, &["completed"], 15);
    assert_eq!(m1["source"], "job_dispatch");
    assert_eq!(m1["task_id"], "j1-fix");
    assert!(m1["body"].as_str().unwrap().contains("worktree"));
    let t = d.wait_task("j1-fix", "review", 15);
    assert_eq!(t["head_sha"], SHA_A, "{t}");

    // verdict revise (r1 < max 2) → revising; PM got a job_event.
    d.job_verdict("j1-fix", SHA_A, "revise").unwrap();
    assert_eq!(d.task_state("j1-fix"), "revising");

    // r2: a fresh deterministic kickoff id, not the r1 one.
    let r2 = d.job_dispatch("j1-fix", json!({})).unwrap();
    let kickoff2 = r2["message"].as_str().unwrap().to_string();
    assert_ne!(kickoff1, kickoff2);
    assert_eq!(r2["task"]["revision"], 2);
    d.wait_message("w1", &kickoff2, &["completed"], 15);
    d.wait_task("j1-fix", "review", 15);

    // pass → verified → accept → done; PM notification routed.
    d.job_verdict("j1-fix", SHA_A, "pass").unwrap();
    assert_eq!(d.task_state("j1-fix"), "verified");
    d.operator_rpc(
        "task_accept",
        json!({"task": "j1-fix", "merged_sha": SHA_C, "by": "pm"}),
    )
    .unwrap();
    assert_eq!(d.task_state("j1-fix"), "done");
    assert_eq!(d.job_state("j1"), "open"); // j1-t1 default task still draft

    // The PM received routed job_event notifications (verified + done)
    // plus the worker_result for each kickoff — self-describing.
    let pm_msgs = d.rpc("agent_show", json!({"alias": "pm"})).unwrap()["messages"]
        .as_array()
        .unwrap()
        .clone();
    let events: Vec<&Value> = pm_msgs
        .iter()
        .filter(|m| m["source"] == "job_event")
        .collect();
    assert!(events.len() >= 2, "{pm_msgs:?}");
    let worker_results: Vec<&Value> = pm_msgs
        .iter()
        .filter(|m| m["source"] == "worker_result")
        .collect();
    assert_eq!(worker_results.len(), 2, "{pm_msgs:?}");
    assert_eq!(worker_results[0]["task_id"], "j1-fix");

    // job events carry the scoped history.
    let evs = d.rpc("job_events", json!({"job": "j1"})).unwrap()["events"]
        .as_array()
        .unwrap()
        .clone();
    let kinds: Vec<&str> = evs.iter().filter_map(|e| e["kind"].as_str()).collect();
    for k in [
        "job_created",
        "task_created",
        "task_dispatched",
        "task_running",
        "task_reported",
        "verdict_recorded",
        "task_done",
    ] {
        assert!(kinds.contains(&k), "missing {k} in {kinds:?}");
    }
    assert!(evs.iter().all(|e| e["job_id"] == "j1"));
}

#[test]
fn job_inbox_pm_receives_notifications() {
    let d = TestDaemon::start();
    d.register_inbox("pm-in");
    d.register_member("w1", "pm-in");
    d.wait_agent("w1", "idle", 10);
    let (spec, sha) = d.spec_file("spec.md", "inbox pm spec");
    d.job_new("pm-in", "j1", &spec, &sha);

    d.task_new_ac("j1", "j1-t2", "w1", format!("ok REPORT_SHA:{SHA_A}"))
        .unwrap();
    let r = d.job_dispatch("j1-t2", json!({})).unwrap();
    let kickoff = r["message"].as_str().unwrap().to_string();
    d.wait_message("w1", &kickoff, &["completed"], 15);
    d.wait_task("j1-t2", "review", 15);
    d.job_verdict("j1-t2", SHA_A, "pass").unwrap();
    d.operator_rpc("task_accept", json!({"task": "j1-t2"}))
        .unwrap();
    assert_eq!(d.task_state("j1-t2"), "done");

    // Drain the PM inbox: worker_result + job_event copies landed.
    let drained = d.rpc("agent_inbox", json!({"alias": "pm-in"})).unwrap()["messages"]
        .as_array()
        .unwrap()
        .clone();
    let sources: Vec<&str> = drained
        .iter()
        .filter_map(|m| m["source"].as_str())
        .collect();
    assert!(sources.contains(&"worker_result"), "{sources:?}");
    assert!(sources.contains(&"job_event"), "{sources:?}");
    let done_note = drained
        .iter()
        .find(|m| m["source"] == "job_event" && m["body"].as_str().unwrap_or("").contains("done"))
        .expect("no done notification");
    assert_eq!(done_note["task_id"], "j1-t2");
}

#[test]
fn verdict_rejects_every_bad_shape() {
    let d = TestDaemon::start();
    d.register("pm");
    d.register_member("w1", "pm");
    d.wait_agent("w1", "idle", 10);
    let (spec, sha) = d.spec_file("spec.md", "verdict rejections");
    d.job_new("pm", "j1", &spec, &sha);
    d.task_new_ac("j1", "j1-t2", "w1", format!("ok REPORT_SHA:{SHA_A}"))
        .unwrap();

    // Not in review → rejected.
    assert!(d.job_verdict("j1-t2", SHA_A, "pass").is_err());

    let r = d.job_dispatch("j1-t2", json!({})).unwrap();
    let kickoff = r["message"].as_str().unwrap().to_string();
    d.wait_message("w1", &kickoff, &["completed"], 15);
    d.wait_task("j1-t2", "review", 15);

    // Wrong sha → rejected.
    let e = d.job_verdict("j1-t2", SHA_B, "pass").unwrap_err();
    assert!(e.to_string().contains("does not match"), "{e}");
    // Malformed sha → rejected.
    assert!(d.job_verdict("j1-t2", "abc123", "pass").is_err());
    // A claimed reviewer or pane is refused, not read (CAD-372) — even
    // from the operator, and even when it names the operator.
    for (field, claim) in [
        ("reviewer", "rev"),
        ("pane", "rev-pane"),
        ("reviewer", "operator"),
    ] {
        let e = d
            .operator_rpc(
                "task_verdict",
                json!({"task": "j1-t2", "sha": SHA_A, "verdict": "pass", field: claim}),
            )
            .unwrap_err();
        assert!(
            e.to_string()
                .contains(&format!("'{field}' is not accepted")),
            "{field}: {e}"
        );
    }
    // Stale revision → rejected.
    assert!(d
        .operator_rpc(
            "task_verdict",
            json!({"task": "j1-t2", "sha": SHA_A, "verdict": "pass", "revision": 7}),
        )
        .is_err());
    // Bad verdict word → rejected.
    assert!(d.job_verdict("j1-t2", SHA_A, "maybe").is_err());

    // The good verdict lands — reviewer recorded, pane flag false.
    d.job_verdict("j1-t2", SHA_A, "pass").unwrap();
    let t = d.rpc("task_show", json!({"task": "j1-t2"})).unwrap()["task"].clone();
    assert_eq!(t["state"], "verified");
    let v = &t["verdicts"][0];
    assert_eq!(v["reviewer"], "operator");
    assert_eq!(v["sha"], SHA_A);
    assert_eq!(v["revision"], 1);
    assert_eq!(v["pane"], Value::Null);
}

#[test]
fn verdict_rejects_null_sha_until_repaired() {
    let d = TestDaemon::start();
    d.register("pm");
    d.register_member("w1", "pm");
    d.wait_agent("w1", "idle", 10);
    let (spec, sha) = d.spec_file("spec.md", "no-sha report");
    d.job_new("pm", "j1", &spec, &sha);
    // No REPORT_SHA directive — the fake reply carries no SHA line.
    d.task_new_ac("j1", "j1-t2", "w1", "plain echo").unwrap();
    let r = d.job_dispatch("j1-t2", json!({})).unwrap();
    let kickoff = r["message"].as_str().unwrap().to_string();
    d.wait_message("w1", &kickoff, &["completed"], 15);
    let t = d.wait_task("j1-t2", "review", 15);
    assert_eq!(t["head_sha"], Value::Null);
    // `job show` flags the missing SHA.
    let show = d.rpc("job_show", json!({"job": "j1"})).unwrap();
    let tj = show["job"]["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == "j1-t2")
        .unwrap();
    assert!(
        tj["attention"].as_str().unwrap().contains("job task sha"),
        "{tj}"
    );

    // Verdict rejected naming the fix.
    let e = d.job_verdict("j1-t2", SHA_A, "pass").unwrap_err();
    assert!(e.to_string().contains("job task sha"), "{e}");

    // `job task sha` repairs it — recorded as an event, verdict proceeds.
    d.operator_rpc(
        "task_sha",
        json!({"task": "j1-t2", "sha": SHA_A, "by": "pm"}),
    )
    .unwrap();
    // Same sha again → idempotent ok; a different sha → rejected.
    d.operator_rpc(
        "task_sha",
        json!({"task": "j1-t2", "sha": SHA_A, "by": "pm"}),
    )
    .unwrap();
    assert!(d
        .rpc(
            "task_sha",
            json!({"task": "j1-t2", "sha": SHA_B, "by": "pm"})
        )
        .is_err());
    d.job_verdict("j1-t2", SHA_A, "pass").unwrap();
    assert_eq!(d.task_state("j1-t2"), "verified");
    let ev = d.events("pm");
    assert!(ev.iter().any(|e| e["kind"] == "task_sha_recorded"));
}

#[test]
fn max_revisions_escalates_to_blocked_once() {
    let d = TestDaemon::start();
    d.register("pm");
    d.register_member("w1", "pm");
    d.wait_agent("w1", "idle", 10);
    let (spec, sha) = d.spec_file("spec.md", "cap test");
    d.rpc(
        "job_new",
        json!({"pm": "pm", "job": "j1", "spec": spec, "spec_sha256": sha,
               "max_revisions": 2}),
    )
    .unwrap();
    d.task_new_ac("j1", "j1-t2", "w1", format!("ok REPORT_SHA:{SHA_A}"))
        .unwrap();

    // r1 → review → revise → revising.
    let r = d.job_dispatch("j1-t2", json!({})).unwrap();
    let k = r["message"].as_str().unwrap().to_string();
    d.wait_message("w1", &k, &["completed"], 15);
    d.wait_task("j1-t2", "review", 15);
    d.job_verdict("j1-t2", SHA_A, "revise").unwrap();
    assert_eq!(d.task_state("j1-t2"), "revising");

    // r2 → review → revise at the cap → blocked, PM notified once.
    let r = d.job_dispatch("j1-t2", json!({})).unwrap();
    let k = r["message"].as_str().unwrap().to_string();
    d.wait_message("w1", &k, &["completed"], 15);
    d.wait_task("j1-t2", "review", 15);
    d.job_verdict("j1-t2", SHA_A, "revise").unwrap();
    assert_eq!(d.task_state("j1-t2"), "blocked");

    // The loop cannot continue: dispatch without --to names reopen.
    let e = d.job_dispatch("j1-t2", json!({})).unwrap_err();
    assert!(e.to_string().contains("reopen"), "{e}");
    // A verdict lands only on review — blocked task rejects.
    assert!(d.job_verdict("j1-t2", SHA_A, "pass").is_err());

    // Exactly one blocked notification to the PM.
    let pm_msgs = d.rpc("agent_show", json!({"alias": "pm"})).unwrap()["messages"]
        .as_array()
        .unwrap()
        .clone();
    let blocked: Vec<&Value> = pm_msgs
        .iter()
        .filter(|m| {
            m["source"] == "job_event" && m["body"].as_str().unwrap_or("").contains("blocked")
        })
        .collect();
    assert_eq!(blocked.len(), 1, "{pm_msgs:?}");

    // Operator reopen re-scopes: draft, revision 0, dispatch works.
    d.operator_rpc("task_reopen", json!({"task": "j1-t2"}))
        .unwrap();
    assert_eq!(d.task_state("j1-t2"), "draft");
    let r = d.job_dispatch("j1-t2", json!({})).unwrap();
    assert_eq!(r["task"]["revision"], 1);
}

#[test]
fn job_cancel_semantics() {
    let d = TestDaemon::start();
    d.register("pm");
    d.register_member("w1", "pm");
    d.register_member("w2", "pm");
    d.wait_agent("w1", "idle", 10);
    let (spec, sha) = d.spec_file("spec.md", "cancel semantics");
    d.job_new("pm", "j1", &spec, &sha);

    // Queued kickoff: stop w2 first so its queue never drains.
    d.operator_rpc("agent_stop", json!({"alias": "w2"}))
        .unwrap();
    d.rpc(
        "task_new",
        json!({"job": "j1", "task": "j1-tq", "assignee": "w2"}),
    )
    .unwrap();
    let r = d.job_dispatch("j1-tq", json!({})).unwrap();
    assert_eq!(r["queued_behind_dead"], true, "{r}");
    let k = r["message"].as_str().unwrap().to_string();
    assert_eq!(d.message_state("w2", &k), "queued");
    d.operator_rpc("task_cancel", json!({"task": "j1-tq", "by": "pm"}))
        .unwrap();
    assert_eq!(d.task_state("j1-tq"), "cancelled");
    // The queued kickoff was cancelled in the same transaction.
    assert_eq!(d.message_state("w2", &k), "cancelled");
    // The agent itself was never stopped by the job layer.
    let w2 = d.rpc("agent_show", json!({"alias": "w2"})).unwrap()["agent"].clone();
    assert_eq!(w2["state"], "stopped"); // operator stop, unchanged

    // Running kickoff: cancel leaves it alone — it completes on its own.
    d.task_new_ac("j1", "j1-tr", "w1", format!("ok REPORT_SHA:{SHA_A}"))
        .unwrap();
    let r = d.job_dispatch("j1-tr", json!({})).unwrap();
    let k = r["message"].as_str().unwrap().to_string();
    d.operator_rpc("task_cancel", json!({"task": "j1-tr", "by": "pm"}))
        .unwrap();
    assert_eq!(d.task_state("j1-tr"), "cancelled");
    // The kickoff still ran to completion; the task stays cancelled.
    // The task edge commits in the message's own finish transaction, so
    // `completed` already carries any task effect — no settle sleep.
    d.wait_message("w1", &k, &["completed"], 15);
    assert_eq!(d.task_state("j1-tr"), "cancelled");
    d.wait_agent("w1", "idle", 10); // agent alive and untouched

    // job cancel cancels every non-terminal task + the job.
    d.rpc("task_new", json!({"job": "j1", "task": "j1-tz"}))
        .unwrap();
    d.operator_rpc("job_cancel", json!({"job": "j1", "by": "pm"}))
        .unwrap();
    assert_eq!(d.job_state("j1"), "cancelled");
    assert_eq!(d.task_state("j1-tz"), "cancelled");
    // Terminal job rejects new tasks/dispatch.
    assert!(d.job_dispatch("j1-tq", json!({})).is_err());
    // job close requires all-done.
    assert!(d
        .operator_rpc("job_close", json!({"job": "j1", "by": "pm"}))
        .is_err());
}

#[test]
fn dispatch_dedupes_live_kickoff_and_reassign_bumps() {
    let d = TestDaemon::start();
    d.register("pm");
    d.register_member("w1", "pm");
    d.register_member("w2", "pm");
    d.wait_agent("w1", "idle", 10);
    let (spec, sha) = d.spec_file("spec.md", "dedupe + reassign");
    d.job_new("pm", "j1", &spec, &sha);
    d.task_new_ac("j1", "j1-t2", "w1", format!("ok REPORT_SHA:{SHA_A}"))
        .unwrap();

    // Live kickoff → second dispatch is the SAME revision, same id.
    // Deterministic: stop w1 first so its queue never drains.
    d.fixture_rpc("agent_stop", json!({"alias": "w1"})).unwrap();
    let r1 = d.job_dispatch("j1-t2", json!({})).unwrap();
    let k1 = r1["message"].as_str().unwrap().to_string();
    assert_eq!(r1["task"]["state"], "dispatched");
    assert_eq!(r1["task"]["revision"], 1);
    assert_eq!(d.message_state("w1", &k1), "queued");
    let r2 = d.job_dispatch("j1-t2", json!({})).unwrap();
    assert_eq!(r2["duplicate"], true, "{r2}");
    assert_eq!(r2["message"], k1);
    assert_eq!(r2["task"]["revision"], 1);
    // Still exactly one kickoff row.
    let t = d.rpc("task_show", json!({"task": "j1-t2"})).unwrap()["task"].clone();
    let kicks: Vec<&Value> = t["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["source"] == "job_dispatch")
        .collect();
    assert_eq!(kicks.len(), 1, "{kicks:?}");

    // Reassign under a live kickoff is refused.
    let e = d.job_dispatch("j1-t2", json!({"to": "w2"})).unwrap_err();
    assert!(e.to_string().contains("live kickoff"), "{e}");

    // The natural reassign path: let the kickoff complete, revise,
    // then --to bumps the revision with a fresh kickoff id.
    d.fixture_rpc("agent_resume", json!({"alias": "w1"}))
        .unwrap();
    d.wait_message("w1", &k1, &["completed"], 15);
    d.wait_task("j1-t2", "review", 15);
    d.job_verdict("j1-t2", SHA_A, "revise").unwrap();
    assert_eq!(d.task_state("j1-t2"), "revising");
    let r = d.job_dispatch("j1-t2", json!({"to": "w2"})).unwrap();
    assert_eq!(r["task"]["revision"], 2, "{r}");
    assert_eq!(r["task"]["assignee"], "w2");
    assert_ne!(r["message"], k1);
    d.wait_message("w2", r["message"].as_str().unwrap(), &["completed"], 15);
    d.wait_task("j1-t2", "review", 15);
}

#[test]
fn task_attached_send_and_self() {
    let d = TestDaemon::start();
    d.register("pm");
    d.register_member("w1", "pm");
    d.wait_agent("w1", "idle", 10);
    let (spec, sha) = d.spec_file("spec.md", "send --task");
    d.job_new("pm", "j1", &spec, &sha);
    d.task_new_ac("j1", "j1-t2", "w1", format!("ok REPORT_SHA:{SHA_A}"))
        .unwrap();

    // `send --task` attaches for indexing — the message completes
    // normally and does NOT drive the task state machine.
    d.send(
        "w1",
        json!({"text": "ping", "message": "adhoc1", "task": "j1-t2"}),
    )
    .unwrap();
    d.wait_message("w1", "adhoc1", &["completed"], 15);
    assert_eq!(d.task_state("j1-t2"), "draft");
    let m = d.rpc("agent_show", json!({"alias": "w1"})).unwrap()["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == "adhoc1")
        .unwrap()
        .clone();
    assert_eq!(m["task_id"], "j1-t2");
    // Task show lists the attached delivery.
    let t = d.rpc("task_show", json!({"task": "j1-t2"})).unwrap()["task"].clone();
    let ids: Vec<&str> = t["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|m| m["id"].as_str())
        .collect();
    assert!(ids.contains(&"adhoc1"), "{ids:?}");

    // `message result --sha` lands the sha on a pty-less fake path? —
    // fake completions go through the adapter text; the --sha flag path
    // is covered by the store-level edge test. Here: attach + report
    // via reconcile path already covered.
    // `agent list` exposes the open task binding.
    let list = d.rpc("agent_list", json!({})).unwrap()["agents"]
        .as_array()
        .unwrap()
        .clone();
    let w1 = list.iter().find(|a| a["alias"] == "w1").unwrap();
    assert_eq!(w1["tasks"], json!(["j1-t2"]), "{w1}");
}

#[test]
fn job_event_parks_on_unrendered_pty_pm() {
    // CAD-185: three 5s retry waits were ~15s of this test and nothing
    // here asserts their length; the four render misses keep the real
    // RENDER_DEADLINE, since that timeout path is what the park proves.
    test_env().set("CADENCE_PTY_RETRY_SECS", "1");
    let state_dir = TempDir::new().unwrap();
    let mock_dir = TempDir::new().unwrap();
    let mock = install_mock_devin(mock_dir.path());
    let state = state_dir.path();
    let cwd = state.to_str().unwrap();
    let store = Store::open(&state.join("cadence.sqlite3")).unwrap();
    store
        .register_agent(&NewAgent {
            alias: "pm",
            provider: "devin",
            endpoint_kind: "pty",
            role: "pm",
            cwd,
            sandbox: "read-only",
            instructions: None,
            params: Some(r#"{"auto_ready":"verified"}"#),
            team_role: None,
            model_policy: None,
        })
        .unwrap();
    let spec = state.join("spec.md");
    let spec_body = b"pty pm park test";
    std::fs::write(&spec, spec_body).unwrap();
    use sha2::{Digest, Sha256};
    let spec_sha = format!("{:x}", Sha256::digest(spec_body));
    store
        .create_job(
            "j1",
            Some("pty pm park test"),
            spec.to_str().unwrap(),
            &spec_sha,
            "pm",
            None,
            None,
            None,
            2,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
    // Seed the notification through the same Store::job_notice →
    // route_job_event path used by job verdict, while leaving worker
    // dispatch/review lifecycle coverage to the other job tests. The
    // queued row is present before daemon boot, so no second Store can
    // reset an active endpoint and the daemon sees its normal wake path.
    store
        .job_notice(
            "j1-t1",
            "verified",
            "fixture:job_event_park",
            "fixture routed job event",
        )
        .unwrap();
    drop(store);
    let socket_dir = mock.dir.join("tmux-state").join(socket_for(state));
    std::fs::create_dir_all(&socket_dir).unwrap();
    atomic_write(socket_dir.join("pm.swallow"), "1");
    let d = TestDaemon::start_on(state.to_path_buf());
    d.wait_agent("pm", "idle", 20);

    // The job_event notification requeues bounded, then parks — the
    // PM pane survives, never fenced.
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        let pm = d.rpc("agent_show", json!({"alias": "pm"})).unwrap();
        let parked = pm["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["source"] == "job_event" && m["state"] == "failed");
        if parked {
            break;
        }
        assert!(Instant::now() < deadline, "job_event never parked");
        thread::sleep(Duration::from_millis(200));
    }
    let evs = d.events("pm");
    let parked_ev = evs
        .iter()
        .find(|e| e["kind"] == "delivery_parked")
        .expect("no delivery_parked event");
    let parked_id = parked_ev["payload"]["message"].as_str().unwrap();
    let msg = d.rpc("agent_show", json!({"alias": "pm"})).unwrap()["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == parked_id)
        .unwrap()
        .clone();
    assert_eq!(msg["result"]["via"], "pty_render_miss", "{msg}");
    // PM still alive + idle, never fenced.
    d.wait_agent("pm", "idle", 15);
    let pm = d.rpc("agent_show", json!({"alias": "pm"})).unwrap()["agent"].clone();
    assert_eq!(pm["dead"], false);
    assert_retry_gaps_under(&d, "pm", parked_id, 3, 4.0);
    emit_park_phase_trace(&d, "job_event_parks_on_unrendered_pty_pm", "pm", parked_id);
}

// ---- CAD-51: `job verdict` worktree verification + qa-verdict bridge ----

/// git in tests — panics on failure, returns trimmed stdout.
fn tgit(dir: &Path, args: &[&str]) -> String {
    let o = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "git {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

fn verify_repo(origin: Option<&str>) -> VerifyRepo {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path().join("repo");
    let wt = repo.join(".cadence/wt/fix");
    std::fs::create_dir_all(&repo).unwrap();
    tgit(&repo, &["init", "-b", "main"]);
    tgit(&repo, &["config", "user.email", "t@t"]);
    tgit(&repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join("f"), "base\n").unwrap();
    tgit(&repo, &["add", "-A"]);
    tgit(&repo, &["commit", "-qm", "init"]);
    let base = tgit(&repo, &["rev-parse", "HEAD"]);
    tgit(
        &repo,
        &["worktree", "add", "-b", "cadence/fix", wt.to_str().unwrap()],
    );
    std::fs::write(wt.join("f"), "work\n").unwrap();
    tgit(&wt, &["commit", "-qam", "work"]);
    let head = tgit(&repo, &["rev-parse", "cadence/fix"]);
    if let Some(url) = origin {
        tgit(&repo, &["remote", "add", "origin", url]);
        if url.contains("github.com") {
            // A GitHub URL is never fetched in tests — place the
            // remote-tracking ref by hand instead.
            tgit(
                &repo,
                &["update-ref", "refs/remotes/origin/cadence/fix", &head],
            );
        } else {
            tgit(&repo, &["push", "-qu", "origin", "cadence/fix"]);
        }
    }
    VerifyRepo {
        _tmp: tmp,
        repo,
        worktree: wt,
        base,
        head,
    }
}

/// pm + worker + job `j1` bound to `repo`.
fn verdict_setup(d: &TestDaemon, repo: &Path) {
    d.register("pm");
    d.register_member("w1", "pm");
    d.wait_agent("pm", "idle", 10);
    d.wait_agent("w1", "idle", 10);
    let (spec, sha) = d.spec_file("spec.md", "do the work");
    d.job_new_repo("pm", "j1", &spec, &sha, repo.to_str().unwrap())
        .unwrap();
}

/// Dispatch `task` to the fake worker; its REPORT_SHA trailer lands
/// `head` as the reported sha and the task reaches `review`.
fn task_to_review(d: &TestDaemon, task: &str, head: &str, scope: Value) {
    let mut new = json!({"job": "j1", "task": task, "assignee": "w1",
        "acceptance": format!("ok REPORT_SHA:{head}")});
    for (k, v) in scope.as_object().unwrap_or(&serde_json::Map::new()) {
        new[k] = v.clone();
    }
    d.rpc("task_new", new).unwrap();
    d.job_dispatch(task, json!({})).unwrap();
    d.wait_task(task, "review", 15);
}

fn verdict_args(task: &str, sha: &str, flag: &str) -> Vec<String> {
    vec![
        "job".to_string(),
        "verdict".to_string(),
        task.to_string(),
        "--sha".to_string(),
        sha.to_string(),
        flag.to_string(),
    ]
}

fn cli_verdict(
    d: &TestDaemon,
    task: &str,
    sha: &str,
    flag: &str,
    extra: &[&str],
    envs: &[(String, String)],
) -> (bool, Value) {
    let mut args: Vec<String> = verdict_args(task, sha, flag);
    args.extend(extra.iter().map(|s| s.to_string()));
    // The reviewer is the verified connection (CAD-372): the CLI runs
    // in a reviewer pane, `qa-cli`, planted for this call.
    let home = TempDir::new().unwrap();
    let mut qa = LaneShell::spawn(home.path());
    plant_pane(d, "qa-cli", qa.pid());
    let bin = env!("CARGO_BIN_EXE_cadence");
    let bin_dir = Path::new(bin).parent().unwrap().display().to_string();
    let quote = |v: &str| format!("'{}'", v.replace('\'', "'\\''"));
    let mut env = format!("PATH={}:\"$PATH\"", quote(&bin_dir));
    for (k, v) in envs {
        env.push_str(&format!(" {k}={}", quote(v)));
    }
    let err = qa.dir.path().join("verdict.err");
    let (rc, out) = qa.run(&format!(
        "env -u CADENCE_ALIAS {env} {} --state-dir {} {} 2>{}",
        quote(bin),
        quote(&d.state.display().to_string()),
        args.iter().map(|a| quote(a)).collect::<Vec<_>>().join(" "),
        quote(&err.display().to_string())
    ));
    let text = if out.trim().is_empty() {
        std::fs::read_to_string(&err).unwrap_or_default()
    } else {
        out
    };
    (
        rc == 0,
        serde_json::from_str(text.trim()).unwrap_or_else(|_| panic!("not json: {text}")),
    )
}

/// The task's recorded verdicts.
fn task_verdicts(d: &TestDaemon, task: &str) -> Value {
    d.rpc("task_show", json!({"task": task})).unwrap()["task"]["verdicts"].clone()
}

#[test]
fn job_verdict_worktree_verify_binds_and_records() {
    let d = TestDaemon::start();
    // A real bare repo gets a real `origin/<branch>` ref via push.
    let bare = TempDir::new().unwrap();
    tgit(bare.path(), &["init", "--bare"]);
    let r = verify_repo(Some(bare.path().to_str().unwrap()));
    verdict_setup(&d, &r.repo);
    task_to_review(
        &d,
        "j1-t2",
        &r.head,
        json!({"worktree": "fix", "branch": "cadence/fix", "base_sha": r.base}),
    );

    let (ok, out) = cli_verdict(&d, "j1-t2", &r.head, "--pass", &[], &[]);
    assert!(ok, "{out}");
    let verify = out["verdict"]["verify"].clone();
    assert_eq!(
        verify["checked"].as_array().unwrap(),
        &json!([
            "commit",
            "branch tip",
            "base ancestor",
            "worktree clean",
            "pushed"
        ])
        .as_array()
        .unwrap()
        .clone(),
        "{verify}"
    );
    assert_eq!(verify["skipped"], json!([]), "{verify}");
    // The origin is a local path — not a GitHub remote, so the bridge
    // reports instead of posting; the verdict still committed.
    assert_eq!(out["status"]["posted"], false, "{out}");
    assert!(
        out["status"]["reason"]
            .as_str()
            .unwrap()
            .contains("not a GitHub remote"),
        "{out}"
    );

    // The verify result is stored on the row and echoed on the event.
    let vs = task_verdicts(&d, "j1-t2");
    assert_eq!(vs.as_array().unwrap().len(), 1, "{vs}");
    assert_eq!(vs[0]["verify"]["checked"], verify["checked"], "{vs}");
    let events = d.rpc("job_events", json!({"job": "j1"})).unwrap()["events"].clone();
    let recorded = events
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "verdict_recorded")
        .expect("verdict_recorded event");
    assert_eq!(
        recorded["payload"]["verify"]["checked"], verify["checked"],
        "{recorded}"
    );
}

#[test]
fn job_verdict_worktree_verify_rejects_each_check() {
    let d = TestDaemon::start();
    let bare = TempDir::new().unwrap();
    tgit(bare.path(), &["init", "--bare"]);
    let r = verify_repo(Some(bare.path().to_str().unwrap()));
    verdict_setup(&d, &r.repo);
    let scope = || json!({"worktree": "fix", "branch": "cadence/fix", "base_sha": r.base});
    let still_review = |task: &str| {
        assert_eq!(
            task_verdicts(&d, task),
            json!([]),
            "verdict row written for {task}"
        );
        assert_eq!(d.task_state(task), "review");
    };

    // Dirty worktree — the uncommitted file names the clean check.
    task_to_review(&d, "j1-t2", &r.head, scope());
    std::fs::write(r.worktree.join("dirty.txt"), "x").unwrap();
    let (ok, out) = cli_verdict(&d, "j1-t2", &r.head, "--pass", &[], &[]);
    assert!(!ok, "{out}");
    let err = out["error"].as_str().unwrap();
    assert!(
        err.contains("worktree clean") && err.contains("uncommitted") && err.contains("dirty.txt"),
        "{err}"
    );
    std::fs::remove_file(r.worktree.join("dirty.txt")).unwrap();
    still_review("j1-t2");

    // Wrong tip — head_sha is the real base commit, not the branch tip.
    task_to_review(&d, "j1-t3", &r.base, scope());
    let (ok, out) = cli_verdict(&d, "j1-t3", &r.base, "--pass", &[], &[]);
    assert!(!ok, "{out}");
    let err = out["error"].as_str().unwrap();
    assert!(
        err.contains("branch tip") && err.contains(&r.head) && err.contains(&r.base),
        "{err}"
    );
    still_review("j1-t3");

    // Base not an ancestor — a newer main commit never joined the branch.
    std::fs::write(r.repo.join("m"), "main\n").unwrap();
    tgit(&r.repo, &["add", "-A"]);
    tgit(&r.repo, &["commit", "-qm", "main work"]);
    let main_tip = tgit(&r.repo, &["rev-parse", "main"]);
    task_to_review(
        &d,
        "j1-t4",
        &r.head,
        json!({"worktree": "fix", "branch": "cadence/fix", "base_sha": main_tip}),
    );
    let (ok, out) = cli_verdict(&d, "j1-t4", &r.head, "--pass", &[], &[]);
    assert!(!ok, "{out}");
    let err = out["error"].as_str().unwrap();
    assert!(
        err.contains("base ancestor") && err.contains(&main_tip) && err.contains(&r.head),
        "{err}"
    );
    still_review("j1-t4");

    // Unpushed — the branch advanced locally, origin stayed behind.
    std::fs::write(r.worktree.join("f"), "more\n").unwrap();
    tgit(&r.worktree, &["commit", "-qam", "more"]);
    let head2 = tgit(&r.repo, &["rev-parse", "cadence/fix"]);
    assert_ne!(head2, r.head);
    task_to_review(&d, "j1-t5", &head2, scope());
    let (ok, out) = cli_verdict(&d, "j1-t5", &head2, "--pass", &[], &[]);
    assert!(!ok, "{out}");
    let err = out["error"].as_str().unwrap();
    assert!(
        err.contains("pushed") && err.contains(&r.head) && err.contains(&head2),
        "{err}"
    );
    still_review("j1-t5");

    // A bogus reported sha never resolves to a commit at all — the
    // worker can report any 40-hex; the check is what binds it.
    let bogus = "1".repeat(40);
    task_to_review(&d, "j1-t6", &bogus, scope());
    let (ok, out) = cli_verdict(&d, "j1-t6", &bogus, "--pass", &[], &[]);
    assert!(!ok, "{out}");
    let err = out["error"].as_str().unwrap();
    assert!(
        err.contains("commit") && err.contains(&bogus) && err.contains("does not resolve"),
        "{err}"
    );
    still_review("j1-t6");
}

#[test]
fn job_verdict_worktree_verify_skips_and_opt_out() {
    let d = TestDaemon::start();
    let r = verify_repo(None); // no origin at all
    verdict_setup(&d, &r.repo);
    task_to_review(
        &d,
        "j1-t2",
        &r.head,
        json!({"worktree": "fix", "branch": "cadence/fix", "base_sha": r.base}),
    );
    // The worktree dir is gone — both the clean check and the pushed
    // check cannot apply.
    std::fs::remove_dir_all(&r.worktree).unwrap();
    let (ok, out) = cli_verdict(&d, "j1-t2", &r.head, "--pass", &["--no-status"], &[]);
    assert!(ok, "{out}");
    let verify = out["verdict"]["verify"].clone();
    let skipped: Vec<&str> = verify["skipped"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["check"].as_str().unwrap())
        .collect();
    assert_eq!(
        verify["checked"].as_array().unwrap(),
        &json!(["commit", "branch tip", "base ancestor"])
            .as_array()
            .unwrap()
            .clone(),
        "{verify}"
    );
    assert!(skipped.contains(&"worktree clean"), "{verify}");
    assert!(skipped.contains(&"pushed"), "{verify}");

    // No base_sha → the ancestor check skips too.
    task_to_review(
        &d,
        "j1-t3",
        &r.head,
        json!({"worktree": "fix", "branch": "cadence/fix"}),
    );
    let (ok, out) = cli_verdict(&d, "j1-t3", &r.head, "--pass", &["--no-status"], &[]);
    assert!(ok, "{out}");
    let skipped: Vec<&str> = out["verdict"]["verify"]["skipped"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["check"].as_str().unwrap())
        .collect();
    assert!(skipped.contains(&"base ancestor"), "{out}");

    // --no-verify-worktree records the opt-out on the verdict.
    task_to_review(
        &d,
        "j1-t4",
        &r.head,
        json!({"worktree": "fix", "branch": "cadence/fix", "base_sha": r.base}),
    );
    let (ok, out) = cli_verdict(
        &d,
        "j1-t4",
        &r.head,
        "--pass",
        &["--no-verify-worktree", "--no-status"],
        &[],
    );
    assert!(ok, "{out}");
    let verify = &out["verdict"]["verify"];
    assert_eq!(verify["checked"], json!([]), "{verify}");
    assert!(
        verify["skipped"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["reason"]
                .as_str()
                .unwrap()
                .contains("--no-verify-worktree")),
        "{verify}"
    );
    let vs = task_verdicts(&d, "j1-t4");
    assert!(
        vs[0]["verify"]["skipped"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["reason"]
                .as_str()
                .unwrap()
                .contains("--no-verify-worktree")),
        "{vs}"
    );
}

#[test]
fn job_verdict_status_bridge_posts_qa_verdict() {
    let d = TestDaemon::start();
    let r = verify_repo(Some("https://github.com/acme/widgets.git"));
    let gh = fake_gh();
    verdict_setup(&d, &r.repo);
    let scope = || json!({"worktree": "fix", "branch": "cadence/fix", "base_sha": r.base});
    let open_pr = |head: &str| {
        (
            "FAKE_GH_PRS".to_string(),
            format!("[{{\"number\": 7, \"headRefOid\": \"{head}\"}}]"),
        )
    };

    // pass → success on the head sha, task + revision in the description.
    task_to_review(&d, "j1-t2", &r.head, scope());
    let envs = gh.envs(&[open_pr(&r.head)]);
    let (ok, out) = cli_verdict(&d, "j1-t2", &r.head, "--pass", &[], &envs);
    assert!(ok, "{out}");
    assert_eq!(
        out["status"],
        json!({"posted": true, "pr": 7, "sha": r.head}),
        "{out}"
    );
    let calls = gh.calls();
    assert!(
        calls.iter().any(|c| c.contains(&format!(
            "api\t--method\tPOST\trepos/acme/widgets/statuses/{}\t-f\tcontext=qa-verdict\t-f\tstate=success\t-f\tdescription=pass — j1-t2 r1",
            r.head
        ))),
        "{calls:?}"
    );

    // revise → failure; blocked → failure. Same head sha throughout.
    task_to_review(&d, "j1-t3", &r.head, scope());
    let (ok, out) = cli_verdict(&d, "j1-t3", &r.head, "--revise", &[], &envs);
    assert!(ok, "{out}");
    assert_eq!(out["status"]["posted"], true, "{out}");
    d.job_dispatch("j1-t3", json!({})).unwrap();
    d.wait_task("j1-t3", "review", 15);
    let (ok, out) = cli_verdict(&d, "j1-t3", &r.head, "--blocked", &[], &envs);
    assert!(ok, "{out}");
    assert_eq!(out["status"]["posted"], true, "{out}");
    let calls = gh.calls();
    assert!(
        calls
            .iter()
            .any(|c| c.contains("state=failure") && c.contains("description=revise — j1-t3 r1")),
        "{calls:?}"
    );
    assert!(
        calls
            .iter()
            .any(|c| c.contains("state=failure") && c.contains("description=blocked — j1-t3 r2")),
        "{calls:?}"
    );

    // --pr names the PR instead of branch discovery.
    task_to_review(&d, "j1-t4", &r.head, scope());
    let envs = gh.envs(&[
        ("FAKE_GH_PR_NUM".to_string(), "9".to_string()),
        ("FAKE_GH_HEAD".to_string(), r.head.clone()),
    ]);
    let (ok, out) = cli_verdict(&d, "j1-t4", &r.head, "--pass", &["--pr", "9"], &envs);
    assert!(ok, "{out}");
    assert_eq!(out["status"]["pr"], 9, "{out}");
    assert!(
        gh.calls().iter().any(|c| c.contains("pr\tview\t9")),
        "{:?}",
        gh.calls()
    );
}

#[test]
fn job_verdict_status_bridge_failures_keep_the_verdict() {
    let d = TestDaemon::start();
    let r = verify_repo(Some("https://github.com/acme/widgets.git"));
    let gh = fake_gh();
    verdict_setup(&d, &r.repo);
    let scope = || json!({"worktree": "fix", "branch": "cadence/fix", "base_sha": r.base});
    let envs_of = |extra: &[(String, String)]| -> Vec<(String, String)> { gh.envs(extra) };

    // No open PR — the verdict commits, the status reports why.
    task_to_review(&d, "j1-t2", &r.head, scope());
    let envs = envs_of(&[("FAKE_GH_PRS".to_string(), "[]".to_string())]);
    let (ok, out) = cli_verdict(&d, "j1-t2", &r.head, "--pass", &[], &envs);
    assert!(ok, "{out}");
    assert_eq!(out["status"]["posted"], false, "{out}");
    assert!(
        out["status"]["reason"]
            .as_str()
            .unwrap()
            .contains("no open PR for branch cadence/fix"),
        "{out}"
    );
    assert_eq!(d.task_state("j1-t2"), "verified");

    // A moved head is reported — never posted to a sha the reviewer
    // did not name.
    task_to_review(&d, "j1-t3", &r.head, scope());
    let other = "1".repeat(40);
    let envs = envs_of(&[(
        "FAKE_GH_PRS".to_string(),
        format!("[{{\"number\": 7, \"headRefOid\": \"{other}\"}}]"),
    )]);
    let (ok, out) = cli_verdict(&d, "j1-t3", &r.head, "--pass", &[], &envs);
    assert!(ok, "{out}");
    assert_eq!(
        out["status"]["reason"].as_str().unwrap(),
        format!("pr head {other} is not the judged sha {}", r.head),
        "{out}"
    );
    assert!(
        !gh.calls().iter().any(|c| c.contains("statuses/")),
        "{:?}",
        gh.calls()
    );
    assert_eq!(d.task_state("j1-t3"), "verified");

    // A failing gh leaves the verdict in place with the reason.
    task_to_review(&d, "j1-t4", &r.head, scope());
    let envs = envs_of(&[("FAKE_GH_FAIL".to_string(), "1".to_string())]);
    let (ok, out) = cli_verdict(&d, "j1-t4", &r.head, "--pass", &[], &envs);
    assert!(ok, "{out}");
    assert_eq!(out["status"]["posted"], false, "{out}");
    assert!(
        out["status"]["reason"]
            .as_str()
            .unwrap()
            .contains("forced failure"),
        "{out}"
    );
    assert_eq!(d.task_state("j1-t4"), "verified");

    // --no-status makes no gh call at all.
    task_to_review(&d, "j1-t5", &r.head, scope());
    let before = gh.calls().len();
    let envs = envs_of(&[]);
    let (ok, out) = cli_verdict(&d, "j1-t5", &r.head, "--pass", &["--no-status"], &envs);
    assert!(ok, "{out}");
    assert_eq!(out["status"]["posted"], false, "{out}");
    assert_eq!(gh.calls().len(), before, "{:?}", gh.calls());
    assert_eq!(d.task_state("j1-t5"), "verified");
}

#[test]
fn job_verdict_unscoped_task_is_unchanged() {
    let d = TestDaemon::start();
    verdict_setup(&d, Path::new("/tmp"));
    // No worktree/branch scope — the old path, no verify, no bridge.
    task_to_review(&d, "j1-t2", SHA_A, json!({}));
    let (ok, out) = cli_verdict(&d, "j1-t2", SHA_A, "--pass", &[], &[]);
    assert!(ok, "{out}");
    assert!(out["verdict"]["verify"].is_null(), "{out}");
    assert_eq!(out["status"]["posted"], false, "{out}");
    assert!(
        out["status"]["reason"]
            .as_str()
            .unwrap()
            .contains("no branch"),
        "{out}"
    );
    assert_eq!(d.task_state("j1-t2"), "verified");
}

/// A job kickoff's stall goes to the PM as a `job_event`, the event is
/// job/task-scoped, and the task row carries the flag while it lasts.
#[test]
fn job_kickoff_stall_flags_task_and_notifies_pm() {
    let d = TestDaemon::start();
    let _mock = d.mock_stub();
    stall_sample(2);
    d.register_inbox("pm");
    d.register_stub("w1", json!({"upstream": "pm", "auto_ready": "verified"}));
    d.wait_agent("w1", "idle", 20);
    let (spec, sha) = d.spec_file("spec.md", "stall me");
    d.rpc(
        "job_new",
        json!({"pm": "pm", "job": "j1", "spec": spec, "spec_sha256": sha,
               "stall_secs": 3, "task_assignee": "w1"}),
    )
    .unwrap();
    d.job_dispatch("j1-t1", json!({})).unwrap();
    d.wait_task("j1-t1", "running", 15);

    let e = d.wait_event("w1", "turn_stalled", 30);
    assert_eq!(e["payload"]["task"], "j1-t1", "{e}");
    // Job-scoped: the job event stream sees it too.
    let ev = d.rpc("job_events", json!({"job": "j1"})).unwrap();
    assert!(
        ev["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["kind"].as_str() == Some("turn_stalled")),
        "{ev}"
    );
    // The PM notice is a `job_event` — one per episode.
    let notices = wait_source(&d, "pm", "job_event", 1, 10);
    let stalled = notices
        .iter()
        .filter(|m| m["body"].as_str().unwrap_or("").contains("no activity"))
        .count();
    assert_eq!(stalled, 1, "{notices:?}");
    // The task row is flagged while the kickoff is stalled.
    let show = d.rpc("job_show", json!({"job": "j1"})).unwrap();
    let task = &show["job"]["tasks"][0];
    assert_eq!(task["stalled"], true, "{task}");
    assert!(task["silent_secs"].as_u64().unwrap_or(0) >= 3, "{task}");
    assert_eq!(d.task_state("j1-t1"), "running");
    stall_sample(0);
}

/// One dispatch: `issue start` side effects + exactly one templated
/// kickoff + comment + message ref. A second run reuses the worktree
/// and refuses the duplicate while the first is live. Fenced and
/// out-of-group workers are refused before anything is created, as is
/// a `--summary` that breaks the pty single-line rule. `--job` binds
/// the kickoff through `job dispatch` to the scoped task. `issue
/// finish` then refuses while the owner has a live message, while the
/// worktree is dirty, and while the branch is unmerged+unpushed —
/// and succeeds after the merge.
#[test]
fn dispatch_kickoff_and_finish_guards() {
    // Seed the group before the daemon starts: pm plus its members,
    // one fenced, one outside the group.
    // w1/w2 are `inbox` endpoints — no actor drains their queue,
    // so a queued kickoff stays live for the duplicate checks and
    // exercises the CAD-64 inbox-owner exemption in finish. The
    // others are fake.
    let (_seeded, state) = seeded_state(
        &[
            ("pm", None, "fake", "worker"),
            ("w1", Some("{\"upstream\":\"pm\"}"), "inbox", "worker"),
            ("w2", Some("{\"upstream\":\"pm\"}"), "inbox", "worker"),
            ("fenced", Some("{\"upstream\":\"pm\"}"), "fake", "worker"),
            ("outsider", None, "fake", "worker"),
        ],
        |store, _cwd| {
            store
                .set_agent_state("fenced", "attention", Some("test fence"))
                .unwrap();
        },
    );
    // Tracker + project repo + issues fixture (same shape as the
    // issue-start test). The daemon reads the tracker itself on
    // `dispatch_send` (claim check + lane resolution), so the pm dir
    // binds before it starts — a test daemon never falls back to ~/pm.
    let (tmp, pm_dir, repo, home) = pm_lab_dirs();
    test_env().set("CADENCE_PM_DIR", pm_dir.to_str().unwrap());
    let d = TestDaemon::start_on(state);
    d.wait_agent("fenced", "attention", 10);

    let git = git_ok();
    git_f_repo(&repo, &git, |repo| {
        // A cargo checkout — CAD-95 r3 plants the dep-cache farm only in
        // repos with a Cargo.toml; this fixture wants the shared-farm
        // assertions, so it declares itself a cargo package (build
        // output gitignored, as real repos do).
        std::fs::write(
            repo.join("Cargo.toml"),
            "[package]\nname = \"m\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::write(repo.join(".gitignore"), "/target\n").unwrap();
    });
    let cli = cadence_cli_json(&d.state, &pm_dir, &home);
    let repo_s = repo.canonicalize().unwrap().to_str().unwrap().to_string();
    demo_project_init(&cli, &repo_s);
    demo_issue_news(&cli, &["One", "Two", "Three", "Four"]);
    let tracker_commits = || {
        String::from_utf8_lossy(
            &std::process::Command::new("git")
                .arg("-C")
                .arg(&pm_dir)
                .args(["rev-list", "--count", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .trim()
        .parse::<usize>()
        .unwrap()
    };
    let note = tmp.path().join("kickoff.md");
    std::fs::write(&note, "# kickoff D-1").unwrap();
    let note_s = note.canonicalize().unwrap().to_str().unwrap().to_string();

    // A fenced worker refuses before anything is created.
    let (ok, err) = cli(&[
        "dispatch",
        "D-1",
        "--to",
        "fenced",
        "--note",
        &note_s,
        "--reply-to",
        "pm",
    ]);
    assert!(
        !ok && err["error"].as_str().unwrap().contains("fenced"),
        "{err}"
    );
    let wt1 = repo.join(".cadence/wt/d-1-one");
    assert!(!wt1.exists());

    // A body that breaks the single-line rule refuses pre-creation.
    let (ok, err) = cli(&[
        "dispatch",
        "D-1",
        "--to",
        "w1",
        "--note",
        &note_s,
        "--reply-to",
        "pm",
        "--summary",
        "line one\nline two",
    ]);
    assert!(
        !ok && err["error"].as_str().unwrap().contains("single line"),
        "{err}"
    );
    assert!(!wt1.exists());
    let before = tracker_commits();

    // The dispatch: worktree+branch, owner w1, exactly one kickoff.
    let (ok, out) = cli(&[
        "dispatch",
        "D-1",
        "--to",
        "w1",
        "--note",
        &note_s,
        "--reply-to",
        "pm",
    ]);
    assert!(ok, "{out}");
    assert_eq!(out["dispatched"], true);
    assert_eq!(out["created"], true);
    assert!(wt1.is_dir());
    // CAD-95: dispatch goes through `issue start` — the worktree's
    // hashed cargo subdirs are linked into the shared dep cache and
    // the lane's own target dir is reported.
    let shared = repo.join(".cadence/target/shared");
    assert_eq!(
        out["target_dir"].as_str().unwrap(),
        wt1.join("target").to_string_lossy()
    );
    assert_eq!(
        std::fs::read_link(wt1.join("target/debug/deps")).unwrap(),
        shared.join("debug/deps")
    );
    assert!(!wt1.join(".cargo").exists());
    let msg_id = out["message"].as_str().unwrap().to_string();
    assert!(!msg_id.is_empty());
    let show = d.rpc("agent_show", json!({"alias": "w1"})).unwrap();
    let msgs = show["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 1);
    let kickoff = &msgs[0];
    assert_eq!(kickoff["id"].as_str().unwrap(), msg_id);
    assert_eq!(kickoff["state"].as_str().unwrap(), "queued");
    assert_eq!(kickoff["reply_to"].as_str().unwrap(), "pm");
    let body = kickoff["body"].as_str().unwrap();
    assert!(
        body.starts_with(&format!("read {note_s} — D-1:"))
            && body.contains(".cadence/wt/d-1-one")
            && body.contains("(branch cadence/d-1-one, base ")
            && body.contains("Commit trailer: Issue: D-1")
            && body.contains("PR to main; reply to pm."),
        "{body}"
    );
    // Tracker: start + comment + ref commits; the comment and the
    // message ref name the worker and the kickoff id.
    assert_eq!(tracker_commits(), before + 3);
    let issue = cli(&["issue", "show", "D-1", "--json"]).1;
    assert_eq!(issue["owner"].as_str().unwrap(), "w1");
    assert!(
        issue["comments"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["body"].as_str().unwrap().contains("Dispatched to w1")),
        "{issue}"
    );
    assert!(
        issue["refs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["kind"] == "message" && r["path"] == msg_id),
        "{issue}"
    );

    // A second identical run reuses the worktree and refuses the
    // duplicate while the kickoff is still live — no new commits.
    let (ok, out) = cli(&[
        "dispatch",
        "D-1",
        "--to",
        "w1",
        "--note",
        &note_s,
        "--reply-to",
        "pm",
    ]);
    assert!(ok, "{out}");
    assert_eq!(out["dispatched"], false);
    assert_eq!(out["duplicate"], true);
    assert_eq!(out["message"].as_str().unwrap(), msg_id);
    assert_eq!(out["created"], false);
    assert_eq!(tracker_commits(), before + 3);
    let show = d.rpc("agent_show", json!({"alias": "w1"})).unwrap();
    assert_eq!(show["messages"].as_array().unwrap().len(), 1);

    // --job: an out-of-group worker is refused before the job exists.
    let (spec, _sha) = d.spec_file("spec.md", "job dispatch spec");
    let (ok, err) = cli(&[
        "dispatch",
        "D-2",
        "--to",
        "outsider",
        "--note",
        &note_s,
        "--reply-to",
        "pm",
        "--job",
        "--spec",
        &spec,
    ]);
    assert!(
        !ok && err["error"].as_str().unwrap().contains("group"),
        "{err}"
    );
    assert!(!repo.join(".cadence/wt/d-2-two").exists());

    // --job to a member: job + scoped task + a task_dispatch kickoff,
    // all bound to the issue.
    let (ok, out) = cli(&[
        "dispatch",
        "D-2",
        "--to",
        "w2",
        "--note",
        &note_s,
        "--reply-to",
        "pm",
        "--job",
        "--spec",
        &spec,
    ]);
    assert!(ok, "{out}");
    assert_eq!(out["dispatched"], true);
    let (job_id, task_id) = (
        out["job"].as_str().unwrap().to_string(),
        out["task"].as_str().unwrap().to_string(),
    );
    assert_eq!(task_id, format!("{job_id}-t1"));
    let job = d.rpc("job_show", json!({"job": job_id})).unwrap();
    let tasks = job["job"]["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0]["assignee"].as_str().unwrap(), "w2");
    assert_eq!(tasks[0]["worktree"].as_str().unwrap(), "d-2-two");
    assert_eq!(tasks[0]["state"].as_str().unwrap(), "dispatched");
    // The kickoff rides the task — the issue's message ref matches.
    let issue = cli(&["issue", "show", "D-2", "--json"]).1;
    let ref_msg = issue["refs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["kind"] == "message")
        .and_then(|r| r["path"].as_str())
        .unwrap()
        .to_string();
    assert_eq!(ref_msg, out["message"].as_str().unwrap());

    // `issue finish` with w1's kickoff still queued: the kickoff's
    // message ref is recorded against THIS worktree, but a queued
    // message on an `inbox` mailbox is durable backlog — it drains
    // only on `cadence inbox`, never on its own (CAD-64). Only the
    // unmerged branch blocks.
    std::fs::write(wt1.join("work.txt"), "x").unwrap();
    git(&wt1, &["add", "-A"]);
    git(&wt1, &["commit", "-qm", "d-1 work"]);
    idle(&wt1);
    let (ok, err) = cli(&["issue", "finish", "D-1"]);
    assert!(!ok, "{err}");
    let msg = err["error"].as_str().unwrap();
    assert!(
        msg.contains("neither merged") && !msg.contains(&msg_id),
        "{msg}"
    );
    assert!(wt1.is_dir());

    // Merged: the queued inbox kickoff never blocks — finish succeeds
    // without --force and the mail stays queued, unconsumed.
    git(&repo, &["merge", "-q", "cadence/d-1-one"]);
    let (ok, out) = cli(&["issue", "finish", "D-1"]);
    assert!(
        ok && out["finished"] == true
            && out["overrode"] == json!([])
            && out["merged_by"] == "ancestry",
        "{out}"
    );
    assert!(!wt1.exists());
    let show = d.rpc("agent_show", json!({"alias": "w1"})).unwrap();
    assert_eq!(show["messages"].as_array().unwrap()[0]["state"], "queued");

    // A non-inbox owner: a mock-devin pty pane holding a RUNNING
    // message recorded against THIS worktree makes finish refuse
    // naming the message, and --force records the override. mk1 is
    // unbound until the test links it — before that, finish only
    // refuses the unmerged branch (CAD-94: a busy owner elsewhere is
    // not a reason).
    let _mock = d.mock_devin();
    d.register_devin("dvb", None);
    d.wait_agent("dvb", "idle", 15);
    let (ok, _) = cli(&["issue", "start", "D-3", "--owner", "dvb"]);
    assert!(ok);
    d.send("dvb", json!({"text": "keep working", "message": "mk1"}))
        .unwrap();
    d.fixture_rpc("agent_ready", json!({"alias": "dvb"}))
        .unwrap();
    d.wait_message("dvb", "mk1", &["running"], 10);
    let wt3 = repo.join(".cadence/wt/d-3-three");
    // Give the branch real work so survivability blocks too — the
    // unbound running message must NOT add a refusal of its own.
    std::fs::write(wt3.join("work3.txt"), "x").unwrap();
    git(&wt3, &["add", "-A"]);
    git(&wt3, &["commit", "-qm", "d-3 work"]);
    idle(&wt3);
    let (ok, err) = cli(&["issue", "finish", "D-3"]);
    assert!(!ok, "{err}");
    assert!(
        err["error"].as_str().unwrap().contains("neither merged"),
        "{err}"
    );
    // Record mk1 against D-3 → the same live message now blocks, named.
    let (ok, out) = cli(&["issue", "ref", "D-3", "message", "mk1"]);
    assert!(ok, "{out}");
    let (ok, err) = cli(&["issue", "finish", "D-3"]);
    assert!(!ok, "{err}");
    let msg = err["error"].as_str().unwrap();
    assert!(
        msg.contains("mk1") && msg.contains("running") && msg.contains("dvb"),
        "{msg}"
    );
    assert!(wt3.is_dir());
    let (ok, out) = cli(&["issue", "finish", "D-3", "--force"]);
    assert!(ok && out["finished"] == true, "{out}");
    assert!(
        out["overrode"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o == "bound-message"),
        "{out}"
    );
    assert!(!wt3.exists());

    // D-4: owner w2 idle (its kickoff went to D-2's task, and w2's
    // queued message is the dispatch on D-2 — wait, w2 HAS a live
    // message from the job dispatch). Use a fresh started issue owned
    // by 'pm' — pm has no inbound messages.
    let (ok, _) = cli(&["issue", "start", "D-4", "--owner", "pm"]);
    assert!(ok);
    let wt4 = repo.join(".cadence/wt/d-4-four");
    // Dirty refusal lists the files.
    std::fs::write(wt4.join("wip.txt"), "x").unwrap();
    let (ok, err) = cli(&["issue", "finish", "D-4"]);
    assert!(
        !ok && err["error"].as_str().unwrap().contains("wip.txt"),
        "{err}"
    );
    // Unmerged+unpushed refusal once committed.
    git(&wt4, &["add", "-A"]);
    git(&wt4, &["commit", "-qm", "wip"]);
    idle(&wt4);
    let (ok, err) = cli(&["issue", "finish", "D-4"]);
    assert!(
        !ok && err["error"].as_str().unwrap().contains("neither merged"),
        "{err}"
    );
    // Merged → finish succeeds, refs closed in one commit.
    git(&repo, &["merge", "-q", "cadence/d-4-four"]);
    let before = tracker_commits();
    let (ok, out) = cli(&["issue", "finish", "D-4"]);
    assert!(
        ok && out["finished"] == true
            && out["overrode"] == json!([])
            && out["merged_by"] == "ancestry",
        "{out}"
    );
    assert!(!wt4.exists());
    assert_eq!(tracker_commits(), before + 1);
    let issue = cli(&["issue", "show", "D-4", "--json"]).1;
    assert!(
        issue["refs"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["kind"] != "worktree" || r["closed"] == true),
        "{issue}"
    );
}

/// CAD-107: dispatch commits the `message` ref BEFORE the send — a
/// finish racing the dispatch sees the binding as soon as the ref
/// lands, and a send that then fails leaves only a stale ref, which
/// binds nothing.
#[test]
fn dispatch_records_ref_before_send() {
    let (_seeded, state) = seeded_state(
        &[
            ("pm", None, "fake", "worker"),
            ("w1", Some("{\"upstream\":\"pm\"}"), "inbox", "worker"),
        ],
        |_, _| {},
    );
    // The daemon reads the tracker itself on `dispatch_send` (claim
    // check + lane resolution) — bind the pm dir before it starts; a
    // test daemon never falls back to ~/pm.
    let (tmp, pm_dir, repo, home) = pm_lab_dirs();
    test_env().set("CADENCE_PM_DIR", pm_dir.to_str().unwrap());
    let d = TestDaemon::start_on(state);

    let git = |dir: &Path, args: &[&str]| {
        let o = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(o.status.success());
    };
    git_f_repo(&repo, &git, |_| {});
    let cli = cadence_cli_json(&d.state, &pm_dir, &home);
    let repo_s = repo.canonicalize().unwrap().to_str().unwrap().to_string();
    demo_project_init(&cli, &repo_s);
    assert!(cli(&["issue", "new", "Reffirst", "--project", "demo"]).0);
    let note = tmp.path().join("kickoff.md");
    std::fs::write(&note, "# kickoff").unwrap();
    let note_s = note.canonicalize().unwrap().to_str().unwrap().to_string();

    // --reply-to names an agent that does not exist — the daemon's
    // enqueue rejects it, so the send fails AFTER the ref commits.
    let (ok, err) = cli(&[
        "dispatch",
        "D-1",
        "--to",
        "w1",
        "--note",
        &note_s,
        "--reply-to",
        "ghost",
    ]);
    assert!(!ok, "{err}");

    // The binding landed anyway — before the send — and the failed
    // send closed it: kept as history, never a live binding.
    let issue = cli(&["issue", "show", "D-1", "--json"]).1;
    let mref = issue["refs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["kind"] == "message")
        .expect("the message ref must be recorded before the send");
    let mid = mref["path"].as_str().unwrap().to_string();
    assert!(!mid.is_empty());
    assert_eq!(
        mref["closed"], true,
        "a failed send closes its orphan ref: {issue}"
    );
    // A second dispatch is not a duplicate — the closed ref is not a
    // live kickoff, so the retry sends fresh rather than refusing.
    let (ok, out) = cli(&[
        "dispatch",
        "D-1",
        "--to",
        "w1",
        "--note",
        &note_s,
        "--reply-to",
        "pm",
    ]);
    assert!(
        ok && out["dispatched"] == true,
        "the closed orphan must not read as an in-flight dispatch: {out}"
    );

    // A stale ref binds nothing: the forced finish still works and
    // the dead ref never becomes a bound-message block.
    let (ok, out) = cli(&["issue", "finish", "D-1", "--force"]);
    assert!(ok && out["finished"] == true, "{out}");
    assert!(
        !out["overrode"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o == "bound-message"),
        "a ref for a message that was never sent must not block: {out}"
    );

    // The daemon-side trigger the second-ref guard exists for: a
    // same-revision `task_dispatch` retry ignores the caller-minted id
    // and returns the still-live kickoff's. `issue dispatch --job`
    // always mints a fresh `<job>-t1` (a new job per `issue start`),
    // so this path can't be driven through the CLI — prove the rpc
    // contract directly so the guard's premise stays honest.
    let created = d
        .rpc(
            "job_new",
            json!({"pm": "pm", "spec": "s", "spec_sha256": "x",
                   "title": "t", "task_assignee": "w1"}),
        )
        .unwrap();
    let task = format!("{}-t1", created["job"]["id"].as_str().unwrap());
    let first = d
        .fixture_rpc(
            "task_dispatch",
            json!({"task": task, "message": "mint-one", "by": "pm"}),
        )
        .unwrap();
    assert_eq!(first["message"], "mint-one");
    let second = d
        .fixture_rpc(
            "task_dispatch",
            json!({"task": task, "message": "mint-two", "by": "pm"}),
        )
        .unwrap();
    assert_eq!(
        second["message"], "mint-one",
        "a retry returns the live kickoff id, not the minted one"
    );
}

/// CAD-467: dispatching to a freshly joined worker folds its still-queued
/// `bootstrap-<alias>` into the kickoff — the queued onboarding turn is
/// cancelled and its body rides ahead of the kickoff text, so the lane's
/// first turn is the task and still teaches identity and the report
/// command. A bootstrap already `running` keeps its turn — the kickoff
/// queues behind it, unfolded — and a kickoff this worker already
/// REPORTED in this lane is the late duplicate: recorded on the issue,
/// never re-sent, with the report's sha measured against the lane head.
/// A completed kickoff bound to a different worktree is another lane's
/// history and never suppresses the new dispatch.
#[test]
fn dispatch_folds_bootstrap_and_suppresses_reported_duplicate() {
    // Inbox endpoints: no actor drains them, so queued rows stay
    // put for the assertions.
    let (_seeded, state) = seeded_state(
        &[
            ("pm", None, "fake", "worker"),
            ("w1", Some("{\"upstream\":\"pm\"}"), "inbox", "worker"),
            ("w2", Some("{\"upstream\":\"pm\"}"), "inbox", "worker"),
            ("w3", Some("{\"upstream\":\"pm\"}"), "inbox", "worker"),
            ("w4", Some("{\"upstream\":\"pm\"}"), "inbox", "worker"),
            ("w5", Some("{\"upstream\":\"pm\"}"), "inbox", "worker"),
        ],
        |_, _| {},
    );
    // The daemon reads the tracker itself on `dispatch_send` (claim
    // check + lane resolution) — bind the pm dir before it starts; a
    // test daemon never falls back to ~/pm.
    let (tmp, pm_dir, repo, home) = pm_lab_dirs();
    test_env().set("CADENCE_PM_DIR", pm_dir.to_str().unwrap());
    let d = TestDaemon::start_on(state);

    let git = git_stdout();
    git_f_repo(&repo, &git, |_| {});
    let cli = cadence_cli_json(&d.state, &pm_dir, &home);
    assert!(cli(&["issue", "init"]).0);
    git(&pm_dir, &["config", "user.email", "t@t"]);
    git(&pm_dir, &["config", "user.name", "t"]);
    let repo_s = repo.canonicalize().unwrap().to_str().unwrap().to_string();
    assert!(cli(&["issue", "project", "add", "demo", "--prefix", "D", "--repo", &repo_s,]).0);
    demo_issue_news(
        &cli,
        &[
            "Fold",
            "Reported",
            "Runningboot",
            "Bigboot",
            "Forge",
            "Forgefields",
            "Sendfail",
            "Reffail",
        ],
    );
    let note = tmp.path().join("kickoff.md");
    std::fs::write(&note, "# kickoff").unwrap();
    let note_s = note.canonicalize().unwrap().to_str().unwrap().to_string();
    let dispatch = |issue: &str, to: &str| -> (bool, Value) {
        cli(&[
            "dispatch",
            issue,
            "--to",
            to,
            "--note",
            &note_s,
            "--reply-to",
            "pm",
        ])
    };

    // The freshly joined worker's bootstrap, still queued — seeded the
    // way `join` writes it (deterministic id, bootstrap source).
    d.send(
        "w1",
        json!({"text": "bootstrap body", "message": "bootstrap-w1", "source": "bootstrap"}),
    )
    .unwrap();
    assert_eq!(d.message_state("w1", "bootstrap-w1"), "queued");

    // The dispatch folds it: cancelled, its body ahead of the kickoff's.
    let (ok, out) = dispatch("D-1", "w1");
    assert!(ok, "{out}");
    assert_eq!(out["dispatched"], true, "{out}");
    assert_eq!(out["bootstrap"], "folded", "{out}");
    let mid = out["message"].as_str().unwrap().to_string();
    let show = d.rpc("agent_show", json!({"alias": "w1"})).unwrap();
    let msgs = show["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 2, "{msgs:?}");
    let boot = msgs.iter().find(|m| m["id"] == "bootstrap-w1").unwrap();
    assert_eq!(boot["state"], "cancelled", "{boot}");
    assert_eq!(boot["result"]["via"], "message_cancel", "{boot}");
    assert!(
        boot["result"]["reason"]
            .as_str()
            .unwrap_or_default()
            .contains(&format!("folded into kickoff {mid}")),
        "{boot}"
    );
    let kickoff = msgs
        .iter()
        .find(|m| m["id"].as_str() == Some(&mid))
        .unwrap();
    assert_eq!(kickoff["state"], "queued", "{kickoff}");
    let body = kickoff["body"].as_str().unwrap();
    assert!(
        body.starts_with(&format!("bootstrap body read {note_s} — D-1:")),
        "the bootstrap body rides ahead of the kickoff: {body}"
    );
    assert!(
        body.contains("cadence message result <id> --token <turn_id>"),
        "the kickoff carries the report command: {body}"
    );
    let issue = cli(&["issue", "show", "D-1", "--json"]).1;
    assert!(
        issue["comments"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["body"]
                .as_str()
                .unwrap_or_default()
                .contains("folded into this kickoff")),
        "{issue}"
    );

    // A bootstrap already running keeps its turn — nothing is
    // cancelled; the kickoff queues behind it unfolded.
    {
        let conn = rusqlite::Connection::open(d.state.join("cadence.sqlite3")).unwrap();
        conn.execute(
            "INSERT INTO messages(id,alias,body,reply_to,source,state,turn_id,created,started)
             VALUES('bootstrap-w2','w2','boot2',NULL,'bootstrap','running','t-1',0,0)",
            [],
        )
        .unwrap();
    }
    let (ok, out) = dispatch("D-2", "w2");
    assert!(ok, "{out}");
    assert_eq!(out["dispatched"], true, "{out}");
    assert_eq!(out["bootstrap"], "running", "{out}");
    let show = d.rpc("agent_show", json!({"alias": "w2"})).unwrap();
    let msgs = show["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 2, "{msgs:?}");
    assert_eq!(
        msgs.iter().find(|m| m["id"] == "bootstrap-w2").unwrap()["state"],
        "running"
    );
    assert_eq!(
        d.message_state("w2", out["message"].as_str().unwrap()),
        "queued"
    );

    // The late duplicate: D-3's kickoff, reported by w1 in this lane.
    let (ok, out) = dispatch("D-3", "w1");
    assert!(ok, "{out}");
    assert_eq!(out["dispatched"], true, "{out}");
    let kick_id = out["message"].as_str().unwrap().to_string();
    let wt = out["worktree"].as_str().unwrap().to_string();
    let head = git(Path::new(&wt), &["rev-parse", "HEAD"]);
    {
        let conn = rusqlite::Connection::open(d.state.join("cadence.sqlite3")).unwrap();
        conn.execute(
            "UPDATE messages SET state='completed', completed=1,
                 result=?1 WHERE id=?2",
            rusqlite::params![
                json!({"status": "completed", "via": "pty_report",
                       "sha": head, "turn_id": "t-9"})
                .to_string(),
                kick_id
            ],
        )
        .unwrap();
    }
    let (ok, out) = dispatch("D-3", "w1");
    assert!(ok, "{out}");
    assert_eq!(out["dispatched"], false, "{out}");
    assert_eq!(out["duplicate"], true, "{out}");
    assert_eq!(out["duplicate_kind"], "reported", "{out}");
    assert_eq!(out["message"], kick_id.as_str(), "{out}");
    assert_eq!(out["reported"]["via"], "pty_report", "{out}");
    assert_eq!(out["reported"]["sha"], head.as_str(), "{out}");
    assert_eq!(out["reported"]["lane_head"], head.as_str(), "{out}");
    assert_eq!(out["reported"]["same_head"], true, "{out}");
    // Nothing new reached the worker; the suppression is on the issue.
    let show = d.rpc("agent_show", json!({"alias": "w1"})).unwrap();
    assert_eq!(show["messages"].as_array().unwrap().len(), 3, "{show}");
    let issue = cli(&["issue", "show", "D-3", "--json"]).1;
    assert!(
        issue["comments"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["body"]
                .as_str()
                .unwrap_or_default()
                .contains("Late duplicate dispatch suppressed")),
        "{issue}"
    );

    // A completed kickoff the daemon recorded against ANOTHER worktree
    // is that lane's history: the re-dispatch sends fresh. (The proof
    // is the message row, so the row is what moves.)
    {
        let conn = rusqlite::Connection::open(d.state.join("cadence.sqlite3")).unwrap();
        conn.execute(
            "UPDATE messages SET worktree='/elsewhere' WHERE id=?1",
            rusqlite::params![kick_id],
        )
        .unwrap();
    }
    let (ok, out) = dispatch("D-3", "w1");
    assert!(ok, "{out}");
    assert_eq!(out["dispatched"], true, "{out}");
    assert!(
        out["duplicate"].is_null() || out["duplicate"] == false,
        "{out}"
    );
    let show = d.rpc("agent_show", json!({"alias": "w1"})).unwrap();
    assert_eq!(show["messages"].as_array().unwrap().len(), 4, "{show}");
    // The fresh kickoff carries the daemon-recorded lane provenance
    // the reported check trusts — the issue and this lane's worktree.
    let new_kick = show["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"].as_str() == out["message"].as_str())
        .unwrap();
    assert_eq!(new_kick["issue"], "D-3", "{new_kick}");
    assert_eq!(
        new_kick["worktree"].as_str(),
        Some(wt.as_str()),
        "{new_kick}"
    );

    // The reviewer's forgery: a `message` ref planted on ANOTHER issue
    // pointing at D-3's completed kickoff must never suppress — the
    // row says D-3, not D-5. `issue ref` writes exactly the shape a
    // frontmatter edit would.
    assert!(cli(&["issue", "ref", "D-5", "message", &kick_id]).0);
    let (ok, out) = dispatch("D-5", "w1");
    assert!(ok, "{out}");
    assert_eq!(
        out["dispatched"], true,
        "a planted ref to another issue's kickoff must not suppress: {out}"
    );

    // Same forgery with every ref field filled — `agent` and
    // `worktree` on the ref match the target and lane perfectly — but
    // the row it points at is a plain message with no recorded lane:
    // absent provenance never matches.
    let (ok, started) = cli(&["issue", "start", "D-6", "--owner", "w1", "--by", "pm"]);
    assert!(ok, "{started}");
    let wt6 = started["worktree"].as_str().unwrap().to_string();
    d.send("w1", json!({"text": "ordinary ask", "message": "m-plain"}))
        .unwrap();
    {
        let conn = rusqlite::Connection::open(d.state.join("cadence.sqlite3")).unwrap();
        conn.execute(
            "UPDATE messages SET state='completed', completed=1,
                 result='{\"status\":\"completed\",\"via\":\"pty_report\"}'
             WHERE id='m-plain'",
            [],
        )
        .unwrap();
    }
    assert!(cli(&["issue", "ref", "D-6", "message", "m-plain"]).0);
    // Forge the ref's lane fields too — the check never reads them.
    let issue_file = pm_dir.join("demo").join("D-6").join("issue.md");
    let text = std::fs::read_to_string(&issue_file).unwrap();
    let marked = text.replacen(
        "path: m-plain",
        &format!("path: m-plain\n  worktree: {wt6}\n  agent: w1"),
        1,
    );
    assert_ne!(marked, text, "the planted ref must be in the file");
    std::fs::write(&issue_file, marked).unwrap();
    git(&pm_dir, &["add", "-A"]);
    git(&pm_dir, &["commit", "-qm", "forged ref fields"]);
    let (ok, out) = dispatch("D-6", "w1");
    assert!(ok, "{out}");
    assert_eq!(
        out["dispatched"], true,
        "forged ref fields on a lane-less row must not suppress: {out}"
    );

    // Fold failure paths: a send that fails leaves the fold-intended
    // bootstrap queued and says so on the issue.
    d.send(
        "w4",
        json!({"text": "boot4", "message": "bootstrap-w4", "source": "bootstrap"}),
    )
    .unwrap();
    let (ok, err) = cli(&[
        "dispatch",
        "D-7",
        "--to",
        "w4",
        "--note",
        &note_s,
        "--reply-to",
        "ghost",
    ]);
    assert!(!ok, "{err}");
    assert_eq!(
        d.message_state("w4", "bootstrap-w4"),
        "queued",
        "a failed send must not strand the bootstrap"
    );
    let issue = cli(&["issue", "show", "D-7", "--json"]).1;
    assert!(
        issue["comments"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["body"]
                .as_str()
                .unwrap_or_default()
                .contains("bootstrap-w4 was left queued")),
        "{issue}"
    );
    // The retry folds it normally — nothing was consumed by the
    // failed attempt.
    let (ok, out) = dispatch("D-7", "w4");
    assert!(ok, "{out}");
    assert_eq!(out["bootstrap"], "folded", "{out}");
    assert_eq!(d.message_state("w4", "bootstrap-w4"), "cancelled");

    // A ref-write failure fails the dispatch before the send and
    // before any cancel: the bootstrap stays queued. The issue is
    // pre-started identically (same owner, same claim holder) so the
    // lock only bites at the dispatch's own ref write.
    d.send(
        "w5",
        json!({"text": "boot5", "message": "bootstrap-w5", "source": "bootstrap"}),
    )
    .unwrap();
    let (ok, started) = cli(&["issue", "start", "D-8", "--owner", "w5", "--by", "pm"]);
    assert!(ok, "{started}");
    std::fs::write(pm_dir.join(".write.lock"), "held").unwrap();
    let (ok, err) = dispatch("D-8", "w5");
    std::fs::remove_file(pm_dir.join(".write.lock")).unwrap();
    assert!(!ok, "{err}");
    assert_eq!(
        d.message_state("w5", "bootstrap-w5"),
        "queued",
        "a failed ref write must not strand the bootstrap"
    );
    let show = d.rpc("agent_show", json!({"alias": "w5"})).unwrap();
    assert_eq!(
        show["messages"].as_array().unwrap().len(),
        1,
        "no kickoff row may exist after the failed ref write: {show}"
    );
    // And the recovered retry folds + sends.
    let (ok, out) = dispatch("D-8", "w5");
    assert!(ok, "{out}");
    assert_eq!(out["bootstrap"], "folded", "{out}");
    assert_eq!(d.message_state("w5", "bootstrap-w5"), "cancelled");

    // A bootstrap too large to fold leaves the fold alone: still
    // queued, the kickoff sends unfolded behind it.
    let big = "b".repeat(3900);
    d.send(
        "w3",
        json!({"text": big, "message": "bootstrap-w3", "source": "bootstrap"}),
    )
    .unwrap();
    let (ok, out) = dispatch("D-4", "w3");
    assert!(ok, "{out}");
    assert_eq!(out["dispatched"], true, "{out}");
    assert_eq!(out["bootstrap"], "queued", "{out}");
    let show = d.rpc("agent_show", json!({"alias": "w3"})).unwrap();
    let msgs = show["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 2, "{msgs:?}");
    assert_eq!(
        msgs.iter().find(|m| m["id"] == "bootstrap-w3").unwrap()["state"],
        "queued",
        "the oversized bootstrap keeps its own turn"
    );
    let kickoff = msgs
        .iter()
        .find(|m| m["id"].as_str() == out["message"].as_str())
        .unwrap();
    assert_eq!(kickoff["state"], "queued", "{kickoff}");
    assert!(
        !kickoff["body"].as_str().unwrap().starts_with('b'),
        "the kickoff went out unfolded: {}",
        kickoff["body"]
    );
}

/// CAD-94: the finish guard is per worktree, not per agent. An owner
/// busy on worktree A does not block finishing the same owner's
/// merged worktree B; a process with cwd inside B is refused naming
/// the pid; a live message recorded against B is refused naming the
/// message id.
#[test]
fn finish_guard_per_worktree() {
    let (_seeded, state) = seeded_state(
        &[
            ("pm", None, "fake", "worker"),
            ("w1", Some("{\"upstream\":\"pm\"}"), "inbox", "worker"),
        ],
        |store, cwd| {
            // A fenced devin/pty agent that was never launched: endpoint
            // none + state attention reads `dead: true` (a `starting`
            // agent races the daemon's failed-launch → `stopped` parking,
            // which would read alive). Its queue can never start — a
            // queued message bound to its worktree must not block finish.
            store
                .register_agent(&NewAgent {
                    alias: "deadpty",
                    provider: "devin",
                    endpoint_kind: "pty",
                    role: "worker",
                    cwd,
                    sandbox: "read-only",
                    instructions: None,
                    params: Some("{\"upstream\":\"pm\"}"),
                    team_role: None,
                    model_policy: None,
                })
                .unwrap();
            store
                .set_agent_state("deadpty", "attention", Some("never launched"))
                .unwrap();
            store
                .enqueue("deadpty", "queued forever", None, "mkdead", "test")
                .unwrap();
        },
    );
    // The daemon reads the tracker itself on `dispatch_send` (claim
    // check + lane resolution) — bind the pm dir before it starts; a
    // test daemon never falls back to ~/pm.
    let (tmp, pm_dir, repo, home) = pm_lab_dirs();
    test_env().set("CADENCE_PM_DIR", pm_dir.to_str().unwrap());
    let d = TestDaemon::start_on(state);

    let git = git_ok();
    git_f_repo(&repo, &git, |_| {});
    let bin_dir = Path::new(env!("CARGO_BIN_EXE_cadence"))
        .parent()
        .unwrap()
        .to_path_buf();
    let cli = cadence_cli_json(&d.state, &pm_dir, &home);
    let repo_s = repo.canonicalize().unwrap().to_str().unwrap().to_string();
    demo_project_init(&cli, &repo_s);
    demo_issue_news(
        &cli,
        &[
            "Awt", "Bwt", "Cwt", "Dwt", "Ghost", "Scoped", "Inboxrun", "Nonowner",
        ],
    );
    let note = tmp.path().join("kickoff.md");
    std::fs::write(&note, "# kickoff").unwrap();
    let note_s = note.canonicalize().unwrap().to_str().unwrap().to_string();

    // w1 busy in worktree A: a queued kickoff recorded against D-1.
    let (ok, out) = cli(&[
        "dispatch",
        "D-1",
        "--to",
        "w1",
        "--note",
        &note_s,
        "--reply-to",
        "pm",
    ]);
    assert!(ok && out["dispatched"] == true, "{out}");
    let msg_a = out["message"].as_str().unwrap().to_string();

    // Same owner's merged worktree B finishes without --force: the
    // dispatch's message ref carries worktree A, so it doesn't bind
    // this target at all (and a queued inbox message is backlog, not
    // live work).
    let (ok, _) = cli(&["issue", "start", "D-2", "--owner", "w1"]);
    assert!(ok);
    let wt_b = repo.join(".cadence/wt/d-2-bwt");
    std::fs::write(wt_b.join("b.txt"), "x").unwrap();
    git(&wt_b, &["add", "-A"]);
    git(&wt_b, &["commit", "-qm", "b work"]);
    idle(&wt_b);
    git(&repo, &["merge", "-q", "cadence/d-2-bwt"]);
    let (ok, out) = cli(&["issue", "finish", "D-2"]);
    assert!(
        ok && out["finished"] == true && out["overrode"] == json!([]),
        "busy elsewhere must not block: {out}"
    );
    assert!(!wt_b.exists());

    // A process with cwd inside worktree C refuses naming the pid.
    let (ok, _) = cli(&["issue", "start", "D-3", "--owner", "w1"]);
    assert!(ok);
    let wt_c = repo.join(".cadence/wt/d-3-cwt");
    let mut shell = std::process::Command::new("sleep")
        .arg("300")
        .current_dir(&wt_c)
        .spawn()
        .unwrap();
    let (ok, err) = cli(&["issue", "finish", "D-3"]);
    assert!(!ok, "{err}");
    let msg = err["error"].as_str().unwrap();
    assert!(
        msg.contains(&shell.id().to_string()) && msg.contains("cwd inside"),
        "proc refusal names the pid: {msg}"
    );
    shell.kill().unwrap();
    let _ = shell.wait();

    // A queued message blocks only on a LIVE non-inbox owner — give
    // C to a live devin pane and bind a queued message to it. The
    // pane runs in the project's main checkout, as a real lane does —
    // `dispatch` refuses a pty worker outside the project (CAD-202).
    let _mock = d.mock_devin();
    d.fixture_rpc(
        "agent_register",
        json!({"alias": "dv", "provider": "devin", "endpoint_kind": "pty",
               "cwd": repo_s}),
    )
    .unwrap();
    d.wait_agent("dv", "idle", 15);
    let (ok, _) = cli(&["issue", "set", "D-3", "owner=dv"]);
    assert!(ok);
    d.send("dv", json!({"text": "queued against C", "message": "mkc"}))
        .unwrap();
    let (ok, _) = cli(&["issue", "ref", "D-3", "message", "mkc"]);
    assert!(ok);
    let (ok, err) = cli(&["issue", "finish", "D-3"]);
    assert!(!ok, "{err}");
    let msg = err["error"].as_str().unwrap();
    assert!(
        msg.contains("mkc")
            && msg.contains("recorded against this worktree")
            && !msg.contains(&msg_a),
        "message refusal names the bound id: {msg}"
    );
    assert!(wt_c.is_dir());

    // A dead pty owner with a queued bound message does not block —
    // its queue can never start.
    let show = d.rpc("agent_show", json!({"alias": "deadpty"})).unwrap();
    assert_eq!(show["agent"]["dead"], true, "fixture must read dead");
    let (ok, _) = cli(&["issue", "start", "D-4", "--owner", "deadpty"]);
    assert!(ok);
    let wt_d = repo.join(".cadence/wt/d-4-dwt");
    std::fs::write(wt_d.join("d.txt"), "x").unwrap();
    git(&wt_d, &["add", "-A"]);
    git(&wt_d, &["commit", "-qm", "d work"]);
    idle(&wt_d);
    git(&repo, &["merge", "-q", "cadence/d-4-dwt"]);
    let (ok, _) = cli(&["issue", "ref", "D-4", "message", "mkdead"]);
    assert!(ok);
    let (ok, out) = cli(&["issue", "finish", "D-4"]);
    assert!(
        ok && out["finished"] == true && out["overrode"] == json!([]),
        "queued on a dead owner must not block: {out}"
    );
    assert!(!wt_d.exists());

    // An owner the daemon has never heard of cannot be using the
    // worktree — Rejected is an answer, not a transport failure.
    let (ok, _) = cli(&["issue", "start", "D-5"]);
    assert!(ok);
    let (ok, _) = cli(&["issue", "set", "D-5", "owner=ghost"]);
    assert!(ok);
    let wt_g = repo.join(".cadence/wt/d-5-ghost");
    std::fs::write(wt_g.join("g.txt"), "x").unwrap();
    git(&wt_g, &["add", "-A"]);
    git(&wt_g, &["commit", "-qm", "g work"]);
    idle(&wt_g);
    git(&repo, &["merge", "-q", "cadence/d-5-ghost"]);
    // The probe runs WITHOUT the pm lock: with the lock file held, a
    // stale-socket daemon (it was there and stopped answering) still
    // returns the unreachable refusal — it never waits on (or times
    // out against) the lock.
    std::fs::write(pm_dir.join(".write.lock"), "held").unwrap();
    let dead_state = tmp.path().join("deadstate");
    std::fs::create_dir_all(&dead_state).unwrap();
    std::fs::write(dead_state.join("cadence.sock"), "stale").unwrap();
    let cli_on = |state_dir: &Path, args: &[&str]| -> std::process::Output {
        std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
            .arg("--state-dir")
            .arg(state_dir)
            .args(args)
            .env("CADENCE_PM_DIR", &pm_dir)
            .env("HOME", &home)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    bin_dir.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env_remove("CADENCE_ALIAS")
            .output()
            .unwrap()
    };
    let out = cli_on(&dead_state, &["issue", "finish", "D-5"]);
    std::fs::remove_file(pm_dir.join(".write.lock")).unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!out.status.success());
    assert!(
        text.contains("unreachable") && !text.contains("locked"),
        "the probe must not wait on the pm lock: {text}"
    );
    // A cleanly stopped daemon removes its socket: the same finish on
    // a socket-less state dir is "no agents", and the /proc + pane
    // scans carry the check — no --force needed.
    std::fs::remove_file(dead_state.join("cadence.sock")).unwrap();
    let out = cli_on(&dead_state, &["issue", "finish", "D-5", "--json"]);
    let nod = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.status.success(),
        "no daemon at all must not block a clean merged finish: {nod}"
    );
    let out: Value = serde_json::from_str(String::from_utf8_lossy(&out.stdout).trim()).unwrap();
    assert!(
        out["finished"] == true && out["overrode"] == json!([]),
        "unknown owner must not block: {out}"
    );
    assert!(!wt_g.exists());
    // A second finish is the idempotent no-op.
    let (ok, out) = cli(&["issue", "finish", "D-5"]);
    assert!(ok && out["finished"] == false, "already finished: {out}");

    // D-6: a bound live message must be found on the agent that
    // HOLDS it, not only the current owner — re-assigning the issue
    // leaves the earlier dispatchee's message bound (CAD-107). And a
    // worktree-scoped message ref must not bind a re-started pair.
    let (ok, out) = cli(&[
        "dispatch",
        "D-6",
        "--to",
        "dv",
        "--note",
        &note_s,
        "--reply-to",
        "pm",
    ]);
    assert!(ok && out["dispatched"] == true, "{out}");
    let msg_d6 = out["message"].as_str().unwrap().to_string();
    let (ok, err) = cli(&["issue", "finish", "D-6"]);
    assert!(!ok, "{err}");
    let msg = err["error"].as_str().unwrap();
    assert!(msg.contains(&msg_d6), "bound refusal names the id: {msg}");
    // owner=w1 now, but the bound message lives on dv — it must
    // still block.
    let (ok, _) = cli(&["issue", "set", "D-6", "owner=w1"]);
    assert!(ok);
    let (ok, err) = cli(&["issue", "finish", "D-6"]);
    assert!(!ok, "{err}");
    assert!(
        err["error"].as_str().unwrap().contains(&msg_d6),
        "a non-owner recipient's bound message still blocks: {err}"
    );
    // Re-start under a new --name while the old pair is open would
    // fork the issue's work — refused, naming the open lane (CAD-274).
    // Once the old pair is force-finished, the re-started pair is not
    // bound to a message scoped to the old worktree. The re-starts run
    // as `pm`, which dispatched D-6 and holds its claim (CAD-383).
    let (ok, err) = cli(&["issue", "start", "D-6", "--name", "scd", "--by", "pm"]);
    assert!(
        !ok && err["error"].as_str().unwrap().contains("d-6-"),
        "a second lane under --name is refused: {err}"
    );
    let (ok, out) = cli(&["issue", "finish", "D-6", "--force"]);
    assert!(
        ok && out["overrode"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o == "bound-message"),
        "the bound-message block is what --force overrode: {out}"
    );
    let (ok, out) = cli(&["issue", "start", "D-6", "--name", "scd", "--by", "pm"]);
    assert!(ok, "{out}");
    // The new pair's work is merged by ancestry and the still-live
    // message is scoped to the removed pair — the finish succeeds
    // clean.
    let wt_scd = repo.join(".cadence/wt/d-6-scd");
    std::fs::write(wt_scd.join("scd.txt"), "x").unwrap();
    git(&wt_scd, &["add", "-A"]);
    git(&wt_scd, &["commit", "-qm", "scd work"]);
    idle(&wt_scd);
    git(&repo, &["merge", "-q", "cadence/d-6-scd"]);
    let (ok, out) = cli(&["issue", "finish", "D-6"]);
    assert!(
        ok && out["finished"] == true && out["overrode"] == json!([]),
        "a ref scoped to the old worktree must not bind the new one: {out}"
    );
    assert!(!wt_scd.exists());

    // D-7: `running` is live even on an inbox endpoint — the durable
    // backlog exemption covers only `queued`. The ref is unscoped
    // (`issue ref` writes no worktree) and still binds.
    let (ok, _) = cli(&["issue", "start", "D-7", "--owner", "w1"]);
    assert!(ok);
    {
        let store = Store::open(&d.state.join("cadence.sqlite3")).unwrap();
        store
            .enqueue("w1", "running on inbox", None, "mir", "test")
            .unwrap();
        store.mark_running("mir", "turn-mir").unwrap();
    }
    let (ok, _) = cli(&["issue", "ref", "D-7", "message", "mir"]);
    assert!(ok);
    let (ok, err) = cli(&["issue", "finish", "D-7"]);
    assert!(!ok, "{err}");
    let msg = err["error"].as_str().unwrap();
    assert!(
        msg.contains("mir") && msg.contains("running"),
        "a running message on an inbox owner must block: {msg}"
    );

    // D-8: the running-on-inbox rule discriminates by recipient, not
    // owner — w1 (inbox) holds the bound running message while D-8 is
    // owned by dv, and it still blocks (D-7 kept as the owner case).
    let (ok, out) = cli(&[
        "dispatch",
        "D-8",
        "--to",
        "w1",
        "--note",
        &note_s,
        "--reply-to",
        "pm",
    ]);
    assert!(ok && out["dispatched"] == true, "{out}");
    let msg_d8 = out["message"].as_str().unwrap().to_string();
    {
        let store = Store::open(&d.state.join("cadence.sqlite3")).unwrap();
        store.mark_running(&msg_d8, "turn-d8").unwrap();
    }
    let (ok, _) = cli(&["issue", "set", "D-8", "owner=dv"]);
    assert!(ok);
    let (ok, err) = cli(&["issue", "finish", "D-8"]);
    assert!(!ok, "{err}");
    let msg = err["error"].as_str().unwrap();
    assert!(
        msg.contains(&msg_d8) && msg.contains("running"),
        "a non-owner recipient's running inbox message must block: {msg}"
    );
}

/// CAD-242: `issue finish` holds a present merged worktree while an
/// unreconciled `unknown` still refers to it — bound to the worktree,
/// or sitting on an agent whose cwd is that directory (a child counts;
/// a sibling prefix does not). A reconciled lane with a stale fence
/// error still finishes. `--force` records `unreconciled-unknown`; the
/// sweep cannot force.
#[test]
fn finish_holds_unreconciled_unknown() {
    let (_seeded, state) = seeded_state(&[], |_, _| {});
    let d = TestDaemon::start_on(state);
    let (tmp, pm_dir, repo, home) = pm_lab_dirs();

    let git = git_ok();
    git_f_repo(&repo, &git, |_| {});
    let cli_raw = cadence_cli_raw(&d.state, &pm_dir, &home);
    let cli = |args: &[&str]| -> (bool, Value) {
        let (code, stdout, stderr) = cli_raw(args);
        let text = if stdout.is_empty() { stderr } else { stdout };
        (
            code == 0,
            serde_json::from_str(text.trim()).unwrap_or_else(|_| panic!("not json: {text}")),
        )
    };
    let repo_s = repo.canonicalize().unwrap().to_str().unwrap().to_string();
    demo_project_init(&cli, &repo_s);
    demo_issue_news(
        &cli,
        &["Reconciled", "Samecwd", "Bound", "Elsewhere", "Child"],
    );
    for id in ["D-1", "D-2", "D-3", "D-4", "D-5"] {
        let (ok, out) = cli(&["issue", "start", id]);
        assert!(ok, "{out}");
        let (ok, _) = cli(&["issue", "set", id, "owner="]);
        assert!(ok);
    }
    let (ok, _) = cli(&["issue", "set", "D-3", "owner=bound"]);
    assert!(ok);
    let (ok, out) = cli(&["issue", "ref", "D-3", "message", "m-bound"]);
    assert!(ok, "{out}");

    let wt = |slug: &str| repo.join(format!(".cadence/wt/{slug}"));
    for slug in [
        "d-1-reconciled",
        "d-2-samecwd",
        "d-3-bound",
        "d-4-elsewhere",
        "d-5-child",
    ] {
        std::fs::write(wt(slug).join(format!("{slug}.txt")), "x").unwrap();
        git(&wt(slug), &["add", "-A"]);
        git(&wt(slug), &["commit", "-qm", slug]);
        idle(&wt(slug));
        git(&repo, &["merge", "-q", &format!("cadence/{slug}")]);
    }
    let child_cwd = wt("d-5-child").join("nested");
    std::fs::create_dir_all(&child_cwd).unwrap();
    // A sibling whose name merely extends the worktree's prefix must
    // not count as cwd-on-worktree.
    let sibling = repo.join(".cadence/wt/d-4-elsewhere-extra");
    std::fs::create_dir_all(&sibling).unwrap();
    let elsewhere = tmp.path().join("other");
    std::fs::create_dir_all(&elsewhere).unwrap();

    let body = "secret-body-should-not-leak";
    {
        let store = Store::open(&d.state.join("cadence.sqlite3")).unwrap();
        let reg = |alias: &str, cwd: &Path| {
            store
                .register_agent(&NewAgent {
                    alias,
                    provider: "fake",
                    endpoint_kind: "inbox",
                    role: "worker",
                    cwd: cwd.to_str().unwrap(),
                    sandbox: "read-only",
                    instructions: None,
                    params: None,
                    team_role: None,
                    model_policy: None,
                })
                .unwrap();
            store.set_enabled(alias, false).unwrap();
        };
        reg("settled", &wt("d-1-reconciled"));
        reg("holder", &wt("d-2-samecwd"));
        reg("bound", &elsewhere);
        reg("stray", &sibling);
        reg("child", &child_cwd);
        let fence = |alias: &str, id: &str| {
            store.enqueue(alias, body, None, id, "test").unwrap();
            store.mark_running(id, &format!("turn-{id}")).unwrap();
            let message = store.message(id).unwrap().unwrap();
            store
                .finish(
                    &message,
                    "unknown",
                    &json!({
                        "status": "unknown",
                        "text": "",
                        "error": "Uncertain provider outcome"
                    }),
                    Some("Uncertain provider outcome"),
                )
                .unwrap();
            store
                .set_agent_state(alias, "attention", Some("Uncertain provider outcome"))
                .unwrap();
        };
        fence("settled", "m-settled");
        fence("holder", "m-holder");
        fence("bound", "m-bound");
        fence("stray", "m-stray");
        fence("child", "m-child");
        store
            .reconcile(
                "m-settled",
                "interrupted",
                Some("outcome was a no-op"),
                "operator",
                None,
            )
            .unwrap();
    }

    let settled = d.rpc("agent_show", json!({"alias": "settled"})).unwrap();
    assert_eq!(settled["agent"]["state"], "stopped", "{settled}");
    assert!(
        settled["agent"]["error"]
            .as_str()
            .unwrap_or_default()
            .contains("Uncertain provider outcome"),
        "a reconciled fence must keep its stale error: {settled}"
    );
    assert_eq!(settled["messages"][0]["state"], "interrupted", "{settled}");
    let holder = d.rpc("agent_show", json!({"alias": "holder"})).unwrap();
    assert_eq!(holder["messages"][0]["state"], "unknown", "{holder}");

    let (code, stdout, _) = cli_raw(&["issue", "finish", "--merged", "--dry-run", "--json"]);
    assert_eq!(code, 1, "{stdout}");
    let plan: Value = serde_json::from_str(stdout.trim()).unwrap();
    let rows = plan["rows"].as_array().unwrap();
    let outcome = |id: &str| -> (String, String) {
        rows.iter()
            .find(|r| r["issue"] == id)
            .map(|r| {
                (
                    r["outcome"].as_str().unwrap().to_string(),
                    r["reason"].as_str().unwrap_or_default().to_string(),
                )
            })
            .unwrap_or_else(|| panic!("missing {id}: {plan}"))
    };
    assert_eq!(outcome("D-1").0, "would-finish", "{plan}");
    for (id, alias, mid) in [
        ("D-2", "holder", "m-holder"),
        ("D-3", "bound", "m-bound"),
        ("D-5", "child", "m-child"),
    ] {
        let (o, reason) = outcome(id);
        assert_eq!(o, "refused", "{id} {plan}");
        assert!(
            reason.contains(alias)
                && reason.contains(mid)
                && reason.contains("unreconciled")
                && !reason.contains(body),
            "{id} reason must name the alias and the unreconciled unknown, not the body: {reason}"
        );
    }
    assert_eq!(outcome("D-4").0, "would-finish", "{plan}");
    for slug in [
        "d-1-reconciled",
        "d-2-samecwd",
        "d-3-bound",
        "d-4-elsewhere",
        "d-5-child",
    ] {
        assert!(wt(slug).is_dir(), "{slug} must survive dry-run");
    }

    let (code, stdout, _) = cli_raw(&["issue", "finish", "--merged", "--json"]);
    assert_eq!(code, 1, "{stdout}");
    let swept: Value = serde_json::from_str(stdout.trim()).unwrap();
    let rows = swept["rows"].as_array().unwrap();
    let swept_outcome = |id: &str| {
        rows.iter()
            .find(|r| r["issue"] == id)
            .map(|r| r["outcome"].as_str().unwrap().to_string())
            .unwrap_or_else(|| panic!("missing {id}: {swept}"))
    };
    assert_eq!(swept_outcome("D-1"), "finished", "{swept}");
    assert_eq!(swept_outcome("D-4"), "finished", "{swept}");
    assert_eq!(swept_outcome("D-2"), "refused", "{swept}");
    assert_eq!(swept_outcome("D-3"), "refused", "{swept}");
    assert_eq!(swept_outcome("D-5"), "refused", "{swept}");
    assert!(!wt("d-1-reconciled").exists());
    assert!(!wt("d-4-elsewhere").exists());
    assert!(wt("d-2-samecwd").is_dir() && wt("d-3-bound").is_dir() && wt("d-5-child").is_dir());
    for id in ["D-2", "D-3", "D-5"] {
        let issue = cli(&["issue", "show", id, "--json"]).1;
        let open = issue["refs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["kind"] == "worktree" && r["closed"] != true);
        assert!(open, "sweep must not close {id}'s worktree ref: {issue}");
    }

    let (ok, out) = cli(&["issue", "finish", "D-2", "--force"]);
    assert!(ok && out["finished"] == true, "{out}");
    assert!(
        out["overrode"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o == "unreconciled-unknown"),
        "{out}"
    );
    assert!(!wt("d-2-samecwd").exists());

    // The sweep still has no force flag: the remaining unknowns stay.
    let (code, stdout, _) = cli_raw(&["issue", "finish", "--merged", "--json"]);
    assert_eq!(code, 1, "{stdout}");
    let again: Value = serde_json::from_str(stdout.trim()).unwrap();
    let rows = again["rows"].as_array().unwrap();
    for id in ["D-3", "D-5"] {
        let row = rows.iter().find(|r| r["issue"] == id).unwrap();
        assert_eq!(row["outcome"], "refused", "{again}");
        assert!(
            row["reason"]
                .as_str()
                .unwrap_or_default()
                .contains("unreconciled"),
            "{again}"
        );
    }
    assert!(wt("d-3-bound").is_dir() && wt("d-5-child").is_dir());
}

/// CAD-93: `issue finish --merged` sweeps every open worktree ref
/// whose branch is merged and whose guard passes — one row per
/// worktree (finished | skipped | refused), exit 1 on any refusal,
/// and `--dry-run` changes nothing. Ownerless issues need no daemon.
#[test]
fn finish_merged_sweep() {
    let tmp = TempDir::new().unwrap();
    let (pm_dir, repo, home, state) = (
        tmp.path().join("pm"),
        tmp.path().join("repo"),
        tmp.path().join("home"),
        tmp.path().join("state"),
    );
    for dir in [&pm_dir, &repo, &home, &state] {
        std::fs::create_dir_all(dir).unwrap();
    }
    // A live daemon: the bound-message enumeration must not fail
    // open — a daemon that cannot answer `agent_list` is itself a
    // refusal now, so the idle-path rows need one that answers.
    let _d = TestDaemon::start_on(state.clone());
    let git = git_ok();
    git_f_repo(&repo, &git, |_| {});
    let bin_dir = Path::new(env!("CARGO_BIN_EXE_cadence"))
        .parent()
        .unwrap()
        .to_path_buf();
    let cli_raw = cadence_cli_raw(&state, &pm_dir, &home);
    let cli = |args: &[&str]| -> (bool, Value) {
        let (code, stdout, stderr) = cli_raw(args);
        let text = if stdout.is_empty() { stderr } else { stdout };
        (
            code == 0,
            serde_json::from_str(text.trim()).unwrap_or_else(|_| panic!("not json: {text}")),
        )
    };
    assert!(cli(&["issue", "init"]).0);
    let repo_s = repo.canonicalize().unwrap().to_str().unwrap().to_string();
    assert!(cli(&["issue", "project", "add", "demo", "--prefix", "D", "--repo", &repo_s,]).0);
    for title in ["Merged", "Inuse", "Unmerged", "Dirty", "Ghost"] {
        assert!(cli(&["issue", "new", title, "--project", "demo"]).0);
    }
    // All started ownerless — no daemon involvement anywhere; D-5
    // keeps an owner the daemon can't answer for.
    for id in ["D-1", "D-2", "D-3", "D-4", "D-5"] {
        let (ok, out) = cli(&["issue", "start", id]);
        assert!(ok, "{out}");
        let (ok, _) = cli(&["issue", "set", id, "owner="]);
        assert!(ok);
    }
    let (ok, _) = cli(&["issue", "set", "D-5", "owner=ghost"]);
    assert!(ok);
    let wt = |slug: &str| repo.join(format!(".cadence/wt/{slug}"));
    // D-1 merged+idle, D-2 merged+in-use, D-3 unmerged, D-4 merged+dirty,
    // D-5 merged with an owner check the daemon can't answer.
    // Each branch writes its own file — identical diffs would collapse
    // to one SHA and every branch would read `merged_by: ancestry`.
    for slug in [
        "d-1-merged",
        "d-2-inuse",
        "d-3-unmerged",
        "d-4-dirty",
        "d-5-ghost",
    ] {
        std::fs::write(wt(slug).join(format!("{slug}.txt")), "x").unwrap();
        git(&wt(slug), &["add", "-A"]);
        git(&wt(slug), &["commit", "-qm", "work"]);
        idle(&wt(slug));
    }
    git(&repo, &["merge", "-q", "cadence/d-1-merged"]);
    git(&repo, &["merge", "-q", "cadence/d-2-inuse"]);
    git(&repo, &["merge", "-q", "cadence/d-4-dirty"]);
    git(&repo, &["merge", "-q", "cadence/d-5-ghost"]);
    let mut shell = std::process::Command::new("sleep")
        .arg("300")
        .current_dir(wt("d-2-inuse"))
        .spawn()
        .unwrap();
    std::fs::write(wt("d-4-dirty").join("wip.txt"), "x").unwrap();

    // --dry-run: the plan, nothing changes, refusals mean exit 1.
    let commits_before = String::from_utf8_lossy(
        &std::process::Command::new("git")
            .arg("-C")
            .arg(&pm_dir)
            .args(["rev-list", "--count", "HEAD"])
            .output()
            .unwrap()
            .stdout,
    )
    .trim()
    .to_string();
    let (code, stdout, _) = cli_raw(&["issue", "finish", "--merged", "--dry-run", "--json"]);
    assert_eq!(code, 1, "{stdout}");
    let plan: Value = serde_json::from_str(stdout.trim()).unwrap();
    let rows = plan["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 5, "{plan}");
    let outcome = |id: &str| {
        rows.iter()
            .find(|r| r["issue"] == id)
            .map(|r| {
                (
                    r["outcome"].as_str().unwrap().to_string(),
                    r["reason"].as_str().unwrap_or_default().to_string(),
                )
            })
            .unwrap()
    };
    assert_eq!(outcome("D-1").0, "would-finish", "{plan}");
    let (o, r) = outcome("D-2");
    assert!(
        o == "refused" && r.contains(&shell.id().to_string()),
        "{plan}"
    );
    assert_eq!(
        outcome("D-3"),
        ("skipped".into(), "unmerged".into()),
        "{plan}"
    );
    let (o, r) = outcome("D-4");
    assert!(o == "refused" && r.contains("uncommitted"), "{plan}");
    // A ghost owner the daemon has never heard of is ABSENT, not
    // unreachable — the row would finish.
    assert_eq!(outcome("D-5").0, "would-finish", "{plan}");
    // …and the fail-open the round-2 review closed, now split by how
    // the daemon is absent. A STALE socket — a daemon that was there
    // and stopped answering — still refuses the enumeration: a bound
    // task could hide anywhere. No socket at all is a cleanly stopped
    // daemon: "no agents", and the /proc + pane scans carry the check.
    let dead_state = tmp.path().join("deadstate");
    std::fs::create_dir_all(&dead_state).unwrap();
    std::fs::write(dead_state.join("cadence.sock"), "stale").unwrap();
    let sweep_on = |state_dir: &Path| -> (i32, Value) {
        let dead = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
            .arg("--state-dir")
            .arg(state_dir)
            .args(["issue", "finish", "--merged", "--dry-run", "--json"])
            .env("CADENCE_PM_DIR", &pm_dir)
            .env("HOME", &home)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    bin_dir.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env_remove("CADENCE_ALIAS")
            .output()
            .unwrap();
        (
            dead.status.code().unwrap_or(-1),
            serde_json::from_str(String::from_utf8_lossy(&dead.stdout).trim()).unwrap_or_else(
                |_| {
                    panic!(
                        "not json: {}{}",
                        String::from_utf8_lossy(&dead.stdout),
                        String::from_utf8_lossy(&dead.stderr)
                    )
                },
            ),
        )
    };
    let (code, dead_plan) = sweep_on(&dead_state);
    assert_eq!(code, 1);
    let dead_rows = dead_plan["rows"].as_array().unwrap();
    for id in ["D-1", "D-5"] {
        let row = dead_rows.iter().find(|r| r["issue"] == id).unwrap();
        assert_eq!(row["outcome"], "refused", "{dead_plan}");
        assert!(
            row["reason"]
                .as_str()
                .unwrap_or_default()
                .contains("unreachable"),
            "a stale-socket daemon refuses the enumeration: {dead_plan}"
        );
    }
    // The same rows on a socket-less dir — the daemon is simply not
    // running, so nothing enumerates and nothing refuses for it.
    std::fs::remove_file(dead_state.join("cadence.sock")).unwrap();
    let (code, gone_plan) = sweep_on(&dead_state);
    assert_eq!(code, 1, "{gone_plan}"); // D-2/D-4 still refuse on their own
    let gone_rows = gone_plan["rows"].as_array().unwrap();
    for id in ["D-1", "D-5"] {
        let row = gone_rows.iter().find(|r| r["issue"] == id).unwrap();
        assert_eq!(
            row["outcome"], "would-finish",
            "no daemon at all means no agents — {id} is clean: {gone_plan}"
        );
    }
    // Nothing changed: dirs exist, refs open, tracker untouched.
    for slug in [
        "d-1-merged",
        "d-2-inuse",
        "d-3-unmerged",
        "d-4-dirty",
        "d-5-ghost",
    ] {
        assert!(wt(slug).is_dir(), "{slug} must survive dry-run");
    }
    let commits_after = String::from_utf8_lossy(
        &std::process::Command::new("git")
            .arg("-C")
            .arg(&pm_dir)
            .args(["rev-list", "--count", "HEAD"])
            .output()
            .unwrap()
            .stdout,
    )
    .trim()
    .to_string();
    assert_eq!(commits_before, commits_after, "dry-run must not commit");

    // Real sweep: D-1+D-5 finish, D-2/D-4 refuse, D-3 skips — exit 1.
    let (code, stdout, _) = cli_raw(&["issue", "finish", "--merged", "--json"]);
    assert_eq!(code, 1, "{stdout}");
    let out: Value = serde_json::from_str(stdout.trim()).unwrap();
    let rows = out["rows"].as_array().unwrap();
    let outcome = |id: &str| {
        rows.iter()
            .find(|r| r["issue"] == id)
            .map(|r| r["outcome"].as_str().unwrap().to_string())
            .unwrap()
    };
    assert_eq!(outcome("D-1"), "finished", "{out}");
    assert_eq!(outcome("D-2"), "refused", "{out}");
    assert_eq!(outcome("D-3"), "skipped", "{out}");
    assert_eq!(outcome("D-4"), "refused", "{out}");
    assert_eq!(outcome("D-5"), "finished", "{out}");
    assert!(!wt("d-1-merged").exists());
    assert!(wt("d-2-inuse").is_dir() && wt("d-3-unmerged").is_dir());
    assert!(wt("d-4-dirty").is_dir() && !wt("d-5-ghost").exists());

    // Clear the refusals and the second sweep exits 0 on skip-only.
    shell.kill().unwrap();
    let _ = shell.wait();
    std::fs::remove_file(wt("d-4-dirty").join("wip.txt")).unwrap();
    let (code, stdout, _) = cli_raw(&["issue", "finish", "--merged", "--json"]);
    assert_eq!(code, 0, "{stdout}");
    let out: Value = serde_json::from_str(stdout.trim()).unwrap();
    let rows = out["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 3, "{out}"); // D-1/D-5 finished → no row
    assert!(!wt("d-2-inuse").exists() && !wt("d-4-dirty").exists());
    assert!(wt("d-3-unmerged").is_dir());

    // The done-hint: status=done with an open worktree prints it.
    let (code, _, stderr) = cli_raw(&["issue", "set", "D-3", "status=done"]);
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stderr.contains("worktree open: run cadence issue finish D-3"),
        "{stderr}"
    );

    // CAD-106: a merged PR binds its recorded head COMMIT, not the
    // branch name. A github origin plus a `gh` stub answers per
    // branch: D-6's tip IS the recorded merge head (`merged_by: pr`);
    // D-7 reuses the name with an extra commit the recorded head does
    // not cover — the sweep must report it unmerged and leave the
    // branch alone.
    git(
        &repo,
        &["remote", "add", "origin", "https://github.com/x/y.git"],
    );
    for title in [
        "Prbound",
        "Prreused",
        "Prancestor",
        "Remoteok",
        "Remoteunm",
        "Remoteahead",
        "Remoteforce",
        "Remotestale",
    ] {
        assert!(cli(&["issue", "new", title, "--project", "demo"]).0);
    }
    // D-9..D-12 are started after the sweep — an untouched branch
    // reads merged-by-ancestry and the sweep would finish its
    // worktree out from under the --remote legs.
    for id in ["D-6", "D-7", "D-8"] {
        let (ok, out) = cli(&["issue", "start", id]);
        assert!(ok, "{out}");
        let (ok, _) = cli(&["issue", "set", id, "owner="]);
        assert!(ok);
    }
    let sha = |dir: &Path, rev: &str| -> String {
        let o = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["rev-parse", rev])
            .output()
            .unwrap();
        assert!(o.status.success());
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    };
    // D-6's single commit IS the merged PR head; D-7 records that
    // same position as its merged head, then adds an unmerged commit.
    std::fs::write(wt("d-6-prbound").join("pr6.txt"), "x").unwrap();
    git(&wt("d-6-prbound"), &["add", "-A"]);
    git(&wt("d-6-prbound"), &["commit", "-qm", "merged head"]);
    idle(&wt("d-6-prbound"));
    let tip6 = sha(&repo, "cadence/d-6-prbound");
    std::fs::write(wt("d-7-prreused").join("pr7.txt"), "x").unwrap();
    git(&wt("d-7-prreused"), &["add", "-A"]);
    git(&wt("d-7-prreused"), &["commit", "-qm", "merged head"]);
    idle(&wt("d-7-prreused"));
    let tip7a = sha(&repo, "cadence/d-7-prreused");
    std::fs::write(wt("d-7-prreused").join("extra.txt"), "x").unwrap();
    git(&wt("d-7-prreused"), &["add", "-A"]);
    git(&wt("d-7-prreused"), &["commit", "-qm", "unmerged extra"]);
    idle(&wt("d-7-prreused"));
    let tip7b = sha(&repo, "cadence/d-7-prreused");
    // D-8: the accepted ancestor path — the branch tip is an ancestor
    // of the recorded PR head (a local branch behind the merged head).
    // The head commit is made on a scratch branch so it exists in the
    // object store without moving the recorded branch.
    std::fs::write(wt("d-8-prancestor").join("pa.txt"), "x").unwrap();
    git(&wt("d-8-prancestor"), &["add", "-A"]);
    git(&wt("d-8-prancestor"), &["commit", "-qm", "work"]);
    idle(&wt("d-8-prancestor"));
    git(&wt("d-8-prancestor"), &["checkout", "-q", "-b", "scratch8"]);
    std::fs::write(wt("d-8-prancestor").join("more.txt"), "x").unwrap();
    git(&wt("d-8-prancestor"), &["add", "-A"]);
    git(&wt("d-8-prancestor"), &["commit", "-qm", "pr head"]);
    idle(&wt("d-8-prancestor"));
    let head8 = sha(&repo, "scratch8");
    git(
        &wt("d-8-prancestor"),
        &["checkout", "-q", "cadence/d-8-prancestor"],
    );
    let gh_bin = tmp.path().join("ghbin");
    std::fs::create_dir_all(&gh_bin).unwrap();
    std::fs::write(
        gh_bin.join("gh"),
        format!(
            "#!/bin/sh\nfor a in \"$@\"; do case \"$a\" in\n\
             cadence/d-6-prbound) printf '[{{\"number\":6,\"headRefOid\":\"{tip6}\",\"baseRefName\":\"main\"}}]'; exit 0;;\n\
             cadence/d-7-prreused) printf '[{{\"number\":7,\"headRefOid\":\"{tip7a}\",\"baseRefName\":\"main\"}}]'; exit 0;;\n\
             cadence/d-8-prancestor) printf '[{{\"number\":8,\"headRefOid\":\"{head8}\",\"baseRefName\":\"main\"}}]'; exit 0;;\n\
             esac; done\nprintf '[]'\n"
        ),
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(gh_bin.join("gh"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
    }
    let cli_gh = |args: &[&str]| -> (i32, String) {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
            .arg("--state-dir")
            .arg(&state)
            .args(args)
            .env("CADENCE_PM_DIR", &pm_dir)
            .env("HOME", &home)
            .env(
                "PATH",
                format!(
                    "{}:{}:{}",
                    gh_bin.display(),
                    bin_dir.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env_remove("CADENCE_ALIAS")
            .output()
            .unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).to_string(),
        )
    };
    let (code, stdout) = cli_gh(&["issue", "finish", "--merged", "--dry-run", "--json"]);
    // Skipped rows are not refusals — the dry-run exits clean.
    assert_eq!(code, 0, "{stdout}");
    let plan: Value = serde_json::from_str(stdout.trim()).unwrap();
    let rows = plan["rows"].as_array().unwrap();
    let row = |id: &str| rows.iter().find(|r| r["issue"] == id).unwrap().clone();
    assert_eq!(row("D-6")["outcome"], "would-finish", "{plan}");
    assert_eq!(row("D-6")["merged_by"], "pr", "{plan}");
    assert_eq!(row("D-7")["outcome"], "skipped", "{plan}");
    assert_eq!(row("D-7")["reason"], "unmerged", "{plan}");
    // Tip an ancestor of the recorded head — accepted via pr too.
    assert_eq!(row("D-8")["outcome"], "would-finish", "{plan}");
    assert_eq!(row("D-8")["merged_by"], "pr", "{plan}");
    // Real sweep: D-6/D-8 finish (each tip is covered by the recorded
    // head); D-7's extra commit keeps it skipped — worktree AND
    // branch stay.
    let (_, stdout) = cli_gh(&["issue", "finish", "--merged", "--json"]);
    let out: Value = serde_json::from_str(stdout.trim()).unwrap();
    let rows = out["rows"].as_array().unwrap();
    let row = |id: &str| rows.iter().find(|r| r["issue"] == id).unwrap().clone();
    assert_eq!(row("D-6")["outcome"], "finished", "{out}");
    assert_eq!(row("D-6")["deleted_branch"], true, "{out}");
    assert_eq!(row("D-7")["outcome"], "skipped", "{out}");
    assert_eq!(row("D-8")["outcome"], "finished", "{out}");
    assert_eq!(row("D-8")["deleted_branch"], true, "{out}");
    assert!(
        !wt("d-6-prbound").exists()
            && !wt("d-8-prancestor").exists()
            && wt("d-7-prreused").is_dir(),
        "the reused-name branch must survive: {out}"
    );
    assert_eq!(
        sha(&repo, "cadence/d-7-prreused"),
        tip7b,
        "the unmerged tip must still resolve — the branch survived: {out}"
    );

    // --remote: the remote delete is gated on merge evidence covering
    // the RESOLVED remote tip — never on survivability's "pushed"
    // (that evidence is the remote itself). A real local bare remote
    // replaces the github one; push + update-ref seed the remote and
    // its tracking ref deterministically.
    let remote_git = tmp.path().join("remote.git");
    git(tmp.path(), &["init", "--bare", "remote.git"]);
    let remote_s = remote_git
        .canonicalize()
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    git(&repo, &["remote", "set-url", "origin", &remote_s]);
    for id in ["D-9", "D-10", "D-11", "D-12", "D-13"] {
        let (ok, out) = cli(&["issue", "start", id]);
        assert!(ok, "{out}");
        let (ok, _) = cli(&["issue", "set", id, "owner="]);
        assert!(ok);
    }
    let git_ok = |dir: &Path, args: &[&str]| -> bool {
        std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap()
            .status
            .success()
    };
    let track = |slug: &str, tip: &str| {
        git(
            &repo,
            &[
                "update-ref",
                &format!("refs/remotes/origin/cadence/{slug}"),
                tip,
            ],
        );
    };
    let remote_has = |slug: &str| {
        git_ok(
            &remote_git,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/heads/cadence/{slug}"),
            ],
        )
    };
    let push = |slug: &str| {
        git(&repo, &["push", "-q", "origin", &format!("cadence/{slug}")]);
    };
    let commit_in = |slug: &str, file: &str| {
        std::fs::write(wt(slug).join(file), "x").unwrap();
        git(&wt(slug), &["add", "-A"]);
        git(&wt(slug), &["commit", "-qm", file]);
        idle(&wt(slug));
    };

    // D-9 merged + remote at the merged tip → remote deleted too.
    commit_in("d-9-remoteok", "r9.txt");
    push("d-9-remoteok");
    track("d-9-remoteok", &sha(&repo, "cadence/d-9-remoteok"));
    git(&repo, &["merge", "-q", "cadence/d-9-remoteok"]);
    let (ok, out) = cli(&["issue", "finish", "D-9", "--remote"]);
    assert!(
        ok && out["finished"] == true
            && out["deleted_branch"] == true
            && out["remote_deleted"] == true,
        "merged branch + matching remote deletes both: {out}"
    );
    assert!(!remote_has("d-9-remoteok"), "the remote branch is gone");

    // D-10 unmerged-but-pushed: the remote was the local's only
    // evidence — with --remote it is kept for lack of merge coverage,
    // and the local stays with it. Both copies survive, row explains.
    commit_in("d-10-remoteunm", "r10.txt");
    let tip10 = sha(&repo, "cadence/d-10-remoteunm");
    push("d-10-remoteunm");
    track("d-10-remoteunm", &tip10);
    let (ok, out) = cli(&["issue", "finish", "D-10", "--remote"]);
    assert!(
        ok && out["finished"] == true
            && out["deleted_branch"] == false
            && out["remote_deleted"] == false
            && out["branch_note"]
                .as_str()
                .unwrap_or_default()
                .contains("kept")
            && out["remote_note"]
                .as_str()
                .unwrap_or_default()
                .contains("kept"),
        "unmerged-but-pushed keeps both copies: {out}"
    );
    assert_eq!(sha(&repo, "cadence/d-10-remoteunm"), tip10);
    assert!(remote_has("d-10-remoteunm"));

    // D-11 merged, but origin moved past the covered tip — the remote
    // carries commits no evidence covers: remote kept; the merged
    // local is still deleted on its own covered tip.
    commit_in("d-11-remoteahead", "r11.txt");
    push("d-11-remoteahead");
    track("d-11-remoteahead", &sha(&repo, "cadence/d-11-remoteahead"));
    git(&repo, &["merge", "-q", "cadence/d-11-remoteahead"]);
    git(&wt("d-11-remoteahead"), &["checkout", "-q", "-b", "scr11"]);
    commit_in("d-11-remoteahead", "ahead.txt");
    let tip11b = sha(&repo, "scr11");
    git(
        &wt("d-11-remoteahead"),
        &[
            "push",
            "-q",
            "origin",
            "scr11:refs/heads/cadence/d-11-remoteahead",
        ],
    );
    track("d-11-remoteahead", &tip11b);
    git(
        &wt("d-11-remoteahead"),
        &["checkout", "-q", "cadence/d-11-remoteahead"],
    );
    let (ok, out) = cli(&["issue", "finish", "D-11", "--remote"]);
    assert!(
        ok && out["finished"] == true
            && out["deleted_branch"] == true
            && out["remote_deleted"] == false
            && out["remote_note"]
                .as_str()
                .unwrap_or_default()
                .contains("kept"),
        "origin-ahead keeps the remote, deletes the merged local: {out}"
    );
    assert!(remote_has("d-11-remoteahead"));

    // D-12 same as D-10 but --force: the uncovered remote is deleted
    // anyway and the override is recorded.
    commit_in("d-12-remoteforce", "r12.txt");
    push("d-12-remoteforce");
    track("d-12-remoteforce", &sha(&repo, "cadence/d-12-remoteforce"));
    let (ok, out) = cli(&["issue", "finish", "D-12", "--remote", "--force"]);
    assert!(
        ok && out["remote_deleted"] == true
            && out["deleted_branch"] == true
            && out["overrode"]
                .as_array()
                .unwrap()
                .iter()
                .any(|o| o == "remote-delete-uncovered"),
        "--force deletes the uncovered remote and records it: {out}"
    );
    assert!(!remote_has("d-12-remoteforce"));

    // D-13: the tracking ref is stale — synced at tip A, then the
    // SERVER advanced the branch to B while refs/remotes/origin still
    // names A. The pre-delete fetch must reveal B; the stale tracking
    // ref can never prove coverage. Remote kept, B survives, and the
    // row explains.
    commit_in("d-13-remotestale", "r13.txt");
    let tip13a = sha(&repo, "cadence/d-13-remotestale");
    push("d-13-remotestale");
    git(&repo, &["merge", "-q", "cadence/d-13-remotestale"]);
    git(&wt("d-13-remotestale"), &["checkout", "-q", "-b", "scr13"]);
    commit_in("d-13-remotestale", "ahead.txt");
    let tip13b = sha(&repo, "scr13");
    git(
        &wt("d-13-remotestale"),
        &[
            "push",
            "-q",
            "origin",
            "scr13:refs/heads/cadence/d-13-remotestale",
        ],
    );
    git(
        &wt("d-13-remotestale"),
        &["checkout", "-q", "cadence/d-13-remotestale"],
    );
    // Push updated the tracking ref to B — force it back to A, the
    // stale view a fetch must correct before the gate reads it.
    track("d-13-remotestale", &tip13a);
    let (ok, out) = cli(&["issue", "finish", "D-13", "--remote"]);
    assert!(
        ok && out["finished"] == true
            && out["deleted_branch"] == true
            && out["remote_deleted"] == false
            && out["remote_note"]
                .as_str()
                .unwrap_or_default()
                .contains("kept"),
        "a stale tracking ref must not authorize remote deletion: {out}"
    );
    assert!(remote_has("d-13-remotestale"), "B must survive: {out}");
    assert_eq!(
        sha(&remote_git, "refs/heads/cadence/d-13-remotestale"),
        tip13b,
        "the server-side advance survives intact"
    );
}

#[test]
fn automatic_monitor_dispatch_is_separate_guarded_and_restart_safe() {
    let mut d = TestDaemon::start();
    d.register("pm");
    let cwd = d.dir.path().to_str().unwrap().to_string();
    let caller_quota_at = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();
    for (alias, params) in [
        ("w1", json!({"upstream": "pm"})),
        // These caller-owned values must be ignored by automatic admission;
        // the trusted fixture rows are seeded below after provider open.
        (
            "w2",
            json!({"upstream": "pm", "quota": {
                "source": "provider", "agent": "w2", "observed_at": caller_quota_at,
                "state": "available", "remaining": 0
            }}),
        ),
        (
            "w3",
            json!({"upstream": "pm", "quota": {
                "source": "provider", "agent": "w3", "observed_at": caller_quota_at,
                "state": "available", "remaining": 4
            }}),
        ),
    ] {
        d.register_pcp(alias, "fake", "fake", &cwd, &params.to_string())
            .unwrap();
    }
    for alias in ["pm", "w1", "w2", "w3"] {
        d.wait_agent(alias, "idle", 10);
    }
    let quota_now = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    seed_provider_quota(&d, "w2", quota_now);
    seed_provider_quota(&d, "w3", quota_now - 301);
    // Leave one durable kickoff queued for a stopped worker. The automatic
    // retry must reuse it, proving the existing duplicate-only branch is the
    // idempotency boundary rather than minting another revision.
    let (spec, sha) = d.spec_file("automatic-monitor.md", "coordinator test");
    let project = d.dir.path().to_str().unwrap().to_string();
    d.job_new_repo("pm", "ajob", &spec, &sha, &project).unwrap();
    for (task, assignee) in [
        ("ajob-fresh", "w2"),
        ("ajob-duplicate", "w1"),
        ("ajob-blocked", "w3"),
    ] {
        d.task_new_ac("ajob", task, assignee, "run the focused coordinator checks")
            .unwrap();
    }
    d.operator_rpc("agent_stop", json!({"alias": "w1"}))
        .unwrap();
    d.wait_agent("w1", "stopped", 10);
    let existing = d
        .operator_rpc(
            "task_dispatch",
            json!({"task": "ajob-duplicate", "by": "operator"}),
        )
        .unwrap();
    let existing_kickoff = existing["message"].as_str().unwrap().to_string();
    assert_eq!(existing["duplicate"], false);

    let invalid = d.operator_rpc(
        "monitor_register",
        json!({"monitor": "auto-invalid", "project": project,
               "owner": "operator", "tasks": ["ajob-fresh"],
               "interval_secs": 1, "auto_dispatch_enabled": true}),
    );
    assert!(invalid
        .unwrap_err()
        .to_string()
        .contains("separate manual dispatch permission"));

    let registered = d
        .operator_rpc(
            "monitor_register",
            json!({"monitor": "auto", "project": project,
                   "owner": "operator", "tasks": ["ajob-blocked", "ajob-duplicate", "ajob-fresh"],
                   "interval_secs": 1, "dispatch_enabled": true,
                   "auto_dispatch_enabled": true}),
        )
        .unwrap();
    assert_eq!(registered["monitor"]["dispatch_enabled"], true);
    assert_eq!(registered["monitor"]["auto_dispatch_enabled"], true);
    wait_monitor_state(&d, "auto", "active", 5);

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let show = d.rpc("agent_show", json!({"alias": "w2"})).unwrap();
        let dispatched = show["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|message| message["source"] == "job_dispatch")
            .count();
        if dispatched == 1 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "automatic task was not dispatched: {show}; monitor={}; events={}",
            d.rpc("monitor_show", json!({"monitor": "auto"})).unwrap(),
            d.rpc("agent_events", json!({"alias": "w2"})).unwrap()
        );
        thread::sleep(Duration::from_millis(50));
    }
    let w2 = d.rpc("agent_show", json!({"alias": "w2"})).unwrap();
    assert_eq!(
        w2["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|message| message["source"] == "job_dispatch")
            .count(),
        1,
        "fresh automatic dispatch must mint one kickoff"
    );

    let blocked_deadline = Instant::now() + Duration::from_secs(5);
    let blocked = loop {
        let page = d.rpc("monitor_alerts", json!({"monitor": "auto"})).unwrap();
        if let Some(alert) = page["alerts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|alert| alert["kind"] == "dispatch_blocked")
        {
            break alert.clone();
        }
        assert!(
            Instant::now() < blocked_deadline,
            "missing automatic block alert: {page}"
        );
        thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(blocked["task"], "ajob-blocked");
    assert!(blocked["last_error"]
        .as_str()
        .unwrap()
        .contains("quota unknown"));
    assert!(blocked["payload"]["next_action"].is_string());
    let blocked_seq = blocked["seq"].as_i64().unwrap();
    // A later reconcile refuses again: poll until it has counted its
    // attempt on the same row, then check nothing new was minted.
    let retried_deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let page = d.rpc("monitor_alerts", json!({"monitor": "auto"})).unwrap();
        let attempts = page["alerts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|alert| alert["seq"] == blocked_seq)
            .and_then(|alert| alert["attempts"].as_i64())
            .unwrap_or(0);
        if attempts >= 2 {
            break;
        }
        assert!(
            Instant::now() < retried_deadline,
            "blocked dispatch was never retried: {page}"
        );
        thread::sleep(Duration::from_millis(50));
    }
    let repeated = d.rpc("monitor_alerts", json!({"monitor": "auto"})).unwrap();
    let alerts = repeated["alerts"].as_array().unwrap();
    assert_eq!(
        alerts
            .iter()
            .filter(|alert| alert["kind"] == "dispatch_blocked")
            .count(),
        1
    );
    assert_eq!(alerts[0]["seq"], blocked_seq);
    assert!(alerts[0]["attempts"].as_i64().unwrap() >= 2);

    // Provider evidence can arrive after a guarded refusal. The next
    // automatic attempt must reuse the same alert row and resolve it when
    // the durable kickoff is committed; a successful dispatch is not a new
    // alert and does not leave a stale open blocker behind.
    let refreshed_quota_at = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    seed_provider_quota(&d, "w3", refreshed_quota_at);
    let store = Store::open(&d.state.join("cadence.sqlite3")).unwrap();
    // Keep the fixture's worker lifecycle evidence explicit while changing
    // only the provider allowance sample.
    store.set_enabled("w3", true).unwrap();
    store.set_agent_state("w3", "idle", None).unwrap();
    let resolved_deadline = Instant::now() + Duration::from_secs(5);
    let resolved = loop {
        let page = d.rpc("monitor_alerts", json!({"monitor": "auto"})).unwrap();
        if let Some(alert) = page["alerts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|alert| alert["seq"] == blocked_seq && alert["state"] == "resolved")
        {
            break alert.clone();
        }
        assert!(
            Instant::now() < resolved_deadline,
            "automatic dispatch did not resolve the prior block: {page}"
        );
        thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(
        resolved["payload"]["resolution"],
        "automatic dispatch succeeded"
    );
    assert!(resolved["last_error"].is_null(), "{resolved}");
    let open_after_resolution = d
        .rpc("monitor_alerts", json!({"monitor": "auto", "open": true}))
        .unwrap();
    assert!(
        open_after_resolution["alerts"]
            .as_array()
            .unwrap()
            .iter()
            .all(|alert| alert["task"] != "ajob-blocked"),
        "resolved dispatch blocker must leave no open alert: {open_after_resolution}"
    );

    // The pre-existing kickoff is reused after a monitor tick and restart;
    // no second job_dispatch message appears for the stopped worker.
    let duplicate = d
        .operator_rpc(
            "monitor_dispatch",
            json!({"monitor": "auto", "task": "ajob-duplicate"}),
        )
        .unwrap();
    assert_eq!(duplicate["duplicate"], true);
    assert_eq!(duplicate["message"], existing_kickoff);
    let state = d.state.clone();
    d.operator_rpc("shutdown", json!({})).unwrap();
    d.handle.take().unwrap().join().unwrap().unwrap();
    let restarted_at = epoch_now();
    let d2 = TestDaemon::start_on(state);
    wait_monitor_state(&d2, "auto", "active", 5);
    // The first post-restart reconcile records its reuse of the live
    // kickoff after its dispatch transaction — the barrier for "no second
    // kickoff was minted".
    let reused = d2.wait_event_where(
        "daemon",
        "monitor_dispatch",
        |e| {
            e["payload"]["task"] == "ajob-duplicate"
                && e["payload"]["automatic"] == true
                && e["at"].as_f64().is_some_and(|at| at > restarted_at)
        },
        5,
    );
    assert_eq!(reused["payload"]["duplicate"], true, "{reused}");
    assert_eq!(reused["payload"]["message"], existing_kickoff, "{reused}");
    let w1 = d2.rpc("agent_show", json!({"alias": "w1"})).unwrap();
    assert_eq!(
        w1["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|message| message["source"] == "job_dispatch")
            .count(),
        1
    );
    let restored = d2
        .rpc("monitor_alerts", json!({"monitor": "auto"}))
        .unwrap();
    assert!(restored["alerts"]
        .as_array()
        .unwrap()
        .iter()
        .any(|alert| { alert["seq"] == blocked_seq && alert["kind"] == "dispatch_blocked" }));
    let _ = d2.operator_rpc("monitor_stop", json!({"monitor": "auto"}));
}

#[test]
fn automatic_monitor_dispatch_serializes_competing_callers() {
    let mut d = TestDaemon::start();
    d.register("pm");
    let cwd = d.dir.path().to_str().unwrap().to_string();
    d.register_pcp(
        "w1",
        "fake",
        "fake",
        &cwd,
        &json!({"upstream": "pm", "quota":
                   {"source": "provider", "state": "available"}})
        .to_string(),
    )
    .unwrap();
    d.wait_agent("pm", "idle", 10);
    d.wait_agent("w1", "idle", 10);
    let quota_at = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    seed_provider_quota(&d, "w1", quota_at);
    let (spec, sha) = d.spec_file("automatic-race.md", "serialize dispatch");
    let project = d.dir.path().to_str().unwrap().to_string();
    d.job_new_repo("pm", "race-job", &spec, &sha, &project)
        .unwrap();
    d.task_new_ac(
        "race-job",
        "race-task",
        "w1",
        "serialize automatic dispatch",
    )
    .unwrap();

    // CAD-408: stop the daemon — and with it the monitor watcher, which
    // `serve` joins — before the auto-dispatch monitor exists. A monitor
    // registered over RPC is due at once, so a watcher tick before the
    // shutdown dispatched the task, the fake worker completed it, and both
    // competing callers then saw 'review'. Registering against the store
    // afterwards leaves the two callers below as the only dispatchers.
    let state = d.state.clone();
    d.operator_rpc("shutdown", json!({})).unwrap();
    d.handle.take().unwrap().join().unwrap().unwrap();
    let store = Arc::new(Store::open(&state.join("cadence.sqlite3")).unwrap());
    store
        .register_monitor(
            "race-monitor",
            &project,
            "operator",
            60,
            &["race-task".to_string()],
            true,
            true,
        )
        .unwrap();
    // The transaction under test requires an active monitor; this explicit
    // check is the only one that runs.
    let at = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();
    store.check_monitor("race-monitor", at).unwrap();
    assert_eq!(
        store.task("race-task").unwrap().state,
        "draft",
        "no dispatch may precede the competing callers"
    );
    store.set_enabled("w1", true).unwrap();
    store.set_agent_state("w1", "idle", None).unwrap();

    let barrier = Arc::new(Barrier::new(3));
    let mut handles = Vec::new();
    for _ in 0..2 {
        let store = Arc::clone(&store);
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            let pending = HashSet::new();
            barrier.wait();
            store.dispatch_automatic_monitor_task(
                "race-monitor",
                "race-task",
                &pending,
                "monitor:race-monitor",
            )
        }));
    }
    barrier.wait();
    let results: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect();
    assert_eq!(
        results.iter().filter(|result| result.is_ok()).count(),
        2,
        "both callers should receive the same durable kickoff: {results:?}"
    );
    assert_eq!(
        results
            .iter()
            .filter(|result| result.as_ref().unwrap().2)
            .count(),
        1,
        "one competing caller must reuse the live kickoff: {results:?}"
    );
    let kickoff_ids: HashSet<_> = results
        .iter()
        .map(|result| result.as_ref().unwrap().1.clone())
        .collect();
    assert_eq!(
        kickoff_ids.len(),
        1,
        "dedupe key must be stable: {results:?}"
    );
    let tasks = store.tasks_for_job("race-job").unwrap();
    assert_eq!(
        tasks
            .iter()
            .filter(|task| task.state == "dispatched")
            .count(),
        1,
        "exactly one task row may be claimed: {tasks:?}"
    );
    assert_eq!(
        tasks
            .iter()
            .map(|task| store.messages_for_task(&task.id).unwrap().len())
            .sum::<usize>(),
        1,
        "the transaction must mint one kickoff"
    );
}

/// CAD-373: `job task reopen` is the proven operator's or the job's own
/// PM's (the cadence skill tells PMs to run it) — decided by the
/// connection. The task's assignee, a peer worker and another group's
/// PM are refused, forged identity fields or not; the job's PM pane
/// reopens it and is who the record names.
#[test]
fn task_reopen_is_the_operator_or_the_jobs_pm() {
    let d = TestDaemon::start();
    let home = TempDir::new().unwrap();
    let mut pm = LaneShell::spawn(home.path());
    plant_pane(&d, "pm", pm.pid());
    d.register_member("w1", "pm");
    d.wait_agent("w1", "idle", 10);
    let (spec, sha) = d.spec_file("spec.md", "reopen rule");
    d.job_new("pm", "j1", &spec, &sha);
    for task in ["j1-a", "j1-b"] {
        d.task_new_ac("j1", task, "w1", format!("ok REPORT_SHA:{SHA_A}"))
            .unwrap();
        d.job_dispatch(task, json!({})).unwrap();
        d.wait_task(task, "review", 15);
        d.job_verdict(task, SHA_A, "blocked").unwrap();
    }
    d.operator_rpc("agent_stop", json!({"alias": "w1"}))
        .unwrap();
    d.wait_agent("w1", "stopped", 15);
    let mut worker = LaneShell::spawn(home.path());
    plant_pane(&d, "w1", worker.pid());
    let mut peer = LaneShell::spawn(home.path());
    plant_pane(&d, "w9", peer.pid());
    let mut other_pm = LaneShell::spawn(home.path());
    plant_pane(&d, "pm2", other_pm.pid());

    let reopen = json!({"task": "j1-a"});
    for (shell, who, rule) in [
        (&mut worker, "assignee", "is the task's assignee"),
        (&mut peer, "peer", "is not job 'j1''s PM"),
        (&mut other_pm, "other PM", "is not job 'j1''s PM"),
    ] {
        let r = shell.rpc(&d.state, "task_reopen", reopen.clone());
        assert_refused(&r, "job task reopen", rule, who);
        for (field, value) in FORGED_IDENTITY
            .iter()
            .chain(&[("by", "pm"), ("pane", "pm")])
        {
            let r = shell.rpc(&d.state, "task_reopen", forged(&reopen, field, value));
            // Refused either for the field itself or, for a field the
            // gate does not list (`owner`), by the caller rule.
            assert_refused(
                &r,
                "job task reopen",
                "",
                &format!("{who} forging {field}={value}"),
            );
        }
    }
    // The PM itself cannot name someone else either.
    let r = pm.rpc(&d.state, "task_reopen", forged(&reopen, "by", "operator"));
    assert_refused(
        &r,
        "job task reopen",
        "'by' is not accepted",
        "pm forging by",
    );
    assert_eq!(d.task_state("j1-a"), "blocked");

    // The job's PM pane reopens, recorded as itself; so does the operator.
    let r = pm.rpc(&d.state, "task_reopen", reopen);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(d.task_state("j1-a"), "draft");
    d.operator_rpc("task_reopen", json!({"task": "j1-b"}))
        .unwrap();
    assert_eq!(d.task_state("j1-b"), "draft");
    let by: Vec<Value> = d
        .events("pm")
        .into_iter()
        .filter(|e| e["kind"] == "task_reopened")
        .map(|e| e["payload"]["by"].clone())
        .collect();
    assert_eq!(by, vec![json!("pm"), json!("operator")], "{by:?}");
}

/// CAD-372: the verdict's reviewer is the verified connection. The
/// assignee's own pane cannot pass its task — not bare, not claiming
/// `reviewer:"operator"` or another agent, not naming a `pane` — and a
/// distinct agent's pane passes it, recorded as that agent.
#[test]
fn verdict_reviewer_is_the_verified_caller() {
    let d = TestDaemon::start();
    d.register("pm");
    d.register_member("w1", "pm");
    d.wait_agent("w1", "idle", 10);
    let (spec, sha) = d.spec_file("spec.md", "self review");
    d.job_new("pm", "j1", &spec, &sha);
    d.task_new_ac("j1", "j1-t", "w1", format!("ok REPORT_SHA:{SHA_A}"))
        .unwrap();
    d.job_dispatch("j1-t", json!({})).unwrap();
    d.wait_task("j1-t", "review", 15);
    // The worker's pane from here on: its own processes derive `w1`.
    d.operator_rpc("agent_stop", json!({"alias": "w1"}))
        .unwrap();
    d.wait_agent("w1", "stopped", 15);
    let home = TempDir::new().unwrap();
    let mut worker = LaneShell::spawn(home.path());
    plant_pane(&d, "w1", worker.pid());
    let mut qa = LaneShell::spawn(home.path());
    plant_pane(&d, "qa", qa.pid());

    let pass = json!({"task": "j1-t", "sha": SHA_A, "verdict": "pass"});
    let r = worker.rpc(&d.state, "task_verdict", pass.clone());
    assert_refused(&r, "job verdict", "assignee", "assignee bare");
    for (field, value) in [
        ("reviewer", "operator"),
        ("reviewer", "qa"),
        ("pane", "qa"),
        ("by", "qa"),
    ] {
        let r = worker.rpc(&d.state, "task_verdict", forged(&pass, field, value));
        assert_refused(
            &r,
            "job verdict",
            "caller identity is connection-bound",
            &format!("assignee claiming {field}={value}"),
        );
    }
    // The CLI from the worker's pane: same refusal, nothing recorded.
    let (rc, out) = worker.cadence(
        &d.state,
        &format!("job verdict j1-t --sha {SHA_A} --pass --no-verify-worktree --no-status"),
    );
    assert_ne!(rc, 0, "{out}");
    assert!(out.contains("assignee"), "{out}");
    assert_eq!(d.task_state("j1-t"), "review");
    assert_eq!(task_verdicts(&d, "j1-t"), json!([]));

    // A distinct verified reviewer passes it, and is who is recorded.
    let r = qa.rpc(&d.state, "task_verdict", pass);
    assert_eq!(r["ok"], true, "{r}");
    assert_eq!(d.task_state("j1-t"), "verified");
    let verdicts = task_verdicts(&d, "j1-t");
    assert_eq!(verdicts[0]["reviewer"], "qa", "{verdicts}");
    let recorded = d
        .events("pm")
        .into_iter()
        .find(|e| e["kind"] == "verdict_recorded")
        .expect("verdict_recorded");
    assert_eq!(recorded["payload"]["reviewer"], "qa", "{recorded}");
    assert_eq!(recorded["payload"]["pane"], "qa", "{recorded}");
}

/// CAD-202: issue dispatch to a pty lane checks the pane's cwd against
/// the issue project's repos — outside refuses by name, `--force`
/// dispatches and records the override on the issue, a deleted cwd
/// always refuses, an in-repo lane dispatches as before, and `issue
/// start --job --assignee` applies the same check.
#[test]
fn dispatch_checks_pty_lane_cwd_against_project_repos() {
    // The daemon reads the tracker itself on `dispatch_send` (claim
    // check + lane resolution) — bind the pm dir before it starts; a
    // test daemon never falls back to ~/pm.
    let tmp = TempDir::new().unwrap();
    let (pm_dir, repo, home, elsewhere, gone) = (
        tmp.path().join("pm"),
        tmp.path().join("repo"),
        tmp.path().join("home"),
        tmp.path().join("elsewhere"),
        tmp.path().join("gone"),
    );
    test_env().set("CADENCE_PM_DIR", pm_dir.to_str().unwrap());
    let d = TestDaemon::start();
    let _mock = d.mock_stub();
    for dir in [&pm_dir, &repo, &home, &elsewhere, &gone] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let git = |dir: &Path, args: &[&str]| {
        let o = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(o.status.success(), "git {}", args.join(" "));
    };
    git_f_repo(&repo, &git, |_| {});
    let cli = |args: &[&str]| lane_cli(&d, &pm_dir, &home, args);
    assert!(cli(&["issue", "init"]).0);
    let repo_s = repo.canonicalize().unwrap().to_str().unwrap().to_string();
    assert!(cli(&["issue", "project", "add", "demo", "--prefix", "D", "--repo", &repo_s]).0);
    for title in ["One", "Two", "Three", "Four"] {
        assert!(cli(&["issue", "new", title, "--project", "demo"]).0);
    }
    let note = tmp.path().join("kickoff.md");
    std::fs::write(&note, "# kickoff").unwrap();
    let note_s = note.canonicalize().unwrap().to_str().unwrap().to_string();
    let spec = tmp.path().join("spec.md");
    std::fs::write(&spec, "# spec").unwrap();
    let spec_s = spec.to_str().unwrap().to_string();

    d.register("pm");
    for (alias, cwd) in [("out", &elsewhere), ("inr", &repo), ("gone", &gone)] {
        d.fixture_rpc(
            "agent_register",
            json!({"alias": alias, "provider": "tui-stub", "endpoint_kind": "pty",
                   "cwd": cwd.to_str().unwrap(),
                   "params": json!({"upstream": "pm"}).to_string()}),
        )
        .unwrap();
    }
    for alias in ["pm", "out", "inr", "gone"] {
        d.wait_agent(alias, "idle", 20);
    }
    std::fs::remove_dir_all(&gone).unwrap();
    let dispatch = |issue: &str, to: &str, extra: &[&str]| {
        let mut args = vec![
            "dispatch",
            issue,
            "--to",
            to,
            "--note",
            &note_s,
            "--reply-to",
            "pm",
        ];
        args.extend_from_slice(extra);
        cli(&args)
    };

    // Outside every project repo: refused by name, nothing created.
    let (ok, err) = dispatch("D-1", "out", &[]);
    assert!(!ok, "{err}");
    assert!(
        err["error"]
            .as_str()
            .unwrap()
            .starts_with("cwd_outside_project:"),
        "{err}"
    );
    assert!(!repo.join(".cadence/wt/d-1-one").exists());
    assert!(
        d.rpc("agent_show", json!({"alias": "out"})).unwrap()["messages"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    // `--force` dispatches and the override lands on the issue.
    let (ok, out) = dispatch("D-1", "out", &["--force"]);
    assert!(ok, "{out}");
    assert_eq!(out["dispatched"], true, "{out}");
    assert!(
        out["cwd_override"]
            .as_str()
            .unwrap()
            .contains("cwd_outside_project"),
        "{out}"
    );
    let issue = cli(&["issue", "show", "D-1", "--json"]).1;
    assert!(
        issue["comments"].as_array().unwrap().iter().any(|c| {
            let body = c["body"].as_str().unwrap_or("");
            body.contains("Dispatched to out") && body.contains("Dispatch override (--force")
        }),
        "{issue}"
    );

    // A deleted cwd refuses even with --force.
    let (ok, err) = dispatch("D-2", "gone", &["--force"]);
    assert!(!ok, "{err}");
    assert!(
        err["error"].as_str().unwrap().starts_with("cwd_deleted:"),
        "{err}"
    );
    assert!(!repo.join(".cadence/wt/d-2-two").exists());

    // The normal case: a lane inside the project's repo dispatches.
    let (ok, out) = dispatch("D-3", "inr", &[]);
    assert!(ok, "{out}");
    assert_eq!(out["dispatched"], true, "{out}");
    assert!(out["cwd_override"].is_null(), "{out}");

    // `issue start --job --assignee` applies the same check.
    let start = |extra: &[&str]| {
        let mut args = vec![
            "issue",
            "start",
            "D-4",
            "--job",
            "--pm",
            "pm",
            "--spec",
            &spec_s,
            "--assignee",
            "out",
        ];
        args.extend_from_slice(extra);
        cli(&args)
    };
    let (ok, err) = start(&[]);
    assert!(!ok, "{err}");
    assert!(
        err["error"]
            .as_str()
            .unwrap()
            .starts_with("cwd_outside_project:"),
        "{err}"
    );
    assert!(!repo.join(".cadence/wt/d-4-four").exists());
    let (ok, out) = start(&["--force"]);
    assert!(ok, "{out}");
    assert!(out["cwd_override"].is_string(), "{out}");
    let issue = cli(&["issue", "show", "D-4", "--json"]).1;
    assert!(
        issue["comments"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["body"]
                .as_str()
                .unwrap_or("")
                .contains("Dispatch override (--force")),
        "{issue}"
    );
}

/// CAD-159 (ADR-0002 phase 1, PM decision 2026-09-23): `dispatch` on
/// an issue whose `## Acceptance` section has no checklist items warns
/// — on stderr, in the JSON `acceptance` block and once on the
/// dispatch comment — and still dispatches, on both the plain and the
/// `--job` path. A bare `- [ ]` stub is not an item. An issue with
/// items dispatches with no warning and its kickoff lists them (the
/// CAD-238 section-scoped readback).
#[test]
fn dispatch_warns_on_empty_acceptance() {
    use cadence_agent::issue::dispatch::parse_acceptance_listing;
    use cadence_agent::issue::parse::AcceptanceItem;
    let member = Some("{\"upstream\":\"pm\"}");
    let (_seeded, state) = seeded_state(
        &[
            ("pm", None, "fake", "worker"),
            ("w1", member, "inbox", "worker"),
            ("j1", member, "inbox", "worker"),
            ("j2", member, "inbox", "worker"),
            ("j3", member, "inbox", "worker"),
        ],
        |_, _| {},
    );
    // The daemon reads the tracker itself on `dispatch_send` (claim
    // check + lane resolution) — bind the pm dir before it starts; a
    // test daemon never falls back to ~/pm.
    let (tmp, pm_dir, repo, home) = pm_lab_dirs();
    test_env().set("CADENCE_PM_DIR", pm_dir.to_str().unwrap());
    let d = TestDaemon::start_on(state);

    let git = git_ok();
    git_f_repo(&repo, &git, |_| {});
    let bin_dir = Path::new(env!("CARGO_BIN_EXE_cadence"))
        .parent()
        .unwrap()
        .to_path_buf();
    // (success, stdout JSON — or the stderr error JSON, stderr text)
    let cli = |args: &[&str]| -> (bool, Value, String) {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
            .arg("--state-dir")
            .arg(&d.state)
            .args(args)
            .env("CADENCE_PM_DIR", &pm_dir)
            .env("HOME", &home)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    bin_dir.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env_remove("CADENCE_ALIAS")
            .operator_output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
        let text = if out.stdout.is_empty() {
            stderr.clone()
        } else {
            String::from_utf8_lossy(&out.stdout).to_string()
        };
        (
            out.status.success(),
            serde_json::from_str(text.trim()).unwrap_or_else(|_| panic!("not json: {text}")),
            stderr,
        )
    };
    assert!(cli(&["issue", "init"]).0);
    let repo_s = repo.canonicalize().unwrap().to_str().unwrap().to_string();
    assert!(cli(&["issue", "project", "add", "demo", "--prefix", "D", "--repo", &repo_s]).0);
    // D-1/D-4 empty (the heading `issue new` writes), D-2/D-5 stub-only,
    // D-3/D-6 populated through the CAD-238 authoring command.
    for title in ["Empty", "Stub", "Full", "Jempty", "Jstub", "Jfull"] {
        assert!(cli(&["issue", "new", title, "--project", "demo"]).0);
    }
    for id in ["D-2", "D-5"] {
        let path = pm_dir.join("demo").join(id).join("issue.md");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("## Acceptance"), "{text}");
        std::fs::write(&path, format!("{text}- [ ]\n- [x]   \n")).unwrap();
    }
    // An explicit identity: CI runners have no global git config.
    git(
        &pm_dir,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-qam",
            "stub acceptance",
        ],
    );
    let criteria = tmp.path().join("acceptance.md");
    // CAD-300: the first item's own text carries the listing's old
    // delimiter and a checked box; it must still read as one unchecked
    // item.
    std::fs::write(
        &criteria,
        "- [ ] first criterion; [x] not a second item\n- [x] second is done\n",
    )
    .unwrap();
    let criteria_s = criteria.to_str().unwrap();
    for id in ["D-3", "D-6"] {
        let (ok, out, _) = cli(&["issue", "acceptance", id, "--from", criteria_s]);
        assert!(ok, "{out}");
    }
    let note = tmp.path().join("kickoff.md");
    std::fs::write(&note, "# kickoff").unwrap();
    let note_s = note.canonicalize().unwrap().to_str().unwrap().to_string();
    let (spec, _sha) = d.spec_file("spec.md", "acceptance warning spec");
    let listed = r#"1) [ ] "first criterion; [x] not a second item"; 2) [x] "second is done""#;
    let expected = vec![
        AcceptanceItem {
            text: "first criterion; [x] not a second item".into(),
            checked: false,
        },
        AcceptanceItem {
            text: "second is done".into(),
            checked: true,
        },
    ];

    for (id, worker, job, populated) in [
        ("D-1", "w1", false, false),
        ("D-2", "w1", false, false),
        ("D-3", "w1", false, true),
        ("D-4", "j1", true, false),
        ("D-5", "j2", true, false),
        ("D-6", "j3", true, true),
    ] {
        let mut args = vec![
            "dispatch",
            id,
            "--to",
            worker,
            "--note",
            &note_s,
            "--reply-to",
            "pm",
        ];
        if job {
            args.extend(["--job", "--spec", &spec]);
        }
        let (ok, out, stderr) = cli(&args);
        assert!(ok, "{id}: {out} {stderr}");
        assert_eq!(out["dispatched"], true, "{id}: {out}");
        let acc = &out["acceptance"];
        let msg_id = out["message"].as_str().unwrap().to_string();
        let show = d.rpc("agent_show", json!({"alias": worker})).unwrap();
        let body = show["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["id"] == msg_id.as_str())
            .and_then(|m| m["body"].as_str())
            .unwrap_or_else(|| panic!("{id}: kickoff {msg_id} not found: {show}"))
            .to_string();
        let issue = cli(&["issue", "show", id, "--json"]).1;
        let comments: Vec<&str> = issue["comments"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|c| c["body"].as_str())
            .collect();
        let warned = comments
            .iter()
            .filter(|c| c.contains("Acceptance warning"))
            .count();
        if populated {
            assert_eq!(acc["items"], 2, "{id}: {out}");
            assert_eq!(acc["warning"], Value::Null, "{id}: {out}");
            assert_eq!(
                acc["criteria"][0]["text"], "first criterion; [x] not a second item",
                "{id}: {out}"
            );
            assert_eq!(acc["criteria"][1]["done"], true, "{id}: {out}");
            assert!(!stderr.contains("warning"), "{id}: {stderr}");
            assert_eq!(warned, 0, "{id}: {comments:?}");
            assert!(
                body.contains(&format!("Acceptance: {listed}.")),
                "{id}: {body}"
            );
            // Read back from the kickoff the worker receives: exactly
            // the two items, checked state intact.
            let tail = body.split_once(" Acceptance: ").unwrap().1;
            let (back, rest) = parse_acceptance_listing(tail)
                .unwrap_or_else(|| panic!("{id}: listing does not parse: {body}"));
            assert_eq!(back, expected, "{id}: {body}");
            assert!(rest.starts_with('.'), "{id}: {body}");
            if job {
                let job = d.rpc("job_show", json!({"job": out["job"]})).unwrap();
                assert_eq!(job["job"]["tasks"][0]["acceptance"], listed, "{id}: {job}");
            }
        } else {
            assert_eq!(acc["items"], 0, "{id}: {out}");
            assert_eq!(acc["criteria"], json!([]), "{id}: {out}");
            let warning = acc["warning"]
                .as_str()
                .unwrap_or_else(|| panic!("{id}: no warning: {out}"));
            let how = format!("cadence issue acceptance {id} --from <file>");
            assert!(
                warning.contains(id) && warning.contains(&how),
                "{id}: {warning}"
            );
            assert!(
                stderr.contains("warning") && stderr.contains(&how),
                "{id}: {stderr}"
            );
            // Recorded exactly once, on the dispatch's tracker comment.
            assert_eq!(warned, 1, "{id}: {comments:?}");
            assert!(
                comments
                    .iter()
                    .any(|c| c.contains("Dispatched to") && c.contains("Acceptance warning")),
                "{id}: {comments:?}"
            );
            assert!(!body.contains("Acceptance:"), "{id}: {body}");
        }
        assert!(
            body.len() <= 4000 && !body.contains('\n'),
            "{id}: kickoff stays one line: {body}"
        );
    }

    // A re-run while the kickoff is live is a duplicate: it still
    // reports the acceptance block but records nothing new.
    let (ok, out, stderr) = cli(&[
        "dispatch",
        "D-1",
        "--to",
        "w1",
        "--note",
        &note_s,
        "--reply-to",
        "pm",
    ]);
    assert!(ok && out["duplicate"] == true, "{out}");
    assert_eq!(out["acceptance"]["items"], 0, "{out}");
    // CAD-300 (QA R4): nothing was sent, so nothing says it was.
    let warning = out["acceptance"]["warning"].as_str().unwrap_or_default();
    assert!(
        warning.contains("cadence issue acceptance D-1 --from <file>")
            && !warning.contains("Dispatched anyway"),
        "{out}"
    );
    assert!(
        stderr.contains("warning") && !stderr.contains("Dispatched anyway"),
        "{stderr}"
    );
    let issue = cli(&["issue", "show", "D-1", "--json"]).1;
    let warned = issue["comments"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| {
            c["body"]
                .as_str()
                .unwrap_or_default()
                .contains("Acceptance warning")
        })
        .count();
    assert_eq!(warned, 1, "{issue}");
}

/// CAD-160 (ADR-0002 phase 1): criteria are never truncated, dropped or
/// replaced by a pointer. A `dispatch` whose acceptance items cannot
/// fit the 4000-char pty kickoff whole refuses — plain and `--job`
/// alike — naming the ceiling and the note or spec file, and leaves no
/// worktree, branch, job or queued message behind.
#[test]
fn dispatch_refuses_criteria_past_the_pty_ceiling() {
    let member = Some("{\"upstream\":\"pm\"}");
    let (_seeded, state) = seeded_state(
        &[
            ("pm", None, "fake", "worker"),
            ("w1", member, "inbox", "worker"),
        ],
        |_, _| {},
    );
    // The daemon reads the tracker itself on `dispatch_send` (claim
    // check + lane resolution) — bind the pm dir before it starts; a
    // test daemon never falls back to ~/pm.
    let (tmp, pm_dir, repo, home) = pm_lab_dirs();
    test_env().set("CADENCE_PM_DIR", pm_dir.to_str().unwrap());
    let d = TestDaemon::start_on(state);

    let git = |dir: &Path, args: &[&str]| -> String {
        let o = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8_lossy(&o.stdout).to_string()
    };
    git_f_repo(&repo, &git, |_| {});
    let bin_dir = Path::new(env!("CARGO_BIN_EXE_cadence"))
        .parent()
        .unwrap()
        .to_path_buf();
    let cli = |args: &[&str]| -> (bool, String) {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
            .arg("--state-dir")
            .arg(&d.state)
            .args(args)
            .env("CADENCE_PM_DIR", &pm_dir)
            .env("HOME", &home)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    bin_dir.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env_remove("CADENCE_ALIAS")
            .output()
            .unwrap();
        (
            out.status.success(),
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        )
    };
    assert!(cli(&["issue", "init"]).0);
    let repo_s = repo.canonicalize().unwrap().to_str().unwrap().to_string();
    assert!(cli(&["issue", "project", "add", "demo", "--prefix", "D", "--repo", &repo_s]).0);
    // 60 items of ~95 bytes: the listing alone is well past 4000.
    let criteria = tmp.path().join("acceptance.md");
    let items: String = (0..60)
        .map(|i| format!("- [ ] criterion {i} {}\n", "x".repeat(80)))
        .collect();
    std::fs::write(&criteria, items).unwrap();
    let criteria_s = criteria.to_str().unwrap();
    // QA N1 probe: 37 such items fit under 4000 alone but not beside
    // the job kickoff's fixed fields.
    let gap = tmp.path().join("gap.md");
    let items: String = (0..37)
        .map(|i| format!("- [ ] criterion {i} {}\n", "x".repeat(80)))
        .collect();
    std::fs::write(&gap, items).unwrap();
    for title in ["Plain", "Job", "Gap"] {
        assert!(cli(&["issue", "new", title, "--project", "demo"]).0);
    }
    for (id, file) in [
        ("D-1", criteria_s),
        ("D-2", criteria_s),
        ("D-3", gap.to_str().unwrap()),
    ] {
        let (ok, out) = cli(&["issue", "acceptance", id, "--from", file]);
        assert!(ok, "{out}");
    }
    let note = tmp.path().join("kickoff.md");
    std::fs::write(&note, "# kickoff").unwrap();
    let note_s = note.canonicalize().unwrap().to_str().unwrap().to_string();
    let (spec, _sha) = d.spec_file("spec.md", "ceiling spec");

    let (ok, out) = cli(&[
        "dispatch",
        "D-1",
        "--to",
        "w1",
        "--note",
        &note_s,
        "--reply-to",
        "pm",
    ]);
    assert!(!ok, "{out}");
    assert!(out.contains("4000-char") && out.contains(&note_s), "{out}");
    for id in ["D-2", "D-3"] {
        let (ok, out) = cli(&[
            "dispatch",
            id,
            "--to",
            "w1",
            "--note",
            &note_s,
            "--reply-to",
            "pm",
            "--job",
            "--spec",
            &spec,
        ]);
        assert!(!ok, "{id}: {out}");
        assert!(
            out.contains("4000-char") && out.contains(&spec),
            "{id}: {out}"
        );
    }

    // Nothing was created or queued.
    assert!(!repo.join(".cadence").join("wt").exists());
    assert_eq!(git(&repo, &["branch", "--list", "cadence/*"]), "");
    let show = d.rpc("agent_show", json!({"alias": "w1"})).unwrap();
    assert_eq!(show["messages"], json!([]), "{show}");
    let jobs = d.rpc("job_list", json!({"all": true})).unwrap();
    assert_eq!(jobs["jobs"], json!([]), "{jobs}");
}

/// CAD-388 R2-1 (CAD-167): `dispatch --job`'s kickoff pre-check measures
/// the lane `issue start` will actually bind. An issue whose open lane
/// was started under a `--name` of another length than its title slug
/// is measured with that lane's names: criteria that fit only beside
/// the title-slug names refuse before `issue start` (no orphaned job),
/// and criteria that fit only beside the shorter real names dispatch.
#[test]
fn dispatch_job_precheck_measures_the_issues_existing_lane() {
    let member = Some("{\"upstream\":\"pm\"}");
    let (_seeded, state) = seeded_state(
        &[
            ("pm", None, "fake", "worker"),
            ("w1", member, "inbox", "worker"),
        ],
        |_, _| {},
    );
    // The daemon reads the tracker itself on `dispatch_send` (claim
    // check + lane resolution) — bind the pm dir before it starts; a
    // test daemon never falls back to ~/pm.
    let (tmp, pm_dir, repo, home) = pm_lab_dirs();
    test_env().set("CADENCE_PM_DIR", pm_dir.to_str().unwrap());
    let d = TestDaemon::start_on(state);

    let git = git_stdout();
    git_f_repo(&repo, &git, |_| {});
    let base_sha = git(&repo, &["rev-parse", "HEAD"]);
    let bin_dir = Path::new(env!("CARGO_BIN_EXE_cadence"))
        .parent()
        .unwrap()
        .to_path_buf();
    let cli = |args: &[&str]| -> (bool, String) {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
            .arg("--state-dir")
            .arg(&d.state)
            .args(args)
            .env("CADENCE_PM_DIR", &pm_dir)
            .env("HOME", &home)
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    bin_dir.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            .env_remove("CADENCE_ALIAS")
            .operator_output()
            .unwrap();
        (
            out.status.success(),
            format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        )
    };
    assert!(cli(&["issue", "init"]).0);
    let repo_s = repo.canonicalize().unwrap().to_str().unwrap().to_string();
    assert!(cli(&["issue", "project", "add", "demo", "--prefix", "D", "--repo", &repo_s]).0);
    // D-1: short title, lane started under a long --name (QA's probe).
    // D-2, the mirror: long title, lane started under a short --name.
    let long_name = "l".repeat(58);
    let long_title = "m".repeat(40);
    for title in ["T", long_title.as_str()] {
        assert!(cli(&["issue", "new", title, "--project", "demo"]).0);
    }
    for (id, name) in [("D-1", long_name.as_str()), ("D-2", "s")] {
        let (ok, out) = cli(&[
            "issue", "start", id, "--name", name, "--owner", "w1", "--by", "pm",
        ]);
        assert!(ok, "{out}");
    }
    let (spec, _sha) = d.spec_file("spec.md", "lane spec");
    let spec_canon = Path::new(&spec)
        .canonicalize()
        .unwrap()
        .to_string_lossy()
        .into_owned();

    // The kickoff `job dispatch` would send for one criterion of `len`
    // bytes on lane `wt` — the daemon's own builder; None past the
    // ceiling.
    let kickoff = |id: &str, wt: &str, len: usize| -> Option<usize> {
        use cadence_agent::store::{job_kickoff, Job, Task};
        let job = Job {
            id: "job-00000000".into(),
            title: None,
            spec_path: spec_canon.clone(),
            spec_sha256: None,
            pm_alias: String::new(),
            issue_id: Some(id.into()),
            repo: None,
            base_ref: None,
            state: "open".into(),
            max_revisions: 0,
            stall_secs: None,
            error: None,
            created: 0.0,
            updated: 0.0,
        };
        let task = Task {
            id: "job-00000000-t1".into(),
            job_id: job.id.clone(),
            title: None,
            role: "worker".into(),
            assignee: None,
            spec_path: None,
            acceptance: Some(format!("1) [ ] \"{}\"", "x".repeat(len))),
            worktree: Some(wt.into()),
            branch: Some(format!("cadence/{wt}")),
            base_sha: Some(base_sha.clone()),
            head_sha: None,
            state: "draft".into(),
            revision: 0,
            dispatch_message: None,
            error: None,
            created: 0.0,
            updated: 0.0,
        };
        job_kickoff(&job, &task, 1, &"0".repeat(32), "fake", "inbox")
            .ok()
            .map(|k| k.len())
    };
    // The longest criterion lane `wt`'s kickoff fits (it is monotone
    // in the criterion's length; the compact form kicks in near the
    // ceiling, so search rather than extrapolate).
    let fits_up_to = |id: &str, wt: &str| -> usize {
        let (mut lo, mut hi) = (1, 8000);
        assert!(kickoff(id, wt, lo).is_some() && kickoff(id, wt, hi).is_none());
        while hi - lo > 1 {
            let mid = (lo + hi) / 2;
            if kickoff(id, wt, mid).is_some() {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        lo
    };
    let real1 = format!("d-1-{long_name}");
    let (title1, real2, title2) = ("d-1-t", "d-2-s", format!("d-2-{}", "m".repeat(32)));
    // Each criterion sits mid-way between what the title-slug lane and
    // the real lane fit, so the two land on opposite sides of the
    // ceiling (the band is ~2× the name-length difference wide).
    let (r1, t1) = (fits_up_to("D-1", &real1), fits_up_to("D-1", title1));
    assert!(t1 > r1 + 20, "{r1} {t1}");
    let len1 = (r1 + t1) / 2;
    let (r2, t2) = (fits_up_to("D-2", real2), fits_up_to("D-2", &title2));
    assert!(r2 > t2 + 20, "{r2} {t2}");
    let len2 = (r2 + t2) / 2;
    for (id, len) in [("D-1", len1), ("D-2", len2)] {
        let file = tmp.path().join(format!("{id}.md"));
        std::fs::write(&file, format!("- [ ] {}\n", "x".repeat(len))).unwrap();
        let (ok, out) = cli(&["issue", "acceptance", id, "--from", file.to_str().unwrap()]);
        assert!(ok, "{out}");
    }
    let note = tmp.path().join("kickoff.md");
    std::fs::write(&note, "# kickoff").unwrap();
    let note_s = note.canonicalize().unwrap().to_str().unwrap().to_string();
    let dispatch = |id: &str| {
        cli(&[
            "dispatch",
            id,
            "--to",
            "w1",
            "--note",
            &note_s,
            "--reply-to",
            "pm",
            "--job",
            "--spec",
            &spec,
        ])
    };

    // D-1 refuses before `issue start`: no job, no message, no new lane.
    let (ok, out) = dispatch("D-1");
    assert!(!ok, "{out}");
    assert!(
        out.contains("4000-char") && out.contains("Nothing was created"),
        "{out}"
    );
    let jobs = d.rpc("job_list", json!({"all": true})).unwrap();
    assert_eq!(jobs["jobs"], json!([]), "{jobs}");
    let show = d.rpc("agent_show", json!({"alias": "w1"})).unwrap();
    assert_eq!(show["messages"], json!([]), "{show}");
    let lanes = git(
        &repo,
        &["branch", "--list", "cadence/*", "--format=%(refname:short)"],
    );
    assert_eq!(
        lanes,
        format!("cadence/{real1}\ncadence/{real2}"),
        "no lane minted from the title"
    );

    // D-2 dispatches on its existing short lane.
    let (ok, out) = dispatch("D-2");
    assert!(ok, "{out}");
    assert!(out.contains(&format!("cadence/{real2}")), "{out}");
    let jobs = d.rpc("job_list", json!({"all": true})).unwrap();
    let jobs = jobs["jobs"].as_array().unwrap();
    assert_eq!(jobs.len(), 1, "{jobs:?}");
    assert_eq!(jobs[0]["issue"], "D-2", "{jobs:?}");
}
