//! CAD-810: topics and subscriptions — group fan-out with one shared
//! transcript.
//!
//! A topic is `(type, source)`: `fleet`, `project:<key>`, `job:<id>`,
//! `peer:<job>`. A subscription is keyed by the member's durable uid
//! (CAD-819), so a removed or re-registered alias never inherits a
//! membership. A post writes ONE transcript row and enqueues ONE turnless
//! delivery per current subscriber through the existing queue (source
//! `topic:<type>`, `notify_agent`). The caller rule lives in the RPC
//! layer; this module is storage only.
//!
//! Subscribers are the allowlist for read and publish. A removed subscriber
//! keeps its transcript rows; its delivery rows stay as history.

use rusqlite::params;
use serde_json::{json, Value};

use super::{now, Priority, Sender, Store, StoreConn};
use crate::error::{Error, Result};

// The alias and uid fences are kept in the query itself so classification and
// the content or receipt it protects share a single SQLite read snapshot.
const PRIVATE_TOPIC_ALIASES: &str = "SELECT alias FROM assistants
    UNION SELECT alias FROM assistant_event_fences
    UNION SELECT alias FROM effect_member_owners
    UNION SELECT alias FROM assist_notice_deliveries
    UNION SELECT alias FROM effect_notice_deliveries";
const PRIVATE_TOPIC_UIDS: &str = "SELECT g.uid FROM agents g JOIN assistants a ON a.alias=g.alias
    UNION SELECT m.sender_uid FROM messages m JOIN assistants a ON a.alias=m.sender
      WHERE m.sender_uid IS NOT NULL
    UNION SELECT m.recipient_uid FROM messages m JOIN assistants a ON a.alias=m.recipient
      WHERE m.recipient_uid IS NOT NULL
    UNION SELECT f.agent_uid FROM assistant_event_fences f WHERE f.agent_uid IS NOT NULL
    UNION SELECT e.agent_uid FROM effect_member_owners e WHERE e.agent_uid IS NOT NULL";

fn visible_topic_post() -> String {
    format!(
        "p.sender NOT IN ({PRIVATE_TOPIC_ALIASES}) AND (
          (p.sender_uid IS NULL AND p.sender IN ('operator', 'daemon'))
          OR (p.sender_uid IS NOT NULL AND p.sender_uid NOT IN ({PRIVATE_TOPIC_UIDS}))
        )"
    )
}

fn visible_topic_recipient(alias: &str, uid: &str) -> String {
    format!(
        "{alias} IS NOT NULL AND {uid} IS NOT NULL
         AND {alias} NOT IN ({PRIVATE_TOPIC_ALIASES})
         AND {uid} NOT IN ({PRIVATE_TOPIC_UIDS})
         AND NOT EXISTS (SELECT 1 FROM agents g WHERE g.uid={uid} AND g.alias<>{alias})
         AND NOT EXISTS (SELECT 1 FROM agents g WHERE g.alias={alias} AND g.uid<>{uid})"
    )
}

pub(super) fn refuse_private_topic_uid(conn: &impl StoreConn, uid: &str) -> Result<()> {
    let restricted = conn.query_opt(
        "SELECT 1 FROM agents g JOIN assistants a ON a.alias=g.alias WHERE g.uid=?1
         UNION SELECT 1 FROM assistant_event_fences f WHERE f.agent_uid=?1
         UNION SELECT 1 FROM effect_member_owners e WHERE e.agent_uid=?1
         LIMIT 1",
        [uid],
        |_| Ok(()),
    )?;
    if restricted.is_some() {
        return Err(Error::rejected(super::RESTRICTED_LIFECYCLE));
    }
    Ok(())
}

fn ordinary_topic_subscribers(conn: &impl StoreConn, topic: &str) -> Result<Vec<(String, String)>> {
    let visible = visible_topic_recipient("a.alias", "a.uid");
    let sql = format!(
        "SELECT s.uid,a.alias FROM topic_subscriptions s
           JOIN agents a ON a.uid=s.uid
          WHERE s.topic_id=? AND a.archived_at IS NULL AND {visible}
          ORDER BY s.added,s.uid"
    );
    Ok(conn.query_vec(&sql, [topic], |r| Ok((r.get(0)?, r.get(1)?)))?)
}

