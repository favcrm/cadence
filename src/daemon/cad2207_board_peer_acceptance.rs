//! CAD-2207 peer-attribution acceptance — independent author.
//!
//! Review finding: `board_peer_agent` / `board_peer_operator` accepted a
//! caller-selected TCP four-tuple, so any operator could name an arbitrary
//! live socket and have the daemon resolve the process behind it — a probe of
//! unrelated connections, and with an agent on the client end a
//! classification oracle. The agreed contract is that the daemon answers only
//! the process that actually serves that connection: the RPC caller must hold
//! BOTH the listening socket bound to `server` and the accepted server-side
//! socket of the `(server, peer)` four-tuple
//! (`peer::server_socket_owned_by`).
//!
//! This file pins that contract on the real kernel, with real processes:
//!
//! 1. POSITIVE — ordinary client: the caller owns the listener and the
//!    accepted socket, and a separate OS process is an ordinary non-agent
//!    client. `board_peer_agent` must answer `{"agent": null}` (the
//!    classification the board needs) rather than refusing.
//! 2. POSITIVE — operator proof: the caller owns both ends of a self
//!    connection; `board_peer_operator` must answer `{"operator": true}`.
//! 3. NEGATIVE — foreign server: a DIFFERENT OS process owns the listener and
//!    the accepted server-side sockets; the caller owns only its own client
//!    end. Both verbs must refuse, generically. One client is an ordinary
//!    process (the caller itself), the other is a registered ordinary agent
//!    whose real pid and `/proc` start time are recorded through the store's
//!    own guarded `set_identity`; without the server-side proof
//!    `board_peer_agent` would have returned that alias. The refusal must
//!    carry no alias.
//!
//! The operator identity is the repo's standard test-seam caller assertion
//! (exactly as the other daemon fixtures run operator verbs); what is NOT
//! simulated is the kernel side: every RPC names this test process's real
//! pid and a tuple of established loopback sockets held by real child
//! processes, and the server-side ownership proof reads `/proc` for that
//! pid. The child OS processes are this test binary re-spawned in a child
//! mode (`--exact … --include-ignored`); they own or connect sockets and hold
//! them open until the parent closes their stdin. No forged pid and no
//! source-text assertions: the single pre-condition is that the production
//! `peer::tcp_peer_agent` resolves the agent-client tuple to the registered
//! alias, proving the refusals below are the ownership proof and not a dead
//! or ambiguous socket.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::{ServeOptions, Shared};
use crate::adapter::Identity;
use crate::store::NewAgent;
use crate::test_seam::{scoped, Asserted};

/// The ordinary agent the foreign-server client is registered as. Not an
/// `assist-` alias, so a missing ownership proof would return the name
/// itself — the leak this acceptance forbids.
const PROBE_ALIAS: &str = "cad2207-probe-worker";
const PROBE_GENERATION: &str = "cad2207-peer-probe-generation";

/// A libtest child prints `test <name> ... ` BEFORE the test body, so the
/// body's first `println!` shares that line. Markers are therefore searched
/// anywhere in a line, never at a fixed offset, and every wait is bounded so
/// a protocol slip fails the test instead of hanging the runner.
const CHILD_MARKER_TIMEOUT: Duration = Duration::from_secs(30);
const CHILD_EXIT_TIMEOUT: Duration = Duration::from_secs(30);

/// A real, separately owned socket set, driven by this test binary in a
/// child mode.
struct ChildLink {
    child: Child,
    mode: String,
    lines: Receiver<String>,
    reader: Option<std::thread::JoinHandle<()>>,
}

