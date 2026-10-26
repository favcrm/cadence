//! CAD-1853: explicit connection to one real AgenticOS Cloudflare
//! Worker/Pi Durable conversational agent.
//!
//! One stable remote cloud-chat target per local alias. The contract derives
//! the invoker's provenance (the human user, or one enrolled local peer)
//! separately and a provider session per `(target, source, canonical
//! conversation)`. Native therefore keys every session by the canonical
//! thread plus the caller's immutable source (agent UID + registration
//! epoch) and pins it: one source never overwrites, adopts or reads another
//! source's provider binding. The canonical Thread stays one durable work
//! conversation, with each entry attributed to its real sender.
//!
//! Native owns: the connection record, per-source grants, durable operation
//! correlation, canonical Threads appends and truthful projections. The
//! published HTTP contract lives in [`wire`].

use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::remote_cli::RemoteTarget;
use crate::store::cloud_agent::{CloudSource, Owner};
use crate::store::{CloudAgent, CloudOperation, Store, CLOUD_AGENT_CONSENT_SCOPE};

pub const PROVIDER: &str = "agenticos-cloud";
pub const ENDPOINT_KIND: &str = "cloud";
/// The contract's request cap: 32768 UTF-8 bytes.
pub const REQUEST_CAP: usize = 32_768;
pub const OPERATION_CAP: usize = 128;

pub const SCOPE_READ: &str = "cloud_agent.read";
pub const SCOPE_WRITE: &str = "cloud_agent.write";

/// The verified invoking source. It is derived by the daemon, never supplied
/// by a request. The operator is the human user; a peer is pinned to its
/// immutable local agent UID and registration epoch. Both carry the FULL
/// logical owner (issuer + subject) from the verified credential/enrollment,
/// so a replaced owner can never inherit another owner's grant, provider
/// session or cached reply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    pub kind: &'static str,
    pub uid: String,
    pub epoch: i64,
    pub owner: Owner,
}

/// The immutable registration stamp of a local agent row: its `agents.created`
/// witness in integer microseconds. This is not a generation counter.
pub fn registration_epoch(created: f64) -> i64 {
    ((created * 1_000_000.0).round() as i64).max(1)
}

impl Source {
    pub fn operator(owner: Owner) -> Source {
        Source {
            kind: crate::store::cloud_agent::SOURCE_USER,
            // The contract's stable user ownership input is
            // `user:<subject_id>` — never a credential/token/session id.
            uid: owner.subject.clone(),
            epoch: 0,
            owner,
        }
    }

    /// The immutable identity of a local agent row is its `agents.uid`
    /// (`agt-...`, minted once at registration, stable across daemon restart
    /// and resume, new after re-registration). The registration stamp is the
    /// row's immutable `agents.created` in integer microseconds; the existing
    /// `agent_identity` (`created` + `created_bits`, compared by
    /// `identity_matches`) verifies the original row. This is a registration
    /// timestamp, not a new generation counter: the UID is the alias-reuse
    /// fence.
    pub fn registered(agent: &crate::store::Agent, owner: Owner) -> Source {
        Source {
            kind: crate::store::cloud_agent::SOURCE_PEER,
            uid: agent.uid.clone(),
            epoch: registration_epoch(agent.created),
            owner,
        }
    }

    pub fn key(&self) -> (&'static str, &str, i64) {
        (self.kind, &self.uid, self.epoch)
    }

    pub fn matches(&self, operation: &CloudOperation) -> bool {
        operation.source_kind == self.kind
            && operation.source_uid == self.uid
            && operation.registration_epoch == self.epoch
            && operation.owner == self.owner
    }
}

/// One request/reply sender, derived by the daemon — never a request field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Sender {
    Operator,
    Agent(String),
}

impl Sender {
    pub(crate) fn from_caller(caller: &crate::peer::AgentCaller) -> Sender {
        match caller {
            crate::peer::AgentCaller::Operator => Sender::Operator,
            crate::peer::AgentCaller::Agent(alias) => Sender::Agent(alias.clone()),
        }
    }

    /// Resolve the durable source. The operator's stable ownership input is
    /// the saved login's verified issuer + subject for this exact target; a
    /// peer's is the verified parent recorded in its own enrolled credential
    /// together with its immutable agent row. A missing/re-registered row, a
    /// replaced human or an unenrolled peer is refused rather than
    /// approximated.
    pub fn source_for(&self, store: &Store, target: &RemoteTarget) -> Result<Source> {
        match self {
            Sender::Operator => {
                let pin = crate::remote_cli::cloud_agent_identity(target)?;
                Ok(Source::operator(Owner::new(pin.issuer, pin.subject_id)))
            }
            Sender::Agent(alias) => {
                let agent = store.agent(alias)?;
                let epoch = registration_epoch(agent.created);
                // Never the current human login: a peer's custody is its own
                // enrolled child credential, whose record names the parent.
                let pin = crate::remote_cli::cloud_agent_peer_owner(target, &agent.uid, epoch)?;
                Ok(Source::registered(
                    &agent,
                    Owner::new(pin.issuer, pin.subject_id),
                ))
            }
        }
    }

    pub fn label(&self) -> &str {
        match self {
            Sender::Operator => "operator",
            Sender::Agent(alias) => alias,
        }
    }

    pub fn to_json(&self) -> Value {
        match self {
            Sender::Operator => json!({"kind": "user", "label": "operator"}),
            Sender::Agent(alias) => json!({"kind": "local_peer", "label": alias}),
        }
    }
}

