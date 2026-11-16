//! CAD-2215 independently authored hosted-CLI privacy acceptance.
//! The product implementer must not edit or weaken this check.
//! The fixture drives the real HTTP route, signature verification and daemon.

use super::*;

#[path = "cad2215_assistant_privacy_http_acceptance.rs"]
mod cad2215_assistant_privacy_http_acceptance;

fn cli_envelope(rig: &Rig, role: &str) -> String {
    let now = crate::issue::time::now_epoch();
    let (sub, actor) = match role {
        "owner" => ("cad2215-cli-owner", "operator"),
        "member" => ("cad2215-cli-member", "users/cad2215-cli-member"),
        _ => panic!("unsupported fixture role"),
    };
    let header = json!({"alg": "EdDSA", "typ": "agenticos-wiki-actor/1", "kid": KID});
    let claims = json!({
        "iss": rig.issuer,
        "aud": format!("http://{}", rig.public_host),
        "sub": sub,
        "actor": actor,
        "principal_kind": "user",
        "role_at_issue": role,
        "organization_id": ORG,
        "scope": ["cli.read"],
        "credential_id": format!("cad2215-{sub}"),
        "iat": now,
        "exp": now + 120,
        "jti": format!("cad2215-cli-{}", uuid::Uuid::new_v4()),
    });
    let signed = format!(
        "{}.{}",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).unwrap()),
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap()),
    );
    format!(
        "{signed}.{}",
        URL_SAFE_NO_PAD.encode(rig.key.sign(signed.as_bytes()).as_ref())
    )
}

fn cli_call(rig: &Rig, role: &str, verb: &str, args: Value) -> Value {
    let url = format!("http://127.0.0.1:{}/api/cli/{verb}", rig.board_port);
    let mut response = http_agent()
        .post(&url)
        .header("Host", &rig.public_host)
        .header("Content-Type", "application/json")
        .header(
            "Authorization",
            &format!("Bearer wikienv_{}", cli_envelope(rig, role)),
        )
        .send_json(args)
        .expect("hosted CLI request failed");
    assert_eq!(response.status().as_u16(), 200, "{role} {verb} refused");
    response
        .body_mut()
        .read_json()
        .expect("hosted CLI response is not JSON")
}

fn seed_messages(rig: &Rig) {
    let state = rig._dir.path();
    let opts = ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", rig.pm.to_str().unwrap());
    let shared = crate::daemon::Shared::new(state, &opts).unwrap();
    for (id, task, text) in [
        (
            "cad2215-cli-visible-message",
            "cad2215-visible-task",
            "CAD2215_VISIBLE_MESSAGE",
        ),
        (
            "cad2215-cli-secret-message",
            "cad2215-secret-task",
            "CAD2215_SECRET_MESSAGE",
        ),
        (
            "cad2215-cli-free-message",
            "cad2215-free-task",
            "CAD2215_FREE_MESSAGE",
        ),
    ] {
        shared
            .store
            .enqueue_task("cad2215-worker", text, None, id, "user", Some(task))
            .unwrap();
    }
}

