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
//! The codec is hand-rolled: ONE `Mutex<TcpStream>` writer serializes
//! every outbound frame — requests, pongs and close replies — so no two
//! WebSocket state machines ever write the same socket concurrently.
//! Connect, handshake, writes and close are all bounded by socket
//! timeouts; a dead transport flips `disconnected` and resolves every
//! pending request with `OutcomeUnknown` — callers must not retry
//! blindly.

use std::io::{BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::link::{DisconnectHook, Incoming, MessageHandler, Pending};
use crate::error::{Error, Result};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(35);
const CONNECT_DEADLINE: Duration = Duration::from_secs(10);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);

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
    writer: Option<TcpStream>,
}

/// The one serialized write path for every outbound frame.
fn write_frame(stream: &mut TcpStream, opcode: u8, payload: &[u8]) -> Result<()> {
    let mut frame = Vec::with_capacity(payload.len() + 14);
    frame.push(0x80 | opcode);
    let mask = uuid::Uuid::new_v4().as_bytes()[..4].to_vec();
    let n = payload.len();
    if n < 126 {
        frame.push(0x80 | n as u8);
    } else if n < 65536 {
        frame.push(0x80 | 126);
        frame.extend_from_slice(&(n as u16).to_be_bytes());
    } else {
        frame.push(0x80 | 127);
        frame.extend_from_slice(&(n as u64).to_be_bytes());
    }
    frame.extend_from_slice(&mask);
    frame.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
    stream
        .write_all(&frame)
        .and_then(|()| stream.flush())
        .map_err(|e| Error::unknown(format!("Provider connection closed while writing: {e}")))
}

/// Read one frame. Returns `Ok(None)` on clean EOF. Server frames are
/// normally unmasked; a masked one is tolerated and unmasked anyway.
/// Returns `(fin, opcode, payload)`.
fn read_frame(reader: &mut BufReader<TcpStream>) -> Result<Option<(bool, u8, Vec<u8>)>> {
    let mut hdr = [0u8; 2];
    match reader.read_exact(&mut hdr) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(Error::unknown(format!("Provider connection lost: {e}"))),
    }
    let (fin, opcode, flags) = (hdr[0] & 0x80 != 0, hdr[0] & 0x0F, hdr[1]);
    let mut length = (flags & 0x7F) as u64;
    if length == 126 {
        let mut ext = [0u8; 2];
        reader
            .read_exact(&mut ext)
            .map_err(|e| Error::unknown(format!("frame truncated: {e}")))?;
        length = u16::from_be_bytes(ext) as u64;
    } else if length == 127 {
        let mut ext = [0u8; 8];
        reader
            .read_exact(&mut ext)
            .map_err(|e| Error::unknown(format!("frame truncated: {e}")))?;
        length = u64::from_be_bytes(ext);
    }
    if length > 8 * 1024 * 1024 {
        return Err(Error::unknown("Provider frame exceeds 8MiB"));
    }
    let mask = if flags & 0x80 != 0 {
        let mut m = [0u8; 4];
        reader
            .read_exact(&mut m)
            .map_err(|e| Error::unknown(format!("frame truncated: {e}")))?;
        Some(m)
    } else {
        None
    };
    let mut payload = vec![0u8; length as usize];
    reader
        .read_exact(&mut payload)
        .map_err(|e| Error::unknown(format!("frame truncated: {e}")))?;
    if let Some(mask) = mask {
        for (i, b) in payload.iter_mut().enumerate() {
            *b ^= mask[i % 4];
        }
    }
    Ok(Some((fin, opcode, payload)))
}

