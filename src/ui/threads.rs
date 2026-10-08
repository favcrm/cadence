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
//! - `GET  /api/app-installations/<id>/conversations` — the master's
//!   conversations for that installation (CAD-1098; operator-gated in
//!   serve.rs): `{alias, install_id, general, conversations: [...]}`;
//!   an unknown installation is 404. There is no `/api/threads/...`
//!   GET for it.
//! - `POST /api/threads/<alias>/conversations` `{"install_id",
//!   "context_id", "subject"?, "general"?}` — make or find a
//!   conversation (operator-only, like the messages POST); answers
//!   `{conversation, created}`.
//! - `?conversation=<id>` on the thread read and stream selects that
//!   conversation's entries (an unknown id is 404; home when absent).
//! - `POST /api/threads/<alias>/messages` `{"text", "message"?, "app"?,
//!   "conversation"?}` —
//!   the operator's message, queued to the agent like `cadence send`.
//!   `app` (`{install_id, context_id}`, CAD-802) is never authority:
//!   the daemon proves both against its store and stamps the verified
//!   binding on the entry.
//!
//! The POST instructs an agent. It passes the board's write guards, and
//! a caller the board attributes to an agent (a pane, or a managed
//! endpoint's tool process — CAD-335 phase 1) is refused here; the
//! daemon refuses an agent connection again on its side. A caller tied
//! to no agent is the operator by DEFAULT, not by positive proof — the
//! same gap every board write has until CAD-313 lands operator identity
//! for the web UI.

use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{json, Value};
use tiny_http::Request;

use super::{
    busy_response, err_response, header_value, json_response, parse_json, read_body, write_err,
    HttpResp,
};
use crate::client;
use crate::error::Error;

/// A chat message body is at most the queue's 48 000 bytes plus JSON.
const MESSAGE_CAP: u64 = 64 * 1024;
/// Seconds one stream long-poll waits on the daemon before a keepalive.
const STREAM_WAIT: u64 = 15;

/// Maximum simultaneous multipart bodies held by this board process.
/// This bounds this HTTP ingress only; it is not a host-wide job limit.
const CHAT_UPLOAD_INGRESS_MAX: usize = 2;
static CHAT_UPLOAD_INGRESS: AtomicUsize = AtomicUsize::new(0);

/// RAII admission for a board upload body. Acquired before `read_body` and
/// retained until the request has removed its owned staging file.
pub(crate) struct ChatUploadIngressPermit;

impl ChatUploadIngressPermit {
    pub(crate) fn try_acquire() -> Option<Self> {
        let mut active = CHAT_UPLOAD_INGRESS.load(Ordering::Acquire);
        loop {
            if active >= CHAT_UPLOAD_INGRESS_MAX {
                return None;
            }
            match CHAT_UPLOAD_INGRESS.compare_exchange_weak(
                active,
                active + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Some(Self),
                Err(observed) => active = observed,
            }
        }
    }
}

