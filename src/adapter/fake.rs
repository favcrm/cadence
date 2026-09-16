//! In-process test provider (`endpoint_kind = "fake"`).
//!
//! Drives the same [`ProviderAdapter`] contract as a real managed process
//! without model calls, so tests exercise queueing, routing, approvals and
//! failure fences end-to-end. It is a test fixture, not a provider; the
//! doctor output marks it accordingly.
//!
//! Prompt directives:
//! - `NEED_INPUT:<detail>` — emit a provider approval request and block
//!   until `agent respond` answers it, `interrupt` aborts it, or the
//!   answer deadline passes.
//! - `SLEEP:<secs>` — hold the turn; ignores `interrupt`, ends early only
//!   when the transport is closed (mirrors a provider that ignores
//!   cancellation but dies with its process).
//! - `DISCONNECT` — sever the transport; the turn returns `OutcomeUnknown`.
//! - `FAIL:<text>` — the provider reports a failed turn.
//! - `BAD_STATUS` — return an unclassifiable completion status.
//! - anything else — `FAKE_REPLY: <prompt>`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::{AdapterHooks, Identity, ProviderAdapter, ProviderRequest, TurnResult};
use crate::error::{Error, Result};
use crate::store::Agent;

const ANSWER_TIMEOUT: Duration = Duration::from_secs(120);

/// Answer channel payload: `Some` is a provider response, `None` is an
/// interrupt abort.
type AnswerTx = mpsc::Sender<Option<Value>>;

pub struct FakeAdapter {
    hooks: AdapterHooks,
    pending: Mutex<HashMap<String, AnswerTx>>,
    next_request: AtomicU64,
    next_turn: AtomicU64,
    disconnected: AtomicBool,
}

impl FakeAdapter {
    pub fn new(hooks: AdapterHooks) -> Self {
        Self {
            hooks,
            pending: Mutex::new(HashMap::new()),
            next_request: AtomicU64::new(0),
            next_turn: AtomicU64::new(0),
            disconnected: AtomicBool::new(false),
        }
    }

    fn turn_id(&self) -> String {
        format!(
            "fake-turn-{}",
            self.next_turn.fetch_add(1, Ordering::SeqCst) + 1
        )
    }
}

impl ProviderAdapter for FakeAdapter {
    fn open(&self, agent: &Agent) -> Result<Identity> {
        Ok(Identity {
            thread_id: format!("fake-thread-{}", agent.alias),
            session_id: format!("fake-session-{}", agent.alias),
            model: Some("fake-1".to_string()),
            pid: std::process::id(),
            endpoint: None,
            generation: None,
        })
    }

    fn run_turn(
        &self,
        prompt: &str,
        _client_message_id: &str,
        on_started: &dyn Fn(&str),
    ) -> Result<TurnResult> {
        let turn_id = self.turn_id();
        on_started(&turn_id);
        if prompt == "DISCONNECT" {
            self.disconnected.store(true, Ordering::SeqCst);
            return Err(Error::unknown(
                "Connection lost during turn; provider outcome is unknown",
            ));
        }
        if let Some(detail) = prompt.strip_prefix("FAIL:") {
            return Ok(TurnResult {
                turn_id,
                status: "failed".to_string(),
                text: String::new(),
                stop_reason: None,
                error: Some(detail.to_string()),
            });
        }
        if let Some(detail) = prompt.strip_prefix("NEED_INPUT:") {
            let request_no = self.next_request.fetch_add(1, Ordering::SeqCst) + 1;
            let request_id = format!("fake-req-{request_no}");
            let (tx, rx) = mpsc::channel();
            self.pending.lock().unwrap().insert(request_id.clone(), tx);
            (self.hooks.on_request)(ProviderRequest {
                id: json!(request_id),
                method: "item/commandExecution/requestApproval".to_string(),
                params: json!({"command": detail}),
            });
            let answer = match rx.recv_timeout(ANSWER_TIMEOUT) {
                Ok(answer) => answer,
                Err(_) => {
                    self.pending.lock().unwrap().remove(&request_id);
                    return Err(Error::unknown(
                        "Approval wait deadline reached; provider outcome needs review",
                    ));
                }
            };
            self.pending.lock().unwrap().remove(&request_id);
            let Some(answer) = answer else {
                // Interrupted while waiting for the decision.
                return Ok(TurnResult {
                    turn_id,
                    status: "interrupted".to_string(),
                    text: String::new(),
                    stop_reason: Some("interrupted".to_string()),
                    error: None,
                });
            };
            return Ok(TurnResult {
                turn_id,
                status: "completed".to_string(),
                text: format!("FAKE_DECIDED:{answer}"),
                stop_reason: None,
                error: None,
            });
        }
        if let Some(secs) = prompt
            .strip_prefix("SLEEP:")
            .and_then(|s| s.parse::<u64>().ok())
        {
            // Ignores interrupt; ends early only when the transport dies.
            let deadline = Instant::now() + Duration::from_secs(secs.min(300));
            while Instant::now() < deadline {
                if self.disconnected.load(Ordering::SeqCst) {
                    return Err(Error::unknown(
                        "Connection lost during turn; provider outcome is unknown",
                    ));
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            return Ok(TurnResult {
                turn_id,
                status: "completed".to_string(),
                text: "FAKE_SLEPT".to_string(),
                stop_reason: None,
                error: None,
            });
        }
        if prompt == "BAD_STATUS" {
            return Ok(TurnResult {
                turn_id,
                status: "melted".to_string(),
                text: String::new(),
                stop_reason: None,
                error: None,
            });
        }
        Ok(TurnResult {
            turn_id,
            status: "completed".to_string(),
            text: format!("FAKE_REPLY: {prompt}"),
            stop_reason: Some("end_turn".to_string()),
            error: None,
        })
    }

    fn respond(&self, request_id: &Value, result: Value) -> Result<()> {
        let id = request_id.as_str().unwrap_or_default().to_string();
        let target = self.pending.lock().unwrap().remove(&id);
        match target {
            Some(tx) => {
                let _ = tx.send(Some(result));
                Ok(())
            }
            None => Err(Error::rejected("Request is no longer pending")),
        }
    }

    fn interrupt(&self) {
        // Abort every outstanding provider request wait.
        for (_, tx) in self.pending.lock().unwrap().drain() {
            let _ = tx.send(None);
        }
    }

    fn disconnected(&self) -> bool {
        self.disconnected.load(Ordering::SeqCst)
    }

    fn close(&self) {
        // Mirror a dying provider: sever the transport and abort waits.
        self.disconnected.store(true, Ordering::SeqCst);
        self.interrupt();
    }
}
