//! CAD-1412: human DMs and human/mixed groups over the existing Threads
//! transcript (`topics` / `topic_posts`).
//!
//! A human is identified by the verified `(issuer, organization, subject)`
//! tuple, never by a handle or an agent alias. Each room is one topic of
//! type `human_room`; membership, history cutoffs and read markers live in
//! the tables below. Room membership never starts an agent turn: posts here
//! write transcript rows only, with no queue intent and no provider effect.
//!
//! Every entrypoint takes a [`VerifiedHuman`] that the caller must have
//! built from a server-verified principal. Each one re-checks the actor's
//! organization and active membership inside the same write transaction.
//!
//! Schema v44 DDL lives in [`SCHEMA_V44`], applied by the v44 migration in
//! `store/schema.rs` (v41-v43 are the CAD-1380 assistant migrations).

use rusqlite::params;
use sha2::{Digest, Sha256};

use super::topics::{check_post_body, post_id, TopicPost};
use super::{now, Store, StoreConn};
use crate::error::{Error, Result};

/// Topic type every human room uses; the room id is the topic source.
pub const ROOM_TOPIC_TYPE: &str = "human_room";

/// Room kinds: a fixed pair, or a purpose group of humans only or of humans
/// plus agents.
pub const KIND_DM: &str = "dm";
pub const KIND_HUMAN_GROUP: &str = "human_group";
pub const KIND_MIXED_GROUP: &str = "mixed_group";

/// The DDL for schema v44. Every object is `IF NOT EXISTS`.
pub const SCHEMA_V44: &str = "CREATE TABLE IF NOT EXISTS human_principals(
        id TEXT PRIMARY KEY,
        org TEXT NOT NULL,
        issuer TEXT NOT NULL,
        subject TEXT NOT NULL,
        name TEXT NOT NULL,
        seen REAL NOT NULL,
        UNIQUE(issuer, org, subject));
     CREATE TABLE IF NOT EXISTS human_rooms(
        id TEXT PRIMARY KEY,
        topic_id TEXT NOT NULL UNIQUE REFERENCES topics(id),
        org TEXT NOT NULL,
        kind TEXT NOT NULL CHECK(kind IN ('dm','human_group','mixed_group')),
        pair TEXT UNIQUE,
        title TEXT NOT NULL,
        creator TEXT NOT NULL REFERENCES human_principals(id),
        created REAL NOT NULL);
     CREATE TABLE IF NOT EXISTS human_room_members(
        room TEXT NOT NULL REFERENCES human_rooms(id),
        kind TEXT NOT NULL CHECK(kind IN ('human','agent')),
        member TEXT NOT NULL,
        cutoff INTEGER NOT NULL,
        added REAL NOT NULL,
        removed REAL,
        PRIMARY KEY(room, member));
     CREATE TABLE IF NOT EXISTS human_read_markers(
        room TEXT NOT NULL REFERENCES human_rooms(id),
        human TEXT NOT NULL REFERENCES human_principals(id),
        seq INTEGER NOT NULL,
        PRIMARY KEY(room, human));";

/// The DDL for schema v47 (CAD-1423): per-human notification preferences,
/// device push consents, and private opaque notification intents. Every
/// object is `IF NOT EXISTS`. Intents carry opaque ids only, never message
/// text, and are written in the same transaction as the post they describe.
pub const SCHEMA_V45: &str = "CREATE TABLE IF NOT EXISTS human_notification_prefs(
        human TEXT PRIMARY KEY REFERENCES human_principals(id),
        dm INTEGER NOT NULL DEFAULT 1,
        mention INTEGER NOT NULL DEFAULT 1,
        sound INTEGER NOT NULL DEFAULT 0,
        preview INTEGER NOT NULL DEFAULT 0,
        quiet TEXT NOT NULL DEFAULT 'off' CHECK(quiet IN ('off','22-08')),
        zone TEXT NOT NULL DEFAULT 'UTC',
        updated REAL NOT NULL);
     CREATE TABLE IF NOT EXISTS human_notification_reads(
        room TEXT NOT NULL REFERENCES human_rooms(id),
        human TEXT NOT NULL REFERENCES human_principals(id),
        seq INTEGER NOT NULL,
        PRIMARY KEY(room, human));
     CREATE TABLE IF NOT EXISTS human_room_mutes(
        room TEXT NOT NULL REFERENCES human_rooms(id),
        human TEXT NOT NULL REFERENCES human_principals(id),
        muted REAL NOT NULL,
        PRIMARY KEY(room, human));
     CREATE TABLE IF NOT EXISTS human_push_devices(
        id TEXT PRIMARY KEY,
        org TEXT NOT NULL,
        issuer TEXT NOT NULL,
        subject TEXT NOT NULL,
        human TEXT NOT NULL REFERENCES human_principals(id),
        endpoint TEXT NOT NULL UNIQUE,
        p256dh TEXT NOT NULL,
        auth TEXT NOT NULL,
        session TEXT NOT NULL,
        enrolled REAL NOT NULL,
        expires REAL NOT NULL,
        revoked REAL);
     CREATE TABLE IF NOT EXISTS human_push_deliveries(
        intent INTEGER NOT NULL REFERENCES human_notification_intents(id),
        device TEXT NOT NULL REFERENCES human_push_devices(id),
        state TEXT NOT NULL CHECK(state IN ('pending','accepted','failed','suppressed')),
        attempts INTEGER NOT NULL DEFAULT 0,
        next_at REAL NOT NULL DEFAULT 0,
        lease TEXT,
        lease_until REAL NOT NULL DEFAULT 0,
        PRIMARY KEY(intent, device));
     CREATE TABLE IF NOT EXISTS human_push_presence(
        device TEXT NOT NULL REFERENCES human_push_devices(id),
        tab TEXT NOT NULL,
        room TEXT NOT NULL,
        expires REAL NOT NULL,
        PRIMARY KEY(device, tab));
     CREATE TABLE IF NOT EXISTS human_notification_intents(
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        room TEXT NOT NULL REFERENCES human_rooms(id),
        post TEXT NOT NULL,
        seq INTEGER NOT NULL,
        recipient TEXT NOT NULL REFERENCES human_principals(id),
        reason TEXT NOT NULL CHECK(reason IN ('dm','mention','group')),
        attempts INTEGER NOT NULL DEFAULT 0,
        created REAL NOT NULL,
        UNIQUE(post, recipient));";

/// A human principal the caller has already verified. Only server-side
/// verification may construct it; raw request fields are not a source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedHuman {
    pub id: String,
    pub org: String,
    issuer: String,
    subject: String,
    /// Presentation label only; never an authority input. Falls back to the
    /// verified subject when the directory has no name.
    pub name: String,
}

impl VerifiedHuman {
    /// Derive the stable principal id from the verified tuple. Refuses an
    /// empty field or one containing a newline (the tuple's separator).
    pub fn verified(issuer: &str, org: &str, subject: &str) -> Result<Self> {
        Self::with_name(issuer, org, subject, subject)
    }

    /// As [`Self::verified`], with a trusted presentation name.
    pub fn with_name(issuer: &str, org: &str, subject: &str, name: &str) -> Result<Self> {
        for field in [issuer, org, subject] {
            if field.is_empty() || field.contains('\n') {
                return Err(Error::rejected("human identity fields must be non-empty"));
            }
        }
        let digest = Sha256::digest(format!("{issuer}\n{org}\n{subject}").as_bytes());
        let hex: String = digest.iter().take(12).map(|b| format!("{b:02x}")).collect();
        Ok(Self {
            id: format!("human-{hex}"),
            org: org.to_string(),
            issuer: issuer.to_string(),
            subject: subject.to_string(),
            name: if name.trim().is_empty() {
                subject.to_string()
            } else {
                name.to_string()
            },
        })
    }
}

/// Read-only room summary for the room list (authorized rooms only).
#[derive(Debug, Clone, PartialEq)]
pub struct HumanRoomSummary {
    pub id: String,
    pub topic: String,
    pub kind: String,
    pub title: String,
    pub creator: String,
    pub created: f64,
    pub unread: i64,
}

/// Row shape of the room list query (internal).
struct RoomListRow {
    id: String,
    topic: String,
    kind: String,
    title: String,
    creator: String,
    created: f64,
    cutoff: i64,
    marker: i64,
}

