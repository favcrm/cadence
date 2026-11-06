//! CAD-810: topic/subscription RPCs — `topic_create`, `topic_subscribe`,
//! `topic_unsubscribe`, `topic_publish`, `topic_read`.
//!
//! The caller is derived from the connection (`agent_caller` / operator
//! proof), never from a request field. The allowlists are the CAD-810
//! caller rule, not an operator-only gate:
//!
//! - operator: `fleet:` and `project:` topics (create, subscribe, publish,
//!   read) and any topic it may read or publish to;
//! - a job's PM: `peer:<its-job>` (create) and subscribe/publish for its own
//!   job's members only;
//! - a worker: publish and read only on a topic it is subscribed to.
//!
//! The subscriber list is the allowlist for publish and read; a forged
//! topic or source is refused before anything is written.

use super::*;
use crate::proto::identifier;
use crate::store::Topic;

/// The topic type of a `type:source` wire pair, validated.
fn topic_kind(kind: &str) -> Result<&str> {
    match kind {
        "fleet" | "project" | "job" | "peer" => Ok(kind),
        _ => Err(Error::rejected(
            "topic type must be fleet, project, job or peer",
        )),
    }
}

/// The job id named by a `peer:` or `job:` topic source, validated.
fn job_source(source: &str) -> Result<&str> {
    identifier(source, "job ID")?;
    Ok(source)
}

fn reject_fields(params: &Value, allowed: &[&str], verb: &str) -> Result<()> {
    if let Some(obj) = params.as_object() {
        if let Some(field) = obj.keys().find(|k| !allowed.contains(&k.as_str())) {
            return Err(Error::rejected(format!(
                "{verb} takes {} only; field '{field}' is not accepted",
                allowed.join(", ")
            )));
        }
    }
    Ok(())
}

impl Shared {
    /// The caller as the topic rule sees it: the operator, or an agent
    /// alias the daemon derived. An unproven, underivable caller is refused.
    fn topic_caller(&self, verb: &str, peer_pid: u32) -> Result<crate::peer::AgentCaller> {
        self.agent_caller(peer_pid, verb)
    }

    /// The job's members: its PM plus the assignees of its tasks.
    fn job_member_aliases(&self, job_id: &str) -> Result<Vec<String>> {
        let job = self.store.job(job_id)?;
        let mut members = vec![job.pm_alias];
        for task in self.store.tasks_for_job(job_id)? {
            if let Some(a) = task.assignee {
                if !members.contains(&a) {
                    members.push(a);
                }
            }
        }
        Ok(members)
    }

    /// CAD-810: may `caller` create `kind:source`?
    fn may_create_topic(
        &self,
        caller: &crate::peer::AgentCaller,
        kind: &str,
        source: &str,
    ) -> Result<()> {
        use crate::peer::AgentCaller::{Agent, Operator};
        match (caller, kind) {
            (Operator, "fleet" | "project") => Ok(()),
            (Agent(alias), "peer") => {
                let job = self.store.job(source)?;
                if job.pm_alias == *alias {
                    Ok(())
                } else {
                    Err(Error::rejected(
                        "topic_create refused: only the job's PM may create its peer topic",
                    ))
                }
            }
            _ => Err(Error::rejected(format!(
                "topic_create refused: this caller may not create a {kind} topic"
            ))),
        }
    }

    /// CAD-810: may `caller` change the membership of `topic`? Operator
    /// for fleet/project; the job's PM for its own job's members. A worker
    /// may not change membership.
    /// CAD-1412: a human room is reached only through the human-threads
    /// RPCs and their verified ACL. The legacy topic verbs serve every
    /// topic to the operator, so a human room is refused here before any
    /// caller check (its transcript and membership never leak this way).
    fn refuse_human_room(topic: &Topic, verb: &str) -> Result<()> {
        if topic.kind == crate::store::human_threads::ROOM_TOPIC_TYPE {
            return Err(Error::rejected(format!(
                "{verb} refused: this topic is a private human room — use the human-threads API"
            )));
        }
        Ok(())
    }

    /// CAD-1412: the durable uid of an agent caller of a human room. The
    /// operator's generic topic verbs never reach a human room's transcript.
    fn human_room_agent(&self, caller: &crate::peer::AgentCaller, verb: &str) -> Result<String> {
        match caller {
            crate::peer::AgentCaller::Agent(alias) => Ok(self.store.agent(alias)?.uid),
            crate::peer::AgentCaller::Operator => Err(Error::rejected(format!(
                "{verb} refused: this topic is a private human room — use the human-threads API"
            ))),
        }
    }

