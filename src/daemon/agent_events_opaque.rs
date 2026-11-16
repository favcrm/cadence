//! CAD-2215: opaque agent-event pages for every reader that is not a proven
//! operator.
//!
//! A scoped reader continues an agent's feed with an encrypted position
//! instead of the global `events.seq`. The position is bound (see
//! [`super::scoped_cursor`]) to:
//!
//! * the domain `agent-events` and the forward direction,
//! * the reader's durable principal identity — never a request claim, never
//!   the board connection that relayed the call,
//! * the exact stream it reads: a registered agent's canonical alias plus the
//!   durable `uid` minted at registration (so a re-registered alias can never
//!   inherit an old cursor), or the daemon's own stream as a fixed resource
//!   inside the codec's workspace namespace,
//! * a fixed-size SHA-256 digest of the reader's readable project set
//!   (length-framed, sorted) — the authorization the token was minted under,
//!   so a grant or withdrawal refuses it and restoring that same set resumes
//!   it. The dynamic issue map and the private-job denial rows are deliberately
//!   not bound: ordinary work and hidden event/issue insertion must not turn a
//!   continuation into an activity oracle;
//! * the codec's workspace namespace.
//!
//! Every visible row also carries a stable keyed `id`. It is derived from the
//! workspace, the domain, the stream identity and the event's own sequence —
//! not from the key that seals cursors — so an ordinary daemon restart or an
//! active-key rotation leaves it unchanged.
//!
//! A token hides the numeric position but it is not authority: the handler
//! re-proves the reader, the project scope, the private-job boundary, the
//! member-row boundary, the stream identity and the cloud projection on every
//! request, before and after selection. Missing key material always refuses a
//! scoped read; nothing here downgrades to numeric positions.
//!
//! Rows a reader may not see (private/project denial, `assist_intent`,
//! withheld cloud) are removed *before* the page boundaries are derived: the
//! page cursor is the last VISIBLE row's cursor, `has_older` counts visible
//! rows only, and the handler scans past hidden rows instead of stopping on
//! them. An empty continuation echoes the exact supplied token.

use std::collections::BTreeSet;

use serde_json::{json, Value};

use super::identity::PublicReadPrincipal;
use super::scoped_cursor::{CursorBinding, CursorDirection, ScopedCursorCodec};
use super::Shared;
use crate::error::{Error, Result};
use crate::store;

/// The one scoped cursor format this daemon speaks.
pub(super) const FORMAT: &str = cadence_core::board_api::AGENT_EVENTS_CURSOR_FORMAT;

/// The canonical cursor domain of an agent-event feed.
const CURSOR_DOMAIN: &str = "agent-events";

