//! `cadence ui` — the board: a small synchronous HTTP server
//! (tiny_http, no async runtime — the daemon is plain threads too)
//! serving the built SPA plus a JSON API on loopback.
//!
//! No auth, by decision — containment is the defence: loopback bind, no
//! CORS headers, a Host allowlist against DNS rebinding, id grammar
//! checked before any path is touched, and no file reads outside the PM
//! dir or `--dist`. Writes are I2: POST/PATCH/DELETE routes must pass
//! four cross-site guards (known write route, exact JSON/octet-stream
//! content type, `X-Cadence-Board: 1`, same-origin Origin/Sec-Fetch-Site)
//! before any work is done, then go through `issue::write` — the same
//! writer the CLI uses — so CLI and API cannot disagree.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use clap::Subcommand;
use serde::Deserialize;
use serde_json::{json, Value};
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};

use crate::adapter::registry;
use crate::client;
use crate::error::{Error, Result};
use crate::issue::{board, model, project, write as issue_write, Pm};

#[derive(Subcommand)]
pub enum UiAction {
    /// Serve the board + JSON API in the foreground.
    Run {
        /// Bind address [default: 127.0.0.1].
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        /// Port [default: 3010].
        #[arg(long, default_value_t = 3010)]
        port: u16,
        /// Serve the SPA from this directory (required when the binary
        /// was built without `--features ui`).
        #[arg(long)]
        dist: Option<PathBuf>,
        /// Extra allowed Host header values (repeatable).
        #[arg(long = "allow-host")]
        allow_hosts: Vec<String>,
    },
    /// Detached `ui run`: pid + log under the state dir, mirrors
    /// `daemon start`.
    Start {
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        #[arg(long, default_value_t = 3010)]
        port: u16,
        #[arg(long)]
        dist: Option<PathBuf>,
        #[arg(long = "allow-host")]
        allow_hosts: Vec<String>,
    },
    /// Stop the detached UI server.
    Stop,
    /// Report UI server health.
    Status,
}

/// `cadence ui …`
pub fn run_cli(state_dir: &Path, action: &UiAction) -> Result<i32> {
    match action {
        UiAction::Run {
            host,
            port,
            dist,
            allow_hosts,
        } => {
            serve(
                state_dir,
                &crate::issue::default_dir()?,
                host,
                *port,
                dist.clone(),
                allow_hosts,
            )?;
            Ok(0)
        }
        UiAction::Start {
            host,
            port,
            dist,
            allow_hosts,
        } => start(state_dir, host, *port, dist.clone(), allow_hosts),
        UiAction::Stop => stop(state_dir),
        UiAction::Status => status(state_dir),
    }
}

// ---------- server ----------

/// The Host allowlist: the gateway vhost (with or without its port —
/// browsers send `:18000`, a hand-set header may not) plus the bind
/// address forms.
fn host_allowed(host: &str, port: u16, extra: &[String]) -> bool {
    let host = host.trim().to_ascii_lowercase();
    host == "cadence.localhost"
        || host == "cadence.localhost:18000"
        || host == format!("127.0.0.1:{port}")
        || host == format!("localhost:{port}")
        || host == format!("[::1]:{port}")
        || extra.iter().any(|h| h.eq_ignore_ascii_case(&host))
}

fn json_response(value: Value) -> Response<std::io::Cursor<Vec<u8>>> {
    let body = serde_json::to_vec_pretty(&value).unwrap_or_default();
    let mut resp = Response::from_data(body).with_status_code(StatusCode(200));
    resp.add_header(Header::from_bytes("Content-Type", "application/json").unwrap());
    resp
}

fn err_response(code: u16, message: &str) -> Response<std::io::Cursor<Vec<u8>>> {
    let body = serde_json::to_vec_pretty(&json!({"error": message})).unwrap_or_default();
    let mut resp = Response::from_data(body).with_status_code(StatusCode(code));
    resp.add_header(Header::from_bytes("Content-Type", "application/json").unwrap());
    resp
}

/// Defence in depth on an unauthenticated loopback origin that renders
/// agent-written Markdown.
const CSP: &str = "default-src 'self'; img-src 'self' data:; font-src 'self' data:; style-src 'self' 'unsafe-inline'; base-uri 'none'; frame-ancestors 'none'";

/// Every response gets nosniff + no-referrer; HTML additionally gets
/// the CSP. Returns whether the response is HTML.
fn add_security_headers(resp: &mut Response<std::io::Cursor<Vec<u8>>>) -> bool {
    let is_html = resp
        .headers()
        .iter()
        .any(|h| h.field.equiv("Content-Type") && h.value.as_str().starts_with("text/html"));
    resp.add_header(Header::from_bytes("X-Content-Type-Options", "nosniff").unwrap());
    resp.add_header(Header::from_bytes("Referrer-Policy", "no-referrer").unwrap());
    if is_html {
        resp.add_header(Header::from_bytes("Content-Security-Policy", CSP).unwrap());
    }
    is_html
}

/// Percent-decode a URL path/query component (UTF-8, `+` untouched in
/// path context). Returns None on malformed input.
fn pct_decode(raw: &str) -> Option<String> {
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

fn content_type(path: &str) -> &'static str {
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
fn static_file(dist: Option<&Path>, path: &str) -> Option<(String, Vec<u8>)> {
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
            _ => path
                .strip_prefix("/assets/")
                .and_then(|name| embedded::ASSETS.get(name).copied()),
        };
        return bytes.map(|b| (path.to_string(), b.to_vec()));
    }
    #[allow(unreachable_code)]
    None
}