impl ChildLink {
    fn spawn(mode: &str, addr: Option<SocketAddr>) -> ChildLink {
        let exe = std::env::current_exe().expect("the test binary path");
        let name = child_test_name();
        let mut command = Command::new(exe);
        command.args([
            name.as_str(),
            "--exact",
            "--include-ignored",
            "--test-threads",
            "1",
            "--nocapture",
        ]);
        command.env("CAD2207_PEER_CHILD", mode);
        if let Some(addr) = addr {
            command.env("CAD2207_PEER_CHILD_ADDR", addr.to_string());
        }
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = crate::reaper::spawn(&mut command)
            .expect("the fixture cannot re-spawn this test binary");
        let stdout = child.stdout.take().expect("child stdout");
        let (sender, lines) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            let mut out = BufReader::new(stdout);
            loop {
                let mut line = String::new();
                match out.read_line(&mut line) {
                    Ok(0) => break,
                    Ok(_) => {
                        if sender.send(line).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        ChildLink {
            child,
            mode: mode.to_string(),
            lines,
            reader: Some(reader),
        }
    }

    fn id(&self) -> u32 {
        self.child.id()
    }

    fn timeout(&mut self, prefix: &str, seen: &[String]) -> ! {
        let _ = self.child.kill();
        panic!(
            "child '{}' did not report '{prefix}' within {CHILD_MARKER_TIMEOUT:?}; \
             last output: {seen:?}",
            self.mode
        );
    }

    /// Wait (bounded) for a line containing `prefix`, and return the text
    /// after it. The marker may share a line with libtest's `test …` header.
    fn expect_line(&mut self, prefix: &str) -> String {
        let deadline = Instant::now() + CHILD_MARKER_TIMEOUT;
        let mut seen: Vec<String> = Vec::new();
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                self.timeout(prefix, &seen);
            }
            match self.lines.recv_timeout(left) {
                Ok(line) => {
                    if let Some(index) = line.find(prefix) {
                        return line[index + prefix.len()..].trim().to_string();
                    }
                    if seen.len() == 8 {
                        seen.remove(0);
                    }
                    seen.push(line.trim_end().to_string());
                }
                Err(RecvTimeoutError::Timeout) => self.timeout(prefix, &seen),
                Err(RecvTimeoutError::Disconnected) => {
                    let _ = self.child.kill();
                    panic!(
                        "child '{}' closed stdout before '{prefix}'; last output: {seen:?}",
                        self.mode
                    );
                }
            }
        }
    }

    fn expect_addr(&mut self, prefix: &str) -> SocketAddr {
        let raw = self.expect_line(prefix);
        raw.parse()
            .unwrap_or_else(|_| panic!("child reported a malformed address: {raw}"))
    }

    /// Closing stdin is the child's stop signal; sockets it holds close with
    /// the process. The wait is bounded so a child that ignores EOF cannot
    /// hang the test.
    fn finish(mut self) {
        drop(self.child.stdin.take());
        let deadline = Instant::now() + CHILD_EXIT_TIMEOUT;
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(50));
                }
                _ => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    break;
                }
            }
        }
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

/// The libtest name of the child test in this module (crate prefix stripped,
/// exactly as libtest names unit tests).
fn child_test_name() -> String {
    let path = module_path!();
    let path = path.split_once("::").map(|(_, rest)| rest).unwrap_or(path);
    format!("{path}::peer_child")
}

/// The child process. Ignored in the ordinary suite; the parent spawns it
/// with `--exact … --include-ignored` and a mode in the environment.
#[ignore]
#[test]
fn peer_child() {
    let Ok(mode) = std::env::var("CAD2207_PEER_CHILD") else {
        return;
    };
    match mode.as_str() {
        // Owns the listener AND the accepted server-side sockets. The parent
        // is never the owner of these fds.
        "server" => {
            let listener = TcpListener::bind(("127.0.0.1", 0)).expect("child: bind");
            println!(
                "PORT {}",
                listener.local_addr().expect("child: addr").port()
            );
            std::io::stdout().flush().expect("child: flush");
            let mut held = Vec::new();
            for _ in 0..2 {
                let (socket, _) = listener.accept().expect("child: accept");
                println!(
                    "ACCEPTED {}",
                    socket.local_addr().expect("child: accepted addr")
                );
                std::io::stdout().flush().expect("child: flush");
                held.push(socket);
            }
            println!("READY");
            std::io::stdout().flush().expect("child: flush");
            block_on_stdin();
            drop(held);
            drop(listener);
        }
        // An ordinary client. Its local address is the `peer` half of the
        // tuple; the socket is held open while the parent dispatches.
        "connect" => {
            let addr: SocketAddr = std::env::var("CAD2207_PEER_CHILD_ADDR")
                .expect("child: address")
                .parse()
                .expect("child: socket address");
            let socket = TcpStream::connect(addr).expect("child: connect");
            println!("LOCAL {}", socket.local_addr().expect("child: local addr"));
            std::io::stdout().flush().expect("child: flush");
            block_on_stdin();
            drop(socket);
        }
        other => panic!("unknown child mode '{other}'"),
    }
}

fn block_on_stdin() {
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
}

fn fixture() -> (tempfile::TempDir, Arc<Shared>) {
    let dir = tempfile::Builder::new()
        .prefix("cad2207-peers-")
        .tempdir()
        .expect("temp fixture dir");
    let state = dir.path().to_path_buf();
    let pm = state.join("pm");
    crate::issue::Pm::init(&pm).expect("fixture PM");
    let options = ServeOptions::default();
    options
        .provider_env
        .set("CADENCE_PM_DIR", pm.to_str().expect("pm path"));
    let shared = Shared::new(&state, &options).expect("fixture daemon view");
    (dir, shared)
}

/// Dispatch one board-peer verb as this process (a real, un-forged pid) from
/// the operator side.
fn board_rpc(
    shared: &Arc<Shared>,
    method: &str,
    server: SocketAddr,
    peer: SocketAddr,
) -> crate::error::Result<Value> {
    let params = json!({"server": server.to_string(), "peer": peer.to_string()});
    scoped(Asserted::Operator, || {
        shared.dispatch(method, &params, std::process::id())
    })
}

/// A refusal may name no agent: no alias, no assistant family.
fn assert_generic(err: &crate::error::Error, verb: &str) {
    let text = err.to_string();
    assert!(
        !text.contains(PROBE_ALIAS),
        "{verb} leaked the client agent's alias on refusal: {text}"
    );
    assert!(
        !text.contains("assist-"),
        "{verb} leaked an assistant alias on refusal: {text}"
    );
}

