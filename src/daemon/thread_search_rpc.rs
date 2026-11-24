//! CAD-2663: `thread_search` — a native agent searches its own conversation.
//!
//! The thread searched is the one the caller's own running turn belongs to:
//! the turn's app conversation, else the agent's home thread — the thread
//! its continuity pack is built from (CAD-1098 I7). Nothing in the request
//! names an agent, thread or conversation.
//!
//! What can be found is what the agent's continuity pack could carry back
//! to it, under its current read scope:
//! - only entries whose message reached the agent, the pack's rule:
//!   delivered, and not the turn the search runs in;
//! - an issue-bound message only while the agent's project scope admits
//!   that issue, and a task only while no private-job denial covers it —
//!   the scoped-agent rules `thread_read` applies;
//! - a message bound to no issue or task (a plain conversation turn) as
//!   the pack carries it;
//! - the cloud projection over every row.
//!
//! A hit carries an opaque handle sealed to this agent, thread, turn and
//! read scope; `around` with that handle reads a few entries either side.
//! No global thread position is returned. Text is returned as stored
//! (secret-redacted on the way in), with turn tokens redacted on the way
//! out; a payload is never returned.

use super::scoped_cursor::{CursorBinding, CursorDirection};
use super::*;
use crate::thread_search::{self as search, Query};

#[cfg(all(test, feature = "test-seam"))]
#[path = "thread_search_rpc/cad2663_refusal_acceptance.rs"]
mod cad2663_refusal_acceptance;
#[cfg(all(test, feature = "test-seam"))]
#[path = "thread_search_rpc/cad2663_tests.rs"]
mod cad2663_tests;

/// Request fields `thread_search` accepts. Anything naming an agent, thread
/// or conversation is refused by name, never treated as a narrower read.
const FIELDS: [&str; 7] = [
    "message", "token", "query", "limit", "around", "before", "after",
];
const PURPOSE: &str = "thread search";
const DOMAIN: &str = "agent-thread-search";
/// Entries read per store batch while scanning.
const BATCH: i64 = 200;

/// Matching entries with their scores.
type Found = Vec<(search::Score, store::ThreadEntry)>;

/// One verified caller's searchable view of one thread.
struct View<'a> {
    thread: &'a store::Thread,
    current: &'a str,
    issues: Vec<&'a str>,
    denials: &'a store::PrivateJobDenials,
    projection: super::cloud_agent_rpc::CloudProjection,
}