    /// CAD-1412: an agent member reads a human room's transcript page. The
    /// store rechecks the active membership and the join cutoff.
    fn human_room_agent_read(
        &self,
        caller: &crate::peer::AgentCaller,
        topic: &Topic,
        params: &Value,
        limit: i64,
    ) -> Result<Value> {
        let uid = self.human_room_agent(caller, "topic_read")?;
        if params.get("after").is_some() {
            return Err(Error::rejected(
                "topic_read: a human room pages by before only",
            ));
        }
        let before = optional_i64(params, "before");
        let org = crate::board_identity::read_config(&self.state_dir)?.company;
        // CAD-2207: a member's personal assistant is not readable by another
        // agent merely because both sit in the same shared room. It is withheld
        // from every agent connection but itself; the store's membership and
        // join-cutoff checks still decide the ordinary rows. Author identity is
        // resolved from durable state (the binding survives a removed agents
        // row), and withheld rows must not shorten the page: older pages are
        // scanned until `limit` visible posts (or the history/cap) are reached.
        let reader = match caller {
            crate::peer::AgentCaller::Agent(alias) => alias.as_str(),
            // `human_room_agent` above already refused the operator.
            crate::peer::AgentCaller::Operator => "",
        };
        let mut post_visibility: std::collections::BTreeMap<(String, String), bool> =
            Default::default();
        let mut visible: Vec<store::TopicPost> = Vec::new();
        let mut scan_cursor = before;
        let mut more_before = false;
        let mut scanned: i64 = 0;
        // The oldest seq this scan examined; the next page starts strictly
        // below it, so a withheld-only tail never strands the client.
        let mut oldest_examined: Option<i64> = None;
        while (visible.len() as i64) < limit {
            let (page, page_more) =
                self.store
                    .human_agent_read(&org, &uid, &topic.id, scan_cursor, limit)?;
            if page.is_empty() {
                more_before = false;
                break;
            }
            let page_len = page.len() as i64;
            scanned += page_len;
            oldest_examined = page.first().map(|post| post.seq);
            let mut processed = 0i64;
            let mut last_processed: Option<i64> = None;
            for post in page.into_iter().rev() {
                if (visible.len() as i64) >= limit {
                    break;
                }
                processed += 1;
                last_processed = Some(post.seq);
                let key = (
                    post.sender_uid.clone().unwrap_or_default(),
                    post.sender.clone(),
                );
                let shown = if let Some(shown) = post_visibility.get(&key) {
                    *shown
                } else {
                    let shown =
                        match self.room_post_author(post.sender_uid.as_deref(), &post.sender)? {
                            super::members_rpc::RoomPostAuthor::Ordinary => true,
                            super::members_rpc::RoomPostAuthor::Private { alias, .. } => {
                                alias == reader
                            }
                            super::members_rpc::RoomPostAuthor::Unproven => false,
                        };
                    post_visibility.insert(key, shown);
                    shown
                };
                if shown {
                    visible.push(post);
                }
            }
            if processed < page_len {
                // Stopped at `limit` inside the page: older rows remain.
                more_before = true;
                oldest_examined = last_processed;
                break;
            }
            more_before = page_more;
            if !page_more || scanned >= limit.saturating_mul(8) {
                break;
            }
            scan_cursor = oldest_examined;
        }
        visible.reverse();
        let mut out: Vec<Value> = Vec::with_capacity(visible.len());
        for post in &visible {
            let mut row = post.to_json();
            row["body"] = json!(store::conversations::body(&post.body));
            row["request"] = Value::Null;
            row["deliveries"] = json!([]);
            out.push(row);
        }
        Ok(json!({
            "topic": topic.to_json(),
            "posts": out,
            "cursor": oldest_examined.unwrap_or(0),
            "more_before": more_before,
        }))
    }

    fn may_manage_members(
        &self,
        caller: &crate::peer::AgentCaller,
        topic: &Topic,
        member: &str,
    ) -> Result<()> {
        use crate::peer::AgentCaller::{Agent, Operator};
        match caller {
            Operator => Ok(()),
            Agent(alias) if matches!(topic.kind.as_str(), "job" | "peer") => {
                let job = job_source(&topic.source)?;
                let job_row = self.store.job(job)?;
                let members = self.job_member_aliases(job)?;
                if job_row.pm_alias == *alias && members.iter().any(|m| m == member) {
                    Ok(())
                } else {
                    Err(Error::rejected(
                        "topic membership refused: only the job's PM may manage its own members",
                    ))
                }
            }
            _ => Err(Error::rejected(
                "topic membership refused: this caller may not change the topic's members",
            )),
        }
    }

