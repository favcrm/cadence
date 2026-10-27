//! CAD-1853: daemon RPC for the explicit cloud-agent connection.
//!
//! One stable cloud-chat target per alias. The caller is the proven operator
//! or exactly one verified local agent; the daemon derives its immutable
//! source (agent UID + registration epoch) and checks that source's explicit
//! grant. A request field never names a sender, role, tenant, session, owner
//! or remote id. Live credential and scope are re-checked immediately before
//! every transport call that can generate or disclose a model turn, including
//! a queued or recovered one.
//!
//! Remote calls use the real published HTTP contract
//! ([`crate::cloud_agent::wire::HostedCloudTransport`]) with the saved
//! hosted-cadence login; no credential value is read by a caller, placed in a
//! request body or returned.

use super::*;
use crate::cloud_agent::{
    self, wire::HostedCloudTransport, CloudTransport, Sender, Source, SCOPE_READ, SCOPE_WRITE,
};
use crate::remote_cli::RemoteTarget;
use crate::store::cloud_agent::{CloudSource, SOURCE_PEER, SOURCE_USER};

fn allow(params: &Value, keys: &[&str], verb: &str) -> Result<()> {
    let object = params
        .as_object()
        .ok_or_else(|| Error::rejected(format!("{verb} parameters must be an object")))?;
    if let Some(key) = object.keys().find(|key| !keys.contains(&key.as_str())) {
        return Err(Error::invalid(
            "cloud_agent_identity_field",
            format!(
                "{verb} refused: '{key}' is not a transport field — sender, role, tenant, \
                 session and remote ids are derived by the daemon, never supplied"
            ),
        ));
    }
    Ok(())
}

fn cloud_agent_id(params: &Value) -> Result<&str> {
    let id = required_str(params, "cloud_agent_id")?;
    if id.len() < 8 || id.len() > 64 || !id.starts_with("cld-") {
        return Err(Error::invalid(
            "cloud_agent_invalid_id",
            "cloud_agent_id must be a local cloud-agent id",
        ));
    }
    Ok(id)
}

/// What one admission covers for one operation, fixed when the read began.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Admission {
    /// The operation had settled before this read was admitted, and — for the
    /// one operation the platform was asked about — the platform's own answer
    /// is terminal-completed. Its whole durable row set may be projected.
    Settled,
    /// The operation was admitted but had NOT settled when this read began.
    /// Only the caller's own request text may be projected: a settlement that
    /// lands later is never served under an admission that did not cover it.
    Request,
}

/// Only cloud-scope refusals and remote uncertainty become incomplete
/// history. Identity, isolation, execution and local storage errors propagate.
fn projection_denial(error: Error) -> Result<String> {
    match error.code() {
        Some(
            code @ ("cloud_agent_not_connected"
            | "cloud_agent_consent_required"
            | "cloud_agent_scope_revoked"
            | "cloud_agent_unauthorized"
            | "cloud_agent_unavailable"
            | "cloud_agent_uncertain"
            | "cloud_agent_receipt_conflict"
            | "cloud_agent_reply_missing"
            | "cloud_agent_authority_missing"),
        ) => Ok(code.to_string()),
        _ if matches!(error, Error::OutcomeUnknown(_)) => Ok("cloud_agent_uncertain".to_string()),
        _ => Err(error),
    }
}

/// Distinguish transport uncertainty from a local receipt-write failure that
/// the dedicated read path may also wrap as `cloud_agent_uncertain`.
struct ProjectionReadTransport {
    remote_failed: std::cell::Cell<bool>,
}

impl CloudTransport for ProjectionReadTransport {
    fn connect(
        &self,
        target: &RemoteTarget,
        conversation_id: &str,
        source: Option<&Source>,
    ) -> Result<cloud_agent::RemoteAgent> {
        let answer = HostedCloudTransport.connect(target, conversation_id, source);
        self.remote_failed.set(answer.is_err());
        answer
    }

    fn submit(
        &self,
        target: &RemoteTarget,
        request: &cloud_agent::SubmitRequest<'_>,
    ) -> Result<cloud_agent::RemoteOperation> {
        HostedCloudTransport.submit(target, request)
    }

    fn read(
        &self,
        target: &RemoteTarget,
        conversation_id: &str,
        operation_id: &str,
        source: &Source,
    ) -> Result<cloud_agent::RemoteOperation> {
        let answer = HostedCloudTransport.read(target, conversation_id, operation_id, source);
        self.remote_failed.set(answer.is_err());
        answer
    }

    fn cancel(
        &self,
        target: &RemoteTarget,
        conversation_id: &str,
        operation_id: &str,
        source: &Source,
    ) -> Result<()> {
        HostedCloudTransport.cancel(target, conversation_id, operation_id, source)
    }
}

/// What a single row may project under this admission.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Row {
    /// Project the row whole.
    Full,
    /// Project only the caller's own request text (an unsettled operation).
    Request,
    /// Never project this row.
    Never,
}

/// What the platform itself reported for the one operation an admission
/// asked about.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PlatformState {
    /// Terminal-completed: the stored receipt is the caller's own settled
    /// content, so it may be projected.
    Completed,
    /// Nonterminal or failed — or a settled receipt its terminal answer
    /// contradicts, which the read above refuses outright. Either way no
    /// stored reply text is presented.
    Unsettled,
}

/// One cloud connection's admitted operation states.
struct CloudScope {
    cloud_agent_id: String,
    source: Source,
    admitted: std::collections::BTreeMap<String, Admission>,
}

/// The caller's own admitted view of one alias.
///
/// THE RULE, enforced by construction in [`CloudProjection::row`]: a
/// projection renders a row only if that row was affirmatively admitted in a
/// snapshot taken when the read began — BEFORE any authority proof — and the
/// projection re-resolves that snapshot whenever an observation names
/// anything outside it ([`CloudProjection::needs_readmission`]). Nothing is
/// admitted by default: an unknown message id, a row that appeared during a
/// wait, and a derived row that resolved after admission are all withheld
/// until a fresh admission names them, so a read can never widen the set of
/// rows it renders beyond what authority was proven for.
///
/// A cloud alias carries the caller's own source per connection, and every
/// projected row must resolve to an operation that source was admitted for,
/// in the state the admission covered. An ordinary local alias projects
/// unchanged — except for cloud-DERIVED rows (a routed `worker_result` whose
/// immutable origin reaches a cloud request/result): those are cloud content
/// off the cloud alias too. Delivery-bound UIDs (or the operator) still need
/// current read admission on each backing connection.
pub(super) struct CloudProjection {
    /// A denied cloud alias must not become an ordinary empty snapshot.
    cloud_alias: bool,
    /// Observed cloud ids, including denied ones. New ids require readmission;
    /// already-denied ids do not trigger a second proof in the same call.
    known: std::collections::BTreeSet<String>,
    scopes: Vec<CloudScope>,
    /// Cloud-derived rows on an ordinary alias by message id, with the
    /// delivered UID binding.
    derived: std::collections::HashMap<String, crate::store::cloud_agent::CloudDerivedRow>,
    /// Current read admission, separate from immutable delivery provenance.
    admitted_origins: std::collections::BTreeSet<String>,
    /// The ordinary mail of an ordinary alias, enumerated in the same
    /// snapshot. This is what makes that alias' own rows admitted explicitly
    /// rather than by absence from [`Self::derived`]. Empty when this
    /// projection is unscoped (the alias carries no cloud content at all) or
    /// scoped (a cloud alias' rows route through [`Self::scopes`]).
    ordinary: std::collections::BTreeSet<String>,
    /// The caller's verified agent UID, when the connection derives one.
    viewer: Option<String>,
    /// The connection is provably the operator.
    operator: bool,
    /// Why cloud content was withheld for this read, if it was. The read still
    /// succeeds and the page carries this, so a reader is told the cloud
    /// history is incomplete rather than shown an empty one as if complete.
    pub(super) withheld: Option<String>,
}

