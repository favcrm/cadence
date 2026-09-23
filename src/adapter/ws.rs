//! JSON-RPC over a WebSocket to a managed `codex app-server --listen
//! ws://127.0.0.1:PORT` child — the same wire protocol as stdio, over a
//! loopback endpoint an official Codex TUI can attach to with
//! `codex resume --remote`.
//!
//! Ownership and trust boundary, stated plainly: the adapter owns only
//! the process group it spawns and the endpoint is loopback-only, but an
//! unauthenticated `ws://127.0.0.1` port is reachable by ANY local user —
//! SO_PEERCRED does not apply here, and the port is discoverable via
//! `ss`. The URL is recorded in the agent record (private 0700 state
//! dir); nothing else publishes it. Do not treat the endpoint as
//! same-UID isolated.
//!
//! The wire is tungstenite (vetted: correct nonces, accept-hash
//! verification, fragmentation, control frames). Exactly ONE owner — the
//! I/O thread — touches the `WebSocket`; outbound payloads travel over a
//! channel it drains between bounded reads, so requests, pongs and close
//! replies can never interleave on the socket. Connect, handshake,
//! writes and close are bounded by socket timeouts plus an absolute
//! connect deadline (a drip-feeding peer cannot stall startup: the
//! upgrade runs in a helper thread and is abandoned by shutting down the
//! socket). A dead transport flips `disconnected` and resolves every
//! pending request with `OutcomeUnknown` — callers must not retry
//! blindly.

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tungstenite::{Message, WebSocket};

use super::link::{DisconnectHook, Incoming, MessageHandler, Pending};
use crate::error::{Error, Result};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(35);
const CONNECT_DEADLINE: Duration = Duration::from_secs(10);
const HANDSHAKE_BOUND: Duration = Duration::from_secs(5);
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);
/// The I/O loop's read tick: also the upper bound on send latency.
const READ_POLL: Duration = Duration::from_millis(50);

/// What a successful `launch` produced.
pub struct Launch {
    pub pid: u32,
    /// `ws://127.0.0.1:PORT` the official TUI attaches to.
    pub endpoint: String,
}

type Ws = WebSocket<TcpStream>;

/// Outbound work for the single I/O owner.
enum Out {
    Text(String),
    Close,
}

pub struct WsAdapter {
    /// Command prefix; `--listen <url>` is appended at launch.
    command: Vec<String>,
    env_scrub: Vec<String>,
    on_message: MessageHandler,
    on_disconnect: DisconnectHook,
    inner: Mutex<Inner>,
    pending: Pending,
    disconnected: AtomicBool,
    endpoint: Mutex<Option<String>>,
}

struct Inner {
    child: Option<Child>,
    /// Queue to the I/O owner thread.
    tx: Option<Sender<Out>>,
    /// A socket clone kept only to force-shutdown the connection.
    killer: Option<TcpStream>,
}

impl WsAdapter {
    pub fn new(
        command: &[String],
        env_scrub: &[&str],
        on_message: MessageHandler,
        on_disconnect: DisconnectHook,
    ) -> Arc<Self> {
        Arc::new(Self {
            command: command.to_vec(),
            env_scrub: env_scrub.iter().map(|s| s.to_string()).collect(),
            on_message,
            on_disconnect,
            inner: Mutex::new(Inner {
                child: None,
                tx: None,
                killer: None,
            }),
            pending: Pending::new(),
            disconnected: AtomicBool::new(false),
            endpoint: Mutex::new(None),
        })
    }