    /// CAD-810: may `caller` publish to, or read, `topic`? The operator may;
    /// an agent only when it is a current subscriber (the subscriber list is
    /// the allowlist).
    fn may_use_topic(
        &self,
        caller: &crate::peer::AgentCaller,
        topic: &Topic,
        verb: &str,
    ) -> Result<()> {
        use crate::peer::AgentCaller::{Agent, Operator};
        match caller {
            Operator => Ok(()),
            Agent(alias) => {
                let agent = self.store.agent(alias)?;
                if self.store.topic_is_subscriber(&topic.id, &agent.uid)? {
                    Ok(())
                } else {
                    Err(Error::rejected(format!(
                        "{verb} refused: agent '{alias}' is not subscribed to this topic"
                    )))
                }
            }
        }
    }

    /// `topic_create {type, source}` — idempotent: an existing topic is
    /// returned, `created: false`.
    pub(super) fn rpc_topic_create(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        reject_fields(params, &["type", "source"], "topic_create")?;
        let caller = self.topic_caller("topic_create", peer_pid)?;
        let kind = topic_kind(required_str(params, "type")?)?;
        let source = required_str(params, "source")?;
        if kind == "job" || kind == "peer" {
            job_source(source)?;
        } else {
            identifier(source, "topic source")?;
        }
        self.may_create_topic(&caller, kind, source)?;
        let (topic, created) = self.store.topic_ensure(kind, source)?;
        // Finalized shape (contract): the topic id as `topic`, flat
        // `type`/`source`, and `created` true only on the first create.
        Ok(json!({
            "topic": topic.id,
            "type": topic.kind,
            "source": topic.source,
            "created": created,
        }))
    }

    /// `topic_subscribe {topic, alias}` / `topic_unsubscribe {topic, alias}`.
    pub(super) fn rpc_topic_membership(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
        subscribe: bool,
    ) -> Result<Value> {
        let verb = if subscribe {
            "topic_subscribe"
        } else {
            "topic_unsubscribe"
        };
        reject_fields(params, &["topic", "alias"], verb)?;
        let caller = self.topic_caller(verb, peer_pid)?;
        let topic_id_value = required_str(params, "topic")?;
        identifier(topic_id_value, "topic ID")?;
        let topic = self
            .store
            .topic(topic_id_value)?
            .ok_or_else(|| Error::rejected("Unknown topic"))?;
        let member = self.resolve_alias(required_str(params, "alias")?)?;
        let agent = self.store.agent(&member)?;
        Self::refuse_human_room(&topic, verb)?;
        self.may_manage_members(&caller, &topic, &member)?;
        if subscribe {
            self.store.topic_subscribe(&topic.id, &agent.uid)?;
        } else {
            self.store.topic_unsubscribe(&topic.id, &agent.uid)?;
        }
        Ok(json!({"topic": topic.id, "alias": member, "uid": agent.uid,
                  "subscribed": subscribe}))
    }

    /// `topic_publish {topic, text, id}` — one shared transcript row, then
    /// one turnless delivery to each current subscriber through the
    /// existing queue, each woken. A retry of the same `id` is a duplicate
    /// and creates no second row or delivery.
    pub(super) fn rpc_topic_publish(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        reject_fields(params, &["topic", "text", "id"], "topic_publish")?;
        let caller = self.topic_caller("topic_publish", peer_pid)?;
        let topic_key = required_str(params, "topic")?;
        identifier(topic_key, "topic ID")?;
        let topic = self
            .store
            .topic(topic_key)?
            .ok_or_else(|| Error::rejected("Unknown topic"))?;
        let text = required_str(params, "text")?;
        let client_id = required_str(params, "id")?;
        identifier(client_id, "post ID")?;
        if topic.kind == crate::store::human_threads::ROOM_TOPIC_TYPE {
            let uid = self.human_room_agent(&caller, "topic_publish")?;
            let org = crate::board_identity::read_config(&self.state_dir)?.company;
            let (post, fresh) = self
                .store
                .human_agent_post(&org, &uid, &topic.id, client_id, text)?;
            return Ok(json!({
                "topic": topic.id,
                "message": post.id,
                "duplicate": !fresh,
                "deliveries": [],
            }));
        }
        self.may_use_topic(&caller, &topic, "topic_publish")?;
        let (sender_uid, sender_label) = match &caller {
            crate::peer::AgentCaller::Operator => (None, "operator".to_string()),
            crate::peer::AgentCaller::Agent(alias) => {
                (Some(self.store.agent(alias)?.uid), alias.clone())
            }
        };
        // One transaction writes the post, every subscriber's intent and each
        // daemon topic notice. Notices never start a turn: the store makes them
        // available without touching the adapter (see `consume_topic_notice_in`).
        let (post, fresh) = self.store.topic_publish(
            &topic.id,
            client_id,
            sender_uid.as_deref(),
            &sender_label,
            text,
        )?;
        let stored = self.store.topic_deliveries(&post)?;
        // Wake each notified member after the commit, so its parked actor
        // claims the notice now (the claim consumes it without a turn).
        if fresh {
            for d in &stored {
                self.notify_agent(&d.alias);
            }
            self.wake();
        }
        let deliveries: Vec<Value> = stored
            .into_iter()
            .map(|d| {
                json!({"alias": d.alias, "uid": d.uid, "message": d.message,
                       "state": d.state})
            })
            .collect();
        Ok(json!({
            "topic": topic.id,
            "message": post,
            "duplicate": !fresh,
            "deliveries": deliveries,
        }))
    }

