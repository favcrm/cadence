//! Persistent local controller: Unix-socket server + one actor per agent.
//!
//! The socket lives in a 0700 state directory and accepts only same-UID
//! peers (`SO_PEERCRED`). That establishes same-user access — it is not a
//! hostile same-user isolation boundary.
//!
//! Each registered agent gets one actor thread that owns its provider
//! adapter and serializes turns. The daemon relaunches enabled actors on
//! start — except actors fenced by an `unknown` in-flight attempt, which
//! stay in `attention` until a human reconciles them.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::io::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use uuid::Uuid;

use crate::adapter::{self, AdapterHooks, ProviderAdapter, ProviderRequest, TurnResult};
use crate::error::{Error, Result};
use crate::proto;
use crate::store::{Agent, Message, Store, Take};

/// A `(Mutex, Condvar)` pair used for queue/event wakeups.
pub struct Notify {
    lock: Mutex<()>,
    cv: Condvar,
}

impl Default for Notify {
    fn default() -> Self {
        Self::new()
    }
}

impl Notify {
    pub fn new() -> Self {
        Self {
            lock: Mutex::new(()),
            cv: Condvar::new(),
        }
    }
    pub fn notify_all(&self) {
        let _guard = self.lock.lock().unwrap();
        self.cv.notify_all();
    }
    /// Wait until `deadline`; returns false if it expired.
    pub fn wait_until(&self, deadline: Instant) -> bool {
        let guard = self.lock.lock().unwrap();
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return false;
        }
        let _ = self.cv.wait_timeout(guard, remaining).unwrap();
        Instant::now() < deadline
    }
}

struct PendingRequest {
    alias: String,
    id: Value,
    method: String,
    params: Value,
}

/// Per-agent control surface shared by dispatch and the actor thread.
struct AgentCtl {
    /// The actor's live adapter, published so `agent respond` can answer
    /// provider requests while a turn waits.
    adapter: Mutex<Option<Arc<dyn ProviderAdapter>>>,
    wake: Notify,
    thread: Mutex<Option<JoinHandle<()>>>,
}

pub struct Shared {
    pub store: Store,
    /// Broadcast on any queue/event change.
    changed: Notify,
    pending: Mutex<HashMap<String, PendingRequest>>,
    agents: Mutex<HashMap<String, Arc<AgentCtl>>>,
    closing: AtomicBool,
    provider_log_dir: PathBuf,
}

impl Shared {
    pub fn new(state_dir: &Path) -> Result<Arc<Self>> {
        let store = Store::open(&state_dir.join("cadence.sqlite3"))?;
        let provider_log_dir = state_dir.join("agents");
        std::fs::create_dir_all(&provider_log_dir)?;
        Ok(Arc::new(Self {
            store,
            changed: Notify::new(),
            pending: Mutex::new(HashMap::new()),
            agents: Mutex::new(HashMap::new()),
            closing: AtomicBool::new(false),
            provider_log_dir,
        }))
    }

    fn wake(&self) {
        self.changed.notify_all();
    }

    /// Spawn the actor for `alias` unless it is already running or fenced
    /// by an unknown outcome.
    pub fn launch_actor(self: &Arc<Self>, alias: &str) -> Result<()> {
        {
            let agents = self.agents.lock().unwrap();
            if let Some(ctl) = agents.get(alias) {
                if ctl
                    .thread
                    .lock()
                    .unwrap()
                    .as_ref()
                    .is_some_and(|h| !h.is_finished())
                {
                    return Err(Error::rejected("Agent is still running or stopping"));
                }
            }
        }
        if self.store.has_unknown(alias)? {
            self.store.set_agent_state(
                alias,
                "attention",
                Some("Uncertain provider outcome requires review"),
            )?;
            let _ = self.store.event_public(
                alias,
                "attention",
                json!({"reason": "uncertain_turn_preserved"}),
            );
            self.wake();
            return Ok(());
        }
        self.store.set_agent_state(alias, "starting", None)?;
        let ctl = Arc::new(AgentCtl {
            adapter: Mutex::new(None),
            wake: Notify::new(),
            thread: Mutex::new(None),
        });
        let shared = Arc::clone(self);
        let owned = alias.to_string();
        let spawned = Arc::clone(&ctl);
        let handle = thread::spawn(move || shared.run_actor(&owned, spawned));
        *ctl.thread.lock().unwrap() = Some(handle);
        self.agents.lock().unwrap().insert(alias.to_string(), ctl);
        Ok(())
    }

