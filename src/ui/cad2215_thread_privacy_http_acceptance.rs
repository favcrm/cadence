//! CAD-2215: signed-board thread relay acceptance. The assistant binding and
//! request ledger come from the real signed assistant route; issue/assignment
//! updates below are synthetic fixture bindings, not approval or OS isolation.
use super::*;
use std::io::{BufRead, BufReader};
use std::time::Instant;

fn shared(rig: &Rig) -> std::sync::Arc<crate::daemon::Shared> {
    let opts = ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", rig.pm.to_str().unwrap());
    crate::daemon::Shared::new(rig._dir.path(), &opts).unwrap()
}

fn push(shared: &crate::daemon::Shared, alias: &str, task: &str, id: &str, text: &str) {
    shared
        .store
        .enqueue_task(alias, text, None, id, "user", Some(task))
        .unwrap();
}

fn get_status(rig: &Rig, cookie: &str, path: &str) -> (u16, Value) {
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

struct ThreadStream {
    reader: BufReader<TcpStream>,
}
impl ThreadStream {
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
            "thread stream refused: {line}"
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
        assert!(stream
            .frame(Instant::now() + Duration::from_secs(25))
            .contains(": ping"));
        stream
    }
    fn frame(&mut self, deadline: Instant) -> String {
        let mut frame = String::new();
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .filter(|d| !d.is_zero())
                .expect("thread stream checkpoint deadline");
            self.reader
                .get_ref()
                .set_read_timeout(Some(remaining))
                .unwrap();
            let mut line = String::new();
            assert!(
                self.reader
                    .read_line(&mut line)
                    .expect("thread stream checkpoint timeout")
                    > 0,
                "thread stream closed before checkpoint"
            );
            if line == "\n" || line == "\r\n" {
                if !frame.is_empty() {
                    return frame;
                }
            } else {
                frame.push_str(&line);
            }
        }
    }
    fn through(&mut self, marker: &str) -> String {
        let deadline = Instant::now() + Duration::from_secs(25);
        let mut all = String::new();
        loop {
            let frame = self.frame(deadline);
            let found = frame.contains(marker);
            all.push_str(&frame);
            if found {
                return all;
            }
        }
    }
}

