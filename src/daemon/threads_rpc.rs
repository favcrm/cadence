//! CAD-534: `cadence daemon` threads RPC handlers — moved verbatim from src/daemon.rs.

use super::*;

impl Shared {
    /// The entries as JSON. A message-linked entry whose stored message
    /// belongs to this alias and to the entry's own thread carries
    /// `payload.delivery` (`state`, `started`, `completed`); its other
    /// payload keys are kept. Anything else gets no delivery.
    fn entries_json(&self, alias: &str, entries: &[store::ThreadEntry]) -> Result<Vec<Value>> {
        let mut out = Vec::with_capacity(entries.len());
        for entry in entries {
            let mut j = entry.to_json();
            let Some(id) = &entry.message_id else {
                out.push(j);
                continue;
            };
            let Some(message) = self.store.message(id)?.filter(|m| m.alias == alias) else {
                out.push(j);
                continue;
            };
            let same_thread = self
                .store
                .message_conversation(alias, id)?
                .is_some_and(|t| t.id == entry.thread_id);
            let mut payload = match &entry.payload {
                None | Some(Value::Null) => serde_json::Map::new(),
                Some(Value::Object(obj)) => obj.clone(),
                Some(_) => {
                    out.push(j);
                    continue;
                }
            };
            if same_thread {
                payload.insert(
                    "delivery".into(),
                    json!({"state": message.state, "started": message.started, "completed": message.completed}),
                );
                j["payload"] = Value::Object(payload);
            }
            out.push(j);
        }
        Ok(out)
    }

    /// CAD-1374: the verified human behind an owner write. The board
    /// sends `public_session` only for a public-host request, and refuses
    /// a public-host request that carries none, so an absent bearer is the
    /// operator's local write and gets no human. A present bearer is
    /// re-checked against the daemon's own live row, which must map to the
    /// owner role; the subject/session/company come from that row only.
    /// `public_session` is never a source of authority on its own.
    pub(super) fn verified_owner_human(&self, params: &Value) -> Result<Option<Value>> {
        // Only an ABSENT field means the local operator chat. A present
        // null, non-string or empty proof is malformed and refused, never
        // read as absent (that would downgrade a hosted request).
        let token = match params.get("public_session") {
            None => return Ok(None),
            Some(Value::String(token)) if !token.is_empty() => token.as_str(),
            Some(_) => {
                return Err(Error::rejected(
                    "thread send refused: the board session proof is malformed",
                ))
            }
        };
        let granted = match self.current_public_session(token)? {
            crate::daemon::public_membership::Authority::Granted {
                sub,
                role,
                realm,
                display,
                view,
                ..
            } => (sub, role, realm, display, view),
            crate::daemon::public_membership::Authority::Denied => {
                return Err(Error::rejected(
                    "thread send refused: the board session is not live",
                ))
            }
        };
        let (sub, role, realm, display, view) = granted;
        if !role.is_operator() {
            return Err(Error::rejected(
                "thread send is the board owner's chat; this session is a member",
            ));
        }
        Ok(Some(json!({
            "sub": sub,
            "company": realm.company,
            "session": view.id,
            "role": role.as_str(),
            "display": display,
        })))
    }

    /// `thread_read` — a page of an agent's thread after `after`,
    /// optionally long-polling up to `wait` seconds (≤ 30) for the next
    /// entry, the way `agent_events` does. Read-only.
    pub(super) fn rpc_thread_read(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let alias = self.resolve_alias(required_str(params, "alias")?)?;
        // CAD-1098: `conversation` selects one thread of this alias by
        // id (validated; unknown answers an error and creates nothing).
        // Without it the read is the home thread, unchanged.
        let conversation = match optional_text(params, "conversation")? {
            None => None,
            Some(id) => {
                crate::proto::identifier(id, "conversation ID")?;
                Some(
                    self.store
                        .thread_by_id(id)?
                        .filter(|t| t.alias == alias)
                        .ok_or_else(|| Error::rejected("Unknown conversation"))?,
                )
            }
        };
        let after = optional_i64(params, "after").unwrap_or(0);
        if after < 0 {
            return Err(Error::rejected("Thread cursor must be nonnegative"));
        }
        let limit = optional_i64(params, "limit").unwrap_or(100);
        if !(1..=store::THREAD_PAGE_MAX).contains(&limit) {
            return Err(Error::rejected(format!(
                "Thread page limit must be 1-{}",
                store::THREAD_PAGE_MAX
            )));
        }
        // CAD-328: `tail` (the newest page) or `before` (the page below a
        // seq) read backwards for a chat view; neither waits.
        let tail = params.get("tail").and_then(Value::as_bool).unwrap_or(false);
        let before = optional_i64(params, "before");
        if tail || before.is_some() {
            if params.get("after").is_some() || params.get("wait").is_some() {
                return Err(Error::rejected(
                    "Thread read takes either after/wait (forward) or tail/before (backward)",
                ));
            }
            if before.is_some_and(|b| b < 1) {
                return Err(Error::rejected("Thread 'before' must be a positive seq"));
            }
            let thread = match &conversation {
                Some(thread) => Some(thread.clone()),
                None => self.store.thread(&alias)?,
            };
            let (entries, more) = match &conversation {
                Some(thread) => self
                    .store
                    .thread_entries_before_of(&thread.id, before, limit)?,
                None => self.store.thread_entries_before(&alias, before, limit)?,
            };
            let cursor = entries.last().map(|e| e.seq).unwrap_or(0);
            return Ok(json!({
                "alias": alias,
                "thread": thread.as_ref().map(store::Thread::to_json),
                "entries": self.entries_json(&alias, &entries)?,
                "cursor": cursor,
                "more_before": more,
            }));
        }
        let wait = optional_u64(params, "wait").unwrap_or(0).min(30);
        let deadline = Instant::now() + Duration::from_secs(wait);
        loop {
            let thread = match &conversation {
                Some(thread) => Some(thread.clone()),
                None => self.store.thread(&alias)?,
            };
            let entries = match &conversation {
                Some(thread) => self.store.thread_entries_of(&thread.id, after, limit)?,
                None => self.store.thread_entries(&alias, after, limit)?,
            };
            if !entries.is_empty()
                || Instant::now() >= deadline
                || self.closing.load(Ordering::SeqCst)
            {
                let cursor = entries.last().map(|e| e.seq).unwrap_or(after);
                return Ok(json!({
                    "alias": alias,
                    "thread": thread.as_ref().map(store::Thread::to_json),
                    "entries": self.entries_json(&alias, &entries)?,
                    "cursor": cursor,
                }));
            }
            let step = deadline.min(Instant::now() + Duration::from_secs(1));
            self.changed.wait_until(step);
        }
    }

