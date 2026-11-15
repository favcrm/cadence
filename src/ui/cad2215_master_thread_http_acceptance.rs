//! CAD-2215 signed-board master Home / app-thread read acceptance.
//! The app installation, its approved current digest, the exact General
//! conversation and the member grant are real (all through real RPCs); the
//! entry-to-message fixture bindings are synthetic while the message's task
//! and workspace-visible issue are real store bindings.
//! This is not cloud-authority or local-agent OS-isolation coverage.
use super::*;

fn response(rig: &Rig, cookie: &str, path: &str) -> (u16, Value) {
    let mut response = http_agent()
        .get(&format!("http://127.0.0.1:{}{path}", rig.board_port))
        .header("Host", &rig.public_host)
        .header("Cookie", cookie)
        .header("Origin", &format!("http://{}", rig.public_host))
        .header("Sec-Fetch-Site", "same-origin")
        .call()
        .expect("thread GET failed");
    let status = response.status().as_u16();
    let body = response
        .body_mut()
        .read_json()
        .expect("thread response is not JSON");
    (status, body)
}

fn page(rig: &Rig, cookie: &str, path: &str) -> Value {
    let (status, body) = response(rig, cookie, path);
    assert_eq!(status, 200, "thread GET {path} refused: {body}");
    body
}

fn append(
    shared: &crate::daemon::Shared,
    thread: &str,
    kind: &str,
    text: &str,
    message: &str,
) -> i64 {
    shared
        .store
        .thread_append_to(
            thread,
            crate::store::NewEntry {
                role: crate::store::ROLE_AGENT,
                kind,
                text,
                payload: None,
                message_id: Some(message),
            },
        )
        .unwrap()
        .expect("fixture thread not found")
}

