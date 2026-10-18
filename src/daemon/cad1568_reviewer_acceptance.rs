// CAD-1568 guard acceptance authored independently of both the server
// implementer and the ticket author's check — a second signer on the
// parent/child principal boundary (AGENTS.md "Gates and security work").
// Included at the END of team_assistants_refusal so it runs on the same
// real daemon + signed-JWKS fixture (`Fx`, `op`, `op_err`,
// `cad1568_cli_assertion{,_for}` mint real Ed25519 envelopes over the
// fixture key; the platform serves the JWKS the daemon's own re-verify
// fetches). Implementation writers must not edit or weaken this file.
//
// The ticket's boundary this proves end to end: "CLI delegation must
// remain within the consenting human's existing scopes" — a valid,
// correctly signed cli envelope naming an enrolled CHILD AGENT
// (`principal_kind: "agent"`, actor `agents/<id>`) that shares its owner
// parent's `sub` must inherit NOTHING of that human's messaging plane:
// it cannot read the parent's durable CLI message and cannot enqueue a
// master send. Every refused case is paired with the human owner control
// in the same fixture, and the refusal must name the permission boundary
// (`not_permitted`) — never `signature_invalid` (the envelope verifies)
// or `insufficient_scope` (the scope grant is exactly right) — and must
// leave no durable effect.

#[test]
fn cad1568_reviewer_child_principal_inherits_no_owner_cli_authority() {
    let fx = Fx::new();
    // A real registered but offline recipient; no provider/model launches —
    // the send stays durably `queued` for readback.
    let cwd = fx.dir.path().join("reviewer-offline-master");
    std::fs::create_dir_all(&cwd).unwrap();
    fx.shared
        .store
        .register_agent(&crate::store::NewAgent {
            alias: "master",
            provider: "inbox",
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

    let parent_sub = "owner-parent";
    let parent_message = "cad1568-reviewer-parent-message";
    let child_actor = "agents/child-of-owner-parent";

    // Positive control: the verified human OWNER sends to master and the
    // durable queued receipt carries his stable id — the fixture's send
    // path is live before the refusal is judged.
    let accepted = fx.op(
        "cli_message_send",
        json!({
            "cli_envelope": cad1568_cli_assertion(
                &fx, parent_sub, "owner", "cli.write", "rvw-parent-send"
            ),
            "alias": "master",
            "text": "parent coordination request",
            "message": parent_message
        }),
    );
    assert_eq!(accepted["message"], parent_message);
    assert_eq!(accepted["state"], "queued", "offline is not completed");
    assert_eq!(accepted["duplicate"], false);
    let stored = fx
        .shared
        .store
        .message(parent_message)
        .unwrap()
        .unwrap();
    assert_eq!(
        stored.body, "parent coordination request",
        "the control send never reached the durable store"
    );
    // Positive control: the same human reads his own CLI message back.
    let own_read = fx.op(
        "cli_message_read",
        json!({
            "cli_envelope": cad1568_cli_assertion(
                &fx, parent_sub, "owner", "cli.read", "rvw-parent-read"
            ),
            "message": parent_message
        }),
    );
    assert_eq!(own_read["state"], "queued");
    assert_eq!(own_read["text"], "parent coordination request");

    // Negative: the parent's child agent — its own actor, the parent's
    // `sub`, a VALID signature, owner role and the right scope — tries to
    // read the parent's CLI message. Verified ≠ authorized: the refusal
    // must be `not_permitted`, the human-principal boundary.
    let child_read = fx
        .op_err(
            "cli_message_read",
            json!({
                "cli_envelope": cad1568_cli_assertion_for(
                    &fx, parent_sub, "owner", "cli.read",
                    "rvw-child-read", "agent", child_actor
                ),
                "message": parent_message
            }),
        )
        .unwrap_err();
    assert_eq!(
        child_read.code(),
        Some("not_permitted"),
        "child inherited human read authority, or failed for a non-permission reason: {child_read}"
    );
    assert_ne!(
        child_read.code(),
        Some("signature_invalid"),
        "the child envelope must verify — the boundary, not the signature"
    );
    assert_ne!(
        child_read.code(),
        Some("insufficient_scope"),
        "the child envelope must carry the right grant — the boundary, not the scope"
    );

    // Negative: the same child tries to enqueue its own master send under
    // the parent's `sub`. The refusal must be the same permission boundary
    // and must leave NO durable row — a refused send enqueues nothing.
    let child_message = "cad1568-reviewer-child-send";
    let child_send = fx
        .op_err(
            "cli_message_send",
            json!({
                "cli_envelope": cad1568_cli_assertion_for(
                    &fx, parent_sub, "owner", "cli.write",
                    "rvw-child-send", "agent", child_actor
                ),
                "alias": "master",
                "text": "child impersonating the parent",
                "message": child_message
            }),
        )
        .unwrap_err();
    assert_eq!(
        child_send.code(),
        Some("not_permitted"),
        "child inherited human send authority, or failed for a non-permission reason: {child_send}"
    );
    assert!(
        fx.shared.store.message(child_message).unwrap().is_none(),
        "a refused child send left a durable message"
    );

    // Discriminator: a byte-level forgery of the same child envelope fails
    // the signature check instead — `not_permitted` can only be returned
    // by a principal the daemon fully verified.
    let mut forged = cad1568_cli_assertion_for(
        &fx, parent_sub, "owner", "cli.read", "rvw-forged", "agent", child_actor,
    );
    // Corrupt the FIRST char of the signature segment: the trailing char
    // of a 64-byte signature's base64url carries padding bits only, so a
    // flip there can decode identically and still verify.
    let sig_start = forged.rfind('.').unwrap() + 1;
    let flip = if forged.as_bytes()[sig_start] == b'A' {
        'B'
    } else {
        'A'
    };
    forged.replace_range(sig_start..=sig_start, &flip.to_string());
    let forgery = fx
        .op_err(
            "cli_message_read",
            json!({"cli_envelope": forged, "message": parent_message}),
        )
        .unwrap_err();
    assert_eq!(
        forgery.code(),
        Some("signature_invalid"),
        "a forged envelope must die at the signature check: {forgery}"
    );

    // The parent's durable message is untouched by every refusal above.
    let untouched = fx.shared.store.message(parent_message).unwrap().unwrap();
    assert_eq!(untouched.body, "parent coordination request");
    assert_eq!(untouched.state, "queued");
}
