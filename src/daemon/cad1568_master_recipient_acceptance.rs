// CAD-1568 independent acceptance authored by cad1568-spec-68c5 from the
// ticket and /tmp/cad1568-revision-contract.md. Product implementers must
// not edit or weaken this check. Included in team_assistants_refusal to
// exercise its real daemon dispatch and signed-envelope/JWKS verification.
// Native identifiers are fixture setup only; no provider is launched.

#[test]
fn cad1568_master_native_collision_refused_then_canonical_home_receipt_preserved() {
    // Exercise both native lookup columns independently: either used to
    // let a request spelling "master" select a different registered worker.
    for native_field in ["thread", "session"] {
        let fx = Fx::new();
        let worker = "cad1568-native-collision-worker";
        let subject = "cad1568-canonical-owner";
        let message = "cad1568-canonical-master-message";
        let body = "coordinate the synthetic wiki reference";
        let register = |alias: &str| {
            let cwd = fx.dir.path().join(alias);
            std::fs::create_dir_all(&cwd).unwrap();
            fx.shared
                .store
                .register_agent(&crate::store::NewAgent {
                    alias,
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
        };
        register(worker);
        fx.shared
            .store
            .set_identity(
                worker,
                &crate::adapter::Identity {
                    thread_id: if native_field == "thread" {
                        "master".into()
                    } else {
                        "cad1568-unrelated-thread".into()
                    },
                    session_id: if native_field == "session" {
                        "master".into()
                    } else {
                        "cad1568-unrelated-session".into()
                    },
                    model: None,
                    effort: None,
                    pid: 0,
                    endpoint: None,
                    generation: None,
                    attach: None,
                },
            )
            .unwrap();
        assert!(fx.shared.store.agent_opt("master").unwrap().is_none());
        assert_eq!(
            fx.shared
                .store
                .agent_by_native("master")
                .unwrap()
                .unwrap()
                .alias,
            worker,
            "the native-ID collision must exist before testing the guard"
        );
        assert!(fx.shared.store.messages(worker).unwrap().is_empty());
        let send = |nonce: &str| {
            json!({
                "cli_envelope": cad1568_cli_assertion(
                    &fx, subject, "owner", "cli.write", nonce
                ),
                "alias": "master",
                "text": body,
                "message": message
            })
        };
        // Valid human-owner signature and scope: the refusal must name
        // the missing canonical recipient, not a credential failure.
        let refused = fx
            .op_err("cli_message_send", send("canonical-master-absent"))
            .unwrap_err();
        assert_eq!(
            refused.code(),
            Some("not_found"),
            "{native_field} collision must refuse the absent exact master: {refused}"
        );
        assert!(fx.shared.store.message(message).unwrap().is_none());
        assert!(
            fx.shared.store.messages(worker).unwrap().is_empty(),
            "refused master send reached the colliding worker"
        );
        assert!(fx.shared.store.thread(worker).unwrap().is_none());
        assert!(fx.shared.store.thread("master").unwrap().is_none());

        // Positive control keeps the collision in place, adds the actual
        // canonical recipient and uses a fresh signed nonce. Start with
        // no Home: the send must create and bind it transactionally.
        register("master");
        assert!(fx.shared.store.thread("master").unwrap().is_none());
        let accepted = fx.op("cli_message_send", send("canonical-master-present"));
        assert_eq!(accepted["message"], message);
        assert_eq!(accepted["alias"], "master");
        assert_eq!(accepted["state"], "queued", "offline is not running");
        assert_eq!(accepted["duplicate"], false);
        assert_eq!(accepted["workspace_id"], ORG);
        assert_eq!(accepted["sender_subject"], subject);
        assert_eq!(accepted["sender"], format!("cli-{subject}"));
        let home = fx.shared.store.thread("master").unwrap().unwrap();
        assert!(!home.id.is_empty());
        assert_eq!(home.alias, "master");
        assert!(home.install_id.is_none(), "CLI send must target Home");
        let landed = fx
            .shared
            .store
            .message_conversation("master", message)
            .unwrap()
            .expect("first CLI send must persist its conversation entry");
        assert_eq!(landed.id, home.id);
        assert_eq!(accepted["conversation"], home.id);
        let stored = fx.shared.store.message(message).unwrap().unwrap();
        assert_eq!(stored.alias, "master");
        assert_eq!(stored.body, body);
        assert_eq!(stored.state, "queued");

        let duplicate = fx.op("cli_message_send", send("canonical-master-duplicate"));
        assert_eq!(duplicate["duplicate"], true);
        assert_eq!(duplicate["state"], "queued");
        let owned = fx.op(
            "cli_message_read",
            json!({
                "cli_envelope": cad1568_cli_assertion(
                    &fx, subject, "owner", "cli.read", "canonical-master-owned-read"
                ),
                "message": message
            }),
        );
        assert_eq!(owned["state"], "queued");
        assert_eq!(owned["text"], body);
        for field in [
            "message",
            "alias",
            "workspace_id",
            "sender_subject",
            "sender",
            "conversation",
        ] {
            assert_eq!(
                duplicate[field], accepted[field],
                "duplicate changed {field}"
            );
            assert_eq!(owned[field], accepted[field], "owned read changed {field}");
        }
        assert_eq!(fx.shared.store.queued_count("master").unwrap(), 1);
        let entries = fx
            .shared
            .store
            .thread_entries_of(&home.id, 0, 100)
            .unwrap();
        assert_eq!(
            entries
                .iter()
                .filter(|entry| entry.message_id.as_deref() == Some(message))
                .count(),
            1,
            "duplicate must not append a second message entry"
        );
        assert!(fx.shared.store.messages(worker).unwrap().is_empty());
        assert_eq!(
            fx.shared
                .store
                .message_conversation("master", message)
                .unwrap()
                .unwrap()
                .id,
            home.id,
            "retry/read must retain the original persisted Home binding"
        );
    }
}
