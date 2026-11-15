//! CAD-2215 independent mixed-version thread relay acceptance.
//! The product implementer must not edit or weaken this check.
//! A test-only switch simulates unacknowledged unscoped old-daemon pages;
//! real signed session admission and the real thread store remain in use.

use super::*;
use std::io::{BufRead, BufReader};

#[allow(clippy::type_complexity)] // one fixture callback, one shape
fn legacy_thread_rig(
    legacy: Arc<AtomicBool>,
    observer: Arc<dyn Fn(&str, &str) + Send + Sync>,
) -> Rig {
    let dir = tempfile::Builder::new()
        .prefix("cad2215-legacy-thread")
        .tempdir()
        .unwrap();
    let state = dir.path().to_path_buf();
    let pm = state.join("pm");
    crate::issue::Pm::init(&pm).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let mut threads = Vec::new();
    {
        let daemon_state = state.clone();
        let opts = ServeOptions {
            test_seam: true,
            stop: Some(stop.clone()),
            thread_read_ignore_relay: Some(legacy),
            assist_request_sweep_observer: Some(observer),
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
                "legacy-mode daemon did not start"
            );
            std::thread::yield_now();
        }
    }
    let key = Ed25519KeyPair::from_seed_unchecked(&[140; 32]).unwrap();
    let public_key = URL_SAFE_NO_PAD.encode(key.public_key().as_ref());
    let verify_public = key.public_key().as_ref().to_vec();
    let platform = loopback_server();
    let platform_port = match platform.server_addr() {
        tiny_http::ListenAddr::IP(a) => a.port(),
        _ => panic!("fixture platform address"),
    };
    let public_host = format!("localhost:{platform_port}");
    let issuer = format!("http://{public_host}");
    let authority: Arc<Mutex<Authority>> = Arc::new(Mutex::new(Authority {
        status: 200,
        ..Authority::default()
    }));
    {
        let (stop, authority, issuer, public_host) = (
            stop.clone(),
            authority.clone(),
            issuer.clone(),
            public_host.clone(),
        );
        threads.push(std::thread::spawn(move || loop {
            if stop.load(Ordering::SeqCst) {
                break;
            }
            if let Ok(Some(request)) = platform.recv_timeout(Duration::from_millis(200)) {
                answer_platform(
                    request,
                    &authority,
                    &issuer,
                    &public_host,
                    &public_key,
                    &verify_public,
                );
            }
        }));
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
            authority
                .lock()
                .unwrap()
                .members
                .insert(sub.to_string(), MemberAuthority::live(sub));
        }
    }
    let mut board_port = 0;
    for port in 3110..=3199u16 {
        if port == platform_port {
            continue;
        }
        let (startup, ready) = std::sync::mpsc::channel();
        let opts = ServeOpts {
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
        let thread = std::thread::spawn(move || drop(crate::ui::serve(&state, &pm, &opts)));
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
    assert!(board_port > 0, "fixture board could not bind");
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

fn thread_get(rig: &Rig, cookie: &str, path: &str) -> (u16, String) {
    let mut response = http_agent()
        .get(&format!("http://127.0.0.1:{}{path}", rig.board_port))
        .header("Host", &rig.public_host)
        .header("Cookie", cookie)
        .call()
        .expect("thread HTTP request failed");
    let status = response.status().as_u16();
    let text = response
        .body_mut()
        .read_to_string()
        .expect("thread response unreadable");
    (status, text)
}

struct OpenThread {
    reader: BufReader<TcpStream>,
}
impl OpenThread {
    fn open(rig: &Rig, cookie: &str, after: i64) -> Self {
        let mut socket = TcpStream::connect(("127.0.0.1", rig.board_port)).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(25)))
            .unwrap();
        write!(socket, "GET /api/threads/cad2215-worker/stream?after={after} HTTP/1.1\r\nHost: {}\r\nCookie: {}\r\nAccept: text/event-stream\r\nConnection: close\r\n\r\n", rig.public_host, cookie).unwrap();
        let mut reader = BufReader::new(socket);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        assert!(
            line.starts_with("HTTP/1.1 200 "),
            "current scoped stream refused: {line}"
        );
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
        let mut stream = Self { reader };
        let ping = stream
            .next(std::time::Instant::now() + Duration::from_secs(25))
            .expect("stream closed before ping");
        assert!(ping.contains(": ping"), "stream did not initialize: {ping}");
        stream
    }
    fn next(&mut self, deadline: std::time::Instant) -> Option<String> {
        let mut frame = String::new();
        loop {
            let left = deadline
                .checked_duration_since(std::time::Instant::now())
                .filter(|d| !d.is_zero())
                .expect("legacy stream did not reach closure deadline");
            self.reader.get_ref().set_read_timeout(Some(left)).unwrap();
            let mut line = String::new();
            match self.reader.read_line(&mut line) {
                Ok(0) => return None,
                Ok(_) if line == "\n" || line == "\r\n" => {
                    if !frame.is_empty() {
                        return Some(frame);
                    }
                }
                Ok(_) => frame.push_str(&line),
                Err(e) => panic!("legacy stream did not reach closure: {e}"),
            }
        }
    }
    fn through(&mut self, marker: &str) -> String {
        let deadline = std::time::Instant::now() + Duration::from_secs(25);
        let mut all = String::new();
        loop {
            let frame = self
                .next(deadline)
                .expect("control stream closed before marker");
            let found = frame.contains(marker);
            all.push_str(&frame);
            if found {
                return all;
            }
        }
    }
    fn drain_to_eof(&mut self) -> String {
        let deadline = std::time::Instant::now() + Duration::from_secs(25);
        let mut all = String::new();
        while let Some(frame) = self.next(deadline) {
            all.push_str(&frame);
        }
        all
    }
}

