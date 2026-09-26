//! CAD-139: an idea can be researched and planned, then it stops.
//! The operator's decision is the only thing that creates tickets.
#![allow(clippy::disallowed_methods)]
mod common;
use common::*;

use serde_json::json;
use serde_json::Value;
use std::time::Duration;
use std::time::Instant;

const PLAN: &str = "FAKE_PLAN: options: do-nothing | smallest-useful | full theme \
recommendation: smallest-useful acceptance: the gate holds \
tickets: Ship the toggle — a setting exists || Document it — the help page names it END_PLAN";

fn file_idea(f: &PlanFixture, text: &str) -> String {
    let (ok, out) = f.cli(&["report", "--kind", "idea", "--project", "demo", "-m", text]);
    assert!(ok, "{out}");
    out["id"].as_str().unwrap().to_string()
}

fn ping(f: &PlanFixture) {
    f.d.operator_rpc("reports_changed", json!({})).unwrap();
}

fn wait_tag(f: &PlanFixture, id: &str, tag: &str) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        ping(f);
        if f.front(id).tags.iter().any(|t| t == tag) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{id} never gained {tag}; status {} tags {:?}",
            f.front(id).status,
            f.front(id).tags
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn message_ids(f: &PlanFixture, alias: &str) -> Vec<String> {
    f.d.rpc("agent_show", json!({"alias": alias})).unwrap()["messages"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|m| m["id"].as_str().map(str::to_string))
        .collect()
}

fn comment_text(f: &PlanFixture, id: &str) -> String {
    let dir = f.pm_dir.join("demo").join(id).join("comments");
    let mut out = String::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in rd.flatten() {
        out.push_str(&std::fs::read_to_string(entry.path()).unwrap_or_default());
        out.push('\n');
    }
    out
}

fn issue_ids(f: &PlanFixture) -> Vec<String> {
    let mut ids = Vec::new();
    for entry in std::fs::read_dir(f.pm_dir.join("demo")).unwrap().flatten() {
        if entry.path().join("issue.md").is_file() {
            ids.push(entry.file_name().to_string_lossy().to_string());
        }
    }
    ids.sort();
    ids
}

