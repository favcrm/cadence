//! CAD-534: `cadence daemon` monitors RPC handlers — moved verbatim from src/daemon.rs.
//!
//! CAD-2215: a reader that is not a proven operator gets no global sequence
//! position from this module. `monitor_list`/`monitor_show` omit the monitor's
//! internal `event_cursor` watermark, and `monitor_alerts` answers with an
//! opaque page: every alert carries a stable keyed row `id` and a distinct
//! encrypted `ack` token, the page continues with an encrypted `cursor`, and
//! no row exposes `seq`, `event_seq` or the `event:<seq>` fingerprint. A
//! proven operator keeps the legacy numeric document unchanged.
//!
//! The tokens are bound (see [`super::scoped_cursor`]) to the verified reader
//! principal, the stored monitor identity (id plus its stored `created`
//! timestamp), the codec workspace namespace, the domain, the direction and
//! the canonical `open` filter. They are never authorization: the monitor's
//! project coverage and private-job boundary are re-proven before and after
//! every read selection. Missing key material refuses a scoped read; nothing
//! here downgrades to numeric positions or falls back to the operator
//! document.
//!
//! A native scoped acknowledgement is the one mutating path. It runs a single
//! authorization critical section: the tracker lock is held while the reader's
//! project scope is re-resolved, the enrollment/slots guard is held while the
//! native identity evidence is compared against transaction-local pane,
//! registration and enrollment rows, and then one `Store::write_tx` re-reads
//! the monitor identity, coverage and private-job denials, checks the alert's
//! task belongs to that coverage, and mutates the alert in the same snapshot.
//! A supported project-policy, coverage, private-grant, registration or
//! enrollment revocation that commits before that transaction is therefore
//! observed by it; an acknowledgement that commits first is not retroactively
//! cancelled. Native process death and arbitrary same-UID filesystem writes
//! are outside what these locks can serialize. Operator numeric
//! acknowledgement and read-relay proofs stay outside this path (relay proofs
//! cannot acknowledge at all).

use std::fmt::Write as _;

use cadence_core::board_api::{
    MONITOR_CURSOR_FORMAT, MONITOR_OPERATOR_SCOPE_ACK, READER_SCOPE_FIELD,
};
use serde_json::{json, Value};

use super::identity::{scope_allows_issue, ProjectScope, PublicReadPrincipal};
use super::job_events_opaque;
use super::scoped_cursor::{CursorBinding, CursorDirection, ScopedCursorCodec};
use super::*;

/// The exact value of [`READER_SCOPE_FIELD`] on a scoped monitor reply: the
/// daemon understood the relayed reader proof and applied the reader boundary
/// to this answer. Bump the suffix when this changes.
pub(crate) const MONITOR_READER_SCOPE_ACK: &str = "cad2215.monitor-read.opaque.v1";

const ALERTS_DOMAIN: &str = "monitor-alerts";
const ACK_DOMAIN: &str = "monitor-alert-ack";
const ALERTS_FILTERS_OPEN: &str = "open=1";
const ALERTS_FILTERS_ALL: &str = "open=0";
const ACK_FILTERS: &str = "alert";

/// A length-framed, injective encoding of one identifier tuple (the same
/// wire discipline [`job_events_opaque`] uses). Only ever compared as opaque
/// text; injectivity keeps two monitors or two readers apart.
fn framed_into(out: &mut String, fields: &[&str]) {
    for field in fields {
        // Writing to a `String` cannot fail.
        let _ = write!(out, "{}:{}", field.len(), field);
    }
}

/// The resource a monitor token is bound to: the stored monitor id and the
/// stored `created` timestamp, kept bit-exact. Re-registration of an identical
/// scope returns the existing row (so `created` is stable), and a monitor is
/// never deleted; a changed `created` or id is refused by the reproof.
fn monitor_resource(monitor: &store::Monitor) -> String {
    let mut out = String::with_capacity(monitor.id.len() + 32);
    framed_into(&mut out, &["monitor", monitor.id.as_str()]);
    let _ = write!(out, "16:{:016x}", monitor.created.to_bits());
    out
}

/// One verified reader of a monitor request. `principal` is `None` only for
/// an operator-authorized reader (a proven operator connection, an
/// operator-role board session, or an operator-proven `read_as`), which keeps
/// the legacy numeric document.
struct MonitorReader {
    scope: Option<ProjectScope>,
    /// Whether the request named its reader through a trusted relay proof.
    relayed: bool,
    principal: Option<String>,
    cli: Option<super::cli_rpc::CliPrincipal>,
    public: Option<PublicReadPrincipal>,
}

impl MonitorReader {
    fn scoped(&self) -> bool {
        self.principal.is_some()
    }

    /// Two resolutions describe the same authority. Only the verified
    /// principal and the readable project SET are compared: the scope's issue
    /// map is dynamic, so an unrelated issue/job inserted during selection
    /// must not turn an otherwise-authorized read into a refusal. Relay
    /// principals are re-verified on every resolution and are deliberately not
    /// compared; the principal they produce is the decision.
    fn same_authority(&self, other: &Self) -> bool {
        if self.relayed != other.relayed || self.principal != other.principal {
            return false;
        }
        match (&self.scope, &other.scope) {
            (None, None) => true,
            (Some((readable, _)), Some((current, _))) => readable == current,
            _ => false,
        }
    }
}

/// The native identity evidence a scoped acknowledgement pinned before taking
/// the tracker guard. It records only what the caller-rule proof used: the
/// alias, the nearest pane/enrollment decision, the ancestor chain, the
/// registration uid and the matched pane row. Nothing here is a request claim.
struct MonitorAckIdentity {
    peer_pid: u32,
    alias: String,
    chain: Vec<u32>,
    slot: super::identity::SlotWho,
    uid: String,
    pane: Option<(u32, Option<u64>)>,
}

