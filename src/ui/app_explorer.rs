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
use tiny_http::{Method, Request};

use super::{err_response, json_response, operator, read_body, write_err, HttpResp, ServeOpts};
use crate::client;
use std::path::Path;

const BODY_CAP: u64 = 48 * 1024;

/// The `member_as` claim this request's caller carries — only a named
/// member's session supplies one; the operator's and an agent's are
/// `None`, so those callers get full rows.
fn member_as(caller: &operator::Caller) -> Option<String> {
    match caller {
        operator::Caller::Named(named) if !named.operator => Some(named.author.clone()),
        _ => None,
    }
}

fn relay(state_dir: &Path, method: &str, mut params: Value, member: Option<&str>) -> HttpResp {
    if let Some(member) = member {
        params["member_as"] = json!(member);
    }
    match client::rpc(state_dir, method, params) {
        Ok(v) => json_response(v),
        Err(e) => write_err(&e),
    }
}

fn board_caller(
    request: &Request,
    state_dir: &Path,
    opts: &ServeOpts,
) -> Result<operator::Caller, HttpResp> {
    operator::board_caller(request, state_dir, opts, false)
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
    let member = member_as(&caller);
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
        // The operator's Needs-you list — operator-only at the caller.
        return relay(
            state_dir,
            "app_install_requests_list",
            json!({}),
            member.as_deref(),
        );
    }
    let _ = query;
    err_response(404, "no such app explorer route")
}

/// POST writes admitted by `RouteClass`. `caller` is the admitted
/// board caller (only `AgentAllowed` routes reach member verbs).
pub(super) fn write(
    request: &mut Request,
    method: &Method,
    path: &str,
    caller: Option<&operator::Caller>,
    state_dir: &Path,
) -> HttpResp {
    if *method != Method::Post {
        return err_response(405, "method not allowed");
    }
    let member = caller.and_then(member_as);
    match path {
        // The operator-only writes relay over the board's own connection.
        "/api/app-catalog/install" => body_rpc(
            request,
            state_dir,
            "app_workspace_install_entry",
            &["catalog_id", "expected_digest"],
            None,
        ),
        "/api/app-catalog/git-check" => body_rpc(
            request,
            state_dir,
            "app_catalog_git_check",
            &["url", "git_ref", "dir"],
            None,
        ),
        p if p.starts_with("/api/app-installations/") => {
            let tail = &p["/api/app-installations/".len()..];
            let mut segs = tail.split('/');
            let (id, verb) = (
                segs.next().unwrap_or_default(),
                segs.next().unwrap_or_default(),
            );
            if id.is_empty() {
                return err_response(400, "missing installation id");
            }
            match verb {
                "update-check" => relay(
                    state_dir,
                    "app_workspace_update_check",
                    json!({"install_id": id}),
                    None,
                ),
                "remove-preview" => relay(
                    state_dir,
                    "app_workspace_remove_preview",
                    json!({"install_id": id}),
                    None,
                ),
                "remove" => body_rpc_install(
                    request,
                    state_dir,
                    "app_workspace_remove",
                    id,
                    &["expected_generation", "expected_digest", "request_id"],
                ),
                "restore" => relay(
                    state_dir,
                    "app_workspace_restore",
                    json!({"install_id": id}),
                    None,
                ),
                _ => err_response(404, "no such app-installation route"),
            }
        }
        // Member verbs: favorites + requests (AgentAllowed).
        "/api/app-favorites" => body_rpc(
            request,
            state_dir,
            "app_favorites_put",
            &["install_ids"],
            member.as_deref(),
        ),
        "/api/app-favorites/default" => body_rpc(
            request,
            state_dir,
            "app_favorites_put_default",
            &["install_ids"],
            None,
        ),
        "/api/app-favorites/opened" => body_rpc(
            request,
            state_dir,
            "app_favorites_opened",
            &["install_id"],
            member.as_deref(),
        ),
        "/api/app-catalog/request" => body_rpc(
            request,
            state_dir,
            "app_install_request",
            &["catalog_id"],
            member.as_deref(),
        ),
        "/api/app-requests/dismiss" => body_rpc(
            request,
            state_dir,
            "app_install_request_dismiss",
            &["id"],
            None,
        ),
        _ => err_response(404, "no such app explorer write"),
    }
}

/// Read a bounded JSON body, forward the allow-listed fields plus the
/// caller's `member_as` (when the route admits a member).
fn body_rpc(
    request: &mut Request,
    state_dir: &Path,
    method: &str,
    fields: &[&str],
    member: Option<&str>,
) -> HttpResp {
    let bytes = match read_body(request, BODY_CAP) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let value: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => return err_response(400, "body must be a JSON object"),
    };
    let mut params = json!({});
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
    match client::rpc(state_dir, method, params) {
        Ok(v) => json_response(v),
        Err(e) => write_err(&e),
    }
}

/// Like `body_rpc`, but `install_id` comes from the path, not the
/// body — the caller never names the install in two places.
fn body_rpc_install(
    request: &mut Request,
    state_dir: &Path,
    method: &str,
    install_id: &str,
    fields: &[&str],
) -> HttpResp {
    let bytes = match read_body(request, BODY_CAP) {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let value: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => return err_response(400, "body must be a JSON object"),
    };
    let mut params = json!({"install_id": install_id});
    if let Some(obj) = value.as_object() {
        for f in fields {
            if let Some(v) = obj.get(*f) {
                params[*f] = v.clone();
            }
        }
    }
    match client::rpc(state_dir, method, params) {
        Ok(v) => json_response(v),
        Err(e) => write_err(&e),
    }
}
