//! CAD-1374: the PM activity intent. A hosted board human's mutation is
//! recorded as a sealed `pm_mutation_prepared` event BEFORE the mutation
//! runs; if that insert fails, the call fails and nothing is claimed.
//!
//! `pm_activity_prepare` is the public RPC. It accepts only `action`,
//! `resource` and `public_session`, refuses any agent connection (even one
//! holding an app grant), proves the operator connection, then re-checks the
//! cookie against the live membership. A member may prepare only
//! `issue_comment_added`; the owner may prepare any of the six issue actions.
//!
//! `prepare_pm_activity_receipt` is the internal factory for wiki handlers.
//! It takes no actor from a request. Its caller must already have proved the
//! original wiki ACL. It accepts the five wiki actions and the six issue
//! actions.

use serde_json::{json, Value};

use super::*;
use crate::board_identity::Role;
use crate::daemon::public_membership::Authority;
use crate::issue::{model, time};

/// The only fields the RPC accepts. Anything else is refused.
const ALLOWED_FIELDS: &[&str] = &["action", "resource", "public_session"];

/// The six issue actions the RPC accepts.
const ISSUE_ACTIONS: &[&str] = &[
    "issue_created",
    "issue_updated",
    "issue_link_changed",
    "issue_ref_added",
    "issue_comment_added",
    "issue_artifact_added",
];

/// The five wiki actions. Only the internal factory accepts these.
const WIKI_ACTIONS: &[&str] = &[
    "wiki_written",
    "wiki_blob_uploaded",
    "wiki_directory_created",
    "wiki_moved",
    "wiki_removed",
];

/// Issue resources: a project key or `issue:<ID>`.
const ISSUE_RESOURCE_MAX: usize = 128;

/// Wiki resources: a move carries two normalized paths (each up to the wiki
/// `PATH_CAP` of 2048 bytes) plus a namespace and separator. The wiki ACL and
/// path normalization are enforced by the wiki handler, not here.
const WIKI_RESOURCE_MAX: usize = 2 * 2048 + 64;

/// The one issue action a member may prepare.
const MEMBER_ACTION: &str = "issue_comment_added";

fn known_action(action: &str) -> bool {
    ISSUE_ACTIONS.contains(&action) || WIKI_ACTIONS.contains(&action)
}

/// A bounded, control-free resource. `issue_created` names a project key;
/// the other issue actions name `issue:<ID>`; wiki resources are paths and
/// are checked by the wiki store.
fn bounded_resource(action: &str, resource: &str) -> Result<String> {
    let max = if WIKI_ACTIONS.contains(&action) {
        WIKI_RESOURCE_MAX
    } else {
        ISSUE_RESOURCE_MAX
    };
    let bounded = (1..=max).contains(&resource.len()) && !resource.chars().any(char::is_control);
    if !bounded {
        return Err(Error::rejected(format!(
            "pm activity resource must be 1-{max} characters with no control characters"
        )));
    }
    if action == "issue_created" {
        let key = resource
            .strip_prefix("project:")
            .ok_or_else(|| Error::rejected("pm activity resource must be project:<KEY>"))?;
        model::check_key(key)?;
    } else if !WIKI_ACTIONS.contains(&action) {
        let id = resource
            .strip_prefix("issue:")
            .ok_or_else(|| Error::rejected("pm activity resource must be issue:<ID>"))?;
        model::check_id(id)?;
    }
    Ok(resource.to_string())
}

