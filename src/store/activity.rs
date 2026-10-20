//! CAD-1374: the owner-only activity projection. Reads the verified human
//! receipts the store already keeps — core `queued` events, the daemon's
//! `activity_denied` / `activity_export_prepared` receipts, and the per-install
//! app receipts — and projects only safe canonical metadata. Reads are
//! read-only: no table is created, migrated or written, and no directory is
//! enumerated. The company is a trusted daemon value; it is never taken from
//! the request.
//!
//! The captured interval is a time bound, not an atomic multi-database
//! snapshot: each source is read in turn, and `coverage` says so.

use std::cmp::Ordering;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};

use base64::Engine;
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use uuid::Uuid;

use super::{open_read_only, Store};
use crate::error::{Error, Result};
use crate::issue::app_catalog::InstallationId;

pub const SCHEMA_VERSION: i64 = 1;

/// Explorer receipts (CAD-1374) written with a verified human in the same
/// transaction as their state change.
const EXPLORER_RECEIPTS: [&str; 5] = [
    "app_favorites_saved",
    "app_favorites_default_saved",
    "app_install_requested",
    "app_install_request_reopened",
    "app_install_request_dismissed",
];
/// CAD-1374: the daemon-stream receipts of app runs, install capability
/// decisions, workspace effects and public membership revocation. Each is
/// projected only when it is this company's canonical frame.
const APP_FAMILY_KINDS: [&str; 9] = [
    "app_install_capability_approved",
    "app_install_capability_revoked",
    "app_run_created",
    "app_run_execution_approved",
    "app_run_cancelled",
    "app_run_step_dispatched",
    "app_workspace_mutation_prepared",
    "app_workspace_mutation_finished",
    "public_authority_revoked",
];
/// CAD-1374: the daemon's board-session frames. Only these three kinds are
/// projected; the legacy best-effort notices (`board_session_opened` and the
/// like) are not canonical and are never listed.
const SESSION_KINDS: [&str; 3] = [
    "board_session_mutation_prepared",
    "board_session_mutation_finished",
    "board_session_security_denied",
];
/// The exact session actions a mutation frame may name.
const SESSION_MUTATIONS: [&str; 3] = [
    "board_session_opened",
    "board_session_logged_out",
    "board_session_revoked",
];
/// The static pre-auth refusal codes a security denial may name.
const SESSION_DENIAL_REASONS: [&str; 9] = [
    "agent_connection",
    "capability_unavailable",
    "assertion_invalid",
    "assertion_expired",
    "assertion_replayed",
    "issuer_mismatch",
    "audience_mismatch",
    "not_a_member",
    "role_unmapped",
];
const SESSION_REVISION_AVAILABILITY: &str = "session_state_has_no_content_revision";
/// The exact PM mutation actions a prepared intent or a commit receipt may name.
const PM_ACTIONS: [&str; 11] = [
    "issue_created",
    "issue_updated",
    "issue_link_changed",
    "issue_ref_added",
    "issue_comment_added",
    "issue_artifact_added",
    "wiki_written",
    "wiki_blob_uploaded",
    "wiki_directory_created",
    "wiki_moved",
    "wiki_removed",
];
/// Largest `git log` output read for PM receipts. A larger history fails the
/// page rather than returning a silently partial trail.
const PM_GIT_OUTPUT_CAP: usize = 16 * 1024 * 1024;
/// Bounded resource identifier: wide enough for a two-path wiki move.
const RESOURCE_MAX: usize = 8192;
const RECEIPT_TEXT_MAX: usize = 256;

pub const DEFAULT_LIMIT: usize = 100;
pub const MAX_LIMIT: usize = 200;

/// The filters and bounds of one activity query. `company` is the trusted
/// daemon/session value, never a request field.
#[derive(Debug, Clone, Default)]
pub struct ActivityQuery {
    pub company: String,
    pub sub: Option<String>,
    pub action: Option<String>,
    pub target: Option<String>,
    pub outcome: Option<String>,
    pub from: Option<f64>,
    pub until: Option<f64>,
    pub limit: Option<usize>,
    pub cursor: Option<String>,
}

/// A projected receipt row. Only canonical metadata; never a body.
#[derive(Debug, Clone)]
struct Row {
    at: f64,
    event_id: String,
    action: String,
    target: String,
    outcome: String,
    /// The verified human, or `null` (UNKNOWN actor) when none is proven.
    human: Value,
    source: String,
    revision_or_digest: Value,
    /// Explicit receipt metadata of a CAD-1374 explorer write: `reason`,
    /// `request_id`, `correlation_id`, `causation_id`. `null` for every other
    /// source, so legacy rows are never backfilled with invented metadata.
    receipt: Value,
}

impl Row {
    fn key(&self) -> (f64, &str) {
        (self.at, self.event_id.as_str())
    }

    fn to_json(&self) -> Value {
        json!({
            "event_id": self.event_id,
            "at": self.at,
            "action": self.action,
            "target": self.target,
            "outcome": self.outcome,
            "human": self.human,
            "source": self.source,
            "revision_or_digest": self.revision_or_digest,
            "receipt": self.receipt,
        })
    }
}

/// Ascending `(at, event_id)` order: the page order and the cursor order.
fn order(a: &Row, b: &Row) -> Ordering {
    a.at.partial_cmp(&b.at)
        .unwrap_or(Ordering::Equal)
        .then_with(|| a.event_id.cmp(&b.event_id))
}

/// Keep only the `fetch` earliest rows, so one source holds bounded memory.
fn keep_earliest(rows: &mut Vec<Row>, fetch: usize) {
    rows.sort_by(order);
    rows.truncate(fetch);
}

/// Push a row, trimming the buffer to its earliest `fetch` rows whenever it
/// doubles. Nothing is dropped before it could be among the earliest.
fn push_bounded(rows: &mut Vec<Row>, row: Row, fetch: usize) {
    rows.push(row);
    if rows.len() > fetch * 2 {
        keep_earliest(rows, fetch);
    }
}

/// The cursor a page returns. Its bindings must match the query that
/// reads the next page; it selects rows and is never authority.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    v: i64,
    company: String,
    sub: Option<String>,
    action: Option<String>,
    target: Option<String>,
    outcome: Option<String>,
    from: f64,
    until: f64,
    limit: usize,
    last_at: f64,
    last_id: String,
}

