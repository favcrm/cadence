//! CAD-2215 independent scoped live-stream acceptance.
//! The product implementer must not edit or weaken this check.
//!
//! This drives the REAL signed board (`/api/stream` over HTTP on the fixture
//! board port, fed by the daemon read model it relays) for two member
//! readers:
//!
//! - a member who cannot read one of the tracker's projects, and
//! - a member who reads the project but cannot see another member's private
//!   assistant job inside it.
//!
//! The contract under test is per-reader live updates: a change the reader can
//! observe produces frames, and a change only another reader can observe
//! produces no frame at all — no legacy collection invalidation (`issues`,
//! `jobs`, `agents`, `monitoring`, `aggregates`, `threads`, `master`), no
//! entity upsert or tombstone, and no `agent_meta`/counter frame.
//!
//! Ordering is event-driven, never timed. The operator stream's marker for the
//! hidden change is the barrier: reading it proves the shared watcher already
//! published that tick. A hidden TRACKER change is judged by the COUNT of the
//! generic `issues` invalidation in the member interval against a control tick
//! that a visible tracker change produced — the hidden tick must add none — and
//! by the absence of the hidden entity/tombstone. A hidden DAEMON change is
//! judged by the absence of the daemon classes
//! (`jobs`/`agents`/`agent`/`threads`/`monitoring`). Count- and class-based
//! judgments, not exact batch equality, stay truthful when this fixture's
//! daemon legitimately coalesces its own member-visible card frames into a
//! window.
//!
//! Heartbeats are protocol liveness, not observations: each stream's opening
//! `: ping` is asserted, and later `: ping`/`event: heartbeat` frames are
//! skipped by the reader instead of being counted as leakage or as a barrier.
//!
//! Limits of this check, stated so it is not read as more than it is:
//! - Project policy, job ownership and the platform membership rows are local
//!   fixtures. This exercises the board's HTTP/SSE authorization over them.
//! - It does NOT establish local-agent process/filesystem isolation; the
//!   tracker files and the daemon socket remain readable by same-UID
//!   processes (CAD-2216).
//! - The member-request `issue` binding surface itself is covered by the
//!   sibling binding/mutation acceptance modules; the request here only seeds
//!   the private job's hidden unit under a real ownership ledger.
//! - The fixture parks every monitor's next check after `seed` (the seeded
//!   monitors are due immediately, and a due check rewrites the member's
//!   monitoring projection) so the daemon loop cannot inject `monitoring`
//!   frames while the assertions run. An independently inserted alert on the
//!   readable task is the positive control that the member monitoring
//!   projection is still live and delivered, so forbidding `monitoring` in the
//!   hidden interval is not vacuous.
//! - The delivery-projection failure is driven by the REAL `delivery.json`
//!   file (unparseable JSON), not a test seam: a member baseline that cannot
//!   be resolved before the SSE head is refused with HTTP 503, and a later
//!   unprovable projection closes an established stream instead of
//!   heartbeating while dropping that reader's updates.
//! - The scoped THREAD projection (`visible_threads_fp`) has no fixture-only
//!   failure trigger: its only failure is a `thread_read` daemon error, which
//!   a real-fixture file mutation cannot produce without a new seam. That
//!   branch is therefore not exercised here.

use super::*;
#[path = "cad2215_scoped_monitor_frames_http_acceptance.rs"]
mod cad2215_scoped_monitor_frames_http_acceptance;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

/// A per-frame deadline is a test failure, never evidence of absence.
const STREAM_WAIT: Duration = Duration::from_secs(30);

/// One signed member SSE connection. Frames are complete `event:`/`data:`
/// blocks; EOF is the server closing the stream, which is an observation.
struct MemberStream {
    reader: BufReader<TcpStream>,
    who: &'static str,
}

/// Every non-keepalive frame read between two observations.
struct Interval {
    frames: Vec<String>,
    closed: bool,
}

fn event_name(frame: &str) -> Option<&str> {
    frame.strip_prefix("event: ")?.split('\n').next()
}

impl MemberStream {
    fn open(rig: &Rig, cookie: &str, entities: bool, who: &'static str) -> Self {
        let path = if entities {
            "/api/stream?entities=1"
        } else {
            "/api/stream"
        };
        let mut socket = TcpStream::connect(("127.0.0.1", rig.board_port)).unwrap();
        socket.set_read_timeout(Some(STREAM_WAIT)).unwrap();
        write!(
            socket,
            "GET {path} HTTP/1.1\r\nHost: {}\r\nCookie: {cookie}\r\n\
             Accept: text/event-stream\r\nConnection: close\r\n\r\n",
            rig.public_host
        )
        .unwrap();
        let mut reader = BufReader::new(socket);
        let mut status = String::new();
        reader.read_line(&mut status).unwrap();
        let mut head = String::new();
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" || line == "\n" {
                break;
            }
            head.push_str(&line);
        }
        if !status.starts_with("HTTP/1.1 200 ") {
            // The board answers a refused stream with a normal response and
            // closes; read the remainder so the refusal is reported verbatim.
            let mut body = String::new();
            let _ = reader.read_to_string(&mut body);
            panic!("{who} live stream was refused ({status}): {body}\nheaders: {head}");
        }
        let mut stream = Self { reader, who };
        // One absolute deadline covers hello, the opening keepalive and every
        // frame of a checkpoint read; a keepalive must not extend it.
        let deadline = Instant::now() + STREAM_WAIT;
        let hello = stream
            .next_frame(deadline, "hello")
            .unwrap_or_else(|| panic!("{who} stream closed before hello"));
        assert!(
            hello.contains("event: hello"),
            "{who} stream did not open with a hello frame: {hello}"
        );
        if entities {
            assert!(
                hello.contains("\"entities\":true"),
                "{who} requested entity patches but the hello did not enable them: {hello}"
            );
        } else {
            assert!(
                !hello.contains("\"entities\":true"),
                "{who} legacy stream must not negotiate entity patches: {hello}"
            );
        }
        let ping = stream
            .next_frame(deadline, "the opening keepalive")
            .unwrap_or_else(|| panic!("{who} stream closed before its opening keepalive"));
        assert!(
            ping.starts_with(": ping"),
            "{who} stream did not send its opening keepalive: {ping}"
        );
        stream
    }

    /// Read one complete frame. A closed connection is `None`; a read deadline
    /// that expires before a frame ends is a failure, so absence can never be
    /// manufactured by a timeout.
    fn next_frame(&mut self, deadline: Instant, what: &str) -> Option<String> {
        let mut frame = String::new();
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .filter(|left| !left.is_zero())
                .unwrap_or_else(|| {
                    panic!(
                        "{} stream did not reach {what} before the deadline",
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
                Ok(_) if line == "\n" || line == "\r\n" => {
                    if !frame.is_empty() {
                        return Some(frame);
                    }
                }
                Ok(_) => frame.push_str(&line),
                Err(e) => panic!("{} stream failed before {what}: {e}", self.who),
            }
        }
    }

    /// The next data frame: keepalives are liveness, never observations.
    fn next_event(&mut self, deadline: Instant, what: &str) -> Option<String> {
        loop {
            let frame = self.next_frame(deadline, what)?;
            if frame.starts_with(':') || frame.starts_with("event: heartbeat") {
                continue;
            }
            return Some(frame);
        }
    }

    /// The next data frame, or a panic naming the checkpoint that did not
    /// arrive. Keepalives are skipped.
    fn next_data(&mut self, what: &str) -> String {
        let deadline = Instant::now() + STREAM_WAIT;
        self.next_event(deadline, what)
            .unwrap_or_else(|| panic!("{} stream closed before {what}", self.who))
    }

    /// Read event-driven until the next frame of `kind`. Only the permitted
    /// intervening entity kinds (`issue`, `agent`, `plan`) may be skipped, and
    /// each must be free of the caller's hidden/private sentinels; any other
    /// event kind or a leaked sentinel fails loudly. The absolute deadline
    /// panics too, so silence is never the expected frame.
    fn next_kind(&mut self, kind: &str, what: &str, forbidden: &[&str]) -> String {
        let deadline = Instant::now() + STREAM_WAIT;
        loop {
            let frame = self
                .next_event(deadline, what)
                .unwrap_or_else(|| panic!("{} stream closed before {what}", self.who));
            let name = event_name(&frame);
            if name == Some(kind) {
                return frame;
            }
            assert!(
                matches!(name, Some("issue" | "agent" | "plan")),
                "{} received an unexpected event kind while waiting for {what}: {frame}",
                self.who
            );
            for marker in forbidden {
                assert!(
                    !frame.contains(marker),
                    "{} leaked hidden/private marker {marker} while waiting for {what}: {frame}",
                    self.who
                );
            }
        }
    }

    /// True when the next raw frame read observes EOF. A keepalive or any
    /// frame before EOF is a failure, not a close: it proves the server chose
    /// to keep the stream alive. The deadline is absolute.
    fn closed(&mut self, what: &str) -> bool {
        let deadline = Instant::now() + STREAM_WAIT;
        self.next_frame(deadline, what).is_none()
    }

    /// Diagnostic-only: read under one absolute deadline and return every
    /// non-keepalive frame seen plus whether `marker` was reached. Expiry
    /// returns the frames seen so far; it is never evidence of silence, only
    /// material for a failed checkpoint's report.
    fn collect_until(&mut self, marker: &str) -> (String, bool) {
        let deadline = Instant::now() + STREAM_WAIT;
        let mut seen = String::new();
        let mut frame = String::new();
        loop {
            let Some(remaining) = deadline
                .checked_duration_since(Instant::now())
                .filter(|left| !left.is_zero())
            else {
                return (seen, false);
            };
            self.reader
                .get_ref()
                .set_read_timeout(Some(remaining))
                .unwrap();
            let mut line = String::new();
            match self.reader.read_line(&mut line) {
                Ok(0) => return (seen, false),
                Ok(_) if line == "\n" || line == "\r\n" => {
                    if !frame.is_empty()
                        && !frame.starts_with(':')
                        && !frame.starts_with("event: heartbeat")
                    {
                        let found = frame.contains(marker);
                        seen.push_str(&frame);
                        if found {
                            return (seen, true);
                        }
                    }
                    frame.clear();
                }
                Ok(_) => frame.push_str(&line),
                Err(_) => return (seen, false),
            }
        }
    }

    /// Read through the frame containing `marker`, returning every data frame
    /// seen on the way (the operator positive-control form).
    fn through(&mut self, marker: &str) -> String {
        // One absolute deadline for the whole checkpoint: skipped keepalives
        // must not buy the read another STREAM_WAIT.
        let deadline = Instant::now() + STREAM_WAIT;
        let mut seen = String::new();
        loop {
            let frame = self
                .next_event(deadline, marker)
                .unwrap_or_else(|| panic!("{} stream closed before {marker}", self.who));
            let found = frame.contains(marker);
            seen.push_str(&frame);
            if found {
                return seen;
            }
        }
    }

    /// Read until the frame containing `marker`, or until the server closes.
    fn until_checkpoint_or_close(&mut self, marker: &str) -> Interval {
        // One absolute deadline across skipped keepalives; EOF is explicit and
        // is the only non-time observation that ends the interval early.
        let deadline = Instant::now() + STREAM_WAIT;
        let mut frames = Vec::new();
        loop {
            let Some(frame) = self.next_event(deadline, marker) else {
                return Interval {
                    frames,
                    closed: true,
                };
            };
            let done = frame.contains(marker);
            frames.push(frame);
            if done {
                return Interval {
                    frames,
                    closed: false,
                };
            }
        }
    }

    /// Read through the first data frame satisfying `want`, returning every
    /// frame seen on the way (inclusive). EOF ends the interval; a read
    /// deadline is a failure, never silence.
    fn until_where_or_close(&mut self, want: impl Fn(&str) -> bool) -> Interval {
        let deadline = Instant::now() + STREAM_WAIT;
        let mut frames = Vec::new();
        loop {
            let Some(frame) = self.next_event(deadline, "the expected completion row") else {
                return Interval {
                    frames,
                    closed: true,
                };
            };
            let done = want(&frame);
            frames.push(frame);
            if done {
                return Interval {
                    frames,
                    closed: false,
                };
            }
        }
    }
}

