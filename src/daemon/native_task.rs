//! Bounded private native-task ingress. Not an operator RPC or generic exec:
//! only the constructor-created control channel reaches this fixed Pi bridge.
//! Alias/model/prompt are DATA; root kernel admission, immutable route policy
//! and separately authenticated one-use Pi operation still precede Node effects.
use super::*;
use crate::adapter::{AdapterHooks, ProviderAdapter};
use crate::installer_bundle::constructor::private_wire::{self, Packet};
use std::os::unix::net::UnixDatagram;
// AOS accepts at most 65,536 decoded bytes in 4,096 consecutive parts per task
// (agenticos-v2 native-runtime-task-wire.ts `event()`); past either limit it
// aborts the runtime. CAD therefore keeps a task's output inside that budget.
const TASK_OUTPUT_MAX_BYTES: usize = 65_536;
const TASK_OUTPUT_MAX_PARTS: u64 = 4_096;
const TASK_PART_BYTES: usize = 16_384;
/// Last bytes of an over-budget task: a valid NDJSON event on its own line.
const TASK_OUTPUT_TRUNCATED_MARKER: &[u8] = b"\n{\"type\":\"output-truncated\"}\n";
struct StreamState {
    next: u64,
    closed: bool,
    bytes: usize,
    truncated: bool,
    done: bool,
}
impl StreamState {
    fn new() -> Self {
        Self {
            next: 0,
            closed: false,
            bytes: 0,
            truncated: false,
            done: false,
        }
    }
}
pub(super) struct Task {
    pub(super) id: String,
    adapter: Arc<dyn ProviderAdapter>,
    stream: Arc<Mutex<StreamState>>,
    worker: thread::JoinHandle<Result<()>>,
}
impl Task {
    pub(super) fn start(
        shared: &Arc<Shared>,
        control: Arc<UnixDatagram>,
        id: String,
        alias: String,
        model: String,
        prompt: String,
        registrations: &mut Registrations<'_>,
    ) -> Result<Self> {
        require_task_alias(&alias)?;
        if id.len() != 32
            || !id
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            || prompt.is_empty()
            || prompt.len() > 32768
            || !model.contains('/')
        {
            return Err(Error::rejected("native task selectors refused"));
        }
        let params = serde_json::to_string(&json!({"model":model}))?;
        // Row reuse is DATA only, reached after the unchanged Root family-exit
        // and daemon worker/stream retirement fences. Never adopt a saved
        // endpoint, session or PID; every task builds a fresh protected adapter.
        let agent = registrations.select(
            &alias,
            // Match the helper's held guest-writable repository leaf. The
            // supervisor-owned /workspace ancestor remains non-writable.
            "/workspace/company",
            &params,
        )?;
        let part = Arc::new(Mutex::new(StreamState::new()));
        let stream = part.clone();
        let event_control = control.clone();
        let event_task = id.clone();
        let event_part = part.clone();
        let request_control = control.clone();
        let request_task = id.clone();
        let request_part = part.clone();
        let hooks = AdapterHooks {
            on_event: Box::new(move |method, value| {
                if emit(
                    &event_control,
                    &event_task,
                    &event_part,
                    &json!({"type":"event","method":method,"value":value}),
                )
                .is_err()
                {
                    unsafe { libc::_exit(125) }
                }
            }),
            // Never auto-approve a provider request. Surface it as data only.
            on_request: Box::new(move |request| {
                if emit(&request_control,&request_task,&request_part,&json!({"type":"request","id":request.id,"method":request.method,"params":request.params})).is_err(){unsafe{libc::_exit(125)}}
            }),
        };
        let adapter: Arc<dyn ProviderAdapter> = Arc::from(crate::adapter::build(
            &agent,
            hooks,
            &shared.state_dir.join("native-pi.stderr.log"),
            &shared.provider_env,
            Some(21001),
        )?);
        let running = adapter.clone();
        let task = id.clone();
        let worker = thread::spawn(move || -> Result<()> {
            let outcome = (|| -> Result<()> {
                let identity = running.open(&agent)?;
                if identity.model.as_deref() != Some(model.as_str()) {
                    running.close();
                    return Err(Error::unknown(
                        "native Pi model readback differs from election",
                    ));
                }
                emit(
                    &control,
                    &task,
                    &part,
                    &json!({"type":"opened","model":identity.model,"pidObservation":identity.pid}),
                )?;
                let started_control = control.clone();
                let started_task = task.clone();
                let started_part = part.clone();
                let result = running.run_turn(&prompt, &task, &move |turn| {
                    if emit(
                        &started_control,
                        &started_task,
                        &started_part,
                        &json!({"type":"turn-started","turn":turn}),
                    )
                    .is_err()
                    {
                        unsafe { libc::_exit(125) }
                    }
                })?;
                emit(
                    &control,
                    &task,
                    &part,
                    &json!({"type":"result","turn":result.turn_id,"status":result.status,"text":result.text,"error":result.error,"stopReason":result.stop_reason}),
                )?;
                // Retain the actual adapter/control after completion. A result or
                // EOF does NOT certify owned retirement or discharge host UNKNOWN.
                Ok(())
            })();
            if let Err(error) = &outcome {
                // Failure is explicit task DATA, never successful EOF/retire.
                // Keep actual control for Root's independent physical cleanup.
                emit(
                    &control,
                    &task,
                    &part,
                    &json!({"type":"failed","error":error.to_string()}),
                )?;
            }
            outcome
        });
        Ok(Self {
            id,
            adapter,
            stream,
            worker,
        })
    }
    pub(super) fn interrupt(&self) {
        self.adapter.interrupt();
    }
    pub(super) fn retire(self) -> Result<()> {
        self.adapter.close();
        let until = Instant::now() + Duration::from_secs(30);
        while !self.worker.is_finished() {
            if Instant::now() >= until {
                return Err(Error::unknown("native task retirement/turn join UNKNOWN"));
            }
            thread::sleep(Duration::from_millis(1));
        }
        let outcome = self
            .worker
            .join()
            .map_err(|_| Error::unknown("native task worker lost"))?;
        // A failed/cancelled turn is not a successful result. Its failure event
        // remains visible; this join acknowledges control quiescence ONLY after
        // Root independently retired the actual namespace family. Seal streaming
        // under the SAME callback mutex before the retirement ACK/new generation.
        self.stream
            .lock()
            .map_err(|_| Error::unknown("native stream poisoned"))?
            .closed = true;
        if let Err(error) = outcome {
            tracing::warn!("retired native turn failed: {error}");
        }
        Ok(())
    }
}
// Only remember rows actually registered by this retained native controller,
// on its SAME Store. This bookkeeping cannot prove retirement or authorize a
// launch: the private controller and Root retain those independent fences.
pub(super) struct Registrations<'store> {
    store: &'store crate::store::Store,
    aliases: std::collections::HashSet<String>,
    created: std::collections::HashMap<String, f64>,
}
impl<'store> Registrations<'store> {
    pub(super) fn new(store: &'store crate::store::Store) -> Self {
        Self {
            store,
            aliases: std::collections::HashSet::new(),
            created: std::collections::HashMap::new(),
        }
    }
    fn select(&mut self, alias: &str, cwd: &str, params: &str) -> Result<crate::store::Agent> {
        require_task_alias(alias)?;
        let expected = registration(alias, cwd, params);
        if !self.aliases.contains(alias) {
            if self.aliases.len() >= 4096 {
                return Err(Error::rejected("native registration history exhausted"));
            }
            // An existing foreign alias still hits the real global duplicate
            // guard. Never catch that failure and fetch/adopt an arbitrary row.
            self.store.register_agent(&expected)?;
        }
        let agent = self.store.agent(alias)?;
        // Alias/config equality cannot adopt a legitimately removed and
        // re-registered row. Retain the first committed creation witness,
        // never normalize it or update it to make a replacement eligible.
        if !agent.created.is_finite()
            || agent.created <= 0.0
            || (self.aliases.contains(alias) && self.created.get(alias) != Some(&agent.created))
        {
            return Err(Error::rejected("native saved registration continuity lost"));
        }
        // A native successor is not an operator resume/reconcile. Preserve
        // durable uncertainty and recorded errors without any repair writes.
        if !matches!(
            agent.state.as_str(),
            "starting" | "idle" | "stopped" | "offline"
        ) || agent.error.is_some()
            || self.store.has_unknown(alias)?
        {
            return Err(Error::rejected("native saved registration is fenced"));
        }
        let expected_params: Value = serde_json::from_str(params)?;
        if agent.alias != expected.alias
            || agent.provider != expected.provider
            || agent.endpoint_kind != expected.endpoint_kind
            || agent.role != expected.role
            || agent.cwd != expected.cwd
            || agent.sandbox != expected.sandbox
            || agent.instructions.is_some()
            || agent.team_role.is_some()
            || !agent.enabled
            || agent.params.as_ref() != Some(&expected_params)
            || agent.thread_id.is_some()
            || agent.session_id.is_some()
            || agent.model.is_some()
            || agent.effort.is_some()
            || agent.pid.is_some()
            || agent.pid_start.is_some()
            || agent.endpoint.is_some()
            || agent.generation.is_some()
        {
            // No mutation into eligibility, deletion or fallback model. Use
            // only this checked real row snapshot for a fresh adapter open.
            return Err(Error::rejected(
                "native saved registration differs from election",
            ));
        }
        self.aliases.insert(alias.to_owned());
        self.created
            .entry(alias.to_owned())
            .or_insert(agent.created);
        Ok(agent)
    }
}