impl Default for CloudProjection {
    /// Nothing admitted: every row withholds. The fail-closed projection for
    /// content that cannot be classified at all.
    fn default() -> Self {
        Self {
            cloud_alias: true,
            known: std::collections::BTreeSet::new(),
            scopes: Vec::new(),
            derived: std::collections::HashMap::new(),
            admitted_origins: std::collections::BTreeSet::new(),
            ordinary: std::collections::BTreeSet::new(),
            viewer: None,
            operator: false,
            withheld: None,
        }
    }
}

impl CloudProjection {
    /// A cloud alias, admitted per operation for this caller's own source.
    fn cloud(scopes: Vec<CloudScope>, known: std::collections::BTreeSet<String>) -> Self {
        Self {
            cloud_alias: true,
            known,
            scopes,
            derived: std::collections::HashMap::new(),
            admitted_origins: std::collections::BTreeSet::new(),
            ordinary: std::collections::BTreeSet::new(),
            viewer: None,
            operator: false,
            withheld: None,
        }
    }

    /// An ordinary alias holding cloud-derived rows: those rows are bound to
    /// the UIDs their delivery carried, and the alias' own mail is admitted
    /// by enumeration — the snapshot names exactly the rows that were
    /// ordinary when the read began.
    fn derived_rows(
        rows: Vec<crate::store::cloud_agent::CloudDerivedRow>,
        ordinary: Vec<String>,
        viewer: Option<String>,
        operator: bool,
    ) -> Self {
        Self {
            cloud_alias: false,
            known: std::collections::BTreeSet::new(),
            scopes: Vec::new(),
            derived: rows
                .into_iter()
                .map(|row| (row.message_id.clone(), row))
                .collect(),
            admitted_origins: std::collections::BTreeSet::new(),
            ordinary: ordinary.into_iter().collect(),
            viewer,
            operator,
            withheld: None,
        }
    }

    /// The admission covering the operation that a projected row's canonical
    /// message id names, on this caller's own source. The operation row is the
    /// attribution witness: the cloud request/reply carries no caller-supplied
    /// owner, and the canonical message id IS the operation id. `None` means
    /// the row is not the caller's own admitted operation.
    fn admission(&self, message_id: &str) -> Option<Admission> {
        self.scopes
            .iter()
            .find_map(|scope| scope.admitted.get(message_id).copied())
    }

    /// Whether the delivery binding of a derived row reaches this caller.
    /// THE rule for a routed cloud row: the operator, or a verified agent
    /// that is the delivery's sender or recipient. Every reader of such a row
    /// asks this, directly or through [`Self::bound`], so the two cannot
    /// drift into disagreeing about who may read a cloud result.
    fn cloud_row_readable(&self, sender_uid: Option<&str>, recipient_uid: Option<&str>) -> bool {
        if self.operator {
            return true;
        }
        let Some(viewer) = self.viewer.as_deref() else {
            return false;
        };
        sender_uid == Some(viewer) || recipient_uid == Some(viewer)
    }

    fn bound(&self, row: &crate::store::cloud_agent::CloudDerivedRow) -> bool {
        self.cloud_row_readable(row.sender_uid.as_deref(), row.recipient_uid.as_deref())
    }

    /// What this snapshot admits for one message id. Default DENY: a row the
    /// snapshot did not name is withheld, never assumed ordinary — an id
    /// absent from the derived map may be a routed cloud result that resolved
    /// after admission, and rendering it would hand another source's reply to
    /// whoever asks.
    fn row(&self, message_id: &str) -> Row {
        if self.cloud_alias {
            return match self.admission(message_id) {
                Some(Admission::Settled) => Row::Full,
                Some(Admission::Request) => Row::Request,
                None => Row::Never,
            };
        }
        if let Some(row) = self.derived.get(message_id) {
            return if self.bound(row)
                && row
                    .cloud_agent_id
                    .as_ref()
                    .is_some_and(|id| self.admitted_origins.contains(id))
            {
                Row::Full
            } else {
                Row::Never
            };
        }
        // An ordinary alias: only the mail this snapshot enumerated.
        if self.ordinary.contains(message_id) {
            Row::Full
        } else {
            Row::Never
        }
    }