/// What the published contract reported for one connected cloud target.
#[derive(Clone, Debug)]
pub struct RemoteAgent {
    pub remote_agent_id: String,
    pub organization_id: String,
    pub audience: String,
    pub provenance: Value,
    /// The contract's stable, non-secret logical-owner id. Native pins it on
    /// the source and refuses an unexpected change.
    pub invoker_id: String,
    pub remote_session_id: String,
    pub provider_session_id: Option<String>,
    pub capabilities: Value,
    pub runtime: String,
    pub model_label: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemoteStatus {
    Queued,
    Running,
    Parked,
    Completed,
    Failed,
    Unknown,
}

impl RemoteStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            RemoteStatus::Queued => crate::store::cloud_agent::STATUS_QUEUED,
            RemoteStatus::Running => crate::store::cloud_agent::STATUS_RUNNING,
            RemoteStatus::Parked => crate::store::cloud_agent::STATUS_PARKED,
            RemoteStatus::Completed => crate::store::cloud_agent::STATUS_COMPLETED,
            RemoteStatus::Failed => crate::store::cloud_agent::STATUS_FAILED,
            RemoteStatus::Unknown => crate::store::cloud_agent::STATUS_UNKNOWN,
        }
    }
}

#[derive(Clone, Debug)]
pub struct SubmitRequest<'a> {
    pub remote_agent_id: &'a str,
    pub conversation_id: &'a str,
    pub operation_id: &'a str,
    pub text: &'a str,
    pub source: &'a Source,
}

#[derive(Clone, Debug)]
pub struct RemoteOperation {
    pub status: RemoteStatus,
    pub remote_operation_id: Option<String>,
    pub remote_session_id: Option<String>,
    pub provider_session_id: Option<String>,
    /// Present when the contract echoed the invoker (submit); readback does
    /// not restate identity because the credential proves it server-side.
    pub invoker_id: Option<String>,
    pub reply: Option<String>,
    pub error: Option<(String, String)>,
}

/// The published transport. Its one production implementation is
/// [`wire::HostedCloudTransport`]; there is no scripted/mock transport in a
/// release build.
pub trait CloudTransport {
    fn connect(&self, target: &RemoteTarget, conversation_id: &str) -> Result<RemoteAgent>;
    fn submit(&self, target: &RemoteTarget, request: &SubmitRequest<'_>)
        -> Result<RemoteOperation>;
    fn read(
        &self,
        target: &RemoteTarget,
        conversation_id: &str,
        operation_id: &str,
        source: &Source,
    ) -> Result<RemoteOperation>;
    fn cancel(
        &self,
        target: &RemoteTarget,
        conversation_id: &str,
        operation_id: &str,
        source: &Source,
    ) -> Result<()>;
}

pub fn validate_operation_id(value: &str) -> Result<()> {
    if value.len() < 8
        || value.len() > OPERATION_CAP
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    {
        return Err(Error::invalid(
            "cloud_agent_operation_invalid",
            "operation id must be 8..=128 ASCII letters, digits, '-' or '_'",
        ));
    }
    Ok(())
}

pub fn validate_request_text(text: &str) -> Result<()> {
    if text.trim().is_empty() {
        return Err(Error::invalid(
            "cloud_agent_request_empty",
            "a cloud-agent request needs non-empty text",
        ));
    }
    if text.len() > REQUEST_CAP {
        return Err(Error::invalid(
            "cloud_agent_request_too_large",
            format!("a cloud-agent request is at most {REQUEST_CAP} UTF-8 bytes"),
        ));
    }
    Ok(())
}

/// The connection's endpoint must be the issuer-verified board origin: HTTPS
/// with a host and no userinfo, path, query or fragment. `https://host` and
/// `https://host/` are the same origin (a saved hosted login stores the
/// former). A loopback HTTP origin is admitted only in a `test-seam` build so
/// the real client transport can be exercised against a local fixture.
pub fn validate_endpoint(endpoint: &str) -> Result<()> {
    let uri: ureq::http::Uri = endpoint
        .parse()
        .map_err(|_| Error::invalid("cloud_agent_endpoint", "cloud-agent endpoint is invalid"))?;
    let authority = uri.authority().ok_or_else(|| {
        Error::invalid("cloud_agent_endpoint", "cloud-agent endpoint needs a host")
    })?;
    let https = uri.scheme_str() == Some("https");
    let loopback = cfg!(feature = "test-seam")
        && uri.scheme_str() == Some("http")
        && matches!(uri.host(), Some("127.0.0.1") | Some("localhost"));
    let path = uri
        .path_and_query()
        .map(|p| p.as_str())
        .unwrap_or("/")
        .to_string();
    if (!https && !loopback)
        || authority.as_str().contains('@')
        || !(path == "/" || path.is_empty())
        || endpoint.contains('#')
    {
        return Err(Error::invalid(
            "cloud_agent_endpoint",
            "cloud-agent endpoint must be an exact HTTPS origin",
        ));
    }
    Ok(())
}

/// An opaque cloud conversation key namespaced by this exact local store
/// instance, so two Cadence instances never derive the same key for the same
/// alias. Canonical thread ids are UUIDv4, but the home conversation is
/// created before its thread exists; the instance namespace covers both.
fn conversation_key(instance: &str, scope: &str) -> String {
    let digest = CloudOperation::digest(instance, "cloud-conversation", scope, "", 0, "");
    format!("conv-{}", &digest[..40])
}

fn home_conversation_id(instance: &str, organization_id: &str, alias: &str) -> String {
    conversation_key(instance, &format!("home|{organization_id}|{alias}"))
}

/// What a connect adds to the store, the transport and the live-authority
/// check: the local alias it adopts, the human it was granted by, and the
/// working directory the credential was saved under.
pub struct ConnectRequest<'a> {
    pub alias: &'a str,
    pub owner: &'a Owner,
    pub cwd: &'a std::path::Path,
    pub granted_by: &'a str,
}

