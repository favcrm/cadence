//! CAD-1019 slice 3: `POST /api/cli/<verb>` — the container end of the
//! remote CLI (the design contract `docs/design/remote-cli.md`;
//! AgenticOS's `POST /__platform/cli/call` forwards here, AOS-128).
//!
//! The bearer is the credential: `Authorization: Bearer wikienv_<env>`
//! where `<env>` is the compact cli actor envelope the issuer minted
//! ([`crate::cli_actor`]). The route re-verifies it independently —
//! signature against the platform JWKS (`{iss}/.well-known/
//! agenticos-board-jwks.json`, the same keyring `board_identity` uses
//! for sign-in assertions), `iss`/`aud`/org/time bounds, then the
//! per-verb `cli.read`/`cli.write` scope. Cookie sessions never satisfy
//! this route (no session is consulted) and this bearer satisfies no
//! other route — `handle` diverts `/api/cli/*` here before any session
//! or write-gate check, and only on this board's own public host.
//!
//! Every verb is dispatched to the exact call the local CLI or board
//! makes: tracker reads go through the board read model / tracker
//! files, agent and message verbs relay the daemon RPCs verbatim, and
//! the write verbs run `issue::write`/`agent_send` with attribution
//! derived from the verified envelope's actor — never operator
//! authority (a `users/…`-derived handle, and `source` carries the
//! provenance label where the verb has no author field).
//!
//! Fail closed: a missing, forged, expired, wrong-audience or
//! wrong-org envelope, an unlisted verb, or an insufficient scope is a
//! 401/403 with the contract's `error.code` and no side effect.
//! Operator-only verbs are not in the table at all — an unlisted
//! `POST /api/cli/shutdown` is refused before its body is read.

use std::path::Path;
use std::sync::Mutex;

use base64::Engine;
use serde::Deserialize;
use serde_json::{json, Value};
use tiny_http::{Header, Method, Request, Response, StatusCode};

use super::read_model;
use super::write_path::{
    self, header_value, parse_json, read_body, write_err, write_reply, HttpResp,
};
use super::ServeOpts;
use crate::board_identity;
use crate::cli_actor::{self, CliActor, Verb};
use crate::client;
use crate::issue::{board, history, model, write as issue_write, Pm};

/// One process-wide JWKS cache for this route family (the daemon keeps
/// its own for sign-in assertions; the board's verify is in-process).
/// The cache keys on the issuer — a stale or foreign-issuer entry
/// refetches, so per-fixture issuers in tests stay honest.
static CLI_JWKS: Mutex<board_identity::JwksCache> = Mutex::new(board_identity::JwksCache::new());

/// Serializes `Spent::load`+`consume` — the single-use check-and-record
/// races on the on-disk set otherwise (two in-flight replays of one
/// jti could both pass). Held for the file read+write only.
static SPENT_LOCK: Mutex<()> = Mutex::new(());

/// A refusal in the contract's `error.code` shape — `{ok:false,
/// error:{code,message}}` plus `no-store`; never a redirect, never a
/// cookie.
fn cli_fail(status: u16, code: &str, message: &str) -> HttpResp {
    let body = serde_json::to_vec(&json!({
        "ok": false,
        "error": {"code": code, "message": message},
    }))
    .unwrap_or_default();
    let mut resp = Response::from_data(body).with_status_code(StatusCode(status));
    resp.add_header(Header::from_bytes("Content-Type", "application/json").unwrap());
    resp.add_header(Header::from_bytes("Cache-Control", "no-store").unwrap());
    resp
}

/// `Rejection.code` → HTTP: `capability_unavailable` (JWKS/config) is
/// ours, everything else is the caller's credential.
fn status_of(code: &str) -> u16 {
    match code {
        "capability_unavailable" => 503,
        _ => 401,
    }
}

/// POST body → bounded JSON object; the forward sends
/// `JSON.stringify(checked.arguments ?? {})` — anything else is a
/// malformed call, refused before a field can name anything.
fn read_args(request: &mut Request) -> std::result::Result<Value, HttpResp> {
    let bytes = read_body(request, write_path::JSON_CAP)?;
    if bytes.iter().all(|b| b.is_ascii_whitespace()) {
        return Ok(json!({}));
    }
    let value: Value = parse_json(&bytes)?;
    if !value.is_object() {
        return Err(cli_fail(
            400,
            "invalid_request",
            "cli call arguments must be a JSON object",
        ));
    }
    // A request field can never name authority — the same refusal the
    // worker's `checkCliCall` applies before forwarding.
    for field in [
        "actor",
        "role",
        "org",
        "user",
        "as",
        "principal",
        "operator",
        "wiki_as",
    ] {
        if value.get(field).is_some() {
            return Err(cli_fail(
                400,
                "invalid_request",
                "cli call arguments cannot carry an identity field",
            ));
        }
    }
    Ok(value)
}

