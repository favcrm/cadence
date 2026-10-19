// CAD-1568 guard acceptance authored by the ticket author/integration manager,
// independently of the server implementer. Included in team_assistants_refusal
// to reuse its real daemon + signed-JWKS fixture. Implementation writers must
// not edit or weaken this check.

fn cad1568_cli_assertion(fx: &Fx, subject: &str, role: &str, scope: &str, nonce: &str) -> String {
    cad1568_cli_assertion_for(
        fx,
        subject,
        role,
        scope,
        nonce,
        "user",
        &format!("users/{subject}"),
    )
}

fn cad1568_cli_assertion_for(
    fx: &Fx,
    subject: &str,
    role: &str,
    scope: &str,
    nonce: &str,
    kind: &str,
    actor: &str,
) -> String {
    let now = crate::issue::time::now_epoch();
    let header = URL_SAFE_NO_PAD.encode(
        serde_json::to_vec(&json!({
            "alg": "EdDSA", "typ": "agenticos-wiki-actor/1", "kid": KID
        }))
        .unwrap(),
    );
    let claims = URL_SAFE_NO_PAD.encode(
        serde_json::to_vec(&json!({
            "iss": fx.issuer,
            "aud": format!("http://{}", fx.host),
            "sub": subject,
            "actor": actor,
            "principal_kind": kind,
            "role_at_issue": role,
            "organization_id": ORG,
            "scope": [scope],
            "credential_id": format!("credential-{subject}"),
            "iat": now,
            "exp": now + 120,
            "jti": nonce
        }))
        .unwrap(),
    );
    let signed = format!("{header}.{claims}");
    format!(
        "{signed}.{}",
        URL_SAFE_NO_PAD.encode(fx.key.sign(signed.as_bytes()).as_ref())
    )
}

