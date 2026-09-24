//! `/api/threads/<alias>` — the operator's chat with an agent over its
//! durable thread (CAD-319). The board holds no thread state: every
//! route is a daemon RPC (`thread_read`, `thread_send`), like monitor
//! acks, so a board built from a newer binary stays compatible with an
//! older live daemon (a missing method answers 501).
//!
//! - `GET  /api/threads/<alias>?after=<seq>&limit=<n>` — one page;
//!   `?tail=1` or `?before=<seq>` read the newest page / the page below
//!   a seq instead (`more_before` says whether older entries remain).
//! - `GET  /api/threads/<alias>/stream[?after=<seq>]` — server-sent
//!   events, one `entry` frame per entry with `id: <seq>`; a reconnect
//!   resumes from `Last-Event-ID` (preferred) or `after`.
//! - `POST /api/threads/<alias>/messages` `{"text", "message"?}` — the
//!   operator's message, queued to the agent like `cadence send`.
//!
//! The POST instructs an agent. It passes the board's write guards, and
//! a caller the board attributes to an agent (a pane, or a managed
//! endpoint's tool process — CAD-335 phase 1) is refused here; the
//! daemon refuses an agent connection again on its side. A caller tied
//! to no agent is the operator by DEFAULT, not by positive proof — the
//! same gap every board write has until CAD-313 lands operator identity
//! for the web UI.

use std::io::Write;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{json, Value};
use tiny_http::Request;

use super::{
    err_response, guard_fail, header_value, json_response, parse_json, read_body, write_caller,
    write_guard, HttpResp, ServeOpts, WriteCaller,
};
use crate::client;
use crate::error::Error;

/// A chat message body is at most the queue's 48 000 bytes plus JSON.
const MESSAGE_CAP: u64 = 64 * 1024;
/// Seconds one stream long-poll waits on the daemon before a keepalive.
const STREAM_WAIT: u64 = 15;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ThreadMessageReq {
    text: String,
    /// Client-chosen id: a retried POST is the same message, not two.
    message: Option<String>,
}