/// The signed header's `kid`, peeked for the JWKS cache lookup only —
/// `verify` re-checks `kid == key.kid` over the same bytes it signs.
fn peek_kid(envelope: &str) -> Result<String, HttpResp> {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let head = envelope.split('.').next().unwrap_or_default();
    let bytes = URL_SAFE_NO_PAD
        .decode(head)
        .map_err(|_| cli_fail(401, "assertion_invalid", "envelope is not base64url"))?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| {
        cli_fail(
            401,
            "bad_header",
            "envelope header is not the contract shape",
        )
    })?;
    Ok(value["kid"].as_str().unwrap_or_default().to_string())
}

/// Verify the bearer into a [`CliActor`] or answer the refusal. The
/// `aud`/`iss`/`organization_id` pins come from this board's configured
/// public identity, never from the envelope or the request.
fn cli_caller(request: &Request, public: &crate::ui::PublicBoard) -> Result<CliActor, HttpResp> {
    let bearer = header_value(request, "Authorization");
    let envelope = match cli_actor::envelope_from_bearer(bearer.as_deref()) {
        Ok(e) => e,
        Err(r) => return Err(cli_fail(status_of(r.code), r.code, &r.message)),
    };
    let config = board_identity::Config {
        host: public.host.clone(),
        issuer: public.issuer.clone(),
        company: public.company.clone(),
    };
    let now = crate::issue::time::now_epoch();
    let kid = peek_kid(&envelope)?;
    let key = {
        let mut cache = CLI_JWKS.lock().unwrap_or_else(|e| e.into_inner());
        match cache.key(&config.issuer, &kid, now) {
            Ok(k) => k,
            Err(e) => {
                let (code, status) = match e.code() {
                    Some("assertion_invalid") => ("assertion_invalid", 401),
                    _ => ("capability_unavailable", 503),
                };
                return Err(cli_fail(status, code, &e.to_string()));
            }
        }
    };
    cli_actor::verify(&envelope, &config, &key, now)
        .map_err(|r| cli_fail(status_of(r.code), r.code, &r.message))
}

/// `POST /api/cli/<tail>` — `handle` calls this only on the public
/// host, ahead of the cookie-session gates, with the already-decoded
/// path. `tail` is everything after `/api/cli/`: exactly one segment,
/// an allowlisted verb — a nested or unknown one is refused before the
/// body or the credential is read.
pub(super) fn post(
    request: &mut Request,
    method: &Method,
    tail: &str,
    state_dir: &Path,
    pm_dir: &Path,
    opts: &ServeOpts,
) -> HttpResp {
    if *method != Method::Post {
        return cli_fail(405, "invalid_request", "cli calls are POST");
    }
    if tail.is_empty() || tail.contains('/') {
        return cli_fail(404, "not_found", "no such cli route");
    }
    let Some(verb) = Verb::of(tail) else {
        return cli_fail(403, "cli_verb_refused", "the verb is not allowlisted");
    };
    let Some(public) = opts.public.as_ref() else {
        // `handle` diverts here only on the public host — but the
        // credential surface never fails open.
        return cli_fail(
            503,
            "capability_unavailable",
            "cli authority is not configured",
        );
    };
    let caller = match cli_caller(request, public) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    if !cli_actor::scope_covers(&caller, verb.scope()) {
        return cli_fail(
            403,
            "insufficient_scope",
            "the envelope's granted scopes do not cover this verb",
        );
    }
    // Direct authenticated route calls (not only `serve`) bind the real
    // boot bridge. This occurs after issuer-derived auth/scope and before
    // replay-state writes, tracker opens or mutation.
    let configured;
    let opts = if matches!(verb, Verb::IssueNew | Verb::IssueComment | Verb::IssueSet) {
        match crate::issue::durability::tracker::runtime_from_env(pm_dir) {
            Ok(Some((mode, hosted))) => {
                configured = {
                    let mut bound = opts.clone();
                    bound.durability_mode = mode;
                    bound.durability = hosted;
                    bound
                };
                &configured
            }
            Ok(None) => opts,
            Err(e) => return cli_fail(503, "capability_unavailable", &e.to_string()),
        }
    } else {
        opts
    };
    if matches!(verb, Verb::IssueNew | Verb::IssueComment | Verb::IssueSet) {
        if let Err(response) = durable_gate(opts) {
            return response;
        }
    }
    let args = match read_args(request) {
        Ok(a) => a,
        Err(resp) => return resp,
    };
    // The envelope is single-use (the contract, and the review's
    // Important): a bearer can be replayed at this route directly,
    // skipping the worker's live-bearer rebind, so the container is
    // the single-use authority. Only a verified, authorized,
    // well-formed call consumes — a forged replay never burns the
    // real jti and a malformed call leaves it usable — and the
    // consume is right before dispatch so a refused replay writes
    // nothing. The set persists (`cli-jtis.json` in state_dir): a
    // restart inside the ≤300 s window still refuses.
    let spent = {
        // The check-and-record is one critical section: two
        // simultaneous replays of the same jti must not both load
        // a pre-consume set.
        let _guard = SPENT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut set = cli_actor::Spent::load(state_dir);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or_default();
        set.consume(&caller.jti, caller.exp, now)
    };
    if !spent {
        return cli_fail(
            401,
            "assertion_replayed",
            "the envelope's jti was already consumed",
        );
    }
    dispatch(verb, &args, &caller, state_dir, pm_dir, opts)
}