#[test]
fn signed_hosted_cli_member_reads_are_scoped_and_owner_retains_recovery() {
    let rig = start(&[]);
    seed(&rig);
    seed_messages(&rig);

    for (verb, args) in [
        ("agent_list", json!({})),
        ("agent_show", json!({"alias": "cad2215-worker"})),
        ("status", json!({})),
    ] {
        let owner = cli_call(&rig, "owner", verb, args.clone());
        let member = cli_call(&rig, "member", verb, args);
        let owner_text = owner.to_string();
        let member_text = member.to_string();
        assert!(
            owner_text.contains("cad2215-worker"),
            "owner {verb} lacks fixture agent"
        );
        assert!(
            member_text.contains("cad2215-worker"),
            "member {verb} lacks fixture agent"
        );
        // IDs in each owner's actual response prove that the corresponding
        // member absence is not explained by an empty or disconnected daemon.
        for id in [
            "cad2215-visible-task",
            "cad2215-secret-task",
            "cad2215-free-task",
        ] {
            assert!(owner_text.contains(id), "owner {verb} cannot recover {id}");
        }
        assert!(
            member_text.contains("cad2215-visible-task"),
            "member {verb} lost readable work"
        );
        for secret in [
            "cad2215-secret-job",
            "cad2215-secret-task",
            "Secret job",
            "Secret task",
            "cad2215-free-job",
            "cad2215-free-task",
            "Free job",
            "Free task",
            "cad2215-cli-secret-message",
            "CAD2215_SECRET_MESSAGE",
            "cad2215-cli-free-message",
            "CAD2215_FREE_MESSAGE",
        ] {
            assert!(
                !member_text.contains(secret),
                "member {verb} leaked {secret}"
            );
        }
        if verb == "agent_show" {
            for text in [
                "CAD2215_VISIBLE_MESSAGE",
                "CAD2215_SECRET_MESSAGE",
                "CAD2215_FREE_MESSAGE",
            ] {
                assert!(
                    owner_text.contains(text),
                    "owner show cannot recover {text}"
                );
            }
        }
    }
}

#[test]
fn signed_cli_show_scopes_terminal_history_before_limit_and_keeps_live_work() {
    let rig = start(&[]);
    seed(&rig);
    let state = rig._dir.path();
    let opts = ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", rig.pm.to_str().unwrap());
    let shared = crate::daemon::Shared::new(state, &opts).unwrap();
    for (id, task, body) in [
        (
            "cad2215-window-live",
            "cad2215-visible-task",
            "CAD2215_WINDOW_LIVE",
        ),
        (
            "cad2215-window-readable",
            "cad2215-visible-task",
            "CAD2215_WINDOW_READABLE",
        ),
        (
            "cad2215-window-private",
            "cad2215-secret-task",
            "CAD2215_WINDOW_PRIVATE",
        ),
    ] {
        shared
            .store
            .enqueue_task("cad2215-worker", body, None, id, "user", Some(task))
            .unwrap();
    }
    // Sequence, not wall-clock delay, proves readable is older than private.
    // The first row remains live: the history limit must not drop it.
    let db = rusqlite::Connection::open(state.join("cadence.sqlite3")).unwrap();
    assert_eq!(db.execute(
        "UPDATE messages SET state='completed' WHERE id IN ('cad2215-window-readable','cad2215-window-private')",
        [],
    ).unwrap(), 2);
    let (readable_seq, private_seq): (i64, i64) = db
        .query_row(
            "SELECT (SELECT seq FROM messages WHERE id='cad2215-window-readable'),
                (SELECT seq FROM messages WHERE id='cad2215-window-private')",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert!(
        readable_seq < private_seq,
        "fixture did not create the intended ordering"
    );

    let args = json!({"alias": "cad2215-worker", "limit": 1});
    let owner = cli_call(&rig, "owner", "agent_show", args.clone()).to_string();
    let member = cli_call(&rig, "member", "agent_show", args).to_string();
    assert!(
        owner.contains("CAD2215_WINDOW_PRIVATE"),
        "owner cannot recover newest private terminal row"
    );
    assert!(
        !owner.contains("CAD2215_WINDOW_READABLE"),
        "owner limit did not select the newest terminal row"
    );
    assert!(
        owner.contains("CAD2215_WINDOW_LIVE"),
        "owner lost live work under history limit"
    );
    assert!(
        member.contains("CAD2215_WINDOW_READABLE"),
        "hidden terminal row displaced readable history"
    );
    assert!(
        member.contains("CAD2215_WINDOW_LIVE"),
        "member lost readable live work under history limit"
    );
    assert!(
        !member.contains("CAD2215_WINDOW_PRIVATE"),
        "member received private terminal body"
    );
    assert!(
        !member.contains("cad2215-window-private"),
        "member received private terminal identifier"
    );
}
