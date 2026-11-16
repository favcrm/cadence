//! Independently authored CAD-2337 refusal acceptance; implementers must not
//! edit or weaken it. Written from the ticket, not the implementation.
//!
//! The ticket: a daemon that died leaves `cadence.sock` behind, and hosted
//! `issue finish` used to refuse every lane ("Daemon unreachable while
//! enumerating agents/tasks"). A socket nobody listens on is a stopped daemon.
//! That relaxation must not weaken the guard: whenever real use is possible
//! finish still refuses and removes nothing. Every case below drives the real
//! hosted release (`finish_guidance::release_with`) on a real git repository
//! and lane, against a state dir that holds either a stale socket file or a
//! live fake daemon speaking the daemon's line protocol.
//!
//! Only temp state dirs are used; the production socket is never named.

use super::*;
use crate::client;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

const ORG: &str = "ws_test";
const ISSUE: &str = "CAD-1";
const LANE: &str = "test";
const TRANSPORT_TEXT: &str = "Daemon is not reachable at /x/cadence.sock — start it with \
                              `cadence daemon start`";

/// Hosted finish needs no board request; any request fails the release.
struct NoBoard;

impl CoordHttp for NoBoard {
    fn send(&mut self, method: &str, path: &str, _: Option<&Value>) -> Result<Reply> {
        Err(Error::rejected(format!(
            "unexpected board request during hosted finish: {method} {path}"
        )))
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

/// A hosted lane bound to a finished assignment, with a state dir of its own.
struct Lane {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    path: PathBuf,
    auth: PathBuf,
    state: PathBuf,
}

impl Lane {
    /// `merged`: the lane's work is fast-forwarded into the default branch.
    /// Otherwise it carries a commit that survives nowhere.
    fn new(merged: bool) -> Self {
        assert!(
            std::env::var_os("CADENCE_SOCKET").is_none(),
            "CADENCE_SOCKET would redirect the probes away from the fixture's state dir"
        );
        let tmp = tempfile::Builder::new().prefix("c2337").tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let auth = tmp.path().join("auth");
        let state = tmp.path().join("state");
        for dir in [&repo, &auth, &state] {
            fs::create_dir(dir).unwrap();
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).unwrap();
        }
        git(&repo, &["init", "-b", "main"]);
        git(&repo, &["config", "user.email", "t@t"]);
        git(&repo, &["config", "user.name", "t"]);
        git(&repo, &["config", "commit.gpgsign", "false"]);
        fs::write(repo.join(".gitignore"), ".cadence/\n.env\n").unwrap();
        fs::write(repo.join("file"), "base\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-m", "base"]);
        git(&repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        git(
            &repo,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/main",
            ],
        );
        let base = git(&repo, &["rev-parse", "HEAD"]);
        let branch = layout::branch(LANE);
        let path = ensure_lane(&repo, LANE, Some(&base)).expect("real hosted lane setup");
        fs::write(path.join("file"), "lane work\n").unwrap();
        git(&path, &["commit", "-am", "lane work"]);
        if merged {
            git(&repo, &["merge", "--ff-only", &branch]);
            git(&repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        }
        let mut ledger = Ledger::load(&auth, ORG).unwrap();
        let attempt: Attempt = serde_json::from_value(json!({
            "operation_id": "op_test", "issue_id": ISSUE, "action": "start", "target": null,
            "assignment_id": "hla_test", "generation": 1, "claim_id": "hlc_test",
            "state": "result", "fenced": false, "reassign_operation_id": null,
            "worktree": path, "lane": LANE, "lane_claim": "hlc_test", "lane_generation": 1
        }))
        .unwrap();
        ledger
            .attempts
            .insert(attempt_key(ISSUE, "start", None), attempt);
        ledger.save(&auth, ORG).unwrap();
        let lane = Self {
            _tmp: tmp,
            repo,
            path,
            auth,
            state,
        };
        lane.idle();
        lane
    }

    /// Every file that is not ignored last changed hours ago, so only the
    /// check under test can refuse.
    fn idle(&self) {
        let past = std::time::SystemTime::now() - Duration::from_secs(2 * 3600);
        for rel in git(&self.path, &["ls-files"]).lines() {
            fs::OpenOptions::new()
                .write(true)
                .open(self.path.join(rel))
                .unwrap()
                .set_modified(past)
                .unwrap();
        }
    }

    fn finish(&self) -> Result<Value> {
        let coord = Coordinator::new(NoBoard, ORG.into(), self.auth.clone()).unwrap();
        finish_guidance::release_with(&coord, ISSUE, None, &self.state, &|_, _| None)
    }

    fn tip(&self) -> Option<String> {
        let branch = layout::branch(LANE);
        (!git(&self.repo, &["branch", "--list", &branch]).is_empty())
            .then(|| git(&self.repo, &["rev-parse", &branch]))
    }

    fn registered(&self) -> bool {
        git(&self.repo, &["worktree", "list", "--porcelain"]).contains(self.path.to_str().unwrap())
    }

    /// A daemon that died: the socket file is there, nothing listens on it.
    fn stale_socket(&self) {
        drop(UnixListener::bind(client::socket_path(&self.state)).unwrap());
        let socket = client::socket_path(&self.state);
        assert!(socket.exists(), "the stale socket file must exist");
        let refused = UnixStream::connect(&socket).unwrap_err();
        assert_eq!(refused.kind(), std::io::ErrorKind::ConnectionRefused);
    }

    /// Finish refuses and removes nothing; returns the refusal text.
    fn refused_and_kept(&self) -> String {
        let tip = self.tip().expect("the lane branch exists before finish");
        let content = fs::read(self.path.join("file")).unwrap();
        let error = self
            .finish()
            .expect_err("hosted finish must refuse this lane");
        assert_eq!(error.kind(), "rejected", "{error}");
        assert!(self.path.is_dir(), "the lane checkout was removed");
        assert!(self.registered(), "the lane is no longer registered");
        assert_eq!(self.tip(), Some(tip), "the lane branch moved or went");
        assert_eq!(fs::read(self.path.join("file")).unwrap(), content);
        error.to_string()
    }

    fn assert_released(&self) {
        let done = self
            .finish()
            .expect("a merged, clean, idle lane must be released");
        assert_eq!(done["finished"], true, "{done}");
        assert_eq!(done["removed_worktree"], true, "{done}");
        assert!(!self.path.exists(), "the lane checkout still exists");
        assert!(!self.registered());
        assert_eq!(self.tip(), None, "the lane branch was not deleted");
    }
}

/// What the fake daemon does with one request.
enum Answer {
    Ok(Value),
    /// A raw frame (an error answer).
    Frame(Value),
    /// Take the request and hang up with no answer.
    Close,
    /// Take the request and say nothing for this long.
    Hang(Duration),
}

/// A live daemon on the state dir's socket. It answers with the real line
/// protocol and records the methods it was asked.
struct FakeDaemon {
    stop: Arc<AtomicBool>,
    calls: Arc<Mutex<Vec<String>>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl FakeDaemon {
    fn serve(state: &Path, answer: impl Fn(&str, &Value) -> Answer + Send + 'static) -> Self {
        let listener = UnixListener::bind(client::socket_path(state)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let calls = Arc::new(Mutex::new(Vec::new()));
        let (stop_t, calls_t) = (stop.clone(), calls.clone());
        let thread = std::thread::spawn(move || {
            while !stop_t.load(Ordering::SeqCst) {
                let Ok((stream, _)) = listener.accept() else {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut line = String::new();
                if BufReader::new(&stream).read_line(&mut line).is_err() {
                    continue;
                }
                let Ok(request) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                let method = request["method"].as_str().unwrap_or_default().to_string();
                calls_t.lock().unwrap().push(method.clone());
                match answer(&method, &request["params"]) {
                    Answer::Ok(result) => {
                        let _ = writeln!(&stream, "{}", crate::proto::ok(result));
                    }
                    Answer::Frame(frame) => {
                        let _ = writeln!(&stream, "{frame}");
                    }
                    Answer::Close => {}
                    Answer::Hang(how_long) => std::thread::sleep(how_long),
                }
            }
        });
        Self {
            stop,
            calls,
            thread: Some(thread),
        }
    }

    fn asked(&self, method: &str) -> bool {
        self.calls.lock().unwrap().iter().any(|m| m == method)
    }
}

impl Drop for FakeDaemon {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn error_frame(kind: &str, message: &str) -> Value {
    json!({"ok": false, "error": {"kind": kind, "message": message}})
}

/// A refusal never tells the reader to run a bare `cadence daemon start`: a
/// hosted default org refuses it as remote.
fn assert_no_bare_daemon_start(text: &str) {
    let rest = text.replace("cadence --org local daemon start", "");
    assert!(
        !rest.contains("cadence daemon start"),
        "bare `cadence daemon start` suggested: {text}"
    );
}

/// A refusal that says the daemon could not be checked, never one that reads
/// as if the daemon were stopped.
fn assert_reads_as_unverified_daemon(text: &str) {
    let lower = text.to_lowercase();
    assert!(
        lower.contains("unreachable")
            || lower.contains("inconclusive")
            || lower.contains("cannot enumerate")
            || lower.contains("cannot check"),
        "must say the check could not run: {text}"
    );
    for stopped in ["stopped", "no listener", "stale"] {
        assert!(!lower.contains(stopped), "reads as {stopped}: {text}");
    }
}

// 1. Stale socket, a process standing in the worktree.
#[test]
fn a_stale_socket_still_refuses_while_a_process_stands_in_the_worktree() {
    let lane = Lane::new(true);
    lane.stale_socket();
    let mut child = Command::new("sleep")
        .arg("60")
        .current_dir(&lane.path)
        .spawn()
        .unwrap();
    let reason = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| lane.refused_and_kept()));
    let _ = child.kill();
    let _ = child.wait();
    let reason = reason.unwrap();
    assert!(
        reason.contains("cwd") && reason.contains(&child.id().to_string()),
        "the /proc scan must name the process: {reason}"
    );
}

// 2. Stale socket, uncommitted work or unmerged work.
#[test]
fn a_stale_socket_still_refuses_a_lane_with_uncommitted_changes() {
    let lane = Lane::new(true);
    lane.stale_socket();
    fs::write(lane.path.join("file"), "uncommitted edit\n").unwrap();
    lane.idle();
    let reason = lane.refused_and_kept();
    assert!(reason.contains("uncommitted changes"), "{reason}");
}

#[test]
fn a_stale_socket_still_refuses_a_lane_with_unmerged_work() {
    let lane = Lane::new(false);
    lane.stale_socket();
    let reason = lane.refused_and_kept();
    assert!(reason.contains("not shown merged"), "{reason}");
}

// 5. Positive controls: nothing else is in the way, so the daemon check alone
// decides.
#[test]
fn a_stale_socket_does_not_block_a_merged_clean_idle_lane() {
    let lane = Lane::new(true);
    lane.stale_socket();
    lane.assert_released();
}

#[test]
fn a_live_daemon_that_lists_no_agents_does_not_block_a_merged_clean_idle_lane() {
    let lane = Lane::new(true);
    let daemon = FakeDaemon::serve(&lane.state, |method, _| match method {
        "agent_list" => Answer::Ok(json!({"agents": []})),
        other => Answer::Frame(error_frame("rejected", &format!("unexpected {other}"))),
    });
    lane.assert_released();
    assert!(daemon.asked("agent_list"), "the daemon was never consulted");
}

// 3. A live daemon that cannot give a usable answer still refuses.
#[test]
fn a_live_daemon_that_takes_the_request_and_hangs_up_still_refuses() {
    let lane = Lane::new(true);
    let daemon = FakeDaemon::serve(&lane.state, |_, _| Answer::Close);
    let reason = lane.refused_and_kept();
    assert!(daemon.asked("agent_list"), "the daemon was never consulted");
    assert_reads_as_unverified_daemon(&reason);
    assert_no_bare_daemon_start(&reason);
}

#[test]
fn a_live_daemon_that_hangs_past_the_timeout_still_refuses() {
    let lane = Lane::new(true);
    let daemon = FakeDaemon::serve(&lane.state, |_, _| {
        Answer::Hang(Duration::from_millis(1500))
    });
    let reason = crate::issue::finish::with_probe_timeout(Duration::from_millis(250), || {
        lane.refused_and_kept()
    });
    assert!(daemon.asked("agent_list"), "the daemon was never consulted");
    assert_reads_as_unverified_daemon(&reason);
    assert_no_bare_daemon_start(&reason);
}

#[test]
fn a_live_daemon_that_answers_inconclusively_still_refuses() {
    let lane = Lane::new(true);
    let daemon = FakeDaemon::serve(&lane.state, |_, _| {
        Answer::Frame(error_frame("provider", "upstream is down"))
    });
    let reason = lane.refused_and_kept();
    assert!(daemon.asked("agent_list"), "the daemon was never consulted");
    assert_reads_as_unverified_daemon(&reason);
    assert_no_bare_daemon_start(&reason);
}

#[test]
fn a_live_daemon_whose_error_borrows_the_transport_wording_still_refuses() {
    let lane = Lane::new(true);
    let daemon = FakeDaemon::serve(&lane.state, |_, _| {
        Answer::Frame(error_frame("internal", TRANSPORT_TEXT))
    });
    let reason = lane.refused_and_kept();
    assert!(daemon.asked("agent_list"), "the daemon was never consulted");
    assert_reads_as_unverified_daemon(&reason);
    assert_no_bare_daemon_start(&reason);
    assert!(
        reason.contains("cadence --org local daemon start"),
        "the hint must name the local org: {reason}"
    );
}

#[test]
fn a_live_daemon_whose_agent_lookup_fails_still_refuses() {
    let lane = Lane::new(true);
    let cwd = lane.path.to_string_lossy().into_owned();
    let daemon = FakeDaemon::serve(&lane.state, move |method, _| match method {
        "agent_list" => Answer::Ok(json!({"agents": [{"alias": "worker", "cwd": cwd}]})),
        _ => Answer::Frame(error_frame("internal", TRANSPORT_TEXT)),
    });
    let reason = lane.refused_and_kept();
    assert!(daemon.asked("agent_show"), "the agent was never looked up");
    assert_reads_as_unverified_daemon(&reason);
    assert_no_bare_daemon_start(&reason);
    assert!(
        reason.contains("cadence --org local daemon start"),
        "the hint must name the local org: {reason}"
    );
}

// 4. A live daemon reporting running work bound to this worktree.
#[test]
fn a_live_daemon_with_a_running_message_bound_to_the_worktree_refuses() {
    let lane = Lane::new(true);
    let wt = lane.path.to_string_lossy().into_owned();
    let daemon = FakeDaemon::serve(&lane.state, move |method, _| match method {
        "agent_list" => Answer::Ok(json!({"agents": [
            {"alias": "worker", "cwd": "/elsewhere", "tasks": ["t1"]}
        ]})),
        "task_show" => Answer::Ok(json!({"task": {"worktree": wt, "assignee": "worker"}})),
        "agent_show" => Answer::Ok(json!({
            "agent": {"alias": "worker", "dead": false, "endpoint_kind": "pty"},
            "messages": [{"id": "m-77", "state": "running", "task_id": "t1", "body": "work"}]
        })),
        other => Answer::Frame(error_frame("rejected", &format!("unexpected {other}"))),
    });
    let reason = lane.refused_and_kept();
    assert!(daemon.asked("agent_show"), "the agent was never looked up");
    assert!(
        reason.contains("worker") && reason.contains("m-77") && reason.contains("running"),
        "the refusal must name the agent and message: {reason}"
    );
}

// The predicate behind the relaxation: only a provably dead listener counts.
#[test]
fn only_a_refused_or_missing_socket_has_no_listener() {
    let dir = tempfile::Builder::new().prefix("c2337").tempdir().unwrap();

    let stale = dir.path().join("stale.sock");
    drop(UnixListener::bind(&stale).unwrap());
    assert!(client::socket_has_no_listener(&stale), "stale socket");
    assert!(
        client::socket_has_no_listener(&dir.path().join("missing.sock")),
        "missing socket"
    );

    let live = dir.path().join("live.sock");
    let _listener = UnixListener::bind(&live).unwrap();
    assert!(!client::socket_has_no_listener(&live), "live listener");

    // A listener whose backlog is full: connect(2) answers EAGAIN, not
    // ECONNREFUSED — a daemon that is there but busy.
    let full = dir.path().join("full.sock");
    let (fd, held) = full_backlog(&full);
    assert!(!client::socket_has_no_listener(&full), "full backlog");
    for c in held {
        unsafe { libc::close(c) };
    }
    unsafe { libc::close(fd) };
}

/// A listening unix socket with `listen(0)` and its queue filled by
/// nonblocking connects. Returns the listener fd and the held client fds.
fn full_backlog(path: &Path) -> (i32, Vec<i32>) {
    use std::os::unix::ffi::OsStrExt;
    let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (slot, byte) in addr.sun_path.iter_mut().zip(path.as_os_str().as_bytes()) {
        *slot = *byte as libc::c_char;
    }
    let len = std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t;
    let addr_ptr = (&addr as *const libc::sockaddr_un).cast::<libc::sockaddr>();
    let server = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    assert!(server >= 0);
    assert_eq!(unsafe { libc::bind(server, addr_ptr, len) }, 0);
    assert_eq!(unsafe { libc::listen(server, 0) }, 0);
    let mut held = Vec::new();
    let mut saturated = false;
    for _ in 0..16 {
        let client = unsafe {
            libc::socket(
                libc::AF_UNIX,
                libc::SOCK_STREAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
                0,
            )
        };
        assert!(client >= 0);
        let rc = unsafe { libc::connect(client, addr_ptr, len) };
        held.push(client);
        if rc != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EAGAIN) {
            saturated = true;
            break;
        }
    }
    assert!(saturated, "could not fill the listen backlog");
    (server, held)
}
