//! Board endpoints behind the chat-first Home (CAD-328). The thread
//! itself is `/api/threads/master` (threads.rs); these are the operator's
//! decisions on the cards around it and the "since you left" read:
//!
//! - `POST /api/plans/<EPIC>/approve` `{}` and
//!   `POST /api/plans/<EPIC>/reject` `{"reason"}` — relay the daemon's
//!   operator-only `plan_approve` / `plan_reject` (CAD-360).
//! - `POST /api/issues/<ID>/answers` `{"question", "text"}` — file an
//!   `answer` report (CAD-341) on the question report `question`,
//!   authored `operator`, then hand it to the daemon's `answer_route`
//!   (CAD-447) so the question's author is told; the reply's `route`
//!   says whether it was.
//! - `POST /api/delivery/<ID>/merge` `{}` and
//!   `POST /api/delivery/<ID>/decline` `{"reason"}` — the worker loop's
//!   merge decision (CAD-431). Merge runs [`crate::delivery::merge`] in
//!   this board process — the operator's own `gh` enqueues the PR pinned
//!   to the reviewed head; the daemon never runs `gh`. Decline relays the
//!   daemon's operator-only `delivery_decline`.
//! - `GET /api/master/summary?since=<epoch secs>` — relay the daemon's
//!   `master_summary` (CAD-339) without posting it; a daemon without the
//!   method answers 501 so the UI shows "not available".
//!
//! Who is trusted as the operator here (CAD-313, CAD-428): every route
//! in this file is operator-only in `operator::WRITE_ROUTES`, so
//! `operator::admit` has admitted the request before any handler here
//! runs — exactly a request that passes all of:
//!
//! 1. read-only off, and the cross-site guards (`write_guard`);
//! 2. a live **operator session** ([`operator::board_caller`]): the
//!    HttpOnly cookie a `cadence ui login` link was exchanged for plus
//!    the page's `X-Cadence-Session` key, presented on the origin it was issued for (this board's own
//!    `cadence-<port>.localhost` name, or the proven `tailscale serve`
//!    proxy) with that origin's `Origin` header, by a peer tied to no
//!    agent — or whose socket is another uid's proxy. No session is
//!    refused `operator_session_required` — whatever relays it (nginx,
//!    `socat`, a gateway vhost), whatever Host, `Tailscale-*` or
//!    `X-Forwarded-*` header it carries, and however detached from its
//!    pane the sender is. A peer tied to an agent is refused
//!    `operator_only` (and a session it presents is revoked,
//!    `session_from_agent`); a session-bearing peer the board cannot
//!    attribute (its socket closed early) is refused `caller_identity`;
//! 3. positive process proof on the HTTP peer
//!    ([`prove_operator_peer`]), the rule the daemon applies to its own
//!    operator verbs — the board is never less strict than the RPC it
//!    relays. The proven tailnet proxy passes: its peer is tailscaled.
//!
//! No request field names who decided: the request bodies deny unknown
//! fields, and the daemon derives the operator from the board's
//! connection. Residual (ADR 0004 §1.3, phase 2 under CAD-280): any
//! same-uid process that passes `operator_proof` — one cadence did not
//! launch passes with no evasion at all (an agent CLI the operator
//! started by hand), an agent's `setsid -f` child with a scrubbed env
//! and stdio passes by evasion — and reads the operator secret can mint
//! its own login link.

use serde::Deserialize;
use serde_json::json;
use tiny_http::Request;

use super::{
    agent_roots, coded_response, err_response, guard_fail, json_response, operator, parse_json,
    read_body, tailnet_proxy, write_reply, HttpResp, ServeOpts,
};
use crate::client;
use crate::error::Error;
use crate::issue::{model, task_report, Pm};

