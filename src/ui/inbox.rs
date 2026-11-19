//! CAD-2476: Board HTTP API for the Agent Inbox & assistant-to-assistant relay.
//!
//! - `GET  /api/inbox` — durable inbox feed, unread count, cursor, and
//!   workspace directory (colleagues + specialists)
//! - `POST /api/inbox/send` — send a durable message (checkin/handoff/reply)
//!   without creating a board ticket
//! - `POST /api/inbox/ack` — advance the read watermark (`through` seq)
//! - `POST /api/inbox/promote` — opt-in promotion of an inbox message to a
//!   tracked board issue
//! - `POST /api/inbox/decide` — operator approval or decline of a specialist
//!   `decision` proposal in the inbox
//!
//! Every route is `OperatorOnly` in [`super::endpoints`]; request bodies use
//! `#[serde(deny_unknown_fields)]` so no forged sender, actor, or steering
//! flag can reach the daemon.

use std::path::Path;

use serde::Deserialize;
use serde_json::{json, Value};
use tiny_http::Request;

use super::{
    err_response, home, json_response, operator, parse_json, read_body, write_err, HttpResp,
};
use crate::client;
use crate::issue::{model, project, write as issue_write, Pm};

const BODY_CAP: u64 = 64 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InboxSendReq {
    to: String,
    text: String,
    kind: Option<String>,
    reply_to: Option<String>,
    artifact: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InboxAckReq {
    alias: Option<String>,
    through: i64,
    reader: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InboxPromoteReq {
    alias: Option<String>,
    message: String,
    project: Option<String>,
    title: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InboxDecideReq {
    alias: Option<String>,
    message: String,
    decision: String,
    reason: Option<String>,
}

fn valid_alias(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 80
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

fn query_param<'a>(query: &'a str, key: &str) -> Option<&'a str> {
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == key && !v.is_empty()).then_some(v)
    })
}

/// `GET /api/inbox?alias=<alias>&unread_only=true|false&after=<seq>&limit=<n>&reader=<name>`
pub(super) fn feed(request: &Request, state_dir: &Path) -> HttpResp {
    let query = request.url().split_once('?').map(|(_, q)| q).unwrap_or("");
    let alias = query_param(query, "alias").unwrap_or(crate::master::ALIAS);
    if !valid_alias(alias) {
        return err_response(400, "bad agent alias");
    }
    let unread_only = matches!(query_param(query, "unread_only"), Some("true" | "1"));
    let after = match query_param(query, "after") {
        None => None,
        Some(raw) => match raw.parse::<i64>() {
            Ok(v) if v >= 0 => Some(v),
            _ => return err_response(400, "bad after seq"),
        },
    };
    let limit = match query_param(query, "limit") {
        None => None,
        Some(raw) => match raw.parse::<u64>() {
            Ok(v) if (1..=100).contains(&v) => Some(v),
            _ => return err_response(400, "limit must be 1..=100"),
        },
    };
    let reader = query_param(query, "reader");
    if let Some(r) = reader {
        if !valid_alias(r) {
            return err_response(400, "bad reader name");
        }
    }
    let mut req = json!({"alias": alias, "unread_only": unread_only});
    if let Some(a) = after {
        req["after"] = json!(a);
    }
    if let Some(l) = limit {
        req["limit"] = json!(l);
    }
    if let Some(r) = reader {
        req["reader"] = json!(r);
    }
    match client::rpc(state_dir, "agent_inbox_feed", req) {
        Ok(reply) => json_response(reply),
        Err(error) => home::rpc_err(&error, "agent_inbox_feed"),
    }
}

