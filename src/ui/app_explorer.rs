//! CAD-1129: the board's `/api/app-*` explorer routes — the catalog,
//! home, favorites, requests and the soft-remove/restore relays.
//!
//! Reads ride `board_caller` like `wiki`'s: an operator session, a
//! named member's session, or an attributed agent peer. The caller's
//! `member_as` claim is the only member evidence the daemon accepts —
//! never a client-supplied field — and the board folds it in from the
//! verified `Caller`, exactly like `wiki_as`. The daemon re-proves it
//! against a live public member session before a member-shaped row
//! leaves, so a forged `member_as` cannot borrow a person's favorites
//! or request attribution.
//!
//! Writes stay the operator's alone — `admit`/`RouteClass` classifies
//! them before a handler runs. The member verbs the plan opens
//! (favorite, request) are `AgentAllowed` so a named member's session
//! writes as that member; the operator's own writes go through the
//! same route with `member_as` absent.

use serde_json::{json, Value};
use tiny_http::Request;

use super::{err_response, json_response, operator, read_body, HttpResp, ServeOpts};
use crate::client;
use std::path::Path;

const BODY_CAP: u64 = 48 * 1024;

/// The explorer's allowlist of callers: the operator (full rows) or a
/// named member's session (member rows, `member_as` re-proved by the
/// daemon). An agent peer is neither — the board relays over its own
/// operator connection, so relaying an agent would act as the operator.
/// Refused with 403 on every explorer route, read or write.
fn principal(caller: &operator::Caller) -> Result<Option<String>, HttpResp> {
    match caller {
        operator::Caller::Operator(_) => Ok(None),
        operator::Caller::Named(named) if named.operator => Ok(None),
        operator::Caller::Named(named) => Ok(Some(named.author.clone())),
        operator::Caller::Agent(_) => Err(err_response(
            403,
            "the apps explorer is for the operator and signed-in members; agents use the CLI",
        )),
    }
}

fn relay(state_dir: &Path, method: &str, mut params: Value, member: Option<&str>) -> HttpResp {
    if let Some(member) = member {
        params["member_as"] = json!(member);
    }
    match client::rpc(state_dir, method, params) {
        Ok(v) => json_response(v),
        Err(e) => super::home::rpc_err(&e, method),
    }
}

/// `relay` for a body-less attributed write: the server-held proof rides
/// alongside the path-derived params; no body is read.
fn relay_session(
    state_dir: &Path,
    method: &str,
    mut params: Value,
    proof: Option<&str>,
) -> HttpResp {
    if let Some(proof) = proof {
        params["public_session"] = json!(proof);
    }
    match client::rpc(state_dir, method, params) {
        Ok(v) => json_response(v),
        Err(e) => super::home::rpc_err(&e, method),
    }
}

fn board_caller(
    request: &Request,
    state_dir: &Path,
    opts: &ServeOpts,
) -> Result<operator::Caller, HttpResp> {
    operator::board_caller(request, state_dir, opts, false, None)
}

/// `GET /api/app-installations/{id}/automatic-updates`. Absence is off.
pub(super) fn automatic_updates_get(state_dir: &Path, install: &str) -> HttpResp {
    relay(
        state_dir,
        "app_automatic_updates_get",
        json!({"install_id": install}),
        None,
    )
}

/// GET reads: `/api/app-catalog`, `/api/app-catalog/<id>`,
/// `/api/app-home`, `/api/app-favorites`, `/api/app-requests`.
pub(super) fn read(
    request: &Request,
    path: &str,
    query: &dyn Fn(&str) -> Option<String>,
    state_dir: &Path,
    opts: &ServeOpts,
) -> HttpResp {
    let caller = match board_caller(request, state_dir, opts) {
        Ok(c) => c,
        Err(resp) => return resp,
    };
    let member = match principal(&caller) {
        Ok(m) => m,
        Err(resp) => return resp,
    };
    if path == "/api/app-catalog" {
        return relay(state_dir, "app_catalog_list", json!({}), member.as_deref());
    }
    if let Some(id) = path.strip_prefix("/api/app-catalog/") {
        if id.is_empty() || id.contains('/') {
            return err_response(404, "no such app-catalog route");
        }
        return relay(
            state_dir,
            "app_catalog_show",
            json!({"id": id}),
            member.as_deref(),
        );
    }
    if path == "/api/app-home" {
        return relay(state_dir, "app_home", json!({}), member.as_deref());
    }
    if path == "/api/app-favorites" {
        return relay(state_dir, "app_favorites_get", json!({}), member.as_deref());
    }
    if path == "/api/app-requests" {
        // The operator's Needs-you list: a member's session is refused
        // here, not relayed as the operator.
        if member.is_some() {
            return err_response(403, "install requests are the operator's list");
        }
        return relay(state_dir, "app_install_requests_list", json!({}), None);
    }
    let _ = query;
    err_response(404, "no such app explorer route")
}

