//! JSON-RPC over a WebSocket to a managed `codex app-server --listen
//! ws://127.0.0.1:PORT` child — the same wire protocol as stdio, over a
//! loopback endpoint an official Codex TUI can attach to with
//! `codex resume --remote`.
//!
//! The adapter owns only the process group it creates and binds loopback
//! only: the endpoint is discoverable through the agent record (private
//! state dir), never published elsewhere. A dead transport flips
//! `disconnected` and resolves every pending request with
//! `OutcomeUnknown` — callers must not retry blindly.

use std::net::{TcpListener, TcpStream};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tungstenite::protocol::Role;
use tungstenite::{Message, WebSocket};

use super::link::{DisconnectHook, Incoming, MessageHandler, Pending};
use crate::error::{Error, Result};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(35);
const CONNECT_DEADLINE: Duration = Duration::from_secs(10);

/// What a successful `launch` produced.
pub struct Launch {
    pub pid: u32,
    /// `ws://127.0.0.1:PORT` the official TUI attaches to.
    pub endpoint: String,
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
    writer: Option<WebSocket<TcpStream>>,
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
                writer: None,
            }),
            pending: Pending::new(),
            disconnected: AtomicBool::new(false),
            endpoint: Mutex::new(None),
        })
    }

    /// Reserve a loopback port, spawn `cmd --listen ws://127.0.0.1:PORT`
    /// in its own process group, then connect and finish the WebSocket
    /// handshake. The pre-bind/drop leaves a small reclaim race, bounded
    /// to loopback and detected by the connect retry loop.
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
        let mut child = command.spawn()?;
        let pid = child.id();

        let ws = match self.connect(&url, &mut child) {
            Ok(ws) => ws,
            Err(e) => {
                Self::kill_child(&mut child);
                return Err(e);
            }
        };
        // Split the socket: the reader thread keeps the post-handshake
        // WebSocket; a raw-socket clone becomes the writer half. TCP is
        // full-duplex, so both halves may write concurrently.
        let write_stream = ws
            .get_ref()
            .try_clone()
            .map_err(|e| Error::internal(format!("endpoint clone failed: {e}")))?;
        let writer = WebSocket::from_raw_socket(write_stream, Role::Client, None);
        {
            let mut inner = self.inner.lock().unwrap();
            inner.writer = Some(writer);
            inner.child = Some(child);
        }
        *self.endpoint.lock().unwrap() = Some(url.clone());
        let adapter = Arc::clone(self);
        thread::spawn(move || adapter.read_loop(ws));
        Ok(Launch { pid, endpoint: url })
    }

    /// Retry connects until the app-server accepts; bail early if the
    /// child already exited (bad binary, port lost to the reclaim race).
    fn connect(&self, url: &str, child: &mut Child) -> Result<WebSocket<TcpStream>> {
        let deadline = Instant::now() + CONNECT_DEADLINE;
        loop {
            if let Some(status) = child.try_wait().ok().flatten() {
                return Err(Error::provider(format!(
                    "app-server exited before accepting (status {status}); see provider log"
                )));
            }
            let connected = url
                .strip_prefix("ws://")
                .and_then(|addr| TcpStream::connect(addr).ok())
                .and_then(|stream| tungstenite::client(url, stream).map(|(ws, _)| ws).ok());
            match connected {
                Some(ws) => return Ok(ws),
                None if Instant::now() < deadline => thread::sleep(Duration::from_millis(50)),
                None => {
                    return Err(Error::provider(
                        "app-server endpoint did not accept connections; see provider log",
                    ))
                }
            }
        }
    }

    fn read_loop(self: Arc<Self>, mut ws: WebSocket<TcpStream>) {
        loop {
            let text = match ws.read() {
                Ok(Message::Text(text)) => text.to_string(),
                Ok(Message::Close(_)) | Err(_) => break,
                // Pings are answered by tungstenite; binary is unused.
                _ => continue,
            };
            let Ok(message) = serde_json::from_str::<Value>(&text) else {
                continue;
            };
            if message.get("method").is_none() {
                self.pending.resolve(&message);
                continue;
            }
            let method = message["method"].as_str().unwrap_or_default().to_string();
            let params = message.get("params").cloned().unwrap_or(Value::Null);
            if let Some(id) = message.get("id").cloned() {
                (self.on_message)(Incoming::Request { id, method, params });
            } else {
                (self.on_message)(Incoming::Notification { method, params });
            }
        }
        self.disconnected.store(true, Ordering::SeqCst);
        self.pending.fail_all("Provider connection lost");
        (self.on_disconnect)();
    }

    /// Write one outbound frame. A write failure means the request may or
    /// may not have reached the provider — `OutcomeUnknown`, not a retry.
    pub fn send(&self, message: Value) -> Result<()> {
        if self.disconnected.load(Ordering::SeqCst) {
            return Err(Error::unknown("Provider connection is closed"));
        }
        let mut inner = self.inner.lock().unwrap();
        let writer = inner
            .writer
            .as_mut()
            .ok_or_else(|| Error::unknown("Provider connection is closed"))?;
        writer
            .send(Message::Text(message.to_string().into()))
            .map_err(|e| Error::unknown(format!("Provider connection closed while writing: {e}")))
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

    /// Graceful close frame, socket shutdown (unblocks the reader), then
    /// kill the provider process group — scoped to our own group only.
    pub fn close(&self) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(mut writer) = inner.writer.take() {
            let _ = writer.send(Message::Close(None));
            let _ = writer.get_ref().shutdown(std::net::Shutdown::Both);
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
    }
}