/// Alias grammar checked before it reaches the daemon — the same one
/// `/api/agents/<alias>` uses.
fn valid_alias(alias: &str) -> bool {
    !alias.is_empty()
        && alias.len() <= 80
        && alias
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

/// `(alias, sub)` for `/api/threads/<alias>[/<sub>]`, `None` when the
/// path is not a thread route at all.
pub(super) fn route(path: &str) -> Option<(&str, Option<&str>)> {
    let tail = path.strip_prefix("/api/threads/")?;
    let mut segs = tail.splitn(2, '/');
    let alias = segs.next().unwrap_or_default();
    Some((alias, segs.next()))
}

/// Daemon error → HTTP: unreachable 503, an older daemon without the
/// method 501, an unknown agent 404, any other refusal 400.
fn rpc_err(e: &Error) -> HttpResp {
    let text = e.to_string();
    if text.starts_with("Daemon is not reachable") {
        return err_response(503, &text);
    }
    if text.contains("Unknown method 'thread_") {
        return err_response(501, "this daemon does not support threads");
    }
    if text.contains("Unknown managed agent") {
        return err_response(404, &text);
    }
    match e {
        Error::Internal(_) => err_response(500, &text),
        _ => err_response(400, &text),
    }
}

fn cursor_arg(raw: Option<String>, what: &str) -> std::result::Result<i64, HttpResp> {
    match raw {
        None => Ok(0),
        Some(v) => v
            .trim()
            .parse::<i64>()
            .ok()
            .filter(|n| *n >= 0)
            .ok_or_else(|| err_response(400, &format!("{what} must be a nonnegative integer"))),
    }
}

/// `GET /api/threads/<alias>` — one page after `after`.
pub(super) fn read(
    state_dir: &std::path::Path,
    alias: &str,
    query: &dyn Fn(&str) -> Option<String>,
) -> HttpResp {
    if !valid_alias(alias) {
        return err_response(400, "bad agent alias");
    }
    let after = match cursor_arg(query("after"), "after") {
        Ok(after) => after,
        Err(resp) => return resp,
    };
    let limit = match query("limit") {
        None => 100,
        Some(v) => match v.trim().parse::<i64>() {
            Ok(n) if (1..=crate::store::THREAD_PAGE_MAX).contains(&n) => n,
            _ => {
                return err_response(
                    400,
                    &format!("limit must be 1-{}", crate::store::THREAD_PAGE_MAX),
                )
            }
        },
    };
    // CAD-328: `?tail=1` (the newest page) or `?before=<seq>` read
    // backwards — a chat view opens on its latest entries.
    let tail = matches!(query("tail").as_deref(), Some("1" | "true"));
    let before = match query("before") {
        None => None,
        Some(v) => match v.trim().parse::<i64>() {
            Ok(n) if n >= 1 => Some(n),
            _ => return err_response(400, "before must be a positive seq"),
        },
    };
    let params = if tail || before.is_some() {
        if query("after").is_some() {
            return err_response(400, "after cannot be combined with tail or before");
        }
        let mut p = json!({"alias": alias, "limit": limit});
        match before {
            Some(b) => p["before"] = json!(b),
            None => p["tail"] = json!(true),
        }
        p
    } else {
        json!({"alias": alias, "after": after, "limit": limit})
    };
    match client::rpc(state_dir, "thread_read", params) {
        Ok(page) => json_response(page),
        Err(e) => rpc_err(&e),
    }
}

/// `GET /api/threads/<alias>/stream` — SSE, written straight onto the
/// socket like `/api/stream` (tiny_http buffers small chunked writes).
/// Each daemon long-poll returns the next entries or times out into a
/// `: ping`; a failed write is the client hanging up. Resumes after
/// `Last-Event-ID`, else `?after=`, else from the start.
pub(super) fn stream(
    request: Request,
    state_dir: &std::path::Path,
    alias: &str,
    query: &dyn Fn(&str) -> Option<String>,
) {
    if !valid_alias(alias) {
        let _ = request.respond(err_response(400, "bad agent alias"));
        return;
    }
    let resume = header_value(&request, "Last-Event-ID").or_else(|| query("after"));
    let mut cursor = match cursor_arg(resume, "Last-Event-ID / after") {
        Ok(cursor) => cursor,
        Err(resp) => {
            let _ = request.respond(resp);
            return;
        }
    };
    // Fail before the stream opens when the alias or daemon is wrong —
    // a 404/501/503 beats an event stream that only ever errors.
    if let Err(e) = client::rpc(
        state_dir,
        "thread_read",
        json!({"alias": alias, "after": cursor, "limit": 1}),
    ) {
        let _ = request.respond(rpc_err(&e));
        return;
    }
    let mut w = request.into_writer();
    let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                Cache-Control: no-store\r\nConnection: close\r\n\
                X-Content-Type-Options: nosniff\r\n\r\n";
    let frame = |w: &mut dyn Write, bytes: &[u8]| -> bool {
        w.write_all(bytes).and_then(|_| w.flush()).is_ok()
    };
    if !frame(&mut w, head.as_bytes()) || !frame(&mut w, b": ping\n\n") {
        return;
    }
    loop {
        let started = Instant::now();
        match client::rpc(
            state_dir,
            "thread_read",
            json!({"alias": alias, "after": cursor, "limit": 100, "wait": STREAM_WAIT}),
        ) {
            Ok(page) => {
                let entries = page["entries"].as_array().cloned().unwrap_or_default();
                if entries.is_empty() && !frame(&mut w, b": ping\n\n") {
                    return;
                }
                for entry in entries {
                    let Some(seq) = entry["seq"].as_i64() else {
                        continue;
                    };
                    let bytes = format!("id: {seq}\nevent: entry\ndata: {entry}\n\n");
                    if !frame(&mut w, bytes.as_bytes()) {
                        return;
                    }
                    cursor = cursor.max(seq);
                }
            }
            Err(e) => {
                let data = json!({"error": e.to_string()});
                if !frame(&mut w, format!("event: error\ndata: {data}\n\n").as_bytes()) {
                    return;
                }
                // A down daemon answers at once — never spin on it.
                let spent = started.elapsed();
                if spent < Duration::from_secs(2) {
                    std::thread::sleep(Duration::from_secs(2) - spent);
                }
            }
        }
    }
}

/// `POST /api/threads/<alias>/messages` — the operator's chat message.
/// Guards first (read-only, the cross-site write guards, caller
/// attribution), then the body, then the daemon.
pub(super) fn post_message(
    request: &mut Request,
    state_dir: &std::path::Path,
    opts: &ServeOpts,
    alias: &str,
) -> HttpResp {
    if !valid_alias(alias) {
        return err_response(400, "bad agent alias");
    }
    if opts.read_only {
        return guard_fail("read_only", "board is read-only — writes are disabled");
    }
    if let Err(resp) = write_guard(request, "application/json", opts) {
        return resp;
    }
    match write_caller(request, state_dir, opts) {
        Ok(WriteCaller::Operator(_)) => {}
        Ok(WriteCaller::Agent(agent)) => {
            return guard_fail(
                "caller_agent",
                &format!(
                    "thread message refused: this request comes from agent '{agent}' — \
                     only the operator writes into an agent's chat; agents message \
                     each other with `cadence send`"
                ),
            )
        }
        Err(resp) => return resp,
    }
    let bytes = match read_body(request, MESSAGE_CAP) {
        Ok(bytes) => bytes,
        Err(resp) => return resp,
    };
    let req: ThreadMessageReq = match parse_json(&bytes) {
        Ok(req) => req,
        Err(resp) => return resp,
    };
    let mut params = json!({"alias": alias, "text": req.text});
    if let Some(message) = req.message {
        params["message"] = Value::String(message);
    }
    match client::rpc(state_dir, "thread_send", params) {
        Ok(receipt) => json_response(receipt),
        Err(e) => rpc_err(&e),
    }
}
