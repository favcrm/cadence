//! `cadence mcp-permission` — a stdio MCP server backing Claude's
//! `--permission-prompt-tool`.
//!
//! A managed claude worker launched with `--broker-approvals` gets
//! `--mcp-config <generated> --strict-mcp-config
//! --permission-prompt-tool mcp__cadence__approve`; every tool call the
//! permission mode can't pre-decide lands here as a `tools/call` for the
//! single `approve` tool. The server records the prompt as a durable
//! Cadence request (`request_open`), blocks on `request_wait` for the
//! operator's `agent respond` decision, and answers the CLI with the
//! wire shape it expects — `{"behavior":"allow","updatedInput":{…}}` or
//! `{"behavior":"deny","message":"…"}` inside a text content block.
//!
//! Verified against `claude` 2.1.277: newline-delimited JSON-RPC 2.0
//! over stdio (no Content-Length framing); `initialize` →
//! `notifications/initialized` → `tools/list` → `tools/call` whose
//! `arguments` carry `tool_name`, `input` and `tool_use_id`.
//!
//! Identity comes from `CADENCE_ALIAS`/`CADENCE_STATE_DIR` in the
//! environment (the generated mcp-config sets both explicitly). The
//! alias only names the agent: the daemon accepts these request RPCs
//! solely from that agent's own connection — this server runs as a
//! child of the brokered provider, whose enrollment proves it (CAD-376).
//! `CADENCE_PERMISSION_TIMEOUT_SECS` bounds the wait — expiry denies
//! rather than hanging the turn. A daemon restart mid-wait is retried
//! until the deadline, then denied: the pending map is in-memory, so a
//! restarted daemon reports the request `closed`.

use std::io::{BufRead, Write};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::client;
use crate::error::{Error, Result};

/// The one tool this server exposes; the CLI resolves it as
/// `mcp__<server-name>__approve`.
const TOOL: &str = "approve";
/// Default decision deadline — `--permission-timeout-secs` lands in
/// `CADENCE_PERMISSION_TIMEOUT_SECS` via the generated config.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(900);
/// One `request_wait` bounds a single socket call; the deadline loop
/// re-issues until answered or expired so a daemon restart surfaces
/// instead of hanging one read forever.
const WAIT_SLICE_SECS: u64 = 60;
/// Backoff between retries while the daemon socket is unreachable
/// (restart window).
const RETRY: Duration = Duration::from_millis(500);

/// Serve MCP over stdio until stdin closes.
pub fn run(timeout_secs: Option<u64>) -> Result<i32> {
    let alias = std::env::var("CADENCE_ALIAS").ok();
    let state_dir = client::state_dir().ok();
    let timeout = timeout_secs
        .or_else(|| {
            std::env::var("CADENCE_PERMISSION_TIMEOUT_SECS")
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
        })
        .map(|s| Duration::from_secs(s.max(1)))
        .unwrap_or(DEFAULT_TIMEOUT);
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        // Notifications carry no id — only requests get answers.
        let Some(id) = msg.get("id").cloned() else {
            continue;
        };
        let method = msg
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let response = match method {
            "initialize" => json!({"jsonrpc": "2.0", "id": id, "result": {
                // Echo the client's version — the probe showed
                // claude-code 2.1.277 offering "2025-11-25".
                "protocolVersion": msg
                    .pointer("/params/protocolVersion")
                    .and_then(Value::as_str)
                    .unwrap_or("2025-11-25"),
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "cadence",
                               "version": env!("CARGO_PKG_VERSION")}}}),
            "tools/list" => json!({"jsonrpc": "2.0", "id": id, "result": {
            "tools": [{
                "name": TOOL,
                "description": "Ask the cadence operator to approve a tool call",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "tool_name": {"type": "string"},
                        "input": {"type": "object"},
                        "tool_use_id": {"type": "string"},
                    },
                    "required": ["tool_name", "input"],
                }}]}}),
            "tools/call" => handle_call(&msg, alias.as_deref(), state_dir.as_deref(), timeout, id),
            _ => json!({"jsonrpc": "2.0", "id": id,
                        "error": {"code": -32601, "message": "method not found"}}),
        };
        if writeln!(out, "{response}")
            .and_then(|()| out.flush())
            .is_err()
        {
            break;
        }
    }
    Ok(0)
}

/// One `tools/call`: only `approve` exists — anything else is an
/// MCP-level invalid-params error.
fn handle_call(
    msg: &Value,
    alias: Option<&str>,
    state_dir: Option<&std::path::Path>,
    timeout: Duration,
    id: Value,
) -> Value {
    let params = &msg["params"];
    if params.get("name").and_then(Value::as_str) != Some(TOOL) {
        return json!({"jsonrpc": "2.0", "id": id,
                      "error": {"code": -32602,
                                "message": "unknown tool — only 'approve' exists"}});
    }
    let args = &params["arguments"];
    let tool = args
        .get("tool_name")
        .and_then(Value::as_str)
        .unwrap_or("tool");
    let input = args.get("input").cloned().unwrap_or(json!({}));
    let verdict = decide(alias, state_dir, tool, &input, timeout);
    // The CLI reads a JSON text payload out of the content block —
    // the shape the probe confirmed for both allow and deny.
    json!({"jsonrpc": "2.0", "id": id, "result": {
        "content": [{"type": "text", "text": verdict.to_string()}]}})
}

