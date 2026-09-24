//! Who the board trusts as the operator (CAD-313, CAD-428; ADR 0004
//! phase 1). Exactly one thing makes a board request the operator's: a
//! live **operator session** — an HttpOnly cookie the daemon issued in
//! exchange for a single-use `cadence ui login` link — presented on the
//! origin it was issued for, by a process tied to no agent. Nothing
//! else does: not a loopback peer, not a relay (nginx, `socat`, an
//! agent's own proxy — the board sees the relay, never who is behind
//! it), not a Host, not a `Tailscale-User-*` or `X-Forwarded-*` header,
//! not the absence of a pane tie.
//!
//! [`board_caller`] is the one rule, as a table ([`decide`]):
//!
//! | session | the TCP peer | caller |
//! |---|---|---|
//! | valid | tied to an agent (pane or managed endpoint) | refused `session_from_agent`; the session is revoked |
//! | valid | the proven `tailscale serve` proxy (CAD-336) | the operator, as `<login> (tailscale)` (no login: refused) |
//! | valid | tied to no agent, or unattributable with its client socket alive and another uid's (sshd, tailscaled) | the operator, `operator (ui)` |
//! | valid | unattributable otherwise (socket already closed, or ours with no visible owner) | refused `caller_identity` |
//! | none/invalid | tied to an agent | that agent (agent-allowed routes only) |
//! | none/invalid | tied to no agent, a relay, or the proxy | refused `operator_session_required` |
//! | none/invalid | unattributable | refused `caller_identity` |
//!
//! A session is bound to its origin: `loopback` — on this board's own
//! name, `cadence-<port>.localhost:<port>` ([`board_host`]), and no
//! other Host, so the cookie is never sent to another port's server — or
//! `tailnet` (a request the tailnet proof passes). Any other request has
//! no origin, so no session. A write that carries a
//! session cookie must also carry `Origin`, and it must be this very
//! request's own scheme and Host — a cookie replayed from another
//! origin, or by a client that sends none, is refused.
//!
//! [`WRITE_ROUTES`] classifies every write, and [`admit`] enforces the
//! class before any handler runs. Operator-only routes additionally run
//! positive process proof on the HTTP peer (`home::prove_operator_peer`):
//! the board is never less strict than the daemon verb it relays.

use serde::Deserialize;
use serde_json::{json, Value};
use tiny_http::{Header, Request, Response, StatusCode};

use super::{
    agent_roots, coded_response, guard_fail, header_value, parse_json, proxied_actor, read_body,
    tailnet_proxy, write_guard, HttpResp, ServeOpts, UI_ACTOR,
};
use crate::client;
use crate::operator_auth::Origin;

/// What a write route requires (ADR 0004 §5.9).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RouteClass {
    /// An operator session, or a caller attributed to exactly one agent
    /// (which writes as that agent, never as `operator`).
    AgentAllowed,
    /// An operator session (plus process proof on the peer).
    OperatorOnly,
    /// The login exchange and logout: the nonce or the session itself is
    /// the credential.
    Session,
    /// Never over HTTP (memory curation needs a native agent endpoint).
    Refused,
}

/// One write route: method, path pattern (`*` is one non-empty
/// segment), class.
#[derive(Clone, Copy, Debug)]
pub struct WriteRoute {
    pub method: &'static str,
    pub pattern: &'static str,
    pub class: RouteClass,
}

const fn route(method: &'static str, pattern: &'static str, class: RouteClass) -> WriteRoute {
    WriteRoute {
        method,
        pattern,
        class,
    }
}