    /// `thread_send` — the operator's chat message to an agent: starts
    /// the alias's thread on first use and queues the text exactly like
    /// `agent_send`, recorded as an `operator` entry. CAD-1591 adds one
    /// optional field, `expected_recipient_uid`: the contextual-reply
    /// recipient constraint — a nonempty uid, never authority, fenced
    /// against the alias's live row inside the enqueue transaction, so
    /// a removed or replaced participant can never silently receive the
    /// send. An omitted field is the ordinary path, unchanged.
    ///
    /// It instructs an agent, so an agent must never reach it: a
    /// connection the daemon attributes to a pane or managed endpoint is
    /// refused, and so is one whose identity cannot be derived (fail
    /// closed), and a connection tied to no agent must still be provably
    /// the operator (CAD-276's positive proof, CAD-339): the board relays
    /// browser writes from its own process, so the browser's identity is
    /// the board's to establish (CAD-313). Identity-shaped and routing fields are refused rather
    /// than read.
    pub(super) fn rpc_thread_send(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        if let Some(obj) = params.as_object() {
            if let Some(field) = obj.keys().find(|k| {
                !matches!(
                    k.as_str(),
                    "alias"
                        | "text"
                        | "message"
                        | "refs"
                        | "app"
                        | "conversation"
                        | "attachments"
                        | "media_refs"
                        | "expected_recipient_uid"
                        | "public_session"
                        | "session"
                )
            }) {
                return Err(Error::rejected(format!(
                    "thread send takes alias, text, message, refs, app, conversation, attachments, \
                     media_refs, expected_recipient_uid and the server-held session proofs only; \
                     field '{field}' is not accepted"
                )));
            }
        }
        // CAD-339 (review round 1): no agent identity is not enough — a
        // detached child of an agent derives none. The connection must
        // be provably the operator (CAD-276); the board relays browser
        // writes from its own operator process (CAD-313 gap).
        self.operator_chat("thread send", peer_pid)?;
        let human = self.verified_owner_human(params)?;
        let alias = self.resolve_alias(required_str(params, "alias")?)?;
        // CAD-1591: the contextual-reply recipient constraint — an
        // optional nonempty uid, checked against the alias's live row
        // inside the enqueue transaction (never a precheck that can
        // race). It is a typed argument like `human`: stripped from the
        // forwarded params, so `expected_recipient_uid` is never a send
        // request field on any verb.
        let expected_uid = match params.get("expected_recipient_uid") {
            None | Some(Value::Null) => None,
            Some(Value::String(uid)) if !uid.is_empty() => Some(uid.clone()),
            Some(_) => {
                return Err(Error::rejected(
                    "expected_recipient_uid must be a nonempty recipient uid string",
                ));
            }
        };
        // CAD-802: a raw `app` binding rides through; `send_as`
        // proves it against the store and stamps the normalized form
        // on the entry — one proving place for every caller.
        let mut send = params.clone();
        send["alias"] = json!(alias);
        send["source"] = json!("operator");
        // The thread starts inside the enqueue transaction: a refused
        // message leaves no thread and no `thread_created` event. The
        // daemon-derived human is a typed argument, never a field of `send`.
        if let Some(obj) = send.as_object_mut() {
            obj.remove("public_session");
            obj.remove("expected_recipient_uid");
        }
        let mut receipt = self.send_with_human_expected(
            &send,
            &|_| Ok(store::Sender::OperatorChat),
            &|verb| {
                Err(Error::rejected(format!(
                    "{verb} refused: this send carries no caller to steer with"
                )))
            },
            human.as_ref(),
            expected_uid.as_deref(),
            None,
        )?;
        // The thread the message landed in: its conversation, which is
        // home for an unbound send (CAD-1098).
        let landed = match receipt.get("message").and_then(Value::as_str) {
            Some(id) => self.store.message_conversation(&alias, id)?,
            None => None,
        };
        receipt["thread"] = landed
            .or(self.store.thread(&alias)?)
            .as_ref()
            .map(store::Thread::to_json)
            .unwrap_or(Value::Null);
        Ok(receipt)
    }
}
