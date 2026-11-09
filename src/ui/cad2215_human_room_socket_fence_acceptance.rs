//! Independent CAD-2215 real-board socket-fence acceptance.
//! The implementation owner must not edit this test.
use super::*;
use std::io::{BufRead, BufReader, Write};
use std::net::Shutdown;
use std::os::fd::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

// An already queued connection takes precedence over the teardown wakeup.
fn accept_ready(listener: &UnixListener, wake: &UnixStream) -> bool {
    let mut fds = [
        libc::pollfd {
            fd: listener.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: wake.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    loop {
        let rc = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, -1) };
        if rc < 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            panic!("fixture poll: {e}");
        }
        assert_eq!(
            fds[0].revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL),
            0,
            "fixture listener poll error: {}",
            fds[0].revents
        );
        if fds[0].revents & libc::POLLIN != 0 {
            return true;
        }
        if fds[1].revents != 0 {
            return false;
        }
    }
}

struct SocketFence {
    accept: JoinHandle<()>,
    workers: Arc<Mutex<Vec<JoinHandle<()>>>>,
    proxy_wake: UnixStream,
    replacement_wake: UnixStream,
    swapped: mpsc::Receiver<()>,
    seen: Arc<Mutex<Vec<String>>>,
    original: PathBuf,
    renamed: PathBuf,
}

impl SocketFence {
    fn install(state: &Path, close_after_health: bool) -> Self {
        assert!(
            std::env::var_os("CADENCE_SOCKET").is_none(),
            "fixture requires CADENCE_SOCKET unset"
        );
        let original = state.join("cadence.sock");
        let renamed = state.join("fence-real.sock");
        assert!(!renamed.exists());
        std::fs::rename(&original, &renamed).unwrap();
        let proxy = UnixListener::bind(&original).unwrap();
        let (proxy_wake, proxy_wait) = UnixStream::pair().unwrap();
        let (replacement_wake, replacement_wait) = UnixStream::pair().unwrap();
        let workers = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let fired = Arc::new(AtomicBool::new(false));
        let (swap_tx, swapped) = mpsc::channel();
        let (accept_workers, accept_seen, accept_fired) = (workers.clone(), seen.clone(), fired);
        let (socket_path, real_path) = (original.clone(), renamed.clone());
        let replacement_wait = Arc::new(Mutex::new(Some(replacement_wait)));
        let accept = thread::spawn(move || {
            while accept_ready(&proxy, &proxy_wait) {
                let (board, _) = proxy.accept().expect("proxy accept");
                let (state, real, workers, seen, fired, tx, replacement_wait) = (
                    socket_path.clone(),
                    real_path.clone(),
                    accept_workers.clone(),
                    accept_seen.clone(),
                    accept_fired.clone(),
                    swap_tx.clone(),
                    replacement_wait.clone(),
                );
                let worker = thread::spawn(move || {
                    let daemon = UnixStream::connect(real).expect("connect genuine daemon");
                    board
                        .set_read_timeout(Some(Duration::from_secs(8)))
                        .unwrap();
                    board
                        .set_write_timeout(Some(Duration::from_secs(8)))
                        .unwrap();
                    daemon
                        .set_read_timeout(Some(Duration::from_secs(8)))
                        .unwrap();
                    daemon
                        .set_write_timeout(Some(Duration::from_secs(8)))
                        .unwrap();
                    let mut input = BufReader::new(board.try_clone().unwrap());
                    let mut output = board;
                    let mut daemon_input = BufReader::new(daemon.try_clone().unwrap());
                    let mut daemon_output = daemon;
                    loop {
                        let mut request = String::new();
                        if !matches!(input.read_line(&mut request), Ok(n) if n > 0) {
                            break;
                        }
                        let parsed: Value =
                            serde_json::from_str(&request).expect("board RPC frame");
                        let method = parsed["method"].as_str().expect("RPC method");
                        if daemon_output.write_all(request.as_bytes()).is_err() {
                            break;
                        }
                        let mut response = String::new();
                        if !matches!(daemon_input.read_line(&mut response), Ok(n) if n > 0) {
                            break;
                        }
                        if method == "health" && !fired.swap(true, Ordering::SeqCst) {
                            let frame: Value =
                                serde_json::from_str(&response).expect("daemon health frame");
                            assert_eq!(frame["ok"], true, "genuine health must succeed: {frame}");
                            assert!(
                                frame["result"]["capabilities"]
                                    .as_array()
                                    .unwrap()
                                    .iter()
                                    .any(|c| c.as_str()
                                        == Some(
                                            crate::adapter::registry::HUMAN_ROOM_OPAQUE_CAPABILITY
                                        )),
                                "genuine daemon capability missing: {frame}"
                            );
                            std::fs::remove_file(&state).unwrap();
                            let replacement = UnixListener::bind(&state).unwrap();
                            let wait = replacement_wait.lock().unwrap().take().unwrap();
                            let seen = Arc::clone(&seen);
                            let replacement_worker = thread::spawn(move || {
                                while accept_ready(&replacement, &wait) {
                                    let (mut stream, _) =
                                        replacement.accept().expect("replacement accept");
                                    seen.lock().unwrap().push("<connection>".to_string());
                                    stream
                                        .set_read_timeout(Some(Duration::from_secs(3)))
                                        .unwrap();
                                    stream
                                        .set_write_timeout(Some(Duration::from_secs(3)))
                                        .unwrap();
                                    let mut reader = BufReader::new(stream.try_clone().unwrap());
                                    loop {
                                        let mut line = String::new();
                                        if !matches!(reader.read_line(&mut line), Ok(n) if n > 0) {
                                            break;
                                        }
                                        let frame: Value =
                                            serde_json::from_str(&line).expect("replacement frame");
                                        let method =
                                            frame["method"].as_str().expect("replacement method");
                                        seen.lock().unwrap().push(method.to_string());
                                        let error = json!({"ok":false,"error":{"kind":"rejected","message":"incompatible replacement"}});
                                        if writeln!(stream, "{error}").is_err() {
                                            break;
                                        }
                                    }
                                }
                            });
                            workers.lock().unwrap().push(replacement_worker);
                            tx.send(()).unwrap();
                            if close_after_health {
                                output.shutdown(Shutdown::Read).unwrap();
                                output.write_all(response.as_bytes()).unwrap();
                                output.shutdown(Shutdown::Write).unwrap();
                                break;
                            }
                        }
                        if output.write_all(response.as_bytes()).is_err() {
                            break;
                        }
                    }
                });
                accept_workers.lock().unwrap().push(worker);
            }
        });
        Self {
            accept,
            workers,
            proxy_wake,
            replacement_wake,
            swapped,
            seen,
            original,
            renamed,
        }
    }

