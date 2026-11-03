//! CAD-1608: compact peer interface over the existing message store.
//!
//! A live registered agent can list the peers in its group, send one
//! message, and read each routed reply once. The sender, the reply
//! route and the source are taken from the connection. Request fields
//! cannot name a sender, steer a turn, attach a task, or answer an
//! approval. The send receipt is this attempt's disposition. An
//! unknown disposition is returned as unknown and is not retried.

use super::*;

const SEND_FIELDS: &[&str] = &["alias", "text", "message", "recipient_uid"];

/// Delivery word for one send attempt. It never means a task completed,
/// an approval was granted, or a model acted. An unrecognized store
/// state stays `unknown` so the caller does not treat it as success.
pub(super) fn peer_disposition(state: &str, recipient_state: &str) -> &'static str {
    match state {
        "failed" | "cancelled" | "refused" => "refused",
        "unknown" | "interrupted" => "unknown",
        "queued" | "submitting"
            if matches!(recipient_state, "stopped" | "offline" | "stopping") =>
        {
            "held"
        }
        "queued" | "submitting" => "queued",
        "running" | "submitted" | "completed" | "available" => "delivered",
        _ => "unknown",
    }
}

fn reachability(state: &str) -> &'static str {
    match state {
        "idle" | "busy" | "running" | "waiting_input" | "attention" | "starting" => "reachable",
        "stopped" | "offline" | "stopping" => "unreachable",
        _ => "unknown",
    }
}

fn group_root(agent: &Agent) -> &str {
    agent
        .params
        .as_ref()
        .and_then(|params| params.get("upstream"))
        .and_then(Value::as_str)
        .filter(|upstream| !upstream.is_empty())
        .unwrap_or(agent.alias.as_str())
}

/// `peer` is relevant to `caller` when it is the caller's group root or
/// another agent whose upstream is that same root. The caller itself is
/// not a peer, and an agent outside the group is not listed.
fn relevant_peer(caller: &Agent, peer: &Agent) -> bool {
    if peer.alias == caller.alias {
        return false;
    }
    let root = group_root(caller);
    peer.alias == root || group_root(peer) == root
}

fn allow(params: &Value, allowed: &[&str]) -> Result<()> {
    let object = params
        .as_object()
        .ok_or_else(|| Error::rejected("peer request must be an object"))?;
    if let Some(key) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(Error::rejected(format!(
            "peer request refused: '{key}' is not accepted"
        )));
    }
    Ok(())
}

fn text_field<'a>(params: &'a Value, field: &str) -> Result<Option<&'a str>> {
    match params.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if !value.is_empty() => Ok(Some(value)),
        Some(_) => Err(Error::rejected(format!(
            "peer request refused: '{field}' must be a non-empty string"
        ))),
    }
}

fn require_live(agent: &Agent) -> Result<()> {
    if matches!(
        agent.state.as_str(),
        "idle" | "busy" | "running" | "waiting_input"
    ) {
        Ok(())
    } else {
        Err(Error::rejected(format!(
            "peer interface refused: agent '{}' is {} — only a live endpoint can call it",
            agent.alias, agent.state
        )))
    }
}

impl Shared {
    fn peer_caller(&self, peer_pid: u32) -> Result<Agent> {
        match self.caller_identity(peer_pid)? {
            Caller::Agent(verified) => {
                // Re-read so a stop between the ancestry check and the
                // send is the row this call actually uses.
                let agent = self.store.agent(&verified.agent.alias)?;
                require_live(&agent)?;
                Ok(agent)
            }
            Caller::NoAgentIdentity => Err(Error::rejected(
                "peer interface refused: the caller is not a registered agent",
            )),
        }
    }

    pub(super) fn rpc_peer_list(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        allow(params, &[])?;
        let caller = self.peer_caller(peer_pid)?;
        let mut peers: Vec<Value> = self
            .store
            .agents()?
            .into_iter()
            .filter(|agent| relevant_peer(&caller, agent))
            .map(|agent| {
                json!({
                    "alias": agent.alias,
                    "uid": agent.uid,
                    "provider": agent.provider,
                    "transport": agent.endpoint_kind,
                    "state": agent.state,
                    "reachability": reachability(&agent.state),
                })
            })
            .collect();
        peers.sort_by(|left, right| {
            left["alias"]
                .as_str()
                .unwrap_or_default()
                .cmp(right["alias"].as_str().unwrap_or_default())
        });
        Ok(json!({
            "sender": {"alias": caller.alias, "uid": caller.uid},
            "peers": peers,
        }))
    }