/// Issue the same signed stream request but do NOT expect SSE: report the
/// status line and the rest of the response. Used where the board is required
/// to refuse before the event-stream headers go out.
fn probe_stream(rig: &Rig, cookie: &str, entities: bool) -> (String, String) {
    let path = if entities {
        "/api/stream?entities=1"
    } else {
        "/api/stream"
    };
    let mut socket = TcpStream::connect(("127.0.0.1", rig.board_port)).unwrap();
    socket.set_read_timeout(Some(STREAM_WAIT)).unwrap();
    write!(
        socket,
        "GET {path} HTTP/1.1\r\nHost: {}\r\nCookie: {cookie}\r\n\
         Accept: text/event-stream\r\nConnection: close\r\n\r\n",
        rig.public_host
    )
    .unwrap();
    let mut reader = BufReader::new(socket);
    let mut status = String::new();
    reader.read_line(&mut status).unwrap();
    let mut rest = String::new();
    let _ = reader.read_to_string(&mut rest);
    (status.trim_end().to_string(), rest)
}

fn scoped_issue(id: &str, title: &str) -> String {
    format!(
        "---\nid: {id}\ntitle: {title}\nstatus: backlog\npriority: P2\n\
         created: '2026-01-01T00:00:00Z'\n---\n"
    )
}

fn write_issue(rig: &Rig, project: &str, id: &str, title: &str) {
    std::fs::write(
        rig.pm.join(project).join(id).join("issue.md"),
        scoped_issue(id, title),
    )
    .unwrap();
}

/// PROJECTION-ONLY delivery fixture: a legacy/unknown record for `project`,
/// written through the real delivery file writer. It is deliberately NOT
/// `Record::new_ordinary`: it names no dispatch receipt and is not ordinary
/// delivery. It exists only to exercise the stream's delivery-file parse and
/// close plumbing and the provenance-suppression rule; this fixture never
/// fabricates a dispatch id to claim otherwise.
fn save_projection_only_record(state_dir: &std::path::Path, issue: &str, project: &str) {
    let mut records = std::collections::BTreeMap::new();
    records.insert(
        issue.to_string(),
        crate::delivery::Record::new(issue, project, "cad2215-worker", 1),
    );
    crate::delivery::save(state_dir, &records).unwrap();
}

/// How many frames of `kind` an interval contains. Frame CLASSES, not batch
/// shapes, are the stable contract here; unrelated coalesced frames are
/// ignored.
fn count_kind(interval: &Interval, kind: &str) -> usize {
    interval
        .frames
        .iter()
        .filter(|frame| event_name(frame) == Some(kind))
        .count()
}

/// Park every monitor's next check far in the future so the daemon's monitor
/// loop cannot inject member-visible `monitoring` frames while the stream
/// assertions run. `register_monitor` leaves the seeded monitors due
/// immediately, and a due check rewrites heartbeat/last-check/alert state,
/// which moves the member monitoring projection. Any check that already ran
/// is settled before the stream's baseline; no new one can start.
fn freeze_monitors(rig: &Rig) {
    let db = rusqlite::Connection::open(rig._dir.path().join("cadence.sqlite3")).unwrap();
    db.execute(
        "UPDATE monitors SET next_check_at=4000000000 WHERE state <> 'off'",
        [],
    )
    .unwrap();
}

/// One independently caused, member-visible monitoring change: a new alert on
/// the readable VIS-1 task's monitor. It moves both the shared `monitor_list`
/// counts and the reader's scoped monitoring projection, so the member must
/// receive a `monitoring` frame — proving that forbidding it in the hidden
/// interval is not vacuous.
fn visible_monitor_alert(rig: &Rig) {
    let db = rusqlite::Connection::open(rig._dir.path().join("cadence.sqlite3")).unwrap();
    db.execute(
        "INSERT INTO monitor_alerts\
         (seq,monitor_id,task_id,event_seq,fingerprint,kind,payload,state,created,updated) \
         VALUES(221599,'cad2215-visible-monitor','cad2215-visible-task',9,\
         'cad2215-visible-monitor-positive','test',\
         '{\"marker\":\"cad2215-visible-monitor-positive\"}','open',2,2)",
        [],
    )
    .unwrap();
}

