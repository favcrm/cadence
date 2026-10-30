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
//! files, agent and message verbs relay daemon RPCs, and the write
//! verbs run `issue::write`/the daemon's `cli_message_*` handlers with
//! attribution derived from the verified envelope's actor — never
//! operator authority (a `users/…`-derived handle, and `source` carries
//! the provenance label where the verb has no author field). The
//! message verbs do NOT relay verbatim: the raw signed envelope rides
//! to `cli_message_send`/`cli_message_read`/`cli_message_inbox` as
//! `cli_envelope` over this proven board connection, and the daemon
//! re-verifies it against its own trust root (CAD-1568) — a
//! `cli_*`-carrying relayed call can never widen into operator
//! attribution or another member's mail.
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

#[path = "cli_delivery_reads.rs"]
mod delivery_reads;

#[cfg(all(test, feature = "cli-runtime-test"))]
pub(crate) fn prime_jwks_for_test(issuer: &str, kid: &str, now: i64) -> crate::Result<()> {
    CLI_JWKS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .key(issuer, kid, now)
        .map(|_| ())
}

/// Serializes `Spent::load`+`consume` — the single-use check-and-record
/// races on the on-disk set otherwise (two in-flight replays of one
/// jti could both pass). Held for the file read+write only.
pub(super) static SPENT_LOCK: Mutex<()> = Mutex::new(());

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