/// Connect the explicitly selected hosted workspace to its stable cloud
/// agent. `authorize` re-checks the live credential and both scopes
/// immediately before the request.
pub fn connect(
    store: &Store,
    transport: &dyn CloudTransport,
    target: &RemoteTarget,
    request: &ConnectRequest<'_>,
    authorize: impl Fn() -> Result<()>,
) -> Result<CloudAgent> {
    let ConnectRequest {
        alias,
        owner,
        cwd,
        granted_by,
    } = *request;
    validate_endpoint(&target.endpoint)?;
    crate::proto::identifier(alias, "Cloud agent alias")?;
    if let Some(existing) = store.agent_opt(alias)? {
        if existing.provider != PROVIDER || existing.archived_at.is_some() {
            return Err(Error::gate_coded(
                "cloud_agent_alias_taken",
                format!("agent alias '{alias}' is already in use by a local runtime"),
            ));
        }
    }
    authorize()?;
    // The handshake POST reloads the credential; pin the owner it must
    // still prove before any connect metadata is adopted.
    let bound_target = RemoteTarget {
        identity: Some(crate::remote_cli::CliIdentityPin {
            subject_id: owner.subject.clone(),
            issuer: owner.issuer.clone(),
        }),
        ..target.clone()
    };
    let conversation_id = home_conversation_id(store.instance_identity(), &target.org_id, alias);
    let remote = transport.connect(&bound_target, &conversation_id)?;
    if remote.remote_agent_id.is_empty() || remote.remote_agent_id.len() > 200 {
        return Err(Error::rejected(
            "cloud-agent contract returned an invalid remote agent id",
        ));
    }
    if remote.organization_id != target.org_id || remote.audience != target.endpoint {
        return Err(Error::gate_coded(
            "cloud_agent_wrong_tenant",
            "cloud-agent contract answered for a different organization or audience",
        ));
    }
    // Rev2: the human bridge credential's provenance kind is `user`.
    if remote.provenance["kind"].as_str() != Some(crate::store::cloud_agent::SOURCE_USER) {
        return Err(Error::gate_coded(
            "cloud_agent_provenance_mismatch",
            "cloud-agent contract derived a different principal than this connection \
             was created for",
        ));
    }
    for capability in ["tools", "mcp", "skills", "approvals", "cancellation"] {
        if remote.capabilities[capability].as_bool() == Some(true) {
            return Err(Error::gate_coded(
                "cloud_agent_capability_refused",
                format!(
                    "cloud runtime advertises unsupported {capability} capability; this \
                     connection is conversational-only"
                ),
            ));
        }
    }
    if remote.capabilities["conversation"].as_bool() != Some(true) {
        return Err(Error::gate_coded(
            "cloud_agent_capability_refused",
            "cloud runtime does not advertise conversational capability",
        ));
    }

    let now = crate::store::now_public();
    if store.agent_opt(alias)?.is_none() {
        let cwd = cwd.to_string_lossy().into_owned();
        let params = json!({"placement": "cloud", "endpoint": target.endpoint}).to_string();
        store.register_agent(&crate::store::NewAgent {
            alias,
            provider: PROVIDER,
            endpoint_kind: ENDPOINT_KIND,
            role: "worker",
            cwd: &cwd,
            sandbox: "read-only",
            instructions: None,
            params: Some(&params),
            team_role: None,
            model_policy: None,
        })?;
        store.set_gc_keep_by(alias, true, &json!({"reason": "cloud agent connection"}))?;
    }
    // The canonical recipient is the exact local registration this
    // connection was created against. It is pinned here, not re-read before
    // each send, so a later alias re-registration cannot inherit this
    // connection's turns or replies.
    let recipient_uid = store.agent(alias)?.uid;
    let agent = CloudAgent {
        id: local_id(&target.org_id, &remote.remote_agent_id),
        alias: alias.to_string(),
        organization_id: target.org_id.clone(),
        org_slug: target.org.clone(),
        endpoint: target.endpoint.clone(),
        remote_agent_id: remote.remote_agent_id.clone(),
        runtime: remote.runtime.clone(),
        model: remote.model_label.clone(),
        capabilities: remote.capabilities.clone(),
        provider_session_id: remote.provider_session_id.clone(),
        provenance: remote.provenance.clone(),
        recipient_uid,
        consent_scope: CLOUD_AGENT_CONSENT_SCOPE.to_string(),
        state: crate::store::cloud_agent::STATE_CONNECTED.to_string(),
        created: now,
        updated: now,
    };
    let agent = store.cloud_agent_connect(&agent)?;
    let source = Source::operator(owner.clone());
    store.cloud_agent_source_grant(&CloudSource {
        cloud_agent_id: agent.id.clone(),
        kind: source.kind.to_string(),
        uid: source.uid.clone(),
        registration_epoch: source.epoch,
        owner: source.owner.clone(),
        label: "operator".to_string(),
        scope: CLOUD_AGENT_CONSENT_SCOPE.to_string(),
        granted_by: granted_by.to_string(),
        granted_at: now,
        invoker_id: Some(remote.invoker_id.clone()),
        state: crate::store::cloud_agent::SOURCE_ACTIVE.to_string(),
    })?;

    let thread = store.ensure_thread(alias)?;
    store.cloud_agent_bind_session(
        &agent.id,
        source.kind,
        &source.uid,
        source.epoch,
        &source.owner,
        &thread.id,
        &conversation_id,
        &remote.remote_session_id,
        remote.provider_session_id.as_deref(),
        Some(&remote.invoker_id),
    )?;
    Ok(agent)
}

