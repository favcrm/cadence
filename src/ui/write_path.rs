//! The I2 write path: the four cross-site guards (`write_guard`), the
//! write-route dispatcher (`write_route`) and every mutating handler it
//! fans out to, plus the shared response plumbing (`send`, `coded_response`,
//! `write_reply`) the read side reuses.

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

use serde::Deserialize;
use serde_json::{json, Value};
use tiny_http::{Header, Method, Request, Response, StatusCode};

use super::serve::{add_security_headers, err_response, json_response};
use super::ServeOpts;
use super::{
    app_audiences, app_content, app_contexts, app_records, app_release, app_runs, apps,
    connections, crm_send, crm_smtp, home, lane, operator, read_model, social_publish, stages,
    threads, updates, wiki, workflows,
};
use crate::adapter::registry;
use crate::client;
use crate::error::{Error, Result};
use crate::issue::{board, model, report, write as issue_write, Pm};

// ---------- write path (I2) ----------

/// JSON write bodies are small — fields, links, a comment, a body
/// replace. Artifact bytes go through the octet-stream route, capped at
/// `artifact_max_bytes` while reading.
pub(crate) const JSON_CAP: u64 = 256 * 1024;

/// The actor an operator write commits as — visible in `git log`
/// subjects. A pane's write commits as its alias
/// ([`operator::board_caller`]).
pub(crate) const UI_ACTOR: &str = "operator (ui)";

pub(crate) type HttpResp = Response<std::io::Cursor<Vec<u8>>>;

pub(crate) fn header_value(request: &Request, name: &'static str) -> Option<String> {
    request
        .headers()
        .iter()
        .find(|h| h.field.equiv(name))
        .map(|h| h.value.as_str().to_string())
}

pub(crate) fn guard_fail(check: &str, msg: &str) -> HttpResp {
    let body =
        serde_json::to_vec_pretty(&json!({"error": msg, "check": check})).unwrap_or_default();
    let mut resp = Response::from_data(body).with_status_code(StatusCode(403));
    resp.add_header(Header::from_bytes("Content-Type", "application/json").unwrap());
    resp
}

/// Allowed write origins: the allowlisted hosts over http plus the
/// explicit `--allow-origin` entries (the tailnet https origin lands
/// there). A same-origin browser page sends `Origin: <scheme>://<host>`
/// — anything else, or a cross-site `Sec-Fetch-Site`, is not our board.
pub(crate) fn origin_allowed(
    origin: &str,
    port: u16,
    hosts: &[String],
    origins: &[String],
) -> bool {
    let origin = origin.trim().to_ascii_lowercase();
    let mut allowed = vec![
        "http://cadence.localhost".to_string(),
        "http://cadence.localhost:18000".to_string(),
        format!("http://{}", operator::board_host(port)),
        format!("http://127.0.0.1:{port}"),
        format!("http://localhost:{port}"),
        format!("http://[::1]:{port}"),
    ];
    allowed.extend(
        hosts
            .iter()
            .map(|h| format!("http://{}", h.trim().to_ascii_lowercase())),
    );
    allowed.extend(origins.iter().map(|o| o.trim().to_ascii_lowercase()));
    allowed.contains(&origin)
}

