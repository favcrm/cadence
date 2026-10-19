//! CAD-1568: `cli_message_*` daemon RPCs — the daemon end of the remote
//! CLI's principal-bound messaging surface. The board's `/api/cli/<verb>`
//! route verifies the cli actor envelope, consumes its single-use `jti`
//! at the HTTP boundary (the ONE defined replay-consumption point) and
//! forwards the raw signed bytes as `cli_envelope` to these handlers —
//! the same relay precedent the member plane's `member_*` verbs use.
//!
//! The daemon then **re-verifies the Ed25519 signature and every claim
//! pin against its own `board-identity.json` + the platform JWKS**
//! ([`cli_caller`]), so the board relay — or any caller that can assert a
//! `source`/handle-shaped field — can never mint or widen a remote
//! principal's authority. `cli_envelope` is honored only on a
//! daemon-proven operator/board connection: an agent connection, an
//! unproven connection, or any caller presenting a `cli_envelope` is
//! refused at `proven_operator` before the envelope is read. The daemon
//! does NOT re-consume the jti (the route is the single consumption
//! point); it re-checks the cryptography only.
//!
//! The only send recipient is `master`, for a verified human workspace
//! owner. Personal assistants require their separate assistant authority;
//! local workers use the local-assignment plane. Neither is silently
//! reached through CLI credentials.
//!
//! Readback is limited to this human's CLI sends, with durable state and
//! sanitized results. Absent and foreign message IDs have the same refusal.
//! Passive inbox peeks retain the workspace-owner boundary; they never
//! expose a managed assistant's private conversation.

use super::*;

use crate::board_identity;

/// Daemon-side JWKS cache for the cli family — independent of the HTTP
/// route's and of the member plane's, so the daemon's re-verify depends
/// on no relay's cache.
static CLI_JWKS: Mutex<board_identity::JwksCache> = Mutex::new(board_identity::JwksCache::new());

/// The controlled cross-repo rig fetches the real fixture issuer's JWKS
/// through its disposable TLS trust before worker threads handle requests.
/// Signature and claim verification remain on the production request path.
#[cfg(all(test, feature = "cli-runtime-test"))]
pub(crate) fn prime_jwks_for_test(issuer: &str, kid: &str, now: i64) -> Result<()> {
    CLI_JWKS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .key(issuer, kid, now)
        .map(|_| ())
}

/// Refuse any request field outside `allowed` — the cli surface admits
/// only `cli_envelope` plus the verb's own arguments; an unlisted or
/// identity field is never silently dropped.
fn cli_reject_fields(params: &Value, allowed: &[&str], verb: &str) -> Result<()> {
    let object = params
        .as_object()
        .ok_or_else(|| Error::rejected(format!("{verb} takes an object")))?;
    if let Some(field) = object.keys().find(|k| !allowed.contains(&k.as_str())) {
        return Err(Error::rejected(format!(
            "{verb} takes {} only; field '{field}' is not accepted",
            allowed.join(", ")
        )));
    }
    Ok(())
}

/// The verified cli principal a `cli_message_*` verb acts for — the
/// re-verified envelope's claims plus this board's trust-root config.
pub(super) struct CliPrincipal {
    /// The daemon's configured issuer, audience and workspace trust root.
    pub config: board_identity::Config,
    pub actor: crate::cli_actor::CliActor,
}

