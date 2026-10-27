use super::*;

#[test]
fn an_undeliverable_compaction_pack_is_settled_once() {
    let dir = tempfile::tempdir().unwrap();
    let opts = ServeOptions::default();
    let no_pm = dir.path().join("no-pm");
    opts.provider_env
        .set("CADENCE_PM_DIR", no_pm.to_str().unwrap());
    let shared = Shared::new(dir.path(), &opts).unwrap();
    let cwd = dir.path().to_str().unwrap().to_string();
    shared
        .store
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
    shared.store.ensure_thread("lead").unwrap();
    // Only the compaction note: nothing for a pack to carry.
    shared
        .store
        .thread_append(
            "lead",
            store::NewEntry {
                role: store::ROLE_SYSTEM,
                kind: store::KIND_MESSAGE,
                text: "compacted",
                payload: Some(json!({"event": crate::continuity::COMPACTED_EVENT})),
                message_id: None,
            },
        )
        .unwrap();
    assert!(shared.store.compaction_pending("lead").unwrap());
    for id in ["k1", "k2"] {
        shared
            .store
            .enqueue_sent(
                "lead",
                "an ask",
                None,
                id,
                "user",
                None,
                &store::Sender::Operator,
            )
            .unwrap();
        let Take::Message(m) = shared.store.take_queued("lead").unwrap() else {
            panic!("nothing queued");
        };
        assert_eq!(shared.continuity_prompt("lead", "fake", &m), "an ask");
        shared
            .store
            .finish(&m, "completed", &json!({"text": "ok"}), None)
            .unwrap();
    }
    assert!(!shared.store.compaction_pending("lead").unwrap());
    let settled = shared
        .store
        .thread_entries("lead", 0, 100)
        .unwrap()
        .into_iter()
        .filter(|e| e.payload.as_ref().and_then(|p| p.get("outcome")) == Some(&json!("skipped")))
        .count();
    assert_eq!(settled, 1, "settled once, not per turn");
}

#[test]
fn continuity_packs_skip_pty_and_cloud_endpoints() {
    let dir = tempfile::tempdir().unwrap();
    let opts = ServeOptions::default();
    // A tracker path that does not exist: never the host's ~/pm.
    let no_pm = dir.path().join("no-pm");
    opts.provider_env
        .set("CADENCE_PM_DIR", no_pm.to_str().unwrap());
    let shared = Shared::new(dir.path(), &opts).unwrap();
    let cwd = dir.path().to_str().unwrap().to_string();
    for (alias, provider, kind, packs) in [
        ("p-claude", "claude", "pty", false),
        ("p-devin", "devin", "pty", false),
        ("p-cursor", "cursor", "pty", false),
        ("c-devin", "devin", "cloud", false),
        ("m-claude", "claude", "managed", true),
        ("w-codex", "codex", "managed-ws", true),
    ] {
        shared
            .store
            .register_agent(&NewAgent {
                alias,
                provider,
                endpoint_kind: kind,
                role: "worker",
                cwd: &cwd,
                sandbox: "read-only",
                instructions: None,
                params: None,
                team_role: None,
                model_policy: None,
            })
            .unwrap();
        shared.store.ensure_thread(alias).unwrap();
        shared
            .store
            .enqueue(alias, "an earlier ask", None, &format!("{alias}-0"), "user")
            .unwrap();
        let Take::Message(first) = shared.store.take_queued(alias).unwrap() else {
            panic!("nothing queued for {alias}");
        };
        shared
            .store
            .finish(
                &first,
                "completed",
                &json!({"text": "an earlier answer"}),
                None,
            )
            .unwrap();
        shared
            .continuity_due
            .lock()
            .unwrap()
            .insert(alias.to_string(), crate::continuity::Reason::New);
        shared
            .store
            .thread_append(
                alias,
                store::NewEntry {
                    role: store::ROLE_SYSTEM,
                    kind: store::KIND_MESSAGE,
                    text: "compacted",
                    payload: Some(json!({"event": crate::continuity::COMPACTED_EVENT})),
                    message_id: None,
                },
            )
            .unwrap();
        shared
            .store
            .enqueue_sent(
                alias,
                "the ask",
                None,
                &format!("{alias}-1"),
                "user",
                None,
                &store::Sender::Operator,
            )
            .unwrap();
        let Take::Message(message) = shared.store.take_queued(alias).unwrap() else {
            panic!("nothing queued for {alias}");
        };
        let prompt = shared.continuity_prompt(alias, kind, &message);
        let delivered = shared
            .store
            .events_tail(alias, 50)
            .unwrap()
            .iter()
            .any(|e| e.kind == crate::continuity::PACK_EVENT);
        if packs {
            assert!(
                prompt.starts_with(crate::continuity::PACK_BEGIN),
                "{alias}: {prompt}"
            );
            assert!(prompt.ends_with("the ask"), "{alias}");
            assert!(prompt.contains("an earlier answer"), "{alias}");
            assert!(delivered, "{alias}");
        } else if kind == "pty" {
            // CAD-565: the paste is the delivery notice — the body
            // rides only inside its bounded preview.
            assert!(prompt.starts_with("[cadence] "), "{alias}");
            assert!(prompt.contains("the ask"), "{alias}");
            assert!(prompt.contains("cadence message read"), "{alias}");
            assert!(!delivered, "{alias}");
        } else {
            assert_eq!(prompt, "the ask", "{alias}");
            assert!(!delivered, "{alias}");
        }
    }
}
