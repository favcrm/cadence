//! Independent CAD-1193 negative acceptance, transport layer: the
//! bounded metadata reads (`client::MetaBudget::read`/`read_private`)
//! must fail closed with `Err` inside one ABSOLUTE per-call deadline,
//! at every stage of the one-request/one-connection unix-socket RPC —
//! a request that can never reach a daemon (saturated accept
//! backlog), a request write backed up against a peer that accepts
//! but never drains, and a reply that trickles a byte inside every
//! inactivity window. A per-read inactivity timeout is NOT the bound:
//! bytes arriving inside it reset it, and neither it nor any syscall
//! covers a blocking `writeln!`. This check asserts the absolute
//! window only, never any one syscall's timeout.
//!
//! The listeners are owned fixtures over the REAL socket-path layout
//! and the REAL wire format. They accept, read or send only
//! invalid/partial bytes; none supplies a response frame, an identity,
//! a session, a PID, an operator fact, a refusal or a success — so the
//! only verdicts under test are `Err` within the bound. A byte from a
//! fixture must never parse as daemon authority. The companion native
//! HTTP refusal test (tests/cad1193_operator_metadata.rs, unmodified)
//! already proves the downstream guard.
//!
//! Test-side cleanup is bounded by construction: every fixture thread
//! parks only on the shared `STOP` flag or on writes the client's own
//! close interrupts, every client call runs under a harness watchdog
//! far below the 700 s default, and a panic hook releases all fixtures
//! so a failed assertion can never leave a hung client thread.
#![cfg(target_os = "linux")]

use cadence_agent::client;
use cadence_agent::error::Result;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
use std::sync::{Arc, Mutex, Once};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// The ticket's absolute per-call ceiling asserted by this check:
/// READ_CAP (2 s) plus generous scheduling tolerance for a shared,
/// loaded host. A trickle at 200 ms/byte sits far inside any
/// inactivity timer yet cannot satisfy this bound, so a pass means an
/// absolute deadline ended the call.
const ABSOLUTE_BOUND: Duration = Duration::from_secs(8);
/// Harness watchdog around every client call: long enough never to
/// flake on a loaded lane, short enough that a broken (unbounded)
/// client is reported by this test, not left parked for the 700 s
/// global default.
const WATCHDOG: Duration = Duration::from_secs(60);
/// A spent shared budget or an exhausted one must refuse well inside
/// the ticket's five-second envelope — immediately, not after waiting.
const PROMPT: Duration = Duration::from_secs(5);
/// Trickle pacing: every byte lands well inside any per-read
/// inactivity timeout, so only an absolute deadline can end the read.
const TRICKLE_GAP: Duration = Duration::from_millis(200);
/// Sanity cap on backlog filler probes; unreachable on a real kernel.
const MAX_FILLERS: usize = 64;

static PANIC_HOOK: Once = Once::new();
static STOP: AtomicBool = AtomicBool::new(false);
static WORKERS: Mutex<Vec<JoinHandle<()>>> = Mutex::new(Vec::new());

fn install_cleanup_hook() {
    PANIC_HOOK.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            previous(info);
            // A failed assert must not leave a fixture trickling or a
            // backlog parked forever: wake every worker so the process
            // unwinds instead of waiting out an unbounded client.
            STOP.store(true, SeqCst);
            release_all();
        }));
    });
}

fn spawn(worker: impl FnOnce() + Send + 'static) {
    WORKERS.lock().unwrap().push(std::thread::spawn(worker));
}

fn release_all() {
    let workers = std::mem::take(&mut *WORKERS.lock().unwrap());
    for worker in workers {
        let _ = worker.join();
    }
}

fn isolated_state(artifacts: &Path, name: &str) -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::Builder::new()
        .prefix(&format!("{name}-"))
        .tempdir_in(artifacts)
        .unwrap();
    let state = root.path().join("state");
    std::fs::create_dir(&state).unwrap();
    (root, state)
}

/// Run one real client call on a worker under the harness watchdog.
/// `WATCHDOG` is the only outer bound; the assertion on the elapsed
/// `Duration` is what proves the client's own deadline. The worker is
/// deliberately DETACHED, never joined by `release_all`: a client
/// without an absolute bound can park in an uninterruptible blocking
/// connect/write/read (the very defect under test), and joining that
/// worker would hang teardown — the process-exit is its cleanup.
fn bounded_call(f: impl FnOnce() -> Result<Value> + Send + 'static) -> (Result<Value>, Duration) {
    let (tx, rx) = std::sync::mpsc::sync_channel(0);
    std::thread::spawn(move || {
        let start = Instant::now();
        let result = f();
        let _ = tx.send((result, start.elapsed()));
    });
    match rx.recv_timeout(WATCHDOG) {
        Ok(done) => done,
        Err(_) => panic!("client metadata read outlasted the {WATCHDOG:?} harness watchdog"),
    }
}

