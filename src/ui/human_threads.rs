//! CAD-1412: `/api/human-threads/*` — the public-host HTTP surface for
//! human rooms (DMs and purpose-specific groups).
//!
//! The only credential is the `__Host-aos-board-session` cookie, read here
//! and relayed to the daemon as `token`. The daemon's `human_current_actor`
//! guard does the live roster check; this layer never reads a roster and
//! never takes identity from the request. Each route forwards a fixed
//! whitelist of fields, so a body or query can't add a field the RPC would
//! read.

use std::path::Path;

use serde_json::{json, Map, Value};
use tiny_http::{Method, Request};

use super::serve::json_response;
use super::write_path::{coded_response, read_body, write_guard, HttpResp};
use super::ServeOpts;
use crate::client;

/// The cap on a human-threads request body.
const BODY_CAP: u64 = 64 * 1024;
/// The longest history page a GET may ask for.
const HISTORY_MAX: i64 = 100;
/// Fields a body may never carry: identity is the session's, not the caller's.
const IDENTITY_FIELDS: &[&str] = &[
    "token",
    "issuer",
    "org",
    "organization_id",
    "subject",
    "as",
    "role",
    "actor",
    "sub",
];

fn bad_request(code: &str, message: &str) -> HttpResp {
    coded_response(400, code, message, None)
}

/// Keep only the named fields of `body`, refusing any identity field and any
/// field not on the route's whitelist.
fn whitelist(body: &Value, allowed: &[&str]) -> Result<Map<String, Value>, HttpResp> {
    let Some(obj) = body.as_object() else {
        return Err(bad_request(
            "invalid_request",
            "the body must be a JSON object",
        ));
    };
    for key in obj.keys() {
        if IDENTITY_FIELDS.contains(&key.as_str()) {
            return Err(bad_request(
                "identity_field_refused",
                &format!("'{key}' is not accepted: identity comes from the sign-in"),
            ));
        }
        if !allowed.contains(&key.as_str()) {
            return Err(bad_request(
                "invalid_request",
                &format!("'{key}' is not a field of this request"),
            ));
        }
    }
    Ok(obj.clone())
}

/// The `before`/`limit` pagination a history read carries, validated.
fn page_params(query: &str) -> Result<Map<String, Value>, HttpResp> {
    let mut out = Map::new();
    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        match k {
            "before" | "limit" => {
                let n: i64 = v.parse().map_err(|_| {
                    bad_request("invalid_request", &format!("{k} must be an integer"))
                })?;
                if k == "limit" && !(1..=HISTORY_MAX).contains(&n) {
                    return Err(bad_request(
                        "invalid_request",
                        &format!("limit must be 1-{HISTORY_MAX}"),
                    ));
                }
                if k == "before" && n < 0 {
                    return Err(bad_request("invalid_request", "before must be >= 0"));
                }
                out.insert(k.to_string(), json!(n));
            }
            // A query may not carry identity or any other field.
            _ => {
                return Err(bad_request(
                    "invalid_request",
                    &format!("'{k}' is not a query parameter of this request"),
                ))
            }
        }
    }
    Ok(out)
}

/// The member picker accepts only a search string, never identity selectors.
fn people_params(query: &str) -> Result<Map<String, Value>, HttpResp> {
    let mut out = Map::new();
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let key = super::pct_decode(key)
            .ok_or_else(|| bad_request("invalid_request", "invalid search query"))?;
        let value = super::pct_decode(&value.replace('+', " "))
            .ok_or_else(|| bad_request("invalid_request", "invalid search query"))?;
        if key != "query" || out.contains_key(&key) {
            return Err(bad_request(
                "invalid_request",
                "only one query parameter is accepted",
            ));
        }
        out.insert(key, json!(value));
    }
    Ok(out)
}

/// The room id a `/rooms/<id>...` path names, and its tail.
fn room_path(rest: &str) -> Option<(&str, &str)> {
    let tail = rest.strip_prefix("/rooms/")?;
    let (room, after) = match tail.find('/') {
        Some(i) => (&tail[..i], &tail[i..]),
        None => (tail, ""),
    };
    (!room.is_empty()).then_some((room, after))
}

/// `/rooms/<id>/members/<kind>/<member>`.
fn member_path(rest: &str) -> Option<(&str, &str, &str)> {
    let tail = rest.strip_prefix("/rooms/")?;
    let mut parts = tail.split('/');
    let room = parts.next()?;
    if parts.next()? != "members" {
        return None;
    }
    let kind = parts.next()?;
    let member = parts.next()?;
    if parts.next().is_some() || room.is_empty() || member.is_empty() {
        return None;
    }
    Some((room, kind, member))
}