/// Metadata a claimed CAD-1374 explorer receipt must carry. A missing or
/// mistyped field fails the page; it is never read as an empty value.
fn explorer_receipt_of(value: &Value) -> Result<Value> {
    if value.get("schema_version").and_then(Value::as_i64) != Some(SCHEMA_VERSION) {
        return Err(Error::internal("activity explorer receipt is malformed"));
    }
    let text = |field: &str| {
        value
            .get(field)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| Error::internal("activity explorer receipt is malformed"))
    };
    let (request_id, correlation_id) = (text("request_id")?, text("correlation_id")?);
    let causation = match value.get("causation_id") {
        Some(Value::Null) => Value::Null,
        _ => return Err(Error::internal("activity explorer receipt is malformed")),
    };
    Ok(json!({
        "reason": text("reason")?,
        "request_id": request_id,
        "correlation_id": correlation_id,
        "causation_id": causation,
    }))
}

/// The digest a claimed new explorer receipt projects, checked by kind. A
/// favorites save carries exactly `sha256:<64 lowercase hex>`; an install
/// request carries an explicit null. Anything else fails the page.
fn explorer_digest(kind: &str, value: &Value) -> Result<Value> {
    let malformed = || Error::internal("activity explorer receipt is malformed");
    match kind {
        "app_favorites_saved" | "app_favorites_default_saved" => {
            let digest = value
                .get("digest")
                .and_then(Value::as_str)
                .ok_or_else(malformed)?;
            let hex = digest.strip_prefix("sha256:").ok_or_else(malformed)?;
            if hex.len() != 64
                || !hex
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                return Err(malformed());
            }
            Ok(json!(digest))
        }
        _ => match value.get("digest") {
            Some(Value::Null) => Ok(Value::Null),
            _ => Err(malformed()),
        },
    }
}

/// The refusal for a stored human that is present but not a verified human of
/// this company. Static: it never carries the stored value.
fn bad_human() -> Error {
    Error::internal("activity human receipt is malformed or names another company")
}

/// Canonical verified human (`company, sub, session, display, role`) from a
/// stored value. Absent or `null` is UNKNOWN (`null`). Anything present that
/// is not a verified human of the trusted company is an error, never a
/// downgrade to UNKNOWN that would expose the foreign target.
fn actor_of(raw: Option<&Value>, company: &str) -> Result<Value> {
    let Some(raw) = raw.filter(|v| !v.is_null()) else {
        return Ok(Value::Null);
    };
    let object = raw.as_object().ok_or_else(bad_human)?;
    let text = |field: &str| object.get(field).and_then(Value::as_str);
    let nonempty = |field: &str| text(field).filter(|s| !s.is_empty());
    let (Some(c), Some(sub), Some(session)) =
        (nonempty("company"), nonempty("sub"), nonempty("session"))
    else {
        return Err(bad_human());
    };
    if c != company {
        return Err(bad_human());
    }
    // A legacy receipt may carry no role; a present role must be a known one.
    let role = match object.get("role") {
        None | Some(Value::Null) => Value::Null,
        Some(Value::String(r)) if r == "operator" || r == "member" => json!(r),
        Some(_) => return Err(bad_human()),
    };
    Ok(json!({
        "sub": sub,
        "company": company,
        "session": session,
        "display": text("display").unwrap_or(""),
        "role": role,
    }))
}

/// [`actor_of`] for a human column stored as JSON text.
fn actor_from_text(raw: Option<&str>, company: &str) -> Result<Value> {
    let Some(text) = raw else {
        return Ok(Value::Null);
    };
    let parsed: Value = serde_json::from_str(text).map_err(|_| bad_human())?;
    actor_of(Some(&parsed), company)
}

fn matches(row: &Row, q: &ActivityQuery) -> bool {
    // An explicit subject filter never matches UNKNOWN, even an empty one.
    q.sub
        .as_deref()
        .is_none_or(|s| row.human["sub"].as_str() == Some(s))
        && q.action.as_deref().is_none_or(|a| a == row.action)
        && q.target.as_deref().is_none_or(|t| t == row.target)
        && q.outcome.as_deref().is_none_or(|o| o == row.outcome)
        && q.from.is_none_or(|f| row.at >= f)
        && q.until.is_none_or(|u| row.at <= u)
}

/// Keep rows strictly after the cursor position in `(at, event_id)` order.
fn after(row: &Row, last: Option<&(f64, String)>) -> bool {
    match last {
        None => true,
        Some((at, id)) => row.key() > (*at, id.as_str()),
    }
}

