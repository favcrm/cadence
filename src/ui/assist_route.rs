//! CAD-1380/AOS-216: `POST /api/assist/<verb>` — the container end of the
//! member-assistant remote surface (AgenticOS's
//! `POST /__platform/assistant/call` forwards here). A distinct route family
//! from `/api/cli/` so the member-plane verb allowlist and single-use
//! replay memory stay separate from the shared remote-CLI surface.
//!
//! The bearer is the credential: `Authorization: Bearer wikienv_<env>`
//! where `<env>` is the compact assistant actor envelope the issuer minted
//! ([`crate::assistant_actor`]). The route re-verifies it independently —
//! signature against the platform JWKS, `iss`/`aud`/org/time pins — then
//! the per-verb `assist.*` scope. Cookie sessions never satisfy this route
//! and this bearer satisfies no other route: `handle` diverts
//! `/api/assist/*` here before any session or write-gate check, and only on
//! this board's own public host.
//!
//! Member authority stays member-bound end to end: the signed envelope
//! is forwarded verbatim to the daemon as `member_envelope`, which
//! re-verifies the Ed25519 signature and every claim pin against its own
//! `board-identity.json` + the platform JWKS (the member proof is
//! daemon-verifiable, not a relayed field the relay could widen). The
//! daemon then re-binds the envelope's `{org,sub,assistant}` to the
//! stored assistant row it owns — never to operator authority and never
//! to a request field.
//!
//! Fail closed: a missing, forged, expired, wrong-audience or wrong-org
//! envelope, an unlisted verb, or an insufficient scope is a 401/403 with
//! the contract's `error.code` and no side effect. Operator-only and
//! cli/wiki verbs are not in the table at all.

use std::path::Path;
use std::sync::Mutex;

use serde_json::{json, Value};
use tiny_http::{Header, Method, Request, Response, StatusCode};

use super::write_path::{self, header_value, parse_json, read_body, write_err, HttpResp};
use super::ServeOpts;
use crate::assistant_actor::{self, AssistActor, AssistVerb};
use crate::board_identity;
use crate::client;

/// One process-wide JWKS cache for this route family (independent of the
/// cli family's). Keys on the issuer — a stale or foreign-issuer entry
/// refetches.
static ASSIST_JWKS: Mutex<board_identity::JwksCache> = Mutex::new(board_identity::JwksCache::new());

/// Serializes `Spent::load`+`consume` for the assist family.
static ASSIST_SPENT_LOCK: Mutex<()> = Mutex::new(());