fn needs(f: &PlanFixture) -> Vec<Value> {
    overview_at(&f.tmp.path().join("home"), &f.d.state, Some(&f.pm_dir), &[])["needs_me"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// The pipeline spends one research turn and one plan turn, then
/// stops. Approval is the operator's connection. It creates exactly
/// the proposed children. A second decision does not.
#[test]
fn idea_pipeline_stops_at_the_gate_and_approval_creates_children() {
    let f = PlanFixture::start_idea_router();
    let quiet = file_idea(&f, "quiet idea stays held\nthe switch is off");
    ping(&f);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let events = f.daemon_events("intake_idea");
        if events.iter().any(|e| e["issue"] == quiet) {
            assert_eq!(
                events.iter().find(|e| e["issue"] == quiet).unwrap()["auto_research"],
                false
            );
            break;
        }
        assert!(Instant::now() < deadline, "no intake_idea for {quiet}");
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(f.front(&quiet).status, "backlog");
    assert!(!f.front(&quiet).tags.iter().any(|t| t == "plan-ready"));

    let yaml = f.pm_dir.join("demo/project.yaml");
    let mut text = std::fs::read_to_string(&yaml).unwrap();
    text.push_str("\nintake:\n  auto_research: true\n  max_per_day: 1\n");
    std::fs::write(&yaml, text).unwrap();
    std::fs::write(
        f.pm_dir.join("demo/team.yaml"),
        "roles:\n  researcher:\n    alias: rsch-1\n  architect:\n    alias: arch-1\n",
    )
    .unwrap();
    for alias in ["rsch-1", "arch-1", "dev-1"] {
        f.d.register(alias);
        f.d.wait_agent(alias, "idle", 15);
    }

    let (ok, dup_target) = f.cli(&[
        "issue",
        "new",
        "shared title for dedupe",
        "--project",
        "demo",
    ]);
    assert!(ok, "{dup_target}");
    let dup_target = dup_target["id"].as_str().unwrap().to_string();
    let dup = file_idea(&f, "shared title for dedupe\nsame title, do not research");
    let happy = file_idea(&f, &format!("pipeline toggle idea\n{PLAN}\n"));
    let capped = file_idea(&f, "fresh widgets left untouched\nthis one waits");

    wait_tag(&f, &happy, "plan-ready");
    assert_eq!(f.front(&happy).status, "review");
    assert!(
        comment_text(&f, &happy).contains("kind: research"),
        "research note"
    );
    assert!(
        comment_text(&f, &happy).contains("kind: plan"),
        "plan comment"
    );
    let research_id = format!("idea-r-{}", happy.to_ascii_lowercase());
    let plan_id = format!("idea-p-{}", happy.to_ascii_lowercase());
    assert!(message_ids(&f, "rsch-1").contains(&research_id));
    assert!(message_ids(&f, "arch-1").contains(&plan_id));
    assert!(
        !message_ids(&f, "rsch-1")
            .iter()
            .any(|id| id.contains(&quiet.to_ascii_lowercase())),
        "switch-off idea was researched"
    );
    assert!(
        message_ids(&f, "dev-1").is_empty(),
        "a developer was messaged"
    );
    assert_eq!(f.lanes(), (String::new(), false));

    assert_eq!(f.front(&quiet).status, "backlog");
    assert_eq!(
        f.front(&dup).duplicate_of.as_deref(),
        Some(dup_target.as_str())
    );
    assert_eq!(f.front(&dup).status, "backlog");
    assert!(!message_ids(&f, "rsch-1")
        .iter()
        .any(|id| id.contains(&dup.to_ascii_lowercase())));
    assert!(comment_text(&f, &capped).contains("daily cap"));
    assert_eq!(f.front(&capped).status, "backlog");
    assert!(!f.front(&capped).tags.iter().any(|t| t == "plan-ready"));
    assert!(!message_ids(&f, "rsch-1")
        .iter()
        .any(|id| id.contains(&capped.to_ascii_lowercase())));

    let before = issue_ids(&f);
    assert!(
        !before.iter().any(|id| {
            let front = f.front(id);
            front.parent.as_deref() == Some(happy.as_str())
        }),
        "children exist before the decision"
    );

    let state = f.d.state.join("idea-pipeline.json");
    let mut pipeline: Value =
        serde_json::from_str(&std::fs::read_to_string(&state).unwrap()).unwrap();
    pipeline["records"][&happy]["plan_ready_at"] = json!(0);
    std::fs::write(&state, serde_json::to_string_pretty(&pipeline).unwrap()).unwrap();
    wait_tag(&f, &happy, "idea-stale");
    assert!(f.front(&happy).tags.iter().any(|t| t == "plan-ready"));
    assert!(comment_text(&f, &happy).contains("14 days"));

    let rows = needs(&f);
    let plan_row = rows
        .iter()
        .find(|r| r["kind"] == "idea_plan" && r["subject"]["id"] == happy);
    let plan_row = plan_row.expect("needs-me idea_plan");
    let title = plan_row["title"].as_str().unwrap();
    assert!(
        title.starts_with(&format!("idea plan ready for your decision — {happy} ")),
        "{title}"
    );
    assert_eq!(plan_row["audience"], "operator");
    assert!(
        !rows
            .iter()
            .any(|r| r["kind"] == "review_no_pr" && r["subject"]["id"] == happy),
        "plan-ready must not look like a missing PR"
    );
    assert!(rows
        .iter()
        .any(|r| r["kind"] == "idea_duplicate" && r["subject"]["id"] == dup));

    let home = tempfile::TempDir::new().unwrap();
    let mut pane = LaneShell::spawn(home.path());
    plant_pane(&f.d, "pane-9", pane.pid());
    let refused = pane.rpc(
        &f.d.state,
        "idea_decide",
        json!({"issue": &happy, "action": "approve"}),
    );
    let msg = refused["error"]["message"].as_str().unwrap_or_default();
    assert!(msg.contains("operator action"), "{refused}");
    let forged = pane.rpc(
        &f.d.state,
        "idea_decide",
        forged(
            &json!({"issue": &happy, "action": "approve"}),
            "by",
            "operator",
        ),
    );
    assert_eq!(forged["ok"], false, "{forged}");
    let err =
        f.d.unproven_rpc("idea_decide", json!({"issue": &happy, "action": "approve"}))
            .unwrap_err()
            .to_string();
    assert!(
        err.contains("not provably the operator") || err.contains("operator action"),
        "{err}"
    );
    let err =
        f.d.operator_rpc(
            "idea_decide",
            json!({"issue": &happy, "action": "approve", "by": "operator"}),
        )
        .unwrap_err()
        .to_string();
    assert!(err.contains("'by'"), "{err}");
    assert_eq!(issue_ids(&f), before, "a refused decision created tickets");

    let port = start_board(&f.pm_dir, &f.d.state);
    let path = format!("/api/ideas/{happy}/decide");
    let (status, reply) = board_http(
        port,
        &cad328_post(port, &path, THREAD_GUARDS, r#"{"action":"approve"}"#),
    );
    assert_eq!(status, 403, "{reply}");
    assert!(reply.contains("operator_session_required"), "{reply}");
    let mut wk = ManagedWorker::start(&f.d, "wk");
    let request = cad328_post(port, &path, THREAD_GUARDS, r#"{"action":"approve"}"#);
    let r = wk.exec(&[
        "bash",
        "-c",
        DEV_TCP_CLIENT,
        "_",
        &port.to_string(),
        &request,
    ]);
    assert_eq!(r["rc"], 0, "{r}");
    let out = r["out"].as_str().unwrap();
    assert!(out.contains(" 403 "), "{out}");
    assert!(out.contains("operator_only"), "{out}");
    let op = sign_in(&f.d.state, port);
    // A signed-in browser still cannot smuggle `by`. When this process
    // is an agent, the board refuses before the body is read — the
    // same operator gate, not a looser one.
    let (status, reply) = board_http(
        port,
        &cad328_post(
            port,
            &path,
            &op_guards(&op),
            r#"{"action":"approve","by":"operator"}"#,
        ),
    );
    assert!(
        status == 400 || (status == 403 && reply.contains("operator")),
        "{status} {reply}"
    );
    assert_eq!(issue_ids(&f), before);

    let mut oks = 0;
    let mut errs = Vec::new();
    std::thread::scope(|s| {
        let mut joins = Vec::new();
        for _ in 0..2 {
            joins.push(s.spawn(|| {
                f.d.operator_rpc("idea_decide", json!({"issue": &happy, "action": "approve"}))
            }));
        }
        for join in joins {
            match join.join().unwrap() {
                Ok(out) => {
                    oks += 1;
                    assert_eq!(out["decision"]["source"], "operator", "{out}");
                    assert_eq!(out["decision"]["recorded_via"], "operator_connection");
                    assert_eq!(out["decision"]["action"], "approve");
                }
                Err(e) => errs.push(e.to_string()),
            }
        }
    });
    assert_eq!(
        oks, 1,
        "one approval wins, the other is a duplicate: {errs:?}"
    );
    assert!(errs.iter().all(|e| e.contains("already")), "{errs:?}");

    let children: Vec<_> = issue_ids(&f)
        .into_iter()
        .filter(|id| f.front(id).parent.as_deref() == Some(happy.as_str()))
        .collect();
    assert_eq!(children.len(), 2, "{children:?}");
    let mut titles: Vec<_> = children
        .iter()
        .map(|id| f.front(id).title.clone())
        .collect();
    titles.sort();
    assert_eq!(
        titles,
        vec!["Document it".to_string(), "Ship the toggle".to_string()]
    );
    for id in &children {
        let front = f.front(id);
        assert_eq!(front.status, "backlog");
        assert_eq!(front.blocked_by, vec![happy.clone()]);
        assert_eq!(front.priority, "P3");
    }
    let decided = f.front(&happy);
    assert_eq!(decided.status, "done");
    assert!(decided.tags.iter().any(|t| t == "planned"));
    assert!(!decided.tags.iter().any(|t| t == "plan-ready"));
    let decision = f.daemon_events("idea_decision");
    assert_eq!(decision.len(), 1, "{decision:?}");
    assert_eq!(decision[0]["children"].as_array().unwrap().len(), 2);

    let rows = needs(&f);
    assert!(!rows
        .iter()
        .any(|r| r["kind"] == "idea_plan" && r["subject"]["id"] == happy));
    let again =
        f.d.operator_rpc("idea_decide", json!({"issue": &happy, "action": "approve"}))
            .unwrap_err()
            .to_string();
    assert!(again.contains("already"), "{again}");
    let still: Vec<_> = issue_ids(&f)
        .into_iter()
        .filter(|id| f.front(id).parent.as_deref() == Some(happy.as_str()))
        .collect();
    assert_eq!(still.len(), 2);
}
