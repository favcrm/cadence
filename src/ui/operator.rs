//! Who the board trusts as the operator (CAD-313, CAD-428; ADR 0004
//! phase 1). Exactly one thing makes a board request the operator's: a
//! live **operator session** — an HttpOnly cookie the daemon issued in
//! exchange for a single-use `cadence ui login` link, AND the page's
//! session key in `X-Cadence-Session` ([`SESSION_HEADER`]; a cookie leaked
//! to another port's listener is worthless without it) — presented on
//! the origin it was issued for, by a process tied to no agent. Nothing
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
//! | valid operator | tied to no agent, or unattributable with its client socket alive and another uid's (sshd, tailscaled) | the operator, `operator (ui)` |
//! | valid public | same | the session's named user, `<name> <email> (board)` (CAD-526) |
//! | valid | unattributable otherwise (socket already closed, or ours with no visible owner) | refused `caller_identity` |
//! | none/invalid | tied to an agent | that agent (agent-allowed routes only) |
//! | none/invalid | tied to no agent, a relay, or the proxy | refused `operator_session_required` / `board_session_required` |
//! | none/invalid | unattributable | refused `caller_identity` |
//!
//! A session is bound to its origin: `loopback` — on this board's own
//! name, `cadence-<port>.localhost:<port>` ([`board_host`]), and no
//! other Host, so the cookie is never sent to another port's server;
//! `tailnet` (a request the tailnet proof passes); or `public` (CAD-526)
//! — this instance's configured AgenticOS board host, where the
//! `__Host-aos-board-session` cookie alone is the credential (the
//! contract's session has no page key) and every session names a
//! platform-verified user. Any other request has no origin, so no
//! session. A write that carries a session cookie must also carry
//! `Origin`, and it must be this very request's own scheme and Host —
//! a cookie replayed from another origin, or by a client that sends
//! none, is refused.
//!
//! [`WRITE_ROUTES`] classifies every write, and [`admit`] enforces the
//! class before any handler runs. Operator-only routes additionally run
//! positive process proof on the HTTP peer (`home::prove_operator_peer`):
//! the board is never less strict than the daemon verb it relays.

use serde::Deserialize;
use serde_json::{json, Value};
use tiny_http::{Header, Request, Response, StatusCode};