    fn on_provider_event(&self, alias: &str, method: &str, params: Value) {
        // Token streams and tool details stay in the provider transcript;
        // we record the lifecycle envelope only.
        let _ = self.store.event_public(
            alias,
            "provider_event",
            json!({
                "method": method, "data": params,
            }),
        );
        self.wake();
    }

    fn on_provider_request(self: &Arc<Self>, alias: &str, request: ProviderRequest) {
        let handle = Uuid::new_v4().simple().to_string();
        self.pending.lock().unwrap().insert(
            handle.clone(),
            PendingRequest {
                alias: alias.to_string(),
                id: request.id,
                method: request.method.clone(),
                params: request.params.clone(),
            },
        );
        let _ = self.store.set_agent_state(alias, "waiting_input", None);
        let _ = self.store.event_public(
            alias,
            "input_required",
            json!({"request": handle, "method": request.method, "params": request.params}),
        );
        self.wake();
    }

    /// The actor loop: own the adapter, serialize turns, preserve unknown
    /// outcomes, and stop cleanly on disable/shutdown.
    fn run_actor(self: &Arc<Self>, alias: &str, ctl: Arc<AgentCtl>) {
        let outcome = self.actor_inner(alias, &ctl);
        // Cleanup always runs: close adapter, clear ctl, final state.
        if let Some(adapter) = ctl.adapter.lock().unwrap().take() {
            adapter.close();
        }
        {
            let mut pending = self.pending.lock().unwrap();
            pending.retain(|_, req| req.alias != alias);
        }
        let closing = self.closing.load(Ordering::SeqCst);
        let _ = self.store.set_pid(alias, None);
        match outcome {
            Err(ref error) => {
                let _ = self
                    .store
                    .set_agent_state(alias, "attention", Some(&error.to_string()));
                let _ = self.store.event_public(
                    alias,
                    "attention",
                    json!({"reason": error.to_string()}),
                );
            }
            Ok(()) => {
                let agent = self.store.agent(alias);
                let enabled = agent.as_ref().map(|a| a.enabled).unwrap_or(false);
                let state = if !enabled {
                    "stopped"
                } else if closing {
                    "offline"
                } else {
                    "idle"
                };
                let _ = self.store.set_agent_state(alias, state, None);
            }
        }
        self.wake();
    }

