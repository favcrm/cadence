//! CAD-2215: opaque job-event pages for every reader that is not a proven
//! operator.
//!
//! A scoped reader continues a job's feed with an encrypted position instead
//! of the global `events.seq`. The position is bound (see
//! [`super::scoped_cursor`]) to:
//!
//! * the domain `job-events` and the forward direction,
//! * the reader's durable principal identity — never a request claim, never
//!   the board connection that relayed the call,
//! * the exact stored job identity — its primary-key id plus the stored
//!   `created` timestamp the token was minted under — so the reproof detects a
//!   change to either. `created` is a REAL `now()` timestamp, not a uniqueness
//!   token, and no production job-deletion path exists today; the binding pins
//!   exactly what the store holds,
//! * the codec's workspace namespace.
//!
//! Every visible row also carries a stable keyed `id`. It is derived from the
//! workspace, the domain, the job identity and the event's own sequence — not
//! from the key that seals cursors — so an ordinary daemon restart or an
//! active-key rotation leaves it unchanged.
//!
//! A token hides the numeric position but it is not authority: the handlers
//! re-prove the reader, the project scope, the private-job boundary and the
//! job itself on every request, before and after selection. Missing key
//! material always refuses a scoped read; nothing here downgrades to numeric
//! positions.

use std::fmt::Write as _;

use serde_json::{json, Value};

use super::identity::PublicReadPrincipal;
use super::scoped_cursor::{CursorBinding, CursorDirection, ScopedCursorCodec};
use super::Shared;
use crate::error::{Error, Result};
use crate::store;

/// The one scoped cursor format this daemon speaks.
pub(super) const FORMAT: &str = cadence_core::board_api::JOB_EVENTS_CURSOR_FORMAT;

/// The canonical cursor domain of a job-event feed.
const CURSOR_DOMAIN: &str = "job-events";

/// The semantic filter text bound into a job-event cursor. `job_events` has
/// no filter dimension — the job (the resource), the direction and the reader
/// are the whole query — so the canonical value is empty.
const CURSOR_FILTERS: &str = "";

/// The verified reader of one native `job_events` request.
///
/// `principal` is `None` only for an operator-authorized reader (a proven
/// operator connection, an operator-role board session, or an operator-proven
/// `read_as`); every other reader carries the opaque identity its cursor is
/// bound to.
pub(super) struct VerifiedJobEventReader {
    pub scope: Option<super::identity::ProjectScope>,
    /// Whether the request named its reader through a relay proof.
    pub relayed: bool,
    pub principal: Option<String>,
    /// The relay principals the request actually proved, kept so the caller
    /// can re-prove the same private-job boundary after selection.
    pub cli: Option<super::cli_rpc::CliPrincipal>,
    pub public: Option<PublicReadPrincipal>,
}

impl VerifiedJobEventReader {
    /// Two resolutions describe the same authority. Relay principals are
    /// deliberately not compared: they are re-verified on every resolution
    /// and the scope/principal they produce is the decision.
    pub(super) fn same_authority(&self, other: &Self) -> bool {
        self.scope == other.scope
            && self.relayed == other.relayed
            && self.principal == other.principal
    }
}

/// The `cursor_format` a request named, or `None` when it named none.
pub(super) fn requested_format<'a>(params: &'a Value, verb: &str) -> Result<Option<&'a str>> {
    match params.get("cursor_format") {
        None => Ok(None),
        Some(Value::String(format)) => Ok(Some(format.as_str())),
        Some(_) => Err(Error::invalid(
            "job_events_cursor_format",
            format!("{verb} refused: cursor_format must be \"{FORMAT}\""),
        )),
    }
}

/// The `after` a request named, before any interpretation. A present value is
/// an opaque token; every other shape is kept distinguishable so a handler
/// refuses it instead of coercing it to a position.
pub(super) enum SuppliedAfter<'a> {
    Absent,
    Text(&'a str),
    Number,
    /// `null`, a boolean, an array or an object.
    Unsupported,
}

