//! CAD-1752: `hire_propose` - Ask Master to recruit.
//!
//! `hire_propose {role, name?, job?}` is the master's, on its live assigned
//! turn, in its Home conversation: the turn is resolved from the proven
//! caller exactly as `artifact_put` resolves it, and every other caller
//! (any other agent, a stale or replayed turn, the operator CLI) is refused.
//! It records ONE inert `hire_proposal` thread entry for the operator to
//! review. It never registers an agent, writes no agent row and creates no
//! home dir - the operator's Hire does that, through the operator-only
//! register route.

use serde_json::Value;

use crate::proto::identifier;

use super::*;

impl Shared {
    pub(super) fn rpc_hire_propose(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        super::reject_identity_fields(params, "hire propose")?;
        if let Some(field) = params.as_object().and_then(|o| {
            o.keys()
                .find(|k| !matches!(k.as_str(), "role" | "name" | "job"))
        }) {
            return Err(Error::rejected(format!(
                "hire propose field '{field}' is not accepted - scope and turn \
                 are the daemon's, never a request field"
            )));
        }
        let (_alias, message) =
            self.master_live_turn("hire propose", "proposes hires", peer_pid)?;
        let master = self.store.agent(crate::master::ALIAS)?;
        let thread = self
            .store
            .message_conversation(&master.alias, &message)?
            .ok_or_else(|| {
                Error::rejected("hire propose: the turn's conversation cannot be resolved")
            })?;
        if !(thread.is_home() || thread.is_recruit()) {
            return Err(Error::rejected(
                "hire propose is made in the master's Home or Recruit conversation",
            ));
        }
        self.prove_home_conversation(Some(&thread.id))?;

        let role_id = required_str(params, "role")?;
        let role = crate::agent_roles::find(role_id).ok_or_else(|| {
            Error::rejected(format!(
                "Unknown catalogue role '{role_id}' - one of: {}",
                crate::agent_roles::ids()
            ))
        })?;
        let name = optional_text(params, "name")?
            .map(str::trim)
            .filter(|n| !n.is_empty());
        if let Some(name) = name {
            identifier(name, "Agent name")?;
            crate::master::refuse_reserved_alias(name)?;
            if self.store.agent_opt(name)?.is_some() {
                return Err(Error::rejected(format!(
                    "Agent '{name}' already exists - propose another name, or none"
                )));
            }
        }
        let job = match optional_text(params, "job")? {
            Some(job) if !job.trim().is_empty() => job.to_string(),
            _ => role.instructions(),
        };
        if job.len() > crate::store::MAX_INSTRUCTIONS_BYTES {
            return Err(Error::rejected(format!(
                "the proposed job must be at most {} UTF-8 bytes",
                crate::store::MAX_INSTRUCTIONS_BYTES
            )));
        }
        let digest = crate::delegation::sha256_hex(job.as_bytes());
        let proposal = json!({
            "role": role.id, "title": role.title, "name": name,
            "job": job, "digest": digest,
        });
        let text = match name {
            Some(name) => format!("Proposed hire: {} as {name}", role.title),
            None => format!("Proposed hire: {}", role.title),
        };
        let seq = self
            .store
            .hire_proposal_put(&thread.id, &message, &text, proposal.clone())?;
        self.changed.notify_all();
        Ok(json!({"state": "proposed", "entry": seq, "proposal": proposal}))
    }
}
