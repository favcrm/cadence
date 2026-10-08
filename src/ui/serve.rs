//! The board's HTTP server core: the request dispatcher (`handle`), the
//! accept loop (`serve`), the response/static-file helpers they share, the
//! embedded SPA build, and the `/api/agents` read model it serves.

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use serde_json::{json, Value};
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};

use super::setup::{board_boot_agent_uid, setup_get, setup_refusal, WRITE_LOCK};
use super::stream::stream_events;
use super::write_path::{
    artifact_response, header_value, model_defaults_get, request_identity, send, with_agents,
    write_route, HttpResp,
};
use super::{
    app_assistant, app_audiences, app_chat, app_content, app_contexts, app_explorer, app_records,
    app_release, app_runs, app_screens, apps, cli_route, connections, crm_send, delivery_sync,
    home, lane, operator, platform_account, read_model, social_publish, stages, threads, updates,
    wiki, workflows,
};
use super::{push_device_login_config, ready_file, tailnet_url, ServeOpts, READY_NONCE_ENV};
use crate::adapter::registry;
use crate::client;
use crate::doctor::host::redact_argv;
use crate::error::{Error, Result};
use crate::issue::{board, context, history, model, project, Pm};

// ---------- server ----------

/// The Host allowlist: the gateway vhost (with or without its port —
/// browsers send `:18000`, a hand-set header may not) plus the bind
/// address forms.
pub(crate) fn host_allowed(host: &str, port: u16, extra: &[String]) -> bool {
    let host = host.trim().to_ascii_lowercase();
    host == "cadence.localhost"
        || host == "cadence.localhost:18000"
        || host == operator::board_host(port)
        || host == format!("127.0.0.1:{port}")
        || host == format!("localhost:{port}")
        || host == format!("[::1]:{port}")
        || extra.iter().any(|h| h.eq_ignore_ascii_case(&host))
}

pub(crate) fn json_response(value: Value) -> Response<std::io::Cursor<Vec<u8>>> {
    let body = serde_json::to_vec_pretty(&value).unwrap_or_default();
    use sha2::{Digest, Sha256};
    let etag = format!("\"{:x}\"", Sha256::digest(&body));
    let mut resp = Response::from_data(body).with_status_code(StatusCode(200));
    resp.add_header(Header::from_bytes("ETag", etag).unwrap());
    resp.add_header(Header::from_bytes("Cache-Control", "private, no-cache").unwrap());
    resp.add_header(Header::from_bytes("Vary", "Cookie, X-Cadence-Session").unwrap());
    resp.add_header(Header::from_bytes("Content-Type", "application/json").unwrap());
    resp
}

pub(crate) fn err_response(code: u16, message: &str) -> Response<std::io::Cursor<Vec<u8>>> {
    let body = serde_json::to_vec_pretty(&json!({"error": message})).unwrap_or_default();
    let mut resp = Response::from_data(body).with_status_code(StatusCode(code));
    resp.add_header(Header::from_bytes("Content-Type", "application/json").unwrap());
    resp
}

/// Defence in depth on an unauthenticated loopback origin that renders
/// agent-written Markdown. Social source previews use only these reviewed
/// image CDNs; the client checks the same host families before rendering.
pub(crate) const CSP: &str = "default-src 'self'; img-src 'self' data: https://cdninstagram.com https://*.cdninstagram.com https://fbcdn.net https://*.fbcdn.net; font-src 'self' data:; style-src 'self' 'unsafe-inline'; base-uri 'none'; frame-ancestors 'none'";

/// Every response gets nosniff + no-referrer; HTML additionally gets
/// the CSP. Returns whether the response is HTML.
///
/// CAD-1006: a response that already carries a `Content-Security-Policy`
/// header keeps it — `send` must not append the board's own CSP onto the
/// private screen frame, which carries its own nonce-pinned policy. This
/// never removes or weakens the board CSP on any other response; it only
/// refrains from stacking a second, contradictory policy on a response
/// that deliberately set one.
pub(crate) fn add_security_headers(resp: &mut Response<std::io::Cursor<Vec<u8>>>) -> bool {
    let is_html = resp
        .headers()
        .iter()
        .any(|h| h.field.equiv("Content-Type") && h.value.as_str().starts_with("text/html"));
    let has_csp = resp
        .headers()
        .iter()
        .any(|h| h.field.equiv("Content-Security-Policy"));
    resp.add_header(Header::from_bytes("X-Content-Type-Options", "nosniff").unwrap());
    resp.add_header(Header::from_bytes("Referrer-Policy", "no-referrer").unwrap());
    if is_html && !has_csp {
        resp.add_header(Header::from_bytes("Content-Security-Policy", CSP).unwrap());
    }
    is_html
}

/// Percent-decode a URL path/query component (UTF-8, `+` untouched in
/// path context). Returns None on malformed input.
pub(crate) fn pct_decode(raw: &str) -> Option<String> {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                let hex = bytes.get(i + 1..i + 3)?;
                let v = u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok()?;
                out.push(v);
                i += 3;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

pub(crate) fn context_query(
    raw_query: &str,
) -> std::result::Result<(Option<String>, Option<String>), &'static str> {
    let mut role = None;
    let mut expected_revision = None;
    for raw_pair in raw_query.split('&').filter(|pair| !pair.is_empty()) {
        let (raw_key, raw_value) = raw_pair
            .split_once('=')
            .ok_or("context query values must use key=value")?;
        let key = pct_decode(raw_key).ok_or("malformed context query")?;
        let value = pct_decode(raw_value).ok_or("malformed context query")?;
        match key.as_str() {
            "role" if role.is_none() => role = Some(context::canonical_role(&value).to_string()),
            "expected_revision" if expected_revision.is_none() => expected_revision = Some(value),
            "role" | "expected_revision" => return Err("duplicate context query key"),
            _ => return Err("unknown context query key"),
        }
    }
    if let Some(value) = role.as_deref() {
        if !context::valid_role(value) {
            return Err("bad context role");
        }
    }
    if let Some(value) = expected_revision.as_deref() {
        if !context::valid_revision(value) {
            return Err("bad expected revision");
        }
    }
    Ok((role, expected_revision))
}

pub(crate) fn content_type(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or_default() {
        "html" => "text/html; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "woff2" => "font/woff2",
        "json" => "application/json",
        "map" => "application/json",
        _ => "application/octet-stream",
    }
}

/// Static payload: `dist` dir first (dev / `--features ui` absent),
/// then the embedded build when compiled in.
pub(crate) fn static_file(dist: Option<&Path>, path: &str) -> Option<(String, Vec<u8>)> {
    if let Some(dist) = dist {
        let rel = path.trim_start_matches('/');
        if rel.is_empty()
            || rel
                .split('/')
                .any(|s| s.is_empty() || s == "." || s == "..")
        {
            return None;
        }
        let base = dist.canonicalize().ok()?;
        let file = base.join(rel).canonicalize().ok()?;
        if !file.starts_with(&base) || !file.is_file() {
            return None;
        }
        return std::fs::read(&file).ok().map(|b| (path.to_string(), b));
    }
    #[cfg(feature = "ui")]
    {
        let bytes: Option<&[u8]> = match path {
            "/" | "/index.html" => Some(embedded::INDEX.as_bytes()),
            "/assets/index.js" => Some(embedded::JS.as_bytes()),
            "/assets/index.css" => Some(embedded::CSS.as_bytes()),
            // Brand files Vite copies from `ui/public/` to the dist root —
            // without these arms the SPA fallback would answer with HTML.
            "/favicon.svg" => Some(embedded::FAVICON),
            "/icon.svg" => Some(embedded::ICON),
            "/apple-touch-icon.png" => Some(embedded::APPLE_TOUCH_ICON),
            _ => path
                .strip_prefix("/assets/")
                .and_then(|name| embedded::ASSETS.get(name).copied()),
        };
        return bytes.map(|b| (path.to_string(), b.to_vec()));
    }
    #[allow(unreachable_code)]
    None
}

/// What a non-API GET answers with.
#[derive(Debug)]
pub(crate) enum StaticAnswer {
    /// A file of the build — or the SPA shell (`index.html`) for a client route.
    File(String, Vec<u8>),
    /// A path that names a file the build does not have.
    Missing,
    /// No build to serve at all.
    NoBuild,
}

/// Extensions the build serves as files. A missing one is a 404, never
/// the SPA shell — with `nosniff`, the browser still must not be handed
/// HTML under a script, stylesheet, or image name. Wiki pages (`.md`)
/// and dotted ids (`CAD-1.2`) are not in this set.
pub(crate) fn is_static_ext(ext: &str) -> bool {
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "html"
            | "js"
            | "mjs"
            | "css"
            | "svg"
            | "png"
            | "jpg"
            | "jpeg"
            | "gif"
            | "webp"
            | "ico"
            | "woff"
            | "woff2"
            | "ttf"
            | "map"
            | "json"
            | "webmanifest"
    )
}

