use super::*;

#[test]
fn task_bound_send_restates_objective_and_outstanding_criteria() {
    let (_dir, shared) =
        task_bound_fixture(r#"1) [ ] "sends via V2"; 2) [x] "tests green"; 3) [ ] "docs""#);
    let text = "use the V1 email provider";
    shared
        .rpc_send(&json!({"alias": "w1", "text": text, "task": "j1-t1", "message": "m1"}))
        .unwrap();
    let body = stored_body(&shared, "m1");
    assert_eq!(
        body,
        "use the V1 email provider — Task j1-t1 (job j1) is still open; this message \
             amends it and does not replace it. Objective: Wire the email provider. Spec: \
             /specs/j1.md. Outstanding criteria: 1) [ ] \"sends via V2\"; 2) [ ] \"docs\"."
    );
    let objective = body.find("Objective:").unwrap();
    let criteria = body.find("Outstanding criteria:").unwrap();
    assert!(body.starts_with(text) && objective < criteria, "{body}");
    assert!(
        !body.contains("tests green"),
        "checked item restated: {body}"
    );
    assert!(!crate::adapter::pty::has_control_chars(&body), "{body}");
    let message = shared.store.message("m1").unwrap().unwrap();
    assert_eq!(message.task_id.as_deref(), Some("j1-t1"));
    assert_eq!(message.reply_to.as_deref(), Some("pm"));
}

#[test]
fn untasked_and_terminal_task_sends_are_byte_identical() {
    let (_dir, shared) = task_bound_fixture(r#"1) [ ] "sends via V2""#);
    let text = "  plain chat — no task. ";
    shared
        .rpc_send(&json!({"alias": "w1", "text": text, "message": "m1"}))
        .unwrap();
    assert_eq!(stored_body(&shared, "m1"), text);
    shared.store.cancel_task("j1-t1", "test").unwrap();
    shared
        .rpc_send(&json!({"alias": "w1", "text": text, "task": "j1-t1", "message": "m2"}))
        .unwrap();
    assert_eq!(stored_body(&shared, "m2"), text);
}

#[test]
fn task_bound_send_to_a_non_assignee_is_byte_identical() {
    let (_dir, shared) = task_bound_fixture(r#"1) [ ] "sends via V2""#);
    let text = "PR is up";
    shared
        .rpc_send(&json!({"alias": "pm", "text": text, "task": "j1-t1", "message": "r1"}))
        .unwrap();
    assert_eq!(stored_body(&shared, "r1"), text);
}

#[test]
fn urgent_superseding_task_message_restates_objective_and_criteria() {
    let (_dir, shared) =
        task_bound_fixture(r#"1) [ ] "sends via V2"; 2) [x] "tests green"; 3) [ ] "docs""#);
    shared
        .rpc_send(&json!({"alias": "w1", "text": "old scope", "task": "j1-t1", "message": "m1"}))
        .unwrap();
    shared
        .rpc_send(
            &json!({"alias": "w1", "text": "current scope", "task": "j1-t1",
                               "message": "m2", "priority": "urgent", "supersedes": ["m1"]}),
        )
        .unwrap();
    let m2 = shared.store.message("m2").unwrap().unwrap();
    assert_eq!(m2.priority, store::Priority::Urgent);
    assert_eq!(
        m2.body,
        "current scope — Task j1-t1 (job j1) is still open; this message amends it and \
             does not replace it. Objective: Wire the email provider. Spec: /specs/j1.md. \
             Outstanding criteria: 1) [ ] \"sends via V2\"; 2) [ ] \"docs\"."
    );
    let m1 = shared.store.message("m1").unwrap().unwrap();
    assert_eq!(m1.state, "cancelled");
    assert_eq!(m1.result.unwrap()["superseded_by"], "m2");
    // A nudge is pasted at once — it has no queue to steer.
    let err = shared
        .rpc_send(&json!({"alias": "w1", "text": "x", "nudge": true, "priority": "urgent"}))
        .unwrap_err()
        .to_string();
    assert!(err.contains("takes no --priority or --supersedes"), "{err}");
    // A caller-supplied identity is refused, not read.
    let err = shared
        .rpc_send(&json!({"alias": "w1", "text": "x", "priority": "urgent", "by": "pm"}))
        .unwrap_err()
        .to_string();
    assert!(err.contains("'by' is not accepted"), "{err}");
    // PR #252 QA N1: a priority that is not a string — the stored
    // rank, a bool, an array — is refused, never queued as normal.
    for (id, bad) in [
        ("p1", json!(1)),
        ("p2", json!(true)),
        ("p3", json!(["urgent"])),
    ] {
        let err = shared
            .rpc_send(&json!({"alias": "w1", "text": "x", "message": id, "priority": bad}))
            .unwrap_err()
            .to_string();
        assert!(err.contains("'priority' must be a string"), "{err}");
        assert!(shared.store.message(id).unwrap().is_none(), "{id} queued");
    }
}

#[test]
fn blank_task_bound_send_is_refused() {
    let (_dir, shared) = task_bound_fixture(r#"1) [ ] "sends via V2""#);
    for (id, text) in [("e1", ""), ("e2", "   ")] {
        let err = shared
            .rpc_send(&json!({"alias": "w1", "text": text, "task": "j1-t1", "message": id}))
            .unwrap_err()
            .to_string();
        assert!(err.contains("Prompt must contain"), "{err}");
        assert!(shared.store.message(id).unwrap().is_none());
    }
    assert_eq!(shared.store.queued_count("w1").unwrap(), 0);
}

#[test]
fn task_bound_send_past_the_pty_ceiling_refuses_and_queues_nothing() {
    let long = format!(r#"1) [ ] "{}""#, "c".repeat(4100));
    let (_dir, shared) = task_bound_fixture(&long);
    let err = shared
        .rpc_send(&json!({"alias": "w1", "text": "steer", "task": "j1-t1", "message": "m1"}))
        .unwrap_err()
        .to_string();
    assert!(err.contains("4000-char"), "{err}");
    assert!(err.contains("spec file /specs/j1.md"), "{err}");
    assert!(shared.store.message("m1").unwrap().is_none());
    assert_eq!(shared.store.queued_count("w1").unwrap(), 0);

    let criteria = "c".repeat(800);
    let (_dir, shared) = task_bound_fixture(&format!(r#"1) [ ] "{criteria}""#));
    shared
        .store
        .create_task(
            "j1",
            "j1-t2",
            Some(&"T".repeat(3000)),
            Some("w1"),
            None,
            Some(&format!(r#"1) [ ] "{criteria}""#)),
            None,
            None,
            None,
        )
        .unwrap();
    let text = "p".repeat(500);
    shared
        .rpc_send(&json!({"alias": "w1", "text": text, "task": "j1-t2", "message": "m2"}))
        .unwrap();
    let body = stored_body(&shared, "m2");
    assert!(
        body.len() <= crate::adapter::pty::MAX_BODY,
        "{} bytes",
        body.len()
    );
    assert!(body.starts_with(&text), "{body}");
    assert!(body.contains("T…. Spec:"), "{body}");
    assert!(
        body.ends_with(&format!("Outstanding criteria: 1) [ ] \"{criteria}\".")),
        "{body}"
    );
}