#[test]
fn signed_member_master_home_and_operator_cards_do_not_change_visible_pages() {
    let (rig, member, owner, install, digest) =
        super::cad2215_app_conversation_grant_http_acceptance::master_thread_real_grant_fixture();
    let opts = ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", rig.pm.to_str().unwrap());
    let shared = crate::daemon::Shared::new(rig._dir.path(), &opts).unwrap();
    shared
        .store
        .register_agent(&crate::store::NewAgent {
            alias: "master",
            provider: "fake",
            endpoint_kind: "fake",
            role: "pm",
            cwd: rig._dir.path().to_str().unwrap(),
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .unwrap();
    let home = shared.store.ensure_thread("master").unwrap();
    let home_forward = "/api/threads/master?limit=1";
    let home_tail = "/api/threads/master?tail=1&limit=1";
    let home_before = "/api/threads/master?before=9223372036854775807&limit=1";
    let home_paths = [home_forward, home_tail, home_before];
    let home_empty: Vec<_> = home_paths
        .iter()
        .map(|path| response(&rig, &member, path))
        .collect();

    // Real message/task/issue binding; app entry linking below is synthetic.
    shared
        .store
        .enqueue_task(
            "master",
            "CAD2215_HOME_PRIVATE",
            None,
            "cad2215-master-bound-message",
            "user",
            Some("cad2215-visible-task"),
        )
        .unwrap();
    let home_secret = append(
        &shared,
        &home.id,
        crate::store::KIND_ARTIFACT,
        "CAD2215_HOME_PRIVATE_ARTIFACT",
        "cad2215-master-bound-message",
    );
    for (path, baseline) in home_paths.iter().zip(&home_empty) {
        let actual = response(&rig, &member, path);
        assert_eq!(
            &actual, baseline,
            "Home-only entries changed member response or metadata at {path}"
        );
        assert!(!actual.1.to_string().contains("CAD2215_HOME_PRIVATE"));
    }
    let operator_home = page(&rig, &owner, "/api/threads/master?tail=1&limit=100");
    assert!(
        operator_home.to_string().contains("CAD2215_HOME_PRIVATE"),
        "operator lost Home recovery: {operator_home}"
    );
    assert!(operator_home
        .to_string()
        .contains("CAD2215_HOME_PRIVATE_ARTIFACT"));
    assert!(home_secret > 0);

    // The installed app and current digest were approved through real RPCs.
    // The exact General conversation exists before granting any member access.
    let (app, created) = shared
        .store
        .conversation_create(
            "master",
            &install,
            None,
            crate::store::ConversationKind::General,
        )
        .unwrap();
    assert!(created);
    let first_seq = append(
        &shared,
        &app.id,
        crate::store::KIND_ASSISTANT_TEXT,
        "CAD2215_APP_VISIBLE_FIRST",
        "cad2215-master-bound-message",
    );
    let base = format!("/api/threads/master?conversation={}&", app.id);
    let forward = format!("{base}limit=1");
    let tail = format!("{base}tail=1&limit=1");
    let after = format!("{base}after={first_seq}&limit=1");
    for path in [&forward, &tail, &after] {
        let (status, body) = response(&rig, &member, path);
        assert_ne!(
            status, 200,
            "member accessed ungranted app conversation: {body}"
        );
        assert!(
            !body.to_string().contains("CAD2215_APP_VISIBLE_FIRST"),
            "ungranted app content leaked: {body}"
        );
    }
    let operator_before = page(&rig, &owner, &format!("{base}tail=1&limit=100"));
    assert!(
        operator_before
            .to_string()
            .contains("CAD2215_APP_VISIBLE_FIRST"),
        "operator recovery failed before grant: {operator_before}"
    );
    super::cad2215_app_conversation_grant_http_acceptance::master_thread_grant(
        &rig, &owner, &install, &digest, &app.id,
    );
    let member_forward = page(&rig, &member, &forward);
    let member_tail = page(&rig, &member, &tail);
    let member_after = page(&rig, &member, &after);
    assert!(
        member_forward
            .to_string()
            .contains("CAD2215_APP_VISIBLE_FIRST"),
        "authorized app entry missing: {member_forward}"
    );
    assert!(member_tail
        .to_string()
        .contains("CAD2215_APP_VISIBLE_FIRST"));
    assert_eq!(member_after["entries"], json!([]));
    let hidden_seq = append(
        &shared,
        &app.id,
        crate::store::KIND_ARTIFACT,
        "CAD2215_APP_PRIVATE_ARTIFACT",
        "cad2215-master-bound-message",
    );
    assert!(hidden_seq > first_seq);
    for (path, baseline) in [
        (&forward, &member_forward),
        (&tail, &member_tail),
        (&after, &member_after),
    ] {
        assert_eq!(
            &page(&rig, &member, path),
            baseline,
            "operator-only card changed member page or cursor metadata: {path}"
        );
    }
    let second_seq = append(
        &shared,
        &app.id,
        crate::store::KIND_ASSISTANT_TEXT,
        "CAD2215_APP_VISIBLE_SECOND",
        "cad2215-master-bound-message",
    );
    assert!(second_seq > hidden_seq);
    let following = page(&rig, &member, &after);
    assert!(
        following.to_string().contains("CAD2215_APP_VISIBLE_SECOND"),
        "hidden card consumed limit or blocked continuation: {following}"
    );
    assert!(!following
        .to_string()
        .contains("CAD2215_APP_PRIVATE_ARTIFACT"));
    let newest = page(&rig, &member, &tail);
    assert!(
        newest.to_string().contains("CAD2215_APP_VISIBLE_SECOND"),
        "tail omitted newest visible entry: {newest}"
    );
    let before = format!("{base}before={second_seq}&limit=1");
    let previous = page(&rig, &member, &before);
    assert!(
        previous.to_string().contains("CAD2215_APP_VISIBLE_FIRST"),
        "operator card blocked backward pagination: {previous}"
    );
    assert!(!previous
        .to_string()
        .contains("CAD2215_APP_PRIVATE_ARTIFACT"));
    let operator_app = page(&rig, &owner, &format!("{base}tail=1&limit=100"));
    for marker in [
        "CAD2215_APP_VISIBLE_FIRST",
        "CAD2215_APP_PRIVATE_ARTIFACT",
        "CAD2215_APP_VISIBLE_SECOND",
    ] {
        assert!(
            operator_app.to_string().contains(marker),
            "operator app recovery omitted {marker}: {operator_app}"
        );
    }
    super::cad2215_app_conversation_grant_http_acceptance::master_thread_revoke(
        &rig, &owner, &install, &app.id,
    );
    for path in [&forward, &tail, &after] {
        let (status, body) = response(&rig, &member, path);
        assert_ne!(
            status, 200,
            "revoked member retained app conversation: {body}"
        );
        for marker in [
            "CAD2215_APP_VISIBLE_FIRST",
            "CAD2215_APP_VISIBLE_SECOND",
            "CAD2215_APP_PRIVATE_ARTIFACT",
        ] {
            assert!(
                !body.to_string().contains(marker),
                "revoked member saw {marker}: {body}"
            );
        }
    }
    let operator_after = page(&rig, &owner, &format!("{base}tail=1&limit=100"));
    for marker in [
        "CAD2215_APP_VISIBLE_FIRST",
        "CAD2215_APP_PRIVATE_ARTIFACT",
        "CAD2215_APP_VISIBLE_SECOND",
    ] {
        assert!(
            operator_after.to_string().contains(marker),
            "operator recovery after revoke lost {marker}: {operator_after}"
        );
    }
}