#[test]
fn a_foreign_process_owning_the_server_side_cannot_be_probed() {
    let (dir, shared) = fixture();

    // ---- POSITIVE: the caller owns the server side; an ordinary non-agent
    // client is classified as no agent -------------------------------------
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("listener");
    let server = listener.local_addr().expect("listener addr");
    let mut ordinary = ChildLink::spawn("connect", Some(server));
    let peer = ordinary.expect_addr("LOCAL ");
    let (accepted, _) = listener.accept().expect("accept the ordinary client");
    let answer = board_rpc(&shared, "board_peer_agent", server, peer)
        .expect("the board that owns the server side was refused");
    assert!(
        answer["agent"].is_null(),
        "an ordinary non-agent client must classify as no agent: {answer}"
    );
    // Keep both ends alive until after the classification above.
    drop(accepted);
    drop(listener);
    ordinary.finish();

    // ---- POSITIVE: the caller truly owns both ends; the operator proof
    // runs and answers ------------------------------------------------------
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("operator listener");
    let server = listener.local_addr().expect("operator listener addr");
    let client = TcpStream::connect(server).expect("self connection");
    let self_peer = client.local_addr().expect("self peer addr");
    let (accepted, _) = listener.accept().expect("accept the self connection");
    let answer = board_rpc(&shared, "board_peer_operator", server, self_peer)
        .expect("the board that owns the server side failed its operator proof");
    assert_eq!(
        answer["operator"], true,
        "the owning board's operator proof did not answer: {answer}"
    );
    drop(accepted);
    drop(client);
    drop(listener);

    // ---- NEGATIVE: a DIFFERENT process owns the listener and the accepted
    // server-side sockets ------------------------------------------------
    let mut foreign = ChildLink::spawn("server", None);
    let port: u16 = foreign
        .expect_line("PORT ")
        .parse()
        .expect("child listener port");
    let foreign_server: SocketAddr = format!("127.0.0.1:{port}")
        .parse()
        .expect("foreign listener address");

    // The caller's own client end of a connection it does NOT serve.
    let caller_client = TcpStream::connect(foreign_server).expect("caller client");
    let caller_peer = caller_client.local_addr().expect("caller peer");

    // A registered ordinary agent on the client end: without the ownership
    // proof, this tuple resolves to PROBE_ALIAS.
    let mut agent_client = ChildLink::spawn("connect", Some(foreign_server));
    let agent_peer = agent_client.expect_addr("LOCAL ");
    foreign.expect_line("ACCEPTED ");
    foreign.expect_line("ACCEPTED ");
    foreign.expect_line("READY");

    let agent_pid = agent_client.id();
    shared
        .store
        .register_agent(&NewAgent {
            alias: PROBE_ALIAS,
            provider: "pi",
            endpoint_kind: "managed",
            role: "worker",
            cwd: dir.path().to_str().expect("fixture path"),
            sandbox: "read-only",
            instructions: None,
            params: None,
            team_role: None,
            model_policy: None,
        })
        .expect("fixture cannot register the probe agent");
    shared
        .store
        .set_identity(
            PROBE_ALIAS,
            &Identity {
                thread_id: String::new(),
                session_id: String::new(),
                model: None,
                effort: None,
                pid: agent_pid,
                endpoint: None,
                generation: Some(PROBE_GENERATION.to_string()),
                attach: None,
            },
        )
        .expect("fixture cannot record the probe agent's real process");

    // Pre-condition, on the same production resolver the RPC uses: the
    // tuple is live and its agent client resolves. The refusals below are
    // therefore the server-side ownership proof, not a dead socket.
    let start = crate::peer::proc_starttime(agent_pid).expect("probe /proc start");
    let roots = crate::peer::AgentRoots {
        managed: crate::peer::AgentPids::classify([(
            PROBE_ALIAS.to_string(),
            agent_pid,
            Some(start),
        )]),
        ..Default::default()
    };
    assert_eq!(
        crate::peer::tcp_peer_agent(foreign_server, agent_peer, &roots)
            .expect("the tuple lookup must not be ambiguous"),
        Some(PROBE_ALIAS.to_string()),
        "the established tuple does not resolve to the registered agent client"
    );

    // The caller owns only its own client end. Both verbs must refuse.
    for method in ["board_peer_agent", "board_peer_operator"] {
        let err = board_rpc(&shared, method, foreign_server, caller_peer)
            .expect_err("a caller that does not own the server side was served");
        assert_generic(&err, method);
    }

    // The same foreign server with a registered agent on the client end:
    // still refused, and the alias never crosses the wire.
    for method in ["board_peer_agent", "board_peer_operator"] {
        let err = board_rpc(&shared, method, foreign_server, agent_peer)
            .expect_err("a caller that does not own the server side learned the agent");
        assert_generic(&err, method);
    }

    drop(caller_client);
    foreign.finish();
    agent_client.finish();
}
