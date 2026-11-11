//! CAD-2215 real loopback anonymous-read privacy acceptance.
//! The product implementer must not edit or weaken this check.
//! This does not establish local-agent process/filesystem isolation.

use super::*;
use std::io::{BufRead, BufReader};

struct LocalBoard {
    port: u16,
    host: String,
    stop: Arc<AtomicBool>,
}

fn local_board(rig: &Rig) -> LocalBoard {
    let stop = Arc::new(AtomicBool::new(false));
    for port in 3210..=3309u16 {
        let (startup, ready) = std::sync::mpsc::channel();
        let opts = ServeOpts {
            host: "127.0.0.1".into(),
            port,
            stop: Some(stop.clone()),
            startup: Some(startup),
            allow_anonymous_reads: true,
            ..ServeOpts::default()
        };
        let state = rig._dir.path().to_path_buf();
        let pm = rig.pm.clone();
        let thread = std::thread::spawn(move || drop(crate::ui::serve(&state, &pm, &opts)));
        match ready
            .recv_timeout(Duration::from_secs(30))
            .expect("board startup did not answer")
        {
            Ok(()) => {
                return LocalBoard {
                    port,
                    host: format!("cadence-{port}.localhost:{port}"),
                    stop,
                }
            }
            Err(_) => {
                thread.join().unwrap();
            }
        }
    }
    panic!("no local board port available");
}

impl Drop for LocalBoard {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

fn local_session(rig: &Rig, board: &LocalBoard) -> (String, String) {
    let state = rig._dir.path();
    crate::operator_auth::ensure_secret(state).unwrap();
    let secret = crate::operator_auth::read_secret(state).unwrap();
    let minted = crate::client::rpc(
        state,
        "operator_link_mint",
        json!({"secret":secret,"origin":"loopback"}),
    )
    .expect("real operator login mint refused");
    let nonce = minted["nonce"]
        .as_str()
        .expect("operator login nonce missing");
    let url = format!("http://127.0.0.1:{}/api/session", board.port);
    let mut response = http_agent()
        .post(&url)
        .header("Host", &board.host)
        .header("Origin", &format!("http://{}", board.host))
        .header("Sec-Fetch-Site", "same-origin")
        .header("Content-Type", "application/json")
        .header("X-Cadence-Board", "1")
        .send_json(json!({"nonce":nonce}))
        .expect("local operator session exchange failed");
    assert_eq!(
        response.status().as_u16(),
        200,
        "genuine local operator session refused"
    );
    let cookie = response
        .headers()
        .get("set-cookie")
        .expect("no operator cookie")
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let body: Value = response.body_mut().read_json().unwrap();
    let key = body["session_key"]
        .as_str()
        .filter(|s| !s.is_empty())
        .expect("operator page key missing")
        .to_string();
    (cookie, key)
}

fn local_get(board: &LocalBoard, path: &str, session: Option<&(String, String)>) -> (u16, Value) {
    let url = format!("http://127.0.0.1:{}{path}", board.port);
    let mut req = http_agent().get(&url).header("Host", &board.host);
    if let Some((cookie, key)) = session {
        req = req
            .header("Cookie", cookie)
            .header("X-Cadence-Session", key);
    }
    let mut response = req.call().expect("local board GET failed");
    let status = response.status().as_u16();
    let body = response
        .body_mut()
        .read_json()
        .expect("local board response not JSON");
    (status, body)
}

struct LocalStream {
    reader: BufReader<TcpStream>,
    who: &'static str,
}
impl LocalStream {
    fn open(
        board: &LocalBoard,
        session: Option<&(String, String)>,
        who: &'static str,
    ) -> (u16, Self) {
        let mut socket = TcpStream::connect(("127.0.0.1", board.port)).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(25)))
            .unwrap();
        write!(
            socket,
            "GET /api/stream?entities=1 HTTP/1.1\r\nHost: {}\r\n",
            board.host
        )
        .unwrap();
        if let Some((cookie, key)) = session {
            write!(socket, "Cookie: {cookie}\r\nX-Cadence-Session: {key}\r\n").unwrap();
        }
        write!(
            socket,
            "Accept: text/event-stream\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        let mut reader = BufReader::new(socket);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let status = line.split_whitespace().nth(1).unwrap().parse().unwrap();
        loop {
            line.clear();
            assert!(
                reader.read_line(&mut line).unwrap() > 0,
                "stream closed in headers"
            );
            if line == "\r\n" || line == "\n" {
                break;
            }
        }
        let mut stream = Self { reader, who };
        if status == 200 {
            let hello = stream
                .next(std::time::Instant::now() + Duration::from_secs(25), "hello")
                .expect("stream closed before hello");
            assert!(hello.contains("event: hello"), "{} stream lacks hello", who);
        }
        (status, stream)
    }
    fn next(&mut self, deadline: std::time::Instant, checkpoint: &str) -> Option<String> {
        let mut frame = String::new();
        loop {
            let remaining = deadline
                .checked_duration_since(std::time::Instant::now())
                .filter(|d| !d.is_zero())
                .unwrap_or_else(|| panic!("{} stream timed out at {checkpoint}", self.who));
            self.reader
                .get_ref()
                .set_read_timeout(Some(remaining))
                .unwrap();
            let mut line = String::new();
            match self.reader.read_line(&mut line) {
                Ok(0) => return None,
                Ok(_) if line == "\n" || line == "\r\n" => {
                    if !frame.is_empty() {
                        return Some(frame);
                    }
                }
                Ok(_) => frame.push_str(&line),
                Err(e) => panic!("{} stream timed out at {checkpoint}: {e}", self.who),
            }
        }
    }
    fn until(&mut self, marker: &str, allow_close: bool) -> String {
        let deadline = std::time::Instant::now() + Duration::from_secs(25);
        let mut frames = String::new();
        loop {
            match self.next(deadline, marker) {
                Some(frame) => {
                    let done = frame.contains(marker);
                    frames.push_str(&frame);
                    if done {
                        return frames;
                    }
                }
                None if allow_close => return frames,
                None => panic!("{} stream closed before {marker}", self.who),
            }
        }
    }
}

