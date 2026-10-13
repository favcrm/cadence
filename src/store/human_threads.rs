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