fn monitor_visible(
    scope: &Option<ProjectScope>,
    denied: &store::PrivateJobDenials,
    coverage: &[(String, Option<String>)],
) -> bool {
    scope.is_none()
        || (!coverage.is_empty()
            && coverage.iter().all(|(task, issue)| {
                scope_allows_issue(scope, issue.as_deref()) && !denied.tasks.contains(task)
            }))
}

/// A monitor reply for one reader. A scoped reader never receives the
/// monitor's internal `event_cursor` watermark.
fn monitor_json(
    monitor: &store::Monitor,
    coverage: &[String],
    open_alerts: i64,
    total_alerts: i64,
    scoped: bool,
) -> Value {
    let mut value = monitor.to_json(coverage, open_alerts, total_alerts);
    if scoped {
        if let Some(object) = value.as_object_mut() {
            object.remove("event_cursor");
        }
    }
    value
}

/// The scoped continuation of a monitor-alert page.
enum SuppliedAfter<'a> {
    Absent,
    Text(&'a str),
    Number,
    /// `null`, a boolean, an array or an object.
    Unsupported,
}

fn supplied_after(params: &Value) -> SuppliedAfter<'_> {
    match params.get("after") {
        None => SuppliedAfter::Absent,
        Some(Value::String(token)) => SuppliedAfter::Text(token),
        Some(Value::Number(_)) => SuppliedAfter::Number,
        Some(_) => SuppliedAfter::Unsupported,
    }
}

fn cursor_format(verb: &str) -> Error {
    Error::invalid(
        "monitor_alerts_cursor_format",
        format!(
            "{verb} refused: cursor_format \"{MONITOR_CURSOR_FORMAT}\" is required for a scoped reader"
        ),
    )
}

fn numeric_cursor(verb: &str) -> Error {
    Error::invalid(
        "monitor_alerts_cursor_numeric",
        format!(
            "{verb} refused: a scoped reader must send an opaque cursor, \
             never a numeric position"
        ),
    )
}

fn invalid_after(verb: &str) -> Error {
    Error::invalid(
        "monitor_alerts_cursor_invalid",
        format!("{verb} refused: after must be an opaque cursor string"),
    )
}

fn invalid_cursor(verb: &str) -> Error {
    Error::invalid(
        "monitor_alerts_cursor_invalid",
        format!("{verb} refused: the cursor is not valid for this reader and monitor"),
    )
}

fn numeric_ack(verb: &str) -> Error {
    Error::invalid(
        "monitor_alert_ack_numeric",
        format!(
            "{verb} refused: a scoped reader must send an opaque ack token, \
             never a numeric position"
        ),
    )
}

fn invalid_ack(verb: &str) -> Error {
    Error::invalid(
        "monitor_alert_ack_invalid",
        format!("{verb} refused: the ack token is not valid for this reader and monitor"),
    )
}

fn alerts_binding<'a>(principal: &'a str, resource: &'a str, open_only: bool) -> CursorBinding<'a> {
    CursorBinding {
        domain: ALERTS_DOMAIN,
        direction: CursorDirection::Forward,
        principal,
        resource,
        filters: if open_only {
            ALERTS_FILTERS_OPEN
        } else {
            ALERTS_FILTERS_ALL
        },
    }
}

fn ack_binding<'a>(principal: &'a str, resource: &'a str) -> CursorBinding<'a> {
    CursorBinding {
        domain: ACK_DOMAIN,
        direction: CursorDirection::Forward,
        principal,
        resource,
        filters: ACK_FILTERS,
    }
}

/// One scoped alert row: a stable keyed identity and its own ack token, never
/// the global `seq`, the observed `event_seq` or the `event:<seq>` fingerprint.
///
/// The daemon's own producers write alert payloads with a fixed, documented
/// key set, and both `check_monitor` and the dispatch-blocked recorder copy
/// the observed global `event_seq` into `payload.event_seq`. For a scoped
/// reader that one known field is projected out; no payload is rewritten by
/// lexical shape, so ordinary content is untouched.
fn scoped_alert_json(
    codec: &ScopedCursorCodec,
    principal: &str,
    resource: &str,
    alert: &store::MonitorAlert,
) -> Result<Value> {
    let position = alert.seq.to_be_bytes();
    let id = codec
        .stable_row_id(ALERTS_DOMAIN, resource, &position)
        .map_err(|_| Error::internal("monitor alert identity unavailable"))?;
    let ack = codec
        .seal(alert.seq, &ack_binding(principal, resource))
        .map_err(|_| Error::internal("monitor alert acknowledgement unavailable"))?;
    let mut payload = alert.payload.clone();
    if let Some(object) = payload.as_object_mut() {
        object.remove("event_seq");
    }
    Ok(json!({
        "id": id,
        "ack": ack,
        "monitor": alert.monitor_id,
        "task": alert.task_id,
        "kind": alert.kind,
        "payload": payload,
        "state": alert.state,
        "attempts": alert.attempts,
        "last_error": alert.last_error,
        "created": alert.created,
        "updated": alert.updated,
    }))
}

/// One scoped page. With no visible rows the page echoes the exact supplied
/// token (an empty continuation) or `null` (an initial page) — never a
/// re-sealed position, because a fresh nonce must not read as activity.
fn scoped_alert_page(
    codec: &ScopedCursorCodec,
    principal: &str,
    resource: &str,
    alerts: &[store::MonitorAlert],
    supplied: Option<&str>,
    open_only: bool,
    verb: &str,
) -> Result<Value> {
    let binding = alerts_binding(principal, resource, open_only);
    if alerts.is_empty() {
        // An empty page must validate too, rather than succeeding with a
        // principal the next request will refuse.
        codec
            .validate_binding(&binding)
            .map_err(|_| invalid_cursor(verb))?;
    }
    let mut rows = Vec::with_capacity(alerts.len());
    for alert in alerts {
        rows.push(scoped_alert_json(codec, principal, resource, alert)?);
    }
    let cursor = match alerts.last() {
        Some(alert) => Some(
            codec
                .seal(alert.seq, &binding)
                .map_err(|_| invalid_cursor(verb))?,
        ),
        None => supplied.map(str::to_string),
    };
    let mut page = json!({
        "cursor": cursor,
        "cursor_format": MONITOR_CURSOR_FORMAT,
        "alerts": rows,
    });
    page[READER_SCOPE_FIELD] = json!(MONITOR_READER_SCOPE_ACK);
    Ok(page)
}