    pub(super) fn rpc_peer_send(self: &Arc<Self>, params: &Value, peer_pid: u32) -> Result<Value> {
        allow(params, SEND_FIELDS)?;
        let caller = self.peer_caller(peer_pid)?;
        let alias = self.resolve_alias(required_str(params, "alias")?)?;
        let text = required_str(params, "text")?;
        if text.is_empty() || text.len() > 48_000 {
            return Err(Error::rejected(
                "peer send refused: text must contain 1-48000 characters",
            ));
        }
        let message = match text_field(params, "message")? {
            Some(id) => crate::proto::identifier(id, "Message id")?,
            None => Uuid::new_v4().simple().to_string(),
        };
        let pinned = text_field(params, "recipient_uid")?;
        let Some(recipient) = self.store.agent_opt(&alias)? else {
            return Err(Error::rejected(format!(
                "peer send refused: '{alias}' is not a peer of this caller"
            )));
        };
        if !relevant_peer(&caller, &recipient) {
            return Err(Error::rejected(format!(
                "peer send refused: '{alias}' is not a peer of this caller"
            )));
        }
        if pinned.is_some_and(|uid| uid != recipient.uid) {
            return Err(Error::rejected(
                "peer send refused: the recipient changed before the message was queued; nothing was sent",
            ));
        }
        if self.store.message_exists(&message)? {
            return Err(Error::conflict_coded(
                "duplicate",
                "peer send refused: this message id was already used; nothing was changed",
            ));
        }
        let recipient_uid = recipient.uid.clone();
        let transport = recipient.endpoint_kind.clone();
        // The reply route is the caller's own alias. A request cannot
        // point the result at another agent, an approval, or a task.
        let request = json!({
            "alias": alias,
            "text": text,
            "message": message,
            "reply_to": caller.alias,
            "source": "peer",
        });
        let receipt = self.send_with_expected(
            &request,
            &|_alias| Ok(store::Sender::Agent(caller.alias.clone())),
            &|verb| {
                Err(Error::rejected(format!(
                    "{verb} refused: a peer send cannot steer, approve, or mutate a task"
                )))
            },
            Some(recipient_uid.as_str()),
        )?;
        if receipt["duplicate"].as_bool() == Some(true) {
            return Err(Error::conflict_coded(
                "duplicate",
                "peer send refused: this message id was already used; nothing was changed",
            ));
        }
        let state = receipt["state"].as_str().unwrap_or("unknown");
        let recipient_state = self
            .store
            .agent_opt(&alias)?
            .map(|agent| agent.state)
            .unwrap_or_else(|| "unknown".to_string());
        Ok(json!({
            "message": receipt["message"],
            "disposition": peer_disposition(state, &recipient_state),
            "sender": {"alias": caller.alias, "uid": caller.uid},
            "recipient": {"alias": alias, "uid": recipient_uid},
            "transport": transport,
        }))
    }

    pub(super) fn rpc_peer_replies(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        allow(params, &[])?;
        let caller = self.peer_caller(peer_pid)?;
        let replies = self
            .store
            .take_peer_replies(&caller.alias, &caller.uid)?
            .into_iter()
            .map(|message| {
                json!({
                    "message": message.id,
                    "origin": message.origin,
                    "sender": message.sender,
                    "sender_uid": message.sender_uid,
                    "recipient": message.recipient,
                    "body": message.body,
                    "disposition": peer_disposition(&message.state, &caller.state),
                })
            })
            .collect::<Vec<_>>();
        Ok(json!({"replies": replies}))
    }
}

#[cfg(test)]
mod tests {
    use super::peer_disposition;

    #[test]
    fn peer_disposition_keeps_unknown_unknown() {
        assert_eq!(peer_disposition("unknown", "idle"), "unknown");
        assert_eq!(peer_disposition("interrupted", "idle"), "unknown");
        assert_eq!(peer_disposition("not-a-state", "idle"), "unknown");
        assert_eq!(peer_disposition("queued", "idle"), "queued");
        assert_eq!(peer_disposition("queued", "stopped"), "held");
        assert_eq!(peer_disposition("failed", "idle"), "refused");
        assert_eq!(peer_disposition("running", "busy"), "delivered");
    }
}