/// A member-visible daemon change: a running message on the readable VIS-1
/// task. The operator sees its body; a member row exposes only its ID
/// (`member_running_item` keeps id/state/task/created and drops the body), so
/// the positive control proves the body on the operator, the ID plus a generic
/// `agents` invalidation on both member clients, and the same ID on a scoped
/// HTTP read.
fn visible_message(rig: &Rig, shared: &crate::daemon::Shared, marker: &str, id: &str) {
    shared
        .store
        .enqueue_task(
            "cad2215-worker",
            marker,
            None,
            id,
            "user",
            Some("cad2215-visible-task"),
        )
        .unwrap();
    let db = rusqlite::Connection::open(rig._dir.path().join("cadence.sqlite3")).unwrap();
    assert_eq!(
        db.execute("UPDATE messages SET state='running' WHERE id=?1", [id])
            .unwrap(),
        1
    );
}

/// No hidden identifier and no entity tombstone may reach this reader.
fn assert_no_hidden(interval: &Interval, forbidden: &[&str]) {
    for frame in &interval.frames {
        for needle in forbidden {
            assert!(
                !frame.contains(needle),
                "member stream leaked {needle}: {frame}"
            );
        }
        assert!(
            !frame.contains("\"op\":\"delete\""),
            "member stream received an entity tombstone: {frame}"
        );
    }
}

/// A hidden-only change is judged by its frame CLASSES, not by an exact batch
/// shape: this fixture's daemon can legitimately coalesce its own
/// member-visible frames (`monitoring`, a card rebuilt from agent state) into
/// any tick, so comparing whole sequences fails on unrelated traffic. A
/// tracker-hidden change can only surface as the shared `issues` invalidation
/// (or the hidden entity/tombstone); a daemon-hidden change as
/// `jobs`/`agents`/`agent`/`threads`. The sentinel is deliberately a change of
/// the OTHER class, so it cannot itself supply one of those frames.
fn assert_no_frame_kind(interval: &Interval, forbidden: &[&str], what: &str) {
    for frame in &interval.frames {
        let Some(name) = event_name(frame) else {
            continue;
        };
        assert!(
            !forbidden.contains(&name),
            "hidden-only change ({what}) produced a {name} frame before the sentinel: {frame}"
        );
    }
}

/// A hidden-only change must not close the reader's stream or swallow its
/// sentinel, which is published after the hidden tick.
fn assert_sentinel_reached(interval: &Interval, sentinel: &str, what: &str) {
    assert!(
        !interval.closed,
        "member stream closed before {sentinel} while a hidden change ({what}) was published"
    );
    assert!(
        interval
            .frames
            .last()
            .is_some_and(|frame| frame.contains(sentinel)),
        "member stream did not reach the visible sentinel {sentinel} for {what}: {:?}",
        interval.frames
    );
}

/// A member who cannot read one project still receives live updates for the
/// projects it can read, and nothing at all for the project it cannot.
#[test]
fn project_restricted_member_receives_only_readable_stream_changes() {
    let rig = start(&[]);
    seed(&rig);
    freeze_monitors(&rig);
    let member = login(&rig, "cad2215-scoped-project-member", "member");
    let operator = login(&rig, "cad2215-scoped-project-operator", "owner");
    // Both client shapes against the same reader: entity patches and the
    // pre-CAD-325 legacy frames.
    let mut op = MemberStream::open(&rig, &operator, true, "project operator");
    let mut entity = MemberStream::open(&rig, &member, true, "project member entity");
    let mut legacy = MemberStream::open(&rig, &member, false, "project member legacy");

    // Positive control: an authorized visible change reaches both clients.
    let visible = "CAD2215_SCOPED_PROJECT_VISIBLE";
    write_issue(&rig, "visible", "VIS-1", visible);
    assert!(
        op.through(visible).contains(visible),
        "operator lost the readable change"
    );
    assert!(
        entity.through(visible).contains(visible),
        "entity member stream did not receive an authorized visible change"
    );
    assert!(
        legacy.through(visible).contains(visible),
        "legacy member stream did not receive an authorized visible change"
    );

    // Drain the first tick, then measure the control tick: a member-visible
    // tracker change. Counting only the generic `issues` class (rather than
    // the whole batch shape) keeps the check truthful when the daemon
    // legitimately coalesces a card frame rebuilt from its own state into the
    // window; the hidden tick must not add a second one.
    let drain = "CAD2215_SCOPED_PROJECT_DRAIN";
    write_issue(&rig, "visible", "VIS-1", drain);
    assert!(op.through(drain).contains(drain));
    assert!(entity.through(drain).contains(drain));
    assert!(legacy.through(drain).contains(drain));
    let control = "CAD2215_SCOPED_PROJECT_CONTROL";
    write_issue(&rig, "visible", "VIS-1", control);
    assert!(op.through(control).contains(control));
    let entity_control = entity.until_checkpoint_or_close(control);
    let legacy_control = legacy.until_checkpoint_or_close(control);

    // The operator sees the private project's change, which proves the shared
    // watcher published this tracker tick.
    let hidden = "CAD2215_SCOPED_PROJECT_HIDDEN";
    write_issue(&rig, "secret", "SEC-1", hidden);
    assert!(
        op.through(hidden).contains(hidden),
        "operator positive control lost the private project's change"
    );

    let checkpoint = "CAD2215_SCOPED_PROJECT_CHECK";
    write_issue(&rig, "visible", "VIS-1", checkpoint);
    assert!(op.through(checkpoint).contains(checkpoint));
    let entity_hidden = entity.until_checkpoint_or_close(checkpoint);
    let legacy_hidden = legacy.until_checkpoint_or_close(checkpoint);
    for (hidden_interval, control_interval) in [
        (&entity_hidden, &entity_control),
        (&legacy_hidden, &legacy_control),
    ] {
        assert_no_hidden(hidden_interval, &[hidden, "SEC-1"]);
        assert_eq!(
            count_kind(hidden_interval, "issues"),
            count_kind(control_interval, "issues"),
            "the hidden tracker change added a generic `issues` invalidation: {:?}",
            hidden_interval.frames
        );
        assert_sentinel_reached(hidden_interval, checkpoint, hidden);
    }

    // The ordinary authorized HTTP read is the positive control that the
    // latest visible change exists and the hidden one is not readable. The
    // last title this test wrote to VIS-1 is `checkpoint`.
    let read = get(&rig, &member, "/api/issues").to_string();
    assert!(
        read.contains(checkpoint),
        "member lost the latest readable issue change: {read}"
    );
    assert!(
        !read.contains(hidden),
        "member HTTP read leaked the hidden issue: {read}"
    );
    assert!(
        !read.contains("SEC-1"),
        "member HTTP read leaked the hidden issue id"
    );
}

/// Start the real rig with the opt-in member-request queue fixture: the
/// observer receives the daemon sweep's `(request_id, outcome)` witness, and
/// the synthetic `hosted-board` `assist.revalidate` service is enrolled so a
/// promotion re-proves the request's ORIGINAL credential on the real path.
fn start_queued(subs: &[&str]) -> (Rig, mpsc::Receiver<(String, String)>) {
    let (tx, rx) = mpsc::channel::<(String, String)>();
    let tx = Mutex::new(tx);
    #[allow(clippy::type_complexity)] // one fixture callback, one shape
    let observer: Arc<dyn Fn(&str, &str) + Send + Sync> =
        Arc::new(move |id: &str, outcome: &str| {
            let _ = tx
                .lock()
                .unwrap()
                .send((id.to_string(), outcome.to_string()));
        });
    (start_with_queue_service(subs, Some(observer), true), rx)
}

