//! Human room RPCs (CAD-1412). Each room method takes `token` (the board's
//! public session cookie, relayed by the HTTP layer), then runs the one
//! native guard `human_current_actor`: operator connection, stored scope
//! checked against server config, the live roster from the canonical public
//! host, and the directory write. The verified subject and the roster are
//! the only identity inputs; request fields never supply tenancy.
//!
//! Room membership is identity authority only: nothing here grants
//! operator, approval, merge or agent-execution power.

use std::sync::Arc;

use serde_json::{json, Value};

use cadence_core::board_api::{HUMAN_ROOM_CURSOR_FORMAT, HUMAN_ROOM_SCOPE_ACK, READER_SCOPE_FIELD};

use super::job_events_opaque;
use super::scoped_cursor::{CursorBinding, CursorDirection, ScopedCursorCodec};
use super::{optional_i64, optional_str, required_str, Shared};
use crate::error::{Error, Result};
use crate::store::human_threads::{
    HumanRoomCursorScope, HumanRoomMember, HumanRoomSummary, RoomMember, VerifiedHuman,
    ROOM_TOPIC_TYPE,
};
use crate::store::TopicPost;

/// The longest read page one call returns.
const HISTORY_MAX: i64 = 100;

fn room_param(params: &Value) -> Result<&str> {
    let room = required_str(params, "room")?;
    if room.is_empty() || room.len() > 96 {
        return Err(Error::invalid("invalid_request", "room id is not valid"));
    }
    Ok(room)
}

fn client_id_param(params: &Value) -> Result<&str> {
    let id = required_str(params, "client_id")?;
    if id.is_empty() || id.len() > 128 {
        return Err(Error::invalid("invalid_request", "client_id is not valid"));
    }
    Ok(id)
}

/// A required array of non-empty strings, at most 100 entries.
fn string_list(params: &Value, field: &str) -> Result<Vec<String>> {
    match params.get(field) {
        None | Some(Value::Null) => Ok(vec![]),
        Some(Value::Array(items)) if items.len() <= 100 => items
            .iter()
            .map(|v| {
                v.as_str()
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .ok_or_else(|| {
                        Error::invalid("invalid_request", format!("{field} must be strings"))
                    })
            })
            .collect(),
        Some(_) => Err(Error::invalid(
            "invalid_request",
            format!("{field} must be an array of at most 100 entries"),
        )),
    }
}

fn summary_json(row: &HumanRoomSummary) -> Value {
    json!({
        "id": row.id,
        "kind": row.kind,
        "title": row.title,
        "creator": row.creator,
        "created": row.created,
        "unread": row.unread,
    })
}

fn group_json(g: &crate::store::human_threads::NotificationGroup) -> Value {
    json!({
        "room": g.room,
        "kind": g.kind,
        "title": g.title,
        "unread": g.unread,
        "muted": g.muted,
        "mention": g.mention,
    })
}

fn prefs_json(p: &crate::store::human_threads::NotificationPrefs) -> Value {
    json!({
        "dm": p.dm,
        "mention": p.mention,
        "sound": p.sound,
        "preview": p.preview,
        "quiet": p.quiet.as_str(),
        "zone": p.zone,
    })
}

fn member_json(m: &HumanRoomMember) -> Value {
    json!({"kind": m.kind, "id": m.id, "label": m.label})
}

fn require_cursor_format(params: &Value) -> Result<()> {
    if params.get("cursor_format").and_then(Value::as_str) != Some(HUMAN_ROOM_CURSOR_FORMAT) {
        return Err(Error::invalid(
            "human_room_cursor_format",
            "human room requires opaque-v1 cursors",
        ));
    }
    Ok(())
}

fn cursor_param<'a>(params: &'a Value, field: &str) -> Result<Option<&'a str>> {
    match params.get(field) {
        None => Ok(None),
        Some(Value::String(token)) if !token.is_empty() => Ok(Some(token)),
        Some(Value::Number(_)) => Err(Error::invalid(
            "human_room_cursor_numeric",
            "human room requires an opaque cursor",
        )),
        Some(_) => Err(Error::invalid(
            "human_room_cursor_invalid",
            "human room cursor is invalid",
        )),
    }
}

/// Bind positions to the verified human, room incarnation and join boundary.
struct HumanRoomCursors {
    codec: ScopedCursorCodec,
    principal: String,
    resource: String,
}

impl HumanRoomCursors {
    fn new(
        shared: &Shared,
        actor: &VerifiedHuman,
        room: &str,
        scope: &HumanRoomCursorScope,
    ) -> Result<Self> {
        let created = format!("{:016x}", scope.created);
        let cutoff = scope.cutoff.to_string();
        Ok(Self {
            codec: shared.scoped_cursor_codec()?,
            principal: job_events_opaque::framed(&[&actor.org, &actor.id]),
            resource: job_events_opaque::framed(&[
                "human-room",
                room,
                &scope.topic,
                &created,
                &cutoff,
            ]),
        })
    }

    fn binding(&self, direction: CursorDirection) -> CursorBinding<'_> {
        CursorBinding {
            domain: match direction {
                CursorDirection::Backward => "human-room-history",
                CursorDirection::Forward => "human-room-marker",
            },
            direction,
            principal: &self.principal,
            resource: &self.resource,
            filters: "",
        }
    }

    fn open(&self, token: &str, direction: CursorDirection) -> Result<i64> {
        self.codec
            .open(token, &self.binding(direction))
            .map_err(|_| {
                Error::invalid("human_room_cursor_invalid", "human room cursor is invalid")
            })
    }

    fn seal(&self, seq: i64, direction: CursorDirection) -> Result<String> {
        self.codec
            .seal(seq, &self.binding(direction))
            .map_err(|_| super::scoped_cursor_config::unavailable())
    }

    /// Stable only for this principal and membership incarnation; unlike a
    /// cursor, this contains no page position and permits safe page merging.
    fn scope_id(&self) -> Result<String> {
        self.codec
            .stable_row_id(
                "human-room-scope",
                &self.resource,
                self.principal.as_bytes(),
            )
            .map_err(|_| super::scoped_cursor_config::unavailable())
    }

    /// Never relay the global sequence, including on write receipts.
    fn post(
        &self,
        post: &TopicPost,
        cutoff: i64,
        meta: Option<&crate::store::human_threads::HumanPostMeta>,
    ) -> Result<Value> {
        if post.seq <= cutoff {
            return Err(Error::rejected("human room post is unavailable"));
        }
        Ok(json!({
            "id": post.id,
            "sender": {"id": post.sender_uid, "label": post.sender},
            "body": crate::store::conversations::body(&post.body),
            "created": post.created,
            "read_cursor": self.seal(post.seq, CursorDirection::Forward)?,
            "silent": meta.is_some_and(|m| m.silent),
            "outcomes": meta.map(|m| m.outcomes.as_slice()).unwrap_or(&[]),
        }))
    }
}

#[cfg(test)]
fn inject_page_fault(shared: &Shared, page: &mut Value) {
    if let Some(fault) = &shared.human_room_page_fault {
        match fault.load(std::sync::atomic::Ordering::Acquire) {
            1 => {
                page.as_object_mut()
                    .expect("response object")
                    .remove(READER_SCOPE_FIELD);
            }
            2 => {
                if let Some(post) = page.get_mut("post") {
                    post["seq"] = json!(1);
                } else if let Some(post) = page
                    .get_mut("posts")
                    .and_then(Value::as_array_mut)
                    .and_then(|p| p.first_mut())
                {
                    post["seq"] = json!(1);
                }
            }
            3 => page[READER_SCOPE_FIELD] = json!("wrong-scope"),
            _ => {}
        }
    }
}

