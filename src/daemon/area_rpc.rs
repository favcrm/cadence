//! `area_ack` (CAD-378): the owner of a code area acks a lane's change
//! to it, clearing the Needs-you `area_ack` row.
//!
//! The acker is bound from the connection ([`Shared::agent_caller`]):
//! the proven operator, or the agent whose alias is the area's owner PM
//! in the project's `PROJECT.md` `areas:`. Any other agent — the lane's
//! own worker included — a detached child of the owner (no identity,
//! not provably the operator) and identity-shaped request fields are
//! refused before anything is written. The ack is recorded in the
//! daemon's state dir (`area_acks.json`) plus an `area_acked` event —
//! never a tracker comment, whose author any writer can claim.

use serde_json::{json, Value};

use super::{optional_str, reject_identity_fields, required_str, Shared, DAEMON_ALIAS};
use crate::error::{Error, Result};
use crate::issue::{self, areas, Pm};
use crate::peer::AgentCaller;

/// Longest ack note.
const NOTE_MAX: usize = 500;

impl Shared {
    pub(super) fn rpc_area_ack(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        const VERB: &str = "area ack";
        reject_identity_fields(params, VERB)?;
        let caller = self.agent_caller(peer_pid, VERB)?;
        let id = issue::model::check_id(required_str(params, "issue")?)?;
        let name = required_str(params, "area")?;
        let note = optional_str(params, "note").filter(|n| !n.trim().is_empty());
        if note.is_some_and(|n| n.len() > NOTE_MAX || n.chars().any(char::is_control)) {
            return Err(Error::rejected(format!(
                "{VERB}: note must be one line of at most {NOTE_MAX} bytes"
            )));
        }
        let pm = Pm::at(&self.pm_dir()?)?;
        let (project, _) = issue::write::issue_dir(&pm, &id)?;
        let all = areas::load(&pm.dir, &project.key)?;
        let area = all.iter().find(|a| a.name == name).ok_or_else(|| {
            Error::rejected(format!(
                "{VERB}: project {} declares no area '{name}' — known: {}",
                project.key,
                if all.is_empty() {
                    "none".to_string()
                } else {
                    all.iter()
                        .map(|a| a.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            ))
        })?;
        let by = match caller {
            AgentCaller::Operator => "operator".to_string(),
            AgentCaller::Agent(alias) if area.pm.as_deref() == Some(alias.as_str()) => alias,
            AgentCaller::Agent(alias) => {
                return Err(Error::rejected(format!(
                    "{VERB}: area '{name}' is owned by {} — only its owner PM{} or the \
                     operator may ack; this connection is agent '{alias}'",
                    area.owner(),
                    area.pm
                        .as_deref()
                        .map(|p| format!(" ({p})"))
                        .unwrap_or_default()
                )));
            }
        };
        let record = json!({
            "issue": id,
            "area": name,
            "project": project.key,
            "owner": area.owner(),
            "by": by,
            "at": issue::time::iso(issue::time::now_epoch()),
            "note": note,
        });
        areas::record_ack(&self.state_dir, &areas::ack_key(&id, name), record.clone())?;
        let _ = self
            .store
            .event_public(DAEMON_ALIAS, "area_acked", record.clone());
        self.wake();
        Ok(record)
    }
}
