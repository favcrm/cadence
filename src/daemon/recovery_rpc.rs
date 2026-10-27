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
    match path {
        Some(path) => with_note(
            e,
            &format!("the attempt was recorded at {}", path.display()),
        ),
        None => e,
    }
}

/// The error, with a note appended. The original structured code is
/// preserved: the note adds context, it does not change what happened.
fn with_note(e: Error, note: &str) -> Error {
    let msg = format!("{e} ({note})");
    let code = e.code().unwrap_or_default();
    match e.kind() {
        "conflict" => Error::conflict_coded(known_code(code, "recovery_failed"), msg),
        "busy" => Error::busy_coded(known_code(code, "resource_busy"), msg),
        "gate" => Error::gate_coded(known_code(code, "recovery_gate"), msg),
        "unknown" => Error::unknown(msg),
        "internal" => Error::internal(msg),
        _ => Error::rejected(msg),
    }
}

/// The stable codes this flow emits, as the `&'static str` the error
/// constructors require: a code that reached a caller must survive the
/// note, not degrade to a generic one.
fn known_code(code: &str, fallback: &'static str) -> &'static str {
    match code {
        "legacy_write_lock" => "legacy_write_lock",
        "stale_review" => "stale_review",
        "resource_busy" => "resource_busy",
        "recovery_gate" => "recovery_gate",
        "recovery_failed" => "recovery_failed",
        "recovery_evidence" => "recovery_evidence",
        _ => fallback,
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
        let recorded = match self.record_recovery_attempt(&attempt) {
            Ok(path) => Some(path),
            Err(record_error) => {
                // The evidence could not be finalized. A change that
                // landed without a record is NOT a success: the
                // tracker's state is the only account of what happened.
                let note = format!(
                    "the attempt could not be recorded, so there is no durable account of this \
                     execution ({record_error}); the tracker's state is the only record"
                );
                return Err(match outcome.error {
                    Some(e) => with_note(e, &note),
                    None => Error::internal(format!(
                        "the recovery was applied but {note}. Inspect the tracker's state with \
                         `cadence issue recover review` and settle the paths by hand; a retry is \
                         not safe before the state is inspected."
                    )),
                });
            }
        };
        match outcome.error {
            Some(e) => Err(with_attempt_path(e, recorded.as_deref())),
            None => Ok({
                let mut attempt = attempt;
                attempt["record"] = json!(recorded.as_deref().map(|p| p.display().to_string()));
                attempt
            }),
        }
    }

    /// Record the attempt under the daemon's state dir. The daemon owns
    /// that dir, so the record outlives the call that produced it.
    ///
    /// The name is unique per attempt and the file is created
    /// exclusively: two executions in the same second must never
    /// overwrite each other's record, and a truncated record would
    /// point at a different outcome and lose the preimages.
    fn record_recovery_attempt(&self, attempt: &Value) -> Result<std::path::PathBuf> {
        let dir = self.state_dir.join("recovery");
        std::fs::create_dir_all(&dir).map_err(|e| {
            Error::internal(format!(
                "the recovery attempt could not be recorded: its directory could not be created \
                 ({e}). The tracker's state is the only record of what happened."
            ))
        })?;
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        for attempt_no in 0..64u32 {
            let path = dir.join(format!("{nanos}-{}-{attempt_no}.json", std::process::id()));
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut file) => {
                    use std::io::Write;
                    file.write_all(attempt.to_string().as_bytes())
                        .map_err(|e| {
                            Error::internal(format!(
                                "the recovery attempt could not be written ({e}). The tracker's \
                             state is the only record of what happened."
                            ))
                        })?;
                    return Ok(path);
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => {
                    return Err(Error::internal(format!(
                        "the recovery attempt could not be recorded ({e}). The tracker's state \
                         is the only record of what happened."
                    )));
                }
            }
        }
        Err(Error::internal(
            "the recovery attempt could not be recorded: no unique record name was free. The \
             tracker's state is the only record of what happened."
                .to_string(),
        ))
    }

    /// `tracker_recovery_status` — the truthful outcome of a recorded
    /// attempt and the remaining blockers. Read-only.
    pub(super) fn rpc_tracker_recovery_status(
        &self,
        params: &Value,
        _peer_pid: u32,
    ) -> Result<Value> {
        // The status is computed from the daemon's OWN record: a caller
        // must not be able to hand in an attempt that claims success.
        let raw = required_str(params, "attempt_path")?;
        let root = self
            .state_dir
            .join("recovery")
            .canonicalize()
            .map_err(|e| {
                Error::rejected(format!(
                "the daemon has no recovery records to read ({e}); only an attempt it recorded \
                 can be inspected"
            ))
            })?;
        let path = std::path::Path::new(raw).canonicalize().map_err(|e| {
            Error::rejected(format!(
                "{raw} is not a readable recovery attempt ({e}); pass the path the daemon \
                 printed after `recover execute`"
            ))
        })?;
        if !path.starts_with(&root) {
            return Err(Error::rejected(format!(
                "{raw} is not one of the daemon's own recovery records; pass the path it \
                 printed after `recover execute`"
            )));
        }
        let text = std::fs::read_to_string(&path).map_err(|e| {
            Error::rejected(format!(
                "the recovery attempt {raw} could not be read ({e})"
            ))
        })?;
        let attempt: Value = serde_json::from_str(&text).map_err(|e| {
            Error::rejected(format!(
                "the recovery attempt {raw} is not valid JSON ({e})"
            ))
        })?;
        recovery::status(&self.pm()?, &attempt)
    }
}