/// A decision or answer body — a reason or an answer plus JSON.
const BODY_CAP: u64 = 64 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DecideReq {
    reason: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AnswerReq {
    /// The question report's file name on the same ticket.
    question: String,
    text: String,
}

/// `(epic, verb)` for `/api/plans/<epic>/<verb>`, `None` when the path
/// is not a plan route.
pub(super) fn plan_route(path: &str) -> Option<(&str, &str)> {
    let tail = path.strip_prefix("/api/plans/")?;
    let mut segs = tail.splitn(2, '/');
    Some((
        segs.next().unwrap_or_default(),
        segs.next().unwrap_or_default(),
    ))
}

/// The issue id of `/api/issues/<id>/answers`, `None` otherwise.
pub(super) fn answer_route(path: &str) -> Option<&str> {
    let id = path
        .strip_prefix("/api/issues/")?
        .strip_suffix("/answers")?;
    (!id.is_empty() && !id.contains('/')).then_some(id)
}

/// Whether this client may make the operator's board decisions — what
/// `/api/meta?operator=1` reports so the UI offers them only to the
/// operator. The same checks `operator::admit` runs on an operator-only
/// write, minus the browser write guards: a writable board, a live
/// operator session (CAD-313) presented by a caller tied to no agent,
/// and the positive proof on the peer. Anything unprovable is `false`.
/// The UI's answer is a courtesy; every write still runs the full check.
///
/// The board relays decisions over its OWN daemon connection, so the
/// board process must be the operator's too ([`board_is_operator`]): a
/// board an agent started would have the daemon refuse every relayed
/// decision, and shows no buttons.
pub(super) fn operator_viewer(
    request: &Request,
    state_dir: &std::path::Path,
    opts: &ServeOpts,
) -> bool {
    if opts.read_only {
        return false;
    }
    match operator::board_caller(request, state_dir, opts, false) {
        // CAD-526: a public session's verified `owner` role is the
        // operator claim on that surface — the platform relay's peer is
        // not a process this host can prove, and need not be.
        Ok(operator::Caller::Named(named)) => {
            named.operator && board_is_operator(state_dir, opts.seam.is_some())
        }
        Ok(operator::Caller::Operator(_)) => {
            prove_operator_peer(request, state_dir, opts, "reading the operator role").is_ok()
                && board_is_operator(state_dir, opts.seam.is_some())
        }
        _ => false,
    }
}

/// The board process itself passes the operator proof the daemon will
/// run on its connection ([`crate::peer::operator_proof`]): no pane or
/// managed provider on its ancestry, not a daemon descendant, no agent
/// environment, a session leader on its ancestry. Unprovable is false.
/// `seam_armed` is the fixture's own declaration — CAD-482 — that this
/// board runs as the operator's: a board attached to a credential that
/// actually exists *is* its operator's board, exactly as a pane-free
/// `ui run` is in production — unless the board process itself asserts
/// a non-operator identity (`CADENCE_TEST_AS=agent:<alias>` marks a
/// fixture board an agent started, identical in a pane and in CI).
/// `armed` answers live, so a board started before its daemon mints
/// becomes operator's when the token lands.
pub(super) fn board_is_operator(state_dir: &std::path::Path, seam_armed: bool) -> bool {
    if seam_armed && crate::test_seam::armed(state_dir) {
        // An unparseable CADENCE_TEST_AS (Err) is not the operator —
        // a misspelled assertion refuses loudly, never ambient.
        return matches!(
            crate::test_seam::env_asserted(),
            Ok(None) | Ok(Some(crate::test_seam::Asserted::Operator))
        );
    }
    let Some(daemon_pid) = client::rpc(state_dir, "health", json!({}))
        .ok()
        .and_then(|h| h["pid"].as_u64())
        .and_then(|p| u32::try_from(p).ok())
    else {
        return false;
    };
    let Ok(roots) = agent_roots(state_dir) else {
        return false;
    };
    // SAFETY: geteuid has no preconditions and cannot fail.
    let uid = unsafe { libc::geteuid() };
    // Fenced deny lists (CAD-385): a reused pid denies nothing, a row
    // with no recorded start keeps denying.
    roots
        .operator_proof(std::process::id(), uid, daemon_pid)
        .is_ok()
}

/// CAD-276's positive operator proof, run on the board's TCP peer — the
/// rule the daemon applies to its own operator verbs
/// ([`crate::peer::tcp_peer_operator_proof`]): the peer walks cleanly,
/// no registered pane or managed provider is on its ancestry, it does
/// not descend from the daemon process (a detached child of a
/// daemon-launched tool re-parents to the daemon under `daemon run`),
/// it carries no agent environment and holds no pane pty, and its
/// session leader is on its ancestry. A request proven to come through
/// `tailscale serve` (CAD-336) is the tailnet login's and passes: its
/// peer is tailscaled. Anything unprovable — a daemon that cannot say
/// its pid, agents it cannot list — refuses with 403 `operator_proof`,
/// before anything is written.
pub(super) fn prove_operator_peer(
    request: &Request,
    state_dir: &std::path::Path,
    opts: &ServeOpts,
    what: &str,
) -> std::result::Result<(), HttpResp> {
    let refuse = |why: String| {
        guard_fail(
            "operator_proof",
            &format!(
                "{what} refused: this request is not provably from the operator — {why}. \
                 Decide from the operator's own browser or shell, outside every pane \
                 and managed endpoint"
            ),
        )
    };
    // CAD-482: a seam assertion answers here exactly as it does on the
    // daemon — `operator` proves, anything else refuses, identically in
    // a pane and in CI.
    if let Some(asserted) = crate::test_seam::asserted() {
        return match asserted {
            crate::test_seam::Asserted::Operator => Ok(()),
            other => Err(refuse(format!(
                "the request's seam assertion is '{}', not the operator's",
                other.as_str()
            ))),
        };
    }
    if matches!(tailnet_proxy(request, opts), Some(Ok(()))) {
        return Ok(());
    }
    let daemon_pid = client::rpc(state_dir, "health", json!({}))
        .ok()
        .and_then(|h| h["pid"].as_u64())
        .and_then(|p| u32::try_from(p).ok())
        .ok_or_else(|| refuse("the daemon's pid cannot be read".to_string()))?;
    let roots = agent_roots(state_dir).map_err(refuse)?;
    let peer = request
        .remote_addr()
        .ok_or_else(|| refuse("the request has no peer address".to_string()))?;
    // SAFETY: geteuid has no preconditions and cannot fail.
    let uid = unsafe { libc::geteuid() };
    crate::peer::tcp_peer_operator_proof(opts.port, *peer, uid, daemon_pid, &roots).map_err(refuse)
}

/// Daemon error → HTTP for the relayed operator RPCs.
pub(super) fn rpc_err(e: &Error, method: &str) -> HttpResp {
    let text = e.to_string();
    if text.starts_with("Daemon is not reachable") {
        return coded_response(503, "daemon_unavailable", &text, None);
    }
    if text.contains(&format!("Unknown method '{method}'")) {
        return coded_response(
            501,
            "unsupported_daemon",
            &format!("this daemon does not support {method}"),
            None,
        );
    }
    // The daemon's own operator gate refused the board's connection.
    if text.contains("operator action") || text.contains("not provably the operator") {
        return coded_response(403, "operator_proof", &text, None);
    }
    match e {
        Error::Internal(_) => err_response(500, &text),
        Error::Rejected(m) if m.starts_with("Unknown issue") => {
            coded_response(404, "unknown_issue", m, None)
        }
        Error::Rejected(m) if m.contains("already") => coded_response(409, "decided", m, None),
        _ if e.kind() == "conflict" => {
            coded_response(409, e.code().unwrap_or("conflict"), &text, e.revision())
        }
        _ => coded_response(400, e.code().unwrap_or("invalid_request"), &text, None),
    }
}

/// `POST /api/plans/<epic>/approve|reject`.
pub(super) fn decide_plan(
    request: &mut Request,
    state_dir: &std::path::Path,
    epic: &str,
    verb: &str,
) -> HttpResp {
    let approve = match verb {
        "approve" => true,
        "reject" => false,
        _ => return err_response(404, "no such plan route"),
    };
    let Ok(epic) = model::check_id(epic) else {
        return err_response(400, "bad plan epic id");
    };
    let bytes = match read_body(request, BODY_CAP) {
        Ok(bytes) => bytes,
        Err(resp) => return resp,
    };
    let req: DecideReq = match parse_json(&bytes) {
        Ok(req) => req,
        Err(resp) => return resp,
    };
    let reason = req
        .reason
        .as_deref()
        .map(str::trim)
        .filter(|r| !r.is_empty());
    let mut params = json!({ "epic": epic });
    if approve {
        if reason.is_some() {
            return err_response(400, "an approval takes no reason");
        }
    } else {
        let Some(reason) = reason else {
            return coded_response(400, "reason_required", "a rejection needs a reason", None);
        };
        params["reason"] = json!(reason);
    }
    let method = if approve {
        "plan_approve"
    } else {
        "plan_reject"
    };
    match client::rpc(state_dir, method, params) {
        Ok(out) => json_response(out),
        Err(e) => rpc_err(&e, method),
    }
}

/// `(issue, verb)` for `/api/delivery/<id>/<verb>`, `None` otherwise.
pub(super) fn delivery_route(path: &str) -> Option<(&str, &str)> {
    let tail = path.strip_prefix("/api/delivery/")?;
    let (id, verb) = tail.split_once('/')?;
    (!id.is_empty() && !verb.contains('/')).then_some((id, verb))
}

/// `POST /api/delivery/<id>/merge|decline` (CAD-431) — the same
/// operator rule as the plan decision (`operator::admit`).
pub(super) fn decide_delivery(
    request: &mut Request,
    state_dir: &std::path::Path,
    opts: &ServeOpts,
    id: &str,
    verb: &str,
) -> HttpResp {
    let merge = match verb {
        "merge" => true,
        "decline" => false,
        _ => return err_response(404, "no such delivery route"),
    };
    let Ok(id) = model::check_id(id) else {
        return err_response(400, "bad issue id");
    };
    let bytes = match read_body(request, BODY_CAP) {
        Ok(bytes) => bytes,
        Err(resp) => return resp,
    };
    let req: DecideReq = match parse_json(&bytes) {
        Ok(req) => req,
        Err(resp) => return resp,
    };
    let reason = req
        .reason
        .as_deref()
        .map(str::trim)
        .filter(|r| !r.is_empty());
    let (out, method) = if merge {
        if reason.is_some() {
            return err_response(400, "a merge takes no reason");
        }
        let gh = opts
            .gh
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| crate::delivery::GH.to_string());
        (
            crate::delivery::merge(state_dir, &id, &gh),
            "delivery_merge",
        )
    } else {
        let Some(reason) = reason else {
            return coded_response(400, "reason_required", "a decline needs a reason", None);
        };
        (
            client::rpc(
                state_dir,
                "delivery_decline",
                json!({"issue": id, "reason": reason}),
            ),
            "delivery_decline",
        )
    };
    match out {
        Ok(out) => json_response(out),
        Err(e) => rpc_err(&e, method),
    }
}