/// Wiki addresses (`/wiki` and `/wiki/<path>`) name pages and blobs in
/// the store, including `note.md` and `photo.png`. They are never build
/// files. File bytes stay on `/api/wiki/file` only.
pub(crate) fn is_wiki_path(path: &str) -> bool {
    path == "/wiki" || path.starts_with("/wiki/")
}

/// Whether a path is a client-side route (ui/src/lib/router.ts) that the
/// SPA shell answers so deep links and refreshes work. `/api` and
/// `/assets/` never are. A missing build asset (a static extension,
/// outside the wiki) is a 404. Anything else — including a wiki page and
/// a dotted issue id — is a route. One rule for every screen, not a
/// branch per path.
pub(crate) fn is_client_route(path: &str) -> bool {
    if path == "/api" || path.starts_with("/api/") || path.starts_with("/assets/") {
        return false;
    }
    if is_wiki_path(path) {
        return true;
    }
    !matches!(
        path.rsplit('/').next().unwrap_or_default().rsplit_once('.'),
        Some((_, ext)) if is_static_ext(ext)
    )
}

/// `Accept: application/json` asks for the JSON 404, not the shell.
/// A browser navigation sends `text/html` (and `*/*`), which does not
/// count — `*/*` would hide every refresh.
pub(crate) fn prefers_json(accept: Option<&str>) -> bool {
    let Some(accept) = accept else {
        return false;
    };
    accept.split(',').any(|part| {
        part.split(';')
            .next()
            .unwrap_or("")
            .trim()
            .eq_ignore_ascii_case("application/json")
    })
}

/// A build file when one matches; else the SPA shell for a client route.
/// `accept` is the request's Accept header. A wiki GET that prefers JSON
/// stays a 404 so the file API remains `/api/wiki/file`.
pub(crate) fn static_answer_for(
    dist: Option<&Path>,
    path: &str,
    accept: Option<&str>,
) -> StaticAnswer {
    let target = if path == "/" { "/index.html" } else { path };
    if !path.starts_with("/api/") {
        if let Some((name, bytes)) = static_file(dist, target) {
            return StaticAnswer::File(name, bytes);
        }
    }
    if !is_client_route(path) || (is_wiki_path(path) && prefers_json(accept)) {
        return StaticAnswer::Missing;
    }
    match static_file(dist, "/index.html") {
        Some((name, bytes)) => StaticAnswer::File(name, bytes),
        None => StaticAnswer::NoBuild,
    }
}

#[cfg(feature = "ui")]
mod embedded {
    use std::collections::HashMap;
    use std::sync::LazyLock;

    pub const INDEX: &str = include_str!("../../ui/dist/index.html");
    pub const JS: &str = include_str!("../../ui/dist/assets/index.js");
    pub const CSS: &str = include_str!("../../ui/dist/assets/index.css");
    pub const FAVICON: &[u8] = include_bytes!("../../ui/dist/favicon.svg");
    pub const ICON: &[u8] = include_bytes!("../../ui/dist/icon.svg");
    pub const APPLE_TOUCH_ICON: &[u8] = include_bytes!("../../ui/dist/apple-touch-icon.png");

    /// The latin woff2 files the CSS references (woff fallbacks are not
    /// embedded — every supported browser takes woff2 first).
    pub static ASSETS: LazyLock<HashMap<&'static str, &'static [u8]>> = LazyLock::new(|| {
        HashMap::from([
            (
                "ibm-plex-sans-latin-400-normal.woff2",
                include_bytes!("../../ui/dist/assets/ibm-plex-sans-latin-400-normal.woff2")
                    as &[u8],
            ),
            (
                "ibm-plex-sans-latin-500-normal.woff2",
                include_bytes!("../../ui/dist/assets/ibm-plex-sans-latin-500-normal.woff2")
                    as &[u8],
            ),
            (
                "ibm-plex-sans-latin-600-normal.woff2",
                include_bytes!("../../ui/dist/assets/ibm-plex-sans-latin-600-normal.woff2")
                    as &[u8],
            ),
            (
                "ibm-plex-mono-latin-400-normal.woff2",
                include_bytes!("../../ui/dist/assets/ibm-plex-mono-latin-400-normal.woff2")
                    as &[u8],
            ),
            (
                "ibm-plex-mono-latin-500-normal.woff2",
                include_bytes!("../../ui/dist/assets/ibm-plex-mono-latin-500-normal.woff2")
                    as &[u8],
            ),
            (
                "ibm-plex-mono-latin-600-normal.woff2",
                include_bytes!("../../ui/dist/assets/ibm-plex-mono-latin-600-normal.woff2")
                    as &[u8],
            ),
        ])
    });
}

/// One task assignment enriched with its job context for the board. The
/// join stays exact: agent tasks are joined through the job's task rows and
/// `jobs.issue_id`, never by scanning message text for issue-shaped tokens.
#[derive(Clone)]
pub(crate) struct TaskBinding {
    issue: String,
    task_state: String,
    task_title: Option<String>,
    job: String,
    job_title: Option<String>,
    job_state: String,
}

/// `job_list` as the board reads it: every job, each row carrying its
/// tasks (`task_list`, CAD-325) so bindings need no per-job `job_show`.
pub(crate) fn board_job_list(state_dir: &Path) -> Option<Value> {
    client::rpc(
        state_dir,
        "job_list",
        json!({"all": true, "tasks_detail": true}),
    )
    .ok()
}

/// task id → issue/job context for every task that belongs to an
/// issue-bound job.
pub(crate) fn task_issue_map(state_dir: &Path) -> HashMap<String, TaskBinding> {
    match board_job_list(state_dir) {
        Some(list) => task_issue_map_from(state_dir, &list),
        None => HashMap::new(),
    }
}

/// [`task_issue_map`] over a `job_list` already in hand. A row without
/// `task_list` comes from a daemon older than CAD-325 and falls back to
/// that job's `job_show` — the old per-job fan-out, which cost one RPC
/// per issue-bound job ever created on every board read.
pub(crate) fn task_issue_map_from(state_dir: &Path, list: &Value) -> HashMap<String, TaskBinding> {
    let mut map = HashMap::new();
    for job in list["jobs"].as_array().cloned().unwrap_or_default() {
        let Some(issue) = job["issue"].as_str().map(str::to_string) else {
            continue;
        };
        if issue.is_empty() {
            continue;
        }
        let Some(job_id) = job["id"].as_str() else {
            continue;
        };
        let show = if job["task_list"].is_array() {
            json!({"job": {"title": job["title"], "state": job["state"],
                           "tasks": job["task_list"]}})
        } else {
            let Ok(show) = client::rpc(state_dir, "job_show", json!({"job": job_id})) else {
                continue;
            };
            show
        };
        let job_title = show["job"]["title"].as_str().map(str::to_string);
        let job_state = show["job"]["state"]
            .as_str()
            .unwrap_or("unknown")
            .to_string();
        for task in show["job"]["tasks"].as_array().cloned().unwrap_or_default() {
            if let (Some(tid), Some(state)) = (task["id"].as_str(), task["state"].as_str()) {
                map.insert(
                    tid.to_string(),
                    TaskBinding {
                        issue: issue.clone(),
                        task_state: state.to_string(),
                        task_title: task["title"].as_str().map(str::to_string),
                        job: job_id.to_string(),
                        job_title: job_title.clone(),
                        job_state: job_state.clone(),
                    },
                );
            }
        }
    }
    map
}

/// Keep an ad-hoc current-message description useful without putting the
/// queued prompt or provider credentials in browser JSON. The shared argv
/// scrubber handles credential-shaped flags, headers, URIs and token forms;
/// the character cap keeps one large prompt from becoming an agent row.
pub(crate) fn current_message_summary(m: &Value) -> Option<String> {
    let body = m["body"].as_str()?.trim();
    if body.is_empty() {
        return None;
    }
    let head: String = body.chars().take(512).collect();
    let tokens: Vec<String> = head.split_whitespace().map(str::to_string).collect();
    let safe = redact_argv(&tokens);
    if safe.is_empty() {
        return None;
    }
    let mut summary: String = safe.chars().take(180).collect();
    if safe.chars().count() > 180 {
        summary.push('…');
    }
    Some(summary)
}

/// One running message reduced for the board: id, task and a bounded
/// redacted description for ad-hoc current work. Never the turn token:
/// it is `message_report`'s credential and the board serves any local
/// HTTP caller (CAD-375; the daemon withholds it from the board's own
/// connection too).
pub(crate) fn running_json(m: &Value) -> Value {
    json!({
        "id": m["id"],
        "task": m["task_id"],
        "created": m["created"],
        "summary": current_message_summary(m),
    })
}

