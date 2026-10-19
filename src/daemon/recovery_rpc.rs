//! CAD-1601: `cadence daemon` tracker-recovery RPC handlers. Review and
//! status are read-only; decide and execute are operator actions gated by
//! `operator_connection` before any other work. The core lives in
//! `crate::issue::recovery`; these handlers only parse and forward.

use super::*;
use crate::issue::recovery::{self, Choice};

const DECIDE_VERB: &str = "tracker recovery decide";
const EXECUTE_VERB: &str = "tracker recovery execute";

/// Name the recorded attempt in the refusal, without changing what the
/// refusal IS: the kind still decides the exit code, so a caller that
/// branches on it (busy is retryable, conflict is not) still branches
/// correctly.
fn with_attempt_path(e: Error, path: Option<&std::path::Path>) -> Error {
    let Some(path) = path else {
        return e;
    };
    let msg = format!("{e} (the attempt was recorded at {})", path.display());
    match e.kind() {
        "conflict" => Error::conflict_coded("recovery_failed", msg),
        "busy" => Error::busy_coded("resource_busy", msg),
        "gate" => Error::gate_coded("recovery_gate", msg),
        "internal" => Error::internal(msg),
        _ => Error::rejected(msg),
    }
}

/// A required object param (a JSON blob passed through to the core).
fn required_object<'a>(params: &'a Value, field: &str) -> Result<&'a Value> {
    match params.get(field) {
        Some(value) if value.is_object() => Ok(value),
        Some(_) => Err(Error::rejected(format!(
            "tracker recovery: '{field}' must be a JSON object; pass the file \
             printed by the previous recovery step unchanged, then retry"
        ))),
        None => Err(Error::rejected(format!(
            "tracker recovery: missing required parameter '{field}'; pass the \
             JSON object printed by the previous recovery step, then retry"
        ))),
    }
}

/// A required array of literal path strings. Every item must be a string;
/// the core validates the paths themselves.
fn required_path_list(params: &Value, field: &str) -> Result<Vec<String>> {
    let items = params.get(field).and_then(Value::as_array).ok_or_else(|| {
        Error::rejected(format!(
            "tracker recovery: missing or non-array parameter '{field}'; pass \
             at least one literal repo-relative path, then retry"
        ))
    })?;
    items
        .iter()
        .map(|v| {
            v.as_str().map(str::to_string).ok_or_else(|| {
                Error::rejected(format!(
                    "tracker recovery: every item in '{field}' must be a string; \
                     pass literal repo-relative paths, then retry"
                ))
            })
        })
        .collect()
}

/// The required `choice` string, parsed by the core's own vocabulary.
fn required_choice(params: &Value) -> Result<Choice> {
    let raw = required_str(params, "choice").map_err(|_| {
        Error::rejected(
            "tracker recovery: missing required parameter 'choice'; use keep, \
             discard or leave, then retry",
        )
    })?;
    Choice::parse(raw)
}

impl Shared {
    /// `tracker_recovery_review` — the complete, read-only review of an
    /// interrupted tracker write. No operator gate: it changes nothing.
    pub(super) fn rpc_tracker_recovery_review(
        &self,
        params: &Value,
        _peer_pid: u32,
    ) -> Result<Value> {
        let blocked_operation = optional_str(params, "blocked_operation");
        recovery::review(&self.pm()?, blocked_operation)
    }

    /// `tracker_recovery_decide` — the positively authorized decision
    /// preview bound to a reviewed state. Operator only, checked first.
    pub(super) fn rpc_tracker_recovery_decide(
        &self,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        self.operator_connection(DECIDE_VERB, params, peer_pid)?;
        let review = required_object(params, "review")?;
        let choice = required_choice(params)?;
        let paths = required_path_list(params, "paths")?;
        recovery::decide(&self.pm()?, review, choice, &paths)
    }

    /// `tracker_recovery_execute` — the one mutation boundary. Operator
    /// only, checked first; the core re-checks the state binding under the
    /// real tracker lock.
    pub(super) fn rpc_tracker_recovery_execute(
        &self,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        self.operator_connection(EXECUTE_VERB, params, peer_pid)?;
        let review = required_object(params, "review")?;
        let decision = required_object(params, "decision")?;
        let choice = required_choice(params)?;
        let paths = required_path_list(params, "paths")?;
        let outcome = recovery::execute_detailed(&self.pm()?, review, decision, choice, &paths);
        // The attempt records WHO made it: the daemon's positive
        // operator proof for THIS connection, never a caller-supplied
        // name. No token, no argv, no environment.
        let mut attempt = outcome.attempt;
        attempt["actor"] = json!({
            "kind": "operator",
            "proof": "operator_connection for this connection",
            "peer_pid": peer_pid,
        });
        // The attempt is recorded either way, so a FAILED execution is
        // still confirmable afterwards: `recover status --attempt <file>`
        // reads it back. The refusal keeps its own kind, so the exit
        // code still says whether a retry can help.
        let recorded = self.record_recovery_attempt(&attempt);
        match outcome.error {
            Some(e) => Err(with_attempt_path(e, recorded.as_deref())),
            None => Ok(attempt),
        }
    }

    /// Record the attempt under the daemon's state dir. The daemon owns
    /// that dir, so the record outlives the call that produced it.
    fn record_recovery_attempt(&self, attempt: &Value) -> Option<std::path::PathBuf> {
        let dir = self.state_dir.join("recovery");
        std::fs::create_dir_all(&dir).ok()?;
        let name = format!(
            "{}-{}.json",
            crate::issue::time::basic(crate::issue::time::now_epoch()),
            std::process::id()
        );
        let path = dir.join(name);
        std::fs::write(&path, attempt.to_string()).ok()?;
        Some(path)
    }

    /// `tracker_recovery_status` — the truthful outcome of a recorded
    /// attempt and the remaining blockers. Read-only.
    pub(super) fn rpc_tracker_recovery_status(
        &self,
        params: &Value,
        _peer_pid: u32,
    ) -> Result<Value> {
        let attempt = required_object(params, "attempt")?;
        recovery::status(&self.pm()?, attempt)
    }
}