    fn actor_inner(self: &Arc<Self>, alias: &str, ctl: &Arc<AgentCtl>) -> Result<()> {
        let agent = self.store.agent(alias)?;
        let log_path = self.provider_log_dir.join(format!("{alias}.provider.log"));
        let shared = Arc::clone(self);
        let owned = alias.to_string();
        let hooks = AdapterHooks {
            on_event: Box::new(move |method, params| {
                shared.on_provider_event(&owned, method, params)
            }),
            on_request: {
                let shared = Arc::clone(self);
                let owned = alias.to_string();
                Box::new(move |request| shared.on_provider_request(&owned, request))
            },
        };
        let adapter = adapter::build(&agent, hooks, &log_path)?;
        let adapter: Arc<dyn ProviderAdapter> = Arc::from(adapter);
        let identity = adapter.open(&agent)?;
        self.store.set_identity(
            alias,
            &identity.thread_id,
            &identity.session_id,
            identity.model.as_deref(),
            identity.pid,
        )?;
        *ctl.adapter.lock().unwrap() = Some(Arc::clone(&adapter));
        self.wake();
        loop {
            if self.closing.load(Ordering::SeqCst) {
                return Ok(());
            }
            match self.store.take_queued(alias)? {
                Take::Stop => return Ok(()),
                Take::Empty => {
                    if adapter.disconnected() {
                        return Err(Error::unknown("Provider process disconnected while idle"));
                    }
                    ctl.wake.wait_until(Instant::now() + Duration::from_secs(5));
                }
                Take::Message(message) => {
                    let started_id = message.id.clone();
                    let shared = Arc::clone(self);
                    let outcome = adapter.run_turn(&message.body, &message.id, &move |turn| {
                        let _ = shared.store.mark_running(&started_id, turn);
                        shared.wake();
                    });
                    match outcome {
                        Ok(result) => self.complete(&message, result)?,
                        Err(Error::OutcomeUnknown(_)) => {
                            return self.unknown(alias, &message);
                        }
                        // A provider/adapter error is actor-fatal: record
                        // the failed attempt, then land in `attention`.
                        Err(error) => {
                            self.store.finish(
                                &message,
                                "failed",
                                &json!({"status": "failed", "text": "", "error": error.to_string()}),
                                Some(&error.to_string()),
                            )?;
                            self.wake();
                            return Err(error);
                        }
                    }
                }
            }
        }
    }

    fn complete(&self, message: &Message, result: TurnResult) -> Result<()> {
        let status = match result.status.as_str() {
            "completed" | "failed" | "interrupted" => result.status.clone(),
            other => {
                return Err(Error::unknown(format!(
                    "Unexpected provider completion status: {other}"
                )))
            }
        };
        self.store.finish(
            message,
            &status,
            &json!({
                "turn_id": result.turn_id,
                "status": status,
                "text": result.text,
                "stop_reason": result.stop_reason,
                "error": result.error,
            }),
            result.error.as_deref(),
        )?;
        self.wake();
        Ok(())
    }

    /// An `OutcomeUnknown` never becomes a retry: mark the attempt and
    /// fence the actor for review.
    fn unknown(&self, alias: &str, message: &Message) -> Result<()> {
        self.store.finish(
            message,
            "unknown",
            &json!({"status": "unknown", "text": "", "error": "provider outcome uncertain"}),
            Some("provider outcome uncertain"),
        )?;
        self.store.set_agent_state(
            alias,
            "attention",
            Some("Uncertain provider outcome requires review"),
        )?;
        let _ = self
            .store
            .event_public(alias, "attention", json!({"reason": "uncertain_turn"}));
        self.wake();
        Err(Error::unknown("Uncertain provider outcome requires review"))
    }

    // ---- dispatch ----

