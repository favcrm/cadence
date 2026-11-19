//! CAD-2476 daemon guard acceptance checks.
//!
//! Drives the real daemon RPC dispatch (`Shared::dispatch`) under caller-scoped
//! agent and operator identities (`test_seam`) to prove:
//! - Non-owning agents are refused from reading (`agent_inbox_feed`), marking
//!   (`agent_inbox_mark`), or promoting (`agent_inbox_promote`) another agent's
//!   inbox, while owning agents and the operator succeed.
//! - Agent callers are ALWAYS refused from `agent_inbox_decide` (peer messages
//!   never grant approval authority); the operator succeeds and routes a
//!   correlated reply back to the specialist.
//! - Steering (`nudge`) combined with inbox `kind` is refused.
//! - Read-only `checkin` auto-reply policy (`params.inbox.checkin_auto_reply`)
//!   records a non-interruptive `auto_answered: true` audit entry (`unread: 0`)
//!   and delivers a correlated `reply` back to `reply_to`.

use crate::daemon::Shared;
use crate::store::NewAgent;
use crate::test_seam::{scoped, Asserted};
use serde_json::{json, Value};

fn register_agent(shared: &Shared, cwd: &str, alias: &str, team_role: &str, params: Option<Value>) {
    let params_str = params.map(|v| v.to_string());
    shared
        .store
        .register_agent(&NewAgent {
            alias,
            provider: "fake",
            endpoint_kind: "fake",
            role: "worker",
            cwd,
            sandbox: "read-only",
            instructions: None,
            params: params_str.as_deref(),
            team_role: Some(team_role),
            model_policy: None,
        })
        .unwrap();
}