/// `POST /api/inbox/send`
pub(super) fn send_message(request: &mut Request, state_dir: &Path) -> HttpResp {
    let bytes = match read_body(request, BODY_CAP) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let req: InboxSendReq = match parse_json(&bytes) {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    let to = req.to.trim();
    let text = req.text.trim();
    if !valid_alias(to) {
        return err_response(400, "bad target agent alias");
    }
    if text.is_empty() {
        return err_response(400, "message text is required");
    }
    let kind = match req.kind.as_deref() {
        None => None,
        Some(raw) => match crate::inbox::InboxKind::parse(raw) {
            Ok(k) => Some(k.as_str().to_string()),
            Err(e) => return err_response(400, &e.to_string()),
        },
    };
    let reply_to = match req
        .reply_to
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        None => None,
        Some(r) if valid_alias(r) => Some(r.to_string()),
        Some(_) => return err_response(400, "bad reply_to alias"),
    };
    let artifact = match req
        .artifact
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        None => None,
        Some(raw) => match crate::inbox::validate_artifact(raw) {
            Ok(a) => Some(a),
            Err(e) => return err_response(400, &e.to_string()),
        },
    };
    let mut rpc_req = json!({"alias": to, "text": text});
    if let Some(k) = kind {
        rpc_req["kind"] = json!(k);
    }
    if let Some(r) = reply_to {
        rpc_req["reply_to"] = json!(r);
    }
    if let Some(a) = artifact {
        rpc_req["artifact"] = json!(a);
    }
    match client::rpc(state_dir, "agent_send", rpc_req) {
        Ok(reply) => json_response(reply),
        Err(error) => home::rpc_err(&error, "agent_send"),
    }
}

/// `POST /api/inbox/ack`
pub(super) fn ack(request: &mut Request, state_dir: &Path) -> HttpResp {
    let bytes = match read_body(request, BODY_CAP) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let req: InboxAckReq = match parse_json(&bytes) {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    let alias = req
        .alias
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(crate::master::ALIAS);
    if !valid_alias(alias) {
        return err_response(400, "bad agent alias");
    }
    if req.through < 0 {
        return err_response(400, "through must be a non-negative seq");
    }
    let mut rpc_req = json!({"alias": alias, "through": req.through});
    if let Some(r) = req
        .reader
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if !valid_alias(r) {
            return err_response(400, "bad reader name");
        }
        rpc_req["reader"] = json!(r);
    }
    match client::rpc(state_dir, "agent_inbox_mark", rpc_req) {
        Ok(reply) => json_response(reply),
        Err(error) => home::rpc_err(&error, "agent_inbox_mark"),
    }
}