/// Injective identifier escape: the safe alphabet passes through, every other
/// byte (including `%` and `/`) is percent-encoded. Nothing is truncated.
fn escape(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for byte in raw.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Gaps the coverage block reports: absence is never silently an empty trail.
#[derive(Default)]
struct Gaps {
    missing_record_files: usize,
    absent_receipt_tables: usize,
    receipts_without_actor_column: usize,
}

impl Store {
    /// Every installation the trusted catalog knows, active or archived.
    /// Reads `app_contexts` only: the record file set is never enumerated
    /// from the filesystem.
    fn app_activity_installs(&self) -> Result<Vec<String>> {
        let conn = self.conn();
        let mut stmt =
            conn.prepare("SELECT DISTINCT install_id FROM app_contexts ORDER BY install_id")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// The owner-only activity page: merged across every source, ordered by
    /// `(at, event_id)`, bounded by `limit`. Reads are not one global atomic
    /// snapshot; `coverage` says what was captured.
    pub fn activity_query(&self, state_dir: &Path, query: &ActivityQuery) -> Result<Value> {
        self.activity_page(state_dir, None, query)
    }

    /// The same page, plus the configured PM repo's `Activity-Receipt` commits.
    pub fn activity_query_with_pm(
        &self,
        state_dir: &Path,
        query: &ActivityQuery,
        pm_dir: &Path,
    ) -> Result<Value> {
        self.activity_page(state_dir, Some(pm_dir), query)
    }

    fn activity_page(
        &self,
        state_dir: &Path,
        pm_dir: Option<&Path>,
        query: &ActivityQuery,
    ) -> Result<Value> {
        let limit = query.limit.unwrap_or(DEFAULT_LIMIT);
        if limit == 0 || limit > MAX_LIMIT {
            return Err(Error::rejected(format!(
                "activity limit must be 1-{MAX_LIMIT}"
            )));
        }
        let until_default = super::now();
        let (from, until, last) = match &query.cursor {
            Some(text) => {
                let c = decode_cursor(text, query, limit)?;
                (c.from, c.until, Some((c.last_at, c.last_id)))
            }
            None => (
                query.from.unwrap_or(0.0),
                query.until.unwrap_or(until_default),
                None,
            ),
        };
        if from > until {
            return Err(Error::rejected("activity interval starts after it ends"));
        }
        let effective = ActivityQuery {
            from: Some(from),
            until: Some(until),
            ..query.clone()
        };

        // Each source keeps at most limit+1 earliest eligible rows past the
        // cursor, so the merged page can report whether more rows exist.
        let fetch = limit + 1;
        let mut gaps = Gaps::default();
        let mut rows = self.core_activity_rows(&effective, last.as_ref(), fetch)?;
        app_activity_rows(
            state_dir,
            self,
            &effective,
            last.as_ref(),
            fetch,
            &mut gaps,
            &mut rows,
        )?;
        if let Some(pm_dir) = pm_dir {
            pm_git_rows(pm_dir, &effective, last.as_ref(), fetch, &mut rows)?;
        }
        keep_earliest(&mut rows, fetch);
        rows.dedup_by(|a, b| a.event_id == b.event_id);

        let has_more = rows.len() > limit;
        rows.truncate(limit);
        let next_cursor = if has_more {
            let last = rows.last().expect("page has a row when more exist");
            Some(encode_cursor(&Cursor {
                v: SCHEMA_VERSION,
                company: query.company.clone(),
                sub: query.sub.clone(),
                action: query.action.clone(),
                target: query.target.clone(),
                outcome: query.outcome.clone(),
                from,
                until,
                limit,
                last_at: last.at,
                last_id: last.event_id.clone(),
            })?)
        } else {
            None
        };

        Ok(json!({
            "schema_version": SCHEMA_VERSION,
            "company": query.company,
            "interval": {"from": from, "until": until},
            "events": rows.iter().map(Row::to_json).collect::<Vec<_>>(),
            "row_count": rows.len(),
            "next_cursor": next_cursor,
            // `complete` means this page reached the end of the captured
            // interval; it is not a claim about other sources or writers.
            "complete": next_cursor.is_none(),
            "coverage": {
                "included_sources": [
                    "core events: queued messages, conversation creates and PM prepared intents (this company only), daemon activity_denied and activity_export_prepared",
                    "PM repo HEAD Activity-Receipt trailers (pm_git; read only when a PM repo is given)",
                    "app_record_revisions",
                    "app_content_revisions",
                    "app_record_csv_imports (current receipt state)",
                    "app favorites save and install request create/reopen/dismiss (verified human only)",
                    "app install capability approve/revoke, app run created/approved/cancelled, app run step dispatch and workspace install/remove/restore prepared and finished (this company's canonical receipts only); a dispatch or automatic step is actor_kind system with human null, and initiating_human is metadata only",
                    "public membership revocation prepared intent (public_authority_revoked, system actor, target_user)",
                    "board session mutation prepared, completed and uncertain frames (opened, logged out, revoked; this company's daemon-stream frames only) and pre-auth security denials (no human, empty targets)"
                ],
                "not_yet_covered": [
                    "platform membership and credential lifecycle records from AOS are not integrated (AOS-218)",
                    "session expiry, sweep, open-link and generic HTTP permission guards (only the board session mutations and denials above are listed)",
                    "other approvals and grants beyond the app install, app run and dispatch decisions above",
                    "favorites and install requests saved before CAD-1374 carry no company and are not listed",
                    "legacy app receipts without a company (installer consent, bootstrap consent, the legacy workspace removed notification) are not listed and never assigned an owner",
                    "PM commits without a canonical Activity-Receipt trailer and legacy PM Actor labels are not attributed; legacy history is UNKNOWN",
                    "queued messages and other core rows without a verified human are listed with human null (UNKNOWN actor), never attributed",
                    "reads are not one atomic snapshot across sources; the interval is the captured bound"
                ],
                "gaps": {
                    "missing_record_files": gaps.missing_record_files,
                    "absent_receipt_tables": gaps.absent_receipt_tables,
                    "receipts_without_actor_column": gaps.receipts_without_actor_column,
                },
            },
        }))
    }

    /// Queued messages and the daemon's activity receipts from the events
    /// stream. A stored payload that cannot be read fails the page.
    fn core_activity_rows(
        &self,
        q: &ActivityQuery,
        last: Option<&(f64, String)>,
        fetch: usize,
    ) -> Result<Vec<Row>> {
        let conn = self.conn();
        let (from, until) = (q.from.unwrap_or(0.0), q.until.unwrap_or(0.0));
        let mut stmt = conn.prepare(
            "SELECT seq, alias, kind, payload, at FROM events
             WHERE kind IN ('queued','activity_denied','activity_export_prepared','conversation_created','pm_mutation_prepared',
                            'app_favorites_saved','app_favorites_default_saved',
                            'app_install_requested','app_install_request_reopened',
                            'app_install_request_dismissed',
                            'app_install_capability_approved','app_install_capability_revoked',
                            'app_run_created','app_run_execution_approved','app_run_cancelled','app_run_step_dispatched',
                            'app_workspace_mutation_prepared','app_workspace_mutation_finished','public_authority_revoked',
                            'board_session_mutation_prepared','board_session_mutation_finished','board_session_security_denied')
               AND at>=? AND at<=? ORDER BY at, seq",
        )?;
        let raws = stmt.query_map(params![from, until], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, f64>(4)?,
            ))
        })?;
        let mut out = Vec::new();
        for raw in raws {
            let (seq, alias, kind, payload, at) = raw?;
            let value: Value = serde_json::from_str(&payload)
                .map_err(|_| Error::internal("activity event payload is unreadable"))?;
            let operation = value.get("operation").and_then(Value::as_str).unwrap_or("");
            let company = value.get("company").and_then(Value::as_str);
            if SESSION_KINDS.contains(&kind.as_str()) {
                // Only the daemon stream writes these frames; any other alias
                // carrying the kind is not canonical.
                if alias != Store::DAEMON_STREAM {
                    continue;
                }
                if let Some(row) = session_row(seq, &kind, at, &value, q)? {
                    if matches(&row, q) && after(&row, last) {
                        push_bounded(&mut out, row, fetch);
                    }
                }
                continue;
            }
            if APP_FAMILY_KINDS.contains(&kind.as_str()) {
                // Only the daemon stream writes these receipts; the same kind
                // on any other alias is not a canonical receipt.
                if alias != Store::DAEMON_STREAM {
                    continue;
                }
                if let Some(row) = app_family_row(seq, &kind, at, &value, q)? {
                    if matches(&row, q) && after(&row, last) {
                        push_bounded(&mut out, row, fetch);
                    }
                }
                continue;
            }
            if kind == "conversation_created" || kind == "pm_mutation_prepared" {
                // Only this company's canonical rows. Legacy (unscoped) and
                // foreign rows are skipped, never assigned a tenant.
                if company != Some(q.company.as_str()) {
                    continue;
                }
                let row = pm_scoped_row(seq, &kind, at, &alias, &value, q)?;
                if matches(&row, q) && after(&row, last) {
                    push_bounded(&mut out, row, fetch);
                }
                continue;
            }
            if EXPLORER_RECEIPTS.contains(&kind.as_str()) {
                // Only a verified human of this company is listed. Legacy
                // rows (no company) and other companies' rows are skipped,
                // never guessed or refused.
                if company != Some(q.company.as_str()) {
                    continue;
                }
                // A claimed new receipt must carry its verified human; an
                // absent human is malformed, never a silent UNKNOWN actor.
                if value.get("human").is_none_or(Value::is_null) {
                    return Err(bad_human());
                }
            } else if kind != "queued" && company != Some(q.company.as_str()) {
                return Err(bad_human());
            }
            let field = |name: &str| value.get(name).and_then(Value::as_str).unwrap_or("");
            let request_target = || {
                format!(
                    "catalog:{}/request:{}",
                    escape(field("catalog_id")),
                    escape(field("id"))
                )
            };
            let mut receipt = Value::Null;
            let claimed = EXPLORER_RECEIPTS.contains(&kind.as_str());
            let (action, outcome, target) = match kind.as_str() {
                "queued" => {
                    let target = match value.get("message").and_then(Value::as_str) {
                        Some(message) => {
                            format!("alias:{}/message:{}", escape(&alias), escape(message))
                        }
                        None => format!("alias:{}", escape(&alias)),
                    };
                    ("message_queued", "queued", target)
                }
                "app_favorites_saved" => {
                    receipt = explorer_receipt_of(&value)?;
                    (
                        "favorites_saved",
                        "saved",
                        format!("favorites:{}", escape(field("owner"))),
                    )
                }
                "app_favorites_default_saved" => {
                    receipt = explorer_receipt_of(&value)?;
                    (
                        "favorites_default_saved",
                        "saved",
                        "favorites:default".to_string(),
                    )
                }
                "app_install_requested" => {
                    receipt = explorer_receipt_of(&value)?;
                    ("install_request_created", "open", request_target())
                }
                "app_install_request_reopened" => {
                    receipt = explorer_receipt_of(&value)?;
                    ("install_request_reopened", "open", request_target())
                }
                "app_install_request_dismissed" => {
                    receipt = explorer_receipt_of(&value)?;
                    ("install_request_dismissed", "dismissed", request_target())
                }
                "activity_denied" => (
                    "activity_denied",
                    "denied",
                    format!("operation:{}", escape(operation)),
                ),
                _ => (
                    "activity_export_prepared",
                    "prepared",
                    format!("operation:{}", escape(operation)),
                ),
            };
            let row = Row {
                at,
                event_id: format!("event-{seq:020}"),
                action: action.to_string(),
                target,
                outcome: outcome.to_string(),
                human: actor_of(value.get("human"), &q.company)?,
                source: "core".to_string(),
                revision_or_digest: if claimed {
                    explorer_digest(&kind, &value)?
                } else {
                    Value::Null
                },
                receipt,
            };
            if matches(&row, q) && after(&row, last) {
                push_bounded(&mut out, row, fetch);
            }
        }
        keep_earliest(&mut out, fetch);
        Ok(out)
    }
}