fn assert_err_bounded(label: &str, outcome: (Result<Value>, Duration)) -> Duration {
    let (result, elapsed) = outcome;
    assert!(
        result.is_err(),
        "{label}: a stalled or invalid transport must never yield Ok: {result:?}"
    );
    assert!(
        elapsed <= ABSOLUTE_BOUND,
        "{label}: exceeded the absolute {ABSOLUTE_BOUND:?} bound ({elapsed:?}); \
         connect/write/inactivity escapes leave a metadata read unbounded"
    );
    elapsed
}

/// Probe connect on a worker: `Some(stream)` if the kernel completed
/// the handshake inside `window`, `None` while it stays parked in
/// connect(2) (backlog full). Detached like the client worker: a
/// probe parked in a blocking connect is released only when a queue
/// slot frees, so it must never be joined by teardown.
fn probe_connect(path: &Path, window: Duration) -> Option<UnixStream> {
    let path = path.to_path_buf();
    let (tx, rx) = std::sync::mpsc::sync_channel(0);
    std::thread::spawn(move || {
        let _ = tx.send(UnixStream::connect(&path).ok());
    });
    rx.recv_timeout(window).ok().flatten()
}

/// Accept loop helper: poll a nonblocking listener until `STOP`,
/// handing each connection to `on_conn`. Owned fixture only.
fn serve_until_stop(listener: UnixListener, on_conn: impl Fn(UnixStream) + Send + 'static) {
    listener.set_nonblocking(true).unwrap();
    while !STOP.load(SeqCst) {
        match listener.accept() {
            Ok((peer, _)) => on_conn(peer),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => return,
        }
    }
}