/// Every write route the board answers, classified in one place — and
/// ENFORCED from here: `write_route` runs [`admit`] on the class before
/// any handler, and the handlers themselves check no caller. A write
/// that matches no entry is [`RouteClass::OperatorOnly`]
/// ([`route_class`]) — a new route fails closed until it is listed.
pub const WRITE_ROUTES: &[WriteRoute] = &[
    route("POST", "/api/issues", RouteClass::AgentAllowed),
    route("PATCH", "/api/issues/*", RouteClass::AgentAllowed),
    route("POST", "/api/issues/*/links", RouteClass::AgentAllowed),
    route("DELETE", "/api/issues/*/links", RouteClass::AgentAllowed),
    route("POST", "/api/issues/*/refs", RouteClass::AgentAllowed),
    route("POST", "/api/issues/*/comments", RouteClass::AgentAllowed),
    route("POST", "/api/issues/*/artifacts", RouteClass::AgentAllowed),
    route(
        "POST",
        "/api/monitors/*/alerts/*/ack",
        RouteClass::AgentAllowed,
    ),
    route("POST", "/api/issues/*/answers", RouteClass::OperatorOnly),
    route("POST", "/api/plans/*/approve", RouteClass::OperatorOnly),
    route("POST", "/api/plans/*/reject", RouteClass::OperatorOnly),
    route("POST", "/api/delivery/*/merge", RouteClass::OperatorOnly),
    route("POST", "/api/delivery/*/decline", RouteClass::OperatorOnly),
    route(
        "POST",
        "/api/settings/model-defaults",
        RouteClass::OperatorOnly,
    ),
    route("POST", "/api/threads/*/messages", RouteClass::OperatorOnly),
    route("POST", "/api/epics/*/stage", RouteClass::OperatorOnly),
    route("POST", "/api/memories/*/*/accept", RouteClass::Refused),
    route("POST", "/api/memories/*/*/reject", RouteClass::Refused),
    route("POST", "/api/session", RouteClass::Session),
    route("POST", "/api/session/logout", RouteClass::Session),
];

fn matches(pattern: &str, path: &str) -> bool {
    let mut want = pattern.split('/');
    let mut got = path.split('/');
    loop {
        match (want.next(), got.next()) {
            (None, None) => return true,
            (Some("*"), Some(seg)) if !seg.is_empty() => {}
            (Some(w), Some(g)) if w == g => {}
            _ => return false,
        }
    }
}

/// The class of a write — [`RouteClass::OperatorOnly`] when unlisted.
pub fn route_class(method: &str, path: &str) -> RouteClass {
    WRITE_ROUTES
        .iter()
        .find(|r| r.method == method && matches(r.pattern, path))
        .map(|r| r.class)
        .unwrap_or(RouteClass::OperatorOnly)
}

/// Admit a write by its class ([`route_class`]) before any handler
/// runs — the one place the board enforces who may write:
///
/// - `OperatorOnly` (and every unlisted write): read-only off, the
///   cross-site guards, an operator session ([`board_caller`]) — an
///   agent is refused `operator_only` — then positive process proof on
///   the HTTP peer (`home::prove_operator_peer`), because the handler
///   relays over the board's own daemon connection;
/// - `AgentAllowed`: read-only off, the guards, and [`board_caller`]:
///   the operator's session, or the one agent the peer is tied to;
/// - `Session` and `Refused`: `None` — the handler owns its credential
///   (the nonce, the session) or its refusal (memory curation).
pub(super) fn admit(
    request: &Request,
    method: &str,
    path: &str,
    state_dir: &std::path::Path,
    opts: &ServeOpts,
) -> Result<Option<Caller>, HttpResp> {
    let class = route_class(method, path);
    if matches!(class, RouteClass::Session | RouteClass::Refused) {
        return Ok(None);
    }
    if opts.read_only {
        return Err(guard_fail(
            "read_only",
            "board is read-only — writes are disabled",
        ));
    }
    let ct = if path.ends_with("/artifacts") {
        "application/octet-stream"
    } else {
        "application/json"
    };
    write_guard(request, ct, opts)?;
    let caller = board_caller(request, state_dir, opts, true)?;
    if class == RouteClass::OperatorOnly {
        match &caller {
            Caller::Agent(alias) => {
                return Err(guard_fail(
                    "operator_only",
                    &format!(
                        "{method} {path} is the operator's decision — this request comes \
                         from agent '{alias}'; decide from the operator's browser"
                    ),
                ))
            }
            Caller::Operator(_) => super::home::prove_operator_peer(
                request,
                state_dir,
                opts,
                &format!("{method} {path}"),
            )?,
        }
    }
    Ok(Some(caller))
}

