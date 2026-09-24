//! plan_work: area tests split from tests/integration.rs (CAD-426).
//! End-to-end tests: real socket daemon in-process, fake provider.
//! These exercise the observable contract — queue order, idempotency,
//! restart fencing, approval brokering, serialization — without model calls.
// A test binary never runs the CAD-308 reaper (only `daemon run` does),
// so its own spawns need not go through `cadence_agent::reaper`.
#![allow(clippy::disallowed_methods)]
mod common;
use common::*;

use cadence_agent::client;
use cadence_agent::store::{NewAgent, Store, Take};
use serde_json::{json, Value};
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};
use tempfile::TempDir;

/// CAD-359: `plan propose` creates the epic (plan: proposed) and every
/// ticket in backlog — acceptance, size, agent and blocked_by links —
/// in ONE tracker commit, and emits `plan_proposed`. A ticket without
/// acceptance, or a credential in the text, is refused before anything
/// is written. CAD-360: until the operator approves, no ticket starts
/// (named reason); a non-plan issue starts exactly as before; approval
/// moves tickets to ready and lets them start; `plan show` and the
/// board's issue detail carry state and size-weighted progress.
#[test]
fn plan_propose_approve_gate_and_progress() {
    let f = PlanFixture::start();
    let (ok, out) = f.cli(&["issue", "new", "Loose issue", "--project", "demo"]);
    assert!(ok, "{out}");
    let before = f.commits();

    // CAD-298: an empty-acceptance ticket refuses the whole plan.
    let bad = PLAN_MD.replace("### Acceptance\n- [ ] copy reviewed\n", "no criteria\n");
    let err = f.propose(&bad).unwrap_err().to_string();
    assert!(
        err.contains("Ticket 3 \"Polish\" has no acceptance criteria"),
        "{err}"
    );
    // CAD-109: a credential in the plan text refuses it, unechoed.
    let tok = cad109_token("figd_", "plan", 40);
    let secret = PLAN_MD.replace("The setup wizard.", &format!("The setup wizard {tok}."));
    let err = f.propose(&secret).unwrap_err().to_string();
    assert!(err.contains("cadence-figma-token"), "{err}");
    assert!(!err.contains(&tok[5..]), "{err}");
    assert_eq!(f.commits(), before, "refusals write nothing");
    assert!(!f.pm_dir.join("demo/D-2").exists());

    // The plan: D-2 epic, D-3..D-5 tickets, one commit.
    let out = f.propose(PLAN_MD).unwrap();
    assert_eq!(out["epic"], "D-2", "{out}");
    assert_eq!(out["tickets"], json!(["D-3", "D-4", "D-5"]), "{out}");
    assert_eq!(out["proposed_by"], "operator", "{out}");
    assert_eq!(f.commits(), before + 1, "one commit for the whole plan");
    let msg = f.last_commit();
    for id in ["D-2", "D-3", "D-4", "D-5"] {
        assert!(msg.contains(&format!("Issue: {id}\n")), "{msg}");
    }
    let epic = f.front("D-2");
    let plan = epic.plan.clone().unwrap();
    assert_eq!(plan.state, "proposed");
    assert_eq!(plan.proposed_by, "operator");
    assert_eq!(plan.tickets, vec!["D-3", "D-4", "D-5"]);
    assert_eq!(epic.item_type.as_deref(), Some("epic"));
    let wizard = f.front("D-3");
    assert_eq!(
        (wizard.status.as_str(), wizard.parent.as_deref()),
        ("backlog", Some("D-2"))
    );
    assert_eq!(wizard.size.as_deref(), Some("L"));
    assert_eq!(wizard.owner.as_deref(), Some("dev-1"));
    assert_eq!(wizard.plan_epic.as_deref(), Some("D-2"));
    assert_eq!(f.front("D-4").blocked_by, vec!["D-3".to_string()]);
    let (ok, show) = f.cli(&["issue", "show", "D-3", "--json"]);
    assert!(ok, "{show}");
    assert_eq!(show["acceptance"].as_array().unwrap().len(), 2, "{show}");
    let events = f.daemon_events("plan_proposed");
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0]["epic"], "D-2");
    assert_eq!(events[0]["ticket_count"], 3);

    // Gate: a proposed plan's ticket does not start — named reason,
    // nothing created. The loose issue starts as it always has.
    let (ok, err) = f.cli(&["issue", "start", "D-3"]);
    assert!(!ok);
    let err = err.to_string();
    assert!(
        err.contains("plan D-2 is proposed — approve it with `cadence plan approve D-2`"),
        "{err}"
    );
    assert_eq!(f.front("D-3").status, "backlog");
    assert_eq!(
        f.lanes(),
        (String::new(), false),
        "a refused start leaves no branch or worktree"
    );
    // The board never shows unapproved work as ready or in flight.
    let (ok, err) = f.cli(&["issue", "set", "D-3", "status=ready"]);
    assert!(
        !ok && err.to_string().contains("plan D-2 is proposed"),
        "{err}"
    );
    let (ok, out) = f.cli(&["issue", "set", "D-3", "status=backlog"]);
    assert!(ok, "{out}");
    // `dispatch` refuses at the same gate, before any other check.
    let (ok, err) = f.cli(&[
        "dispatch",
        "D-4",
        "--to",
        "w1",
        "--note",
        "/nonexistent",
        "--reply-to",
        "pm",
    ]);
    assert!(
        !ok && err.to_string().contains("plan D-2 is proposed"),
        "{err}"
    );
    let (ok, out) = f.cli(&["issue", "start", "D-1"]);
    assert!(ok, "non-plan issue must start as before: {out}");
    // The epic itself is never started.
    let (ok, err) = f.cli(&["issue", "start", "D-2"]);
    assert!(!ok && err.to_string().contains("D-2 is a plan"), "{err}");

    let (ok, show) = f.cli(&["plan", "show", "D-2"]);
    assert!(ok, "{show}");
    assert_eq!(show["plan"]["state"], "proposed", "{show}");
    assert_eq!(show["plan"]["progress"]["total_weight"], 12, "{show}");

    // Approve (operator): tickets backlog → ready, one commit.
    let before = f.commits();
    let out =
        f.d.operator_rpc("plan_approve", json!({"epic": "D-2"}))
            .unwrap();
    assert_eq!(out["state"], "approved", "{out}");
    assert_eq!(out["ready"], json!(["D-3", "D-4", "D-5"]), "{out}");
    assert_eq!(f.commits(), before + 1);
    let plan = f.front("D-2").plan.unwrap();
    assert_eq!(
        (plan.state.as_str(), plan.decided_by.as_deref()),
        ("approved", Some("operator"))
    );
    assert!(plan.decided_at.is_some());
    assert_eq!(f.front("D-5").status, "ready");
    assert_eq!(f.daemon_events("plan_approved").len(), 1);
    let err =
        f.d.operator_rpc("plan_approve", json!({"epic": "D-2"}))
            .unwrap_err()
            .to_string();
    assert!(err.contains("already approved"), "{err}");

    let (ok, out) = f.cli(&["issue", "start", "D-3"]);
    assert!(ok, "approved plan's ticket starts: {out}");

    // Progress: S=1 M=3 L=8, unsized = M.
    assert!(f.cli(&["issue", "set", "D-3", "status=done"]).0);
    assert!(f.cli(&["issue", "set", "D-4", "status=dropped"]).0);
    let (ok, show) = f.cli(&["issue", "show", "D-2", "--json"]);
    assert!(ok, "{show}");
    let progress = &show["plan"]["progress"];
    assert_eq!(progress["done_weight"], 8, "{show}");
    assert_eq!(progress["total_weight"], 11, "{show}");
    assert_eq!(progress["ratio"], 0.73, "{show}");
    assert_eq!(show["plan"]["tickets"][0]["weight"], 8, "{show}");
    let (ok, loose) = f.cli(&["issue", "show", "D-1", "--json"]);
    assert!(ok && loose["plan"].is_null(), "{loose}");

    // Reject: recorded with its reason; its ticket never starts.
    let out = f
        .propose("---\ntitle: Later\ngoal: g\n---\n## Only\n### Acceptance\n- [ ] a\n")
        .unwrap();
    assert_eq!(out["epic"], "D-6", "{out}");
    let err =
        f.d.operator_rpc("plan_reject", json!({"epic": "D-6"}))
            .unwrap_err()
            .to_string();
    assert!(err.contains("--reason"), "{err}");
    let out =
        f.d.operator_rpc("plan_reject", json!({"epic": "D-6", "reason": "not now"}))
            .unwrap();
    assert_eq!(out["state"], "rejected", "{out}");
    assert_eq!(
        f.front("D-6").plan.unwrap().reason.as_deref(),
        Some("not now")
    );
    assert_eq!(f.front("D-7").status, "backlog");
    let (ok, err) = f.cli(&["issue", "start", "D-7"]);
    assert!(
        !ok && err.to_string().contains("plan D-6 is rejected"),
        "{err}"
    );
}

