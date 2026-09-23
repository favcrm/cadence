//! JSON-RPC over a child's stdio pipes — one newline-delimited message per
//! line, matching the Codex app-server / ACP wire style.
//!
//! `new_lines` selects a raw event-stream mode for providers whose wire
//! is newline-delimited JSON but not JSON-RPC (Claude `stream-json`):
//! every parsed line is dispatched as a notification whose method is the
//! line's `type` field, and there is no request/response correlation.
//!
//! The adapter owns only the process group it creates. A dead transport
//! flips `disconnected` and resolves every pending request with
//! `OutcomeUnknown` — callers must not retry blindly.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::process::CommandExt;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde_json::{json, Value};

use super::link::{DisconnectHook, Incoming, MessageHandler, Pending};
use crate::error::{Error, Result};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(35);

/// Environment scrubbing applied to the child at launch. `names` are
/// removed verbatim; every inherited variable whose name starts with a
/// `prefixes` entry is removed unless it appears in `keep` — the
/// keep-list is operator-set configuration that must survive (e.g.
/// `CLAUDE_CONFIG_DIR`). Prefix scrubbing enumerates the daemon's own
/// environment at spawn, so new leak names are caught by rule rather
/// than by list.
pub struct EnvScrub {
    names: Vec<String>,
    prefixes: Vec<String>,
    keep: Vec<String>,
}

impl EnvScrub {
    /// Exact-name scrubbing (the codex transport's list).
    pub fn names(names: &[&str]) -> Self {
        Self {
            names: names.iter().map(|s| s.to_string()).collect(),
            prefixes: Vec::new(),
            keep: Vec::new(),
        }
    }

    /// Prefix scrubbing: every inherited `PREFIX*` is removed except the
    /// `keep` names, which the operator sets on purpose.
    pub fn prefixes(prefixes: &[&str], keep: &[&str]) -> Self {
        Self {
            names: Vec::new(),
            prefixes: prefixes.iter().map(|s| s.to_string()).collect(),
            keep: keep.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// Exact names removed in addition to any prefix rule.
    pub fn and_names(mut self, names: &[&str]) -> Self {
        self.names
            .extend(names.iter().map(|name| (*name).to_string()));
        self
    }

    pub fn removes_name(&self, name: &str) -> bool {
        self.names.iter().any(|have| have == name)
            || (self.prefixes.iter().any(|prefix| name.starts_with(prefix.as_str()))
                && !self.keep.iter().any(|keep| keep == name))
    }
}

pub struct StdioAdapter {
    command: Vec<String>,
    env_scrub: EnvScrub,
    on_message: MessageHandler,
    on_disconnect: DisconnectHook,
    inner: Mutex<Inner>,
    pending: Pending,
    disconnected: AtomicBool,
    /// Raw newline-delimited event stream (no JSON-RPC framing).
    raw_lines: bool,
}

struct Inner {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
}

impl StdioAdapter {
    pub fn new(
        command: &[String],
        env_scrub: EnvScrub,
        on_message: MessageHandler,
        on_disconnect: DisconnectHook,
    ) -> Arc<Self> {
        Self::with_mode(command, env_scrub, on_message, on_disconnect, false)
    }

    /// Raw newline-delimited mode: each parsed line arrives as a
    /// notification whose `method` is the line's `type` field and whose
    /// `params` is the whole event object. Used by Claude `stream-json`.
    pub fn new_lines(
        command: &[String],
        env_scrub: EnvScrub,
        on_message: MessageHandler,
        on_disconnect: DisconnectHook,
    ) -> Arc<Self> {
        Self::with_mode(command, env_scrub, on_message, on_disconnect, true)
    }

    fn with_mode(
        command: &[String],
        env_scrub: EnvScrub,
        on_message: MessageHandler,
        on_disconnect: DisconnectHook,
        raw_lines: bool,
    ) -> Arc<Self> {
        Arc::new(Self {
            command: command.to_vec(),
            env_scrub,
            on_message,
            on_disconnect,
            inner: Mutex::new(Inner {
                child: None,
                stdin: None,
            }),
            pending: Pending::new(),
            disconnected: AtomicBool::new(false),
            raw_lines,
        })
    }

    /// Spawn the provider in its own process group and start the reader
    /// thread. Environment identity variables are scrubbed so the child
    /// cannot inherit a parent conversation's identity; `env` then
    /// injects this agent's own variables (CADENCE_ALIAS, …). Returns
    /// the pid.
    pub fn launch(
        self: &Arc<Self>,
        cwd: &str,
        stderr_log: &std::path::Path,
        env: &[(String, String)],
    ) -> Result<u32> {
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
        for name in &self.env_scrub.names {
            command.env_remove(name);
        }
        if !self.env_scrub.prefixes.is_empty() {
            for (key, _) in std::env::vars_os() {
                let Some(key) = key.to_str() else { continue };
                if self.env_scrub.keep.iter().any(|k| k == key) {
                    continue;
                }
                if self
                    .env_scrub
                    .prefixes
                    .iter()
                    .any(|p| key.starts_with(p.as_str()))
                {
                    command.env_remove(key);
                }
            }
        }
        for (key, value) in env {
            command.env(key, value);
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
            if self.raw_lines {
                let method = message
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("line")
                    .to_string();
                (self.on_message)(Incoming::Notification {
                    method,
                    params: message,
                });
                continue;
            }
            if message.get("method").is_none() {
                // Response to one of our requests.
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
        self.pending.fail_all("Provider process disconnected");
        // Wake protocol-level waiters (e.g. a turn-completion wait) that
        // do not sit on a pending RPC channel.
        (self.on_disconnect)();
    }

    /// Write one outbound frame. When the transport is already
    /// disconnected or stdin is already gone, provably no bytes left —
    /// a provider error, not `OutcomeUnknown`. Only a failed
    /// `write_all`/`flush` is genuinely uncertain: the request may or
    /// may not have reached the provider.
    pub fn send(&self, message: Value) -> Result<()> {
        if self.disconnected.load(Ordering::SeqCst) {
            return Err(Error::provider("Provider connection is closed"));
        }
        let mut inner = self.inner.lock().unwrap();
        let stdin = inner
            .stdin
            .as_mut()
            .ok_or_else(|| Error::provider("Provider connection is closed"))?;
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

    pub fn disconnected(&self) -> bool {
        self.disconnected.load(Ordering::SeqCst)
    }

    /// Signal the process group with `sig` (SIGINT interrupts a turn,
    /// SIGTERM ends the process). No-op once the child has exited.
    fn signal_group(&self, sig: libc::c_int) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(child) = inner.child.as_mut() {
            if child.try_wait().ok().flatten().is_none() {
                unsafe {
                    libc::kill(-(child.id() as i32), sig);
                }
            }
        }
    }

    /// Interrupt the running turn without killing the process (SIGINT
    /// to the provider's own process group).
    pub fn interrupt(&self) {
        self.signal_group(libc::SIGINT);
    }

    /// Drop the stdin pipe — a stream-json provider treats EOF as a
    /// clean shutdown request and exits on its own.
    pub fn close_stdin(&self) {
        self.inner.lock().unwrap().stdin.take();
    }

    /// Poll for child exit up to `timeout`; true when the process ended.
    pub fn wait_exit(&self, timeout: Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            {
                let mut inner = self.inner.lock().unwrap();
                match inner.child.as_mut().map(|c| c.try_wait()) {
                    Some(Ok(Some(_))) => return true,
                    Some(Ok(None)) => {}
                    _ => return true, // no child (or reaped) -> treat as exited
                }
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
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