/// The schema for v39. Every object is `IF NOT EXISTS`, so a half-applied
/// migration converges on reopen.
pub(super) const SCHEMA_V39: &str = "CREATE TABLE IF NOT EXISTS topics(
        id TEXT PRIMARY KEY,
        type TEXT NOT NULL,
        source TEXT NOT NULL,
        created REAL NOT NULL,
        UNIQUE(type, source));
     CREATE TABLE IF NOT EXISTS topic_subscriptions(
        topic_id TEXT NOT NULL REFERENCES topics(id),
        uid TEXT NOT NULL,
        added REAL NOT NULL,
        PRIMARY KEY(topic_id, uid));
     CREATE TABLE IF NOT EXISTS topic_posts(
        seq INTEGER PRIMARY KEY AUTOINCREMENT,
        id TEXT UNIQUE NOT NULL,
        topic_id TEXT NOT NULL REFERENCES topics(id),
        sender_uid TEXT,
        sender TEXT NOT NULL,
        body TEXT NOT NULL,
        created REAL NOT NULL,
        request_message TEXT);
     CREATE INDEX IF NOT EXISTS topic_posts_topic ON topic_posts(topic_id, seq);
     CREATE TABLE IF NOT EXISTS topic_deliveries(
        post_id TEXT NOT NULL REFERENCES topic_posts(id),
        uid TEXT NOT NULL,
        alias TEXT NOT NULL,
        message_id TEXT,
        state TEXT NOT NULL DEFAULT 'pending',
        PRIMARY KEY(post_id, uid));";

/// The daemon-stream event a topic-only transition records, so the board's
/// live feed can invalidate the Threads view (CAD-818). Topic posts and
/// deliveries are visible as messages too; this covers create, membership
/// and post/delivery-intent writes that no message row carries.
pub const TOPIC_CHANGED_EVENT: &str = "topic_changed";

/// The bounded body length a post may carry (the queued-message cap).
pub const POST_MAX_BYTES: usize = 48_000;

/// A topic row.
#[derive(Debug, Clone)]
pub struct Topic {
    pub id: String,
    pub kind: String,
    pub source: String,
    pub created: f64,
}

impl Topic {
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "type": self.kind,
            "source": self.source,
            "created": self.created,
        })
    }
}

/// The stable topic id for a `(type, source)` pair.
pub fn topic_id(kind: &str, source: &str) -> String {
    let digest = <sha2::Sha256 as sha2::Digest>::digest(format!("{kind}:{source}").as_bytes());
    let hex: String = digest.iter().take(12).map(|b| format!("{b:02x}")).collect();
    format!("topic-{hex}")
}

/// The stable id for one post, derived from the topic and the caller's id
/// so a retry of the same envelope names the same transcript row.
pub fn post_id(topic: &str, client_id: &str) -> String {
    let digest = <sha2::Sha256 as sha2::Digest>::digest(format!("{topic}|{client_id}").as_bytes());
    let hex: String = digest.iter().take(12).map(|b| format!("{b:02x}")).collect();
    format!("post-{hex}")
}

/// One subscriber's delivery intent for a post. `message` is set once the
/// delivery is queued; `state` is `pending` until then.
#[derive(Debug, Clone)]
pub struct TopicDelivery {
    pub uid: String,
    pub alias: String,
    pub message: Option<String>,
    pub state: String,
}

/// CAD-2207: how one queued request binds to a topic transcript post.
///
/// `Existing` is a post its caller already committed in its own transaction
/// (a human room's member-authorized Ask). `New` is a generic operator Ask
/// whose post must be inserted in the same write transaction as the queue row
/// and the request binding, so a refusal leaves no post, delivery intent,
/// binding or queued message behind.
#[derive(Debug, Clone, Copy)]
pub(crate) enum TopicRequestPost<'a> {
    Existing(&'a str),
    New {
        topic: &'a str,
        client_id: &'a str,
        body: &'a str,
    },
}

/// A receipt and its identity proof from one store snapshot.
pub(crate) struct VisibleTopicReceipt {
    pub uid: String,
    pub alias: String,
    pub message: Option<String>,
    pub state: String,
    pub removed: bool,
}

impl VisibleTopicReceipt {
    pub(crate) fn to_json(&self) -> Value {
        json!({
            "uid": self.uid,
            "alias": self.alias,
            "message": self.message,
            "state": self.state,
            "removed": self.removed,
        })
    }
}

/// One transcript row of a topic.
#[derive(Debug, Clone)]
pub struct TopicPost {
    pub seq: i64,
    pub id: String,
    pub sender_uid: Option<String>,
    pub sender: String,
    pub body: String,
    pub created: f64,
}

impl TopicPost {
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "seq": self.seq,
            "sender": {"uid": self.sender_uid, "alias": self.sender},
            "body": self.body,
            "created": self.created,
        })
    }
}