// UNIT selector validation only; no task/history/Store/adapter effects.
fn require_task_alias(alias: &str) -> Result<()> {
    crate::proto::identifier(alias, "native task alias").map(|_| ())
}

// Store registration metadata is not native launch permission. The adapter's
// fixed guest UID and alias-derived, signed master/worker selection remain the
// independent physical launch path. `cwd` is fixed by Task::start; tests use an
// isolated existing directory to exercise the real Store contract without Root.
fn registration<'a>(alias: &'a str, cwd: &'a str, params: &'a str) -> crate::store::NewAgent<'a> {
    crate::store::NewAgent {
        alias,
        provider: "pi",
        endpoint_kind: "managed",
        role: if crate::master::is_master(alias) {
            "pm"
        } else {
            "worker"
        },
        cwd,
        sandbox: if crate::master::is_master(alias) {
            "read-only"
        } else {
            "workspace-write"
        },
        instructions: None,
        params: Some(params),
        team_role: None,
        // The task carries an explicit elected model. Provider-default would
        // conflict with it; ordinary Store resolution keeps the explicit value.
        model_policy: None,
    }
}

// Independently authored DATA refusal body; implementers register only.
#[cfg(test)]
#[path = "native_registration_acceptance.rs"]
mod registration_acceptance;

#[cfg(test)]
#[path = "native_task_budget_acceptance.rs"]
mod budget_acceptance;

