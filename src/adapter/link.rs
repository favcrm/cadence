//! Shared JSON-RPC request/response correlation for frame transports.
//!
//! Both `StdioAdapter` and `WsAdapter` multiplex outbound requests over
//! one connection and resolve them by response id. The rules live here
//! once so the transports cannot diverge: a send failure or timeout is
//! `OutcomeUnknown` (delivery is uncertain), never a silent retry.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Mutex};
use std::time::Duration;

use serde_json::Value;

use crate::error::{Error, Result};

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

/// Transport callback for non-response frames.
pub type MessageHandler = Box<dyn Fn(Incoming) + Send + Sync>;
/// Transport callback for connection loss.
pub type DisconnectHook = Box<dyn Fn() + Send + Sync>;

/// In-flight requests awaiting a correlated response.
pub struct Pending {
    map: Mutex<HashMap<u64, mpsc::Sender<Result<Value>>>>,
    next_id: AtomicU64,
}

impl Default for Pending {
    fn default() -> Self {
        Self {
            map: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(0),
        }
    }
}

impl Pending {
    pub fn new() -> Self {
        Self::default()
    }

    /// Resolve one response by id. Returns true when a waiter claimed it.
    pub fn resolve(&self, message: &Value) -> bool {
        let Some(id) = message.get("id").and_then(Value::as_u64) else {
            return false;
        };
        if let Some(target) = self.map.lock().unwrap().remove(&id) {
            let _ = target.send(Ok(message.clone()));
            return true;
        }
        false
    }

    /// Fail every waiter — the transport is gone and every outstanding
    /// request outcome is uncertain.
    pub fn fail_all(&self, reason: &str) {
        for (_, target) in self.map.lock().unwrap().drain() {
            let _ = target.send(Err(Error::unknown(reason)));
        }
    }

    /// One request/response round-trip with a bounded wait.
    pub fn request(
        &self,
        send: impl Fn(Value) -> Result<()>,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value> {
        let request_id = self.next_id.fetch_add(1, Ordering::SeqCst) + 1;
        let (tx, rx) = mpsc::channel();
        self.map.lock().unwrap().insert(request_id, tx);
        let sent = send(serde_json::json!({"id": request_id, "method": method, "params": params}));
        let result = match sent {
            Err(e) => Err(e),
            Ok(()) => match rx.recv_timeout(timeout) {
                Ok(inner) => inner,
                Err(mpsc::RecvTimeoutError::Timeout) => Err(Error::unknown(format!(
                    "No response to {method}; do not blindly retry"
                ))),
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    Err(Error::unknown("Provider connection lost"))
                }
            },
        };
        self.map.lock().unwrap().remove(&request_id);
        let response = result?;
        if let Some(error) = response.get("error") {
            return Err(Error::provider(error.to_string()));
        }
        Ok(response.get("result").cloned().unwrap_or(Value::Null))
    }
}