/// The three header guards every write request must pass, checked
/// before any work: exact content type (never a "simple" form type), the
/// custom `X-Cadence-Board: 1` marker, and same-origin Origin /
/// Sec-Fetch-Site when the browser sends them. A cross-site page cannot
/// satisfy any of the three without a preflight this server never
/// answers (OPTIONS is 405; no `Access-Control-*` header is ever sent).
pub(crate) fn write_guard(
    request: &Request,
    want_ct: &str,
    opts: &ServeOpts,
) -> std::result::Result<(), HttpResp> {
    let ct = header_value(request, "Content-Type").unwrap_or_default();
    // `multipart/form-data` is the one non-exact rule (CAD-580 wiki
    // upload): the boundary parameter must ride the type, so the check
    // is a bounded prefix — never a bare "simple" form type.
    let ct_ok = if want_ct == "multipart/form-data" {
        let v = ct.trim();
        v.starts_with("multipart/form-data; boundary=") && v.len() <= 200
    } else {
        ct.trim() == want_ct
    };
    if !ct_ok {
        return Err(guard_fail(
            "content_type",
            &format!("content-type must be exactly '{want_ct}'"),
        ));
    }
    if header_value(request, "X-Cadence-Board").as_deref() != Some("1") {
        return Err(guard_fail("x_cadence_board", "missing X-Cadence-Board: 1"));
    }
    if let Some(origin) = header_value(request, "Origin") {
        if !origin_allowed(&origin, opts.port, &opts.allow_hosts, &opts.allow_origins) {
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
pub(crate) fn read_body(request: &mut Request, cap: u64) -> std::result::Result<Vec<u8>, HttpResp> {
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

pub(crate) fn parse_json<T: for<'de> Deserialize<'de>>(
    bytes: &[u8],
) -> std::result::Result<T, HttpResp> {
    serde_json::from_slice(bytes).map_err(|e| err_response(400, &format!("bad request json: {e}")))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NewIssueReq {
    project: String,
    title: String,
    priority: Option<String>,
    owner: Option<String>,
    component: Option<String>,
    tags: Option<Vec<String>>,
    parent: Option<String>,
    blocked_by: Option<Vec<String>>,
    /// CAD-140: the board's report/idea composer files the description
    /// with the issue — one call instead of create-then-comment.
    /// `None`/blank keeps `new_issue`'s default body.
    pub(crate) body: Option<String>,
}

/// CAD-140: `POST /api/reports` — file a question, feedback, idea or
/// bug through the canonical intake path. `project` is the board the
/// operator files from: ideas land there, while questions, feedback
/// and bugs route to the `cadence` project like `cadence report` (the
/// server decides — the UI never routes). Unknown fields are refused
/// so a forged `by`/`actor` never reaches the write.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReportReq {
    kind: String,
    project: String,
    title: String,
    priority: Option<String>,
    body: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PatchReq {
    status: Option<String>,
    priority: Option<String>,
    /// `""` clears owner.
    owner: Option<String>,
    /// `""` clears component.
    component: Option<String>,
    title: Option<String>,
    body: Option<String>,
    /// Replaces the tag list; `[]` clears it.
    tags: Option<Vec<String>>,
    if_rev: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LinkReq {
    #[serde(rename = "type")]
    kind: String,
    target: String,
    if_rev: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RefReq {
    kind: String,
    url: Option<String>,
    path: Option<String>,
    label: Option<String>,
    if_rev: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CommentReq {
    body: String,
    if_rev: Option<String>,
}

/// A write op's outcome → HTTP response. Conflicts are 409 with the
/// reason; success re-reads the issue and returns the fresh card and
/// detail payloads so the UI needs no second fetch.
pub(crate) fn write_reply(
    pm: &Pm,
    state_dir: &Path,
    id: &str,
    out: Value,
    created: bool,
) -> HttpResp {
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
            let mut body = json!({"issue": detail, "card": card, "warnings": warnings});
            // CAD-447: an answer's delivery to the asker.
            if let Some(route) = out.get("route") {
                body["route"] = route.clone();
            }
            let body = serde_json::to_vec_pretty(&body).unwrap_or_default();
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

/// CAD-140: `POST /api/reports` — the board's report/idea composer.
/// This is `cadence report`'s write path (`report::file`), not a bare
/// issue create: bodies pass the secret scan and prose scrub, the
/// context block is captured, intake kind/tags take their canonical
/// shape, and the PM inbox gets its heads-up line. Routing is
/// `report::file`'s: an idea files into `project` (the viewed board),
/// every other kind files into the `cadence` project. Answers the
/// filed report's `{issue, card}` plus the filing record, 201.
pub(crate) fn post_report(
    request: &mut Request,
    state_dir: &Path,
    pm_dir: &Path,
    actor: &str,
) -> HttpResp {
    let bytes = match read_body(request, JSON_CAP) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let req: ReportReq = match parse_json(&bytes) {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    let kind = match req.kind.trim() {
        "question" => report::Kind::Question,
        "feedback" => report::Kind::Feedback,
        "idea" => report::Kind::Idea,
        "bug" => report::Kind::Bug,
        other => {
            return err_response(
                400,
                &format!("report kind is question, feedback, idea or bug — not '{other}'"),
            );
        }
    };
    let title = req.title.trim();
    if title.is_empty() {
        return err_response(400, "a report needs a title");
    }
    if title.chars().count() > 200 {
        return err_response(400, "report title exceeds 200 characters");
    }
    let pm = match Pm::at(pm_dir) {
        Ok(pm) => pm,
        Err(e) => return err_response(503, &e.to_string()),
    };
    // Ideas file into the viewed board; the project flag always wins
    // in `report::file`. Every other kind is about cadence itself and
    // files into the `cadence` project from wherever it is filed.
    let project_flag = match kind {
        report::Kind::Idea => Some(req.project.as_str()),
        _ => None,
    };
    let text = match req.body.as_deref().map(str::trim).filter(|b| !b.is_empty()) {
        Some(details) => format!("{title}\n\n{details}"),
        None => title.to_string(),
    };
    // No shell cwd answers for a browser file: the state dir stands
    // in, so the context block names the board service honestly and
    // no tracker checkout is claimed as the reporter's repo.
    let out = match report::file(
        &pm,
        kind,
        project_flag,
        None,
        req.priority.as_deref(),
        &text,
        actor,
        state_dir,
        state_dir,
    ) {
        Ok(out) => out,
        // The intake path refuses bad input (oversize bodies, the
        // secret scan) as rejections — 400 with the reason, never a
        // 500: nothing was written and the caller can fix the text.
        Err(e) if e.kind() == "rejected" => return err_response(400, &e.to_string()),
        Err(e) => return write_err(&e),
    };
    let id = out["id"].as_str().unwrap_or_default().to_string();
    match issue_payloads(&pm, state_dir, &id) {
        Ok((card, detail)) => {
            let body = json!({
                "issue": detail, "card": card,
                "warnings": out.get("secret_warnings").cloned().unwrap_or(json!([])),
                "report": out,
            });
            let bytes = serde_json::to_vec_pretty(&body).unwrap_or_default();
            let mut resp = Response::from_data(bytes).with_status_code(StatusCode(201));
            resp.add_header(Header::from_bytes("Content-Type", "application/json").unwrap());
            resp
        }
        Err(e) => err_response(500, &format!("report filed but reload failed: {e}")),
    }
}

/// Fresh card + detail payloads for one id after a write.
pub(crate) fn issue_payloads(pm: &Pm, state_dir: &Path, id: &str) -> Result<(Value, Value)> {
    let read = read_model::get(state_dir, &pm.dir).board(pm, None);
    let by_id: HashMap<String, &board::View> = read
        .views
        .iter()
        .map(|v| (v.issue.front.id.clone(), v))
        .collect();
    let view = by_id
        .get(id)
        .ok_or_else(|| Error::rejected(format!("unknown issue '{id}'")))?;
    let ctx = read.ctx(&by_id);
    Ok((
        read.card(&ctx, view),
        with_agents(
            crate::issue::work::detail_json(&pm.dir, &ctx, view),
            &read.by_issue,
            id,
        ),
    ))
}

/// Map a writer error to an HTTP status: unknown ids are 404, rejections
/// are 400, internals are 500.
pub(crate) fn write_err(e: &Error) -> HttpResp {
    match e {
        Error::Rejected(m) if m.starts_with("Unknown issue") => err_response(404, m),
        Error::Rejected(m) => err_response(400, m),
        other => err_response(500, &other.to_string()),
    }
}

/// The actor a request would write as, header-wise, and the tailnet
/// proof behind it — `/api/meta`'s `actor` and `tailnet_proof`. A
/// request [`tailnet_proxy`] proves came through `tailscale serve`
/// writes as its `Tailscale-User-Login` ([`proxied_actor`]) — or not at
/// all when it carries none; any other request as the plain operator,
/// and the header is never considered. `tailnet_proof` is `null` for a
/// request that is not tailnet-shaped, `{"proven": true, "login"}`, or
/// `{"proven": false, "check", "why"}` naming the check that refused.
pub(crate) fn request_identity(request: &Request, opts: &ServeOpts) -> (String, Value) {
    match tailnet_proxy(request, opts) {
        None => (UI_ACTOR.to_string(), Value::Null),
        Some(Ok(())) => {
            match proxied_actor(header_value(request, "Tailscale-User-Login").as_deref()) {
                Ok(actor) => (actor, json!({"proven": true, "login": true})),
                Err(_) => (
                    NO_TAILNET_LOGIN.to_string(),
                    json!({"proven": true, "login": false}),
                ),
            }
        }
        Some(Err(r)) => (
            UI_ACTOR.to_string(),
            json!({"proven": false, "check": r.check.as_str(), "why": r.why}),
        ),
    }
}

/// `/api/meta`'s actor for a proven proxy request without a login.
pub(crate) const NO_TAILNET_LOGIN: &str = "none (tailnet request without a login — writes refused)";

/// The actor of a request proven to come through the serve proxy:
/// `<login> (tailscale)` from its `Tailscale-User-Login`. A proven
/// request with no usable login — a Funnel client from the internet, a
/// tagged node — names nobody, so it is refused rather than written as
/// `operator (ui)`.
pub(crate) fn proxied_actor(login: Option<&str>) -> std::result::Result<String, String> {
    login
        .and_then(sanitize_actor)
        .map(|l| format!("{l} (tailscale)"))
        .ok_or_else(|| {
            "the tailscale proxy sent no usable Tailscale-User-Login — a Funnel or \
             tagged-node client names nobody to write as"
                .to_string()
        })
}

/// Is this request from the `tailscale serve` proxy (CAD-336)? `None`
/// — not tailnet-shaped at all (tailscale mode off, or the Host is not
/// the tailnet name). `Some(Ok)` — tailnet-shaped and proven by
/// [`crate::tailnet_proof::prove`]. `Some(Err)` — tailnet-shaped but
/// unproven: Host and loopback are caller-controlled, so the identity
/// headers are not trusted.
/// Does the request name the tailnet host — did it come (or claim to
/// come) through `tailscale serve`? Proof aside.
pub(crate) fn tailnet_host(request: &Request, opts: &ServeOpts) -> bool {
    let Some((dns, _)) = opts.tailnet.as_ref() else {
        return false;
    };
    let host = header_value(request, "Host").unwrap_or_default();
    let name = host.split(':').next().unwrap_or_default();
    name.eq_ignore_ascii_case(dns)
}

pub(crate) fn tailnet_proxy(
    request: &Request,
    opts: &ServeOpts,
) -> Option<std::result::Result<(), crate::tailnet_proof::Refusal>> {
    if !tailnet_host(request, opts) {
        return None;
    }
    Some(match request.remote_addr() {
        Some(peer) => crate::tailnet_proof::prove(
            opts.tailscaled_socket.as_deref(),
            &opts.tailnet_latch,
            opts.port,
            *peer,
        ),
        None => Err(crate::tailnet_proof::Refusal {
            check: crate::tailnet_proof::Check::ClientSocket,
            why: "the request has no peer address".to_string(),
        }),
    })
}

/// The live agents a write can be attributed to, read over the
/// daemon's `agent_list` RPC:
///
/// - registered panes, pane pid → alias — the same rows the daemon's
///   slot identity resolves against (`pty` endpoints with a pid and a
///   generation);
/// - managed endpoints (CAD-335), provider pid → alias — a claude or
///   codex `managed`/`managed-ws` endpoint with a pid: the provider
///   process the daemon launched, the same pid its build-slot
///   enrollment roots at. The daemon clears the pid when the endpoint
///   closes, stops or errors.
///
/// Each pid comes with the process start time the daemon recorded
/// with it (`pid_start`, CAD-385) and is classified against `/proc`
/// now ([`crate::peer::AgentPids`]): a reused pid attributes nothing,
/// and a row without one — an older daemon's list — refuses a write
/// tied to it.
///
/// No store file means no agent was ever registered here: provably no
/// agents. A store the daemon cannot answer for is an error.
pub(crate) fn agent_roots(
    state_dir: &Path,
) -> std::result::Result<crate::peer::AgentRoots, String> {
    if !state_dir.join("cadence.sqlite3").exists() {
        return Ok(crate::peer::AgentRoots::default());
    }
    let list = client::rpc(state_dir, "agent_list", json!({}))
        .map_err(|e| format!("the daemon cannot list registered agents ({e})"))?;
    let agents = list["agents"]
        .as_array()
        .ok_or_else(|| "the daemon's agent list is malformed".to_string())?;
    let mut panes = Vec::new();
    let mut managed = Vec::new();
    for a in agents {
        let (Some(pid), Some(alias)) = (
            a["pid"].as_u64().and_then(|p| u32::try_from(p).ok()),
            a["alias"].as_str(),
        ) else {
            continue;
        };
        let row = (alias.to_string(), pid, a["pid_start"].as_u64());
        let kind = a["endpoint_kind"].as_str().unwrap_or_default();
        if kind == "pty" && !a["generation"].is_null() {
            panes.push(row);
        } else if pid > 1
            && registry::enrolls_build_slots(a["provider"].as_str().unwrap_or_default(), kind)
        {
            managed.push(row);
        }
    }
    Ok(crate::peer::AgentRoots {
        panes: crate::peer::AgentPids::classify(panes),
        managed: crate::peer::AgentPids::classify(managed),
    })
}

/// The login lands in a commit `Actor:` trailer — take the first
/// whitespace-free token of printable ASCII, bounded, else no trust.
pub(crate) fn sanitize_actor(raw: &str) -> Option<String> {
    let tok = raw.split_whitespace().next().unwrap_or_default();
    if tok.is_empty()
        || tok.len() > 120
        || !tok.chars().all(|c| c.is_ascii() && !c.is_ascii_control())
    {
        return None;
    }
    Some(tok.to_string())
}

pub(crate) fn coded_response(
    status: u16,
    code: &str,
    message: &str,
    revision: Option<i64>,
) -> HttpResp {
    let body = serde_json::to_vec_pretty(&json!({
        "error": message,
        "code": code,
        "revision": revision,
    }))
    .unwrap_or_default();
    let mut resp = Response::from_data(body).with_status_code(StatusCode(status));
    resp.add_header(Header::from_bytes("Content-Type", "application/json").unwrap());
    resp
}

pub(crate) fn health_supports_model_defaults(health: &Value) -> bool {
    health["capabilities"].as_array().is_some_and(|caps| {
        caps.iter()
            .any(|cap| cap.as_str() == Some(registry::MODEL_DEFAULTS_CAPABILITY))
    })
}

pub(crate) fn daemon_unavailable(err: &Error) -> bool {
    err.to_string().starts_with("Daemon is not reachable")
}

/// Health capability is the compatibility gate. An older daemon stays
/// reachable and answers `health` without `model_defaults`.
pub(crate) fn require_model_defaults(state_dir: &Path) -> std::result::Result<(), HttpResp> {
    match client::rpc(state_dir, "health", json!({})) {
        Err(err) => Err(coded_response(
            503,
            "daemon_unavailable",
            &err.to_string(),
            None,
        )),
        Ok(health) => {
            if health_supports_model_defaults(&health) {
                Ok(())
            } else {
                Err(coded_response(
                    501,
                    "unsupported_daemon",
                    "this daemon does not support model defaults",
                    None,
                ))
            }
        }
    }
}

pub(crate) fn settings_rpc_error(err: Error) -> HttpResp {
    if daemon_unavailable(&err) {
        return coded_response(503, "daemon_unavailable", &err.to_string(), None);
    }
    if err.kind() == "conflict" {
        return coded_response(409, "revision_conflict", &err.to_string(), err.revision());
    }
    if err.kind() == "internal" {
        return err_response(500, &err.to_string());
    }
    let code = err.code().unwrap_or("invalid_request");
    coded_response(400, code, &err.to_string(), err.revision())
}

pub(crate) fn model_defaults_get(state_dir: &Path, read_only: bool) -> HttpResp {
    if let Err(resp) = require_model_defaults(state_dir) {
        return resp;
    }
    match client::rpc(state_dir, "model_defaults_get", json!({})) {
        Ok(mut snapshot) => {
            if let Some(obj) = snapshot.as_object_mut() {
                obj.insert("read_only".to_string(), json!(read_only));
            }
            json_response(snapshot)
        }
        Err(err) => settings_rpc_error(err),
    }
}

pub(crate) fn read_settings_body(request: &mut Request) -> std::result::Result<Vec<u8>, HttpResp> {
    let cap = crate::model_defaults::MAX_HTTP_BODY_BYTES as u64;
    let mut buf = Vec::new();
    let mut limited = request.as_reader().take(cap + 1);
    if let Err(e) = limited.read_to_end(&mut buf) {
        return Err(coded_response(
            400,
            "invalid_request",
            &format!("body read failed: {e}"),
            None,
        ));
    }
    if buf.len() as u64 > cap {
        return Err(coded_response(
            400,
            "invalid_request",
            &format!("settings body exceeds {cap} bytes"),
            None,
        ));
    }
    Ok(buf)
}

/// `POST /api/settings/model-defaults` — operator-only, admitted by
/// `operator::admit` (session plus process proof) before this runs.
pub(crate) fn model_defaults_post(request: &mut Request, state_dir: &Path) -> HttpResp {
    if let Err(resp) = require_model_defaults(state_dir) {
        return resp;
    }
    let bytes = match read_settings_body(request) {
        Ok(bytes) => bytes,
        Err(resp) => return resp,
    };
    let document = match std::str::from_utf8(&bytes) {
        Ok(text) => text,
        Err(_) => {
            return coded_response(
                400,
                "invalid_request",
                "settings body must be UTF-8 JSON",
                None,
            )
        }
    };
    match client::rpc(
        state_dir,
        "model_defaults_set",
        json!({"document": document}),
    ) {
        Ok(mut snapshot) => {
            if let Some(obj) = snapshot.as_object_mut() {
                obj.insert("read_only".to_string(), json!(false));
            }
            json_response(snapshot)
        }
        Err(err) => settings_rpc_error(err),
    }
}

/// Dispatch POST/PATCH/DELETE on the write routes. Every route passes
/// `write_guard` before reading a body or touching the PM dir, and every
/// op goes through `issue::write` — one write path for CLI and API.
#[allow(clippy::too_many_arguments)]
pub(crate) fn write_route(
    mut request: Request,
    method: &Method,
    path: &str,
    query: &dyn Fn(&str) -> Option<String>,
    state_dir: &Path,
    pm_dir: &Path,
    opts: &ServeOpts,
    send: &dyn Fn(Request, HttpResp),
) {
    // CAD-313: signing in and out — the nonce, or the session itself,
    // is the credential (`operator`).
    if path == "/api/session" || path == "/api/session/logout" {
        if *method != Method::Post {
            send(request, err_response(405, "method not allowed"));
            return;
        }
        let resp = if path == "/api/session" {
            operator::open(&mut request, state_dir, opts)
        } else {
            operator::logout(&request, state_dir, opts)
        };
        send(request, resp);
        return;
    }
    // CAD-777: device-grant sign-in — same login class as `/api/session`.
    // Unconfigured boards answer 404 inside the handlers, like an
    // unknown shape.
    if path == "/api/session/device/code" || path == "/api/session/device/poll" {
        if *method != Method::Post {
            send(request, err_response(405, "method not allowed"));
            return;
        }
        let resp = if path == "/api/session/device/code" {
            operator::device_code(&mut request, state_dir, opts)
        } else {
            operator::device_poll(&mut request, state_dir, opts)
        };
        send(request, resp);
        return;
    }
    // Setup is detect only in the board: applying a fix is the
    // operator's command to run (CAD-327).
    if path == "/api/setup" {
        send(
            request,
            err_response(405, "setup is read-only here — GET only"),
        );
        return;
    }
    // CAD-786: the recipient's one-click unsubscribe — the token in
    // the path is the credential, so the write bypasses the board's
    // session gate entirely (classified `RecipientToken` in
    // `operator::WRITE_ROUTES`). Only the exact 43-char minted shape
    // reaches the daemon; anything else is a plain 404.
    if path.starts_with("/unsubscribe/") {
        match (crm_send::unsubscribe_token(path), *method == Method::Post) {
            (Some(token), true) => {
                let response = crm_send::unsubscribe_redeem(&token, state_dir);
                send(request, response);
            }
            (Some(_), false) => send(request, err_response(405, "method not allowed")),
            (None, _) => send(request, err_response(404, "not found")),
        }
        return;
    }
    // CAD-313: every other write is admitted HERE by its class in
    // `operator::WRITE_ROUTES` (unlisted: operator-only) before any
    // handler runs; the handlers below check no caller themselves.
    let caller = match operator::admit(&request, method.as_str(), path, state_dir, opts) {
        Ok(caller) => caller,
        Err(resp) => {
            send(request, resp);
            return;
        }
    };
    // CAD-561: the operator's Update button — the same pipeline the CLI
    // runs, in this process; the card polls `GET /api/update`.
    if path == "/api/update" || path == "/api/update/check" {
        if *method != Method::Post {
            send(request, err_response(405, "method not allowed"));
            return;
        }
        let resp = if path == "/api/update" {
            updates::start(state_dir, opts)
        } else {
            updates::check_now(state_dir)
        };
        send(request, resp);
        return;
    }
    if path == "/api/settings/model-defaults" {
        if *method != Method::Post {
            send(request, err_response(405, "method not allowed"));
            return;
        }
        let resp = model_defaults_post(&mut request, state_dir);
        send(request, resp);
        return;
    }
    // CAD-140: filing a report or idea — the same `report::file` path
    // `cadence report` uses (scrubbing, context, intake shape, PM
    // heads-up), never a bare issue create. Admitted above, like every
    // write; the actor below is the admitted caller, never a field.
    if path == "/api/reports" {
        if *method != Method::Post {
            send(request, err_response(405, "method not allowed"));
            return;
        }
        let Some(caller) = &caller else {
            send(request, err_response(500, "unadmitted write"));
            return;
        };
        let resp = post_report(&mut request, state_dir, pm_dir, caller.actor());
        send(request, resp);
        return;
    }
    // Monitor acknowledgement: this is a daemon-owned durable write, kept
    // beside (and behind the same browser write guards as) tracker writes.
    // The UI never mutates the monitor SQLite store directly, which keeps a
    // board built from a newer binary compatible with an older live daemon.
    if let Some(tail) = path.strip_prefix("/api/monitors/") {
        let mut segs = tail.split('/');
        let monitor = segs.next().unwrap_or_default();
        let alerts = segs.next().unwrap_or_default();
        let seq = segs.next().unwrap_or_default();
        let ack = segs.next().unwrap_or_default();
        let shape_ok = *method == Method::Post
            && !monitor.is_empty()
            && alerts == "alerts"
            && seq.parse::<i64>().is_ok_and(|n| n > 0)
            && ack == "ack"
            && segs.next().is_none();
        if !shape_ok {
            send(request, err_response(404, "no such monitor write route"));
            return;
        }
        let Some(caller) = caller else {
            send(request, err_response(500, "unadmitted write"));
            return;
        };
        // `MonitorAlert` uses the protocol identifier grammar for its audit
        // actor.  The browser actor includes a display suffix, so record
        // the derived author — `operator`, or the pane's alias — rather
        // than passing an invalid or user-controlled value to the daemon.
        let seq = seq.parse::<i64>().unwrap_or_default();
        match client::rpc(
            state_dir,
            "monitor_alert_ack",
            json!({"monitor": monitor, "alert": seq, "by": caller.author()}),
        ) {
            Ok(value) => send(
                request,
                json_response(json!({"ok": true, "alert": value["alert"]})),
            ),
            Err(error) => send(request, err_response(400, &error.to_string())),
        }
        return;
    }

    // Memory curation: POST /api/memories/<project>/<slug>/accept|reject.
    // The CSRF/write guards still run first, but HTTP cannot prove the
    // native socket/PTY identity required by the memory daemon. Refuse
    // explicitly instead of proxying the UI server's peer as a PM.
    if let Some(tail) = path.strip_prefix("/api/memories/") {
        let mut segs = tail.splitn(3, '/');
        let (key, slug, verb) = (
            segs.next().unwrap_or_default(),
            segs.next().unwrap_or_default(),
            segs.next(),
        );
        let shape_ok = matches!(verb, Some("accept" | "reject"))
            && *method == Method::Post
            && !key.is_empty()
            && crate::memory::valid_slug(slug);
        if !shape_ok {
            send(request, err_response(404, "no such write route"));
            return;
        }
        if opts.read_only {
            send(
                request,
                guard_fail("read_only", "board is read-only — writes are disabled"),
            );
            return;
        }
        if let Err(resp) = write_guard(&request, "application/json", opts) {
            send(request, resp);
            return;
        }
        let _ = (key, slug, verb);
        send(
            request,
            write_err(&Error::rejected(
                "memory curation through HTTP is unsupported — use an authenticated native agent endpoint",
            )),
        );
        return;
    }
    // The operator's plan decision (CAD-328 → CAD-360 RPCs) and answer
    // to a question report (CAD-341) — guarded, operator-only, inside
    // `home`.
    if let Some(id) = home::idea_route(path) {
        if *method != Method::Post {
            send(request, err_response(405, "method not allowed"));
            return;
        }
        let Some(caller) = &caller else {
            send(request, err_response(500, "unadmitted write"));
            return;
        };
        let resp = home::decide_idea(&mut request, state_dir, pm_dir, caller.actor(), id);
        send(request, resp);
        return;
    }
    if let Some((epic, verb)) = home::plan_route(path) {
        if *method != Method::Post {
            send(request, err_response(405, "method not allowed"));
            return;
        }
        let resp = home::decide_plan(&mut request, state_dir, epic, verb);
        send(request, resp);
        return;
    }
    // CAD-431: the operator's merge decision — guarded, operator-only,
    // inside `home`.
    if let Some((id, verb)) = home::delivery_route(path) {
        if *method != Method::Post {
            send(request, err_response(405, "method not allowed"));
            return;
        }
        let Some(caller) = &caller else {
            send(request, err_response(500, "unadmitted write"));
            return;
        };
        let resp = home::decide_delivery(&mut request, state_dir, caller.actor(), id, verb);
        send(request, resp);
        return;
    }
    // An epic stage move (CAD-432) — relayed to the daemon's
    // `epic_stage`, operator-only on the board (see `stages`).
    if let Some(epic) = stages::stage_route(path) {
        if *method != Method::Post {
            send(request, err_response(405, "method not allowed"));
            return;
        }
        let resp = stages::move_stage(&mut request, state_dir, epic);
        send(request, resp);
        return;
    }
    // A workflow run (CAD-496) — relayed to the daemon's
    // `plan_propose`, operator-only on the board (see `workflows`).
    if let Some((key, name)) = workflows::propose_route(path) {
        if *method != Method::Post {
            send(request, err_response(405, "method not allowed"));
            return;
        }
        let resp = workflows::propose(&mut request, state_dir, key, name);
        send(request, resp);
        return;
    }
    if let Some(route) = app_contexts::route(path) {
        let writable = matches!(route, app_contexts::Route::List(_)) || !route.is_read();
        if *method != Method::Post || !writable {
            send(request, err_response(405, "method not allowed"));
            return;
        }
        let response = app_contexts::handle(&mut request, state_dir, route, true);
        send(request, response);
        return;
    }
    if let Some(route) = app_audiences::route(path) {
        let writable = !route.is_read();
        if *method != Method::Post || !writable {
            send(request, err_response(405, "method not allowed"));
            return;
        }
        let response = app_audiences::handle(&mut request, state_dir, route, true);
        send(request, response);
        return;
    }
    if let Some(route) = app_records::route(path) {
        let writable = matches!(route, app_records::Route::List(..)) || !route.is_read();
        if *method != Method::Post || !writable {
            send(request, err_response(405, "method not allowed"));
            return;
        }
        let response = app_records::handle(&mut request, state_dir, route, true);
        send(request, response);
        return;
    }
    if let Some(route) = app_content::route(path) {
        let writable = !route.is_read();
        if *method != Method::Post || !writable {
            send(request, err_response(405, "method not allowed"));
            return;
        }
        let response = app_content::handle(&mut request, state_dir, route, true);
        send(request, response);
        return;
    }
    if let Some(route) = connections::route(path) {
        let writable = matches!(route, connections::Route::List) || !route.is_read();
        if *method != Method::Post || !writable {
            send(request, err_response(405, "method not allowed"));
            return;
        }
        let response = connections::handle(&mut request, state_dir, route, true);
        send(request, response);
        return;
    }
    // CAD-785: every CRM SMTP route is operator-proof POST-only —
    // there is no read section, so even `show` requires the full
    // operator session, exactly like the daemon RPC it relays.
    if let Some(route) = crm_smtp::route(path) {
        if *method != Method::Post {
            send(request, err_response(405, "method not allowed"));
            return;
        }
        let response = crm_smtp::handle(&mut request, state_dir, route);
        send(request, response);
        return;
    }
    // CAD-786: the send verbs — operator-proof POST-only writes,
    // exactly like the daemon RPCs they relay.
    if let Some(route) = crm_send::route(path) {
        // Origin is both: POST writes it, GET reads it (read
        // dispatch below takes the GET).
        let write = !route.is_read() || route.is_origin();
        if *method != Method::Post || !write {
            send(request, err_response(405, "method not allowed"));
            return;
        }
        let response = crm_send::handle_write(&mut request, state_dir, route);
        send(request, response);
        return;
    }
    if let Some(route) = app_release::route(path) {
        if *method != Method::Post || !route.is_write() {
            send(request, err_response(405, "method not allowed"));
            return;
        }
        let response = app_release::handle(&mut request, state_dir, route, true);
        send(request, response);
        return;
    }
    if let Some(route) = app_runs::route(path) {
        let writable = matches!(route, app_runs::Route::List) || !route.is_read();
        if *method != Method::Post || !writable {
            send(request, err_response(405, "method not allowed"));
            return;
        }
        let response = app_runs::handle(&mut request, state_dir, route, true);
        send(request, response);
        return;
    }
    if let Some(route) = social_publish::route(path) {
        let writable = matches!(route, social_publish::Route::List) || !route.is_read();
        if *method != Method::Post || !writable {
            send(request, err_response(405, "method not allowed"));
            return;
        }
        let response = social_publish::handle(&mut request, state_dir, route, true);
        send(request, response);
        return;
    }
    let catalog_recovery = path
        .strip_prefix("/api/app-installations/migrations/")
        .and_then(|tail| tail.strip_suffix("/recover"))
        .filter(|id| !id.is_empty() && !id.contains('/'));
    let install_upgrade_recovery = path
        .strip_prefix("/api/app-installations/")
        .and_then(|tail| tail.strip_suffix("/upgrade/recover"))
        .filter(|id| !id.is_empty() && !id.contains('/'));
    let install_upgrade_check = path
        .strip_prefix("/api/app-installations/")
        .and_then(|tail| tail.strip_suffix("/upgrade/check"))
        .filter(|id| !id.is_empty() && !id.contains('/'));
    let install_upgrade = path
        .strip_prefix("/api/app-installations/")
        .and_then(|tail| tail.strip_suffix("/upgrade"))
        .filter(|id| !id.is_empty() && !id.contains('/'));
    let install_recovery = path
        .strip_prefix("/api/app-installations/")
        .and_then(|tail| tail.strip_suffix("/recover"))
        .filter(|id| !id.is_empty() && !id.contains('/'));
    if path == "/api/app-installations/migrate"
        || catalog_recovery.is_some()
        || install_recovery.is_some()
        || install_upgrade.is_some()
        || install_upgrade_check.is_some()
        || install_upgrade_recovery.is_some()
    {
        if *method != Method::Post {
            send(request, err_response(405, "method not allowed"));
            return;
        }
        let (operation, id) = if let Some(id) = catalog_recovery {
            ("app_workspace_migration_recover", Some(id))
        } else if let Some(id) = install_upgrade_recovery {
            ("app_workspace_upgrade_recover", Some(id))
        } else if let Some(id) = install_upgrade_check {
            ("app_workspace_upgrade_check", Some(id))
        } else if let Some(id) = install_upgrade {
            ("app_workspace_upgrade", Some(id))
        } else if let Some(id) = install_recovery {
            ("app_workspace_recover", Some(id))
        } else {
            ("app_workspace_migrate", None)
        };
        let response = apps::workspace(&mut request, state_dir, operation, id);
        send(request, response);
        return;
    }
    if path == "/api/app-installations" {
        if *method != Method::Post {
            send(request, err_response(405, "method not allowed"));
            return;
        }
        let response = apps::workspace(&mut request, state_dir, "app_workspace_install", None);
        send(request, response);
        return;
    }
    // CAD-996: operator-only bounded manual bundle upload — a `{files}` map
    // staged to a server-derived external temp dir (NOT under pm.dir, which
    // the installer refuses as a source), then installed via the unchanged
    // source-path installer. OperatorOnly in WRITE_ROUTES.
    if path == "/api/app-installations/upload" {
        if *method != Method::Post {
            send(request, err_response(405, "method not allowed"));
            return;
        }
        let response = apps::workspace_upload(&mut request, state_dir);
        send(request, response);
        return;
    }
    // An app approval (CAD-557) — relayed to the daemon's
    // `app_approve`, operator-only on the board (see `apps`).
    if let Some((key, name)) = apps::approve_route(path) {
        if *method != Method::Post {
            send(request, err_response(405, "method not allowed"));
            return;
        }
        let resp = apps::approve(&mut request, state_dir, key, name);
        send(request, resp);
        return;
    }
    // An app revocation (CAD-577) — relayed to the daemon's
    // `app_revoke`, operator-only on the board (see `apps`).
    if let Some((key, name)) = apps::revoke_route(path) {
        if *method != Method::Post {
            send(request, err_response(405, "method not allowed"));
            return;
        }
        let resp = apps::revoke(&mut request, state_dir, key, name);
        send(request, resp);
        return;
    }
    // The app's default team (CAD-577) — relayed to the daemon's
    // `app_set_team`, operator-only on the board (see `apps`).
    if let Some((key, name)) = apps::team_route(path) {
        if *method != Method::Post {
            send(request, err_response(405, "method not allowed"));
            return;
        }
        let resp = apps::set_team(&mut request, state_dir, key, name);
        send(request, resp);
        return;
    }
    // "Add worker" (CAD-577) — relayed to the daemon's `app_add_worker`,
    // operator-only on the board (see `apps`).
    if let Some((key, name)) = apps::worker_route(path) {
        if *method != Method::Post {
            send(request, err_response(405, "method not allowed"));
            return;
        }
        let resp = apps::add_worker(&mut request, state_dir, key, name);
        send(request, resp);
        return;
    }
    if let Some(id) = home::answer_route(path) {
        if *method != Method::Post {
            send(request, err_response(405, "method not allowed"));
            return;
        }
        let Some(caller) = &caller else {
            send(request, err_response(500, "unadmitted write"));
            return;
        };
        let resp = home::answer(&mut request, state_dir, pm_dir, caller.actor(), id);
        send(request, resp);
        return;
    }
    // CAD-606: operator Kick off. `admit` already required an operator
    // session; the handler relays `issue_kickoff`, which checks the
    // connection again.
    if let Some(id) = home::kickoff_route(path) {
        if *method != Method::Post {
            send(request, err_response(405, "method not allowed"));
            return;
        }
        let resp = home::post_kickoff(&mut request, state_dir, pm_dir, id);
        send(request, resp);
        return;
    }
    // The composer's slash commands and Stop (CAD-551) — operator-only,
    // relayed to the daemon's `master_command`; `home::master_command`
    // refuses every verb outside its own allowlist before the relay.
    if path == "/api/master/command" {
        if *method != Method::Post {
            send(request, err_response(405, "method not allowed"));
            return;
        }
        let resp = home::master_command(&mut request, state_dir);
        send(request, resp);
        return;
    }
    // The operator's chat message to an agent (CAD-319) — guarded and
    // caller-attributed inside `threads::post_message`.
    if let Some((alias, sub)) = threads::route(path) {
        if *method != Method::Post || sub != Some("messages") {
            send(request, err_response(404, "no such thread write route"));
            return;
        }
        let resp = threads::post_message(&mut request, state_dir, alias);
        send(request, resp);
        return;
    }
    // The Needs-you rail's snooze/dismiss (CAD-574) — operator-only,
    // relayed to the daemon's `needs_dismiss`.
    if let Some((id, verb)) = home::permission_route(path) {
        if *method != Method::Post {
            send(request, err_response(405, "method not allowed"));
            return;
        }
        let resp = home::decide_permission(&mut request, state_dir, id, verb);
        send(request, resp);
        return;
    }
    if let Some(verb) = home::needs_route(path) {
        if *method != Method::Post {
            send(request, err_response(405, "method not allowed"));
            return;
        }
        let resp = home::decide_need(&mut request, state_dir, verb);
        send(request, resp);
        return;
    }
    // The rail's agent resume/unfence (CAD-574) — operator-only on the
    // board; the daemon's own rules for each verb still apply.
    if let Some((alias, verb)) = home::agent_action_route(path) {
        if *method != Method::Post {
            send(request, err_response(405, "method not allowed"));
            return;
        }
        let resp = home::agent_action(&mut request, state_dir, alias, verb);
        send(request, resp);
        return;
    }
    // CAD-608: the issue page's lane. Admitted above (operator-only);
    // the handler then relays on an operator assertion so an in-process
    // board test reaches the daemon's operator gate. Production builds
    // leave that assertion as a no-op — the board process is the proof.
    if let Some(tail) = path.strip_prefix("/api/issues/") {
        if let Some((id, verb)) = lane::write_target(tail) {
            if *method != Method::Post {
                send(request, err_response(405, "method not allowed"));
                return;
            }
            let resp = lane::post(&mut request, state_dir, id, verb);
            send(request, resp);
            return;
        }
    }
    // The wiki routes (CAD-580): the caller `admit` derived rides
    // `wiki_as` to the daemon — writes and uploads relay, paths and
    // ACLs are the daemon's, the board only ever shrinks a caller.
    if let Some(tail) = path.strip_prefix("/api/wiki/") {
        let Some(caller) = caller else {
            send(request, err_response(500, "unadmitted write"));
            return;
        };
        let resp = wiki::write(
            &mut request,
            method,
            tail,
            &caller,
            query,
            state_dir,
            pm_dir,
        );
        send(request, resp);
        return;
    }
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
    let Some(caller) = caller else {
        send(request, err_response(500, "unadmitted write"));
        return;
    };
    let actor = caller.actor().to_string();
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
        // CAD-140: a blank body keeps `new_issue`'s default; a real one
        // is capped like `cadence report`'s (BODY_MAX) so a paste cannot
        // stuff the tracker through the board in one call.
        let body = req.body.as_deref().map(str::trim).filter(|b| !b.is_empty());
        if body.is_some_and(|b| b.len() > crate::issue::report::BODY_MAX) {
            send(
                request,
                err_response(400, "issue body exceeds the 32 KB cap — trim it"),
            );
            return;
        }
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
            &req.tags.unwrap_or_default(),
            None,
            body,
            &actor,
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
        match issue_write::attach_bytes(&pm, &id, &name, &bytes, false, &actor) {
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
                    tags: req.tags,
                },
                req.if_rev.as_deref(),
                &actor,
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
                &actor,
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
                    None,
                    None,
                    req.if_rev.as_deref(),
                    &actor,
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
                Some(caller.author()),
                Some("ui"),
                req.if_rev.as_deref(),
                &actor,
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
pub(crate) fn artifact_response(view: &board::View, name: &str) -> HttpResp {
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

pub(crate) fn send(request: Request, mut resp: HttpResp, head_only: bool) {
    add_security_headers(&mut resp);
    // Only after the route has run its authorization and validation. A
    // validator can never turn an operator-only refusal into a 304.
    let not_modified = request.method() == &Method::Get
        && resp.status_code() == StatusCode(200)
        && resp
            .headers()
            .iter()
            .find(|h| h.field.equiv("ETag"))
            .is_some_and(|etag| {
                header_value(&request, "If-None-Match").is_some_and(|values| {
                    values.split(',').any(|v| v.trim() == etag.value.as_str())
                })
            });
    if not_modified {
        let mut bare = Response::empty(StatusCode(304));
        for h in resp.headers() {
            bare.add_header(h.clone());
        }
        let _ = request.respond(bare);
    } else if head_only {
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
pub(crate) fn with_agents(mut payload: Value, by_issue: &Value, id: &str) -> Value {
    if let Some(agents) = by_issue.get(id) {
        payload["agents"] = agents.clone();
    }
    payload
}