/// `arguments["k"]` as a string — a present-but-nonstring field is a
/// malformed call, never a silent default.
fn arg_str<'a>(args: &'a Value, key: &str) -> Result<Option<&'a str>, HttpResp> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.as_str())),
        _ => Err(cli_fail(
            400,
            "invalid_request",
            &format!("cli argument '{key}' must be a string"),
        )),
    }
}

fn arg_u64(args: &Value, key: &str) -> Result<Option<u64>, HttpResp> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v.as_u64().map(Some).ok_or_else(|| {
            cli_fail(
                400,
                "invalid_request",
                &format!("cli argument '{key}' must be a nonnegative integer"),
            )
        }),
    }
}

/// `arguments["k"]` as a string array (tags, ids, set pairs).
fn arg_strs(args: &Value, key: &str) -> Result<Vec<String>, HttpResp> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) => items
            .iter()
            .map(|v| {
                v.as_str().map(str::to_string).ok_or_else(|| {
                    cli_fail(
                        400,
                        "invalid_request",
                        &format!("cli argument '{key}' must be strings"),
                    )
                })
            })
            .collect(),
        _ => Err(cli_fail(
            400,
            "invalid_request",
            &format!("cli argument '{key}' must be an array of strings"),
        )),
    }
}

/// A daemon-RPC call relayed verbatim — the HTTP path is never less
/// strict than the RPC it relays: the board's own connection carries
/// no more authority than the caller's scope already proved.
fn rpc(state_dir: &Path, method: &str, params: Value) -> HttpResp {
    match client::rpc(state_dir, method, params) {
        Ok(v) => json_ok(v),
        Err(e) => write_err(&e),
    }
}

fn json_ok(v: Value) -> HttpResp {
    let mut resp = Response::from_data(serde_json::to_vec(&v).unwrap_or_default())
        .with_status_code(StatusCode(200));
    resp.add_header(Header::from_bytes("Content-Type", "application/json").unwrap());
    resp.add_header(Header::from_bytes("Cache-Control", "no-store").unwrap());
    resp
}

fn pm_at(pm_dir: &Path, opts: &ServeOpts) -> Result<Pm, HttpResp> {
    let mut pm =
        Pm::at(pm_dir).map_err(|e| cli_fail(503, "tracker_unavailable", &e.to_string()))?;
    // CAD-1180: attach the hosted durability capability when the board
    // is configured for it. `None` leaves the Pm exactly as the local
    // path opens it — the write functions then skip capture/persist.
    // The pre-write gate (`durable_gate`) has already refused an
    // unusable capability before this opens the tracker.
    pm.attach_durability_mode(opts.durability_mode);
    if let Some(hosted) = opts.durability.clone() {
        pm.attach_durability(hosted);
    }
    Ok(pm)
}