    /// CAD-818: the Threads stamp the board's live feed fingerprints. It is
    /// the newest topic-changed event and the newest message seq, read-only.
    /// Its value moves on every topic create/membership/post, and on every
    /// conversation message, so the feed invalidates the Threads view.
    pub(super) fn rpc_threads_stamp(self: &Arc<Self>) -> Result<Value> {
        let (topic_seq, message_seq, transition_ms) = self.store.threads_stamp()?;
        Ok(json!({"topic_event": topic_seq, "message": message_seq,
                  "transition_ms": transition_ms}))
    }

    /// CAD-821 `topic_request {topic, target, text, id}` — the one-target
    /// Ask. Operator-only. The target must be a current subscriber.
    ///
    /// Order, so no refusal leaves a side effect:
    /// 1. topic, target and membership are checked first;
    /// 2. the request message id is scoped to the topic and the client id
    ///    (`topic-req-<hash>`), so two topics never share one;
    /// 3. a retry is compared with the stored post: same envelope returns
    ///    the bound request with `duplicate: true`, a different one is a
    ///    conflict — both before any send;
    /// 4. the transcript post is written, then the one request is queued
    ///    through `send_as` (the `thread_send` path: same envelope,
    ///    idempotence and `operator` source), then the post is bound to it.
    pub(super) fn rpc_topic_request(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        reject_fields(params, &["topic", "target", "text", "id"], "topic_request")?;
        self.operator_chat("topic_request", peer_pid)?;
        let topic_key = required_str(params, "topic")?;
        identifier(topic_key, "topic ID")?;
        let topic = self
            .store
            .topic(topic_key)?
            .ok_or_else(|| Error::rejected("Unknown topic"))?;
        Self::refuse_human_room(&topic, "topic_request")?;
        let text = required_str(params, "text")?;
        let client_id = required_str(params, "id")?;
        identifier(client_id, "request ID")?;
        let target = self.resolve_alias(required_str(params, "target")?)?;
        let target_uid = self.store.agent(&target)?.uid;
        if !self.store.topic_is_subscriber(&topic.id, &target_uid)? {
            return Err(Error::rejected(
                "topic_request refused: target is not a member of this group",
            ));
        }
        let post = crate::store::topic_post_id(&topic.id, client_id);
        let request_id = crate::store::topic_request_id(&topic.id, client_id);
        // Envelope check before any side effect: an existing post under this
        // client id must match the same target, text and topic.
        if let Some(existing) = self.store.topic_post_by_id(&post)? {
            if existing.body != text || existing.sender != "operator" {
                return Err(Error::rejected(
                    "Request id was already used with different content",
                ));
            }
            if let Some(bound) = self.store.topic_post_request(&post)? {
                // A retry names its target again: the bound request must have
                // been queued to that same UID, or the retry is refused. A
                // missing or unaddressed request fails closed.
                let bound_uid = self.store.message(&bound)?.and_then(|m| m.recipient_uid);
                if bound_uid.as_deref() != Some(target_uid.as_str()) {
                    return Err(Error::rejected(
                        "Request id was already used with a different target",
                    ));
                }
                return Ok(json!({
                    "topic": topic.id,
                    "message": post,
                    "request": bound,
                    "target": {"alias": target, "uid": target_uid},
                    "duplicate": true,
                }));
            }
        }
        // The one request's body names the group. Its real UTF-8 byte length
        // is checked here, before any post insert or send, so an oversize
        // wrapped body leaves no committed post, request or bind.
        let body = format!("[{} {}] {}", topic.kind, topic.source, text);
        if body.len() > store::ENQUEUE_BYTES {
            return Err(Error::rejected("Prompt must contain 1-48000 characters"));
        }
        // The post is written once. A retry that finds an unbound post (a
        // crash between the post and the send) falls through to the send:
        // the stable request id and envelope make that send idempotent, so
        // it returns the same request, never a second one.
        let (post, _fresh) = self
            .store
            .topic_post_insert(&topic.id, client_id, None, "operator", text)?;
        // The one request, through the thread_send path (operator source,
        // caller-derived, id-idempotent).
        let send =
            json!({"alias": target, "text": body, "message": request_id, "source": "operator"});
        let receipt = self.send_with_human_expected(
            &send,
            &|_| Ok(crate::store::Sender::OperatorChat),
            &|verb| {
                Err(Error::rejected(format!(
                    "{verb} refused: this send carries no caller to steer with"
                )))
            },
            None,
            Some(&target_uid),
            Some(super::messages_rpc::SendBinding::TopicPost(&post)),
        )?;
        let request = receipt
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or(&request_id)
            .to_string();
        Ok(json!({
            "topic": topic.id,
            "message": post,
            "request": request,
            "target": {"alias": target, "uid": target_uid},
            "duplicate": receipt.get("duplicate").and_then(Value::as_bool).unwrap_or(false),
        }))
    }