use super::{
    agent_roots, coded_response, guard_fail, header_value, parse_json, pct_decode, proxied_actor,
    read_body, tailnet_proxy, write_guard, HttpResp, ServeOpts, UI_ACTOR,
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
    // CAD-606: board Kick off — operator-only, same gate as `issue_kickoff`.
    route("POST", "/api/issues/*/kickoff", RouteClass::OperatorOnly),
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
    // CAD-551: the composer's slash commands and Stop — the daemon's
    // `master_command` is operator-only, so the relay is too.
    route("POST", "/api/master/command", RouteClass::OperatorOnly),
    // CAD-574: the rail's needs-row snooze/dismiss and agent
    // resume/unfence — the relays cross the daemon's operator gate, so
    // the board routes are the operator's alone (unlisted writes would
    // fail closed the same way; listed for the reader).
    route("POST", "/api/needs/dismiss", RouteClass::OperatorOnly),
    route("POST", "/api/needs/snooze", RouteClass::OperatorOnly),
    route("POST", "/api/agents/*/resume", RouteClass::OperatorOnly),
    route("POST", "/api/agents/*/unfence", RouteClass::OperatorOnly),
    route("POST", "/api/epics/*/stage", RouteClass::OperatorOnly),
    // CAD-561: the board's Update button and its check — operator-only,
    // like every other action that replaces the running build.
    route("POST", "/api/update", RouteClass::OperatorOnly),
    route("POST", "/api/update/check", RouteClass::OperatorOnly),
    // CAD-496: the board relays `plan_propose` over its own daemon
    // connection, so the daemon attributes the run to whoever that
    // connection proves — operator-only, like `plan_approve`.
    // CAD-547: `<app>/<wf>` names an installed app's workflow — an
    // extra path segment, still the operator's call (unlisted writes
    // would fail closed the same way; listed for the reader).
    route(
        "POST",
        "/api/projects/*/workflows/*/propose",
        RouteClass::OperatorOnly,
    ),
    route(
        "POST",
        "/api/projects/*/workflows/*/*/propose",
        RouteClass::OperatorOnly,
    ),
    // CAD-557: the board relays `app_approve` over its own daemon
    // connection, so the daemon attributes the approval to whoever that
    // connection proves — operator-only, like `plan_approve`.
    route("POST", "/api/apps/*/*/approve", RouteClass::OperatorOnly),
    // CAD-580: the wiki routes carry the caller's own wiki_as to the
    // daemon — every admitted caller (operator, named member, the
    // attributed agent) writes, and the daemon's per-prefix allowlist
    // decides where. The board relays; it never widens a caller.
    route("PUT", "/api/wiki/file", RouteClass::AgentAllowed),
    route("POST", "/api/wiki/upload", RouteClass::AgentAllowed),
    route("POST", "/api/wiki/mkdir", RouteClass::AgentAllowed),
    route("POST", "/api/wiki/mv", RouteClass::AgentAllowed),
    route("POST", "/api/wiki/rm", RouteClass::AgentAllowed),
    // CAD-577: the board relays `app_revoke` — withdrawing an approval
    // and every grant it derived is the operator's, like the approval.
    route("POST", "/api/apps/*/*/revoke", RouteClass::OperatorOnly),
    // CAD-577: the board relays `app_set_team` — the app's default team
    // is an operator-only write, attributed to the board's proven
    // connection by the daemon.
    route("POST", "/api/apps/*/*/team", RouteClass::OperatorOnly),
    // CAD-577: the board relays `app_add_worker` — joining a new worker
    // for one team role is the operator's, like the team write itself.
    route("POST", "/api/apps/*/*/worker", RouteClass::OperatorOnly),
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
///   agent or a `member`-role named user is refused — then positive
///   process proof on the HTTP peer (`home::prove_operator_peer`),
///   because the handler relays over the board's own daemon connection.
///   A public session that asserts the `owner` role stands in for that
///   proof: the platform's verified role is the operator claim on this
///   surface (CAD-526 — the public peer is the platform relay, which a
///   process proof cannot identify);
/// - `AgentAllowed`: read-only off, the guards, and [`board_caller`]:
///   the operator's session, a named user's session, or the one agent
///   the peer is tied to;
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
    } else if path == "/api/wiki/upload" {
        // Multipart is the one non-exact rule — `write_guard` checks
        // the bounded `multipart/form-data; boundary=` prefix; the
        // header/origin/fetch-site guards still run exact.
        "multipart/form-data"
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
            // A member reads/participates/requests but never decides:
            // approvals, grants and operator methods are the owner's
            // role (contract §2, CAD-526).
            Caller::Named(named) if !named.operator => {
                return Err(guard_fail(
                    "member_role",
                    &format!(
                        "{method} {path} needs the board owner's role — this session is \
                         {}'s, mapped `member`",
                        named.actor
                    ),
                ))
            }
            // The verified `owner` role is the operator claim on the
            // public surface — no loopback process proof applies to a
            // request the platform relay delivered.
            Caller::Named(_) => {}
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

/// A public session's user as a board caller (CAD-526): what a verified
/// assertion proved, kept to the attribution fields a write needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Named {
    /// `Fable Chen <fable@example.com> (board)` — the audit actor.
    pub actor: String,
    /// The `[A-Za-z0-9_-]` comment-author / ack handle.
    pub author: String,
    /// `owner`-mapped sessions pass operator-only routes.
    pub operator: bool,
}

/// A board write's caller, once [`board_caller`] has decided.
pub(super) enum Caller {
    /// Holds a live operator session: `actor` is `operator (ui)` or a
    /// proven tailnet login; comment authors and monitor acks record
    /// `operator`.
    Operator(String),
    /// Holds a live public session minted from a platform assertion —
    /// the named user, never a shared operator identity.
    Named(Named),
    /// A process tied to a registered pane or a live managed endpoint,
    /// with no session: its alias is the actor and author.
    Agent(String),
}

impl Caller {
    pub(super) fn actor(&self) -> &str {
        match self {
            Caller::Operator(actor) => actor,
            Caller::Named(named) => &named.actor,
            Caller::Agent(alias) => alias,
        }
    }