impl Shared {
    pub(super) fn rpc_thread_search(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        let fields = params
            .as_object()
            .ok_or_else(|| Error::rejected("thread search takes an object"))?;
        if let Some(field) = fields.keys().find(|key| !FIELDS.contains(&key.as_str())) {
            return Err(Error::rejected(format!(
                "thread search does not take `{field}`: it reads only the calling \
                 agent's own conversation"
            )));
        }
        let Caller::Agent(verified) = self.caller_identity(peer_pid)? else {
            return Err(Error::rejected(
                "thread search requires the calling native agent",
            ));
        };
        if crate::adapter::agent_tool_route(&verified.agent.provider).is_none() {
            return Err(Error::rejected(format!(
                "thread search is not available to the `{}` harness",
                verified.agent.provider
            )));
        }
        let alias = verified.agent.alias.clone();
        let (id, token) = self.held_turn(&alias, params, PURPOSE, "turn")?;
        if self.adoption_pending(&alias, &id, &token) {
            return Err(Error::rejected(
                "thread search requires a settled native turn adoption",
            ));
        }
        let (agent, message) = self
            .store
            .owned_turn(&verified.agent, &id, &token, PURPOSE)?;
        let around = match params.get("around") {
            None => None,
            Some(Value::String(handle)) if !handle.is_empty() => Some(handle.as_str()),
            Some(_) => return Err(Error::rejected("thread search `around` is a hit handle")),
        };
        let query = match (around, optional_str(params, "query")) {
            (None, Some(raw)) => Some(Query::parse(raw)?),
            (None, None) => return Err(Error::rejected("thread search needs a query")),
            (Some(_), None) => None,
            (Some(_), Some(_)) => {
                return Err(Error::rejected(
                    "thread search takes a query, or `around` a hit, not both",
                ))
            }
        };
        let side = |field: &str| -> Result<usize> {
            match params.get(field) {
                None => Ok(search::SIDE_DEFAULT),
                Some(value) if around.is_some() => value
                    .as_u64()
                    .filter(|n| *n as usize <= search::SIDE_MAX)
                    .map(|n| n as usize)
                    .ok_or_else(|| {
                        Error::rejected(format!(
                            "thread search `{field}` must be 0-{}",
                            search::SIDE_MAX
                        ))
                    }),
                Some(_) => Err(Error::rejected(format!(
                    "thread search `{field}` needs `around`"
                ))),
            }
        };
        let (before, after) = (side("before")?, side("after")?);
        let limit = match params.get("limit") {
            None => search::HITS_DEFAULT,
            Some(value) if around.is_none() => value
                .as_u64()
                .filter(|n| (1..=search::HITS_MAX as u64).contains(n))
                .map(|n| n as usize)
                .ok_or_else(|| {
                    Error::rejected(format!(
                        "thread search limit must be 1-{}",
                        search::HITS_MAX
                    ))
                })?,
            Some(_) => return Err(Error::rejected("thread search `limit` needs a query")),
        };

        // The conversation the turn belongs to, exactly as its pack.
        let thread = self
            .store
            .message_conversation(&alias, &message.id)?
            .or(self.store.thread(&alias)?);
        let Some(thread) = thread else {
            return match query {
                Some(_) => Ok(json!({"hits": [], "matches": 0, "scanned": 0,
                                     "complete": true, "truncated": false})),
                None => Err(Error::rejected("thread search hit is no longer readable")),
            };
        };

        // The scoped-agent read rules thread_read applies to this caller.
        let scope = self
            .project_scope(peer_pid, PURPOSE)?
            .ok_or_else(|| Error::rejected("thread search requires an agent read scope"))?;
        let principal = super::job_events_opaque::agent_read_principal(
            self, params, peer_pid, None, None, PURPOSE,
        )?
        .ok_or_else(|| Error::rejected("thread search requires the calling agent"))?;
        let denials = self.private_job_denials_for_read(params, None, None, peer_pid)?;
        let projection = self.admit_cloud_projection(&alias, peer_pid, PURPOSE)?;
        let codec = self.scoped_cursor_codec()?;
        let filters = super::agent_events_opaque::project_set_filters(&scope.0);
        let created = format!("{:016x}", thread.created.to_bits());
        let resource = super::job_events_opaque::framed(&[
            DOMAIN,
            &alias,
            &agent.uid,
            &thread.id,
            &created,
            &message.id,
        ]);
        let binding = CursorBinding {
            domain: DOMAIN,
            direction: CursorDirection::Forward,
            principal: &principal,
            resource: &resource,
            filters: &filters,
        };
        let mut view = View {
            thread: &thread,
            current: &message.id,
            issues: scope
                .1
                .iter()
                .filter(|(_, project)| scope.0.contains(*project))
                .map(|(issue, _)| issue.as_str())
                .collect(),
            denials: &denials,
            projection,
        };
        let redact = TokenRedactor::new()?;
        let answer = match (&query, around) {
            (Some(query), _) => {
                let (found, scanned, complete) = self.search_scan(&mut view, query)?;
                let total = found.len();
                let mut hits = Vec::new();
                let mut bytes = 0;
                let mut truncated = false;
                for (score, entry) in found.into_iter().take(limit) {
                    let snippet = query.snippet(&redact.text(&entry.text));
                    if bytes + snippet.len() > search::HITS_BYTES_MAX {
                        truncated = true;
                        break;
                    }
                    bytes += snippet.len();
                    let handle = codec
                        .seal(entry.seq, &binding)
                        .map_err(|_| Error::rejected("thread search could not seal a hit"))?;
                    hits.push(json!({
                        "handle": handle,
                        "role": entry.role,
                        "kind": entry.kind,
                        "created": entry.created,
                        "phrase": score.phrase,
                        "words": score.words,
                        "snippet": snippet,
                    }));
                }
                json!({
                    "hits": hits,
                    "matches": total,
                    "scanned": scanned,
                    "complete": complete,
                    "truncated": truncated || total > hits.len(),
                })
            }
            (None, Some(handle)) => {
                let seq = codec
                    .open(handle, &binding)
                    .map_err(|_| Error::rejected("thread search hit is no longer readable"))?;
                self.search_window(&mut view, seq, before, after, &redact)?
            }
            (None, None) => unreachable!("query or around was required above"),
        };
        // The read scope must still be the one the answer was selected under.
        let current = self.project_scope(peer_pid, PURPOSE)?;
        if current.as_ref().map(|scope| &scope.0) != Some(&scope.0) {
            return Err(Error::rejected(
                "thread search refused: reader access changed",
            ));
        }
        Ok(answer)
    }