/// Bind the validated logical owner onto the transport target. The
/// credential load that supplies the HTTP token re-proves this exact
/// issuer+subject (`load_credential`), so a saved login replaced between
/// `authorize` and the request is refused instead of spending under the
/// replacement owner. Peer sources use their own custody, never the human.
fn target_for_source(target: &RemoteTarget, source: &Source) -> RemoteTarget {
    let mut bound = target.clone();
    bound.identity = (source.kind == crate::store::cloud_agent::SOURCE_USER).then(|| {
        crate::remote_cli::CliIdentityPin {
            subject_id: source.owner.subject.clone(),
            issuer: source.owner.issuer.clone(),
        }
    });
    bound
}

fn local_id(organization_id: &str, remote_agent_id: &str) -> String {
    let digest = CloudOperation::digest(organization_id, remote_agent_id, "", "", 0, "");
    format!("cld-{}", &digest[..24])
}

/// Send (or explicitly wake) one request as the derived source.
///
/// `wake=false` durably queues the request without transport. A later call
/// with the same operation id, identical payload and `wake=true` may submit
/// it; a different payload is a conflict, and an operation whose submit
/// outcome is unknown is resolved only by readback. `authorize` runs
/// immediately before the transport, so a revoked source/credential refuses
/// a queued or recovered turn before model spend.
#[allow(clippy::too_many_arguments)]
pub fn send(
    store: &Store,
    transport: &dyn CloudTransport,
    agent: &CloudAgent,
    target: &RemoteTarget,
    thread_id: &str,
    sender: &Sender,
    source: &Source,
    operation_id: &str,
    text: &str,
    wake: bool,
    expected_recipient_uid: Option<&str>,
    authorize: impl Fn() -> Result<()>,
) -> Result<CloudOperation> {
    validate_operation_id(operation_id)?;
    validate_request_text(text)?;
    let target = &target_for_source(target, source);
    if !agent.connected() {
        return Err(Error::gate_coded(
            "cloud_agent_not_connected",
            "cloud agent is disconnected — reconnect it explicitly first",
        ));
    }
    let thread = store.thread_by_id(thread_id)?.ok_or_else(|| {
        Error::invalid(
            "cloud_agent_thread_unknown",
            "cloud-agent request names an unknown work conversation",
        )
    })?;
    if thread.alias != agent.alias || thread.install_id.is_some() {
        return Err(Error::gate_coded(
            "cloud_agent_thread_refused",
            "cloud-agent requests are bound to this agent's own work conversation",
        ));
    }
    // A contextual reply captured the recipient it was composed for. The
    // authoritative comparison happens inside the enqueue transaction; this
    // early check only refuses the obvious stale value before any local row.
    if let Some(expected) = expected_recipient_uid {
        if expected != agent.recipient_uid {
            return Err(Error::rejected(
                "expected_recipient_uid does not name the cloud recipient this \
                 connection was created against; nothing was sent",
            ));
        }
    }
    let binding = store.cloud_agent_session(
        &agent.id,
        source.kind,
        &source.uid,
        source.epoch,
        &source.owner,
        thread_id,
    )?;
    let (conversation_id, remote_session_id) = match &binding {
        Some(session) => (
            session.conversation_id.clone(),
            session.remote_session_id.clone(),
        ),
        None => (
            conversation_key(store.instance_identity(), thread_id),
            String::new(),
        ),
    };
    let digest = CloudOperation::digest(
        &agent.id,
        thread_id,
        source.kind,
        &source.uid,
        source.epoch,
        text,
    );
    let now = crate::store::now_public();
    let draft = CloudOperation {
        operation_id: operation_id.to_string(),
        cloud_agent_id: agent.id.clone(),
        source_kind: source.kind.to_string(),
        source_uid: source.uid.clone(),
        registration_epoch: source.epoch,
        owner: source.owner.clone(),
        recipient_uid: agent.recipient_uid.clone(),
        sender_label: sender.label().to_string(),
        thread_id: thread_id.to_string(),
        conversation_id,
        remote_session_id,
        request_digest: digest,
        request_text: text.to_string(),
        status: crate::store::cloud_agent::STATUS_QUEUED.to_string(),
        remote_operation_id: None,
        message_id: None,
        reply_text: None,
        reply_message_id: None,
        error_code: None,
        error_message: None,
        created: now,
        updated: now,
    };
    // A canonical message id is global. A collision with a different
    // target/source must refuse BEFORE any operation claim exists, and a
    // failed enqueue must not leave a poisoned claim row behind.
    if let Some(existing) = store.message(operation_id)? {
        let sender_uid =
            (source.kind != crate::store::cloud_agent::SOURCE_USER).then_some(source.uid.as_str());
        let same_identity = existing.alias == agent.alias
            && existing.source == "cloud_agent"
            && existing.sender_uid.as_deref() == sender_uid
            && existing.recipient_uid.as_deref() == Some(agent.recipient_uid.as_str());
        if !same_identity {
            return Err(Error::conflict_coded(
                "cloud_agent_message_conflict",
                "this operation id is already a different target or source's canonical \
                 message; nothing was sent",
            ));
        }
        if existing.body != text {
            return Err(Error::conflict_coded(
                "cloud_agent_replay_conflict",
                "operation id was already used with a different request; nothing was sent",
            ));
        }
    }
    let (operation, created) = store.cloud_agent_operation_begin(&draft)?;
    if !created {
        // The witness was admitted by exactly one logical owner. A replaced
        // human or a re-enrolled peer alias is a different owner and never
        // inherits this operation's request, reply or provider session.
        if !source.matches(&operation) {
            return Err(Error::gate_coded(
                "cloud_agent_source_mismatch",
                "this operation belongs to a different verified logical owner; nothing \
                 was read or sent",
            ));
        }
        if let Some(expected) = expected_recipient_uid {
            if expected != operation.recipient_uid {
                return Err(Error::rejected(
                    "expected_recipient_uid does not name the recipient this operation \
                     was admitted for; nothing was sent",
                ));
            }
        }
        match operation.status.as_str() {
            crate::store::cloud_agent::STATUS_QUEUED
                if operation.remote_operation_id.is_none() && operation.reply_text.is_none() =>
            {
                if !wake {
                    // Queue-only replay: no transport, and no cached reply.
                    authorize()?;
                    return Ok(operation);
                }
                canonical_enqueue(store, agent, &operation, sender, expected_recipient_uid)?;
                submit_queued(
                    store, transport, agent, target, &operation, source, &authorize,
                )
            }
            // A settled row is still a cached read: re-prove the current
            // authority and source before returning it.
            _ => read_operation_with(
                store,
                transport,
                agent,
                target,
                &operation.operation_id,
                source,
                &authorize,
            ),
        }
    } else {
        if let Err(error) =
            canonical_enqueue(store, agent, &operation, sender, expected_recipient_uid)
        {
            // Nothing was sent and no canonical row exists for this claim:
            // remove it so the refusal does not poison the id.
            let _ = store.cloud_agent_operation_discard(&agent.id, &operation.operation_id);
            return Err(error);
        }
        if !wake {
            return Ok(operation);
        }
        submit_queued(
            store, transport, agent, target, &operation, source, &authorize,
        )
    }
}

