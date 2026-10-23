// CAD-1767 G2 (member-assist-history half), written by the check author
// before the fix. Included in team_assistants_refusal so it uses that
// file's real member fixture (provisioned binding, signed envelope). The
// implementer must not edit or weaken this check.

/// A real `hire_proposal` entry (written by the store's own put path) sits
/// between two ordinary replies in the member's bound conversation. The
/// operator's view of the thread holds it (control); the member's
/// `member_assist_history` returns both replies and never the proposal, on
/// the forward branch and on both paged branches (`tail`, `before`).
///
/// Guard: `operator_only_entries_hidden` in `rpc_member_assist_history`
/// (daemon/members_rpc.rs).
#[test]
fn cad1767_member_history_never_carries_a_hire_proposal() {
    use crate::store::NewEntry;
    let _flag = AssistantFlagOn::arm();
    let fx = Fx::new();
    let rt = fx.member_runtime("sub-hp");
    let reply = |text: &str| {
        fx.shared
            .store
            .thread_append_to(
                &rt.thread_id,
                NewEntry {
                    role: "agent",
                    kind: "assistant_text",
                    text,
                    payload: None,
                    message_id: None,
                },
            )
            .unwrap()
            .expect("the member's bound thread exists")
    };
    reply("visible reply one");
    fx.shared
        .store
        .hire_proposal_put(
            &rt.thread_id,
            "m-cad1767-member",
            "Proposed hire: Accountant as ledger-needle",
            json!({"role": "accountant", "title": "Accountant", "name": "ledger-needle",
                   "job": "Chase invoices.", "digest": "c1767"}),
        )
        .unwrap();
    let last = reply("visible reply two");

    let all = fx
        .shared
        .store
        .thread_entries_of(&rt.thread_id, 0, 100)
        .unwrap();
    assert!(
        all.iter().any(|e| e.kind == "hire_proposal"),
        "the proposal was not written to the member's thread"
    );

    let env = fx.member_envelope("sub-hp");
    let mut wrong = Vec::new();
    for (branch, params) in [
        ("forward", json!({"member_envelope": env, "after": 0})),
        ("tail", json!({"member_envelope": env, "tail": true})),
        (
            "before",
            json!({"member_envelope": env, "before": last + 1}),
        ),
    ] {
        let page = fx.op("member_assist_history", params);
        let entries = page["entries"].as_array().unwrap();
        let texts: Vec<&str> = entries.iter().filter_map(|e| e["text"].as_str()).collect();
        assert!(
            texts.contains(&"visible reply one") && texts.contains(&"visible reply two"),
            "{branch}: the member lost an ordinary entry: {page}"
        );
        if entries.iter().any(|e| e["kind"] == json!("hire_proposal"))
            || page.to_string().contains("ledger-needle")
        {
            wrong.push(format!("{branch}: {page}"));
        }
    }
    assert!(
        wrong.is_empty(),
        "a member received a hire proposal: {wrong:#?}"
    );
}