/// `POST /api/issues/<id>/answers` — the operator's answer to one open
/// question report. Answers `{issue, card, warnings}` like every issue
/// write, so the drawer and the rail update without a second fetch.
pub(super) fn answer(
    request: &mut Request,
    state_dir: &std::path::Path,
    pm_dir: &std::path::Path,
    actor: &str,
    id: &str,
) -> HttpResp {
    let Ok(id) = model::check_id(id) else {
        return err_response(400, "bad issue id");
    };
    let bytes = match read_body(request, BODY_CAP) {
        Ok(bytes) => bytes,
        Err(resp) => return resp,
    };
    let req: AnswerReq = match parse_json(&bytes) {
        Ok(req) => req,
        Err(resp) => return resp,
    };
    let pm = match Pm::at(pm_dir) {
        Ok(pm) => pm,
        Err(e) => return err_response(503, &e.to_string()),
    };
    // The author is the operator — derived above, never read from the
    // request or from this process's environment.
    let filed = task_report::prepare_answer(&pm, &id, &req.question, &req.text, "operator")
        .and_then(|p| task_report::store(&pm, &p, actor));
    match filed {
        Ok(out) => {
            // CAD-447: the accepted answer goes to the question's author.
            // The answer stands whatever the daemon says; its reply rides
            // along as `route`.
            let mut out = out;
            let report = out["report"].as_str().unwrap_or_default().to_string();
            out["route"] = client::route_answer(state_dir, &id, &report);
            // Best effort: wake the daemon's report router (CAD-339). A
            // daemon without the method, or one that is down, just waits.
            let _ = client::rpc(state_dir, "reports_changed", json!({}));
            write_reply(&pm, state_dir, &id, out, true)
        }
        Err(e) => super::write_err(&e),
    }
}