/// One active room member. `kind` is `human` or `agent`; `id` is the verified
/// human id or the agent's durable uid; `label` is presentation only.
#[derive(Debug, Clone, PartialEq)]
pub struct HumanRoomMember {
    pub kind: String,
    pub id: String,
    pub label: Option<String>,
}

/// One initial member of a new group: a verified human id, or an agent alias
/// that resolves to the agent's current durable uid at write time.
#[derive(Debug, Clone)]
pub enum RoomMember {
    Human(String),
    Agent(String),
}

/// The stable room id for a topic-scoped room, keyed by the creation
/// request so a retry names the same room.
fn room_id(org: &str, key: &str) -> String {
    let digest = Sha256::digest(format!("room|{org}|{key}").as_bytes());
    let hex: String = digest.iter().take(12).map(|b| format!("{b:02x}")).collect();
    format!("room-{hex}")
}

/// Room row needed by every guard.
struct Room {
    org: String,
    kind: String,
    creator: String,
    topic: String,
}

fn room_in(conn: &impl StoreConn, room: &str) -> Result<Option<Room>> {
    Ok(conn.query_opt(
        "SELECT org,kind,creator,topic_id FROM human_rooms WHERE id=?",
        [room],
        |r| {
            Ok(Room {
                org: r.get(0)?,
                kind: r.get(1)?,
                creator: r.get(2)?,
                topic: r.get(3)?,
            })
        },
    )?)
}

/// The room row for an actor who may read it: same organization, active
/// membership. Anything else is refused with one message, so a caller
/// cannot tell an outsider from a removed member.
fn readable_room(conn: &impl StoreConn, actor: &VerifiedHuman, room: &str) -> Result<Room> {
    let row = room_in(conn, room)?
        .filter(|r| r.org == actor.org)
        .ok_or_else(|| Error::rejected("Room not found"))?;
    let active: Option<i64> = conn.query_opt(
        "SELECT 1 FROM human_room_members
          WHERE room=? AND kind='human' AND member=? AND removed IS NULL",
        params![room, actor.id],
        |r| r.get(0),
    )?;
    if active.is_none() {
        return Err(Error::rejected("Room not found"));
    }
    Ok(row)
}

/// Refuse unless the actor is the room's creator and an active member.
fn creator_room(conn: &impl StoreConn, actor: &VerifiedHuman, room: &str) -> Result<Room> {
    let row = readable_room(conn, actor, room)?;
    if row.creator != actor.id {
        return Err(Error::rejected(
            "Only the room creator may change membership",
        ));
    }
    Ok(row)
}

fn max_seq(conn: &impl StoreConn, topic: &str) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COALESCE(MAX(seq),0) FROM topic_posts WHERE topic_id=?",
        [topic],
        |r| r.get(0),
    )?)
}

/// The history cutoff of an active agent member of the topic's room in `org`.
/// Anything else (no room, another org, not a member, removed) is one refusal.
fn agent_cutoff_in(conn: &impl StoreConn, org: &str, agent_uid: &str, topic: &str) -> Result<i64> {
    conn.query_opt(
        "SELECT m.cutoff FROM human_room_members m JOIN human_rooms r ON r.id=m.room
          WHERE r.topic_id=? AND r.org=? AND m.kind='agent' AND m.member=? AND m.removed IS NULL",
        params![topic, org, agent_uid],
        |r| r.get(0),
    )?
    .ok_or_else(|| Error::rejected("Room not found"))
}

/// The current alias of a live agent uid.
fn agent_alias_in(conn: &impl StoreConn, agent_uid: &str) -> Result<String> {
    conn.query_opt("SELECT alias FROM agents WHERE uid=?", [agent_uid], |r| {
        r.get(0)
    })?
    .ok_or_else(|| Error::rejected("Agent not found"))
}

/// Write one transcript post under `client_id`, or return the stored one when
/// the same id and body come from the same sender. No queue, no turn.
fn insert_post(
    conn: &impl StoreConn,
    topic: &str,
    client_id: &str,
    sender_uid: &str,
    sender: &str,
    body: &str,
) -> Result<(TopicPost, bool)> {
    let post = post_id(topic, client_id);
    if let Some(existing) = Store::human_post_in(conn, &post)? {
        if existing.body != body || existing.sender_uid.as_deref() != Some(sender_uid) {
            return Err(Error::rejected(
                "Post id was already used with different content",
            ));
        }
        return Ok((existing, false));
    }
    conn.execute(
        "INSERT INTO topic_posts(id,topic_id,sender_uid,sender,body,created)
         VALUES(?,?,?,?,?,?)",
        params![post, topic, sender_uid, sender, body, now()],
    )?;
    let saved =
        Store::human_post_in(conn, &post)?.ok_or_else(|| Error::internal("posted row missing"))?;
    // CAD-1423: the one fresh-commit point for human and agent replies. The
    // intent names the real sender uid, so no caller fabricates a human.
    notification_intents(conn, topic, sender_uid, &saved)?;
    Ok((saved, true))
}

/// A human's post in a room the actor may read, under their verified name.
fn write_human_post(
    conn: &impl StoreConn,
    actor: &VerifiedHuman,
    topic: &str,
    client_id: &str,
    body: &str,
) -> Result<(TopicPost, bool)> {
    let sender = principal_name(conn, &actor.org, &actor.id)?.unwrap_or_else(|| actor.name.clone());
    insert_post(conn, topic, client_id, &actor.id, &sender, body)
}