/// The canonical filter text bound into an agent-event cursor: a fixed-size
/// SHA-256 digest of the reader's *readable project set*.
///
/// The pre-image is injectively framed with fixed-width big-endian fields: the
/// set's cardinality, then each key's UTF-8 byte length and bytes in the set's
/// sorted order. No two different sets can frame identically, and the digest
/// is fixed-size, so a workspace with many projects never exceeds the codec's
/// filter bound.
///
/// It deliberately excludes the dynamic issue map and the private-job denial
/// rows: those move with ordinary work and hidden insertions, and binding them
/// would turn a read into an activity oracle (a refusal would reveal work the
/// reader may not see). The readable set is authorization: a grant or a
/// withdrawal changes it, so a token minted under the old set no longer opens,
/// and restoring the set makes the same token open again.
pub(super) fn project_set_filters(readable: &BTreeSet<String>) -> String {
    use sha2::{Digest, Sha256};
    use std::fmt::Write as _;
    let mut hasher = Sha256::new();
    hasher.update((readable.len() as u64).to_be_bytes());
    for key in readable {
        hasher.update((key.len() as u64).to_be_bytes());
        hasher.update(key.as_bytes());
    }
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// The verified reader of one `agent_events` request.
///
/// `principal` is `None` only for an operator-authorized reader (a proven
/// operator connection, an operator-role board session, or an operator-proven
/// `read_as`); every other reader carries the opaque identity its cursor is
/// bound to.
pub(super) struct VerifiedAgentEventReader {
    pub scope: Option<super::identity::ProjectScope>,
    /// Whether the request named its reader through a relay proof.
    pub relayed: bool,
    pub principal: Option<String>,
    /// The relay principals the request actually proved, kept so the caller
    /// can re-prove the same private-job boundary after selection.
    pub cli: Option<super::cli_rpc::CliPrincipal>,
    pub public: Option<PublicReadPrincipal>,
}

impl VerifiedAgentEventReader {
    fn readable(&self) -> Option<&BTreeSet<String>> {
        self.scope.as_ref().map(|(readable, _)| readable)
    }

    /// The canonical filter text a token is bound to: the reader's readable
    /// project set. Authorization changes move it; ordinary work, hidden
    /// event/issue insertion and the dynamic issue map/denial rows do not.
    pub(super) fn readable_filters(&self) -> String {
        match self.readable() {
            Some(readable) => project_set_filters(readable),
            // Operators never seal a cursor; the empty text is never used.
            None => String::new(),
        }
    }

    /// Two resolutions describe the same authority. Relay principals are
    /// deliberately not compared: they are re-verified on every resolution
    /// and the scope/principal they produce is the decision. Only the
    /// readable project set is compared — the issue map is data, and a hidden
    /// issue or event arriving must not turn a continuation into a refusal.
    pub(super) fn same_authority(&self, other: &Self) -> bool {
        self.relayed == other.relayed
            && self.principal == other.principal
            && self.readable() == other.readable()
    }
}

/// The stream a token is bound to: a registered agent's canonical alias plus
/// its durable `uid` (minted at registration, so re-registering the alias
/// produces a new identity), or the daemon's own row-less stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct AgentStream {
    alias: String,
    uid: Option<String>,
}

impl AgentStream {
    pub(super) fn daemon(alias: &str) -> Self {
        Self {
            alias: alias.to_string(),
            uid: None,
        }
    }

    pub(super) fn registered(alias: &str, uid: &str) -> Self {
        Self {
            alias: alias.to_string(),
            uid: Some(uid.to_string()),
        }
    }

    /// The exact resource text bound into the cursor. The daemon stream has
    /// no agents row; its identity is the fixed `daemon-stream` resource
    /// inside the codec's workspace namespace.
    fn resource(&self) -> String {
        match self.uid.as_deref() {
            Some(uid) => super::job_events_opaque::framed(&["agent", self.alias.as_str(), uid]),
            None => super::job_events_opaque::framed(&["daemon-stream"]),
        }
    }

    /// Whether a later resolution names the same stream. A re-registered
    /// alias has a new `uid`, so it is a different stream even though it
    /// keeps its name.
    pub(super) fn same_identity(&self, other: &Self) -> bool {
        self.alias == other.alias && self.uid == other.uid
    }
}

/// The `cursor_format` a request named, or `None` when it named none. The
/// request shape is settled before any agent is named or any event is read,
/// so a downgrade attempt learns nothing.
pub(super) fn requested_format<'a>(params: &'a Value, verb: &str) -> Result<Option<&'a str>> {
    match params.get("cursor_format") {
        None => Ok(None),
        Some(Value::String(format)) => Ok(Some(format.as_str())),
        Some(_) => Err(Error::invalid(
            "agent_events_cursor_format",
            format!("{verb} refused: cursor_format must be \"{FORMAT}\""),
        )),
    }
}

/// The `after` a request named, before any interpretation. A present value is
/// an opaque token; every other shape is kept distinguishable so the handler
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
        "agent_events_cursor_format",
        format!("{verb} refused: cursor_format \"{FORMAT}\" is required for a scoped reader"),
    )
}

