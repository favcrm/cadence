//! Independent CAD-2215 app-conversation grant acceptance.
//! Synthetic live platform roster; not a local-agent isolation claim.
//!
//! Two surfaces, both real:
//! * the daemon's `app_member_access_{list,grant,revoke}` verbs, driven through
//!   the real `client::rpc` channel with the same verified-board-session proof
//!   the board relays (`grant_native`), including the live-roster, digest,
//!   project, private-job and in-flight-revoke guards;
//! * the public board's `member-access` HTTP family
//!   (`/api/app-installations/<id>/member-access[/revoke]`), driven over TCP
//!   with the real session cookie: list + live roster, one exact grant, the
//!   member/anonymous/forged/stale/unproven-roster refusals, and an exact
//!   revoke that withdraws only the chosen conversation. Those tests compile
//!   and are expected to be red until the board route lands; they never reach
//!   a fake echo, and every refusal is paired with a positive control.
#[path = "cad2215_human_room_socket_fence_acceptance.rs"]
mod cad2215_human_room_socket_fence_acceptance;
use super::*;

fn respond(request: tiny_http::Request, status: u16, body: Value) {
    let response = tiny_http::Response::from_string(body.to_string())
        .with_status_code(tiny_http::StatusCode(status))
        .with_header(tiny_http::Header::from_bytes("Content-Type", "application/json").unwrap());
    let _ = request.respond(response);
}

#[derive(Default)]
struct GrantRoster {
    owner_cookie: String,
    wrong_org: bool,
    target_present: bool,
    /// The platform answers 200 with a roster a reader cannot classify
    /// (a member with no role). The guard must treat it as unavailable.
    malformed: bool,
    allowed_cookies: Vec<String>,
}

/// The live-roster shape the fixture platform returns for one verified
/// owner cookie: the owner themself, one ordinary member, and the target.
fn roster_body(issuer: &str, org: &str, target_present: bool, malformed: bool) -> Value {
    if malformed {
        return json!({"issuer":issuer,"organization_id":org,
            "members":[{"subject_id":"ana","name":"Ana"}]});
    }
    let mut members = vec![
        json!({"subject_id":"grant-owner","name":"Owner","role":"owner"}),
        json!({"subject_id":"ben","name":"Ben","role":"member"}),
    ];
    if target_present {
        members.push(json!({"subject_id":"ana","name":"Ana","role":"member"}));
    }
    json!({"issuer":issuer,"organization_id":org,"members":members})
}

fn grant_rig(
    live_ana: Arc<AtomicBool>,
    roster: Arc<Mutex<GrantRoster>>,
    observer: Arc<dyn Fn(&str, &str) + Send + Sync>,
    hook: Option<crate::daemon::AppGrantForwardSnapshotHook>,
    human_room_page_fault: Option<Arc<std::sync::atomic::AtomicU8>>,
) -> Rig {
    grant_rig_with_authority(
        live_ana,
        roster,
        observer,
        hook,
        human_room_page_fault,
        None,
    )
}

// A second independently initialized workspace may use exactly the same
// verified platform issuer and signing key as the first. The first rig must
// stay alive while the second uses its platform server.
fn grant_rig_with_authority(
    live_ana: Arc<AtomicBool>,
    roster: Arc<Mutex<GrantRoster>>,
    observer: Arc<dyn Fn(&str, &str) + Send + Sync>,
    hook: Option<crate::daemon::AppGrantForwardSnapshotHook>,
    human_room_page_fault: Option<Arc<std::sync::atomic::AtomicU8>>,
    authority_rig: Option<&Rig>,
) -> Rig {
    let dir = tempfile::Builder::new()
        .prefix("cad2215-app-grant")
        .tempdir()
        .unwrap();
    let state = dir.path().to_path_buf();
    let pm = state.join("pm");
    crate::issue::Pm::init(&pm).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let mut threads = Vec::new();
    let key = Ed25519KeyPair::from_seed_unchecked(&[140; 32]).unwrap();
    let public_key = URL_SAFE_NO_PAD.encode(key.public_key().as_ref());
    let verify_public = key.public_key().as_ref().to_vec();
    let platform = loopback_server();
    let platform_port = match platform.server_addr() {
        tiny_http::ListenAddr::IP(a) => a.port(),
        _ => panic!("fixture platform bound unexpected address"),
    };
    let public_host = authority_rig
        .map(|rig| rig.public_host.clone())
        .unwrap_or_else(|| format!("localhost:{platform_port}"));
    let issuer = authority_rig
        .map(|rig| rig.issuer.clone())
        .unwrap_or_else(|| format!("http://{public_host}"));
    let authority: Arc<Mutex<Authority>> = Arc::new(Mutex::new(Authority {
        status: 200,
        ..Authority::default()
    }));
    for sub in ["ana", "ben"] {
        authority
            .lock()
            .unwrap()
            .members
            .insert(sub.to_string(), MemberAuthority::live(sub));
    }
    {
        let (stop, authority, issuer, public_host, roster) = (
            stop.clone(),
            authority.clone(),
            issuer.clone(),
            public_host.clone(),
            roster.clone(),
        );
        threads.push(std::thread::spawn(move || loop {
            if stop.load(Ordering::SeqCst) {
                break;
            }
            if let Ok(Some(request)) = platform.recv_timeout(Duration::from_millis(200)) {
                if request.url() == "/__platform/threads/roster" {
                    let cookie = request
                        .headers()
                        .iter()
                        .find(|h| h.field.equiv("Cookie"))
                        .map(|h| h.value.as_str().to_string())
                        .unwrap_or_default();
                    let snapshot = roster.lock().unwrap();
                    if cookie.is_empty()
                        || (cookie != snapshot.owner_cookie
                            && !snapshot
                                .allowed_cookies
                                .iter()
                                .any(|allowed| allowed == &cookie))
                    {
                        drop(snapshot);
                        respond(request, 401, json!({"error":"unauthorized"}));
                        continue;
                    }
                    let org = if snapshot.wrong_org {
                        "forged-organization"
                    } else {
                        ORG
                    };
                    let body =
                        roster_body(&issuer, org, snapshot.target_present, snapshot.malformed);
                    drop(snapshot);
                    respond(request, 200, body);
                } else {
                    answer_platform(
                        request,
                        &authority,
                        &issuer,
                        &public_host,
                        &public_key,
                        &verify_public,
                    );
                }
            }
        }));
    }
    let lookup: crate::daemon::MembershipLookupForFixture =
        Arc::new(move |_hash, sub, company, instance| {
            assert_eq!(company, ORG);
            assert_eq!(instance, "abc1");
            let role = match sub {
                "grant-owner" => crate::board_identity::Role::Operator,
                "ana" if live_ana.load(Ordering::SeqCst) => crate::board_identity::Role::Member,
                "ben" => crate::board_identity::Role::Member,
                _ => return Ok(None),
            };
            Ok(Some(role))
        });
    let mode =
        crate::daemon::TestMembershipMode::Native(crate::daemon::TestNativeMembershipContext {
            company: ORG.into(),
            instance: "abc1".into(),
            realm: crate::operator_auth::PublicRealm {
                issuer: issuer.clone(),
                company: ORG.into(),
                host: public_host.clone(),
            },
        });
    {
        let daemon_state = state.clone();
        let opts = ServeOptions {
            test_seam: true,
            stop: Some(stop.clone()),
            membership_mode_for_test: Some(mode),
            membership_lookup_for_test: Some(lookup),
            assist_request_sweep_observer: Some(observer),
            app_grant_forward_snapshot_hook: hook,
            human_room_page_fault,
            ..ServeOptions::default()
        };
        opts.provider_env
            .set("CADENCE_PM_DIR", pm.to_str().unwrap());
        threads.push(std::thread::spawn(move || {
            crate::daemon::serve_with(&daemon_state, opts).unwrap()
        }));
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while !state.join("cadence.sock").exists() || Seam::token_at(&state).is_none() {
            assert!(
                std::time::Instant::now() < deadline,
                "app grant fixture daemon failed to start"
            );
            std::thread::yield_now();
        }
    }
    {
        let opts = ServeOptions::default();
        opts.provider_env
            .set("CADENCE_PM_DIR", pm.to_str().unwrap());
        let shared = crate::daemon::Shared::new(&state, &opts).unwrap();
        for sub in ["ana", "ben"] {
            shared
                .store
                .assistant_bind(
                    ORG,
                    sub,
                    &format!("assist_{sub}"),
                    &format!("assist-agent-{sub}"),
                    Some("rev0"),
                )
                .unwrap();
        }
    }
    let mut board_port = 0;
    for port in 3110..=3199u16 {
        if port == platform_port {
            continue;
        }
        let (startup, ready) = std::sync::mpsc::channel();
        let board = ServeOpts {
            host: "127.0.0.1".into(),
            port,
            stop: Some(stop.clone()),
            startup: Some(startup),
            test_seam: true,
            allow_hosts: vec![public_host.clone()],
            public: Some(PublicBoard {
                host: public_host.clone(),
                issuer: issuer.clone(),
                company: ORG.into(),
                authorize_url: format!("{issuer}/authorize"),
            }),
            ..ServeOpts::default()
        };
        let (state, pm) = (state.clone(), pm.clone());
        let thread = std::thread::spawn(move || drop(crate::ui::serve(&state, &pm, &board)));
        match ready.recv_timeout(Duration::from_secs(30)).unwrap() {
            Ok(()) => {
                board_port = port;
                threads.push(thread);
                break;
            }
            Err(_) => {
                thread.join().unwrap();
            }
        }
    }
    assert!(board_port > 0, "app grant fixture board could not bind");
    let rig = Rig {
        _dir: dir,
        pm,
        board_port,
        public_host,
        issuer,
        key,
        authority,
        stop,
        _threads: threads,
    };
    enroll_queue_service(&rig);
    rig
}

fn grant_native(rig: &Rig, method: &str, params: Value) -> crate::Result<Value> {
    crate::test_seam::scoped(crate::test_seam::Asserted::Operator, || {
        crate::client::rpc(rig._dir.path(), method, params)
    })
}

fn grant_page(rig: &Rig, cookie: &str, conversation: &str) -> (u16, Value) {
    let path = format!("/api/threads/master?conversation={conversation}&limit=100");
    let mut response = http_agent()
        .get(&format!("http://127.0.0.1:{}{path}", rig.board_port))
        .header("Host", &rig.public_host)
        .header("Cookie", cookie)
        .header("Origin", &format!("http://{}", rig.public_host))
        .header("Sec-Fetch-Site", "same-origin")
        .call()
        .unwrap();
    let status = response.status().as_u16();
    let body = response.body_mut().read_json().unwrap();
    (status, body)
}