fn submit_queued(
    store: &Store,
    transport: &dyn CloudTransport,
    agent: &CloudAgent,
    target: &RemoteTarget,
    operation: &CloudOperation,
    source: &Source,
    authorize: &impl Fn() -> Result<()>,
) -> Result<CloudOperation> {
    if let Some(other) = store.cloud_agent_unsettled(
        &agent.id,
        &operation.source_kind,
        &operation.source_uid,
        operation.registration_epoch,
        &operation.owner,
        &operation.conversation_id,
        &operation.operation_id,
    )? {
        return Err(Error::conflict_coded(
            "cloud_agent_session_busy",
            format!(
                "this work conversation already has an unsettled turn ({other}); read it \
                 back before sending another — nothing was sent"
            ),
        ));
    }
    authorize()?;
    let request = SubmitRequest {
        remote_agent_id: &agent.remote_agent_id,
        conversation_id: &operation.conversation_id,
        operation_id: &operation.operation_id,
        text: &operation.request_text,
        source,
    };
    // Durable submit intent BEFORE the request leaves: if the process dies
    // after the contract accepted but before this call returns, the row is
    // `running` with no remote id and the readback driver reconciles it. A
    // never-woken `queued` row stays excluded (no implicit submit).
    store.cloud_agent_operation_set(
        &operation.cloud_agent_id,
        &operation.operation_id,
        crate::store::cloud_agent::STATUS_RUNNING,
        None,
        None,
        None,
        None,
    )?;
    let outcome = match transport.submit(target, &request) {
        Ok(outcome) => outcome,
        Err(error) => return Err(record_transport_failure(store, agent, operation, error)),
    };
    apply_remote(store, agent, operation, outcome)
}

fn record_transport_failure(
    store: &Store,
    agent: &CloudAgent,
    operation: &CloudOperation,
    error: Error,
) -> Error {
    // Only a genuinely uncertain outcome is non-terminal. Known refusals —
    // including `session_busy` and `operation_conflict`, which the contract
    // decides BEFORE any model enqueue — settle the operation as failed and
    // are never replayed. A `busy` kind by itself is not uncertainty.
    let uncertain = matches!(error, Error::OutcomeUnknown(_))
        || matches!(
            error.code(),
            Some(
                "cloud_agent_uncertain" | "mutation_outcome_uncertain" | "cloud_agent_unavailable"
            )
        );
    if uncertain {
        let _ = store.cloud_agent_operation_set(
            &operation.cloud_agent_id,
            &operation.operation_id,
            crate::store::cloud_agent::STATUS_UNKNOWN,
            None,
            None,
            None,
            Some((
                "cloud_agent_uncertain",
                "the cloud call was interrupted; its outcome is unknown — read it back, \
                 do not resend it",
            )),
        );
        Error::unknown_coded(
            "cloud_agent_uncertain",
            "the cloud call was interrupted; its outcome is unknown — read it back, \
             do not resend it",
        )
    } else {
        // A definitive refusal happened before any model call. Settle the
        // canonical message AND the receipt together so it is never left
        // `running` forever; if that settle cannot run, keep it unknown so
        // readback may retry rather than declaring a false final result.
        let code = error
            .code()
            .unwrap_or("cloud_agent_remote_refused")
            .to_string();
        if canonical_finish(
            store,
            agent,
            operation,
            crate::store::cloud_agent::STATUS_FAILED,
            None,
            Some(&code),
            None,
        )
        .is_err()
        {
            let _ = store.cloud_agent_operation_set(
                &operation.cloud_agent_id,
                &operation.operation_id,
                crate::store::cloud_agent::STATUS_UNKNOWN,
                None,
                None,
                None,
                Some((&code, "a definitive refusal could not be settled locally")),
            );
        }
        error
    }
}