    pub(super) fn author(&self) -> &str {
        match self {
            Caller::Operator(_) => "operator",
            Caller::Named(named) => &named.author,
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
            // CAD-526: this instance's configured public name — the
            // `aud` assertions are minted for — is its own origin, as
            // distinct from the local login surface as tailnet is.
            if let Some(public) = &opts.public {
                if host == public.host {
                    return ReqOrigin::Known(Origin::Public);
                }
            }
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

/// The public session cookie (contract §7): `__Host-` pins `Secure`,
/// `Path=/` and no `Domain`, so a sibling host under `*.BOARD_DOMAIN`
/// can neither toss nor overwrite it.
pub(crate) const PUBLIC_COOKIE: &str = "__Host-aos-board-session";
/// The unprefixed spelling the platform worker accepts inbound as a
/// local-dev allowance — the board reads it too; it always mints the
/// `__Host-` name.
const PUBLIC_COOKIE_INSECURE: &str = "aos-board-session";

/// The session cookie's name on this board. The port keeps two boards
/// on one host from clobbering each other's cookie (cookies ignore
/// ports); `__Host-` on the https origins (tailnet, and the public
/// board host — CAD-526) makes the browser enforce `Secure`, `Path=/`
/// and no `Domain`.
fn cookie_name(opts: &ServeOpts, origin: Origin) -> String {
    match origin {
        Origin::Loopback => format!("cadence_operator_{}", opts.port),
        Origin::Tailnet => format!("__Host-cadence_operator_{}", opts.port),
        Origin::Public => PUBLIC_COOKIE.to_string(),
    }
}

/// The session token the request presents for `origin`, if any.
fn session_cookie(request: &Request, opts: &ServeOpts, origin: Origin) -> Option<String> {
    // The public surface reads both the `__Host-` name and the worker's
    // unprefixed local-dev allowance (contract §7); the minted name
    // wins when a request carries both.
    let names = match origin {
        Origin::Public => vec![
            PUBLIC_COOKIE.to_string(),
            PUBLIC_COOKIE_INSECURE.to_string(),
        ],
        _ => vec![cookie_name(opts, origin)],
    };
    request
        .headers()
        .iter()
        .filter(|h| h.field.equiv("Cookie"))
        .flat_map(|h| h.value.as_str().split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(k, _)| names.iter().any(|n| n == *k))
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
        // `*.board.localhost[:port]` is a trustworthy http context; a
        // real board host only ever serves https (contract §1).
        Origin::Public => format!("{}://{host}", public_scheme(&host)),
    }
}

/// The scheme a public board host speaks (contract §1): `*.localhost`
/// is a trustworthy context where plain http applies; every real board
/// host is https — `cadencecloud.app` sits on an HSTS-preloaded TLD.
pub(crate) fn public_scheme(host: &str) -> &'static str {
    let name = host.split(':').next().unwrap_or_default();
    if name == "localhost" || name.ends_with(".localhost") {
        "http"
    } else {
        "https"
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
/// Public sessions (CAD-526) have no page key: the `__Host-` cookie is
/// the whole credential on an origin no other port can share.
pub(super) const SESSION_HEADER: &str = "X-Cadence-Session";

/// The page's session key, `""` when the request carries none.
fn session_key(request: &Request) -> String {
    header_value(request, SESSION_HEADER)
        .map(|k| k.trim().to_string())
        .unwrap_or_default()
}

/// Ask the daemon whether the cookie's `token` and the page's `key`
/// together are a live session on `origin`. A `public` request is
/// answered by `board_session_check` — the cookie alone — and can only
/// ever name a `public` session row.
fn check_session(
    state_dir: &std::path::Path,
    token: &str,
    key: &str,
    origin: Origin,
) -> Result<Option<Value>, HttpResp> {
    let (method, params) = if origin == Origin::Public {
        ("board_session_check", json!({"token": token}))
    } else {
        (
            "operator_session_check",
            json!({"token": token, "key": key, "origin": origin.as_str()}),
        )
    };
    match client::rpc(state_dir, method, params) {
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
            &format!("board sessions are checked by the daemon: {e}"),
            None,
        )),
    }
}

/// The live public session this request's `__Host-` cookie names — the
/// read gate for the board's public surface (CAD-526): on that host a
/// request either carries a valid session or gets bounced to the
/// platform sign-in. `Err` is a daemon failure the caller must answer.
pub(super) fn public_session(
    request: &Request,
    state_dir: &std::path::Path,
    opts: &ServeOpts,
) -> Result<Option<Value>, HttpResp> {
    match request_origin(request, opts) {
        ReqOrigin::Known(Origin::Public) => match session_cookie(request, opts, Origin::Public) {
            Some(token) => check_session(state_dir, &token, "", Origin::Public),
            None => Ok(None),
        },
        _ => Ok(None),
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
    /// A verified public session's named user (CAD-526).
    Named(Named),
    Agent(String),
    /// A live session presented by an agent: revoke it, refuse.
    Stolen(String),
    Refuse(&'static str, String),
}

/// The live session a request proved. `Operator` rows exist on the
/// loopback/tailnet surfaces; `Named` only on the public one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Held {
    Operator,
    Named(Named),
}

pub(super) const SESSION_REQUIRED: &str =
    "board writes need the operator's session — this request \
     carries none (a caller tied to no agent, or any relay, is never the operator by default). \
     Sign in with `cadence ui login` from the operator's own shell";

/// The same refusal on the public surface: sign-in is the platform's.
pub(super) const BOARD_SESSION_REQUIRED: &str =
    "board writes need a board session — this request carries none. \
     Sign in through your company's AgenticOS workspace";

/// The board's one caller rule (module table), pure so every row is
/// unit-tested — the proven-proxy rows included, which no integration
/// test can reach (a test cannot own a socket as tailscaled's uid).
///
/// `required` is the refusal a sessionless non-agent caller gets —
/// `board_session_required` on the public host, `operator_session_required`
/// elsewhere.
pub(super) fn decide(
    held: Option<Held>,
    attribution: Attribution,
    required: (&'static str, &'static str),
) -> Verdict {
    match (held, attribution) {
        (Some(_), Attribution::Agent(alias)) => Verdict::Stolen(alias),
        // The tailnet rows arise only on the tailnet origin, where a
        // named public session can never be presented — but a proven
        // login names the operator whatever cookie rode along.
        (Some(_), Attribution::Proxy(Ok(actor))) => Verdict::Operator(actor),
        (Some(_), Attribution::Proxy(Err(why))) => {
            Verdict::Refuse("caller_identity", format!("board write refused: {why}."))
        }
        (Some(Held::Named(named)), Attribution::NoAgent | Attribution::Foreign(_)) => {
            Verdict::Named(named)
        }
        (Some(Held::Operator), Attribution::NoAgent | Attribution::Foreign(_)) => {
            Verdict::Operator(UI_ACTOR.to_string())
        }
        (Some(Held::Named(_)), Attribution::Unknown(why)) => Verdict::Refuse(
            "caller_identity",
            format!(
                "board write refused: a board session is honoured only from a caller \
                 the board can attribute, or another uid's proxy — {why}"
            ),
        ),
        (Some(Held::Operator), Attribution::Unknown(why)) => Verdict::Refuse(
            "caller_identity",
            format!(
                "board write refused: an operator session is honoured only from a caller \
                 the board can attribute, or another uid's proxy — {why}"
            ),
        ),
        (None, Attribution::Agent(alias)) => Verdict::Agent(alias),
        (None, Attribution::NoAgent | Attribution::Proxy(_)) => {
            Verdict::Refuse(required.0, required.1.to_string())
        }
        (None, Attribution::Unknown(why) | Attribution::Foreign(why)) => Verdict::Refuse(
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
/// CAD-482: a seam-asserted request carries its caller in the headers —
/// `operator` is the same attribution a local un-owned browser gets,
/// `agent:<alias>` ties the write to that agent, `unproven` is
/// unattributable by assertion.
fn attribute(
    request: &Request,
    state_dir: &std::path::Path,
    opts: &ServeOpts,
    origin: &ReqOrigin,
) -> Attribution {
    if let Some(asserted) = crate::test_seam::asserted() {
        return match asserted {
            crate::test_seam::Asserted::Operator => Attribution::NoAgent,
            crate::test_seam::Asserted::Agent(alias) => Attribution::Agent(alias),
            crate::test_seam::Asserted::Unproven => {
                Attribution::Unknown("the request asserts an unproven caller".to_string())
            }
        };
    }
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

/// What a live session row means for [`decide`]: the operator's on
/// loopback/tailnet, the assertion-named user's on the public host. A
/// `public` row whose record carries no user is no session at all —
/// the contract's sessions are always named.
fn held_of(origin: Origin, session: &Value) -> Option<Held> {
    match origin {
        Origin::Public => {
            serde_json::from_value::<crate::operator_auth::BoardUser>(session["user"].clone())
                .ok()
                .map(|user| {
                    Held::Named(Named {
                        actor: user.actor(),
                        author: user.handle.clone(),
                        operator: user.is_operator(),
                    })
                })
        }
        _ => Some(Held::Operator),
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
    let (held, token) = match &origin {
        ReqOrigin::Known(o) => match session_cookie(request, opts, *o) {
            Some(token) => {
                if write {
                    require_own_origin(request, *o)?;
                }
                match check_session(state_dir, &token, &session_key(request), *o)? {
                    Some(session) => (held_of(*o, &session), Some(token)),
                    None => (None, None),
                }
            }
            None => (None, None),
        },
        ReqOrigin::NoSession(_) => (None, None),
    };
    let required = match &origin {
        ReqOrigin::Known(Origin::Public) => ("board_session_required", BOARD_SESSION_REQUIRED),
        _ => ("operator_session_required", SESSION_REQUIRED),
    };
    let attribution = attribute(request, state_dir, opts, &origin);
    match decide(held, attribution, required) {
        Verdict::Operator(actor) => Ok(Caller::Operator(actor)),
        Verdict::Named(named) => Ok(Caller::Named(named)),
        Verdict::Agent(alias) => Ok(Caller::Agent(alias)),
        Verdict::Stolen(alias) => {
            let token = token.unwrap_or_default();
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
                Origin::Public => "sign in through your company's AgenticOS workspace",
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
        // Public sessions have no page key, so the cookie alone answers.
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
    match origin {
        // The contract's full set (§7): `Secure` applies even on
        // `*.board.localhost` — those names are trustworthy contexts.
        Origin::Public => format!(
            "Path=/; HttpOnly; SameSite=Lax; Secure; Max-Age={}",
            max_age.min(crate::operator_auth::PUBLIC_SESSION_SECS)
        ),
        Origin::Tailnet => {
            format!("Path=/; HttpOnly; SameSite=Strict; Secure; Max-Age={max_age}")
        }
        Origin::Loopback => format!("Path=/; HttpOnly; SameSite=Strict; Max-Age={max_age}"),
    }
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
    if origin == Origin::Public {
        // The public surface signs in through the platform assertion —
        // `POST /__platform/session`, never a local login link (contract
        // §9: refuse other sign-in methods on it).
        return guard_fail(
            "session_origin",
            "sign-in on this board is the platform's — POST a verified assertion to \
             /__platform/session, or browse to /__platform/login",
        );
    }
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

// ---------- `/__platform/*` — the AgenticOS sign-in contract (CAD-526) ----------
//
// What the board itself answers on its public name
// (`docs/…/aos-board-identity-contract.md` §3, §8): the assertion-bearing
// session create, the login bounce, and a 404 for anything else under the
// reserved prefix. The worker's copies answer without a container; these
// are the same answers a request that did reach the container gets.

/// `{"ok": false, "error": {code, message}}` — the envelope the contract's
/// handoff page reads (`(await res.json()).error.code`), the same shape
/// the platform worker's `fail()` emits.
fn platform_fail(status: u16, code: &'static str, message: &str) -> HttpResp {
    let body = serde_json::to_vec(&json!({
        "ok": false,
        "error": {"code": code, "message": message},
    }))
    .unwrap_or_default();
    let mut resp = Response::from_data(body).with_status_code(StatusCode(status));
    resp.add_header(Header::from_bytes("Content-Type", "application/json").unwrap());
    no_store(resp)
}

/// The platform error codes the handoff page surfaces (contract §9).
/// `capability_unavailable` is the misconfigured-JWKS/config answer.
fn platform_status(code: &str) -> u16 {
    match code {
        "capability_unavailable" => 503,
        "not_a_member" | "role_unmapped" | "audience_mismatch" => 403,
        _ => 401,
    }
}

/// Percent-encode for a query value — `?to=<absolute URL>` needs the
/// URL's `?`, `&` and `#` escaped. Unreserved RFC 3986 characters pass.
fn pct_encode(raw: &str) -> String {
    raw.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// `302` to the platform's authorize endpoint with `to=<absolute URL>`
/// (contract §3 step 1): what an absent or expired credential answers.
fn redirect_to_authorize(to: &str, opts: &ServeOpts) -> HttpResp {
    let Some(public) = &opts.public else {
        return platform_fail(
            503,
            "capability_unavailable",
            "this board has no platform sign-in configured",
        );
    };
    let location = format!("{}?to={}", public.authorize_url, pct_encode(to));
    let mut resp = Response::from_data(Vec::new()).with_status_code(StatusCode(302));
    if let Ok(h) = Header::from_bytes("Location", location.as_bytes()) {
        resp.add_header(h);
    }
    no_store(resp)
}

/// The absolute URL a `to` may name (contract's `parseBoardUrl` mirror):
/// this board's public host only — a `to` for anywhere else is refused,
/// and a `to` under `/__platform/` collapses to `/` (it would loop).
fn board_url(raw: &str, opts: &ServeOpts) -> Option<String> {
    let public = opts.public.as_ref()?;
    let origin = format!("{}://{}", public_scheme(&public.host), public.host);
    let rest = raw
        .strip_prefix("https://")
        .or_else(|| raw.strip_prefix("http://"))?;
    let (hostport, tail) = match rest.split_once('/') {
        Some((h, t)) => (h, format!("/{t}")),
        None => (rest, "/".to_string()),
    };
    if hostport.contains('@') || !hostport.eq_ignore_ascii_case(&public.host) {
        return None;
    }
    let mut tail = tail.split('#').next().unwrap_or("/").to_string();
    if tail.is_empty() || tail.starts_with("//") || tail.starts_with("/__platform/") {
        tail = "/".to_string();
    }
    // Plain http `to`s name only a localhost board (contract §3).
    if !raw.starts_with("https://") && public_scheme(&public.host) != "http" {
        return None;
    }
    Some(format!("{origin}{tail}"))
}

/// The absolute URL of this very request — the `to` a browser
/// navigation bounces with.
fn request_url(request: &Request) -> String {
    let host = header_value(request, "Host").unwrap_or_default();
    format!("{}://{}{}", public_scheme(&host), host, request.url())
}

/// `GET /__platform/login` — the container-side bounce (contract §3):
/// `?to=<absolute board URL>` is honoured when it names this host;
/// anything else sends the user to the board root.
pub(super) fn platform_login(opts: &ServeOpts, query: &str) -> HttpResp {
    let fallback = opts
        .public
        .as_ref()
        .map(|public| format!("{}://{}/", public_scheme(&public.host), public.host))
        .unwrap_or_else(|| "/".to_string());
    let to = query
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| *k == "to")
        .and_then(|(_, v)| pct_decode(v))
        .and_then(|raw| board_url(&raw, opts))
        .unwrap_or(fallback);
    redirect_to_authorize(&to, opts)
}

/// The contract's response for any other read on `/__platform/*`:
/// `/__platform/callback` is the worker's own static page — a request
/// that still reached the container is bounced to `/`, everything else
/// is a plain 404 (contract §5/§8).
pub(super) fn platform_read(path: &str) -> HttpResp {
    if path == "/__platform/callback" {
        let mut resp = Response::from_data(Vec::new()).with_status_code(StatusCode(302));
        if let Ok(h) = Header::from_bytes("Location", "/") {
            resp.add_header(h);
        }
        return no_store(resp);
    }
    platform_unknown()
}

/// `other /__platform/*` → 404 (contract §8) — every method included:
/// a non-GET on the reserved prefix never matches a board route.
pub(super) fn platform_unknown() -> HttpResp {
    platform_fail(404, "not_found", "Unknown route.")
}

/// A read on the public host without a live board session (contract
/// §9): a browser navigation bounces `302` to authorize with the
/// current URL as `to`; an API/fetch read gets `401` —
/// `board_session_required` — so no SPA request is mistaken for a
/// navigation.
pub(super) fn session_bounce(request: &Request, path: &str, opts: &ServeOpts) -> HttpResp {
    let navigation = !path.starts_with("/api/")
        && header_value(request, "Accept")
            .unwrap_or_default()
            .contains("text/html");
    if navigation {
        redirect_to_authorize(&request_url(request), opts)
    } else {
        platform_fail(
            401,
            "board_session_required",
            "a board session is required — sign in through your company's workspace",
        )
    }
}

/// `POST /__platform/session` — the only request that reaches the
/// container unauthenticated (contract §5): the compact JWS in
/// `{assertion}` is the credential; the daemon verifies it against the
/// platform JWKS before a session exists. `X-Cadence-Board` is
/// deliberately not required — the handoff page is the worker's own
/// static file; the JSON content-type alone blocks a plain cross-site
/// form post, and the single-use `jti` is the real replay defence.
pub(super) fn platform_session(
    request: &mut Request,
    state_dir: &std::path::Path,
    opts: &ServeOpts,
) -> HttpResp {
    if opts.public.is_none() {
        return platform_fail(404, "not_found", "Unknown route.");
    }
    let ct = header_value(request, "Content-Type").unwrap_or_default();
    if ct.trim() != "application/json" {
        return platform_fail(
            400,
            "assertion_invalid",
            "request body must be application/json",
        );
    }
    // A browser caller must be same-origin here; a non-browser one
    // (the contract §10 CLI) sends neither header.
    if let Some(sfs) = header_value(request, "Sec-Fetch-Site") {
        if !sfs.eq_ignore_ascii_case("same-origin") {
            return platform_fail(403, "assertion_invalid", "cross-site request rejected");
        }
    }
    if let Some(origin) = header_value(request, "Origin") {
        let host = header_value(request, "Host").unwrap_or_default();
        let own = format!("{}://{}", public_scheme(&host), host.to_ascii_lowercase());
        if !origin.trim().eq_ignore_ascii_case(&own) {
            return platform_fail(403, "assertion_invalid", "origin is not this board's");
        }
    }
    let bytes = match read_body(request, 8192) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct PlatformSessionReq {
        assertion: String,
    }
    let req: PlatformSessionReq = match parse_json(&bytes) {
        Ok(r) => r,
        Err(_) => return platform_fail(400, "assertion_invalid", "request body is not valid JSON"),
    };
    if req.assertion.is_empty() {
        return platform_fail(400, "assertion_invalid", "request body is not valid JSON");
    }
    let user_agent = header_value(request, "User-Agent").unwrap_or_default();
    match client::rpc(
        state_dir,
        "board_session_open",
        json!({"assertion": req.assertion, "user_agent": user_agent}),
    ) {
        Ok(opened) => {
            let token = opened["token"].as_str().unwrap_or_default();
            let body = serde_json::to_vec(&json!({"ok": true})).unwrap_or_default();
            let mut resp = Response::from_data(body).with_status_code(StatusCode(200));
            resp.add_header(Header::from_bytes("Content-Type", "application/json").unwrap());
            set_cookie(
                &mut resp,
                &format!(
                    "{PUBLIC_COOKIE}={token}; {}",
                    cookie_attrs(Origin::Public, crate::operator_auth::PUBLIC_SESSION_SECS)
                ),
            );
            no_store(resp)
        }
        Err(e) => {
            let code = e.code().unwrap_or("assertion_invalid");
            let code: &'static str = match code {
                "assertion_replayed" => "assertion_replayed",
                "assertion_expired" => "assertion_expired",
                "audience_mismatch" => "audience_mismatch",
                "issuer_mismatch" => "issuer_mismatch",
                "not_a_member" => "not_a_member",
                "role_unmapped" => "role_unmapped",
                "capability_unavailable" => "capability_unavailable",
                _ => "assertion_invalid",
            };
            platform_fail(platform_status(code), code, &e.to_string())
        }
    }
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
            route_class("POST", "/api/issues/CAD-1/kickoff"),
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
        // An app approval is the operator's, like the plan's.
        assert_eq!(
            route_class("POST", "/api/apps/demo/studio/approve"),
            RouteClass::OperatorOnly
        );
        // The app's default team and "Add worker" are the operator's too
        // (CAD-577).
        assert_eq!(
            route_class("POST", "/api/apps/demo/studio/team"),
            RouteClass::OperatorOnly
        );
        assert_eq!(
            route_class("POST", "/api/apps/demo/studio/worker"),
            RouteClass::OperatorOnly
        );
        // CAD-577 revoke and CAD-580 wiki both sit after approve.
        assert_eq!(
            route_class("POST", "/api/apps/demo/studio/revoke"),
            RouteClass::OperatorOnly
        );
        assert_eq!(
            route_class("PUT", "/api/wiki/file"),
            RouteClass::AgentAllowed
        );
        assert_eq!(
            route_class("POST", "/api/wiki/rm"),
            RouteClass::AgentAllowed
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
        let op = || Some(Held::Operator);
        let required = ("operator_session_required", "sign in");
        let member = || {
            Some(Held::Named(Named {
                actor: "Fable <f@e.co> (board)".into(),
                author: "fable".into(),
                operator: false,
            }))
        };
        // With a session.
        assert_eq!(
            decide(op(), agent(), required),
            Verdict::Stolen("pane-a".into())
        );
        assert_eq!(
            decide(op(), proxy("fable@example.com"), required),
            Verdict::Operator("fable@example.com (tailscale)".into())
        );
        assert!(matches!(
            decide(op(), Attribution::Proxy(Err("no login".into())), required),
            Verdict::Refuse("caller_identity", _)
        ));
        assert_eq!(
            decide(op(), Attribution::NoAgent, required),
            Verdict::Operator(UI_ACTOR.into())
        );
        // Another uid's live socket (sshd, tailscaled) is excused; a
        // socket that is gone (an early-closed replay) is not.
        assert_eq!(
            decide(op(), Attribution::Foreign("sshd".into()), required),
            Verdict::Operator(UI_ACTOR.into())
        );
        assert!(matches!(
            decide(op(), Attribution::Unknown("socket gone".into()), required),
            Verdict::Refuse("caller_identity", _)
        ));
        // A named public session carries its own actor (CAD-526) and is
        // subject to the same agent/attribution rules.
        assert_eq!(
            decide(member(), Attribution::NoAgent, required),
            Verdict::Named(Named {
                actor: "Fable <f@e.co> (board)".into(),
                author: "fable".into(),
                operator: false,
            })
        );
        assert_eq!(
            decide(member(), agent(), required),
            Verdict::Stolen("pane-a".into())
        );
        assert!(matches!(
            decide(member(), Attribution::Unknown("x".into()), required),
            Verdict::Refuse("caller_identity", _)
        ));
        assert!(matches!(
            decide(None, Attribution::Foreign("sshd".into()), required),
            Verdict::Refuse("caller_identity", _)
        ));
        // Without one: never the operator, whatever the peer.
        assert_eq!(
            decide(None, agent(), required),
            Verdict::Agent("pane-a".into())
        );
        assert!(matches!(
            decide(None, Attribution::NoAgent, required),
            Verdict::Refuse("operator_session_required", _)
        ));
        assert!(matches!(
            decide(None, proxy("fable@example.com"), required),
            Verdict::Refuse("operator_session_required", _)
        ));
        assert!(matches!(
            decide(None, Attribution::Unknown("x".into()), required),
            Verdict::Refuse("caller_identity", _)
        ));
        // The public surface's sessionless refusal carries its own code.
        assert!(matches!(
            decide(
                None,
                Attribution::NoAgent,
                ("board_session_required", "sign in through your workspace"),
            ),
            Verdict::Refuse("board_session_required", _)
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
        assert_eq!(cookie_name(&opts, Origin::Public), PUBLIC_COOKIE);
        let l = cookie_attrs(Origin::Loopback, 60);
        assert!(l.contains("HttpOnly") && l.contains("SameSite=Strict") && l.contains("Path=/"));
        assert!(!l.contains("Secure") && !l.contains("Domain"));
        assert!(cookie_attrs(Origin::Tailnet, 60).contains("Secure"));
        // The contract's public-cookie set (§7): `__Host-` name, Secure,
        // Path=/, no Domain, HttpOnly, SameSite=Lax, 60-minute ceiling.
        let p = cookie_attrs(Origin::Public, 99999);
        assert!(p.contains("Secure") && p.contains("Path=/") && p.contains("HttpOnly"));
        assert!(p.contains("SameSite=Lax") && !p.contains("Domain"));
        assert!(p.contains("Max-Age=3600"), "{p}");
    }
}