#[cfg(test)]
#[path = "native_task_terminal_acceptance.rs"]
mod terminal_acceptance;

/// Bytes reserved after the marker for one terminal event (`result`/`failed`).
const TERMINAL_EVENT_MAX: usize = 4_096;
/// A `result` keeps `status` in its minimal line only if serialized <= this.
const TERMINAL_STATUS_MAX: usize = 64;

/// Terminal events are the `result` and `failed` shapes built in `Task::start`.
fn terminal_kind(value: &Value) -> Option<&'static str> {
    match value.get("type").and_then(Value::as_str) {
        Some("result") => Some("result"),
        Some("failed") => Some("failed"),
        _ => None,
    }
}

/// Fixed-shape fallback for a terminal event too large to send whole.
fn minimal_terminal(kind: &str, value: &Value) -> Vec<u8> {
    let status = value
        .get("status")
        .and_then(|s| serde_json::to_string(s).ok())
        .filter(|s| s.len() <= TERMINAL_STATUS_MAX);
    match (kind, status) {
        ("result", Some(status)) => {
            format!("{{\"type\":\"result\",\"status\":{status},\"truncated\":true}}\n")
        }
        ("result", None) => "{\"type\":\"result\",\"truncated\":true}\n".to_owned(),
        _ => "{\"type\":\"failed\",\"truncated\":true}\n".to_owned(),
    }
    .into_bytes()
}

fn send_parts(
    control: &UnixDatagram,
    task: &str,
    sequence: &mut StreamState,
    bytes: &[u8],
) -> Result<()> {
    use base64::Engine;
    sequence.bytes += bytes.len();
    for chunk in bytes.chunks(TASK_PART_BYTES) {
        let index = sequence.next;
        sequence.next = sequence
            .next
            .checked_add(1)
            .ok_or_else(|| Error::unknown("native stream exhausted"))?;
        if index >= 9_007_199_254_740_991 {
            return Err(Error::unknown("native task stream exhausted"));
        }
        private_wire::send_child(
            control,
            &Packet::TaskEvent {
                version: 1,
                task: task.to_owned(),
                part: index,
                bytes: base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(chunk),
            },
        )?;
    }
    Ok(())
}

/// Whether `len` bytes (one part when `len <= TASK_PART_BYTES`) still fit AOS's
/// hard limits right now, with nothing reserved.
fn fits_hard(sequence: &StreamState, len: usize) -> bool {
    let parts = len.div_ceil(TASK_PART_BYTES) as u64;
    sequence.bytes + len <= TASK_OUTPUT_MAX_BYTES && sequence.next + parts <= TASK_OUTPUT_MAX_PARTS
}