/// `GET /api/master/summary?since=<epoch secs>`.
pub(super) fn master_summary(
    state_dir: &std::path::Path,
    query: &dyn Fn(&str) -> Option<String>,
) -> HttpResp {
    let Some(since) = query("since")
        .and_then(|v| v.trim().parse::<i64>().ok())
        .filter(|n| *n >= 0)
    else {
        return err_response(400, "since must be epoch seconds");
    };
    match client::rpc(state_dir, "master_summary", json!({ "since": since })) {
        Ok(out) => json_response(out),
        Err(e) => rpc_err(&e, "master_summary"),
    }
}

/// `GET /api/outbox[?effect_id=]` (CAD-546) — the `local` platform's
/// outbox listing, relayed through the daemon's operator-only
/// `platform_outbox`.
pub(super) fn outbox(
    request: &Request,
    state_dir: &std::path::Path,
    opts: &ServeOpts,
    effect_id: Option<String>,
) -> HttpResp {
    if let Err(resp) = outbox_gate(request, state_dir, opts, "GET /api/outbox") {
        return resp;
    }
    let params = match effect_id {
        Some(eid) => json!({ "effect_id": eid }),
        None => json!({}),
    };
    match client::rpc(state_dir, "platform_outbox", params) {
        Ok(out) => json_response(out),
        Err(e) => rpc_err(&e, "platform_outbox"),
    }
}