/// Queue one private notification intent per current human recipient of a
/// freshly committed post, in the same transaction. Recipients are active
/// humans after their history cutoff, excluding the sender. The intent holds
/// opaque ids and a reason only, never the message text. Agent members get no
/// intent, so a notification never wakes an agent.
fn notification_intents(
    conn: &impl StoreConn,
    topic: &str,
    sender_uid: &str,
    post: &TopicPost,
) -> Result<()> {
    let room = conn.query_opt(
        "SELECT id,kind FROM human_rooms WHERE topic_id=?",
        [topic],
        |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
    )?;
    let Some((room, kind)) = room else {
        return Ok(());
    };
    let recipients: Vec<(String, String)> = conn.query_vec(
        "SELECT m.member, COALESCE(p.name,'')
           FROM human_room_members m
           JOIN human_principals p ON p.id=m.member
          WHERE m.room=? AND m.kind='human' AND m.removed IS NULL
            AND m.member<>? AND m.cutoff<?",
        params![room, sender_uid, post.seq],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let mentions_by_name = |name: &str| !name.is_empty() && post.body.contains(&format!("@{name}"));
    for (recipient, name) in recipients {
        let reason = if kind == KIND_DM {
            "dm"
        } else if mentions_by_name(&name) {
            "mention"
        } else {
            "group"
        };
        conn.execute(
            "INSERT OR IGNORE INTO human_notification_intents(room,post,seq,recipient,reason,created)
             VALUES(?,?,?,?,?,?)",
            params![room, post.id, post.seq, recipient, reason, now()],
        )?;
    }
    Ok(())
}

/// The newest `limit` rows of `topic` after `cutoff` and below `upper`, ascending.
fn page_rows(
    conn: &impl StoreConn,
    topic: &str,
    cutoff: i64,
    upper: i64,
    limit: i64,
) -> Result<Vec<TopicPost>> {
    let mut rows: Vec<TopicPost> = conn.query_vec(
        "SELECT seq,id,sender_uid,sender,body,created FROM topic_posts
          WHERE topic_id=? AND seq>? AND seq<? ORDER BY seq DESC LIMIT ?",
        params![topic, cutoff, upper, limit.clamp(1, 200)],
        |r| {
            Ok(TopicPost {
                seq: r.get(0)?,
                id: r.get(1)?,
                sender_uid: r.get(2)?,
                sender: r.get(3)?,
                body: r.get(4)?,
                created: r.get(5)?,
            })
        },
    )?;
    rows.reverse();
    Ok(rows)
}

/// Record the verified principal once. An existing non-empty display name is
/// kept; the actor's own name is only a fallback for a first sight.
fn upsert_principal(conn: &impl StoreConn, actor: &VerifiedHuman) -> Result<()> {
    conn.execute(
        "INSERT INTO human_principals(id,org,issuer,subject,name,seen)
         VALUES(?,?,?,?,?,?)
         ON CONFLICT(id) DO UPDATE SET seen=excluded.seen",
        params![
            actor.id,
            actor.org,
            actor.issuer,
            actor.subject,
            actor.name,
            now()
        ],
    )?;
    Ok(())
}

/// The display label stored for a principal, if it is in the org.
fn principal_name(conn: &impl StoreConn, org: &str, human: &str) -> Result<Option<String>> {
    Ok(conn.query_opt(
        "SELECT name FROM human_principals WHERE id=? AND org=?",
        params![human, org],
        |r| r.get(0),
    )?)
}

/// Presentation title for a room stored without one: the active members'
/// existing labels (human names, current agent aliases) excluding the viewer.
/// A member without a live label is skipped, never shown by uid.
fn presentation_title(
    conn: &impl StoreConn,
    room: &str,
    kind: &str,
    viewer: &str,
) -> Result<String> {
    let labels: Vec<Option<String>> = conn.query_vec(
        "SELECT COALESCE(p.name, a.alias)
           FROM human_room_members m
           LEFT JOIN human_principals p ON m.kind='human' AND p.id=m.member
           LEFT JOIN agents a ON m.kind='agent' AND a.uid=m.member
          WHERE m.room=? AND m.removed IS NULL AND m.member<>?
          ORDER BY m.added, m.member",
        params![room, viewer],
        |r| r.get(0),
    )?;
    let names: Vec<String> = labels
        .into_iter()
        .flatten()
        .filter(|n| !n.is_empty())
        .collect();
    Ok(match (kind, names.is_empty()) {
        (_, false) => names.join(", "),
        (KIND_DM, true) => "Direct message".into(),
        (_, true) => "Group".into(),
    })
}

impl Store {
    /// Open the durable DM between `actor` and `peer`, or return the
    /// existing one. `(room id, created)`. The pair is keyed by the sorted
    /// principal ids inside one org; a removed or missing member is never
    /// re-added here.
    pub fn human_dm_open(&self, actor: &VerifiedHuman, peer: &str) -> Result<(String, bool)> {
        if peer == actor.id {
            return Err(Error::rejected("A DM needs two different people"));
        }
        self.write_tx(|conn| {
            upsert_principal(conn, actor)?;
            let peer_org: Option<String> =
                conn.query_opt("SELECT org FROM human_principals WHERE id=?", [peer], |r| {
                    r.get(0)
                })?;
            if peer_org.as_deref() != Some(actor.org.as_str()) {
                return Err(Error::rejected("Peer is not a member of this organization"));
            }
            let (a, b) = if actor.id.as_str() <= peer {
                (actor.id.as_str(), peer)
            } else {
                (peer, actor.id.as_str())
            };
            let pair = format!("{}|{a}|{b}", actor.org);
            if let Some(existing) = conn.query_opt(
                "SELECT id FROM human_rooms WHERE pair=?",
                [pair.as_str()],
                |r| r.get::<_, String>(0),
            )? {
                return Ok((existing, false));
            }
            let id = room_id(&actor.org, &pair);
            let topic = super::topics::topic_id(ROOM_TOPIC_TYPE, &id);
            let at = now();
            conn.execute(
                "INSERT INTO topics(id,type,source,created) VALUES(?,?,?,?)",
                params![topic, ROOM_TOPIC_TYPE, id, at],
            )?;
            conn.execute(
                "INSERT INTO human_rooms(id,topic_id,org,kind,pair,title,creator,created)
                 VALUES(?,?,?,?,?,'',?,?)",
                params![id, topic, actor.org, KIND_DM, pair, actor.id, at],
            )?;
            for human in [a, b] {
                conn.execute(
                    "INSERT INTO human_room_members(room,kind,member,cutoff,added)
                     VALUES(?,'human',?,0,?)",
                    params![id, human, at],
                )?;
            }
            // No daemon-stream event: a legacy `topic_changed` would hand
            // room ids and post ids to every legacy subscriber. Clients
            // poll the authorized room list instead (CAD-1412).
            Ok((id, true))
        })
    }

    /// Create a purpose group. `human_group` takes humans only; a
    /// `mixed_group` needs at least one agent. The creator is its first
    /// human member. The group starts with no history: nothing from any
    /// other room is copied in.
    pub fn human_group_create(
        &self,
        actor: &VerifiedHuman,
        kind: &str,
        title: &str,
        key: &str,
        members: &[RoomMember],
    ) -> Result<String> {
        if kind != KIND_HUMAN_GROUP && kind != KIND_MIXED_GROUP {
            return Err(Error::rejected(
                "Group kind must be human_group or mixed_group",
            ));
        }
        let agents = members
            .iter()
            .filter(|m| matches!(m, RoomMember::Agent(_)))
            .count();
        if (kind == KIND_MIXED_GROUP) != (agents > 0) {
            return Err(Error::rejected(
                "Mixed groups need agents; human groups take none",
            ));
        }
        if title.trim().is_empty() || title.len() > 200 {
            return Err(Error::rejected("Group title must be 1-200 bytes"));
        }
        self.write_tx(|conn| {
            upsert_principal(conn, actor)?;
            let id = room_id(&actor.org, &format!("group|{}|{key}", actor.id));
            if room_in(conn, &id)?.is_some() {
                readable_room(conn, actor, &id)?;
                return Ok(id);
            }
            let topic = super::topics::topic_id(ROOM_TOPIC_TYPE, &id);
            let at = now();
            conn.execute(
                "INSERT INTO topics(id,type,source,created) VALUES(?,?,?,?)",
                params![topic, ROOM_TOPIC_TYPE, id, at],
            )?;
            conn.execute(
                "INSERT INTO human_rooms(id,topic_id,org,kind,pair,title,creator,created)
                 VALUES(?,?,?,?,NULL,?,?,?)",
                params![id, topic, actor.org, kind, title, actor.id, at],
            )?;
            conn.execute(
                "INSERT INTO human_room_members(room,kind,member,cutoff,added)
                 VALUES(?,'human',?,0,?)",
                params![id, actor.id, at],
            )?;
            for member in members {
                match member {
                    RoomMember::Human(human) => {
                        Self::human_member_in(conn, &actor.org, &id, human, at)?;
                    }
                    RoomMember::Agent(alias) => {
                        let uid: Option<String> = conn.query_opt(
                            "SELECT uid FROM agents WHERE alias=?",
                            [alias.as_str()],
                            |r| r.get(0),
                        )?;
                        let uid = uid.ok_or_else(|| Error::rejected("Agent not found"))?;
                        conn.execute(
                            "INSERT INTO human_room_members(room,kind,member,cutoff,added)
                             VALUES(?,'agent',?,0,?)",
                            params![id, uid, at],
                        )?;
                    }
                }
            }
            Ok(id)
        })
    }

    /// Add a human to a group (creator only). A rejoin after removal gets a
    /// new cutoff, so it never reveals history from before the removal.
    pub fn human_group_add_human(
        &self,
        actor: &VerifiedHuman,
        room: &str,
        human: &str,
    ) -> Result<()> {
        self.write_tx(|conn| {
            let row = creator_room(conn, actor, room)?;
            if row.kind == KIND_DM {
                return Err(Error::rejected("A DM has a fixed pair of members"));
            }
            Self::human_member_in(conn, &actor.org, room, human, now())
        })
    }

    fn human_member_in(
        conn: &impl StoreConn,
        org: &str,
        room: &str,
        human: &str,
        at: f64,
    ) -> Result<()> {
        let peer_org: Option<String> = conn.query_opt(
            "SELECT org FROM human_principals WHERE id=?",
            [human],
            |r| r.get(0),
        )?;
        if peer_org.as_deref() != Some(org) {
            return Err(Error::rejected("Member is not in this organization"));
        }
        let topic = room_in(conn, room)?
            .map(|r| r.topic)
            .ok_or_else(|| Error::rejected("Room not found"))?;
        let cutoff = max_seq(conn, &topic)?;
        conn.execute(
            "INSERT INTO human_room_members(room,kind,member,cutoff,added)
             VALUES(?,'human',?,?,?)
             ON CONFLICT(room,member) DO UPDATE
               SET removed=NULL, cutoff=excluded.cutoff, added=excluded.added
               WHERE human_room_members.removed IS NOT NULL",
            params![room, human, cutoff, at],
        )?;
        Ok(())
    }

    /// Add an agent to a mixed group by alias (creator only). The stored
    /// member is the agent's current durable uid.
    pub fn human_group_add_agent(
        &self,
        actor: &VerifiedHuman,
        room: &str,
        alias: &str,
    ) -> Result<()> {
        self.write_tx(|conn| {
            let row = creator_room(conn, actor, room)?;
            if row.kind != KIND_MIXED_GROUP {
                return Err(Error::rejected("Only mixed groups take agents"));
            }
            let uid: Option<String> =
                conn.query_opt("SELECT uid FROM agents WHERE alias=?", [alias], |r| {
                    r.get(0)
                })?;
            let uid = uid.ok_or_else(|| Error::rejected("Agent not found"))?;
            let cutoff = max_seq(conn, &row.topic)?;
            conn.execute(
                "INSERT INTO human_room_members(room,kind,member,cutoff,added)
                 VALUES(?,'agent',?,?,?)
                 ON CONFLICT(room,member) DO UPDATE
                   SET removed=NULL, cutoff=excluded.cutoff, added=excluded.added
                   WHERE human_room_members.removed IS NOT NULL",
                params![room, uid, cutoff, now()],
            )?;
            Ok(())
        })
    }

    /// Remove a member from a group (creator only). The row and history stay;
    /// the removed human is refused everywhere in this room. The creator
    /// cannot remove themselves and a DM has no removals.
    pub fn human_group_remove(
        &self,
        actor: &VerifiedHuman,
        room: &str,
        kind: &str,
        member: &str,
    ) -> Result<()> {
        self.write_tx(|conn| {
            let row = creator_room(conn, actor, room)?;
            if row.kind == KIND_DM {
                return Err(Error::rejected("A DM has a fixed pair of members"));
            }
            if kind == "human" && member == actor.id {
                return Err(Error::rejected("The creator cannot remove themselves"));
            }
            let changed = conn.execute(
                "UPDATE human_room_members SET removed=?
                  WHERE room=? AND kind=? AND member=? AND removed IS NULL",
                params![now(), room, kind, member],
            )?;
            if changed == 0 {
                return Err(Error::rejected("Member not found"));
            }
            Ok(())
        })
    }

    /// Post to a room as a verified active human. Writes one transcript
    /// row. No queue intent, no agent turn, no provider effect. A retry with
    /// the same `client_id` and body returns the existing post.
    pub fn human_room_post(
        &self,
        actor: &VerifiedHuman,
        room: &str,
        client_id: &str,
        body: &str,
    ) -> Result<(TopicPost, bool)> {
        check_post_body(body)?;
        self.write_tx(|conn| {
            let row = readable_room(conn, actor, room)?;
            write_human_post(conn, actor, &row.topic, client_id, body)
        })
    }

    fn human_post_in(conn: &impl StoreConn, post: &str) -> Result<Option<TopicPost>> {
        Ok(conn.query_opt(
            "SELECT seq,id,sender_uid,sender,body,created FROM topic_posts WHERE id=?",
            [post],
            |r| {
                Ok(TopicPost {
                    seq: r.get(0)?,
                    id: r.get(1)?,
                    sender_uid: r.get(2)?,
                    sender: r.get(3)?,
                    body: r.get(4)?,
                    created: r.get(5)?,
                })
            },
        )?)
    }

    /// A page of the room's history the actor may see: rows after their own
    /// join cutoff, newest-first from `before` (or the tail), returned
    /// ascending. Read-only.
    pub fn human_room_history(
        &self,
        actor: &VerifiedHuman,
        room: &str,
        before: Option<i64>,
        limit: i64,
    ) -> Result<Vec<TopicPost>> {
        self.read_tx(|conn| {
            let row = readable_room(conn, actor, room)?;
            let cutoff: i64 = conn.query_row(
                "SELECT cutoff FROM human_room_members WHERE room=? AND kind='human' AND member=?",
                params![room, actor.id],
                |r| r.get(0),
            )?;
            page_rows(conn, &row.topic, cutoff, before.unwrap_or(i64::MAX), limit)
        })
    }

    /// Server-side directory of verified principals for one org. Records a
    /// presentation name for an id; it grants no room access. Existing
    /// non-empty names are kept. Callers must derive the tuples from a
    /// trusted roster, never a raw request body.
    pub fn human_directory_upsert(
        &self,
        org: &str,
        people: &[(VerifiedHuman, String)],
    ) -> Result<()> {
        self.write_tx(|conn| {
            for (human, name) in people {
                if human.org != org {
                    return Err(Error::rejected(
                        "Directory entry is outside this organization",
                    ));
                }
                let label = if name.trim().is_empty() {
                    &human.name
                } else {
                    name
                };
                conn.execute(
                    "INSERT INTO human_principals(id,org,issuer,subject,name,seen)
                     VALUES(?,?,?,?,?,?)
                     ON CONFLICT(id) DO UPDATE SET
                       name=CASE WHEN human_principals.name='' THEN excluded.name
                                 ELSE human_principals.name END,
                       seen=excluded.seen",
                    params![human.id, org, human.issuer, human.subject, label, now()],
                )?;
            }
            Ok(())
        })
    }

    /// Rooms the actor is an active member of in their org, newest first.
    /// `unread` counts visible posts from others after the marker and cutoff.
    pub fn human_rooms_for(&self, actor: &VerifiedHuman) -> Result<Vec<HumanRoomSummary>> {
        self.read_tx(|conn| {
            let rows: Vec<RoomListRow> = conn.query_vec(
                "SELECT r.id, r.topic_id, r.kind, r.title, r.creator, r.created,
                        m.cutoff, COALESCE(k.seq,0)
                   FROM human_rooms r
                   JOIN human_room_members m
                     ON m.room=r.id AND m.kind='human' AND m.member=? AND m.removed IS NULL
                   LEFT JOIN human_read_markers k ON k.room=r.id AND k.human=?
                  WHERE r.org=?
                  ORDER BY r.created DESC, r.id",
                params![actor.id, actor.id, actor.org],
                |r| {
                    Ok(RoomListRow {
                        id: r.get(0)?,
                        topic: r.get(1)?,
                        kind: r.get(2)?,
                        title: r.get(3)?,
                        creator: r.get(4)?,
                        created: r.get(5)?,
                        cutoff: r.get(6)?,
                        marker: r.get(7)?,
                    })
                },
            )?;
            let mut out = Vec::with_capacity(rows.len());
            for RoomListRow {
                id,
                topic,
                kind,
                title,
                creator,
                created,
                cutoff,
                marker,
            } in rows
            {
                let unread: i64 = conn.query_row(
                    "SELECT COUNT(*) FROM topic_posts
                      WHERE topic_id=? AND seq>? AND seq>? AND (sender_uid IS NULL OR sender_uid<>?)",
                    params![topic, cutoff, marker, actor.id],
                    |r| r.get(0),
                )?;
                let title = if title.is_empty() {
                    presentation_title(conn, &id, &kind, &actor.id)?
                } else {
                    title
                };
                out.push(HumanRoomSummary {
                    id,
                    topic,
                    kind,
                    title,
                    creator,
                    created,
                    unread,
                });
            }
            Ok(out)
        })
    }

    /// Active members of a room the actor may read. Agent rows carry the
    /// durable uid; `alias` is the current presentation label, `None` when the
    /// uid no longer resolves to a live agent.
    pub fn human_room_members(
        &self,
        actor: &VerifiedHuman,
        room: &str,
    ) -> Result<Vec<HumanRoomMember>> {
        self.read_tx(|conn| {
            readable_room(conn, actor, room)?;
            let rows: Vec<(String, String, Option<String>)> = conn.query_vec(
                "SELECT m.kind, m.member, COALESCE(p.name, a.alias)
                   FROM human_room_members m
                   LEFT JOIN human_principals p ON m.kind='human' AND p.id=m.member
                   LEFT JOIN agents a ON m.kind='agent' AND a.uid=m.member
                  WHERE m.room=? AND m.removed IS NULL
                  ORDER BY m.added, m.member",
                [room],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?;
            Ok(rows
                .into_iter()
                .map(|(kind, id, label)| HumanRoomMember { kind, id, label })
                .collect())
        })
    }

    /// The topic and current alias of an Ask target: an active agent member of
    /// a room the actor may read. Read-only, checked before any write.
    pub fn human_room_ask_target(
        &self,
        actor: &VerifiedHuman,
        room: &str,
        agent_uid: &str,
    ) -> Result<(String, String)> {
        self.read_tx(|conn| {
            let row = readable_room(conn, actor, room)?;
            agent_cutoff_in(conn, &actor.org, agent_uid, &row.topic)?;
            Ok((row.topic, agent_alias_in(conn, agent_uid)?))
        })
    }

    /// The human's Ask post, written in one transaction that rechecks the
    /// actor's membership and the target's. Transcript only: the request is
    /// queued by the caller.
    pub fn human_room_ask_post(
        &self,
        actor: &VerifiedHuman,
        room: &str,
        agent_uid: &str,
        client_id: &str,
        body: &str,
    ) -> Result<(TopicPost, bool)> {
        check_post_body(body)?;
        self.write_tx(|conn| {
            let row = readable_room(conn, actor, room)?;
            agent_cutoff_in(conn, &actor.org, agent_uid, &row.topic)?;
            write_human_post(conn, actor, &row.topic, client_id, body)
        })
    }

    /// The private human room whose Ask produced this queued request message,
    /// resolved through the trusted post→request binding
    /// (`topic_post_bind_request`). Read-only; `None` when no room post
    /// carries that request message.
    pub fn human_room_for_request(&self, message_id: &str) -> Result<Option<String>> {
        self.read_tx(|conn| {
            Ok(conn.query_opt(
                "SELECT r.id FROM human_rooms r
                   JOIN topic_posts p ON p.topic_id=r.topic_id
                  WHERE p.request_message=?",
                [message_id],
                |r| r.get::<_, String>(0),
            )?)
        })
    }

    /// A page of a room's history for an active agent member: rows after the
    /// agent's cutoff, newest-first from `before`, returned ascending, with
    /// whether older rows remain. Read-only.
    pub fn human_agent_read(
        &self,
        org: &str,
        agent_uid: &str,
        topic: &str,
        before: Option<i64>,
        limit: i64,
    ) -> Result<(Vec<TopicPost>, bool)> {
        self.read_tx(|conn| {
            let cutoff = agent_cutoff_in(conn, org, agent_uid, topic)?;
            let rows = page_rows(conn, topic, cutoff, before.unwrap_or(i64::MAX), limit)?;
            let more = match rows.first() {
                Some(oldest) => {
                    let older: i64 = conn.query_row(
                        "SELECT COUNT(*) FROM topic_posts WHERE topic_id=? AND seq>? AND seq<?",
                        params![topic, cutoff, oldest.seq],
                        |r| r.get(0),
                    )?;
                    older > 0
                }
                None => false,
            };
            Ok((rows, more))
        })
    }

    /// An agent's transcript post in a room it is an active member of. The
    /// sender is its current uid and alias. No queue, no turn, no provider.
    pub fn human_agent_post(
        &self,
        org: &str,
        agent_uid: &str,
        topic: &str,
        client_id: &str,
        body: &str,
    ) -> Result<(TopicPost, bool)> {
        check_post_body(body)?;
        self.write_tx(|conn| {
            agent_cutoff_in(conn, org, agent_uid, topic)?;
            let alias = agent_alias_in(conn, agent_uid)?;
            insert_post(conn, topic, client_id, agent_uid, &alias, body)
        })
    }

    /// Current notification preferences for the actor. Defaults: DM and
    /// mention alerts on; sound, content preview and quiet hours off.
    pub fn notification_prefs(&self, actor: &VerifiedHuman) -> Result<NotificationPrefs> {
        self.read_tx(|conn| prefs_in(conn, &actor.id))
    }

    /// Replace the actor's preferences. Identity comes from the verified actor
    /// only; a missing row is created with the defaults first.
    pub fn notification_prefs_set(
        &self,
        actor: &VerifiedHuman,
        prefs: &NotificationPrefs,
    ) -> Result<()> {
        self.write_tx(|conn| {
            upsert_principal(conn, actor)?;
            conn.execute(
                "INSERT INTO human_notification_prefs(human,dm,mention,sound,preview,quiet,zone,updated)
                 VALUES(?,?,?,?,?,?,?,?)
                 ON CONFLICT(human) DO UPDATE SET dm=excluded.dm, mention=excluded.mention,
                   sound=excluded.sound, preview=excluded.preview, quiet=excluded.quiet,
                   zone=excluded.zone, updated=excluded.updated",
                params![
                    actor.id,
                    prefs.dm,
                    prefs.mention,
                    prefs.sound,
                    prefs.preview,
                    prefs.quiet.as_str(),
                    prefs.zone,
                    now()
                ],
            )?;
            Ok(())
        })
    }

    /// Enroll one browser push endpoint for the actor and mint its opaque
    /// device id. The endpoint is unique across all humans: a different human
    /// cannot take it over. A same-human retry refreshes its keys and returns
    /// the same id. Another human's endpoint is refused as a conflict.
    pub fn push_device_enroll(
        &self,
        actor: &VerifiedHuman,
        endpoint: &str,
        p256dh: &str,
        auth: &str,
        session: &str,
    ) -> Result<String> {
        self.write_tx(|conn| {
            upsert_principal(conn, actor)?;
            let owner: Option<(String, String)> = conn.query_opt(
                "SELECT id,human FROM human_push_devices WHERE endpoint=? AND revoked IS NULL",
                [endpoint],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            if let Some((id, human)) = owner {
                if human != actor.id {
                    return Err(Error::rejected(
                        "push endpoint is already enrolled for another account",
                    ));
                }
                conn.execute(
                    "UPDATE human_push_devices SET p256dh=?, auth=?, session=?, expires=? WHERE id=?",
                    params![p256dh, auth, session, now() + PUSH_CONSENT_SECS, id],
                )?;
                return Ok(id);
            }
            // A fresh consent replaces only a revoked endpoint. Retire its
            // device-local children before rotating the referenced device id.
            let revoked: Option<String> = conn.query_opt(
                "SELECT id FROM human_push_devices WHERE endpoint=? AND revoked IS NOT NULL",
                [endpoint],
                |r| r.get(0),
            )?;
            if let Some(revoked) = revoked {
                conn.execute(
                    "DELETE FROM human_push_presence WHERE device=?",
                    [&revoked],
                )?;
                conn.execute(
                    "DELETE FROM human_push_deliveries WHERE device=?",
                    [&revoked],
                )?;
            }
            let id = format!("device-{}", uuid::Uuid::new_v4().simple());
            conn.execute(
                "INSERT INTO human_push_devices(id,org,issuer,subject,human,endpoint,p256dh,auth,session,enrolled,expires)
                 VALUES(?,?,?,?,?,?,?,?,?,?,?)
                 ON CONFLICT(endpoint) DO UPDATE SET
                   id=excluded.id, org=excluded.org, issuer=excluded.issuer,
                   subject=excluded.subject, human=excluded.human,
                   p256dh=excluded.p256dh, auth=excluded.auth, session=excluded.session,
                   enrolled=excluded.enrolled, expires=excluded.expires, revoked=NULL
                 WHERE human_push_devices.revoked IS NOT NULL",
                params![
                    id,
                    actor.org,
                    actor.issuer,
                    actor.subject,
                    actor.id,
                    endpoint,
                    p256dh,
                    auth,
                    session,
                    now(),
                    now() + PUSH_CONSENT_SECS
                ],
            )?;
            Ok(id)
        })
    }

    /// Rebind one existing, live browser consent to the actor's current
    /// session. Endpoint is only a locator; the verified identity tuple must
    /// still own the exact unexpired, unrevoked row. No consent fields change.
    pub fn push_device_rebind(
        &self,
        actor: &VerifiedHuman,
        endpoint: &str,
        session: &str,
    ) -> Result<Option<String>> {
        self.write_tx(|conn| {
            let at = now();
            let device: Option<String> = conn.query_opt(
                "SELECT id FROM human_push_devices
                 WHERE endpoint=? AND human=? AND org=? AND issuer=? AND subject=?
                   AND revoked IS NULL AND expires>?",
                params![
                    endpoint,
                    actor.id,
                    actor.org,
                    actor.issuer,
                    actor.subject,
                    at
                ],
                |r| r.get(0),
            )?;
            let Some(device) = device else {
                return Ok(None);
            };
            let changed = conn.execute(
                "UPDATE human_push_devices SET session=?
                 WHERE id=? AND endpoint=? AND human=? AND org=? AND issuer=? AND subject=?
                   AND revoked IS NULL AND expires>?",
                params![
                    session,
                    device,
                    endpoint,
                    actor.id,
                    actor.org,
                    actor.issuer,
                    actor.subject,
                    at
                ],
            )?;
            Ok((changed == 1).then_some(device))
        })
    }

    /// Revoke one of the actor's own push devices. Returns whether a live
    /// device was removed. Another human's device is concealed the same as a
    /// missing one: `false`, nothing changed.
    pub fn push_device_revoke(&self, actor: &VerifiedHuman, device: &str) -> Result<bool> {
        self.write_tx(|conn| {
            let changed = conn.execute(
                "UPDATE human_push_devices SET revoked=?
                  WHERE id=? AND human=? AND revoked IS NULL",
                params![now(), device, actor.id],
            )?;
            Ok(changed > 0)
        })
    }

    /// Mute or unmute one room for the actor. Requires current active
    /// membership (the same guard as reading the room). A mute only stops
    /// interruptions: unread counts and transcript are unchanged.
    pub fn human_room_set_mute(
        &self,
        actor: &VerifiedHuman,
        room: &str,
        muted: bool,
    ) -> Result<()> {
        self.write_tx(|conn| {
            readable_room(conn, actor, room)?;
            if muted {
                conn.execute(
                    "INSERT INTO human_room_mutes(room,human,muted) VALUES(?,?,?)
                     ON CONFLICT(room,human) DO UPDATE SET muted=excluded.muted",
                    params![room, actor.id, now()],
                )?;
            } else {
                conn.execute(
                    "DELETE FROM human_room_mutes WHERE room=? AND human=?",
                    params![room, actor.id],
                )?;
            }
            Ok(())
        })
    }

    /// The actor's notification history: one group per room with received
    /// messages, most unread first. A message counts as unread when it is after
    /// the actor's history cutoff and the notification cursor, and not sent by
    /// the actor. The notification cursor is separate from the thread read
    /// marker, so inbox dismissal never marks a transcript read.
    pub fn notification_groups(&self, actor: &VerifiedHuman) -> Result<Vec<NotificationGroup>> {
        self.read_tx(|conn| {
            let rows: Vec<(String, String, String, i64, i64, bool)> = conn.query_vec(
                "SELECT r.id, r.kind, r.title,
                        (SELECT COUNT(*) FROM topic_posts t
                          WHERE t.topic_id=r.topic_id AND t.seq>m.cutoff
                            AND t.seq>COALESCE(n.seq,0)
                            AND t.seq>COALESCE(k.seq,0)
                            AND (t.sender_uid IS NULL OR t.sender_uid<>?)),
                        COALESCE((SELECT MAX(i.seq) FROM human_notification_intents i
                          WHERE i.room=r.id AND i.recipient=m.member
                            AND i.reason IN ('dm','mention') AND i.seq>m.cutoff
                            AND i.seq>COALESCE(n.seq,0) AND i.seq>COALESCE(k.seq,0)),0),
                        EXISTS(SELECT 1 FROM human_notification_intents i
                          WHERE i.room=r.id AND i.recipient=m.member AND i.reason='mention'
                            AND i.seq>m.cutoff AND i.seq>COALESCE(n.seq,0)
                            AND i.seq>COALESCE(k.seq,0))
                   FROM human_rooms r
                   JOIN human_room_members m
                     ON m.room=r.id AND m.kind='human' AND m.member=? AND m.removed IS NULL
                   LEFT JOIN human_read_markers k ON k.room=r.id AND k.human=?
                   LEFT JOIN human_notification_reads n ON n.room=r.id AND n.human=?
                  WHERE r.org=? AND EXISTS(SELECT 1 FROM topic_posts t
                    WHERE t.topic_id=r.topic_id AND t.seq>m.cutoff
                      AND (t.sender_uid IS NULL OR t.sender_uid<>?))",
                params![actor.id, actor.id, actor.id, actor.id, actor.org, actor.id],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                },
            )?;
            let mut groups = Vec::with_capacity(rows.len());
            for (room, kind, title, unread, alert_seq, mention) in rows {
                let muted: bool = conn
                    .query_opt(
                        "SELECT 1 FROM human_room_mutes WHERE room=? AND human=?",
                        params![room, actor.id],
                        |r| r.get::<_, i64>(0),
                    )?
                    .is_some();
                let title = if title.is_empty() {
                    presentation_title(conn, &room, &kind, &actor.id)?
                } else {
                    title
                };
                groups.push(NotificationGroup {
                    room,
                    kind,
                    title,
                    unread,
                    muted,
                    mention,
                    alert_seq,
                });
            }
            groups.sort_by(|a, b| b.unread.cmp(&a.unread).then(a.room.cmp(&b.room)));
            Ok(groups)
        })
    }

    /// Dismiss the actor's notification inbox up to each room's current
    /// newest post. This is NOT a thread read: it never moves the room read
    /// marker, so transcripts keep their own unread state. Returns the groups
    /// left unread (none, unless new posts arrive during the call).
    pub fn notification_mark_all_read(&self, actor: &VerifiedHuman) -> Result<()> {
        self.write_tx(|conn| {
            let rooms: Vec<(String, String)> = conn.query_vec(
                "SELECT r.id, r.topic_id FROM human_rooms r
                   JOIN human_room_members m
                     ON m.room=r.id AND m.kind='human' AND m.member=? AND m.removed IS NULL
                  WHERE r.org=?",
                params![actor.id, actor.org],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            for (room, topic) in rooms {
                let top = max_seq(conn, &topic)?;
                conn.execute(
                    "INSERT INTO human_notification_reads(room,human,seq) VALUES(?,?,?)
                     ON CONFLICT(room,human) DO UPDATE SET seq=MAX(seq,excluded.seq)",
                    params![room, actor.id, top],
                )?;
            }
            Ok(())
        })
    }

    /// Read-only candidates for the background initiator: `(intent, device)`
    /// pairs whose device consent is live (not revoked, not expired), whose
    /// recipient is a current human member after the cutoff, whose intent is
    /// within its TTL, and whose delivery row (if any) is pending, past its
    /// backoff and not under a live lease. Eligibility, presence and mutes are
    /// rechecked by the platform's prepare, so this only narrows the candidates.
    pub fn push_due_refs(&self, limit: i64) -> Result<Vec<(i64, String)>> {
        self.read_tx(|conn| {
            let at = now();
            let rows = conn.query_vec(
                "SELECT i.id,d.id FROM human_notification_intents i
                   JOIN human_push_devices d ON d.human=i.recipient
                   JOIN human_room_members m ON m.room=i.room AND m.member=i.recipient
                   LEFT JOIN human_push_deliveries v ON v.intent=i.id AND v.device=d.id
                  WHERE i.created>? AND d.revoked IS NULL AND d.expires>?
                    AND m.kind='human' AND m.removed IS NULL AND m.cutoff<i.seq
                    AND (v.intent IS NULL OR (v.state='pending' AND v.next_at<=?
                         AND v.attempts<? AND v.lease_until<=?))
                  ORDER BY i.id,d.id LIMIT ?",
                params![
                    at - PUSH_INTENT_TTL_SECS,
                    at,
                    at,
                    PUSH_MAX_ATTEMPTS,
                    at,
                    limit.clamp(1, 100)
                ],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            Ok(rows)
        })
    }

    /// The stored preferences for one recipient, or the defaults.
    pub fn notification_prefs_for(&self, human: &str) -> Result<NotificationPrefs> {
        self.read_tx(|conn| prefs_in(conn, human))
    }

    /// Whether an intent still names an interruption its recipient may get:
    /// an active member after the cutoff, the preference on for this reason,
    /// and the room not muted for them.
    pub fn push_recipient_eligible(&self, intent: &PushIntent) -> Result<bool> {
        self.read_tx(|conn| {
            let member: Option<i64> = conn.query_opt(
                "SELECT 1 FROM human_room_members m JOIN human_rooms r ON r.id=m.room
                  WHERE m.room=? AND m.kind='human' AND m.member=? AND m.removed IS NULL
                    AND m.cutoff<? AND r.id=?",
                params![intent.room, intent.recipient, intent.seq, intent.room],
                |r| r.get(0),
            )?;
            if member.is_none() {
                return Ok(false);
            }
            let prefs = prefs_in(conn, &intent.recipient)?;
            let allowed = match intent.reason.as_str() {
                "dm" => prefs.dm,
                "mention" => prefs.mention,
                _ => false,
            };
            if !allowed || quiet_now(prefs.quiet, &prefs.zone, now_secs()) {
                return Ok(false);
            }
            let muted: Option<i64> = conn.query_opt(
                "SELECT 1 FROM human_room_mutes WHERE room=? AND human=?",
                params![intent.room, intent.recipient],
                |r| r.get(0),
            )?;
            Ok(muted.is_none())
        })
    }

    /// One intent by its id, for a platform prepare. `None` when it is gone.
    pub fn push_intent_for_prepare(&self, intent: i64) -> Result<Option<PushIntent>> {
        self.read_tx(|conn| {
            Ok(conn.query_opt(
                "SELECT id,room,post,seq,recipient,reason,attempts FROM human_notification_intents WHERE id=?",
                [intent],
                |r| {
                    Ok(PushIntent {
                        id: r.get(0)?,
                        room: r.get(1)?,
                        post: r.get(2)?,
                        seq: r.get(3)?,
                        recipient: r.get(4)?,
                        reason: r.get(5)?,
                        attempts: r.get(6)?,
                    })
                },
            )?)
        })
    }

    /// Claim one device's delivery for at most `lease_secs`. The claim is
    /// atomic: a live lease, a settled device, or a device still backing off
    /// is refused as `None`, so a replay or a concurrent claim cannot send.
    pub fn push_delivery_claim(
        &self,
        intent: i64,
        device: &str,
        lease: &str,
        lease_secs: f64,
    ) -> Result<Option<PushDevice>> {
        self.write_tx(|conn| {
            let at = now();
            let changed = conn.execute(
                "INSERT INTO human_push_deliveries(intent,device,state,attempts,next_at,lease,lease_until)
                 SELECT ?,d.id,'pending',0,0,?,?
                   FROM human_push_devices d
                  WHERE d.id=? AND d.revoked IS NULL AND d.expires>?
                 ON CONFLICT(intent,device) DO UPDATE SET lease=excluded.lease,
                   lease_until=excluded.lease_until
                 WHERE human_push_deliveries.state='pending'
                   AND human_push_deliveries.next_at<=?
                   AND human_push_deliveries.attempts<?
                   AND human_push_deliveries.lease_until<=?",
                params![
                    intent,
                    lease,
                    at + lease_secs,
                    device,
                    at,
                    at,
                    PUSH_MAX_ATTEMPTS,
                    at
                ],
            )?;
            // A refused claim (live lease, settled or backing-off device, or
            // an ineligible device) changes no row. A replay of the same jti
            // therefore cannot re-read the device it already claimed.
            if changed == 0 {
                return Ok(None);
            }
            Ok(conn.query_opt(
                "SELECT id,endpoint,p256dh,auth,session,subject,issuer,org FROM human_push_devices WHERE id=?",
                [device],
                |r| {
                    Ok(PushDevice {
                        id: r.get(0)?,
                        endpoint: r.get(1)?,
                        p256dh: r.get(2)?,
                        auth: r.get(3)?,
                        session: r.get(4)?,
                        subject: r.get(5)?,
                        issuer: r.get(6)?,
                        org: r.get(7)?,
                    })
                },
            )?)
        })
    }

    /// Finish a claimed delivery under its live lease. Only the lease holder
    /// may finish; a stale or wrong lease changes nothing and returns false.
    /// `accepted` and `suppressed` are terminal; `gone` revokes the endpoint;
    /// `retry` uses the bounded backoff.
    pub fn push_delivery_finish(
        &self,
        intent: i64,
        device: &str,
        lease: &str,
        outcome: &str,
    ) -> Result<bool> {
        self.write_tx(|conn| {
            let at = now();
            let owned: Option<i64> = conn.query_opt(
                "SELECT 1 FROM human_push_deliveries WHERE intent=? AND device=? AND lease=? AND lease_until>?",
                params![intent, device, lease, at],
                |r| r.get(0),
            )?;
            if owned.is_none() {
                return Ok(false);
            }
            match outcome {
                "accepted" | "suppressed" => {
                    conn.execute(
                        "UPDATE human_push_deliveries SET state=?, lease=NULL, lease_until=0
                          WHERE intent=? AND device=?",
                        params![outcome, intent, device],
                    )?;
                }
                "gone" => {
                    conn.execute(
                        "UPDATE human_push_devices SET revoked=? WHERE id=? AND revoked IS NULL",
                        params![at, device],
                    )?;
                    conn.execute(
                        "UPDATE human_push_deliveries SET state='failed', lease=NULL, lease_until=0
                          WHERE intent=? AND device=?",
                        params![intent, device],
                    )?;
                }
                _ => {
                    let attempts: i64 = conn
                        .query_opt(
                            "SELECT attempts FROM human_push_deliveries WHERE intent=? AND device=?",
                            params![intent, device],
                            |r| r.get(0),
                        )?
                        .unwrap_or(0)
                        + 1;
                    let backoff = PUSH_BACKOFF_BASE_SECS * (1u64 << attempts.min(10)) as f64;
                    let state = if attempts >= PUSH_MAX_ATTEMPTS {
                        "failed"
                    } else {
                        "pending"
                    };
                    conn.execute(
                        "UPDATE human_push_deliveries SET state=?, attempts=?, next_at=?, lease=NULL, lease_until=0
                          WHERE intent=? AND device=?",
                        params![state, attempts, at + backoff, intent, device],
                    )?;
                }
            }
            Ok(true)
        })
    }

    /// Record a terminal `suppressed` row for one
    /// intent and device after the platform's prepare proof was verified. Only
    /// the intent's own recipient device in `org` matches. A pending row with no
    /// live lease is settled; a live lease or an already settled row is left
    /// untouched. Wrong or missing refs insert nothing.
    pub fn push_suppress_device(&self, intent: i64, device: &str, org: &str) -> Result<()> {
        self.write_tx(|conn| {
            conn.execute(
                "INSERT INTO human_push_deliveries(intent,device,state,attempts,next_at,lease,lease_until)
                 SELECT i.id,d.id,'suppressed',0,0,NULL,0
                   FROM human_notification_intents i JOIN human_push_devices d ON d.human=i.recipient
                  WHERE i.id=? AND d.id=? AND d.org=? AND d.revoked IS NULL
                 ON CONFLICT(intent,device) DO UPDATE SET state='suppressed', lease=NULL, lease_until=0
                 WHERE human_push_deliveries.state='pending' AND human_push_deliveries.lease_until<=?",
                params![intent, device, org, now()],
            )?;
            Ok(())
        })
    }

    /// Report one owned device tab's visible room (or clear only that tab).
    /// Reports expire after 30 seconds and only suppress that device's
    /// interruption. Another tab cannot clear this tab's visible room.
    pub fn push_presence_set(
        &self,
        actor: &VerifiedHuman,
        device: &str,
        tab: &str,
        room: Option<&str>,
    ) -> Result<bool> {
        self.write_tx(|conn| {
            let owned: Option<i64> = conn.query_opt(
                "SELECT 1 FROM human_push_devices WHERE id=? AND human=? AND revoked IS NULL",
                params![device, actor.id],
                |r| r.get(0),
            )?;
            if owned.is_none() {
                return Ok(false);
            }
            conn.execute(
                "DELETE FROM human_push_presence WHERE device=? AND (tab=? OR expires<=?)",
                params![device, tab, now()],
            )?;
            if let Some(room) = room {
                conn.execute(
                    "INSERT INTO human_push_presence(device,tab,room,expires) VALUES(?,?,?,?)",
                    params![device, tab, room, now() + 30.0],
                )?;
            }
            Ok(true)
        })
    }

    /// The authoritative human that owns one live push device, or `None` when
    /// the device is absent or revoked. Prepare refuses a device not owned by the intent's recipient.
    pub fn push_device_owner(&self, device: &str) -> Result<Option<String>> {
        self.read_tx(|conn| {
            let owner: Option<String> = conn.query_opt(
                "SELECT human FROM human_push_devices WHERE id=? AND revoked IS NULL",
                [device],
                |r| r.get(0),
            )?;
            Ok(owner)
        })
    }

    /// Whether a live delivery lease is held for this intent and device.
    pub fn push_delivery_lease_live(&self, intent: i64, device: &str) -> Result<bool> {
        self.read_tx(|conn| {
            let row: Option<i64> = conn.query_opt(
                "SELECT 1 FROM human_push_deliveries WHERE intent=? AND device=? AND lease IS NOT NULL AND lease_until>?",
                params![intent, device, now()],
                |r| r.get(0),
            )?;
            Ok(row.is_some())
        })
    }

    /// Whether this one device is visibly showing `room` for `human`, from its
    /// own live presence report. Other devices of the same human are unaffected.
    pub fn push_device_visible(&self, device: &str, human: &str, room: &str) -> Result<bool> {
        self.read_tx(|conn| {
            let row: Option<i64> = conn.query_opt(
                "SELECT 1 FROM human_push_presence p JOIN human_push_devices d ON d.id=p.device
                  WHERE p.device=? AND d.human=? AND p.room=? AND p.expires>? LIMIT 1",
                params![device, human, room, now()],
                |r| r.get(0),
            )?;
            Ok(row.is_some())
        })
    }

    /// Revoke every consent bound to one session digest. Called on logout and on
    /// an account or realm change, before the public-session row is pruned, so
    /// an expired row can never leave a live consent behind.
    pub fn push_consents_revoke_session(&self, session: &str) -> Result<usize> {
        self.write_tx(|conn| {
            Ok(conn.execute(
                "UPDATE human_push_devices SET revoked=? WHERE session=? AND revoked IS NULL",
                params![now(), session],
            )?)
        })
    }

    /// Mark the room read up to `seq`. Monotonic: a lower seq never moves the
    /// marker back. Returns the unread count after the update, counting posts
    /// from others that the actor may see.
    pub fn human_room_mark_read(&self, actor: &VerifiedHuman, room: &str, seq: i64) -> Result<i64> {
        self.write_tx(|conn| {
            let row = readable_room(conn, actor, room)?;
            // A requested seq past the newest post would hide future posts;
            // clamp it to the room's current maximum and never below zero.
            let top = max_seq(conn, &row.topic)?;
            let seq = seq.clamp(0, top);
            conn.execute(
                "INSERT INTO human_read_markers(room,human,seq) VALUES(?,?,?)
                 ON CONFLICT(room,human) DO UPDATE SET seq=MAX(seq,excluded.seq)",
                params![room, actor.id, seq],
            )?;
            let cutoff: i64 = conn.query_row(
                "SELECT cutoff FROM human_room_members WHERE room=? AND kind='human' AND member=?",
                params![room, actor.id],
                |r| r.get(0),
            )?;
            let marker: i64 = conn.query_row(
                "SELECT seq FROM human_read_markers WHERE room=? AND human=?",
                params![room, actor.id],
                |r| r.get(0),
            )?;
            Ok(conn.query_row(
                "SELECT COUNT(*) FROM topic_posts
                  WHERE topic_id=? AND seq>? AND seq>? AND (sender_uid IS NULL OR sender_uid<>?)",
                params![row.topic, cutoff, marker, actor.id],
                |r| r.get(0),
            )?)
        })
    }
}

/// One human's notification preferences. Presentation and interruption policy
/// only: none of these grants room access, Ask, or agent authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationPrefs {
    pub dm: bool,
    pub mention: bool,
    pub sound: bool,
    pub preview: bool,
    /// `off` or `22-08`: quiet from 22:00 to 08:00 in `zone`.
    pub quiet: QuietHours,
    /// The user's IANA time zone, validated by the caller. Defaults to UTC.
    pub zone: String,
}

/// The quiet-hours preset. One simple choice, evaluated in the user's own
/// IANA time zone at send time. No custom schedule, no per-day rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuietHours {
    Off,
    NightTwentyTwoToEight,
}

impl QuietHours {
    pub fn parse(preset: &str) -> Option<Self> {
        match preset {
            "off" => Some(Self::Off),
            "22-08" => Some(Self::NightTwentyTwoToEight),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::NightTwentyTwoToEight => "22-08",
        }
    }
    /// Whether `hour` (0-23, local to the user's zone) is inside the quiet window.
    pub fn covers(self, hour: u32) -> bool {
        match self {
            Self::Off => false,
            Self::NightTwentyTwoToEight => !(8..22).contains(&hour),
        }
    }
}

/// Is `at` (unix seconds) inside the user's quiet window? An unknown or
/// unparseable zone fails closed to quiet: the user chose quiet hours, so an
/// interruption is suppressed rather than guessed. Unread counts are unaffected.
pub fn quiet_now(quiet: QuietHours, zone: &str, at: i64) -> bool {
    if quiet == QuietHours::Off {
        return false;
    }
    use chrono::{TimeZone, Timelike};
    let Ok(tz) = zone.parse::<chrono_tz::Tz>() else {
        return true;
    };
    match tz.timestamp_opt(at, 0).single() {
        Some(local) => quiet.covers(local.hour()),
        None => true,
    }
}

impl Default for NotificationPrefs {
    fn default() -> Self {
        Self {
            dm: true,
            mention: true,
            sound: false,
            preview: false,
            quiet: QuietHours::Off,
            zone: "UTC".to_string(),
        }
    }
}

/// The preferences a human has, or the defaults when none are stored.
fn prefs_in(conn: &impl StoreConn, human: &str) -> Result<NotificationPrefs> {
    let row = conn.query_opt(
        "SELECT dm,mention,sound,preview,quiet,zone FROM human_notification_prefs WHERE human=?",
        [human],
        |r| {
            let quiet: String = r.get(4)?;
            Ok(NotificationPrefs {
                dm: r.get::<_, i64>(0)? != 0,
                mention: r.get::<_, i64>(1)? != 0,
                sound: r.get::<_, i64>(2)? != 0,
                preview: r.get::<_, i64>(3)? != 0,
                quiet: QuietHours::parse(&quiet).unwrap_or(QuietHours::Off),
                zone: r.get(5)?,
            })
        },
    )?;
    Ok(row.unwrap_or_default())
}

/// Delivery tuning: attempts before a device is marked failed, and the base
/// of the exponential backoff in seconds.
pub const PUSH_MAX_ATTEMPTS: i64 = 5;
pub const PUSH_BACKOFF_BASE_SECS: f64 = 30.0;

/// The per-device delivery lease, in seconds.
pub const LEASE_SECS_F64: f64 = 30.0;

/// How long an intent stays deliverable by the background initiator.
pub const PUSH_INTENT_TTL_SECS: f64 = 3600.0;

/// A push consent lives at most this long. It is independent of the public
/// session, which expires in an hour: an expired consent stops sending, and
/// a fresh sign-in plus an explicit re-enable is required to extend it.
pub const PUSH_CONSENT_SECS: f64 = 30.0 * 24.0 * 3600.0;

/// Unix seconds now, for quiet-hours evaluation at send time.
fn now_secs() -> i64 {
    now() as i64
}

/// One inbox group: a room with unread messages for the viewer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationGroup {
    pub room: String,
    pub kind: String,
    pub title: String,
    pub unread: i64,
    pub muted: bool,
    /// True when an unread mention intent remains for this viewer.
    pub mention: bool,
    /// Latest unread DM/mention intent sequence, or zero when none is eligible.
    pub alert_seq: i64,
}

/// One pending private push intent for a recipient. Opaque ids and a reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushIntent {
    pub id: i64,
    pub room: String,
    pub post: String,
    pub seq: i64,
    pub recipient: String,
    pub reason: String,
    pub attempts: i64,
}

/// One live browser push device of a recipient, with its bound session digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushDevice {
    pub id: String,
    pub endpoint: String,
    pub p256dh: String,
    pub auth: String,
    pub session: String,
    pub subject: String,
    pub issuer: String,
    pub org: String,
}