/// Accept one signed member job request and prove GENUINE autonomous
/// promotion, then read the authorized job id from owner-scoped request
/// detail. The accepted receipt names no job; a bounded absolute deadline
/// waits for this request's own sweep outcome, never a sleep.
fn promoted_member_job(
    rig: &Rig,
    events: &mpsc::Receiver<(String, String)>,
    sub: &str,
    title: &str,
    key: &str,
    issue: &str,
) -> String {
    let envelope = rig.envelope(sub, serde_json::json!({}), serde_json::json!({}));
    let (status, receipt) = rig.call(
        "job_request",
        &envelope,
        &serde_json::json!({"title": title, "request_key": key, "issue": issue}),
    );
    assert_eq!(
        status, 200,
        "signed member job request {key} refused: {receipt}"
    );
    let expected = serde_json::json!({
        "request_id": crate::store::assist_request_id(ORG, sub, key),
        "status": "accepted",
    });
    assert_eq!(
        receipt, expected,
        "request {key} receipt is not the occupancy-independent accepted shape: {receipt}"
    );
    let request_id = receipt["request_id"].as_str().unwrap().to_string();

    let deadline = Instant::now() + STREAM_WAIT;
    let mut last = None::<String>;
    loop {
        let left = deadline
            .checked_duration_since(Instant::now())
            .filter(|left| !left.is_zero())
            .unwrap_or_else(|| {
                panic!("request {request_id} was never promoted; last sweep outcome: {last:?}")
            });
        let (seen, outcome) = events.recv_timeout(left).unwrap_or_else(|_| {
            panic!("request {request_id} was never promoted; last sweep outcome: {last:?}")
        });
        if seen != request_id {
            continue;
        }
        if outcome == "created" {
            break;
        }
        last = Some(outcome);
    }

    // A fresh single-use signature for the owner-scoped detail read; the
    // request envelope was spent.
    let list_envelope = rig.envelope(sub, serde_json::json!({}), serde_json::json!({}));
    let (status, list) = rig.call("job_list", &list_envelope, &serde_json::json!({}));
    assert_eq!(
        status, 200,
        "signed owner request list {key} failed: {list}"
    );
    let row = list["requests"]
        .as_array()
        .and_then(|rows| {
            rows.iter()
                .find(|row| row["request_id"] == request_id.as_str())
        })
        .unwrap_or_else(|| panic!("owner request list lost promoted request {request_id}: {list}"));
    assert_eq!(
        row["issue"], issue,
        "promoted request {request_id} names the wrong issue: {row}"
    );
    assert_eq!(
        row["status"], "created",
        "request {request_id} is not in the promoted state: {row}"
    );
    row["job"]
        .as_str()
        .expect("promoted request detail has no authorized job id")
        .to_string()
}