#[test]
fn cad1193_metadata_transport_deadline_is_absolute_across_rpc_stages() {
    // `MetaBudget::read` honors a caller's CADENCE_SOCKET override;
    // `read_private` never does. Clear any inherited value so the
    // shared-path read is pinned to this test's own fixture socket —
    // the same isolation the frozen HTTP check gives its daemon.
    std::env::remove_var("CADENCE_SOCKET");
    install_cleanup_hook();
    let artifacts = Path::new("/tmp/e8qa/cad1193-transport-acceptance");
    std::fs::create_dir_all(artifacts).unwrap();

    // -- Stage 0: an already-exhausted budget errors without
    //    transport, on either socket flavor. Baseline: `read` and
    //    `read_private` are `Err`, never a caller-supplied fallback,
    //    when no dependency time remains.
    {
        let (_root, state) = isolated_state(artifacts, "exhausted");
        let exhausted = client::MetaBudget::exhausted();
        for label in ["read", "read_private"] {
            let start = Instant::now();
            let result = match label {
                "read" => exhausted.read(&state, "agent_list", json!({})),
                _ => exhausted.read_private(&state, "agent_list", json!({})),
            };
            assert!(result.is_err(), "{label} on an exhausted budget must Err");
            assert!(
                start.elapsed() < PROMPT,
                "{label} on an exhausted budget must not wait for transport"
            );
        }
        eprintln!("CAD1193 transport: exhausted budget refuses without transport");
    }

    // -- Stage 1: request never reaches a daemon whose accept
    //    backlog is saturated --
    // A real socket bound and listening with backlog 1 whose owner
    // never accepts. On Linux a unix `connect` to a full listen queue
    // is queued pending and returns success, but the request bytes
    // sit unread on a connection no peer will ever drain — the wire
    // stage that actually stalls is the WRITE, exactly as with a
    // wedged daemon. We saturate with a held pending stream (proved:
    // a follow-up connect stops completing), then send a request far
    // larger than the peer buffer so the client's write phase blocks.
    // Under a blocking-connect client this same queue also stalls the
    // connect itself; the bound must cover the whole RPC either way.
    {
        let (_root, state) = isolated_state(artifacts, "backlog");
        let socket = client::socket_path(&state);
        // Raw fd: std offers no backlog control, and the stalled-queue
        // repro needs an exact, tiny backlog.
        let listener_fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
        assert!(
            listener_fd >= 0,
            "fixture socket(2): {:?}",
            std::io::Error::last_os_error()
        );
        let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
        addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
        let bytes = socket.to_str().unwrap().as_bytes();
        assert!(bytes.len() < addr.sun_path.len());
        addr.sun_path[..bytes.len()]
            .copy_from_slice(unsafe { &*(bytes as *const [u8] as *const [libc::c_char]) });
        let len = (std::mem::offset_of!(libc::sockaddr_un, sun_path) + bytes.len() + 1)
            as libc::socklen_t;
        unsafe {
            assert_eq!(
                libc::bind(
                    listener_fd,
                    (&addr as *const libc::sockaddr_un).cast::<libc::sockaddr>(),
                    len
                ),
                0,
                "fixture bind(2): {:?}",
                std::io::Error::last_os_error()
            );
            assert_eq!(
                libc::listen(listener_fd, 1),
                0,
                "fixture listen(2): {:?}",
                std::io::Error::last_os_error()
            );
        }
        // Occupy the single backlog slot with a pending connection
        // held open until teardown. Proved by `probe_connect`: once
        // the queue is saturated a further connect stops completing
        // inside the window. The probe that stays blocked is itself a
        // pending connection and keeps occupying a slot.
        let filler = probe_connect(&socket, Duration::from_secs(2))
            .expect("first connect fills the backlog slot");
        let held: Arc<Mutex<Vec<UnixStream>>> = Default::default();
        held.lock().unwrap().push(filler);
        let mut saturated = probe_connect(&socket, Duration::from_secs(2)).is_none();
        for _ in 0..MAX_FILLERS {
            if saturated {
                break;
            }
            match probe_connect(&socket, Duration::from_secs(2)) {
                Some(stream) => held.lock().unwrap().push(stream),
                None => saturated = true,
            }
        }
        assert!(
            saturated,
            "fixture could not saturate a backlog-1 listen queue after {MAX_FILLERS} probes"
        );
        let held_ref = held.clone();
        spawn(move || {
            while !STOP.load(SeqCst) {
                std::thread::sleep(Duration::from_millis(20));
            }
            drop(held_ref.lock().unwrap().drain(..).collect::<Vec<_>>());
        });
        let budget = client::MetaBudget::fresh(client::MetaBudget::LIMIT);
        let outcome = bounded_call({
            let state = state.clone();
            move || {
                // Far larger than the pending connection's buffer, so
                // the write phase cannot complete while no peer ever
                // accepts or drains — the request never reaches a
                // daemon and no reply is ever supplied.
                let huge = "x".repeat(8 * 1024 * 1024);
                budget.read(&state, "agent_list", json!({"pad": huge}))
            }
        });
        let elapsed = assert_err_bounded("request against a saturated accept backlog", outcome);
        eprintln!("CAD1193 transport: saturated-backlog request refused in {elapsed:?}");
        unsafe { libc::close(listener_fd) };
    }

    // -- Stage 2: write blocked into a peer that never drains --
    // The fixture accepts but never reads, so once the client's
    // request overruns the peer's receive buffer the blocking
    // `writeln!` cannot complete. No inactivity timer covers a write;
    // only an absolute deadline bounds this. The fixture supplies no
    // bytes at all — nothing to mistake for a daemon reply.
    {
        let (_root, state) = isolated_state(artifacts, "writestall");
        let socket = client::socket_path(&state);
        let listener = UnixListener::bind(&socket).unwrap();
        spawn(move || {
            serve_until_stop(listener, |peer| {
                // Hold the connected end open without ever reading so
                // the client's send side stays backed up. Leaking the
                // peer keeps it open through process teardown; no
                // worker is needed to park it.
                std::mem::forget(peer);
            });
        });
        let budget = client::MetaBudget::fresh(client::MetaBudget::LIMIT);
        let outcome = bounded_call({
            let state = state.clone();
            move || {
                // A request far larger than one socket buffer forces
                // the client's own write phase to block against the
                // never-draining peer before any reply is possible.
                let huge = "x".repeat(8 * 1024 * 1024);
                budget.read(&state, "agent_list", json!({"pad": huge}))
            }
        });
        let elapsed = assert_err_bounded("write against a peer that never drains", outcome);
        eprintln!("CAD1193 transport: stalled write refused in {elapsed:?}");
    }

    // -- Stage 3: a reply that trickles bytes inside every inactivity
    //    window, past the absolute deadline --
    // Each connection gets a dribble of INVALID bytes, one per
    // TRICKLE_GAP — every byte resets a per-read inactivity timer, so
    // only an absolute deadline can end the call. The bytes are
    // deliberately never '{' and never newline: no complete frame, no
    // JSON object, nothing parseable as a daemon reply, success or
    // refusal. Even a client that somehow read to EOF can only reach
    // "malformed", never Ok.
    {
        let (_root, state) = isolated_state(artifacts, "trickle");
        let socket = client::socket_path(&state);
        let listener = UnixListener::bind(&socket).unwrap();
        spawn(move || {
            serve_until_stop(listener, |mut peer| {
                spawn(move || {
                    // Drain the request line so the client's write
                    // completes; the fault lives in the reply only.
                    let _ = peer.set_read_timeout(Some(Duration::from_secs(5)));
                    let mut byte = [0u8; 1];
                    loop {
                        match peer.read(&mut byte) {
                            Ok(0) | Err(_) => break,
                            Ok(_) if byte[0] == b'\n' => break,
                            Ok(_) => {}
                        }
                    }
                    let _ = peer.set_read_timeout(None);
                    // Bounded write too: a pathological client that
                    // stopped reading could otherwise park this
                    // worker on a full send buffer past teardown.
                    let _ = peer.set_write_timeout(Some(Duration::from_secs(5)));
                    while !STOP.load(SeqCst) {
                        if peer.write_all(b"z").is_err() {
                            return;
                        }
                        std::thread::sleep(TRICKLE_GAP);
                    }
                    let _ = peer.shutdown(std::net::Shutdown::Both);
                });
            });
        });
        for label in ["read", "read_private"] {
            let budget = client::MetaBudget::fresh(client::MetaBudget::LIMIT);
            let outcome = bounded_call({
                let state = state.clone();
                move || match label {
                    "read" => budget.read(&state, "daemon_info", json!({})),
                    _ => budget.read_private(&state, "daemon_info", json!({})),
                }
            });
            let elapsed =
                assert_err_bounded(&format!("trickled invalid response via {label}"), outcome);
            eprintln!("CAD1193 transport: {label} trickle refused in {elapsed:?}");
        }
    }

    // -- Stage 4: one SHARED budget across calls is one envelope --
    // After the first read consumes the budget, the next read on the
    // same MetaBudget must refuse promptly — the bound is one
    // deadline for the request's dependent reads, not fresh time per
    // call. The silent fixture proves the spending read actually
    // waited (its Err comes from the daemon's silence, not a refusal
    // the fixture supplied).
    {
        let (_root, state) = isolated_state(artifacts, "shared");
        let socket = client::socket_path(&state);
        let listener = UnixListener::bind(&socket).unwrap();
        spawn(move || {
            serve_until_stop(listener, |mut peer| {
                spawn(move || {
                    // Answer with silence: read and discard the
                    // request, then hold the connection open until
                    // teardown. No bytes ever flow back.
                    let _ = peer.set_read_timeout(Some(Duration::from_secs(2)));
                    let mut sink = [0u8; 4096];
                    while matches!(peer.read(&mut sink), Ok(n) if n > 0) {}
                    while !STOP.load(SeqCst) {
                        std::thread::sleep(Duration::from_millis(20));
                    }
                });
            });
        });
        let shared = client::MetaBudget::fresh(Duration::from_secs(2));
        let (first, first_elapsed) = bounded_call({
            let state = state.clone();
            move || shared.read(&state, "agent_list", json!({}))
        });
        assert!(first.is_err(), "silent dependency must Err, {first:?}");
        assert!(
            first_elapsed <= ABSOLUTE_BOUND,
            "silent dependency read {first_elapsed:?} exceeded {ABSOLUTE_BOUND:?}"
        );
        let start = Instant::now();
        let second = shared.read(&state, "agent_list", json!({}));
        assert!(
            second.is_err(),
            "a spent budget must not yield Ok: {second:?}"
        );
        assert!(
            start.elapsed() < PROMPT,
            "a spent shared budget must refuse promptly, not re-wait a dependency"
        );
        eprintln!(
            "CAD1193 transport: shared budget is one envelope \
             (first {first_elapsed:?}, second immediate)"
        );
    }

    STOP.store(true, SeqCst);
    release_all();
    eprintln!("CAD1193 transport acceptance complete");
}