/// One real request against the public board's `member-access` family. The
/// status is the behaviour under test; the body is JSON when the board
/// produced JSON and stays opaque otherwise, so a refusal is measured by its
/// status and by the state it failed to change, never by matching copy.
fn member_access(
    rig: &Rig,
    method: &str,
    cookie: Option<&str>,
    install: &str,
    tail: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let url = format!(
        "http://127.0.0.1:{}/api/app-installations/{install}/member-access{tail}",
        rig.board_port
    );
    let mut response = match method {
        "GET" => {
            let mut request = http_agent()
                .get(&url)
                .header("Host", &rig.public_host)
                .header("Origin", &format!("http://{}", rig.public_host))
                .header("Sec-Fetch-Site", "same-origin");
            if let Some(cookie) = cookie {
                request = request.header("Cookie", cookie);
            }
            request.call()
        }
        "POST" => {
            let mut request = http_agent()
                .post(&url)
                .header("Host", &rig.public_host)
                .header("Origin", &format!("http://{}", rig.public_host))
                .header("Sec-Fetch-Site", "same-origin")
                .header("X-Cadence-Board", "1")
                // The board's write guard compares the type EXACTLY: the body
                // is serialized here so no `; charset=` suffix is added.
                .header("Content-Type", "application/json");
            if let Some(cookie) = cookie {
                request = request.header("Cookie", cookie);
            }
            let body = serde_json::to_vec(&body.expect("a member-access POST names its body"))
                .expect("the member-access body serializes");
            request.send(body)
        }
        other => panic!("unsupported member-access method {other}"),
    }
    .expect("member-access request");
    let status = response.status().as_u16();
    let bytes = response.body_mut().read_to_vec().unwrap_or_default();
    let value = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| json!({"opaque": String::from_utf8_lossy(&bytes).into_owned()}));
    (status, value)
}

fn grants_of(body: &Value) -> Vec<Value> {
    body["grants"]
        .as_array()
        .unwrap_or_else(|| panic!("member-access list has no grants array: {body}"))
        .clone()
}

struct MemberAccess {
    rig: Rig,
    roster: Arc<Mutex<GrantRoster>>,
    owner: String,
    ana: String,
    ben: String,
    install: String,
    digest: String,
    selected: String,
    other: String,
}

/// A real workspace app installation, approved, with two real app
/// conversations and one visible entry in each. Every request under test goes
/// to the public board; the install/approve calls here are setup through the
/// real local operator entry points, not the surface under test.
fn member_access_fixture() -> MemberAccess {
    let roster = Arc::new(Mutex::new(GrantRoster {
        target_present: true,
        ..GrantRoster::default()
    }));
    let (observer, _events) = queue_observer();
    let rig = grant_rig(
        Arc::new(AtomicBool::new(true)),
        roster.clone(),
        observer,
        None,
        None,
    );
    seed(&rig);
    let owner = login(&rig, "grant-owner", "owner");
    roster.lock().unwrap().owner_cookie = owner.clone();
    let ana = login(&rig, "ana", "member");
    let ben = login(&rig, "ben", "member");
    let source = format!("{}/tests/fixtures/apps/crm", env!("CARGO_MANIFEST_DIR"));
    let installed = grant_native(&rig, "app_workspace_install", json!({"source":source}))
        .expect("real workspace app install");
    let install = installed["install_id"]
        .as_str()
        .expect("real install ID")
        .to_string();
    let digest = installed["digest"]
        .as_str()
        .expect("real bundle digest")
        .to_string();
    grant_native(
        &rig,
        "app_local_install_approve",
        json!({"install_id":install,"digest":digest}),
    )
    .expect("real installed bundle approval");
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
    // The General conversation may be contextless; any other conversation the
    // exact-grant contract accepts must carry a real, proven app context.
    let context_config = crate::store::app_contexts::ContextConfig::new(
        "Members",
        std::collections::BTreeMap::new(),
    )
    .expect("the fixture context config");
    let context = shared
        .store
        .app_context_create(&install, &context_config, "cad2215-members")
        .expect("a real app context")["context"]["id"]
        .as_str()
        .expect("the app context ID")
        .to_string();
    let (selected, _) = shared
        .store
        .conversation_create(
            "master",
            &install,
            None,
            crate::store::ConversationKind::General,
        )
        .unwrap();
    let (other, _) = shared
        .store
        .conversation_create(
            "master",
            &install,
            Some(&context),
            crate::store::ConversationKind::Fresh,
        )
        .unwrap();
    assert_ne!(selected.id, other.id);
    for (id, marker, conversation) in [
        (
            "access-selected",
            "ACCESS_SELECTED_CONVERSATION",
            &selected.id,
        ),
        ("access-other", "ACCESS_OTHER_CONVERSATION", &other.id),
    ] {
        shared
            .store
            .enqueue_task("master", marker, None, id, "user", None)
            .unwrap();
        shared
            .store
            .thread_append_to(
                conversation,
                crate::store::NewEntry {
                    role: crate::store::ROLE_AGENT,
                    kind: crate::store::KIND_ASSISTANT_TEXT,
                    text: marker,
                    payload: None,
                    message_id: Some(id),
                },
            )
            .unwrap()
            .expect("app conversation missing");
    }
    MemberAccess {
        rig,
        roster,
        owner,
        ana,
        ben,
        install,
        digest,
        selected: selected.id,
        other: other.id,
    }
}

pub(super) fn master_thread_real_grant_fixture() -> (Rig, String, String, String, String) {
    let roster = Arc::new(Mutex::new(GrantRoster {
        target_present: true,
        ..GrantRoster::default()
    }));
    let (observer, _events) = queue_observer();
    let rig = grant_rig(
        Arc::new(AtomicBool::new(true)),
        roster.clone(),
        observer,
        None,
        None,
    );
    seed(&rig);
    let member = login(&rig, "ana", "member");
    let owner = login(&rig, "grant-owner", "owner");
    roster.lock().unwrap().owner_cookie = owner.clone();
    let source = format!("{}/tests/fixtures/apps/crm", env!("CARGO_MANIFEST_DIR"));
    let installed = grant_native(&rig, "app_workspace_install", json!({"source":source}))
        .expect("real workspace app install");
    let install = installed["install_id"]
        .as_str()
        .expect("real install ID")
        .to_string();
    let digest = installed["digest"]
        .as_str()
        .expect("real bundle digest")
        .to_string();
    grant_native(
        &rig,
        "app_local_install_approve",
        json!({"install_id":install,"digest":digest}),
    )
    .expect("real installed bundle approval");
    (rig, member, owner, install, digest)
}

pub(super) fn master_thread_grant(
    rig: &Rig,
    owner: &str,
    install: &str,
    digest: &str,
    conversation: &str,
) {
    let owner_token = owner.split_once('=').expect("owner session cookie").1;
    grant_native(rig, "app_member_access_grant", json!({"install_id":install,"member_sub":"ana", "conversation_id":conversation,"expected_digest":digest,"public_session":owner_token})).expect("verified owner + live roster exact grant");
}

pub(super) fn master_thread_revoke(rig: &Rig, owner: &str, install: &str, conversation: &str) {
    let owner_token = owner.split_once('=').expect("owner session cookie").1;
    grant_native(rig, "app_member_access_revoke", json!({"install_id":install,"member_sub":"ana", "conversation_id":conversation,"public_session":owner_token})).expect("verified owner exact grant revoke");
}