pub(super) fn supplied_after(params: &Value) -> SuppliedAfter<'_> {
    match params.get("after") {
        None => SuppliedAfter::Absent,
        Some(Value::String(token)) => SuppliedAfter::Text(token),
        Some(Value::Number(_)) => SuppliedAfter::Number,
        Some(_) => SuppliedAfter::Unsupported,
    }
}

pub(super) fn format_required(verb: &str) -> Error {
    Error::invalid(
        "job_events_cursor_format",
        format!("{verb} refused: cursor_format \"{FORMAT}\" is required for a scoped reader"),
    )
}

pub(super) fn numeric_cursor(verb: &str) -> Error {
    Error::invalid(
        "job_events_cursor_numeric",
        format!(
            "{verb} refused: a scoped reader must send an opaque cursor, \
             never a numeric position"
        ),
    )
}

pub(super) fn invalid_after(verb: &str) -> Error {
    Error::invalid(
        "job_events_cursor_invalid",
        format!("{verb} refused: after must be an opaque cursor string"),
    )
}

fn invalid_cursor(verb: &str) -> Error {
    Error::invalid(
        "job_events_cursor_invalid",
        format!("{verb} refused: the cursor is not valid for this reader and job"),
    )
}

/// A length-framed, injective encoding of one identifier tuple. Each field is
/// `<decimal byte length>:<bytes>`, concatenated, so a separator inside a
/// field can never make two different tuples encode identically. The encoding
/// is only ever compared as opaque text — nothing parses it — but the codec
/// authenticates it, so injectivity is what keeps two readers or two jobs
/// apart.
pub(super) fn framed(fields: &[&str]) -> String {
    let mut out = String::with_capacity(fields.iter().map(|field| field.len() + 21).sum());
    framed_into(&mut out, fields);
    out
}

/// Append one length-framed tuple to `out`, formatting the length straight
/// into the destination — no per-field `String`.
fn framed_into(out: &mut String, fields: &[&str]) {
    for field in fields {
        // Writing to a `String` cannot fail.
        let _ = write!(out, "{}:{}", field.len(), field);
    }
}

/// The resource a job-event token is bound to: the stored job id and the
/// stored `created` timestamp, kept bit-exact so the SQLite REAL round trip
/// cannot move it. `create_job` validates the id as a lowercase identifier and
/// enforces it as the primary key, and no production path deletes a job.
/// `created` is a REAL `now()` timestamp, not a uniqueness token — equality is
/// possible — so this names the identity the store holds, not an invented
/// incarnation; the reproof refuses if either value changes. The tuple is
/// length-framed anyway, so a row written by another path (or a future id
/// grammar) can never alias another job's binding.
pub(super) fn job_resource(job: &store::Job) -> String {
    let mut out = String::with_capacity(job.id.len() + 32);
    framed_into(&mut out, &["job", job.id.as_str()]);
    // The creation-timestamp field is a fixed 16-character hex encoding, so
    // this is exactly `framed(&["job", &job.id, &hex])` written into the same
    // buffer.
    let _ = write!(out, "16:{:016x}", job.created.to_bits());
    out
}

fn binding<'a>(principal: &'a str, resource: &'a str) -> CursorBinding<'a> {
    CursorBinding {
        domain: CURSOR_DOMAIN,
        direction: CursorDirection::Forward,
        principal,
        resource,
        filters: CURSOR_FILTERS,
    }
}

/// Open one supplied token under exactly this reader and job. Any tamper,
/// another reader, another job id or creation timestamp, or another workspace
/// is the same generic refusal.
pub(super) fn open(
    codec: &ScopedCursorCodec,
    principal: &str,
    job: &store::Job,
    token: &str,
    verb: &str,
) -> Result<i64> {
    let resource = job_resource(job);
    codec
        .open(token, &binding(principal, &resource))
        .map_err(|_| invalid_cursor(verb))
}

/// The opaque principal identity of a directly connected, proven agent. The
/// durable `uid` is minted at registration, so a re-registered alias never
/// inherits an old cursor.
pub(super) fn agent_principal(uid: &str) -> String {
    framed(&["agent", uid])
}