pub(crate) fn agent_activity_seconds(value: &Value) -> Option<f64> {
    let seconds = if let Some(seconds) = value.as_f64() {
        seconds
    } else {
        let source = value.as_str()?;
        // Reuse the issue parser for calendar validation, then account for
        // fractional seconds and an optional RFC 3339 timezone offset.
        let base = source.get(..19)?;
        let mut suffix = source.get(19..)?;
        let epoch = crate::issue::time::parse_iso(&format!("{base}Z"))? as f64;
        let fraction = if let Some(rest) = suffix.strip_prefix('.') {
            let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
            if digits == 0 {
                return None;
            }
            let (part, zone) = rest.split_at(digits);
            suffix = zone;
            format!("0.{part}").parse::<f64>().ok()?
        } else {
            0.0
        };
        let offset = if suffix == "Z" {
            0
        } else {
            let bytes = suffix.as_bytes();
            if bytes.len() != 6 || !matches!(bytes[0], b'+' | b'-') || bytes[3] != b':' {
                return None;
            }
            let hours = suffix.get(1..3)?.parse::<i64>().ok()?;
            let minutes = suffix.get(4..6)?.parse::<i64>().ok()?;
            if hours > 23 || minutes > 59 {
                return None;
            }
            let direction = if bytes[0] == b'+' { 1 } else { -1 };
            direction * (hours * 3600 + minutes * 60)
        };
        epoch + fraction - offset as f64
    };
    (seconds.is_finite() && seconds > 0.0 && seconds <= 8.64e12).then_some(seconds)
}

/// The tail of an agent's event log for the drawer — the last `n`
/// events via the same `events` RPC the CLI long-polls.
pub(crate) fn agent_events_tail(state_dir: &Path, alias: &str, cursor: i64, n: i64) -> Vec<Value> {
    let after = (cursor - n).max(0);
    client::rpc(state_dir, "events", json!({"alias": alias, "after": after}))
        .map(|r| {
            r["events"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|e| e["seq"].as_i64().unwrap_or(0) > cursor - n)
                .collect()
        })
        .unwrap_or_default()
}

/// What the board needs from the daemon — agent rows enriched with the
/// exact task/issue binding, plus the per-agent queue/fence counts and
/// the per-issue agent map for cards and drawers.
/// `daemon: "unreachable"` instead of a 500 when the socket is down:
/// the board still renders. Built from an `agent_list` asked with
/// `board: true` and a [`board_job_list`] — the read model fetches both
/// once and shares the job list with the cards' job outcomes.
pub(crate) fn agents_payload_from(
    state_dir: &Path,
    list: Option<Value>,
    jobs: Option<&Value>,
) -> Value {
    let Some(list) = list else {
        return json!({"daemon": "unreachable", "agents": [], "totals": null, "by_issue": {}});
    };
    let task_map = jobs
        .map(|jobs| task_issue_map_from(state_dir, jobs))
        .unwrap_or_default();
    let agents = list["agents"].as_array().cloned().unwrap_or_default();
    let mut out = Vec::new();
    let mut inboxes = 0i64;
    let mut by_issue: HashMap<String, Vec<Value>> = HashMap::new();
    let mut totals = json!({"running": 0, "queued": 0, "fenced": 0, "parked": 0, "inboxes": 0});
    for agent in &agents {
        let alias = agent["alias"].as_str().unwrap_or_default();
        let provider = agent["provider"].as_str().unwrap_or_default();
        let kind = agent["endpoint_kind"].as_str().unwrap_or_default();
        let resume = registry::resume_command(
            provider,
            kind,
            agent["thread_id"].as_str().unwrap_or_default(),
            agent["session_id"].as_str().unwrap_or_default(),
            agent["endpoint"].as_str().unwrap_or_default(),
        );
        // Mailboxes are not workers — they still appear on the Agents
        // screen (as kind "inbox") but never count as busy/fenced.
        let actor = registry::has_actor(provider, kind);
        if !actor {
            inboxes += 1;
            // CAD-480: the mailbox's unread backlog and its oldest
            // unread age are the row's queue evidence — the daemon
            // computes both in `agent.inbox`.
            let unread = agent["inbox"]["queued"].as_i64().unwrap_or(0);
            let oldest_unread_age_secs = agent["inbox"]["oldest_age_secs"].clone();
            totals["queued"] = json!(totals["queued"].as_i64().unwrap_or(0) + unread);
            out.push(json!({
                "alias": alias, "provider": agent["provider"],
                "endpoint_kind": agent["endpoint_kind"],
                "role": agent["role"],
                "team_role": agent["team_role"],
                "model_selection": agent["model_selection"],
                "model_lookup_role": agent["model_lookup_role"],
                "model": agent["model"],
                "model_reported": agent["model_reported"],
                "model_configured": agent["model_configured"],
                "model_source": agent["model_source"],
                "effort": agent["effort"],
                "effort_reported": agent["effort_reported"],
                "effort_source": agent["effort_source"],
                "effort_applicable": agent["effort_applicable"],
                "quota": agent["quota"],
                "usage_limit": agent["usage_limit"],
                "state": "inbox", "group": agent["params"]["upstream"].as_str().unwrap_or(alias),
                "group_root": agent["params"]["upstream"].is_null(),
                "running": 0, "queued": unread, "unread": unread,
                "oldest_unread_age_secs": oldest_unread_age_secs,
                "unknown": 0, "parked": 0,
                "fenced": false, "on": [], "tasks": [], "message": Value::Null,
                "dead": agent["dead"], "inbox": true,
            }));
            continue;
        }
        // A CAD-325 daemon folds the show slice into the row (`board`);
        // an older one answers it per agent.
        let show = match &agent["board"] {
            board if board.is_object() => Ok(board.clone()),
            _ => client::rpc(state_dir, "agent_show", json!({"alias": alias})),
        };
        let (mut running, mut parked) = (0i64, 0i64);
        let mut running_msgs: Vec<Value> = Vec::new();
        let mut last_activity = Value::Null;
        let mut latest_activity = 0.0;
        let (queued, unknown, cursor) = match &show {
            Ok(show) => {
                for m in show["messages"].as_array().cloned().unwrap_or_default() {
                    for ts in ["completed", "started", "created"] {
                        let at = &m[ts];
                        if let Some(seconds) = agent_activity_seconds(at) {
                            if seconds > latest_activity {
                                latest_activity = seconds;
                                last_activity = at.clone();
                            }
                        }
                    }
                    match m["state"].as_str() {
                        Some("running") => {
                            running += 1;
                            running_msgs.push(running_json(&m));
                        }
                        _ => {
                            if m["result"]["via"].as_str() == Some("pty_render_miss") {
                                parked += 1;
                            }
                        }
                    }
                }
                if let Some(n) = show["parked"].as_i64() {
                    parked = n;
                }
                (
                    show["queued"].as_i64().unwrap_or(0),
                    show["unknown"].as_i64().unwrap_or(0),
                    show["event_cursor"].as_i64().unwrap_or(0),
                )
            }
            Err(_) => (0, 0, 0),
        };
        // Exact binding: the agent's assigned tasks joined through
        // `jobs.issue_id`. A running kickoff's task is the live one.
        let mut on: Vec<String> = Vec::new();
        let mut bound: Vec<Value> = Vec::new();
        for tid in agent["tasks"].as_array().cloned().unwrap_or_default() {
            let Some(tid) = tid.as_str() else { continue };
            let Some(binding) = task_map.get(tid) else {
                continue;
            };
            if !on.contains(&binding.issue) {
                on.push(binding.issue.clone());
            }
            let message = running_msgs
                .iter()
                .find(|m| m["task"].as_str() == Some(tid))
                .cloned()
                .unwrap_or(Value::Null);
            bound.push(json!({
                "task": tid,
                "task_state": binding.task_state,
                "title": binding.task_title,
                "issue": binding.issue,
                "job": binding.job,
                "job_title": binding.job_title,
                "job_state": binding.job_state,
                "message": message,
            }));
            by_issue
                .entry(binding.issue.clone())
                .or_default()
                .push(json!({
                    "alias": alias,
                    "task": tid,
                    "task_state": binding.task_state,
                    "state": agent["state"],
                    "message": message["id"].clone(),
                    "resume": resume,
                }));
        }
        let fenced = unknown > 0 || agent["state"].as_str() == Some("attention");
        totals["running"] = json!(totals["running"].as_i64().unwrap_or(0) + running);
        totals["queued"] = json!(totals["queued"].as_i64().unwrap_or(0) + queued);
        totals["parked"] = json!(totals["parked"].as_i64().unwrap_or(0) + parked);
        if fenced {
            totals["fenced"] = json!(totals["fenced"].as_i64().unwrap_or(0) + 1);
        }
        // The daemon's own fence text already names the recovery path —
        // the board renders it as copyable code, verbatim.
        let recovery = if fenced {
            agent["error"].as_str().map(str::to_string)
        } else {
            None
        };
        out.push(json!({
            "alias": alias,
            "provider": agent["provider"],
            "endpoint_kind": agent["endpoint_kind"],
            "role": agent["role"],
            "team_role": agent["team_role"],
            "model_selection": agent["model_selection"],
            "model_lookup_role": agent["model_lookup_role"],
            // Preserve provider evidence so the UI can distinguish a
            // confirmed effective model from a requested/configured one.
            "model": agent["model"],
            "model_reported": agent["model_reported"],
            "model_configured": agent["model_configured"],
            "model_source": agent["model_source"],
            "effort": agent["effort"],
            "effort_reported": agent["effort_reported"],
            "effort_source": agent["effort_source"],
            "effort_applicable": agent["effort_applicable"],
            // Quota integrations can add this account/pool-scoped view;
            // absent data stays absent and is rendered unavailable below.
            "quota": agent["quota"],
            "usage_limit": agent["usage_limit"],
            "state": agent["state"],
            "group": agent["params"]["upstream"].as_str().unwrap_or(alias),
            "group_root": agent["params"]["upstream"].is_null(),
            "running": running, "queued": queued, "unknown": unknown,
            "parked": parked, "fenced": fenced,
            "on": on,
            "tasks": bound,
            "message": running_msgs.first().cloned().unwrap_or(Value::Null),
            "running_messages": running_msgs,
            "recovery": recovery,
            "resume": resume,
            "resume_hint": "after stop",
            "dead": agent["dead"],
            "last_activity": last_activity,
            "silent_secs": agent["silent_secs"],
            "stalled": agent["stalled"],
            // CAD-96: `stopped (auto, idle 72m)` — null unless auto-stopped.
            "state_label": agent["state_label"],
            "event_cursor": cursor,
        }));
    }
    totals["inboxes"] = json!(inboxes);
    json!({"daemon": "reachable", "agents": out, "totals": totals, "by_issue": by_issue})
}