/// The stored peer subject must name a current roster member; the store
/// principal is then derived from the verified tenancy and exact subject.
fn roster_peer(
    current: &super::human_current::CurrentActor,
    subject: &str,
) -> Result<VerifiedHuman> {
    if !current
        .roster
        .members
        .iter()
        .any(|m| m.subject_id == subject)
    {
        return Err(Error::rejected(
            "that person is not a current member of this organization",
        ));
    }
    VerifiedHuman::verified(
        &current.scope.issuer,
        &current.scope.organization_id,
        subject,
    )
}

impl Shared {
    /// `human_roster_scope {token}` — identity only, for the HTTP layer's
    /// tenancy display. No roster fetch, no authority.
    pub(super) fn rpc_human_roster_scope(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        let token = required_str(params, "token")?;
        let (scope, sub, _) = self.verified_human_session(token, params, peer_pid)?;
        Ok(json!({"scope": {
            "issuer": scope.issuer,
            "organization_id": scope.organization_id,
            "subject": sub,
        }}))
    }

    /// Search the complete live roster before bounding the browser's results.
    /// The same current-actor guard as room writes owns identity and tenancy.
    pub(super) fn rpc_human_people_search(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        let current = self.human_current_actor(required_str(params, "token")?, params, peer_pid)?;
        let query = optional_str(params, "query")
            .unwrap_or("")
            .trim()
            .to_lowercase();
        let mut matches = current.roster.members.iter().filter(|member| {
            member.subject_id != current.subject
                && (query.is_empty() || member.name.to_lowercase().contains(&query))
        });
        let people = matches
            .by_ref()
            .take(20)
            .map(|member| {
                let human = VerifiedHuman::with_name(
                    &current.scope.issuer,
                    &current.scope.organization_id,
                    &member.subject_id,
                    &member.name,
                )?;
                Ok(json!({"id": human.id, "subject": member.subject_id, "name": human.name}))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(
            json!({"actor": current.actor.id, "people": people, "has_more": matches.next().is_some()}),
        )
    }

    /// `human_agents_list {token}` — active shared agents by durable uid.
    /// Archived agents and personal assistants (`assistants` table) are excluded.
    pub(super) fn rpc_human_agents_list(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        self.human_current_actor(required_str(params, "token")?, params, peer_pid)?;
        let agents: Vec<Value> = self
            .store
            .human_shareable_agents()?
            .into_iter()
            .map(|(uid, alias)| json!({"uid": uid, "alias": alias}))
            .collect();
        Ok(json!({"agents": agents}))
    }

    /// `human_room_ask {token, room, agent_uid, client_id, body}`. Explicit
    /// Ask to one current agent member. Order, so no refusal leaves a side
    /// effect: target and membership are checked first; the human's post is
    /// written; then exactly one normal request is queued with a stable id
    /// and bound to that post. The queued text carries only opaque ids: the
    /// private body stays in the room transcript.
    /// The one Ask authority: the operator's chat proof, the verified session
    /// is the signed operator, and the live roster role is `owner`. Read-only,
    /// shared by `can_ask` (the UI hint) and the Ask itself, so they cannot
    /// diverge. Membership confers none of it.
    pub(super) fn ask_authority(
        &self,
        token: &str,
        peer_pid: u32,
        current: &super::human_current::CurrentActor,
    ) -> bool {
        if self.operator_chat("human_room_ask", peer_pid).is_err() {
            return false;
        }
        let signed_operator = self
            .operator_auth()
            .verified_human_scope(token, self.operator_now())
            .ok()
            .flatten()
            .is_some_and(|(_, user)| user.is_operator());
        signed_operator && current.self_role == "owner"
    }

    pub(super) fn rpc_human_room_ask(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        let token = required_str(params, "token")?;
        require_cursor_format(params)?;
        let current = self.human_current_actor(token, params, peer_pid)?;
        if !self.ask_authority(token, peer_pid, &current) {
            return Err(Error::rejected(
                "Asking an agent needs the operator role on this board",
            ));
        }
        let room = room_param(params)?;
        let scope = self.store.human_room_cursor_scope(&current.actor, room)?;
        let cursors = HumanRoomCursors::new(self, &current.actor, room, &scope)?;
        let agent_uid = required_str(params, "agent_uid")?;
        let key = client_id_param(params)?;
        let body = required_str(params, "body")?;
        let (topic, alias) = self
            .store
            .human_room_ask_target(&current.actor, room, agent_uid)?;
        let post_key = crate::store::topic_post_id(&topic, key);
        let request_id = crate::store::topic_request_id(&topic, key);
        // A retry of a bound Ask must name the same member uid, or it is refused
        // before any post or send (no duplicate to a reassigned or other member).
        if let Some(bound) = self.store.topic_post_request(&post_key)? {
            let bound_uid = self.store.message(&bound)?.and_then(|m| m.recipient_uid);
            if bound_uid.as_deref() != Some(agent_uid) {
                return Err(Error::rejected(
                    "Request id was already used with a different target",
                ));
            }
        }
        let queued = format!(
            "[{ROOM_TOPIC_TYPE}] A teammate asked you a question in a private room. \
             Read it: `cadence topic read {topic}`. Reply in the same room: \
             `cadence topic publish {topic} \"<your reply>\" --id <unique-id>`. \
             Asked post: {post_key}.",
        );
        if queued.len() > crate::store::ENQUEUE_BYTES {
            return Err(Error::rejected("Prompt must contain 1-48000 characters"));
        }
        let (post, _fresh) =
            self.store
                .human_room_ask_post(&current.actor, room, agent_uid, key, body)?;
        let post_receipt = cursors.post(&post, scope.cutoff, None)?;
        // The queue names the selected member's uid; the enqueue transaction
        // refuses if the alias now belongs to another uid (no reassignment race).
        let send =
            json!({"alias": alias, "text": queued, "message": request_id, "source": "operator"});
        let receipt = self.send_with_human_expected(
            &send,
            &|_| Ok(crate::store::Sender::OperatorChat),
            &|verb| {
                Err(Error::rejected(format!(
                    "{verb} refused: this send carries no caller to steer with"
                )))
            },
            None,
            Some(agent_uid),
            Some(super::messages_rpc::SendBinding::TopicPost(&post.id)),
        )?;
        let duplicate = receipt
            .get("duplicate")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        self.reprove_human_room(token, params, peer_pid, room, &current.actor, &scope)?;
        let mut result = json!({
            "request": request_id,
            "post": post_receipt,
            "target": {"uid": agent_uid, "alias": alias},
            "duplicate": duplicate,
        });
        result[READER_SCOPE_FIELD] = json!(HUMAN_ROOM_SCOPE_ACK);
        #[cfg(test)]
        inject_page_fault(self, &mut result);
        Ok(result)
    }

    /// `human_rooms_list {token}`
    pub(super) fn rpc_human_rooms_list(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        let current = self.human_current_actor(required_str(params, "token")?, params, peer_pid)?;
        let rooms = self.store.human_rooms_for(&current.actor)?;
        Ok(json!({"rooms": rooms.iter().map(summary_json).collect::<Vec<_>>()}))
    }

    /// `human_dm_open {token, peer_subject}`
    pub(super) fn rpc_human_dm_open(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        let current = self.human_current_actor(required_str(params, "token")?, params, peer_pid)?;
        let peer = roster_peer(&current, required_str(params, "peer_subject")?)?;
        let (room, created) = self.store.human_dm_open(&current.actor, &peer.id)?;
        Ok(json!({"room": room, "created": created}))
    }

    /// `human_group_create {token, kind, title?, client_id, humans, agents}`
    pub(super) fn rpc_human_group_create(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        let current = self.human_current_actor(required_str(params, "token")?, params, peer_pid)?;
        let kind = required_str(params, "kind")?;
        let title = optional_str(params, "title").unwrap_or("Group");
        let key = client_id_param(params)?;
        let mut members = Vec::new();
        for subject in string_list(params, "humans")? {
            members.push(RoomMember::Human(roster_peer(&current, &subject)?.id));
        }
        for uid in string_list(params, "agents")? {
            let alias = self
                .store
                .alias_of_uid(&uid)?
                .ok_or_else(|| Error::rejected("Agent not found"))?;
            members.push(RoomMember::Agent(alias));
        }
        let room = self
            .store
            .human_group_create(&current.actor, kind, title, key, &members)?;
        Ok(json!({"room": room}))
    }

    fn reprove_human_room(
        &self,
        token: &str,
        params: &Value,
        peer_pid: u32,
        room: &str,
        actor: &VerifiedHuman,
        expected: &HumanRoomCursorScope,
    ) -> Result<()> {
        let current = self.human_current_actor(token, params, peer_pid)?;
        if current.actor.id != actor.id
            || self.store.human_room_cursor_scope(&current.actor, room)? != *expected
        {
            return Err(Error::rejected("human room access changed"));
        }
        Ok(())
    }

    /// `human_room_read {token, room, cursor_format, before?, limit?}`
    pub(super) fn rpc_human_room_read(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        let token = required_str(params, "token")?;
        let current = self.human_current_actor(token, params, peer_pid)?;
        require_cursor_format(params)?;
        let room = room_param(params)?;
        let limit = optional_i64(params, "limit").unwrap_or(50);
        if !(1..=HISTORY_MAX).contains(&limit) {
            return Err(Error::invalid(
                "invalid_request",
                format!("limit must be 1-{HISTORY_MAX}"),
            ));
        }
        let supplied = cursor_param(params, "before")?;
        let scope = self.store.human_room_cursor_scope(&current.actor, room)?;
        let cursors = HumanRoomCursors::new(self, &current.actor, room, &scope)?;
        let position = supplied
            .map(|token| cursors.open(token, CursorDirection::Backward))
            .transpose()?;
        let (room_kind, creator) = self.store.human_room_meta(&current.actor, room)?;
        let members = self.store.human_room_members(&current.actor, room)?;
        let (posts, more_before) =
            self.store
                .human_room_history_scoped(&current.actor, room, &scope, position, limit)?;
        let before = posts
            .first()
            .map(|post| cursors.seal(post.seq, CursorDirection::Backward))
            .transpose()?
            .or_else(|| supplied.map(str::to_owned));
        let post_ids: Vec<String> = posts.iter().map(|p| p.id.clone()).collect();
        let meta_map = self.store.human_room_post_meta_for(&post_ids)?;
        let rows = posts
            .iter()
            .map(|post| cursors.post(post, scope.cutoff, meta_map.get(&post.id)))
            .collect::<Result<Vec<_>>>()?;
        let can_ask = self.ask_authority(token, peer_pid, &current);
        let can_manage = room_kind != crate::store::human_threads::KIND_DM
            && (creator == current.actor.id || current.self_role == "owner");
        let (agent_chain_count, agent_chain_limit) =
            self.store.human_room_chain_state(&current.actor, room)?;
        let events = self.store.human_room_events_for(&current.actor, room)?;
        let event_rows: Vec<Value> = events
            .into_iter()
            .map(|ev| json!({"id": ev.id, "text": ev.text, "created": ev.created}))
            .collect();
        self.reprove_human_room(token, params, peer_pid, room, &current.actor, &scope)?;
        let mut page = json!({
            "room": room,
            "scope_id": cursors.scope_id()?,
            "members": members.iter().map(member_json).collect::<Vec<_>>(),
            "posts": rows,
            "can_ask": can_ask,
            "can_manage": can_manage,
            "agent_chain_count": agent_chain_count,
            "agent_chain_limit": agent_chain_limit,
            "events": event_rows,
            "before": before,
            "more_before": more_before,
        });
        page[READER_SCOPE_FIELD] = json!(HUMAN_ROOM_SCOPE_ACK);
        #[cfg(test)]
        inject_page_fault(self, &mut page);
        Ok(page)
    }

    /// `human_room_post {token, room, client_id, body, silent?}` — posts to the
    /// channel transcript, resolves `@mentions` fail-closed against active
    /// channel membership, and dispatches queued agent turns under caller
    /// authority without waking any human teammate's personal assistant.
    pub(super) fn rpc_human_room_post(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        let token = required_str(params, "token")?;
        let current = self.human_current_actor(token, params, peer_pid)?;
        require_cursor_format(params)?;
        let room = room_param(params)?;
        let scope = self.store.human_room_cursor_scope(&current.actor, room)?;
        let cursors = HumanRoomCursors::new(self, &current.actor, room, &scope)?;
        let client_id = client_id_param(params)?;
        let body = required_str(params, "body")?;
        let explicit_silent = match params.get("silent") {
            None | Some(Value::Null) => false,
            Some(Value::Bool(v)) => *v,
            Some(_) => {
                return Err(Error::invalid(
                    "invalid_request",
                    "silent must be a boolean",
                ))
            }
        };
        let (room_kind, _) = self.store.human_room_meta(&current.actor, room)?;
        let (post, created) = self.store.human_room_post_with_silent(
            &current.actor,
            room,
            client_id,
            body,
            explicit_silent,
        )?;

        let existing_meta = self.store.human_room_post_meta(&post.id)?;
        let expected_silent = explicit_silent || crate::store::mentions::starts_with_note(body);
        let meta = if created || existing_meta.is_none() {
            let use_owner_authority = self.ask_authority(token, peer_pid, &current);
            let authority_mode = if use_owner_authority {
                "owner"
            } else {
                "member"
            };
            let source_tag = if use_owner_authority {
                "operator"
            } else {
                "member"
            };
            let resolved =
                self.resolve_and_dispatch_channel_mentions(ChannelMentionDispatchArgs {
                    org: &current.actor.org,
                    room,
                    room_kind: &room_kind,
                    topic: &scope.topic,
                    post_id: &post.id,
                    client_id,
                    body,
                    explicit_silent,
                    mode: ChannelMentionMode::Human {
                        sender_human_id: &current.actor.id,
                        use_owner_authority,
                        authority_mode,
                        source_tag,
                        roster: &current.roster,
                        issuer: &current.scope.issuer,
                        org_id: &current.scope.organization_id,
                    },
                })?;
            resolved.meta
        } else {
            let stored = existing_meta.unwrap_or_default();
            if stored.silent != expected_silent {
                return Err(Error::rejected(
                    "Post id was already used with different content",
                ));
            }
            stored
        };

        let post = cursors.post(&post, scope.cutoff, Some(&meta))?;
        self.reprove_human_room(token, params, peer_pid, room, &current.actor, &scope)?;
        let mut result = json!({
            "post": post,
            "created": created,
        });
        result[READER_SCOPE_FIELD] = json!(HUMAN_ROOM_SCOPE_ACK);
        #[cfg(test)]
        inject_page_fault(self, &mut result);
        Ok(result)
    }

    /// `human_notifications {token}` — all the actor's inbox groups, the
    /// notification preferences, and push availability. The verified current
    /// human rides in the same response so identity and feed change together.
    /// Push availability comes from the platform broker; no key lives here.
    pub(super) fn rpc_human_notifications(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        let current = self.human_current_actor(required_str(params, "token")?, params, peer_pid)?;
        let groups = self.store.notification_groups(&current.actor)?;
        let prefs = self.store.notification_prefs(&current.actor)?;
        // The platform owns the VAPID key. Push is available only when the
        // platform broker reports a configured public key; any failure to
        // read that answer is unavailable, never an error for the inbox.
        // Only a native membership reads the platform's push config. Any other
        // mode is unavailable without a network call.
        let push = match &self.membership_mode {
            crate::daemon::public_membership::MembershipMode::Native(_) => {
                match crate::notify_host::push_config() {
                    Ok(cfg) => json!({"available": cfg.available, "public_key": cfg.public_key}),
                    Err(_) => json!({"available": false, "public_key": null}),
                }
            }
            _ => json!({"available": false, "public_key": null}),
        };
        let unread_total: i64 = groups.iter().map(|g| g.unread).sum();
        Ok(json!({
            "human": {"id": current.actor.id, "name": current.actor.name},
            "unread_total": unread_total,
            "groups": groups.iter().map(group_json).collect::<Vec<_>>(),
            "preferences": prefs_json(&prefs),
            "push": push,
        }))
    }

    /// `human_notifications_read {token}` — dismiss the inbox. Never a thread
    /// read: transcript unread state is untouched.
    pub(super) fn rpc_human_notifications_read(
        &self,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        let current = self.human_current_actor(required_str(params, "token")?, params, peer_pid)?;
        self.store.notification_mark_all_read(&current.actor)?;
        let groups = self.store.notification_groups(&current.actor)?;
        let unread_total: i64 = groups.iter().map(|g| g.unread).sum();
        Ok(json!({"unread_total": unread_total}))
    }

    /// `human_notification_prefs_set {token, dm, mention, sound, preview, quiet}`
    pub(super) fn rpc_human_notification_prefs_set(
        &self,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        let current = self.human_current_actor(required_str(params, "token")?, params, peer_pid)?;
        let flag = |field: &str| -> Result<bool> {
            params.get(field).and_then(Value::as_bool).ok_or_else(|| {
                Error::invalid("invalid_request", format!("{field} must be a boolean"))
            })
        };
        // The quiet preset and the zone are validated at the boundary: an
        // unknown preset or an unparseable IANA zone is refused, never stored.
        let quiet = params
            .get("quiet")
            .and_then(Value::as_str)
            .and_then(crate::store::human_threads::QuietHours::parse)
            .ok_or_else(|| Error::invalid("invalid_request", "quiet must be off or 22-08"))?;
        let zone = params
            .get("zone")
            .and_then(Value::as_str)
            .filter(|z| z.parse::<chrono_tz::Tz>().is_ok())
            .ok_or_else(|| {
                Error::invalid("invalid_request", "zone must be a valid IANA time zone")
            })?
            .to_string();
        let prefs = crate::store::human_threads::NotificationPrefs {
            dm: flag("dm")?,
            mention: flag("mention")?,
            sound: flag("sound")?,
            preview: flag("preview")?,
            quiet,
            zone,
        };
        self.store.notification_prefs_set(&current.actor, &prefs)?;
        Ok(json!({"preferences": prefs_json(&prefs)}))
    }

    /// `human_room_mute {token, room, muted}` — current membership required.
    pub(super) fn rpc_human_room_mute(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        let current = self.human_current_actor(required_str(params, "token")?, params, peer_pid)?;
        let room = room_param(params)?;
        let muted = params
            .get("muted")
            .and_then(Value::as_bool)
            .ok_or_else(|| Error::invalid("invalid_request", "muted must be a boolean"))?;
        self.store
            .human_room_set_mute(&current.actor, room, muted)?;
        Ok(json!({"muted": muted}))
    }

    /// `human_push_subscribe {token, endpoint, p256dh, auth}`. Validates the
    /// provider endpoint and the browser keys before any write. Returns the
    /// server-minted device id.
    pub(super) fn rpc_human_push_subscribe(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        let current = self.human_current_actor(required_str(params, "token")?, params, peer_pid)?;
        let endpoint = required_str(params, "endpoint")?;
        let p256dh = required_str(params, "p256dh")?;
        let auth = required_str(params, "auth")?;
        crate::web_push::check_endpoint(endpoint)?;
        crate::web_push::subscription_keys(p256dh, auth)?;
        // Bind the device to this session by digest only. The raw cookie is
        // never stored; the dispatcher later asks the fresh-proof seam by it.
        let session = crate::operator_auth::digest(required_str(params, "token")?);
        let device =
            self.store
                .push_device_enroll(&current.actor, endpoint, p256dh, auth, &session)?;
        Ok(json!({"device_id": device}))
    }

    /// `human_push_rebind {token, endpoint}` — rebind only an existing,
    /// unexpired consent for this exact verified human and browser endpoint.
    pub(super) fn rpc_human_push_rebind(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        let token = required_str(params, "token")?;
        let current = self.human_current_actor(token, params, peer_pid)?;
        let endpoint = required_str(params, "endpoint")?;
        crate::web_push::check_endpoint(endpoint)?;
        let session = crate::operator_auth::digest(token);
        let device = self
            .store
            .push_device_rebind(&current.actor, endpoint, &session)?;
        Ok(json!({"device_id": device}))
    }

    /// `human_push_unsubscribe {token, device_id}` — returns `removed`. Another
    /// human's device reports `removed: false` and nothing changes.
    pub(super) fn rpc_human_push_unsubscribe(
        &self,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        let current = self.human_current_actor(required_str(params, "token")?, params, peer_pid)?;
        let device = required_str(params, "device_id")?;
        let removed = self.store.push_device_revoke(&current.actor, device)?;
        Ok(json!({"removed": removed}))
    }

    /// `human_room_mark_read {token, room, cursor_format, cursor}`
    pub(super) fn rpc_human_room_mark_read(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        let current = self.human_current_actor(required_str(params, "token")?, params, peer_pid)?;
        require_cursor_format(params)?;
        if params.get("seq").is_some() {
            return Err(Error::invalid(
                "human_room_cursor_numeric",
                "human room requires an opaque cursor",
            ));
        }
        let room = room_param(params)?;
        let token = cursor_param(params, "cursor")?.ok_or_else(|| {
            Error::invalid("human_room_cursor_invalid", "human room cursor is required")
        })?;
        let scope = self.store.human_room_cursor_scope(&current.actor, room)?;
        let cursors = HumanRoomCursors::new(self, &current.actor, room, &scope)?;
        let seq = cursors.open(token, CursorDirection::Forward)?;
        let unread = self
            .store
            .human_room_mark_read(&current.actor, room, seq, &scope)?;
        let mut result = json!({"unread": unread});
        result[READER_SCOPE_FIELD] = json!(HUMAN_ROOM_SCOPE_ACK);
        Ok(result)
    }

    /// `human_room_add {token, room, human_subject | agent_uid}` — channel
    /// creator or workspace owner. A human must be a fresh current member.
    pub(super) fn rpc_human_room_add(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        let current = self.human_current_actor(required_str(params, "token")?, params, peer_pid)?;
        let is_owner = current.self_role == "owner";
        let room = room_param(params)?;
        match (
            optional_str(params, "human_subject"),
            optional_str(params, "agent_uid"),
        ) {
            (Some(_), Some(_)) => {
                return Err(Error::invalid(
                    "invalid_request",
                    "name either human_subject or agent_uid, not both",
                ));
            }
            (Some(subject), None) => {
                let peer = roster_peer(&current, subject)?;
                self.store
                    .human_group_add_human(&current.actor, is_owner, room, &peer.id)?;
            }
            (None, Some(uid)) => {
                let alias = self
                    .store
                    .alias_of_uid(uid)?
                    .ok_or_else(|| Error::rejected("Agent not found"))?;
                self.store
                    .human_group_add_agent(&current.actor, is_owner, room, &alias)?;
            }
            (None, None) => {
                return Err(Error::invalid(
                    "invalid_request",
                    "name a human_subject or an agent_uid",
                ));
            }
        }
        Ok(json!({"ok": true}))
    }

    pub(super) fn dispatch_channel_agent_mention(
        self: &Arc<Self>,
        args: ChannelAgentDispatch<'_>,
    ) -> Result<String> {
        let req_id = crate::store::topic_request_id(
            args.topic,
            &format!("{}:{}", args.client_id, args.agent_uid),
        );
        let binding = if *args.primary_unbound {
            *args.primary_unbound = false;
            Some(super::messages_rpc::SendBinding::TopicPost(args.post_id))
        } else {
            None
        };
        let send = json!({
            "alias": args.agent_alias,
            "text": args.queued_text,
            "message": req_id,
            "source": args.source_tag
        });
        let sender = args.sender;
        self.send_with_human_expected(
            &send,
            &|_| Ok(sender.clone()),
            &|verb| {
                Err(Error::rejected(format!(
                    "{verb} refused: this send carries no steering authority"
                )))
            },
            None,
            Some(args.agent_uid),
            binding,
        )?;
        self.store.human_room_bind_post_request(
            args.post_id,
            args.agent_uid,
            &req_id,
            args.origin_actor,
            args.authority_mode,
        )?;
        Ok(req_id)
    }

    pub(super) fn resolve_and_dispatch_channel_mentions(
        self: &Arc<Self>,
        args: ChannelMentionDispatchArgs<'_>,
    ) -> Result<ChannelMentionResolution> {
        let parsed = crate::store::mentions::parse_mentions(args.body, args.explicit_silent);
        let active_agents = self.store.human_room_active_agents(args.org, args.room)?;
        let active_humans = self.store.human_room_active_humans(args.org, args.room)?;
        let active_human_ids: std::collections::HashSet<&str> =
            active_humans.iter().map(|(id, _)| id.as_str()).collect();
        let shareable_agents = self.store.human_shareable_agents()?;
        let mut primary_unbound = self
            .store
            .human_room_post_has_no_primary_request(args.post_id)?;
        let mut outcomes = Vec::new();
        let mut deliveries = Vec::new();
        let mut queued_hops: i64 = 0;

        let roster_member_id = |issuer: &str, org_id: &str, subject_id: &str| {
            VerifiedHuman::verified(issuer, org_id, subject_id)
                .ok()
                .map(|v| v.id)
        };
        let roster_matches_handle = |m: &crate::daemon::human_current::RosterMember, h: &str| {
            m.name.trim().eq_ignore_ascii_case(h)
                || m.subject_id.trim().eq_ignore_ascii_case(h)
                || crate::store::mentions::human_handle_matches(&m.name, h)
                || crate::store::mentions::human_handle_matches(&m.subject_id, h)
        };
        let principal_matches_handle = |hid: &str, name: &str, h: &str| {
            hid.eq_ignore_ascii_case(h)
                || (!name.is_empty()
                    && (name.trim().eq_ignore_ascii_case(h)
                        || crate::store::mentions::human_handle_matches(name, h)))
        };

        for handle in &parsed.handles {
            if let Some((uid, alias)) = active_agents
                .iter()
                .find(|(_, a)| a.eq_ignore_ascii_case(handle))
            {
                match &args.mode {
                    ChannelMentionMode::AgentHop {
                        sender_agent_uid,
                        sender_alias,
                        hop_member_origin,
                        chain_count,
                        chain_limit,
                        ..
                    } => {
                        if uid == sender_agent_uid {
                            continue;
                        }
                        if parsed.silent {
                            outcomes.push(crate::store::mentions::MentionOutcome {
                                kind: "silent".to_string(),
                                handle: format!("@{alias}"),
                                label: alias.clone(),
                                reason: Some("Silent note (/note)".to_string()),
                            });
                            continue;
                        }
                        if *chain_count + queued_hops >= *chain_limit {
                            outcomes.push(crate::store::mentions::MentionOutcome {
                                kind: "blocked_chain".to_string(),
                                handle: format!("@{alias}"),
                                label: alias.clone(),
                                reason: Some(
                                    "Human-Turn Brake active (waiting_for_person)".to_string(),
                                ),
                            });
                            continue;
                        }
                        let queued_text = format!(
                            "[{ROOM_TOPIC_TYPE}] @{sender_alias} mentioned you in a channel. \
                             Read it: `cadence topic read {topic}`. Reply in the same channel: \
                             `cadence topic publish {topic} \"<your reply>\" --id <unique-id>`. \
                             Source post: {post_id}.",
                            topic = args.topic,
                            post_id = args.post_id,
                        );
                        let (source_tag, origin_actor, authority_mode, sender) =
                            match hop_member_origin {
                                Some(member_origin) => (
                                    "member",
                                    *member_origin,
                                    "member",
                                    crate::store::Sender::Member((*member_origin).to_string()),
                                ),
                                None => (
                                    "agent",
                                    *sender_alias,
                                    "agent_hop",
                                    crate::store::Sender::Agent((*sender_alias).to_string()),
                                ),
                            };
                        let req_id = self.dispatch_channel_agent_mention(ChannelAgentDispatch {
                            topic: args.topic,
                            post_id: args.post_id,
                            client_id: args.client_id,
                            agent_uid: uid,
                            agent_alias: alias,
                            queued_text: &queued_text,
                            source_tag,
                            origin_actor,
                            authority_mode,
                            sender,
                            primary_unbound: &mut primary_unbound,
                        })?;
                        queued_hops += 1;
                        outcomes.push(crate::store::mentions::MentionOutcome {
                            kind: "queued".to_string(),
                            handle: format!("@{alias}"),
                            label: alias.clone(),
                            reason: Some(format!(
                                "Queued agent hop ({}/{})",
                                *chain_count + queued_hops,
                                chain_limit
                            )),
                        });
                        deliveries.push(json!({
                            "uid": uid,
                            "alias": alias,
                            "message": req_id
                        }));
                    }
                    ChannelMentionMode::Human {
                        sender_human_id,
                        use_owner_authority,
                        authority_mode,
                        source_tag,
                        ..
                    } => {
                        if parsed.silent {
                            outcomes.push(crate::store::mentions::MentionOutcome {
                                kind: "silent".to_string(),
                                handle: format!("@{alias}"),
                                label: alias.clone(),
                                reason: Some("Silent note (/note)".to_string()),
                            });
                            continue;
                        }
                        let queued_text = format!(
                            "[{ROOM_TOPIC_TYPE}] Teammate @mentioned you in a channel (origin: {origin}). \
                             Read it: `cadence topic read {topic}`. Reply in the same channel: \
                             `cadence topic publish {topic} \"<your reply>\" --id <unique-id>`. \
                             Source post: {post_id}.",
                            origin = sender_human_id,
                            topic = args.topic,
                            post_id = args.post_id,
                        );
                        let req_id = self.dispatch_channel_agent_mention(ChannelAgentDispatch {
                            topic: args.topic,
                            post_id: args.post_id,
                            client_id: args.client_id,
                            agent_uid: uid,
                            agent_alias: alias,
                            queued_text: &queued_text,
                            source_tag,
                            origin_actor: sender_human_id,
                            authority_mode,
                            sender: if *use_owner_authority {
                                crate::store::Sender::OperatorChat
                            } else {
                                crate::store::Sender::Member((*sender_human_id).to_string())
                            },
                            primary_unbound: &mut primary_unbound,
                        })?;
                        outcomes.push(crate::store::mentions::MentionOutcome {
                            kind: "queued".to_string(),
                            handle: format!("@{alias}"),
                            label: alias.clone(),
                            reason: Some(
                                if *use_owner_authority {
                                    "Queued (owner authority)"
                                } else {
                                    "Queued (member authority)"
                                }
                                .to_string(),
                            ),
                        });
                        deliveries.push(json!({
                            "uid": uid,
                            "alias": alias,
                            "message": req_id
                        }));
                    }
                }
                continue;
            }

            let sender_human = match &args.mode {
                ChannelMentionMode::Human {
                    sender_human_id, ..
                } => Some(*sender_human_id),
                ChannelMentionMode::AgentHop { .. } => None,
            };
            let mut seen_humans = std::collections::HashSet::new();
            let mut matched_humans = Vec::new();
            let mut matched_self = false;
            for (hid, name) in &active_humans {
                if principal_matches_handle(hid, name, handle) {
                    if sender_human == Some(hid.as_str()) {
                        matched_self = true;
                        continue;
                    }
                    if seen_humans.insert(hid.clone()) {
                        let label = if name.trim().is_empty() {
                            hid.clone()
                        } else {
                            name.clone()
                        };
                        matched_humans.push(label);
                    }
                }
            }
            if let ChannelMentionMode::Human {
                roster,
                issuer,
                org_id,
                ..
            } = &args.mode
            {
                for m in &roster.members {
                    if let Some(hid) = roster_member_id(issuer, org_id, &m.subject_id) {
                        if active_human_ids.contains(hid.as_str())
                            && roster_matches_handle(m, handle)
                        {
                            if sender_human == Some(hid.as_str()) {
                                matched_self = true;
                                continue;
                            }
                            if seen_humans.insert(hid) {
                                let label = if m.name.trim().is_empty() {
                                    m.subject_id.clone()
                                } else {
                                    m.name.clone()
                                };
                                matched_humans.push(label);
                            }
                        }
                    }
                }
            }

            if !matched_humans.is_empty() {
                for human_label in matched_humans {
                    outcomes.push(crate::store::mentions::MentionOutcome {
                        kind: if parsed.silent { "silent" } else { "notified" }.to_string(),
                        handle: format!("@{handle}"),
                        label: human_label,
                        reason: Some(if parsed.silent {
                            "Silent note (/note)".to_string()
                        } else {
                            "Notified (bell inbox · assistant stays isolated)".to_string()
                        }),
                    });
                }
            } else if matched_self {
                continue;
            } else if let Some((_, alias)) = shareable_agents
                .iter()
                .find(|(_, a)| a.eq_ignore_ascii_case(handle))
            {
                if args.room_kind == crate::store::human_threads::KIND_DM {
                    outcomes.push(crate::store::mentions::MentionOutcome {
                        kind: "blocked_dm".to_string(),
                        handle: format!("@{alias}"),
                        label: alias.clone(),
                        reason: Some("Shared agents are excluded from 1:1 DMs".to_string()),
                    });
                } else {
                    outcomes.push(crate::store::mentions::MentionOutcome {
                        kind: "blocked_acl".to_string(),
                        handle: format!("@{alias}"),
                        label: alias.clone(),
                        reason: Some("Not added to this channel".to_string()),
                    });
                }
            } else {
                let non_member_label = match &args.mode {
                    ChannelMentionMode::Human {
                        roster,
                        issuer,
                        org_id,
                        ..
                    } => roster
                        .members
                        .iter()
                        .find(|m| {
                            let is_active = roster_member_id(issuer, org_id, &m.subject_id)
                                .is_some_and(|hid| active_human_ids.contains(hid.as_str()));
                            !is_active && roster_matches_handle(m, handle)
                        })
                        .map(|m| m.name.clone()),
                    ChannelMentionMode::AgentHop { org_principals, .. } => org_principals
                        .iter()
                        .find(|(hid, name)| {
                            !active_human_ids.contains(hid.as_str())
                                && principal_matches_handle(hid, name, handle)
                        })
                        .map(|(hid, name)| {
                            if name.trim().is_empty() {
                                hid.clone()
                            } else {
                                name.clone()
                            }
                        }),
                };
                if let Some(label) = non_member_label {
                    outcomes.push(crate::store::mentions::MentionOutcome {
                        kind: "blocked_acl".to_string(),
                        handle: format!("@{handle}"),
                        label,
                        reason: Some("Not a member of this channel".to_string()),
                    });
                }
            }
        }

        self.store
            .human_room_save_post_meta(args.post_id, parsed.silent, &outcomes)?;
        Ok(ChannelMentionResolution {
            meta: crate::store::human_threads::HumanPostMeta {
                silent: parsed.silent,
                outcomes,
            },
            deliveries,
        })
    }

    /// `issue_comment_mentions {issue_id, comment_key, body, token | local_operator}` —
    /// dispatch `@mentions` from a committed issue comment using verified board caller authority
    /// without waking any teammate's personal assistant.
    pub(super) fn rpc_issue_comment_mentions(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        self.operator_connection("issue_comment_mentions", params, peer_pid)?;
        strict_fields(
            params,
            &["issue_id", "comment_key", "body", "token", "local_operator"],
        )?;
        let issue_id_raw = required_str(params, "issue_id")?;
        let issue_id = crate::issue::model::check_id(issue_id_raw)?;
        let comment_key = bounded_ref(params, "comment_key")?;
        let body = required_str(params, "body")?;

        let token = optional_str(params, "token");
        let local_operator = match params.get("local_operator") {
            None | Some(Value::Null) => false,
            Some(Value::Bool(v)) => *v,
            Some(_) => {
                return Err(Error::invalid(
                    "invalid_request",
                    "local_operator must be a boolean",
                ))
            }
        };
        if token.is_some() && local_operator {
            return Err(Error::invalid(
                "invalid_request",
                "specify either token or local_operator, not both",
            ));
        }

        enum CommentHumans {
            Roster {
                members: Vec<crate::daemon::human_current::RosterMember>,
                issuer: String,
                org_id: String,
            },
            Principals(Vec<(String, String)>),
        }

        let (use_owner_authority, origin_actor, org, humans) = if let Some(token) = token {
            let current = self.human_current_actor(token, params, peer_pid)?;
            let use_owner = self.ask_authority(token, peer_pid, &current);
            (
                use_owner,
                current.actor.id,
                current.actor.org,
                CommentHumans::Roster {
                    members: current.roster.members,
                    issuer: current.scope.issuer,
                    org_id: current.scope.organization_id,
                },
            )
        } else if local_operator {
            self.operator_chat("issue_comment_mentions", peer_pid)?;
            let org = crate::board_identity::read_config(&self.state_dir)
                .ok()
                .map(|c| c.company)
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "local".to_string());
            let principals = self.store.org_human_principals(&org)?;
            (
                true,
                "operator".to_string(),
                org,
                CommentHumans::Principals(principals),
            )
        } else {
            return Err(Error::rejected(
                "issue_comment_mentions requires a verified board session",
            ));
        };

        let authority_mode = if use_owner_authority {
            "owner"
        } else {
            "member"
        };
        let source_tag = if use_owner_authority {
            "operator"
        } else {
            "member"
        };
        let parsed = crate::store::mentions::parse_mentions(body, false);
        let shareable_agents = self.store.human_shareable_agents()?;
        let post_key = format!("issue:{issue_id}:{comment_key}");
        let topic_tag = format!("issue:{issue_id}");
        let mut outcomes = Vec::new();
        let mut primary_unbound = false;

        for handle in &parsed.handles {
            if let Some((uid, alias)) = shareable_agents
                .iter()
                .find(|(_, a)| a.eq_ignore_ascii_case(handle))
            {
                if parsed.silent {
                    outcomes.push(crate::store::mentions::MentionOutcome {
                        kind: "silent".to_string(),
                        handle: format!("@{alias}"),
                        label: alias.clone(),
                        reason: Some("Silent note (/note)".to_string()),
                    });
                } else {
                    let queued_text = format!(
                        "[issue_comment] Teammate @mentioned you on issue {issue_id} \
                         (comment: {comment_key}, origin: {origin_actor}). Read `{issue_id}` with \
                         `cadence show {issue_id}` and reply on the ticket if needed."
                    );
                    self.dispatch_channel_agent_mention(ChannelAgentDispatch {
                        topic: &topic_tag,
                        post_id: &post_key,
                        client_id: comment_key,
                        agent_uid: uid,
                        agent_alias: alias,
                        queued_text: &queued_text,
                        source_tag,
                        origin_actor: &origin_actor,
                        authority_mode,
                        sender: if use_owner_authority {
                            crate::store::Sender::OperatorChat
                        } else {
                            crate::store::Sender::Member(origin_actor.clone())
                        },
                        primary_unbound: &mut primary_unbound,
                    })?;
                    outcomes.push(crate::store::mentions::MentionOutcome {
                        kind: "queued".to_string(),
                        handle: format!("@{alias}"),
                        label: alias.clone(),
                        reason: Some(
                            if use_owner_authority {
                                "Queued (owner authority)"
                            } else {
                                "Queued (member authority)"
                            }
                            .to_string(),
                        ),
                    });
                }
                continue;
            }

            let matched_label = match &humans {
                CommentHumans::Roster {
                    members,
                    issuer,
                    org_id,
                } => members.iter().find_map(|m| {
                    let matches = m.name.trim().eq_ignore_ascii_case(handle)
                        || m.subject_id.trim().eq_ignore_ascii_case(handle)
                        || crate::store::mentions::human_handle_matches(&m.name, handle)
                        || crate::store::mentions::human_handle_matches(&m.subject_id, handle);
                    if !matches {
                        return None;
                    }
                    let hid = VerifiedHuman::verified(issuer, org_id, &m.subject_id)
                        .ok()
                        .map(|v| v.id);
                    if hid.as_deref() == Some(origin_actor.as_str()) {
                        return None;
                    }
                    Some(if m.name.trim().is_empty() {
                        m.subject_id.clone()
                    } else {
                        m.name.clone()
                    })
                }),
                CommentHumans::Principals(principals) => {
                    principals.iter().find_map(|(hid, name)| {
                        if hid == &origin_actor {
                            return None;
                        }
                        let matches = hid.eq_ignore_ascii_case(handle)
                            || (!name.is_empty()
                                && (name.trim().eq_ignore_ascii_case(handle)
                                    || crate::store::mentions::human_handle_matches(name, handle)));
                        matches.then(|| {
                            if name.trim().is_empty() {
                                hid.clone()
                            } else {
                                name.clone()
                            }
                        })
                    })
                }
            };
            if let Some(label) = matched_label {
                outcomes.push(crate::store::mentions::MentionOutcome {
                    kind: if parsed.silent { "silent" } else { "notified" }.to_string(),
                    handle: format!("@{handle}"),
                    label,
                    reason: Some(if parsed.silent {
                        "Silent note (/note)".to_string()
                    } else {
                        "Notified on ticket · personal assistant stays isolated".to_string()
                    }),
                });
            }
        }

        if !outcomes.is_empty() {
            let _ = self.store.event_public(
                &org,
                "issue_comment_mentions",
                json!({
                    "issue": issue_id,
                    "comment": comment_key,
                    "origin_actor": origin_actor,
                    "silent": parsed.silent,
                    "outcomes": outcomes,
                }),
            );
        }

        Ok(json!({
            "silent": parsed.silent,
            "outcomes": outcomes,
        }))
    }

    /// `human_room_remove {token, room, kind, member}` — channel creator or
    /// workspace owner.
    pub(super) fn rpc_human_room_remove(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        let current = self.human_current_actor(required_str(params, "token")?, params, peer_pid)?;
        let is_owner = current.self_role == "owner";
        let room = room_param(params)?;
        let kind = required_str(params, "kind")?;
        let member = required_str(params, "member")?;
        self.store
            .human_group_remove(&current.actor, is_owner, room, kind, member)?;
        Ok(json!({"ok": true}))
    }
}

pub(super) struct ChannelAgentDispatch<'a> {
    pub topic: &'a str,
    pub post_id: &'a str,
    pub client_id: &'a str,
    pub agent_uid: &'a str,
    pub agent_alias: &'a str,
    pub queued_text: &'a str,
    pub source_tag: &'a str,
    pub origin_actor: &'a str,
    pub authority_mode: &'a str,
    pub sender: crate::store::Sender,
    pub primary_unbound: &'a mut bool,
}