#[test]
fn anonymous_loopback_reads_cannot_inherit_operator_project_authority() {
    let rig = start(&[]);
    // Scoped member reads need the operator-provisioned keyring; without it
    // the daemon answers `scoped_cursor_unavailable`, not an authorization
    // verdict.
    let _keys = initialize_queue_keyring(&rig);
    seed(&rig);
    let opts = ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", rig.pm.to_str().unwrap());
    let shared = crate::daemon::Shared::new(rig._dir.path(), &opts).unwrap();
    shared
        .store
        .enqueue_task(
            "cad2215-worker",
            "CAD2215_ANON_UNBOUND_INITIAL",
            None,
            "cad2215-anon-initial-message",
            "user",
            Some("cad2215-free-task"),
        )
        .unwrap();
    let db = rusqlite::Connection::open(rig._dir.path().join("cadence.sqlite3")).unwrap();
    assert_eq!(
        db.execute(
            "UPDATE messages SET state='running' WHERE id='cad2215-anon-initial-message'",
            []
        )
        .unwrap(),
        1
    );
    let board = local_board(&rig);
    let session = local_session(&rig, &board);
    // An unsigned, non-sensitive route remains usable under this option.
    let (health_status, _) = local_get(&board, "/api/health", None);
    assert_eq!(health_status, 200, "anonymous health route unavailable");

    for path in ["/api/agents", "/api/agents/cad2215-worker", "/api/overview"] {
        let (owner_status, owner) = local_get(&board, path, Some(&session));
        assert_eq!(
            owner_status, 200,
            "authenticated operator recovery failed at {path}"
        );
        let owner = owner.to_string();
        if path == "/api/overview" {
            assert!(
                owner.contains("cad2215-secret-monitor"),
                "operator cannot recover private monitor"
            );
            assert!(
                owner.contains("cad2215-free-monitor"),
                "operator cannot recover unbound monitor"
            );
        } else {
            assert!(
                owner.contains("cad2215-secret-task"),
                "operator cannot recover private task"
            );
            assert!(
                owner.contains("cad2215-free-task"),
                "operator cannot recover unbound task"
            );
        }
        let (status, body) = local_get(&board, path, None);
        assert!(
            status == 200 || status == 401 || status == 403 || status == 404,
            "unexpected anonymous response at {path}: {status}"
        );
        let text = body.to_string();
        for hidden in [
            "cad2215-secret-job",
            "cad2215-secret-task",
            "cad2215-free-job",
            "cad2215-free-task",
            "cad2215-secret-monitor",
            "cad2215-free-monitor",
            "cad2215-secret-payload",
            "cad2215-free-payload",
            "Secret job",
            "Secret task",
        ] {
            assert!(!text.contains(hidden), "anonymous {path} leaked {hidden}");
        }
    }

    // Refusing anonymous SSE is a safe cutover. If admitted, exercise an
    // actual unbound-message entity change and a later visible issue barrier.
    let (owner_status, mut owner) = LocalStream::open(&board, Some(&session), "operator");
    assert_eq!(owner_status, 200, "operator recovery stream unavailable");
    let (anonymous_status, mut anonymous) = LocalStream::open(&board, None, "anonymous");
    assert!(
        anonymous_status == 200 || anonymous_status == 401 || anonymous_status == 403,
        "unexpected anonymous stream status {anonymous_status}"
    );
    if anonymous_status == 200 {
        let opts = ServeOptions::default();
        opts.provider_env
            .set("CADENCE_PM_DIR", rig.pm.to_str().unwrap());
        let shared = crate::daemon::Shared::new(rig._dir.path(), &opts).unwrap();
        shared
            .store
            .enqueue_task(
                "cad2215-worker",
                "CAD2215_ANON_UNBOUND_STREAM",
                None,
                "cad2215-anon-unbound-message",
                "user",
                Some("cad2215-free-task"),
            )
            .unwrap();
        let db = rusqlite::Connection::open(rig._dir.path().join("cadence.sqlite3")).unwrap();
        assert_eq!(
            db.execute(
                "UPDATE messages SET state='running' WHERE id='cad2215-anon-unbound-message'",
                []
            )
            .unwrap(),
            1
        );
        let owner_frames = owner.until("CAD2215_ANON_UNBOUND_STREAM", false);
        assert!(
            owner_frames.contains("cad2215-free-task"),
            "operator stream did not see unbound task"
        );
        let visible = rig.pm.join("visible/VIS-1/issue.md");
        std::fs::write(visible, "---\nid: VIS-1\ntitle: CAD2215_ANON_STREAM_BARRIER\nstatus: backlog\npriority: P2\ncreated: '2026-01-01T00:00:00Z'\n---\n").unwrap();
        let _ = owner.until("CAD2215_ANON_STREAM_BARRIER", false);
        let anonymous_frames = anonymous.until("CAD2215_ANON_STREAM_BARRIER", true);
        for hidden in [
            "cad2215-free-task",
            "cad2215-anon-unbound-message",
            "CAD2215_ANON_UNBOUND_STREAM",
        ] {
            assert!(
                !anonymous_frames.contains(hidden),
                "anonymous stream leaked {hidden}"
            );
        }
    }
}