fn emit(
    control: &UnixDatagram,
    task: &str,
    part: &Mutex<StreamState>,
    value: &Value,
) -> Result<()> {
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    let mut sequence = part
        .lock()
        .map_err(|_| Error::unknown("native stream poisoned"))?;
    if sequence.closed || sequence.done {
        return Ok(());
    } // refuse late data; no retired-session mutation
    let terminal = terminal_kind(value);
    if sequence.truncated {
        // Data is dropped. The FIRST terminal event ends the stream: whole if
        // small, else the minimal line. Errors would make the caller `_exit`.
        if let Some(kind) = terminal {
            sequence.done = true;
            if bytes.len() > TERMINAL_EVENT_MAX {
                bytes = minimal_terminal(kind, value);
            }
            if fits_hard(&sequence, bytes.len()) {
                send_parts(control, task, &mut sequence, &bytes)?;
            }
        }
        return Ok(());
    }
    // The marker, TERMINAL_EVENT_MAX bytes and one part are reserved, so the
    // marker and one terminal event always fit AOS's budget. An event inside the
    // data budget is sent unchanged.
    let marker = TASK_OUTPUT_TRUNCATED_MARKER.len();
    let parts_left = TASK_OUTPUT_MAX_PARTS
        .saturating_sub(2)
        .saturating_sub(sequence.next) as usize;
    let room = TASK_OUTPUT_MAX_BYTES
        .saturating_sub(marker)
        .saturating_sub(TERMINAL_EVENT_MAX)
        .saturating_sub(sequence.bytes)
        .min(parts_left.saturating_mul(TASK_PART_BYTES));
    if bytes.len() <= room {
        return send_parts(control, task, &mut sequence, &bytes);
    }
    // A terminal event that fits the reserve goes out whole, with no marker:
    // nothing was lost.
    let reserve_fits = bytes.len() <= TERMINAL_EVENT_MAX
        && sequence.bytes + bytes.len() + marker <= TASK_OUTPUT_MAX_BYTES
        && sequence.next + 2 <= TASK_OUTPUT_MAX_PARTS;
    if terminal.is_some() && reserve_fits {
        return send_parts(control, task, &mut sequence, &bytes);
    }
    // Truncate: the event that does not fit is cut at a UTF-8 boundary as data
    // (unless it is a small terminal event whose reserve is spent, which is
    // sent whole below), then the marker.
    sequence.truncated = true;
    if terminal.is_none() || bytes.len() > TERMINAL_EVENT_MAX {
        let mut cut = room;
        while cut > 0 && (bytes[cut] & 0xC0) == 0x80 {
            cut -= 1;
        }
        send_parts(control, task, &mut sequence, &bytes[..cut])?;
    }
    send_parts(control, task, &mut sequence, TASK_OUTPUT_TRUNCATED_MARKER)?;
    if let Some(kind) = terminal {
        sequence.done = true;
        if bytes.len() > TERMINAL_EVENT_MAX {
            bytes = minimal_terminal(kind, value);
        }
        if fits_hard(&sequence, bytes.len()) {
            send_parts(control, task, &mut sequence, &bytes)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Registration-contract check only: actual SQLite/Store/model resolution,
    // no fabricated Root grant, custody, service, launch or provider response.
    #[test]
    fn native_task_registration_uses_real_store_grammar_and_explicit_model() {
        let dir = tempfile::tempdir().unwrap();
        let store =
            Arc::new(crate::store::Store::open(&dir.path().join("registration.db")).unwrap());
        let cwd = dir.path().to_str().unwrap();
        let params = r#"{"model":"openai-codex/gpt-6.1-sol"}"#;
        let mut registrations = Registrations::new(&store);
        for (alias, role, sandbox) in [
            ("master", "pm", "read-only"),
            ("native-worker", "worker", "workspace-write"),
        ] {
            let first = registrations.select(alias, cwd, params).unwrap();
            let second = registrations.select(alias, cwd, params).unwrap();
            assert_eq!(first.created, second.created); // SAME durable row, no deletion.
            assert_eq!(second.provider, "pi");
            assert_eq!(second.endpoint_kind, "managed");
            assert_eq!(second.role, role);
            assert_eq!(second.sandbox, sandbox);
            assert_eq!(second.cwd, cwd);
            assert_eq!(second.params.unwrap()["model"], "openai-codex/gpt-6.1-sol");
        }
        assert_eq!(store.agents().unwrap().len(), 2);
    }
}
