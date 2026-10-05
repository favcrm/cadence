//! Bounded private native-task ingress. Not an operator RPC or generic exec:
//! only the constructor-created control channel reaches this fixed Pi bridge.
//! Alias/model/prompt are DATA; root kernel admission, immutable route policy
//! and separately authenticated one-use Pi operation still precede Node effects.
use super::*;
use crate::adapter::{AdapterHooks, ProviderAdapter};
use crate::installer_bundle::constructor::private_wire::{self, Packet};
use std::os::unix::net::UnixDatagram;
pub(super) struct Task {
    pub(super) id: String,
    adapter: Arc<dyn ProviderAdapter>,
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
    ) -> Result<Self> {
        if id.len() != 32
            || !id
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            || alias.is_empty()
            || alias.len() > 192
            || prompt.is_empty()
            || prompt.len() > 32768
            || !model.contains('/')
        {
            return Err(Error::rejected("native task selectors refused"));
        }
        let params = serde_json::to_string(&json!({"model":model}))?;
        // No stored/restored endpoint/PID is adopted. This bounded fresh path
        // registers ONE new row and opens the actual root-owned launch channel.
        shared.store.register_agent(&crate::store::NewAgent {
            alias: &alias,
            provider: "pi",
            endpoint_kind: "managed",
            role: if crate::master::is_master(&alias) {
                "master"
            } else {
                "worker"
            },
            cwd: "/workspace",
            sandbox: "native-protected",
            instructions: None,
            params: Some(&params),
            team_role: None,
            model_policy: Some("provider_default"),
        })?;
        let agent = shared.store.agent(&alias)?;
        let part = Arc::new(Mutex::new(0u64));
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
        self.worker
            .join()
            .map_err(|_| Error::unknown("native task worker lost"))?
    }
}
fn emit(control: &UnixDatagram, task: &str, part: &Mutex<u64>, value: &Value) -> Result<()> {
    use base64::Engine;
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    let mut sequence = part
        .lock()
        .map_err(|_| Error::unknown("native stream poisoned"))?;
    for chunk in bytes.chunks(16384) {
        let index = *sequence;
        *sequence = sequence
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