    fn finish(mut self) -> Vec<String> {
        self.swapped
            .recv_timeout(Duration::from_secs(15))
            .expect("genuine capability swap did not occur");
        self.proxy_wake.write_all(&[1]).unwrap();
        self.replacement_wake.write_all(&[1]).unwrap();
        self.accept.join().expect("proxy accept thread");
        // Workers can register the replacement worker; never hold the lock
        // across a join and drain until no registered workers remain.
        loop {
            let worker = self.workers.lock().unwrap().pop();
            match worker {
                Some(worker) => worker.join().expect("fixture worker"),
                None => break,
            }
        }
        std::fs::remove_file(&self.original).unwrap();
        std::fs::rename(&self.renamed, &self.original).unwrap();
        let methods = self.seen.lock().unwrap().clone();
        methods
    }
}

fn count_body(rig: &Rig, body: &str) -> i64 {
    room_fixture_db(rig)
        .query_row(
            "SELECT count(*) FROM topic_posts WHERE body=?1",
            [body],
            |r| r.get(0),
        )
        .unwrap()
}

fn human_room_socket_path_replacement(close_after_health: bool) {
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
    let ana = login(&rig, "ana", "member");
    let ben = login(&rig, "ben", "member");
    roster.lock().unwrap().allowed_cookies = vec![ana.clone(), ben.clone()];
    let group = room_ok(
        &rig,
        &ana,
        "POST",
        "/api/human-threads/groups",
        Some(json!({
            "kind":"human_group", "title":"socket fence", "client_id":"fence-group",
            "humans":["ben"], "agents":[]
        })),
    );
    let room = group["room"].as_str().expect("created room");
    let control = room_post(&rig, &ana, room, "fence-control", "fence positive control");
    assert_room_post(&control["post"]);
    assert_eq!(count_body(&rig, "fence positive control"), 1);
    let body = if close_after_health {
        "fence closed unique body"
    } else {
        "fence pinned unique body"
    };
    assert_eq!(count_body(&rig, body), 0);
    let fence = SocketFence::install(rig._dir.path(), close_after_health);
    let (status, response) = room_http(
        &rig,
        &ana,
        "POST",
        &format!("/api/human-threads/rooms/{room}/posts"),
        Some(json!({"client_id":"fence-attempt", "body":body})),
    );
    let replacement_methods = fence.finish();
    assert!(
        replacement_methods.is_empty(),
        "board reconnected to replacement: {replacement_methods:?}"
    );
    if close_after_health {
        assert_eq!(
            status, 503,
            "closed pinned session must report unavailable: {response}"
        );
        assert_eq!(
            count_body(&rig, body),
            0,
            "closed connection applied mutation"
        );
    } else {
        assert_eq!(
            status, 200,
            "pinned original daemon must accept post: {response}"
        );
        assert_room_post(&response["post"]);
        assert_eq!(
            count_body(&rig, body),
            1,
            "original daemon must apply exactly one post"
        );
    }
}

#[test]
fn human_room_socket_path_replacement_keeps_the_checked_connection() {
    human_room_socket_path_replacement(false);
}

#[test]
fn human_room_socket_path_replacement_never_reconnects_after_close() {
    human_room_socket_path_replacement(true);
}
