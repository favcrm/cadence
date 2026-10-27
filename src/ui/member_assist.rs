//! CAD-1485: `POST /api/member-assistant/{state,history,send}` — the
//! board-side surface a signed-in member's own browser chat talks to.
//!
//! This is the cookie-facing twin of `/api/assist/<verb>`: the browser
//! carries only its ordinary `__Host-aos-board-session` cookie plus the
//! write guards (exact JSON type, `X-Cadence-Board`, same-origin
//! Origin/Sec-Fetch-Site) and the board's one write admission
//! (`operator::board_caller(.., write=true)` — present exact own-Origin,
//! no agent/unattributable peer, a live named session; member AND owner
//! are admitted, an attributed agent presenting a session is refused as
//! stolen). The member's assistant authority never enters
//! the browser: the route resolves the live public session's verified
//! `(sub, org)` server-side, asks AgenticOS's closed issuer endpoint for
//! a fresh signed envelope bound to THAT session's original platform
//! binding (`POST {issuer}/__platform/assistant/board-authorize` on the
//! configured pinned issuer origin — never a redirect, never a host
//! derived from the audience — `Authorization: BoardSession <cookie>`),
//! then relays the existing
//! `/api/assist` dispatch — envelope verification, single-use jti and
//! the daemon's own re-verification all unchanged.
//!
//! Fail closed everywhere: no session, no configured public identity, a
//! session whose `sub`/`org` does not match this board's company, a
//! refused or unreachable issuer, a missing/malformed envelope, or a
//! member verb refusal all answer non-2xx and change nothing. No path
//! here reaches `operator`, `Master`, `send_as`, or a caller-named
//! member — the member is only ever the session's own.

use std::path::Path;

use serde_json::{json, Value};
use tiny_http::{Header, Method, Request, Response, StatusCode};

use super::assist_route;
use super::operator;
use super::write_path::{guard_fail, parse_json, read_body, write_guard, HttpResp, JSON_CAP};
use super::ServeOpts;
use crate::assistant_actor::AssistVerb;
use crate::operator_auth::Origin;

/// A refusal in the contract's `error.code` shape (same envelope the
/// assist family answers).
fn member_fail(status: u16, code: &str, message: &str) -> HttpResp {
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

/// The closed verbs this surface accepts — the path's single segment.
#[derive(Clone, Copy, PartialEq, Eq)]
enum MemberVerb {
    State,
    History,
    Send,
}

impl MemberVerb {
    fn of(segment: &str) -> Option<MemberVerb> {
        match segment {
            "state" => Some(MemberVerb::State),
            "history" => Some(MemberVerb::History),
            "send" => Some(MemberVerb::Send),
            _ => None,
        }
    }

    /// The `AssistVerb` the daemon serves — the shared `run_member_verb`
    /// dispatch and the scope check both read this.
    fn assist_verb(self) -> AssistVerb {
        match self {
            MemberVerb::State => AssistVerb::AssistState,
            MemberVerb::History => AssistVerb::AssistHistory,
            MemberVerb::Send => AssistVerb::AssistSend,
        }
    }

    /// The verb the issuer's `board-authorize` call names — the contract's
    /// wire field, identical to this route's own path segment.
    fn verb_segment(self) -> &'static str {
        match self {
            MemberVerb::State => "state",
            MemberVerb::History => "history",
            MemberVerb::Send => "send",
        }
    }
}

/// What the request body may carry per verb — the daemon's own allowed
/// fields, minus `member_envelope` (server-minted, never browser-held).
/// Anything else, and every identity field, refuses at the boundary.
fn read_member_args(request: &mut Request, verb: MemberVerb) -> Result<Value, HttpResp> {
    let bytes = read_body(request, JSON_CAP)?;
    if bytes.iter().all(|b| b.is_ascii_whitespace()) {
        return Ok(json!({}));
    }
    let value: Value = parse_json(&bytes)?;
    let Some(obj) = value.as_object() else {
        return Err(member_fail(
            400,
            "invalid_request",
            "member assistant arguments must be a JSON object",
        ));
    };
    // Browser fields are chat arguments only — never identity, envelope,
    // or a member selector. The session proves who the member is.
    const IDENTITY: &[&str] = &[
        "actor",
        "role",
        "org",
        "organization_id",
        "user",
        "sub",
        "subject",
        "as",
        "principal",
        "operator",
        "assistant",
        "assistant_id",
        "member_as",
        "member_envelope",
        "conversation",
        "conversation_id",
        "alias",
        "install_id",
        "app",
        "issuer",
    ];
    let allowed: &[&str] = match verb {
        MemberVerb::State => &[],
        MemberVerb::History => &["after", "before", "tail", "limit"],
        MemberVerb::Send => &["text", "message"],
    };
    for key in obj.keys() {
        if IDENTITY.contains(&key.as_str()) {
            return Err(member_fail(
                400,
                "identity_field_refused",
                &format!("'{key}' is not accepted: identity comes from the sign-in"),
            ));
        }
        if !allowed.contains(&key.as_str()) {
            return Err(member_fail(
                400,
                "invalid_request",
                &format!("'{key}' is not a field of this request"),
            ));
        }
    }
    Ok(value)
}