#[test]
fn exact_app_conversation_grant_never_widens_project_or_member_authority() {
    let _flag = AssistantFlagOn::arm();
    let live_ana = Arc::new(AtomicBool::new(true));
    let roster = Arc::new(Mutex::new(GrantRoster {
        target_present: true,
        ..GrantRoster::default()
    }));
    let (observer, events) = queue_observer();
    let rig = grant_rig(live_ana.clone(), roster.clone(), observer, None, None);
    seed(&rig);
    let owner = login(&rig, "grant-owner", "owner");
    roster.lock().unwrap().owner_cookie = owner.clone();
    let ana = login(&rig, "ana", "member");
    let ben = login(&rig, "ben", "member");
    let owner_token = owner.split_once('=').unwrap().1;
    let ben_token = ben.split_once('=').unwrap().1;
    let ana_token = ana.split_once('=').unwrap().1;

    let source = format!("{}/tests/fixtures/apps/crm", env!("CARGO_MANIFEST_DIR"));
    let installed = grant_native(&rig, "app_workspace_install", json!({"source":source}))
        .expect("real workspace app install");
    let install = installed["install_id"].as_str().unwrap().to_string();
    let digest = installed["digest"].as_str().unwrap().to_string();
    grant_native(
        &rig,
        "app_local_install_approve",
        json!({"install_id":install,"digest":digest}),
    )
    .expect("real current bundle approval");
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
    let (selected, _) = shared
        .store
        .conversation_create(
            "master",
            &install,
            None,
            crate::store::ConversationKind::General,
        )
        .unwrap();
    let (other, _) = shared
        .store
        .conversation_create(
            "master",
            &install,
            None,
            crate::store::ConversationKind::Fresh,
        )
        .unwrap();
    assert_ne!(selected.id, other.id);

    // A genuinely member-owned private job, created by the actual joined
    // worker with the member's own original credential, not a fake grant.
    let envelope = rig.envelope("ben", json!({}), json!({}));
    let (status, receipt) = rig.call(
        "job_request",
        &envelope,
        &json!({"title":"private app fixture", "request_key":"app-grant-private", "issue":"VIS-9"}),
    );
    assert_eq!(status, 200, "private request failed: {receipt}");
    let private_job = promoted_job(&rig, &events, "ben", &receipt);
    assert!(shared
        .store
        .assist_request_owns(ORG, "ben", &private_job)
        .unwrap());
    assert!(!shared
        .store
        .assist_request_owns(ORG, "ana", &private_job)
        .unwrap());
    let private_task = shared
        .store
        .tasks_for_job(&private_job)
        .unwrap()
        .first()
        .unwrap()
        .id
        .clone();
    let db = rusqlite::Connection::open(rig._dir.path().join("cadence.sqlite3")).unwrap();
    db.execute_batch("INSERT INTO jobs(id,title,spec_path,pm_alias,issue_id,repo,state,created,updated) VALUES('grant-sec-job','Secret','spec.md','pm','SEC-1','secret','open',1,1); INSERT INTO tasks(id,job_id,title,assignee,state,created,updated) VALUES('grant-sec-task','grant-sec-job','Secret task','cad2215-worker','running',1,1);").unwrap();
    for (id, marker, task, conversation) in [
        ("grant-unbound", "GRANT_UNBOUND_VISIBLE", None, &selected.id),
        ("grant-other", "GRANT_OTHER_CONVERSATION", None, &other.id),
        (
            "grant-other-readable",
            "GRANT_OTHER_READABLE_PROJECT",
            Some("cad2215-visible-task"),
            &other.id,
        ),
        (
            "grant-readable",
            "GRANT_READABLE_PROJECT",
            Some("cad2215-visible-task"),
            &selected.id,
        ),
        (
            "grant-private",
            "GRANT_PRIVATE_JOB",
            Some(private_task.as_str()),
            &selected.id,
        ),
        (
            "grant-sec-explicit",
            "GRANT_SEC_EXPLICIT",
            None,
            &selected.id,
        ),
        (
            "grant-sec-task",
            "GRANT_SEC_TASK_DERIVED",
            Some("grant-sec-task"),
            &selected.id,
        ),
        ("grant-absent-task", "GRANT_ABSENT_TASK", None, &selected.id),
        ("grant-cloud", "GRANT_CLOUD_ORIGIN", None, &selected.id),
    ] {
        shared
            .store
            .enqueue_task("master", marker, None, id, "user", task)
            .unwrap();
        shared
            .store
            .thread_append_to(
                conversation,
                crate::store::NewEntry {
                    role: crate::store::ROLE_AGENT,
                    kind: crate::store::KIND_ASSISTANT_TEXT,
                    text: marker,
                    payload: None,
                    message_id: Some(id),
                },
            )
            .unwrap()
            .expect("app conversation missing");
    }
    let other_bound = shared
        .store
        .message("grant-other-readable")
        .unwrap()
        .expect("other readable message missing");
    assert_eq!(other_bound.task_id.as_deref(), Some("cad2215-visible-task"));
    let other_task = shared.store.task("cad2215-visible-task").unwrap();
    let other_job = shared.store.job(&other_task.job_id).unwrap();
    assert_eq!(other_job.issue_id.as_deref(), Some("VIS-1"), "other conversation's positive-control message is not genuinely bound to the readable project");
    assert_eq!(
        db.execute(
            "UPDATE messages SET issue='SEC-1' WHERE id='grant-sec-explicit'",
            []
        )
        .unwrap(),
        1
    );
    assert_eq!(
        db.execute(
            "UPDATE messages SET issue=NULL WHERE id='grant-sec-task'",
            []
        )
        .unwrap(),
        1
    );
    assert_eq!(
        db.execute(
            "UPDATE messages SET task_id='grant-missing-task' WHERE id='grant-absent-task'",
            []
        )
        .unwrap(),
        1
    );
    assert_eq!(
        db.execute(
            "UPDATE messages SET source='cloud_agent' WHERE id='grant-cloud'",
            []
        )
        .unwrap(),
        1
    );
    let task_binding = shared.store.message("grant-sec-task").unwrap().unwrap();
    assert!(task_binding.issue.is_none());
    assert_eq!(task_binding.task_id.as_deref(), Some("grant-sec-task"));
    shared
        .store
        .mark_running("grant-unbound", "grant-unbound-turn")
        .unwrap();

    let params = json!({"install_id":install,"member_sub":"ana","conversation_id":selected.id,"expected_digest":digest,"public_session":owner_token});
    let before = grant_page(&rig, &ana, &selected.id);
    assert!(
        !before.1.to_string().contains("GRANT_UNBOUND_VISIBLE"),
        "ungranted member read app conversation"
    );
    assert!(
        !before.1.to_string().contains("GRANT_READABLE_PROJECT"),
        "ungranted selected conversation exposed its readable-project message: {before:?}"
    );
    let other_before = grant_page(&rig, &ana, &other.id);
    assert!(!other_before.1.to_string().contains("GRANT_OTHER_READABLE_PROJECT"), "ungranted other conversation exposed a readable-project message before grant: {other_before:?}");
    let native_other_before = grant_native(
        &rig,
        "thread_read",
        json!({"alias":"master","conversation":other.id,"limit":100,"public_session":ana_token}),
    );
    assert!(
        !format!("{native_other_before:?}").contains("GRANT_OTHER_READABLE_PROJECT"),
        "native ungranted other conversation leaked readable content: {native_other_before:?}"
    );
    let mut denied = params.clone();
    denied["public_session"] = json!(ben_token);
    assert!(
        grant_native(&rig, "app_member_access_grant", denied).is_err(),
        "member B granted app access"
    );
    let mut forged_sub = params.clone();
    forged_sub["public_session"] = json!(ben_token);
    forged_sub["sub"] = json!("grant-owner");
    assert!(
        grant_native(&rig, "app_member_access_grant", forged_sub).is_err(),
        "member forged the owner's subject"
    );
    let mut forged_org = params.clone();
    forged_org["public_session"] = json!(ben_token);
    forged_org["company"] = json!(ORG);
    assert!(
        grant_native(&rig, "app_member_access_grant", forged_org).is_err(),
        "member forged owner tenancy"
    );
    let mut forged = params.clone();
    forged["org"] = json!("forged-organization");
    assert!(
        grant_native(&rig, "app_member_access_grant", forged).is_err(),
        "forged organization was accepted"
    );
    let mut forged = params.clone();
    forged["member_sub"] = json!("not-on-roster");
    assert!(
        grant_native(&rig, "app_member_access_grant", forged).is_err(),
        "subject not on live roster was granted"
    );
    let mut stale = params.clone();
    stale["expected_digest"] = json!(format!("sha256:{}", "0".repeat(64)));
    assert!(
        grant_native(&rig, "app_member_access_grant", stale).is_err(),
        "stale digest was granted"
    );
    roster.lock().unwrap().wrong_org = true;
    assert!(
        grant_native(&rig, "app_member_access_grant", params.clone()).is_err(),
        "wrong-organization roster was accepted"
    );
    roster.lock().unwrap().wrong_org = false;
    roster.lock().unwrap().target_present = false;
    assert!(
        grant_native(&rig, "app_member_access_grant", params.clone()).is_err(),
        "absent target was granted"
    );
    roster.lock().unwrap().target_present = true;
    assert!(
        shared
            .store
            .app_member_grants_list(ORG, &install)
            .unwrap()
            .is_empty(),
        "a refused forged/stale/roster request left a grant"
    );
    grant_native(&rig, "app_member_access_grant", params.clone())
        .expect("verified owner + live roster grant denied");
    let native_thread = grant_native(&rig, "thread_read", json!({"alias":"master", "conversation":selected.id, "limit":100, "public_session":ana_token})).expect("native granted thread read");
    assert_eq!(
        native_thread[cadence_core::board_api::READER_SCOPE_FIELD],
        cadence_core::board_api::THREAD_SCOPE_ACK
    );
    assert!(native_thread.to_string().contains("GRANT_UNBOUND_VISIBLE"));
    let master_state = grant_native(&rig, "master_state", json!({"public_session":ana_token}))
        .expect("native granted master state");
    assert_eq!(
        master_state[cadence_core::board_api::READER_SCOPE_FIELD],
        cadence_core::board_api::MASTER_STATE_SCOPE_ACK
    );
    assert!(
        master_state.to_string().contains("grant-unbound"),
        "master state omitted admitted running message: {master_state}"
    );
    let agent_list = grant_native(
        &rig,
        "agent_list",
        json!({"board":true,"public_session":ana_token}),
    )
    .expect("native granted agent list");
    assert_eq!(
        agent_list[cadence_core::board_api::READER_SCOPE_FIELD],
        cadence_core::board_api::AGENT_LIST_SCOPE_ACK
    );
    assert!(
        agent_list.to_string().contains("grant-unbound"),
        "agent list omitted admitted running message: {agent_list}"
    );
    for (surface, value) in [
        ("thread", &native_thread),
        ("master", &master_state),
        ("agents", &agent_list),
    ] {
        for secret in [
            "grant-private",
            "grant-sec-explicit",
            "grant-sec-task",
            "grant-absent-task",
            "grant-cloud",
            "grant-other",
        ] {
            assert!(
                !value.to_string().contains(secret),
                "{surface} projection leaked {secret}: {value}"
            );
        }
    }
    let (status, visible) = grant_page(&rig, &ana, &selected.id);
    assert_eq!(status, 200, "granted conversation unavailable: {visible}");
    let body = visible.to_string();
    for marker in ["GRANT_UNBOUND_VISIBLE", "GRANT_READABLE_PROJECT"] {
        assert!(
            body.contains(marker),
            "granted reader missing positive control {marker}: {visible}"
        );
    }
    for marker in [
        "GRANT_OTHER_CONVERSATION",
        "GRANT_PRIVATE_JOB",
        "GRANT_SEC_EXPLICIT",
        "GRANT_SEC_TASK_DERIVED",
        "GRANT_ABSENT_TASK",
        "GRANT_CLOUD_ORIGIN",
    ] {
        assert!(!body.contains(marker), "grant leaked {marker}: {visible}");
    }
    let other_page = grant_page(&rig, &ana, &other.id);
    assert!(!other_page
        .1
        .to_string()
        .contains("GRANT_OTHER_CONVERSATION"));
    assert!(!other_page.1.to_string().contains("GRANT_OTHER_READABLE_PROJECT"), "grant for selected conversation widened to another readable-project conversation: {other_page:?}");
    let native_other = grant_native(
        &rig,
        "thread_read",
        json!({"alias":"master","conversation":other.id,"limit":100,"public_session":ana_token}),
    );
    assert!(
        !format!("{native_other:?}").contains("GRANT_OTHER_READABLE_PROJECT"),
        "native other conversation bypassed exact grant: {native_other:?}"
    );
    let (owner_other_status, owner_other) = grant_page(&rig, &owner, &other.id);
    assert_eq!(
        owner_other_status, 200,
        "operator cannot recover other conversation: {owner_other}"
    );
    assert!(
        owner_other
            .to_string()
            .contains("GRANT_OTHER_READABLE_PROJECT"),
        "operator lost other conversation's real readable-project entry: {owner_other}"
    );
    let ben_page = grant_page(&rig, &ben, &selected.id);
    assert!(!ben_page.1.to_string().contains("GRANT_UNBOUND_VISIBLE"));
    let (status, operator) = grant_page(&rig, &owner, &selected.id);
    assert_eq!(status, 200);
    for marker in [
        "GRANT_UNBOUND_VISIBLE",
        "GRANT_READABLE_PROJECT",
        "GRANT_PRIVATE_JOB",
        "GRANT_SEC_EXPLICIT",
        "GRANT_SEC_TASK_DERIVED",
        "GRANT_ABSENT_TASK",
    ] {
        assert!(
            operator.to_string().contains(marker),
            "operator recovery omitted {marker}: {operator}"
        );
    }
    let cloud_fixture = shared
        .store
        .message("grant-cloud")
        .unwrap()
        .expect("cloud fixture message disappeared");
    assert_eq!(
        cloud_fixture.source, "cloud_agent",
        "cloud provenance fixture is not real stored cloud source"
    );
    assert_eq!(
        operator["cloud_withheld"], "cloud_agent_authority_missing",
        "operator was not told cloud source authority is missing: {operator}"
    );
    assert!(
        !operator.to_string().contains("GRANT_CLOUD_ORIGIN"),
        "unproven cloud authority exposed the cloud-origin text"
    );
    let catalog = crate::issue::app_catalog::Catalog::load(&rig.pm).unwrap();
    let app_id = crate::issue::app_catalog::InstallationId::parse(&install).unwrap();
    let entry = catalog.entries().get(&app_id).unwrap();
    let base = rig.pm.join(".apps/installations").join(&install);
    let bundle = match &entry.bundle_revision {
        Some(rev) => base.join("revisions").join(rev).join("bundle"),
        None => base.join("bundle"),
    };
    let manifest = bundle.join("app.md");
    let old_manifest = std::fs::read(&manifest).unwrap();
    let mut changed_manifest = old_manifest.clone();
    changed_manifest.extend_from_slice(b"\n");
    std::fs::write(&manifest, &changed_manifest).unwrap();
    let changed = grant_page(&rig, &ana, &selected.id);
    assert!(
        !changed.1.to_string().contains("GRANT_UNBOUND_VISIBLE"),
        "digest drift retained app grant"
    );
    assert!(
        !changed.1.to_string().contains("GRANT_READABLE_PROJECT"),
        "digest drift leaked selected readable-project content: {changed:?}"
    );
    let other_drift = grant_page(&rig, &ana, &other.id);
    assert!(
        !other_drift
            .1
            .to_string()
            .contains("GRANT_OTHER_READABLE_PROJECT"),
        "digest drift exposed other conversation: {other_drift:?}"
    );
    let native_drift = grant_native(
        &rig,
        "thread_read",
        json!({"alias":"master","conversation":selected.id,"limit":100,"public_session":ana_token}),
    );
    assert!(
        !format!("{native_drift:?}").contains("GRANT_READABLE_PROJECT"),
        "native digest drift leaked selected readable-project content: {native_drift:?}"
    );
    std::fs::write(&manifest, old_manifest).unwrap();
    assert!(
        grant_page(&rig, &ana, &selected.id)
            .1
            .to_string()
            .contains("GRANT_UNBOUND_VISIBLE"),
        "restoring exact approved bundle failed to restore grant"
    );
    live_ana.store(false, Ordering::SeqCst);
    let (removed_status, removed_body) = grant_page(&rig, &ana, &selected.id);
    assert!(
        !removed_body.to_string().contains("GRANT_UNBOUND_VISIBLE"),
        "removed member retained grant: status={removed_status} body={removed_body}"
    );
    assert_ne!(
        removed_status, 200,
        "removed member session returned a successful page: {removed_body}"
    );
    live_ana.store(true, Ordering::SeqCst);
    // The authoritative missing-membership result permanently revoked the
    // old session; reinstatement must not silently resurrect that credential.
    let (old_status, old_body) = grant_page(&rig, &ana, &selected.id);
    assert!(!old_body.to_string().contains("GRANT_UNBOUND_VISIBLE"),
        "withdrawn session revived after membership reinstatement: status={old_status} body={old_body}");
    assert_ne!(
        old_status, 200,
        "withdrawn old session returned a successful page after reinstatement: {old_body}"
    );
    let ana_reinstated = login(&rig, "ana", "member");
    assert_ne!(
        ana_reinstated, ana,
        "fresh login reused withdrawn session token"
    );
    let (recovered_status, recovered_body) = grant_page(&rig, &ana_reinstated, &selected.id);
    assert_eq!(
        recovered_status, 200,
        "fresh reinstated member denied: status={recovered_status} body={recovered_body}"
    );
    assert!(
        recovered_body.to_string().contains("GRANT_UNBOUND_VISIBLE"),
        "fresh live member did not recover grant: status={recovered_status} body={recovered_body}"
    );
    grant_native(&rig, "app_member_access_revoke", json!({"install_id":install,"member_sub":"ana","conversation_id":selected.id,"public_session":owner_token})).expect("owner revoke failed");
    let (revoked_status, revoked_body) = grant_page(&rig, &ana_reinstated, &selected.id);
    assert!(!revoked_body.to_string().contains("GRANT_UNBOUND_VISIBLE"),
        "revoked grant still exposed unbound conversation: status={revoked_status} body={revoked_body}");
    assert!(!revoked_body.to_string().contains("GRANT_READABLE_PROJECT"), "revoked selected conversation leaked its readable-project content: status={revoked_status} body={revoked_body}");
    let other_after_revoke = grant_page(&rig, &ana_reinstated, &other.id);
    assert!(
        !other_after_revoke
            .1
            .to_string()
            .contains("GRANT_OTHER_READABLE_PROJECT"),
        "revocation exposed other readable-project conversation: {other_after_revoke:?}"
    );
    let reinstated_token = ana_reinstated.split_once('=').unwrap().1;
    let native_revoked = grant_native(
        &rig,
        "thread_read",
        json!({"alias":"master","conversation":selected.id,"limit":100,"public_session":reinstated_token}),
    );
    assert!(
        !format!("{native_revoked:?}").contains("GRANT_READABLE_PROJECT"),
        "native revoked selected conversation leaked readable-project content: {native_revoked:?}"
    );
    assert!(
        grant_page(&rig, &owner, &selected.id)
            .1
            .to_string()
            .contains("GRANT_UNBOUND_VISIBLE"),
        "revocation broke operator recovery"
    );
}