/// `/api/agents/<alias>` — the drawer detail: the daemon's own
/// `agent_show` plus the last 20 events and the recovery/resume
/// commands the row chips hinted at.
pub(crate) fn agent_detail(state_dir: &Path, alias: &str) -> std::result::Result<Value, String> {
    let show =
        client::rpc(state_dir, "agent_show", json!({"alias": alias})).map_err(|e| e.to_string())?;
    let agent = &show["agent"];
    let cursor = show["event_cursor"].as_i64().unwrap_or(0);
    let events = agent_events_tail(state_dir, alias, cursor, 20);
    let provider = agent["provider"].as_str().unwrap_or_default();
    let kind = agent["endpoint_kind"].as_str().unwrap_or_default();
    let running: Vec<Value> = show["messages"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter(|m| m["state"].as_str() == Some("running"))
        .map(running_json)
        .collect();
    let fenced =
        show["unknown"].as_i64().unwrap_or(0) > 0 || agent["state"].as_str() == Some("attention");
    // `tasks` lives on the agent_list row, not the show payload.
    let tasks = client::rpc(state_dir, "agent_list", json!({}))
        .ok()
        .and_then(|l| {
            l["agents"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .find(|a| a["alias"].as_str() == Some(alias))
        })
        .map(|a| a["tasks"].clone())
        .unwrap_or(json!([]));
    // Bound issues through the same tasks × jobs.issue_id join.
    let task_map = task_issue_map(state_dir);
    let mut issues: Vec<&str> = Vec::new();
    for t in tasks.as_array().cloned().unwrap_or_default() {
        if let Some(binding) = t.as_str().and_then(|tid| task_map.get(tid)) {
            if !issues.contains(&binding.issue.as_str()) {
                issues.push(&binding.issue);
            }
        }
    }
    Ok(json!({
        "agent": agent,
        "queued": show["queued"],
        "unknown": show["unknown"],
        "running": running,
        "events": events,
        "fenced": fenced,
        "recovery": if fenced { agent["error"].clone() } else { Value::Null },
        "resume": registry::resume_command(
            provider, kind,
            agent["thread_id"].as_str().unwrap_or_default(),
            agent["session_id"].as_str().unwrap_or_default(),
            agent["endpoint"].as_str().unwrap_or_default(),
        ),
        "tasks": tasks,
        "on": issues,
    }))
}

fn handle(mut request: Request, state_dir: &Path, pm_dir: &Path, opts: &ServeOpts) {
    // CAD-482: a seam-armed board honors the assertion headers; the
    // scope makes the asserted identity visible to attribution and to
    // the daemon calls this request relays. Absent headers, or absent
    // the seam, the real peer checks run unchanged. A malformed or
    // forged assertion refuses the request outright.
    let _seam_scope = match crate::test_seam::scope_headers(
        opts.seam.as_ref(),
        header_value(&request, crate::test_seam::AS_HEADER).as_deref(),
        header_value(&request, crate::test_seam::TOKEN_HEADER).as_deref(),
    ) {
        Ok(scope) => scope,
        Err(why) => {
            let _ = request.respond(err_response(403, &why));
            return;
        }
    };
    let method = request.method().clone();
    let head_only = method == Method::Head;
    let is_write = matches!(
        method,
        Method::Post | Method::Patch | Method::Delete | Method::Put
    );
    if !matches!(method, Method::Get | Method::Head) && !is_write {
        let _ = request.respond(err_response(405, "method not allowed"));
        return;
    }
    let host = request
        .headers()
        .iter()
        .find(|h| h.field.equiv("Host"))
        .map(|h| h.value.as_str().to_string())
        .unwrap_or_default();
    if !host_allowed(&host, opts.port, &opts.allow_hosts) {
        let _ = request.respond(err_response(421, "misdirected request — Host not allowed"));
        return;
    }
    let raw_url = request.url().to_string();
    let (raw_path, raw_query) = raw_url.split_once('?').unwrap_or((&raw_url, ""));
    let Some(path) = pct_decode(raw_path) else {
        let _ = request.respond(err_response(400, "malformed path"));
        return;
    };
    if path.contains("..") || path.contains('\0') {
        let _ = request.respond(err_response(400, "bad path"));
        return;
    }
    let query = |key: &str| -> Option<String> {
        raw_query.split('&').find_map(|kv| {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            if k == key {
                pct_decode(v)
            } else {
                None
            }
        })
    };

    // Every value of a repeatable key — `tag=a&tag=b` and `tag=a,b` agree.
    let query_all = |key: &str| -> Vec<String> {
        raw_query
            .split('&')
            .filter_map(|kv| {
                let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
                (k == key).then(|| pct_decode(v)).flatten()
            })
            .flat_map(|v| {
                v.split(',')
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .collect()
    };

    let send = |req: Request, resp: HttpResp| send(req, resp, head_only);

    // CAD-786: the recipient's unsubscribe confirmation page — the
    // token in the path is the credential, so the page is answered
    // before every session gate, on any host this board serves. The
    // matching POST is handled as a `RecipientToken` write.
    if matches!(method, Method::Get) && path.starts_with("/unsubscribe/") {
        let response = match crm_send::unsubscribe_token(&path) {
            Some(token) => crm_send::unsubscribe_page(&token),
            None => err_response(404, "not found"),
        };
        send(request, response);
        return;
    }

    // CAD-526: a request that names this board's public host is on the
    // platform sign-in surface. `/__platform/*` is the contract's
    // reserved prefix; every other request needs the
    // `__Host-aos-board-session` the session endpoint mints — never the
    // local login flow (`operator::open` refuses `Origin::Public` too).
    let public_host = opts
        .public
        .as_ref()
        .is_some_and(|p| host.trim().eq_ignore_ascii_case(&p.host));
    // CAD-747: a hosted agent shares this network namespace and can dial
    // the board's loopback port (or the image's blind TCP relay) itself.
    // Host is therefore a surface selector, never proof of the Worker.
    // In hosted-only mode the public board session gate must cover every
    // protected route, even when the caller chooses a local Host.
    if opts.board_public_only && !public_host {
        if matches!(method, Method::Get | Method::Head)
            && raw_path == "/api/health"
            && raw_query.is_empty()
        {
            send(
                request,
                json_response(json!({
                    "ok": true,
                    "build": crate::overview::BUILD_COMMIT,
                })),
            );
        } else {
            send(
                request,
                err_response(421, "hosted board requires its public Host"),
            );
        }
        return;
    }
    if public_host {
        if path.starts_with("/__platform/") {
            let resp = match (method.as_str(), path.as_str()) {
                ("GET" | "HEAD", "/__platform/login") => operator::platform_login(opts, raw_query),
                ("GET" | "HEAD", _) => operator::platform_read(&path),
                ("POST", "/__platform/session") => {
                    operator::platform_session(&mut request, state_dir, opts)
                }
                _ => operator::platform_unknown(),
            };
            send(request, resp);
            return;
        }
        // CAD-1019: `POST /api/cli/<verb>` — the container end of the
        // remote CLI (AOS-128's `/__platform/cli/call` forwards here).
        // The `wikienv_` bearer is the credential — cookie sessions
        // never satisfy it and it satisfies no other route — so the
        // family is diverted ahead of the session gates and the write
        // path's guards (its content-type/origin marks are the
        // platform relay's, not a browser's). The verb's own checks
        // run inside `cli_route::post`, before any body or handler.
        if let Some(tail) = path.strip_prefix("/api/cli/") {
            let resp = cli_route::post(&mut request, &method, tail, state_dir, pm_dir, opts);
            send(request, resp);
            return;
        }
        // AOS-150's dedicated owner-intent GET is a narrow signed-assertion
        // capability, not a board-cookie read. It is served only on this
        // board's configured public Host; the daemon independently verifies
        // issuer/JWKS/scope/time and consumes the assertion jti once.
        if let Some(route) = social_publish::route(&path) {
            if let Some(intent_id) = route.owner_intent_id() {
                let response = if method != Method::Get {
                    err_response(405, "signed owner-intent route is GET only")
                } else if opts
                    .public
                    .as_ref()
                    .is_none_or(|public| !host.trim().eq_ignore_ascii_case(&public.host))
                {
                    err_response(404, "owner intent not found")
                } else {
                    social_publish::handle_owner_intent_read(&request, state_dir, intent_id)
                };
                send(request, response);
                return;
            }
        }
        if is_write {
            let send_write = |req: Request, resp: HttpResp| {
                read_model::get(state_dir, pm_dir).invalidate();
                send(req, resp)
            };
            write_route(
                request,
                &method,
                &path,
                &query,
                state_dir,
                pm_dir,
                opts,
                &send_write,
            );
            return;
        }
        // A read on the public host needs a live board session —
        // `/api/health` and `/api/version` stay open so a probe can see
        // the board is up, and a signed-out tab can still learn the
        // serving build, without holding a credential.
        if path != "/api/health" && path != "/api/version" {
            match operator::public_session(&request, state_dir, opts) {
                Ok(Some(_)) => {}
                Ok(None) => {
                    let resp = operator::session_bounce(&request, &path, opts);
                    send(request, resp);
                    return;
                }
                Err(resp) => {
                    send(request, resp);
                    return;
                }
            }
        }
        // Signed-in reads fall through to the shared dispatch below.
    }

    if is_write {
        // The writer's next read must see its write (CAD-325): drop the
        // read model's daemon-side caches before the answer goes out.
        let send_write = |req: Request, resp: HttpResp| {
            read_model::get(state_dir, pm_dir).invalidate();
            send(req, resp)
        };
        write_route(
            request,
            &method,
            &path,
            &query,
            state_dir,
            pm_dir,
            opts,
            &send_write,
        );
        return;
    }

    match path.as_str() {
        // What the SPA needs to render itself correctly for this
        // client: read-only mode, the actor this request would write
        // as, and the tailnet URL when sharing is armed. Build identity
        // rides too — the serving binary's, plus the daemon's when
        // reachable (`daemon_info` carries the running build, which is
        // the one deploy drift measures).
        "/api/meta" => {
            let daemon = client::rpc(state_dir, "daemon_info", json!({})).ok();
            let (actor, tailnet_proof) = request_identity(&request, opts);
            // CAD-432: may this client make the operator's board
            // decisions — the same proof those writes run. It walks
            // /proc, so it is computed only when asked (`?operator=1`,
            // once per page load), never on the 30 s poll.
            let operator = matches!(query("operator").as_deref(), Some("1" | "true"))
                .then(|| home::operator_viewer(&request, state_dir, opts));
            let session = operator::meta(&request, state_dir, opts);
            // Display the same verified identity that attributes public
            // board writes, never a name supplied by request fields.
            // Public-origin sessions carry a verified user; every other
            // session is the operator's, matching `held_of`'s
            // attribution exactly.
            let actor = if session["session"]["origin"].as_str() == Some("public") {
                serde_json::from_value::<crate::operator_auth::BoardUser>(
                    session["session"]["user"].clone(),
                )
                .map(|user| user.actor())
                .unwrap_or(actor)
            } else {
                actor
            };
            send(
                request,
                json_response(json!({
                    "read_only": opts.read_only,
                    "signed_in": session["signed_in"],
                    "hosted": session["hosted"],
                    "session": session["session"],
                    "login_hint": session["login_hint"],
                    "device_login": session["device_login"],
                    "tab_signed_out": session["tab_signed_out"],
                    "actor": actor,
                    "tailnet_proof": tailnet_proof,
                    "operator": operator,
                    "platform_account_configured": opts.public.as_ref().is_some_and(|board| board.issuer == "http://api.internal"),
                    "tailnet_url": opts
                        .tailnet
                        .as_ref()
                        .map(|(dns, port)| tailnet_url(dns, *port)),
                    "version": env!("CARGO_PKG_VERSION"),
                    "build_commit": crate::overview::BUILD_COMMIT,
                    "build_time": crate::overview::BUILD_TIME,
                    "daemon": daemon,
                    // CAD-446: the `gh` this board's sync runs, fixed at
                    // start; `null` when the board runs no sync.
                    "delivery_sync": opts.delivery_sync.as_ref().map(|s| s.meta()),
                })),
            );
        }
        // CAD-561: the Update card's view and the draining banner.
        "/api/update" => send(request, updates::get(state_dir)),
        "/api/update/banner" => send(request, updates::banner_get(state_dir)),
        "/api/settings/model-defaults" => {
            send(request, model_defaults_get(state_dir, opts.read_only));
        }
        "/api/platform-account" => send(request, platform_account::get(opts)),
        // CAD-615: the operator's master permission rules and pending
        // requests. The board relays over its own daemon connection, so
        // the HTTP peer is admitted here — the same operator proof as
        // the decision writes.
        "/api/master/permissions" => {
            if let Err(resp) = operator::admit_operator_read(&request, state_dir, opts) {
                send(request, resp);
            } else {
                let resp = match client::rpc(state_dir, "master_permission_list", json!({})) {
                    Ok(out) => json_response(out),
                    Err(e) => home::rpc_err(&e, "master_permission_list"),
                };
                send(request, resp);
            }
        }
        // The serving binary's build id and nothing else — cheap, and
        // unauthenticated like `/api/health`, so a tab whose stream is
        // stuck reconnecting can compare it against its own bundle's.
        "/api/version" => {
            send(
                request,
                json_response(json!({"build": crate::overview::BUILD_ID})),
            );
        }
        "/api/health" => {
            let pm = Pm::at(pm_dir).ok();
            let (projects, issues) = match &pm {
                Some(pm) => {
                    let p = project::list(&pm.dir).map(|l| l.len()).unwrap_or(0);
                    let i = board::load_all(&pm.dir, None).map(|l| l.len()).unwrap_or(0);
                    (p, i)
                }
                None => (0, 0),
            };
            let daemon = if client::rpc(state_dir, "health", json!({})).is_ok() {
                "reachable"
            } else {
                "unreachable"
            };
            send(
                request,
                json_response(json!({
                    "ok": true,
                    "pm_dir": pm.as_ref().map(|p| p.dir.clone()),
                    "pm_present": pm.is_some(),
                    "projects": projects, "issues": issues,
                    "daemon": daemon,
                    "embedded": cfg!(feature = "ui"),
                    // CAD-561: which build is answering, so
                    // `cadence update`'s health check can require the
                    // board to come back on the new release.
                    "build": crate::overview::BUILD_COMMIT,
                })),
            );
        }
        "/api/setup" => {
            // Refused before anything runs: no probe for a viewer.
            let resp = match setup_refusal(&request, opts) {
                Some(refused) => refused,
                None => {
                    let fresh = matches!(query("fresh").as_deref(), Some("1" | "true"));
                    setup_get(state_dir, pm_dir, opts.port, fresh)
                }
            };
            send(request, resp);
        }
        "/api/overview" => {
            let mut overview = read_model::get(state_dir, pm_dir).overview();
            // CAD-446: a page view asks for a delivery sync (bounded,
            // never during a back-off) and shows the sync's problem.
            if let Some(sync) = &opts.delivery_sync {
                sync.nudge();
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                if let Some(needs) = overview["needs_me"].as_array_mut() {
                    needs.extend(sync.needs_rows(now));
                }
                // CAD-574: the sync rows are appended after the build —
                // the dismissal filter runs again over the union so a
                // sync row is suppressible like any other.
                let dismissed = crate::needs_dismiss::dismissed(state_dir);
                if !dismissed.is_empty() {
                    if let Some(needs) = overview["needs_me"].as_array_mut() {
                        let kept = crate::needs_dismiss::filter_rows(
                            std::mem::take(needs),
                            &dismissed,
                            now,
                        );
                        *needs = kept;
                    }
                }
            }
            send(request, json_response(overview))
        }
        "/api/projects" => match Pm::at(pm_dir) {
            Ok(pm) => {
                send(
                    request,
                    json_response(read_model::get(state_dir, pm_dir).projects(&pm)),
                );
            }
            Err(e) => send(request, err_response(503, &e.to_string())),
        },
        "/api/issues" => match Pm::at(pm_dir) {
            Ok(pm) => {
                let filter = query("project");
                if let Some(p) = &filter {
                    if !model::valid_key(p) {
                        send(request, err_response(400, "bad project key"));
                        return;
                    }
                }
                // The same slices as `issue ls`: tag (all of — the
                // grammar's exception), every other key any-of over
                // repeated/comma values, open=1.
                let slice = board::Filter {
                    tags: query_all("tag"),
                    epics: query_all("epic"),
                    owners: query_all("owner"),
                    statuses: query_all("status"),
                    components: query_all("component"),
                    priorities: query_all("priority"),
                    types: query_all("type"),
                    milestones: query_all("milestone"),
                    plans: query_all("plan"),
                    open: query("open").is_some_and(|v| v == "1" || v == "true"),
                };
                if let Err(e) = slice.validate() {
                    send(request, err_response(400, &e.to_string()));
                    return;
                }
                let read = read_model::get(state_dir, pm_dir).board(&pm, filter.as_deref());
                // CAD-405: each card carries its `work` block.
                let by_id = read.by_id();
                let ctx = read.ctx(&by_id);
                send(
                    request,
                    json_response(json!({
                        "issues": read
                            .views
                            .iter()
                            .filter(|v| slice.matches(v))
                            .map(|v| read.card(&ctx, v))
                            .collect::<Vec<_>>(),
                    })),
                );
            }
            Err(e) => send(request, err_response(503, &e.to_string())),
        },
        // `GET /api/epics?project=` — issues with children and their
        // children's progress; the payload `issue epic ls --json` prints.
        "/api/epics" => match Pm::at(pm_dir) {
            Ok(pm) => {
                let filter = query("project");
                if filter.as_ref().is_some_and(|p| !model::valid_key(p)) {
                    send(request, err_response(400, "bad project key"));
                    return;
                }
                // Every project loads so cross-project children count.
                let read = read_model::get(state_dir, pm_dir).board(&pm, None);
                send(
                    request,
                    json_response(json!({
                        "epics": crate::issue::work::epics_json(
                            &pm.dir,
                            &read.views,
                            filter.as_deref(),
                            crate::issue::time::now_epoch(),
                            &read.approvals,
                        ),
                    })),
                );
            }
            Err(e) => send(request, err_response(503, &e.to_string())),
        },
        "/api/milestones" => send(
            request,
            stages::milestones(state_dir, pm_dir, query("project").as_deref()),
        ),
        "/api/agents" => send(
            request,
            json_response(read_model::get(state_dir, pm_dir).agents()),
        ),
        // CAD-546: the `local` platform's outbox — an operator-only
        // read, the same proof the board's operator write routes take.
        "/api/outbox" => {
            let resp = home::outbox(&request, state_dir, opts, query("effect_id"));
            send(request, resp);
        }
        "/api/memories" => match Pm::at(pm_dir) {
            Ok(pm) => {
                // The `memory ls` grammar: keys repeat and comma-join
                // (any-of), different keys AND.
                let project = query_all("project");
                let status = query_all("status");
                let kind = query_all("type");
                let component = query_all("component");
                let paths = query_all("path");
                if let Err(e) = crate::filter::check_set("status", &status, crate::memory::STATUSES)
                    .and_then(|_| crate::filter::check_set("type", &kind, crate::memory::TYPES))
                {
                    send(request, err_response(400, &e.to_string()));
                    return;
                }
                let (mems, errors) = crate::memory::load_all_report(&pm.dir);
                let projects = project::list(&pm.dir).unwrap_or_default();
                let payload: Vec<Value> =
                    mems.iter()
                        .filter(|m| {
                            let scope = &m.front.scope;
                            crate::filter::any_of(&project, Some(m.project.as_str()))
                                && crate::filter::any_of(&status, Some(m.front.status.as_str()))
                                && crate::filter::any_of(&kind, Some(m.front.kind.as_str()))
                                && (component.is_empty()
                                    || scope.project
                                    || scope.components.iter().any(|c| component.contains(c)))
                                && (paths.is_empty()
                                    || scope.project
                                    || scope.paths.iter().any(|g| {
                                        paths.iter().any(|p| crate::memory::glob_match(g, p))
                                    }))
                        })
                        .map(|m| {
                            let fresh = crate::memory::Freshness::among(&projects, &m.project);
                            crate::memory::card_json(m, &fresh)
                        })
                        .collect();
                send(
                    request,
                    json_response(json!({
                        "memories": payload,
                        "memory_errors": errors,
                    })),
                );
            }
            Err(e) => send(request, err_response(503, &e.to_string())),
        },
        "/api/stream" => {
            if head_only {
                send(request, err_response(405, "stream is GET only"));
            } else {
                stream_events(request, state_dir, pm_dir);
            }
        }
        _ => {
            // The wiki reads (CAD-580): the request's caller — session,
            // named member, attributed agent — rides `wiki_as`; a caller
            // the board cannot attribute is refused before the daemon
            // sees it. Blob pages stream `.blobs/<sha>` with ranges.
            if let Some(tail) = path.strip_prefix("/api/wiki/") {
                let resp = wiki::read(&request, tail, &query, state_dir, pm_dir, opts);
                send(request, resp);
                return;
            }
            // A selected project's bounded, tracked-document context. The
            // project key is resolved through the PM registry before any repo
            // path is touched; no request value becomes a filesystem path.
            if let Some(tail) = path.strip_prefix("/api/projects/") {
                let mut segs = tail.split('/');
                let key = segs.next().unwrap_or_default();
                let sub = segs.next();
                if sub == Some("context") && segs.next().is_none() {
                    if !model::valid_key(key) {
                        send(request, err_response(400, "bad project key"));
                        return;
                    }
                    let (role, expected_revision) = match context_query(raw_query) {
                        Ok(query) => query,
                        Err(error) => {
                            send(request, err_response(400, error));
                            return;
                        }
                    };
                    match Pm::at(pm_dir) {
                        Ok(pm) => match project::list(&pm.dir) {
                            Ok(projects) => match projects.iter().find(|p| p.key == key) {
                                Some(selected) => send(
                                    request,
                                    json_response(context::bundle(
                                        &pm,
                                        selected,
                                        role.as_deref(),
                                        expected_revision.as_deref(),
                                    )),
                                ),
                                None => send(request, err_response(404, "unknown project")),
                            },
                            Err(error) => send(request, err_response(503, &error.to_string())),
                        },
                        Err(error) => send(request, err_response(503, &error.to_string())),
                    }
                    return;
                }
                // `/api/projects/<key>/workflows[/<name>/preview]` — the
                // project's workflow templates beside PROJECT.md (CAD-496).
                if let Some(read) = workflows::read_route(&path) {
                    match Pm::at(pm_dir) {
                        Ok(pm) => send(request, workflows::read(&pm, state_dir, &query, read)),
                        Err(e) => send(request, err_response(503, &e.to_string())),
                    }
                    return;
                }
            }
            // CAD-1006: the nonce-consumed frame document — exact prefix,
            // ordered before every `/api/app-installations/` arm. The
            // nonce is the SOLE authority relayed to `app_screen_consume`
            // (peer guard + burned cap + stored-session liveness + digest/
            // approval re-proof), so it deliberately does NOT run
            // `admit_operator_read`.
            if let Some(nonce) = app_screens::frame_route(&path) {
                let response = app_screens::frame(&request, state_dir, opts, nonce);
                send(request, response);
                return;
            }
            // CAD-1110: the approved app's chat descriptor — the install
            // id is the path's only, the operator proof is the sibling
            // reads', and every non-found case is one 404.
            if let Some(install) = app_chat::route(&path) {
                if let Err(response) = operator::admit_operator_read(&request, state_dir, opts) {
                    send(request, response);
                    return;
                }
                send(request, app_chat::handle(state_dir, install));
                return;
            }
            if let Some(route) = app_assistant::route(&path) {
                if !route.is_read() {
                    send(request, err_response(405, "method not allowed"));
                    return;
                }
                if let Err(response) = operator::admit_operator_read(&request, state_dir, opts) {
                    send(request, response);
                    return;
                }
                let response = app_assistant::handle(&mut request, state_dir, route, raw_query);
                send(request, response);
                return;
            }
            if let Some(route) = app_contexts::route(&path) {
                if !route.is_read() {
                    send(request, err_response(405, "method not allowed"));
                    return;
                }
                if let Err(response) = operator::admit_operator_read(&request, state_dir, opts) {
                    send(request, response);
                    return;
                }
                let response = app_contexts::handle(&mut request, state_dir, route, false);
                send(request, response);
                return;
            }
            if let Some(route) = app_audiences::route(&path) {
                if !route.is_read() {
                    send(request, err_response(405, "method not allowed"));
                    return;
                }
                if let Err(response) = operator::admit_operator_read(&request, state_dir, opts) {
                    send(request, response);
                    return;
                }
                let response = app_audiences::handle(&mut request, state_dir, route, false);
                send(request, response);
                return;
            }
            if let Some(route) = app_records::route(&path) {
                if !route.is_read() {
                    send(request, err_response(405, "method not allowed"));
                    return;
                }
                if let Err(response) = operator::admit_operator_read(&request, state_dir, opts) {
                    send(request, response);
                    return;
                }
                let response = app_records::handle(&mut request, state_dir, route, false);
                send(request, response);
                return;
            }
            if let Some(route) = app_content::route(&path) {
                if !route.is_read() {
                    send(request, err_response(405, "method not allowed"));
                    return;
                }
                if let Err(response) = operator::admit_operator_read(&request, state_dir, opts) {
                    send(request, response);
                    return;
                }
                let response = app_content::handle(&mut request, state_dir, route, false);
                send(request, response);
                return;
            }
            if let Some(route) = connections::route(&path) {
                if !route.is_read() {
                    send(request, err_response(405, "method not allowed"));
                    return;
                }
                if let Err(response) = operator::admit_operator_read(&request, state_dir, opts) {
                    send(request, response);
                    return;
                }
                let response = connections::handle(&mut request, state_dir, route, false);
                send(request, response);
                return;
            }
            if let Some(route) = crm_send::route(&path) {
                if !route.is_read() {
                    send(request, err_response(405, "method not allowed"));
                    return;
                }
                if let Err(response) = operator::admit_operator_read(&request, state_dir, opts) {
                    send(request, response);
                    return;
                }
                let response = crm_send::handle_read(state_dir, route, &query);
                send(request, response);
                return;
            }
            if let Some(route) = app_release::route(&path) {
                if !route.is_read() {
                    send(request, err_response(405, "method not allowed"));
                    return;
                }
                if let Err(response) = operator::admit_operator_read(&request, state_dir, opts) {
                    send(request, response);
                    return;
                }
                let response = app_release::handle(&mut request, state_dir, route, false);
                send(request, response);
                return;
            }
            if let Some(route) = app_runs::route(&path) {
                if !route.is_read() {
                    send(request, err_response(405, "method not allowed"));
                    return;
                }
                if let Err(response) = operator::admit_operator_read(&request, state_dir, opts) {
                    send(request, response);
                    return;
                }
                let response = app_runs::handle(&mut request, state_dir, route, false);
                send(request, response);
                return;
            }
            if let Some(route) = social_publish::route(&path) {
                if !route.is_read() {
                    send(request, err_response(405, "method not allowed"));
                    return;
                }
                if let Err(response) = operator::admit_operator_read(&request, state_dir, opts) {
                    send(request, response);
                    return;
                }
                let response = social_publish::handle(
                    &mut request,
                    state_dir,
                    route,
                    false,
                    opts.public.as_ref(),
                );
                send(request, response);
                return;
            }
            // CAD-1098: `/api/app-installations/<id>/conversations` — the
            // master's conversations for that installation.
            if let Some(install) = path
                .strip_prefix("/api/app-installations/")
                .and_then(|tail| tail.strip_suffix("/conversations"))
                .filter(|id| !id.is_empty() && !id.contains('/'))
            {
                if let Err(response) = operator::admit_operator_read(&request, state_dir, opts) {
                    send(request, response);
                    return;
                }
                let response =
                    threads::conversations(state_dir, crate::master::ALIAS, &|key: &str| {
                        (key == "install").then(|| install.to_string())
                    });
                send(request, response);
                return;
            }
            if path == "/api/app-installations"
                || path
                    .strip_prefix("/api/app-installations/")
                    .is_some_and(|id| !id.is_empty() && !id.contains('/'))
            {
                if let Err(response) = operator::admit_operator_read(&request, state_dir, opts) {
                    send(request, response);
                    return;
                }
                let id = path.strip_prefix("/api/app-installations/");
                let method = if id.is_some() {
                    "app_workspace_show"
                } else {
                    "app_workspace_list"
                };
                let response = apps::workspace(&mut request, state_dir, method, id);
                send(request, response);
                return;
            }
            // CAD-1129: the apps Explorer — `/api/app-catalog`,
            // `/api/app-home`, `/api/app-favorites`, `/api/app-requests`.
            // Member-capable reads ride `board_caller` like `/api/wiki`;
            // the daemon re-proves `member_as` before a member row leaves.
            if path == "/api/app-catalog"
                || path.starts_with("/api/app-catalog/")
                || path == "/api/app-home"
                || path == "/api/app-favorites"
                || path == "/api/app-requests"
            {
                let response = app_explorer::read(&request, &path, &query, state_dir, opts);
                send(request, response);
                return;
            }
            // `/api/apps[/<project>/<name>[/runs|/outputs]]` — installed
            // apps: the list, or one app's guide, workflows, rubrics,
            // bindings and doctor findings, its runs and its outputs
            // (CAD-557, CAD-563).
            if let Some(read) = apps::read_route(&path) {
                match Pm::at(pm_dir) {
                    Ok(pm) => {
                        let resp = apps::read(&request, &pm, state_dir, opts, &query, read);
                        send(request, resp);
                    }
                    Err(e) => send(request, err_response(503, &e.to_string())),
                }
                return;
            }
            // `/api/memories/<project>/<slug>` — memory detail.
            if let Some(tail) = path.strip_prefix("/api/memories/") {
                let mut segs = tail.splitn(2, '/');
                let (key, slug) = (
                    segs.next().unwrap_or_default(),
                    segs.next().unwrap_or_default(),
                );
                if !crate::memory::valid_slug(slug) || key.is_empty() || slug.contains('/') {
                    send(request, err_response(400, "bad memory path"));
                    return;
                }
                match Pm::at(pm_dir).and_then(|pm| crate::memory::find(&pm, Some(key), slug)) {
                    Ok((proj, m)) => {
                        let fresh = crate::memory::Freshness::for_project(Some(&proj));
                        send(
                            request,
                            json_response(crate::memory::detail_json(&m, &fresh)),
                        )
                    }
                    Err(e) => send(request, err_response(404, &e.to_string())),
                }
                return;
            }
            // `/api/master/models` — the master's model-picker read
            // (CAD-575): operator-gated like the `master_models` RPC
            // it relays.
            if path == "/api/master/models" {
                let resp = home::master_models(&request, state_dir, opts);
                send(request, resp);
                return;
            }
            // `/api/master/summary?since=` — "since you left" (CAD-328).
            if path == "/api/master/summary" {
                send(request, home::master_summary(state_dir, &query));
                return;
            }
            // `/api/master/state` — the header chips + turn state (CAD-551).
            if path == "/api/master/state" {
                send(request, home::master_state(state_dir));
                return;
            }
            // `/api/threads/<alias>[/stream]` — an agent's chat (CAD-319).
            if let Some((alias, sub)) = threads::route(&path) {
                match sub {
                    None => send(request, threads::read(state_dir, alias, &query)),
                    Some("stream") if head_only => {
                        send(request, err_response(405, "stream is GET only"))
                    }
                    Some("stream") => threads::stream(request, state_dir, alias, &query),
                    Some(_) => send(request, err_response(404, "no such thread route")),
                }
                return;
            }
            // `/api/agents/<alias>` — the drawer detail endpoint.
            if let Some(alias) = path.strip_prefix("/api/agents/") {
                if alias.is_empty()
                    || alias.len() > 80
                    || !alias
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
                {
                    send(request, err_response(400, "bad agent alias"));
                    return;
                }
                match agent_detail(state_dir, alias) {
                    Ok(detail) => send(request, json_response(detail)),
                    Err(e) => send(request, err_response(404, &e)),
                }
                return;
            }
            // `/api/issues/<ID>[/file|/activity|/history|/artifacts/<name>]`
            // — id grammar checked before the id is ever a path component.
            if let Some(tail) = path.strip_prefix("/api/issues/") {
                let mut segs = tail.splitn(2, '/');
                let id_raw = segs.next().unwrap_or_default();
                let sub = segs.next();
                if sub.is_some_and(|s| {
                    !matches!(s, "file" | "activity" | "history" | "kickoff" | "lane")
                        && !s.starts_with("artifacts/")
                }) {
                    send(request, err_response(404, "no such route"));
                    return;
                }
                let Ok(id) = model::check_id(id_raw) else {
                    send(request, err_response(400, "bad issue id"));
                    return;
                };
                match Pm::at(pm_dir) {
                    Ok(pm) => {
                        let read = read_model::get(state_dir, pm_dir).board(&pm, None);
                        let by_id: std::collections::HashMap<String, &board::View> = read
                            .views
                            .iter()
                            .map(|v| (v.issue.front.id.clone(), v))
                            .collect();
                        let Some(view) = by_id.get(&id) else {
                            send(request, err_response(404, "unknown issue"));
                            return;
                        };
                        match sub {
                            Some("kickoff") => {
                                let resp = home::kickoff_options(&request, state_dir, opts, &id);
                                send(request, resp);
                            }
                            None => send(
                                request,
                                json_response(with_agents(
                                    crate::issue::work::detail_json(
                                        &pm.dir,
                                        &read.ctx(&by_id),
                                        view,
                                    ),
                                    &read.by_issue,
                                    &id,
                                )),
                            ),
                            Some("file") => {
                                let file = view.issue.dir.join("issue.md");
                                match std::fs::read(&file) {
                                    Ok(bytes) => {
                                        let mut resp = Response::from_data(bytes);
                                        resp.add_header(
                                            Header::from_bytes(
                                                "Content-Type",
                                                "text/markdown; charset=utf-8",
                                            )
                                            .unwrap(),
                                        );
                                        send(request, resp);
                                    }
                                    Err(_) => send(request, err_response(404, "no issue.md")),
                                }
                            }
                            Some("activity") => send(
                                request,
                                json_response(json!({
                                    "id": id,
                                    "activity": board::activity_json(&pm.dir, view),
                                })),
                            ),
                            // `GET /api/issues/<ID>/history?limit=N` —
                            // the same entries `issue log` prints.
                            Some("history") => {
                                let limit = match query("limit") {
                                    Some(raw) => match raw.parse::<usize>() {
                                        Ok(n) if n > 0 => n,
                                        _ => {
                                            send(request, err_response(400, "bad limit"));
                                            return;
                                        }
                                    },
                                    None => 50,
                                };
                                match history::log(&pm.dir, &view.issue, limit) {
                                    Ok(h) => send(
                                        request,
                                        json_response(json!({"id": id, "history": h})),
                                    ),
                                    Err(e) => send(request, err_response(503, &e.to_string())),
                                }
                            }
                            Some("lane") => send(request, lane::show(state_dir, &id)),
                            Some(s) if s.starts_with("artifacts/") => {
                                let name = s.strip_prefix("artifacts/").unwrap_or_default();
                                send(request, artifact_response(view, name));
                            }
                            Some(_) => unreachable!(),
                        }
                    }
                    Err(e) => send(request, err_response(503, &e.to_string())),
                }
                return;
            }
            if path.starts_with("/api/") {
                send(request, err_response(404, "no such route"));
                return;
            }
            // Static: a file of the build, the SPA shell for a client
            // route, or 404 for a file that is not there.
            match static_answer_for(
                opts.dist.as_deref(),
                &path,
                header_value(&request, "Accept").as_deref(),
            ) {
                StaticAnswer::File(name, bytes) => {
                    let mut resp = Response::from_data(bytes);
                    resp.add_header(
                        Header::from_bytes("Content-Type", content_type(&name)).unwrap(),
                    );
                    resp.add_header(Header::from_bytes("Cache-Control", "no-store").unwrap());
                    send(request, resp);
                }
                StaticAnswer::Missing => send(request, err_response(404, "no such file")),
                StaticAnswer::NoBuild => send(
                    request,
                    err_response(
                        503,
                        "no SPA build — pass --dist or rebuild with --features ui",
                    ),
                ),
            }
        }
    }
}
pub fn serve(state_dir: &Path, pm_dir: &Path, opts: &ServeOpts) -> Result<()> {
    if opts.board_public_only && opts.public.is_none() {
        return Err(Error::rejected(
            "board public-only mode requires a public board identity",
        ));
    }
    // CAD-526: a publicly-named board hands the daemon its trust root —
    // `operator/board-identity.json`, under the same uid-private rules
    // as the operator secret — before the first request can mint a
    // session against it.
    if let Some(public) = &opts.public {
        crate::board_identity::write_config(
            state_dir,
            &crate::board_identity::Config {
                host: public.host.clone(),
                issuer: public.issuer.clone(),
                company: public.company.clone(),
            },
        )?;
    }
    let mut opts = opts.clone();
    // The readiness nonce reaches a spawned board through its
    // environment (`ui start` sets it); an in-process fixture sets the
    // field. Either way it is consumed here — it never appears in a
    // response, and nothing after this point needs the env var.
    if let Ok(nonce) = std::env::var(READY_NONCE_ENV) {
        std::env::remove_var(READY_NONCE_ENV);
        if !nonce.is_empty() && opts.ready_nonce.is_none() {
            opts.ready_nonce = Some(nonce);
        }
    }
    // The tailnet proof's operator latch starts with this process: read
    // tailscaled's operator user now, never trust a caller-made latch.
    opts.agent_uid = board_boot_agent_uid(state_dir, opts.agent_uid)?;
    opts.tailnet_latch = if opts.tailnet.is_some() {
        crate::tailnet_proof::OperatorLatch::at_startup(opts.tailscaled_socket.as_deref())
    } else {
        Default::default()
    };
    let server = Server::http(format!("{}:{}", opts.host, opts.port)).map_err(|e| {
        if let Some(startup) = opts.startup.take() {
            let kind = e
                .downcast_ref::<std::io::Error>()
                .map_or(std::io::ErrorKind::Other, std::io::Error::kind);
            let _ = startup.send(Err(kind));
        }
        Error::internal(format!("ui bind {}:{}: {e}", opts.host, opts.port))
    })?;
    // CAD-841: the daemon owns the device-login config — no pin file,
    // no lock, nothing else for a board process to write or clear. A
    // `ui run --device-login-*` push lands only now, bound port in
    // hand: a failed start never replaced the live settings other
    // boards of this state dir serve (review r1).
    if let Some(triple) = &opts.device_login_push {
        push_device_login_config(state_dir, triple)?;
    }
    // CAD-446: merge decisions appear without a terminal — this process
    // (the operator's, when it proves so) reads the loop's PRs with the
    // operator's `gh`. Started only once the port is ours; a read-only
    // board writes nothing, observations included.
    // `gh` is fixed to an absolute path once, here: neither the sync
    // nor Merge looks it up on PATH again.
    let gh = delivery_sync::resolve_gh(
        opts.gh.as_deref().unwrap_or(Path::new(crate::delivery::GH)),
        std::env::var_os("PATH").as_deref(),
    );
    if let Ok(abs) = &gh {
        opts.gh = Some(abs.clone());
    }
    // CAD-482: a seam-armed board attaches to the credential its
    // fixture daemon minted — refused loudly on other builds/dirs so a
    // fixture never silently falls back to ambient identity. A state
    // dir that still carries the minted token re-attaches: `daemon
    // restart --ui` respawns this process without the arming env.
    opts.seam = crate::test_seam::attach_if_requested(
        state_dir,
        opts.test_seam || crate::test_seam::armed(state_dir),
    )?;
    opts.delivery_sync = (!opts.read_only).then(|| {
        delivery_sync::start(
            state_dir,
            pm_dir,
            opts.delivery_sync_every,
            gh,
            opts.seam.is_some(),
        )
    });
    if let Some(startup) = opts.startup.take() {
        let _ = startup.send(Ok(()));
    }
    let opts = &opts;
    // Only now — the port is bound — prove readiness to the `ui start`
    // that spawned this board. A bind failure above never writes it, so
    // the waiting start sees the dead child plus its ui.log, not a
    // foreign board's health answer (CAD-817).
    // CAD-1207 test seam: a board that binds but never proves readiness,
    // so `ui start`'s timeout path can be exercised. Compiled only under
    // the `test-seam` feature, which release builds refuse (CAD-482).
    #[cfg(feature = "test-seam")]
    let withhold_ready = std::env::var_os("CADENCE_TEST_UI_WITHHOLD_READY").is_some();
    #[cfg(not(feature = "test-seam"))]
    let withhold_ready = false;
    if let Some(nonce) = opts.ready_nonce.as_ref().filter(|_| !withhold_ready) {
        let marker = json!({"pid": std::process::id(), "nonce": nonce});
        // A `ui.json` save owns `ui.tmp`; keep the marker's sidecar apart.
        let tmp = ready_file(state_dir).with_extension("ready.tmp");
        if std::fs::write(&tmp, marker.to_string()).is_ok() {
            let _ = std::fs::rename(&tmp, ready_file(state_dir));
        }
    }
    eprintln!("cadence ui listening on http://{}:{}", opts.host, opts.port);
    loop {
        let request = match &opts.stop {
            None => match server.recv() {
                Ok(request) => request,
                Err(_) => break,
            },
            Some(stop) => {
                if stop.load(std::sync::atomic::Ordering::SeqCst) {
                    break;
                }
                match server.recv_timeout(Duration::from_millis(100)) {
                    Ok(Some(request)) => request,
                    Ok(None) => continue,
                    Err(_) => break,
                }
            }
        };
        // Thread per request: `/api/stream` holds its connection open
        // for the session's lifetime and must not starve the board.
        let (state_dir, pm_dir, opts) =
            (state_dir.to_path_buf(), pm_dir.to_path_buf(), opts.clone());
        std::thread::spawn(move || {
            // CAD-777: the device sign-in exchange is exempt — its
            // handlers make issuer HTTP calls (up to 20 s each), so a
            // slow issuer or a `/code` spammer would stall every
            // unrelated board write. Safe: their only shared state is
            // the pending map under its own mutex, and the daemon
            // serializes the mint itself.
            let is_write = matches!(
                request.method(),
                Method::Post | Method::Patch | Method::Delete
            ) && !matches!(
                request.url().split('?').next().unwrap_or(""),
                "/api/session/device/code" | "/api/session/device/poll"
            );
            if is_write {
                let _guard = WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
                handle(request, &state_dir, &pm_dir, &opts);
            } else {
                handle(request, &state_dir, &pm_dir, &opts);
            }
        });
    }
    Ok(())
}