fn app_activity_rows(
    state_dir: &Path,
    store: &Store,
    q: &ActivityQuery,
    last: Option<&(f64, String)>,
    fetch: usize,
    gaps: &mut Gaps,
    out: &mut Vec<Row>,
) -> Result<()> {
    for install in store.app_activity_installs()? {
        let path = crate::store::app_records::record_db_path(state_dir, &install)?;
        // Never create or migrate a record file from a read. A catalog
        // install with no file is reported as a coverage gap.
        if !path.is_file() {
            gaps.missing_record_files += 1;
            continue;
        }
        let conn = open_read_only(&path)
            .map_err(|_| Error::internal("activity record store is unreadable"))?;
        install_rows(&conn, &install, q, last, fetch, gaps, out)?;
    }
    Ok(())
}

/// Column names of a table; empty when the table is absent.
fn table_columns(conn: &Connection, table: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let names = stmt.query_map([], |r| r.get::<_, String>(1))?;
    Ok(names.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn install_rows(
    conn: &Connection,
    install: &str,
    q: &ActivityQuery,
    last: Option<&(f64, String)>,
    fetch: usize,
    gaps: &mut Gaps,
    out: &mut Vec<Row>,
) -> Result<()> {
    let (from, until) = (q.from.unwrap_or(0.0), q.until.unwrap_or(0.0));
    // (table, context column, resource column, revision column, digest column,
    //  action, resource label, outcome)
    let sources = [
        (
            "app_record_revisions",
            "context_id",
            "record_id",
            "revision",
            "body_digest",
            "record_saved",
            "record",
            "saved",
        ),
        (
            "app_content_revisions",
            "context_id",
            "campaign_id",
            "revision",
            "content_digest",
            "content_saved",
            "campaign",
            "saved",
        ),
        (
            "app_record_csv_imports",
            "context_id",
            "request_id",
            "NULL",
            "decisions_digest",
            "csv_import",
            "import",
            "receipt_state",
        ),
    ];
    for (table, context_col, resource_col, revision_col, digest_col, action, label, outcome) in
        sources
    {
        let columns = table_columns(conn, table)?;
        if columns.is_empty() {
            // A table absent in an older file has no receipts from it.
            gaps.absent_receipt_tables += 1;
            continue;
        }
        let has_human = columns.iter().any(|c| c == "human");
        if !has_human {
            // Pre-CAD-1374 receipts: no actor is known, so none is guessed.
            gaps.receipts_without_actor_column += 1;
        }
        let human_col = if has_human { "human" } else { "NULL" };
        let sql = format!(
            "SELECT {context_col}, {resource_col}, {revision_col}, {digest_col}, at, {human_col}
             FROM {table} WHERE at>=? AND at<=?"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params![from, until], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, rusqlite::types::Value>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, f64>(4)?,
                r.get::<_, Option<String>>(5)?,
            ))
        })?;
        for raw in rows {
            let (context, resource, revision, digest, at, human_raw) = raw?;
            let revision = match revision {
                rusqlite::types::Value::Integer(n) => json!(n),
                _ => Value::Null,
            };
            // Collision-free: the full stored tuple, not a lossy sanitized id.
            let key = serde_json::to_vec(&json!([table, install, context, resource, revision]))
                .map_err(|e| Error::internal(e.to_string()))?;
            let row = Row {
                at,
                event_id: format!(
                    "app-{}",
                    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(key)
                ),
                action: action.to_string(),
                target: format!(
                    "context:{}/{}/{}:{}",
                    escape(install),
                    escape(&context),
                    label,
                    escape(&resource)
                ),
                outcome: outcome.to_string(),
                human: actor_from_text(human_raw.as_deref(), &q.company)?,
                source: format!("app:{table}"),
                revision_or_digest: json!({
                    "resource": resource,
                    "revision": revision,
                    "digest": digest,
                }),
                receipt: Value::Null,
            };
            if matches(&row, q) && after(&row, last) {
                push_bounded(out, row, fetch);
            }
        }
    }
    Ok(())
}