/// Verify the bearer into the [`CliActor`] plus the raw compact
/// envelope — the actor derives every attribution, and the bytes go to
/// the daemon's `cli_message_*` RPCs as `cli_envelope`, which re-verifies
/// them against its own trust root (CAD-1568). The `aud`/`iss`/
/// `organization_id` pins come from this board's configured public
/// identity, never from the envelope or the request.
fn cli_caller(
    request: &Request,
    public: &crate::ui::PublicBoard,
) -> Result<(CliActor, String), HttpResp> {
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
        .map(|actor| (actor, envelope))
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
    let (caller, envelope) = match cli_caller(request, public) {
        Ok(pair) => pair,
        Err(resp) => return resp,
    };
    if !cli_actor::scope_covers(&caller, verb.scope()) {
        return cli_fail(
            403,
            "insufficient_scope",
            "the envelope's granted scopes do not cover this verb",
        );
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
    dispatch(verb, &args, &caller, &envelope, state_dir, pm_dir, opts)
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
/// no more authority than the caller's scope already proved. A typed
/// daemon refusal keeps its contract `code` (a recipient-class or
/// ownership refusal the remote caller acts on); an untyped refusal is
/// `invalid_request`, a daemon-internal failure `capability_unavailable`
/// — never a fabricated success.
fn rpc(state_dir: &Path, method: &str, params: Value) -> HttpResp {
    match client::rpc(state_dir, method, params) {
        Ok(v) => json_ok(v),
        Err(e) => rpc_error(e),
    }
}

/// [`rpc`]'s refusal mapping, for a caller that needs the answer first.
fn rpc_error(e: crate::error::Error) -> HttpResp {
    match e {
        e @ crate::error::Error::Structured(_) => {
            let code = e.code().unwrap_or("invalid_request").to_string();
            let status = match code.as_str() {
                "capability_unavailable" => 503,
                "insufficient_scope" | "not_permitted" | "recipient_class_unsupported" => 403,
                "revision_conflict" => 409,
                "not_found" => 404,
                _ => 400,
            };
            cli_fail(status, &code, &e.to_string())
        }
        e => match e.kind() {
            "rejected" => cli_fail(400, "invalid_request", &e.to_string()),
            _ => cli_fail(503, "capability_unavailable", &e.to_string()),
        },
    }
}

fn json_ok(v: Value) -> HttpResp {
    let mut resp = Response::from_data(serde_json::to_vec(&v).unwrap_or_default())
        .with_status_code(StatusCode(200));
    resp.add_header(Header::from_bytes("Content-Type", "application/json").unwrap());
    resp.add_header(Header::from_bytes("Cache-Control", "no-store").unwrap());
    resp
}

fn pm_at(pm_dir: &Path) -> Result<Pm, HttpResp> {
    Pm::at(pm_dir).map_err(|e| cli_fail(503, "tracker_unavailable", &e.to_string()))
}

/// `Some(refusal)` when this project exists but does not admit the
/// verified caller. The answer is the one an absent project gives, so a
/// restricted project cannot be told apart from a missing one
/// (AOS-274 / SEC-003). A project that does not exist is left to the
/// verb's own unknown-project answer; an unreadable tracker refuses.
fn restricted(pm: &Pm, key: &str, actor: &CliActor) -> Option<HttpResp> {
    if super::project_policy::refused(pm, key, &actor.policy_spelling()) {
        Some(cli_fail(404, "not_found", "unknown project"))
    } else {
        None
    }
}

/// [`restricted`] for a verb that names an issue id rather than a project
/// key: resolve the issue's project first. An unknown id is left to the
/// verb's own not-found answer.
fn restricted_issue(pm: &Pm, id: &str, actor: &CliActor) -> Option<HttpResp> {
    let Ok(issue) = board::find_issue(&pm.dir, id) else {
        return None;
    };
    restricted(pm, &issue.project, actor)
}

/// The project keys this verified caller may read. Fail closed: an
/// unknown actor spelling or an unreadable tracker yields an empty set,
/// so a listing contributes no row, count or placeholder for a project
/// the caller may not see.
fn readable_projects(pm: &Pm, actor: &CliActor) -> std::collections::BTreeSet<String> {
    super::project_policy::readable(pm, &actor.policy_spelling())
}

/// The verb → the same call the CLI/board makes for it. `actor` is the
/// verified envelope identity — `handle` for author/provenance fields.
/// No request field ever renames it. `envelope` is the raw compact JWS
/// the route just verified: the message verbs relay it as
/// `cli_envelope` to the daemon's `cli_message_*` RPCs, which re-verify
/// it against the daemon's own trust root rather than trusting this
/// connection's authority (CAD-1568).
fn dispatch(
    verb: Verb,
    args: &Value,
    actor: &CliActor,
    envelope: &str,
    state_dir: &Path,
    pm_dir: &Path,
    _opts: &ServeOpts,
) -> HttpResp {
    match verb {
        // ---- cli.read ----
        Verb::MyWork => {
            if !args.as_object().is_some_and(|args| args.is_empty()) {
                return cli_fail(400, "invalid_request", "my_work accepts no arguments");
            }
            // Envelope verification permits `operator` only for an Owner user;
            // its attribution handle is still derived from that user's sub.
            if actor.actor != "operator" && !actor.actor.starts_with("users/") {
                return cli_fail(403, "human_session_required", "my_work requires a user");
            }
            let policy_actor = actor.policy_spelling();
            match super::my_work::Snapshot::load(state_dir, pm_dir, &policy_actor) {
                Ok(snapshot) => {
                    match snapshot.payload(pm_dir, &actor.handle, Some(&actor.sub), &policy_actor) {
                        Some(body) => json_ok(body),
                        None => cli_fail(
                            403,
                            "project_access_changed",
                            "My work project access changed; refresh",
                        ),
                    }
                }
                Err(error) => write_err(&error),
            }
        }
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
        Verb::IssueLs => match pm_at(pm_dir) {
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
                // AOS-274: a project this caller may not read contributes
                // no row at all — filtered before the filter and the card.
                let allowed = readable_projects(&pm, actor);
                json_ok(json!({
                    "issues": read.views.iter()
                        .filter(|v| filter.matches(v) && allowed.contains(&v.issue.project))
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
                match pm_at(pm_dir) {
                    Err(resp) => resp,
                    Ok(pm) => {
                        let read = read_model::get(state_dir, pm_dir).board(&pm, None);
                        let by_id = read.by_id();
                        match by_id.get(&id) {
                            Some(view) => {
                                if let Some(resp) = restricted(&pm, &view.issue.project, actor) {
                                    return resp;
                                }
                                json_ok(json!({
                                    "issue": crate::issue::work::detail_json(
                                        &pm.dir,
                                        &read.ctx(&by_id),
                                        view,
                                    ),
                                }))
                            }
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
                match pm_at(pm_dir) {
                    Err(resp) => resp,
                    Ok(pm) => match board::find_issue(&pm.dir, &id) {
                        Ok(issue) => {
                            if let Some(resp) = restricted(&pm, &issue.project, actor) {
                                return resp;
                            }
                            match history::log(&pm.dir, &issue, limit.max(1)) {
                                Ok(h) => json_ok(json!({"id": id, "history": h})),
                                Err(e) => cli_fail(503, "tracker_unavailable", &e.to_string()),
                            }
                        }
                        Err(e) => cli_fail(404, "not_found", &e.to_string()),
                    },
                }
            }
            _ => cli_fail(400, "invalid_request", "issue_history needs arguments.id"),
        },
        // CAD-1568: the message verbs relay the re-verifiable signed
        // envelope as `cli_envelope` to the daemon's `cli_message_*`
        // RPCs — the daemon re-verifies it over this proven board
        // connection and scopes everything by the verified principal;
        // a bare `agent_inbox`/`message_read` relay would see an
        // operator-class connection and read any member's mail.
        Verb::MessageRead => match arg_str(args, "message") {
            Ok(Some(_)) => {
                match arg_u64(args, "offset") {
                    Ok(_) => {}
                    Err(resp) => return resp,
                }
                match arg_u64(args, "limit") {
                    Ok(_) => {}
                    Err(resp) => return resp,
                }
                let mut params = args.clone();
                params["cli_envelope"] = json!(envelope);
                rpc(state_dir, "cli_message_read", params)
            }
            _ => cli_fail(
                400,
                "invalid_request",
                "message_read needs arguments.message",
            ),
        },
        Verb::MessageInbox => match arg_str(args, "alias") {
            Ok(Some(_)) => {
                match arg_u64(args, "after") {
                    Ok(_) => {}
                    Err(resp) => return resp,
                }
                match arg_str(args, "reader") {
                    Ok(_) => {}
                    Err(resp) => return resp,
                }
                let mut params = args.clone();
                params["cli_envelope"] = json!(envelope);
                rpc(state_dir, "cli_message_inbox", params)
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
        // CAD-1586: settings management relays to the daemon's
        // `cli_settings_command` — the envelope is forwarded verbatim
        // (`cli_envelope`) and re-verified against the daemon's own
        // trust root, so a relayed call can never widen into local
        // operator authority. The op's settings.* scope and the
        // principal class are enforced daemon-side; this relay carries
        // no more than the signed envelope proves.
        Verb::SettingsAgents => {
            let mut params = args.clone();
            if params.get("operation").and_then(Value::as_str).is_none() {
                return cli_fail(
                    400,
                    "invalid_request",
                    "settings_agents needs arguments.operation",
                );
            }
            params["cli_envelope"] = json!(envelope);
            rpc(state_dir, "cli_settings_command", params)
        }
        // CAD-1586: master launch diagnosis — a strict translation
        // into the shared settings command path (`master.status` in
        // the op table), never a separate handler. Args admit only an
        // optional `grant_id`; anything else is refused before relay.
        // The envelope rides verbatim and is re-verified daemon-side,
        // so this relay grants no authority the claim lacks.
        Verb::MasterStatus => {
            if let Some(object) = args.as_object() {
                for key in object.keys() {
                    if key != "grant_id" {
                        return cli_fail(
                            400,
                            "invalid_request",
                            "master_status arguments admit only 'grant_id'",
                        );
                    }
                }
            }
            let mut op_payload = serde_json::Map::new();
            if let Some(grant) = args.get("grant_id").and_then(Value::as_str) {
                op_payload.insert("grant_id".to_string(), json!(grant));
            }
            rpc(
                state_dir,
                "cli_settings_command",
                json!({
                    "cli_envelope": envelope,
                    "operation": "master.status",
                    "expected_revision": Value::Null,
                    "payload": Value::Object(op_payload),
                }),
            )
        }
        // CAD-1946: the merge gate's reads — human Owner only.
        Verb::DeliveryRequirements | Verb::AuditApproval | Verb::AuditVerdicts => {
            delivery_reads::serve(verb, args, actor, state_dir, pm_dir)
        }
        // ---- cli.write ----
        Verb::IssueNew => match pm_at(pm_dir) {
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
                if let Some(resp) = restricted(&pm, &req.project, actor) {
                    return resp;
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
                match pm_at(pm_dir) {
                    Err(resp) => resp,
                    Ok(pm) => {
                        if let Some(resp) = restricted_issue(&pm, &id, actor) {
                            return resp;
                        }
                        match issue_write::add_comment(
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
                        }
                    }
                }
            }
            (Err(resp), _) | (_, Err(resp)) => resp,
            _ => cli_fail(
                400,
                "invalid_request",
                "issue_comment needs arguments.id and arguments.body",
            ),
        },
        Verb::IssueSet => match (arg_strs(args, "ids"), arg_strs(args, "set")) {
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
                match pm_at(pm_dir) {
                    Err(resp) => resp,
                    Ok(pm) => {
                        if let Some(resp) = restricted_issue(&pm, &ids[0], actor) {
                            return resp;
                        }
                        match issue_write::set_fields_if_rev(
                            &pm,
                            &ids,
                            &pairs,
                            &actor.handle,
                            None,
                            Some(if_rev),
                        ) {
                            Ok(out) => write_reply(&pm, state_dir, &ids[0], out, false),
                            Err(e) => write_err(&e),
                        }
                    }
                }
            }
            (Err(resp), _) | (_, Err(resp)) => resp,
        },
        // Human Owner only: verified `role_at_issue=owner` AND
        // `principal_kind=user`. An agent can be minted with the owner
        // role and must not administer policy. Neither value is an
        // argument. The record this verb writes decides who may read
        // the project, so a member must not widen it or grant access.
        // This arm does not consult the project's own restriction —
        // administering a policy cannot depend on the policy it is
        // administering.
        Verb::ProjectPolicy => {
            if actor.role_at_issue != "owner" || actor.principal_kind != "user" {
                return cli_fail(
                    403,
                    "not_permitted",
                    "project_policy is human-owner only — a member or an agent cannot record project access",
                );
            }
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct PolicyReq {
                project: String,
                visibility: String,
                grants: Vec<String>,
            }
            let req: PolicyReq = match serde_json::from_value(args.clone()) {
                Ok(r) => r,
                Err(e) => {
                    return cli_fail(400, "invalid_request", &format!("project_policy: {e}"));
                }
            };
            let visibility = match req.visibility.as_str() {
                "workspace-visible" => crate::issue::project::Visibility::WorkspaceVisible,
                "private" => crate::issue::project::Visibility::Private,
                other => {
                    return cli_fail(
                        400,
                        "invalid_request",
                        &format!(
                            "project_policy: unknown visibility `{other}` — \
                             workspace-visible or private"
                        ),
                    );
                }
            };
            match pm_at(pm_dir) {
                Err(resp) => resp,
                Ok(pm) => match issue_write::set_project_policy(
                    &pm,
                    &req.project,
                    visibility,
                    &req.grants,
                    &format!("users/{}", actor.handle),
                ) {
                    Ok(out) => json_ok(out),
                    Err(e) => write_err(&e),
                },
            }
        }
        // CAD-1568: never a bare `agent_send` relay — that connection is
        // operator-class, so the daemon would record the message as the
        // operator's. The signed envelope rides as `cli_envelope` and
        // the daemon's `cli_message_send` re-verifies it, derives the
        // principal and enforces the remote recipient classes itself.
        // Arguments are exactly `alias`, `text` and the stable `message`
        // id; the daemon refuses every other field, so routing
        // (`reply_to`, `task`) and steering options are rejected, never
        // silently dropped.
        Verb::MessageSend => match (arg_str(args, "alias"), arg_str(args, "text")) {
            (Ok(Some(_)), Ok(Some(text))) => {
                if text.trim().is_empty() {
                    return cli_fail(400, "invalid_request", "message_send text is empty");
                }
                match arg_str(args, "message") {
                    Ok(Some(id)) if !id.trim().is_empty() => {}
                    Ok(_) => {
                        return cli_fail(
                            400,
                            "invalid_request",
                            "message_send needs a stable arguments.message id — supply one \
                             so a retry after a lost response dedupes instead of resubmitting blind",
                        )
                    }
                    Err(resp) => return resp,
                }
                let mut params = args.clone();
                params["cli_envelope"] = json!(envelope);
                rpc(state_dir, "cli_message_send", params)
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
mod policy_tests {
    use super::*;

    fn actor_with_role(spelling: &str, role: &str) -> CliActor {
        let principal_kind = if spelling.starts_with("agents/") {
            "agent"
        } else if spelling.starts_with("service/") {
            "service"
        } else {
            "user"
        };
        CliActor {
            sub: "sub".into(),
            actor: spelling.into(),
            role_at_issue: role.into(),
            principal_kind: principal_kind.into(),
            scopes: vec!["cli.read".into(), "cli.write".into()],
            credential_id: "cred".into(),
            jti: "jti".into(),
            exp: i64::MAX,
            // Exactly what `verify` derives from the claims, so the helper
            // cannot drift from the production chain.
            handle: cli_actor::handle_of(spelling, "sub"),
        }
    }

    /// A member envelope — the role the gate must refuse for the verbs
    /// the contract reserves to the Owner.
    fn actor(spelling: &str) -> CliActor {
        actor_with_role(spelling, "member")
    }

    fn pm_with(policy: &str) -> (tempfile::TempDir, Pm) {
        let dir = tempfile::tempdir().unwrap();
        Pm::init(dir.path()).unwrap();
        let project = dir.path().join("demo");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            project.join("project.yaml"),
            format!("key: demo\nprefix: D\n{policy}"),
        )
        .unwrap();
        let pm = Pm::at(dir.path()).unwrap();
        (dir, pm)
    }

    #[test]
    fn a_restricted_project_refuses_every_caller_shape() {
        let (_dir, pm) =
            pm_with("policy:\n  version: 1\n  visibility: private\n  members:\n  - users/mia\n");
        // The Owner keeps administrative access without an explicit grant.
        assert!(restricted(&pm, "demo", &actor("operator")).is_none());
        assert!(restricted(&pm, "demo", &actor("users/mia")).is_none());
        // Everyone else gets the answer an absent project gives, including
        // the reserved namespaces and an unrecognised spelling.
        for spelling in ["users/stranger", "agents/dev", "service/svc", "mystery"] {
            let refusal = restricted(&pm, "demo", &actor(spelling))
                .unwrap_or_else(|| panic!("{spelling} was admitted"));
            assert_eq!(refusal.status_code(), 404, "{spelling}");
        }
        // An absent project is not this guard's business — the verb keeps
        // its own unknown-project answer.
        assert!(restricted(&pm, "ghost", &actor("users/stranger")).is_none());
    }

    #[test]
    fn a_listing_omits_projects_the_caller_may_not_read() {
        let (_dir, pm) =
            pm_with("policy:\n  version: 1\n  visibility: private\n  members:\n  - users/mia\n");
        let open = pm.dir.join("open");
        std::fs::create_dir_all(&open).unwrap();
        std::fs::write(open.join("project.yaml"), "key: open\nprefix: O\n").unwrap();

        let stranger = readable_projects(&pm, &actor("users/stranger"));
        assert!(stranger.contains("open"));
        assert!(!stranger.contains("demo"), "restricted project listed");

        let owner = readable_projects(&pm, &actor("operator"));
        assert!(owner.contains("demo") && owner.contains("open"));

        // Fail closed: an unknown spelling reads nothing at all.
        assert!(readable_projects(&pm, &actor("mystery")).is_empty());
    }

    /// Recording access is the Owner's alone, and the record it writes is
    /// the one the read guard then consults.
    #[test]
    fn recording_a_policy_is_owner_only_and_advances_the_record() {
        let (_dir, pm) = pm_with("");
        let state = tempfile::tempdir().unwrap();
        let opts = ServeOpts::default();
        // A project with no recorded policy has no version on disk: the
        // first recorded policy is 1, and only a later write advances it.
        let policy_of = || {
            crate::issue::project::load(&pm.dir.join("demo").join("project.yaml"))
                .expect("project.yaml loads")
                .policy
        };
        let call = |actor: &CliActor, args: Value| {
            dispatch(
                Verb::ProjectPolicy,
                &args,
                actor,
                "",
                state.path(),
                &pm.dir,
                &opts,
            )
            .status_code()
        };

        // The gate reads the role the envelope was minted with, so every
        // member spelling is refused — including one that looks like a
        // grant a policy might carry, and the reserved namespaces.
        for spelling in ["users/mia", "agents/dev", "service/svc", "mystery"] {
            for args in [
                json!({"project": "demo"}),
                json!({"project": "demo", "visibility": "private"}),
                json!({"project": "demo", "grants": ["users/mia"]}),
            ] {
                assert_eq!(
                    call(&actor(spelling), args.clone()),
                    403,
                    "{spelling} {args}"
                );
            }
        }
        // …and none of those refusals reached the tracker: the project is
        // left with no recorded policy at all, not a widened one.
        assert!(policy_of().is_none(), "a refused call wrote a policy");

        // An agent minted with the owner role is not a human Owner.
        let owner_agent = actor_with_role("agents/lead", "owner");
        assert_eq!(
            call(
                &owner_agent,
                json!({"project": "demo", "visibility": "workspace-visible", "grants": ["users/mia"]})
            ),
            403
        );
        assert!(policy_of().is_none(), "an owner-role agent wrote a policy");

        // A traversal key is refused for a human Owner and writes nothing,
        // inside the tracker or beside it.
        assert_eq!(
            call(
                &actor_with_role("users/owner", "owner"),
                json!({"project": "../aos274-escape", "visibility": "workspace-visible", "grants": ["users/mia"]})
            ),
            400
        );
        assert!(!pm
            .dir
            .join("..")
            .join("aos274-escape")
            .join("project.yaml")
            .exists());
        assert!(policy_of().is_none(), "a bad key wrote a policy");

        // The Owner's write lands. A first policy is version 1, and a
        // second effective change advances it — the monotonic advance a
        // stale projection relies on to refuse the previous policy.
        let owner = actor_with_role("users/owner", "owner");
        assert_eq!(
            call(
                &owner,
                json!({"project": "demo", "visibility": "workspace-visible", "grants": ["users/mia"]})
            ),
            200
        );
        let first = policy_of().expect("policy recorded");
        assert_eq!(first.version, 1);
        assert_eq!(
            first.visibility,
            crate::issue::project::Visibility::WorkspaceVisible
        );
        assert_eq!(
            first
                .members
                .iter()
                .map(crate::issue::project::Grant::as_str)
                .collect::<Vec<_>>(),
            vec!["users/mia"]
        );

        assert_eq!(
            call(
                &owner,
                json!({"project": "demo", "visibility": "workspace-visible", "grants": ["users/mia", "users/dev"]})
            ),
            200
        );
        let second = policy_of().expect("policy recorded");
        assert_eq!(second.version, 2);
        assert_eq!(
            second
                .members
                .iter()
                .map(crate::issue::project::Grant::as_str)
                .collect::<Vec<_>>(),
            vec!["users/mia", "users/dev"]
        );

        // The isolation gate holds for the Owner too, and a refused
        // activation leaves the recorded policy exactly as it was.
        assert_eq!(
            call(
                &owner,
                json!({"project": "demo", "visibility": "private", "grants": []})
            ),
            400
        );
        assert_eq!(policy_of().expect("policy recorded"), second);
    }

    /// A malformed call is refused before any write.
    #[test]
    fn a_policy_call_naming_an_unknown_field_or_visibility_is_refused() {
        let (_dir, pm) = pm_with("");
        let state = tempfile::tempdir().unwrap();
        let opts = ServeOpts::default();
        let owner = actor_with_role("operator", "owner");
        for args in [
            json!({"project": "demo", "visibility": "public", "grants": []}),
            json!({"project": "demo", "visibility": "workspace-visible", "grants": [], "member": "users/mia"}),
            json!({"project": "demo", "visibility": "workspace-visible", "grants": ["everyone"]}),
            json!({"visibility": "workspace-visible", "grants": ["users/mia"]}),
        ] {
            let refusal = dispatch(
                Verb::ProjectPolicy,
                &args,
                &owner,
                "",
                state.path(),
                &pm.dir,
                &opts,
            );
            assert_eq!(refusal.status_code(), 400, "{args}");
        }
        // A denied call is not a partial one: the record is untouched.
        let project = crate::issue::project::load(&pm.dir.join("demo").join("project.yaml"))
            .expect("project.yaml loads");
        assert!(project.policy.is_none(), "a refused call wrote a policy");
    }

    /// A project with no recorded policy keeps today's access — it is not
    /// implicitly private (REQ-007).
    #[test]
    fn a_project_without_a_recorded_policy_stays_readable() {
        let (_dir, pm) = pm_with("");
        assert!(restricted(&pm, "demo", &actor("users/anyone")).is_none());
        assert!(readable_projects(&pm, &actor("users/anyone")).contains("demo"));
    }

    /// The board and the hosted CLI must name the same member, or a grant
    /// recorded on one surface would refuse the caller arriving through
    /// the other (AOS-274 adapter mapping).
    #[test]
    fn the_cli_keys_a_policy_on_the_spelling_the_board_stores() {
        // The store holds only the sanitized form — `Grant::parse` refuses
        // a raw subject — and that is what the board derives from a
        // session, so it is the only spelling a grant can carry.
        let (_dir, pm) = pm_with(
            "policy:\n  version: 1\n  visibility: private\n  members:\n  - users/auth0_usr_9_x\n",
        );
        let sanitized = CliActor {
            sub: "auth0|usr:9/x".into(),
            actor: "users/auth0|usr:9/x".into(),
            role_at_issue: "member".into(),
            principal_kind: "user".into(),
            scopes: vec!["cli.read".into(), "cli.write".into()],
            credential_id: "cred".into(),
            jti: "jti".into(),
            exp: i64::MAX,
            handle: cli_actor::handle_of("users/auth0|usr:9/x", "auth0|usr:9/x"),
        };
        // The granted member is admitted on the hosted CLI too, though the
        // envelope's `sub` is not itself handle-shaped.
        assert!(
            restricted(&pm, "demo", &sanitized).is_none(),
            "the granted member was refused through the hosted CLI"
        );
        assert!(readable_projects(&pm, &sanitized).contains("demo"));

        // …and it widens nothing: a grant naming someone else still
        // refuses this caller, on both the read guard and the listing.
        let (_other_dir, other) =
            pm_with("policy:\n  version: 1\n  visibility: private\n  members:\n  - users/mia\n");
        assert!(restricted(&other, "demo", &sanitized).is_some());
        assert!(readable_projects(&other, &sanitized).is_empty());
    }
}

#[cfg(test)]
mod cad1179_acceptance;

#[cfg(test)]
mod aos274_policy_http;

#[cfg(test)]
mod cad1500_acceptance;

#[cfg(test)]
mod cad1500_policy_acceptance;