/// `POST /api/inbox/promote` — opt-in promotion of an inbox message to a
/// tracked board issue.
pub(super) fn promote(
    request: &mut Request,
    state_dir: &Path,
    pm_dir: &Path,
    caller: &operator::Caller,
) -> HttpResp {
    let bytes = match read_body(request, BODY_CAP) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let req: InboxPromoteReq = match parse_json(&bytes) {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    let alias = req
        .alias
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(crate::master::ALIAS);
    if !valid_alias(alias) {
        return err_response(400, "bad agent alias");
    }
    let message_id = req.message.trim();
    if message_id.is_empty() {
        return err_response(400, "message id is required");
    }
    let feed = match client::rpc(
        state_dir,
        "agent_inbox_feed",
        json!({"alias": alias, "limit": 100}),
    ) {
        Ok(v) => v,
        Err(error) => return home::rpc_err(&error, "agent_inbox_feed"),
    };
    let Some(msg) = feed["messages"].as_array().and_then(|rows| {
        rows.iter()
            .find(|row| row["id"].as_str() == Some(message_id))
    }) else {
        return err_response(404, "inbox message not found");
    };
    if let Some(existing_issue) = msg["issue"].as_str().filter(|s| !s.is_empty()) {
        return json_response(json!({
            "ok": true,
            "alias": alias,
            "message": message_id,
            "issue": existing_issue,
            "already_promoted": true,
        }));
    }

    let pm = match Pm::at(pm_dir) {
        Ok(pm) => pm,
        Err(e) => return err_response(503, &e.to_string()),
    };
    let spelling = operator::policy_caller(caller);
    let project_key = match req
        .project
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(raw) => {
            let Ok(key) = model::check_key(raw) else {
                return err_response(400, "bad project key");
            };
            if super::project_policy::refused(&pm, &key, &spelling) {
                return err_response(404, "unknown project");
            }
            key
        }
        None => {
            let projects = match project::list(&pm.dir) {
                Ok(list) => list,
                Err(e) => return err_response(503, &e.to_string()),
            };
            let accessible: Vec<String> = projects
                .into_iter()
                .map(|p| p.key)
                .filter(|key| !super::project_policy::refused(&pm, key, &spelling))
                .collect();
            match accessible.first() {
                Some(first) => first.clone(),
                None => return err_response(400, "no accessible project available for promotion"),
            }
        }
    };

    let body_text = msg["body"].as_str().unwrap_or("").trim();
    let title = req
        .title
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| default_promote_title(msg, body_text));
    let from = msg["sender"].as_str().unwrap_or("unknown");
    let kind = msg["kind"].as_str().unwrap_or("message");
    let mut issue_body = format!(
        "Promoted from Agent Inbox ({alias}) message `{message_id}`.\n\n\
         - **From:** `{from}`\n\
         - **Kind:** `{kind}`\n"
    );
    if let Some(artifact) = msg["artifact"].as_str().filter(|s| !s.is_empty()) {
        issue_body.push_str(&format!("- **Artifact:** `{artifact}`\n"));
    }
    issue_body.push_str("\n---\n\n");
    issue_body.push_str(body_text);

    let created = match issue_write::new_issue(
        &pm,
        &pm.dir,
        Some(&project_key),
        &title,
        Some("P2"),
        None,
        &[],
        None,
        None,
        &[],
        None,
        Some(&issue_body),
        caller.actor(),
    ) {
        Ok(out) => out,
        Err(e) => return write_err(&e),
    };
    let new_id = created["id"].as_str().unwrap_or_default().to_string();
    if new_id.is_empty() {
        return err_response(500, "issue creation returned no id");
    }
    if let Err(error) = client::rpc(
        state_dir,
        "agent_inbox_promote",
        json!({"alias": alias, "message": message_id, "issue": new_id}),
    ) {
        return home::rpc_err(&error, "agent_inbox_promote");
    }
    json_response(json!({
        "ok": true,
        "alias": alias,
        "message": message_id,
        "issue": new_id,
        "project": project_key,
        "title": title,
    }))
}

fn default_promote_title(msg: &Value, body: &str) -> String {
    let first_line = body
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("Promoted inbox message");
    let cleaned: String = first_line.chars().take(96).collect();
    if cleaned.is_empty() {
        format!(
            "Follow-up from {}",
            msg["sender"].as_str().unwrap_or("colleague")
        )
    } else {
        cleaned
    }
}

/// `POST /api/inbox/decide` — operator approval or decline of a specialist
/// `decision` proposal in the inbox.
pub(super) fn decide(request: &mut Request, state_dir: &Path) -> HttpResp {
    let bytes = match read_body(request, BODY_CAP) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let req: InboxDecideReq = match parse_json(&bytes) {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    let alias = req
        .alias
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(crate::master::ALIAS);
    if !valid_alias(alias) {
        return err_response(400, "bad agent alias");
    }
    let message_id = req.message.trim();
    if message_id.is_empty() {
        return err_response(400, "message id is required");
    }
    let decision = req.decision.trim();
    if !matches!(decision, "approved" | "declined") {
        return err_response(400, "decision must be 'approved' or 'declined'");
    }
    let mut rpc_req = json!({
        "alias": alias,
        "message": message_id,
        "decision": decision,
    });
    if let Some(r) = req
        .reason
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        rpc_req["reason"] = json!(r);
    }
    match client::rpc(state_dir, "agent_inbox_decide", rpc_req) {
        Ok(reply) => json_response(reply),
        Err(error) => home::rpc_err(&error, "agent_inbox_decide"),
    }
}