pub(super) enum ChannelMentionMode<'a> {
    Human {
        sender_human_id: &'a str,
        use_owner_authority: bool,
        authority_mode: &'a str,
        source_tag: &'a str,
        roster: &'a crate::daemon::human_current::Roster,
        issuer: &'a str,
        org_id: &'a str,
    },
    AgentHop {
        sender_agent_uid: &'a str,
        sender_alias: &'a str,
        hop_member_origin: Option<&'a str>,
        chain_count: i64,
        chain_limit: i64,
        org_principals: &'a [(String, String)],
    },
}

pub(super) struct ChannelMentionDispatchArgs<'a> {
    pub org: &'a str,
    pub room: &'a str,
    pub room_kind: &'a str,
    pub topic: &'a str,
    pub post_id: &'a str,
    pub client_id: &'a str,
    pub body: &'a str,
    pub explicit_silent: bool,
    pub mode: ChannelMentionMode<'a>,
}

pub(super) struct ChannelMentionResolution {
    pub meta: crate::store::human_threads::HumanPostMeta,
    pub deliveries: Vec<Value>,
}

/// Refuse any field the delivery RPC does not name. A caller-chosen endpoint,
/// recipient or key is never authority.
fn strict_fields(params: &Value, allowed: &[&str]) -> Result<()> {
    let obj = params.as_object().ok_or_else(|| {
        Error::invalid("invalid_request", "the delivery request must be an object")
    })?;
    if obj.keys().all(|k| allowed.contains(&k.as_str())) {
        Ok(())
    } else {
        Err(Error::invalid(
            "invalid_request",
            "a delivery request field is not allowed",
        ))
    }
}