/// RFC 6455 client upgrade. The key need not be random on a loopback
/// endpoint we spawned ourselves; a fixed valid key keeps this
/// dependency-free. We verify status 101 + Upgrade: websocket only.
/// Headers are read byte-wise — no buffer — so nothing past `\r\n\r\n`
/// (e.g. a frame sent immediately after upgrade) can be swallowed.
fn ws_handshake(stream: &mut TcpStream, url: &str) -> Result<()> {
    let host = url.strip_prefix("ws://").unwrap_or(url);
    let request = format!(
        "GET / HTTP/1.1\r\nHost: {host}\r\nUpgrade: websocket\r\n\
         Connection: Upgrade\r\nSec-WebSocket-Key: Y2FkZW5jZS1rZXk=\r\n\
         Sec-WebSocket-Version: 13\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|e| Error::provider(format!("handshake write failed: {e}")))?;
    let mut headers = Vec::with_capacity(256);
    let mut byte = [0u8; 1];
    loop {
        stream
            .read_exact(&mut byte)
            .map_err(|e| Error::provider(format!("handshake read failed: {e}")))?;
        headers.push(byte[0]);
        if headers.ends_with(b"\r\n\r\n") {
            break;
        }
        if headers.len() > 8192 {
            return Err(Error::provider("handshake response exceeds 8KiB"));
        }
    }
    let headers = String::from_utf8_lossy(&headers);
    let mut lines = headers.split("\r\n");
    let status = lines.next().unwrap_or_default();
    if !status.contains(" 101") {
        return Err(Error::provider(format!(
            "endpoint refused upgrade: {}",
            status.trim()
        )));
    }
    if !lines.any(|l| l.eq_ignore_ascii_case("upgrade: websocket")) {
        return Err(Error::provider(
            "endpoint did not confirm websocket upgrade",
        ));
    }
    Ok(())
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
        let child = command.spawn()?;
        let pid = child.id();
        self.inner.lock().unwrap().child = Some(child);

        let stream = match self.connect(&url) {
            Ok(stream) => stream,
            Err(e) => {
                self.kill_owned_child();
                return Err(e);
            }
        };
        let reader_stream = match stream.try_clone() {
            Ok(clone) => clone,
            Err(e) => {
                self.kill_owned_child();
                return Err(Error::internal(format!("endpoint clone failed: {e}")));
            }
        };
        self.inner.lock().unwrap().writer = Some(stream);
        *self.endpoint.lock().unwrap() = Some(url.clone());
        let adapter = Arc::clone(self);
        thread::spawn(move || adapter.read_loop(reader_stream));
        Ok(Launch { pid, endpoint: url })
    }

    /// Retry connect+handshake until the app-server accepts, the
    /// deadline passes, the child exits — or `close` took it.
    fn connect(&self, url: &str) -> Result<TcpStream> {
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
            let attempt = (|| -> Result<TcpStream> {
                let mut stream = TcpStream::connect_timeout(&addr, Duration::from_millis(500))
                    .map_err(|e| Error::provider(format!("connect failed: {e}")))?;
                let _ = stream.set_nodelay(true);
                let _ = stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT));
                let _ = stream.set_write_timeout(Some(WRITE_TIMEOUT));
                ws_handshake(&mut stream, url)?;
                // The reader must block indefinitely; writes stay
                // bounded for the socket's lifetime.
                let _ = stream.set_read_timeout(None);
                Ok(stream)
            })();
            match attempt {
                Ok(stream) => return Ok(stream),
                Err(e) if Instant::now() >= deadline => return Err(e),
                Err(_) => thread::sleep(Duration::from_millis(50)),
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

    fn read_loop(self: Arc<Self>, stream: TcpStream) {
        let mut reader = BufReader::new(stream);
        let mut fragment: Option<Vec<u8>> = None;
        loop {
            match read_frame(&mut reader) {
                Ok(Some((fin, 0x0, payload))) => {
                    if let Some(buf) = fragment.as_mut() {
                        buf.extend_from_slice(&payload);
                        if fin {
                            let text = std::mem::take(buf);
                            fragment = None;
                            self.dispatch_frame(&text);
                        }
                    }
                }
                Ok(Some((fin, 0x1, payload))) => {
                    if fin {
                        self.dispatch_frame(&payload);
                    } else {
                        fragment = Some(payload);
                    }
                }
                Ok(Some((_, 0x8, _))) => {
                    let _ = self.write_frame(0x8, &[]);
                    break;
                }
                Ok(Some((_, 0x9, payload))) => {
                    let _ = self.write_frame(0xA, &payload);
                }
                // pong, binary, or unknown opcodes carry no protocol data.
                Ok(Some(_)) => {}
                Ok(None) | Err(_) => break,
            }
        }
        self.mark_dead();
    }

    fn dispatch_frame(&self, payload: &[u8]) {
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

    /// One serialized outbound frame through the shared writer.
    fn write_frame(&self, opcode: u8, payload: &[u8]) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        let writer = inner
            .writer
            .as_mut()
            .ok_or_else(|| Error::unknown("Provider connection is closed"))?;
        write_frame(writer, opcode, payload)
    }

    /// Connection is gone: flip the flag once, fail every waiter, wake
    /// protocol-level waiters. Idempotent across reader/close races.
    fn mark_dead(&self) {
        if self.disconnected.swap(true, Ordering::SeqCst) {
            return;
        }
        self.pending.fail_all("Provider connection lost");
        (self.on_disconnect)();
    }

    /// Write one outbound JSON frame. A write failure means the request
    /// may or may not have reached the provider — `OutcomeUnknown`.
    pub fn send(&self, message: Value) -> Result<()> {
        if self.disconnected.load(Ordering::SeqCst) {
            return Err(Error::unknown("Provider connection is closed"));
        }
        let payload = message.to_string();
        self.write_frame(0x1, payload.as_bytes())
            .inspect_err(|_| self.mark_dead())
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
    /// Safe to call while `connect` is still running: it kills the
    /// published child, which the connect loop observes.
    pub fn close(&self) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(mut writer) = inner.writer.take() {
            let _ = write_frame(&mut writer, 0x8, &[]);
            let _ = writer.shutdown(std::net::Shutdown::Both);
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