fn apply_remote(
    store: &Store,
    agent: &CloudAgent,
    operation: &CloudOperation,
    outcome: RemoteOperation,
) -> Result<CloudOperation> {
    // The contract's stable owner id is pinned on the source the first time
    // it is seen. A different one is a different logical owner: no reply is
    // recorded and no session is adopted.
    if let Some(invoker) = outcome.invoker_id.as_deref() {
        store.cloud_agent_source_pin_invoker(
            &operation.cloud_agent_id,
            &operation.source_kind,
            &operation.source_uid,
            operation.registration_epoch,
            &operation.owner,
            invoker,
        )?;
    }
    if let Some(session) = outcome.remote_session_id.as_deref() {
        if operation.remote_session_id.is_empty() {
            store.cloud_agent_bind_session(
                &agent.id,
                &operation.source_kind,
                &operation.source_uid,
                operation.registration_epoch,
                &operation.owner,
                &operation.thread_id,
                &operation.conversation_id,
                session,
                outcome.provider_session_id.as_deref(),
                outcome.invoker_id.as_deref(),
            )?;
        } else if operation.remote_session_id != session {
            store.cloud_agent_operation_set(
                &operation.cloud_agent_id,
                &operation.operation_id,
                crate::store::cloud_agent::STATUS_UNKNOWN,
                outcome.remote_operation_id.as_deref(),
                None,
                None,
                Some((
                    "cloud_agent_session_mismatch",
                    "the contract answered with a different provider session than this \
                     source's work conversation is bound to; no reply was recorded and \
                     the outcome stays unknown",
                )),
            )?;
            return Err(Error::unknown_coded(
                "cloud_agent_session_mismatch",
                "the contract answered with a different provider session than this \
                 source's work conversation is bound to; no reply was recorded",
            ));
        }
    }
    if outcome.status == RemoteStatus::Completed {
        let Some(reply) = outcome.reply.as_deref() else {
            store.cloud_agent_operation_set(
                &operation.cloud_agent_id,
                &operation.operation_id,
                crate::store::cloud_agent::STATUS_UNKNOWN,
                outcome.remote_operation_id.as_deref(),
                None,
                None,
                Some((
                    "cloud_agent_reply_missing",
                    "the contract reported completion without assistant text; the turn \
                     stays unknown and is read back — no reply was fabricated",
                )),
            )?;
            return Err(Error::unknown_coded(
                "cloud_agent_reply_missing",
                "the contract reported completion without assistant text — the turn is unknown",
            ));
        };
        return canonical_finish(
            store,
            agent,
            operation,
            crate::store::cloud_agent::STATUS_COMPLETED,
            Some(reply),
            None,
            outcome.remote_operation_id.as_deref(),
        );
    }
    let error = match (outcome.status, &outcome.error) {
        (RemoteStatus::Failed, Some((code, message))) => Some((code.as_str(), message.as_str())),
        (RemoteStatus::Failed, None) => Some((
            "cloud_agent_remote_failed",
            "the cloud runtime reported the turn failed",
        )),
        _ => None,
    };
    if outcome.status == RemoteStatus::Unknown {
        // An intermediate unknown is NOT a final result: no canonical thread
        // result and no deterministic report id are written. The operation
        // stays non-terminal so a later real completion can settle once.
        return canonical_finish(
            store,
            agent,
            operation,
            crate::store::cloud_agent::STATUS_UNKNOWN,
            None,
            Some("cloud_agent_uncertain"),
            outcome.remote_operation_id.as_deref(),
        );
    }
    if let Some((code, message)) = error {
        return canonical_finish(
            store,
            agent,
            operation,
            crate::store::cloud_agent::STATUS_FAILED,
            None,
            Some(code),
            outcome.remote_operation_id.as_deref(),
        )
        .map_err(|finish_error| {
            Error::unknown_coded(
                "cloud_agent_uncertain",
                format!("could not finish {code} ({message}): {finish_error}"),
            )
        });
    }
    store.cloud_agent_operation_set(
        &operation.cloud_agent_id,
        &operation.operation_id,
        outcome.status.as_str(),
        outcome.remote_operation_id.as_deref(),
        None,
        None,
        None,
    )
}

/// Read one operation back. This never invokes the model. The live
/// credential/scope is re-checked BEFORE any local row is returned or
/// disclosed, so a revoked source no longer reads the result through this
/// surface; an auth/connection refusal leaves the durable outcome unknown.
pub fn read_operation(
    store: &Store,
    transport: &dyn CloudTransport,
    agent: &CloudAgent,
    target: &RemoteTarget,
    operation_id: &str,
    source: &Source,
    authorize: impl Fn() -> Result<()>,
) -> Result<CloudOperation> {
    read_operation_with(
        store,
        transport,
        agent,
        target,
        operation_id,
        source,
        &authorize,
    )
}