impl Shared {
    fn monitor_denials(&self, caller: &AgentCaller) -> Result<store::PrivateJobDenials> {
        match caller {
            AgentCaller::Operator => Ok(Default::default()),
            AgentCaller::Agent(alias) => self
                .store
                .private_job_denials(store::PrivateJobReader::Agent(alias)),
        }
    }

    fn monitor_for_caller(
        &self,
        caller: &AgentCaller,
        id: &str,
    ) -> Result<store::ProjectMonitorView> {
        let scope = self.project_scope_for_caller(caller)?;
        let denied = self.monitor_denials(caller)?;
        let view = self.store.project_monitor_view(id)?;
        if !monitor_visible(&scope, &denied, &view.coverage) {
            return Err(Error::rejected(format!("No such monitor '{id}'")));
        }
        Ok(view)
    }

    /// Resolve the reader of a monitor request from the verified authority the
    /// request carries: the trusted board/CLI relay proof (`public_session`,
    /// `cli_envelope`, operator-proven `read_as`), or the connection's own
    /// caller rule. The board connection is never the reader.
    fn monitor_reader(&self, params: &Value, peer_pid: u32, verb: &str) -> Result<MonitorReader> {
        let relayed = params.get("public_session").is_some()
            || params.get("read_as").is_some()
            || params.get("cli_envelope").is_some();
        let (scope, cli, public) = self.agent_read_scope(params, peer_pid, verb)?;
        // A `None` scope is only reachable for an operator-authorized reader,
        // so it keeps the legacy numeric document with no principal binding.
        if scope.is_none() {
            return Ok(MonitorReader {
                scope,
                relayed,
                principal: None,
                cli,
                public,
            });
        }
        let principal = if let Some(reader) = cli.as_ref() {
            job_events_opaque::cli_principal(reader)
        } else if let Some(reader) = public.as_ref() {
            job_events_opaque::public_principal(reader)
        } else if let Some(claim) = params.get("read_as") {
            let actor = claim
                .as_str()
                .ok_or_else(|| Error::rejected(format!("{verb} refused: invalid read identity")))?;
            job_events_opaque::read_as_principal(&self.store, actor, verb)?
        } else {
            match self.agent_caller(peer_pid, verb)? {
                AgentCaller::Agent(alias) => {
                    let agent = self.store.agent(&alias).map_err(|_| {
                        Error::rejected(format!(
                            "{verb} refused: the caller's registration is unavailable"
                        ))
                    })?;
                    job_events_opaque::agent_principal(&agent.uid)
                }
                AgentCaller::Operator => {
                    return Ok(MonitorReader {
                        scope: None,
                        relayed,
                        principal: None,
                        cli,
                        public,
                    })
                }
            }
        };
        Ok(MonitorReader {
            scope,
            relayed,
            principal: Some(principal),
            cli,
            public,
        })
    }

    /// The reader's private-job boundary. A relayed reader is admitted by the
    /// authority the request proved, never by the board connection.
    fn monitor_denials_for(
        &self,
        reader: &MonitorReader,
        params: &Value,
        peer_pid: u32,
    ) -> Result<store::PrivateJobDenials> {
        self.private_job_denials_for_read(
            params,
            reader.cli.as_ref(),
            reader.public.as_ref(),
            peer_pid,
        )
    }

    fn monitor_view_for(
        &self,
        reader: &MonitorReader,
        params: &Value,
        peer_pid: u32,
        id: &str,
    ) -> Result<store::ProjectMonitorView> {
        let view = self.store.project_monitor_view(id)?;
        let denied = self.monitor_denials_for(reader, params, peer_pid)?;
        if !monitor_visible(&reader.scope, &denied, &view.coverage) {
            return Err(Error::rejected(format!("No such monitor '{id}'")));
        }
        Ok(view)
    }

    /// Re-prove every input the selected page depended on AFTER selection and
    /// before any content leaves: the reader proof, its project scope, its
    /// private-job boundary and the stored monitor identity. Any change is one
    /// refusal, never a mixed snapshot.
    fn reprove_monitor_read(
        &self,
        params: &Value,
        peer_pid: u32,
        verb: &str,
        reader: &MonitorReader,
        view: &store::ProjectMonitorView,
    ) -> Result<()> {
        let changed = || Error::rejected(format!("{verb} refused: reader access changed"));
        let current = self.monitor_reader(params, peer_pid, verb)?;
        if !reader.same_authority(&current) {
            return Err(changed());
        }
        let current_view = self.monitor_view_for(&current, params, peer_pid, &view.monitor.id)?;
        if current_view.coverage != view.coverage
            || monitor_resource(&current_view.monitor) != monitor_resource(&view.monitor)
        {
            return Err(Error::rejected(format!("{verb} refused: coverage changed")));
        }
        Ok(())
    }