#[test]
fn mixed_version_thread_pages_refuse_unacknowledged_scope_on_http_and_open_stream() {
    let _flag = AssistantFlagOn::arm();
    let legacy = Arc::new(AtomicBool::new(false));
    let (observer, events) = queue_observer();
    let rig = legacy_thread_rig(legacy.clone(), observer);
    seed(&rig);
    let opts = ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", rig.pm.to_str().unwrap());
    let shared = crate::daemon::Shared::new(rig._dir.path(), &opts).unwrap();
    shared.store.ensure_thread("cad2215-worker").unwrap();
    let env = rig.envelope("ana", json!({}), json!({}));
    let (status, requested) = rig.call("job_request", &env, &json!({
        "title":"CAD2215_LEGACY_THREAD_PRIVATE_JOB", "request_key":"cad2215-legacy-thread", "issue":"VIS-9"
    }));
    assert_eq!(status, 200, "signed owner request failed: {requested}");
    let job = promoted_job(&rig, &events, "ana", &requested);
    assert!(shared.store.assist_request_owns(ORG, "ana", &job).unwrap());
    assert!(!shared.store.job_is_subscriber(&job, ORG, "ben").unwrap());
    let db = rusqlite::Connection::open(rig._dir.path().join("cadence.sqlite3")).unwrap();
    // Synthetic same-issue transition and ordinary-worker assignment; the
    // ownership/subscriber ledger remains the real signed-request ledger.
    assert_eq!(
        db.execute("UPDATE jobs SET issue_id='VIS-1' WHERE id=?1", [&job])
            .unwrap(),
        1
    );
    let task: String = db
        .query_row(
            "SELECT id FROM tasks WHERE job_id=?1 ORDER BY id LIMIT 1",
            [&job],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        db.execute(
            "UPDATE tasks SET assignee='cad2215-worker',state='running' WHERE id=?1",
            [&task]
        )
        .unwrap(),
        1
    );
    for (id, task_id, text) in [
        (
            "cad2215-legacy-thread-visible",
            "cad2215-visible-task",
            "CAD2215_LEGACY_THREAD_VISIBLE",
        ),
        (
            "cad2215-legacy-thread-private",
            task.as_str(),
            "CAD2215_LEGACY_THREAD_PRIVATE",
        ),
        (
            "cad2215-legacy-thread-secret",
            "cad2215-secret-task",
            "CAD2215_LEGACY_THREAD_SECRET",
        ),
    ] {
        shared
            .store
            .enqueue_task("cad2215-worker", text, None, id, "user", Some(task_id))
            .unwrap();
    }
    let ben = login(&rig, "ben", "member");
    let owner = login(&rig, "cad2215-legacy-thread-owner", "owner");
    let path = "/api/threads/cad2215-worker?tail=1&limit=100";
    let (status, scoped) = thread_get(&rig, &ben, path);
    assert_eq!(status, 200, "current scoped member page refused: {scoped}");
    assert!(
        scoped.contains("CAD2215_LEGACY_THREAD_VISIBLE"),
        "current member lost visible entry"
    );
    for secret in [
        "CAD2215_LEGACY_THREAD_PRIVATE",
        "CAD2215_LEGACY_THREAD_SECRET",
    ] {
        assert!(!scoped.contains(secret), "current daemon leaked {secret}");
    }
    let (status, operator) = thread_get(&rig, &owner, path);
    assert_eq!(status, 200, "operator recovery page refused");
    for marker in [
        "CAD2215_LEGACY_THREAD_VISIBLE",
        "CAD2215_LEGACY_THREAD_PRIVATE",
        "CAD2215_LEGACY_THREAD_SECRET",
    ] {
        assert!(
            operator.contains(marker),
            "operator lacks recovery marker {marker}"
        );
    }
    let scoped_json: Value = serde_json::from_str(&scoped).unwrap();
    let cursor = scoped_json["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry.to_string().contains("CAD2215_LEGACY_THREAD_VISIBLE"))
        .and_then(|entry| entry["seq"].as_i64())
        .expect("visible checkpoint sequence missing");
    // An already-open stream begins with a real scoped page.
    let mut stream = OpenThread::open(&rig, &ben, cursor);
    legacy.store(true, Ordering::Release);
    for path in [
        "/api/threads/cad2215-worker?tail=1&limit=100",
        "/api/threads/cad2215-worker/stream",
        "/api/threads/cad2215-worker/stream?after=0",
    ] {
        let (status, body) = thread_get(&rig, &ben, path);
        assert_eq!(
            status, 502,
            "old daemon {path} did not fail before headers/data: {body}"
        );
        for hidden in [
            "CAD2215_LEGACY_THREAD_PRIVATE",
            "CAD2215_LEGACY_THREAD_SECRET",
            "cad2215-legacy-thread-private",
            "cad2215-legacy-thread-secret",
        ] {
            assert!(!body.contains(hidden), "old daemon {path} leaked {hidden}");
        }
        assert!(
            !body.contains("event: entry"),
            "old daemon opened unverified stream"
        );
    }
    // Trigger a private post-switch event. The operator's independent stream
    // proves the real event exists; the member must reach EOF without it.
    let mut owner_stream = OpenThread::open(&rig, &owner, cursor);
    shared
        .store
        .enqueue_task(
            "cad2215-worker",
            "CAD2215_LEGACY_THREAD_LATE_PRIVATE",
            None,
            "cad2215-legacy-thread-late-private",
            "user",
            Some(&task),
        )
        .unwrap();
    assert!(owner_stream
        .through("CAD2215_LEGACY_THREAD_LATE_PRIVATE")
        .contains("CAD2215_LEGACY_THREAD_LATE_PRIVATE"));
    let member_frames = stream.drain_to_eof();
    for hidden in [
        "CAD2215_LEGACY_THREAD_PRIVATE",
        "CAD2215_LEGACY_THREAD_SECRET",
        "CAD2215_LEGACY_THREAD_LATE_PRIVATE",
        "cad2215-legacy-thread-late-private",
    ] {
        assert!(
            !member_frames.contains(hidden),
            "open stream leaked {hidden}: {member_frames}"
        );
    }
    let (status, operator_after) = thread_get(&rig, &owner, path);
    assert_eq!(
        status, 200,
        "operator recovery failed against old-mode daemon"
    );
    assert!(operator_after.contains("CAD2215_LEGACY_THREAD_LATE_PRIVATE"));
}