/// A bounded opaque reference (device id or lease id).
fn bounded_ref<'a>(params: &'a Value, field: &str) -> Result<&'a str> {
    let v = required_str(params, field)
        .map_err(|_| Error::invalid("invalid_request", format!("{field} is not valid")))?;
    if v.is_empty() || v.len() > 128 {
        return Err(Error::invalid(
            "invalid_request",
            format!("{field} is not valid"),
        ));
    }
    Ok(v)
}

/// The intent reference: a positive safe integer.
fn intent_ref(params: &Value) -> Result<i64> {
    params
        .get("intent_id")
        .and_then(Value::as_i64)
        .filter(|n| (1..=9_007_199_254_740_991).contains(n))
        .ok_or_else(|| {
            Error::invalid(
                "invalid_request",
                "intent_id must be a positive safe integer",
            )
        })
}

/// The platform-service proof for one delivery RPC. The trusted company and
/// instance come from native membership config only; the method and path are
/// fixed by the handler. Verified before any business check or store access.
/// Returns the verified claims; the prepare lease is the proof's `jti`.
fn verify_service_proof(
    shared: &Shared,
    params: &Value,
    path: &str,
    intent_id: i64,
    device_id: &str,
    lease_id: Option<&str>,
    outcome: Option<&str>,
) -> Result<crate::notify_bridge::Claims> {
    let crate::daemon::public_membership::MembershipMode::Native(native) = &shared.membership_mode
    else {
        return Err(Error::invalid(
            "capability_unavailable",
            "notification delivery is unavailable on this daemon",
        ));
    };
    let proof = required_str(params, "proof")?;
    let now = crate::notify_bridge::now_unix()?;
    let expected = crate::notify_bridge::Expected {
        company: &native.company,
        instance: &native.instance,
        method: "POST",
        path,
        intent_id,
        device_id,
        lease_id,
        outcome,
    };
    crate::notify_bridge::verify(proof, &expected, &shared.board_jwks, now)
}