/// The verified public session this request's cookie names — the ONLY
/// member identity this route accepts. `Ok(None)` means no live public
/// session: sign in again. A non-public origin, a session without a
/// named user, or a user/org that is not this board's company is a
/// refusal — never a fallback to operator or a guessed member.
fn member_session(
    request: &Request,
    state_dir: &Path,
    opts: &ServeOpts,
) -> Result<(crate::operator_auth::BoardUser, String), HttpResp> {
    let Some(public) = opts.public.as_ref() else {
        return Err(member_fail(
            503,
            "capability_unavailable",
            "this board has no platform sign-in configured",
        ));
    };
    let Some(session) = operator::public_session(request, state_dir, opts)? else {
        return Err(member_fail(
            401,
            "board_session_required",
            "a board session is required — sign in through your company's workspace",
        ));
    };
    let user: crate::operator_auth::BoardUser =
        match serde_json::from_value(session["user"].clone()) {
            Ok(u) => u,
            Err(_) => {
                return Err(member_fail(
                    401,
                    "board_session_required",
                    "this board session names no verified member",
                ))
            }
        };
    // The session's verified tenancy must be THIS board's company — a
    // public session minted for another org's board is refused, never
    // coerced. The daemon re-checks the envelope's org pin regardless;
    // this stops a foreign session early, before any issuer call.
    if let Some(scope) = session.get("scope") {
        if scope["organization_id"].as_str() != Some(public.company.as_str()) {
            return Err(member_fail(
                403,
                "tenancy_mismatch",
                "this sign-in does not match this board",
            ));
        }
    }
    // The cookie value is possession proof for the server-held binding —
    // passed to the issuer as `BoardSession`, never logged or returned.
    let token = match operator::session_token_for(request, opts, Origin::Public) {
        Some(t) => t,
        None => {
            return Err(member_fail(
                401,
                "board_session_required",
                "a board session is required — sign in through your company's workspace",
            ))
        }
    };
    Ok((user, token))
}

/// Ask the AOS issuer for a fresh assistant envelope bound to THIS
/// session's original platform binding. The cookie is possession proof;
/// the issuer resolves the server-held record and mints under the
/// original session's subject/org — this call carries no `sub`, `org`,
/// `actor` or `audience` the caller could steer.
fn issue_envelope(
    token: &str,
    verb: MemberVerb,
    public: &super::PublicBoard,
) -> Result<String, HttpResp> {
    // The authority endpoint rides the CONFIGURED pinned issuer origin,
    // never a destination derived from the public audience host: the
    // audience stays `public.host` (the board's own name the envelope is
    // minted for), while the wire goes to `public.issuer` — the same pin
    // `verify_envelope`/JWKS trust. The exact contract path is appended
    // unchanged.
    let url = format!(
        "{}/__platform/assistant/board-authorize",
        public.issuer.trim_end_matches('/')
    );
    // `issuer_transport::agent` keeps the existing constructor's test
    // TLS/resolver behavior; production adds no proxy and — because the
    // request carries the session credential — explicit
    // `max_redirects(0)` so a 3xx is a refusal, never a followed hop to a
    // different destination (ureq's production default would follow 10).
    // `http_status_as_error(false)` keeps the non-2xx body readable.
    let agent = crate::issuer_transport::agent(
        ureq::Agent::config_builder()
            .timeout_global(Some(std::time::Duration::from_secs(10)))
            .http_status_as_error(false)
            .max_redirects(0),
    );
    let body = json!({
        "version": "hosted-cadence-board-assistant.v1",
        "verb": verb.verb_segment(),
    });
    let mut response = agent
        .post(&url)
        .header("authorization", &format!("BoardSession {token}"))
        .send_json(&body)
        .map_err(|_| {
            member_fail(
                503,
                "authority_unavailable",
                "member authority could not be reached — sign in again or retry",
            )
        })?;
    let status = response.status().as_u16();
    if status != 200 {
        // Any non-2xx is a refusal — expired/revoked/mismatched binding,
        // missing session, no member grant. No fallback exists.
        return Err(member_fail(
            if status == 401 || status == 403 {
                status
            } else {
                503
            },
            "authority_refused",
            "member authority is not available for this session — sign in again",
        ));
    }
    use std::io::Read;
    let mut bytes = Vec::new();
    if response
        .body_mut()
        .as_reader()
        .take(64 * 1024)
        .read_to_end(&mut bytes)
        .is_err()
    {
        return Err(member_fail(
            503,
            "authority_unavailable",
            "member authority response could not be read",
        ));
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| {
        member_fail(
            503,
            "authority_unavailable",
            "member authority answered an unreadable envelope",
        )
    })?;
    if value.get("version").and_then(Value::as_str) != Some("hosted-cadence-board-assistant.v1") {
        return Err(member_fail(
            503,
            "authority_unavailable",
            "member authority answered an unexpected shape",
        ));
    }
    let envelope = value
        .get("envelope")
        .and_then(Value::as_str)
        .filter(|e| !e.is_empty() && e.len() <= 8192)
        .ok_or_else(|| {
            member_fail(
                503,
                "authority_unavailable",
                "member authority answered no usable envelope",
            )
        })?;
    Ok(envelope.to_string())
}