/// One route: its RPC, the server-owned params (room), and the body fields it
/// may forward.
struct Route {
    rpc: &'static str,
    server: Map<String, Value>,
    body_fields: &'static [&'static str],
}

fn room_route(rpc: &'static str, room: &str, body_fields: &'static [&'static str]) -> Route {
    let mut server = Map::new();
    server.insert("room".to_string(), json!(room));
    Route {
        rpc,
        server,
        body_fields,
    }
}

fn route(method: &Method, rest: &str) -> Result<(Route, bool), HttpResp> {
    let not_found = || coded_response(404, "not_found", "no such human-threads route", None);
    match (method, rest) {
        (Method::Get, "/me") => Ok((
            Route {
                rpc: "human_actor_check",
                server: Map::new(),
                body_fields: &[],
            },
            false,
        )),
        (Method::Get, "/people") => Ok((
            Route {
                rpc: "human_people_search",
                server: Map::new(),
                body_fields: &[],
            },
            false,
        )),
        (Method::Get, "/agents") => Ok((
            Route {
                rpc: "human_agents_list",
                server: Map::new(),
                body_fields: &[],
            },
            false,
        )),
        (Method::Get, "/notifications") => Ok((
            Route {
                rpc: "human_notifications",
                server: Map::new(),
                body_fields: &[],
            },
            false,
        )),
        (Method::Post, "/notifications/read") => Ok((
            Route {
                rpc: "human_notifications_read",
                server: Map::new(),
                body_fields: &[],
            },
            false,
        )),
        (Method::Post, "/notifications/preferences") => Ok((
            Route {
                rpc: "human_notification_prefs_set",
                server: Map::new(),
                body_fields: &["dm", "mention", "sound", "preview", "quiet", "zone"],
            },
            false,
        )),
        (Method::Post, "/push/presence") => Ok((
            Route {
                rpc: "human_push_presence",
                server: Map::new(),
                body_fields: &["device_id", "tab_id", "room"],
            },
            false,
        )),
        (Method::Post, "/push/subscriptions") => Ok((
            Route {
                rpc: "human_push_subscribe",
                server: Map::new(),
                body_fields: &["endpoint", "keys"],
            },
            false,
        )),
        (Method::Post, "/push/subscriptions/rebind") => Ok((
            Route {
                rpc: "human_push_rebind",
                server: Map::new(),
                body_fields: &["endpoint"],
            },
            false,
        )),

        (Method::Delete, p) if p.starts_with("/push/subscriptions/") => {
            let device = &p["/push/subscriptions/".len()..];
            if device.is_empty() || device.contains('/') {
                return Err(not_found());
            }
            let mut r = Route {
                rpc: "human_push_unsubscribe",
                server: Map::new(),
                body_fields: &[],
            };
            r.server.insert("device_id".to_string(), json!(device));
            Ok((r, false))
        }
        (Method::Get, "/rooms") => Ok((
            Route {
                rpc: "human_rooms_list",
                server: Map::new(),
                body_fields: &[],
            },
            false,
        )),
        (Method::Post, "/dm") => Ok((
            Route {
                rpc: "human_dm_open",
                server: Map::new(),
                body_fields: &["peer_subject"],
            },
            false,
        )),
        (Method::Post, "/groups") => Ok((
            Route {
                rpc: "human_group_create",
                server: Map::new(),
                body_fields: &["kind", "title", "client_id", "humans", "agents"],
            },
            false,
        )),
        (Method::Get, p) => match room_path(p) {
            Some((room, "")) => Ok((room_route("human_room_read", room, &[]), true)),
            _ => Err(not_found()),
        },
        (Method::Post, p) => match room_path(p) {
            Some((room, "/posts")) => Ok((
                room_route("human_room_post", room, &["client_id", "body"]),
                false,
            )),
            Some((room, "/ask")) => Ok((
                room_route("human_room_ask", room, &["agent_uid", "client_id", "body"]),
                false,
            )),
            Some((room, "/read")) => {
                Ok((room_route("human_room_mark_read", room, &["seq"]), false))
            }
            Some((room, "/mute")) => Ok((room_route("human_room_mute", room, &["muted"]), false)),
            Some((room, "/members")) => Ok((
                room_route("human_room_add", room, &["human_subject", "agent_uid"]),
                false,
            )),
            _ => Err(not_found()),
        },
        (Method::Delete, p) => match member_path(p) {
            Some((room, kind, member)) => {
                let mut r = room_route("human_room_remove", room, &[]);
                r.server.insert("kind".to_string(), json!(kind));
                r.server.insert("member".to_string(), json!(member));
                Ok((r, false))
            }
            None => Err(not_found()),
        },
        _ => Err(not_found()),
    }
}

/// Route one `/api/human-threads/*` request. The caller has already checked
/// the public host and the session gate. `path` is the decoded path; `query`
/// the raw query string.
pub(crate) fn dispatch(
    request: &mut Request,
    method: &Method,
    path: &str,
    query: &str,
    state_dir: &Path,
    opts: &ServeOpts,
) -> HttpResp {
    // Every write carries the board's custom-header proof before any body
    // read or daemon call: exact JSON type, `X-Cadence-Board: 1`, and an
    // allowed Origin / same-origin Fetch-Site. A cookie alone is no CSRF
    // proof, so a cross-site form cannot post here.
    if matches!(method, Method::Post | Method::Delete) {
        if let Err(resp) = write_guard(request, "application/json", opts) {
            return resp;
        }
    }
    let Some(cookie) =
        super::operator::session_token_for(request, opts, crate::operator_auth::Origin::Public)
    else {
        return coded_response(401, "signed_out", "sign in to use human threads", None);
    };
    let rest = path.trim_start_matches("/api/human-threads");
    let (route, history) = match route(method, rest) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let mut params = Map::new();
    if *method == Method::Post {
        let raw = match read_body(request, BODY_CAP) {
            Ok(b) => b,
            Err(resp) => return resp,
        };
        let body: Value = match serde_json::from_slice(&raw) {
            Ok(v) => v,
            Err(_) => return bad_request("invalid_request", "the body must be JSON"),
        };
        match whitelist(&body, route.body_fields) {
            Ok(mut fields) => {
                // The public push contract nests the browser keys; the daemon
                // reads them flat. Only the two named keys cross over.
                if let Some(keys) = fields.remove("keys") {
                    match push_keys(&keys) {
                        Ok((p256dh, auth)) => {
                            fields.insert("p256dh".to_string(), json!(p256dh));
                            fields.insert("auth".to_string(), json!(auth));
                        }
                        Err(resp) => return resp,
                    }
                }
                params.extend(fields)
            }
            Err(resp) => return resp,
        }
    } else if route.rpc == "human_people_search" {
        match people_params(query) {
            Ok(search) => params.extend(search),
            Err(resp) => return resp,
        }
    } else if history {
        match page_params(query) {
            Ok(page) => params.extend(page),
            Err(resp) => return resp,
        }
    } else if !query.is_empty() {
        return bad_request("invalid_request", "this route takes no query parameters");
    }
    params.extend(route.server);
    params.insert("token".to_string(), json!(cookie));
    match client::rpc(state_dir, route.rpc, Value::Object(params)) {
        Ok(v) => json_response(v),
        Err(e) => daemon_failure(&e),
    }
}

/// The browser's push keys: exactly `{p256dh, auth}`, both strings.
fn push_keys(keys: &Value) -> Result<(String, String), HttpResp> {
    let invalid = || bad_request("invalid_request", "keys must be {p256dh, auth} strings");
    let obj = keys.as_object().ok_or_else(invalid)?;
    if obj.len() != 2 {
        return Err(invalid());
    }
    let p256dh = obj
        .get("p256dh")
        .and_then(Value::as_str)
        .ok_or_else(invalid)?;
    let auth = obj
        .get("auth")
        .and_then(Value::as_str)
        .ok_or_else(invalid)?;
    Ok((p256dh.to_string(), auth.to_string()))
}

/// Map a daemon refusal to the HTTP contract. Codes come from the daemon's
/// own `code` where it set one.
fn daemon_failure(error: &crate::error::Error) -> HttpResp {
    if error.code() == Some("human_session_required") {
        return coded_response(
            401,
            "signed_out",
            "sign in again to use human threads",
            None,
        );
    }
    if error.code() == Some("tenancy_mismatch") {
        return coded_response(
            403,
            "tenancy_mismatch",
            "this sign-in does not match this board",
            None,
        );
    }
    if error.code() == Some("not_a_member") {
        return coded_response(
            403,
            "not_a_member",
            "you are not a current member of this organization",
            None,
        );
    }
    if error.code() == Some("roster_unavailable") {
        return coded_response(
            503,
            "roster_unavailable",
            "the current team roster is unavailable",
            None,
        );
    }
    let message = error.to_string();
    if message.contains("Unknown method") {
        return coded_response(
            501,
            "unsupported_daemon",
            "this daemon does not support human threads — upgrade it",
            None,
        );
    }
    coded_response(400, "rejected", &message, None)
}