/// A board write's caller, once [`board_caller`] has decided.
pub(super) enum Caller {
    /// Holds a live session: `actor` is `operator (ui)` or a proven
    /// tailnet login; comment authors and monitor acks record `operator`.
    Operator(String),
    /// A process tied to a registered pane or a live managed endpoint,
    /// with no session: its alias is the actor and author.
    Agent(String),
}

impl Caller {
    pub(super) fn actor(&self) -> &str {
        match self {
            Caller::Operator(actor) => actor,
            Caller::Agent(alias) => alias,
        }
    }

    pub(super) fn author(&self) -> &str {
        match self {
            Caller::Operator(_) => "operator",
            Caller::Agent(alias) => alias,
        }
    }
}

/// This board's own loopback name: `cadence-<port>.localhost:<port>`.
/// Browsers resolve every `*.localhost` name to loopback (RFC 6761) and
/// scope cookies by host, not port — so a session cookie set on this
/// name is never sent to another board, an agent's dev server or any
/// other service on `127.0.0.1`/`localhost`/`cadence.localhost`, whatever
/// its port. Loopback sessions are opened and honoured on this Host only.
pub(crate) fn board_host(port: u16) -> String {
    format!("cadence-{port}.localhost:{port}")
}

/// Where a request came from, for session binding.
enum ReqOrigin {
    Known(Origin),
    /// No session can be used or opened on this request: the Host is not
    /// this board's own name, or names the tailnet without the proof.
    NoSession(String),
}

fn request_origin(request: &Request, opts: &ServeOpts) -> ReqOrigin {
    match tailnet_proxy(request, opts) {
        Some(Ok(())) => ReqOrigin::Known(Origin::Tailnet),
        Some(Err(r)) => ReqOrigin::NoSession(format!(
            "this request names the tailnet but is not proven to come through tailscale \
             serve — {}: {}",
            r.check.as_str(),
            r.why
        )),
        None => {
            let host = header_value(request, "Host")
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase();
            let own = board_host(opts.port);
            if host == own {
                ReqOrigin::Known(Origin::Loopback)
            } else {
                ReqOrigin::NoSession(format!(
                    "operator sessions live only on this board's own name, http://{own} — \
                     not '{host}'"
                ))
            }
        }
    }
}

/// The session cookie's name on this board. The port keeps two boards
/// on one host from clobbering each other's cookie (cookies ignore
/// ports); `__Host-` on the https tailnet origin makes the browser
/// enforce `Secure`, `Path=/` and no `Domain`.
fn cookie_name(opts: &ServeOpts, origin: Origin) -> String {
    match origin {
        Origin::Loopback => format!("cadence_operator_{}", opts.port),
        Origin::Tailnet => format!("__Host-cadence_operator_{}", opts.port),
    }
}