impl CliPrincipal {
    /// The `cli-<handle>` message source + display attribution — the
    /// message-source identifier grammar (lowercase, digits, hyphens)
    /// over the sanitized handle. Never "operator", never a request
    /// field.
    fn source(&self) -> String {
        let mut s: String = self
            .actor
            .handle
            .chars()
            .map(|c| {
                if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        s.truncate(56);
        let s = s.trim_matches('-');
        format!("cli-{}", if s.is_empty() { "member" } else { s })
    }
}

impl Shared {
    /// Re-verify the remote caller's signed cli envelope at the daemon.
    /// The config is the daemon's own `board-identity.json` (issuer/
    /// audience/company the operator configured), never a request
    /// field; `need` is the `cli.read`/`cli.write` scope the verb the
    /// envelope is riding requires.
    ///
    /// The reusable half of every `cli_message_*` handler (and the hook
    /// daemon tests drive directly): `proven_operator` on the connection
    /// first — only the board relay may carry a relayed envelope — then
    /// the cryptographic re-verify, then the per-verb scope.
    pub(super) fn cli_caller(
        &self,
        params: &Value,
        peer_pid: u32,
        verb: &str,
        need: crate::cli_actor::Scope,
    ) -> Result<CliPrincipal> {
        // The connection itself must be the operator/board relay — the
        // only channel allowed to carry a relayed cli envelope. An
        // agent or unproven connection is refused outright, before the
        // envelope is read.
        self.proven_operator(verb, peer_pid)?;
        let envelope = optional_str(params, "cli_envelope").ok_or_else(|| {
            Error::rejected(format!(
                "{verb} refused: no cli_envelope proof — \
                 a remote-cli operation requires a verified cli actor envelope"
            ))
        })?;
        let config = board_identity::read_config(&self.state_dir).map_err(|_| {
            Error::rejected(format!(
                "{verb} refused: no board identity is configured to verify cli authority"
            ))
        })?;
        let kid = crate::assistant_actor::peek_kid(envelope).ok_or_else(|| {
            Error::rejected(format!(
                "{verb} refused: cli envelope header is not the shape"
            ))
        })?;
        let now = crate::issue::time::now_epoch();
        let key = {
            let mut cache = CLI_JWKS.lock().unwrap_or_else(|e| e.into_inner());
            cache.key(&config.issuer, &kid, now).map_err(|e| {
                Error::rejected(format!(
                    "{verb} refused: cli envelope key check failed: {e}"
                ))
            })?
        };
        let actor = crate::cli_actor::verify(envelope, &config, &key, now).map_err(|r| {
            Error::invalid(
                r.code,
                format!("{verb} refused: cli envelope: {}", r.message),
            )
        })?;
        if !crate::cli_actor::scope_covers(&actor, need) {
            return Err(Error::invalid(
                "insufficient_scope",
                format!("{verb} refused: the cli envelope's scopes do not cover this verb"),
            ));
        }
        // An enrolled agent may share its parent's `sub`, but does not
        // inherit that human's private messages or master-chat authority.
        if actor.principal_kind != "user" {
            return Err(Error::invalid(
                "not_permitted",
                "remote CLI messaging requires a verified human principal",
            ));
        }
        // The durable sender label is the canonical `cli:<org>|<sub>`
        // key; bound the claim that lands in it — the schema bounds
        // `actor` spellings but not `sub`, and a control character must
        // never ride a stored row.
        if actor.sub.is_empty() || actor.sub.len() > 200 || actor.sub.chars().any(char::is_control)
        {
            return Err(Error::invalid(
                "bad_claims",
                format!("{verb} refused: the envelope's subject is not a usable principal id"),
            ));
        }
        Ok(CliPrincipal { config, actor })
    }

    /// The durable sender key the verified principal's own sends carry —
    /// `cli:<canonical (org, sub)>` ([`store::CliAuthority::sender_label`]):
    /// the full verified provenance a readback owns, never a display
    /// name and never `sub` alone.
    fn cli_sender_key(&self, principal: &CliPrincipal) -> String {
        format!(
            "cli:{}",
            store::CliAuthority::principal_key(&principal.config.company, &principal.actor.sub)
        )
    }

    /// The store's stable-id conflict refusal lifted to a typed code a
    /// remote client maps to actionable guidance: a retry with the same
    /// id and DIFFERENT content — or the same id another principal
    /// already owns — is a conflict (re-mint `--message`), never a
    /// success and never a blind-resubmit signal.
    fn cli_send_err(&self, e: Error) -> Error {
        match e {
            Error::Rejected(m) if m == "Message id was already used with different content" => {
                Error::invalid("message_id_conflict", m)
            }
            other => other,
        }
    }

    /// A stored sender label is this principal's: the canonical
    /// `cli:<org>|<sub>` key, compared against the full verified pair —
    /// never a display name and never `sub` alone.
    fn cli_owns(&self, principal: &CliPrincipal, sender: Option<&str>) -> bool {
        sender == Some(self.cli_sender_key(principal).as_str())
    }

    /// Master coordination is the existing human-owner chat boundary.
    fn cli_recipient(&self, principal: &CliPrincipal, raw: &str) -> Result<store::Agent> {
        if raw != crate::master::ALIAS {
            return Err(Error::invalid(
                "recipient_class_unsupported",
                "remote send supports the hosted master only; personal assistants \
                 require assistant authority and local workers use local assignments",
            ));
        }
        if principal.actor.role_at_issue != "owner" {
            return Err(Error::invalid(
                "not_permitted",
                "master coordination requires a verified human workspace owner",
            ));
        }
        // Alias-only: `resolve_alias` also matches native thread/session ids,
        // so a native id equal to "master" must not reach the owner's chat.
        self.store.agent_opt(raw)?.ok_or_else(|| {
            Error::invalid(
                "not_found",
                "Hosted master is not registered in this workspace",
            )
        })
    }

    /// `cli_message_send` — the remote send. Closed params
    /// (`cli_envelope`, `alias`, `text`, `message`): `reply_to`, `task`,
    /// steering and every other send option is refused by
    /// [`cli_reject_fields`], never dropped — a remote send carries no
    /// caller-chosen routing or steering. The stable `message` id is
    /// required: it is the durable identity a lost-response retry
    /// dedupes on (the store's enqueue is idempotent on id+content).
    pub(super) fn rpc_cli_message_send(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        cli_reject_fields(
            params,
            &["cli_envelope", "alias", "text", "message"],
            "cli message send",
        )?;
        let principal = self.cli_caller(
            params,
            peer_pid,
            "cli message send",
            crate::cli_actor::Scope::Write,
        )?;
        let text = required_str(params, "text")?;
        if text.trim().is_empty() {
            return Err(Error::rejected("cli message send text is empty"));
        }
        // The stable id is mandatory for remote sends: it is what a
        // retry after a lost response reconciles on (same id + same
        // content dedupes; same id + different content conflicts). A
        // caller never names a `sys-` daemon id.
        let message = match optional_str(params, "message") {
            Some(id) => {
                let id = proto::identifier(id, "Message ID")?;
                if id.starts_with(proto::DAEMON_MESSAGE_PREFIX) {
                    return Err(Error::rejected(
                        "a remote send may not name a daemon-owned message id",
                    ));
                }
                id
            }
            None => {
                return Err(Error::invalid(
                    "invalid_request",
                    "cli message send requires a stable `message` id — the client \
                     supplies one so a retry after a lost response dedupes \
                     (send again with the same id; `cli_message_read` reports \
                     its durable state)",
                ))
            }
        };
        let raw = required_str(params, "alias")?;
        let recipient = self.cli_recipient(&principal, raw)?;
        let source = principal.source();
        let send = json!({
            "alias": recipient.alias,
            "text": text,
            "message": message,
            "source": source,
        });
        let auth = store::CliAuthority {
            org: principal.config.company,
            sub: principal.actor.sub,
            actor: principal.actor.actor,
            handle: principal.actor.handle,
            credential_id: principal.actor.credential_id,
        };
        let mut receipt = self
            .send_with_expected(
                &send,
                &|_| Ok(store::Sender::Cli(auth.clone())),
                &|_| Err(Error::rejected("remote CLI send does not support steering")),
                Some(&recipient.uid),
            )
            .map_err(|e| self.cli_send_err(e))?;
        let conversation = self
            .store
            .message_conversation(&recipient.alias, &message)?
            .ok_or_else(|| Error::internal("CLI message has no persisted Home conversation"))?;
        receipt["alias"] = Value::String(recipient.alias);
        receipt["sender"] = Value::String(source);
        receipt["workspace_id"] = Value::String(auth.org);
        receipt["sender_subject"] = Value::String(auth.sub);
        receipt["conversation"] = Value::String(conversation.id);
        Ok(receipt)
    }

    /// `cli_message_read` — the principal-owned readback: the same
    /// bounded body window `message_read` returns plus the durable
    /// `state`, sanitized `result` and `error` the remote sender
    /// reconciles against. Ownership is the recorded sender attribution
    /// — never a request field and never an id-prefix enumeration:
    /// the message's canonical `cli:<org>|<sub>` sender label. Browser
    /// member-chat messages are not CLI-owned. Report tokens are never returned.
    pub(super) fn rpc_cli_message_read(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        cli_reject_fields(
            params,
            &["cli_envelope", "message", "offset", "limit"],
            "cli message read",
        )?;
        let principal = self.cli_caller(
            params,
            peer_pid,
            "cli message read",
            crate::cli_actor::Scope::Read,
        )?;
        let id = required_str(params, "message")?;
        let not_found = || Error::invalid("not_found", "No such CLI message");
        let message = self.store.message(id)?.ok_or_else(not_found)?;
        if !self.cli_owns(&principal, message.sender.as_deref()) {
            return Err(not_found());
        }
        let offset = optional_u64(params, "offset").unwrap_or(0) as usize;
        let limit = optional_u64(params, "limit")
            .unwrap_or(messages_rpc::MESSAGE_READ_LIMIT as u64)
            .min(messages_rpc::MESSAGE_READ_LIMIT as u64) as usize;
        let total = message.body.chars().count();
        let text: String = message.body.chars().skip(offset).take(limit).collect();
        let app_context = self
            .store
            .message_app(&message.id)
            .ok()
            .flatten()
            .unwrap_or(Value::Null);
        let conversation = self
            .store
            .message_conversation(&message.alias, &message.id)?
            .ok_or_else(|| Error::internal("CLI message has no persisted Home conversation"))?;
        let mut receipt = json!({
            "message": message.id,
            "alias": message.alias,
            "state": message.state,
            "awaiting_report": message.awaiting_report(),
            "result": message.result.map(cli_sanitized),
            "error": message.error,
            "text": text,
            "offset": offset,
            "limit": limit,
            "chars_total": total,
            "truncated": offset + text.chars().count() < total,
            "app_context": app_context,
        });
        receipt["workspace_id"] = Value::String(principal.config.company);
        receipt["sender_subject"] = Value::String(principal.actor.sub);
        receipt["sender"] = Value::String(message.source);
        receipt["conversation"] = Value::String(conversation.id);
        Ok(receipt)
    }

    /// Passive mailbox inspection retains the existing human-owner
    /// boundary. Managed assistants are not passive inboxes.
    pub(super) fn rpc_cli_message_inbox(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        cli_reject_fields(
            params,
            &["cli_envelope", "alias", "after", "reader"],
            "cli message inbox",
        )?;
        let principal = self.cli_caller(
            params,
            peer_pid,
            "cli message inbox",
            crate::cli_actor::Scope::Read,
        )?;
        if principal.actor.role_at_issue != "owner" {
            return Err(Error::invalid(
                "not_permitted",
                "remote passive inbox inspection requires a human workspace owner; \
                 use message read for your own CLI sends",
            ));
        }
        let alias = required_str(params, "alias")?;
        let mut peek = json!({"alias": alias, "peek": true});
        if let Some(after) = optional_i64(params, "after") {
            peek["after"] = json!(after);
        }
        if let Some(reader) = optional_str(params, "reader") {
            peek["reader"] = json!(reader);
        }
        self.rpc_inbox(&peek, peer_pid)
    }
}

/// Strip every `turn_id` key from a stored result — the report token a
/// recipient submits with; a remote readback never returns it. The rest
/// of the result survives verbatim (`status`, `text`, `sha`, …).
fn cli_sanitized(value: Value) -> Value {
    match value {
        Value::Object(mut map) => {
            map.remove("turn_id");
            let map = map
                .into_iter()
                .map(|(k, v)| (k, cli_sanitized(v)))
                .collect();
            Value::Object(map)
        }
        Value::Array(items) => Value::Array(items.into_iter().map(cli_sanitized).collect()),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::CliAuthority;

    /// The canonical key is collision-free across claim spellings:
    /// `("a|b","c")` and `("a","b|c")` can never encode alike — the
    /// durable sender identity is the full verified pair.
    #[test]
    fn cli_principal_key_is_unambiguous() {
        assert_ne!(
            CliAuthority::principal_key("a|b", "c"),
            CliAuthority::principal_key("a", "b|c"),
        );
    }

    /// An unlisted field — steering, routing or an identity spelling —
    /// is a refusal, never a silent drop.
    #[test]
    fn cli_reject_fields_closes_params() {
        let ok = json!({"cli_envelope": "e", "alias": "master", "text": "t", "message": "m"});
        cli_reject_fields(&ok, &["cli_envelope", "alias", "text", "message"], "v").unwrap();
        for extra in ["reply_to", "task", "nudge", "force", "source", "actor"] {
            let mut bad = ok.clone();
            bad[extra] = json!("x");
            assert!(
                cli_reject_fields(&bad, &["cli_envelope", "alias", "text", "message"], "v")
                    .is_err(),
                "{extra} must be refused"
            );
        }
        assert!(cli_reject_fields(&json!([1]), &["cli_envelope"], "v").is_err());
    }

    /// `cli_sanitized` strips `turn_id` at every depth — a report token
    /// never survives a remote readback — and leaves the rest verbatim.
    #[test]
    fn cli_sanitized_strips_turn_ids_recursively() {
        let raw = json!({
            "status": "done",
            "text": "body",
            "turn_id": "secret",
            "nested": {"turn_id": "secret", "keep": 1},
            "list": [{"turn_id": "secret"}, {"keep": 2}],
        });
        let out = cli_sanitized(raw);
        assert_eq!(out["status"], "done");
        assert_eq!(out["text"], "body");
        assert!(out.get("turn_id").is_none());
        assert!(out["nested"].get("turn_id").is_none());
        assert_eq!(out["nested"]["keep"], 1);
        assert!(out["list"][0].get("turn_id").is_none());
        assert_eq!(out["list"][1]["keep"], 2);
    }
}