fn read_operation_with(
    store: &Store,
    transport: &dyn CloudTransport,
    agent: &CloudAgent,
    target: &RemoteTarget,
    operation_id: &str,
    source: &Source,
    authorize: &impl Fn() -> Result<()>,
) -> Result<CloudOperation> {
    let operation = store
        .cloud_agent_operation(&agent.id, operation_id)?
        .ok_or_else(|| {
            Error::invalid(
                "cloud_agent_operation_unknown",
                "no such cloud-agent operation for this agent",
            )
        })?;
    if !source.matches(&operation) {
        return Err(Error::gate_coded(
            "cloud_agent_source_mismatch",
            "this operation belongs to a different invoking source or logical owner; \
             nothing was read",
        ));
    }
    authorize()?;
    if operation.settled() {
        return Ok(operation);
    }
    // A queue-only operation was never sent (no wake yet): reading it would
    // invent a remote "unknown" the contract never had and poison the later
    // explicit wake. Report the durable local queue state, zero transport.
    if operation.status == crate::store::cloud_agent::STATUS_QUEUED
        && operation.remote_operation_id.is_none()
    {
        return Ok(operation);
    }
    let target = &target_for_source(target, source);
    let outcome = match transport.read(target, &operation.conversation_id, operation_id, source) {
        Ok(outcome) => outcome,
        Err(Error::OutcomeUnknown(_)) => {
            return store.cloud_agent_operation_set(
                &operation.cloud_agent_id,
                &operation.operation_id,
                crate::store::cloud_agent::STATUS_UNKNOWN,
                operation.remote_operation_id.as_deref(),
                None,
                None,
                Some((
                    "cloud_agent_uncertain",
                    "readback was interrupted; the remote outcome is still unknown",
                )),
            );
        }
        Err(error) => return Err(error),
    };
    apply_remote(store, agent, &operation, outcome)
}

/// Explicit cancellation is unsupported: the contract answers
/// `cancel_unsupported` and no local state is ever rendered as stopped.
pub fn cancel(
    store: &Store,
    transport: &dyn CloudTransport,
    target: &RemoteTarget,
    agent: &CloudAgent,
    operation_id: &str,
    source: &Source,
    authorize: impl Fn() -> Result<()>,
) -> Result<()> {
    let operation = store
        .cloud_agent_operation(&agent.id, operation_id)?
        .ok_or_else(|| {
            Error::invalid(
                "cloud_agent_operation_unknown",
                "no such cloud-agent operation for this agent",
            )
        })?;
    if !source.matches(&operation) {
        return Err(Error::gate_coded(
            "cloud_agent_source_mismatch",
            "this operation belongs to a different invoking source or logical owner; \
             nothing was read",
        ));
    }
    authorize()?;
    let target = &target_for_source(target, source);
    transport.cancel(target, &operation.conversation_id, operation_id, source)
}

/// Enqueue the request as a real canonical `messages` row on the cloud
/// agent's alias. This is the same durable message lifecycle the daemon
/// uses for any worker: sender_uid/recipient_uid feed the cross-agent work
/// conversation projection, and `finish` routes the reply to `reply_to`
/// under its original UID fence.
fn canonical_enqueue(
    store: &Store,
    agent: &CloudAgent,
    operation: &CloudOperation,
    sender: &Sender,
    expected_recipient_uid: Option<&str>,
) -> Result<()> {
    let reply_to = match sender {
        Sender::Operator => None,
        Sender::Agent(alias) => Some(alias.as_str()),
    };
    let sender_row = match sender {
        Sender::Operator => crate::store::Sender::Operator,
        Sender::Agent(alias) => crate::store::Sender::Agent(alias.clone()),
    };
    let sender_uid = (operation.source_kind != crate::store::cloud_agent::SOURCE_USER)
        .then_some(operation.source_uid.as_str());
    // One core transaction fences the ORIGINAL source UID + registration
    // stamp and the pinned recipient UID before the row, marks the canonical
    // message running and attaches the operation witness. `expected_uid`
    // is the contextual reply's captured recipient, checked in the same
    // transaction — never a pre-read that could go stale.
    store.enqueue_cloud(
        &agent.alias,
        &operation.request_text,
        reply_to,
        &operation.operation_id,
        &sender_row,
        &operation.recipient_uid,
        expected_recipient_uid,
        sender_uid,
        operation.registration_epoch,
        &agent.id,
    )?;
    Ok(())
}