fn pm_action(raw: Option<&str>) -> Option<&'static str> {
    PM_ACTIONS.iter().copied().find(|a| Some(*a) == raw)
}

/// Non-empty, control-free text of at most `max` bytes.
fn bounded_text(raw: Option<&str>, max: usize) -> Option<&str> {
    raw.filter(|s| (1..=max).contains(&s.len()) && !s.chars().any(char::is_control))
}

/// A UUID, returned in canonical lowercase hyphenated form.
fn uuid_text(raw: Option<&str>) -> Option<String> {
    raw.and_then(|s| Uuid::parse_str(s).ok())
        .map(|u| u.to_string())
}

fn is_sha256_digest(text: &str) -> bool {
    text.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

/// A claimed conversation or PM prepared row of this company. Every field
/// must be canonical; a malformed claimed row fails the page.
fn pm_scoped_row(
    seq: i64,
    kind: &str,
    at: f64,
    alias: &str,
    value: &Value,
    q: &ActivityQuery,
) -> Result<Row> {
    let malformed = || Error::internal("activity scoped receipt is malformed");
    let str_of = |field: &str| value.get(field).and_then(Value::as_str);
    if value.get("schema_version").and_then(Value::as_i64) != Some(SCHEMA_VERSION)
        || value.get("causation_id") != Some(&Value::Null)
        || value
            .get("revision_or_digest")
            .is_some_and(|v| !v.is_null())
    {
        return Err(malformed());
    }
    let request = uuid_text(str_of("request_id")).ok_or_else(malformed)?;
    let correlation = uuid_text(str_of("correlation_id")).ok_or_else(malformed)?;
    if request != correlation {
        return Err(malformed());
    }
    let reason = bounded_text(str_of("reason"), RECEIPT_TEXT_MAX).ok_or_else(malformed)?;
    let human = actor_of(value.get("human"), &q.company)?;
    if human.is_null() {
        return Err(malformed());
    }
    let (action, outcome, target) = if kind == "conversation_created" {
        if str_of("action") != Some("conversation_created") || str_of("outcome") != Some("created")
        {
            return Err(malformed());
        }
        let thread = bounded_text(str_of("thread"), RECEIPT_TEXT_MAX).ok_or_else(malformed)?;
        (
            "conversation_created",
            "created",
            format!("alias:{}/conversation:{}", escape(alias), escape(thread)),
        )
    } else {
        if str_of("outcome") != Some("prepared")
            || value
                .get("at")
                .and_then(Value::as_f64)
                .is_none_or(|n| !n.is_finite())
        {
            return Err(malformed());
        }
        let action = pm_action(str_of("action")).ok_or_else(malformed)?;
        let resource = bounded_text(str_of("resource"), RESOURCE_MAX).ok_or_else(malformed)?;
        (action, "prepared", resource.to_string())
    };
    Ok(Row {
        at,
        event_id: format!("event-{seq:020}"),
        action: action.to_string(),
        target,
        outcome: outcome.to_string(),
        human,
        source: "core".to_string(),
        revision_or_digest: Value::Null,
        receipt: json!({
            "reason": reason,
            "request_id": request,
            "correlation_id": correlation,
            "causation_id": null,
        }),
    })
}

/// PM receipts: the single `Activity-Receipt` trailer in each commit's closing
/// trailer block, for commits reachable from the configured PM repo's HEAD.
/// Only trailer values are read; commit bodies and paths are never projected.
fn pm_git_rows(
    pm_dir: &Path,
    q: &ActivityQuery,
    last: Option<&(f64, String)>,
    fetch: usize,
    out: &mut Vec<Row>,
) -> Result<()> {
    let unreadable = || Error::internal("activity PM history is unreadable");
    let mut cmd = Command::new("git");
    cmd.arg("-C")
        .arg(pm_dir)
        .args([
            "log",
            "--no-color",
            "--format=%H%x1f%(trailers:key=Activity-Receipt,valueonly,separator=%x1e)%x00",
        ])
        .arg("HEAD")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = crate::reaper::spawn(&mut cmd).map_err(|_| unreadable())?;
    let mut stdout = child.stdout.take().ok_or_else(unreadable)?;
    let mut bytes = Vec::new();
    let read = stdout
        .by_ref()
        .take((PM_GIT_OUTPUT_CAP + 1) as u64)
        .read_to_end(&mut bytes);
    if read.is_err() || bytes.len() > PM_GIT_OUTPUT_CAP {
        let _ = child.kill();
        let _ = child.wait();
        return Err(unreadable());
    }
    if !child.wait().map_err(|_| unreadable())?.success() {
        return Err(unreadable());
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| unreadable())?;
    for record in text.split('\0') {
        let record = record.trim_start_matches('\n');
        if record.is_empty() {
            continue;
        }
        let (sha, trailers) = record.split_once('\x1f').ok_or_else(unreadable)?;
        let claims: Vec<&str> = trailers.split('\x1e').filter(|v| !v.is_empty()).collect();
        if claims.is_empty() {
            continue;
        }
        if claims.len() != 1 {
            return Err(Error::internal(
                "activity PM receipt is duplicated or malformed",
            ));
        }
        let receipt: Value = serde_json::from_str(claims[0])
            .map_err(|_| Error::internal("activity PM receipt is malformed"))?;
        if !receipt.is_object() {
            return Err(Error::internal("activity PM receipt is malformed"));
        }
        // Unscoped and foreign receipts are skipped, never assigned a tenant.
        match receipt.get("company").and_then(Value::as_str) {
            Some(company) if company == q.company.as_str() => {}
            _ => continue,
        }
        let row = pm_receipt_row(sha, &receipt, q)?;
        if matches(&row, q) && after(&row, last) {
            push_bounded(out, row, fetch);
        }
    }
    Ok(())
}

/// One canonical PM receipt of this company, committed at `sha`.
fn pm_receipt_row(sha: &str, receipt: &Value, q: &ActivityQuery) -> Result<Row> {
    let malformed = || Error::internal("activity PM receipt is malformed");
    let str_of = |field: &str| receipt.get(field).and_then(Value::as_str);
    if receipt.get("schema_version").and_then(Value::as_i64) != Some(SCHEMA_VERSION)
        || str_of("outcome") != Some("committed")
        || receipt.get("causation_id") != Some(&Value::Null)
        || !str_of("digest").is_some_and(is_sha256_digest)
    {
        return Err(malformed());
    }
    let action = pm_action(str_of("action")).ok_or_else(malformed)?;
    let resource = bounded_text(str_of("resource"), RESOURCE_MAX).ok_or_else(malformed)?;
    let reason = bounded_text(str_of("reason"), RECEIPT_TEXT_MAX).ok_or_else(malformed)?;
    let event_id = uuid_text(str_of("event_id")).ok_or_else(malformed)?;
    let request = uuid_text(str_of("request_id")).ok_or_else(malformed)?;
    let correlation = uuid_text(str_of("correlation_id")).ok_or_else(malformed)?;
    if request != correlation {
        return Err(malformed());
    }
    let at = receipt
        .get("at")
        .and_then(Value::as_f64)
        .filter(|n| n.is_finite())
        .ok_or_else(malformed)?;
    // A human without a role is not a canonical receipt human.
    let human = actor_of(
        receipt
            .get("human")
            .filter(|h| h.get("role").and_then(Value::as_str).is_some()),
        &q.company,
    )?;
    if human.is_null() {
        return Err(malformed());
    }
    Ok(Row {
        at,
        event_id: format!("pm-git:{event_id}"),
        action: action.to_string(),
        target: resource.to_string(),
        outcome: "committed".to_string(),
        human,
        source: "pm_git".to_string(),
        revision_or_digest: json!(sha),
        receipt: json!({
            "reason": reason,
            "request_id": request,
            "correlation_id": correlation,
            "causation_id": null,
        }),
    })
}

/// The exact (action, outcome, resource prefix) a producer writes for `kind`,
/// with its reason. Anything else is malformed, never a generic action.
fn family_matches(kind: &str, action: &str, outcome: &str, resource: &str, reason: &str) -> bool {
    let (outcomes, prefix): (&[&str], &str) = match (kind, action) {
        ("app_install_capability_approved", "app_install_capability_approved") => {
            (&["approved"], "install:")
        }
        ("app_install_capability_revoked", "app_install_capability_revoked") => {
            (&["revoked"], "install:")
        }
        ("app_run_created", "app_run_created") => (&["created"], "app-run:"),
        ("app_run_execution_approved", "app_run_approved") => (&["approved"], "app-run:"),
        ("app_run_cancelled", "app_run_cancelled") => (&["cancelled"], "app-run:"),
        ("app_run_step_dispatched", "app_run_step_dispatched") => {
            (&["prepared", "dispatched"], "app-run:")
        }
        ("public_authority_revoked", "public_authority_revoked") => (&["prepared"], "session:"),
        ("app_workspace_mutation_prepared", "app_workspace_installed") => {
            (&["prepared"], "catalog:")
        }
        ("app_workspace_mutation_prepared", "app_workspace_removed" | "app_workspace_restored") => {
            (&["prepared"], "install:")
        }
        ("app_workspace_mutation_finished", "app_workspace_installed") => {
            (&["installed", "uncertain", "refused"], "catalog:")
        }
        ("app_workspace_mutation_finished", "app_workspace_removed") => (
            &["removed", "already_removed", "uncertain", "refused"],
            "install:",
        ),
        ("app_workspace_mutation_finished", "app_workspace_restored") => (
            &[
                "restored",
                "already_live",
                "already_live_consent_recorded",
                "uncertain",
                "refused",
            ],
            "install:",
        ),
        _ => return false,
    };
    let reason_ok = if kind == "public_authority_revoked" {
        reason == "membership_withdrawn"
    } else {
        reason == action
    };
    outcomes.contains(&outcome) && resource.starts_with(prefix) && reason_ok
}

/// CAD-1374: one app-run, install-capability, workspace or public revocation
/// receipt of this company. A row with no company is legacy and is skipped,
/// never assigned a tenant or a later owner. A claimed row of this company that
/// fails validation fails the page. A dispatch names its performer (`human`,
/// or `system` with a null human); the sealed approver stays a separate
/// `initiating_human` and is never the performing human.
fn app_family_row(
    seq: i64,
    kind: &str,
    at: f64,
    value: &Value,
    q: &ActivityQuery,
) -> Result<Option<Row>> {
    let malformed = || Error::internal("activity app receipt is malformed");
    let str_of = |field: &str| value.get(field).and_then(Value::as_str);
    if str_of("company") != Some(q.company.as_str()) {
        return Ok(None);
    }
    if value.get("schema_version").and_then(Value::as_i64) != Some(SCHEMA_VERSION)
        || value.get("causation_id") != Some(&Value::Null)
        || value
            .get("at")
            .is_some_and(|v| v.as_f64().is_none_or(|n| !n.is_finite()))
    {
        return Err(malformed());
    }
    let request = uuid_text(str_of("request_id")).ok_or_else(malformed)?;
    if uuid_text(str_of("correlation_id")).as_deref() != Some(request.as_str()) {
        return Err(malformed());
    }
    let action = str_of("action").ok_or_else(malformed)?;
    let outcome = str_of("outcome").ok_or_else(malformed)?;
    let resource = bounded_text(str_of("resource"), RESOURCE_MAX).ok_or_else(malformed)?;
    let reason = bounded_text(str_of("reason"), RECEIPT_TEXT_MAX).ok_or_else(malformed)?;
    if !family_matches(kind, action, outcome, resource, reason) {
        return Err(malformed());
    }
    let workspace = kind.starts_with("app_workspace_");
    let revision_or_digest = match value.get("revision_or_digest") {
        None if workspace => return Err(malformed()),
        None | Some(Value::Null) => Value::Null,
        Some(Value::String(d)) if is_sha256_digest(d) => json!(d),
        Some(_) => return Err(malformed()),
    };
    // A workspace receipt states why it has no digest. A valid digest states
    // none; a null digest must carry the exact literal for its outcome. The
    // literal is validated and projected, never a copied payload value.
    let unavailable = if workspace {
        match (
            revision_or_digest.is_null(),
            value.get("revision_or_digest_unavailable_reason"),
        ) {
            (false, Some(Value::Null)) => None,
            (true, Some(Value::String(stated))) => {
                let expected = match outcome {
                    "prepared" => "not_available_before_effect",
                    "refused" | "uncertain" => "outcome_not_confirmed",
                    "installed"
                    | "removed"
                    | "already_removed"
                    | "restored"
                    | "already_live"
                    | "already_live_consent_recorded" => "operation_result_has_no_digest",
                    _ => return Err(malformed()),
                };
                if stated.as_str() != expected {
                    return Err(malformed());
                }
                Some(expected)
            }
            _ => return Err(malformed()),
        }
    } else {
        None
    };
    // Only dispatch and revocation name an actor kind. A human is never
    // inferred for a system actor, and an initiator is never the performer.
    let explicit = matches!(kind, "app_run_step_dispatched" | "public_authority_revoked");
    let actor_kind = str_of("actor_kind");
    let human = actor_of(value.get("human"), &q.company)?;
    let actor_ok = match actor_kind {
        None => !explicit && !human.is_null(),
        Some("human") => kind == "app_run_step_dispatched" && !human.is_null(),
        Some("system") => explicit && human.is_null(),
        Some(_) => false,
    };
    if !actor_ok {
        return Err(malformed());
    }
    let mut receipt = json!({
        "reason": reason,
        "request_id": request,
        "correlation_id": request,
        "causation_id": null,
    });
    if let Some(kind_text) = actor_kind {
        receipt["actor_kind"] = json!(kind_text);
    }
    if kind == "app_run_step_dispatched" {
        receipt["initiating_human"] = actor_of(value.get("initiating_human"), &q.company)?;
    }
    if kind == "public_authority_revoked" {
        let target = bounded_text(str_of("target_user"), RECEIPT_TEXT_MAX).ok_or_else(malformed)?;
        receipt["target_user"] = json!(target);
    }
    if let Some(reason) = unavailable {
        receipt["unavailable_reason"] = json!(reason);
    }
    if kind.starts_with("app_workspace_") {
        if let Some(install) = str_of("install_id") {
            InstallationId::parse(install).map_err(|_| malformed())?;
            receipt["install_id"] = json!(install);
        }
    }
    Ok(Some(Row {
        at,
        event_id: format!("event-{seq:020}"),
        action: action.to_string(),
        target: resource.to_string(),
        outcome: outcome.to_string(),
        human,
        source: "core".to_string(),
        revision_or_digest,
        receipt,
    }))
}

/// CAD-1374: one board-session mutation or pre-auth denial frame of this
/// company, from the daemon stream. A frame with no company or another company
/// is skipped, never assigned a tenant. A claimed frame of this company that
/// fails validation fails the page.
fn session_row(
    seq: i64,
    kind: &str,
    at: f64,
    value: &Value,
    q: &ActivityQuery,
) -> Result<Option<Row>> {
    let malformed = || Error::internal("activity session receipt is malformed");
    let str_of = |field: &str| value.get(field).and_then(Value::as_str);
    if str_of("company") != Some(q.company.as_str()) {
        return Ok(None);
    }
    if value.get("schema_version").and_then(Value::as_i64) != Some(SCHEMA_VERSION)
        || value.get("causation_id") != Some(&Value::Null)
        || value.get("revision_or_digest") != Some(&Value::Null)
        || str_of("revision_availability") != Some(SESSION_REVISION_AVAILABILITY)
        || !value
            .get("at")
            .and_then(Value::as_f64)
            .is_some_and(f64::is_finite)
    {
        return Err(malformed());
    }
    let request = uuid_text(str_of("request_id")).ok_or_else(malformed)?;
    if uuid_text(str_of("correlation_id")).as_deref() != Some(request.as_str()) {
        return Err(malformed());
    }
    let action = str_of("action").ok_or_else(malformed)?;
    let outcome = str_of("outcome").ok_or_else(malformed)?;
    let resource = bounded_text(str_of("resource"), RESOURCE_MAX).ok_or_else(malformed)?;
    let reason = bounded_text(str_of("reason"), RECEIPT_TEXT_MAX).ok_or_else(malformed)?;
    let actor_kind = str_of("actor_kind").ok_or_else(malformed)?;
    let agent = match value.get("agent") {
        None | Some(Value::Null) => None,
        Some(Value::String(a)) => {
            Some(bounded_text(Some(a.as_str()), RECEIPT_TEXT_MAX).ok_or_else(malformed)?)
        }
        Some(_) => return Err(malformed()),
    };

    let denied = kind == "board_session_security_denied";
    let frame_ok = if denied {
        action == "board_session_security_denied"
            && outcome == "refused"
            && resource == "board:session_exchange"
            && SESSION_DENIAL_REASONS.contains(&reason)
    } else {
        SESSION_MUTATIONS.contains(&action)
            && session_reason_ok(action, reason)
            && match kind {
                "board_session_mutation_prepared" => outcome == "prepared",
                _ => matches!(outcome, "completed" | "uncertain"),
            }
    };
    if !frame_ok {
        return Err(malformed());
    }

    let raw_targets = value
        .get("targets")
        .and_then(Value::as_array)
        .ok_or_else(malformed)?;
    let mut sessions = Vec::with_capacity(raw_targets.len());
    let mut projected = Vec::with_capacity(raw_targets.len());
    for raw in raw_targets {
        let (session, target) = session_target(raw, &q.company)?;
        sessions.push(session);
        projected.push(target);
    }
    if denied {
        if !raw_targets.is_empty() {
            return Err(malformed());
        }
    } else {
        let expected = match sessions.as_slice() {
            [one] => format!("session:{one}"),
            _ => "board:sessions".to_string(),
        };
        let single = matches!(action, "board_session_opened" | "board_session_logged_out");
        if sessions.is_empty() || resource != expected || (single && sessions.len() != 1) {
            return Err(malformed());
        }
    }

    // The acting human is the verified human only; `role_basis` is kept as
    // receipt metadata so a snapshot is never read as fresh privilege.
    if value.get("human").is_none() {
        return Err(malformed());
    }
    let mut human = actor_of(value.get("human"), &q.company)?;
    let role_basis = if human.is_null() {
        Value::Null
    } else if human["role"].is_null()
        || value["human"]["role_basis"].as_str() != Some("signed_session_snapshot")
    {
        return Err(malformed());
    } else {
        json!("signed_session_snapshot")
    };
    let by_agent = action == "board_session_revoked" && reason == "presented_by_agent";
    let revoke = action == "board_session_revoked";
    let actor_ok = match actor_kind {
        "human" => {
            !denied
                && !revoke
                && !human.is_null()
                && agent.is_none()
                && projected.len() == 1
                && human["sub"] == projected[0]["sub"]
                && human["session"] == projected[0]["session"]
                && human["role"] == projected[0]["role"]
        }
        "agent" => {
            agent.is_some()
                && human.is_null()
                && (denied || by_agent || action == "board_session_logged_out")
        }
        "operator" => {
            revoke
                && reason != "upload_account_setup_compensation"
                && !by_agent
                && agent.is_none()
                && human.is_null()
        }
        "system" => {
            action == "board_session_revoked"
                && reason == "upload_account_setup_compensation"
                && agent.is_none()
                && human.is_null()
        }
        "unknown" => {
            agent.is_none()
                && human.is_null()
                && (denied || by_agent || action == "board_session_logged_out")
        }
        _ => false,
    };
    if !actor_ok {
        return Err(malformed());
    }
    if !human.is_null() {
        human["display"] = json!("");
    }

    Ok(Some(Row {
        at,
        event_id: format!("event-{seq:020}"),
        action: action.to_string(),
        target: resource.to_string(),
        outcome: outcome.to_string(),
        human,
        source: "core".to_string(),
        revision_or_digest: Value::Null,
        receipt: json!({
            "reason": reason,
            "request_id": request,
            "correlation_id": request,
            "causation_id": null,
            "actor_kind": actor_kind,
            "agent": agent,
            "role_basis": role_basis,
            "targets": projected,
            "revision_availability": SESSION_REVISION_AVAILABILITY,
        }),
    }))
}

/// The exact reason each session mutation action may carry.
fn session_reason_ok(action: &str, reason: &str) -> bool {
    match action {
        "board_session_opened" => reason == "platform_sign_in",
        "board_session_logged_out" => reason == "session_logout",
        "board_session_revoked" => matches!(
            reason,
            "presented_by_agent"
                | "operator_revoke"
                | "operator_revoke_all"
                | "operator_secret_rotate"
                | "operator_device_login_retired"
                | "upload_account_setup_compensation"
        ),
        _ => false,
    }
}

/// One projected target of a board-session frame: a public session reference,
/// its bounded subject, role and signed-snapshot basis, all of the frame's
/// company. Anything else (a foreign company, a local origin) is malformed.
fn session_target(raw: &Value, company: &str) -> Result<(String, Value)> {
    let malformed = || Error::internal("activity session receipt is malformed");
    let str_of = |field: &str| raw.get(field).and_then(Value::as_str);
    let session = str_of("session")
        .filter(|s| {
            s.len() == 8
                && s.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
        .ok_or_else(malformed)?;
    let role = match str_of("role") {
        Some(r) if r == "operator" || r == "member" => r,
        _ => return Err(malformed()),
    };
    let sub = bounded_text(str_of("sub"), 128).ok_or_else(malformed)?;
    if str_of("origin") != Some("public")
        || str_of("role_basis") != Some("signed_session_snapshot")
        || str_of("company") != Some(company)
    {
        return Err(malformed());
    }
    Ok((
        session.to_string(),
        json!({
            "session": session,
            "origin": "public",
            "sub": sub,
            "role": role,
            "role_basis": "signed_session_snapshot",
            "company": company,
        }),
    ))
}

fn encode_cursor(cursor: &Cursor) -> Result<String> {
    let bytes = serde_json::to_vec(cursor).map_err(|e| Error::internal(e.to_string()))?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
}

/// A cursor is selection only: it must decode and match the query's company,
/// filters, limit and captured interval, or the page is refused. An explicit
/// `from`/`until` that differs from the frozen cursor is refused too.
fn decode_cursor(text: &str, q: &ActivityQuery, limit: usize) -> Result<Cursor> {
    let bad = || Error::rejected("activity cursor is malformed or does not match this query");
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(text)
        .map_err(|_| bad())?;
    let cursor: Cursor = serde_json::from_slice(&bytes).map_err(|_| bad())?;
    let same = cursor.v == SCHEMA_VERSION
        && cursor.company == q.company
        && cursor.sub == q.sub
        && cursor.action == q.action
        && cursor.target == q.target
        && cursor.outcome == q.outcome
        && cursor.limit == limit
        && q.from.is_none_or(|f| f == cursor.from)
        && q.until.is_none_or(|u| u == cursor.until)
        && cursor.from.is_finite()
        && cursor.until.is_finite()
        && cursor.last_at.is_finite()
        && cursor.from <= cursor.until
        && cursor.last_at >= cursor.from
        && cursor.last_at <= cursor.until
        && !cursor.last_id.is_empty();
    if !same {
        return Err(bad());
    }
    Ok(cursor)
}