/// The session token the request presents for `origin`, if any.
fn session_cookie(request: &Request, opts: &ServeOpts, origin: Origin) -> Option<String> {
    let name = cookie_name(opts, origin);
    request
        .headers()
        .iter()
        .filter(|h| h.field.equiv("Cookie"))
        .flat_map(|h| h.value.as_str().split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// This request's own origin, as a browser on it would send `Origin`.
fn own_origin(request: &Request, origin: Origin) -> String {
    let host = header_value(request, "Host")
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    match origin {
        Origin::Loopback => format!("http://{host}"),
        Origin::Tailnet => format!("https://{}", host.trim_end_matches(":443")),
    }
}

/// A cookie-bearing write must say where it comes from, and it must be
/// here: `Origin` present and equal to this request's scheme + Host.
fn require_own_origin(request: &Request, origin: Origin) -> Result<(), HttpResp> {
    let want = own_origin(request, origin);
    match header_value(request, "Origin") {
        Some(got) if got.trim().eq_ignore_ascii_case(&want) => Ok(()),
        Some(got) => Err(guard_fail(
            "origin",
            &format!(
                "an operator session is only accepted from its own origin ({want}); \
                 this request says '{got}'"
            ),
        )),
        None => Err(guard_fail(
            "origin",
            "a request carrying an operator session must send Origin",
        )),
    }
}

/// Ask the daemon whether `token` is a live session on `origin`.
/// The header carrying a session's second credential (review round 2,
/// PR #249). The page holds it in `sessionStorage` — scoped to its exact
/// origin, port included — so a listener on another port that receives
/// the cookie (cookies ignore ports) never has it, and a cross-site page
/// cannot set a custom header without a preflight this board refuses.
pub(super) const SESSION_HEADER: &str = "X-Cadence-Session";

/// The page's session key, `""` when the request carries none.
fn session_key(request: &Request) -> String {
    header_value(request, SESSION_HEADER)
        .map(|k| k.trim().to_string())
        .unwrap_or_default()
}

/// Ask the daemon whether the cookie's `token` and the page's `key`
/// together are a live session on `origin`.
fn check_session(
    state_dir: &std::path::Path,
    token: &str,
    key: &str,
    origin: Origin,
) -> Result<Option<Value>, HttpResp> {
    match client::rpc(
        state_dir,
        "operator_session_check",
        json!({"token": token, "key": key, "origin": origin.as_str()}),
    ) {
        Ok(v) if v["valid"] == true => Ok(Some(v["session"].clone())),
        Ok(_) => Ok(None),
        Err(e) if e.to_string().contains("Unknown method") => Err(coded_response(
            501,
            "unsupported_daemon",
            "this daemon does not support operator sessions — upgrade it",
            None,
        )),
        Err(e) => Err(coded_response(
            503,
            "daemon_unavailable",
            &format!("operator sessions are checked by the daemon: {e}"),
            None,
        )),
    }
}

/// How the board's peer attribution came out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Attribution {
    /// Tied to exactly this agent.
    Agent(String),
    /// A local process tied to no agent — the operator's browser, a
    /// relay, or a detached agent child alike.
    NoAgent,
    /// Unattributable, but the connection's client socket is still open
    /// and belongs to ANOTHER uid — a privilege-separated proxy
    /// (tailscaled, sshd) whose process this user cannot see.
    Foreign(String),
    /// Unattributable in any other way: unreadable ancestry, several
    /// agents, no daemon — or a client socket that is already gone or is
    /// this uid's with no visible owner (a sender that closed its end
    /// early to escape attribution).
    Unknown(String),
    /// The proven `tailscale serve` proxy, with its login (or why none).
    Proxy(Result<String, String>),
}

/// What [`decide`] rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Verdict {
    Operator(String),
    Agent(String),
    /// A live session presented by an agent: revoke it, refuse.
    Stolen(String),
    Refuse(&'static str, String),
}

pub(super) const SESSION_REQUIRED: &str =
    "board writes need the operator's session — this request \
     carries none (a caller tied to no agent, or any relay, is never the operator by default). \
     Sign in with `cadence ui login` from the operator's own shell";

/// The board's one caller rule (module table), pure so every row is
/// unit-tested — the proven-proxy rows included, which no integration
/// test can reach (a test cannot own a socket as tailscaled's uid).
pub(super) fn decide(session: bool, attribution: Attribution) -> Verdict {
    match (session, attribution) {
        (true, Attribution::Agent(alias)) => Verdict::Stolen(alias),
        (true, Attribution::Proxy(Ok(actor))) => Verdict::Operator(actor),
        (true, Attribution::Proxy(Err(why))) => {
            Verdict::Refuse("caller_identity", format!("board write refused: {why}."))
        }
        (true, Attribution::NoAgent | Attribution::Foreign(_)) => {
            Verdict::Operator(UI_ACTOR.to_string())
        }
        (true, Attribution::Unknown(why)) => Verdict::Refuse(
            "caller_identity",
            format!(
                "board write refused: an operator session is honoured only from a caller \
                 the board can attribute, or another uid's proxy — {why}"
            ),
        ),
        (false, Attribution::Agent(alias)) => Verdict::Agent(alias),
        (false, Attribution::NoAgent | Attribution::Proxy(_)) => {
            Verdict::Refuse("operator_session_required", SESSION_REQUIRED.to_string())
        }
        (false, Attribution::Unknown(why) | Attribution::Foreign(why)) => Verdict::Refuse(
            "caller_identity",
            format!(
                "board write refused: caller identity underivable — {why}. Writes \
                 attribute the peer process to the registered pane or managed endpoint \
                 it is tied to; the operator writes with a session (`cadence ui login`)."
            ),
        ),
    }
}

/// Attribute the TCP peer to an agent ([`crate::peer::tcp_peer_agent`]).
fn attribute(
    request: &Request,
    state_dir: &std::path::Path,
    opts: &ServeOpts,
    origin: &ReqOrigin,
) -> Attribution {
    if matches!(origin, ReqOrigin::Known(Origin::Tailnet)) {
        return Attribution::Proxy(proxied_actor(
            header_value(request, "Tailscale-User-Login").as_deref(),
        ));
    }
    let found = agent_roots(state_dir).and_then(|roots| {
        let peer = request
            .remote_addr()
            .ok_or_else(|| "the request has no peer address".to_string())?;
        if roots.is_empty() {
            // No live agent to tie the peer to — but the connection must
            // still be one: a loopback client whose socket is already
            // gone (an early-closed replay by an agent that then exited)
            // is unattributable, never "tied to no agent" (review round 2).
            let peer = crate::peer::canonical(*peer);
            if peer.ip().is_loopback() && crate::peer::client_socket(opts.port, peer)?.is_none() {
                return Err(format!(
                    "no local socket is the client end of {peer} → port {}",
                    opts.port
                ));
            }
            return Ok(None);
        }
        crate::peer::tcp_peer_agent(opts.port, *peer, &roots)
    });
    match found {
        Ok(Some(alias)) => Attribution::Agent(alias),
        Ok(None) => Attribution::NoAgent,
        Err(why) => {
            // Only a live client socket of another uid excuses it.
            // SAFETY: geteuid has no preconditions and cannot fail.
            let euid = unsafe { libc::geteuid() };
            let foreign = request
                .remote_addr()
                .and_then(|peer| crate::peer::client_socket(opts.port, *peer).ok().flatten())
                .filter(|(_, uid)| *uid != euid);
            let why = match origin {
                ReqOrigin::NoSession(p) => format!("{why}. {p}"),
                ReqOrigin::Known(_) => why,
            };
            match foreign {
                Some((_, uid)) => {
                    Attribution::Foreign(format!("{why} (its socket is uid {uid}'s)"))
                }
                None => Attribution::Unknown(why),
            }
        }
    }
}

/// The caller of a board request (module doc). `write` requires a
/// cookie-bearing request to carry its own `Origin` (browsers send none
/// on a same-origin GET, so `/api/meta` passes `false`). `Err` is the
/// refusal to send.
pub(super) fn board_caller(
    request: &Request,
    state_dir: &std::path::Path,
    opts: &ServeOpts,
    write: bool,
) -> Result<Caller, HttpResp> {
    let origin = request_origin(request, opts);
    let session = match &origin {
        ReqOrigin::Known(o) => match session_cookie(request, opts, *o) {
            Some(token) => {
                if write {
                    require_own_origin(request, *o)?;
                }
                check_session(state_dir, &token, &session_key(request), *o)?.map(|_| token)
            }
            None => None,
        },
        ReqOrigin::NoSession(_) => None,
    };
    let attribution = attribute(request, state_dir, opts, &origin);
    match decide(session.is_some(), attribution) {
        Verdict::Operator(actor) => Ok(Caller::Operator(actor)),
        Verdict::Agent(alias) => Ok(Caller::Agent(alias)),
        Verdict::Stolen(alias) => {
            let token = session.unwrap_or_default();
            let _ = client::rpc(
                state_dir,
                "operator_session_stolen",
                json!({"token": token, "agent": alias}),
            );
            Err(guard_fail(
                "session_from_agent",
                &format!(
                    "an operator session was presented by agent '{alias}' — the session is \
                     revoked. Sign in again with `cadence ui login`"
                ),
            ))
        }
        Verdict::Refuse(check, msg) => Err(guard_fail(check, &msg)),
    }
}

/// `/api/meta`'s session fields: `operator` (this request holds a live
/// session), `session` (its display id and expiries, never the token)
/// and `login_hint` (the command that signs this origin in).
pub(super) fn meta(request: &Request, state_dir: &std::path::Path, opts: &ServeOpts) -> Value {
    let (hint, cookie, session) = match request_origin(request, opts) {
        ReqOrigin::Known(o) => {
            let hint = match o {
                Origin::Loopback => "cadence ui login",
                Origin::Tailnet => "cadence ui login --tailnet",
            };
            let token = session_cookie(request, opts, o);
            let session = token.as_deref().and_then(|token| {
                check_session(state_dir, token, &session_key(request), o)
                    .ok()
                    .flatten()
            });
            (hint, token.is_some(), session)
        }
        ReqOrigin::NoSession(_) => ("cadence ui login", false, None),
    };
    json!({
        "signed_in": session.is_some(),
        "session": session,
        "login_hint": hint,
        // A cookie but no live session for this page: typically a new
        // tab — the key lives in the signing-in tab's `sessionStorage`.
        "tab_signed_out": cookie && session.is_none(),
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionReq {
    nonce: String,
}

fn set_cookie(resp: &mut HttpResp, value: &str) {
    if let Ok(h) = Header::from_bytes("Set-Cookie", value.as_bytes()) {
        resp.add_header(h);
    }
}

fn no_store(mut resp: HttpResp) -> HttpResp {
    resp.add_header(Header::from_bytes("Cache-Control", "no-store").unwrap());
    resp
}

fn cookie_attrs(origin: Origin, max_age: i64) -> String {
    let secure = if origin == Origin::Tailnet {
        "; Secure"
    } else {
        ""
    };
    format!("Path=/; HttpOnly; SameSite=Strict; Max-Age={max_age}{secure}")
}

/// `POST /api/session {"nonce"}` — exchange a login link for a session:
/// the cookie (`Set-Cookie`) plus `{"session_key"}` for the page, both
/// required on every later write. The write guards apply, the Host must be this board's own
/// name (or the proven tailnet), and `Origin` must be this request's
/// own; the link must have been minted for this origin. The peer is
/// attributed BEFORE the exchange, under the same rule as a
/// session-bearing write: a peer tied to an agent, or one the board
/// cannot attribute (its socket already gone, or this uid's with no
/// visible owner), spends the link and gets nothing.
pub(super) fn open(
    request: &mut Request,
    state_dir: &std::path::Path,
    opts: &ServeOpts,
) -> HttpResp {
    if opts.read_only {
        return guard_fail("read_only", "board is read-only — sign-in is disabled");
    }
    if let Err(resp) = write_guard(request, "application/json", opts) {
        return resp;
    }
    let origin = match request_origin(request, opts) {
        ReqOrigin::Known(o) => o,
        ReqOrigin::NoSession(why) => {
            return guard_fail("session_origin", &format!("sign-in refused: {why}"))
        }
    };
    if let Err(resp) = require_own_origin(request, origin) {
        return resp;
    }
    let bytes = match read_body(request, 4096) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let req: SessionReq = match parse_json(&bytes) {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    let user_agent = header_value(request, "User-Agent").unwrap_or_default();
    let refusal = match attribute(request, state_dir, opts, &ReqOrigin::Known(origin)) {
        Attribution::Agent(alias) => Some((
            "session_from_agent",
            alias.clone(),
            format!("sign-in refused: this request comes from agent '{alias}' — the link is spent"),
        )),
        Attribution::Unknown(why) => Some((
            "caller_identity",
            "unattributable".to_string(),
            format!("sign-in refused: the board cannot attribute this caller — {why}; the link is spent"),
        )),
        Attribution::NoAgent | Attribution::Foreign(_) | Attribution::Proxy(_) => None,
    };
    let opened = client::rpc(
        state_dir,
        "operator_session_open",
        json!({"nonce": req.nonce, "origin": origin.as_str(), "user_agent": user_agent}),
    );
    let opened = match opened {
        Ok(v) => v,
        Err(e) if e.code() == Some("login_link") => {
            return no_store(guard_fail("login_link", &e.to_string()))
        }
        // The daemon saw an agent on the BOARD's own connection: a board
        // an agent started opens no session.
        Err(e) if e.code() == Some("session_from_agent") => {
            return no_store(guard_fail("session_from_agent", &e.to_string()))
        }
        Err(e) if e.to_string().contains("Unknown method") => {
            return coded_response(
                501,
                "unsupported_daemon",
                "this daemon does not support operator sessions — upgrade it",
                None,
            )
        }
        Err(e) => {
            return coded_response(503, "daemon_unavailable", &e.to_string(), None);
        }
    };
    let token = opened["token"].as_str().unwrap_or_default().to_string();
    if let Some((check, agent, msg)) = refusal {
        let _ = client::rpc(
            state_dir,
            "operator_session_stolen",
            json!({"token": token, "agent": agent}),
        );
        return guard_fail(check, &msg);
    }
    let now = crate::issue::time::now_epoch();
    let expires = opened["session"]["expires_at"].as_i64().unwrap_or(now);
    // The key goes to this page only, once: it keeps it in
    // `sessionStorage` and sends it as `X-Cadence-Session`.
    let key = opened["key"].as_str().unwrap_or_default();
    let body = serde_json::to_vec(&json!({ "session_key": key })).unwrap_or_default();
    let mut resp = Response::from_data(body).with_status_code(StatusCode(200));
    resp.add_header(Header::from_bytes("Content-Type", "application/json").unwrap());
    set_cookie(
        &mut resp,
        &format!(
            "{}={token}; {}",
            cookie_name(opts, origin),
            cookie_attrs(origin, (expires - now).max(0))
        ),
    );
    no_store(resp)
}

/// `POST /api/session/logout` — end the presenting session (if any) and
/// clear its cookie. Idempotent.
pub(super) fn logout(request: &Request, state_dir: &std::path::Path, opts: &ServeOpts) -> HttpResp {
    if let Err(resp) = write_guard(request, "application/json", opts) {
        return resp;
    }
    let ReqOrigin::Known(origin) = request_origin(request, opts) else {
        return guard_fail("session_origin", "logout: no session lives on this Host");
    };
    if let Some(token) = session_cookie(request, opts, origin) {
        if let Err(resp) = require_own_origin(request, origin) {
            return resp;
        }
        if let Err(e) = client::rpc(
            state_dir,
            "operator_session_logout",
            json!({"token": token}),
        ) {
            return coded_response(503, "daemon_unavailable", &e.to_string(), None);
        }
    }
    let mut resp = Response::from_data(Vec::new()).with_status_code(StatusCode(204));
    set_cookie(
        &mut resp,
        &format!(
            "{}=; {}",
            cookie_name(opts, origin),
            cookie_attrs(origin, 0)
        ),
    );
    no_store(resp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_table_classifies_and_fails_closed() {
        let operator_only = WRITE_ROUTES
            .iter()
            .filter(|r| r.class == RouteClass::OperatorOnly)
            .count();
        assert!(operator_only >= 7, "the table lost operator-only routes");
        assert_eq!(
            route_class("POST", "/api/plans/CAD-1/approve"),
            RouteClass::OperatorOnly
        );
        assert_eq!(
            route_class("POST", "/api/issues/CAD-1/answers"),
            RouteClass::OperatorOnly
        );
        assert_eq!(
            route_class("POST", "/api/issues/CAD-1/comments"),
            RouteClass::AgentAllowed
        );
        assert_eq!(
            route_class("PATCH", "/api/issues/CAD-1"),
            RouteClass::AgentAllowed
        );
        assert_eq!(route_class("POST", "/api/issues"), RouteClass::AgentAllowed);
        assert_eq!(route_class("POST", "/api/session"), RouteClass::Session);
        // Unlisted writes are operator-only.
        assert_eq!(route_class("POST", "/api/launch"), RouteClass::OperatorOnly);
        assert_eq!(
            route_class("POST", "/api/epics/E-1/stage"),
            RouteClass::OperatorOnly
        );
        assert_eq!(
            route_class("DELETE", "/api/issues/CAD-1"),
            RouteClass::OperatorOnly
        );
        assert_eq!(
            route_class("POST", "/api/issues//comments"),
            RouteClass::OperatorOnly
        );
    }

    /// Every row of the caller table, including the proven-proxy rows no
    /// integration test can reach.
    #[test]
    fn the_caller_table() {
        let agent = || Attribution::Agent("pane-a".into());
        let proxy = |l: &str| Attribution::Proxy(Ok(format!("{l} (tailscale)")));
        // With a session.
        assert_eq!(decide(true, agent()), Verdict::Stolen("pane-a".into()));
        assert_eq!(
            decide(true, proxy("fable@example.com")),
            Verdict::Operator("fable@example.com (tailscale)".into())
        );
        assert!(matches!(
            decide(true, Attribution::Proxy(Err("no login".into()))),
            Verdict::Refuse("caller_identity", _)
        ));
        assert_eq!(
            decide(true, Attribution::NoAgent),
            Verdict::Operator(UI_ACTOR.into())
        );
        // Another uid's live socket (sshd, tailscaled) is excused; a
        // socket that is gone (an early-closed replay) is not.
        assert_eq!(
            decide(true, Attribution::Foreign("sshd".into())),
            Verdict::Operator(UI_ACTOR.into())
        );
        assert!(matches!(
            decide(true, Attribution::Unknown("socket gone".into())),
            Verdict::Refuse("caller_identity", _)
        ));
        assert!(matches!(
            decide(false, Attribution::Foreign("sshd".into())),
            Verdict::Refuse("caller_identity", _)
        ));
        // Without one: never the operator, whatever the peer.
        assert_eq!(decide(false, agent()), Verdict::Agent("pane-a".into()));
        assert!(matches!(
            decide(false, Attribution::NoAgent),
            Verdict::Refuse("operator_session_required", _)
        ));
        assert!(matches!(
            decide(false, proxy("fable@example.com")),
            Verdict::Refuse("operator_session_required", _)
        ));
        assert!(matches!(
            decide(false, Attribution::Unknown("x".into())),
            Verdict::Refuse("caller_identity", _)
        ));
    }

    #[test]
    fn cookie_attributes() {
        let opts = ServeOpts {
            port: 3123,
            ..Default::default()
        };
        assert_eq!(
            cookie_name(&opts, Origin::Loopback),
            "cadence_operator_3123"
        );
        assert_eq!(
            cookie_name(&opts, Origin::Tailnet),
            "__Host-cadence_operator_3123"
        );
        let l = cookie_attrs(Origin::Loopback, 60);
        assert!(l.contains("HttpOnly") && l.contains("SameSite=Strict") && l.contains("Path=/"));
        assert!(!l.contains("Secure") && !l.contains("Domain"));
        assert!(cookie_attrs(Origin::Tailnet, 60).contains("Secure"));
    }
}