/// A refusal in the contract's `error.code` shape.
fn assist_fail(status: u16, code: &str, message: &str) -> HttpResp {
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

fn status_of(code: &str) -> u16 {
    match code {
        "capability_unavailable" => 503,
        _ => 401,
    }
}

/// POST body → bounded JSON object; identity fields are refused outright —
/// a request field can never name the member, the assistant or authority.
fn read_args(request: &mut Request) -> std::result::Result<Value, HttpResp> {
    let bytes = read_body(request, write_path::JSON_CAP)?;
    if bytes.iter().all(|b| b.is_ascii_whitespace()) {
        return Ok(json!({}));
    }
    let value: Value = parse_json(&bytes)?;
    if !value.is_object() {
        return Err(assist_fail(
            400,
            "invalid_request",
            "assist call arguments must be a JSON object",
        ));
    }
    for field in [
        "actor",
        "role",
        "org",
        "user",
        "as",
        "principal",
        "operator",
        "assistant",
        "assistant_id",
        "wiki_as",
        "member_as",
        "member_envelope",
    ] {
        if value.get(field).is_some() {
            return Err(assist_fail(
                400,
                "invalid_request",
                "assist call arguments cannot carry an identity field",
            ));
        }
    }
    Ok(value)
}

/// Peek the signed header's `kid` for the JWKS lookup only.
fn peek_kid(envelope: &str) -> Result<String, HttpResp> {
    assistant_actor::peek_kid(envelope).ok_or_else(|| {
        assist_fail(
            401,
            "bad_header",
            "envelope header is not the contract shape",
        )
    })
}

/// Verify an assistant envelope into an [`AssistActor`] or answer the
/// refusal. This is the single verification the `/api/assist` bearer and
/// the CAD-1485 member-chat surface both run: signature against the
/// platform JWKS, `iss`/`aud`/org/time pins — identical pins either way.
/// The caller supplies the raw signed envelope; how it was obtained
/// (Authorization bearer vs a server-minted board-authorize answer) is
/// the caller's concern.
pub(super) fn verify_envelope(
    envelope: &str,
    public: &crate::ui::PublicBoard,
) -> Result<AssistActor, HttpResp> {
    let config = board_identity::Config {
        host: public.host.clone(),
        issuer: public.issuer.clone(),
        company: public.company.clone(),
    };
    let now = crate::issue::time::now_epoch();
    let kid = peek_kid(envelope)?;
    let key = {
        let mut cache = ASSIST_JWKS.lock().unwrap_or_else(|e| e.into_inner());
        match cache.key(&config.issuer, &kid, now) {
            Ok(k) => k,
            Err(e) => {
                let (code, status) = match e.code() {
                    Some("assertion_invalid") => ("assertion_invalid", 401),
                    _ => ("capability_unavailable", 503),
                };
                return Err(assist_fail(status, code, &e.to_string()));
            }
        }
    };
    assistant_actor::verify(envelope, &config, &key, now)
        .map_err(|r| assist_fail(status_of(r.code), r.code, &r.message))
}

/// Verify the bearer into a [`AssistActor`] or answer the refusal.
fn assist_caller(
    request: &Request,
    public: &crate::ui::PublicBoard,
) -> Result<AssistActor, HttpResp> {
    let bearer = header_value(request, "Authorization");
    let envelope = match assistant_actor::envelope_from_bearer(bearer.as_deref()) {
        Ok(e) => e,
        Err(r) => return Err(assist_fail(status_of(r.code), r.code, &r.message)),
    };
    verify_envelope(&envelope, public)
}

/// `POST /api/assist/<tail>` — diverted on the public host ahead of the
/// cookie-session gates, exactly as `/api/cli/`. One path segment names the
/// verb; a nested or unknown one is refused before body or credential.
pub(super) fn post(
    request: &mut Request,
    method: &Method,
    tail: &str,
    state_dir: &Path,
    pm_dir: &Path,
    opts: &ServeOpts,
) -> HttpResp {
    if *method != Method::Post {
        return assist_fail(405, "invalid_request", "assist calls are POST");
    }
    if tail.is_empty() || tail.contains('/') {
        return assist_fail(404, "not_found", "no such assist route");
    }
    let Some(verb) = AssistVerb::of(tail) else {
        return assist_fail(403, "assist_verb_refused", "the verb is not allowlisted");
    };
    let Some(public) = opts.public.as_ref() else {
        return assist_fail(
            503,
            "capability_unavailable",
            "assistant authority is not configured",
        );
    };
    let caller = match assist_caller(request, public) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    if !assistant_actor::scope_covers(&caller, verb.scope()) {
        return assist_fail(
            403,
            "insufficient_scope",
            "the envelope's granted scopes do not cover this verb",
        );
    }
    let args = match read_args(request) {
        Ok(a) => a,
        Err(resp) => return resp,
    };
    // Recover the raw signed envelope to forward — the daemon re-verifies
    // it, so member proof travels signed, never as a bare relayed field.
    let bearer = header_value(request, "Authorization");
    let envelope = match assistant_actor::envelope_from_bearer(bearer.as_deref()) {
        Ok(e) => e,
        Err(r) => return assist_fail(status_of(r.code), r.code, &r.message),
    };
    run_member_verb(verb, &args, &caller, &envelope, state_dir, pm_dir, opts)
}

/// Consume the envelope's single-use jti and run the verified member
/// verb. This is the ONE defined consumption point (kickoff §3) — the
/// `/api/assist` bearer path and the CAD-1485 member-chat path both go
/// through here, so either way the jti is spent exactly once and the raw
/// signed envelope is forwarded to the daemon for cryptographic
/// re-verification (the daemon does NOT re-consume the jti). Callers
/// must have already run [`verify_envelope`] and, on the bearer path,
/// [`assistant_actor::scope_covers`].
pub(super) fn run_member_verb(
    verb: AssistVerb,
    args: &Value,
    caller: &AssistActor,
    envelope: &str,
    state_dir: &Path,
    pm_dir: &Path,
    opts: &ServeOpts,
) -> HttpResp {
    let spent = {
        let _guard = ASSIST_SPENT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut set = assistant_actor::Spent::load(state_dir);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or_default();
        set.consume(&caller.jti, caller.exp, now)
    };
    if !spent {
        return assist_fail(
            401,
            "assertion_replayed",
            "the envelope's jti was already consumed",
        );
    }
    dispatch(verb, args, caller, envelope, state_dir, pm_dir, opts)
}

fn arg_str<'a>(args: &'a Value, key: &str) -> Result<Option<&'a str>, HttpResp> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.as_str())),
        _ => Err(assist_fail(
            400,
            "invalid_request",
            &format!("assist argument '{key}' must be a string"),
        )),
    }
}