/// Persist the genuine result and route it in ONE core transaction.
///
/// The canonical message is finished (including its UID-fenced
/// `worker_result`) and the cloud receipt is CAS-settled together, so two
/// concurrent readbacks — or a crash between the two writes — can never
/// duplicate the Thread result or its deterministic report id. An
/// intermediate `unknown` is only a receipt update: it writes no result and
/// consumes no report id, leaving the later real completion able to settle.
fn canonical_finish(
    store: &Store,
    agent: &CloudAgent,
    operation: &CloudOperation,
    status: &str,
    reply: Option<&str>,
    error_code: Option<&str>,
    remote_operation_id: Option<&str>,
) -> Result<CloudOperation> {
    if status == crate::store::cloud_agent::STATUS_UNKNOWN {
        return store.cloud_agent_operation_set(
            &operation.cloud_agent_id,
            &operation.operation_id,
            crate::store::cloud_agent::STATUS_UNKNOWN,
            remote_operation_id,
            None,
            None,
            error_code.map(|code| (code, "the cloud turn's outcome is not known yet")),
        );
    }
    let result = json!({
        "status": status,
        "text": reply,
        "cloud_agent": agent.id,
        "cloud_agent_alias": agent.alias,
        "operation": operation.operation_id,
        "remote_operation_id": remote_operation_id,
        "remote_session_id": operation.remote_session_id,
        "runtime": agent.runtime,
        "model": agent.model,
        "error": error_code,
        "source": {
            "kind": operation.source_kind,
            "uid": operation.source_uid,
            "registration_epoch": operation.registration_epoch,
            "label": operation.sender_label,
            "owner": operation.owner.to_json(),
        },
    });
    store.finish_cloud(
        &operation.cloud_agent_id,
        &operation.operation_id,
        &agent.alias,
        status,
        &result,
        error_code,
        remote_operation_id,
        reply,
    )?;
    store
        .cloud_agent_operation(&operation.cloud_agent_id, &operation.operation_id)?
        .ok_or_else(|| {
            Error::unknown_coded(
                "cloud_agent_uncertain",
                "the settled cloud operation could not be read back",
            )
        })
}

/// The agent view: identity, runtime, truthful capabilities, per-source
/// sessions and canonical Threads links. Nothing claims a remote PID, read
/// receipt, stop or shared operator/peer context.
pub fn agent_view(store: &Store, agent: &CloudAgent) -> Result<Value> {
    let thread = store.thread(&agent.alias)?;
    let source_rows = store.cloud_agent_sources(&agent.id)?;
    let pairs = if source_rows
        .iter()
        .any(|source| source.kind == crate::store::cloud_agent::SOURCE_PEER)
    {
        store.work_conversation_pairs()?
    } else {
        Vec::new()
    };
    let sources: Vec<Value> = source_rows.iter().map(|source| source.to_json()).collect();
    let sessions: Vec<Value> = store
        .cloud_agent_sessions(&agent.id)?
        .iter()
        .map(|session| {
            let mut row = session.to_json();
            row["thread"] = thread_link(
                store,
                &session.thread_id,
                (session.source_kind == crate::store::cloud_agent::SOURCE_PEER)
                    .then_some((session.source_uid.as_str(), agent.recipient_uid.as_str())),
                &pairs,
            );
            row
        })
        .collect();
    let operations: Vec<Value> = store
        .cloud_agent_operations(&agent.id, None)?
        .iter()
        .map(|operation| {
            let mut row = operation.to_json();
            // Local status/attribution only: reply text is disclosed through
            // `cloud_agent_read`, which re-checks live authority first, or
            // through the canonical Thread under the board's own rules.
            row["reply"] = operation.reply_text.as_ref().map_or(
                Value::Null,
                |_| json!({"message_id": operation.reply_message_id, "present": true}),
            );
            row["thread"] = thread_link(
                store,
                &operation.thread_id,
                (operation.source_kind == crate::store::cloud_agent::SOURCE_PEER)
                    .then_some((operation.source_uid.as_str(), agent.recipient_uid.as_str())),
                &pairs,
            );
            row
        })
        .collect();
    let mut view = agent.to_json();
    view["thread"] = thread
        .as_ref()
        .map(|t| {
            json!({
                "id": t.id,
                "alias": t.alias,
                "link": format!("/threads?c={}", t.id),
            })
        })
        .unwrap_or(Value::Null);
    view["sources"] = json!(sources);
    view["sessions"] = json!(sessions);
    view["operations"] = json!(operations);
    Ok(view)
}

fn thread_link(
    store: &Store,
    thread_id: &str,
    pair: Option<(&str, &str)>,
    pairs: &[crate::store::conversations::PairSummary],
) -> Value {
    if let Some((sender_uid, recipient_uid)) = pair {
        let (lo, hi) = if sender_uid <= recipient_uid {
            (sender_uid, recipient_uid)
        } else {
            (recipient_uid, sender_uid)
        };
        // Use the same listed identity Threads can resolve, not a second
        // hash convention or a private human-room selector.
        return pairs
            .iter()
            .find(|pair| pair.a_uid == lo && pair.b_uid == hi)
            .map(|pair| {
                json!({
                    "id": pair.id,
                    "kind": "pair",
                    "alias": Value::Null,
                    "link": format!("/threads?c={}", pair.id),
                })
            })
            .unwrap_or(Value::Null);
    }
    match store.thread_by_id(thread_id) {
        Ok(Some(thread)) => json!({
            "id": thread.id,
            "alias": thread.alias,
            "link": format!("/threads?c={}", thread.id),
        }),
        _ => json!({"id": thread_id, "alias": Value::Null, "link": Value::Null}),
    }
}

/// Disconnect one cloud agent locally: stop admitting new turns, drop source
/// grants and keep canonical history. No remote stop is claimed.
pub fn disconnect(store: &Store, agent: &CloudAgent) -> Result<CloudAgent> {
    for source in store.cloud_agent_sources(&agent.id)? {
        if source.active() {
            store.cloud_agent_source_revoke(
                &agent.id,
                &source.kind,
                &source.uid,
                source.registration_epoch,
                &source.owner,
            )?;
        }
    }
    store.cloud_agent_set_state(&agent.id, crate::store::cloud_agent::STATE_DISCONNECTED)
}

pub mod enrollment;
pub mod wire;
