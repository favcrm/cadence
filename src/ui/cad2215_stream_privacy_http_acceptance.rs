//! CAD-2215 independent real signed-board stream acceptance.
//! The product implementer must not edit or weaken this test.
//! This does not establish local-agent process/filesystem isolation.

use super::*;
use std::io::{BufRead, BufReader};

#[path = "cad2215_stream_rebase_race_http_acceptance.rs"]
mod cad2215_stream_rebase_race_http_acceptance;

#[path = "cad2215_board_shared_job_http_acceptance.rs"]
mod cad2215_board_shared_job_http_acceptance;
#[path = "cad2215_job_legacy_http_acceptance.rs"]
mod cad2215_job_legacy_http_acceptance;
#[path = "cad2215_stream_demotion_http_acceptance.rs"]
mod cad2215_stream_demotion_http_acceptance;

struct EventStream {
    reader: BufReader<TcpStream>,
    who: &'static str,
}

impl EventStream {
    fn open(rig: &Rig, cookie: &str, who: &'static str) -> Self {
        let mut socket = TcpStream::connect(("127.0.0.1", rig.board_port)).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(25)))
            .unwrap();
        write!(socket,
            "GET /api/stream?entities=1 HTTP/1.1\r\nHost: {}\r\nCookie: {}\r\nAccept: text/event-stream\r\nConnection: close\r\n\r\n",
            rig.public_host, cookie).unwrap();
        let mut reader = BufReader::new(socket);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        assert!(
            line.starts_with("HTTP/1.1 200 "),
            "signed stream not admitted: {line}"
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
        let mut stream = Self { reader, who };
        let hello = stream
            .next(std::time::Instant::now() + Duration::from_secs(25), "hello")
            .expect("stream closed before hello");
        assert!(
            hello.contains("event: hello"),
            "stream did not send hello: {hello}"
        );
        stream
    }

    // Each complete frame is an observation; timeout is failure, not a
    // substitute for an event-driven checkpoint. EOF is explicit closure.
    fn next(&mut self, deadline: std::time::Instant, checkpoint: &str) -> Option<String> {
        let mut frame = String::new();
        loop {
            let remaining = deadline
                .checked_duration_since(std::time::Instant::now())
                .filter(|d| !d.is_zero())
                .unwrap_or_else(|| {
                    panic!(
                        "{} stream did not reach checkpoint {checkpoint} before absolute deadline",
                        self.who
                    )
                });
            self.reader
                .get_ref()
                .set_read_timeout(Some(remaining))
                .unwrap();
            let mut line = String::new();
            match self.reader.read_line(&mut line) {
                Ok(0) => return None,
                Ok(_) => {
                    if line == "\n" || line == "\r\n" {
                        if !frame.is_empty() {
                            return Some(frame);
                        }
                    } else {
                        frame.push_str(&line);
                    }
                }
                Err(e) => panic!(
                    "{} stream did not reach checkpoint {checkpoint}: {e}",
                    self.who
                ),
            }
        }
    }

    fn through(&mut self, marker: &str) -> String {
        let deadline = std::time::Instant::now() + Duration::from_secs(25);
        let mut frames = String::new();
        loop {
            let frame = self
                .next(deadline, marker)
                .unwrap_or_else(|| panic!("{} stream closed before checkpoint {marker}", self.who));
            let found = frame.contains(marker);
            frames.push_str(&frame);
            if found {
                return frames;
            }
        }
    }

    // A closed stream or a later readable checkpoint is an observable safe
    // end of the interval. Every frame before that point is examined.
    fn until_checkpoint_or_close(&mut self, checkpoint: &str) -> String {
        let deadline = std::time::Instant::now() + Duration::from_secs(25);
        let mut frames = String::new();
        while let Some(frame) = self.next(deadline, checkpoint) {
            let done = frame.contains(checkpoint);
            frames.push_str(&frame);
            if done {
                break;
            }
        }
        frames
    }
}

fn issue_text(id: &str, title: &str) -> String {
    format!("---\nid: {id}\ntitle: {title}\nstatus: backlog\npriority: P2\ncreated: '2026-01-01T00:00:00Z'\n---\n")
}

