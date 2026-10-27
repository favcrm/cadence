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

use super::{optional_i64, optional_str, required_str, Shared};
use crate::error::{Error, Result};
use crate::store::human_threads::{
    HumanRoomMember, HumanRoomSummary, RoomMember, VerifiedHuman, ROOM_TOPIC_TYPE,
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

/// A post as the room shows it: the body redacted by the existing helper
/// (CAD-818 R4), the sender as a durable id plus presentation label.
fn post_json(p: &TopicPost) -> Value {
    json!({
        "id": p.id,
        "seq": p.seq,
        "sender": {"id": p.sender_uid, "label": p.sender},
        "body": crate::store::conversations::body(&p.body),
        "created": p.created,
    })
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

    /// `human_agents_list {token}` — the current agents by durable uid. The
    /// read is identity-only: no grant, no roster beyond the guard's check.
    pub(super) fn rpc_human_agents_list(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        self.human_current_actor(required_str(params, "token")?, params, peer_pid)?;
        let agents: Vec<Value> = self
            .store
            .agents()?
            .into_iter()
            .map(|a| json!({"uid": a.uid, "alias": a.alias}))
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
        let current = self.human_current_actor(token, params, peer_pid)?;
        if !self.ask_authority(token, peer_pid, &current) {
            return Err(Error::rejected(
                "Asking an agent needs the operator role on this board",
            ));
        }
        let room = room_param(params)?;
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
        // The queue names the selected member's uid; the enqueue transaction
        // refuses if the alias now belongs to another uid (no reassignment race).
        let send =
            json!({"alias": alias, "text": queued, "message": request_id, "source": "operator"});
        let receipt = self.send_with_expected(
            &send,
            &|_| Ok(crate::store::Sender::OperatorChat),
            &|verb| {
                Err(Error::rejected(format!(
                    "{verb} refused: this send carries no caller to steer with"
                )))
            },
            Some(agent_uid),
        )?;
        let duplicate = receipt
            .get("duplicate")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        self.store.topic_post_bind_request(&post.id, &request_id)?;
        Ok(json!({
            "request": request_id,
            "post": post_json(&post),
            "target": {"uid": agent_uid, "alias": alias},
            "duplicate": duplicate,
        }))
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

    /// `human_room_read {token, room, before?, limit?}`
    pub(super) fn rpc_human_room_read(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        let current = self.human_current_actor(required_str(params, "token")?, params, peer_pid)?;
        let room = room_param(params)?;
        let limit = optional_i64(params, "limit").unwrap_or(50);
        if !(1..=HISTORY_MAX).contains(&limit) {
            return Err(Error::invalid(
                "invalid_request",
                format!("limit must be 1-{HISTORY_MAX}"),
            ));
        }
        let before = optional_i64(params, "before");
        let members = self.store.human_room_members(&current.actor, room)?;
        let posts = self
            .store
            .human_room_history(&current.actor, room, before, limit)?;
        let can_ask = self.ask_authority(required_str(params, "token")?, peer_pid, &current);
        Ok(json!({
            "room": room,
            "members": members.iter().map(member_json).collect::<Vec<_>>(),
            "posts": posts.iter().map(post_json).collect::<Vec<_>>(),
            "can_ask": can_ask,
        }))
    }

    /// `human_room_post {token, room, client_id, body}` — transcript only.
    pub(super) fn rpc_human_room_post(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        let current = self.human_current_actor(required_str(params, "token")?, params, peer_pid)?;
        let room = room_param(params)?;
        let client_id = client_id_param(params)?;
        let body = required_str(params, "body")?;
        let (post, created) = self
            .store
            .human_room_post(&current.actor, room, client_id, body)?;
        Ok(json!({"post": post_json(&post), "created": created}))
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

    /// `human_room_mark_read {token, room, seq}`
    pub(super) fn rpc_human_room_mark_read(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        let current = self.human_current_actor(required_str(params, "token")?, params, peer_pid)?;
        let room = room_param(params)?;
        let seq = optional_i64(params, "seq")
            .ok_or_else(|| Error::invalid("invalid_request", "seq must be an integer"))?;
        let unread = self.store.human_room_mark_read(&current.actor, room, seq)?;
        Ok(json!({"unread": unread}))
    }

    /// `human_room_add {token, room, human_subject | agent_uid}` — creator
    /// only (store). A human must be a fresh current member.
    pub(super) fn rpc_human_room_add(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        let current = self.human_current_actor(required_str(params, "token")?, params, peer_pid)?;
        let room = room_param(params)?;
        if let Some(subject) = optional_str(params, "human_subject") {
            let peer = roster_peer(&current, subject)?;
            self.store
                .human_group_add_human(&current.actor, room, &peer.id)?;
        } else if let Some(uid) = optional_str(params, "agent_uid") {
            let alias = self
                .store
                .alias_of_uid(uid)?
                .ok_or_else(|| Error::rejected("Agent not found"))?;
            self.store
                .human_group_add_agent(&current.actor, room, &alias)?;
        } else {
            return Err(Error::invalid(
                "invalid_request",
                "name a human_subject or an agent_uid",
            ));
        }
        Ok(json!({"ok": true}))
    }

    /// `human_room_remove {token, room, kind, member}` — creator only.
    pub(super) fn rpc_human_room_remove(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        let current = self.human_current_actor(required_str(params, "token")?, params, peer_pid)?;
        let room = room_param(params)?;
        let kind = required_str(params, "kind")?;
        let member = required_str(params, "member")?;
        self.store
            .human_group_remove(&current.actor, room, kind, member)?;
        Ok(json!({"ok": true}))
    }
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