pub(super) fn numeric_cursor(verb: &str) -> Error {
    Error::invalid(
        "agent_events_cursor_numeric",
        format!(
            "{verb} refused: a scoped reader must send an opaque cursor, \
             never a numeric position"
        ),
    )
}

pub(super) fn invalid_after(verb: &str) -> Error {
    Error::invalid(
        "agent_events_cursor_invalid",
        format!("{verb} refused: after must be an opaque cursor string"),
    )
}

fn invalid_cursor(verb: &str) -> Error {
    Error::invalid(
        "agent_events_cursor_invalid",
        format!("{verb} refused: the cursor is not valid for this reader and stream"),
    )
}

fn binding<'a>(principal: &'a str, resource: &'a str, filters: &'a str) -> CursorBinding<'a> {
    CursorBinding {
        domain: CURSOR_DOMAIN,
        direction: CursorDirection::Forward,
        principal,
        resource,
        filters,
    }
}

/// Open one supplied token under exactly this reader and stream. Any tamper,
/// another reader, another alias or registration uid, another readable
/// project set (authorization), or another workspace is the same generic
/// refusal.
pub(super) fn open(
    codec: &ScopedCursorCodec,
    principal: &str,
    stream: &AgentStream,
    filters: &str,
    token: &str,
    verb: &str,
) -> Result<i64> {
    let resource = stream.resource();
    codec
        .open(token, &binding(principal, &resource, filters))
        .map_err(|_| invalid_cursor(verb))
}

/// The projection of one visible row. Payloads are serialized verbatim: the
/// daemon's own producers write fixed key sets and none carries a global
/// sequence position (the only global position is the `events.seq` column,
/// which a scoped row simply does not include).
fn row_json(
    codec: &ScopedCursorCodec,
    binding: &CursorBinding<'_>,
    resource: &str,
    event: &store::Event,
    verb: &str,
) -> Result<Value> {
    let token = codec
        .seal(event.seq, binding)
        .map_err(|_| invalid_cursor(verb))?;
    let id = codec
        .stable_row_id(CURSOR_DOMAIN, resource, &event.seq.to_be_bytes())
        .map_err(|_| invalid_cursor(verb))?;
    Ok(json!({
        "id": id,
        "cursor": token,
        "alias": &event.alias,
        "kind": &event.kind,
        "payload": &event.payload,
        "job_id": &event.job_id,
        "task_id": &event.task_id,
        "at": event.at,
    }))
}

/// CAD-2215: project one event for a scoped reader.
///
/// The cross-agent turn wiring and two system-generated event kinds carry
/// something a scoped reader must not learn. They are removed BY VERIFIED
/// KIND, never by the lexical shape of arbitrary producer or caller payload
/// (which would silently rewrite ordinary content that merely looks numeric):
///
/// * `turn_id`/`turn`/`reply_to`/`worktree` name a cross-agent exchange the
///   scoped reader is not a party to;
/// * `check_monitor` writes `monitor_alert` on the daemon stream with
///   `payload.event_seq` (the observed global sequence) and
///   `payload.fingerprint` (`event:<seq>`);
/// * `ack_monitor_alert` writes `monitor_alert_ack` with `payload.alert` (the
///   alert's global sequence).
///
/// The operator still reads all of them unchanged.
pub(super) fn project_scoped_event(event: &mut store::Event) {
    let keys: &[&str] = match event.kind.as_str() {
        "monitor_alert" => &[
            "turn_id",
            "turn",
            "reply_to",
            "worktree",
            "event_seq",
            "fingerprint",
        ],
        "monitor_alert_ack" => &["turn_id", "turn", "reply_to", "worktree", "alert"],
        _ => &["turn_id", "turn", "reply_to", "worktree"],
    };
    if let Some(fields) = event.payload.as_object_mut() {
        for key in keys {
            fields.remove(*key);
        }
    }
}