#[test]
fn agent_inbox_guards_refuse_cross_agent_access_and_agent_decisions() {
    let dir = tempfile::Builder::new().prefix("c2476").tempdir().unwrap();
    let pm = dir.path().join("pm");
    crate::issue::Pm::init(&pm).unwrap();
    let opts = crate::daemon::ServeOptions {
        test_seam: true,
        ..Default::default()
    };
    opts.provider_env
        .set("CADENCE_PM_DIR", pm.to_str().unwrap());
    let shared = Shared::new(dir.path(), &opts).unwrap();
    let cwd = dir.path().to_str().unwrap().to_string();

    register_agent(&shared, &cwd, "master", "pm", None);
    register_agent(&shared, &cwd, "assist-alex", "worker", None);
    register_agent(
        &shared,
        &cwd,
        "assist-peter",
        "worker",
        Some(json!({
            "inbox": {
                "checkin_auto_reply": true,
                "auto_reply_text": "Supplier quote is ready for Friday's call."
            }
        })),
    );
    register_agent(&shared, &cwd, "finance-agent", "research", None);

    let pid = std::process::id();

    // 1. Steering (`nudge`) combined with inbox `kind` is refused.
    let nudge_err = scoped(Asserted::Operator, || {
        shared.dispatch(
            "agent_send",
            &json!({
                "alias": "assist-peter",
                "text": "ping",
                "kind": "checkin",
                "nudge": true
            }),
            pid,
        )
    })
    .expect_err("nudge + kind must be refused")
    .to_string();
    assert!(
        nudge_err.contains("nudge") || nudge_err.contains("kind"),
        "unexpected error: {nudge_err}"
    );

    // 2. Read-only check-in to assist-peter triggers opt-in auto-reply:
    //    - assist-peter's inbox records quiet `auto_answered: true` (unread = 0)
    //    - assist-alex's inbox receives correlated reply with daemon-derived sender.
    let checkin_receipt = scoped(Asserted::Agent("assist-alex".into()), || {
        shared.dispatch(
            "agent_send",
            &json!({
                "alias": "assist-peter",
                "text": "Is the supplier quote ready for Friday?",
                "kind": "checkin",
                "reply_to": "assist-alex"
            }),
            pid,
        )
    })
    .expect("colleague checkin succeeds");
    assert_eq!(checkin_receipt["auto_answered"], json!(true));

    let peter_feed = scoped(Asserted::Agent("assist-peter".into()), || {
        shared.dispatch("agent_inbox_feed", &json!({"alias": "assist-peter"}), pid)
    })
    .expect("owner can read own inbox");
    assert_eq!(peter_feed["unread"], json!(0));
    assert_eq!(peter_feed["messages"][0]["auto_answered"], json!(true));
    assert_eq!(peter_feed["messages"][0]["kind"], json!("checkin"));

    let alex_feed = scoped(Asserted::Agent("assist-alex".into()), || {
        shared.dispatch("agent_inbox_feed", &json!({"alias": "assist-alex"}), pid)
    })
    .expect("sender receives correlated auto-reply");
    assert_eq!(alex_feed["unread"], json!(1));
    assert_eq!(alex_feed["messages"][0]["kind"], json!("reply"));
    assert_eq!(alex_feed["messages"][0]["sender"], json!("assist-peter"));
    assert!(alex_feed["messages"][0]["body"]
        .as_str()
        .unwrap_or_default()
        .contains("Supplier quote is ready"));

    // 3. Cross-agent access is refused on feed, mark, and promote.
    assert!(scoped(Asserted::Agent("assist-alex".into()), || {
        shared.dispatch("agent_inbox_feed", &json!({"alias": "assist-peter"}), pid)
    })
    .is_err());
    assert!(scoped(Asserted::Agent("assist-alex".into()), || {
        shared.dispatch(
            "agent_inbox_mark",
            &json!({"alias": "assist-peter", "through": 1}),
            pid,
        )
    })
    .is_err());
    let peter_msg_id = peter_feed["messages"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(scoped(Asserted::Agent("assist-alex".into()), || {
        shared.dispatch(
            "agent_inbox_promote",
            &json!({"alias": "assist-peter", "message": peter_msg_id, "issue": "CAD-999"}),
            pid,
        )
    })
    .is_err());

    // 4. Specialist sends a `decision` proposal to master's inbox.
    let decision_receipt = scoped(Asserted::Agent("finance-agent".into()), || {
        shared.dispatch(
            "agent_send",
            &json!({
                "alias": "master",
                "text": "Book supplier invoice #4821 ($2,400) to AP?",
                "kind": "decision",
                "reply_to": "finance-agent",
                "artifact": "invoices/4821.pdf"
            }),
            pid,
        )
    })
    .expect("specialist decision proposal succeeds");
    let decision_msg_id = decision_receipt["message"].as_str().unwrap().to_string();

    // Agent caller (even the owning colleague assistant or specialist) is REFUSED from approving a decision.
    let agent_decide_err = scoped(Asserted::Agent("assist-alex".into()), || {
        shared.dispatch(
            "agent_inbox_decide",
            &json!({"alias": "assist-alex", "message": decision_msg_id, "decision": "approved"}),
            pid,
        )
    })
    .expect_err("agent caller must never approve inbox decisions")
    .to_string();
    assert!(
        agent_decide_err.contains("operator action"),
        "unexpected refusal: {agent_decide_err}"
    );
    assert!(scoped(Asserted::Agent("master".into()), || {
        shared.dispatch(
            "agent_inbox_decide",
            &json!({"alias": "master", "message": decision_msg_id, "decision": "approved"}),
            pid,
        )
    })
    .is_err());

    // Operator approves the specialist decision proposal; feed reflects `approved`
    // and finance-agent receives a correlated reply.
    let decided = scoped(Asserted::Operator, || {
        shared.dispatch(
            "agent_inbox_decide",
            &json!({
                "alias": "master",
                "message": decision_msg_id,
                "decision": "approved",
                "reason": "Verified PO match"
            }),
            pid,
        )
    })
    .expect("operator can approve decision proposal");
    assert_eq!(decided["decision"], json!("approved"));

    let master_feed = scoped(Asserted::Operator, || {
        shared.dispatch("agent_inbox_feed", &json!({"alias": "master"}), pid)
    })
    .unwrap();
    assert_eq!(
        master_feed["messages"][0]["decision_state"],
        json!("approved")
    );
    assert_eq!(master_feed["messages"][0]["decided_by"], json!("operator"));
    assert_eq!(
        master_feed["messages"][0]["decision_reason"],
        json!("Verified PO match")
    );

    let fin_feed = scoped(Asserted::Agent("finance-agent".into()), || {
        shared.dispatch("agent_inbox_feed", &json!({"alias": "finance-agent"}), pid)
    })
    .unwrap();
    assert_eq!(fin_feed["messages"][0]["kind"], json!("reply"));
    assert!(fin_feed["messages"][0]["body"]
        .as_str()
        .unwrap_or_default()
        .contains("approved"));
}