/// A member who reads the project but not another member's private assistant
/// job receives visible changes and no frame at all for the private job's
/// activity. Both entity and legacy clients are exercised.
#[test]
fn project_reader_gets_no_frame_for_another_members_private_job() {
    let _flag = AssistantFlagOn::arm();
    let (rig, queue_events) = start_queued(&["ana", "ben"]);
    seed(&rig);
    freeze_monitors(&rig);
    // The reader may see every project; the hidden unit is the private job
    // bound to a readable issue. `seed` leaves VIS-9 unoccupied for a genuine
    // member request (VIS-1 already carries the ordinary fixture job).
    std::fs::write(
        rig.pm.join("secret/project.yaml"),
        "key: secret\nprefix: SEC\n",
    )
    .unwrap();

    // A REAL assistant job request owns the private job (the daemon's
    // ownership/subscription ledger, not a test-only label) and binds the
    // readable issue the request selected. The request is accepted-only; the
    // autonomous sweep, re-proving Ana's ORIGINAL recorded credential through
    // the enrolled synthetic `assist.revalidate` service fixture, publishes
    // the canonical job, and the authorized id comes from owner detail.
    let job = promoted_member_job(
        &rig,
        &queue_events,
        "ana",
        "CAD2215_SCOPED_PRIVATE_JOB",
        "cad2215-scoped-private-job",
        "VIS-9",
    );

    let opts = ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", rig.pm.to_str().unwrap());
    let shared = crate::daemon::Shared::new(rig._dir.path(), &opts).unwrap();
    let db = rusqlite::Connection::open(rig._dir.path().join("cadence.sqlite3")).unwrap();
    let bound: Option<String> = db
        .query_row("SELECT issue_id FROM jobs WHERE id=?1", [&job], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(
        bound.as_deref(),
        Some("VIS-9"),
        "the request did not bind the readable issue it selected"
    );
    // The private job's task is carried by a second real worker, so the
    // visible control and the hidden change never rely on one agent having two
    // running turns at once.
    let task: String = db
        .query_row(
            "SELECT id FROM tasks WHERE job_id=?1 ORDER BY id LIMIT 1",
            [&job],
            |r| r.get(0),
        )
        .unwrap();
    shared
        .store
        .register_agent(&crate::store::NewAgent {
            alias: "cad2215-private-worker",
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
    assert_eq!(
        db.execute(
            "UPDATE tasks SET assignee='cad2215-private-worker', state='running' WHERE id=?1",
            [&task]
        )
        .unwrap(),
        1
    );

    let ben = login(&rig, "ben", "member");
    let operator = login(&rig, "cad2215-scoped-job-operator", "owner");
    let mut op = MemberStream::open(&rig, &operator, true, "private-job operator");
    let mut entity = MemberStream::open(&rig, &ben, true, "private-job member entity");
    let mut legacy = MemberStream::open(&rig, &ben, false, "private-job member legacy");

    let visible = "CAD2215_SCOPED_JOB_VISIBLE";
    write_issue(&rig, "visible", "VIS-1", visible);
    assert!(op.through(visible).contains(visible));
    assert!(
        entity.through(visible).contains(visible),
        "entity member stream did not receive an authorized visible change"
    );
    assert!(
        legacy.through(visible).contains(visible),
        "legacy member stream did not receive an authorized visible change"
    );

    // Positive control for the daemon class. The member row exposes only the
    // authorized message ID (`member_running_item` keeps id/state/task/created
    // and drops the body), so the operator must see the body, both member
    // clients must see the ID and never the body, and they must also receive
    // their generic `agents` invalidation. A scoped HTTP read then proves the
    // ID is the member's to read.
    let before_body = "CAD2215_SCOPED_JOB_BEFORE";
    let before_id = "cad2215-scoped-job-before";
    visible_message(&rig, &shared, before_body, before_id);
    assert!(
        op.through(before_body).contains(before_body),
        "operator lost the member-visible daemon control body"
    );
    for stream in [&mut entity, &mut legacy] {
        let interval = stream.until_checkpoint_or_close(before_id);
        assert!(
            !interval.closed,
            "member stream closed on the visible daemon control"
        );
        assert!(
            interval
                .frames
                .iter()
                .any(|frame| frame.contains(before_id)),
            "the authorized message ID was not delivered to the member: {:?}",
            interval.frames
        );
        assert!(
            interval
                .frames
                .iter()
                .any(|frame| event_name(frame) == Some("agents")),
            "the member did not receive its generic `agents` invalidation: {:?}",
            interval.frames
        );
        assert!(
            !interval
                .frames
                .iter()
                .any(|frame| frame.contains(before_body)),
            "member stream leaked the running message body: {:?}",
            interval.frames
        );
    }
    let member_view = get(&rig, &ben, "/api/agents/cad2215-worker").to_string();
    assert!(
        member_view.contains(before_id),
        "the scoped member read lost the authorized message ID: {member_view}"
    );
    assert!(
        !member_view.contains(before_body),
        "the scoped member read leaked the running message body"
    );

    // Drain with a visible TRACKER change: its issue upsert is the batch's
    // last frame, so the daemon-hidden interval starts on an empty queue.
    let drain = "CAD2215_SCOPED_JOB_DRAIN";
    write_issue(&rig, "visible", "VIS-1", drain);
    assert!(op.through(drain).contains(drain));
    assert!(entity.through(drain).contains(drain));
    assert!(legacy.through(drain).contains(drain));

    // Positive control for the monitoring class: an independently caused
    // member-visible alert on the readable task produces a `monitoring` frame,
    // so forbidding it below is not vacuous. Monitor checks are parked, so
    // nothing else can move this projection.
    visible_monitor_alert(&rig);
    assert!(
        op.through("event: monitoring")
            .contains("event: monitoring"),
        "operator did not observe the member-visible monitoring change"
    );
    for stream in [&mut entity, &mut legacy] {
        let frame = stream.next_data("the member-visible monitoring change");
        assert_eq!(
            event_name(&frame),
            Some("monitoring"),
            "a member-visible monitoring change was not delivered: {frame}"
        );
    }

    // Hidden-only change: the private job's worker reports. It moves the
    // shared job/agent/threads fingerprints and nothing the reader can
    // observe; monitor checks are parked, so no `monitoring` churn can mask a
    // leak here.
    let hidden = "CAD2215_SCOPED_PRIVATE_HIDDEN";
    let hidden_message = "cad2215-scoped-private-hidden";
    shared
        .store
        .enqueue_task(
            "cad2215-private-worker",
            hidden,
            None,
            hidden_message,
            "user",
            Some(task.as_str()),
        )
        .unwrap();
    assert_eq!(
        db.execute(
            "UPDATE messages SET state='running' WHERE id=?1",
            [hidden_message]
        )
        .unwrap(),
        1
    );
    assert!(
        op.through(hidden).contains(hidden),
        "operator positive control lost the private job's running message"
    );

    let checkpoint = "CAD2215_SCOPED_JOB_CHECK";
    write_issue(&rig, "visible", "VIS-1", checkpoint);
    assert!(op.through(checkpoint).contains(checkpoint));
    let entity_hidden = entity.until_checkpoint_or_close(checkpoint);
    let legacy_hidden = legacy.until_checkpoint_or_close(checkpoint);
    for interval in [&entity_hidden, &legacy_hidden] {
        assert_no_hidden(
            interval,
            &[
                hidden,
                "CAD2215_SCOPED_PRIVATE_JOB",
                job.as_str(),
                task.as_str(),
                hidden_message,
            ],
        );
        assert_no_frame_kind(
            interval,
            &["jobs", "agents", "agent", "threads", "monitoring"],
            hidden,
        );
        assert_sentinel_reached(interval, checkpoint, hidden);
    }
}

/// Revoking a project mid-stream keeps the reader's readable live updates and
/// removes the revoked project's from them, without a fresh frame for a
/// hidden-only change made after the revocation.
#[test]
fn live_project_revocation_scopes_the_open_member_stream() {
    let rig = start(&[]);
    seed(&rig);
    freeze_monitors(&rig);
    // The member reads every project at admission, so both streams are open
    // before the revocation.
    std::fs::write(
        rig.pm.join("secret/project.yaml"),
        "key: secret\nprefix: SEC\n",
    )
    .unwrap();
    let member = login(&rig, "cad2215-scoped-revoked-member", "member");
    let operator = login(&rig, "cad2215-scoped-revoked-operator", "owner");
    let mut op = MemberStream::open(&rig, &operator, true, "revocation operator");
    let mut entity = MemberStream::open(&rig, &member, true, "revocation member entity");
    let mut legacy = MemberStream::open(&rig, &member, false, "revocation member legacy");

    let before = "CAD2215_SCOPED_REVOKE_BEFORE";
    write_issue(&rig, "visible", "VIS-1", before);
    assert!(op.through(before).contains(before));
    assert!(entity.through(before).contains(before));
    assert!(legacy.through(before).contains(before));
    // Drain the watcher's first (whole-baseline) tick.
    let drain = "CAD2215_SCOPED_REVOKE_DRAIN";
    write_issue(&rig, "visible", "VIS-1", drain);
    assert!(op.through(drain).contains(drain));
    assert!(entity.through(drain).contains(drain));
    assert!(legacy.through(drain).contains(drain));

    // Revoke while the admitted streams stay open, then change the issue that
    // became private. The operator barrier proves the shared tick published.
    std::fs::write(
        rig.pm.join("secret/project.yaml"),
        "key: secret\nprefix: SEC\npolicy:\n  version: 1\n  visibility: private\n",
    )
    .unwrap();
    let hidden = "CAD2215_SCOPED_REVOKE_HIDDEN";
    write_issue(&rig, "secret", "SEC-1", hidden);
    assert!(
        op.through(hidden).contains(hidden),
        "operator positive control lost the revoked project's change"
    );

    // The revocation itself changes what this reader can see (its own project
    // list shrank), so generic invalidation or a tombstone for the entity
    // leaving the reader's view is a consequence of the reader's own scope
    // change here, not a hidden-only leak. The title changed after the
    // revocation is hidden. Authorized readable updates must keep flowing on
    // the same connection.
    let after = "CAD2215_SCOPED_REVOKE_AFTER";
    write_issue(&rig, "visible", "VIS-1", after);
    assert!(op.through(after).contains(after));
    let entity_after = entity.until_checkpoint_or_close(after);
    let legacy_after = legacy.until_checkpoint_or_close(after);
    for interval in [&entity_after, &legacy_after] {
        assert!(
            !interval.closed,
            "member stream closed on revocation instead of keeping authorized live updates"
        );
        for frame in &interval.frames {
            assert!(
                !frame.contains(hidden),
                "revoked member stream leaked the post-revocation private title: {frame}"
            );
        }
        assert!(
            interval
                .frames
                .last()
                .is_some_and(|frame| frame.contains(after)),
            "revoked member did not receive the readable checkpoint: {:?}",
            interval.frames
        );
    }

    // With the revocation settled, measure a visible tracker control tick.
    // The hidden post-revocation change must not add a generic `issues`
    // invalidation beyond the one the sentinel tick itself carries.
    let control = "CAD2215_SCOPED_REVOKE_CONTROL";
    write_issue(&rig, "visible", "VIS-1", control);
    assert!(op.through(control).contains(control));
    let entity_control = entity.until_checkpoint_or_close(control);
    let legacy_control = legacy.until_checkpoint_or_close(control);

    // A hidden-only change must now be fully silent: no invalidation or
    // entity frame of the tracker class, and no tombstone.
    let hidden_again = "CAD2215_SCOPED_REVOKE_HIDDEN_AGAIN";
    write_issue(&rig, "secret", "SEC-1", hidden_again);
    assert!(
        op.through(hidden_again).contains(hidden_again),
        "operator positive control lost the post-revocation private change"
    );
    let checkpoint = "CAD2215_SCOPED_REVOKE_CHECK";
    write_issue(&rig, "visible", "VIS-1", checkpoint);
    assert!(op.through(checkpoint).contains(checkpoint));
    let entity_hidden = entity.until_checkpoint_or_close(checkpoint);
    let legacy_hidden = legacy.until_checkpoint_or_close(checkpoint);
    for (hidden_interval, control_interval) in [
        (&entity_hidden, &entity_control),
        (&legacy_hidden, &legacy_control),
    ] {
        assert_no_hidden(hidden_interval, &[hidden_again, "SEC-1"]);
        assert_eq!(
            count_kind(hidden_interval, "issues"),
            count_kind(control_interval, "issues"),
            "the hidden post-revocation change added a generic `issues` invalidation: {:?}",
            hidden_interval.frames
        );
        assert_sentinel_reached(hidden_interval, checkpoint, hidden_again);
    }

    // A fresh connection from the same revoked session is scoped, not refused
    // wholesale: the readable project still streams.
    let mut reopened = MemberStream::open(&rig, &member, true, "revoked member reopen");
    let reopen = "CAD2215_SCOPED_REVOKE_REOPEN";
    write_issue(&rig, "visible", "VIS-1", reopen);
    assert!(op.through(reopen).contains(reopen));
    assert!(
        reopened.through(reopen).contains(reopen),
        "a project-restricted member's fresh stream did not carry its readable change"
    );
}

/// A member baseline that cannot be resolved before the SSE headers must be
/// refused with a truthful HTTP error, not an apparently empty live stream.
/// The same real fixture then repairs the projection, and a fresh connection
/// carries a healthy member-visible update — so the refusal is the projection,
/// not the board.
#[test]
fn unreadable_delivery_projection_refuses_before_headers_then_opens_after_repair() {
    let rig = start(&[]);
    seed(&rig);
    freeze_monitors(&rig);
    let member = login(&rig, "cad2215-scoped-preheader-member", "member");
    let operator = login(&rig, "cad2215-scoped-preheader-operator", "owner");
    let state = rig._dir.path();

    // Unreadable delivery projection BEFORE any member stream exists. The
    // tracker is healthy, so the route's 403 for an unresolvable caller does
    // not apply; the member baseline itself cannot be resolved, which must be
    // a 503 before any SSE frame goes out.
    std::fs::write(crate::delivery::path(state), b"{ not a delivery record").unwrap();
    for entities in [true, false] {
        let (status, rest) = probe_stream(&rig, &member, entities);
        assert!(
            status.starts_with("HTTP/1.1 503"),
            "a member whose projection cannot be resolved before headers must be \
             refused with 503, got {status}: {rest}"
        );
        assert!(
            !rest.contains("event: hello") && !rest.contains(": ping"),
            "a refused member stream must not emit SSE frames: {rest}"
        );
    }

    // Positive control: the operator (no member projection) still opens and
    // observes a visible change, so the board/daemon are healthy while the
    // member projection is unreadable.
    let mut op = MemberStream::open(&rig, &operator, true, "preheader operator");
    let visible = "CAD2215_SCOPED_PREHEADER_VISIBLE";
    write_issue(&rig, "visible", "VIS-1", visible);
    assert!(
        op.through(visible).contains(visible),
        "operator lost the readable change while the member projection was unreadable"
    );

    // Repair the file with a parseable, deliberately NON-ordinary record (no
    // dispatch receipt is fabricated); the projection only needs the file to
    // parse, and a fresh signed member stream then carries a real visible
    // change on both shapes.
    save_projection_only_record(state, "VIS-1", "visible");
    let mut entity = MemberStream::open(&rig, &member, true, "preheader member entity");
    let mut legacy = MemberStream::open(&rig, &member, false, "preheader member legacy");
    let repaired = "CAD2215_SCOPED_PREHEADER_REPAIRED";
    write_issue(&rig, "visible", "VIS-1", repaired);
    assert!(
        op.through(repaired).contains(repaired),
        "operator lost the repaired update"
    );
    assert!(
        entity.through(repaired).contains(repaired),
        "repaired member entity stream did not receive the visible update"
    );
    assert!(
        legacy.through(repaired).contains(repaired),
        "repaired member legacy stream did not receive the visible update"
    );
}

/// A member SSE connection whose scoped read projection becomes unavailable
/// AFTER it was admitted must close. It must not stay open, keep heartbeating
/// and silently stop delivering that reader's updates.
///
/// The concrete seam-free trigger is the delivery file: `visible_delivery_fp`
/// scopes `delivery.json` to verified-ordinary records of the reader's
/// readable projects and answers `None` when the file cannot be parsed ("the
/// stream must close"). A legacy/unknown record is operator recovery: it is
/// withheld from the member projection, so this test drives that suppression
/// with a projection-only record and keeps the reader's REAL ordinary-change
/// assertion on a visible tracker change.
/// The projection is recomputed on a tracker-signal batch, so a visible issue
/// update after the corruption is the deterministic barrier: the operator
/// (whose stream is not projected) sees it, and the member stream must already
/// be closed with no frame and no heartbeat.
#[test]
fn unreadable_delivery_projection_closes_an_open_member_stream() {
    let rig = start(&[]);
    seed(&rig);
    freeze_monitors(&rig);
    let member = login(&rig, "cad2215-scoped-delivery-member", "member");
    let operator = login(&rig, "cad2215-scoped-delivery-operator", "owner");
    let mut op = MemberStream::open(&rig, &operator, true, "delivery operator");
    let mut entity = MemberStream::open(&rig, &member, true, "delivery member entity");
    let mut legacy = MemberStream::open(&rig, &member, false, "delivery member legacy");
    let state = rig._dir.path();

    // Healthy control 1: an ordinary visible tracker change is delivered.
    let visible = "CAD2215_SCOPED_DELIVERY_VISIBLE";
    write_issue(&rig, "visible", "VIS-1", visible);
    assert!(
        op.through(visible).contains(visible),
        "operator lost the readable change"
    );
    assert!(
        entity.through(visible).contains(visible),
        "entity member did not receive the healthy visible update"
    );
    assert!(
        legacy.through(visible).contains(visible),
        "legacy member did not receive the healthy visible update"
    );
    // The operator's first (whole-baseline) tick ends with its aggregate;
    // consuming it leaves an empty queue for the delivery ticks below. This
    // is an event, not a sleep.
    assert!(
        op.through("event: aggregates")
            .contains("event: aggregates"),
        "operator's baseline tick did not end with its aggregate"
    );

    // Healthy control 2 (provenance suppression, projection-only): a record
    // for a readable project that is NOT verified ordinary must not move the
    // member's ordinary delivery projection. The operator's aggregate proves
    // the delivery tick was published, and the later visible tracker barrier
    // is the reader's REAL change: exactly its own `issues`/`issue` frames
    // appear, so the non-ordinary record contributed no member frame.
    save_projection_only_record(state, "VIS-1", "visible");
    assert!(
        op.through("event: aggregates")
            .contains("event: aggregates"),
        "operator did not observe the delivery-file tick"
    );
    let delivery_visible = "CAD2215_SCOPED_DELIVERY_ORDINARY";
    write_issue(&rig, "visible", "VIS-1", delivery_visible);
    assert!(
        op.through(delivery_visible).contains(delivery_visible),
        "operator lost the visible change after the delivery tick"
    );
    for stream in [&mut entity, &mut legacy] {
        let interval = stream.until_checkpoint_or_close(delivery_visible);
        assert!(
            !interval.closed,
            "member stream closed on the visible change: {:?}",
            interval.frames
        );
        assert_eq!(
            count_kind(&interval, "issues"),
            1,
            "the non-ordinary delivery record produced a member frame: {:?}",
            interval.frames
        );
        assert_eq!(
            count_kind(&interval, "issue"),
            1,
            "the real visible change did not arrive exactly once: {:?}",
            interval.frames
        );
    }

    // The existing positive ordinary-delivery SSE assertion, restored on a
    // REAL validated receipt: a disposable git repo + worktree supply the
    // production lane, the real `task_dispatch` handler mints the taskless
    // no—kickoff row, and the zero-logic acceptance forwarder invokes the
    // real `delivery_start` guard. The dispatch tick is drained behind a
    // visible tracker barrier first, so the delivery tick is the member's
    // next change.
    let opts = ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", rig.pm.to_str().unwrap());
    let shared = crate::daemon::Shared::new(rig._dir.path(), &opts).unwrap();
    let repo = rig._dir.path().join("ordinary-delivery-repo");
    std::fs::create_dir_all(&repo).unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .current_dir(&repo)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "fixture git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "cadence@example.invalid"]);
    git(&["config", "user.name", "cadence"]);
    std::fs::write(repo.join("README"), "fixture\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "fixture"]);
    let lane = crate::worktree::layout::worktrees_dir(&repo).join("vis-9");
    std::fs::create_dir_all(lane.parent().unwrap()).unwrap();
    git(&[
        "worktree",
        "add",
        "-q",
        "-b",
        "vis-9",
        lane.to_str().unwrap(),
    ]);
    std::fs::write(
        rig.pm.join("visible/project.yaml"),
        format!(
            "key: visible\nprefix: VIS\nrepos:\n  - path: {}\n",
            repo.display()
        ),
    )
    .unwrap();
    std::fs::write(
        rig.pm.join("visible/VIS-9/issue.md"),
        format!(
            "---\nid: VIS-9\ntitle: Request issue\nstatus: backlog\npriority: P2\n\
             created: '2026-01-01T00:00:00Z'\nrefs:\n  - kind: worktree\n    path: {}\n---\n",
            lane.display()
        ),
    )
    .unwrap();
    let db = rusqlite::Connection::open(rig._dir.path().join("cadence.sqlite3")).unwrap();
    db.execute_batch(&format!(
        "INSERT INTO jobs(id,title,spec_path,pm_alias,issue_id,repo,state,created,updated)
         VALUES('ordinary-delivery-job','Ordinary delivery','spec.md','cad2215-worker','VIS-9','visible','open',1,1);
         INSERT INTO tasks(id,job_id,title,assignee,state,worktree,created,updated)
         VALUES('ordinary-delivery-task','ordinary-delivery-job','Ordinary delivery','cad2215-worker','draft','{}',1,1);",
        lane.display()
    )).unwrap();
    let dispatched = crate::test_seam::scoped(crate::test_seam::Asserted::Operator, || {
        shared.dispatch(
            "task_dispatch",
            &serde_json::json!({"task":"ordinary-delivery-task","to":"cad2215-worker","by":"operator"}),
            std::process::id(),
        )
    }).unwrap();
    let receipt = dispatched["message"]
        .as_str()
        .expect("real task dispatch minted no receipt")
        .to_string();
    let kickoff = shared
        .store
        .message(&receipt)
        .unwrap()
        .expect("dispatch receipt row missing");
    assert_eq!(
        kickoff.source, "job_dispatch",
        "task dispatch producer contract changed"
    );
    assert_eq!(kickoff.task_id.as_deref(), Some("ordinary-delivery-task"));
    assert_eq!(kickoff.issue.as_deref(), Some("VIS-9"));
    assert_eq!(kickoff.alias, "cad2215-worker");
    let lane = std::fs::canonicalize(&lane).unwrap();
    assert_eq!(
        kickoff.worktree.as_deref(),
        Some(lane.to_str().unwrap()),
        "the real ordinary dispatch bound the wrong lane: {kickoff:?}"
    );
    // Drain the dispatch/complete tick's frames behind a visible change.
    let dispatch_drained = "CAD2215_SCOPED_DISPATCH_DRAINED";
    write_issue(&rig, "visible", "VIS-1", dispatch_drained);
    assert!(op.through(dispatch_drained).contains(dispatch_drained));
    assert!(entity.through(dispatch_drained).contains(dispatch_drained));
    assert!(legacy.through(dispatch_drained).contains(dispatch_drained));
    assert!(
        op.through("event: aggregates")
            .contains("event: aggregates"),
        "operator did not observe the dispatch-drain tick"
    );
    // Real production guard: the receipt's source/issue/worker/lane are
    // revalidated; a fabricated record is never written here.
    shared
        .delivery_start_for_acceptance("VIS-9", "visible", "cad2215-worker", &receipt)
        .unwrap();
    assert!(
        op.through("event: aggregates")
            .contains("event: aggregates"),
        "operator did not observe the readable delivery tick"
    );
    const DELIVERY_HIDDEN: &[&str] = &[
        "SEC-1",
        "secret",
        "cad2215-secret-job",
        "cad2215-secret-task",
        "cad2215-secret-monitor",
        "cad2215-secret-payload",
        "cad2215-free-job",
        "cad2215-free-task",
        "cad2215-free-monitor",
        "cad2215-free-payload",
    ];
    for stream in [&mut entity, &mut legacy] {
        // The dispatch/barrier tick may coalesce the new VIS-9 card ahead of
        // this write; only `issue`/`agent`/`plan` frames free of hidden
        // sentinels may precede the aggregate invalidation, and the helper
        // returns only when the reader's OWN `issues` arrives.
        let _ = stream.next_kind(
            "issues",
            "the reader's own delivery invalidation",
            DELIVERY_HIDDEN,
        );
    }

    // The delivery file becomes unparseable after both connections were open.
    // The watcher's next tick proves it published; the member projections can
    // no longer be scoped, so both member streams must close.
    std::fs::write(crate::delivery::path(state), b"{ not a delivery record").unwrap();
    assert!(
        op.through("event: aggregates")
            .contains("event: aggregates"),
        "operator did not observe the tick that made the delivery projection unreadable"
    );
    assert!(
        entity.closed("the unreadable delivery projection"),
        "member entity stream stayed open (or kept sending) after its delivery projection became unreadable"
    );
    assert!(
        legacy.closed("the unreadable delivery projection"),
        "member legacy stream stayed open (or kept sending) after its delivery projection became unreadable"
    );

    // A later member-visible update still exists and the board/watcher are
    // alive (the operator sees it). The closed member streams must not deliver
    // it, nor resume with a heartbeat.
    let later = "CAD2215_SCOPED_DELIVERY_LATER";
    write_issue(&rig, "visible", "VIS-1", later);
    assert!(
        op.through(later).contains(later),
        "operator lost the later visible update after the projection failure"
    );
    assert!(
        entity.closed("the later visible update"),
        "member entity stream resumed after closing on the unreadable projection"
    );
    assert!(
        legacy.closed("the later visible update"),
        "member legacy stream resumed after closing on the unreadable projection"
    );
}

/// CAD-2215 also covers another member's private task carried by the SAME
/// ordinary worker the member can see. The real runner is sequential: one
/// running turn at a time. This scenario starts a visible ordinary turn,
/// proves the member sees it (authorized ID only; the body is redacted),
/// completes it and requires the member-visible REMOVAL of that turn (the
/// worker row's `running`/`message` clearing plus the legacy `agents`/`jobs`
/// invalidations the daemon emits for the same committed snapshot), then
/// starts the hidden private turn on that same worker and asserts no
/// hidden-only frame before a visible tracker sentinel.
///
/// The completion is the real finished-turn transition (`messages.state=
/// 'completed'`, the state the daemon writes for a finished turn; sibling
/// CAD-2215 acceptance fixtures use the same write) plus the task's terminal
/// state, committed together. `board_job_list` is `open_tasks_only`, so a
/// terminal task's title is in no agent row or SSE payload: the completion
/// witness is the turn's disappearance, never a done-task marker. Waiting on
/// the cleared row itself also proves the board state was fetched AFTER the
/// commit, so a tracker sentinel cannot stand in for a stale tick. At the
/// hidden start the shared worker therefore has exactly one running turn,
/// never a synthetic two-runner state.
#[test]
fn sequential_shared_worker_private_task_after_visible_completion() {
    let _flag = AssistantFlagOn::arm();
    let (rig, queue_events) = start_queued(&["ana", "ben"]);
    seed(&rig);
    freeze_monitors(&rig);

    // REAL assistant ownership: Ana requests a job on the seeded readable,
    // unoccupied VIS-9. Ben reads the project but is neither owner nor
    // subscriber, so the job and its task are hidden to him. The request is
    // accepted-only; the autonomous sweep (original credential, synthetic
    // `assist.revalidate` service fixture) publishes the canonical job, and
    // the authorized id is read from owner-scoped request detail.
    let job = promoted_member_job(
        &rig,
        &queue_events,
        "ana",
        "CAD2215_SEQUENTIAL_PRIVATE_JOB",
        "cad2215-sequential-private-job",
        "VIS-9",
    );

    let opts = ServeOptions::default();
    opts.provider_env
        .set("CADENCE_PM_DIR", rig.pm.to_str().unwrap());
    let shared = crate::daemon::Shared::new(rig._dir.path(), &opts).unwrap();
    let mut db = rusqlite::Connection::open(rig._dir.path().join("cadence.sqlite3")).unwrap();
    let bound: Option<String> = db
        .query_row("SELECT issue_id FROM jobs WHERE id=?1", [&job], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(
        bound.as_deref(),
        Some("VIS-9"),
        "the request did not bind the readable issue it selected"
    );
    assert!(
        shared.store.assist_request_owns(ORG, "ana", &job).unwrap(),
        "the real ownership ledger does not name Ana"
    );
    assert!(
        !shared.store.assist_request_owns(ORG, "ben", &job).unwrap(),
        "Ben appears as an owner of the private job"
    );
    assert!(
        !shared.store.job_is_subscriber(&job, ORG, "ben").unwrap(),
        "Ben appears as a subscriber of the private job"
    );

    // The private job's task runs on the SAME shared ordinary worker as the
    // visible fixture task: that is the coverage this scenario adds.
    let task: String = db
        .query_row(
            "SELECT id FROM tasks WHERE job_id=?1 ORDER BY id LIMIT 1",
            [&job],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        db.execute(
            "UPDATE tasks SET assignee='cad2215-worker', state='running' WHERE id=?1",
            [&task]
        )
        .unwrap(),
        1
    );

    let ben = login(&rig, "ben", "member");
    let operator = login(&rig, "cad2215-scoped-sequence-operator", "owner");
    let mut op = MemberStream::open(&rig, &operator, true, "sequence operator");
    let mut entity = MemberStream::open(&rig, &ben, true, "sequence member entity");
    let mut legacy = MemberStream::open(&rig, &ben, false, "sequence member legacy");

    // Readiness barrier before the first daemon write. A change written into a
    // just-subscribed shared watcher can be absorbed by the baseline the
    // watcher seeds at subscribe (the fixture store writes bypass the board's
    // change mark), so this publishes and drains one visible tracker checkpoint
    // on every stream first. The distinct-worker case does the same; each frame
    // below is an event, not a sleep.
    let ready = "CAD2215_SEQUENTIAL_READY";
    write_issue(&rig, "visible", "VIS-1", ready);
    assert!(
        op.through(ready).contains(ready),
        "operator baseline never settled"
    );
    assert!(
        entity.through(ready).contains(ready),
        "member entity baseline never settled"
    );
    assert!(
        legacy.through(ready).contains(ready),
        "member legacy baseline never settled"
    );
    assert!(
        op.through("event: aggregates")
            .contains("event: aggregates"),
        "operator baseline tick did not end with its aggregate"
    );

    // Phase 1: a visible ordinary turn starts on the shared worker. Members
    // see the authorized message ID and its generic invalidation, never the
    // body (`member_running_item` redacts bodies).
    let start_body = "sequential visible turn started";
    let start_id = "cad2215-sequential-visible-start";
    visible_message(&rig, &shared, start_body, start_id);

    // Diagnostics (author-owned): separate a daemon/board projection problem
    // from watcher loss before blaming the stream. The fixture row must be
    // running, and the operator's own agent detail must already carry the body
    // the stream is expected to relay.
    let running: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM messages WHERE id=?1 AND alias='cad2215-worker' AND state='running'",
            [start_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        running, 1,
        "fixture: the visible start message is not running"
    );
    let operator_view = get(&rig, &operator, "/api/agents/cad2215-worker").to_string();
    assert!(
        operator_view.contains(start_body),
        "operator agent detail does not carry the visible running-message body: {operator_view}"
    );
    let (operator_frames, reached) = op.collect_until(start_body);
    assert!(
        reached,
        "operator stream did not deliver the visible start body; frames observed: {operator_frames}"
    );
    for stream in [&mut entity, &mut legacy] {
        let interval = stream.until_checkpoint_or_close(start_id);
        assert!(
            !interval.closed,
            "member stream closed on the visible start"
        );
        assert!(
            interval.frames.iter().any(|frame| frame.contains(start_id)),
            "the visible start ID was not delivered: {:?}",
            interval.frames
        );
        assert!(
            interval
                .frames
                .iter()
                .any(|frame| event_name(frame) == Some("agents")),
            "the visible start carried no generic `agents` invalidation: {:?}",
            interval.frames
        );
        assert!(
            !interval
                .frames
                .iter()
                .any(|frame| frame.contains(start_body)),
            "member stream leaked the visible start body: {:?}",
            interval.frames
        );
    }
    let view = get(&rig, &ben, "/api/agents/cad2215-worker").to_string();
    assert!(
        view.contains(start_id),
        "scoped member read lost the visible start ID"
    );
    assert!(
        !view.contains(start_body),
        "scoped member read leaked the visible start body"
    );

    // A distinct visible tracker drain after EVERY start control. Starting the
    // turn also moves the VIS-1 card's bound-agent strip, so the start batch
    // can still carry an `issue` refresh (which names the running start id)
    // after the `agent` row the start interval stopped on. Draining it here
    // keeps start-phase frames out of the completion interval.
    let start_drained = "CAD2215_SEQUENTIAL_START_DRAIN";
    write_issue(&rig, "visible", "VIS-1", start_drained);
    assert!(op.through(start_drained).contains(start_drained));
    assert!(entity.through(start_drained).contains(start_drained));
    assert!(legacy.through(start_drained).contains(start_drained));
    assert!(
        op.through("event: aggregates")
            .contains("event: aggregates"),
        "operator start-drain tick did not end with its aggregate"
    );

    // Phase 2: complete the visible turn AND mark its task terminal in one
    // transaction. The board's job projection is `open_tasks_only`, so the
    // done task's title is in no agent row or SSE payload; the completion
    // witness is the running turn's DISAPPEARANCE: the worker row's `running`
    // and `message` clear, and the same committed snapshot moves the legacy
    // `agents` (row) and `jobs` (open task list) invalidations. Waiting on the
    // cleared row itself (not a later sentinel) proves the board state was
    // fetched after this commit; a tick that fetched it before the commit can
    // only show the still-running row.
    // The entity frame, not `agent_meta`: the operator's meta carries totals
    // and `by_issue` rows that can also say `running:0`/`message:null`, but
    // only the `agent` row is the cleared running projection.
    fn completed_row(frame: &str) -> bool {
        frame.starts_with("event: agent\n")
            && frame.contains("\"running\":0")
            && frame.contains("cad2215-worker")
            && frame.contains("\"message\":null")
    }
    {
        let tx = db.transaction().unwrap();
        assert_eq!(
            tx.execute(
                "UPDATE messages SET state='completed',completed=42 WHERE id=?1 AND state='running'",
                [start_id]
            )
            .unwrap(),
            1,
            "the visible message was not running"
        );
        assert_eq!(
            tx.execute(
                "UPDATE tasks SET state='done' WHERE id='cad2215-visible-task'",
                [],
            )
            .unwrap(),
            1,
            "the visible task was not open"
        );
        tx.commit().unwrap();
    }
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM messages WHERE alias='cad2215-worker' AND state='running'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0,
        "the shared worker still has a running turn after the visible completion"
    );
    let operator_done = op.until_where_or_close(completed_row);
    assert!(
        !operator_done.closed,
        "operator stream closed before the completion row: {:?}",
        operator_done.frames
    );
    assert!(
        operator_done
            .frames
            .iter()
            .any(|f| event_name(f) == Some("jobs")),
        "operator saw no legacy `jobs` invalidation for the finished task: {:?}",
        operator_done.frames
    );
    assert!(
        operator_done
            .frames
            .iter()
            .any(|f| event_name(f) == Some("agents")),
        "operator saw no legacy `agents` invalidation for the cleared row: {:?}",
        operator_done.frames
    );
    assert!(
        !operator_done.frames.iter().any(|f| f.contains(start_id)),
        "operator completion frames still name the finished turn: {:?}",
        operator_done.frames
    );
    for stream in [&mut entity, &mut legacy] {
        let interval = stream.until_where_or_close(completed_row);
        assert!(
            !interval.closed,
            "member stream closed before the completion row: {:?}",
            interval.frames
        );
        assert!(
            interval
                .frames
                .iter()
                .any(|f| event_name(f) == Some("jobs")),
            "member saw no legacy `jobs` invalidation for the finished task: {:?}",
            interval.frames
        );
        assert!(
            interval
                .frames
                .iter()
                .any(|f| event_name(f) == Some("agents")),
            "member saw no legacy `agents` invalidation for the cleared row: {:?}",
            interval.frames
        );
        assert!(
            !interval.frames.iter().any(|f| f.contains(start_id)),
            "member completion frames still name the finished turn: {:?}",
            interval.frames
        );
    }
    // A visible tracker sentinel now drains whatever the completion tick left
    // (aggregate/keepalive ordering), so the hidden phase starts from an empty
    // member queue. Nothing about the completion is inferred from it.
    let drained = "CAD2215_SEQUENTIAL_COMPLETED_DRAIN";
    write_issue(&rig, "visible", "VIS-1", drained);
    assert!(op.through(drained).contains(drained));
    assert!(entity.through(drained).contains(drained));
    assert!(legacy.through(drained).contains(drained));

    // Phase 3: the hidden private turn starts on the SAME worker, now the only
    // running turn. The operator sees the body; no member frame may precede a
    // visible tracker sentinel.
    let hidden = "sequential private turn hidden";
    let hidden_message = "cad2215-sequential-private-hidden";
    shared
        .store
        .enqueue_task(
            "cad2215-worker",
            hidden,
            None,
            hidden_message,
            "user",
            Some(task.as_str()),
        )
        .unwrap();
    assert_eq!(
        db.execute(
            "UPDATE messages SET state='running' WHERE id=?1",
            [hidden_message]
        )
        .unwrap(),
        1
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM messages WHERE alias='cad2215-worker' AND state='running'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1,
        "the hidden turn is not the shared worker's only running turn"
    );
    assert!(
        op.through(hidden).contains(hidden),
        "operator positive control lost the hidden private turn"
    );
    let checkpoint = "CAD2215_SEQUENTIAL_CHECK";
    write_issue(&rig, "visible", "VIS-1", checkpoint);
    assert!(op.through(checkpoint).contains(checkpoint));
    let entity_hidden = entity.until_checkpoint_or_close(checkpoint);
    let legacy_hidden = legacy.until_checkpoint_or_close(checkpoint);
    for interval in [&entity_hidden, &legacy_hidden] {
        assert_no_hidden(
            interval,
            &[
                hidden,
                "CAD2215_SEQUENTIAL_PRIVATE_JOB",
                job.as_str(),
                task.as_str(),
                hidden_message,
                start_id,
            ],
        );
        assert_no_frame_kind(
            interval,
            &["jobs", "agents", "agent", "threads", "monitoring"],
            hidden,
        );
        assert_sentinel_reached(interval, checkpoint, hidden);
    }
    // The hidden turn is not readable through the member's scoped HTTP row.
    let view = get(&rig, &ben, "/api/agents/cad2215-worker").to_string();
    assert!(
        !view.contains(hidden_message),
        "scoped member read leaked the hidden message ID"
    );
    assert!(
        !view.contains(hidden),
        "scoped member read leaked the hidden message body"
    );
}