    /// One batch of visible, reached entries in `direction` from `anchor`,
    /// after the cloud projection. A projection that needs readmission
    /// mid-read refuses rather than answer from a stale admission.
    fn search_batch(
        &self,
        view: &mut View<'_>,
        anchor: i64,
        backward: bool,
        limit: i64,
    ) -> Result<(Vec<store::ThreadEntry>, Option<i64>, bool)> {
        let (raw, more) = self.store.thread_entries_reached_scoped(
            &view.thread.id,
            view.current,
            anchor,
            backward,
            limit,
            &view.issues,
            view.denials,
        )?;
        if view.projection.needs_readmission(
            &self.store,
            raw.iter().filter_map(|entry| entry.message_id.as_deref()),
        )? {
            return Err(Error::rejected(
                "thread search refused: reader access changed; search again",
            ));
        }
        let boundary = raw.last().map(|entry| entry.seq);
        let visible = view.projection.entries(&self.store, raw)?;
        Ok((visible, boundary, more))
    }

    /// Every match among the newest [`search::SCAN_MAX`] visible entries,
    /// best first: phrase, then words matched, then newest.
    fn search_scan(&self, view: &mut View<'_>, query: &Query) -> Result<(Found, usize, bool)> {
        let mut found = Vec::new();
        let mut scanned = 0usize;
        let mut anchor = i64::MAX;
        let complete = loop {
            let (visible, boundary, more) = self.search_batch(view, anchor, true, BATCH)?;
            let mut skipped = false;
            for entry in visible {
                if scanned == search::SCAN_MAX {
                    skipped = true;
                    break;
                }
                scanned += 1;
                if let Some(score) = query.score(&entry.text) {
                    found.push((score, entry));
                }
            }
            match boundary {
                _ if skipped => break false,
                Some(seq) if more && scanned < search::SCAN_MAX => anchor = seq,
                Some(_) if more => break false,
                _ => break true,
            }
        };
        found.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.seq.cmp(&a.1.seq)));
        Ok((found, scanned, complete))
    }

    /// The hit at `seq` and up to `before`/`after` visible, reached entries
    /// either side, oldest first. A hit that is no longer one of them — its
    /// row hidden by a changed scope, or never one — is refused.
    fn search_window(
        &self,
        view: &mut View<'_>,
        seq: i64,
        before: usize,
        after: usize,
        redact: &TokenRedactor,
    ) -> Result<Value> {
        let (mut older, _, more_before) =
            self.search_batch(view, seq.saturating_add(1), true, before as i64 + 1)?;
        if older.first().map(|entry| entry.seq) != Some(seq) {
            return Err(Error::rejected("thread search hit is no longer readable"));
        }
        older.reverse();
        let (newer, _, more_after) = if after > 0 {
            self.search_batch(view, seq, false, after as i64)?
        } else {
            (Vec::new(), None, false)
        };
        // The hit is charged first, then neighbours nearest first, one
        // side then the other, so the budget never drops the hit itself.
        // A side stops at its first entry that does not fit, so the window
        // never shows a farther entry with a gap before it.
        let hit = older.pop().expect("the hit was checked above");
        let mut bytes = 0;
        let mut truncated = false;
        let mut kept = Vec::new();
        let mut charge = |entry: store::ThreadEntry| -> bool {
            let text = search::clip(&redact.text(&entry.text), search::WINDOW_ENTRY_MAX);
            if bytes + text.len() > search::WINDOW_BYTES_MAX {
                truncated = true;
                return false;
            }
            bytes += text.len();
            kept.push((entry, text));
            true
        };
        charge(hit);
        let mut sides = [
            Some(older.into_iter().rev().collect::<Vec<_>>().into_iter()),
            Some(newer.into_iter()),
        ];
        while sides.iter().any(Option::is_some) {
            for side in sides.iter_mut() {
                let fits = side
                    .as_mut()
                    .and_then(Iterator::next)
                    .is_some_and(&mut charge);
                if !fits {
                    *side = None;
                }
            }
        }
        kept.sort_by_key(|(entry, _)| entry.seq);
        let entries: Vec<Value> = kept
            .into_iter()
            .map(|(entry, text)| {
                json!({
                    "hit": entry.seq == seq,
                    "role": entry.role,
                    "kind": entry.kind,
                    "created": entry.created,
                    "text": text,
                })
            })
            .collect();
        Ok(json!({
            "entries": entries,
            "more_before": more_before,
            "more_after": more_after,
            "truncated": truncated,
        }))
    }
}

/// Live turn tokens quoted in thread prose are redacted on the way out; the
/// store only guarantees that at export.
struct TokenRedactor(regex::Regex);

impl TokenRedactor {
    fn new() -> Result<Self> {
        regex::Regex::new(&crate::adapter::registry::turn_token_pattern())
            .map(Self)
            .map_err(|e| Error::rejected(format!("thread search redaction unavailable: {e}")))
    }

    fn text(&self, text: &str) -> String {
        self.0.replace_all(text, "[redacted]").into_owned()
    }
}