impl Shared {
    /// `human_notification_delivery_prepare {proof, intent_id, device_id}`.
    /// Platform-only. The proof is verified first. Eligibility and the device's
    /// own presence are checked before the claim, so an ineligible intent never
    /// holds a lease. A live lease is `lease_active`. An ineligible, missing or
    /// already-delivered device is `suppressed` with no private data.
    pub(super) fn rpc_human_notification_delivery_prepare(
        &self,
        params: &Value,
        _peer_pid: u32,
    ) -> Result<Value> {
        strict_fields(params, &["proof", "intent_id", "device_id"])?;
        let intent_id = intent_ref(params)?;
        let device_id = bounded_ref(params, "device_id")?;
        let claims = verify_service_proof(
            self,
            params,
            crate::notify_bridge::PREPARE_PATH,
            intent_id,
            device_id,
            None,
            None,
        )?;
        let suppressed = || json!({"state": "suppressed"});
        let Some(intent) = self.store.push_intent_for_prepare(intent_id)? else {
            return Ok(suppressed());
        };
        if self.store.push_device_owner(device_id)?.as_deref() != Some(intent.recipient.as_str()) {
            return Ok(suppressed());
        }
        // The proof's audience is the verified company (checked against native
        // config). A suppressed device gets a terminal row so the initiator
        // stops re-selecting it and no later burst can send.
        let settle_suppressed = || -> Result<Value> {
            self.store
                .push_suppress_device(intent_id, device_id, &claims.aud)?;
            Ok(suppressed())
        };
        if !self.store.push_recipient_eligible(&intent)? {
            return settle_suppressed();
        }
        if self
            .store
            .push_device_visible(device_id, &intent.recipient, &intent.room)?
        {
            return settle_suppressed();
        }
        let prefs = self.store.notification_prefs_for(&intent.recipient)?;
        // Preview text is opt-in, bounded, and redacted by the existing cleaner.
        let preview = match prefs.preview {
            true => self
                .store
                .topic_post_by_id(&intent.post)?
                .map(|p| crate::store::conversations::body(&p.body))
                .map(|t| t.chars().take(140).collect::<String>()),
            false => None,
        };
        let payload = json!({
            "title": "New message",
            "tag": format!("room-{}", intent.room),
            "url": format!("/threads?h={}", intent.room),
            "silent": !prefs.sound,
            "body": preview,
        })
        .to_string();
        if payload.len() > 2048 {
            return Err(Error::invalid(
                "invalid_request",
                "the delivery payload is too large",
            ));
        }
        // The lease is the prepare proof's jti: one proof, one lease.
        let lease = claims.jti;
        let Some(device) = self.store.push_delivery_claim(
            intent_id,
            device_id,
            &lease,
            crate::store::human_threads::LEASE_SECS_F64,
        )?
        else {
            if self.store.push_delivery_lease_live(intent_id, device_id)? {
                return Err(Error::gate_coded(
                    "lease_active",
                    "this delivery already has a live lease",
                ));
            }
            return Ok(suppressed());
        };
        Ok(json!({
            "lease_id": lease,
            "issuer": device.issuer,
            "organization_id": device.org,
            "subject_id": device.subject,
            "endpoint": device.endpoint,
            "keys": {"p256dh": device.p256dh, "auth": device.auth},
            "payload": payload,
        }))
    }

