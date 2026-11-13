//! CAD-2215: scoped reads widened by an explicit member app-conversation
//! grant.
//!
//! Every existing scoped read funnels through `messages::INDEX_PROJECT_FILTER`,
//! whose admission requires an issue or job binding. An app-conversation
//! message has neither, so it is invisible everywhere — including to a member
//! the operator explicitly granted. These variants add ONE extra branch: a
//! message whose enqueue note lives in a granted conversation. That branch
//! admits only messages with no issue or task binding and applies the cloud
//! exclusion; bound messages still require the existing project and private-job
//! checks, so a grant never bypasses them.
//! The branch is inert (`json_each` over an empty array yields no rows) for
//! every call that passes no conversations.
//!
//! The partition is: project-visible rows keep exactly today's admission;
//! granted rows are admitted only from the named conversations. Nothing else
//! changes.

use std::collections::{BTreeSet, HashMap};

use rusqlite::params;

use super::messages::{row_message, INDEX_PROJECT_FILTER};
use super::{
    Message, PrivateJobDenials, Store, StoreConn, ThreadEntry, KIND_ARTIFACT, KIND_ASSISTANT_TEXT,
    KIND_HIRE_PROPOSAL, KIND_SURFACE, KIND_TURN_RESULT, ROLE_AGENT, THREAD_PAGE_MAX,
};

/// The granted-conversation branch appended after [`INDEX_PROJECT_FILTER`].
/// It binds one scope JSON (`?`) whose `conversations` array names the exact
/// conversation ids the daemon proved for this reader.
const GRANTED_CONVERSATION_FILTER: &str = " OR EXISTS (
    SELECT 1 FROM thread_entries granted_entry
    CROSS JOIN (SELECT ? AS scope) granted_visible
    WHERE granted_entry.message_id = messages.id
      AND granted_entry.thread_id IN (
          SELECT value FROM json_each(granted_visible.scope, '$.conversations'))
      -- The grant admits ONLY a genuinely unbound app message. Any explicit
      -- or task-derived issue must pass INDEX_PROJECT_FILTER's project
      -- admission below; a missing/unresolvable task is never treated as
      -- unbound.
      AND messages.issue IS NULL
      AND messages.task_id IS NULL
      AND (COALESCE(json_extract(granted_visible.scope, '$.exclude_cloud'), 0) = 0 OR (
          NOT EXISTS (SELECT 1 FROM cloud_agents WHERE alias = messages.alias)
          AND NOT EXISTS (
              WITH RECURSIVE ancestry(id, origin, source) AS (
                  SELECT messages.id, messages.origin, messages.source
                  UNION
                  SELECT parent.id, parent.origin, parent.source
                  FROM ancestry JOIN messages parent ON parent.id = ancestry.origin
                  WHERE ancestry.source <> 'cloud_agent'
              )
              SELECT 1 FROM ancestry
              WHERE source = 'cloud_agent'
                 OR (origin IS NOT NULL AND NOT EXISTS (
                     SELECT 1 FROM messages parent WHERE parent.id = ancestry.origin))
              UNION ALL
              SELECT 1 WHERE NOT EXISTS (
                  SELECT 1 FROM ancestry WHERE origin IS NULL)
          )
      )))";

/// A granted conversation's page admits an entry only when its message
/// passes the EXISTING project admission, or is a genuinely unbound app
/// message (no explicit issue AND no task, so no task-derived issue) in this
/// already-granted conversation. A bound message therefore needs
/// `INDEX_PROJECT_FILTER`; the grant branch never substitutes for project
/// permission, and a missing/unresolvable task is never treated as unbound.
/// Message-less system notes are not part of a granted page: the grant
/// covers the conversation's messages, not the daemon's bookkeeping.
fn granted_entry_filter() -> String {
    format!(
        "EXISTS (
    SELECT 1 FROM messages
    CROSS JOIN (SELECT ? AS scope) granted_visible
    WHERE messages.id = thread_entries.message_id
      AND messages.alias = (SELECT alias FROM threads WHERE id = thread_entries.thread_id)
      AND (
        1=1 {}
        OR (
          messages.issue IS NULL
          AND messages.task_id IS NULL
          AND (COALESCE(json_extract(granted_visible.scope, '$.exclude_cloud'), 0) = 0 OR (
              NOT EXISTS (SELECT 1 FROM cloud_agents WHERE alias = messages.alias)
              AND NOT EXISTS (
                  WITH RECURSIVE ancestry(id, origin, source) AS (
                      SELECT messages.id, messages.origin, messages.source
                      UNION
                      SELECT parent.id, parent.origin, parent.source
                      FROM ancestry JOIN messages parent ON parent.id = ancestry.origin
                      WHERE ancestry.source <> 'cloud_agent'
                  )
                  SELECT 1 FROM ancestry
                  WHERE source = 'cloud_agent'
                     OR (origin IS NOT NULL AND NOT EXISTS (
                         SELECT 1 FROM messages parent WHERE parent.id = ancestry.origin))
                  UNION ALL
                  SELECT 1 WHERE NOT EXISTS (
                      SELECT 1 FROM ancestry WHERE origin IS NULL)
              )
          ))
        )
      )
)",
        INDEX_PROJECT_FILTER
    )
}