    pub fn dispatch(self: &Arc<Self>, method: &str, params: &Value) -> Result<Value> {
        match method {
            "health" => Ok(json!({
                "state": "ready",
                "protocol": proto::PROTOCOL_VERSION,
                "capabilities": proto::CAPABILITIES,
            })),
            "shutdown" => {
                self.closing.store(true, Ordering::SeqCst);
                self.wake();
                Ok(json!({"state": "stopping"}))
            }
            "agent_register" => self.rpc_register(params),
            "agent_list" => Ok(json!({
                "agents": self.store.agents()?.iter().map(Agent::to_json).collect::<Vec<_>>()
            })),
            "agent_show" => {
                let alias = required_str(params, "alias")?;
                let agent = self.store.agent(alias)?;
                let messages = self.store.messages(alias)?;
                Ok(json!({
                    "agent": agent.to_json(),
                    "messages": messages.iter().map(Message::to_json).collect::<Vec<_>>(),
                    "event_cursor": self.store.event_cursor(alias)?,
                }))
            }
            "agent_send" => self.rpc_send(params),
            "agent_ask" => self.rpc_ask(params),
            "agent_events" => self.rpc_events(params),
            "agent_requests" => {
                let alias = required_str(params, "alias")?;
                let requests = self
                    .pending
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|(_, req)| req.alias == alias)
                    .map(|(handle, req)| {
                        json!({"request": handle, "method": req.method, "params": req.params})
                    })
                    .collect::<Vec<_>>();
                Ok(json!({"requests": requests}))
            }
            "agent_respond" => self.rpc_respond(params),
            "agent_stop" => self.rpc_stop(params),
            "agent_resume" => {
                let alias = required_str(params, "alias")?;
                self.store.agent(alias)?;
                self.store.set_enabled(alias, true)?;
                self.launch_actor(alias)?;
                Ok(json!({"alias": alias, "state": "starting"}))
            }
            other => Err(Error::rejected(format!("Unknown method '{other}'"))),
        }
    }

    fn rpc_register(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let alias = required_str(params, "alias")?;
        let provider = required_str(params, "provider")?;
        let endpoint = optional_str(params, "endpoint_kind").unwrap_or("managed");
        let role = optional_str(params, "role").unwrap_or("worker");
        let cwd = required_str(params, "cwd")?;
        let sandbox = optional_str(params, "sandbox").unwrap_or("read-only");
        let instructions = optional_str(params, "instructions");
        let cwd = std::fs::canonicalize(cwd)
            .map_err(|_| Error::rejected("Working directory must exist"))?;
        self.store.register_agent(&crate::store::NewAgent {
            alias,
            provider,
            endpoint_kind: endpoint,
            role,
            cwd: &cwd.to_string_lossy(),
            sandbox,
            instructions,
        })?;
        self.launch_actor(alias)?;
        Ok(json!({"alias": alias, "state": "starting", "provider": provider}))
    }

    fn rpc_send(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let alias = required_str(params, "alias")?;
        let text = required_str(params, "text")?;
        let reply_to = optional_str(params, "reply_to");
        let message = optional_str(params, "message")
            .map(str::to_string)
            .unwrap_or_else(|| Uuid::new_v4().simple().to_string());
        let (duplicate, state) = self
            .store
            .enqueue(alias, text, reply_to, &message, "user")?;
        self.notify_agent(alias);
        self.wake();
        Ok(json!({"message": message, "state": state, "duplicate": duplicate}))
    }

    /// Send and wait for the message's terminal state, bounded by `wait`.
    fn rpc_ask(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let wait = optional_u64(params, "wait").unwrap_or(120).min(600);
        let result = self.rpc_send(params)?;
        let message = result["message"].as_str().unwrap_or_default().to_string();
        let deadline = Instant::now() + Duration::from_secs(wait);
        loop {
            let stored = self
                .store
                .message(&message)?
                .ok_or_else(|| Error::internal("Message vanished"))?;
            if is_terminal(&stored.state) || Instant::now() >= deadline {
                return Ok(stored.to_json());
            }
            self.changed
                .wait_until(Instant::now() + Duration::from_millis(250));
        }
    }

    fn rpc_events(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let alias = required_str(params, "alias")?;
        let after = optional_i64(params, "after").unwrap_or(0);
        if after < 0 {
            return Err(Error::rejected("Event cursor must be nonnegative"));
        }
        let wait = optional_u64(params, "wait").unwrap_or(0).min(30);
        let deadline = Instant::now() + Duration::from_secs(wait);
        loop {
            let events = self.store.events(alias, after, 100)?;
            if !events.is_empty() || self.closing.load(Ordering::SeqCst) {
                let cursor = events.last().map(|e| e.seq).unwrap_or(after);
                return Ok(json!({
                    "events": events.iter().map(crate::store::Event::to_json).collect::<Vec<_>>(),
                    "cursor": cursor,
                }));
            }
            if Instant::now() >= deadline {
                return Ok(json!({"events": [], "cursor": after}));
            }
            let step = deadline.min(Instant::now() + Duration::from_secs(1));
            self.changed.wait_until(step);
        }
    }

    fn rpc_respond(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let alias = required_str(params, "alias")?;
        let handle = required_str(params, "request")?;
        let decision = optional_str(params, "decision");
        // Explicit JSON null means "not provided".
        let answers = params.get("answers").filter(|a| !a.is_null()).cloned();
        let pending = {
            let map = self.pending.lock().unwrap();
            map.get(handle).map(|req| {
                (
                    req.alias.clone(),
                    req.id.clone(),
                    req.method.clone(),
                    req.params.clone(),
                )
            })
        };
        let Some((owner, request_id, method, request_params)) = pending else {
            return Err(Error::rejected(
                "Request is no longer pending for this agent",
            ));
        };
        if owner != alias {
            return Err(Error::rejected(
                "Request is no longer pending for this agent",
            ));
        }
        let response = match method.as_str() {
            "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
                match decision {
                    Some("accept") | Some("decline") if answers.is_none() => {
                        json!({"decision": decision.unwrap()})
                    }
                    _ => return Err(Error::rejected("Respond with decision accept or decline")),
                }
            }
            "item/tool/requestUserInput" => {
                if decision.is_some() || !answers.as_ref().is_some_and(Value::is_object) {
                    return Err(Error::rejected("Respond with an answers object"));
                }
                json!({"answers": answers.unwrap()})
            }
            "session/request_permission" => match decision {
                Some("decline") if answers.is_none() => {
                    json!({"outcome": {"outcome": "cancelled"}})
                }
                Some("accept") if answers.is_none() => {
                    let option = request_params
                        .get("options")
                        .and_then(Value::as_array)
                        .and_then(|options| {
                            options.iter().find(|o| {
                                o.get("kind").and_then(Value::as_str) == Some("allow_once")
                            })
                        })
                        .ok_or_else(|| {
                            Error::rejected("Provider did not offer an allow-once option")
                        })?;
                    json!({"outcome": {"outcome": "selected", "optionId": option["optionId"]}})
                }
                _ => return Err(Error::rejected("Respond with decision accept or decline")),
            },
            _ => return Err(Error::rejected(
                "This request type is not supported; stop the agent or use the provider directly",
            )),
        };
        let adapter = self
            .agents
            .lock()
            .unwrap()
            .get(alias)
            .and_then(|ctl| ctl.adapter.lock().unwrap().clone());
        let adapter = adapter
            .ok_or_else(|| Error::internal("Agent adapter is not available for this request"))?;
        adapter.respond(&request_id, response)?;
        self.pending.lock().unwrap().remove(handle);
        self.store.set_agent_state(alias, "busy", None)?;
        let _ = self
            .store
            .event_public(alias, "input_answered", json!({"request": handle}));
        self.wake();
        Ok(json!({"state": "answered"}))
    }

    fn rpc_stop(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let alias = required_str(params, "alias")?;
        self.store.agent(alias)?;
        self.store.set_enabled(alias, false)?;
        self.store.set_agent_state(alias, "stopping", None)?;
        let _ = self.store.event_public(alias, "stop_requested", json!({}));
        let ctl = self.agents.lock().unwrap().get(alias).cloned();
        if let Some(ctl) = ctl {
            if let Some(adapter) = ctl.adapter.lock().unwrap().clone() {
                adapter.interrupt();
            }
            ctl.wake.notify_all();
            let handle = ctl.thread.lock().unwrap().take();
            if let Some(handle) = handle {
                let _ = handle.join();
            }
        }
        self.store.set_agent_state(alias, "stopped", None)?;
        self.wake();
        Ok(json!({"alias": alias, "state": "stopped"}))
    }

    fn notify_agent(&self, alias: &str) {
        if let Some(ctl) = self.agents.lock().unwrap().get(alias) {
            ctl.wake.notify_all();
        }
    }

    /// Cooperative shutdown: stop accepting, stop actors, close adapters.
    fn shutdown(&self) {
        for ctl in self.agents.lock().unwrap().values() {
            if let Some(adapter) = ctl.adapter.lock().unwrap().clone() {
                adapter.interrupt();
            }
            ctl.wake.notify_all();
        }
        let agents: Vec<Arc<AgentCtl>> = self.agents.lock().unwrap().values().cloned().collect();
        for ctl in agents {
            let handle = ctl.thread.lock().unwrap().take();
            if let Some(handle) = handle {
                let _ = handle.join();
            }
        }
    }
}