#[cfg(feature = "ui")]
mod embedded {
    use std::collections::HashMap;
    use std::sync::LazyLock;

    pub const INDEX: &str = include_str!("../ui/dist/index.html");
    pub const JS: &str = include_str!("../ui/dist/assets/index.js");
    pub const CSS: &str = include_str!("../ui/dist/assets/index.css");

    /// The latin woff2 files the CSS references (woff fallbacks are not
    /// embedded — every supported browser takes woff2 first).
    pub static ASSETS: LazyLock<HashMap<&'static str, &'static [u8]>> = LazyLock::new(|| {
        HashMap::from([
            (
                "ibm-plex-sans-latin-400-normal.woff2",
                include_bytes!("../ui/dist/assets/ibm-plex-sans-latin-400-normal.woff2") as &[u8],
            ),
            (
                "ibm-plex-sans-latin-500-normal.woff2",
                include_bytes!("../ui/dist/assets/ibm-plex-sans-latin-500-normal.woff2") as &[u8],
            ),
            (
                "ibm-plex-sans-latin-600-normal.woff2",
                include_bytes!("../ui/dist/assets/ibm-plex-sans-latin-600-normal.woff2") as &[u8],
            ),
            (
                "ibm-plex-mono-latin-400-normal.woff2",
                include_bytes!("../ui/dist/assets/ibm-plex-mono-latin-400-normal.woff2") as &[u8],
            ),
            (
                "ibm-plex-mono-latin-500-normal.woff2",
                include_bytes!("../ui/dist/assets/ibm-plex-mono-latin-500-normal.woff2") as &[u8],
            ),
            (
                "ibm-plex-mono-latin-600-normal.woff2",
                include_bytes!("../ui/dist/assets/ibm-plex-mono-latin-600-normal.woff2") as &[u8],
            ),
        ])
    });
}

/// task id → (issue id, task state) for every task that belongs to an
/// issue-bound job — the join that makes agent binding exact instead of
/// scanning message text for issue-shaped tokens.
fn task_issue_map(state_dir: &Path) -> HashMap<String, (String, String)> {
    let mut map = HashMap::new();
    let Ok(list) = client::rpc(state_dir, "job_list", json!({"all": true})) else {
        return map;
    };
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
        let Ok(show) = client::rpc(state_dir, "job_show", json!({"job": job_id})) else {
            continue;
        };
        for task in show["job"]["tasks"].as_array().cloned().unwrap_or_default() {
            if let (Some(tid), Some(state)) = (task["id"].as_str(), task["state"].as_str()) {
                map.insert(tid.to_string(), (issue.clone(), state.to_string()));
            }
        }
    }
    map
}

/// One running message reduced for the board: id, task, turn token.
fn running_json(m: &Value) -> Value {
    json!({
        "id": m["id"],
        "task": m["task_id"],
        "turn_id": m["turn_id"],
        "created": m["created"],
    })
}