/// One scoped page: every visible row carries a stable keyed `id` and its own
/// forward `cursor`, and the page cursor continues from the last visible row.
/// With no visible rows the page echoes the exact supplied token (an empty
/// continuation) or `null` (an initial page) — never a re-sealed position,
/// because a fresh nonce must not read as activity.
#[allow(clippy::too_many_arguments)]
pub(super) fn scoped_page(
    codec: &ScopedCursorCodec,
    principal: &str,
    stream: &AgentStream,
    filters: &str,
    events: &[store::Event],
    supplied_after: Option<&str>,
    has_older: Option<bool>,
    verb: &str,
) -> Result<Value> {
    let resource = stream.resource();
    let binding = binding(principal, &resource, filters);
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
        let row = row_json(codec, &binding, &resource, event, verb)?;
        cursor = row
            .get("cursor")
            .and_then(Value::as_str)
            .map(str::to_string);
        rows.push(row);
    }
    let mut page = json!({"events": rows, "cursor": cursor});
    if let Some(has_older) = has_older {
        page["has_older"] = json!(has_older);
    }
    Ok(page)
}

impl Shared {
    /// Resolve the stream a token is bound to. A registered alias is refused
    /// if its row vanished; the daemon stream has no row and none is needed.
    pub(super) fn agent_event_stream(
        &self,
        alias: &str,
        daemon: bool,
        verb: &str,
    ) -> Result<AgentStream> {
        if daemon {
            return Ok(AgentStream::daemon(alias));
        }
        let agent = self.store.agent(alias).map_err(|_| {
            Error::rejected(format!(
                "{verb} refused: the stream's registration is unavailable"
            ))
        })?;
        Ok(AgentStream::registered(&agent.alias, &agent.uid))
    }

    /// Resolve the reader of an `agent_events` request from the verified
    /// authority the request carries: the live board session, the
    /// daemon-verified CLI envelope, the operator-proven `read_as` relay, or
    /// the connection's own caller rule. The board connection is never the
    /// reader, and no request field grants access by itself.
    pub(super) fn verified_agent_event_reader(
        &self,
        params: &Value,
        peer_pid: u32,
        verb: &str,
    ) -> Result<VerifiedAgentEventReader> {
        let relayed = params.get("public_session").is_some()
            || params.get("read_as").is_some()
            || params.get("cli_envelope").is_some();
        let (scope, cli, public) = self.agent_read_scope(params, peer_pid, verb)?;
        // A `None` scope is only reachable for an operator-authorized reader,
        // so it keeps the legacy numeric document with no principal binding.
        if scope.is_none() {
            return Ok(VerifiedAgentEventReader {
                scope,
                relayed,
                principal: None,
                cli,
                public,
            });
        }
        let principal = if let Some(reader) = cli.as_ref() {
            super::job_events_opaque::cli_principal(reader)
        } else if let Some(reader) = public.as_ref() {
            super::job_events_opaque::public_principal(reader)
        } else if let Some(claim) = params.get("read_as") {
            let actor = claim
                .as_str()
                .ok_or_else(|| Error::rejected(format!("{verb} refused: invalid read identity")))?;
            super::job_events_opaque::read_as_principal(&self.store, actor, verb)?
        } else {
            match self.agent_caller(peer_pid, verb)? {
                crate::peer::AgentCaller::Agent(alias) => {
                    let agent = self.store.agent(&alias).map_err(|_| {
                        Error::rejected(format!(
                            "{verb} refused: the caller's registration is unavailable"
                        ))
                    })?;
                    super::job_events_opaque::agent_principal(&agent.uid)
                }
                crate::peer::AgentCaller::Operator => {
                    return Ok(VerifiedAgentEventReader {
                        scope: None,
                        relayed,
                        principal: None,
                        cli,
                        public,
                    })
                }
            }
        };
        Ok(VerifiedAgentEventReader {
            scope,
            principal: Some(principal),
            relayed,
            cli,
            public,
        })
    }
}