#[derive(serde::Serialize)]
struct ScopedFilter<'a> {
    issues: &'a [&'a str],
    denied_tasks: &'a BTreeSet<String>,
    denied_issues: &'a BTreeSet<String>,
    exclude_cloud: bool,
    conversations: &'a [String],
}

fn filter_json(
    issues: &[&str],
    denied: &PrivateJobDenials,
    conversations: &[String],
) -> crate::error::Result<String> {
    Ok(serde_json::to_string(&ScopedFilter {
        issues,
        denied_tasks: &denied.tasks,
        denied_issues: &denied.issues,
        exclude_cloud: denied.exclude_cloud,
        conversations,
    })?)
}

fn row_entry(row: &rusqlite::Row) -> rusqlite::Result<ThreadEntry> {
    let payload: Option<String> = row.get("payload")?;
    Ok(ThreadEntry {
        seq: row.get("seq")?,
        thread_id: row.get("thread_id")?,
        role: row.get("role")?,
        kind: row.get("kind")?,
        text: row.get("text")?,
        payload: payload.and_then(|payload| serde_json::from_str(&payload).ok()),
        message_id: row.get("message_id")?,
        created: row.get("created")?,
    })
}

impl Store {
    /// The reader's own in-flight turn, admitted by project policy OR by one
    /// of `conversations`, after applying project and private-job denials.
    pub(crate) fn running_message_scoped_with_conversations(
        &self,
        alias: &str,
        issues: &[&str],
        denied: &PrivateJobDenials,
        conversations: &[String],
    ) -> crate::error::Result<Option<Message>> {
        let conn = self.conn();
        let scope = filter_json(issues, denied, conversations)?;
        let sql = format!(
            "SELECT * FROM messages WHERE alias=? AND state='running'
               AND (1=1 {}{})
             ORDER BY seq DESC LIMIT 1",
            INDEX_PROJECT_FILTER, GRANTED_CONVERSATION_FILTER
        );
        match conn.query_row(&sql, params![alias, scope, scope], row_message) {
            Ok(message) => Ok(Some(message)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    /// The reader's waiting turns, admitted by project policy OR by one of
    /// `conversations`, each with the conversation its enqueue note landed in.
    /// Waiting turns retain delivery ordering; `None` means no enqueue note.
    #[allow(clippy::type_complexity)] // the queue row is one query's shape
    pub(crate) fn queued_metadata_scoped_with_conversations(
        &self,
        alias: &str,
        issues: &[&str],
        denied: &PrivateJobDenials,
        conversations: &[String],
    ) -> crate::error::Result<Vec<(String, String, f64, Option<String>)>> {
        let conn = self.conn();
        let scope = filter_json(issues, denied, conversations)?;
        let sql = format!(
            "WITH queue AS (
                 SELECT messages.id, messages.state, messages.priority, messages.seq,
                        messages.created
                 FROM messages
                 WHERE messages.alias=?1
                   AND messages.state IN ('queued','submitting')
                   AND (1=1 {}{})
             ), first_entry AS (
                 SELECT e.message_id, e.thread_id, MIN(e.seq) AS first_seq
                 FROM thread_entries e JOIN threads t ON t.id=e.thread_id
                 WHERE e.message_id IN (SELECT id FROM queue)
                   AND (t.alias=?1 OR t.archived_alias=?1)
                 GROUP BY e.message_id
             )
             SELECT q.id, q.state, q.created, f.thread_id
             FROM queue q
             LEFT JOIN first_entry f ON f.message_id=q.id
             ORDER BY q.state='submitting' DESC, q.priority DESC, q.seq",
            INDEX_PROJECT_FILTER, GRANTED_CONVERSATION_FILTER
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![alias, scope, scope], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// The per-alias running-id map that feeds `visible_running_messages`,
    /// widened by the same granted conversations. Every denial stays in force.
    pub(crate) fn scoped_running_message_ids_with_conversations(
        &self,
        issues: &[&str],
        denied: &PrivateJobDenials,
        conversations: &[String],
    ) -> crate::error::Result<HashMap<String, BTreeSet<String>>> {
        let conn = self.conn();
        let scope = filter_json(issues, denied, conversations)?;
        let sql = format!(
            "SELECT alias,id FROM messages
             WHERE state='running' AND source<>'topic_notice'
               AND (1=1 {}{})",
            INDEX_PROJECT_FILTER, GRANTED_CONVERSATION_FILTER
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![scope, scope], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut grouped = HashMap::new();
        for row in rows {
            let (alias, id) = row?;
            grouped
                .entry(alias)
                .or_insert_with(BTreeSet::new)
                .insert(id);
        }
        Ok(grouped)
    }

    /// A granted conversation's forward page. The thread itself is the
    /// authority; every entry must carry a message that passes the reader's
    /// private-job, project and cloud denials. The operator-only kinds are
    /// dropped for a member reader, exactly like the scoped page.
    pub(crate) fn thread_entries_after_granted(
        &self,
        thread_id: &str,
        after: i64,
        limit: i64,
        issues: &[&str],
        denied: &PrivateJobDenials,
        member: bool,
    ) -> crate::error::Result<Vec<ThreadEntry>> {
        let conn = self.conn();
        let scope = filter_json(issues, denied, &[])?;
        let sql = format!(
            "SELECT thread_entries.* FROM thread_entries
             WHERE thread_id=? AND seq>? AND {}
               AND (?=0 OR thread_entries.kind NOT IN (?,?,?))
             ORDER BY seq LIMIT ?",
            granted_entry_filter()
        );
        Ok(conn.query_vec(
            &sql,
            params![
                thread_id,
                after,
                scope,
                scope,
                member,
                KIND_ARTIFACT,
                KIND_HIRE_PROPOSAL,
                KIND_SURFACE,
                limit.clamp(1, THREAD_PAGE_MAX)
            ],
            row_entry,
        )?)
    }

    /// A granted conversation's backward page. Same rules as
    /// [`Self::thread_entries_before_scoped`], including selecting before the
    /// cursor and limit so hidden entries neither consume a page nor move it.
    pub(crate) fn thread_entries_before_granted(
        &self,
        thread_id: &str,
        before: Option<i64>,
        limit: i64,
        issues: &[&str],
        denied: &PrivateJobDenials,
        member: bool,
    ) -> crate::error::Result<(Vec<ThreadEntry>, bool)> {
        let conn = self.conn();
        let scope = filter_json(issues, denied, &[])?;
        let limit = limit.clamp(1, THREAD_PAGE_MAX);
        let sql = format!(
            "SELECT thread_entries.* FROM thread_entries
             WHERE thread_id=? AND seq<? AND {}
               AND (?=0 OR thread_entries.kind NOT IN (?,?,?))
             ORDER BY seq DESC LIMIT ?",
            granted_entry_filter()
        );
        let mut rows = conn.query_vec(
            &sql,
            params![
                thread_id,
                before.unwrap_or(i64::MAX),
                scope,
                scope,
                member,
                KIND_ARTIFACT,
                KIND_HIRE_PROPOSAL,
                KIND_SURFACE,
                limit + 1
            ],
            row_entry,
        )?;
        let more = rows.len() as i64 > limit;
        rows.truncate(limit as usize);
        rows.reverse();
        Ok((rows, more))
    }

    /// The newest visible assistant reply in a granted conversation, under
    /// the same denial rules as [`Self::thread_reply_seq_scoped`].
    pub(crate) fn thread_reply_seq_granted(
        &self,
        thread_id: &str,
        issues: &[&str],
        denied: &PrivateJobDenials,
    ) -> crate::error::Result<i64> {
        let conn = self.conn();
        let scope = filter_json(issues, denied, &[])?;
        let sql = format!(
            "SELECT COALESCE((SELECT seq FROM thread_entries
             WHERE thread_id=? AND role=? AND kind IN (?,?)
               AND length(trim(text, ?)) > 0 AND {}
             ORDER BY seq DESC LIMIT 1), 0)",
            granted_entry_filter()
        );
        Ok(conn.query_row(
            &sql,
            params![
                thread_id,
                ROLE_AGENT,
                KIND_ASSISTANT_TEXT,
                KIND_TURN_RESULT,
                super::threads::REPLY_WHITESPACE,
                scope,
                scope
            ],
            |row| row.get(0),
        )?)
    }
}