#[test]
fn signed_member_stream_hides_unbound_work_and_stops_at_revocation() {
    let rig = start(&[]);
    seed(&rig);
    // Both projects are readable at admission. This does not authorize the
    // unbound task. A later fixture mutation makes the secret project private.
    let policy = rig.pm.join("secret/project.yaml");
    std::fs::write(&policy, "key: secret\nprefix: SEC\n").unwrap();
    let member_cookie = login(&rig, "cad2215-stream-member", "member");
    let owner_cookie = login(&rig, "cad2215-stream-owner", "owner");
    let mut owner = EventStream::open(&rig, &owner_cookie, "owner");
    let mut member = EventStream::open(&rig, &member_cookie, "member");

    // Trigger a real agent entity diff containing an unbound running message.
    // Waiting for the owner marker proves the daemon snapshot saw the mutation.
    let opts = ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", rig.pm.to_str().unwrap());
    let shared = crate::daemon::Shared::new(rig._dir.path(), &opts).unwrap();
    shared
        .store
        .enqueue_task(
            "cad2215-worker",
            "CAD2215_STREAM_UNBOUND_TITLE",
            None,
            "cad2215-stream-unbound-message",
            "user",
            Some("cad2215-free-task"),
        )
        .unwrap();
    let db = rusqlite::Connection::open(rig._dir.path().join("cadence.sqlite3")).unwrap();
    assert_eq!(
        db.execute(
            "UPDATE messages SET state='running' WHERE id='cad2215-stream-unbound-message'",
            []
        )
        .unwrap(),
        1
    );
    let owner_initial = owner.through("CAD2215_STREAM_UNBOUND_TITLE");
    assert!(
        owner_initial.contains("cad2215-free-task"),
        "operator did not observe unbound task frame"
    );

    // The later readable issue event is an ordered shared-watcher barrier.
    // All earlier entity and metadata frames on the member socket are checked.
    let visible = rig.pm.join("visible/VIS-1/issue.md");
    std::fs::write(
        &visible,
        issue_text("VIS-1", "CAD2215_STREAM_PUBLIC_BARRIER"),
    )
    .unwrap();
    let _ = owner.through("CAD2215_STREAM_PUBLIC_BARRIER");
    let member_initial = member.through("CAD2215_STREAM_PUBLIC_BARRIER");
    for forbidden in [
        "cad2215-free-task",
        "cad2215-free-job",
        "cad2215-stream-unbound-message",
        "CAD2215_STREAM_UNBOUND_TITLE",
    ] {
        assert!(
            !member_initial.contains(forbidden),
            "member stream leaked unbound {forbidden}"
        );
    }

    // Issue diffs precede agent diffs within one shared-watcher tick. A
    // second later visible issue checkpoint drains any agent frames queued
    // behind the first issue barrier before the policy is revoked.
    std::fs::write(
        &visible,
        issue_text("VIS-1", "CAD2215_STREAM_PRE_REVOKE_DRAIN"),
    )
    .unwrap();
    let _ = owner.through("CAD2215_STREAM_PRE_REVOKE_DRAIN");
    let drained = member.through("CAD2215_STREAM_PRE_REVOKE_DRAIN");
    for forbidden in [
        "cad2215-free-task",
        "cad2215-free-job",
        "cad2215-stream-unbound-message",
        "CAD2215_STREAM_UNBOUND_TITLE",
    ] {
        assert!(
            !drained.contains(forbidden),
            "member pre-revocation stream leaked unbound {forbidden}; received frames: {drained}"
        );
    }
    // Revoke while the already-admitted stream remains open, then cause a
    // protected issue entity update. The operator receives its marker, so
    // refusal/closure on the member is not a vacuous no-change observation.
    std::fs::write(
        &policy,
        "key: secret\nprefix: SEC\npolicy:\n  version: 1\n  visibility: private\n",
    )
    .unwrap();
    let secret = rig.pm.join("secret/SEC-1/issue.md");
    std::fs::write(
        &secret,
        issue_text("SEC-1", "CAD2215_STREAM_REVOKED_SECRET"),
    )
    .unwrap();
    let owner_revoked = owner.through("CAD2215_STREAM_REVOKED_SECRET");
    assert!(owner_revoked.contains("CAD2215_STREAM_REVOKED_SECRET"));

    // If the member remains connected, a second public update supplies a
    // bounded positive checkpoint after the protected owner event. A server
    // that closes revoked streams passes only after actual EOF is observed.
    std::fs::write(&visible, issue_text("VIS-1", "CAD2215_STREAM_AFTER_REVOKE")).unwrap();
    let _ = owner.through("CAD2215_STREAM_AFTER_REVOKE");
    let member_after = member.until_checkpoint_or_close("CAD2215_STREAM_AFTER_REVOKE");
    assert!(
        !member_after.contains("CAD2215_STREAM_REVOKED_SECRET"),
        "revoked stream leaked private issue title"
    );
    assert!(
        !member_after.contains("SEC-1"),
        "revoked stream leaked private issue identifier; received frames: {member_after}"
    );
}