/// CAD-360: approve and reject are operator decisions. A pane agent is
/// refused (and its CLI), a caller with no agent identity that is not
/// the proven operator (a detached, orphaned child of a managed tool)
/// is refused, and nothing is written; the proven operator decides.
/// An agent may propose — attributed to its own lane.
#[test]
fn plan_decisions_are_operator_only() {
    let f = PlanFixture::start();
    let home = TempDir::new().unwrap();
    let mut pane = LaneShell::spawn(home.path());
    plant_pane(&f.d, "pane-1", pane.pid());

    let r = pane.rpc(
        &f.d.state,
        "plan_propose",
        json!({"project": "demo", "text": PLAN_MD}),
    );
    assert_eq!(r["result"]["epic"], "D-1", "{r}");
    assert_eq!(r["result"]["proposed_by"], "pane-1", "{r}");
    let before = f.commits();

    for method in ["plan_approve", "plan_reject"] {
        let r = pane.rpc(
            &f.d.state,
            method,
            json!({"epic": "D-1", "reason": "agent says"}),
        );
        let msg = r["error"]["message"].as_str().unwrap_or_default();
        assert!(
            msg.contains("operator action") && msg.contains("pane-1"),
            "{method}: {r}"
        );
    }
    let mut wk = ManagedWorker::start(&f.d, "wk");
    let r = wk.rpc("detached-bare", "plan_approve", json!({"epic": "D-1"}));
    let msg = r["error"]["message"].as_str().unwrap_or_default();
    assert!(msg.contains("not provably the operator"), "{r}");
    // An identity-shaped field is never read as authority.
    let err =
        f.d.operator_rpc("plan_approve", json!({"epic": "D-1", "by": "operator"}))
            .unwrap_err()
            .to_string();
    assert!(err.contains("'by'"), "{err}");
    assert_eq!(f.commits(), before, "refused decisions write nothing");
    assert_eq!(f.front("D-1").plan.unwrap().state, "proposed");

    let out =
        f.d.operator_rpc("plan_approve", json!({"epic": "D-1"}))
            .unwrap();
    assert_eq!(out["state"], "approved", "{out}");
}