    /// Reserve a loopback port, spawn `cmd --listen ws://127.0.0.1:PORT`
    /// in its own process group, then connect and finish the WebSocket
    /// handshake. The child is published to `inner` BEFORE connecting so
    /// `close` can kill a provider stuck in setup; every post-spawn
    /// failure kills it. The pre-bind/drop leaves a small reclaim race,
    /// bounded to loopback and detected by the connect loop.
    pub fn launch(self: &Arc<Self>, cwd: &str, stderr_log: &std::path::Path) -> Result<Launch> {
        let port = {
            let listener = TcpListener::bind(("127.0.0.1", 0))?;
            listener.local_addr()?.port()
        };
        let url = format!("ws://127.0.0.1:{port}");
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(stderr_log)?;
        let mut command = Command::new(&self.command[0]);
        command
            .args(&self.command[1..])
            .args(["--listen", &url])
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log));
        for name in &self.env_scrub {
            command.env_remove(name);
        }
        // Own process group: signals reach only this provider.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = crate::reaper::spawn(&mut command)?;
        let pid = child.id();
        self.inner.lock().unwrap().child = Some(child);

        match self.connect(&url) {
            Ok((ws, killer)) => {
                let (tx, rx) = channel();
                let mut inner = self.inner.lock().unwrap();
                inner.tx = Some(tx);
                inner.killer = Some(killer);
                drop(inner);
                *self.endpoint.lock().unwrap() = Some(url.clone());
                let adapter = Arc::clone(self);
                thread::spawn(move || adapter.io_loop(ws, rx));
                Ok(Launch { pid, endpoint: url })
            }
            Err(e) => {
                self.kill_owned_child();
                Err(e)
            }
        }
    }

    /// Retry connect+handshake until the app-server accepts, the
    /// absolute deadline passes, the child exits — or `close` took it.
    /// Every attempt is wall-clock bounded: the upgrade runs in a
    /// helper thread and the socket is force-shutdown on timeout, so a
    /// drip-feeding peer cannot hold startup past `CONNECT_DEADLINE`.
    fn connect(&self, url: &str) -> Result<(Ws, TcpStream)> {
        let addr: SocketAddr = url
            .strip_prefix("ws://")
            .and_then(|a| a.parse().ok())
            .ok_or_else(|| Error::internal("endpoint address did not parse"))?;
        let deadline = Instant::now() + CONNECT_DEADLINE;
        loop {
            {
                let mut inner = self.inner.lock().unwrap();
                match inner.child.as_mut() {
                    None => {
                        return Err(Error::unknown("endpoint closed during setup"));
                    }
                    Some(child) => {
                        if child.try_wait().ok().flatten().is_some() {
                            return Err(Error::provider(
                                "app-server exited before accepting; see provider log",
                            ));
                        }
                    }
                }
            }
            match self.try_connect(&addr, url) {
                Ok(pair) => return Ok(pair),
                Err(e) if Instant::now() >= deadline => return Err(e),
                Err(_) => thread::sleep(Duration::from_millis(50)),
            }
        }
    }

    /// One bounded attempt: TCP connect with timeout, then the upgrade
    /// in a helper thread joined via `recv_timeout` — per-socket read
    /// timeouts alone cannot bound a drip-fed response.
    fn try_connect(&self, addr: &SocketAddr, url: &str) -> Result<(Ws, TcpStream)> {
        let stream = TcpStream::connect_timeout(addr, Duration::from_millis(500))
            .map_err(|e| Error::provider(format!("connect failed: {e}")))?;
        stream.set_nodelay(true)?;
        stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
        // Kept to force-shutdown a stuck handshake and later by close().
        let killer = stream
            .try_clone()
            .map_err(|e| Error::internal(format!("endpoint clone failed: {e}")))?;
        let (tx, rx) = channel();
        let url = url.to_string();
        thread::spawn(move || {
            let _ = tx.send(tungstenite::client(url, stream).map(|(ws, _)| ws));
        });
        match rx.recv_timeout(HANDSHAKE_BOUND) {
            Ok(Ok(ws)) => {
                // Bounded read tick lets the I/O owner drain outbound
                // work; writes remain bounded by the socket timeout.
                ws.get_ref().set_read_timeout(Some(READ_POLL))?;
                Ok((ws, killer))
            }
            Ok(Err(e)) => Err(Error::provider(format!("handshake failed: {e}"))),
            Err(_) => {
                let _ = killer.shutdown(std::net::Shutdown::Both);
                Err(Error::provider("handshake exceeded its deadline"))
            }
        }
    }

    /// Kill the published child if it is still ours; used on every
    /// post-spawn failure path so no provider is left behind.
    fn kill_owned_child(&self) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(mut child) = inner.child.take() {
            Self::kill_child(&mut child);
        }
    }

    /// The single I/O owner: drains outbound frames, reads inbound
    /// frames, lets tungstenite handle ping/pong/close/fragmentation.
    /// A read timeout is the poll tick, not an error.
    fn io_loop(self: Arc<Self>, mut ws: Ws, rx: Receiver<Out>) {
        loop {
            if self.disconnected.load(Ordering::SeqCst) {
                break;
            }
            while let Ok(out) = rx.try_recv() {
                let failed = match out {
                    Out::Text(payload) => ws
                        .send(Message::Text(payload.into()))
                        .map_err(|e| e.to_string())
                        .is_err(),
                    Out::Close => ws.close(None).map_err(|e| e.to_string()).is_err(),
                };
                if failed {
                    self.mark_dead();
                    return;
                }
            }
            match ws.read() {
                Ok(Message::Text(text)) => self.dispatch(text.as_bytes()),
                Ok(Message::Binary(payload)) => self.dispatch(&payload),
                Ok(Message::Close(_)) => break,
                // Ping/Pong/Frame: tungstenite queues pongs internally
                // and flushes them on the next operation.
                Ok(_) => {}
                Err(tungstenite::Error::Io(e))
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {}
                Err(_) => break,
            }
        }
        self.mark_dead();
    }

    fn dispatch(&self, payload: &[u8]) {
        let Ok(message) = serde_json::from_slice::<Value>(payload) else {
            return;
        };
        if message.get("method").is_none() {
            self.pending.resolve(&message);
            return;
        }
        let method = message["method"].as_str().unwrap_or_default().to_string();
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        if let Some(id) = message.get("id").cloned() {
            (self.on_message)(Incoming::Request { id, method, params });
        } else {
            (self.on_message)(Incoming::Notification { method, params });
        }
    }

    /// Connection is gone: flip the flag once, fail every waiter, wake
    /// protocol-level waiters. Idempotent across I/O/close races.
    fn mark_dead(&self) {
        if self.disconnected.swap(true, Ordering::SeqCst) {
            return;
        }
        self.pending.fail_all("Provider connection lost");
        (self.on_disconnect)();
    }

    /// Queue one outbound JSON frame with the I/O owner. A queue
    /// failure means the connection is already dead — `OutcomeUnknown`.
    pub fn send(&self, message: Value) -> Result<()> {
        if self.disconnected.load(Ordering::SeqCst) {
            return Err(Error::unknown("Provider connection is closed"));
        }
        let tx = self
            .inner
            .lock()
            .unwrap()
            .tx
            .clone()
            .ok_or_else(|| Error::unknown("Provider connection is closed"))?;
        tx.send(Out::Text(message.to_string())).map_err(|_| {
            self.mark_dead();
            Error::unknown("Provider connection is closed")
        })
    }

    /// JSON-RPC request/response with a bounded wait. A timeout is
    /// `OutcomeUnknown`: the request may have been delivered.
    pub fn request(&self, method: &str, params: Value) -> Result<Value> {
        self.request_timeout(method, params, REQUEST_TIMEOUT)
    }

    pub fn request_timeout(&self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        self.pending
            .request(|msg| self.send(msg), method, params, timeout)
    }

    /// Respond to a provider-initiated request (approval, user input).
    pub fn respond(&self, request_id: &Value, result: Value) -> Result<()> {
        self.send(json!({"id": request_id, "result": result}))
    }

    pub fn pid(&self) -> Option<u32> {
        self.inner.lock().unwrap().child.as_ref().map(Child::id)
    }

    pub fn endpoint(&self) -> Option<String> {
        self.endpoint.lock().unwrap().clone()
    }

    pub fn disconnected(&self) -> bool {
        self.disconnected.load(Ordering::SeqCst)
    }

    fn kill_child(child: &mut Child) {
        if child.try_wait().ok().flatten().is_none() {
            let pgid = -(child.id() as i32);
            unsafe {
                libc::kill(pgid, libc::SIGKILL);
            }
            let _ = child.wait();
        }
    }

    /// Best-effort close frame, socket shutdown (unblocks the I/O
    /// loop), then kill the provider process group — scoped to our own
    /// group only. Safe while `connect` is still running: it kills the
    /// published child, which the connect loop observes.
    pub fn close(&self) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(tx) = inner.tx.take() {
            let _ = tx.send(Out::Close);
        }
        if let Some(killer) = inner.killer.take() {
            let _ = killer.shutdown(std::net::Shutdown::Both);
        }
        if let Some(mut child) = inner.child.take() {
            if child.try_wait().ok().flatten().is_none() {
                let pgid = -(child.id() as i32);
                unsafe {
                    libc::kill(pgid, libc::SIGTERM);
                }
                for _ in 0..50 {
                    if child.try_wait().ok().flatten().is_some() {
                        break;
                    }
                    thread::sleep(Duration::from_millis(100));
                }
                if child.try_wait().ok().flatten().is_none() {
                    unsafe {
                        libc::kill(pgid, libc::SIGKILL);
                    }
                    let _ = child.wait();
                }
            }
        }
        drop(inner);
        self.mark_dead();
    }
}