/// One explorer write, chosen by the endpoint table's row id (CAD-1682):
/// the path is parsed once, by `endpoints::resolve`, so this handler never
/// re-derives its action from the URL.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum WriteAction {
    CatalogInstall,
    CatalogGitCheck,
    CatalogRequest,
    FavoriteSet,
    FavoriteDefault,
    FavoriteOpened,
    RequestDismiss,
    UpdateCheck,
    AutomaticUpdatesSet,
    RemovePreview,
    Remove,
    Restore,
}

/// POST writes admitted by `RouteClass`. `caller` is the admitted board
/// caller (only `AgentAllowed` routes reach member verbs); `install` is the
/// row's bound `{id}` for the per-installation verbs.
pub(super) fn write(
    request: &mut Request,
    action: WriteAction,
    install: Option<&str>,
    caller: Option<&operator::Caller>,
    proof_relay: operator::PublicRelay,
    state_dir: &Path,
) -> HttpResp {
    // No admitted caller, or an agent, never reaches a relay: the member
    // verbs below would otherwise run as the operator.
    let member = match caller.map(principal) {
        Some(Ok(m)) => m,
        Some(Err(resp)) => return resp,
        None => return err_response(401, "an operator or member session is required"),
    };
    let install = install.unwrap_or_default();
    match action {
        WriteAction::CatalogInstall => match public_proof(proof_relay) {
            Ok(proof) => relay_body(
                request,
                state_dir,
                "app_workspace_install_entry",
                json!({}),
                &["catalog_id", "expected_digest"],
                None,
                proof.as_deref(),
            ),
            Err(resp) => resp,
        },
        WriteAction::CatalogGitCheck => body_rpc(
            request,
            state_dir,
            "app_catalog_git_check",
            &["url", "git_ref", "dir"],
            None,
        ),
        WriteAction::UpdateCheck => relay(
            state_dir,
            "app_workspace_update_check",
            json!({"install_id": install}),
            None,
        ),
        WriteAction::AutomaticUpdatesSet => match public_proof(proof_relay) {
            Ok(proof) => body_rpc_install(
                request,
                state_dir,
                "app_automatic_updates_set",
                install,
                &["automatic"],
                proof.as_deref(),
            ),
            Err(resp) => resp,
        },
        WriteAction::RemovePreview => relay(
            state_dir,
            "app_workspace_remove_preview",
            json!({"install_id": install}),
            None,
        ),
        WriteAction::Remove => match public_proof(proof_relay) {
            Ok(proof) => body_rpc_install(
                request,
                state_dir,
                "app_workspace_remove",
                install,
                &["expected_generation", "expected_digest", "request_id"],
                proof.as_deref(),
            ),
            Err(resp) => resp,
        },
        WriteAction::Restore => match public_proof(proof_relay) {
            Ok(proof) => relay_session(
                state_dir,
                "app_workspace_restore",
                json!({"install_id": install}),
                proof.as_deref(),
            ),
            Err(resp) => resp,
        },
        WriteAction::FavoriteSet => match public_proof(proof_relay) {
            Ok(proof) => relay_body(
                request,
                state_dir,
                "app_favorites_put",
                json!({}),
                &["install_ids"],
                member.as_deref(),
                proof.as_deref(),
            ),
            Err(resp) => resp,
        },
        WriteAction::FavoriteDefault => match public_proof(proof_relay) {
            Ok(proof) => relay_body(
                request,
                state_dir,
                "app_favorites_put_default",
                json!({}),
                &["install_ids"],
                None,
                proof.as_deref(),
            ),
            Err(resp) => resp,
        },
        WriteAction::FavoriteOpened => body_rpc(
            request,
            state_dir,
            "app_favorites_opened",
            &["install_id"],
            member.as_deref(),
        ),
        WriteAction::CatalogRequest => match public_proof(proof_relay) {
            Ok(proof) => relay_body(
                request,
                state_dir,
                "app_install_request",
                json!({}),
                &["catalog_id"],
                member.as_deref(),
                proof.as_deref(),
            ),
            Err(resp) => resp,
        },
        WriteAction::RequestDismiss => match public_proof(proof_relay) {
            Ok(proof) => relay_body(
                request,
                state_dir,
                "app_install_request_dismiss",
                json!({}),
                &["id"],
                None,
                proof.as_deref(),
            ),
            Err(resp) => resp,
        },
    }
}