/// Relay the member-bound call to the daemon on a dedicated `member_*`
/// method. The member proof rides as the SIGNED envelope itself
/// (`member_envelope`) — the daemon re-verifies the Ed25519 signature and
/// every claim pin against its own `board-identity.json` + JWKS, so the
/// member identity is daemon-verifiable and a relayed `member_as`-style
/// field is never the proof. The relay channel is a daemon-proven
/// operator/board connection, but the daemon never trusts the connection
/// to name the member — only the signature does.
fn member_rpc(
    state_dir: &Path,
    method: &str,
    _actor: &AssistActor,
    envelope: &str,
    args: &Value,
) -> HttpResp {
    let mut params = args.clone();
    params["member_envelope"] = json!(envelope);
    match client::rpc(state_dir, method, params) {
        Ok(v) => json_ok(v),
        // A `member_*` daemon refusal is an authorization failure — the
        // member lacks the scope, the binding, or the job ACL — so the
        // HTTP surface answers 403 (forbidden), not the generic 400
        // `write_err` maps `Rejected` to. Member-boundary authorization
        // is never a malformed-request 400: malformed args are refused
        // earlier at `read_args`/the verb's own checks.
        Err(crate::error::Error::Rejected(m)) => assist_fail(403, "forbidden", &m),
        Err(e) => write_err(&e),
    }
}

/// CAD-1420: invoke the dedicated `platform_assistant_provision` daemon
/// RPC on the same proven relay before the member verb. Params are the
/// closed pair — the signed `member_envelope` (daemon re-verifies it) and
/// the server-derived `operation`; no caller field is forwarded. A refusal
/// maps to the same 403 contract as a member_* refusal; a `provisioned:`
/// false answer (existing active binding) is the idempotent no-op.
fn provision(state_dir: &Path, operation: &str, envelope: &str) -> Result<(), HttpResp> {
    let params = json!({"member_envelope": envelope, "operation": operation});
    match client::rpc(state_dir, "platform_assistant_provision", params) {
        Ok(_) => Ok(()),
        Err(crate::error::Error::Rejected(m)) => Err(assist_fail(403, "forbidden", &m)),
        Err(e) => Err(write_err(&e)),
    }
}

fn json_ok(v: Value) -> HttpResp {
    let mut resp = Response::from_data(serde_json::to_vec(&v).unwrap_or_default())
        .with_status_code(StatusCode(200));
    resp.add_header(Header::from_bytes("Content-Type", "application/json").unwrap());
    resp.add_header(Header::from_bytes("Cache-Control", "no-store").unwrap());
    resp
}