impl Store {
    /// Create a topic, or return the existing one for `(type, source)`.
    /// Idempotent: the second call returns `(topic, false)`.
    pub fn topic_ensure(&self, kind: &str, source: &str) -> Result<(Topic, bool)> {
        self.write_tx(|conn| {
            let id = topic_id(kind, source);
            let existing: Option<f64> = conn.query_opt(
                "SELECT created FROM topics WHERE id=?",
                [id.as_str()],
                |r| r.get(0),
            )?;
            if let Some(created) = existing {
                return Ok((
                    Topic {
                        id,
                        kind: kind.into(),
                        source: source.into(),
                        created,
                    },
                    false,
                ));
            }
            let created = now();
            conn.execute(
                "INSERT INTO topics(id,type,source,created) VALUES(?,?,?,?)",
                params![id, kind, source, created],
            )?;
            Store::event(
                conn,
                Store::DAEMON_STREAM,
                TOPIC_CHANGED_EVENT,
                json!({"topic": id, "change": "created"}),
            )?;
            Ok((
                Topic {
                    id,
                    kind: kind.into(),
                    source: source.into(),
                    created,
                },
                true,
            ))
        })
    }

    /// The topic with this id, or `None`.
    pub fn topic(&self, id: &str) -> Result<Option<Topic>> {
        self.read_tx(|conn| topic_in(conn, id))
    }

    /// Subscribe (or re-subscribe) an agent uid. Idempotent.
    pub fn topic_subscribe(&self, topic: &str, uid: &str) -> Result<()> {
        self.write_tx(|conn| {
            refuse_private_topic_uid(conn, uid)?;
            let changed = conn.execute(
                "INSERT OR IGNORE INTO topic_subscriptions(topic_id,uid,added) VALUES(?,?,?)",
                params![topic, uid, now()],
            )?;
            if changed > 0 {
                Store::event(
                    conn,
                    Store::DAEMON_STREAM,
                    TOPIC_CHANGED_EVENT,
                    json!({"topic": topic, "change": "subscribed", "uid": uid}),
                )?;
            }
            Ok(())
        })
    }

    /// Unsubscribe an agent uid. The transcript and delivery history stay.
    pub fn topic_unsubscribe(&self, topic: &str, uid: &str) -> Result<()> {
        self.write_tx(|conn| {
            refuse_private_topic_uid(conn, uid)?;
            let changed = conn.execute(
                "DELETE FROM topic_subscriptions WHERE topic_id=? AND uid=?",
                params![topic, uid],
            )?;
            if changed > 0 {
                Store::event(
                    conn,
                    Store::DAEMON_STREAM,
                    TOPIC_CHANGED_EVENT,
                    json!({"topic": topic, "change": "unsubscribed", "uid": uid}),
                )?;
            }
            Ok(())
        })
    }

    /// Topics with visible post counts and latest visible activity, newest first.
    /// Read-only: the operator work list's group rows.
    pub fn topic_listing(&self) -> Result<Vec<(Topic, i64, i64, f64)>> {
        self.read_tx(|conn| {
            let visible = visible_topic_post();
            let sql = format!(
                "SELECT t.id,t.type,t.source,t.created,
                        (SELECT COUNT(*) FROM topic_subscriptions s WHERE s.topic_id=t.id),
                        (SELECT COUNT(*) FROM topic_posts p WHERE p.topic_id=t.id AND {visible}),
                        COALESCE((SELECT MAX(p.created) FROM topic_posts p WHERE p.topic_id=t.id AND {visible}),t.created)
                   FROM topics t ORDER BY 7 DESC, t.created DESC"
            );
            Ok(conn.query_vec(
                &sql,
                [],
                |r| {
                    Ok((
                        Topic {
                            id: r.get(0)?,
                            kind: r.get(1)?,
                            source: r.get(2)?,
                            created: r.get(3)?,
                        },
                        r.get(4)?,
                        r.get(5)?,
                        r.get(6)?,
                    ))
                },
            )?)
        })
    }

