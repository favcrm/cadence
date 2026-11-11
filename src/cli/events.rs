//! CAD-535: `cadence events` — moved verbatim from src/main.rs.

use super::*;

/// The forward position of an agent-event page — whatever the daemon
/// decided THIS reader may hold. The CLI never chooses the kind: a numeric
/// position exists only on a page whose acknowledgement names a proven
/// operator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum AgentEventsAfter {
    /// A proven operator's native sequence position.
    Sequence(i64),
    /// Every other reader's opaque token, carried verbatim.
    Token(String),
}

/// The reader a page's `_reader_scope` acknowledgement names. It is proof
/// from the authenticated daemon, never a caller flag or environment value —
/// an agent pane and an operator both run this binary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AgentEventsReader {
    Operator,
    Scoped,
}

/// One protocol refusal: actionable, and never a page.
fn agent_events_protocol(detail: &str) -> Error {
    Error::rejected(format!(
        "events: the daemon answered outside the shipped agent-event protocol \
         ({detail}); no page was printed"
    ))
}

/// Take the per-response reader acknowledgement off the page (it is protocol
/// evidence, not payload) and name the reader it proves. A missing or unknown
/// acknowledgement is an actionable protocol error: an older daemon must not
/// be read as a scoped raw numeric feed.
pub(super) fn agent_events_ack(page: &mut Value) -> Result<AgentEventsReader> {
    let ack = page
        .as_object_mut()
        .and_then(|body| body.remove(cadence_core::board_api::READER_SCOPE_FIELD));
    match ack.as_ref().and_then(Value::as_str) {
        Some(ack) if ack == cadence_core::board_api::AGENT_EVENTS_SCOPE_ACK => {
            Ok(AgentEventsReader::Scoped)
        }
        Some(ack) if ack == cadence_core::board_api::AGENT_EVENTS_OPERATOR_SCOPE_ACK => {
            Ok(AgentEventsReader::Operator)
        }
        _ => Err(Error::rejected(
            "events: this daemon did not confirm the scoped agent-event protocol \
             (_reader_scope is missing or unknown), so no page was printed. Restart \
             the daemon from a build that supports CAD-2215 and retry",
        )),
    }
}

/// Check one page against the reader it acknowledged and return it (the
/// acknowledgement already stripped) for printing. A scoped page must omit
/// global positions entirely, carry a stable keyed `id` and its own forward
/// `cursor` on every visible event, and end on the last visible cursor — the
/// exact supplied token on an empty page, or null on an initial empty page. An
/// operator page keeps its native numeric document.
pub(super) fn agent_events_page(
    page: Value,
    reader: AgentEventsReader,
    after: Option<&AgentEventsAfter>,
) -> Result<Value> {
    if page.get("has_older").is_some_and(|flag| !flag.is_boolean()) {
        return Err(agent_events_protocol("`has_older` is not a boolean"));
    }
    match reader {
        AgentEventsReader::Operator => {
            if page.get("events").and_then(Value::as_array).is_none() {
                return Err(agent_events_protocol("the page has no `events` array"));
            }
            if page.get("cursor").and_then(Value::as_i64).is_none() {
                return Err(agent_events_protocol(
                    "an operator page has no numeric cursor",
                ));
            }
            Ok(page)
        }
        AgentEventsReader::Scoped => {
            let events = page
                .get("events")
                .and_then(Value::as_array)
                .ok_or_else(|| agent_events_protocol("the page has no `events` array"))?;
            let mut last: Option<&str> = None;
            for event in events {
                let row = event
                    .as_object()
                    .ok_or_else(|| agent_events_protocol("an event row is not an object"))?;
                if row.contains_key("seq") || row.contains_key("event_seq") {
                    return Err(agent_events_protocol(
                        "a scoped event row carries a global sequence position",
                    ));
                }
                if row
                    .get("id")
                    .and_then(Value::as_str)
                    .is_none_or(str::is_empty)
                {
                    return Err(agent_events_protocol("a scoped event row has no stable id"));
                }
                let cursor = row
                    .get("cursor")
                    .and_then(Value::as_str)
                    .filter(|cursor| !cursor.is_empty())
                    .ok_or_else(|| {
                        agent_events_protocol("a scoped event row has no forward cursor")
                    })?;
                last = Some(cursor);
            }
            let echoed = match after {
                Some(AgentEventsAfter::Token(token)) => Some(token.as_str()),
                Some(AgentEventsAfter::Sequence(_)) | None => None,
            };
            let expected = last.or(echoed);
            let page_cursor = match page.get("cursor") {
                None | Some(Value::Null) => None,
                Some(Value::String(cursor)) if !cursor.is_empty() => Some(cursor.as_str()),
                _ => {
                    return Err(agent_events_protocol(
                        "the scoped page cursor is not an opaque token",
                    ))
                }
            };
            if page_cursor != expected {
                return Err(agent_events_protocol(
                    "the scoped page cursor is not the last visible event's cursor",
                ));
            }
            Ok(page)
        }
    }
}

/// The position the next request carries: the page's own forward cursor, never
/// a guessed, clamped or defaulted value.
pub(super) fn agent_events_next(
    page: &Value,
    reader: AgentEventsReader,
) -> Result<Option<AgentEventsAfter>> {
    match reader {
        AgentEventsReader::Operator => page
            .get("cursor")
            .and_then(Value::as_i64)
            .map(|position| Some(AgentEventsAfter::Sequence(position)))
            .ok_or_else(|| agent_events_protocol("an operator page has no numeric cursor")),
        AgentEventsReader::Scoped => Ok(page
            .get("cursor")
            .and_then(Value::as_str)
            .filter(|cursor| !cursor.is_empty())
            .map(|cursor| AgentEventsAfter::Token(cursor.to_string()))),
    }
}