#[test]
fn cad1568_independent_retry_and_readback_cannot_cross_principals() {
    let fx = Fx::new();
    let cwd = fx.dir.path().join("offline-master");
    std::fs::create_dir_all(&cwd).unwrap();
    // A real registered but offline recipient; no provider/model is launched.
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
    let message_id = "cad1568-independent-request";
    let send = |proof: String, id: &str| {
        json!({
            "cli_envelope": proof,
            "alias": "master",
            "text": "coordinate the synthetic tender kit",
            "message": id
        })
    };
    let accepted = fx.op(
        "cli_message_send",
        send(
            cad1568_cli_assertion(&fx, "owner_a", "owner", "cli.write", "send-a"),
            message_id,
        ),
    );
    assert_eq!(accepted["state"], "queued", "offline is not completed");
    assert_eq!(accepted["duplicate"], false);
    let duplicate = fx.op(
        "cli_message_send",
        send(
            cad1568_cli_assertion(&fx, "owner_a", "owner", "cli.write", "retry-a"),
            message_id,
        ),
    );
    assert_eq!(duplicate["duplicate"], true);
    let second_owner = fx.op(
        "cli_message_send",
        send(
            cad1568_cli_assertion(&fx, "owner-a", "owner", "cli.write", "own-send-b"),
            "cad1568-second-owner-message",
        ),
    );
    assert_eq!(
        accepted["sender"], second_owner["sender"],
        "fixture must collide in display attribution to isolate canonical ownership",
    );
    let before = fx.shared.store.message(message_id).unwrap().unwrap();
    let other_owner = fx.op_err(
        "cli_message_send",
        send(
            cad1568_cli_assertion(&fx, "owner-a", "owner", "cli.write", "send-b"),
            message_id,
        ),
    );
    assert!(
        other_owner.is_err(),
        "another principal received a duplicate receipt: {other_owner:?}"
    );
    let after = fx.shared.store.message(message_id).unwrap().unwrap();
    assert_eq!(after.body, before.body);
    assert_eq!(after.sender, before.sender);
    assert_eq!(after.state, before.state);

    let own_read = fx.op(
        "cli_message_read",
        json!({
            "cli_envelope": cad1568_cli_assertion(&fx, "owner_a", "owner", "cli.read", "read-a"),
            "message": message_id
        }),
    );
    assert_eq!(own_read["state"], "queued");
    assert_eq!(own_read["text"], "coordinate the synthetic tender kit");
    assert!(own_read.get("turn_id").is_none());
    let foreign_read = fx.op_err(
        "cli_message_read",
        json!({
            "cli_envelope": cad1568_cli_assertion(&fx, "owner-a", "owner", "cli.read", "read-b"),
            "message": message_id
        }),
    );
    assert!(
        foreign_read.is_err(),
        "another principal read private message text: {foreign_read:?}"
    );
    let absent = fx.op_err(
        "cli_message_read",
        json!({
            "cli_envelope": cad1568_cli_assertion(&fx, "owner-a", "owner", "cli.read", "read-absent"),
            "message": "cad1568-never-created"
        }),
    ).unwrap_err();
    let foreign = foreign_read.unwrap_err();
    assert_eq!(foreign.code(), Some("not_found"));
    assert_eq!(
        foreign.code(),
        absent.code(),
        "foreign IDs expose message existence"
    );
    assert_eq!(
        foreign.to_string(),
        absent.to_string(),
        "foreign IDs expose message existence"
    );

    // An enrolled child has its own actor but may share the parent's sub.
    let child_read = fx
        .op_err(
            "cli_message_read",
            json!({
                "cli_envelope": cad1568_cli_assertion_for(
                    &fx, "owner_a", "owner", "cli.read", "read-child", "agent", "agents/child-a"
                ),
                "message": message_id
            }),
        )
        .unwrap_err();
    assert_eq!(
        child_read.code(),
        Some("not_permitted"),
        "child inherited human read authority"
    );

    let mut unsupported = send(
        cad1568_cli_assertion(&fx, "owner_a", "owner", "cli.write", "assistant-send"),
        "cad1568-assistant-must-not-enqueue",
    );
    unsupported["alias"] = json!("assist-ownera");
    let refused = fx.op_err("cli_message_send", unsupported).unwrap_err();
    assert_eq!(refused.code(), Some("recipient_class_unsupported"));
    assert!(fx
        .shared
        .store
        .message("cad1568-assistant-must-not-enqueue")
        .unwrap()
        .is_none());

    for (subject, role, scope, id) in [
        (
            "member-a",
            "member",
            "cli.write",
            "cad1568-member-master-refused",
        ),
        ("owner_a", "owner", "cli.read", "cad1568-read-scope-refused"),
    ] {
        let refused = fx.op_err(
            "cli_message_send",
            send(cad1568_cli_assertion(&fx, subject, role, scope, id), id),
        );
        assert!(
            refused.is_err(),
            "unauthorized master send accepted: {refused:?}"
        );
        assert!(
            fx.shared.store.message(id).unwrap().is_none(),
            "refusal enqueued work"
        );
    }

    // The synthetic recipient reports through the actual durable completion
    // primitive. Readback must distinguish that terminal result from enqueue.
    fx.shared
        .store
        .mark_running(message_id, "cad1568-private-turn")
        .unwrap();
    let running = fx.shared.store.message(message_id).unwrap().unwrap();
    fx.shared
        .store
        .finish(
            &running,
            "completed",
            &json!({
                "text": "synthetic kit inventory reported",
                "turn_id": "cad1568-private-turn",
                "details": {"turn_id": "cad1568-nested-private", "workbook": "checked"}
            }),
            None,
        )
        .unwrap();
    let completed = fx.op(
        "cli_message_read",
        json!({
            "cli_envelope": cad1568_cli_assertion(&fx, "owner_a", "owner", "cli.read", "read-completed"),
            "message": message_id
        }),
    );
    assert_eq!(completed["state"], "completed");
    assert_eq!(
        completed["result"]["text"],
        "synthetic kit inventory reported"
    );
    assert_eq!(completed["result"]["details"]["workbook"], "checked");
    assert_eq!(completed["awaiting_report"], false);
    assert!(completed["result"].get("turn_id").is_none());
    assert!(completed["result"]["details"].get("turn_id").is_none());
}