    /// `topic_read {topic, tail?|before?|after?, limit?}` — the shared
    /// transcript page. Read-only, and a non-member agent is refused.
    pub(super) fn rpc_topic_read(self: &Arc<Self>, params: &Value, peer_pid: u32) -> Result<Value> {
        reject_fields(
            params,
            &["topic", "tail", "before", "after", "limit"],
            "topic_read",
        )?;
        let caller = self.topic_caller("topic_read", peer_pid)?;
        let topic_key = required_str(params, "topic")?;
        identifier(topic_key, "topic ID")?;
        let topic = self
            .store
            .topic(topic_key)?
            .ok_or_else(|| Error::rejected("Unknown topic"))?;
        let limit = optional_i64(params, "limit").unwrap_or(50);
        if !(1..=crate::store::THREAD_PAGE_MAX).contains(&limit) {
            return Err(Error::rejected(format!(
                "Topic page limit must be 1-{}",
                crate::store::THREAD_PAGE_MAX
            )));
        }
        if topic.kind == crate::store::human_threads::ROOM_TOPIC_TYPE {
            return self.human_room_agent_read(&caller, &topic, params, limit);
        }
        self.may_use_topic(&caller, &topic, "topic_read")?;
        let before = optional_i64(params, "before");
        let after = optional_i64(params, "after");
        let posts = self.store.topic_posts(&topic.id, before, after, limit)?;
        let cursor = posts.last().map(|p| p.seq).unwrap_or(0);
        let more_before = match posts.first() {
            Some(first) => self.store.topic_has_posts_before(&topic.id, first.seq)?,
            None => false,
        };
        let mut out = Vec::with_capacity(posts.len());
        for post in &posts {
            let mut row = post.to_json();
            row["body"] = json!(store::conversations::body(&post.body));
            let request = self.store.topic_post_request(&post.id)?;
            let mut receipts = Vec::new();
            if let Some(req) = &request {
                // An Ask has one bound request: its one receipt names the
                // recipient the request was queued to, not every subscriber.
                let (uid, alias, state) = match self.store.message(req)? {
                    Some(m) => (m.recipient_uid, m.recipient, m.state),
                    None => (None, None, "unknown".into()),
                };
                let removed = match &uid {
                    Some(u) => self.store.alias_of_uid(u)?.is_none(),
                    None => false,
                };
                receipts.push(json!({
                    "uid": uid,
                    "alias": alias,
                    "removed": removed,
                    "message": req,
                    "state": state,
                }));
            } else {
                for d in self.store.topic_deliveries(&post.id)? {
                    // A queued delivery shows its message row's live state (a
                    // queued notice is never shown as seen); a pending intent
                    // shows `pending`.
                    let state = match &d.message {
                        Some(id) => self
                            .store
                            .message(id)?
                            .map(|m| m.state)
                            .unwrap_or_else(|| "unknown".into()),
                        None => d.state.clone(),
                    };
                    let removed = self.store.alias_of_uid(&d.uid)?.is_none();
                    receipts.push(json!({
                        "uid": d.uid,
                        "alias": d.alias,
                        "removed": removed,
                        "message": d.message,
                        "state": state,
                    }));
                }
            }
            row["request"] = json!(request);
            row["deliveries"] = Value::Array(receipts);
            out.push(row);
        }
        Ok(json!({
            "topic": topic.to_json(),
            "posts": out,
            "cursor": cursor,
            "more_before": more_before,
        }))
    }
}