#[test]
fn granted_forward_reply_watermark_and_inflight_revoke_are_reproved() {
    let _flag = AssistantFlagOn::arm();
    let live_ana = Arc::new(AtomicBool::new(true));
    let roster = Arc::new(Mutex::new(GrantRoster {
        target_present: true,
        ..GrantRoster::default()
    }));
    let (observer, _events) = queue_observer();
    let armed = Arc::new(AtomicBool::new(false));
    let wanted = Arc::new(Mutex::new(String::new()));
    let (captured_tx, captured_rx) = std::sync::mpsc::channel::<String>();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let release_rx = Arc::new(Mutex::new(release_rx));
    let hook: crate::daemon::AppGrantForwardSnapshotHook = {
        let armed = armed.clone();
        let wanted = wanted.clone();
        Arc::new(move |id: &str| {
            if *wanted.lock().unwrap() != id {
                return;
            }
            if armed
                .compare_exchange(true, false, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
            {
                return;
            }
            captured_tx
                .send(id.to_string())
                .expect("grant snapshot witness receiver");
            release_rx
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(20))
                .expect("grant snapshot release");
        })
    };
    let rig = grant_rig(live_ana, roster.clone(), observer, Some(hook), None);
    seed(&rig);
    let owner = login(&rig, "grant-owner", "owner");
    roster.lock().unwrap().owner_cookie = owner.clone();
    let ana = login(&rig, "ana", "member");
    let owner_token = owner.split_once('=').unwrap().1;
    let ana_token = ana.split_once('=').unwrap().1;

    let source = format!("{}/tests/fixtures/apps/crm", env!("CARGO_MANIFEST_DIR"));
    let installed = grant_native(&rig, "app_workspace_install", json!({"source":source}))
        .expect("real workspace install");
    let install = installed["install_id"].as_str().unwrap().to_string();
    let digest = installed["digest"].as_str().unwrap().to_string();
    grant_native(
        &rig,
        "app_local_install_approve",
        json!({"install_id":install,"digest":digest}),
    )
    .expect("real current app approval");
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
    let (conversation, _) = shared
        .store
        .conversation_create(
            "master",
            &install,
            None,
            crate::store::ConversationKind::General,
        )
        .unwrap();
    *wanted.lock().unwrap() = conversation.id.clone();
    grant_native(&rig, "app_member_access_grant", json!({"install_id":install,"member_sub":"ana", "conversation_id":conversation.id,"expected_digest":digest,"public_session":owner_token})).expect("verified owner grant");

    shared
        .store
        .enqueue_task(
            "master",
            "GRANTED_VISIBLE_REPLY",
            None,
            "grant-watermark-msg",
            "user",
            None,
        )
        .unwrap();
    let reply_seq = shared
        .store
        .thread_append_to(
            &conversation.id,
            crate::store::NewEntry {
                role: crate::store::ROLE_AGENT,
                kind: crate::store::KIND_ASSISTANT_TEXT,
                text: "GRANTED_VISIBLE_REPLY",
                payload: None,
                message_id: Some("grant-watermark-msg"),
            },
        )
        .unwrap()
        .expect("append visible reply");
    let forward = grant_native(
        &rig,
        "thread_read",
        json!({"alias":"master", "conversation":conversation.id,
        "after":0, "limit":100, "public_session":ana_token}),
    )
    .expect("authorized granted forward page");
    assert_eq!(
        forward[cadence_core::board_api::READER_SCOPE_FIELD],
        cadence_core::board_api::THREAD_SCOPE_ACK
    );
    assert!(
        forward.to_string().contains("GRANTED_VISIBLE_REPLY"),
        "granted forward page omitted visible reply: {forward}"
    );
    assert_eq!(
        forward["reply_seq"], reply_seq,
        "granted forward watermark omitted actual visible assistant reply: {forward}"
    );

    // The hook fires only after the actual daemon has admitted this exact
    // conversation and captured the first scoped grant snapshot. The HTTP
    // request is held there until the real owner revokes the grant.
    armed.store(true, Ordering::SeqCst);
    let state = rig._dir.path().to_path_buf();
    let member_token = ana_token.to_string();
    let conversation_id = conversation.id.clone();
    let reader = std::thread::spawn(move || {
        crate::test_seam::scoped(crate::test_seam::Asserted::Operator, || {
            crate::client::rpc(
                &state,
                "thread_read",
                json!({"alias":"master", "conversation":conversation_id,
                "after":reply_seq, "wait":15, "limit":100, "public_session":member_token}),
            )
        })
    });
    assert_eq!(
        captured_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("actual admitted snapshot not observed"),
        conversation.id
    );
    grant_native(
        &rig,
        "app_member_access_revoke",
        json!({"install_id":install,"member_sub":"ana",
        "conversation_id":conversation.id,"public_session":owner_token}),
    )
    .expect("real owner revocation");
    shared
        .store
        .enqueue_task(
            "master",
            "GRANTED_STALE_AFTER_REVOKE",
            None,
            "grant-revoked-msg",
            "user",
            None,
        )
        .unwrap();
    shared
        .store
        .thread_append_to(
            &conversation.id,
            crate::store::NewEntry {
                role: crate::store::ROLE_AGENT,
                kind: crate::store::KIND_ASSISTANT_TEXT,
                text: "GRANTED_STALE_AFTER_REVOKE",
                payload: None,
                message_id: Some("grant-revoked-msg"),
            },
        )
        .unwrap()
        .expect("append after revoke");
    release_tx.send(()).expect("release real snapshot hook");
    let result = reader.join().expect("inflight thread panicked");
    assert!(result.is_err(), "stale grant returned a page: {result:?}");
    let body = format!("{result:?}");
    assert!(
        !body.contains("GRANTED_STALE_AFTER_REVOKE"),
        "inflight refusal leaked revoked content: {body}"
    );
    assert!(
        !body.contains("GRANTED_VISIBLE_REPLY"),
        "inflight refusal leaked old grant content: {body}"
    );
    let (_, owner_page) = grant_page(&rig, &owner, &conversation.id);
    assert!(
        owner_page
            .to_string()
            .contains("GRANTED_STALE_AFTER_REVOKE"),
        "owner recovery lost actual post-revocation entry"
    );
}