    /// Whether this observation names anything the admission snapshot does
    /// not cover, so the read must re-resolve the whole page's admission
    /// before projecting it.
    ///
    /// Two things land here. On a cloud alias, a row of the caller's own
    /// source that settled after the snapshot was taken. On an ordinary
    /// alias, an id the snapshot enumerates neither as ordinary mail nor as a
    /// bound derived row — which is exactly what a routed cloud result
    /// resolving after admission (or mail arriving during a wait) looks like.
    /// Either way the caller has not been admitted for it, so a read that
    /// projects it under the old snapshot would widen its own view beyond
    /// what authority was proven for.
    pub(super) fn needs_readmission<'a>(
        &self,
        store: &crate::store::Store,
        ids: impl IntoIterator<Item = &'a str>,
    ) -> Result<bool> {
        // Empty snapshots never become allow-all. Known denials remain
        // denials for this invocation; only newly observed ids or a
        // request-only operation that has since settled require readmission.
        for id in ids {
            if !self.cloud_alias {
                if !self.ordinary.contains(id) && !self.derived.contains_key(id) {
                    return Ok(true);
                }
                continue;
            }
            if !self.known.contains(id) {
                return Ok(true);
            }
            for scope in &self.scopes {
                let Some(operation) = store.cloud_agent_operation(&scope.cloud_agent_id, id)?
                else {
                    continue;
                };
                if !scope.source.matches(&operation) {
                    continue;
                }
                let covered = match scope.admitted.get(id) {
                    Some(Admission::Settled) => true,
                    Some(Admission::Request) => !operation.settled(),
                    None => true,
                };
                if !covered {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    pub(super) fn entries(
        &self,
        _store: &crate::store::Store,
        entries: Vec<crate::store::ThreadEntry>,
    ) -> Result<Vec<crate::store::ThreadEntry>> {
        let mut kept = Vec::with_capacity(entries.len());
        for entry in entries {
            let row = match entry.message_id.as_deref() {
                Some(id) => self.row(id),
                // A cloud thread carries no unattributable entry: an entry
                // with no canonical message cannot be shown to be the
                // caller's own. An ordinary alias keeps its own.
                None if !self.cloud_alias => Row::Full,
                None => Row::Never,
            };
            let keep = match row {
                Row::Full => true,
                Row::Request => entry.kind == crate::store::KIND_MESSAGE,
                Row::Never => false,
            };
            if keep {
                // Mark cloud content where it lives, so a reader that is not
                // the operator can drop it without having to see the request
                // and the reply on the same page. The board layer enforces the
                // member rule; it cannot see the daemon's admission.
                let mut entry = entry;
                let origin = entry.message_id.as_deref().and_then(|id| {
                    if self.cloud_alias {
                        self.scopes
                            .iter()
                            .find(|scope| scope.admitted.contains_key(id))
                            .map(|scope| scope.cloud_agent_id.as_str())
                    } else {
                        self.derived
                            .get(id)
                            .and_then(|row| row.cloud_agent_id.as_deref())
                    }
                });
                if let Some(origin) = origin {
                    let mut payload = match entry.payload.take() {
                        Some(Value::Object(payload)) => payload,
                        _ => serde_json::Map::new(),
                    };
                    payload.insert("cloud_agent".to_string(), Value::String(origin.to_string()));
                    entry.payload = Some(Value::Object(payload));
                }
                kept.push(entry);
            }
        }
        Ok(kept)
    }

    pub(super) fn messages(
        &self,
        _store: &crate::store::Store,
        messages: Vec<crate::store::Message>,
    ) -> Result<Vec<crate::store::Message>> {
        let mut kept = Vec::with_capacity(messages.len());
        for mut message in messages {
            match self.row(&message.id) {
                Row::Full => kept.push(message),
                Row::Request => {
                    // The request is the caller's own text; the settled
                    // receipt on the same row is not.
                    message.result = None;
                    message.error = None;
                    kept.push(message);
                }
                Row::Never => {}
            }
        }
        Ok(kept)
    }

    pub(super) fn events(
        &self,
        _store: &crate::store::Store,
        events: Vec<crate::store::Event>,
    ) -> Result<Vec<crate::store::Event>> {
        let mut kept = Vec::with_capacity(events.len());
        for event in events {
            let row = match event.payload.get("message").and_then(Value::as_str) {
                Some(id) => self.row(id),
                // Alias-level metadata (the connection itself) carries no
                // request or result text; the read authority above already
                // admitted the caller to this connection.
                None => Row::Full,
            };
            let keep = match row {
                Row::Full => true,
                // An unsettled operation keeps only its non-result events:
                // the settlement payload is exactly what must not be shown.
                Row::Request => event.payload.get("result").is_none(),
                Row::Never => false,
            };
            if keep {
                kept.push(event);
            }
        }
        Ok(kept)
    }
}

impl Shared {
    fn cloud_agent_target(agent: &crate::store::CloudAgent) -> RemoteTarget {
        Self::cloud_agent_target_for(agent, None)
    }

    /// The transport target for one admitted source. A user source pins the
    /// verified issuer+subject, so the credential load that supplies the
    /// HTTP token ALSO re-proves the owner — a login replaced between the
    /// daemon's check and the request is refused, never charged.
    fn cloud_agent_target_for(
        agent: &crate::store::CloudAgent,
        source: Option<&Source>,
    ) -> RemoteTarget {
        let identity = source
            .filter(|source| source.kind == SOURCE_USER)
            .map(|source| crate::remote_cli::CliIdentityPin {
                subject_id: source.owner.subject.clone(),
                issuer: source.owner.issuer.clone(),
            });
        RemoteTarget {
            org: agent.org_slug.clone(),
            endpoint: agent.endpoint.clone(),
            org_id: agent.organization_id.clone(),
            identity,
        }
    }

    /// Peer eligibility uses the actual verified split-UID boundary, not a
    /// config flag alone: the socket peer UID must be the configured agent
    /// UID (a same-UID owner connection can never act as a peer), and the
    /// caller must still be a verified pane/managed endpoint (agent_caller
    /// already proved pid/start identity). Same-UID mode fails closed.
    fn cloud_peer_isolation(&self, peer_pid: u32) -> Result<()> {
        let agent_uid = self.agent_uid.ok_or_else(|| {
            Error::gate_coded(
                "cloud_agent_peer_isolation_required",
                "cloud-peer turns require the split agent UID boundary; this daemon runs \
                 same-UID, where 0600 files do not isolate one managed peer from another \
                 — nothing was sent",
            )
        })?;
        let socket_uid = super::serve::frame_peer_uid(Some(agent_uid))?;
        if socket_uid != agent_uid {
            return Err(Error::gate_coded(
                "cloud_agent_peer_isolation_required",
                "this connection is not on the split agent UID boundary",
            ));
        }
        if self.slot_identity(peer_pid)?.is_none() {
            return Err(Error::gate_coded(
                "cloud_agent_peer_isolation_required",
                "the caller is not a verified managed agent endpoint",
            ));
        }
        Ok(())
    }

    fn cloud_agent_source_active(
        &self,
        agent: &crate::store::CloudAgent,
        source: &Source,
    ) -> Result<()> {
        let grant = match self.store.cloud_agent_source(
            &agent.id,
            source.kind,
            &source.uid,
            source.epoch,
            &source.owner,
        )? {
            Some(grant) => grant,
            None => {
                // A different UID, epoch or logical owner on the same alias
                // is a removed, re-registered or re-enrolled source: its old
                // consent and provider sessions are not inherited — refused
                // before any network call or cached read.
                let stale =
                    self.store
                        .cloud_agent_sources(&agent.id)?
                        .into_iter()
                        .any(|existing| {
                            existing.kind == source.kind
                                && (existing.uid == source.uid
                                    || existing.registration_epoch == source.epoch
                                    || existing.owner.subject == source.owner.subject)
                        });
                return Err(if stale {
                    Error::gate_coded(
                        "cloud_agent_source_stale",
                        "this invoker's registration generation or verified owner changed; \
                         its old consent, provider sessions and cached replies are not \
                         inherited — enroll the current agent explicitly",
                    )
                } else {
                    Error::gate_coded(
                        "cloud_agent_consent_required",
                        "this invoker source has no explicit cloud-agent consent on this target",
                    )
                });
            }
        };
        if !grant.active() {
            return Err(Error::gate_coded(
                "cloud_agent_consent_required",
                "this invoker source's cloud-agent consent was revoked",
            ));
        }
        Ok(())
    }

    /// The closure every transport call runs immediately before the request.
    /// It never caches: a revoked source, credential or scope refuses a
    /// queued or recovered turn too.
    fn cloud_agent_authority(
        &self,
        agent: &crate::store::CloudAgent,
        source: &Source,
        scope: &'static str,
    ) -> Result<()> {
        if !agent.connected() {
            return Err(Error::gate_coded(
                "cloud_agent_not_connected",
                "cloud agent is disconnected — reconnect it explicitly first",
            ));
        }
        self.cloud_agent_source_active(agent, source)?;
        let target = Self::cloud_agent_target_for(agent, Some(source));
        match source.kind {
            SOURCE_USER => {
                crate::remote_cli::cloud_agent_credential_owned(&target, scope, Some(&source.owner))
            }
            SOURCE_PEER => crate::remote_cli::cloud_agent_peer_ready(
                &target,
                &source.owner,
                &source.uid,
                source.epoch,
                scope,
            )
            .map_err(|error| match error {
                Error::Rejected(_) => Error::gate_coded(
                    "cloud_agent_peer_credential_required",
                    "this peer source has no enrolled cloud-purpose credential under this \
                     verified owner; the operator must run `cloud-agent enroll-peer` first \
                     — nothing was sent",
                ),
                other => other,
            }),
            _ => Err(Error::gate_coded(
                "cloud_agent_source_refused",
                "unknown invoker source kind",
            )),
        }
    }

    fn cloud_agent_by_id(&self, params: &Value) -> Result<crate::store::CloudAgent> {
        let id = cloud_agent_id(params)?;
        self.store
            .cloud_agent(id)?
            .ok_or_else(|| Error::gate_coded("cloud_agent_not_connected", "unknown cloud agent"))
    }

    /// Resolve the derived caller to its source and make sure the target has
    /// an explicit grant for it.
    fn cloud_agent_caller_source(
        &self,
        agent: &crate::store::CloudAgent,
        peer_pid: u32,
        verb: &str,
    ) -> Result<(Sender, Source)> {
        let caller = self.agent_caller(peer_pid, verb)?;
        if matches!(&caller, crate::peer::AgentCaller::Agent(_)) {
            self.cloud_peer_isolation(peer_pid)?;
        }
        let sender = Sender::from_caller(&caller);
        let target = Self::cloud_agent_target(agent);
        let source = sender.source_for(&self.store, &target)?;
        self.cloud_agent_source_active(agent, &source)?;
        Ok((sender, source))
    }

    /// ONE source/owner decision for every generic projection of cloud
    /// content. `thread_read`, `agent_show` and `agent_events` all project a
    /// cloud target's request/result text, so each applies this same check
    /// instead of a bespoke one: the daemon-derived caller must hold the
    /// authority the dedicated cloud reads require — CURRENT read authority
    /// on its own source (the operator, or an enrolled peer whose source
    /// grant and credential are current on this connection, on the split
    /// agent UID boundary). A plain local alias keeps projecting, so a peer's
    /// own UID-bound reports keep working; what does not keep projecting are
    /// the alias' cloud-DERIVED rows, which are bound to their delivery UIDs
    /// by [`Self::admit_derived_cloud_rows`].
    ///
    /// That authority is checked twice, and only the second check can refuse
    /// a revocation that landed after this workspace saved its login: the
    /// local credential/enrollment scope first, then the platform's own
    /// current answer via [`Self::cloud_read_authority`]. A projection never
    /// serves stored reply text on the strength of the local copy alone — and
    /// the platform's answer is also what decides whether an operation is
    /// settled enough to present: only a terminal-completed answer covers its
    /// stored receipt, anything else withholds that operation's text.
    ///
    /// The returned admission also binds every projected row to the state it
    /// had when the read began: a cloud row is shown only to the source/owner
    /// its operation was admitted for, and only as far as that snapshot
    /// reaches. The snapshot is taken BEFORE either authority proof, so the
    /// platform's answer can never widen it: an operation that settled while
    /// the proof ran is not admitted after the fact. The operator is the
    /// owner of its own user source, never a blanket reader of a peer's
    /// entries, and an enrolled peer never reads another source's alias-wide
    /// history. When cloud content exists but no authority metadata resolves
    /// at all — a restored workspace whose four cloud tables were omitted —
    /// the projection fails closed.
    pub(super) fn admit_cloud_projection(
        &self,
        alias: &str,
        peer_pid: u32,
        verb: &str,
    ) -> Result<CloudProjection> {
        let projection = self.cloud_projection_decision(alias, peer_pid, verb, None)?;
        self.observe_cloud_projection(alias, &projection);
        Ok(projection)
    }

    fn observe_cloud_projection(&self, _alias: &str, _projection: &CloudProjection) {
        #[cfg(feature = "test-seam")]
        if let Some(observer) = &self.cloud_projection_observer {
            observer(_alias, _projection.withheld.as_deref());
        }
    }

    fn cloud_projection_decision(
        &self,
        alias: &str,
        peer_pid: u32,
        verb: &str,
        message_id: Option<&str>,
    ) -> Result<CloudProjection> {
        let cloud_agents = self.store.cloud_agents_for_alias(alias)?;
        if cloud_agents.is_empty() {
            let cloud_target = self
                .store
                .agent_opt(alias)?
                .is_some_and(|agent| agent.provider == crate::cloud_agent::PROVIDER);
            if cloud_target || self.store.alias_has_cloud_content(alias)? {
                // Missing cloud authority never supplies caller authority.
                if matches!(
                    self.agent_caller(peer_pid, verb)?,
                    crate::peer::AgentCaller::Agent(_)
                ) {
                    self.cloud_peer_isolation(peer_pid)?;
                }
                let (derived, ordinary) = self.store.cloud_projection_rows(alias, message_id)?;
                let known = derived
                    .into_iter()
                    .map(|row| row.message_id)
                    .chain(ordinary)
                    .collect();
                return Ok(CloudProjection {
                    known,
                    withheld: Some("cloud_agent_authority_missing".to_string()),
                    ..CloudProjection::default()
                });
            }
            return self.admit_derived_cloud_rows(alias, peer_pid, message_id);
        }
        let mut scopes = Vec::with_capacity(cloud_agents.len());
        let mut known = std::collections::BTreeSet::new();
        let mut withheld: Option<String> = None;
        for agent in cloud_agents {
            let operations = self.store.cloud_agent_operations(&agent.id, None)?;
            known.extend(
                operations
                    .iter()
                    .map(|operation| operation.operation_id.clone()),
            );
            let source = match self.cloud_agent_caller_source(&agent, peer_pid, verb) {
                Ok((_, source)) => source,
                Err(error) => {
                    withheld = Some(projection_denial(error)?);
                    continue;
                }
            };
            // FIRST the snapshot, before any authority proof: every operation
            // this caller's own source owned, in the state it had when this
            // read began. Freezing the set here is what makes a proof unable
            // to move a row into the admitted set after the fact — a
            // settlement that lands below is never served under an admission
            // that did not cover it, and the waiting readers re-resolve
            // instead of projecting it (see
            // [`CloudProjection::needs_readmission`]).
            let mut admitted = std::collections::BTreeMap::new();
            for operation in &operations {
                if !source.matches(operation) {
                    continue;
                }
                let admission = if operation.settled() {
                    Admission::Settled
                } else {
                    Admission::Request
                };
                admitted.insert(operation.operation_id.clone(), admission);
            }
            // THEN the authority proofs. Current READ authority — connection
            // state, the active grant and the scope on the verified
            // credential/enrollment — not bare membership: a write-only
            // operator must not read stored replies through a generic
            // projection.
            if let Err(error) = self.cloud_agent_authority(&agent, &source, SCOPE_READ) {
                withheld = Some(projection_denial(error)?);
                continue;
            }
            // The local copy proves what the workspace STORED, not what the
            // platform still allows: a read scope revoked upstream leaves the
            // saved login valid and unexpired. The platform's own answer is
            // required before any stored cloud text is projected — and its
            // state decides whether the stored receipt may be shown at all.
            let answer = match self.cloud_read_authority(&agent, &source, &operations) {
                Ok(answer) => answer,
                Err(error) => {
                    withheld = Some(projection_denial(error)?);
                    continue;
                }
            };
            // The proof cannot widen the snapshot. Even a completed answer
            // leaves a request-only admission request-only until readmission.
            if let Some((operation_id, PlatformState::Unsettled)) = answer {
                admitted.remove(&operation_id);
                withheld = Some("cloud_agent_not_settled".to_string());
            }
            scopes.push(CloudScope {
                cloud_agent_id: agent.id,
                source,
                admitted,
            });
        }
        let mut projection = CloudProjection::cloud(scopes, known);
        projection.withheld = withheld;
        Ok(projection)
    }

    /// The one policy for a routed cloud row: the operator, the delivery's
    /// sender, or its recipient. A row with no cloud provenance is not this
    /// policy's business and reads as before. Every reader of a message body
    /// or a message id asks this, so the rule cannot drift between them.
    pub(super) fn may_read_cloud_row(
        &self,
        peer_pid: u32,
        alias: &str,
        message_id: &str,
        _sender_uid: Option<&str>,
        _recipient_uid: Option<&str>,
    ) -> Result<bool> {
        // The stored snapshot supplies BOTH the binding and the authority;
        // absence from the derived map is never an ordinary allow-all.
        let projection =
            self.cloud_projection_decision(alias, peer_pid, "cloud row read", Some(message_id))?;
        self.observe_cloud_projection(alias, &projection);
        Ok(!matches!(projection.row(message_id), Row::Never))
    }

    /// An ordinary alias can still carry cloud content: settling a turn routes
    /// the reply to its sender as a `worker_result` (or `worker_notice`), and
    /// that copy's immutable `origin` names the cloud operation. Cloud-ness is
    /// a property of that provenance — never of the alias' provider or of a
    /// `source` label on the alias — so those rows are bound to the UIDs the
    /// delivery carried. The reporting worker and its recipient read them, the
    /// operator reads them, and any other connection on the same socket UID
    /// (an unrelated, unenrolled agent) does not. The alias' ordinary local
    /// mail is untouched: a UID-bound report that did not derive from cloud
    /// content has no such provenance and is admitted here by enumeration.
    ///
    /// Both halves are the same snapshot, taken now: the derived rows WITH
    /// their binding, and the ids that were ordinary. Together they enumerate
    /// the whole alias, so [`CloudProjection::row`] has no default to fall
    /// back on — a row that resolved after this point is in neither half and
    /// is withheld until a read re-resolves.
    fn admit_derived_cloud_rows(
        &self,
        alias: &str,
        peer_pid: u32,
        message_id: Option<&str>,
    ) -> Result<CloudProjection> {
        let (rows, ordinary) = self.store.cloud_projection_rows(alias, message_id)?;
        if rows.is_empty() {
            return Ok(CloudProjection::derived_rows(rows, ordinary, None, false));
        }
        let (viewer, operator) = match self.agent_caller(peer_pid, "cloud read")? {
            crate::peer::AgentCaller::Operator => (None, true),
            crate::peer::AgentCaller::Agent(caller) => {
                (Some(self.store.agent(&caller)?.uid), false)
            }
        };
        // Delivery eligibility comes first: unrelated readers keep ordinary
        // mail without attempting cloud access. Each eligible origin below
        // still proves the peer's isolation before resolving its credential.
        let mut projection = CloudProjection::derived_rows(rows, ordinary, viewer, operator);
        let mut origins = std::collections::BTreeSet::new();
        for row in projection.derived.values() {
            if !projection.bound(row) {
                projection.withheld = Some("cloud_agent_delivery_refused".to_string());
            } else if let Some(id) = &row.cloud_agent_id {
                origins.insert(id.clone());
            } else {
                projection.withheld = Some("cloud_agent_authority_missing".to_string());
            }
        }
        // One proof per backing connection, never one proof reused for A/B.
        // Failed origins keep their immutable rows and simply gain no
        // admission; a denied B cannot erase A or become ordinary mail.
        for agent_id in origins {
            let Some(agent) = self.store.cloud_agent(&agent_id)? else {
                projection.withheld = Some("cloud_agent_authority_missing".to_string());
                continue;
            };
            let proof = (|| {
                let (_, source) = self.cloud_agent_caller_source(&agent, peer_pid, "cloud read")?;
                self.cloud_agent_authority(&agent, &source, SCOPE_READ)?;
                let operations = self.store.cloud_agent_operations(&agent.id, None)?;
                self.cloud_read_authority(&agent, &source, &operations)
            })();
            match proof {
                Ok(Some((_, PlatformState::Unsettled))) => {
                    projection.withheld = Some("cloud_agent_not_settled".to_string());
                }
                Ok(_) => {
                    projection.admitted_origins.insert(agent_id);
                }
                Err(error) => projection.withheld = Some(projection_denial(error)?),
            }
        }
        Ok(projection)
    }

    /// The platform's own answer for "may this source read its cloud turn
    /// now, and is that turn settled?" — one readback of the caller's own
    /// newest submitted turn.
    ///
    /// The saved login/enrollment proves only the scope this workspace
    /// stored; the contract is the sole authority on whether it is still
    /// live. A refusal refuses the projection, an unreachable or uncertain
    /// answer propagates as uncertainty, and neither serves stored content.
    /// The answer is not turned into a second read path: [`cloud_agent::
    /// read_operation`] reconciles the durable receipt (a settled reply is
    /// never withdrawn), and this projection only decides what that receipt
    /// may show. With no submitted turn, model-free agent discovery proves
    /// current read capability with this caller's OWN credential and owner
    /// pin; it never creates a model operation or adopts a provider session.
    fn cloud_read_authority(
        &self,
        agent: &crate::store::CloudAgent,
        source: &Source,
        operations: &[crate::store::cloud_agent::CloudOperation],
    ) -> Result<Option<(String, PlatformState)>> {
        let Some(operation) = operations
            .iter()
            .filter(|operation| source.matches(operation))
            .filter(|operation| {
                operation.remote_operation_id.is_some() || operation.claimed_at > 0.0
            })
            .max_by(|a, b| {
                a.updated
                    .total_cmp(&b.updated)
                    .then_with(|| a.operation_id.cmp(&b.operation_id))
            })
        else {
            self.cloud_discover_read_authority(agent, source)?;
            return Ok(None);
        };
        let target = Self::cloud_agent_target_for(agent, Some(source));
        let authorize = || self.cloud_agent_authority(agent, source, SCOPE_READ);
        // The dedicated read's own reconciliation: it proves the read
        // authority again, refuses a terminal answer that contradicts the
        // settled receipt, and reports an unreachable platform as the coded
        // uncertainty a generic projection must not answer from cache.
        let transport = ProjectionReadTransport {
            remote_failed: std::cell::Cell::new(false),
        };
        let answer = cloud_agent::read_operation(
            &self.store,
            &transport,
            agent,
            &target,
            &operation.operation_id,
            source,
            authorize,
        )
        .map_err(|error| {
            if !transport.remote_failed.get() && error.code() == Some("cloud_agent_uncertain") {
                Error::internal(format!("cloud receipt reconciliation failed: {error}"))
            } else {
                error
            }
        })?;
        let state = if answer.status == crate::store::cloud_agent::STATUS_COMPLETED {
            PlatformState::Completed
        } else {
            PlatformState::Unsettled
        };
        Ok(Some((answer.operation_id, state)))
    }

    /// Model-free `agent` discovery when this source has no submitted turn.
    /// Use the same canonical home key as connect, but never persist the
    /// returned session/model metadata: this is a capability proof only.
    fn cloud_discover_read_authority(
        &self,
        agent: &crate::store::CloudAgent,
        source: &Source,
    ) -> Result<()> {
        let grant = self
            .store
            .cloud_agent_source(
                &agent.id,
                source.kind,
                &source.uid,
                source.epoch,
                &source.owner,
            )?
            .ok_or_else(|| {
                Error::gate_coded(
                    "cloud_agent_authority_missing",
                    "the discovery source has no immutable authority record",
                )
            })?;
        let pinned_invoker = grant.invoker_id.as_deref().ok_or_else(|| {
            Error::gate_coded(
                "cloud_agent_authority_missing",
                "the discovery source has no stable upstream owner pin",
            )
        })?;
        let home = self.store.thread(&agent.alias)?.ok_or_else(|| {
            Error::gate_coded(
                "cloud_agent_authority_missing",
                "the connection has no canonical home conversation",
            )
        })?;
        let session = self.store.cloud_agent_session(
            &agent.id,
            source.kind,
            &source.uid,
            source.epoch,
            &source.owner,
            &home.id,
        )?;
        let conversation_id = match &session {
            Some(session) => std::borrow::Cow::Borrowed(session.conversation_id.as_str()),
            None => {
                let home_key = format!("home|{}|{}", agent.organization_id, agent.alias);
                let digest = crate::store::cloud_agent::CloudOperation::digest(
                    self.store.instance_identity(),
                    "cloud-conversation",
                    &home_key,
                    "",
                    0,
                    "",
                );
                std::borrow::Cow::Owned(format!("conv-{}", &digest[..40]))
            }
        };
        let target = Self::cloud_agent_target_for(agent, Some(source));
        self.cloud_agent_authority(agent, source, SCOPE_READ)?;
        let remote = cloud_agent::discover_source(
            &HostedCloudTransport,
            agent,
            &target,
            conversation_id.as_ref(),
            source,
            Some(pinned_invoker),
        )?;
        if session.as_ref().is_some_and(|session| {
            session.remote_session_id != remote.remote_session_id
                || session
                    .provider_session_id
                    .as_deref()
                    .zip(remote.provider_session_id.as_deref())
                    .is_some_and(|(pinned, incoming)| pinned != incoming)
        }) {
            return Err(Error::gate_coded(
                "cloud_agent_session_mismatch",
                "discovery changed the source's conversation binding",
            ));
        }
        Ok(())
    }

    pub(super) fn rpc_cloud_agent(
        self: &Arc<Self>,
        method: &str,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        let transport = HostedCloudTransport;
        match method {
            "cloud_agent_connect" => {
                allow(
                    params,
                    &[
                        "org",
                        "org_id",
                        "endpoint",
                        "alias",
                        "accept_assistant_consent",
                    ],
                    method,
                )?;
                self.operator_connection_on_agent("cloud agent connect", params, peer_pid)?;
                if params
                    .get("accept_assistant_consent")
                    .and_then(Value::as_bool)
                    != Some(true)
                {
                    return Err(Error::gate_coded(
                        "cloud_agent_consent_required",
                        "connecting a cloud agent needs explicit assistant consent: pass \
                         --accept-assistant-consent. A saved cli.write login does not \
                         authorize paid model turns",
                    ));
                }
                let org = required_str(params, "org")?;
                crate::proto::identifier(org, "Cloud-agent org")?;
                let org_id = required_str(params, "org_id")?;
                crate::proto::identifier(org_id, "Cloud-agent workspace id")?;
                let endpoint = required_str(params, "endpoint")?;
                cloud_agent::validate_endpoint(endpoint)?;
                let alias = optional_str(params, "alias").unwrap_or("cloud");
                let target = RemoteTarget {
                    org: org.to_string(),
                    org_id: org_id.to_string(),
                    endpoint: endpoint.to_string(),
                    identity: None,
                };
                let owner = {
                    let pin = crate::remote_cli::cloud_agent_identity(&target)?;
                    crate::store::cloud_agent::Owner::new(
                        pin.issuer.clone(),
                        pin.subject_id.clone(),
                    )
                };
                // Pin the verified owner on the handshake target too: the
                // connect POST reloads the credential and must re-prove it.
                let target = RemoteTarget {
                    identity: Some(crate::remote_cli::CliIdentityPin {
                        subject_id: owner.subject.clone(),
                        issuer: owner.issuer.clone(),
                    }),
                    ..target
                };
                let authorize = || {
                    crate::remote_cli::cloud_agent_credential_owned(
                        &target,
                        SCOPE_READ,
                        Some(&owner),
                    )?;
                    crate::remote_cli::cloud_agent_credential_owned(
                        &target,
                        SCOPE_WRITE,
                        Some(&owner),
                    )
                };
                let agent = cloud_agent::connect(
                    &self.store,
                    &transport,
                    &target,
                    &cloud_agent::ConnectRequest {
                        alias,
                        owner: &owner,
                        cwd: &self.state_dir,
                        granted_by: "operator",
                    },
                    authorize,
                )?;
                self.wake();
                Ok(json!({"agent": cloud_agent::agent_view(&self.store, &agent)?}))
            }
            "cloud_agent_list" => {
                allow(params, &[], method)?;
                let caller = self.agent_caller(peer_pid, "cloud agent list")?;
                let sender = Sender::from_caller(&caller);
                let mut views = Vec::new();
                for agent in self.store.cloud_agents()? {
                    let target = Self::cloud_agent_target(&agent);
                    let source = sender.source_for(&self.store, &target)?;
                    if source.kind == SOURCE_PEER {
                        self.cloud_peer_isolation(peer_pid)?;
                    }
                    if caller_has_source(&self.store, &agent.id, &source)? {
                        views.push(cloud_agent::agent_view(&self.store, &agent)?);
                    }
                }
                Ok(json!({"agents": views}))
            }
            "cloud_agent_show" => {
                allow(params, &["cloud_agent_id"], method)?;
                let agent = self.cloud_agent_by_id(params)?;
                self.cloud_agent_caller_source(&agent, peer_pid, "cloud agent show")?;
                Ok(json!({"agent": cloud_agent::agent_view(&self.store, &agent)?}))
            }
            // Decision A: the operator explicitly enrolls ONE local peer
            // whose REAL process is on the split agent UID boundary. The
            // operator's own socket identity is never accepted as the peer.
            "cloud_agent_enroll_peer" => {
                allow(params, &["cloud_agent_id", "agent"], method)?;
                self.operator_connection_on_agent("cloud agent enroll-peer", params, peer_pid)?;
                let agent = self.cloud_agent_by_id(params)?;
                let peer_alias = required_str(params, "agent")?;
                let peer = self.store.agent(peer_alias)?;
                let agent_uid = self.agent_uid.ok_or_else(|| {
                    Error::gate_coded(
                        "cloud_agent_peer_isolation_required",
                        "cloud-peer enrollment requires the split agent UID boundary; this \
                         daemon runs same-UID and cannot isolate peer credentials",
                    )
                })?;
                let pid = peer
                    .pid
                    .and_then(|pid| u32::try_from(pid).ok())
                    .ok_or_else(|| {
                        Error::gate_coded(
                            "cloud_agent_peer_isolation_required",
                            "the target peer has no verified running process",
                        )
                    })?;
                let (real, effective) = crate::peer::proc_uids(pid).map_err(|_| {
                    Error::gate_coded(
                        "cloud_agent_peer_isolation_required",
                        "the target peer process UID cannot be verified",
                    )
                })?;
                if real != agent_uid && effective != agent_uid {
                    return Err(Error::gate_coded(
                        "cloud_agent_peer_isolation_required",
                        "the target peer process is not on the split agent UID boundary",
                    ));
                }
                let observed_start = crate::peer::proc_starttime(pid).map(|s| s as i64);
                if peer.pid_start != observed_start {
                    return Err(Error::gate_coded(
                        "cloud_agent_peer_isolation_required",
                        "the target peer's recorded pid-start no longer matches — refusing a \
                         reused pid",
                    ));
                }
                let target = Self::cloud_agent_target(&agent);
                crate::remote_cli::cloud_agent_credential(&target, SCOPE_READ)?;
                crate::remote_cli::cloud_agent_credential(&target, SCOPE_WRITE)?;
                let source = cloud_agent::enrollment::enroll_peer(
                    &self.store,
                    &target,
                    &agent,
                    &peer,
                    "operator",
                )?;
                Ok(json!({
                    "agent": cloud_agent::agent_view(&self.store, &agent)?,
                    "source": source.to_json(),
                }))
            }
            "cloud_agent_send" => {
                allow(
                    params,
                    &[
                        "cloud_agent_id",
                        "thread_id",
                        "text",
                        "operation_id",
                        "wake",
                    ],
                    method,
                )?;
                let agent = self.cloud_agent_by_id(params)?;
                let (sender, source) =
                    self.cloud_agent_caller_source(&agent, peer_pid, "cloud agent send")?;
                let target = Self::cloud_agent_target_for(&agent, Some(&source));
                // Submission spends a paid turn (write); serving an already
                // settled replay discloses its reply (read). They are
                // different authorities and are proven separately.
                let write_authority = || self.cloud_agent_authority(&agent, &source, SCOPE_WRITE);
                let read_authority = || self.cloud_agent_authority(&agent, &source, SCOPE_READ);
                let wake = match params.get("wake") {
                    None => false,
                    Some(Value::Bool(value)) => *value,
                    Some(_) => {
                        return Err(Error::invalid(
                            "cloud_agent_wake_invalid",
                            "wake must be a boolean",
                        ))
                    }
                };
                let operation = cloud_agent::send_authorized(
                    &self.store,
                    &transport,
                    &agent,
                    &target,
                    cloud_agent::SendRequest {
                        thread_id: required_str(params, "thread_id")?,
                        sender: &sender,
                        source: &source,
                        operation_id: required_str(params, "operation_id")?,
                        text: required_str(params, "text")?,
                        wake,
                        expected_recipient_uid: None,
                    },
                    &cloud_agent::Authority {
                        submit: &write_authority,
                        read: &read_authority,
                    },
                )?;
                self.wake();
                Ok(json!({"agent": agent.to_json(), "operation": operation.to_json()}))
            }
            "cloud_agent_read" => {
                allow(params, &["cloud_agent_id", "operation_id"], method)?;
                let agent = self.cloud_agent_by_id(params)?;
                let (_sender, source) =
                    self.cloud_agent_caller_source(&agent, peer_pid, "cloud agent read")?;
                let target = Self::cloud_agent_target_for(&agent, Some(&source));
                let authorize = || self.cloud_agent_authority(&agent, &source, SCOPE_READ);
                let operation = cloud_agent::read_operation(
                    &self.store,
                    &transport,
                    &agent,
                    &target,
                    required_str(params, "operation_id")?,
                    &source,
                    authorize,
                )?;
                Ok(json!({"agent": agent.to_json(), "operation": operation.to_json()}))
            }
            "cloud_agent_cancel" => {
                allow(params, &["cloud_agent_id", "operation_id"], method)?;
                let agent = self.cloud_agent_by_id(params)?;
                let (_sender, source) =
                    self.cloud_agent_caller_source(&agent, peer_pid, "cloud agent cancel")?;
                let target = Self::cloud_agent_target_for(&agent, Some(&source));
                let authorize = || self.cloud_agent_authority(&agent, &source, SCOPE_WRITE);
                cloud_agent::cancel(
                    &self.store,
                    &transport as &dyn CloudTransport,
                    &target,
                    &agent,
                    required_str(params, "operation_id")?,
                    &source,
                    authorize,
                )?;
                Ok(json!({"cancelled": false, "state": "unsupported"}))
            }
            "cloud_agent_disconnect" => {
                allow(params, &["cloud_agent_id"], method)?;
                self.operator_connection_on_agent("cloud agent disconnect", params, peer_pid)?;
                let agent = self.cloud_agent_by_id(params)?;
                let agent = cloud_agent::disconnect(&self.store, &agent)?;
                self.wake();
                Ok(json!({"agent": cloud_agent::agent_view(&self.store, &agent)?}))
            }
            _ => Err(Error::rejected("unknown cloud agent method")),
        }
    }

    /// Route a normal `cadence send` / Thread composer write for a cloud
    /// alias through the cloud target instead of the local message queue.
    /// The canonical Thread entry is the same one the cloud reply lands in;
    /// no second conversation store or broker is introduced.
    pub(super) fn cloud_send_from(
        self: &Arc<Self>,
        alias: &str,
        params: &Value,
        peer_pid: u32,
        thread_id: Option<&str>,
        expected_uid: Option<&str>,
        verb: &str,
    ) -> Result<Value> {
        let agent = self
            .store
            .cloud_agent_by_alias(alias)?
            .ok_or_else(|| Error::gate_coded("cloud_agent_not_connected", "unknown cloud agent"))?;
        let (sender, source) = self.cloud_agent_caller_source(&agent, peer_pid, verb)?;
        if let Some(expected) = expected_uid {
            // Fast refusal; the authoritative comparison runs inside the
            // enqueue transaction against the pinned recipient.
            if expected != agent.recipient_uid {
                return Err(Error::rejected(
                    "expected_recipient_uid does not name the cloud recipient this \
                     connection was created against; nothing was sent",
                ));
            }
        }
        let thread = match thread_id {
            Some(thread) => thread.to_string(),
            None => self
                .store
                .thread(alias)?
                .map(|thread| thread.id)
                .ok_or_else(|| {
                    Error::invalid(
                        "cloud_agent_thread_unknown",
                        "cloud agent has no canonical work conversation yet",
                    )
                })?,
        };
        let text = required_str(params, "text")?;
        let operation_id = match optional_str(params, "message") {
            Some(message) => message.to_string(),
            None => format!("cl-{}", uuid::Uuid::new_v4().simple()),
        };
        let target = Self::cloud_agent_target_for(&agent, Some(&source));
        // The normal `cadence send` route also replays an operation id, so it
        // needs the same split: write to submit, read to disclose a settled
        // reply. A write-only credential gets no reply here either.
        let write_authority = || self.cloud_agent_authority(&agent, &source, SCOPE_WRITE);
        let read_authority = || self.cloud_agent_authority(&agent, &source, SCOPE_READ);
        let operation = cloud_agent::send_authorized(
            &self.store,
            &HostedCloudTransport,
            &agent,
            &target,
            cloud_agent::SendRequest {
                thread_id: &thread,
                sender: &sender,
                source: &source,
                operation_id: &operation_id,
                text,
                wake: true,
                expected_recipient_uid: expected_uid,
            },
            &cloud_agent::Authority {
                submit: &write_authority,
                read: &read_authority,
            },
        )?;
        self.wake();
        Ok(json!({
            "alias": alias,
            "message": operation_id,
            "cloud_agent_id": agent.id,
            "thread": thread,
            "operation": operation.to_json(),
        }))
    }

    /// CAD-1853: the bounded cloud readback driver — the normal lifecycle for
    /// a queued cloud turn. It is READBACK ONLY: every non-terminal operation
    /// is read from its original contract operation id; nothing is submitted,
    /// retried with a new id or resubmitted, so an accepted reply is never
    /// lost and a model turn is never started twice. It covers a restart
    /// (the durable operation rows are the worklist) and it settles canonical
    /// Threads + the UID-bound worker result without any manual read.
    ///
    /// One thread, bounded batch and wall deadline per tick, with a rotating
    /// cursor so permanently refused (revoked/unknown) slots cannot starve
    /// later accepted replies.
    pub(super) fn run_cloud_agent_driver(self: &Arc<Self>) {
        let transport = HostedCloudTransport;
        let mut cursor = 0usize;
        while !self.closing.load(Ordering::SeqCst) {
            let read_any = self.cloud_agent_readback_tick(&transport, &mut cursor);
            let pause = if read_any {
                Duration::from_millis(1500)
            } else {
                Duration::from_secs(8)
            };
            let deadline = Instant::now() + pause;
            while !self.closing.load(Ordering::SeqCst) && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(100));
            }
        }
    }

    /// One bounded readback tick. At most [`CLOUD_AGENT_READBACK_CAP`]
    /// operations and one wall deadline, so an unreachable provider never
    /// holds the daemon or spins. `cursor` rotates the flat worklist across
    /// ticks: a stuck/revoked head is skipped, not served forever.
    fn cloud_agent_readback_tick(
        &self,
        transport: &dyn CloudTransport,
        cursor: &mut usize,
    ) -> bool {
        const CLOUD_AGENT_READBACK_CAP: usize = 8;
        const TICK_DEADLINE: Duration = Duration::from_secs(20);
        let pending = match self.store.cloud_agents_with_pending_operations() {
            Ok(ids) => ids,
            Err(error) => {
                eprintln!("cloud-agent readback worklist failed: {error}");
                return false;
            }
        };
        let mut agents: Vec<crate::store::CloudAgent> = Vec::new();
        let mut work: Vec<(usize, crate::store::CloudOperation)> = Vec::new();
        for id in pending {
            let Ok(Some(agent)) = self.store.cloud_agent(&id) else {
                continue;
            };
            // A disconnected target admits no read; its operations stay
            // durable (non-terminal) and resume after an explicit reconnect.
            if !agent.connected() {
                continue;
            }
            let Ok(operations) = self.store.cloud_agent_pending_operations(&agent.id) else {
                continue;
            };
            if operations.is_empty() {
                continue;
            }
            let index = agents.len();
            agents.push(agent);
            work.extend(operations.into_iter().map(|operation| (index, operation)));
        }
        if work.is_empty() {
            *cursor = 0;
            return false;
        }
        let start = *cursor % work.len();
        let take = work.len().min(CLOUD_AGENT_READBACK_CAP);
        let deadline = Instant::now() + TICK_DEADLINE;
        let mut attempted = 0;
        for offset in 0..take {
            if Instant::now() >= deadline || self.closing.load(Ordering::SeqCst) {
                break;
            }
            let (agent_index, operation) = &work[(start + offset) % work.len()];
            let agent = &agents[*agent_index];
            attempted += 1;
            let source = Source {
                kind: if operation.source_kind == SOURCE_USER {
                    SOURCE_USER
                } else {
                    SOURCE_PEER
                },
                uid: operation.source_uid.clone(),
                epoch: operation.registration_epoch,
                owner: operation.owner.clone(),
            };
            let target = Self::cloud_agent_target_for(agent, Some(&source));
            let authorize = || self.cloud_agent_authority(agent, &source, SCOPE_READ);
            let before = (operation.status.as_str(), operation.updated);
            if let Ok(after) = cloud_agent::read_operation(
                &self.store,
                transport,
                agent,
                &target,
                &operation.operation_id,
                &source,
                authorize,
            ) {
                if (after.status.as_str(), after.updated) != before {
                    // The canonical row/receipt is already durable; wake
                    // message_read/wait watchers to project it.
                    self.wake();
                }
            }
        }
        *cursor = (start + attempted) % work.len();
        attempted > 0
    }
}

fn caller_has_source(
    store: &crate::store::Store,
    cloud_agent_id: &str,
    source: &Source,
) -> Result<bool> {
    Ok(store
        .cloud_agent_source(
            cloud_agent_id,
            source.kind,
            &source.uid,
            source.epoch,
            &source.owner,
        )?
        .is_some_and(|grant: CloudSource| grant.active()))
}