/// One `agent_events` request in the scoped protocol. `tail` asks for the
/// newest page; otherwise `after` is the forward position (omitted only when
/// there is no position yet).
pub(super) fn agent_events_request(
    alias: &str,
    after: Option<&AgentEventsAfter>,
    tail: bool,
    wait: u64,
) -> Value {
    let mut request = json!({
        "alias": alias,
        "cursor_format": cadence_core::board_api::AGENT_EVENTS_CURSOR_FORMAT,
    });
    if tail {
        request["tail"] = json!(true);
    } else {
        match after {
            Some(AgentEventsAfter::Sequence(position)) => request["after"] = json!(position),
            Some(AgentEventsAfter::Token(token)) => request["after"] = json!(token),
            None => {}
        }
        request["wait"] = json!(wait);
    }
    request
}

/// `--after` as supplied. All-ASCII-digit text is a numeric sequence, which
/// only a proven operator page accepts; anything else is an opaque token
/// carried verbatim. Neither form is ever coerced into the other.
fn agent_events_after_argument(raw: &str) -> Result<AgentEventsAfter> {
    if raw.is_empty() {
        return Err(Error::rejected(
            "events: --after needs an opaque cursor or a nonnegative sequence",
        ));
    }
    if raw.bytes().all(|byte| byte.is_ascii_digit()) {
        let position = raw
            .parse::<i64>()
            .map_err(|_| Error::rejected("events: --after sequence is out of range"))?;
        return Ok(AgentEventsAfter::Sequence(position));
    }
    Ok(AgentEventsAfter::Token(raw.to_string()))
}

/// CAD-2215: stream one agent's event feed through the scoped protocol. Every
/// request asks for the opaque format; the daemon proves which reader this is.
/// A scoped page is printed only after its acknowledgement names the opaque
/// reader, carries no global sequence, and continues from a visible cursor or
/// exactly the supplied token.
pub(super) fn stream_agent_events(
    state_dir: &Path,
    alias: &str,
    after: Option<&str>,
    wait: u64,
    follow: bool,
) -> Result<i32> {
    let rpc = |params: Value| client::rpc(state_dir, "agent_events", params);
    let mut position = after.map(agent_events_after_argument).transpose()?;
    let mut reader = None;
    if position.is_none() {
        // No --after: the default page is the newest 50 (oldest first within
        // it). --follow anchors there too — history below the page is a
        // `has_older` flag, not a flood.
        let mut page = rpc(agent_events_request(alias, None, true, 0))?;
        let ack = agent_events_ack(&mut page)?;
        page = agent_events_page(page, ack, None)?;
        reader = Some(ack);
        if !cadence_agent::output::stream_json(&page) {
            return Ok(0);
        }
        if !follow {
            return Ok(0);
        }
        position = agent_events_next(&page, ack)?;
    } else if matches!(position.as_ref(), Some(AgentEventsAfter::Sequence(_))) {
        // A numeric --after is only a proven operator's position, and the
        // daemon proves which reader this is: probe a tail page first, then
        // refuse before the number is ever sent as a continuation.
        let mut probe = rpc(agent_events_request(alias, None, true, 0))?;
        let ack = agent_events_ack(&mut probe)?;
        let _ = agent_events_page(probe, ack, None)?;
        if ack != AgentEventsReader::Operator {
            return Err(Error::rejected(
                "events: --after is a numeric sequence, which only a proven operator \
                 page accepts; this reader gets opaque cursors — pass the `cursor` \
                 value a previous page returned instead",
            ));
        }
        reader = Some(ack);
    }
    loop {
        let mut page = rpc(agent_events_request(
            alias,
            position.as_ref(),
            false,
            if follow { 25 } else { wait },
        ))?;
        let ack = agent_events_ack(&mut page)?;
        match reader {
            Some(established) if established != ack => {
                return Err(Error::rejected(
                    "events: the daemon changed the reader this stream is scoped for; \
                     restart the command",
                ))
            }
            Some(_) => {}
            None => reader = Some(ack),
        }
        if ack == AgentEventsReader::Operator
            && matches!(position.as_ref(), Some(AgentEventsAfter::Token(_)))
        {
            return Err(Error::rejected(
                "events: the daemon reports a proven operator, whose cursors are \
                 numeric; the supplied opaque cursor belongs to a scoped reader",
            ));
        }
        let page = agent_events_page(page, ack, position.as_ref())?;
        let empty = page
            .get("events")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty);
        if (!empty || !follow) && !cadence_agent::output::stream_json(&page) {
            return Ok(0);
        }
        position = agent_events_next(&page, ack)?;
        if !follow {
            return Ok(0);
        }
    }
}

pub(super) fn run(
    state_dir: PathBuf,
    alias: Option<String>,
    job: Option<String>,
    after: Option<String>,
    wait: u64,
    follow: bool,
) -> Result<i32> {
    match (alias, job) {
        (Some(alias), None) => {
            stream_agent_events(&state_dir, &alias, after.as_deref(), wait, follow)
        }
        // CAD-2215: the job view shares one validated scoped path with
        // `cadence job events`, so an opaque cursor follows verbatim here
        // too and neither command can print a scoped raw numeric feed.
        (None, Some(job)) => {
            super::job::stream_job_events(&state_dir, &job, after.as_deref(), wait, follow)
        }
        (Some(_), Some(_)) => Err(Error::rejected("events takes an alias or --job, not both")),
        (None, None) => Err(Error::rejected("events needs an alias or --job <job>")),
    }
}
