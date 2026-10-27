//! CAD-1374: the owner-only activity query and export daemon methods.
//!
//! Both methods are reached only through the caller rule. The local operator
//! passes `operator_connection`. A hosted request must also carry a server-held
//! public session that the daemon re-checks and that maps to the board owner
//! role. An absent `public_session` is the local operator; a present but
//! malformed one is refused, never read as a downgrade. Every refusal, from
//! any guard or validation step, is recorded as an `activity_denied` event
//! before the error returns. If that record cannot be written the call is
//! still refused, with a storage-unavailable error.

use std::sync::Arc;

use serde_json::{json, Value};

use super::*;
use crate::store::activity::ActivityQuery;

/// Parameters the query and export accept. Anything else, including any
/// actor, company or human field, is refused as spoofing.
const ALLOWED_FIELDS: &[&str] = &[
    "sub",
    "action",
    "target",
    "outcome",
    "from",
    "until",
    "limit",
    "cursor",
    "public_session",
];

/// A string filter: absent is no filter; any other type is refused, never
/// silently dropped.
fn text_param(params: &Value, field: &str) -> Result<Option<String>> {
    match params.get(field) {
        None => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.clone())),
        Some(_) => Err(Error::rejected(format!(
            "activity {field} must be a string"
        ))),
    }
}

/// A time bound: absent is no bound; a finite UTC epoch is required.
fn time_param(params: &Value, field: &str) -> Result<Option<f64>> {
    match params.get(field) {
        None => Ok(None),
        Some(value) => value
            .as_f64()
            .filter(|n| n.is_finite())
            .map(Some)
            .ok_or_else(|| Error::rejected(format!("activity {field} must be a finite UTC epoch"))),
    }
}

impl Shared {
    /// Refuse any actor, company, human or unknown field before anything runs.
    fn activity_fields(&self, params: &Value) -> Result<()> {
        let Some(obj) = params.as_object() else {
            return Err(Error::rejected("activity request must be an object"));
        };
        if obj.keys().any(|k| !ALLOWED_FIELDS.contains(&k.as_str())) {
            return Err(Error::rejected(
                "activity refused: an unknown or caller-named field is not accepted",
            ));
        }
        Ok(())
    }

    /// The owner gate. Every call first proves the operator connection. A
    /// present `public_session` must then be a live board owner. Returns the
    /// verified owner human, or `None` for the local operator. `sub` is a
    /// subject filter, not an identity, so the connection is checked against
    /// an empty request.
    fn activity_owner(&self, verb: &str, params: &Value, peer_pid: u32) -> Result<Option<Value>> {
        self.operator_connection(verb, &json!({}), peer_pid)?;
        match params.get("public_session") {
            None => Ok(None),
            Some(Value::String(token)) if !token.is_empty() => {
                let (role, realm, view, sub, display) = match self.current_public_session(token)? {
                    crate::daemon::public_membership::Authority::Granted {
                        role,
                        realm,
                        view,
                        sub,
                        display,
                        ..
                    } => (role, realm, view, sub, display),
                    crate::daemon::public_membership::Authority::Denied => {
                        return Err(Error::rejected(format!(
                            "{verb} refused: the board session is not live"
                        )))
                    }
                };
                if !role.is_operator() {
                    return Err(Error::rejected(format!(
                        "{verb} refused: the board owner session proof is missing or is a member's"
                    )));
                }
                Ok(Some(json!({
                    "company": realm.company,
                    "sub": sub,
                    "session": view.id,
                    "display": display,
                    "role": "operator",
                })))
            }
            Some(_) => Err(Error::rejected(format!(
                "{verb} refused: the board session proof is malformed"
            ))),
        }
    }