impl Shared {
    /// Validate a human: non-empty `sub`, `session`, `display`, a canonical
    /// `role`, and this board's company. Only these five fields are kept.
    fn pm_human(&self, human: &Value) -> Result<Value> {
        let company = crate::board_identity::read_config(&self.state_dir)?.company;
        let text = |field: &str| {
            human
                .get(field)
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty())
        };
        let (Some(sub), Some(session), Some(display), Some(role)) =
            (text("sub"), text("session"), text("display"), text("role"))
        else {
            return Err(Error::rejected(
                "pm activity human must carry sub, session, display and role",
            ));
        };
        if human["company"].as_str() != Some(company.as_str())
            || !matches!(role, "operator" | "member")
        {
            return Err(Error::rejected(
                "pm activity human is not this board's company or a canonical role",
            ));
        }
        Ok(json!({
            "sub": sub,
            "company": company,
            "session": session,
            "display": display,
            "role": role,
        }))
    }

    /// Persist the sealed intent and return it. The insert runs first; any
    /// failure returns an error and no context is returned.
    fn persist_pm_intent(&self, action: &str, resource: &str, human: &Value) -> Result<Value> {
        let request = uuid::Uuid::new_v4().to_string();
        let context = json!({
            "schema_version": 1,
            "request_id": request,
            "correlation_id": request,
            "causation_id": null,
            "at": time::now_epoch(),
            "company": human["company"],
            "human": human,
            "action": action,
            "resource": resource,
            "outcome": "prepared",
            "reason": action,
        });
        self.store
            .event_public(DAEMON_ALIAS, "pm_mutation_prepared", context.clone())?;
        Ok(context)
    }

    /// `pm_activity_prepare` — the issue-action intent for a hosted board
    /// human. Refuses agent connections, requires operator connection proof,
    /// and requires a live board session cookie for the human.
    pub(super) fn rpc_pm_activity_prepare(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        let Some(obj) = params.as_object() else {
            return Err(Error::rejected("pm activity request must be an object"));
        };
        if obj.keys().any(|k| !ALLOWED_FIELDS.contains(&k.as_str())) {
            return Err(Error::rejected(
                "pm activity refused: an unknown or caller-named field is not accepted",
            ));
        }
        // A registered agent is refused even where a grant exec would pass.
        if self.slot_identity(peer_pid)?.is_some() {
            return Err(Error::rejected(
                "pm_activity_prepare is an operator action — this connection is an agent",
            ));
        }
        self.operator_connection("pm_activity_prepare", &json!({}), peer_pid)?;
        let token =
            match params.get("public_session") {
                Some(Value::String(token)) if !token.is_empty() => token,
                _ => return Err(Error::rejected(
                    "pm_activity_prepare refused: the board session proof is missing or malformed",
                )),
            };
        let human = match self.current_public_session(token)? {
            Authority::Granted {
                sub,
                role,
                realm,
                display,
                view,
                ..
            } => json!({
                "sub": sub,
                "company": realm.company,
                "session": view.id,
                "display": display,
                "role": role.as_str(),
            }),
            Authority::Denied => {
                return Err(Error::rejected(
                    "pm_activity_prepare refused: the board session is not live",
                ))
            }
        };
        let role = if human["role"] == "operator" {
            Role::Operator
        } else {
            Role::Member
        };
        let action = required_str(params, "action")?;
        let resource = required_str(params, "resource")?;
        if !ISSUE_ACTIONS.contains(&action) {
            return Err(Error::rejected(format!(
                "pm_activity_prepare refused: '{action}' is not an issue action"
            )));
        }
        if role == Role::Member && action != MEMBER_ACTION {
            return Err(Error::rejected(format!(
                "pm_activity_prepare refused: a member may only prepare {MEMBER_ACTION}"
            )));
        }
        let resource = bounded_resource(action, resource)?;
        let human = self.pm_human(&human)?;
        self.persist_pm_intent(action, &resource, &human)
    }

    /// The internal intent factory for the wiki handlers. The caller has
    /// already proved the original wiki ACL. Takes no actor from a request.
    pub(super) fn prepare_pm_activity_receipt(
        &self,
        action: &str,
        resource: &str,
        human: &Value,
    ) -> Result<Value> {
        if !known_action(action) {
            return Err(Error::rejected(format!(
                "pm activity refused: '{action}' is not a prepared action"
            )));
        }
        let resource = bounded_resource(action, resource)?;
        let human = self.pm_human(human)?;
        self.persist_pm_intent(action, &resource, &human)
    }
}