#[test]
fn http_member_access_lists_the_live_roster_and_grants_one_exact_conversation() {
    let _flag = AssistantFlagOn::arm();
    let f = member_access_fixture();

    // The verified owner's list is the real native projection plus the live
    // roster the guard itself re-proves before a grant.
    let (status, list) = member_access(&f.rig, "GET", Some(&f.owner), &f.install, "", None);
    assert_eq!(
        status, 200,
        "verified owner could not list member access: {list}"
    );
    assert_eq!(
        list["install_id"],
        json!(f.install),
        "list is not for the path installation: {list}"
    );
    assert_eq!(
        list["digest"],
        json!(f.digest),
        "list does not name the approved digest: {list}"
    );
    assert_eq!(
        list["grant_reason"],
        json!(""),
        "an approved installation is not grant-ready: {list}"
    );
    let conversation_ids: Vec<&str> = list["conversations"]
        .as_array()
        .expect("conversations array")
        .iter()
        .filter_map(|row| row["id"].as_str())
        .collect();
    for id in [f.selected.as_str(), f.other.as_str()] {
        assert!(
            conversation_ids.contains(&id),
            "owner list omitted conversation {id}: {list}"
        );
    }
    let members = list["members"]
        .as_array()
        .expect("members from the verified live roster");
    for (sub, role) in [
        ("grant-owner", "owner"),
        ("ana", "member"),
        ("ben", "member"),
    ] {
        assert!(
            members
                .iter()
                .any(|m| m["subject_id"] == json!(sub) && m["role"] == json!(role)),
            "live roster member {sub}/{role} missing: {members:?}"
        );
    }
    assert!(
        grants_of(&list).is_empty(),
        "a fresh installation already lists grants: {list}"
    );

    for (label, cookie) in [("member", Some(f.ana.as_str())), ("anonymous", None)] {
        let (status, body) = member_access(&f.rig, "GET", cookie, &f.install, "", None);
        assert!(
            !(200..300).contains(&status),
            "{label} read the operator-only member-access list: {status} {body}"
        );
    }

    let (status, granted) = member_access(
        &f.rig,
        "POST",
        Some(&f.owner),
        &f.install,
        "",
        Some(json!({"member_sub":"ana","conversation_id":f.selected,"expected_digest":f.digest})),
    );
    assert!(
        (200..300).contains(&status),
        "verified owner grant refused: {status} {granted}"
    );

    let (status, after) = member_access(&f.rig, "GET", Some(&f.owner), &f.install, "", None);
    assert_eq!(status, 200, "owner list failed after grant: {after}");
    let rows = grants_of(&after);
    assert_eq!(
        rows.len(),
        1,
        "one exact grant did not persist exactly once: {after}"
    );
    assert_eq!(rows[0]["member_sub"], "ana");
    assert_eq!(rows[0]["conversation_id"], json!(f.selected));
    assert_eq!(rows[0]["install_id"], json!(f.install));
    assert_eq!(rows[0]["state"], "active");
    assert_eq!(
        rows[0]["usable"], true,
        "the exact grant is not usable: {after}"
    );
    assert_eq!(
        after["grant_reason"],
        json!(""),
        "grant_reason became non-empty for an approved install: {after}"
    );

    // The grant's only effect is that one conversation, read by that member.
    let (status, selected_page) = grant_page(&f.rig, &f.ana, &f.selected);
    assert_eq!(
        status, 200,
        "granted member cannot read the granted conversation: {selected_page}"
    );
    assert!(
        selected_page
            .to_string()
            .contains("ACCESS_SELECTED_CONVERSATION"),
        "granted conversation content missing: {selected_page}"
    );
    assert!(
        !selected_page
            .to_string()
            .contains("ACCESS_OTHER_CONVERSATION"),
        "a grant for one conversation exposed another: {selected_page}"
    );
    let (_, other_page) = grant_page(&f.rig, &f.ana, &f.other);
    assert!(
        !other_page.to_string().contains("ACCESS_OTHER_CONVERSATION"),
        "the grant widened to an ungranted conversation: {other_page}"
    );
    let (_, ben_page) = grant_page(&f.rig, &f.ben, &f.selected);
    assert!(
        !ben_page
            .to_string()
            .contains("ACCESS_SELECTED_CONVERSATION"),
        "the grant leaked to another member: {ben_page}"
    );
}

#[test]
fn http_member_access_refuses_member_forged_authority_stale_digest_and_unproven_roster() {
    let _flag = AssistantFlagOn::arm();
    let f = member_access_fixture();
    let valid = json!({"member_sub":"ana","conversation_id":f.selected,"expected_digest":f.digest});

    // Every refusal is paired with the same assertions: no status, no grant
    // row, and no access. The positive control at the end proves the fixture
    // is not blanket-failing.
    let refused = [
        (
            "a member granting itself",
            Some(f.ana.as_str()),
            json!({"member_sub":"ana","conversation_id":f.selected,"expected_digest":f.digest}),
        ),
        (
            "a member granting another member",
            Some(f.ben.as_str()),
            json!({"member_sub":"ana","conversation_id":f.selected,"expected_digest":f.digest}),
        ),
        (
            "an anonymous caller",
            None,
            json!({"member_sub":"ana","conversation_id":f.selected,"expected_digest":f.digest}),
        ),
        (
            "a forged caller subject and tenancy",
            Some(f.ben.as_str()),
            json!({"member_sub":"ana","conversation_id":f.selected,"expected_digest":f.digest,
                   "sub":"grant-owner","company":ORG,"org":ORG}),
        ),
        (
            "a subject the live roster does not name",
            Some(f.owner.as_str()),
            json!({"member_sub":"not-on-roster","conversation_id":f.selected,"expected_digest":f.digest}),
        ),
        (
            "a stale expected digest",
            Some(f.owner.as_str()),
            json!({"member_sub":"ana","conversation_id":f.selected,
                   "expected_digest":format!("sha256:{}", "0".repeat(64))}),
        ),
        (
            "an unknown conversation",
            Some(f.owner.as_str()),
            json!({"member_sub":"ana","conversation_id":"conv_cad2215missing","expected_digest":f.digest}),
        ),
    ];
    for (label, cookie, request) in refused {
        let (status, body) = member_access(&f.rig, "POST", cookie, &f.install, "", Some(request));
        assert!(
            !(200..300).contains(&status),
            "{label} was accepted: {status} {body}"
        );
        let (list_status, list) =
            member_access(&f.rig, "GET", Some(&f.owner), &f.install, "", None);
        assert_eq!(
            list_status, 200,
            "owner list failed after a refusal: {list}"
        );
        assert!(
            grants_of(&list).is_empty(),
            "{label} left a grant behind: {list}"
        );
        let (_, page) = grant_page(&f.rig, &f.ana, &f.selected);
        assert!(
            !page.to_string().contains("ACCESS_SELECTED_CONVERSATION"),
            "{label} produced access: {page}"
        );
    }

    // The live roster is the only authority for "is a member". A roster that
    // names another organization, cannot be classified, or cannot be read
    // refuses the grant and must not become a forged empty list success.
    let owner_cookie = f.owner.clone();
    f.roster.lock().unwrap().wrong_org = true;
    let (status, body) = member_access(
        &f.rig,
        "POST",
        Some(&f.owner),
        &f.install,
        "",
        Some(valid.clone()),
    );
    assert!(
        !(200..300).contains(&status),
        "a wrong-organization roster was accepted: {status} {body}"
    );
    let (list_status, list) = member_access(&f.rig, "GET", Some(&f.owner), &f.install, "", None);
    assert!(
        !(200..300).contains(&list_status),
        "a wrong-organization roster produced a successful list instead of failing closed: {list_status} {list}"
    );
    f.roster.lock().unwrap().wrong_org = false;

    f.roster.lock().unwrap().malformed = true;
    let (status, body) = member_access(
        &f.rig,
        "POST",
        Some(&f.owner),
        &f.install,
        "",
        Some(valid.clone()),
    );
    assert!(
        !(200..300).contains(&status),
        "a malformed roster was accepted: {status} {body}"
    );
    let (list_status, list) = member_access(&f.rig, "GET", Some(&f.owner), &f.install, "", None);
    assert!(
        !(200..300).contains(&list_status),
        "a malformed roster produced a successful list instead of failing closed: {list_status} {list}"
    );
    f.roster.lock().unwrap().malformed = false;

    f.roster.lock().unwrap().owner_cookie = "platform-refused-session".into();
    let (status, body) = member_access(
        &f.rig,
        "POST",
        Some(&f.owner),
        &f.install,
        "",
        Some(valid.clone()),
    );
    assert!(
        !(200..300).contains(&status),
        "an unreadable live roster was accepted: {status} {body}"
    );
    let (list_status, list) = member_access(&f.rig, "GET", Some(&f.owner), &f.install, "", None);
    assert!(
        !(200..300).contains(&list_status),
        "an unreadable live roster produced a successful list instead of failing closed: {list_status} {list}"
    );
    f.roster.lock().unwrap().owner_cookie = owner_cookie;

    // A target the current roster no longer names is never granted, while a
    // still-valid roster keeps listing successfully.
    f.roster.lock().unwrap().target_present = false;
    let (status, body) = member_access(
        &f.rig,
        "POST",
        Some(&f.owner),
        &f.install,
        "",
        Some(valid.clone()),
    );
    assert!(
        !(200..300).contains(&status),
        "a target absent from the live roster was granted: {status} {body}"
    );
    let (list_status, list) = member_access(&f.rig, "GET", Some(&f.owner), &f.install, "", None);
    assert_eq!(
        list_status, 200,
        "the owner list refused a still-valid roster: {list}"
    );
    assert!(
        !list["members"]
            .as_array()
            .expect("members")
            .iter()
            .any(|m| m["subject_id"] == json!("ana")),
        "the list still named a member the live roster had removed: {list}"
    );
    f.roster.lock().unwrap().target_present = true;

    // A path that names an installation other than the approved one cannot
    // become a grant for the approved installation.
    let unknown = format!("{}x", f.install);
    let (status, body) = member_access(
        &f.rig,
        "POST",
        Some(&f.owner),
        &unknown,
        "",
        Some(valid.clone()),
    );
    assert!(
        !(200..300).contains(&status),
        "an unknown path installation was granted: {status} {body}"
    );
    let (_, list) = member_access(&f.rig, "GET", Some(&f.owner), &f.install, "", None);
    assert!(
        grants_of(&list).is_empty(),
        "an unknown path installation left a grant: {list}"
    );

    // Positive control: the identical body succeeds only for the verified
    // owner with a live, matching roster.
    let (status, granted) =
        member_access(&f.rig, "POST", Some(&f.owner), &f.install, "", Some(valid));
    assert!(
        (200..300).contains(&status),
        "the positive control was refused: {status} {granted}"
    );
    let (_, page) = grant_page(&f.rig, &f.ana, &f.selected);
    assert!(
        page.to_string().contains("ACCESS_SELECTED_CONVERSATION"),
        "the positive control produced no access: {page}"
    );
}