/// The opaque principal identity of a daemon-verified CLI envelope: the
/// verified company realm, the envelope's principal kind and its
/// server-issued subject. No handle, session id or request field is used.
pub(super) fn cli_principal(reader: &super::cli_rpc::CliPrincipal) -> String {
    framed(&[
        "cli",
        reader.config.company.as_str(),
        reader.actor.principal_kind.as_str(),
        reader.actor.sub.as_str(),
    ])
}

/// The opaque principal identity of a live board session (`company` scopes
/// the subject to its realm).
pub(super) fn public_principal(reader: &PublicReadPrincipal) -> String {
    framed(&["public", reader.company.as_str(), reader.subject.as_str()])
}

/// The opaque principal identity of an operator-proven `read_as` relay.
/// Several operators relaying the same canonical actor are the same reader,
/// except for an agent spelling: `agents/<alias>` names a mutable alias, so
/// the registered agent's durable `uid` is bound too — the store mints a new
/// uid when an alias is registered again, so a later registration under the
/// same alias cannot inherit an old relayed cursor. An unresolvable alias
/// refuses instead of binding a bare name.
pub(super) fn read_as_principal(store: &store::Store, actor: &str, verb: &str) -> Result<String> {
    if let Some(alias) = actor.strip_prefix("agents/") {
        let agent = store.agent(alias).map_err(|_| {
            Error::rejected(format!(
                "{verb} refused: the relayed agent is not registered"
            ))
        })?;
        return Ok(framed(&["read-as", "agent", alias, agent.uid.as_str()]));
    }
    Ok(framed(&["read-as", actor]))
}

/// The stable principal one scoped AGENT read (`agent list`/`agent show`) was
/// admitted for: the daemon-verified CLI identity, the live board session, the
/// operator-proven `read_as` actor, or the native caller's authenticated alias
/// plus its durable registration uid. `None` is the proven operator, who keeps
/// the unscoped path. A native alias that does not resolve refuses rather than
/// binding a bare name, and a relayed `agents/<alias>` binds that agent's
/// current registration — a re-registration mints a new uid, so it can never
/// inherit the earlier page.
pub(super) fn agent_read_principal(
    shared: &Shared,
    params: &Value,
    peer_pid: u32,
    cli: Option<&super::cli_rpc::CliPrincipal>,
    public: Option<&PublicReadPrincipal>,
    verb: &str,
) -> Result<Option<String>> {
    if let Some(reader) = cli {
        return Ok(Some(cli_principal(reader)));
    }
    if let Some(reader) = public {
        return Ok(Some(public_principal(reader)));
    }
    if let Some(actor) = params.get("read_as").and_then(Value::as_str) {
        return Ok(Some(read_as_principal(&shared.store, actor, verb)?));
    }
    match shared.agent_caller(peer_pid, verb)? {
        crate::peer::AgentCaller::Operator => Ok(None),
        crate::peer::AgentCaller::Agent(alias) => {
            let agent = shared.store.agent(&alias).map_err(|_| {
                Error::rejected(format!(
                    "{verb} refused: the calling agent is not registered"
                ))
            })?;
            Ok(Some(framed(&["agent", alias.as_str(), agent.uid.as_str()])))
        }
    }
}

/// The opaque principal identity of a signed member: the verified company and
/// the verified subject.
pub(super) fn member_principal(org: &str, sub: &str) -> String {
    framed(&["member", org, sub])
}

// Payloads are serialized verbatim for scoped readers.
//
// Stored job-event payloads are written by the daemon's own producers with
// fixed, documented key sets, and none of them carries a global sequence
// position — the only global position in this feed is the `events.seq`
// column, which a scoped row simply does not include. Dropping fields or
// rewriting strings by lexical shape would silently alter ordinary content
// instead (a provider line, a failure reason, or any text equal to
// `event:<n>`), so it is deliberately not done. A future producer that must
// expose a position has to put it in a documented field this module projects
// *by event kind*, never by guesswork.

