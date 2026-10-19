// Independently authored CAD-1584 selected-recipient lifecycle acceptance.
// This deliberately starts at the selected-row/shared-send boundary: the real
// CLI verifier derives authority, then the exact master row is retained while
// lifecycle setup removes/replaces it before send_with_expected. It does not
// claim a deterministic interleaving inside signed cli_message_send or HTTP.
// No production hooks, provider processes or existing checks are changed.

#[test]
fn cad1584_selected_uid_lifecycle_fence_refuses_removal_and_replacement() {
    for native_field in ["thread", "session"] {
        for lifecycle in ["unchanged", "removed", "replaced"] {
            let fx = Fx::new();
            let worker = "cad1584-lifecycle-collision-worker";
            let subject = "cad1584-lifecycle-owner";
            let message = "cad1584-selected-uid-message";
            let body = "coordinate only with the selected canonical master";
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
            register("master");
            fx.shared
                .store
                .set_identity(
                    worker,
                    &crate::adapter::Identity {
                        thread_id: if native_field == "thread" {
                            "master".into()
                        } else {
                            "cad1584-other-thread".into()
                        },
                        session_id: if native_field == "session" {
                            "master".into()
                        } else {
                            "cad1584-other-session".into()
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
            assert_eq!(
                fx.shared
                    .store
                    .agent_by_native("master")
                    .unwrap()
                    .unwrap()
                    .alias,
                worker
            );

            // Authenticate through the real daemon guard, not a fabricated
            // CliAuthority. Canonical selection below mirrors only the exact
            // row boundary; cli_recipient itself remains private to cli_rpc.
            let proof = json!({"cli_envelope": cad1568_cli_assertion(
                &fx, subject, "owner", "cli.write", "cad1584-selected-uid-owner"
            )});
            let principal = scoped(Asserted::Operator, || {
                fx.shared.cli_caller(
                    &proof,
                    std::process::id(),
                    "CAD-1584 lifecycle acceptance",
                    crate::cli_actor::Scope::Write,
                )
            })
            .unwrap();
            assert_eq!(principal.actor.role_at_issue, "owner");
            assert_eq!(principal.actor.principal_kind, "user");
            let selected = fx.shared.store.agent_opt("master").unwrap().unwrap();
            let authority = crate::store::CliAuthority {
                org: principal.config.company,
                sub: principal.actor.sub,
                actor: principal.actor.actor,
                handle: principal.actor.handle,
                credential_id: principal.actor.credential_id,
            };
            let sender_label = authority.sender_label();
            assert!(fx.shared.store.thread("master").unwrap().is_none());
            if lifecycle != "unchanged" {
                fx.shared
                    .store
                    .remove_agent(
                        "master",
                        false,
                        &json!({"by":"operator", "by_kind":"operator"}),
                    )
                    .unwrap();
                if lifecycle == "replaced" {
                    register("master");
                    assert_ne!(fx.shared.store.agent("master").unwrap().uid, selected.uid);
                } else {
                    assert!(fx.shared.store.agent_opt("master").unwrap().is_none());
                    assert_eq!(fx.shared.resolve_alias("master").unwrap(), worker);
                }
            }
            let send = json!({
                "alias": selected.alias,
                "text": body,
                "message": message,
                "source": "cli-cad1584-lifecycle-owner",
            });
            let result = fx.shared.send_with_expected(
                &send,
                &|_| Ok(crate::store::Sender::Cli(authority.clone())),
                &|_| Err(Error::rejected("CLI lifecycle acceptance cannot steer")),
                Some(&selected.uid),
            );
            if lifecycle == "unchanged" {
                let receipt = result.unwrap();
                assert_eq!(receipt["state"], "queued");
                assert_eq!(receipt["duplicate"], false);
                let stored = fx.shared.store.message(message).unwrap().unwrap();
                assert_eq!(stored.alias, "master");
                assert_eq!(stored.recipient_uid.as_deref(), Some(selected.uid.as_str()));
                assert_eq!(stored.sender.as_deref(), Some(sender_label.as_str()));
                assert_eq!(stored.body, body);
                let home = fx.shared.store.thread("master").unwrap().unwrap();
                assert_eq!(
                    fx.shared
                        .store
                        .message_conversation("master", message)
                        .unwrap()
                        .unwrap()
                        .id,
                    home.id
                );
                assert_eq!(fx.shared.store.queued_count("master").unwrap(), 1);
                let entries = fx.shared.store.thread_entries_of(&home.id, 0, 100).unwrap();
                assert_eq!(
                    entries
                        .iter()
                        .filter(|entry| entry.message_id.as_deref() == Some(message))
                        .count(),
                    1
                );
            } else {
                let refused = result.unwrap_err();
                assert!(
                    refused.to_string().contains(
                        "Recipient changed before the request was queued; nothing was sent"
                    ),
                    "{native_field}/{lifecycle} refused at an unexpected boundary: {refused}"
                );
                assert!(fx.shared.store.message(message).unwrap().is_none());
                assert!(fx.shared.store.thread("master").unwrap().is_none());
                assert!(fx
                    .shared
                    .store
                    .message_conversation("master", message)
                    .unwrap()
                    .is_none());
                assert_eq!(fx.shared.store.queued_count("master").unwrap(), 0);
                // messages() lists a live registration and rejects a removed
                // alias; message(id) above checks durable absence either way.
                if lifecycle == "replaced" {
                    assert!(fx.shared.store.messages("master").unwrap().is_empty());
                } else {
                    assert!(fx.shared.store.agent_opt("master").unwrap().is_none());
                }
            }
            assert!(
                fx.shared.store.messages(worker).unwrap().is_empty(),
                "{native_field}/{lifecycle} redirected the canonical send to the worker"
            );
            assert!(fx.shared.store.thread(worker).unwrap().is_none());
        }
    }
}