    /// Record a refused activity call. The actor is the verified live user
    /// when the proof is live, else `null` (UNKNOWN); a client-named actor is
    /// never read. Only static server fields and the verb are written.
    fn record_activity_denial(&self, verb: &str, params: &Value) -> Result<()> {
        let config = crate::board_identity::read_config(&self.state_dir)?;
        let realm = crate::operator_auth::PublicRealm {
            issuer: config.issuer,
            company: config.company,
            host: config.host,
        };
        let human = match params.get("public_session") {
            // Identity-only metadata for the denial record: it never
            // authorizes, and a revoked known subject is still named. A
            // session saved under another realm is UNKNOWN, not re-assigned.
            Some(Value::String(token)) if !token.is_empty() => self
                .operator_auth()
                .public_snapshot(token, self.operator_now())
                .and_then(|snap| {
                    if snap.realm.as_ref() != Some(&realm) {
                        return None;
                    }
                    let user = snap.view.user.as_ref()?;
                    Some(json!({
                        "sub": user.sub,
                        "company": realm.company,
                        "session": snap.view.id,
                        "display": user.name,
                        "role": null,
                    }))
                })
                .unwrap_or(Value::Null),
            _ => Value::Null,
        };
        self.store.event_public(
            DAEMON_ALIAS,
            "activity_denied",
            json!({"operation": verb, "outcome": "denied", "company": realm.company, "human": human}),
        )
    }

    /// Record a refused activity call and return the refusal. If the record
    /// cannot be written, the call still refuses, with a storage-unavailable
    /// error.
    fn deny_activity(&self, verb: &str, params: &Value, refused: Error) -> Error {
        match self.record_activity_denial(verb, params) {
            Ok(()) => refused,
            Err(_) => Error::internal(
                "denial-audit-storage-unavailable: activity refused and not recorded",
            ),
        }
    }

    /// Run the guards and validation for one activity call. Any refusal is
    /// recorded.
    fn activity_gate(
        &self,
        verb: &str,
        params: &Value,
        peer_pid: u32,
    ) -> Result<(Option<Value>, ActivityQuery)> {
        let checked = self
            .activity_fields(params)
            .and_then(|()| self.activity_owner(verb, params, peer_pid))
            .and_then(|human| Ok((human, self.activity_query_params(params)?)));
        checked.map_err(|refused| self.deny_activity(verb, params, refused))
    }

    /// `activity_query` — a page of owner-visible human receipts.
    pub(super) fn rpc_activity_query(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        let (_, query) = self.activity_gate("activity_query", params, peer_pid)?;
        let pm_dir = self
            .pm_dir()
            .map_err(|e| self.deny_activity("activity_query", params, e))?;
        self.store
            .activity_query_with_pm(&self.state_dir, &query, &pm_dir)
            .map_err(|e| self.deny_activity("activity_query", params, e))
    }

    /// `activity_export` — the same page contract. The page is built first;
    /// its prepared receipt is persisted with the actual interval and row
    /// count, and only then are the bytes returned. A failed page or receipt
    /// write returns no page. Nothing is claimed delivered.
    pub(super) fn rpc_activity_export(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        let (human, query) = self.activity_gate("activity_export", params, peer_pid)?;
        let pm_dir = self
            .pm_dir()
            .map_err(|e| self.deny_activity("activity_export", params, e))?;
        let page = self
            .store
            .activity_query_with_pm(&self.state_dir, &query, &pm_dir)
            .map_err(|e| self.deny_activity("activity_export", params, e))?;
        self.store.event_public(
            DAEMON_ALIAS,
            "activity_export_prepared",
            json!({
                "operation": "activity_export",
                "outcome": "prepared",
                "company": query.company,
                "human": human,
                "interval": page["interval"],
                "row_count": page["row_count"],
                "filters": {
                    "sub": query.sub, "action": query.action,
                    "target": query.target, "outcome": query.outcome,
                    "from": query.from, "until": query.until,
                },
            }),
        )?;
        Ok(page)
    }

    /// Build the typed query from validated fields. The company is the
    /// trusted board config, never a request value.
    fn activity_query_params(&self, params: &Value) -> Result<ActivityQuery> {
        let company = crate::board_identity::read_config(&self.state_dir)?.company;
        let limit = match params.get("limit") {
            None => None,
            Some(value) => Some(
                value
                    .as_u64()
                    .map(|n| n as usize)
                    .ok_or_else(|| Error::rejected("activity limit must be an integer"))?,
            ),
        };
        Ok(ActivityQuery {
            company,
            sub: text_param(params, "sub")?,
            action: text_param(params, "action")?,
            target: text_param(params, "target")?,
            outcome: text_param(params, "outcome")?,
            from: time_param(params, "from")?,
            until: time_param(params, "until")?,
            limit,
            cursor: text_param(params, "cursor")?,
        })
    }
}