fn is_terminal(state: &str) -> bool {
    matches!(
        state,
        "completed" | "failed" | "interrupted" | "unknown" | "cancelled"
    )
}

fn required_str<'a>(params: &'a Value, field: &str) -> Result<&'a str> {
    params
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| Error::rejected(format!("Missing required parameter '{field}'")))
}

fn optional_str<'a>(params: &'a Value, field: &str) -> Option<&'a str> {
    params.get(field).and_then(Value::as_str)
}

fn optional_u64(params: &Value, field: &str) -> Option<u64> {
    params.get(field).and_then(Value::as_u64)
}

fn optional_i64(params: &Value, field: &str) -> Option<i64> {
    params.get(field).and_then(Value::as_i64)
}

/// Reject peers that are not the same Unix user.
fn check_peer(stream: &UnixStream) -> Result<()> {
    let mut cred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut cred as *mut _ as *mut libc::c_void,
            &mut len,
        )
    };
    if rc != 0 {
        return Err(Error::internal("Cannot determine socket peer credentials"));
    }
    if cred.uid != unsafe { libc::geteuid() } {
        return Err(Error::rejected("Socket peer is not the same user"));
    }
    Ok(())
}

fn handle_conn(shared: Arc<Shared>, stream: UnixStream) {
    if check_peer(&stream).is_err() {
        return;
    }
    let mut writer = match stream.try_clone() {
        Ok(w) => w,
        Err(_) => return,
    };
    let reader = BufReader::new(stream);
    for line in reader.lines() {
        let Ok(line) = line else { break };
        let response = serde_json::from_str::<Value>(&line)
            .map_err(|_| Error::rejected("Request must be one JSON object per line"))
            .and_then(|frame| {
                let method = frame
                    .get("method")
                    .and_then(Value::as_str)
                    .ok_or_else(|| Error::rejected("Missing 'method'"))?;
                let params = frame.get("params").cloned().unwrap_or(json!({}));
                shared.dispatch(method, &params)
            });
        let frame = match response {
            Ok(result) => proto::ok(result),
            Err(error) => proto::err(&error),
        };
        if writeln!(writer, "{frame}").is_err() {
            break;
        }
    }
}