    /// The newest `topic_changed` event seq and the newest message seq — the
    /// Threads stamp (read-only). Both move on the writes the view shows.
    pub fn threads_stamp(&self) -> Result<(i64, i64, i64)> {
        self.read_tx(|conn| {
            let topic: i64 = conn.query_row(
                "SELECT COALESCE(MAX(seq),0) FROM events WHERE alias=? AND kind=?",
                params![Store::DAEMON_STREAM, TOPIC_CHANGED_EVENT],
                |r| r.get(0),
            )?;
            // A state transition rewrites `state`, `started` and `completed`
            // on an existing row, so the newest seq alone would miss it; the
            // newest of those timestamps moves on every such write.
            let message: i64 =
                conn.query_row("SELECT COALESCE(MAX(seq),0) FROM messages", [], |r| {
                    r.get(0)
                })?;
            let transition: f64 = conn.query_row(
                "SELECT COALESCE(MAX(COALESCE(completed, started, created)),0) FROM messages",
                [],
                |r| r.get(0),
            )?;
            Ok((topic, message, (transition * 1000.0) as i64))
        })
    }

    /// CAD-1700: the master push stamp (read-only, indexed). `(live_seq,
    /// running, queued, newest_thread_entry_seq, transition_ms)`: the newest
    /// live master message seq, the running count, the queued count, the
    /// newest master thread entry across every master thread, and the newest
    /// master message transition. Moves on enqueue, run, finish, and any entry.
    pub fn master_stamp(&self, alias: &str) -> Result<(i64, i64, i64, i64, i64)> {
        self.read_tx(|conn| {
            let live: (i64, i64, i64) = conn.query_row(
                "SELECT COALESCE(MAX(seq),0),
                        COALESCE(SUM(CASE WHEN state='running' THEN 1 ELSE 0 END),0),
                        COALESCE(SUM(CASE WHEN state='queued' THEN 1 ELSE 0 END),0)
                 FROM messages WHERE alias=? AND state IN ('queued','submitting','running')",
                [alias],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?;
            // Per-thread MAX keeps the (thread_id, seq) index doing the work;
            // a global MAX(seq) would move for every agent's thread.
            let entries: i64 = conn.query_row(
                "SELECT COALESCE(MAX(e.seq),0) FROM thread_entries e
                 JOIN threads t ON t.id=e.thread_id WHERE t.alias=?",
                [alias],
                |r| r.get(0),
            )?;
            let transition: f64 = conn.query_row(
                "SELECT COALESCE(MAX(COALESCE(completed, started, created)),0)
                 FROM messages WHERE alias=?",
                [alias],
                |r| r.get(0),
            )?;
            Ok((
                live.0,
                live.1,
                live.2,
                entries,
                (transition * 1000.0) as i64,
            ))
        })
    }

    /// CAD-810 job fan-out, run inside the job transition's own transaction:
    /// when `job:<job id>` exists, the event is posted to its transcript and
    /// one turnless notice goes to each current member. Keyed by the job
    /// delivery id, so a retry of the transition writes nothing twice.
    pub(super) fn job_topic_fanout_in(
        &self,
        tx: &impl StoreConn,
        job: &crate::store::Job,
        delivery: &str,
        body: &str,
    ) -> Result<()> {
        let topic = topic_id("job", &job.id);
        let exists: Option<String> =
            tx.query_opt("SELECT id FROM topics WHERE id=?", [topic.as_str()], |r| {
                r.get(0)
            })?;
        if exists.is_none() {
            return Ok(());
        }
        let post = post_id(&topic, delivery);
        if tx
            .query_opt(
                "SELECT id FROM topic_posts WHERE id=?",
                [post.as_str()],
                |r| r.get::<_, String>(0),
            )?
            .is_some()
        {
            return Ok(());
        }
        tx.execute(
            "INSERT INTO topic_posts(id,topic_id,sender_uid,sender,body,created)
             VALUES(?,?,NULL,'daemon',?,?)",
            params![post, topic, body, now()],
        )?;
        let members = ordinary_topic_subscribers(tx, &topic)?;
        for (uid, alias) in members {
            let notice = delivery_id(&post, &uid);
            tx.execute(
                "INSERT OR IGNORE INTO topic_deliveries(post_id,uid,alias,message_id,state)
                 VALUES(?,?,?,NULL,'pending')",
                params![post, uid, alias],
            )?;
            self.enqueue_tx_as(
                tx,
                &alias,
                body,
                None,
                &notice,
                crate::proto::TOPIC_NOTICE_SOURCE,
                None,
                None,
                None,
                &Sender::Unattributed,
                Priority::Normal,
                true,
                None,
                None,
                None,
                None,
            )?;
            tx.execute(
                "UPDATE topic_deliveries SET message_id=?, state='queued' WHERE post_id=? AND uid=?",
                params![notice, post, uid],
            )?;
        }
        Ok(())
    }

    /// The post with this id, or `None`.
    pub fn topic_post_by_id(&self, id: &str) -> Result<Option<TopicPost>> {
        self.read_tx(|conn| {
            Ok(conn.query_opt(
                "SELECT seq,id,sender_uid,sender,body,created FROM topic_posts WHERE id=?",
                [id],
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
        })
    }

    /// Bind a request post to the queued request message it created (CAD-821).
    /// Set once; a second, different binding is a conflict.
    pub fn topic_post_bind_request(&self, post: &str, message: &str) -> Result<()> {
        self.write_tx(|conn| bind_request_in(conn, post, message))
    }

    /// The request message a post is bound to, if any.
    pub fn topic_post_request(&self, post: &str) -> Result<Option<String>> {
        self.read_tx(|conn| {
            Ok(conn
                .query_opt(
                    "SELECT request_message FROM topic_posts WHERE id=?",
                    [post],
                    |r| r.get::<_, Option<String>>(0),
                )?
                .flatten())
        })
    }

    /// Resolve an existing generic-topic Ask only while its target identity,
    /// binding and stored envelope agree in the same read snapshot.
    pub(crate) fn topic_existing_request_for_target(
        &self,
        post: &str,
        text: &str,
        target: &str,
        target_uid: &str,
    ) -> Result<Option<String>> {
        self.read_tx(|conn| {
            refuse_private_topic_uid(conn, target_uid)?;
            if self.assistant_for_alias_in(conn, target)?.is_some() {
                return Err(Error::rejected(super::RESTRICTED_LIFECYCLE));
            }
            if self.agent_in(conn, target)?.uid != target_uid {
                return Err(Error::rejected("topic request target changed"));
            }
            let existing: Option<(String, String, Option<String>)> = conn.query_opt(
                "SELECT body,sender,request_message FROM topic_posts WHERE id=?",
                [post],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?;
            let Some((body, sender, bound)) = existing else {
                return Ok(None);
            };
            if body != text || sender != "operator" {
                return Err(Error::rejected(
                    "Request id was already used with different content",
                ));
            }
            let Some(bound) = bound else {
                return Ok(None);
            };
            let recipient: Option<(Option<String>, Option<String>, String)> = conn.query_opt(
                "SELECT recipient_uid,recipient,alias FROM messages WHERE id=?",
                [&bound],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?;
            if !recipient.is_some_and(|(uid, label, alias)| {
                uid.as_deref() == Some(target_uid)
                    && label.as_deref() == Some(target)
                    && alias == target
            }) {
                return Err(Error::rejected(
                    "Request id was already used with a different target",
                ));
            }
            Ok(Some(bound))
        })
    }

    /// CAD-810 atomic publish: the post, one intent per current subscriber,
    /// and one daemon-origin topic notice per subscriber are written in ONE
    /// transaction, each notice through the store's own enqueue path (its
    /// `queued` event and identity snapshot are the fence the claim reads).
    /// The intent is bound to its notice message id in the same transaction.
    ///
    /// A retry with the same `client_id` finds the post and returns
    /// `(post, false)`; nothing is written twice, and no intent is stranded
    /// because a failed transaction writes none of them.
    pub fn topic_publish(
        &self,
        topic: &str,
        client_id: &str,
        sender_uid: Option<&str>,
        sender: &str,
        body: &str,
    ) -> Result<(String, bool)> {
        check_post_body(body)?;
        let post = post_id(topic, client_id);
        self.write_tx(|conn| {
            if self.assistant_for_alias_in(conn, sender)?.is_some() {
                return Err(Error::rejected(super::RESTRICTED_LIFECYCLE));
            }
            if let Some(uid) = sender_uid {
                refuse_private_topic_uid(conn, uid)?;
            }
            let existing: Option<(String, String)> = conn.query_opt(
                "SELECT body, sender FROM topic_posts WHERE id=?",
                [post.as_str()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            if let Some((stored, stored_sender)) = existing {
                if stored != body || stored_sender != sender {
                    return Err(Error::rejected(
                        "Post id was already used with different content",
                    ));
                }
                return Ok((post, false));
            }
            conn.execute(
                "INSERT INTO topic_posts(id,topic_id,sender_uid,sender,body,created)
                 VALUES(?,?,?,?,?,?)",
                params![post, topic, sender_uid, sender, body, now()],
            )?;
            let members = ordinary_topic_subscribers(conn, topic)?;
            for (uid, alias) in members {
                let notice = delivery_id(&post, &uid);
                conn.execute(
                    "INSERT OR IGNORE INTO topic_deliveries(post_id,uid,alias,message_id,state)
                     VALUES(?,?,?,NULL,'pending')",
                    params![post, uid, alias],
                )?;
                // The notice is a daemon message: its id is in the daemon's
                // namespace and its source is `topic_notice`, both refused on
                // every ordinary send. It names the topic and the post only.
                let notice_body = format!("[topic {topic}] {body}");
                self.enqueue_tx_as(
                    conn,
                    &alias,
                    &notice_body,
                    None,
                    &notice,
                    crate::proto::TOPIC_NOTICE_SOURCE,
                    None,
                    None,
                    None,
                    &Sender::Unattributed,
                    Priority::Normal,
                    true,
                    None,
                    None,
                    None,
                    None,
                )?;
                conn.execute(
                    "UPDATE topic_deliveries SET message_id=?, state='queued'
                      WHERE post_id=? AND uid=?",
                    params![notice, post, uid],
                )?;
            }
            Store::event(
                conn,
                Store::DAEMON_STREAM,
                TOPIC_CHANGED_EVENT,
                json!({"topic": topic, "change": "posted", "post": post}),
            )?;
            Ok((post, true))
        })
    }

    /// The transcript row for a post, written once per `(topic, client id)`.
    /// Returns `(post_id, created)` and `true` on first write; a retry of
    /// the same envelope returns `false`. A retry naming a different body
    /// is a conflict, never a second row.
    pub fn topic_post_insert(
        &self,
        topic: &str,
        client_id: &str,
        sender_uid: Option<&str>,
        sender: &str,
        body: &str,
    ) -> Result<(String, bool)> {
        self.write_tx(|conn| {
            self.topic_post_insert_in(conn, topic, client_id, sender_uid, sender, body)
        })
    }

    /// `topic_post_insert` inside a caller's write transaction (CAD-2207: a
    /// generic Ask's queue commit owns the transaction, so the post, its
    /// intents, the queued request and the request binding either all commit
    /// or all roll back).
    pub(super) fn topic_post_insert_in(
        &self,
        conn: &impl StoreConn,
        topic: &str,
        client_id: &str,
        sender_uid: Option<&str>,
        sender: &str,
        body: &str,
    ) -> Result<(String, bool)> {
        check_post_body(body)?;
        let id = post_id(topic, client_id);
        let existing: Option<(String, String)> = conn.query_opt(
            "SELECT body, sender FROM topic_posts WHERE id=?",
            [id.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if let Some((stored, stored_sender)) = existing {
            if stored != body || stored_sender != sender {
                return Err(Error::rejected(
                    "Post id was already used with different content",
                ));
            }
            return Ok((id, false));
        }
        conn.execute(
            "INSERT INTO topic_posts(id,topic_id,sender_uid,sender,body,created)
             VALUES(?,?,?,?,?,?)",
            params![id, topic, sender_uid, sender, body, now()],
        )?;
        // Intents for eligible subscribers are committed with the post.
        // Bound assistants need a separate member-authorized path.
        let members = ordinary_topic_subscribers(conn, topic)?;
        for (uid, alias) in members {
            conn.execute(
                "INSERT OR IGNORE INTO topic_deliveries(post_id,uid,alias,message_id,state)
                 VALUES(?,?,?,NULL,'pending')",
                params![id, uid, alias],
            )?;
        }
        Store::event(
            conn,
            Store::DAEMON_STREAM,
            TOPIC_CHANGED_EVENT,
            json!({"topic": topic, "change": "posted", "post": id}),
        )?;
        Ok((id, true))
    }

    /// A page of a topic's transcript: `tail` by default; `before`/`after`
    /// move by seq. Ascending within the page. Read-only.
    pub fn topic_posts(
        &self,
        topic: &str,
        before: Option<i64>,
        after: Option<i64>,
        limit: i64,
    ) -> Result<Vec<TopicPost>> {
        self.topic_posts_filtered(topic, before, after, limit, false)
    }

    /// A generic reader's page; unproved or private authors are excluded
    /// before the body, cursor or limit is selected.
    pub(crate) fn topic_visible_posts(
        &self,
        topic: &str,
        before: Option<i64>,
        after: Option<i64>,
        limit: i64,
    ) -> Result<Vec<TopicPost>> {
        self.topic_posts_filtered(topic, before, after, limit, true)
    }

    fn topic_posts_filtered(
        &self,
        topic: &str,
        before: Option<i64>,
        after: Option<i64>,
        limit: i64,
        visible_only: bool,
    ) -> Result<Vec<TopicPost>> {
        self.read_tx(|conn| {
            let mut base =
                "SELECT seq,id,sender_uid,sender,body,created FROM topic_posts p WHERE topic_id=?1"
                    .to_string();
            if visible_only {
                base.push_str(" AND ");
                base.push_str(&visible_topic_post());
            }
            let row = |r: &rusqlite::Row<'_>| {
                Ok(TopicPost {
                    seq: r.get(0)?,
                    id: r.get(1)?,
                    sender_uid: r.get(2)?,
                    sender: r.get(3)?,
                    body: r.get(4)?,
                    created: r.get(5)?,
                })
            };
            let (sql, cursor, forward) = match (before, after) {
                (Some(b), _) => (
                    format!("{base} AND seq<?2 ORDER BY seq DESC LIMIT ?3"),
                    b,
                    false,
                ),
                (None, Some(a)) => (
                    format!("{base} AND seq>?2 ORDER BY seq ASC LIMIT ?3"),
                    a,
                    true,
                ),
                (None, None) => (
                    format!("{base} AND seq>?2 ORDER BY seq DESC LIMIT ?3"),
                    0,
                    false,
                ),
            };
            let mut rows: Vec<TopicPost> =
                conn.query_vec(&sql, params![topic, cursor, limit], row)?;
            if !forward {
                rows.reverse();
            }
            Ok(rows)
        })
    }

    /// The per-subscriber deliveries of a post: `(uid, alias, message_id)`
    /// in recipient order. The alias is the one stored at delivery time.
    pub fn topic_deliveries(&self, post: &str) -> Result<Vec<TopicDelivery>> {
        self.read_tx(|conn| {
            Ok(conn.query_vec(
                "SELECT uid, alias, message_id, state FROM topic_deliveries
                  WHERE post_id=? ORDER BY uid",
                [post],
                |r| {
                    Ok(TopicDelivery {
                        uid: r.get(0)?,
                        alias: r.get(1)?,
                        message: r.get(2)?,
                        state: r.get(3)?,
                    })
                },
            )?)
        })
    }

    /// Generic-reader delivery receipts, including live state and removed
    /// status, classified and read under the same store snapshot.
    pub(crate) fn topic_visible_deliveries(&self, post: &str) -> Result<Vec<VisibleTopicReceipt>> {
        self.read_tx(|conn| {
            let visible = visible_topic_recipient("d.alias", "d.uid");
            let sql = format!(
                "SELECT d.uid,d.alias,d.message_id,
                        CASE WHEN d.message_id IS NULL THEN d.state ELSE m.state END,
                        NOT EXISTS(SELECT 1 FROM agents g WHERE g.uid=d.uid)
                   FROM topic_deliveries d LEFT JOIN messages m ON m.id=d.message_id
                  WHERE d.post_id=?1 AND {visible}
                    AND (d.message_id IS NULL OR
                         (m.id IS NOT NULL AND m.recipient_uid=d.uid AND m.recipient=d.alias))
                  ORDER BY d.uid"
            );
            Ok(conn.query_vec(&sql, [post], |r| {
                Ok(VisibleTopicReceipt {
                    uid: r.get(0)?,
                    alias: r.get(1)?,
                    message: r.get(2)?,
                    state: r.get(3)?,
                    removed: r.get(4)?,
                })
            })?)
        })
    }

    /// Whether a post has an Ask and, if its recipient is provably ordinary,
    /// the complete receipt. An unproved Ask never exposes its handle.
    pub(crate) fn topic_visible_request(
        &self,
        post: &str,
    ) -> Result<(bool, Option<VisibleTopicReceipt>)> {
        self.read_tx(|conn| {
            let request: Option<Option<String>> = conn.query_opt(
                "SELECT request_message FROM topic_posts WHERE id=?",
                [post],
                |r| r.get(0),
            )?;
            let Some(request) = request.flatten() else {
                return Ok((false, None));
            };
            let visible = visible_topic_recipient("m.recipient", "m.recipient_uid");
            let sql = format!(
                "SELECT m.recipient_uid,m.recipient,m.id,m.state,
                        NOT EXISTS(SELECT 1 FROM agents g WHERE g.uid=m.recipient_uid)
                   FROM messages m
                  WHERE m.id=?1 AND m.alias=m.recipient AND {visible}"
            );
            let receipt = conn.query_opt(&sql, [&request], |r| {
                Ok(VisibleTopicReceipt {
                    uid: r.get(0)?,
                    alias: r.get(1)?,
                    message: Some(r.get(2)?),
                    state: r.get(3)?,
                    removed: r.get(4)?,
                })
            })?;
            Ok((true, receipt))
        })
    }

    /// Whether the topic holds any post older than `before` (a seq).
    pub fn topic_has_posts_before(&self, topic: &str, before: i64) -> Result<bool> {
        self.read_tx(|conn| {
            Ok(conn
                .query_opt(
                    "SELECT 1 FROM topic_posts WHERE topic_id=? AND seq<? LIMIT 1",
                    params![topic, before],
                    |_| Ok(()),
                )?
                .is_some())
        })
    }

    /// Whether a generic reader has an older visible post on this topic.
    pub(crate) fn topic_has_visible_posts_before(&self, topic: &str, before: i64) -> Result<bool> {
        self.read_tx(|conn| {
            let visible = visible_topic_post();
            let sql = format!(
                "SELECT 1 FROM topic_posts p WHERE topic_id=? AND seq<? AND {visible} LIMIT 1"
            );
            Ok(conn
                .query_opt(&sql, params![topic, before], |_| Ok(()))?
                .is_some())
        })
    }

    /// Whether `uid` is a current subscriber of `topic`.
    pub fn topic_is_subscriber(&self, topic: &str, uid: &str) -> Result<bool> {
        self.read_tx(|conn| {
            Ok(conn
                .query_opt(
                    "SELECT 1 FROM topic_subscriptions WHERE topic_id=? AND uid=?",
                    params![topic, uid],
                    |_| Ok(()),
                )?
                .is_some())
        })
    }

    /// Current subscriber uids of `topic`, in subscription order.
    pub fn topic_subscribers(&self, topic: &str) -> Result<Vec<String>> {
        self.read_tx(|conn| {
            Ok(conn.query_vec(
                "SELECT uid FROM topic_subscriptions WHERE topic_id=? ORDER BY added, uid",
                [topic],
                |r| r.get(0),
            )?)
        })
    }
}

/// Reuse the set-once binding guard inside an existing queue transaction.
pub(super) fn bind_request_in(conn: &impl StoreConn, post: &str, message: &str) -> Result<()> {
    // Outer None: no such post. Inner None: the post is unbound.
    let current: Option<Option<String>> = conn.query_opt(
        "SELECT request_message FROM topic_posts WHERE id=?",
        [post],
        |r| r.get::<_, Option<String>>(0),
    )?;
    match current {
        None => Err(Error::rejected("Request post not found")),
        Some(Some(bound)) if bound != message => Err(Error::rejected(
            "Request post already bound to a different message",
        )),
        Some(_) => {
            conn.execute(
                "UPDATE topic_posts SET request_message=? WHERE id=?",
                params![message, post],
            )?;
            Ok(())
        }
    }
}

fn topic_in(conn: &impl StoreConn, id: &str) -> Result<Option<Topic>> {
    Ok(conn.query_opt(
        "SELECT id,type,source,created FROM topics WHERE id=?",
        [id],
        |r| {
            Ok(Topic {
                id: r.get(0)?,
                kind: r.get(1)?,
                source: r.get(2)?,
                created: r.get(3)?,
            })
        },
    )?)
}

/// The stable id of a request message: scoped to the topic and the client
/// id, so two topics never collide on one request id.
pub fn request_id(topic: &str, client_id: &str) -> String {
    let digest =
        <sha2::Sha256 as sha2::Digest>::digest(format!("req|{topic}|{client_id}").as_bytes());
    let hex: String = digest.iter().take(12).map(|b| format!("{b:02x}")).collect();
    format!("topic-req-{hex}")
}

/// The stable queue id for one subscriber's delivery of a post. Ids use
/// the `topic-` namespace, never the daemon's `sys-` prefix, so the
/// caller-message guard still applies to the envelope.
pub fn delivery_id(post: &str, uid: &str) -> String {
    let digest = <sha2::Sha256 as sha2::Digest>::digest(format!("{post}|{uid}").as_bytes());
    let hex: String = digest.iter().take(12).map(|b| format!("{b:02x}")).collect();
    // The id grammar allows lowercase letters, digits and hyphens only, so
    // the kind segment is the source with its underscore spelled as a hyphen.
    format!("{}topic-notice-{hex}", crate::proto::DAEMON_MESSAGE_PREFIX)
}

/// Refuse a body that is empty or over the queue cap.
pub fn check_post_body(body: &str) -> Result<()> {
    if body.is_empty() || body.len() > POST_MAX_BYTES {
        return Err(Error::rejected("Post must contain 1-48000 bytes"));
    }
    Ok(())
}
