//! CAD-1110: `GET /api/app-installations/<id>/chat-descriptor` — the
//! board's read-only relay of the daemon's `app_chat_descriptor`
//! (contract: docs/design/CAD-1109-app-chat-v1.md, "Routing and APIs").
//!
//! The install id comes from the path only. `serve.rs` runs
//! `admit_operator_read` (the same operator proof as every sibling
//! `/api/app-installations/*` read) before this handler, and the daemon
//! re-proves its own `operator_connection` on the relayed call, so the
//! HTTP peer is at least as strict as the RPC it relays (relay parity).
//! 200 carries `{descriptor, digest, app}` with `Cache-Control: no-store`
//! (the client caches by `(install, digest)`, the server never serves a
//! cacheable body); every "no descriptor" case is one 404 that names no
//! other install and no path.

use serde_json::Value;
use tiny_http::{Header, Response, StatusCode};

use super::{err_response, home, HttpResp};
use crate::client;

/// `GET /api/app-installations/<id>/chat-descriptor` → `Some(id)`.
pub(super) fn route(path: &str) -> Option<&str> {
    let install = path
        .strip_prefix("/api/app-installations/")?
        .strip_suffix("/chat-descriptor")?;
    let ok = !install.is_empty()
        && install.len() <= 128
        && install
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'));
    ok.then_some(install)
}

pub(super) fn handle(state_dir: &std::path::Path, install_id: &str) -> HttpResp {
    match client::rpc(
        state_dir,
        "app_chat_descriptor",
        serde_json::json!({ "install_id": install_id }),
    ) {
        Ok(v) if v["found"] == Value::Bool(false) => err_response(404, "no chat descriptor"),
        Ok(v) => {
            let body = serde_json::to_vec(&v).unwrap_or_default();
            let mut resp = Response::from_data(body).with_status_code(StatusCode(200));
            resp.add_header(Header::from_bytes("Content-Type", "application/json").unwrap());
            resp.add_header(Header::from_bytes("Cache-Control", "no-store").unwrap());
            resp.add_header(Header::from_bytes("Vary", "Cookie, X-Cadence-Session").unwrap());
            resp
        }
        Err(e) => home::rpc_err(&e, "app_chat_descriptor"),
    }
}

#[cfg(test)]
mod tests {
    use super::route;

    #[test]
    fn route_takes_the_install_id_from_the_path_only() {
        assert_eq!(
            route("/api/app-installations/install-1/chat-descriptor"),
            Some("install-1")
        );
        for bad in [
            "/api/app-installations//chat-descriptor",
            "/api/app-installations/a/b/chat-descriptor",
            "/api/app-installations/a%2fb/chat-descriptor",
            "/api/app-installations/../chat-descriptor",
            "/api/app-installations/install-1/chat-descriptor/x",
        ] {
            assert_eq!(route(bad), None, "{bad}");
        }
    }
}