/// Run the daemon in the foreground until `shutdown` or a signal.
pub fn serve(state_dir: &Path) -> Result<()> {
    let shared = Shared::new(state_dir)?;
    let socket_path = state_dir.join("cadence.sock");
    if socket_path.exists() {
        std::fs::remove_file(&socket_path)?;
    }
    let listener = UnixListener::bind(&socket_path)?;
    listener.set_nonblocking(true)?;
    // Relaunch enabled actors; fenced ones land in `attention` instead.
    for agent in shared.store.agents()? {
        if agent.enabled {
            shared.launch_actor(&agent.alias)?;
        }
    }
    // Signal-driven shutdown: set the same flag as the rpc.
    {
        let shared = Arc::clone(&shared);
        let mut signals = signal_hook::iterator::Signals::new([
            signal_hook::consts::SIGTERM,
            signal_hook::consts::SIGINT,
        ])
        .map_err(|e| Error::internal(format!("signal hook: {e}")))?;
        thread::spawn(move || {
            for _ in signals.forever() {
                shared.closing.store(true, Ordering::SeqCst);
                shared.wake();
            }
        });
    }
    while !shared.closing.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _)) => {
                let shared = Arc::clone(&shared);
                thread::spawn(move || handle_conn(shared, stream));
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(e.into()),
        }
    }
    shared.shutdown();
    let _ = std::fs::remove_file(&socket_path);
    Ok(())
}