/// The proof every outbox read runs (CAD-546; the app outputs read
/// relays the same ledger, CAD-563): a live operator session on an
/// unattributable caller ([`operator::board_caller`]) — on the public
/// surface the verified `owner` role is the operator claim (CAD-526) —
/// the positive peer proof ([`prove_operator_peer`]) for the loopback
/// claim — the board is never less strict than the RPC it relays — and
/// a board process the daemon itself accepts as operator
/// ([`board_is_operator`]), since the relay crosses the daemon
/// connection in the board's own name. `what` names the route in the
/// refusals.
pub(super) fn outbox_gate(
    request: &Request,
    state_dir: &std::path::Path,
    opts: &ServeOpts,
    what: &str,
) -> std::result::Result<(), HttpResp> {
    match operator::board_caller(request, state_dir, opts, false) {
        Ok(operator::Caller::Operator(_)) => {
            prove_operator_peer(request, state_dir, opts, what)?;
        }
        // CAD-526: the verified `owner` role is the operator claim on
        // the public surface — the platform relay's peer is not a
        // process this host can prove; a member never reads the ledger.
        Ok(operator::Caller::Named(named)) => {
            if !named.operator {
                return Err(guard_fail(
                    "member_role",
                    &format!(
                        "{what} needs the board owner's role — this session is \
                         {}'s, mapped `member`",
                        named.actor
                    ),
                ));
            }
        }
        Ok(operator::Caller::Agent(alias)) => {
            return Err(guard_fail(
                "operator_only",
                &format!(
                    "{what} is the operator's read — this request comes from \
                     agent '{alias}'; read it from the operator's browser"
                ),
            ));
        }
        Err(resp) => return Err(resp),
    }
    if !board_is_operator(state_dir, opts.seam.is_some()) {
        return Err(guard_fail(
            "operator_proof",
            &format!(
                "{what} refused: this board process is not provably the \
                 operator's — the daemon would refuse its relay"
            ),
        ));
    }
    Ok(())
}
