//! JSON-RPC over a child's stdio pipes — one newline-delimited message per
//! line, matching the Codex app-server / ACP wire style.
//!
//! The adapter owns only the process group it creates. A dead transport
//! flips `disconnected` and resolves every pending request with
//! `OutcomeUnknown` — callers must not retry blindly.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::process::CommandExt;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde_json::{json, Value};

use crate::error::{Error, Result};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(35);

/// A decoded inbound frame that is not a response to one of our requests.
pub enum Incoming {
    Notification {
        method: String,
        params: Value,
    },
    Request {
        id: Value,
        method: String,
        params: Value,
    },
}

type MessageHandler = Box<dyn Fn(Incoming) + Send + Sync>;

pub struct StdioAdapter {
    command: Vec<String>,
    env_scrub: Vec<String>,
    on_message: MessageHandler,
    inner: Mutex<Inner>,
    pending: Mutex<HashMap<u64, mpsc::Sender<Result<Value>>>>,
    next_id: AtomicU64,
    disconnected: AtomicBool,
}

struct Inner {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
}

impl StdioAdapter {
    pub fn new(command: &[&str], env_scrub: &[&str], on_message: MessageHandler) -> Arc<Self> {
        Arc::new(Self {
            command: command.iter().map(|s| s.to_string()).collect(),
            env_scrub: env_scrub.iter().map(|s| s.to_string()).collect(),
            on_message,
            inner: Mutex::new(Inner {
                child: None,
                stdin: None,
            }),
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(0),
            disconnected: AtomicBool::new(false),
        })
    }

    /// Spawn the provider in its own process group and start the reader
    /// thread. Environment identity variables are scrubbed so the child
    /// cannot inherit a parent conversation's identity. Returns the pid.
    pub fn launch(self: &Arc<Self>, cwd: &str, stderr_log: &std::path::Path) -> Result<u32> {
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(stderr_log)?;
        let mut command = Command::new(&self.command[0]);
        command
            .args(&self.command[1..])
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
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
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| Error::internal("provider stdin unavailable"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::internal("provider stdout unavailable"))?;
        {
            let mut inner = self.inner.lock().unwrap();
            inner.stdin = Some(stdin);
            inner.child = Some(child);
        }
        let adapter = Arc::clone(self);
        thread::spawn(move || adapter.read_loop(stdout));
        Ok(pid)
    }

    fn read_loop(self: Arc<Self>, stdout: ChildStdout) {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            let Ok(message) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if message.get("method").is_none() {
                // Response to one of our requests.
                if let Some(id) = message.get("id").and_then(Value::as_u64) {
                    let target = self.pending.lock().unwrap().remove(&id);
                    if let Some(target) = target {
                        let _ = target.send(Ok(message));
                    }
                }
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
        for (_, target) in self.pending.lock().unwrap().drain() {
            let _ = target.send(Err(Error::unknown("Provider process disconnected")));
        }
    }

    /// Write one outbound frame. A write failure means the request may or
    /// may not have reached the provider — `OutcomeUnknown`, not a retry.
    pub fn send(&self, message: Value) -> Result<()> {
        if self.disconnected.load(Ordering::SeqCst) {
            return Err(Error::unknown("Provider connection is closed"));
        }
        let mut inner = self.inner.lock().unwrap();
        let stdin = inner
            .stdin
            .as_mut()
            .ok_or_else(|| Error::unknown("Provider connection is closed"))?;
        let mut payload = message.to_string();
        payload.push('\n');
        stdin
            .write_all(payload.as_bytes())
            .and_then(|()| stdin.flush())
            .map_err(|e| Error::unknown(format!("Provider connection closed while writing: {e}")))
    }

    /// JSON-RPC request/response with a bounded wait. A timeout is
    /// `OutcomeUnknown`: the request may have been delivered.
    pub fn request(&self, method: &str, params: Value) -> Result<Value> {
        self.request_timeout(method, params, REQUEST_TIMEOUT)
    }

    pub fn request_timeout(&self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        let request_id = self.next_id.fetch_add(1, Ordering::SeqCst) + 1;
        let (tx, rx) = mpsc::channel();
        self.pending.lock().unwrap().insert(request_id, tx);
        let outcome = self.send(json!({"id": request_id, "method": method, "params": params}));
        let result = match outcome {
            Err(e) => Err(e),
            Ok(()) => match rx.recv_timeout(timeout) {
                Ok(inner) => inner,
                Err(mpsc::RecvTimeoutError::Timeout) => Err(Error::unknown(format!(
                    "No response to {method}; do not blindly retry"
                ))),
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    Err(Error::unknown("Provider process disconnected"))
                }
            },
        };
        self.pending.lock().unwrap().remove(&request_id);
        let response = result?;
        if let Some(error) = response.get("error") {
            return Err(Error::provider(error.to_string()));
        }
        Ok(response.get("result").cloned().unwrap_or(Value::Null))
    }

    /// Respond to a provider-initiated request (approval, user input).
    pub fn respond(&self, request_id: &Value, result: Value) -> Result<()> {
        self.send(json!({"id": request_id, "result": result}))
    }

    pub fn pid(&self) -> Option<u32> {
        self.inner.lock().unwrap().child.as_ref().map(Child::id)
    }

    pub fn disconnected(&self) -> bool {
        self.disconnected.load(Ordering::SeqCst)
    }

    /// Graceful terminate, then kill — scoped to this adapter's own process
    /// group only.
    pub fn close(&self) {
        let mut inner = self.inner.lock().unwrap();
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
                    std::thread::sleep(Duration::from_millis(100));
                }
                if child.try_wait().ok().flatten().is_none() {
                    unsafe {
                        libc::kill(pgid, libc::SIGKILL);
                    }
                    let _ = child.wait();
                }
            }
        }
        inner.stdin.take();
    }
}
