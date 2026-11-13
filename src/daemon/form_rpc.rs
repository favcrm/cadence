//! CAD-2285: `master_ask_form` - the master asks the operator with a form.
//!
//! `master_ask_form {form}` is the master's, on its live assigned turn, in
//! its Home conversation: the turn is resolved from the proven caller
//! exactly as `hire_propose` resolves it, and every other caller (any other
//! agent, a stale or replayed turn, the operator CLI) is refused. The form
//! is checked against the pilot catalog (`crate::agent_form`) before
//! anything is written; then ONE inert `surface` thread entry records it,
//! with the daemon's own fallback text. The operator's answer is an
//! ordinary chat message - the form grants nothing.

use serde_json::Value;

use super::*;

impl Shared {
    pub(super) fn rpc_master_ask_form(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        super::reject_identity_fields(params, "master ask-form")?;
        if let Some(field) = params
            .as_object()
            .and_then(|o| o.keys().find(|k| k.as_str() != "form"))
        {
            return Err(Error::rejected(format!(
                "master ask-form field '{field}' is not accepted - scope and turn \
                 are the daemon's, never a request field"
            )));
        }
        let (_alias, message) =
            self.master_live_turn("master ask-form", "asks with forms", peer_pid)?;
        let form = crate::agent_form::check(required_field(params, "form")?)?;
        let master = self.store.agent(crate::master::ALIAS)?;
        let thread = self
            .store
            .message_conversation(&master.alias, &message)?
            .ok_or_else(|| {
                Error::rejected("master ask-form: the turn's conversation cannot be resolved")
            })?;
        // Home only: the Recruit conversation has no form renderer.
        if !thread.is_home() {
            return Err(Error::rejected(
                "master ask-form is made in the master's Home conversation, \
                 not Recruit or an app conversation",
            ));
        }
        self.prove_home_conversation(Some(&thread.id))?;
        let payload = json!({"a2ui": form.envelope, "digest": form.digest});
        let seq = self
            .store
            .surface_put(&thread.id, &message, &form.fallback, payload)?;
        self.changed.notify_all();
        Ok(json!({"state": "asked", "entry": seq, "fallback": form.fallback}))
    }
}

fn required_field<'a>(params: &'a Value, key: &str) -> Result<&'a Value> {
    params
        .get(key)
        .ok_or_else(|| Error::rejected(format!("master ask-form needs '{key}'")))
}
