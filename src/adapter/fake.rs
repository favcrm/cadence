//! In-process test provider (`endpoint_kind = "fake"`).
//!
//! Drives the same [`ProviderAdapter`] contract as a real managed process
//! without model calls, so tests exercise queueing, routing, approvals and
//! failure fences end-to-end. It is a test fixture, not a provider; the
//! doctor output marks it accordingly.
//!
//! Prompt directives:
//! - `NEED_INPUT:<detail>` — emit a provider approval request and block
//!   until `agent respond` answers it; result echoes the decision.
//! - `DISCONNECT` — sever the transport; the turn returns `OutcomeUnknown`.
//! - `FAIL:<text>` — the provider reports a failed turn.
//! - anything else — `FAKE_REPLY: <prompt>`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

use super::{AdapterHooks, Identity, ProviderAdapter, ProviderRequest, TurnResult};
use crate::error::{Error, Result};
use crate::store::Agent;

const ANSWER_TIMEOUT: Duration = Duration::from_secs(120);

pub struct FakeAdapter {
    hooks: AdapterHooks,
    pending: Mutex<HashMap<String, mpsc::Sender<Value>>>,
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
            return Ok(TurnResult {
                turn_id,
                status: "completed".to_string(),
                text: format!("FAKE_DECIDED:{answer}"),
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
                let _ = tx.send(result);
                Ok(())
            }
            None => Err(Error::rejected("Request is no longer pending")),
        }
    }

    fn interrupt(&self) {}

    fn disconnected(&self) -> bool {
        self.disconnected.load(Ordering::SeqCst)
    }

    fn close(&self) {}
}