impl Drop for ChatUploadIngressPermit {
    fn drop(&mut self) {
        CHAT_UPLOAD_INGRESS.fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ThreadMessageReq {
    text: String,
    /// Client-chosen id: a retried POST is the same message, not two.
    message: Option<String>,
    /// CAD-802: the shell chat's current installation/context. Never
    /// authority — the daemon resolves both against its store and
    /// refuses unknown installs, unknown/archived contexts and any
    /// extra key (including a browser-stamped `verified`).
    app: Option<ThreadApp>,
    /// CAD-574: needs-me subjects the message cites (the rail's "Ask
    /// master"). The daemon's `thread_refs` is the strict side — this
    /// field only carries `{kind,id}` pairs through the relay.
    refs: Option<Vec<ThreadRef>>,
    /// CAD-1098: a selector among the conversations of `app`'s
    /// installation — never authority; the daemon checks it.
    conversation: Option<String>,
    /// CAD-1168: retained chat-file ids from `/api/chat/upload`. The
    /// daemon's `thread_attachments` resolves them — this relay only
    /// carries `{id}` handles through, never names or paths.
    attachments: Option<Vec<ThreadAttachment>>,
}

/// A retained chat-file handle — the daemon re-validates the grammar
/// and existence; the board never dereferences it to a path.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ThreadAttachment {
    id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConversationReq {
    install_id: Option<String>,
    context_id: Option<String>,
    subject: Option<String>,
    general: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ThreadRef {
    kind: String,
    id: String,
}

/// CAD-802: the shell chat's current App. The shape alone travels
/// here — the daemon proves the installation and context against its
/// own store and stamps the verified binding on the entry.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ThreadApp {
    install_id: String,
    /// Absent for an installation-only binding (no context selected).
    context_id: Option<String>,
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
    if text.contains("Unknown method 'thread_") || text.contains("Unknown method 'conversation_") {
        return err_response(501, "this daemon does not support threads");
    }
    if text.contains("Unknown managed agent")
        || text.contains("Unknown conversation")
        || text.contains("Unknown app installation")
    {
        return err_response(404, &text);
    }
    if let Some(resp) = busy_response(e) {
        return resp;
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
    let mut params = if tail || before.is_some() {
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
    if let Some(conversation) = query("conversation") {
        params["conversation"] = Value::String(conversation);
    }
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
    // CAD-1098: the stream follows one conversation when named.
    let conversation = query("conversation");
    let with_conversation = |mut p: Value| {
        if let Some(id) = &conversation {
            p["conversation"] = Value::String(id.clone());
        }
        p
    };
    // Fail before the stream opens when the alias or daemon is wrong —
    // a 404/501/503 beats an event stream that only ever errors.
    if let Err(e) = client::rpc(
        state_dir,
        "thread_read",
        with_conversation(json!({"alias": alias, "after": cursor, "limit": 1})),
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
            with_conversation(
                json!({"alias": alias, "after": cursor, "limit": 100, "wait": STREAM_WAIT}),
            ),
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
/// Operator-only: `operator::admit` has already run the guards, the
/// operator session and the process proof on the peer (CAD-313) — an
/// agent caller is refused there (`operator_only`); agents message each
/// other with `cadence send`.
pub(super) fn post_message(
    request: &mut Request,
    state_dir: &std::path::Path,
    alias: &str,
) -> HttpResp {
    if !valid_alias(alias) {
        return err_response(400, "bad agent alias");
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
    if let Some(refs) = req.refs {
        params["refs"] = json!(refs
            .iter()
            .map(|r| json!({"kind": r.kind, "id": r.id}))
            .collect::<Vec<_>>());
    }
    if let Some(app) = req.app {
        params["app"] = match app.context_id {
            Some(context) => json!({"install_id": app.install_id, "context_id": context}),
            None => json!({"install_id": app.install_id}),
        };
    }
    if let Some(conversation) = req.conversation {
        params["conversation"] = Value::String(conversation);
    }
    if let Some(attachments) = req.attachments {
        params["attachments"] = json!(attachments
            .iter()
            .map(|a| json!({"id": a.id}))
            .collect::<Vec<_>>());
    }
    match client::rpc(state_dir, "thread_send", params) {
        Ok(receipt) => json_response(receipt),
        Err(e) => rpc_err(&e),
    }
}

/// The multipart selector fields the chat upload admits beside `file`:
/// the display hint `name`/`filename` and the explicit app scope
/// `install_id`, `context_id`, `conversation`. Anything else — a
/// caller-supplied `scope`, `sha256`, `path`, actor or digest field —
/// refuses the whole request; the daemon re-proves every value, and a
/// partial app selector (install without conversation, or the reverse)
/// is refused here before any staging.
fn valid_app_selector(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

const UPLOAD_SELECTOR_FIELDS: &[&str] = &[
    "name",
    "filename",
    "install_id",
    "context_id",
    "conversation",
];

/// `POST /api/chat/upload` — CAD-1168: the composer's retained-file
/// upload. The board admits it OperatorOnly (WRITE_ROUTES), streams the
/// multipart `file` part into the daemon's upload-staging dir under an
/// exclusively created canonical name and relays `chat_file_upload` —
/// the daemon re-hashes, caps, sniffs and retains. The caller's
/// filename is a display hint only; no scope, digest or path field is
/// forwarded. An explicit app selector (`install_id`, `conversation`,
/// optional `context_id`) rides through as the daemon's `app` +
/// `conversation` params — never as authority: the daemon proves the
/// installation, the context, the conversation and the current
/// exact-approved declaration again. The minted name is created with
/// `create_new` before it is written or claimed: an occupied name is
/// refused, never truncated or deleted, and only a path this request
/// created is cleaned up. The response carries the daemon's stored
/// row — no filesystem path, cookie or raw URL.
pub(super) fn post_upload(request: &mut Request, state_dir: &Path) -> HttpResp {
    let Some(_ingress) = ChatUploadIngressPermit::try_acquire() else {
        return err_response(429, "chat upload ingress is busy; retry shortly");
    };
    let ct = header_value(request, "Content-Type").unwrap_or_default();
    let Some(boundary) = ct
        .trim()
        .strip_prefix("multipart/form-data; boundary=")
        .map(str::trim)
        .map(|b| b.trim_matches('"'))
    else {
        return err_response(400, "chat upload needs a multipart/form-data boundary");
    };
    if boundary.is_empty() || boundary.len() > 100 {
        return err_response(400, "bad multipart boundary");
    }
    // The cap is the daemon's own plus the multipart envelope — refuse
    // here before any daemon call.
    let cap = crate::store::CHAT_FILE_MAX_BYTES.saturating_add(64 * 1024);
    let body = match read_body(request, cap) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let (fields, file) = match super::parse_multipart(&body, boundary) {
        Ok(v) => v,
        Err(why) => return err_response(400, &why),
    };
    let Some(bytes) = file else {
        return err_response(400, "chat upload needs a 'file' part");
    };
    if bytes.is_empty() {
        return err_response(400, "chat upload: the file part is empty");
    }
    // Unknown selector fields refuse whole, never silently dropped: a
    // browser-supplied scope, digest, path or actor field is not a
    // transport field here.
    if let Some(field) = fields
        .keys()
        .find(|k| !UPLOAD_SELECTOR_FIELDS.contains(&k.as_str()))
    {
        return err_response(400, &format!("chat upload field '{field}' is not accepted"));
    }
    // Selector presence is meaningful: only genuinely absent fields
    // select legacy Home. Present-empty, malformed, or partial app
    // intent is refused before staging rather than normalized to Home.
    let install = match fields.get("install_id") {
        None => None,
        Some(value) if valid_app_selector(value) => Some(value),
        Some(_) => return err_response(400, "bad chat upload install_id"),
    };
    let conversation = match fields.get("conversation") {
        None => None,
        Some(value) if crate::proto::identifier(value, "conversation ID").is_ok() => Some(value),
        Some(_) => return err_response(400, "bad chat upload conversation"),
    };
    let context = match fields.get("context_id") {
        None => None,
        Some(value) if valid_app_selector(value) => Some(value),
        Some(_) => return err_response(400, "bad chat upload context_id"),
    };
    if context.is_some() && install.is_none() {
        return err_response(400, "chat upload context_id needs an install_id");
    }
    match (install, conversation) {
        (None, None) => {}
        (Some(_), Some(_)) => {}
        (Some(_), None) => {
            return err_response(400, "chat upload install_id needs its conversation");
        }
        (None, Some(_)) => {
            return err_response(400, "chat upload conversation needs an install_id");
        }
    }
    // The name is the part's declared filename, or a `name` field — a
    // display hint the daemon sanitizes (basename, no control chars).
    let name = fields
        .get("filename")
        .or_else(|| fields.get("name"))
        .cloned()
        .unwrap_or_else(|| "attachment.txt".to_string());
    let uploads = state_dir.join(crate::wiki::UPLOAD_DIR);
    if let Err(e) = std::fs::create_dir_all(&uploads) {
        return err_response(500, &format!("upload staging failed: {e}"));
    }
    let tmp = uploads.join(format!("upload-{}", uuid::Uuid::new_v4().simple()));
    // Exclusive create is the ownership proof: a minted UUID alone does
    // not prove this request created the path, so an occupied name is
    // refused without truncating or deleting it. Only after the create
    // succeeds may this request clean up — including a partial write.
    let mut staged = match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
    {
        Ok(f) => f,
        Err(e) => return err_response(500, &format!("upload staging failed: {e}")),
    };
    if let Err(e) = staged.write_all(&bytes) {
        drop(staged);
        let _ = std::fs::remove_file(&tmp);
        return err_response(500, &format!("upload staging failed: {e}"));
    }
    if let Err(e) = staged.sync_all() {
        drop(staged);
        let _ = std::fs::remove_file(&tmp);
        return err_response(500, &format!("upload staging failed: {e}"));
    }
    drop(staged);
    // The daemon copies the staged bytes into its own custody before it
    // answers, so this request's staging is removed on every path —
    // success, refusal or an unreachable daemon. Only the tmp this
    // request created is touched. The selector travels as the daemon's
    // own `app`/`conversation` shape; the daemon re-proves all of it.
    let mut params = json!({"name": name, "tmp": tmp});
    if let (Some(install), Some(conversation)) = (install, conversation) {
        let mut app = json!({"install_id": install});
        if let Some(context) = context {
            app["context_id"] = json!(context);
        }
        params["app"] = app;
        params["conversation"] = json!(conversation);
    }
    let outcome = client::rpc(state_dir, "chat_file_upload", params);
    let _ = std::fs::remove_file(&tmp);
    match outcome {
        Ok(v) => json_response(v),
        Err(Error::Structured(details)) if details.kind == "busy" => {
            err_response(429, &details.message)
        }
        Err(Error::Structured(details)) if details.code == crate::store::CHAT_QUOTA_CODE => {
            err_response(413, &details.message)
        }
        Err(e) => write_err(&e),
    }
}

/// `GET /api/app-installations/<id>/conversations` — relays the
/// daemon's `conversation_list` (which proves the installation).
pub(super) fn conversations(
    state_dir: &std::path::Path,
    alias: &str,
    query: &dyn Fn(&str) -> Option<String>,
) -> HttpResp {
    if !valid_alias(alias) {
        return err_response(400, "bad agent alias");
    }
    let Some(install) = query("install").filter(|i| !i.is_empty()) else {
        return err_response(400, "install is required");
    };
    match client::rpc(
        state_dir,
        "conversation_list",
        json!({"alias": alias, "install_id": install}),
    ) {
        Ok(list) => json_response(list),
        Err(e) => rpc_err(&e),
    }
}

/// `POST /api/threads/<alias>/conversations` — relays the daemon's
/// `conversation_create`. Operator-only: `operator::admit` has already
/// run the guards and the process proof on the peer, and the daemon
/// proves the connection again — the board is never less strict.
pub(super) fn post_conversation(
    request: &mut Request,
    state_dir: &std::path::Path,
    alias: &str,
    path_install: Option<&str>,
) -> HttpResp {
    if !valid_alias(alias) {
        return err_response(400, "bad agent alias");
    }
    let bytes = match read_body(request, MESSAGE_CAP) {
        Ok(bytes) => bytes,
        Err(resp) => return resp,
    };
    let req: ConversationReq = match parse_json(&bytes) {
        Ok(req) => req,
        Err(resp) => return resp,
    };
    // On `/api/app-installations/<id>/conversations` the path names the
    // installation and a body `install_id` is refused, never reconciled.
    let install = match (path_install, req.install_id) {
        (Some(_), Some(_)) => return err_response(400, "install_id is named by the path"),
        (Some(path), None) => path.to_string(),
        (None, Some(body)) => body,
        (None, None) => return err_response(400, "install_id is required"),
    };
    let mut params = json!({
        "alias": alias,
        "install_id": install,
    });
    if let Some(context) = req.context_id {
        params["context_id"] = Value::String(context);
    }
    if let Some(subject) = req.subject {
        params["subject"] = Value::String(subject);
    }
    if let Some(general) = req.general {
        params["general"] = Value::Bool(general);
    }
    match client::rpc(state_dir, "conversation_create", params) {
        Ok(created) => json_response(created),
        Err(e) => rpc_err(&e),
    }
}

#[cfg(test)]
pub(super) fn busy_status_for_test(e: &Error) -> u16 {
    rpc_err(e).status_code().0
}