    /// `human_notification_delivery_finish {proof, intent_id, device_id, lease_id, outcome}`.
    /// Platform-only. The proof binds every field. Only the live lease can
    /// finish; a stale or wrong lease is `stale_lease` and changes nothing.
    pub(super) fn rpc_human_notification_delivery_finish(
        &self,
        params: &Value,
        _peer_pid: u32,
    ) -> Result<Value> {
        strict_fields(
            params,
            &["proof", "intent_id", "device_id", "lease_id", "outcome"],
        )?;
        let intent_id = intent_ref(params)?;
        let device_id = bounded_ref(params, "device_id")?;
        let lease_id = bounded_ref(params, "lease_id")?;
        let outcome = required_str(params, "outcome")?;
        if !matches!(outcome, "accepted" | "gone" | "retry" | "suppressed") {
            return Err(Error::invalid(
                "invalid_request",
                "outcome is not recognized",
            ));
        }
        verify_service_proof(
            self,
            params,
            crate::notify_bridge::FINISH_PATH,
            intent_id,
            device_id,
            Some(lease_id),
            Some(outcome),
        )?;
        if !self
            .store
            .push_delivery_finish(intent_id, device_id, lease_id, outcome)?
        {
            return Err(Error::gate_coded(
                "stale_lease",
                "the delivery lease is no longer live",
            ));
        }
        Ok(json!({"ok": true}))
    }

    /// `human_push_presence {token, device_id, tab_id, room}`. Each owned
    /// device tab reports independently; presence expires in 30 seconds.
    pub(super) fn rpc_human_push_presence(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        let current = self.human_current_actor(required_str(params, "token")?, params, peer_pid)?;
        let device = bounded_ref(params, "device_id")?;
        let tab = bounded_ref(params, "tab_id")?;
        let room = match params.get("room") {
            None | Some(Value::Null) => None,
            Some(Value::String(r)) if !r.is_empty() && r.len() <= 96 => Some(r.as_str()),
            Some(_) => return Err(Error::invalid("invalid_request", "room is not valid")),
        };
        let accepted = self
            .store
            .push_presence_set(&current.actor, device, tab, room)?;
        Ok(json!({"accepted": accepted}))
    }
}