/// CAD-1374: the public session proof an attributed verb relays. A
/// public-host write with no bearer is refused before any RPC; a local
/// write carries none and the daemon names no human for it.
fn public_proof(relay: operator::PublicRelay) -> Result<Option<String>, HttpResp> {
    match relay {
        operator::PublicRelay::NotPublic => Ok(None),
        operator::PublicRelay::Proven(proof) => Ok(Some(proof.token().to_string())),
        operator::PublicRelay::Missing => {
            Err(err_response(401, "board session required for this write"))
        }
    }
}

/// Read a bounded JSON body, forward its allow-listed fields onto `base`
/// (a path-derived id, or empty), plus the caller's `member_as` when the
/// route admits a member and the attributed verb's `public_session` proof.
/// Unknown body fields are dropped.
fn relay_body(
    request: &mut Request,
    state_dir: &Path,
    method: &str,
    base: Value,
    fields: &[&str],
    member: Option<&str>,
    proof: Option<&str>,
) -> HttpResp {
    let bytes = match read_body(request, BODY_CAP) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let value: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => return err_response(400, "body must be a JSON object"),
    };
    let mut params = base;
    if let Some(obj) = value.as_object() {
        for f in fields {
            if let Some(v) = obj.get(*f) {
                params[*f] = v.clone();
            }
        }
    }
    if let Some(member) = member {
        params["member_as"] = json!(member);
    }
    if let Some(proof) = proof {
        params["public_session"] = json!(proof);
    }
    match client::rpc(state_dir, method, params) {
        Ok(v) => json_response(v),
        Err(e) => super::home::rpc_err(&e, method),
    }
}

fn body_rpc(
    request: &mut Request,
    state_dir: &Path,
    method: &str,
    fields: &[&str],
    member: Option<&str>,
) -> HttpResp {
    relay_body(request, state_dir, method, json!({}), fields, member, None)
}

/// Like `body_rpc`, but `install_id` comes from the path, not the
/// body — the caller never names the install in two places.
fn body_rpc_install(
    request: &mut Request,
    state_dir: &Path,
    method: &str,
    install_id: &str,
    fields: &[&str],
    proof: Option<&str>,
) -> HttpResp {
    relay_body(
        request,
        state_dir,
        method,
        json!({"install_id": install_id}),
        fields,
        None,
        proof,
    )
}

#[cfg(test)]
mod principal_acceptance {
    //! CAD-1129 F3 (reviewer-written): the explorer's caller allowlist.
    use super::*;
    use crate::ui::operator::{Caller, Named};

    fn named(operator: bool) -> Caller {
        Caller::Named(Named {
            actor: "A <a@x> (board)".into(),
            author: "alice".into(),
            sub: "sub_alice".into(),
            operator,
        })
    }

    #[test]
    fn only_the_operator_and_named_members_pass() {
        let ok = |c: Caller| match principal(&c) {
            Ok(m) => m,
            Err(_) => panic!("caller refused"),
        };
        assert_eq!(ok(Caller::Operator("operator (ui)".into())), None);
        assert_eq!(ok(named(true)), None);
        assert_eq!(ok(named(false)), Some("alice".to_string()));
        match principal(&Caller::Agent("writer".into())) {
            Ok(_) => panic!("an agent passed the explorer gate"),
            Err(refused) => assert_eq!(refused.status_code().0, 403),
        }
    }
}