#[test]
fn http_member_access_revoke_withdraws_only_the_chosen_conversation() {
    let _flag = AssistantFlagOn::arm();
    let f = member_access_fixture();
    for (conversation, marker) in [
        (&f.selected, "ACCESS_SELECTED_CONVERSATION"),
        (&f.other, "ACCESS_OTHER_CONVERSATION"),
    ] {
        let (status, granted) = member_access(
            &f.rig,
            "POST",
            Some(&f.owner),
            &f.install,
            "",
            Some(
                json!({"member_sub":"ana","conversation_id":conversation,"expected_digest":f.digest}),
            ),
        );
        assert!(
            (200..300).contains(&status),
            "owner grant for {conversation} refused: {granted}"
        );
        let (_, page) = grant_page(&f.rig, &f.ana, conversation);
        assert!(
            page.to_string().contains(marker),
            "granted conversation {conversation} unreadable: {page}"
        );
    }

    // A member cannot revoke anybody's grant, including from a claimed body.
    for (label, cookie) in [
        ("another member", f.ben.as_str()),
        ("the granted member", f.ana.as_str()),
    ] {
        let (status, body) = member_access(
            &f.rig,
            "POST",
            Some(cookie),
            &f.install,
            "/revoke",
            Some(json!({"member_sub":"ana","conversation_id":f.selected})),
        );
        assert!(
            !(200..300).contains(&status),
            "{label} revoked a grant: {status} {body}"
        );
        let (_, page) = grant_page(&f.rig, &f.ana, &f.selected);
        assert!(
            page.to_string().contains("ACCESS_SELECTED_CONVERSATION"),
            "{label} withdrew the grant: {page}"
        );
    }

    let (status, revoked) = member_access(
        &f.rig,
        "POST",
        Some(&f.owner),
        &f.install,
        "/revoke",
        Some(json!({"member_sub":"ana","conversation_id":f.selected})),
    );
    assert!(
        (200..300).contains(&status),
        "owner revoke refused: {status} {revoked}"
    );

    let (_, selected) = grant_page(&f.rig, &f.ana, &f.selected);
    assert!(
        !selected
            .to_string()
            .contains("ACCESS_SELECTED_CONVERSATION"),
        "the revoked conversation stayed readable: {selected}"
    );
    let (_, other) = grant_page(&f.rig, &f.ana, &f.other);
    assert!(
        other.to_string().contains("ACCESS_OTHER_CONVERSATION"),
        "revoking one conversation withdrew the other: {other}"
    );

    let (status, list) = member_access(&f.rig, "GET", Some(&f.owner), &f.install, "", None);
    assert_eq!(status, 200, "owner list failed after revoke: {list}");
    let rows = grants_of(&list);
    assert_eq!(
        rows.len(),
        2,
        "an exact revoke did not keep both tombstones: {list}"
    );
    let revoked_row = rows
        .iter()
        .find(|row| (*row)["conversation_id"] == json!(f.selected))
        .expect("revoked row");
    assert_eq!(revoked_row["state"], "revoked");
    assert_ne!(
        revoked_row["usable"],
        json!(true),
        "a revoked grant is still usable: {revoked_row}"
    );
    let active_row = rows
        .iter()
        .find(|row| (*row)["conversation_id"] == json!(f.other))
        .expect("surviving row");
    assert_eq!(active_row["state"], "active");
    assert_eq!(
        active_row["usable"],
        json!(true),
        "the surviving grant lost its usability: {active_row}"
    );

    // There is no install-wide revoke on this surface: a body without the
    // exact conversation must not withdraw the surviving grant.
    let (status, body) = member_access(
        &f.rig,
        "POST",
        Some(&f.owner),
        &f.install,
        "/revoke",
        Some(json!({"member_sub":"ana"})),
    );
    assert!(
        !(200..300).contains(&status),
        "a conversation-less revoke was accepted: {status} {body}"
    );
    let (_, surviving) = grant_page(&f.rig, &f.ana, &f.other);
    assert!(
        surviving.to_string().contains("ACCESS_OTHER_CONVERSATION"),
        "an install-wide revoke withdrew the surviving grant: {surviving}"
    );

    let (status, revoked) = member_access(
        &f.rig,
        "POST",
        Some(&f.owner),
        &f.install,
        "/revoke",
        Some(json!({"member_sub":"ana","conversation_id":f.other})),
    );
    assert!(
        (200..300).contains(&status),
        "revoking the second conversation refused: {status} {revoked}"
    );
    for (conversation, marker) in [
        (&f.selected, "ACCESS_SELECTED_CONVERSATION"),
        (&f.other, "ACCESS_OTHER_CONVERSATION"),
    ] {
        let (page_status, page) = grant_page(&f.rig, &f.ana, conversation);
        assert!(
            !page.to_string().contains(marker),
            "conversation {conversation} stayed readable after both revokes: {page_status} {page}"
        );
    }

    // Revocation withdraws access; it never deletes the conversation's
    // content or the operator's recovery of it.
    let (owner_status, owner_page) = grant_page(&f.rig, &f.owner, &f.other);
    assert_eq!(
        owner_status, 200,
        "revocation broke operator recovery: {owner_page}"
    );
    assert!(
        owner_page.to_string().contains("ACCESS_OTHER_CONVERSATION"),
        "revocation deleted the conversation's recorded content: {owner_page}"
    );
}

#[test]
fn inflight_member_read_rejects_revoked_and_regranted_epoch() {
    let _flag = AssistantFlagOn::arm();
    let live_ana = Arc::new(AtomicBool::new(true));
    let roster = Arc::new(Mutex::new(GrantRoster {
        target_present: true,
        ..GrantRoster::default()
    }));
    let (observer, _events) = queue_observer();
    let armed = Arc::new(AtomicBool::new(false));
    let wanted = Arc::new(Mutex::new(String::new()));
    let (captured_tx, captured_rx) = std::sync::mpsc::channel::<String>();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let release_rx = Arc::new(Mutex::new(release_rx));
    let hook: crate::daemon::AppGrantForwardSnapshotHook = {
        let armed = armed.clone();
        let wanted = wanted.clone();
        Arc::new(move |id: &str| {
            if *wanted.lock().unwrap() != id {
                return;
            }
            if armed
                .compare_exchange(true, false, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
            {
                return;
            }
            captured_tx.send(id.to_string()).expect("snapshot receiver");
            release_rx
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(20))
                .expect("snapshot release");
        })
    };
    let rig = grant_rig(live_ana, roster.clone(), observer, Some(hook), None);
    seed(&rig);
    let owner = login(&rig, "grant-owner", "owner");
    roster.lock().unwrap().owner_cookie = owner.clone();
    let ana = login(&rig, "ana", "member");
    let owner_token = owner.split_once('=').unwrap().1;
    let ana_token = ana.split_once('=').unwrap().1;

    let source = format!("{}/tests/fixtures/apps/crm", env!("CARGO_MANIFEST_DIR"));
    let installed = grant_native(&rig, "app_workspace_install", json!({"source": source}))
        .expect("real workspace install");
    let install = installed["install_id"].as_str().unwrap().to_string();
    let digest = installed["digest"].as_str().unwrap().to_string();
    grant_native(
        &rig,
        "app_local_install_approve",
        json!({
            "install_id": install, "digest": digest
        }),
    )
    .expect("real current app approval");
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
    let (conversation, _) = shared
        .store
        .conversation_create(
            "master",
            &install,
            None,
            crate::store::ConversationKind::General,
        )
        .unwrap();
    *wanted.lock().unwrap() = conversation.id.clone();
    grant_native(
        &rig,
        "app_member_access_grant",
        json!({
            "install_id": install, "member_sub": "ana", "conversation_id": conversation.id,
            "expected_digest": digest, "public_session": owner_token
        }),
    )
    .expect("owner grant A");

    shared
        .store
        .enqueue_task(
            "master",
            "EPOCH_RETAINED_CONTENT",
            None,
            "epoch-retained-msg",
            "user",
            None,
        )
        .unwrap();
    shared
        .store
        .thread_append_to(
            &conversation.id,
            crate::store::NewEntry {
                role: crate::store::ROLE_AGENT,
                kind: crate::store::KIND_ASSISTANT_TEXT,
                text: "EPOCH_RETAINED_CONTENT",
                payload: None,
                message_id: Some("epoch-retained-msg"),
            },
        )
        .unwrap()
        .expect("append retained conversation content");
    let initial = grant_native(
        &rig,
        "thread_read",
        json!({
            "alias": "master", "conversation": conversation.id, "after": 0,
            "limit": 100, "public_session": ana_token
        }),
    )
    .expect("grant A authorizes member read");
    assert!(
        initial.to_string().contains("EPOCH_RETAINED_CONTENT"),
        "initial member read omitted retained content: {initial}"
    );

    armed.store(true, Ordering::SeqCst);
    let state = rig._dir.path().to_path_buf();
    let member_token = ana_token.to_string();
    let conversation_id = conversation.id.clone();
    let reader = std::thread::spawn(move || {
        crate::test_seam::scoped(crate::test_seam::Asserted::Operator, || {
            crate::client::rpc(
                &state,
                "thread_read",
                json!({
                    "alias": "master", "conversation": conversation_id, "after": 0,
                    "limit": 100, "public_session": member_token
                }),
            )
        })
    });
    assert_eq!(
        captured_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("admitted grant A snapshot"),
        conversation.id
    );
    grant_native(
        &rig,
        "app_member_access_revoke",
        json!({
            "install_id": install, "member_sub": "ana", "conversation_id": conversation.id,
            "public_session": owner_token
        }),
    )
    .expect("real owner revocation of grant A");
    grant_native(
        &rig,
        "app_member_access_grant",
        json!({
            "install_id": install, "member_sub": "ana", "conversation_id": conversation.id,
            "expected_digest": digest, "public_session": owner_token
        }),
    )
    .expect("real owner re-grant to the same member and conversation");
    release_tx.send(()).expect("release grant A read");
    let stale = reader.join().expect("in-flight reader panicked");
    assert!(
        stale.is_err(),
        "grant A read succeeded after revoke and re-grant: {stale:?}"
    );
    assert!(
        !format!("{stale:?}").contains("EPOCH_RETAINED_CONTENT"),
        "stale refusal leaked retained content: {stale:?}"
    );

    let (member_status, member_page) = grant_page(&rig, &ana, &conversation.id);
    assert_eq!(
        member_status, 200,
        "fresh member read failed after re-grant: {member_page}"
    );
    assert!(
        member_page.to_string().contains("EPOCH_RETAINED_CONTENT"),
        "fresh member read lost retained content: {member_page}"
    );
    let (owner_status, owner_page) = grant_page(&rig, &owner, &conversation.id);
    assert_eq!(
        owner_status, 200,
        "owner read failed after re-grant: {owner_page}"
    );
    assert!(
        owner_page.to_string().contains("EPOCH_RETAINED_CONTENT"),
        "owner read lost retained content: {owner_page}"
    );
}

fn room_http(
    rig: &Rig,
    cookie: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let url = format!("http://127.0.0.1:{}{path}", rig.board_port);
    let response = match method {
        "GET" => http_agent()
            .get(&url)
            .header("Host", &rig.public_host)
            .header("Cookie", cookie)
            .header("Origin", &format!("http://{}", rig.public_host))
            .header("Sec-Fetch-Site", "same-origin")
            .call(),
        "POST" => http_agent()
            .post(&url)
            .header("Host", &rig.public_host)
            .header("Cookie", cookie)
            .header("Origin", &format!("http://{}", rig.public_host))
            .header("Sec-Fetch-Site", "same-origin")
            .header("X-Cadence-Board", "1")
            .header("Content-Type", "application/json")
            .send(serde_json::to_vec(&body.expect("POST body")).unwrap()),
        "DELETE" => http_agent()
            .delete(&url)
            .header("Host", &rig.public_host)
            .header("Cookie", cookie)
            .header("Origin", &format!("http://{}", rig.public_host))
            .header("Sec-Fetch-Site", "same-origin")
            .header("X-Cadence-Board", "1")
            .header("Content-Type", "application/json")
            .call(),
        _ => panic!("unsupported fixture method"),
    }
    .expect("HTTP request");
    let mut response = response;
    let status = response.status().as_u16();
    let bytes = response.body_mut().read_to_vec().unwrap_or_default();
    let body = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| json!({"opaque": String::from_utf8_lossy(&bytes).into_owned()}));
    (status, body)
}