/// CAD-1180: the pre-write durability gate. When the board declares a
/// hosted capability (`opts.durability` is `Some`), the write is
/// refused `capability_unavailable` **before** any mutation if the
/// capability cannot bind this board's actual company —
/// `opts.public.company`, the same id the actor envelope's
/// `organization_id` was verified against, never a caller field. `None`
/// passes ONLY in explicit Legacy mode; Required without an ordered backend
/// refuses. Reads never call this. Writes check before replay-state mutation.
fn durable_gate(opts: &ServeOpts) -> Result<(), HttpResp> {
    let Some(hosted) = &opts.durability else {
        return if opts.durability_mode == crate::issue::durability::Mode::Required {
            Err(cli_fail(
                503,
                "capability_unavailable",
                "required tracker-v1 backend missing",
            ))
        } else {
            Ok(())
        };
    };
    let expected = opts
        .public
        .as_ref()
        .map(|p| p.company.as_str())
        .unwrap_or_default();
    hosted
        .validate_for(expected)
        .and_then(|()| hosted.validate_mode(opts.durability_mode))
        .map_err(|e| cli_fail(503, "capability_unavailable", &e.to_string()))
}

/// The verb → the same call the CLI/board makes for it. `actor` is the
/// verified envelope identity — `handle` for author/provenance fields.
/// No request field ever renames it.
fn dispatch(
    verb: Verb,
    args: &Value,
    actor: &CliActor,
    state_dir: &Path,
    pm_dir: &Path,
    opts: &ServeOpts,
) -> HttpResp {
    match verb {
        // ---- cli.read ----
        Verb::Status => {
            // `cadence status` is daemon health plus the agent list —
            // the same pair the board's own header reads.
            let health = client::rpc(state_dir, "health", json!({}));
            let agents = client::rpc(state_dir, "agent_list", json!({"board": true}));
            match (health, agents) {
                (Ok(h), Ok(a)) => json_ok(json!({
                    "daemon": h, "agents": a["agents"], "live": a["live"],
                })),
                (Err(e), _) | (_, Err(e)) => write_err(&e),
            }
        }
        Verb::AgentList => {
            let mut params = json!({"board": true});
            for key in ["states", "providers", "kinds"] {
                match arg_strs(args, key) {
                    Ok(v) if !v.is_empty() => params[key] = json!(v),
                    Ok(_) => {}
                    Err(resp) => return resp,
                }
            }
            rpc(state_dir, "agent_list", params)
        }
        Verb::AgentShow => match arg_str(args, "alias") {
            Ok(Some(alias)) => {
                let mut params = json!({"alias": alias});
                match arg_u64(args, "limit") {
                    Ok(Some(n)) => params["limit"] = json!(n),
                    Ok(None) => {}
                    Err(resp) => return resp,
                }
                rpc(state_dir, "agent_show", params)
            }
            _ => cli_fail(400, "invalid_request", "agent_show needs arguments.alias"),
        },
        Verb::IssueLs => match pm_at(pm_dir, opts) {
            Err(resp) => resp,
            Ok(pm) => {
                let filter = board::Filter {
                    tags: arg_strs(args, "tag").unwrap_or_default(),
                    epics: arg_strs(args, "epic").unwrap_or_default(),
                    owners: arg_strs(args, "owner").unwrap_or_default(),
                    statuses: arg_strs(args, "status").unwrap_or_default(),
                    components: arg_strs(args, "component").unwrap_or_default(),
                    priorities: arg_strs(args, "priority").unwrap_or_default(),
                    types: arg_strs(args, "type").unwrap_or_default(),
                    milestones: arg_strs(args, "milestone").unwrap_or_default(),
                    plans: arg_strs(args, "plan").unwrap_or_default(),
                    open: args.get("open").and_then(Value::as_bool) == Some(true),
                };
                if let Err(e) = filter.validate() {
                    return cli_fail(400, "invalid_request", &e.to_string());
                }
                let read = read_model::get(state_dir, pm_dir).board(&pm, None);
                let by_id = read.by_id();
                let ctx = read.ctx(&by_id);
                json_ok(json!({
                    "issues": read.views.iter()
                        .filter(|v| filter.matches(v))
                        .map(|v| read.card(&ctx, v))
                        .collect::<Vec<_>>(),
                }))
            }
        },
        Verb::IssueShow => match arg_str(args, "id") {
            Ok(Some(raw)) => {
                let Ok(id) = model::check_id(raw) else {
                    return cli_fail(400, "invalid_request", "bad issue id");
                };
                match pm_at(pm_dir, opts) {
                    Err(resp) => resp,
                    Ok(pm) => {
                        let read = read_model::get(state_dir, pm_dir).board(&pm, None);
                        let by_id = read.by_id();
                        match by_id.get(&id) {
                            Some(view) => json_ok(json!({
                                "issue": crate::issue::work::detail_json(
                                    &pm.dir,
                                    &read.ctx(&by_id),
                                    view,
                                ),
                            })),
                            None => cli_fail(404, "not_found", "unknown issue"),
                        }
                    }
                }
            }
            _ => cli_fail(400, "invalid_request", "issue_show needs arguments.id"),
        },
        Verb::IssueHistory => match arg_str(args, "id") {
            Ok(Some(raw)) => {
                let Ok(id) = model::check_id(raw) else {
                    return cli_fail(400, "invalid_request", "bad issue id");
                };
                let limit = match arg_u64(args, "limit") {
                    Ok(Some(n)) => n as usize,
                    Ok(None) => 50,
                    Err(resp) => return resp,
                };
                match pm_at(pm_dir, opts) {
                    Err(resp) => resp,
                    Ok(pm) => match board::find_issue(&pm.dir, &id) {
                        Ok(issue) => match history::log(&pm.dir, &issue, limit.max(1)) {
                            Ok(h) => json_ok(json!({"id": id, "history": h})),
                            Err(e) => cli_fail(503, "tracker_unavailable", &e.to_string()),
                        },
                        Err(e) => cli_fail(404, "not_found", &e.to_string()),
                    },
                }
            }
            _ => cli_fail(400, "invalid_request", "issue_history needs arguments.id"),
        },
        Verb::MessageRead => match arg_str(args, "message") {
            Ok(Some(id)) => {
                let mut params = json!({"message": id});
                match arg_u64(args, "offset") {
                    Ok(Some(n)) => params["offset"] = json!(n),
                    Ok(None) => {}
                    Err(resp) => return resp,
                }
                match arg_u64(args, "limit") {
                    Ok(Some(n)) => params["limit"] = json!(n),
                    Ok(None) => {}
                    Err(resp) => return resp,
                }
                rpc(state_dir, "message_read", params)
            }
            _ => cli_fail(
                400,
                "invalid_request",
                "message_read needs arguments.message",
            ),
        },
        Verb::MessageInbox => match arg_str(args, "alias") {
            Ok(Some(alias)) => {
                let mut params = json!({"alias": alias, "peek": true});
                match arg_u64(args, "after") {
                    Ok(Some(n)) => params["after"] = json!(n),
                    Ok(None) => {}
                    Err(resp) => return resp,
                }
                match arg_str(args, "reader") {
                    Ok(Some(r)) => params["reader"] = json!(r),
                    Ok(None) => {}
                    Err(resp) => return resp,
                }
                rpc(state_dir, "agent_inbox", params)
            }
            _ => cli_fail(
                400,
                "invalid_request",
                "message_inbox needs arguments.alias",
            ),
        },
        // `team list` has no CLI/RPC counterpart in this build — the
        // contract's v1 list names it, but there is no local call to
        // relay. Refused rather than silently dropped so the worker's
        // allowlist and this table never disagree silently.
        Verb::TeamList => cli_fail(
            501,
            "cli_verb_refused",
            "team_list has no local counterpart in this build",
        ),
        // ---- cli.write ----
        Verb::IssueNew => match pm_at(pm_dir, opts) {
            Err(resp) => resp,
            Ok(pm) => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct NewReq {
                    project: String,
                    title: String,
                    priority: Option<String>,
                    owner: Option<String>,
                    component: Option<String>,
                    tags: Option<Vec<String>>,
                    parent: Option<String>,
                    blocked_by: Option<Vec<String>>,
                    body: Option<String>,
                }
                let req: NewReq = match serde_json::from_value(args.clone()) {
                    Ok(r) => r,
                    Err(e) => {
                        return cli_fail(400, "invalid_request", &format!("issue_new: {e}"));
                    }
                };
                let body = req.body.as_deref().map(str::trim).filter(|b| !b.is_empty());
                if body.is_some_and(|b| b.len() > crate::issue::report::BODY_MAX) {
                    return cli_fail(
                        400,
                        "invalid_request",
                        "issue body exceeds the 32 KB cap — trim it",
                    );
                }
                match issue_write::new_issue(
                    &pm,
                    &pm.dir,
                    Some(&req.project),
                    &req.title,
                    req.priority.as_deref(),
                    req.parent.as_deref(),
                    &req.blocked_by.unwrap_or_default(),
                    req.owner.as_deref(),
                    req.component.as_deref(),
                    &req.tags.unwrap_or_default(),
                    None,
                    body,
                    &actor.handle,
                ) {
                    Ok(out) => {
                        let id = out["id"].as_str().unwrap_or_default().to_string();
                        write_reply(&pm, state_dir, &id, out, true)
                    }
                    Err(e) => write_err(&e),
                }
            }
        },
        Verb::IssueComment => match (arg_str(args, "id"), arg_str(args, "body")) {
            (Ok(Some(raw)), Ok(Some(body))) => {
                let Ok(id) = model::check_id(raw) else {
                    return cli_fail(400, "invalid_request", "bad issue id");
                };
                let if_rev = match arg_str(args, "if_rev") {
                    Ok(v) => v,
                    Err(resp) => return resp,
                };
                match pm_at(pm_dir, opts) {
                    Err(resp) => resp,
                    Ok(pm) => match issue_write::add_comment(
                        &pm,
                        &id,
                        body,
                        Some(&actor.handle),
                        Some("remote-cli"),
                        if_rev,
                        &actor.actor,
                    ) {
                        Ok(out) => write_reply(&pm, state_dir, &id, out, false),
                        Err(e) => write_err(&e),
                    },
                }
            }
            (Err(resp), _) | (_, Err(resp)) => resp,
            _ => cli_fail(
                400,
                "invalid_request",
                "issue_comment needs arguments.id and arguments.body",
            ),
        },
        Verb::IssueSet => {
            match (arg_strs(args, "ids"), arg_strs(args, "set")) {
                (Ok(ids), Ok(pairs)) => {
                    // Revisions are per-ticket and optimistic: exactly one
                    // issue, and `if_rev` (the `rev` `issue_show` reports)
                    // is mandatory — a remote field edit is never an
                    // unconditional last-writer-wins write. The check runs
                    // inside `set_fields_if_rev` under the tracker lock, at
                    // the point of write; a stale token answers the
                    // contract's conflict payload instead of a 200.
                    if ids.len() != 1 {
                        return cli_fail(
                            400,
                            "invalid_request",
                            "issue_set edits exactly one issue — revisions are per-ticket",
                        );
                    }
                    // No remote override of the `status=done` evidence gate:
                    // the verb never accepted `force` — a caller naming one
                    // is refused, never silently dropped, before any write.
                    if args.get("force").is_some() {
                        return cli_fail(
                            400,
                            "invalid_request",
                            "issue_set does not accept arguments.force",
                        );
                    }
                    let if_rev = match arg_str(args, "if_rev") {
                        Ok(Some(rev)) if !rev.trim().is_empty() => rev,
                        Ok(_) => {
                            return cli_fail(
                                400,
                                "invalid_request",
                                "issue_set needs a nonempty arguments.if_rev — re-read the \
                             issue's rev and send it",
                            );
                        }
                        Err(resp) => return resp,
                    };
                    match pm_at(pm_dir, opts) {
                        Err(resp) => resp,
                        Ok(pm) => match issue_write::set_fields_if_rev(
                            &pm,
                            &ids,
                            &pairs,
                            &actor.handle,
                            None,
                            Some(if_rev),
                        ) {
                            Ok(out) => write_reply(&pm, state_dir, &ids[0], out, false),
                            Err(e) => write_err(&e),
                        },
                    }
                }
                (Err(resp), _) | (_, Err(resp)) => resp,
            }
        }
        Verb::MessageSend => match (arg_str(args, "alias"), arg_str(args, "text")) {
            (Ok(Some(alias)), Ok(Some(text))) => {
                if text.trim().is_empty() {
                    return cli_fail(400, "invalid_request", "message_send text is empty");
                }
                // `source` is the envelope-derived provenance the daemon
                // stamps on the queue entry (identifier grammar,
                // lowercase) — never an identity field, never "operator".
                let source = format!("cli-{}", actor.handle.to_ascii_lowercase());
                let mut params = json!({"alias": alias, "text": text, "source": source});
                for key in ["message", "reply_to", "task"] {
                    match arg_str(args, key) {
                        Ok(Some(v)) => params[key] = json!(v),
                        Ok(None) => {}
                        Err(resp) => return resp,
                    }
                }
                rpc(state_dir, "agent_send", params)
            }
            (Err(resp), _) | (_, Err(resp)) => resp,
            _ => cli_fail(
                400,
                "invalid_request",
                "message_send needs arguments.alias and arguments.text",
            ),
        },
    }
}

#[cfg(test)]
mod cad1179_acceptance;

#[cfg(test)]
mod cad1180_acceptance;

#[cfg(test)]
mod cad1180_host_fixture;
