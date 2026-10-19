// CAD-1584 independently authored by the in-chat acceptance reviewer from the
// canonical-master requirement and AGENTS.md HTTP parity rule. Product
// implementers must not edit or weaken this check. This is not a registered
// Cadence reviewer identity or an operator approval.
// The existing ProvisionHttpRig runs the real isolated daemon and public board;
// Fx supplies only the synthetic issuer/JWKS and real Ed25519 signing key.

fn cad1584_master_http_call(
    rig: &ProvisionHttpRig,
    verb: &str,
    envelope: &str,
    args: &Value,
) -> (u16, Value) {
    let body = serde_json::to_vec(args).unwrap();
    let mut socket = std::net::TcpStream::connect(("127.0.0.1", rig.board_port)).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    use std::io::Write as _;
    write!(
        socket,
        "POST /api/cli/{verb} HTTP/1.1\r\nHost: {}\r\n\
         Authorization: Bearer wikienv_{envelope}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n",
        rig.public_host,
        body.len()
    )
    .unwrap();
    socket.write_all(&body).unwrap();
    let mut response = Vec::new();
    std::io::Read::read_to_end(&mut socket, &mut response).unwrap();
    let split = response
        .windows(4)
        .position(|bytes| bytes == b"\r\n\r\n")
        .expect("CLI HTTP response has no header/body split");
    let header = std::str::from_utf8(&response[..split]).unwrap();
    let status = header
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let value = serde_json::from_slice(&response[split + 4..])
        .expect("CLI HTTP response must carry the real route JSON contract");
    (status, value)
}

#[test]
fn cad1584_http_master_native_collision_refused_then_canonical_home_preserved() {
    for native_field in ["thread", "session"] {
        let fx = Fx::new();
        let rig = ProvisionHttpRig::start(&fx);
        let shared = rig.store();
        let worker = "cad1584-http-native-collision-worker";
        let subject = "cad1584-http-canonical-owner";
        let message = "cad1584-http-canonical-master-message";
        let text = "coordinate the synthetic wiki reference over HTTP";
        let register = |alias: &str| {
            let cwd = rig._root.path().join(alias);
            std::fs::create_dir_all(&cwd).unwrap();
            shared
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
            shared.store.set_enabled(alias, false).unwrap();
        };
        register(worker);
        shared
            .store
            .set_identity(
                worker,
                &crate::adapter::Identity {
                    thread_id: if native_field == "thread" {
                        "master".into()
                    } else {
                        "cad1584-unrelated-thread".into()
                    },
                    session_id: if native_field == "session" {
                        "master".into()
                    } else {
                        "cad1584-unrelated-session".into()
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
        assert!(shared.store.agent_opt("master").unwrap().is_none());
        assert_eq!(
            shared
                .store
                .agent_by_native("master")
                .unwrap()
                .unwrap()
                .alias,
            worker,
            "the colliding native identity must exist before the HTTP refusal"
        );
        assert!(shared.store.messages(worker).unwrap().is_empty());
        assert!(shared.store.thread(worker).unwrap().is_none());
        assert!(shared.store.thread("master").unwrap().is_none());
        let args = json!({"alias": "master", "text": text, "message": message});
        let call_send = |nonce: &str| {
            cad1584_master_http_call(
                &rig,
                "message_send",
                &cad1568_cli_assertion(&fx, subject, "owner", "cli.write", nonce),
                &args,
            )
        };

        // Correct signature, human-owner role and scope traverse the actual
        // public HTTP verifier and board-to-daemon relay. An earlier refusal
        // at transport/signature/scope cannot satisfy this typed response.
        let (status, refused) = call_send("cad1584-http-master-absent");
        assert_eq!(status, 404, "{native_field}: {refused}");
        assert_eq!(refused["ok"], false);
        assert_eq!(refused["error"]["code"], "not_found");
        // Read the owned daemon store directly: an authorization error from
        // a remote message_read is never used as an absence oracle.
        assert!(shared.store.message(message).unwrap().is_none());
        assert!(shared.store.messages(worker).unwrap().is_empty());
        assert_eq!(shared.store.queued_count(worker).unwrap(), 0);
        assert!(shared.store.thread(worker).unwrap().is_none());
        assert!(shared.store.thread("master").unwrap().is_none());

        // Keep the collision; add the exact master and use fresh signed
        // nonces through the same route. No provider launches in this rig.
        register("master");
        let master = shared.store.agent("master").unwrap();
        assert!(shared.store.thread("master").unwrap().is_none());
        let (status, accepted) = call_send("cad1584-http-master-present");
        assert_eq!(status, 200, "{native_field}: {accepted}");
        assert_eq!(accepted["message"], message);
        assert_eq!(accepted["alias"], "master");
        assert_eq!(accepted["workspace_id"], ORG);
        assert_eq!(accepted["sender_subject"], subject);
        assert_eq!(accepted["sender"], format!("cli-{subject}"));
        assert_eq!(accepted["state"], "queued");
        assert_eq!(accepted["duplicate"], false);
        let home = shared.store.thread("master").unwrap().unwrap();
        assert!(!home.id.is_empty());
        assert_eq!(home.alias, "master");
        assert!(home.install_id.is_none());
        assert_eq!(accepted["conversation"], home.id);
        let stored = shared.store.message(message).unwrap().unwrap();
        assert_eq!(stored.alias, "master");
        assert_eq!(stored.recipient_uid.as_deref(), Some(master.uid.as_str()));
        assert_eq!(stored.body, text);
        assert_eq!(stored.state, "queued");
        assert_eq!(
            shared
                .store
                .message_conversation("master", message)
                .unwrap()
                .unwrap()
                .id,
            home.id
        );

        let (status, duplicate) = call_send("cad1584-http-master-duplicate");
        assert_eq!(status, 200, "{duplicate}");
        assert_eq!(duplicate["duplicate"], true);
        assert_eq!(duplicate["state"], "queued");
        let (status, owned) = cad1584_master_http_call(
            &rig,
            "message_read",
            &cad1568_cli_assertion(
                &fx,
                subject,
                "owner",
                "cli.read",
                "cad1584-http-master-owned-read",
            ),
            &json!({"message": message}),
        );
        assert_eq!(status, 200, "{owned}");
        assert_eq!(owned["state"], "queued");
        assert_eq!(owned["text"], text);
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
        assert_eq!(shared.store.queued_count("master").unwrap(), 1);
        assert_eq!(
            shared
                .store
                .thread_entries_of(&home.id, 0, 100)
                .unwrap()
                .iter()
                .filter(|entry| entry.message_id.as_deref() == Some(message))
                .count(),
            1
        );
        assert!(shared.store.messages(worker).unwrap().is_empty());
        assert_eq!(shared.store.queued_count(worker).unwrap(), 0);
        assert!(shared.store.thread(worker).unwrap().is_none());
    }
}