/// The verb → the member-bound daemon call. Every verb carries the
/// member's verified identity via `member_rpc`; none reach a global RPC
/// or a request-named authority.
fn dispatch(
    verb: AssistVerb,
    args: &Value,
    actor: &AssistActor,
    envelope: &str,
    state_dir: &Path,
    _pm_dir: &Path,
    _opts: &ServeOpts,
) -> HttpResp {
    match verb {
        // CAD-1420: the three assist.* member verbs first re-validate their
        // argument shape (unknown fields and a negative history cursor
        // refuse here, before any projection can create a binding from
        // malformed input), then invoke the dedicated platform-projection
        // RPC on the same proven relay, then run the ordinary member_* RPC.
        // The member verb's own binding check still runs — a member with no
        // binding can never reach member_caller's ordinary path; only the
        // verified-envelope + live-authority projection mints one.
        AssistVerb::AssistState => {
            // The daemon accepts NO fields for state — any argument key
            // refuses before the projection can create a binding.
            if let Some(field) = args.as_object().and_then(|o| o.keys().next()) {
                return assist_fail(
                    400,
                    "invalid_request",
                    &format!("assist_state takes no arguments; field '{field}' is not accepted"),
                );
            }
            if let Err(resp) = provision(state_dir, "assist_state", envelope) {
                return resp;
            }
            member_rpc(state_dir, "member_assist_state", actor, envelope, args)
        }
        AssistVerb::AssistHistory => {
            // The daemon's own allowed fields: after,before,tail,limit. An
            // unknown argument refuses BEFORE the projection so a malformed
            // call never creates a binding. A negative `after` cursor is the
            // existing daemon check, mirrored here so the refusal precedes
            // any binding write.
            if let Some(field) = args.as_object().and_then(|o| {
                o.keys()
                    .find(|k| !["after", "before", "tail", "limit"].contains(&k.as_str()))
            }) {
                return assist_fail(
                    400,
                    "invalid_request",
                    &format!(
                        "assist_history takes after, before, tail, limit only; \
                         field '{field}' is not accepted"
                    ),
                );
            }
            for field in ["after", "before", "tail", "limit"] {
                match args.get(field) {
                    None | Some(Value::Null) => {}
                    Some(Value::Number(n))
                        if field == "after" || field == "before" || field == "limit" =>
                    {
                        if field == "after" && n.as_i64().map(|v| v < 0).unwrap_or(false) {
                            return assist_fail(
                                400,
                                "invalid_request",
                                "history cursor must be nonnegative",
                            );
                        }
                    }
                    Some(Value::Bool(_)) if field == "tail" => {}
                    Some(_) => {
                        return assist_fail(
                            400,
                            "invalid_request",
                            &format!("assist argument '{field}' has an invalid shape"),
                        )
                    }
                }
            }
            if let Err(resp) = provision(state_dir, "assist_history", envelope) {
                return resp;
            }
            member_rpc(state_dir, "member_assist_history", actor, envelope, args)
        }
        AssistVerb::AssistSend => {
            // The daemon accepts `text` and the optional idempotent `message`
            // id — any other argument key refuses before the projection can
            // create a binding. `message` is validated here (identifier
            // grammar, never a `sys-` daemon id). The shared verb path
            // (`run_member_verb`) spends the envelope's jti BEFORE calling
            // `dispatch`, so a malformed send is refused before any binding
            // write or effect but costs that jti — the same as every other
            // field check on this path.
            if let Some(field) = args.as_object().and_then(|o| {
                o.keys()
                    .find(|k| !["text", "message"].contains(&k.as_str()))
            }) {
                return assist_fail(
                    400,
                    "invalid_request",
                    &format!("assist_send takes text and optional message; field '{field}' is not accepted"),
                );
            }
            match arg_str(args, "text") {
                Ok(Some(text)) if !text.trim().is_empty() => {
                    match arg_str(args, "message") {
                        Err(resp) => return resp,
                        Ok(Some(mid)) => {
                            if crate::proto::identifier(mid, "Message ID").is_err()
                                || mid.starts_with(crate::proto::DAEMON_MESSAGE_PREFIX)
                            {
                                return assist_fail(
                                    400,
                                    "invalid_request",
                                    "assist argument 'message' is not a usable message id",
                                );
                            }
                        }
                        Ok(None) => {}
                    }
                    if let Err(resp) = provision(state_dir, "assist_send", envelope) {
                        return resp;
                    }
                    member_rpc(state_dir, "member_assist_send", actor, envelope, args)
                }
                _ => assist_fail(400, "invalid_request", "assist_send needs arguments.text"),
            }
        }
        AssistVerb::JobRequest => match (arg_str(args, "title"), arg_str(args, "request_key")) {
            (Ok(Some(_)), Ok(Some(_))) => {
                member_rpc(state_dir, "member_job_request", actor, envelope, args)
            }
            _ => assist_fail(
                400,
                "invalid_request",
                "job_request needs arguments.title and arguments.request_key",
            ),
        },
        AssistVerb::JobList => member_rpc(state_dir, "member_job_list", actor, envelope, args),
        AssistVerb::JobEvidence => match arg_str(args, "job") {
            Ok(Some(_)) => member_rpc(state_dir, "member_job_evidence", actor, envelope, args),
            _ => assist_fail(400, "invalid_request", "job_evidence needs arguments.job"),
        },
        AssistVerb::JobEvents => match arg_str(args, "job") {
            Ok(Some(_)) => member_rpc(state_dir, "member_job_events", actor, envelope, args),
            _ => assist_fail(400, "invalid_request", "job_events needs arguments.job"),
        },
        AssistVerb::JobSubscribe => match arg_str(args, "job") {
            Ok(Some(_)) => member_rpc(state_dir, "member_job_subscribe", actor, envelope, args),
            _ => assist_fail(400, "invalid_request", "job_subscribe needs arguments.job"),
        },
        AssistVerb::JobShare => match arg_str(args, "job") {
            Ok(Some(_)) => member_rpc(state_dir, "member_job_share", actor, envelope, args),
            _ => assist_fail(400, "invalid_request", "job_share needs arguments.job"),
        },
        AssistVerb::JobReconcile => {
            match (
                arg_str(args, "job"),
                arg_str(args, "expected_authority_rev"),
            ) {
                (Ok(Some(_)), Ok(Some(_))) => {
                    member_rpc(state_dir, "member_job_reconcile", actor, envelope, args)
                }
                _ => assist_fail(
                    400,
                    "invalid_request",
                    "job_reconcile needs arguments.job and arguments.expected_authority_rev",
                ),
            }
        }
    }
}
