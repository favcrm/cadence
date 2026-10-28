//! CAD-1085: an agent's durable connection/access projection for its detail
//! page.
//!
//! Read-only and metadata only. It answers the operator from the daemon's
//! own grant rows and connection metadata — never a credential, ciphertext
//! reference or provider response — and it states custody facts as the
//! daemon reports them. Nothing here grants, revokes or verifies authority;
//! those remain the existing operator verbs.

use super::{err_response, json_response, HttpResp};
use crate::client;
use serde_json::json;
use std::path::Path;

#[derive(Clone, Copy)]
pub(super) enum Route<'a> {
    Access(&'a str),
}

impl Route<'_> {
    pub(super) fn is_read(self) -> bool {
        true
    }
}

fn alias_shape(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

pub(super) fn route(path: &str) -> Option<Route<'_>> {
    let tail = path.strip_prefix("/api/agents/")?;
    let alias = tail.strip_suffix("/access")?;
    alias_shape(alias).then_some(Route::Access(alias))
}

pub(super) fn handle(state: &Path, route: Route<'_>) -> HttpResp {
    let Route::Access(alias) = route;
    let grants = match client::rpc(state, "platform_grants", json!({"agent": alias})) {
        Ok(value) => value["grants"].clone(),
        Err(error) => {
            return err_response(
                503,
                &format!(
                    "the agent's grants could not be read: {}",
                    error.code().unwrap_or("unavailable")
                ),
            )
        }
    };
    // `connection_list` decrypts each enrolled projection; a locked key
    // refuses it. That is a truthful unavailability, not an empty list.
    let (connections, connections_error) = match client::rpc(state, "connection_list", json!({})) {
        Ok(value) => (value["connections"].clone(), serde_json::Value::Null),
        Err(error) => (
            json!([]),
            json!(error.code().unwrap_or("custody_unavailable")),
        ),
    };
    let custody = match client::rpc(state, "connection_custody_status", json!({})) {
        Ok(value) => value["custody"].clone(),
        Err(_) => json!({
            "configured": false,
            "unlocked": false,
            "fenced": false,
            "isolation": "none",
            "protected": false,
        }),
    };
    json_response(json!({
        "agent": alias,
        "grants": grants,
        "connections": connections,
        "connections_error": connections_error,
        "custody": custody,
        "network_checked": false,
        "execution_authority": false,
    }))
}
