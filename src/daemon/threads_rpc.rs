//! CAD-534: `cadence daemon` threads RPC handlers — moved verbatim from src/daemon.rs.

use super::*;
use cadence_core::board_api::{ThreadEntryRow, ThreadPage};

impl Shared {
    /// Attach the cloud-withheld reason to a serialized page, so the board can say
    /// the cloud history is unavailable instead of showing an empty one.
    fn stamp_withheld(mut page: serde_json::Value, withheld: Option<String>) -> serde_json::Value {
        if let Some(reason) = withheld {
            page["cloud_withheld"] = serde_json::Value::String(reason);
        }
        page
    }

    /// The entries as JSON. A message-linked entry whose stored message
    /// belongs to this alias and to the entry's own thread carries
    /// `payload.delivery` (`state`, `started`, `completed`); its other
    /// payload keys are kept. Anything else gets no delivery.
    fn entries_json(
        &self,
        alias: &str,
        entries: &[store::ThreadEntry],
    ) -> Result<Vec<ThreadEntryRow>> {
        let mut out = Vec::with_capacity(entries.len());
        for entry in entries {
            let mut row = entry.row();
            let Some(id) = &entry.message_id else {
                out.push(row);
                continue;
            };
            let Some(message) = self.store.message(id)?.filter(|m| m.alias == alias) else {
                out.push(row);
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
                    out.push(row);
                    continue;
                }
            };
            if same_thread {
                payload.insert(
                    "delivery".into(),
                    json!({"state": message.state, "started": message.started, "completed": message.completed}),
                );
                row.payload = Some(Value::Object(payload));
            }
            out.push(row);
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
    pub(super) fn rpc_thread_read(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        let alias = self.resolve_alias(required_str(params, "alias")?)?;
        // CAD-1853 review: a cloud target's request/reply text reaches this
        // projection, so it demands the same source/owner authority as the
        // dedicated cloud read — current READ authority for the caller's own
        // source, and only that source's rows — so a peer with no grant is
        // refused and an enrolled peer never sees another source's reply.
        let mut projection = self.admit_cloud_projection(&alias, peer_pid, "thread read")?;
        let mut cloud_withheld = projection.withheld.clone();
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
            let (raw, more) = match &conversation {
                Some(thread) => self
                    .store
                    .thread_entries_before_of(&thread.id, before, limit)?,
                None => self.store.thread_entries_before(&alias, before, limit)?,
            };
            // The page is observed before it is projected, so anything the
            // admission snapshot does not cover is re-resolved here — before
            // the boundary and the rows are decided, never after. A backward
            // page can carry a routed cloud result or a cloud operation that
            // resolved after this read was admitted, and authority was never
            // proven for it.
            if projection.needs_readmission(
                &self.store,
                raw.iter().filter_map(|entry| entry.message_id.as_deref()),
            )? {
                projection = self.admit_cloud_projection(&alias, peer_pid, "thread read")?;
                cloud_withheld = projection.withheld.clone();
            }
            // The cursor is the last row this page SCANNED, never the last
            // row the projection kept. A projection filters another source's
            // cloud entries out; taking the kept page's last seq would pin
            // the caller's cursor short of rows it must move past, and the
            // filtered content is exactly what must not be exposed.
            let cursor = raw.last().map(|e| e.seq).unwrap_or(0);
            // A backward scan needs the same honesty in the other direction:
            // the LOWEST seq this page scanned is the continuation point. A
            // partially (or wholly) filtered page keeps fewer rows than it
            // scanned, so `entries[0]` may not move at all; without this
            // boundary the consumer would re-read the same hidden page
            // forever and never reach the history below it.
            let scanned_before = raw.first().map(|e| e.seq);
            let entries = projection.entries(&self.store, raw)?;
            // CAD-1700: the complete watermark of the authorized thread,
            // never just this (bounded) page or the cursor's window.
            let reply_seq = match &thread {
                Some(thread) => self.store.thread_reply_seq(&thread.id)?,
                None => 0,
            };
            return Ok(Self::stamp_withheld(
                serde_json::to_value(ThreadPage {
                    alias: alias.clone(),
                    thread: thread.as_ref().map(store::Thread::info),
                    entries: self.entries_json(&alias, &entries)?,
                    cursor,
                    more_before: Some(more),
                    scanned_before,
                    more_after: None,
                    reply_seq,
                })
                .expect("thread page serializes"),
                cloud_withheld.take(),
            ));
        }
        let wait = optional_u64(params, "wait").unwrap_or(0).min(30);
        let deadline = Instant::now() + Duration::from_secs(wait);
        // At most one re-proof per call, so a wait cannot turn a revoked or
        // unsettled operation into a readback loop.
        let mut readmitted = false;
        loop {
            let thread = match &conversation {
                Some(thread) => Some(thread.clone()),
                None => self.store.thread(&alias)?,
            };
            let raw = match &conversation {
                Some(thread) => self.store.thread_entries_of(&thread.id, after, limit)?,
                None => self.store.thread_entries(&alias, after, limit)?,
            };
            // A read that WAITED can observe content that settled after its
            // admission was taken — its own queued turn woken by another
            // connection, for instance — and, on an ordinary alias, a routed
            // cloud result that resolved after admission. Neither was
            // admitted, so re-resolve this page's whole admission once before
            // projecting it: the new snapshot either covers each row (under
            // the platform's own answer, for cloud content) or refuses /
            // withholds it. Projecting under the old admission would serve a
            // row authority was never proven for.
            if !readmitted
                && projection.needs_readmission(
                    &self.store,
                    raw.iter().filter_map(|entry| entry.message_id.as_deref()),
                )?
            {
                projection = self.admit_cloud_projection(&alias, peer_pid, "thread read")?;
                cloud_withheld = projection.withheld.clone();
                readmitted = true;
            }
            // The cursor is the last row this page SCANNED, never the last
            // row the projection kept. Filtering another source's cloud
            // entries out must not leave the caller's `after` on a seq it has
            // already passed, or a consumer would re-read the same hidden
            // page forever; the filtered content itself is never exposed.
            let scanned = raw.last().map(|e| e.seq).unwrap_or(after);
            // Whether the scan reached the page's end is a property of the
            // RAW page, never of the kept one: a projection can hide every
            // row of a full page, so the visible count says nothing about
            // exhaustion.
            let more_after = raw.len() as i64 == limit;
            let entries = projection.entries(&self.store, raw)?;
            if !entries.is_empty()
                || Instant::now() >= deadline
                || self.closing.load(Ordering::SeqCst)
            {
                // CAD-1700: the same complete watermark whatever the
                // cursor — a cold tail or a long catch-up page carries it.
                let reply_seq = match &thread {
                    Some(thread) => self.store.thread_reply_seq(&thread.id)?,
                    None => 0,
                };
                return Ok(Self::stamp_withheld(
                    serde_json::to_value(ThreadPage {
                        alias: alias.clone(),
                        thread: thread.as_ref().map(store::Thread::info),
                        entries: self.entries_json(&alias, &entries)?,
                        cursor: scanned,
                        more_before: None,
                        scanned_before: None,
                        more_after: Some(more_after),
                        reply_seq,
                    })
                    .expect("thread page serializes"),
                    cloud_withheld.take(),
                ));
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
        // CAD-1591: the identity constraint is parsed BEFORE the cloud
        // branch so a cloud target can never bypass it.
        let cloud_expected_uid = match params.get("expected_recipient_uid") {
            None | Some(Value::Null) => None,
            Some(Value::String(uid)) if !uid.is_empty() => Some(uid.as_str()),
            Some(_) => {
                return Err(Error::rejected(
                    "expected_recipient_uid must be a nonempty recipient uid string",
                ));
            }
        };
        if self
            .store
            .agent_opt(&alias)?
            .is_some_and(|agent| agent.provider == crate::cloud_agent::PROVIDER)
        {
            let conversation = optional_str(params, "conversation").map(str::to_string);
            let mut routed = params.clone();
            routed["alias"] = json!(alias);
            return self.cloud_send_from(
                &alias,
                &routed,
                peer_pid,
                conversation.as_deref(),
                cloud_expected_uid,
                "cloud agent thread send",
            );
        }
        // CAD-1591: the contextual-reply recipient constraint — an
        // optional nonempty uid, checked against the alias's live row
        // inside the enqueue transaction (never a precheck that can
        // race). It is a typed argument like `human`: stripped from the
        // forwarded params, so `expected_recipient_uid` is never a send
        // request field on any verb.
        // The uid is borrowed from the request, never copied: the
        // forwarded params below carry their own copy, and this typed
        // argument only has to outlive the enqueue call.
        let expected_uid = match params.get("expected_recipient_uid") {
            None | Some(Value::Null) => None,
            Some(Value::String(uid)) if !uid.is_empty() => Some(uid.as_str()),
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
            expected_uid,
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

#[cfg(test)]
mod cad1700_projection_tests {
    use super::*;
    use crate::store::{
        ConversationKind, NewAgent, NewEntry, KIND_ASSISTANT_TEXT, KIND_MESSAGE, KIND_TOOL_CALL,
        KIND_TURN_RESULT, ROLE_AGENT, ROLE_OPERATOR, ROLE_SYSTEM,
    };
    use crate::test_seam::{scoped, Asserted};

    fn fixture() -> (tempfile::TempDir, Arc<Shared>) {
        let dir = tempfile::tempdir().unwrap();
        let opts = ServeOptions {
            provider_env: crate::adapter::ProviderEnv::refusing_providers(),
            ..Default::default()
        };
        opts.provider_env
            .set("CADENCE_PM_DIR", dir.path().join("no-pm").to_str().unwrap());
        let shared = Shared::new(dir.path(), &opts).unwrap();
        let cwd = dir.path().to_str().unwrap().to_string();
        for (alias, provider, kind) in [("master", "pi", "managed"), ("w1", "fake", "fake")] {
            shared
                .store
                .register_agent(&NewAgent {
                    alias,
                    provider,
                    endpoint_kind: kind,
                    role: "worker",
                    cwd: &cwd,
                    sandbox: "read-only",
                    instructions: None,
                    params: None,
                    team_role: None,
                    model_policy: None,
                })
                .unwrap();
        }
        // `thread_append` writes the alias's existing home thread; it
        // never creates one (the send path does).
        for alias in ["master", "w1"] {
            shared.store.ensure_thread(alias).unwrap();
        }
        (dir, shared)
    }

    fn append(shared: &Arc<Shared>, alias: &str, role: &str, kind: &str, text: &str) -> i64 {
        shared
            .store
            .thread_append(
                alias,
                NewEntry {
                    role,
                    kind,
                    text,
                    payload: None,
                    message_id: None,
                },
            )
            .unwrap()
            .expect("the entry is stored")
    }

    fn read(shared: &Arc<Shared>, params: Value) -> Value {
        scoped(Asserted::Operator, || {
            shared.dispatch("thread_read", &params, std::process::id())
        })
        .expect("thread_read answers")
    }

    /// The newest page is bounded: after 210 non-reply rows the last real
    /// assistant reply is off it, so a client recomputing the unread
    /// watermark from the page loses it. `reply_seq` must be the complete,
    /// authorized thread watermark regardless of the tail/forward/older
    /// cursor, and non-reply, blank and foreign rows must not advance it.
    /// Red before CAD-1700 P2: the page had no `reply_seq` at all.
    #[test]
    fn cad1700_thread_read_tail_keeps_the_complete_reply_watermark() {
        let (_dir, shared) = fixture();
        append(
            &shared,
            "master",
            ROLE_OPERATOR,
            KIND_MESSAGE,
            "Please review",
        );
        append(&shared, "master", ROLE_AGENT, KIND_TOOL_CALL, "git status");
        let reply = append(
            &shared,
            "master",
            ROLE_AGENT,
            KIND_ASSISTANT_TEXT,
            "The review is ready",
        );
        append(
            &shared,
            "master",
            ROLE_SYSTEM,
            KIND_TURN_RESULT,
            "A system notice is not a reply",
        );
        append(
            &shared,
            "master",
            ROLE_OPERATOR,
            KIND_ASSISTANT_TEXT,
            "An operator note is not a reply",
        );
        for blank in ["", " \t\r\n", "\u{00a0}\u{2003}\u{feff}"] {
            append(&shared, "master", ROLE_AGENT, KIND_ASSISTANT_TEXT, blank);
            append(&shared, "master", ROLE_AGENT, KIND_TURN_RESULT, blank);
        }
        // Another agent's real reply belongs to that agent's own thread.
        append(
            &shared,
            "w1",
            ROLE_AGENT,
            KIND_ASSISTANT_TEXT,
            "A foreign reply",
        );
        for i in 0..210 {
            append(
                &shared,
                "master",
                ROLE_AGENT,
                KIND_TOOL_CALL,
                &format!("step {i}"),
            );
        }
        let tail = read(
            &shared,
            json!({"alias": "master", "tail": true, "limit": 200}),
        );
        let entries = tail["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 200, "the page itself stays bounded");
        assert!(
            entries[0]["seq"].as_i64().unwrap() > reply,
            "the real reply is off the newest page"
        );
        assert_eq!(
            tail["reply_seq"],
            json!(reply),
            "a cold tail page still reports the complete reply watermark"
        );
        let forward = read(&shared, json!({"alias": "master", "after": 0, "limit": 5}));
        assert_eq!(
            forward["reply_seq"],
            json!(reply),
            "a forward page reports the same complete watermark"
        );
        let older = read(
            &shared,
            json!({"alias": "master", "before": reply + 1, "limit": 200}),
        );
        assert_eq!(
            older["reply_seq"],
            json!(reply),
            "an older page reports the same complete watermark"
        );
    }

    /// `reply_seq` is the watermark of the thread the read authorized,
    /// never another conversation's or another alias's reply.
    #[test]
    fn cad1700_thread_read_reply_seq_is_scoped_to_the_read_thread() {
        let (_dir, shared) = fixture();
        let home_reply = append(
            &shared,
            "master",
            ROLE_AGENT,
            KIND_ASSISTANT_TEXT,
            "Home reply",
        );
        let (conversation, _) = shared
            .store
            .conversation_create("master", "install-fixture", None, ConversationKind::Fresh)
            .unwrap();
        let conv_reply = shared
            .store
            .thread_append_to(
                &conversation.id,
                NewEntry {
                    role: ROLE_AGENT,
                    kind: KIND_ASSISTANT_TEXT,
                    text: "Conversation reply",
                    payload: None,
                    message_id: None,
                },
            )
            .unwrap()
            .unwrap();
        assert!(conv_reply > home_reply, "the conversation reply is newer");
        let conv = read(
            &shared,
            json!({"alias": "master", "conversation": conversation.id,
                   "tail": true, "limit": 200}),
        );
        assert_eq!(conv["reply_seq"], json!(conv_reply));
        assert_eq!(conv["thread"]["id"], json!(conversation.id));
        let home = read(
            &shared,
            json!({"alias": "master", "tail": true, "limit": 200}),
        );
        assert_eq!(
            home["reply_seq"],
            json!(home_reply),
            "another conversation's reply never advances Home"
        );
    }
}