/// CAD-360: `job dispatch` of a task whose job is bound to a ticket of
/// an unapproved plan is refused with the named reason and queues
/// nothing; after approval it dispatches. A job bound to an issue in
/// no plan dispatches as before.
#[test]
fn plan_gate_refuses_job_dispatch_until_approved() {
    let f = PlanFixture::start();
    let d = &f.d;
    d.register("pm");
    d.register_member("w1", "pm");
    d.wait_agent("w1", "idle", 10);
    assert!(f.cli(&["issue", "new", "Loose", "--project", "demo"]).0);
    let out = f
        .propose("---\ntitle: P\ngoal: g\n---\n## Only\n### Acceptance\n- [ ] a\n")
        .unwrap();
    assert_eq!(out["tickets"], json!(["D-3"]), "{out}");
    let (spec, sha) = d.spec_file("spec.md", "plan job");
    let project = d.dir.path().to_str().unwrap().to_string();
    for (job, issue) in [("jp", "D-3"), ("jl", "D-1")] {
        d.rpc(
            "job_new",
            json!({"pm": "pm", "job": job, "spec": spec, "spec_sha256": sha,
                   "issue": issue, "repo": project}),
        )
        .unwrap();
        d.rpc(
            "task_new",
            json!({"job": job, "task": format!("{job}-t"), "assignee": "w1",
                   "acceptance": "the plan ticket's criterion"}),
        )
        .unwrap();
    }
    let err = d.job_dispatch("jp-t", json!({})).unwrap_err().to_string();
    assert!(
        err.contains("plan D-2 is proposed — approve it with `cadence plan approve D-2`"),
        "{err}"
    );
    assert_eq!(d.task_state("jp-t"), "draft");

    // Review C1: both monitor dispatch paths run the same gate.
    d.rpc(
        "monitor_register",
        json!({"monitor": "manual", "project": project, "owner": "operator",
               "tasks": ["jp-t"], "interval_secs": 1, "dispatch_enabled": true}),
    )
    .unwrap();
    wait_monitor_state(d, "manual", "active", 5);
    let err = d
        .operator_rpc(
            "monitor_dispatch",
            json!({"monitor": "manual", "task": "jp-t"}),
        )
        .unwrap_err()
        .to_string();
    assert!(err.contains("plan D-2 is proposed"), "{err}");
    d.rpc(
        "monitor_register",
        json!({"monitor": "auto", "project": project, "owner": "operator",
               "tasks": ["jp-t"], "interval_secs": 1, "dispatch_enabled": true,
               "auto_dispatch_enabled": true}),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let page = d.rpc("monitor_alerts", json!({"monitor": "auto"})).unwrap();
        let blocked = page["alerts"].as_array().unwrap().iter().any(|a| {
            a["kind"] == "dispatch_blocked"
                && a["task"] == "jp-t"
                && a.to_string().contains("plan D-2 is proposed")
        });
        if blocked {
            break;
        }
        assert!(Instant::now() < deadline, "no plan block alert: {page}");
        thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(
        d.task_state("jp-t"),
        "draft",
        "the automatic path dispatched nothing"
    );
    for monitor in ["manual", "auto"] {
        d.operator_rpc("monitor_stop", json!({"monitor": monitor}))
            .unwrap();
    }

    d.job_dispatch("jl-t", json!({})).unwrap();
    d.operator_rpc("plan_approve", json!({"epic": "D-2"}))
        .unwrap();
    d.job_dispatch("jp-t", json!({})).unwrap();
}

/// CAD-360 review I1: membership is the plan's approved ticket list,
/// not the `parent` link. While the plan is not rejected a ticket's
/// parent cannot be unlinked, and a hand edit that drops it still
/// leaves the ticket gated. Nothing joins a plan by `issue new
/// --parent`, `issue link parent`, or a hand-edited parent — the last
/// is refused at start.
#[test]
fn plan_membership_is_the_list_not_the_link() {
    let f = PlanFixture::start();
    let out = f
        .propose("---\ntitle: P\ngoal: g\n---\n## A\n### Acceptance\n- [ ] a\n")
        .unwrap();
    assert_eq!(
        (out["epic"].as_str(), out["tickets"][0].as_str()),
        (Some("D-1"), Some("D-2"))
    );

    let (ok, err) = f.cli(&["issue", "unlink", "D-2", "parent", "D-1"]);
    assert!(
        !ok && err.to_string().contains("parent link cannot change"),
        "{err}"
    );
    let (ok, err) = f.cli(&[
        "issue",
        "new",
        "Sneak",
        "--project",
        "demo",
        "--parent",
        "D-1",
    ]);
    assert!(!ok && err.to_string().contains("D-1 is a plan"), "{err}");
    assert!(f.cli(&["issue", "new", "Loose", "--project", "demo"]).0);
    let (ok, err) = f.cli(&["issue", "link", "D-3", "parent", "D-1"]);
    assert!(!ok && err.to_string().contains("D-1 is a plan"), "{err}");

    // Hand edit: the ticket loses both parent and marker — the epic's
    // list still holds it.
    let mut t = f.front("D-2");
    t.parent = None;
    t.plan_epic = None;
    f.write_front("D-2", &t);
    let (ok, err) = f.cli(&["issue", "start", "D-2"]);
    assert!(
        !ok && err.to_string().contains("plan D-1 is proposed"),
        "{err}"
    );

    f.d.operator_rpc("plan_approve", json!({"epic": "D-1"}))
        .unwrap();
    // After approval: a hand-parented issue is not an approved ticket.
    let mut loose = f.front("D-3");
    loose.parent = Some("D-1".into());
    f.write_front("D-3", &loose);
    let (ok, err) = f.cli(&["issue", "start", "D-3"]);
    assert!(
        !ok && err.to_string().contains("not one of its tickets"),
        "{err}"
    );
    let (ok, out) = f.cli(&["issue", "start", "D-2"]);
    assert!(ok, "the listed ticket starts: {out}");
}

/// CAD-360 review I2/I3: the gate fails closed. An epic that exists but
/// does not parse refuses its tickets (`plan_unreadable`); an epic an
/// older binary rewrote without `plan:` refuses the tickets that still
/// carry `plan_epic` (`plan_missing`). An issue whose parent is truly
/// absent is not in a plan and starts.
#[test]
fn plan_gate_fails_closed_on_unreadable_or_rewritten_epic() {
    let f = PlanFixture::start();
    f.propose("---\ntitle: P\ngoal: g\n---\n## A\n### Acceptance\n- [ ] a\n## B\n### Acceptance\n- [ ] b\n")
        .unwrap();
    f.d.operator_rpc("plan_approve", json!({"epic": "D-1"}))
        .unwrap();

    // An older binary drops the unknown `plan:` from the epic.
    let mut epic = f.front("D-1");
    epic.plan = None;
    f.write_front("D-1", &epic);
    let (ok, err) = f.cli(&["issue", "start", "D-2"]);
    let err = err.to_string();
    assert!(!ok && err.contains("carries no plan"), "{err}");

    // An epic that no longer parses.
    let path = f.pm_dir.join("demo/D-1/issue.md");
    std::fs::write(&path, "---\nid: [unterminated\n---\n").unwrap();
    let (ok, err) = f.cli(&["issue", "start", "D-3"]);
    assert!(!ok && err.to_string().contains("cannot be read"), "{err}");
    assert_eq!(f.lanes(), (String::new(), false));

    // A truly absent parent is not a plan: the gate passes. (Checked on
    // the gate itself — the tracker's lint hook refuses to commit a
    // dangling parent, so `issue start` could not record it.)
    let mut orphan =
        cadence_agent::issue::model::Front::new("D-9", "Orphan", "2026-01-01T00:00:00Z");
    orphan.parent = Some("D-99".into());
    cadence_agent::issue::plan::gate(&f.pm_dir, &orphan, "").unwrap();
    // …while the same shape under the unreadable epic refuses.
    orphan.parent = Some("D-1".into());
    let err = cadence_agent::issue::plan::gate(&f.pm_dir, &orphan, "").unwrap_err();
    assert!(err.to_string().contains("cannot be read"), "{err}");
}

/// Proposals are capped: at most 50 tickets and 256 KiB of text.
#[test]
fn plan_propose_caps_size() {
    let f = PlanFixture::start();
    let mut many = String::from("---\ntitle: Big\ngoal: g\n---\n");
    for n in 0..51 {
        many.push_str(&format!("## T{n}\n### Acceptance\n- [ ] a\n"));
    }
    let err = f.propose(&many).unwrap_err().to_string();
    assert!(
        err.contains("51 tickets") && err.contains("at most 50"),
        "{err}"
    );
    let huge = format!(
        "---\ntitle: Huge\ngoal: g\n---\n{}\n## A\n### Acceptance\n- [ ] a\n",
        "word ".repeat(60_000)
    );
    // Past the socket frame the CLI would carry: checked on the library
    // entry the daemon calls.
    let pm = cadence_agent::issue::Pm::at(&f.pm_dir).unwrap();
    let err = cadence_agent::issue::plan::propose(
        &pm,
        "demo",
        &huge,
        &cadence_agent::secret::Allowlist::default(),
        "operator",
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("at most 262144 bytes"), "{err}");
    assert!(!f.pm_dir.join("demo/D-1").exists());
}

/// CAD-360 review round 3: `issue claim` moves backlog/ready into doing,
/// so it obeys the plan status rule — a claim on a proposed plan's
/// ticket is refused, the ticket stays in backlog and no commit is
/// made; the same claim on an ordinary issue works as before, and on
/// the ticket once the plan is approved.
#[test]
fn plan_claim_refused_on_unapproved_ticket() {
    let f = PlanFixture::start();
    assert!(f.cli(&["issue", "new", "Loose", "--project", "demo"]).0);
    f.propose("---\ntitle: P\ngoal: g\n---\n## A\n### Acceptance\n- [ ] a\n")
        .unwrap();
    let before = f.commits();
    let (ok, err) = f.cli(&["issue", "claim", "D-3", "--by", "pm"]);
    assert!(
        !ok && err.to_string().contains("plan D-2 is proposed"),
        "{err}"
    );
    assert_eq!(f.front("D-3").status, "backlog");
    assert!(f.front("D-3").claim.is_none());
    assert_eq!(f.commits(), before, "a refused claim writes nothing");
    assert!(!f
        .pm_dir
        .join("demo/D-3/comments")
        .read_dir()
        .unwrap()
        .any(|_| true));

    let (ok, out) = f.cli(&["issue", "claim", "D-1", "--by", "pm"]);
    assert!(ok, "{out}");
    assert_eq!(f.front("D-1").status, "doing");

    f.d.operator_rpc("plan_approve", json!({"epic": "D-2"}))
        .unwrap();
    let (ok, out) = f.cli(&["issue", "claim", "D-3", "--by", "pm"]);
    assert!(ok, "{out}");
    assert_eq!(f.front("D-3").status, "doing");
}

// ---- CAD-405: work model — types, stages, progress, milestones ----

/// CAD-405: `type`, `size` and `milestone` are settable (and checked);
/// `stage` is not — it moves only through `epic_stage`, a gate decision
/// and one tracker commit. A forward move into an operator stage
/// (`build`, `release` by default) needs the proven operator; a pane
/// agent makes the routine moves and sends an epic back, attributed to
/// its own lane; skips and identity fields are refused. `issue epic ls`
/// and `issue show` carry stage, weighted progress and health. An older
/// binary that drops `stage` makes the epic read as its first stage —
/// earlier, never later — so re-entering build asks the operator again.
#[test]
fn work_model_stage_moves_are_gated_and_committed() {
    let f = PlanFixture::start();
    let home = TempDir::new().unwrap();
    let mut pane = LaneShell::spawn(home.path());
    plant_pane(&f.d, "pane-1", pane.pid());
    assert!(f.cli(&["issue", "new", "Epic", "--project", "demo"]).0);
    for title in ["Big", "Small", "Plain"] {
        let (ok, out) = f.cli(&["issue", "new", title, "--project", "demo", "--epic", "D-1"]);
        assert!(ok, "{out}");
    }
    let (ok, out) = f.cli(&["issue", "set", "D-2", "size=l", "status=done"]);
    assert!(ok, "{out}");
    assert_eq!(f.front("D-2").size.as_deref(), Some("L"));
    assert!(f.cli(&["issue", "set", "D-3", "size=S"]).0);
    let (ok, out) = f.cli(&["issue", "set", "D-1", "type=epic", "milestone=m1"]);
    assert!(ok, "{out}");
    for (pair, want) in [
        ("type=story", "Unknown type 'story'"),
        ("size=XL", "Unknown size 'XL'"),
        ("milestone=M1", "Invalid milestone"),
        ("stage=build", "gate decision"),
        ("type=task", "it has children, so it is an epic"),
    ] {
        let (ok, err) = f.cli(&["issue", "set", "D-1", pair]);
        assert!(!ok && err.to_string().contains(want), "{pair}: {err}");
    }

    // Never moved: the first stage, entry time unknown; weighted
    // progress L done of L+S+M = 8/12.
    let (ok, out) = f.cli(&["issue", "epic", "ls", "--json"]);
    assert!(ok, "{out}");
    let w = &out["epics"][0]["work"];
    assert_eq!(w["stage"]["id"], "shape", "{out}");
    assert_eq!(w["stage"]["source"], "default", "{out}");
    assert_eq!(w["stage"]["next_needs_operator"], true, "{out}");
    assert_eq!(w["progress"]["done_weight"], 8, "{out}");
    assert_eq!(w["progress"]["total_weight"], 12, "{out}");
    assert_eq!(w["health"]["state"], "on_track", "{out}");
    assert_eq!(w["milestone"], "m1", "{out}");

    // shape → build is the operator's: the pane is refused, nothing
    // is written.
    let before = f.commits();
    let r = pane.rpc(
        &f.d.state,
        "epic_stage",
        json!({"epic": "D-1", "stage": "build"}),
    );
    let msg = r["error"]["message"].as_str().unwrap_or_default();
    assert!(
        msg.contains("operator action") && msg.contains("pane-1"),
        "{r}"
    );
    let err =
        f.d.operator_rpc("epic_stage", json!({"epic": "D-1", "stage": "verify"}))
            .unwrap_err()
            .to_string();
    assert!(err.contains("skips a stage"), "{err}");
    let err =
        f.d.operator_rpc(
            "epic_stage",
            json!({"epic": "D-1", "stage": "build", "by": "operator"}),
        )
        .unwrap_err()
        .to_string();
    assert!(err.contains("'by'"), "{err}");
    let err =
        f.d.operator_rpc("epic_stage", json!({"epic": "D-2", "stage": "build"}))
            .unwrap_err()
            .to_string();
    assert!(err.contains("only epics have stages"), "{err}");
    assert_eq!(f.commits(), before, "refused moves write nothing");
    assert!(f.front("D-1").stage.is_none());

    let out =
        f.d.operator_rpc(
            "epic_stage",
            json!({"epic": "D-1", "stage": "build", "note": "scope agreed"}),
        )
        .unwrap();
    assert_eq!(
        (out["from"].as_str(), out["to"].as_str()),
        (Some("shape"), Some("build"))
    );
    assert_eq!(out["by"], "operator", "{out}");
    assert_eq!(f.commits(), before + 1, "one commit per move");
    let msg = f.last_commit();
    assert!(
        msg.starts_with("D-1: stage shape → build — scope agreed"),
        "{msg}"
    );
    assert!(
        msg.contains("Issue: D-1\n") && msg.contains("Actor: operator"),
        "{msg}"
    );
    let epic = f.front("D-1");
    assert_eq!(epic.stage.as_deref(), Some("build"));
    assert!(epic.stage_at.is_some());
    assert_eq!(f.daemon_events("epic_stage_moved").len(), 1);

    // build → verify is routine: the pane moves it, as itself; then it
    // sends the epic back to build.
    let r = pane.rpc(
        &f.d.state,
        "epic_stage",
        json!({"epic": "D-1", "stage": "verify"}),
    );
    assert_eq!(r["result"]["by"], "pane-1", "{r}");
    assert!(f.last_commit().contains("Actor: pane-1"));
    let r = pane.rpc(
        &f.d.state,
        "epic_stage",
        json!({"epic": "D-1", "stage": "build"}),
    );
    assert_eq!(r["result"]["forward"], false, "{r}");
    let (ok, show) = f.cli(&["issue", "show", "D-1", "--json"]);
    assert!(ok, "{show}");
    assert_eq!(show["work"]["stage"]["id"], "build", "{show}");
    assert_eq!(show["work"]["stage"]["source"], "field", "{show}");
    assert_eq!(show["work"]["type"], "epic", "{show}");
    assert_eq!(show["work"]["type_source"], "field", "{show}");
    let (ok, card) = f.cli(&["issue", "ls", "--json"]);
    assert!(ok, "{card}");
    assert_eq!(card["issues"][1]["work"]["weight"], 8, "{card}");
    assert_eq!(card["issues"][1]["work"]["type"], "task", "{card}");

    // An older binary rewrites the epic without the keys it does not
    // know: it reads as shape again, and build needs the operator.
    let mut old = f.front("D-1");
    old.stage = None;
    old.stage_at = None;
    old.item_type = None;
    old.milestone = None;
    f.write_front("D-1", &old);
    let (ok, out) = f.cli(&["issue", "epic", "ls", "--json"]);
    assert!(ok, "{out}");
    assert_eq!(out["epics"][0]["work"]["stage"]["id"], "shape", "{out}");
    assert_eq!(
        out["epics"][0]["work"]["type"], "epic",
        "children still make an epic"
    );
    let r = pane.rpc(
        &f.d.state,
        "epic_stage",
        json!({"epic": "D-1", "stage": "build"}),
    );
    assert!(
        r["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("operator action"),
        "{r}"
    );

    // A plan's stage follows the plan: before approval it cannot move.
    f.propose("---\ntitle: P\ngoal: g\n---\n## A\n### Acceptance\n- [ ] a\n")
        .unwrap();
    let err =
        f.d.operator_rpc("epic_stage", json!({"epic": "D-5", "stage": "build"}))
            .unwrap_err()
            .to_string();
    assert!(err.contains("cadence plan approve D-5"), "{err}");
    let (ok, err) = f.cli(&["issue", "set", "D-5", "type=spike"]);
    assert!(
        !ok && err.to_string().contains("it carries a plan"),
        "{err}"
    );
    let (_, out) = f.cli(&["issue", "show", "D-5", "--json"]);
    assert_eq!(
        out["work"]["health"]["reasons"],
        json!([]),
        "a fresh proposal is on track: {out}"
    );
    f.d.operator_rpc("plan_approve", json!({"epic": "D-5"}))
        .unwrap();
    let (_, out) = f.cli(&["issue", "show", "D-5", "--json"]);
    assert_eq!(out["work"]["stage"]["id"], "build", "{out}");
    assert_eq!(out["work"]["stage"]["source"], "plan", "{out}");

    // An approved plan owns shape: its epic never moves back past build
    // (reject or re-propose the plan instead), so plan and stage agree.
    let before = f.commits();
    let r = pane.rpc(
        &f.d.state,
        "epic_stage",
        json!({"epic": "D-5", "stage": "shape"}),
    );
    let msg = r["error"]["message"].as_str().unwrap_or_default();
    assert!(msg.contains("the plan owns 'shape'"), "{r}");
    assert_eq!(f.commits(), before);
    // Moved on to verify, then an older binary drops `stage`: the epic
    // re-reads as build — earlier than recorded, never later.
    let r = pane.rpc(
        &f.d.state,
        "epic_stage",
        json!({"epic": "D-5", "stage": "verify"}),
    );
    assert_eq!(r["result"]["to"], "verify", "{r}");
    let mut old = f.front("D-5");
    old.stage = None;
    old.stage_at = None;
    f.write_front("D-5", &old);
    let (_, out) = f.cli(&["issue", "show", "D-5", "--json"]);
    assert_eq!(out["work"]["stage"]["id"], "build", "{out}");
    assert_eq!(f.daemon_events("epic_stage_moved").len(), 4);
}

/// CAD-405: stages, operator stages, the stage limit and milestones
/// come from the project's optional PROJECT.md (lenient: other keys are
/// ignored; project.yaml is untouched). Milestones roll up their epics'
/// children and loose issues, from the `milestone` field or an
/// `m<n>-…` tag; `milestone=` must be declared when the project
/// declares any. A malformed PROJECT.md degrades readers to the
/// defaults (with `config_error` and a lint warning) and refuses stage
/// moves.
#[test]
fn work_model_project_md_and_milestones() {
    let f = PlanFixture::start();
    let project_md = f.pm_dir.join("demo/PROJECT.md");
    std::fs::write(
        &project_md,
        "---\nproject: demo\nagents: {dev: 2}\nstages: [shape, build, done]\n\
         operator_stages: []\nstage_limit_days: 2\nmilestones:\n  \
         - {id: m1, title: First, exit: \"one chat works\"}\n  - {id: m2, title: Second}\n---\n\
         # Demo\n",
    )
    .unwrap();
    let (ok, out) = f.cli(&[
        "issue",
        "new",
        "Epic",
        "--project",
        "demo",
        "--tag",
        "m1-first",
    ]);
    assert!(ok, "{out}");
    assert!(
        f.cli(&["issue", "new", "Kid", "--project", "demo", "--epic", "D-1"])
            .0
    );
    assert!(f.cli(&["issue", "new", "Loose", "--project", "demo"]).0);
    let (ok, out) = f.cli(&["issue", "set", "D-3", "milestone=m1"]);
    assert!(ok, "{out}");
    let (ok, err) = f.cli(&["issue", "set", "D-3", "milestone=m7"]);
    assert!(
        !ok && err.to_string().contains("demo declares: m1, m2"),
        "{err}"
    );

    // Custom gates apply only once the operator approves them: a pane
    // cannot approve, the operator's approval is recorded with who and
    // when, and then this project has no operator stages — the pane
    // moves shape → build.
    let home = TempDir::new().unwrap();
    let mut pane = LaneShell::spawn(home.path());
    plant_pane(&f.d, "pane-1", pane.pid());
    let r = pane.rpc(
        &f.d.state,
        "project_work_approve",
        json!({"project": "demo"}),
    );
    assert!(
        r["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("operator action"),
        "{r}"
    );
    let (_, out) = f.cli(&["issue", "epic", "ls", "--json"]);
    assert!(
        out["epics"][0]["work"]["config_unapproved"].is_string(),
        "{out}"
    );
    // The CLI verb reaches the daemon; from the test's own process
    // tree it is not the proven operator, so it is refused too.
    let (ok, err) = f.cli(&["issue", "project", "approve-work", "demo"]);
    assert!(!ok && err.to_string().contains("operator action"), "{err}");
    let out =
        f.d.operator_rpc("project_work_approve", json!({"project": "demo"}))
            .unwrap();
    assert_eq!(out["by"], "operator", "{out}");
    assert!(out["at"].is_string() && out["digest"].is_string(), "{out}");
    assert_eq!(out["stages"], json!(["shape", "build", "done"]), "{out}");
    let (_, out) = f.cli(&["issue", "epic", "ls", "--json"]);
    assert!(
        out["epics"][0]["work"]["config_unapproved"].is_null(),
        "{out}"
    );
    let r = pane.rpc(
        &f.d.state,
        "epic_stage",
        json!({"epic": "D-1", "stage": "build"}),
    );
    assert_eq!(r["result"]["by"], "pane-1", "{r}");
    let r = pane.rpc(
        &f.d.state,
        "epic_stage",
        json!({"epic": "D-1", "stage": "verify"}),
    );
    assert!(
        r["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("Unknown stage 'verify'"),
        "{r}"
    );
    assert!(f.cli(&["issue", "set", "D-2", "size=S", "status=done"]).0);

    let (ok, out) = f.cli(&["milestone", "ls", "--json"]);
    assert!(ok, "{out}");
    let rows = out["milestones"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "{out}");
    let m1 = &rows[0];
    assert_eq!(
        (m1["id"].as_str(), m1["title"].as_str()),
        (Some("m1"), Some("First"))
    );
    assert_eq!(m1["exit"], "one chat works");
    assert_eq!(m1["epics"][0]["id"], "D-1", "{m1}");
    assert_eq!(m1["epics"][0]["stage"], "build", "{m1}");
    assert_eq!(m1["issues"][0]["id"], "D-3", "{m1}");
    // D-2 (S, done) + D-3 (unsized = M): 1 / 4.
    assert_eq!(m1["progress"]["done_weight"], 1, "{m1}");
    assert_eq!(m1["progress"]["total_weight"], 4, "{m1}");
    assert_eq!(rows[1]["progress"]["total_weight"], 0);
    let (ok, show) = f.cli(&["milestone", "show", "m1", "--json"]);
    assert!(ok && show["id"] == "m1", "{show}");
    let (ok, err) = f.cli(&["milestone", "show", "m9"]);
    assert!(
        !ok && err.to_string().contains("Unknown milestone 'm9'"),
        "{err}"
    );
    let (ok, err) = f.cli(&["milestone", "ls", "--project", "nope"]);
    assert!(!ok && err.to_string().contains("nope"), "{err}");

    // Migration: a done epic that was never moved reads `done`.
    assert!(f.cli(&["issue", "new", "Shipped", "--project", "demo"]).0);
    assert!(
        f.cli(&["issue", "new", "Part", "--project", "demo", "--epic", "D-4"])
            .0
    );
    assert!(f.cli(&["issue", "set", "D-5", "status=done"]).0);
    let (_, out) = f.cli(&["issue", "show", "D-4", "--json"]);
    assert_eq!(out["work"]["stage"]["id"], "done", "{out}");
    assert_eq!(out["work"]["stage"]["source"], "status", "{out}");

    // A malformed PROJECT.md: readers fall back, writers refuse.
    std::fs::write(&project_md, "---\nstages: [only]\n---\n").unwrap();
    let (ok, out) = f.cli(&["issue", "epic", "ls", "--json"]);
    assert!(ok, "{out}");
    let w = &out["epics"][0]["work"];
    assert!(
        w["config_error"]
            .as_str()
            .unwrap_or_default()
            .contains("at least two"),
        "{out}"
    );
    assert_eq!(
        w["stage"]["id"], "build",
        "a recorded stage in the default list"
    );
    let before = f.commits();
    let epic_file = f.pm_dir.join("demo/D-1/issue.md");
    let epic_bytes = std::fs::read(&epic_file).unwrap();
    let err =
        f.d.operator_rpc("epic_stage", json!({"epic": "D-1", "stage": "verify"}))
            .unwrap_err()
            .to_string();
    assert!(err.contains("PROJECT.md"), "{err}");
    assert_eq!(f.commits(), before, "a refused move commits nothing");
    assert_eq!(
        std::fs::read(&epic_file).unwrap(),
        epic_bytes,
        "a refused move writes nothing"
    );
    let (ok, lint) = f.cli(&["issue", "lint"]);
    assert!(ok, "a bad PROJECT.md only warns: {lint}");
    assert!(
        lint["warnings"].to_string().contains("PROJECT.md"),
        "{lint}"
    );
}

/// CAD-405 review rounds 2–3: a stage read off the status (a done epic
/// never moved) was never entered, so every move out of it is the
/// operator's. A pane agent is refused — nothing written — whether it
/// aims straight at release or build, or at verify (the first step of
/// done → verify → build); the operator makes done → verify → build.
/// A default first stage follows the usual rule (shape → build is the
/// operator's), and a recorded stage moves back as before.
#[test]
fn work_model_status_derived_stage_needs_operator_for_gates() {
    let f = PlanFixture::start();
    let home = TempDir::new().unwrap();
    let mut pane = LaneShell::spawn(home.path());
    plant_pane(&f.d, "pane-1", pane.pid());
    for (title, epic) in [
        ("Done", None),
        ("Kid", Some("D-1")),
        ("Moved", None),
        ("Kid2", Some("D-3")),
    ] {
        let mut args = vec!["issue", "new", title, "--project", "demo"];
        if let Some(e) = epic {
            args.extend(["--epic", e]);
        }
        assert!(f.cli(&args).0, "{title}");
    }
    assert!(f.cli(&["issue", "set", "D-2", "status=done"]).0);
    let (_, out) = f.cli(&["issue", "show", "D-1", "--json"]);
    assert_eq!(out["work"]["stage"]["source"], "status", "{out}");

    let epic_file = f.pm_dir.join("demo/D-1/issue.md");
    let before = f.commits();
    let bytes = std::fs::read(&epic_file).unwrap();
    for to in ["release", "build", "verify", "shape"] {
        let r = pane.rpc(
            &f.d.state,
            "epic_stage",
            json!({"epic": "D-1", "stage": to}),
        );
        let msg = r["error"]["message"].as_str().unwrap_or_default();
        assert!(
            msg.contains("operator action") && msg.contains("pane-1"),
            "{to}: {r}"
        );
    }
    assert_eq!(f.commits(), before, "refused moves commit nothing");
    assert_eq!(
        std::fs::read(&epic_file).unwrap(),
        bytes,
        "refused moves write nothing"
    );

    // The operator walks done → verify → build.
    for (from, to) in [("done", "verify"), ("verify", "build")] {
        let out =
            f.d.operator_rpc("epic_stage", json!({"epic": "D-1", "stage": to}))
                .unwrap();
        assert_eq!(
            (out["from"].as_str(), out["to"].as_str()),
            (Some(from), Some(to))
        );
        assert_eq!(out["by"], "operator", "{out}");
    }
    assert_eq!(f.commits(), before + 2);
    assert_eq!(f.front("D-1").stage.as_deref(), Some("build"));

    // A default first stage: the pane's shape → build is refused as
    // before (build is an operator stage), nothing written.
    let d3 = f.pm_dir.join("demo/D-3/issue.md");
    let (before, bytes) = (f.commits(), std::fs::read(&d3).unwrap());
    let r = pane.rpc(
        &f.d.state,
        "epic_stage",
        json!({"epic": "D-3", "stage": "build"}),
    );
    let msg = r["error"]["message"].as_str().unwrap_or_default();
    assert!(msg.contains("operator action"), "{r}");
    assert_eq!((f.commits(), std::fs::read(&d3).unwrap()), (before, bytes));

    // A recorded stage behaves as before: the operator moves D-3 into
    // build, the pane moves it on to verify and back into build.
    f.d.operator_rpc("epic_stage", json!({"epic": "D-3", "stage": "build"}))
        .unwrap();
    for to in ["verify", "build"] {
        let r = pane.rpc(
            &f.d.state,
            "epic_stage",
            json!({"epic": "D-3", "stage": to}),
        );
        assert_eq!(r["result"]["by"], "pane-1", "{to}: {r}");
    }
    assert_eq!(f.front("D-3").stage.as_deref(), Some("build"));
}

/// CAD-405 review round 1: an agent editing PROJECT.md cannot move an
/// epic past the operator stages. The gate keys (`stages`,
/// `operator_stages`) apply only while they match the digest the
/// operator approved; otherwise readers and stage moves use the default
/// gates and report `config_unapproved`, and `issue lint` warns. Each
/// probe route — reorder then delete, first stage dropped, operator
/// stages renamed, `operator_stages: []` — falls back to the defaults,
/// and every refused move writes nothing. An approval binds the exact
/// keys: a later edit falls back again.
#[test]
fn work_model_unapproved_gate_edits_fall_back_to_defaults() {
    let f = PlanFixture::start();
    let home = TempDir::new().unwrap();
    let mut pane = LaneShell::spawn(home.path());
    plant_pane(&f.d, "pane-1", pane.pid());
    assert!(f.cli(&["issue", "new", "Epic", "--project", "demo"]).0);
    assert!(
        f.cli(&["issue", "new", "Kid", "--project", "demo", "--epic", "D-1"])
            .0
    );
    let project_md = f.pm_dir.join("demo/PROJECT.md");
    let epic_file = f.pm_dir.join("demo/D-1/issue.md");
    let mut pane_move = |to: &str| {
        let r = pane.rpc(
            &f.d.state,
            "epic_stage",
            json!({"epic": "D-1", "stage": to}),
        );
        r["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    };
    let stage = || {
        let (ok, out) = f.cli(&["issue", "epic", "ls", "--json"]);
        assert!(ok, "{out}");
        out["epics"][0]["work"].clone()
    };

    for (route, yaml, tries) in [
        (
            "reorder",
            "stages: [shape, verify, build, release, done]",
            vec![("verify", "skips a stage"), ("build", "operator action")],
        ),
        (
            "first stage dropped",
            "stages: [build, verify, release, done]",
            vec![("build", "operator action"), ("verify", "skips a stage")],
        ),
        (
            "operator stages renamed",
            "stages: [shape, construct, verify, ship, done]",
            vec![
                ("construct", "Unknown stage 'construct'"),
                ("build", "operator action"),
            ],
        ),
        (
            "operator_stages emptied",
            "operator_stages: []",
            vec![("build", "operator action")],
        ),
    ] {
        std::fs::write(&project_md, format!("---\n{yaml}\n---\n")).unwrap();
        let before = f.commits();
        let bytes = std::fs::read(&epic_file).unwrap();
        for (to, want) in tries {
            let err = pane_move(to);
            assert!(err.contains(want), "{route}: → {to}: {err}");
        }
        assert_eq!(f.commits(), before, "{route}: refused moves commit nothing");
        assert_eq!(
            std::fs::read(&epic_file).unwrap(),
            bytes,
            "{route}: refused moves write nothing"
        );
        let w = stage();
        assert_eq!(w["stage"]["id"], "shape", "{route}: {w}");
        assert_eq!(
            w["stage"]["stages"],
            json!(["shape", "build", "verify", "release", "done"]),
            "{route}: the default gates apply"
        );
        assert!(
            w["config_unapproved"]
                .as_str()
                .unwrap_or_default()
                .starts_with("config_unapproved"),
            "{route}: {w}"
        );
        let (ok, lint) = f.cli(&["issue", "lint"]);
        assert!(ok, "{route}: {lint}");
        assert!(
            lint["warnings"].to_string().contains("config_unapproved"),
            "{route}: {lint}"
        );
    }
    // …then deleting the file: still shape, nothing skipped.
    std::fs::remove_file(&project_md).unwrap();
    assert_eq!(stage()["stage"]["id"], "shape");
    assert!(f.front("D-1").stage.is_none());

    // The operator approves `operator_stages: []`: now the pane moves
    // shape → build. Editing the gates afterwards falls back again.
    std::fs::write(&project_md, "---\noperator_stages: []\n---\n").unwrap();
    f.d.operator_rpc("project_work_approve", json!({"project": "demo"}))
        .unwrap();
    let (ok, lint) = f.cli(&["issue", "lint"]);
    assert!(
        ok && !lint["warnings"].to_string().contains("config_unapproved"),
        "{lint}"
    );
    let r = pane.rpc(
        &f.d.state,
        "epic_stage",
        json!({"epic": "D-1", "stage": "build"}),
    );
    assert_eq!(r["result"]["by"], "pane-1", "{r}");
    std::fs::write(
        &project_md,
        "---\noperator_stages: []\nstages: [shape, build, done]\n---\n",
    )
    .unwrap();
    let before = f.commits();
    let r = pane.rpc(
        &f.d.state,
        "epic_stage",
        json!({"epic": "D-1", "stage": "done"}),
    );
    let msg = r["error"]["message"].as_str().unwrap_or_default();
    assert!(msg.contains("skips a stage"), "{r}");
    assert_eq!(f.commits(), before);
    assert!(stage()["config_unapproved"].is_string());
}

// ---- CAD-358: `cadence project new` ----

/// CAD-358: `cadence project new <key> --repo <path>` registers the repo
/// and seeds PROJECT.md (goal, staffing `agents:`, the default stages,
/// empty milestones) in one tracker commit with `Issue:`/`Actor:`
/// trailers, and lint passes on the result. A second identical run
/// changes nothing; a different repo for the key, the reserved key
/// `agents`, an invalid key and a path that is not a git repo are
/// refused with nothing written. Caller rule: the proven operator runs
/// it; a pane agent, a managed endpoint (and its tool subprocess), and
/// detached children of both are refused, as are identity-shaped
/// fields. The master's leg is `project_new_by_the_master`.
#[test]
fn project_new_registers_seeds_and_is_operator_only() {
    let tmp = TempDir::new().unwrap();
    let pm_dir = tmp.path().join("pm");
    let pm = cadence_agent::issue::Pm::init(&pm_dir).unwrap();
    cadence_agent::issue::write::project_add(&pm, "demo", "D", &[], &[], &[], None).unwrap();
    let cwd = tmp.path().to_path_buf();
    cadence_agent::issue::write::new_issue(
        &pm,
        &cwd,
        Some("demo"),
        "Register reminders",
        None,
        None,
        &[],
        None,
        None,
        &[],
        None,
        "",
    )
    .unwrap();
    let git = |dir: &Path, args: &[&str]| {
        let o = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .output()
            .unwrap();
        assert!(o.status.success(), "git {args:?}: {o:?}");
        String::from_utf8_lossy(&o.stdout).to_string()
    };
    let repo = |name: &str| {
        let dir = tmp.path().join(name);
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q", "-b", "main"]);
        dir.canonicalize().unwrap().to_str().unwrap().to_string()
    };
    let (a, b) = (repo("rem"), repo("other"));
    let plain = tmp.path().join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    let plain = plain.to_str().unwrap().to_string();
    let commits = || git(&pm_dir, &["rev-list", "--count", "HEAD"]);

    test_env().set("CADENCE_PM_DIR", pm_dir.to_str().unwrap());
    let d = TestDaemon::start_opts(slot_opts(2, 1, 900, &[]));
    let home = TempDir::new().unwrap();
    let mut pane = LaneShell::spawn(home.path());
    plant_pane(&d, "pane-1", pane.pid());
    let mut wk = ManagedWorker::start(&d, "wk");
    d.wait_agent("wk", "idle", 25);

    let params = json!({"key": "reminders", "repo": a});
    let before = commits();
    let refused = |r: &Value, why: &str, route: &str| {
        assert_eq!(r["ok"], false, "{route}: {r}");
        let msg = r["error"]["message"].as_str().unwrap_or_default();
        assert!(msg.contains(why), "{route}: wanted '{why}': {r}");
    };
    // Agents: a pane, a managed endpoint and its tool subprocess.
    let r = pane.rpc(&d.state, "project_new", params.clone());
    refused(&r, "operator action", "pane");
    refused(&r, "pane-1", "pane");
    for how in ["self", "child"] {
        let r = wk.rpc(how, "project_new", params.clone());
        refused(&r, "operator action", &format!("managed {how}"));
    }
    // Detached children of an agent derive no identity, and are still
    // not the operator.
    let r = wk.rpc("detached", "project_new", params.clone());
    refused(&r, "not provably the operator", "managed detach");
    let r = wk.rpc("detached-bare", "project_new", params.clone());
    refused(
        &r,
        "not provably the operator",
        "managed detach, alias scrubbed",
    );
    let script = d.dir.path().join("claude-enroll.py");
    let frame = json!({"method": "project_new", "params": params}).to_string();
    assert!(
        !frame.contains('\''),
        "the frame rides a single-quoted argv"
    );
    let outs = TempDir::new().unwrap();
    for (i, env) in ["CADENCE_ALIAS=pane-1", "env -u CADENCE_ALIAS"]
        .into_iter()
        .enumerate()
    {
        let out = outs.path().join(format!("pane-{i}.json"));
        let (rc, text) = pane.run(&format!(
            "{env} python3 {} --detached {} '{frame}' {} {}",
            script.display(),
            client::socket_path(&d.state).display(),
            out.display(),
            pane.pid()
        ));
        assert_eq!(rc, 0, "{text}");
        let deadline = Instant::now() + Duration::from_secs(20);
        while !out.exists() {
            assert!(Instant::now() < deadline, "pane detach {i} never answered");
            thread::sleep(Duration::from_millis(20));
        }
        let r: Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
        refused(
            &r,
            "not provably the operator",
            &format!("pane detach ({env})"),
        );
    }
    // Identity is the connection's, never a field — even the operator's.
    let mut spoof = params.clone();
    spoof["actor"] = json!("operator");
    let err = d
        .operator_rpc("project_new", spoof)
        .unwrap_err()
        .to_string();
    assert!(err.contains("connection-bound"), "{err}");
    assert_eq!(commits(), before, "no refused caller wrote anything");
    assert!(!pm_dir.join("reminders").exists());

    // The operator, through the CLI.
    let (ok, out, err) = d.operator_cadence(&[
        "project",
        "new",
        "reminders",
        "--repo",
        &a,
        "--goal",
        "Remind people on time.",
        "--agent",
        "pm=1,dev=2",
        "--issue",
        "D-1",
    ]);
    assert!(ok, "{out}{err}");
    let out: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(out["changed"], true, "{out}");
    assert_eq!(out["prefix"], "REM", "{out}");
    assert_eq!(out["actor"], "operator", "{out}");
    let after = commits();
    assert_eq!(
        after.trim().parse::<u64>().unwrap(),
        before.trim().parse::<u64>().unwrap() + 1,
        "one tracker commit"
    );
    let msg = git(&pm_dir, &["log", "-1", "--format=%B"]);
    assert!(msg.starts_with("project reminders registered"), "{msg}");
    assert!(
        msg.contains("\nIssue: D-1\n") && msg.contains("\nActor: operator\n"),
        "{msg}"
    );
    let files = git(&pm_dir, &["show", "--name-only", "--format=", "HEAD"]);
    assert_eq!(
        files.lines().collect::<Vec<_>>(),
        ["reminders/PROJECT.md", "reminders/project.yaml"],
        "no team.yaml, nothing else"
    );
    let yaml = std::fs::read_to_string(pm_dir.join("reminders/project.yaml")).unwrap();
    let project = cadence_agent::issue::project::load(&pm_dir.join("reminders/project.yaml"))
        .unwrap_or_else(|e| panic!("{e}: {yaml}"));
    assert_eq!(project.repos[0].path.as_deref(), Some(a.as_str()));
    let manifest = std::fs::read_to_string(pm_dir.join("reminders/PROJECT.md")).unwrap();
    assert!(manifest.contains("agents: {pm: 1, dev: 2}"), "{manifest}");
    assert!(manifest.contains("milestones: []"), "{manifest}");
    assert!(
        manifest.contains("## Goal\n\nRemind people on time."),
        "{manifest}"
    );
    let cfg = cadence_agent::issue::work::load_config(&pm_dir, "reminders").unwrap();
    assert!(
        cadence_agent::issue::work::gates_default(&cfg),
        "{manifest}"
    );
    assert!(cfg.milestones.is_empty());

    // Lint passes on the seeded project, with no stage warning.
    let lint = std::process::Command::new(env!("CARGO_BIN_EXE_cadence"))
        .args(["issue", "lint"])
        .env("CADENCE_PM_DIR", &pm_dir)
        .env("HOME", home.path())
        .env_remove("CADENCE_ALIAS")
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&lint.stdout),
        String::from_utf8_lossy(&lint.stderr)
    );
    assert!(lint.status.success(), "{text}");
    assert!(!text.contains("reminders/PROJECT.md"), "{text}");

    // Idempotent: the same key and repo again changes nothing.
    let (ok, out, err) = d.operator_cadence(&["project", "new", "reminders", "--repo", &a]);
    assert!(ok, "{out}{err}");
    assert!(out.contains("\"changed\": false"), "{out}");
    assert_eq!(commits(), after);
    assert_eq!(
        std::fs::read_to_string(pm_dir.join("reminders/PROJECT.md")).unwrap(),
        manifest
    );

    // Refused with nothing written — the tracker too, by path or link.
    let pm_s = pm_dir.to_str().unwrap().to_string();
    let link = tmp.path().join("tracker-link");
    std::os::unix::fs::symlink(&pm_dir, &link).unwrap();
    let link_s = link.to_str().unwrap().to_string();
    for (args, why) in [
        (vec!["reminders", "--repo", b.as_str()], "different repo"),
        (vec!["agents", "--repo", b.as_str()], "reserved"),
        (
            vec!["Not_A_Key", "--repo", b.as_str()],
            "Invalid project key",
        ),
        (vec!["fresh", "--repo", plain.as_str()], "not a git repo"),
        (vec!["fresh", "--repo", pm_s.as_str()], "is the tracker"),
        (vec!["fresh", "--repo", link_s.as_str()], "is the tracker"),
    ] {
        let mut argv = vec!["project", "new"];
        argv.extend(args.iter().copied());
        let (ok, out, err) = d.operator_cadence(&argv);
        assert!(
            !ok && err.contains(why),
            "{args:?}: wanted '{why}': {out}{err}"
        );
    }
    assert_eq!(commits(), after, "a refusal commits nothing");
    for key in ["agents", "Not_A_Key", "fresh"] {
        assert!(!pm_dir.join(key).exists(), "{key}");
    }
    assert_eq!(
        std::fs::read_to_string(pm_dir.join("reminders/project.yaml")).unwrap(),
        yaml
    );
}

/// CAD-358 × CAD-339: the master registers a project by its verified
/// connection — the commit's actor is `master` — while identity fields
/// from the master are refused with nothing written, a detached child
/// of the master is refused (no identity, not provably the operator),
/// and the tracker and the daemon's state dir are refused as the repo.
#[test]
fn project_new_by_the_master() {
    let f = PlanFixture::start();
    let (mut m, _) = f.start_master();
    let git = |dir: &Path, args: &[&str]| {
        let o = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .output()
            .unwrap();
        assert!(o.status.success(), "git {args:?}: {o:?}");
    };
    let rem = f.tmp.path().join("rem");
    std::fs::create_dir_all(&rem).unwrap();
    git(&rem, &["init", "-q", "-b", "main"]);
    let rem = rem.canonicalize().unwrap().to_str().unwrap().to_string();
    let in_state = f.d.state.join("nested-repo");
    std::fs::create_dir_all(&in_state).unwrap();
    git(&in_state, &["init", "-q", "-b", "main"]);
    let before = f.commits();
    let refused = |r: &Value, why: &str, what: &str| {
        assert_eq!(r["ok"], false, "{what}: {r}");
        let msg = r["error"]["message"].as_str().unwrap_or_default();
        assert!(msg.contains(why), "{what}: wanted '{why}': {r}");
    };

    // Identity fields are refused even from the master's own connection.
    for field in ["actor", "by"] {
        let r = m.rpc(
            "self",
            "project_new",
            json!({"key": "rem", "repo": rem, field: "master"}),
        );
        refused(&r, "connection-bound", field);
    }
    // A detached child of the master is not the master and not the
    // operator.
    for how in ["detached", "detached-bare"] {
        let r = m.rpc(how, "project_new", json!({"key": "rem", "repo": rem}));
        refused(&r, "not provably the operator", how);
    }
    // The tracker and the daemon's state dir are never a project's repo.
    let pm_s = f.pm_dir.to_str().unwrap().to_string();
    let state_s = in_state.to_str().unwrap().to_string();
    for (repo, why) in [
        (pm_s.as_str(), "is the tracker"),
        (state_s.as_str(), "is the daemon state dir"),
    ] {
        let (ok, err) = f.as_master(&mut m, &format!("project new other --repo {repo}"));
        assert!(!ok && err.to_string().contains(why), "{repo}: {err}");
    }
    assert_eq!(f.commits(), before, "no refusal wrote anything");
    assert!(!f.pm_dir.join("rem").exists() && !f.pm_dir.join("other").exists());

    // The master itself: one commit, attributed to it.
    let (ok, out) = f.as_master(
        &mut m,
        &format!("project new rem --repo {rem} --goal 'Remind people.'"),
    );
    assert!(ok, "{out}");
    assert_eq!(out["actor"], "master", "{out}");
    assert_eq!(out["changed"], true, "{out}");
    assert_eq!(f.commits(), before + 1);
    let msg = f.last_commit();
    assert!(msg.contains("\nActor: master\n"), "{msg}");
    assert!(f.pm_dir.join("rem/PROJECT.md").is_file());
    let (ok, out) = f.as_master(&mut m, &format!("project new rem --repo {rem}"));
    assert!(ok && out["changed"] == false, "{out}");
    assert_eq!(f.commits(), before + 1);
}

// ---- CAD-324: continuity packs ----

/// A plan with one ticket for `lead` and one for `other`.
const CAD324_PLAN: &str = "---\ntitle: CSV export\ngoal: Users export their data\n---\n\n\
## Build the exporter\nsize: M\nagent: lead\ndepends_on: 2\n\n### Acceptance\n- [ ] a CSV downloads\n\n\
## Write the export docs\nsize: S\nagent: other\n\n### Acceptance\n- [ ] docs name the columns\n";

/// Every `kind` event of `alias`, oldest first.
fn events_of_kind(d: &TestDaemon, alias: &str, kind: &str) -> Vec<Value> {
    d.rpc("agent_events", json!({"alias": alias})).unwrap()["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == kind)
        .cloned()
        .collect()
}

/// The result text of message `id`.
fn result_text(d: &TestDaemon, alias: &str, id: &str) -> String {
    d.wait_message(alias, id, &["completed"], 20)["result"]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// The pack text the fake provider received with its `n`th pack.
fn received_pack(d: &TestDaemon, alias: &str, n: usize) -> String {
    let packs = events_of_kind(d, alias, "fake_pack");
    assert!(packs.len() > n, "no pack #{n}: {packs:#?}");
    packs[n]["payload"]["pack"].as_str().unwrap().to_string()
}

/// CAD-324 end to end on the fake provider: a threaded agent's new
/// session starts with a pack (operator preferences with a secret
/// redacted, only the plan tickets it owns); a later turn on the same
/// session carries none; a compaction gives the next turn a pack with
/// the last turns verbatim; a turn lost mid-flight gives the resumed
/// session a pack — without the message the operator cancelled, a
/// message still queued, or the message it travels with. An agent with
/// no thread never gets one. The thread records each delivery (counts
/// and digest, never the content).
#[test]
fn cad324_continuity_packs_on_new_compacted_and_lost_sessions() {
    let f = PlanFixture::start();
    f.propose(CAD324_PLAN).unwrap();
    let token = cad109_token(&["gh", "p_"].concat(), "cad324-user-md", 36);
    std::fs::create_dir_all(f.pm_dir.join("company")).unwrap();
    std::fs::write(
        f.pm_dir.join("company/USER.md"),
        format!("Prefer small PRs.\nThe deploy key is {token}\n"),
    )
    .unwrap();

    // Who gets what plan state (the daemon's reader, directly).
    let all = cadence_agent::continuity::plan_state(&f.pm_dir, "master", true).unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].tickets.len(), 2, "{all:?}");
    let own = cadence_agent::continuity::plan_state(&f.pm_dir, "lead", false).unwrap();
    assert_eq!(own[0].tickets.len(), 1, "{own:?}");
    assert_eq!(own[0].tickets[0].title, "Build the exporter");
    assert_eq!(own[0].hidden, 1);
    assert!(
        cadence_agent::continuity::plan_state(&f.pm_dir, "nobody", false)
            .unwrap()
            .is_empty()
    );

    // An agent with no thread never gets a pack.
    f.d.register("other");
    f.d.wait_agent("other", "idle", 15);
    f.d.rpc(
        "agent_send",
        json!({"alias": "other", "text": "no chat", "message": "o1"}),
    )
    .unwrap();
    assert_eq!(result_text(&f.d, "other", "o1"), "FAKE_REPLY: no chat");
    assert!(events_of_kind(&f.d, "other", "continuity_pack").is_empty());

    // New session: the first turn carries the pack.
    f.d.register("lead");
    f.d.wait_agent("lead", "idle", 15);
    f.d.operator_rpc(
        "thread_send",
        json!({"alias": "lead", "text": "hello lead", "message": "c1"}),
    )
    .unwrap();
    let first = result_text(&f.d, "lead", "c1");
    assert!(first.starts_with("FAKE_PACK "), "{first}");
    assert!(first.ends_with("FAKE_REPLY: hello lead"), "{first}");
    let pack = received_pack(&f.d, "lead", 0);
    assert!(pack.contains("this is a new provider session"), "{pack}");
    assert!(pack.contains("Prefer small PRs."), "{pack}");
    assert!(!pack.contains(&token[..20]), "{pack}");
    assert!(pack.contains("[redacted:"), "{pack}");
    assert!(pack.contains("\"CSV export\""), "{pack}");
    assert!(pack.contains("Build the exporter"), "{pack}");
    assert!(!pack.contains("Write the export docs"), "{pack}");
    // Its dependency on a ticket it is not shown is counted, not named.
    assert!(!pack.contains("D-3"), "{pack}");
    assert!(pack.contains("depends on 1 ticket(s) not shown"), "{pack}");
    // Stored text arrives quoted.
    assert!(pack.contains("> Prefer small PRs."), "{pack}");
    assert!(!pack.contains("hello lead"), "the current message: {pack}");
    let delivered = events_of_kind(&f.d, "lead", "continuity_pack");
    assert_eq!(delivered.len(), 1, "{delivered:#?}");
    let digest = delivered[0]["payload"]["sha256"].as_str().unwrap();
    assert_eq!(delivered[0]["payload"]["reason"], "new");
    assert_eq!(delivered[0]["payload"]["message"], "c1");
    assert!(first.starts_with(&format!("FAKE_PACK {}", &digest[..12])));
    let note =
        f.d.rpc("thread_read", json!({"alias": "lead", "limit": 500}))
            .unwrap()["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["payload"]["event"] == "continuity_pack")
            .cloned()
            .expect("the thread records the delivery");
    assert_eq!(note["role"], "system", "{note}");
    assert_eq!(note["payload"]["sha256"], digest, "{note}");
    assert!(
        !note["text"].as_str().unwrap().contains("Prefer small PRs"),
        "{note}"
    );

    // Same session, next turn: no pack.
    f.d.operator_rpc(
        "thread_send",
        json!({"alias": "lead", "text": "second", "message": "c2"}),
    )
    .unwrap();
    assert_eq!(result_text(&f.d, "lead", "c2"), "FAKE_REPLY: second");

    // Compaction: the turn after it carries a pack with the last turns.
    f.d.operator_rpc(
        "thread_send",
        json!({"alias": "lead", "text": "COMPACT", "message": "c3"}),
    )
    .unwrap();
    assert_eq!(result_text(&f.d, "lead", "c3"), "FAKE_COMPACTED");
    assert_eq!(events_of_kind(&f.d, "lead", "session_compacted").len(), 1);
    // The compaction is a thread note — what keeps it due across a restart.
    let compacted =
        f.d.rpc("thread_read", json!({"alias": "lead", "limit": 500}))
            .unwrap()["entries"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["payload"]["event"] == "session_compacted")
            .count();
    assert_eq!(compacted, 1);
    f.d.operator_rpc(
        "thread_send",
        json!({"alias": "lead", "text": "after compaction", "message": "c4"}),
    )
    .unwrap();
    assert!(result_text(&f.d, "lead", "c4").starts_with("FAKE_PACK "));
    let pack = received_pack(&f.d, "lead", 1);
    assert!(pack.contains("compacted this session's context"), "{pack}");
    assert!(pack.contains("hello lead"), "{pack}");
    assert!(
        pack.contains("result (completed):\n> FAKE_REPLY: second"),
        "{pack}"
    );
    assert!(!pack.contains("after compaction"), "{pack}");
    // The daemon's own notes are not replayed as turns.
    assert!(!pack.contains("Continuity pack delivered"), "{pack}");
    assert!(!pack.contains("the next turn carries"), "{pack}");
    assert_eq!(
        events_of_kind(&f.d, "lead", "continuity_pack")[1]["payload"]["reason"],
        "compacted"
    );

    // Lost: the provider drops mid-turn; the turn goes unknown.
    fence_agent(&f.d, "lead", "x1");
    for (id, text) in [
        ("w-x", "withdrawn-ask-7f3"),
        ("r1", "first after resume"),
        ("r2", "queued-later-9c2"),
    ] {
        f.d.rpc(
            "agent_send",
            json!({"alias": "lead", "text": text, "message": id}),
        )
        .unwrap();
    }
    f.d.rpc("message_cancel", json!({"message": "w-x"}))
        .unwrap();
    f.d.operator_rpc(
        "message_reconcile",
        json!({"message": "x1", "status": "interrupted", "note": "provider killed"}),
    )
    .unwrap();
    f.d.wait_agent("lead", "stopped", 10);
    f.d.rpc("agent_resume", json!({"alias": "lead"})).unwrap();
    assert!(result_text(&f.d, "lead", "r1").starts_with("FAKE_PACK "));
    assert_eq!(
        result_text(&f.d, "lead", "r2"),
        "FAKE_REPLY: queued-later-9c2"
    );
    let pack = received_pack(&f.d, "lead", 2);
    assert!(pack.contains("lost mid-turn"), "{pack}");
    assert!(pack.contains("DISCONNECT"), "{pack}");
    assert!(pack.contains("result (unknown)"), "{pack}");
    assert!(!pack.contains("withdrawn-ask-7f3"), "cancelled: {pack}");
    assert!(!pack.contains("queued-later-9c2"), "still queued: {pack}");
    assert!(!pack.contains("first after resume"), "current: {pack}");
    assert_eq!(events_of_kind(&f.d, "lead", "continuity_pack").len(), 3);
    assert_eq!(events_of_kind(&f.d, "lead", "fake_pack").len(), 3);
}

/// CAD-324: a compaction stays due across a daemon restart. The state
/// dir is left the way a daemon that died right after the provider
/// compacted leaves it — the session's identity stored, a turn in the
/// thread, the compaction note — and the first turn after the restart
/// carries the pack, once.
#[test]
fn cad324_compaction_pack_survives_a_daemon_restart() {
    let seeded = TempDir::new().unwrap();
    let state = seeded.path().to_path_buf();
    {
        let store = Store::open(&state.join("cadence.sqlite3")).unwrap();
        let cwd = state.to_str().unwrap().to_string();
        store
            .register_agent(&NewAgent {
                alias: "lead",
                provider: "fake",
                endpoint_kind: "fake",
                role: "worker",
                cwd: &cwd,
                sandbox: "read-only",
                instructions: None,
                params: None,
                team_role: None,
                model_policy: None,
            })
            .unwrap();
        // The fake's own session id: the reopen is the same session.
        store
            .set_identity(
                "lead",
                &cadence_agent::adapter::Identity {
                    thread_id: "fake-thread-lead".into(),
                    session_id: "fake-session-lead".into(),
                    model: None,
                    effort: None,
                    pid: 1,
                    endpoint: None,
                    generation: None,
                    attach: None,
                },
            )
            .unwrap();
        store.ensure_thread("lead").unwrap();
        store
            .enqueue("lead", "before the restart", None, "s1", "user")
            .unwrap();
        let Take::Message(m) = store.take_queued("lead").unwrap() else {
            panic!("expected a message");
        };
        store
            .finish(&m, "completed", &json!({"text": "answered before"}), None)
            .unwrap();
        store
            .thread_append(
                "lead",
                cadence_agent::store::NewEntry {
                    role: cadence_agent::store::ROLE_SYSTEM,
                    kind: cadence_agent::store::KIND_MESSAGE,
                    text: "The provider compacted this session's context.",
                    payload: Some(json!({"event": "session_compacted"})),
                    message_id: None,
                },
            )
            .unwrap();
    }
    // The restarted daemon reopens the agent's stored session itself.
    let d = TestDaemon::start_on(state);
    d.wait_agent("lead", "idle", 15);
    d.operator_rpc(
        "thread_send",
        json!({"alias": "lead", "text": "after the restart", "message": "s2"}),
    )
    .unwrap();
    assert!(result_text(&d, "lead", "s2").starts_with("FAKE_PACK "));
    let pack = received_pack(&d, "lead", 0);
    assert!(pack.contains("compacted this session's context"), "{pack}");
    assert!(pack.contains("answered before"), "{pack}");
    let delivered = events_of_kind(&d, "lead", "continuity_pack");
    assert_eq!(delivered.len(), 1, "{delivered:#?}");
    assert_eq!(delivered[0]["payload"]["reason"], "compacted");
    // Delivered once: the next turn carries none.
    d.operator_rpc(
        "thread_send",
        json!({"alias": "lead", "text": "and then", "message": "s3"}),
    )
    .unwrap();
    assert_eq!(result_text(&d, "lead", "s3"), "FAKE_REPLY: and then");
}