    pub(super) fn rpc_monitor_register(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        let tasks = params
            .get("tasks")
            .and_then(Value::as_array)
            .ok_or_else(|| Error::rejected("Monitor registration requires a tasks array"))?
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_string)
                    .ok_or_else(|| Error::rejected("Monitor task ids must be strings"))
            })
            .collect::<Result<Vec<_>>>()?;
        let owner = required_str(params, "owner")?;
        let interval = optional_u64(params, "interval_secs").unwrap_or(60);
        let dispatch_enabled = params
            .get("dispatch_enabled")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let auto_dispatch_enabled = params
            .get("auto_dispatch_enabled")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        // CAD-422: a registered monitor's auto-dispatch bypasses the
        // operator-only `monitor_dispatch` gate, so arming either flag
        // is the operator's act. An agent may still register coverage —
        // its `owner` is already stamped by the caller rule — it just
        // cannot arm dispatch.
        let caller = self.agent_caller(peer_pid, "monitor register")?;
        if dispatch_enabled || auto_dispatch_enabled {
            if let AgentCaller::Agent(alias) = &caller {
                return Err(Error::rejected(format!(
                    "monitor register refused: agent '{alias}' may not enable \
                     dispatch flags — arming a monitor's dispatch is the \
                     operator's act (caller rule, CAD-422)"
                )));
            }
        }
        if matches!(&caller, AgentCaller::Agent(_))
            && !tasks.is_empty()
            && tasks.len() <= store::MAX_MONITOR_TASKS
        {
            let scope = self.project_scope_for_caller(&caller)?;
            let bindings = self.store.task_issue_bindings(&tasks)?;
            let denied = self.monitor_denials(&caller)?;
            for task in &tasks {
                let issue = bindings.get(task).and_then(Option::as_deref);
                if !scope_allows_issue(&scope, issue) || denied.tasks.contains(task) {
                    return Err(Error::rejected(format!("No such task '{task}'")));
                }
            }
        }
        let (monitor, duplicate) = self.store.register_monitor(
            required_str(params, "monitor")?,
            required_str(params, "project")?,
            owner,
            interval,
            &tasks,
            dispatch_enabled,
            auto_dispatch_enabled,
        )?;
        let (monitor, coverage, open, total) = self.store.monitor_view(&monitor.id)?;
        self.wake();
        Ok(json!({
            "monitor": monitor_json(
                &monitor,
                &coverage,
                open,
                total,
                matches!(&caller, AgentCaller::Agent(_)),
            ),
            "duplicate": duplicate,
        }))
    }

    pub(super) fn rpc_monitor_list(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        let verb = "monitor list";
        let reader = self.monitor_reader(params, peer_pid, verb)?;
        let denied = self.monitor_denials_for(&reader, params, peer_pid)?;
        let mut monitors = Vec::new();
        let mut selected = Vec::new();
        for monitor in self.store.monitors()? {
            let view = self.store.project_monitor_view(&monitor.id)?;
            if !monitor_visible(&reader.scope, &denied, &view.coverage) {
                continue;
            }
            selected.push((view.monitor.id.clone(), monitor_resource(&view.monitor)));
            let coverage = view
                .coverage
                .into_iter()
                .map(|(id, _)| id)
                .collect::<Vec<_>>();
            monitors.push(monitor_json(
                &view.monitor,
                &coverage,
                view.open_alerts,
                view.total_alerts,
                reader.scoped(),
            ));
        }
        // Acceptance seam: mutate authority between selection and response.
        #[cfg(test)]
        if let Some(hook) = &self.monitor_read_snapshot_hook {
            hook("list");
        }
        // Re-prove only the selected monitors: the reader's verified principal
        // and readable project set, then each selected row's identity and
        // coverage under the current denial set. An unrelated hidden
        // issue/job inserted during selection cannot change any selected
        // monitor's decision, but a selected monitor that became unauthorized
        // refuses the whole page.
        let current = self.monitor_reader(params, peer_pid, verb)?;
        if !reader.same_authority(&current) {
            return Err(Error::rejected(
                "monitor list refused: reader access changed",
            ));
        }
        let current_denied = self.monitor_denials_for(&current, params, peer_pid)?;
        for (id, resource) in &selected {
            let view = self.store.project_monitor_view(id)?;
            if monitor_resource(&view.monitor) != *resource
                || !monitor_visible(&current.scope, &current_denied, &view.coverage)
            {
                return Err(Error::rejected(
                    "monitor list refused: reader access changed",
                ));
            }
        }
        let mut out = json!({"monitors": monitors});
        if reader.scoped() {
            out[READER_SCOPE_FIELD] = json!(MONITOR_READER_SCOPE_ACK);
        }
        Ok(out)
    }

    pub(super) fn rpc_monitor_show(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        let verb = "monitor show";
        let id = required_str(params, "monitor")?;
        let reader = self.monitor_reader(params, peer_pid, verb)?;
        let view = self.monitor_view_for(&reader, params, peer_pid, id)?;
        // Acceptance seam: mutate authority between selection and response.
        #[cfg(test)]
        if let Some(hook) = &self.monitor_read_snapshot_hook {
            hook("show");
        }
        self.reprove_monitor_read(params, peer_pid, verb, &reader, &view)?;
        let coverage = view
            .coverage
            .into_iter()
            .map(|(id, _)| id)
            .collect::<Vec<_>>();
        let mut out = json!({
            "monitor": monitor_json(
                &view.monitor,
                &coverage,
                view.open_alerts,
                view.total_alerts,
                reader.scoped(),
            ),
        });
        if reader.scoped() {
            out[READER_SCOPE_FIELD] = json!(MONITOR_READER_SCOPE_ACK);
        }
        Ok(out)
    }

    /// `monitor heartbeat` — the monitor's own liveness ping. The
    /// operator, or the monitor's owner bound to a registration that
    /// predates the monitor (a re-registered alias inherits nothing,
    /// CAD-422).
    pub(super) fn rpc_monitor_heartbeat(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        reject_identity_fields(params, "monitor heartbeat")?;
        let id = required_str(params, "monitor")?;
        let caller = self.agent_caller(peer_pid, "monitor heartbeat")?;
        if let AgentCaller::Agent(alias) = &caller {
            let monitor = self.monitor_for_caller(&caller, id)?.monitor;
            let bound = self
                .store
                .agent_opt(&monitor.owner)?
                .filter(|row| row.created <= monitor.created)
                .map(|row| row.alias);
            if bound.as_deref() != Some(alias.as_str()) {
                return Err(Error::rejected(format!(
                    "monitor heartbeat refused: agent '{alias}' is not monitor \
                     '{id}'s owner ('{}') — the operator or that owner pings it \
                     (caller rule, CAD-422)",
                    monitor.owner
                )));
            }
        }
        let monitor = self.store.monitor_heartbeat(id)?;
        let (monitor, coverage, open, total) = self.store.monitor_view(&monitor.id)?;
        self.wake();
        Ok(json!({
            "monitor": monitor_json(
                &monitor,
                &coverage,
                open,
                total,
                matches!(&caller, AgentCaller::Agent(_)),
            ),
        }))
    }

    pub(super) fn rpc_monitor_alerts(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        let verb = "monitor alerts";
        let id = required_str(params, "monitor")?;
        let open_only = params.get("open").and_then(Value::as_bool).unwrap_or(false);
        let limit = optional_i64(params, "limit").unwrap_or(100);
        let reader = self.monitor_reader(params, peer_pid, verb)?;
        if !reader.scoped() {
            let view = self.monitor_view_for(&reader, params, peer_pid, id)?;
            // The legacy numeric continuation, preserved exactly for the
            // operator. A supplied non-numeric value is refused rather than
            // silently reset to the start of the feed.
            let after = match params.get("after") {
                None => 0,
                Some(value) => value
                    .as_i64()
                    .ok_or_else(|| Error::rejected("Event cursor must be a nonnegative integer"))?,
            };
            if after < 0 {
                return Err(Error::rejected("Event cursor must be nonnegative"));
            }
            let alerts = self.store.monitor_alerts(id, after, open_only, limit)?;
            self.reprove_monitor_read(params, peer_pid, verb, &reader, &view)?;
            let cursor = alerts.last().map(|alert| alert.seq).unwrap_or(after);
            let mut page = json!({
                "monitor": id,
                "alerts": alerts.iter().map(store::MonitorAlert::to_json).collect::<Vec<_>>(),
                "cursor": cursor,
            });
            if params.get("cursor_format").and_then(Value::as_str) == Some(MONITOR_CURSOR_FORMAT) {
                page[READER_SCOPE_FIELD] = json!(MONITOR_OPERATOR_SCOPE_ACK);
            }
            return Ok(page);
        }
        // The request shape is settled before the monitor or any alert is
        // read, so a downgrade attempt learns nothing.
        if params.get("cursor_format").and_then(Value::as_str) != Some(MONITOR_CURSOR_FORMAT) {
            return Err(cursor_format(verb));
        }
        let after = supplied_after(params);
        let supplied = match after {
            SuppliedAfter::Absent => None,
            SuppliedAfter::Text(token) => Some(token),
            SuppliedAfter::Number => return Err(numeric_cursor(verb)),
            SuppliedAfter::Unsupported => return Err(invalid_after(verb)),
        };
        let view = self.monitor_view_for(&reader, params, peer_pid, id)?;
        let resource = monitor_resource(&view.monitor);
        // Missing key material refuses a scoped read before any content; it
        // never downgrades to numeric positions.
        let codec = self.scoped_cursor_codec()?;
        let principal = reader
            .principal
            .as_deref()
            .expect("a scoped reader always has a principal");
        let position = match supplied {
            Some(token) => Some(
                codec
                    .open(token, &alerts_binding(principal, &resource, open_only))
                    .map_err(|_| invalid_cursor(verb))?,
            ),
            None => None,
        };
        let alerts = self
            .store
            .monitor_alerts(id, position.unwrap_or(0), open_only, limit)?;
        // Every alert a visible monitor produced belongs to its fixed, fully
        // admitted coverage (both producers refuse anything outside it); a
        // row outside the snapshot cannot be attributed to this reader and is
        // withheld rather than exposed.
        let coverage = view
            .coverage
            .iter()
            .map(|(task, _)| task.as_str())
            .collect::<std::collections::HashSet<_>>();
        let alerts = alerts
            .into_iter()
            .filter(|alert| coverage.contains(alert.task_id.as_str()))
            .collect::<Vec<_>>();
        // Acceptance seam: mutate authority between selection and response.
        #[cfg(test)]
        if let Some(hook) = &self.monitor_read_snapshot_hook {
            hook("alerts");
        }
        self.reprove_monitor_read(params, peer_pid, verb, &reader, &view)?;
        let mut page = scoped_alert_page(
            &codec, principal, &resource, &alerts, supplied, open_only, verb,
        )?;
        page["monitor"] = json!(id);
        Ok(page)
    }

    /// Capture the native identity evidence a scoped acknowledgement must
    /// re-check inside its mutation transaction. It deliberately records only
    /// what the caller-rule proof used (alias, nearest pane/enrollment and the
    /// registration uid) so ordinary concurrent row activity does not refuse.
    fn monitor_ack_identity(&self, peer_pid: u32, verb: &str) -> Result<MonitorAckIdentity> {
        let alias = match self.agent_caller(peer_pid, verb)? {
            AgentCaller::Agent(alias) => alias,
            AgentCaller::Operator => {
                return Err(Error::rejected(format!(
                    "{verb} refused: a native agent identity is required"
                )))
            }
        };
        let chain = if crate::test_seam::asserted().is_some() {
            vec![peer_pid]
        } else {
            crate::adapter::pty::caller_chain(peer_pid).ok_or_else(|| {
                Error::rejected(format!(
                    "{verb} refused: /proc ancestry unreadable — caller identity underivable"
                ))
            })?
        };
        let slot = self.slot_identity(peer_pid)?.ok_or_else(|| {
            Error::rejected(format!(
                "{verb} refused: the caller derives no native agent identity"
            ))
        })?;
        let agent = self.store.agent(&alias)?;
        let pane = self
            .store
            .pty_pane_pids()?
            .into_iter()
            .find(|row| row.0 == alias)
            .map(|row| (row.1, row.2));
        Ok(MonitorAckIdentity {
            peer_pid,
            alias,
            chain,
            slot,
            uid: agent.uid,
            pane,
        })
    }

    /// Re-run the native identity decision inside the acknowledgement write
    /// transaction. `slots` is held by the caller; this helper must not call
    /// `agent_caller`, `slot_identity`, `revalidate_enrollments` or any store
    /// accessor that reacquires the store/slots mutex.
    fn revalidate_monitor_ack_in(
        &self,
        tx: &impl crate::store::StoreConn,
        slots: &mut crate::slots::Slots,
        evidence: &MonitorAckIdentity,
        by: &str,
    ) -> Result<()> {
        let changed =
            || Error::rejected("monitor alert acknowledgement refused: reader access changed");
        if evidence.alias != by {
            return Err(changed());
        }
        if let Some(asserted) = crate::test_seam::asserted() {
            match asserted {
                crate::test_seam::Asserted::Agent(alias) if alias == evidence.alias => {}
                _ => return Err(changed()),
            }
        } else {
            let pane_rows = self.store.pty_pane_pids_in(tx)?;
            if let Some((pid, start)) = evidence.pane {
                let current = pane_rows
                    .iter()
                    .find(|row| row.0 == evidence.alias)
                    .map(|row| (row.1, row.2));
                if current != Some((pid, start)) {
                    return Err(changed());
                }
            }
            let recorded = crate::peer::AgentPids::classify(pane_rows);
            recorded
                .refuse_unproven_on(&evidence.chain)
                .map_err(|why| {
                    Error::rejected(format!("monitor alert acknowledgement refused: {why}"))
                })?;
            let panes = recorded.live();
            let current =
                Self::slot_identity_from(evidence.peer_pid, &evidence.chain, &panes, slots)
                    .map_err(|_| changed())?
                    .ok_or_else(changed)?;
            if current.lane() != evidence.alias {
                return Err(changed());
            }
            match (&evidence.slot, &current) {
                (super::identity::SlotWho::Pane { .. }, super::identity::SlotWho::Pane { .. }) => {}
                (
                    super::identity::SlotWho::Strict(before),
                    super::identity::SlotWho::Strict(now),
                ) => {
                    if before.enrollment_id != now.enrollment_id {
                        return Err(changed());
                    }
                    let enrollment = slots
                        .endpoint_enrollment(&now.enrollment_id)
                        .cloned()
                        .ok_or_else(changed)?;
                    let agent = self.store.agent_in(tx, &evidence.alias)?;
                    if enrollment.owner_actor != evidence.alias
                        || super::identity::owner_generation(&agent).as_deref()
                            != Some(enrollment.owner_generation.as_str())
                        || agent.pid != Some(i64::from(enrollment.root.pid))
                        || agent.pid_start.and_then(|start| u64::try_from(start).ok())
                            != Some(enrollment.root.starttime)
                    {
                        return Err(changed());
                    }
                }
                _ => return Err(changed()),
            }
        }
        let agent = self.store.agent_in(tx, &evidence.alias)?;
        if agent.uid != evidence.uid {
            return Err(changed());
        }
        Ok(())
    }

    pub(super) fn rpc_monitor_alert_ack(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        let verb = "monitor alert acknowledgement";
        // Read-relay proofs establish read authority, not permission to
        // acknowledge. This write is attributed to the native peer.
        if ["public_session", "read_as", "cli_envelope"]
            .iter()
            .any(|key| params.get(*key).is_some())
        {
            return Err(Error::rejected(
                "monitor alert acknowledgement refused: relayed read identity cannot authorize a write",
            ));
        }
        let id = required_str(params, "monitor")?;
        let by = required_str(params, "by")?;
        let reader = self.monitor_reader(params, peer_pid, verb)?;
        if !reader.scoped() {
            // The operator's legacy numeric acknowledgement, unchanged.
            self.monitor_view_for(&reader, params, peer_pid, id)?;
            let seq = optional_i64(params, "alert")
                .ok_or_else(|| Error::rejected("Monitor alert acknowledgement requires --alert"))?;
            let alert = self.store.ack_monitor_alert(id, seq, by)?;
            self.wake();
            return Ok(json!({"alert": alert.to_json()}));
        }
        // A scoped acknowledgement is a token, never a numeric position; the
        // shape is settled before the monitor or alert is read.
        if params.get("alert").is_some() {
            return Err(numeric_ack(verb));
        }
        let token = params
            .get("ack")
            .and_then(Value::as_str)
            .filter(|token| !token.is_empty())
            .ok_or_else(|| invalid_ack(verb))?;
        let view = self.monitor_view_for(&reader, params, peer_pid, id)?;
        let resource = monitor_resource(&view.monitor);
        // Pin the native identity before taking the tracker guard; everything
        // below re-checks it against the same transaction that mutates the
        // alert, so a revocation cannot commit in between.
        let identity = self.monitor_ack_identity(peer_pid, verb)?;
        // The token must name the SAME durable registration the preflight
        // reader resolved: a re-registered alias is a new uid and refuses.
        let principal = job_events_opaque::agent_principal(&identity.uid);
        if reader.principal.as_deref() != Some(principal.as_str()) {
            return Err(Error::rejected(
                "monitor alert acknowledgement refused: registration changed",
            ));
        }
        // Missing key material refuses before the token is interpreted.
        let codec = self.scoped_cursor_codec()?;
        let seq = codec
            .open(token, &ack_binding(&principal, &resource))
            .map_err(|_| invalid_ack(verb))?;
        // Acceptance seam: mutate authority between selection and response.
        #[cfg(test)]
        if let Some(hook) = &self.monitor_read_snapshot_hook {
            hook("ack");
        }
        let pm = self.pm()?;
        let _tracker = pm.lock()?;
        let actor = format!("agents/{}", identity.alias);
        let scope = self.project_scope_for_actor(&actor)?;
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        // Main's test seam: no store connection/tx is held here.
        #[cfg(test)]
        if let Some(hook) = &self.monitor_ack_writer_hook {
            hook();
        }
        let out = self.store.write_tx(|tx| {
            self.revalidate_monitor_ack_in(&*tx, &mut slots, &identity, by)?;
            let current = self.store.project_monitor_view_in(&*tx, id)?;
            if monitor_resource(&current.monitor) != resource {
                return Err(Error::rejected(
                    "monitor alert acknowledgement refused: monitor identity changed",
                ));
            }
            let denied = self
                .store
                .private_job_denials_in(&*tx, store::PrivateJobReader::Agent(&identity.alias))?;
            if !monitor_visible(&scope, &denied, &current.coverage) {
                return Err(Error::rejected(
                    "monitor alert acknowledgement refused: reader access changed",
                ));
            }
            let alert = self.store.monitor_alert_in(&*tx, seq)?;
            if !current
                .coverage
                .iter()
                .any(|(task, _)| task == &alert.task_id)
            {
                return Err(Error::rejected(
                    "monitor alert acknowledgement refused: alert is outside current coverage",
                ));
            }
            let alert = self.store.ack_monitor_alert_in(&*tx, id, seq, by)?;
            let row = scoped_alert_json(&codec, &principal, &resource, &alert)?;
            let mut out = json!({"alert": row});
            out[READER_SCOPE_FIELD] = json!(MONITOR_READER_SCOPE_ACK);
            Ok(out)
        })?;
        drop(slots);
        drop(_tracker);
        self.wake();
        Ok(out)
    }

    /// `monitor stop` — operator only, by the connection (CAD-373).
    pub(super) fn rpc_monitor_stop(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        self.operator_connection("monitor stop", params, peer_pid)?;
        let id = required_str(params, "monitor")?;
        let monitor = self.store.stop_monitor(id)?;
        let (monitor, coverage, open, total) = self.store.monitor_view(&monitor.id)?;
        self.wake();
        Ok(json!({"monitor": monitor.to_json(&coverage, open, total)}))
    }

    /// One guarded handoff into the existing job-dispatch transaction. The
    /// public RPC remains an explicit operator action — proven from the
    /// connection (CAD-373), never from a `pane` field; the monitor
    /// watcher may call the same helper only for a separately persisted
    /// automatic opt-in.
    pub(super) fn rpc_monitor_dispatch(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        self.operator_connection("monitor dispatch", params, peer_pid)?;
        let monitor_id = required_str(params, "monitor")?;
        let task_id = required_str(params, "task")?;
        self.monitor_dispatch_task(monitor_id, task_id, false)
    }

    pub(super) fn monitor_dispatch_task(
        self: &Arc<Self>,
        monitor_id: &str,
        task_id: &str,
        automatic: bool,
    ) -> Result<Value> {
        let _grant = self
            .unattended_lock
            .read()
            .unwrap_or_else(|e| e.into_inner());
        if automatic {
            self.plan_gate_task(task_id)?;
            // CAD-1846: capture the exact planned worker/revision/message and
            // prepare the issue-linked job context outside the write
            // transaction. The monitor's own coverage/readiness/quota/queue
            // gates stay in the store's admission transaction, unchanged.
            let plan = self.store.plan_task_dispatch(task_id, None, None)?;
            if matches!(plan.task.state.as_str(), "dispatched" | "running") && !plan.duplicate {
                return Err(Error::rejected(format!(
                    "Task '{task_id}' has no live kickoff — only draft or revising \
                     tasks are eligible"
                )));
            }
            // An accepted live kickoff keeps its original selection and never
            // recaptures moved sources; a fresh kickoff prepares them here.
            let delivery = if plan.duplicate {
                None
            } else {
                // CAD-622: the automatic watcher is an authoring kickoff
                // too — the job's PM and the planned worker are the
                // authors, and the guard runs before the queue write.
                self.native_app_guard(&plan, None)?;
                Some(self.prepare_job_delivery(&plan, false)?)
            };
            // Hold the pending-request mutex across the store transaction.
            // The snapshot contains every alias, while the transaction
            // re-reads the task's current assignee before applying it, so an
            // approval arriving concurrently cannot be missed or bypassed.
            let pending = self.pending.lock().unwrap();
            let pending_aliases: HashSet<String> = pending
                .values()
                .map(|request| request.alias.clone())
                .collect();
            let (task, message, duplicate, behind_dead) =
                self.store.dispatch_automatic_monitor_task_planned(
                    monitor_id,
                    &plan,
                    delivery,
                    &pending_aliases,
                    &format!("monitor:{monitor_id}"),
                    None,
                )?;
            drop(pending);
            let assignee = task.assignee.clone().ok_or_else(|| {
                Error::internal("automatic dispatch returned a task without an assignee")
            })?;
            let _ = self.store.event_public(
                store::Store::DAEMON_STREAM,
                "monitor_dispatch",
                json!({"monitor": monitor_id, "task": task_id,
                       "message": message, "duplicate": duplicate,
                       "queued_behind_dead": behind_dead,
                       "automatic": true}),
            );
            self.notify_agent(&assignee);
            self.wake();
            return Ok(json!({
                "monitor": monitor_id,
                "task": task.to_json(),
                "message": message,
                "duplicate": duplicate,
                "queued_behind_dead": behind_dead,
            }));
        }
        let monitor = self.store.monitor(monitor_id)?;
        if monitor.state != "active" {
            return Err(Error::rejected(format!(
                "Monitor '{monitor_id}' is {} — dispatch requires an active check",
                monitor.state
            )));
        }
        if !monitor.dispatch_enabled {
            return Err(Error::rejected(format!(
                "Monitor '{monitor_id}' has dispatch disabled — enable it explicitly at registration"
            )));
        }
        if !self.store.monitor_is_covered(monitor_id, task_id)? {
            return Err(Error::rejected(format!(
                "Task '{task_id}' is outside monitor '{monitor_id}' coverage"
            )));
        }
        let task = self.store.task(task_id)?;
        let job = self.store.job(&task.job_id)?;
        self.plan_gate_task(task_id)?;
        if job.state != "open" || job.repo.as_deref() != Some(monitor.project.as_str()) {
            return Err(Error::rejected(format!(
                "Task '{task_id}' is not in monitor project '{}' with an open job",
                monitor.project
            )));
        }
        // A retry of a kickoff already claimed by the worker must reuse the
        // existing message atomically, even though the worker is now busy.
        // The duplicate-only store branch never mints a new revision.
        if matches!(task.state.as_str(), "dispatched" | "running") {
            if let Some((task, message, duplicate, behind_dead)) =
                self.store.duplicate_task_dispatch(task_id)?
            {
                self.store.resolve_monitor_dispatch_blocked(
                    monitor_id,
                    task_id,
                    epoch_secs(),
                    "monitor_dispatch",
                )?;
                // The durable job-dispatch row is already the idempotency
                // evidence for an automatic retry. Manual RPC callers keep
                // their historical event for every explicit invocation;
                // the watcher must not emit one event per interval.
                if !automatic {
                    let _ = self.store.event_public(
                        store::Store::DAEMON_STREAM,
                        "monitor_dispatch",
                        json!({"monitor": monitor_id, "task": task_id,
                               "message": message, "duplicate": duplicate,
                               "queued_behind_dead": behind_dead,
                               "automatic": false}),
                    );
                }
                self.wake();
                return Ok(json!({
                    "monitor": monitor_id,
                    "task": task.to_json(),
                    "message": message,
                    "duplicate": duplicate,
                    "queued_behind_dead": behind_dead,
                }));
            }
            return Err(Error::rejected(format!(
                "Task '{task_id}' has no live kickoff — only draft or revising tasks are eligible"
            )));
        }
        if !matches!(task.state.as_str(), "draft" | "revising") {
            return Err(Error::rejected(format!(
                "Task '{task_id}' is '{}' — only draft or revising tasks are eligible",
                task.state
            )));
        }
        if task
            .acceptance
            .as_deref()
            .is_none_or(|s| s.trim().is_empty())
        {
            return Err(Error::rejected(format!(
                "Task '{task_id}' has no acceptance criteria — dispatch is refused"
            )));
        }
        let assignee = task
            .assignee
            .as_deref()
            .ok_or_else(|| Error::rejected(format!("Task '{task_id}' has no explicit assignee")))?;
        let agent = self.store.agent(assignee)?;
        if !registry::has_actor(&agent.provider, &agent.endpoint_kind) {
            return Err(Error::rejected(format!(
                "Assignee '{assignee}' is a mailbox, not a dispatchable worker"
            )));
        }
        // The fake provider is an in-process fixture and deliberately has no
        // transport endpoint. Every real actor publishes one when open.
        let live_endpoint =
            agent.endpoint.is_some() || (agent.provider == "fake" && agent.endpoint_kind == "fake");
        if !agent.enabled || !live_endpoint || agent.state != "idle" {
            return Err(Error::rejected(format!(
                "Assignee '{assignee}' is not demonstrably idle and live (state {}, endpoint {})",
                agent.state, live_endpoint
            )));
        }
        if registry::ready_gate(&agent.provider, &agent.endpoint_kind)
            && agent
                .params
                .as_ref()
                .and_then(|p| p.get("auto_ready"))
                .and_then(Value::as_str)
                != Some("verified")
        {
            return Err(Error::rejected(format!(
                "Assignee '{assignee}' requires an explicit readiness claim; automatic dispatch is refused"
            )));
        }
        if self
            .pending
            .lock()
            .unwrap()
            .values()
            .any(|request| request.alias == assignee)
        {
            return Err(Error::rejected(format!(
                "Assignee '{assignee}' is waiting on an approval request"
            )));
        }
        if self.store.queued_count(assignee)? > 0 {
            return Err(Error::rejected(format!(
                "Assignee '{assignee}' has queued work; dispatch is refused"
            )));
        }
        let unfinished = self.store.tasks_for_assignee(assignee)?;
        if unfinished.iter().any(|other| other.id != task_id) {
            return Err(Error::rejected(format!(
                "Assignee '{assignee}' already has unfinished task work"
            )));
        }
        // CAD-1846: the same planned preparation and admission as an explicit
        // `task dispatch`, with the monitor's pre-transaction readiness gates
        // above intact. The store re-fences the task, job and exact worker;
        // an accepted live kickoff could not reach here (it returned above).
        let plan = self.store.plan_task_dispatch(task_id, None, None)?;
        // CAD-622: the explicit monitor handoff is the same fresh kickoff —
        // guard it before the queue write.
        self.native_app_guard(&plan, None)?;
        let delivery = Some(self.prepare_job_delivery(&plan, false)?);
        let (task, message, duplicate, behind_dead) = self.store.dispatch_task_planned(
            &plan,
            delivery,
            &format!("monitor:{monitor_id}"),
            None,
            None,
            None,
        )?;
        self.store.resolve_monitor_dispatch_blocked(
            monitor_id,
            task_id,
            epoch_secs(),
            "monitor_dispatch",
        )?;
        let _ = self.store.event_public(
            store::Store::DAEMON_STREAM,
            "monitor_dispatch",
            json!({"monitor": monitor_id, "task": task_id,
                   "message": message, "duplicate": duplicate,
                   "queued_behind_dead": behind_dead,
                   "automatic": automatic}),
        );
        self.notify_agent(assignee);
        self.wake();
        Ok(json!({
            "monitor": monitor_id,
            "task": task.to_json(),
            "message": message,
            "duplicate": duplicate,
            "queued_behind_dead": behind_dead,
        }))
    }
}