/// The tail of an agent's event log for the drawer — the last `n`
/// events via the same `events` RPC the CLI long-polls.
fn agent_events_tail(state_dir: &Path, alias: &str, cursor: i64, n: i64) -> Vec<Value> {
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
/// the board still renders.
fn agents_payload(state_dir: &Path) -> Value {
    let list = match client::rpc(state_dir, "agent_list", json!({})) {
        Ok(list) => list,
        Err(_) => {
            return json!({"daemon": "unreachable", "agents": [], "totals": null, "by_issue": {}});
        }
    };
    let task_map = task_issue_map(state_dir);
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
            out.push(json!({
                "alias": alias, "provider": agent["provider"],
                "endpoint_kind": agent["endpoint_kind"],
                "state": "inbox", "group": agent["params"]["upstream"].as_str().unwrap_or(alias),
                "group_root": agent["params"]["upstream"].is_null(),
                "running": 0, "queued": 0, "unknown": 0, "parked": 0,
                "fenced": false, "on": [], "tasks": [], "message": Value::Null,
                "dead": agent["dead"], "inbox": true,
            }));
            continue;
        }
        let show = client::rpc(state_dir, "agent_show", json!({"alias": alias}));
        let (mut running, mut parked) = (0i64, 0i64);
        let mut running_msgs: Vec<Value> = Vec::new();
        let mut last_activity = Value::Null;
        let (queued, unknown, cursor) = match &show {
            Ok(show) => {
                for m in show["messages"].as_array().cloned().unwrap_or_default() {
                    for ts in ["completed", "started", "created"] {
                        let at = &m[ts];
                        if at
                            .as_str()
                            .map(|a| last_activity.as_str().map(|cur| a > cur).unwrap_or(true))
                            == Some(true)
                        {
                            last_activity = at.clone();
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
            let Some((issue, task_state)) = task_map.get(tid) else {
                continue;
            };
            if !on.contains(issue) {
                on.push(issue.clone());
            }
            let message = running_msgs
                .iter()
                .find(|m| m["task"].as_str() == Some(tid))
                .cloned()
                .unwrap_or(Value::Null);
            bound.push(json!({
                "task": tid, "task_state": task_state, "issue": issue,
                "message": message,
            }));
            by_issue.entry(issue.clone()).or_default().push(json!({
                "alias": alias,
                "task": tid,
                "task_state": task_state,
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
            "event_cursor": cursor,
        }));
    }
    totals["inboxes"] = json!(inboxes);
    json!({"daemon": "reachable", "agents": out, "totals": totals, "by_issue": by_issue})
}

/// `/api/agents/<alias>` — the drawer detail: the daemon's own
/// `agent_show` plus the last 20 events and the recovery/resume
/// commands the row chips hinted at.
fn agent_detail(state_dir: &Path, alias: &str) -> std::result::Result<Value, String> {
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
        if let Some((issue, _)) = t.as_str().and_then(|tid| task_map.get(tid)) {
            if !issues.contains(&issue.as_str()) {
                issues.push(issue);
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

// ---------- write path (I2) ----------

/// JSON write bodies are small — fields, links, a comment, a body
/// replace. Artifact bytes go through the octet-stream route, capped at
/// `artifact_max_bytes` while reading.
const JSON_CAP: u64 = 256 * 1024;

/// The actor the write API commits as — visible in `git log` subjects.
const UI_ACTOR: &str = "operator (ui)";

type HttpResp = Response<std::io::Cursor<Vec<u8>>>;

fn header_value(request: &Request, name: &'static str) -> Option<String> {
    request
        .headers()
        .iter()
        .find(|h| h.field.equiv(name))
        .map(|h| h.value.as_str().to_string())
}

fn guard_fail(check: &str, msg: &str) -> HttpResp {
    let body =
        serde_json::to_vec_pretty(&json!({"error": msg, "check": check})).unwrap_or_default();
    let mut resp = Response::from_data(body).with_status_code(StatusCode(403));
    resp.add_header(Header::from_bytes("Content-Type", "application/json").unwrap());
    resp
}

/// Allowed write origins: the allowlisted hosts over http. A same-origin
/// browser page sends `Origin: http://<host>` — anything else, or a
/// cross-site `Sec-Fetch-Site`, is not our board.
fn origin_allowed(origin: &str, port: u16, hosts: &[String]) -> bool {
    let origin = origin.trim().to_ascii_lowercase();
    let mut allowed = vec![
        "http://cadence.localhost".to_string(),
        "http://cadence.localhost:18000".to_string(),
        format!("http://127.0.0.1:{port}"),
        format!("http://localhost:{port}"),
        format!("http://[::1]:{port}"),
    ];
    allowed.extend(
        hosts
            .iter()
            .map(|h| format!("http://{}", h.trim().to_ascii_lowercase())),
    );
    allowed.contains(&origin)
}

/// The three header guards every write request must pass, checked
/// before any work: exact content type (never a "simple" form type), the
/// custom `X-Cadence-Board: 1` marker, and same-origin Origin /
/// Sec-Fetch-Site when the browser sends them. A cross-site page cannot
/// satisfy any of the three without a preflight this server never
/// answers (OPTIONS is 405; no `Access-Control-*` header is ever sent).
fn write_guard(
    request: &Request,
    want_ct: &str,
    port: u16,
    hosts: &[String],
) -> std::result::Result<(), HttpResp> {
    let ct = header_value(request, "Content-Type").unwrap_or_default();
    if ct.trim() != want_ct {
        return Err(guard_fail(
            "content_type",
            &format!("content-type must be exactly '{want_ct}'"),
        ));
    }
    if header_value(request, "X-Cadence-Board").as_deref() != Some("1") {
        return Err(guard_fail("x_cadence_board", "missing X-Cadence-Board: 1"));
    }
    if let Some(origin) = header_value(request, "Origin") {
        if !origin_allowed(&origin, port, hosts) {
            return Err(guard_fail(
                "origin",
                &format!("origin '{origin}' is not a board origin"),
            ));
        }
    }
    if let Some(sfs) = header_value(request, "Sec-Fetch-Site") {
        if !sfs.eq_ignore_ascii_case("same-origin") {
            return Err(guard_fail(
                "sec_fetch_site",
                &format!("sec-fetch-site '{sfs}' must be 'same-origin'"),
            ));
        }
    }
    Ok(())
}

/// Read a request body, stopping at `cap + 1` — an oversized upload is
/// refused without ever buffering it whole.
fn read_body(request: &mut Request, cap: u64) -> std::result::Result<Vec<u8>, HttpResp> {
    let mut buf = Vec::new();
    let mut limited = request.as_reader().take(cap + 1);
    if let Err(e) = limited.read_to_end(&mut buf) {
        return Err(err_response(400, &format!("body read failed: {e}")));
    }
    if buf.len() as u64 > cap {
        return Err(err_response(
            413,
            &format!("body is over the {cap}-byte cap"),
        ));
    }
    Ok(buf)
}

fn parse_json<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> std::result::Result<T, HttpResp> {
    serde_json::from_slice(bytes).map_err(|e| err_response(400, &format!("bad request json: {e}")))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewIssueReq {
    project: String,
    title: String,
    priority: Option<String>,
    owner: Option<String>,
    component: Option<String>,
    parent: Option<String>,
    blocked_by: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PatchReq {
    status: Option<String>,
    priority: Option<String>,
    /// `""` clears owner.
    owner: Option<String>,
    /// `""` clears component.
    component: Option<String>,
    title: Option<String>,
    body: Option<String>,
    if_rev: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LinkReq {
    #[serde(rename = "type")]
    kind: String,
    target: String,
    if_rev: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RefReq {
    kind: String,
    url: Option<String>,
    path: Option<String>,
    label: Option<String>,
    if_rev: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CommentReq {
    body: String,
    if_rev: Option<String>,
}

/// A write op's outcome → HTTP response. Conflicts are 409 with the
/// reason; success re-reads the issue and returns the fresh card and
/// detail payloads so the UI needs no second fetch.
fn write_reply(pm: &Pm, state_dir: &Path, id: &str, out: Value, created: bool) -> HttpResp {
    if out.get("conflict").is_some() {
        let mut body = out.clone();
        let msg = match out["conflict"].as_str() {
            Some("if_rev") => "if_rev does not match issue.md — re-read and retry".to_string(),
            Some("status_derived") => out["reason"]
                .as_str()
                .unwrap_or("status is derived")
                .to_string(),
            Some("exists") => format!(
                "artifact '{}' already exists",
                out["artifact"].as_str().unwrap_or_default()
            ),
            _ => "conflict".to_string(),
        };
        body["error"] = json!(msg);
        // The fresh card lets the caller resync on the spot.
        if let Ok((card, _)) = issue_payloads(pm, state_dir, id) {
            body["card"] = card;
        }
        let bytes = serde_json::to_vec_pretty(&body).unwrap_or_default();
        let mut resp = Response::from_data(bytes).with_status_code(StatusCode(409));
        resp.add_header(Header::from_bytes("Content-Type", "application/json").unwrap());
        return resp;
    }
    match issue_payloads(pm, state_dir, id) {
        Ok((card, detail)) => {
            let warnings = out.get("warnings").cloned().unwrap_or(json!([]));
            let body = serde_json::to_vec_pretty(&json!({
                "issue": detail, "card": card, "warnings": warnings,
            }))
            .unwrap_or_default();
            let mut resp = Response::from_data(body).with_status_code(StatusCode(if created {
                201
            } else {
                200
            }));
            resp.add_header(Header::from_bytes("Content-Type", "application/json").unwrap());
            resp
        }
        Err(e) => err_response(500, &format!("write committed but reload failed: {e}")),
    }
}

/// Fresh card + detail payloads for one id after a write.
fn issue_payloads(pm: &Pm, state_dir: &Path, id: &str) -> Result<(Value, Value)> {
    let issues = board::load_all(&pm.dir, None)?;
    let jobs = board::fetch_job_outcomes(state_dir);
    let views = board::views_with_jobs(&pm.config.notes_dir(), issues, &jobs);
    let by_id: HashMap<String, &board::View> = views
        .iter()
        .map(|v| (v.issue.front.id.clone(), v))
        .collect();
    let view = by_id
        .get(id)
        .ok_or_else(|| Error::rejected(format!("unknown issue '{id}'")))?;
    let by_issue = agents_payload(state_dir)["by_issue"].clone();
    Ok((
        with_agents(board::card_json(view), &by_issue, id),
        with_agents(board::detail_json(&pm.dir, view, &by_id), &by_issue, id),
    ))
}

/// Map a writer error to an HTTP status: unknown ids are 404, rejections
/// are 400, internals are 500.
fn write_err(e: &Error) -> HttpResp {
    match e {
        Error::Rejected(m) if m.starts_with("Unknown issue") => err_response(404, m),
        Error::Rejected(m) => err_response(400, m),
        other => err_response(500, &other.to_string()),
    }
}

/// Dispatch POST/PATCH/DELETE on the write routes. Every route passes
/// `write_guard` before reading a body or touching the PM dir, and every
/// op goes through `issue::write` — one write path for CLI and API.
#[allow(clippy::too_many_arguments)]
fn write_route(
    mut request: Request,
    method: &Method,
    path: &str,
    query: &dyn Fn(&str) -> Option<String>,
    state_dir: &Path,
    pm_dir: &Path,
    port: u16,
    hosts: &[String],
    send: &dyn Fn(Request, HttpResp),
) {
    let Some(rest) = path.strip_prefix("/api/issues") else {
        send(request, err_response(404, "no such write route"));
        return;
    };
    let (id, sub) = if rest.is_empty() {
        (None, None)
    } else if let Some(tail) = rest.strip_prefix('/') {
        let mut segs = tail.splitn(2, '/');
        (Some(segs.next().unwrap_or_default()), segs.next())
    } else {
        send(request, err_response(404, "no such write route"));
        return;
    };
    // Route shape → expected method. A known shape with the wrong
    // method is 405; an unknown shape is 404.
    let known_sub = matches!(sub, Some("links" | "refs" | "comments" | "artifacts"));
    let shape_ok = matches!(
        (id.is_some(), sub, method),
        (false, None, &Method::Post)
            | (true, None, &Method::Patch)
            | (true, Some("links"), &Method::Post | &Method::Delete)
            | (true, Some("refs" | "comments" | "artifacts"), &Method::Post)
    );
    if !shape_ok {
        let code = if id.is_none() && sub.is_none() || known_sub || (id.is_some() && sub.is_none())
        {
            405
        } else {
            404
        };
        send(request, err_response(code, "no such write route"));
        return;
    }
    let want_ct = if sub == Some("artifacts") {
        "application/octet-stream"
    } else {
        "application/json"
    };
    if let Err(resp) = write_guard(&request, want_ct, port, hosts) {
        send(request, resp);
        return;
    }
    let pm = match Pm::at(pm_dir) {
        Ok(pm) => pm,
        Err(e) => {
            send(request, err_response(503, &e.to_string()));
            return;
        }
    };

    if id.is_none() {
        // POST /api/issues — create.
        let bytes = match read_body(&mut request, JSON_CAP) {
            Ok(b) => b,
            Err(resp) => {
                send(request, resp);
                return;
            }
        };
        let req: NewIssueReq = match parse_json(&bytes) {
            Ok(r) => r,
            Err(resp) => {
                send(request, resp);
                return;
            }
        };
        let blocked_by = req.blocked_by.unwrap_or_default();
        match issue_write::new_issue(
            &pm,
            &pm.dir,
            Some(&req.project),
            &req.title,
            req.priority.as_deref(),
            req.parent.as_deref(),
            &blocked_by,
            req.owner.as_deref(),
            req.component.as_deref(),
            None,
            UI_ACTOR,
        ) {
            Ok(out) => {
                let new_id = out["id"].as_str().unwrap_or_default().to_string();
                send(request, write_reply(&pm, state_dir, &new_id, out, true));
            }
            Err(e) => send(request, write_err(&e)),
        }
        return;
    }

    let id_raw = id.unwrap_or_default();
    let Ok(id) = model::check_id(id_raw) else {
        send(request, err_response(400, "bad issue id"));
        return;
    };
    if sub == Some("artifacts") {
        // POST /api/issues/:id/artifacts?name=<basename> — raw bytes.
        let name = query("name").unwrap_or_default();
        if !model::valid_artifact_name(&name) {
            send(
                request,
                err_response(
                    400,
                    "bad artifact name — [A-Za-z0-9._-]{1,120}, no leading dot",
                ),
            );
            return;
        }
        let cap = pm.config.artifact_max_bytes;
        let bytes = match read_body(&mut request, cap) {
            Ok(b) => b,
            Err(resp) => {
                send(request, resp);
                return;
            }
        };
        match issue_write::attach_bytes(&pm, &id, &name, &bytes, false, UI_ACTOR) {
            Ok(out) => send(request, write_reply(&pm, state_dir, &id, out, false)),
            Err(e) => send(request, write_err(&e)),
        }
        return;
    }

    let bytes = match read_body(&mut request, JSON_CAP) {
        Ok(b) => b,
        Err(resp) => {
            send(request, resp);
            return;
        }
    };
    let out = match (sub, method) {
        (None, &Method::Patch) => match parse_json::<PatchReq>(&bytes) {
            Ok(req) => issue_write::patch_issue(
                &pm,
                &id,
                &issue_write::IssuePatch {
                    status: req.status,
                    priority: req.priority,
                    owner: req.owner,
                    component: req.component,
                    title: req.title,
                    body: req.body,
                },
                req.if_rev.as_deref(),
                UI_ACTOR,
                Some(state_dir),
            ),
            Err(resp) => {
                send(request, resp);
                return;
            }
        },
        (Some("links"), m) => match parse_json::<LinkReq>(&bytes) {
            Ok(req) => issue_write::link(
                &pm,
                &id,
                &req.kind,
                &req.target,
                m == &Method::Delete,
                req.if_rev.as_deref(),
                UI_ACTOR,
                Some(state_dir),
            ),
            Err(resp) => {
                send(request, resp);
                return;
            }
        },
        (Some("refs"), _) => match parse_json::<RefReq>(&bytes) {
            Ok(req) => {
                let target = match (req.url, req.path) {
                    (Some(u), None) | (None, Some(u)) => u,
                    _ => {
                        send(
                            request,
                            err_response(400, "send exactly one of url or path"),
                        );
                        return;
                    }
                };
                issue_write::add_ref(
                    &pm,
                    &id,
                    &req.kind,
                    &target,
                    req.label.as_deref(),
                    req.if_rev.as_deref(),
                    UI_ACTOR,
                )
            }
            Err(resp) => {
                send(request, resp);
                return;
            }
        },
        (Some("comments"), _) => match parse_json::<CommentReq>(&bytes) {
            Ok(req) => issue_write::add_comment(
                &pm,
                &id,
                &req.body,
                Some("operator"),
                Some("ui"),
                req.if_rev.as_deref(),
                UI_ACTOR,
            ),
            Err(resp) => {
                send(request, resp);
                return;
            }
        },
        _ => unreachable!("shape_ok gated"),
    };
    match out {
        Ok(out) => send(request, write_reply(&pm, state_dir, &id, out, false)),
        Err(e) => send(request, write_err(&e)),
    }
}

/// `GET /api/issues/:id/artifacts/:name` — the constrained read. The
/// name must satisfy the write grammar, resolve to a real regular file
/// inside that issue's `artifacts/` (symlinks refused), and is served
/// inline only for a small allowlist; everything else — and always html,
/// svg, xml, js, pdf — downloads as an octet-stream attachment so a
/// rendered report can never drive the write API.
fn artifact_response(view: &board::View, name: &str) -> HttpResp {
    if !model::valid_artifact_name(name) {
        return err_response(400, "bad artifact name");
    }
    let dir = view.issue.dir.join("artifacts");
    let path = dir.join(name);
    if !board::is_real_dir(&dir) || !board::is_real_file(&path) {
        return err_response(404, "no such artifact");
    }
    let Ok(bytes) = std::fs::read(&path) else {
        return err_response(500, "artifact read failed");
    };
    let ext = name
        .rsplit('.')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let (mime, inline) = match ext.as_str() {
        "txt" | "md" | "log" | "json" | "jsonl" | "yaml" | "yml" | "toml" | "rs" | "ts" | "tsx"
        | "css" | "diff" | "patch" => ("text/plain; charset=utf-8", true),
        "png" => ("image/png", true),
        "jpg" | "jpeg" => ("image/jpeg", true),
        "gif" => ("image/gif", true),
        "webp" => ("image/webp", true),
        _ => ("application/octet-stream", false),
    };
    let mut resp = Response::from_data(bytes);
    resp.add_header(Header::from_bytes("Content-Type", mime).unwrap());
    resp.add_header(
        Header::from_bytes("Content-Security-Policy", "sandbox; default-src 'none'").unwrap(),
    );
    resp.add_header(Header::from_bytes("Cache-Control", "no-store").unwrap());
    if !inline {
        resp.add_header(
            Header::from_bytes(
                "Content-Disposition",
                format!("attachment; filename=\"{name}\""),
            )
            .unwrap(),
        );
    }
    resp
}

fn send(request: Request, mut resp: HttpResp, head_only: bool) {
    add_security_headers(&mut resp);
    if head_only {
        // tiny_http does not strip bodies on HEAD — answer with the
        // same headers as GET, minus the body.
        let mut bare = Response::empty(resp.status_code());
        for h in resp.headers() {
            bare.add_header(h.clone());
        }
        let _ = request.respond(bare);
    } else {
        let _ = request.respond(resp);
    }
}

/// Merge the `by_issue` runtime strip into a card/detail payload —
/// cards get `agents: [{alias, task, task_state, state, message,
/// resume}]` only when a job actually binds agents to the issue.
fn with_agents(mut payload: Value, by_issue: &Value, id: &str) -> Value {
    if let Some(agents) = by_issue.get(id) {
        payload["agents"] = agents.clone();
    }
    payload
}

// ---------- /api/stream — server-sent events ----------

/// Newest mtime among regular files under `dir` — the tracker-change
/// fingerprint. Small tree; a full walk every poll is still cheap.
fn dir_mtime(dir: &Path) -> Option<std::time::SystemTime> {
    let mut newest = None;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_dir() {
                stack.push(path);
            } else if meta.is_file() {
                let m = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
                if newest.is_none_or(|n| m > n) {
                    newest = Some(m);
                }
            }
        }
    }
    newest
}

/// A cheap content fingerprint for a JSON value — the serialized bytes
/// through a stable hasher (no new dependency for one hash).
fn value_fp(value: &Value) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    serde_json::to_vec(value).unwrap_or_default().hash(&mut h);
    h.finish()
}

/// The per-agent fingerprint input: the agent_list row plus the queue
/// counters and event cursor that only `agent_show` exposes — any
/// message, fence, or liveness change moves it.
fn agents_fp(state_dir: &Path, list: &Value) -> u64 {
    let mut parts = vec![list.clone()];
    for agent in list["agents"].as_array().cloned().unwrap_or_default() {
        if let Some(alias) = agent["alias"].as_str() {
            if let Ok(show) = client::rpc(state_dir, "agent_show", json!({"alias": alias})) {
                parts.push(json!({
                    "queued": show["queued"], "unknown": show["unknown"],
                    "cursor": show["event_cursor"],
                }));
            }
        }
    }
    value_fp(&json!(parts))
}

/// Poll the three board inputs once a second and push
/// `issues|agents|jobs` event names into `tx` on change. The baseline
/// is taken before the loop so a fresh client only sees deltas.
/// `__tick` is the per-second liveness probe: the reader drops it, and
/// a failed send means the client hung up — stop polling the daemon.
fn watch_changes(
    state_dir: PathBuf,
    pm_dir: PathBuf,
    mut tracker: Option<std::time::SystemTime>,
    mut jobs: u64,
    mut agents: u64,
    tx: std::sync::mpsc::Sender<&'static str>,
) {
    loop {
        std::thread::sleep(Duration::from_secs(1));
        if tx.send("__tick").is_err() {
            return;
        }
        let t = dir_mtime(&pm_dir);
        if t != tracker {
            tracker = t;
            if tx.send("issues").is_err() {
                return;
            }
        }
        if let Ok(list) = client::rpc(&state_dir, "job_list", json!({"all": true})) {
            let fp = value_fp(&list);
            if fp != jobs {
                jobs = fp;
                if tx.send("jobs").is_err() {
                    return;
                }
            }
        }
        if let Ok(list) = client::rpc(&state_dir, "agent_list", json!({})) {
            let fp = agents_fp(&state_dir, &list);
            if fp != agents {
                agents = fp;
                if tx.send("agents").is_err() {
                    return;
                }
            }
        }
    }
}

/// `GET /api/stream` — server-sent events written straight onto the
/// socket. tiny_http's chunked path buffers small writes inside
/// `chunked_transfer::Encoder` (it flushes only on `flush()` or a full
/// chunk), so a reader-based `Response` would never emit a small SSE
/// frame. `into_writer` hands over the socket: the head is written by
/// hand, each frame flushes immediately, and dropping the writer on
/// exit closes the stream — which is also how a dead client surfaces.
fn stream_events(request: Request, state_dir: &Path, pm_dir: &Path) {
    let mut w = request.into_writer();
    // Baselines at connect time, before the client can observe the
    // stream is live — if they were taken inside the spawned thread
    // they would race the client's first action and silently absorb it.
    let tracker0 = dir_mtime(pm_dir);
    let jobs0 = client::rpc(state_dir, "job_list", json!({"all": true}))
        .map(|l| value_fp(&l))
        .unwrap_or(0);
    let agents0 = client::rpc(state_dir, "agent_list", json!({}))
        .map(|l| agents_fp(state_dir, &l))
        .unwrap_or(0);
    let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                Cache-Control: no-store\r\nConnection: close\r\n\r\n";
    if w.write_all(head.as_bytes())
        .and_then(|_| w.flush())
        .is_err()
    {
        return;
    }
    let (tx, rx) = std::sync::mpsc::channel::<&'static str>();
    std::thread::spawn({
        let (state_dir, pm_dir) = (state_dir.to_path_buf(), pm_dir.to_path_buf());
        move || watch_changes(state_dir, pm_dir, tracker0, jobs0, agents0, tx)
    });
    let frame = |w: &mut dyn Write, bytes: &[u8]| -> bool {
        w.write_all(bytes).and_then(|_| w.flush()).is_ok()
    };
    // First frame immediately — proves the stream is live and gives
    // proxies something to flush before the first event exists.
    if !frame(&mut w, b": ping\n\n") {
        return;
    }
    // `: ping` every 15 s of wire silence — the per-second `__tick`
    // would otherwise starve the keepalive, and an idle dead client
    // would never surface without a write.
    let mut ping_at = Instant::now() + Duration::from_secs(15);
    loop {
        match rx.recv_timeout(Duration::from_secs(15)) {
            Ok("__tick") => {}
            Ok(name) => {
                if !frame(&mut w, format!("event: {name}\ndata: {{}}\n\n").as_bytes()) {
                    return;
                }
                ping_at = Instant::now() + Duration::from_secs(15);
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
        }
        if Instant::now() >= ping_at {
            if !frame(&mut w, b": ping\n\n") {
                return;
            }
            ping_at = Instant::now() + Duration::from_secs(15);
        }
    }
}

fn handle(
    request: Request,
    state_dir: &Path,
    pm_dir: &Path,
    port: u16,
    dist: Option<&Path>,
    hosts: &[String],
) {
    let method = request.method().clone();
    let head_only = method == Method::Head;
    let is_write = matches!(method, Method::Post | Method::Patch | Method::Delete);
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
    if !host_allowed(&host, port, hosts) {
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

    let send = |req: Request, resp: HttpResp| send(req, resp, head_only);

    if is_write {
        write_route(
            request, &method, &path, &query, state_dir, pm_dir, port, hosts, &send,
        );
        return;
    }

    match path.as_str() {
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
                })),
            );
        }
        "/api/projects" => match Pm::at(pm_dir) {
            Ok(pm) => {
                let projects = project::list(&pm.dir).unwrap_or_default();
                let issues = board::load_all(&pm.dir, None).unwrap_or_default();
                let payload: Vec<Value> = projects
                    .iter()
                    .map(|p| {
                        json!({
                            "key": p.key, "prefix": p.prefix,
                            "components": p.components,
                            "default_owner": p.default_owner,
                            "repos": p.repos.iter().map(|r| json!({
                                "path": r.path, "remote": r.remote})).collect::<Vec<_>>(),
                            "issues": issues.iter().filter(|i| i.project == p.key).count(),
                        })
                    })
                    .collect();
                send(request, json_response(json!({"projects": payload})));
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
                let issues = board::load_all(&pm.dir, filter.as_deref()).unwrap_or_default();
                let jobs = board::fetch_job_outcomes(state_dir);
                let views = board::views_with_jobs(&pm.config.notes_dir(), issues, &jobs);
                let by_issue = agents_payload(state_dir)["by_issue"].clone();
                send(
                    request,
                    json_response(json!({
                        "issues": views
                            .iter()
                            .map(|v| with_agents(board::card_json(v), &by_issue, &v.issue.front.id))
                            .collect::<Vec<_>>(),
                    })),
                );
            }
            Err(e) => send(request, err_response(503, &e.to_string())),
        },
        "/api/agents" => send(request, json_response(agents_payload(state_dir))),
        "/api/stream" => {
            if head_only {
                send(request, err_response(405, "stream is GET only"));
            } else {
                stream_events(request, state_dir, pm_dir);
            }
        }
        _ => {
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
            // `/api/issues/<ID>[/file|/activity|/artifacts/<name>]` — id
            // grammar checked before the id is ever a path component.
            if let Some(tail) = path.strip_prefix("/api/issues/") {
                let mut segs = tail.splitn(2, '/');
                let id_raw = segs.next().unwrap_or_default();
                let sub = segs.next();
                if sub.is_some_and(|s| {
                    !matches!(s, "file" | "activity") && !s.starts_with("artifacts/")
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
                        let issues = board::load_all(&pm.dir, None).unwrap_or_default();
                        let jobs = board::fetch_job_outcomes(state_dir);
                        let views = board::views_with_jobs(&pm.config.notes_dir(), issues, &jobs);
                        let by_id: std::collections::HashMap<String, &board::View> = views
                            .iter()
                            .map(|v| (v.issue.front.id.clone(), v))
                            .collect();
                        let Some(view) = by_id.get(&id) else {
                            send(request, err_response(404, "unknown issue"));
                            return;
                        };
                        match sub {
                            None => {
                                let by_issue = agents_payload(state_dir)["by_issue"].clone();
                                send(
                                    request,
                                    json_response(with_agents(
                                        board::detail_json(&pm.dir, view, &by_id),
                                        &by_issue,
                                        &id,
                                    )),
                                )
                            }
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
            // Static: `/` → index.html; otherwise a file under dist or
            // the embedded build. No SPA routes exist in I1.
            let target = if path == "/" { "/index.html" } else { &path };
            match static_file(dist, target) {
                Some((name, bytes)) => {
                    let mut resp = Response::from_data(bytes);
                    resp.add_header(
                        Header::from_bytes("Content-Type", content_type(&name)).unwrap(),
                    );
                    resp.add_header(Header::from_bytes("Cache-Control", "no-store").unwrap());
                    send(request, resp);
                }
                None => {
                    if let Some((name, bytes)) = static_file(dist, "/index.html") {
                        let mut resp = Response::from_data(bytes);
                        resp.add_header(
                            Header::from_bytes("Content-Type", content_type(&name)).unwrap(),
                        );
                        send(request, resp);
                    } else {
                        send(
                            request,
                            err_response(
                                503,
                                "no SPA build — pass --dist or rebuild with --features ui",
                            ),
                        );
                    }
                }
            }
        }
    }
}

/// One mutex for every write route — the server is thread-per-request
/// since `/api/stream`, and issue file writes must not interleave.
static WRITE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub fn serve(
    state_dir: &Path,
    pm_dir: &Path,
    host: &str,
    port: u16,
    dist: Option<PathBuf>,
    allow_hosts: &[String],
) -> Result<()> {
    let server = Server::http(format!("{host}:{port}"))
        .map_err(|e| Error::internal(format!("ui bind {host}:{port}: {e}")))?;
    eprintln!("cadence ui listening on http://{host}:{port}");
    for request in server.incoming_requests() {
        // Thread per request: `/api/stream` holds its connection open
        // for the session's lifetime and must not starve the board.
        let (state_dir, pm_dir, dist) =
            (state_dir.to_path_buf(), pm_dir.to_path_buf(), dist.clone());
        let hosts = allow_hosts.to_vec();
        std::thread::spawn(move || {
            let is_write = matches!(
                request.method(),
                Method::Post | Method::Patch | Method::Delete
            );
            if is_write {
                let _guard = WRITE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
                handle(request, &state_dir, &pm_dir, port, dist.as_deref(), &hosts);
            } else {
                handle(request, &state_dir, &pm_dir, port, dist.as_deref(), &hosts);
            }
        });
    }
    Ok(())
}

// ---------- lifecycle (mirrors `daemon start|stop|status`) ----------

fn pid_file(state_dir: &Path) -> PathBuf {
    state_dir.join("ui.pid")
}

fn read_pid(state_dir: &Path) -> Option<i32> {
    std::fs::read_to_string(pid_file(state_dir))
        .ok()?
        .trim()
        .parse()
        .ok()
        .filter(|pid| {
            // Alive check — a stale pidfile is cleaned, not trusted.
            unsafe { libc::kill(*pid, 0) == 0 }
        })
}

/// Tiny blocking GET — enough for health checks without an HTTP client
/// dependency. Returns `(status, body)`.
fn http_get(host: &str, port: u16, path: &str) -> Result<(u16, String)> {
    let mut stream = TcpStream::connect((host, port))
        .map_err(|e| Error::internal(format!("ui not reachable at {host}:{port}: {e}")))?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    write!(stream, "GET {path} HTTP/1.0\r\nHost: {host}:{port}\r\n\r\n")?;
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf)?;
    let text = String::from_utf8_lossy(&buf);
    let status = text
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(0);
    let body = text
        .split("\r\n\r\n")
        .nth(1)
        .unwrap_or_default()
        .to_string();
    Ok((status, body))
}

fn start(
    state_dir: &Path,
    host: &str,
    port: u16,
    dist: Option<PathBuf>,
    allow_hosts: &[String],
) -> Result<i32> {
    std::fs::create_dir_all(state_dir)?;
    if let Some(pid) = read_pid(state_dir) {
        let (code, _) = http_get(host, port, "/api/health").unwrap_or((0, String::new()));
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "state": "already_running", "pid": pid, "health_http": code,
            }))
            .unwrap_or_default()
        );
        return Ok(0);
    }
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(state_dir.join("ui.log"))?;
    let exe = std::env::current_exe()?;
    let mut command = Command::new(exe);
    command
        .arg("--state-dir")
        .arg(state_dir)
        .args(["ui", "run", "--host", host, "--port"])
        .arg(port.to_string());
    if let Some(dist) = &dist {
        command.arg("--dist").arg(dist);
    }
    for h in allow_hosts {
        command.arg("--allow-host").arg(h);
    }
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(log.try_clone()?))
        .stderr(std::process::Stdio::from(log));
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn()?;
    std::fs::write(pid_file(state_dir), child.id().to_string())?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok((200, _)) = http_get(host, port, "/api/health") {
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "state": "started", "pid": child.id(),
                    "url": format!("http://{host}:{port}"),
                    "gateway": "http://cadence.localhost:18000",
                    "log": state_dir.join("ui.log"),
                }))
                .unwrap_or_default()
            );
            return Ok(0);
        }
        if child.try_wait()?.is_some() {
            let _ = std::fs::remove_file(pid_file(state_dir));
            return Err(Error::rejected(format!(
                "ui server exited during start — see {}",
                state_dir.join("ui.log").display()
            )));
        }
        if Instant::now() >= deadline {
            return Err(Error::internal("ui server did not answer within 10s"));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn stop(state_dir: &Path) -> Result<i32> {
    match read_pid(state_dir) {
        Some(pid) => {
            unsafe { libc::kill(pid, libc::SIGTERM) };
            let deadline = Instant::now() + Duration::from_secs(5);
            while unsafe { libc::kill(pid, 0) == 0 } {
                if Instant::now() >= deadline {
                    unsafe { libc::kill(pid, libc::SIGKILL) };
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            let _ = std::fs::remove_file(pid_file(state_dir));
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({"state": "stopped", "pid": pid}))
                    .unwrap_or_default()
            );
        }
        None => {
            let _ = std::fs::remove_file(pid_file(state_dir));
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({"state": "stopped", "note": "no live pid"}))
                    .unwrap_or_default()
            );
        }
    }
    Ok(0)
}

fn status(state_dir: &Path) -> Result<i32> {
    let pid = read_pid(state_dir);
    let health = pid.and_then(|_| {
        // The pidfile records the pid, not the port — try the default.
        http_get("127.0.0.1", 3010, "/api/health").ok()
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "state": if pid.is_some() { "running" } else { "stopped" },
            "pid": pid,
            "health": health.map(|(code, body)| json!({
                "http": code,
                "body": serde_json::from_str::<Value>(&body).unwrap_or(Value::Null),
            })),
        }))
        .unwrap_or_default()
    );
    Ok(0)
}