/// One scoped page: every visible row carries a stable keyed `id` and its own
/// forward `cursor`, and the page cursor continues from the last visible row.
/// With no visible rows the page echoes the exact supplied token (an empty
/// continuation) or `null` (an initial page) — never a re-sealed position,
/// because a fresh nonce must not read as activity.
pub(super) fn scoped_page(
    codec: &ScopedCursorCodec,
    principal: &str,
    job: &store::Job,
    events: &[store::Event],
    supplied_after: Option<&str>,
    has_older: Option<bool>,
    verb: &str,
) -> Result<Value> {
    let resource = job_resource(job);
    let binding = binding(principal, &resource);
    // Sealing validates nonempty pages. An empty page must validate too,
    // rather than succeeding with a principal the next request will refuse.
    if events.is_empty() {
        codec
            .validate_binding(&binding)
            .map_err(|_| invalid_cursor(verb))?;
    }
    let mut rows = Vec::with_capacity(events.len());
    let mut cursor = supplied_after.map(str::to_string);
    for event in events {
        let token = codec
            .seal(event.seq, &binding)
            .map_err(|_| invalid_cursor(verb))?;
        let id = codec
            .stable_row_id(CURSOR_DOMAIN, &resource, &event.seq.to_be_bytes())
            .map_err(|_| invalid_cursor(verb))?;
        rows.push(json!({
            "id": id,
            "cursor": token,
            "alias": event.alias,
            "kind": event.kind,
            "payload": event.payload,
            "job_id": event.job_id,
            "task_id": event.task_id,
            "at": event.at,
        }));
        cursor = Some(token);
    }
    let mut page = json!({"events": rows, "cursor": cursor});
    if let Some(has_older) = has_older {
        page["has_older"] = json!(has_older);
    }
    Ok(page)
}

impl Shared {
    /// Resolve the reader of a native `job_events` request from the verified
    /// authority the request carries: the live board session, the
    /// daemon-verified CLI envelope, the operator-proven `read_as` relay, or
    /// the connection's own caller rule. The board connection is never the
    /// reader, and no request field grants access by itself.
    pub(super) fn verified_job_event_reader(
        &self,
        params: &Value,
        peer_pid: u32,
        verb: &str,
    ) -> Result<VerifiedJobEventReader> {
        let relayed = params.get("public_session").is_some()
            || params.get("read_as").is_some()
            || params.get("cli_envelope").is_some();
        let (scope, cli, public) = self.agent_read_scope(params, peer_pid, verb)?;
        // A `None` scope is only reachable for an operator-authorized reader,
        // so it keeps the legacy numeric document with no principal binding.
        if scope.is_none() {
            return Ok(VerifiedJobEventReader {
                scope,
                relayed,
                principal: None,
                cli,
                public,
            });
        }
        let principal = if let Some(reader) = cli.as_ref() {
            cli_principal(reader)
        } else if let Some(reader) = public.as_ref() {
            public_principal(reader)
        } else if let Some(claim) = params.get("read_as") {
            let actor = claim
                .as_str()
                .ok_or_else(|| Error::rejected(format!("{verb} refused: invalid read identity")))?;
            read_as_principal(&self.store, actor, verb)?
        } else {
            match self.agent_caller(peer_pid, verb)? {
                crate::peer::AgentCaller::Agent(alias) => {
                    let agent = self.store.agent(&alias).map_err(|_| {
                        Error::rejected(format!(
                            "{verb} refused: the caller's registration is unavailable"
                        ))
                    })?;
                    agent_principal(&agent.uid)
                }
                crate::peer::AgentCaller::Operator => {
                    return Ok(VerifiedJobEventReader {
                        scope: None,
                        relayed,
                        principal: None,
                        cli,
                        public,
                    })
                }
            }
        };
        Ok(VerifiedJobEventReader {
            scope,
            relayed,
            principal: Some(principal),
            cli,
            public,
        })
    }
}