/// `POST /api/member-assistant/<verb>` — diverted on the public host
/// ahead of the cookie gates, exactly as the human-threads family.
/// The write guards, the verified session, the closed body whitelist
/// and the server-held authority all run before the existing
/// `/api/assist` dispatch sees the call — the daemon's own
/// verification (signature, pins, binding, live-authority pull) is
/// unchanged.
pub(super) fn post(
    request: &mut Request,
    method: &Method,
    tail: &str,
    state_dir: &Path,
    pm_dir: &Path,
    opts: &ServeOpts,
) -> HttpResp {
    if *method != Method::Post {
        return member_fail(405, "invalid_request", "member assistant calls are POST");
    }
    if tail.is_empty() || tail.contains('/') {
        return member_fail(404, "not_found", "no such member-assistant route");
    }
    let Some(verb) = MemberVerb::of(tail) else {
        return member_fail(404, "not_found", "no such member-assistant route");
    };
    // The JSON/CSRF body guards run first, then the board's one write
    // admission — `board_caller(.., write=true)`: a cookie-bearing request
    // must carry a PRESENT Origin equal to this request's own public
    // origin, an attributed agent or agent-uid peer presenting a live
    // session is refused as stolen (and the session revoked), an
    // unattributable peer is refused, and the caller must be a live
    // named public session (member OR owner) or an operator. Only a
    // `Named` caller reaches the member surface; `Operator` is the
    // loopback/tailnet owner on the same rule and is admitted, while an
    // `Agent` can never reach a public-origin cookie route as its own
    // member and is refused by the session-shape check below.
    if opts.read_only {
        return guard_fail("read_only", "board is read-only — writes are disabled");
    }
    if let Err(resp) = write_guard(request, "application/json", opts) {
        return resp;
    }
    let caller = match operator::board_caller(request, state_dir, opts, true, None) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    let (user, token) = match member_session(request, state_dir, opts) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    // `board_caller` already refused an agent/foreign/unproven peer and
    // a stolen session outright; a bare `Agent` caller (an agent's own
    // write with no session) carries no public cookie — `member_session`
    // refuses it below. This match only documents the admitted shapes.
    match &caller {
        operator::Caller::Named(_) | operator::Caller::Operator(_) => {}
        operator::Caller::Agent(_) => {
            return member_fail(
                403,
                "agent_caller",
                "member assistant calls come from the member's own signed-in browser",
            );
        }
    }
    let args = match read_member_args(request, verb) {
        Ok(a) => a,
        Err(resp) => return resp,
    };
    let public = match opts.public.as_ref() {
        Some(p) => p,
        None => {
            return member_fail(
                503,
                "capability_unavailable",
                "this board has no platform sign-in configured",
            )
        }
    };
    let envelope = match issue_envelope(&token, verb, public) {
        Ok(e) => e,
        Err(resp) => return resp,
    };
    // The fresh envelope rides the existing `/api/assist` dispatch —
    // same verification, same single-use jti, same daemon re-verify,
    // same per-verb argument checks. The browser's `message` id (when
    // present) is the idempotent retry key the daemon already honours.
    dispatch_member(verb, &args, &user, &envelope, state_dir, pm_dir, opts)
}

/// Hand the verified call to the existing member dispatch: mint the
/// bearer and run the same `assist_route` verb handler the AOS relay
/// reaches. Reuses `assist_route`'s verification+spent+provision+RPC
/// path so this surface is never a second, weaker member gate.
fn dispatch_member(
    verb: MemberVerb,
    args: &Value,
    user: &crate::operator_auth::BoardUser,
    envelope: &str,
    state_dir: &Path,
    pm_dir: &Path,
    opts: &ServeOpts,
) -> HttpResp {
    let Some(public) = opts.public.as_ref() else {
        return member_fail(
            503,
            "capability_unavailable",
            "assistant authority is not configured",
        );
    };
    // Re-verify the minted envelope locally before spending its jti —
    // the same `assist_caller` pins, against this board's own config.
    let caller = match assist_route::verify_envelope(envelope, public) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    // The verified session's member must be the envelope's member — a
    // server-bound guard so a binding minted for another subject can
    // never satisfy this request.
    if caller.sub != user.sub || caller.org != public.company {
        return member_fail(
            403,
            "member_mismatch",
            "the assistant authority does not name this session's member",
        );
    }
    // The scope the envelope carries must cover the verb — the same
    // `scope_covers` the bearer path enforces. A member-chat call never
    // widens the envelope's grant.
    if !crate::assistant_actor::scope_covers(&caller, verb.assist_verb().scope()) {
        return member_fail(
            403,
            "insufficient_scope",
            "the envelope's granted scopes do not cover this verb",
        );
    }
    assist_route::run_member_verb(
        verb.assist_verb(),
        args,
        &caller,
        envelope,
        state_dir,
        pm_dir,
        opts,
    )
}