#[test]
fn anonymous_loopback_cannot_read_ungranted_app_conversation_even_for_readable_project() {
    let (rig, _member, _owner, install, _digest) =
        super::cad2215_app_conversation_grant_http_acceptance::master_thread_real_grant_fixture();
    // The fixture helper seeded the real workspace-visible VIS-1 project,
    // issue, job and task and installed/approved the app, but granted no one.
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
    let (app, _) = shared
        .store
        .conversation_create(
            "master",
            &install,
            None,
            crate::store::ConversationKind::General,
        )
        .unwrap();
    shared
        .store
        .enqueue_task(
            "master",
            "ANON_APP_PROJECT_PRIVATE_2215",
            None,
            "anon-app-project-message",
            "user",
            Some("cad2215-visible-task"),
        )
        .unwrap();
    shared
        .store
        .thread_append_to(
            &app.id,
            crate::store::NewEntry {
                role: crate::store::ROLE_AGENT,
                kind: crate::store::KIND_ASSISTANT_TEXT,
                text: "ANON_APP_PROJECT_PRIVATE_2215",
                payload: None,
                message_id: Some("anon-app-project-message"),
            },
        )
        .unwrap()
        .expect("actual app conversation disappeared");
    let message = shared
        .store
        .message("anon-app-project-message")
        .unwrap()
        .unwrap();
    assert_eq!(message.task_id.as_deref(), Some("cad2215-visible-task"));
    let task = shared.store.task("cad2215-visible-task").unwrap();
    assert_eq!(
        shared.store.job(&task.job_id).unwrap().issue_id.as_deref(),
        Some("VIS-1")
    );

    // Positive anonymous control: the SAME source project is genuinely
    // readable on an ordinary non-Master thread, proving this is not a
    // blanket anonymous denial or an unreadable issue fixture.
    shared.store.ensure_thread("cad2215-worker").unwrap();
    shared
        .store
        .enqueue_task(
            "cad2215-worker",
            "ANON_PROJECT_CONTROL_2215",
            None,
            "anon-project-control-message",
            "user",
            Some("cad2215-visible-task"),
        )
        .unwrap();
    shared
        .store
        .thread_append(
            "cad2215-worker",
            crate::store::NewEntry {
                role: crate::store::ROLE_AGENT,
                kind: crate::store::KIND_ASSISTANT_TEXT,
                text: "ANON_PROJECT_CONTROL_2215",
                payload: None,
                message_id: Some("anon-project-control-message"),
            },
        )
        .unwrap()
        .expect("ordinary control thread missing");

    let board = local_board(&rig);
    let operator = local_session(&rig, &board);
    let (health, _) = local_get(&board, "/api/health", None);
    assert_eq!(health, 200, "anonymous loopback option is not active");
    let (control_status, control) =
        local_get(&board, "/api/threads/cad2215-worker?limit=100", None);
    assert_eq!(
        control_status, 200,
        "anonymous ordinary readable-project control failed: {control}"
    );
    assert!(
        control.to_string().contains("ANON_PROJECT_CONTROL_2215"),
        "anonymous control lost readable project: {control}"
    );

    let path = format!("/api/threads/master?conversation={}&limit=100", app.id);
    let (op_status, op_body) = local_get(&board, &path, Some(&operator));
    assert_eq!(
        op_status, 200,
        "real local operator cannot recover app conversation: {op_body}"
    );
    assert!(
        op_body
            .to_string()
            .contains("ANON_APP_PROJECT_PRIVATE_2215"),
        "operator recovery lacks actual project-bound app content: {op_body}"
    );
    let (status, body) = local_get(&board, &path, None);
    assert!(
        matches!(status, 200 | 400 | 401 | 403 | 404 | 503),
        "unexpected anonymous response: {status} {body}"
    );
    assert!(
        !body.to_string().contains("ANON_APP_PROJECT_PRIVATE_2215"),
        "anonymous read inherited a member conversation grant: status={status} body={body}"
    );
    // Exercise the real native relay too; identity comes from the proven
    // operator connection plus service/anonymous read_as, not request text
    // impersonating a member.
    let native = crate::test_seam::scoped(crate::test_seam::Asserted::Operator, || {
        crate::client::rpc(
            rig._dir.path(),
            "thread_read",
            json!({"alias":"master","conversation":app.id,"limit":100,"read_as":"service/anonymous"}),
        )
    });
    assert!(
        !format!("{native:?}").contains("ANON_APP_PROJECT_PRIVATE_2215"),
        "native anonymous relay leaked app content: {native:?}"
    );
}