/// Open the request and wait out the deadline for the operator's
/// decision; every failure mode lands on `deny` — a hung permission
/// prompt must never wedge the provider.
fn decide(
    alias: Option<&str>,
    state_dir: Option<&std::path::Path>,
    tool: &str,
    input: &Value,
    timeout: Duration,
) -> Value {
    let deny = |message: &str| json!({"behavior": "deny", "message": message});
    let (Some(alias), Some(dir)) = (alias, state_dir) else {
        return deny("cadence identity env missing (CADENCE_ALIAS/CADENCE_STATE_DIR)");
    };
    let deadline = Instant::now() + timeout;
    // Minted once per tool call and sent with the open: a retry after
    // a lost response re-opens the same handle — the daemon dedupes,
    // so a transport hiccup can never double-notify the PM.
    let handle = format!("perm-{}", uuid::Uuid::new_v4().simple());
    // `request_open` retries only on transport errors — a rejection
    // (unknown alias, not brokered) is final and denies immediately.
    let handle = loop {
        if Instant::now() >= deadline {
            return deny("timed out reaching the cadence daemon");
        }
        match client::rpc(
            dir,
            "request_open",
            json!({"alias": alias, "kind": "approval", "tool": tool,
                   "request": handle,
                   "input_summary": summarize(input), "input": input}),
        ) {
            Ok(v) => break v["request"].as_str().unwrap_or_default().to_string(),
            Err(Error::Rejected(m)) => return deny(&m),
            Err(_) => std::thread::sleep(RETRY),
        }
    };
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            // Local deadline: retire the server-side request so the
            // agent leaves waiting_input — a decision parked at the
            // boundary is still honored.
            if let Ok(v) = client::rpc(dir, "request_close", json!({"request": handle})) {
                if v["state"].as_str() == Some("answered") {
                    return verdict(&v["answer"], input);
                }
            }
            return deny("permission request timed out waiting on an operator decision");
        }
        match client::rpc(
            dir,
            "request_wait",
            json!({"request": handle,
                   "wait": WAIT_SLICE_SECS.min(remaining.as_secs().max(1))}),
        ) {
            Ok(v) => match v["state"].as_str() {
                Some("answered") => return verdict(&v["answer"], input),
                Some("closed") => {
                    let reason = v["reason"].as_str().unwrap_or("request closed");
                    return deny(&format!("permission request closed: {reason}"));
                }
                // "waiting" — re-issue until the deadline.
                _ => continue,
            },
            Err(Error::Rejected(m)) => return deny(&m),
            // Socket down/refused — a daemon restart is retried until
            // the deadline, then denied above.
            Err(_) => std::thread::sleep(RETRY),
        }
    }
}

/// The operator's answer → the provider's wire verdict: accept allows
/// with the original input; decline denies carrying the reason.
fn verdict(answer: &Value, input: &Value) -> Value {
    if answer["decision"].as_str() == Some("accept") {
        json!({"behavior": "allow", "updatedInput": input})
    } else {
        let reason = answer["reason"].as_str().unwrap_or("declined by operator");
        json!({"behavior": "deny", "message": reason})
    }
}

/// The one-line summary `agent requests` shows: Bash's command when it
/// has one, else the input compacted to a bounded single line.
fn summarize(input: &Value) -> String {
    if let Some(cmd) = input.get("command").and_then(Value::as_str) {
        return cmd.chars().take(240).collect();
    }
    let compact = input.to_string();
    compact.chars().take(240).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verdict_maps_decisions_to_wire_shapes() {
        let input = json!({"command": "ls"});
        let accept = verdict(&json!({"decision": "accept"}), &input);
        assert_eq!(accept["behavior"], "allow");
        assert_eq!(accept["updatedInput"], input);
        let decline = verdict(&json!({"decision": "decline", "reason": "no"}), &input);
        assert_eq!(decline["behavior"], "deny");
        assert_eq!(decline["message"], "no");
        let bare = verdict(&json!({"decision": "decline"}), &input);
        assert_eq!(bare["message"], "declined by operator");
    }

    #[test]
    fn summarize_prefers_command_and_bounds_length() {
        assert_eq!(
            summarize(&json!({"command": "git status", "other": 1})),
            "git status"
        );
        let long = "x".repeat(500);
        assert_eq!(summarize(&json!({"command": long})).len(), 240);
        assert!(summarize(&json!({"a": 1})).contains("\"a\":1"));
    }
}
