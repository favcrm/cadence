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
/// off the cloud alias too, and only a UID the delivery was bound to, or the
/// operator, may read them.
pub(super) struct CloudProjection {
    scopes: Vec<CloudScope>,
    /// Cloud-derived rows on an ordinary alias by message id, with the
    /// delivered UID binding.
    derived: std::collections::HashMap<String, crate::store::cloud_agent::CloudDerivedRow>,
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
}

impl CloudProjection {
    /// A cloud alias, admitted per operation for this caller's own source.
    fn cloud(scopes: Vec<CloudScope>) -> Self {
        Self {
            scopes,
            derived: std::collections::HashMap::new(),
            ordinary: std::collections::BTreeSet::new(),
            viewer: None,
            operator: false,
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
            scopes: Vec::new(),
            derived: rows
                .into_iter()
                .map(|row| (row.message_id.clone(), row))
                .collect(),
            ordinary: ordinary.into_iter().collect(),
            viewer,
            operator,
        }
    }

    /// The admission covering the operation that a projected row's canonical
    /// message id names, on this caller's own source. The operation row is the
    /// attribution witness: the cloud request/reply carries no caller-supplied
    /// owner, and the canonical message id IS the operation id. `None` means
    /// the row is not the caller's own admitted operation.
    fn admission(
        &self,
        store: &crate::store::Store,
        message_id: &str,
    ) -> Result<Option<Admission>> {
        for scope in &self.scopes {
            if let Some(operation) =
                store.cloud_agent_operation(&scope.cloud_agent_id, message_id)?
            {
                // The operation resolves on this connection: it belongs to
                // this source or to no one the caller may read.
                if !scope.source.matches(&operation) {
                    return Ok(None);
                }
                return Ok(scope.admitted.get(message_id).copied());
            }
        }
        Ok(None)
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
    fn row(&self, store: &crate::store::Store, message_id: &str) -> Result<Row> {
        if !self.scopes.is_empty() {
            return Ok(match self.admission(store, message_id)? {
                Some(Admission::Settled) => Row::Full,
                Some(Admission::Request) => Row::Request,
                None => Row::Never,
            });
        }
        if let Some(row) = self.derived.get(message_id) {
            return Ok(if self.bound(row) {
                Row::Full
            } else {
                Row::Never
            });
        }
        // An ordinary alias: only the mail this snapshot enumerated.
        Ok(if self.ordinary.contains(message_id) {
            Row::Full
        } else {
            Row::Never
        })
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
        // No shortcut, not even for an alias that is empty right now: an
        // empty snapshot is a fact about this instant, and the row that
        // arrives next would otherwise be rendered by a projection that never
        // enumerated it. Admission is unconditional; the cost is one lookup
        // per row on an alias with no cloud content at all.
        for id in ids {
            if self.scopes.is_empty() {
                if !self.ordinary.contains(id) && !self.derived.contains_key(id) {
                    return Ok(true);
                }
                continue;
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
                    None => false,
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
        store: &crate::store::Store,
        entries: Vec<crate::store::ThreadEntry>,
    ) -> Result<Vec<crate::store::ThreadEntry>> {
        let mut kept = Vec::with_capacity(entries.len());
        for entry in entries {
            let row = match entry.message_id.as_deref() {
                Some(id) => self.row(store, id)?,
                // A cloud thread carries no unattributable entry: an entry
                // with no canonical message cannot be shown to be the
                // caller's own. An ordinary alias keeps its own.
                None if self.scopes.is_empty() => Row::Full,
                None => Row::Never,
            };
            let keep = match row {
                Row::Full => true,
                Row::Request => entry.kind == crate::store::KIND_MESSAGE,
                Row::Never => false,
            };
            if keep {
                kept.push(entry);
            }
        }
        Ok(kept)
    }

    pub(super) fn messages(
        &self,
        store: &crate::store::Store,
        messages: Vec<crate::store::Message>,
    ) -> Result<Vec<crate::store::Message>> {
        let mut kept = Vec::with_capacity(messages.len());
        for mut message in messages {
            match self.row(store, &message.id)? {
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
        store: &crate::store::Store,
        events: Vec<crate::store::Event>,
    ) -> Result<Vec<crate::store::Event>> {
        let mut kept = Vec::with_capacity(events.len());
        for event in events {
            let row = match event.payload.get("message").and_then(Value::as_str) {
                Some(id) => self.row(store, id)?,
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
            .map_err(|_| {
                Error::gate_coded(
                    "cloud_agent_peer_credential_required",
                    "this peer source has no enrolled cloud-purpose credential under this \
                     verified owner; the operator must run `cloud-agent enroll-peer` first \
                     — nothing was sent",
                )
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
        let sender = Sender::from_caller(&caller);
        let target = Self::cloud_agent_target(agent);
        let source = sender.source_for(&self.store, &target)?;
        if source.kind == SOURCE_PEER {
            self.cloud_peer_isolation(peer_pid)?;
        }
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
        let cloud_agents = self.store.cloud_agents_for_alias(alias)?;
        if cloud_agents.is_empty() {
            let cloud_target = self
                .store
                .agent_opt(alias)?
                .is_some_and(|agent| agent.provider == crate::cloud_agent::PROVIDER);
            if cloud_target || self.store.alias_has_cloud_content(alias)? {
                return Err(Error::gate_coded(
                    "cloud_agent_authority_missing",
                    format!(
                        "{verb} refused: this alias holds cloud-agent content but no cloud \
                         connection authority resolves; reconnect it explicitly"
                    ),
                ));
            }
            return self.admit_derived_cloud_rows(alias, peer_pid);
        }
        let mut scopes = Vec::with_capacity(cloud_agents.len());
        for agent in cloud_agents {
            let (_, source) = self.cloud_agent_caller_source(&agent, peer_pid, verb)?;
            // FIRST the snapshot, before any authority proof: every operation
            // this caller's own source owned, in the state it had when this
            // read began. Freezing the set here is what makes a proof unable
            // to move a row into the admitted set after the fact — a
            // settlement that lands below is never served under an admission
            // that did not cover it, and the waiting readers re-resolve
            // instead of projecting it (see
            // [`CloudProjection::needs_readmission`]).
            let mut admitted = std::collections::BTreeMap::new();
            for operation in self.store.cloud_agent_operations(&agent.id, None)? {
                if !source.matches(&operation) {
                    continue;
                }
                let admission = if operation.settled() {
                    Admission::Settled
                } else {
                    Admission::Request
                };
                admitted.insert(operation.operation_id, admission);
            }
            // THEN the authority proofs. Current READ authority — connection
            // state, the active grant and the scope on the verified
            // credential/enrollment — not bare membership: a write-only
            // operator must not read stored replies through a generic
            // projection.
            self.cloud_agent_authority(&agent, &source, SCOPE_READ)?;
            // The local copy proves what the workspace STORED, not what the
            // platform still allows: a read scope revoked upstream leaves the
            // saved login valid and unexpired. The platform's own answer is
            // required before any stored cloud text is projected — and its
            // state decides whether the stored receipt may be shown at all.
            let answer = self.cloud_read_authority(&agent, &source)?;
            // Decided from the snapshot AND the answer together: the snapshot
            // names the operations, the answer may only change the state of
            // one it names. It never ADMITS an operation the snapshot did not
            // name — that operation did not exist when the read began, so its
            // content was never admitted for this read.
            if let Some((operation_id, state)) = answer {
                match state {
                    // Only the platform's own terminal-completed answer
                    // covers the stored receipt — and only for an operation
                    // this read already admitted.
                    PlatformState::Completed => {
                        if let Some(admission) = admitted.get_mut(&operation_id) {
                            *admission = Admission::Settled;
                        }
                    }
                    // Nonterminal or failed: the platform does not report
                    // this operation settled, so no stored reply text for it
                    // is presented — an unsettled projection, not the cached
                    // completed receipt.
                    PlatformState::Unsettled => {
                        admitted.remove(&operation_id);
                    }
                }
            }
            scopes.push(CloudScope {
                cloud_agent_id: agent.id,
                source,
                admitted,
            });
        }
        Ok(CloudProjection::cloud(scopes))
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
        sender_uid: Option<&str>,
        recipient_uid: Option<&str>,
    ) -> Result<bool> {
        // Build the same admission the projection builds and ask the same
        // predicate: one derivation of the caller, one rule. A row with no
        // cloud provenance is not this policy's business.
        let projection = self.admit_derived_cloud_rows(alias, peer_pid)?;
        if !projection.derived.contains_key(message_id) {
            return Ok(true);
        }
        Ok(projection.cloud_row_readable(sender_uid, recipient_uid))
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
    fn admit_derived_cloud_rows(&self, alias: &str, peer_pid: u32) -> Result<CloudProjection> {
        let rows = self.store.cloud_derived_messages(alias)?;
        // An alias with no derived rows is NOT an alias that cannot grow one:
        // the first routed delivery may resolve after this snapshot. So the
        // absence of derived rows stays a fact about this snapshot and never
        // becomes a promise about later observations — the ordinary ids are
        // enumerated either way, and a row that resolves afterwards is in
        // neither half, which is what makes `row` withhold it until a read
        // re-resolves.
        let ordinary = self.store.ordinary_message_ids(alias)?;
        let (viewer, operator) = match self.connection_caller(peer_pid) {
            Ok(caller_rule::Who::Operator) => (None, true),
            Ok(caller_rule::Who::Agent(caller)) => {
                (self.store.agent(&caller).ok().map(|agent| agent.uid), false)
            }
            // A connection that derives no identity reads none of the cloud
            // rows; its ordinary mail still projects as before.
            Ok(caller_rule::Who::Unproven(_)) | Err(_) => (None, false),
        };
        Ok(CloudProjection::derived_rows(
            rows, ordinary, viewer, operator,
        ))
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
    /// may show. Nothing is probed when the caller owns no turn that crossed
    /// the submission boundary — and, crucially, such a caller's later
    /// settlement is not covered by this admission either, so the waiting
    /// readers re-prove rather than serve it.
    fn cloud_read_authority(
        &self,
        agent: &crate::store::CloudAgent,
        source: &Source,
    ) -> Result<Option<(String, PlatformState)>> {
        let Some(operation) = self
            .store
            .cloud_agent_operations(&agent.id, None)?
            .into_iter()
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
            return Ok(None);
        };
        let target = Self::cloud_agent_target_for(agent, Some(source));
        let authorize = || self.cloud_agent_authority(agent, source, SCOPE_READ);
        // The dedicated read's own reconciliation: it proves the read
        // authority again, refuses a terminal answer that contradicts the
        // settled receipt, and reports an unreachable platform as the coded
        // uncertainty a generic projection must not answer from cache.
        let answer = cloud_agent::read_operation(
            &self.store,
            &HostedCloudTransport,
            agent,
            &target,
            &operation.operation_id,
            source,
            authorize,
        )?;
        let state = if answer.status == crate::store::cloud_agent::STATUS_COMPLETED {
            PlatformState::Completed
        } else {
            PlatformState::Unsettled
        };
        Ok(Some((answer.operation_id, state)))
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