#[test]
fn signed_board_thread_pages_and_stream_enforce_project_and_private_ownership() {
    let _flag = AssistantFlagOn::arm();
    let rig = start(&["ana", "ben"]);
    let _keys = initialize_queue_keyring(&rig);
    seed(&rig);
    let shared = shared(&rig);
    // Synthetic fixture setup: unattributed task enqueues do not create home threads.
    shared.store.ensure_thread("cad2215-worker").unwrap();
    let ana = login(&rig, "ana", "member");
    let ben = login(&rig, "ben", "member");
    let owner = login(&rig, "cad2215-thread-owner", "owner");

    // Two visible entries establish a positive forward and backward baseline.
    push(
        &shared,
        "cad2215-worker",
        "cad2215-visible-task",
        "cad2215-thread-visible-1",
        "CAD2215_THREAD_VISIBLE_1",
    );
    push(
        &shared,
        "cad2215-worker",
        "cad2215-visible-task",
        "cad2215-thread-visible-2",
        "CAD2215_THREAD_VISIBLE_2",
    );
    let forward = "/api/threads/cad2215-worker?limit=1";
    let tail = "/api/threads/cad2215-worker?tail=1&limit=1";
    let first = get(&rig, &ben, forward);
    let last = get(&rig, &ben, tail);
    assert!(
        first.to_string().contains("CAD2215_THREAD_VISIBLE_1"),
        "member lost first authorized row: {first}"
    );
    assert!(
        last.to_string().contains("CAD2215_THREAD_VISIBLE_2"),
        "member lost last authorized row: {last}"
    );
    let last_seq = last["entries"][0]["seq"].as_i64().expect("visible seq");
    let before = format!("/api/threads/cad2215-worker?before={last_seq}&limit=1");
    let previous = get(&rig, &ben, &before);
    assert!(
        previous.to_string().contains("CAD2215_THREAD_VISIBLE_1"),
        "member lost backward page: {previous}"
    );
    let empty_path = format!("/api/threads/cad2215-worker?after={last_seq}&limit=1");
    let empty = get(&rig, &ben, &empty_path);

    // Hidden-only insertion must change neither returned metadata nor rows,
    // and must not occupy the limit. The three task bindings are synthetic
    // in seed(): their project associations are resolved through jobs.
    push(
        &shared,
        "cad2215-worker",
        "cad2215-secret-task",
        "cad2215-thread-secret",
        "CAD2215_THREAD_SECRET",
    );
    push(
        &shared,
        "cad2215-worker",
        "cad2215-free-task",
        "cad2215-thread-unbound",
        "CAD2215_THREAD_UNBOUND",
    );
    for (path, baseline) in [
        (forward, &first),
        (tail, &last),
        (before.as_str(), &previous),
        (empty_path.as_str(), &empty),
    ] {
        let now = get(&rig, &ben, path);
        assert_eq!(
            &now, baseline,
            "hidden-only rows changed page or metadata at {path}"
        );
    }
    let operator = get(&rig, &owner, "/api/threads/cad2215-worker?tail=1&limit=100");
    for marker in [
        "CAD2215_THREAD_VISIBLE_1",
        "CAD2215_THREAD_SECRET",
        "CAD2215_THREAD_UNBOUND",
    ] {
        assert!(
            operator.to_string().contains(marker),
            "operator recovery omitted {marker}: {operator}"
        );
    }

    // The stream begins after the last visible row, so it must encounter the
    // hidden rows. A subsequent authorized entry is the ordered checkpoint.
    let mut member_stream = ThreadStream::open(&rig, &ben, last_seq);
    let mut owner_stream = ThreadStream::open(&rig, &owner, last_seq);
    let operator_frames = owner_stream.through("CAD2215_THREAD_UNBOUND");
    assert!(operator_frames.contains("CAD2215_THREAD_SECRET"));
    push(
        &shared,
        "cad2215-worker",
        "cad2215-visible-task",
        "cad2215-thread-barrier",
        "CAD2215_THREAD_BARRIER",
    );
    let frames = member_stream.through("CAD2215_THREAD_BARRIER");
    for hidden in [
        "CAD2215_THREAD_SECRET",
        "CAD2215_THREAD_UNBOUND",
        "cad2215-thread-secret",
        "cad2215-thread-unbound",
    ] {
        assert!(
            !frames.contains(hidden),
            "member stream leaked {hidden}: {frames}"
        );
    }

    // A real assistant request supplies the canonical private ownership
    // ledger and readable issue binding. The dispatch assignment is synthetic.
    let env = rig.envelope("ana", json!({}), json!({}));
    let (status, requested) = rig.call("job_request", &env,
        &json!({"title":"CAD2215_THREAD_PRIVATE_JOB", "request_key":"cad2215-thread-private", "issue":"VIS-9"}));
    assert_eq!(
        status, 200,
        "signed assistant job request failed: {requested}"
    );
    let job = requested["job"].as_str().expect("canonical job");
    let binding = shared
        .store
        .assistant_binding(ORG, "ana")
        .unwrap()
        .expect("canonical assistant binding");
    assert!(shared.store.assist_request_owns(ORG, "ana", job).unwrap());
    assert!(!shared.store.assist_request_owns(ORG, "ben", job).unwrap());
    // Synthetic legacy transition: the real signed creation above was bound
    // to VIS-9; simulate an older retained job that lost its binding.
    let db = rusqlite::Connection::open(rig._dir.path().join("cadence.sqlite3")).unwrap();
    assert_eq!(
        db.execute("UPDATE jobs SET issue_id=NULL WHERE id=?1", [job])
            .unwrap(),
        1
    );
    // Even the owner cannot read evidence before the job has a resolvable
    // authorized project. The following binding is synthetic fixture setup.
    let unbound_env = rig.envelope("ana", json!({}), json!({}));
    let (unbound_status, unbound_body) =
        rig.call("job_evidence", &unbound_env, &json!({"job": job}));
    assert!(
        matches!(unbound_status, 400 | 401 | 403 | 404),
        "owner read unbound job evidence: {unbound_body}"
    );
    assert_eq!(
        db.execute("UPDATE jobs SET issue_id='VIS-1' WHERE id=?1", [job])
            .unwrap(),
        1
    );
    // Independently prove ownership after the project binding is in place.
    let ana_env = rig.envelope("ana", json!({}), json!({}));
    let (ana_evidence_status, ana_evidence) =
        rig.call("job_evidence", &ana_env, &json!({"job": job}));
    assert_eq!(
        ana_evidence_status, 200,
        "owner evidence control failed: {ana_evidence}"
    );
    let ben_env = rig.envelope("ben", json!({}), json!({}));
    let (ben_evidence_status, ben_evidence) =
        rig.call("job_evidence", &ben_env, &json!({"job": job}));
    assert!(
        matches!(ben_evidence_status, 401 | 403 | 404),
        "foreign evidence control unexpectedly admitted: {ben_evidence}"
    );
    let task: String = db
        .query_row(
            "SELECT id FROM tasks WHERE job_id=?1 ORDER BY id LIMIT 1",
            [job],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        db.execute(
            "UPDATE tasks SET assignee=?1,state='running' WHERE id=?2",
            rusqlite::params![binding.alias, task]
        )
        .unwrap(),
        1
    );
    // Ensure the canonical assistant alias has the home thread this route reads.
    shared.store.ensure_thread(&binding.alias).unwrap();
    push(
        &shared,
        &binding.alias,
        &task,
        "cad2215-thread-private-message",
        "CAD2215_THREAD_PRIVATE_ASSISTANT",
    );
    let path = format!("/api/threads/{}?tail=1&limit=100", binding.alias);
    let (ana_status, ana_body) = get_status(&rig, &ana, &path);
    let (operator_status, operator_body) = get_status(&rig, &owner, &path);
    for (who, status, body) in [
        ("owner member", ana_status, ana_body),
        ("operator", operator_status, operator_body),
    ] {
        assert_eq!(status, 200, "{who} private thread refused: {body}");
        assert!(
            body.to_string()
                .contains("CAD2215_THREAD_PRIVATE_ASSISTANT"),
            "{who} private thread lacks content: {body}"
        );
    }
    let (status, body) = get_status(&rig, &ben, &path);
    let missing_alias = "cad2215-definitely-missing-thread";
    let missing_path = format!("/api/threads/{missing_alias}?tail=1&limit=100");
    let (missing_status, missing_body) = get_status(&rig, &ben, &missing_path);
    assert!(
        matches!(status, 200 | 403 | 404),
        "unexpected private refusal {status}: {body}"
    );
    let text = body.to_string();
    for hidden in [
        "CAD2215_THREAD_PRIVATE_ASSISTANT",
        "cad2215-thread-private-message",
        task.as_str(),
        job,
    ] {
        assert!(
            !text.contains(hidden),
            "foreign private thread leaked {hidden}: {body}"
        );
    }
    if status == 200 {
        assert!(
            !text.contains(binding.alias.as_str()),
            "successful foreign thread response disclosed its identity: {body}"
        );
    } else if status == missing_status {
        assert_eq!(
            text.replace(binding.alias.as_str(), "<requested>"),
            missing_body
                .to_string()
                .replace(missing_alias, "<requested>"),
            "private and nonexistent thread refusals disclose different information"
        );
    }
}