fn room_ok(rig: &Rig, cookie: &str, method: &str, path: &str, body: Option<Value>) -> Value {
    let (status, value) = room_http(rig, cookie, method, path, body);
    assert_eq!(status, 200, "{method} {path}: {value}");
    value
}

fn room_post(rig: &Rig, cookie: &str, room: &str, key: &str, body: &str) -> Value {
    room_ok(
        rig,
        cookie,
        "POST",
        &format!("/api/human-threads/rooms/{room}/posts"),
        Some(json!({"client_id":key,"body":body})),
    )
}

fn assert_room_post(post: &Value) -> String {
    let post = post.as_object().expect("post object");
    assert!(
        !post.contains_key("seq"),
        "global post sequence escaped: {post:?}"
    );
    let id = post
        .get("id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .expect("post id");
    assert!(!id.is_empty());
    post.get("read_cursor")
        .and_then(Value::as_str)
        .filter(|s| s.starts_with("csc1_") && s.len() > 5)
        .expect("opaque read cursor")
        .to_string()
}

fn room_fixture_db(rig: &Rig) -> rusqlite::Connection {
    rusqlite::Connection::open(rig._dir.path().join("cadence.sqlite3")).unwrap()
}

fn room_post_seq(rig: &Rig, id: &str) -> i64 {
    room_fixture_db(rig)
        .query_row("SELECT seq FROM topic_posts WHERE id=?1", [id], |row| {
            row.get(0)
        })
        .unwrap()
}

fn room_marker(rig: &Rig, room: &str, human: &str) -> Option<i64> {
    use rusqlite::OptionalExtension;
    room_fixture_db(rig)
        .query_row(
            "SELECT seq FROM human_read_markers WHERE room=?1 AND human=?2",
            rusqlite::params![room, human],
            |row| row.get(0),
        )
        .optional()
        .unwrap()
}

#[test]
fn human_room_opaque_history_and_receipts_use_real_guards() {
    let roster = Arc::new(Mutex::new(GrantRoster {
        target_present: true,
        ..GrantRoster::default()
    }));
    let (observer, _events) = queue_observer();
    let fault = Arc::new(std::sync::atomic::AtomicU8::new(0));
    let rig = grant_rig(
        Arc::new(AtomicBool::new(true)),
        roster.clone(),
        observer,
        None,
        Some(fault.clone()),
    );
    let _keys = initialize_queue_keyring(&rig);
    let ana = login(&rig, "ana", "member");
    let ben = login(&rig, "ben", "member");
    roster.lock().unwrap().allowed_cookies = vec![ana.clone(), ben.clone()];

    let create = |key: &str| {
        let result = room_ok(
            &rig,
            &ana,
            "POST",
            "/api/human-threads/groups",
            Some(json!({
                "kind":"human_group", "title":key, "client_id":key,
                "humans":["ben"], "agents":[]
            })),
        );
        result["room"].as_str().expect("room id").to_string()
    };
    let a = create("opaque-room-a");
    let b = create("opaque-room-b");
    assert_ne!(a, b);
    let a_path = format!("/api/human-threads/rooms/{a}");
    let b_path = format!("/api/human-threads/rooms/{b}");

    let a1 = room_post(&rig, &ben, &a, "opaque-a1", "A private first");
    assert!(a1.get("_reader_scope").is_none());
    assert_room_post(&a1["post"]);
    let b1 = room_post(&rig, &ana, &b, "opaque-b1", "B private first");
    assert!(b1.get("_reader_scope").is_none());
    assert_room_post(&b1["post"]);

    // Install a genuine pre-existing numeric marker before exercising the
    // opaque mark-read contract. This is fixture setup, never an
    // authorization shortcut for a tested HTTP request.
    let a1_id = a1["post"]["id"].as_str().unwrap();
    let a1_seq = room_post_seq(&rig, a1_id);
    let actor =
        crate::store::human_threads::VerifiedHuman::verified(&rig.issuer, ORG, "ana").unwrap();
    room_fixture_db(&rig)
        .execute(
            "INSERT INTO human_read_markers(room,human,seq) VALUES(?1,?2,?3)",
            rusqlite::params![&a, &actor.id, a1_seq],
        )
        .expect("seed pre-existing numeric read marker");
    assert_eq!(room_marker(&rig, &a, &actor.id), Some(a1_seq));
    let b_actor =
        crate::store::human_threads::VerifiedHuman::verified(&rig.issuer, ORG, "ben").unwrap();
    let ben_marker = room_marker(&rig, &a, &b_actor.id);

    let a2 = room_post(&rig, &ben, &a, "opaque-a2", "A private second");
    let receipt_cursor = assert_room_post(&a2["post"]);
    assert!(a2.get("_reader_scope").is_none());
    let a2_seq = room_post_seq(&rig, a2["post"]["id"].as_str().unwrap());
    assert!(a2_seq > a1_seq);
    let b2 = room_post(&rig, &ana, &b, "opaque-b2", "B private second");
    assert_room_post(&b2["post"]);

    let history = |cookie: &str, path: &str| room_ok(&rig, cookie, "GET", path, None);
    let first = history(&ana, &format!("{a_path}?limit=1"));
    assert!(first.get("_reader_scope").is_none());
    assert_eq!(first["room"], json!(a));
    assert_eq!(first["more_before"], json!(true));
    let posts = first["posts"].as_array().expect("posts");
    assert_eq!(posts.len(), 1);
    assert_eq!(posts[0]["id"], a2["post"]["id"]);
    let cursor = assert_room_post(&posts[0]);
    let before = first["before"]
        .as_str()
        .filter(|s| s.starts_with("csc1_"))
        .unwrap()
        .to_string();
    let repeat = history(&ana, &format!("{a_path}?limit=1"));
    let scope_a = first["scope_id"]
        .as_str()
        .filter(|s| !s.is_empty())
        .expect("scope id")
        .to_string();
    assert_eq!(
        repeat["scope_id"], scope_a,
        "same membership scope must remain stable across refresh"
    );
    let ben_before_rejoin = history(&ben, &format!("{a_path}?limit=1"));
    assert_ne!(
        ben_before_rejoin["scope_id"], scope_a,
        "scope id must bind principal"
    );
    let ben_scope_old = ben_before_rejoin["scope_id"].as_str().unwrap().to_string();
    let stale_before = ben_before_rejoin["before"].as_str().unwrap().to_string();
    let stale_read = assert_room_post(&ben_before_rejoin["posts"][0]);
    let room_b_ana = history(&ana, &format!("{b_path}?limit=1"));
    assert_ne!(room_b_ana["scope_id"], scope_a, "scope id must bind room");
    assert_eq!(repeat["posts"][0]["id"], posts[0]["id"]);
    assert_ne!(
        repeat["before"], first["before"],
        "before must be randomized"
    );
    assert_ne!(
        repeat["posts"][0]["read_cursor"], posts[0]["read_cursor"],
        "read token must be randomized"
    );
    assert_ne!(
        receipt_cursor, cursor,
        "receipt and read tokens must be randomized"
    );
    let older = history(&ana, &format!("{a_path}?limit=1&before={before}"));
    assert_eq!(older["posts"][0]["id"], a1["post"]["id"]);
    assert_room_post(&older["posts"][0]);
    let b_page = history(&ben, &format!("{b_path}?limit=10"));
    let b_posts = b_page["posts"].as_array().unwrap();
    assert!(b_posts.iter().any(|p| p["id"] == b1["post"]["id"]));
    assert!(b_posts.iter().any(|p| p["id"] == b2["post"]["id"]));
    for p in b_posts {
        assert_room_post(p);
    }

    for (cookie, path) in [(&ben, &a_path), (&ana, &b_path)] {
        let (status, result) = room_http(
            &rig,
            cookie,
            "GET",
            &format!("{path}?limit=1&before={before}"),
            None,
        );
        assert!(
            !(200..300).contains(&status),
            "foreign before accepted: {result}"
        );
        let (status, result) = room_http(
            &rig,
            cookie,
            "POST",
            &format!("{path}/read"),
            Some(json!({"cursor":cursor})),
        );
        assert!(
            !(200..300).contains(&status),
            "foreign read cursor accepted: {result}"
        );
    }
    for value in [json!({"seq":a2_seq}), json!({"cursor":"invalid"})] {
        let (status, result) =
            room_http(&rig, &ana, "POST", &format!("{a_path}/read"), Some(value));
        assert!(
            !(200..300).contains(&status),
            "invalid read accepted: {result}"
        );
    }
    let (status, result) = room_http(
        &rig,
        &ana,
        "GET",
        &format!("{a_path}?before={a1_seq}"),
        None,
    );
    assert!(
        !(200..300).contains(&status),
        "numeric before accepted: {result}"
    );
    assert_eq!(room_marker(&rig, &a, &actor.id), Some(a1_seq));
    assert_eq!(room_marker(&rig, &a, &b_actor.id), ben_marker);
    room_ok(
        &rig,
        &ana,
        "POST",
        &format!("{a_path}/read"),
        Some(json!({"cursor":cursor})),
    );
    assert_eq!(room_marker(&rig, &a, &actor.id), Some(a2_seq));
    assert_eq!(room_marker(&rig, &a, &b_actor.id), ben_marker);
    let delete_path = format!("{a_path}/members/human/{}", b_actor.id);
    room_ok(&rig, &ana, "DELETE", &delete_path, None);
    for (method, path, body) in [
        ("GET", format!("{a_path}?before={stale_before}"), None),
        (
            "POST",
            format!("{a_path}/read"),
            Some(json!({"cursor":stale_read})),
        ),
    ] {
        let (status, value) = room_http(&rig, &ben, method, &path, body);
        assert!(
            !(200..300).contains(&status),
            "removed member accepted: {value}"
        );
    }
    room_ok(
        &rig,
        &ana,
        "POST",
        &format!("{a_path}/members"),
        Some(json!({"human_subject":"ben"})),
    );
    for (method, path, body) in [
        ("GET", format!("{a_path}?before={stale_before}"), None),
        (
            "POST",
            format!("{a_path}/read"),
            Some(json!({"cursor":stale_read})),
        ),
    ] {
        let (status, value) = room_http(&rig, &ben, method, &path, body);
        assert!(
            !(200..300).contains(&status),
            "stale token survived rejoin: {value}"
        );
    }
    assert_eq!(room_marker(&rig, &a, &b_actor.id), ben_marker);
    let new_post = room_post(&rig, &ana, &a, "after-rejoin", "post after rejoin");
    assert_eq!(new_post["created"], true);
    let duplicate = room_post(&rig, &ana, &a, "after-rejoin", "post after rejoin");
    assert_eq!(duplicate["created"], false);
    assert_eq!(duplicate["post"]["id"], new_post["post"]["id"]);
    assert_room_post(&duplicate["post"]);
    let after_rejoin = history(&ben, &format!("{a_path}?limit=10"));
    assert_ne!(
        after_rejoin["scope_id"], ben_scope_old,
        "rejoin must change scope id"
    );
    let visible = after_rejoin["posts"].as_array().unwrap();
    assert_eq!(
        visible.len(),
        1,
        "old membership history must not reappear and retry must not insert a duplicate"
    );
    assert_eq!(visible[0]["id"], new_post["post"]["id"]);
    let fresh_read = assert_room_post(&visible[0]);
    room_ok(
        &rig,
        &ben,
        "POST",
        &format!("{a_path}/read"),
        Some(json!({"cursor":fresh_read})),
    );
    let fresh_before = after_rejoin["before"].as_str().unwrap();
    let older = history(&ben, &format!("{a_path}?before={fresh_before}"));
    assert!(older["posts"].as_array().unwrap().is_empty());

    // Mutations use unique keys: corrupted receipts follow real admission and
    // may already have committed; never blindly replay an uncertain write.
    for mode in 1..=3 {
        fault.store(mode, Ordering::SeqCst);
        let (status, value) = room_http(&rig, &ana, "GET", &format!("{a_path}?limit=1"), None);
        assert!(
            !(200..300).contains(&status),
            "fault {mode} history relayed: {value}"
        );
        assert!(value.get("posts").is_none());
        let (status, value) = room_http(
            &rig,
            &ana,
            "POST",
            &format!("{a_path}/posts"),
            Some(json!({
                "client_id":format!("fault-{mode}"), "body":format!("fault post {mode}")
            })),
        );
        assert!(
            !(200..300).contains(&status),
            "fault {mode} receipt relayed: {value}"
        );
        assert!(value.get("post").is_none());
    }
    fault.store(0, Ordering::SeqCst);
    let clean = history(&ana, &format!("{a_path}?limit=10"));
    assert!(clean["posts"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["id"] == a2["post"]["id"]));
    assert_room_post(&room_post(&rig, &ana, &a, "clean-control", "clean control")["post"]);
}

#[test]
fn human_room_cross_workspace_same_keys_replay_is_refused() {
    let roster = Arc::new(Mutex::new(GrantRoster {
        target_present: true,
        ..GrantRoster::default()
    }));
    let (observer_a, _events_a) = queue_observer();
    let a = grant_rig(
        Arc::new(AtomicBool::new(true)),
        roster.clone(),
        observer_a,
        None,
        None,
    );
    let keys_a = initialize_queue_keyring(&a);
    let (observer_b, _events_b) = queue_observer();
    let b = grant_rig_with_authority(
        Arc::new(AtomicBool::new(true)),
        roster.clone(),
        observer_b,
        None,
        None,
        Some(&a),
    );
    let keys_b = initialize_queue_keyring(&b);
    assert_eq!(a.issuer, b.issuer);
    assert_eq!(a.public_host, b.public_host);
    for file in ["identity.key", "cursor.key"] {
        assert_eq!(
            std::fs::read(keys_a.path().join(file)).unwrap(),
            std::fs::read(keys_b.path().join(file)).unwrap()
        );
    }
    let ana_a = login(&a, "ana", "member");
    let ben_a = login(&a, "ben", "member");
    let ana_b = login(&b, "ana", "member");
    roster.lock().unwrap().allowed_cookies = vec![ana_a.clone(), ben_a.clone(), ana_b.clone()];
    let created = room_ok(
        &a,
        &ana_a,
        "POST",
        "/api/human-threads/groups",
        Some(json!({
            "kind":"human_group", "title":"workspace control", "client_id":"workspace-control",
            "humans":["ben"], "agents":[]
        })),
    );
    let room = created["room"].as_str().unwrap().to_string();
    let path = format!("/api/human-threads/rooms/{room}");
    let first = room_post(&a, &ben_a, &room, "workspace-first", "first");
    let second = room_post(&a, &ben_a, &room, "workspace-second", "second");
    let actor =
        crate::store::human_threads::VerifiedHuman::verified(&a.issuer, ORG, "ana").unwrap();
    let first_seq = room_post_seq(&a, first["post"]["id"].as_str().unwrap());
    let second_seq = room_post_seq(&a, second["post"]["id"].as_str().unwrap());
    assert!(second_seq > first_seq);
    room_fixture_db(&a)
        .execute(
            "INSERT INTO human_read_markers(room,human,seq) VALUES(?1,?2,?3)",
            rusqlite::params![&room, &actor.id, first_seq],
        )
        .unwrap();

    // Copy only the room fixture, not workspace metadata, keys, sessions or
    // credentials. This creates identical durable room incarnations, members,
    // post positions and read markers under B's independently initialized ID.
    let db_b = room_fixture_db(&b);
    db_b.execute(
        "ATTACH DATABASE ?1 AS source",
        [a._dir.path().join("cadence.sqlite3").to_str().unwrap()],
    )
    .unwrap();
    db_b.execute_batch(
        "BEGIN;
        INSERT INTO human_principals SELECT * FROM source.human_principals;
        INSERT INTO topics SELECT * FROM source.topics WHERE type='human_room';
        INSERT INTO human_rooms SELECT * FROM source.human_rooms;
        INSERT INTO human_room_members SELECT * FROM source.human_room_members;
        INSERT INTO topic_posts SELECT * FROM source.topic_posts;
        INSERT INTO human_read_markers SELECT * FROM source.human_read_markers;
        COMMIT;",
    )
    .unwrap();
    db_b.execute_batch("DETACH DATABASE source").unwrap();
    let db_a = room_fixture_db(&a);
    let workspace = |db: &rusqlite::Connection| -> String {
        db.query_row(
            "SELECT workspace_id FROM connection_metadata WHERE singleton=1",
            [],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_ne!(workspace(&db_a), workspace(&db_b));
    let room_scope = |db: &rusqlite::Connection| -> (String, f64, i64, String, String, String) {
        db.query_row("SELECT r.topic_id,r.created,m.cutoff,p.issuer,p.org,p.subject FROM human_rooms r JOIN human_room_members m ON m.room=r.id JOIN human_principals p ON p.id=m.member WHERE r.id=?1 AND m.member=?2 AND m.removed IS NULL",
            rusqlite::params![&room, &actor.id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).unwrap()
    };
    assert_eq!(room_scope(&db_a), room_scope(&db_b));
    assert_eq!(room_marker(&b, &room, &actor.id), Some(first_seq));

    let page_a = room_ok(&a, &ana_a, "GET", &format!("{path}?limit=1"), None);
    let before_a = page_a["before"].as_str().unwrap();
    let read_a = assert_room_post(&page_a["posts"][0]);
    assert_eq!(page_a["posts"][0]["id"], second["post"]["id"]);
    let page_b = room_ok(&b, &ana_b, "GET", &format!("{path}?limit=1"), None);
    assert_eq!(page_b["posts"][0]["id"], second["post"]["id"]);
    assert_ne!(
        page_a["scope_id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .unwrap(),
        page_b["scope_id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .unwrap()
    );
    let (status, reply) = room_http(
        &b,
        &ana_b,
        "GET",
        &format!("{path}?limit=1&before={before_a}"),
        None,
    );
    assert!(
        !(200..300).contains(&status),
        "workspace A history cursor accepted in B: {reply}"
    );
    assert_eq!(room_marker(&b, &room, &actor.id), Some(first_seq));
    let (status, reply) = room_http(
        &b,
        &ana_b,
        "POST",
        &format!("{path}/read"),
        Some(json!({"cursor":read_a})),
    );
    assert!(
        !(200..300).contains(&status),
        "workspace A read cursor accepted in B: {reply}"
    );
    assert_eq!(room_marker(&b, &room, &actor.id), Some(first_seq));
    let before_b = page_b["before"].as_str().unwrap();
    let older_b = room_ok(
        &b,
        &ana_b,
        "GET",
        &format!("{path}?limit=1&before={before_b}"),
        None,
    );
    assert_eq!(older_b["posts"][0]["id"], first["post"]["id"]);
    let read_b = assert_room_post(&page_b["posts"][0]);
    room_ok(
        &b,
        &ana_b,
        "POST",
        &format!("{path}/read"),
        Some(json!({"cursor":read_b})),
    );
    assert_eq!(room_marker(&b, &room, &actor.id), Some(second_seq));
    assert_eq!(room_marker(&a, &room, &actor.id), Some(first_seq));
}

#[test]
fn human_room_signed_owner_ask_has_opaque_idempotent_receipt() {
    let roster = Arc::new(Mutex::new(GrantRoster {
        target_present: true,
        ..GrantRoster::default()
    }));
    let (observer, _events) = queue_observer();
    let rig = grant_rig(
        Arc::new(AtomicBool::new(true)),
        roster.clone(),
        observer,
        None,
        None,
    );
    let _keys = initialize_queue_keyring(&rig);
    let owner = login(&rig, "grant-owner", "owner");
    roster.lock().unwrap().owner_cookie = owner.clone();
    let opts = ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", rig.pm.to_str().unwrap());
    let shared = crate::daemon::Shared::new(rig._dir.path(), &opts).unwrap();
    shared
        .store
        .register_agent(&crate::store::NewAgent {
            alias: "room-ask-worker",
            provider: "fake",
            endpoint_kind: "fake",
            role: "worker",
            cwd: rig._dir.path().to_str().unwrap(),
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .unwrap();
    let uid: String = room_fixture_db(&rig)
        .query_row(
            "SELECT uid FROM agents WHERE alias='room-ask-worker'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!uid.is_empty());
    let group = room_ok(
        &rig,
        &owner,
        "POST",
        "/api/human-threads/groups",
        Some(json!({
            "kind":"mixed_group", "title":"Owner Ask", "client_id":"owner-ask-room",
            "humans":["ana"], "agents":[uid]
        })),
    );
    let room = group["room"].as_str().unwrap();
    let path = format!("/api/human-threads/rooms/{room}");
    let page = room_ok(&rig, &owner, "GET", &path, None);
    assert_eq!(
        page["can_ask"], true,
        "fixture must have a real signed operator and live owner role"
    );
    assert!(page["members"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["kind"] == "agent" && m["id"] == uid));
    let request_body =
        json!({"agent_uid":uid,"client_id":"owner-ask-once","body":"Please review this question"});
    let result = room_ok(
        &rig,
        &owner,
        "POST",
        &format!("{path}/ask"),
        Some(request_body.clone()),
    );
    assert_eq!(result["duplicate"], false);
    assert!(result.get("_reader_scope").is_none());
    assert!(result.get("seq").is_none());
    assert_room_post(&result["post"]);
    assert_eq!(result["target"]["uid"], uid);
    let request = result["request"]
        .as_str()
        .filter(|s| !s.is_empty())
        .unwrap();
    let duplicate = room_ok(
        &rig,
        &owner,
        "POST",
        &format!("{path}/ask"),
        Some(request_body),
    );
    assert_eq!(duplicate["duplicate"], true);
    assert_eq!(duplicate["request"], request);
    assert_eq!(duplicate["post"]["id"], result["post"]["id"]);
    assert!(duplicate.get("seq").is_none());
    assert_room_post(&duplicate["post"]);
    let db = room_fixture_db(&rig);
    let count: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM messages WHERE id=?1 AND recipient_uid=?2",
            rusqlite::params![request, &uid],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        count, 1,
        "one durable request for the selected registered agent"
    );
    let posts: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM topic_posts WHERE request_message=?1",
            [request],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(posts, 1, "one bound Ask post");
}
