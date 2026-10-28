//! CAD-1085: the bounded keyless native model broker.
//!
//! The broker is the only path from the enrolled Pi runtime to the configured
//! infrastructure model. It owns:
//!
//! - the durable server-owned task binding taken before a Pi prompt
//!   ([`NativeModelBroker::bind_task`]), so a message/actor/session change
//!   fences new preparation;
//! - the three strict runtime operations (`prepare`, `invoke`, `result`) and
//!   their typed receipts ([`NativeModelBroker::handle_runtime`]);
//! - whole-operation admission: typed transcript + approved tool
//!   declarations + current role/model/effort/route/source are validated, all
//!   local rate/concurrency dimensions are reserved, the credential-free
//!   input binding is frozen and the committed ledger head is checkpointed
//!   **before** any private SDK code that can mint a JWT or send inference;
//! - the one-shot private SDK child and the closed mapping of its outcome;
//! - the trusted human source link ([`NativeModelBroker::apply_human_attempt`])
//!   for a fixed native provider purpose.
//!
//! There is no generic `text.generate` replacement here, no fallback model or
//! provider, no ambient credential discovery and no tool/agent runtime: the
//! selected provider protocol is the only thing the private child may speak.

pub(crate) mod catalog;
pub(crate) mod dto;
pub(crate) mod sdk;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use zeroize::Zeroizing;

use cadence_core::connection_dispatch::{pi_model_decision, Digest, ModelId, Revision, RoleV1};

use crate::daemon::credential_entry_rpc::{
    BoundedSecretInputV1, SecretKind, VerifiedHumanAttemptV1,
};
use crate::daemon::protected_runtime::{ManagementProfileEvidence, ProtectedRuntimeCoordinator};
use crate::daemon::Shared;
use crate::error::{Error, Result};
use crate::platform::encrypted_sqlite::ExpectedIdentity;
use crate::protected_runtime::ops::{OpsMethod, RuntimeCaller};
use crate::protected_runtime::strict;
use crate::store::native_model_turn as durable;

use self::catalog::NativeModelProfile;
use self::dto::{
    ClosedModelTurnErrorV1, InvokeArgsV1, NativeAssistantV1, NativeBillingProjectionV1,
    NativeMessageV1, NativeToolV1,
};

/// The absolute admitted-execution deadline for one private SDK child,
/// independent of the 10s frame budget.
const EXECUTION_DEADLINE: Duration = Duration::from_secs(60);
/// The vault `exchange` identity of a broker-private native model source.
const SOURCE_EXCHANGE: &str = "native-model-v1";
/// The one provider this initial qualified profile supports.
const PROVIDER: &str = "devin";
/// Prepare is finite even though it is not inference admission.
const PREPARE_PER_MINUTE: usize = 60;
const MAX_ARTIFACT_BYTES: usize = 1024 * 1024;

fn refused(code: &'static str, message: &str) -> Error {
    Error::invalid(code, message)
}

/// The operator's Pi policy is keyed by the settings catalog model id, while
/// the stored launch/agent model is `provider/id`. Accept the exact id, or the
/// bare catalog id when the provider is this profile's native route. The
/// provider is checked separately against the resolved route and source, so
/// this never lets a caller pick a different provider.
fn native_model_allowed(
    role: RoleV1,
    model: &ModelId,
    policy: Option<&cadence_core::connection_dispatch::RoleModelPolicyV1>,
) -> bool {
    if pi_model_decision(role, model, policy).is_ok() {
        return true;
    }
    let bare = catalog_model_id(model.as_str());
    bare != model.as_str()
        && ModelId::try_from(bare.to_string())
            .map(|id| pi_model_decision(role, &id, policy).is_ok())
            .unwrap_or(false)
}

/// The settings catalog row is keyed by the bare model id; the launch/agent
/// model carries `provider/id`. The native route fixes the provider, so the
/// catalog lookup uses the bare id only when the prefix is this route's.
fn catalog_model_id(model: &str) -> &str {
    match model.split_once('/') {
        Some((provider, bare)) if provider == PROVIDER => bare,
        _ => model,
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| i64::try_from(duration.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// Approved tool identity from the measured immutable registry. The broker
/// compares guest declarations through the exact observer schema-digest domain;
/// runtime qualification proves the implementation and loaded registry.
#[derive(Clone, Debug)]
pub(crate) struct NativeModelApprovedTool {
    pub name: String,
    pub schema_digest: String,
}

/// The live, measured runtime facts the broker admits. The coordinator
/// produces these from the authenticated owner recheck and the loaded
/// registry; no caller JSON can create one.
#[derive(Clone, Debug)]
pub(crate) struct NativeModelRuntimeEvidence {
    pub workspace_id: String,
    pub agent_uid: String,
    pub agent_alias: String,
    pub session_binding: String,
    pub runtime_id: String,
    pub runtime_generation: u64,
    pub owner_epoch: u64,
    pub os_uid: u32,
    pub node_pid: u32,
    pub node_starttime: u64,
    pub container_id: String,
    pub image_digest: String,
    pub profile_digest: String,
    pub manifest_digest: String,
    pub sdk_graph_digest: String,
    pub approved_tools: Vec<NativeModelApprovedTool>,
    pub snapshot_id: String,
    pub snapshot_expires_ms: u64,
    pub loaded_receipt_id: String,
}

/// The server-derived task binding the adapter takes before a Pi prompt.
#[derive(Clone, Debug)]
pub(crate) struct NativeModelTaskBinding {
    pub client_message_id: String,
    pub server_message_id: String,
    pub turn_token: String,
}

/// The broker. Holds only a `Weak` back-reference to the daemon, so there is
/// no `Arc` cycle and no second authority cache.
pub(crate) struct NativeModelBroker {
    shared: Weak<Shared>,
    coordinator: Arc<ProtectedRuntimeCoordinator>,
    profile: Option<NativeModelProfile>,
    /// The one active task per exact runtime/session. A restart drops it,
    /// which denies preparation until the adapter re-binds from durable state.
    active: Arc<Mutex<HashMap<String, durable::TaskRow>>>,
    /// Requests currently executing in this process. A second invoke is a
    /// status read, never a second provider call.
    inflight: Arc<Mutex<HashSet<String>>>,
    /// Finite prepare pacing. Prepare is not admission, so this is local.
    prepare_window: Mutex<HashMap<String, Vec<Instant>>>,
}

impl NativeModelBroker {
    pub(crate) fn new(
        shared: &Arc<Shared>,
        coordinator: Arc<ProtectedRuntimeCoordinator>,
    ) -> Result<Arc<Self>> {
        let profile = NativeModelProfile::load_default()?;
        Ok(Arc::new(Self {
            shared: Arc::downgrade(shared),
            coordinator,
            profile,
            active: Arc::new(Mutex::new(HashMap::new())),
            inflight: Arc::new(Mutex::new(HashSet::new())),
            prepare_window: Mutex::new(HashMap::new()),
        }))
    }

    fn shared(&self) -> Result<Arc<Shared>> {
        self.shared
            .upgrade()
            .ok_or_else(|| Error::invalid("runtime_unqualified", "the daemon is shutting down"))
    }

    fn profile(&self) -> Result<&NativeModelProfile> {
        self.profile.as_ref().ok_or_else(|| {
            refused(
                "route_unavailable",
                "no native model route is provisioned on this daemon",
            )
        })
    }

    fn route(&self, provider: &str) -> Result<&catalog::RouteFacts> {
        self.profile()?.route(provider).ok_or_else(|| {
            refused(
                "route_unavailable",
                "the configured provider has no qualified native model route",
            )
        })
    }

    fn runtime_key(runtime_id: &str, session_binding: &str) -> String {
        format!("{runtime_id}\u{1f}{session_binding}")
    }

    /// Live authenticated profile facts and their typed owner epoch. Human
    /// attempts must also match these facts to their already-claimed context.
    fn management_owner_evidence(
        &self,
        workspace_id: &str,
    ) -> Result<(ManagementProfileEvidence, Revision)> {
        let evidence = self.coordinator.management_profile_evidence()?;
        if evidence.workspace_id != workspace_id {
            return Err(refused(
                "request_changed",
                "the protected profile does not match the durable workspace",
            ));
        }
        if evidence.fenced {
            return Err(refused(
                "runtime_unqualified",
                "protected source writes are fenced",
            ));
        }
        let owner_epoch = Revision::new(evidence.owner_epoch)
            .map_err(|_| Error::internal("the authenticated owner epoch is not canonical"))?;
        Ok((evidence, owner_epoch))
    }

    fn measured(&self, caller: &RuntimeCaller) -> Result<NativeModelRuntimeEvidence> {
        let evidence = self.coordinator.measure_native_model_caller(caller)?;
        if evidence.agent_uid != caller.agent_uid()
            || evidence.session_binding != caller.session_binding()
            || evidence.runtime_id != caller.runtime_id()
            || evidence.runtime_generation != caller.runtime_generation()
            || evidence.os_uid != caller.os_uid()
            || evidence.node_pid != caller.peer_pid()
            || evidence.node_starttime != caller.peer_starttime()
        {
            return Err(refused(
                "runtime_unqualified",
                "only the measured Pi process may open a native model turn",
            ));
        }
        if evidence.node_pid == 0 || evidence.node_starttime == 0 {
            return Err(refused(
                "runtime_unqualified",
                "the measured Pi process identity is unavailable",
            ));
        }
        if evidence.runtime_generation == 0 || evidence.owner_epoch == 0 || evidence.os_uid == 0 {
            return Err(refused(
                "runtime_unqualified",
                "the measured runtime identity is incomplete",
            ));
        }
        for (value, what) in [
            (&evidence.workspace_id, "workspace"),
            (&evidence.agent_uid, "agent"),
            (&evidence.session_binding, "session"),
            (&evidence.runtime_id, "runtime"),
        ] {
            if value.is_empty() || value.len() > 200 || value.bytes().any(|b| b.is_ascii_control())
            {
                return Err(refused(
                    "runtime_unqualified",
                    &format!("the measured {what} identity is malformed"),
                ));
            }
        }
        for digest in [
            &evidence.image_digest,
            &evidence.profile_digest,
            &evidence.manifest_digest,
            &evidence.sdk_graph_digest,
        ] {
            strict::digest(digest).map_err(|_| {
                refused(
                    "runtime_unqualified",
                    "the measured runtime digest is malformed",
                )
            })?;
        }
        if !is_canonical_uuid(&evidence.snapshot_id)
            || !is_canonical_uuid(&evidence.loaded_receipt_id)
        {
            return Err(refused(
                "loaded_state_mismatch",
                "the loaded protected state selector is malformed",
            ));
        }
        if evidence.snapshot_expires_ms == 0 || (evidence.snapshot_expires_ms as i64) <= now_ms() {
            return Err(refused(
                "stale_configuration",
                "the loaded protected snapshot has expired",
            ));
        }
        for tool in &evidence.approved_tools {
            if tool.name.is_empty()
                || tool.name.len() > 64
                || !tool
                    .name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            {
                return Err(refused(
                    "runtime_unqualified",
                    "an approved tool declaration is malformed",
                ));
            }
            strict::digest(&tool.schema_digest).map_err(|_| {
                refused(
                    "runtime_unqualified",
                    "an approved tool digest is malformed",
                )
            })?;
        }
        Ok(evidence)
    }

    fn active_task(&self, evidence: &NativeModelRuntimeEvidence) -> Result<durable::TaskRow> {
        let key = Self::runtime_key(&evidence.runtime_id, &evidence.session_binding);
        let active = self
            .active
            .lock()
            .map_err(|_| Error::internal("native model task lock poisoned"))?;
        let task = active.get(&key).ok_or_else(|| {
            refused(
                "principal_required",
                "no server-owned model task is bound to this runtime turn",
            )
        })?;
        if task.state != durable::TASK_ACTIVE
            || task.agent_uid != evidence.agent_uid
            || task.session_binding != evidence.session_binding
        {
            return Err(refused("runtime_revoked", "the bound model task is fenced"));
        }
        Ok(task.clone())
    }

    /// The durable chat conversation this live runtime turn was started from.
    ///
    /// Every input is server-owned: the measured runtime, the active bound
    /// task and its durable client message. A private-room Ask resolves to
    /// the room its source post was written in; otherwise the message's own
    /// enqueue note names the conversation. An unbound turn refuses here —
    /// the random runtime session binding is never a chat.
    pub(crate) fn conversation_for_turn(&self, caller: &RuntimeCaller) -> Result<String> {
        let evidence = self.measured(caller)?;
        let task = self.active_task(&evidence)?;
        let shared = self.shared()?;
        if let Some(room) = shared
            .store
            .human_room_for_request(&task.client_message_id)?
        {
            return Ok(room);
        }
        shared
            .store
            .message_conversation(&evidence.agent_alias, &task.client_message_id)?
            .map(|thread| thread.id)
            .ok_or_else(|| {
                refused(
                    "request_not_ready",
                    "the bound model task has no durable chat conversation",
                )
            })
    }

    fn pace_prepare(&self, agent_uid: &str) -> Result<()> {
        let mut windows = self
            .prepare_window
            .lock()
            .map_err(|_| Error::internal("native model prepare lock poisoned"))?;
        let now = Instant::now();
        let entry = windows.entry(agent_uid.to_string()).or_default();
        entry.retain(|at| now.duration_since(*at) < Duration::from_secs(60));
        if entry.len() >= PREPARE_PER_MINUTE {
            return Err(refused(
                "rate_limit",
                "the native model preparation rate limit is exhausted",
            ));
        }
        entry.push(now);
        Ok(())
    }

    // -- runtime operations -------------------------------------------------

    pub(crate) fn handle_runtime(
        &self,
        caller: &RuntimeCaller,
        method: OpsMethod,
        args: &Value,
    ) -> Result<Value> {
        match method {
            OpsMethod::NativeModelTurnPrepareV1 => self.prepare(caller, args),
            OpsMethod::NativeModelTurnV1 => self.invoke(caller, args),
            OpsMethod::NativeModelTurnResultV1 => self.result(caller, args),
            _ => Err(refused(
                "operation_not_granted",
                "the runtime operation is not a native model turn",
            )),
        }
    }

    fn prepare(&self, caller: &RuntimeCaller, args: &Value) -> Result<Value> {
        let parsed = dto::parse_prepare_args(args)?;
        let evidence = self.measured(caller)?;
        let task = self.active_task(&evidence)?;
        self.pace_prepare(&evidence.agent_uid)?;
        let shared = self.shared()?;
        let route = self.route(PROVIDER)?.clone();

        struct Prepared {
            request_id: String,
            request_index: i64,
            configuration_id: String,
            configuration_revision: i64,
            snapshot_id: String,
            loaded_receipt_id: String,
        }

        let workspace = evidence.workspace_id.clone();
        let agent_uid = evidence.agent_uid.clone();
        let task_id = task.durable_task_id.clone();
        let index = i64::try_from(parsed.request_index)
            .map_err(|_| Error::invalid("invalid_request", "the request index is out of range"))?;
        let prepared = shared.store.write_tx(move |tx| {
            let authority = crate::store::connection_dispatch::authority_in(tx)?;
            if !authority.enabled || !authority.ever_enabled || authority.fenced {
                return Err(refused(
                    "runtime_unqualified",
                    "protected custody is not enabled on this store",
                ));
            }
            if authority.workspace_id != workspace {
                return Err(refused(
                    "loaded_state_mismatch",
                    "the protected workspace changed",
                ));
            }
            let live = durable::task_by_id_in(tx, &task_id)?
                .ok_or_else(|| refused("runtime_revoked", "the model task is unknown"))?;
            if live.state != durable::TASK_ACTIVE {
                return Err(refused("runtime_revoked", "the model task is fenced"));
            }
            if let Some(existing) =
                durable::request_by_index_in(tx, &workspace, &agent_uid, &task_id, index)?
            {
                return Ok(Prepared {
                    request_id: existing.request_id,
                    request_index: existing.request_index,
                    configuration_id: existing.configuration_id,
                    configuration_revision: existing.configuration_revision,
                    snapshot_id: existing.snapshot_id,
                    loaded_receipt_id: existing.loaded_receipt_id,
                });
            }
            let max = durable::max_request_index_in(tx, &task_id)?;
            let next = max.map(|value| value + 1).unwrap_or(0);
            if index != next {
                return Err(refused(
                    "invalid_request",
                    "the request index is not the contiguous next index",
                ));
            }
            let agent = durable::agent_facts_by_uid_in(tx, &agent_uid)?
                .ok_or_else(|| refused("principal_required", "the agent is unknown"))?;
            let model = agent
                .model
                .as_deref()
                .ok_or_else(|| refused("route_unavailable", "no native model is configured"))?;
            let role = role_of(&agent.role);
            let role_policy = crate::store::connection_dispatch::role_model_policy_in(tx)?;
            let model_id = ModelId::try_from(model.to_string()).map_err(|_| {
                refused(
                    "route_unavailable",
                    "the configured native model is malformed",
                )
            })?;
            if !native_model_allowed(role, &model_id, role_policy.as_ref()) {
                return Err(refused(
                    "operation_not_granted",
                    "the configured native model is not allowed for this role",
                ));
            }
            crate::store::connection_dispatch::require_model_provider_in(
                tx,
                catalog_model_id(model_id.as_str()),
                &route.provider,
            )?;
            let source = durable::active_source_in(tx, &workspace, PROVIDER)?.ok_or_else(|| {
                refused(
                    "auth_unavailable",
                    "no positive-human native model source is linked",
                )
            })?;
            let resolved = route
                .resolve(model, agent.effort.as_deref(), Some(source.origin.as_str()))
                .map_err(|_| {
                    refused(
                        "route_unavailable",
                        "the configured native model route is unavailable",
                    )
                })?;
            let revision = durable::next_configuration_revision_in(tx, &agent_uid)?;
            let configuration_id = uuid::Uuid::new_v4().to_string();
            let approved_tools = approved_tool_bindings(&route)?;
            let configuration = durable::ConfigurationRow {
                configuration_id: configuration_id.clone(),
                revision,
                workspace_id: workspace.clone(),
                agent_uid: agent_uid.clone(),
                session_binding: evidence.session_binding.clone(),
                runtime_id: evidence.runtime_id.clone(),
                runtime_generation: i64::try_from(evidence.runtime_generation)
                    .map_err(|_| Error::internal("runtime generation out of range"))?,
                owner_epoch: i64::try_from(evidence.owner_epoch)
                    .map_err(|_| Error::internal("owner epoch out of range"))?,
                role: agent.role.clone(),
                model: model.to_string(),
                model_uid: resolved.model_uid.clone(),
                effort: resolved.effort.clone(),
                provider: PROVIDER.to_string(),
                source_id: source.source_id.clone(),
                source_revision: source.revision,
                account: source.account.clone(),
                origin: resolved.origin.clone(),
                destination: destination(&resolved.origin),
                billing_route: "subscription".to_string(),
                sdk_graph_digest: evidence.sdk_graph_digest.clone(),
                approved_tools_json: approved_tools,
                manifest_digest: evidence.manifest_digest.clone(),
                snapshot_id: evidence.snapshot_id.clone(),
                snapshot_expires_ms: i64::try_from(evidence.snapshot_expires_ms)
                    .map_err(|_| Error::internal("snapshot expiry out of range"))?,
                loaded_receipt_id: evidence.loaded_receipt_id.clone(),
                max_output_tokens: resolved.max_output_tokens as i64,
                created_ms: now_ms(),
            };
            let stored = durable::put_configuration_in(tx, &configuration)?;
            let request_id = uuid::Uuid::new_v4().to_string();
            let row = durable::RequestRow {
                request_id: request_id.clone(),
                workspace_id: workspace.clone(),
                agent_uid: agent_uid.clone(),
                durable_task_id: task_id.clone(),
                request_index: index,
                configuration_id: stored.configuration_id.clone(),
                configuration_revision: stored.revision,
                snapshot_id: stored.snapshot_id.clone(),
                loaded_receipt_id: stored.loaded_receipt_id.clone(),
                source_id: stored.source_id.clone(),
                account: stored.account.clone(),
                state: durable::STATE_PREPARED.to_string(),
                input_digest: None,
                assistant_json: None,
                usage_json: None,
                error_json: None,
                billing_json: NativeBillingProjectionV1::NOT_DISPATCHED
                    .to_json()
                    .to_string(),
                created_ms: now_ms(),
            };
            durable::insert_prepared_in(tx, &row)?;
            Ok(Prepared {
                request_id,
                request_index: index,
                configuration_id: stored.configuration_id,
                configuration_revision: stored.revision,
                snapshot_id: stored.snapshot_id,
                loaded_receipt_id: stored.loaded_receipt_id,
            })
        })?;
        Ok(json!({
            "v": 1,
            "request_id": prepared.request_id,
            "request_index": prepared.request_index.to_string(),
            "configuration_id": prepared.configuration_id,
            "configuration_revision": prepared.configuration_revision.to_string(),
            "snapshot_id": prepared.snapshot_id,
            "loaded_receipt_id": prepared.loaded_receipt_id,
        }))
    }

    /// A bound request may only be re-read as a duplicate when the guest
    /// supplies exactly the selectors and input that were frozen. The frozen
    /// configuration is loaded by the row's own id, not by the parsed one, so
    /// a later configuration revision cannot turn a changed duplicate into a
    /// status read.
    fn verify_bound_duplicate(
        &self,
        parsed: &dto::InvokeArgsV1,
        row: &durable::RequestRow,
        evidence: &NativeModelRuntimeEvidence,
    ) -> Result<()> {
        let Some(existing) = row.input_digest.as_deref() else {
            return Ok(());
        };
        let shared = self.shared()?;
        let configuration = shared
            .store
            .native_model_configuration(&row.configuration_id)?
            .ok_or_else(|| refused("invalid_request", "the frozen configuration is unknown"))?;
        if parsed.configuration_id != row.configuration_id
            || parsed.expected_configuration_revision
                != u64::try_from(row.configuration_revision).unwrap_or(u64::MAX)
            || parsed.snapshot_id != row.snapshot_id
            || parsed.loaded_receipt_id != row.loaded_receipt_id
        {
            return Err(refused(
                "idempotency_conflict",
                "the model request was already bound to different selectors",
            ));
        }
        let route = self.route(PROVIDER)?.clone();
        let active = authorize_tools(&parsed.messages, evidence, &route)?;
        let binding = dto::operation_binding(parsed, &active)?;
        let digest = operation_input_digest(
            &binding,
            &configuration,
            parsed.max_output_tokens,
            parsed.tool_choice.as_str(),
        )?;
        if existing != digest {
            return Err(refused(
                "idempotency_conflict",
                "the model request was already bound to different input",
            ));
        }
        Ok(())
    }

    fn invoke(&self, caller: &RuntimeCaller, args: &Value) -> Result<Value> {
        let parsed = dto::parse_invoke_args(args)?;
        let evidence = self.measured(caller)?;
        let task = self.active_task(&evidence)?;
        let shared = self.shared()?;
        let route = self.route(PROVIDER)?.clone();

        let row = shared
            .store
            .native_model_request(&parsed.request_id)?
            .ok_or_else(|| refused("invalid_request", "the model request selector is unknown"))?;
        if row.workspace_id != evidence.workspace_id
            || row.agent_uid != evidence.agent_uid
            || row.durable_task_id != task.durable_task_id
        {
            return Err(refused(
                "principal_required",
                "the model request does not belong to this principal",
            ));
        }
        // A duplicate or settled request is a status read only when its
        // immutable selectors and input digest still match; a changed
        // duplicate is an idempotency conflict, never a second provider call.
        if row.state != durable::STATE_PREPARED || row.input_digest.is_some() {
            self.verify_bound_duplicate(&parsed, &row, &evidence)?;
            return receipt(&row);
        }
        let configuration = shared
            .store
            .native_model_configuration(&parsed.configuration_id)?
            .ok_or_else(|| {
                refused(
                    "stale_configuration",
                    "the model configuration is not current",
                )
            })?;
        if configuration.configuration_id != row.configuration_id
            || configuration.revision != row.configuration_revision
        {
            return Err(refused(
                "stale_configuration",
                "the model configuration changed",
            ));
        }
        if parsed.expected_configuration_revision
            != u64::try_from(configuration.revision).unwrap_or(u64::MAX)
        {
            return Err(refused(
                "stale_configuration",
                "the model configuration revision changed",
            ));
        }
        if parsed.snapshot_id != configuration.snapshot_id
            || parsed.loaded_receipt_id != configuration.loaded_receipt_id
        {
            return Err(refused(
                "loaded_state_mismatch",
                "the loaded protected state changed",
            ));
        }
        if configuration.snapshot_expires_ms <= now_ms() {
            return Err(refused(
                "stale_configuration",
                "the loaded protected snapshot has expired",
            ));
        }
        if configuration.runtime_generation
            != i64::try_from(evidence.runtime_generation).unwrap_or(-1)
            || configuration.owner_epoch != i64::try_from(evidence.owner_epoch).unwrap_or(-1)
            || configuration.runtime_id != evidence.runtime_id
            || configuration.session_binding != evidence.session_binding
            || configuration.workspace_id != evidence.workspace_id
            || configuration.manifest_digest != evidence.manifest_digest
            || configuration.sdk_graph_digest != evidence.sdk_graph_digest
            || configuration.snapshot_id != evidence.snapshot_id
            || configuration.loaded_receipt_id != evidence.loaded_receipt_id
            || u64::try_from(configuration.snapshot_expires_ms).ok()
                != Some(evidence.snapshot_expires_ms)
        {
            return Err(refused(
                "stale_configuration",
                "the protected runtime configuration changed",
            ));
        }
        if parsed.max_output_tokens == 0
            || i64::from(parsed.max_output_tokens) > configuration.max_output_tokens
        {
            return Err(refused(
                "payload_too_large",
                "the model output bound exceeds the frozen configuration",
            ));
        }

        // Current server authority: the agent settings, source and route must
        // still be exactly what the configuration froze.
        let agent = shared
            .store
            .native_model_agent(&evidence.agent_uid)?
            .ok_or_else(|| refused("principal_required", "the agent is unknown"))?;
        if agent.role != configuration.role
            || agent.model.as_deref() != Some(configuration.model.as_str())
        {
            return Err(refused(
                "stale_configuration",
                "the agent role, model or effort changed",
            ));
        }
        let source = shared
            .store
            .native_model_source(&evidence.workspace_id, &configuration.source_id)?
            .ok_or_else(|| refused("auth_unavailable", "the model source is unknown"))?;
        if source.state != durable::SOURCE_ACTIVE
            || source.revision != configuration.source_revision
            || source.account != configuration.account
            || source.origin != configuration.origin
        {
            return Err(refused(
                "auth_unavailable",
                "the model source changed or is not active",
            ));
        }
        if configuration.approved_tools_json != approved_tool_bindings(&route)? {
            return Err(refused(
                "stale_configuration",
                "the approved tool declarations changed",
            ));
        }
        let resolved = route
            .resolve(
                &configuration.model,
                agent.effort.as_deref(),
                Some(source.origin.as_str()),
            )
            .map_err(|_| refused("route_unavailable", "the model route is unavailable"))?;
        if resolved.model_uid != configuration.model_uid
            || resolved.origin != configuration.origin
            || resolved.effort != configuration.effort
            || resolved.provider != configuration.provider
            || i64::from(resolved.max_output_tokens) != configuration.max_output_tokens
        {
            return Err(refused(
                "route_unavailable",
                "the model route no longer matches the frozen configuration",
            ));
        }

        let active = authorize_tools(&parsed.messages, &evidence, &route)?;
        let binding = dto::operation_binding(&parsed, &active)?;
        let input_digest = operation_input_digest(
            &binding,
            &configuration,
            parsed.max_output_tokens,
            parsed.tool_choice.as_str(),
        )?;
        if let Some(existing) = &row.input_digest {
            if existing != &input_digest {
                return Err(refused(
                    "idempotency_conflict",
                    "the model request was already bound to different input",
                ));
            }
        }

        // One in-process executor per request. A concurrent duplicate sees the
        // status row and returns without provider traffic.
        {
            let mut inflight = self
                .inflight
                .lock()
                .map_err(|_| Error::internal("native model inflight lock poisoned"))?;
            if !inflight.insert(row.request_id.clone()) {
                let current = shared
                    .store
                    .native_model_request(&row.request_id)?
                    .unwrap_or(row.clone());
                // A concurrent duplicate is still a status read only for the
                // frozen selectors and input.
                self.verify_bound_duplicate(&parsed, &current, &evidence)?;
                return receipt(&current);
            }
        }
        let guard = InflightGuard {
            inflight: Arc::clone(&self.inflight),
            request_id: row.request_id.clone(),
        };

        let outcome = self.admit_and_execute(
            &shared,
            &evidence,
            &task,
            &route,
            &source,
            &configuration,
            &parsed,
            &input_digest,
        );
        drop(guard);
        outcome
    }

    #[allow(clippy::too_many_arguments)]
    fn admit_and_execute(
        &self,
        shared: &Arc<Shared>,
        evidence: &NativeModelRuntimeEvidence,
        task: &durable::TaskRow,
        route: &catalog::RouteFacts,
        source: &durable::SourceRow,
        configuration: &durable::ConfigurationRow,
        parsed: &InvokeArgsV1,
        input_digest: &str,
    ) -> Result<Value> {
        let vault = shared
            .platform_custody
            .encrypted()
            .cloned()
            .ok_or_else(|| {
                refused(
                    "auth_unavailable",
                    "the daemon has no protected encrypted custody",
                )
            })?;
        if vault.key_id().is_none() {
            return Err(refused("auth_unavailable", "protected custody is locked"));
        }

        let workspace = evidence.workspace_id.clone();
        let agent_uid = evidence.agent_uid.clone();
        let task_id = task.durable_task_id.clone();
        let request_id = parsed.request_id.clone();
        let configuration_id = configuration.configuration_id.clone();
        let configuration_revision = configuration.revision;
        let source_id = source.source_id.clone();
        let source_revision = source.revision;
        let digest = input_digest.to_string();
        let limits = route.limits;

        let admission = self
            .coordinator
            .native_model_admission_reader(evidence)
            .map_err(|_| {
                refused(
                    "runtime_revoked",
                    "the protected runtime is not current for admission",
                )
            })?;
        let admission_reader = &admission;
        let committed = shared.store.write_tx(move |tx| {
            admission_reader.require().map_err(|_| {
                refused(
                    "runtime_revoked",
                    "the runtime witness expired before commitment",
                )
            })?;
            let now = now_ms();
            let authority = crate::store::connection_dispatch::authority_in(tx)?;
            if !authority.enabled || authority.fenced {
                return Err(refused(
                    "runtime_unqualified",
                    "protected custody is not enabled on this store",
                ));
            }
            if authority.workspace_id != workspace {
                return Err(refused(
                    "loaded_state_mismatch",
                    "the protected workspace changed",
                ));
            }
            let live = durable::task_by_id_in(tx, &task_id)?
                .ok_or_else(|| refused("runtime_revoked", "the model task is unknown"))?;
            if live.state != durable::TASK_ACTIVE {
                return Err(refused("runtime_revoked", "the model task is fenced"));
            }
            let current = durable::configuration_in(tx, &configuration_id)?
                .ok_or_else(|| refused("stale_configuration", "the configuration is unknown"))?;
            if current.revision != configuration_revision || current.snapshot_expires_ms <= now {
                return Err(refused(
                    "stale_configuration",
                    "the model configuration changed",
                ));
            }
            let loaded = crate::store::connection_dispatch::current_snapshot_in(
                tx,
                &agent_uid,
                evidence.runtime_generation,
                now as u64,
            )?
            .ok_or_else(|| {
                refused(
                    "loaded_state_mismatch",
                    "the loaded protected state is no longer current",
                )
            })?;
            if loaded.snapshot.snapshot_id.as_str() != current.snapshot_id
                || loaded.receipt.loaded_receipt_id.as_str() != current.loaded_receipt_id
                || loaded.authorization_revision != authority.authorization_revision
            {
                return Err(refused(
                    "loaded_state_mismatch",
                    "the issued authority changed before commitment",
                ));
            }
            let agent = durable::agent_facts_by_uid_in(tx, &agent_uid)?
                .ok_or_else(|| refused("principal_required", "the agent is unknown"))?;
            if agent.role != current.role || agent.model.as_deref() != Some(current.model.as_str())
            {
                return Err(refused(
                    "stale_configuration",
                    "the agent role or model changed before commitment",
                ));
            }
            let model_id = ModelId::try_from(current.model)
                .map_err(|_| refused("stale_configuration", "the configured model is malformed"))?;
            let role_policy = crate::store::connection_dispatch::role_model_policy_in(tx)?;
            if !native_model_allowed(role_of(&agent.role), &model_id, role_policy.as_ref()) {
                return Err(refused(
                    "operation_not_granted",
                    "the configured model is not allowed for this role",
                ));
            }
            crate::store::connection_dispatch::require_model_provider_in(
                tx,
                catalog_model_id(model_id.as_str()),
                &route.provider,
            )?;
            let resolved = route.resolve(
                model_id.as_str(),
                agent.effort.as_deref(),
                Some(current.origin.as_str()),
            )?;
            if resolved.model_uid != current.model_uid
                || resolved.origin != current.origin
                || resolved.effort != current.effort
                || resolved.provider != current.provider
                || i64::from(resolved.max_output_tokens) != current.max_output_tokens
            {
                return Err(refused(
                    "stale_configuration",
                    "the effective model route changed before commitment",
                ));
            }
            let source_row = durable::source_in(tx, &workspace, &source_id)?
                .ok_or_else(|| refused("auth_unavailable", "the model source is unknown"))?;
            if source_row.state != durable::SOURCE_ACTIVE
                || source_row.revision != source_revision
                || source_row.account != current.account
                || source_row.origin != current.origin
                || source_row.provider != current.provider
            {
                return Err(refused(
                    "auth_unavailable",
                    "the model source changed or is not active",
                ));
            }
            let usage = durable::window_usage_in(tx, &workspace, &agent_uid, &source_id, now)?;
            if usage.agent_requests >= i64::from(limits.agent.requests_per_minute)
                || usage.account_requests >= i64::from(limits.account.requests_per_minute)
                || usage.workspace_requests >= i64::from(limits.workspace.requests_per_minute)
            {
                return Err(refused(
                    "rate_limit",
                    "the native model request rate limit is exhausted",
                ));
            }
            if usage.agent_unresolved >= i64::from(limits.agent.unresolved)
                || usage.account_unresolved >= i64::from(limits.account.unresolved)
                || usage.workspace_unresolved >= i64::from(limits.workspace.unresolved)
            {
                return Err(refused(
                    "concurrency_limit",
                    "the native model concurrency limit is exhausted",
                ));
            }
            let row = durable::request_in(tx, &request_id)?
                .ok_or_else(|| refused("invalid_request", "the request is unknown"))?;
            if row.state != durable::STATE_PREPARED {
                return Ok(None);
            }
            if let Some(existing) = &row.input_digest {
                if existing != &digest {
                    return Err(refused(
                        "idempotency_conflict",
                        "the model request was already bound to different input",
                    ));
                }
                return Ok(None);
            }
            durable::freeze_input_in(tx, &request_id, &digest)?;
            durable::commit_request_in(tx, &request_id, now)?;
            Ok(Some(resolved))
        })?;
        let Some(resolved) = committed else {
            let current = shared
                .store
                .native_model_request(&parsed.request_id)?
                .ok_or_else(|| refused("invalid_request", "the request is unknown"))?;
            // A concurrent commit won: the loser is a status read only for the
            // exact frozen input, never for a changed duplicate.
            self.verify_bound_duplicate(parsed, &current, evidence)?;
            return receipt(&current);
        };

        // The whole operation is reserved and committed; the private child may
        // run only after the external rollback floor holds this head.
        let digest_typed = Digest::try_from(runtime_binding_digest(
            evidence,
            &configuration.snapshot_id,
            &configuration.loaded_receipt_id,
        ))
        .map_err(|_| Error::internal("runtime binding digest is malformed"))?;
        let epoch = Revision::try_from(evidence.owner_epoch.to_string())
            .map_err(|_| Error::internal("owner epoch is malformed"))?;
        if let Err(_error) =
            shared
                .store
                .protected_checkpoint_head(&evidence.workspace_id, epoch, digest_typed)
        {
            drop(admission);
            let current = self.mark_unknown(shared, &parsed.request_id, 0, 0, 0)?;
            return receipt(&current);
        }
        drop(admission);

        // The secret is read only after admission and only into a zeroizing
        // buffer; it is never stored, logged or projected.
        let key_id: [u8; 16] = source
            .key_id
            .as_slice()
            .try_into()
            .map_err(|_| Error::internal("the native model source key id is malformed"))?;
        let identity = ExpectedIdentity {
            workspace_id: source.workspace_id.clone(),
            connection_id: source.source_id.clone(),
            provider: source.provider.clone(),
            account: source.account.clone(),
            credential_revision: u64::try_from(source.revision)
                .map_err(|_| Error::internal("the source revision is out of range"))?,
            exchange: SOURCE_EXCHANGE.to_string(),
            key_id,
        };
        let secret = match vault.read_exact(&source.vault_reference, &identity) {
            Ok(secret) => secret,
            Err(_) => {
                let current = self.mark_unknown(shared, &parsed.request_id, 0, 0, 0)?;
                return receipt(&current);
            }
        };
        if secret.is_empty() {
            let current = self.mark_unknown(shared, &parsed.request_id, 0, 0, 0)?;
            return receipt(&current);
        }

        let commitment = commitment_digest(
            input_digest,
            source,
            configuration,
            parsed.max_output_tokens,
            parsed.tool_choice.as_str(),
            evidence,
        )?;
        let messages = {
            let mut out = Vec::with_capacity(parsed.messages.len());
            for message in &parsed.messages {
                out.push(message.to_pi_json());
            }
            Value::Array(out)
        };
        let tools = {
            let active = dto::active_tools(&parsed.messages)?;
            Value::Array(active.iter().map(NativeToolV1::to_json).collect())
        };
        let plan = sdk::child_plan(
            &route.sdk,
            &route.provider,
            &configuration.origin,
            &configuration.model,
            &resolved.model_id,
            &configuration.model_uid,
            resolved.reasoning,
            &configuration.effort,
            &resolved.thinking_level_map,
            parsed.max_output_tokens,
            parsed.tool_choice.as_str(),
            &messages,
            &tools,
            &commitment,
            input_digest,
            &evidence.sdk_graph_digest,
            &source.source_id,
            &source.account,
            u64::try_from(source.revision).unwrap_or(0),
            &configuration.configuration_id,
            u64::try_from(configuration.revision).unwrap_or(0),
            &configuration.snapshot_id,
            &configuration.loaded_receipt_id,
            evidence.runtime_generation,
            evidence.owner_epoch,
        );
        let child = match sdk::run_child(sdk::ChildRequest {
            pins: &route.sdk,
            plan: &plan,
            api_key: &secret,
            deadline: EXECUTION_DEADLINE,
        }) {
            Ok(child) => child,
            Err(error) if matches!(error.code(), Some("route_unavailable")) => {
                return self
                    .settle_refused(
                        shared,
                        &parsed.request_id,
                        ClosedModelTurnErrorV1::route_unavailable(),
                    )
                    .and_then(|row| receipt(&row));
            }
            Err(error) if matches!(error.code(), Some("payload_too_large")) => {
                return self
                    .settle_refused(
                        shared,
                        &parsed.request_id,
                        ClosedModelTurnErrorV1::payload_too_large(),
                    )
                    .and_then(|row| receipt(&row));
            }
            Err(_) => {
                let current = self.mark_unknown(shared, &parsed.request_id, 0, 0, 0)?;
                return receipt(&current);
            }
        };
        self.settle_child(
            shared,
            evidence,
            configuration,
            route,
            parsed,
            &child,
            input_digest,
            &secret,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn settle_child(
        &self,
        shared: &Arc<Shared>,
        evidence: &NativeModelRuntimeEvidence,
        configuration: &durable::ConfigurationRow,
        route: &catalog::RouteFacts,
        parsed: &InvokeArgsV1,
        child: &sdk::ChildOutcome,
        input_digest: &str,
        secret: &[u8],
    ) -> Result<Value> {
        let (auth_requests, inference_requests) = child.counts();
        let billing = child.billing();
        let now = now_ms();
        let request_id = parsed.request_id.clone();
        match child {
            sdk::ChildOutcome::Done {
                assistant, usage, ..
            } => {
                let leaks = crate::platform::refuse_leak(
                    "native model result",
                    &assistant.to_json().to_string(),
                    secret,
                )
                .is_err();
                if leaks
                    || authorize_result(
                        assistant,
                        &parsed.messages,
                        evidence,
                        configuration,
                        route,
                        parsed,
                    )
                    .is_err()
                {
                    // The upstream turn completed and consumed real usage: a
                    // result refusal retains it. The unsafe assistant stays
                    // private and is never projected to the guest.
                    let row = shared.store.write_tx(|tx| {
                        let row = durable::request_in(tx, &request_id)?
                            .ok_or_else(|| refused("invalid_request", "the request is unknown"))?;
                        durable::settle_request_in(
                            tx,
                            &row,
                            durable::STATE_RESULT_REFUSED,
                            // A screened/unauthorised assistant is never
                            // persisted: the credential echo must not reach
                            // core SQLite or WAL.
                            None,
                            Some(usage.to_json().to_string().as_str()),
                            Some(
                                ClosedModelTurnErrorV1::result_refused()
                                    .to_json()
                                    .to_string()
                                    .as_str(),
                            ),
                            &billing.to_json().to_string(),
                            auth_requests,
                            inference_requests,
                            now,
                        )?;
                        durable::request_in(tx, &request_id)?
                            .ok_or_else(|| Error::internal("the request vanished"))
                    })?;
                    return receipt(&row);
                }
                let row = shared.store.write_tx(|tx| {
                    let row = durable::request_in(tx, &request_id)?
                        .ok_or_else(|| refused("invalid_request", "the request is unknown"))?;
                    durable::settle_request_in(
                        tx,
                        &row,
                        durable::STATE_DONE,
                        Some(assistant.to_json().to_string().as_str()),
                        Some(usage.to_json().to_string().as_str()),
                        None,
                        &billing.to_json().to_string(),
                        auth_requests,
                        inference_requests,
                        now,
                    )?;
                    durable::request_in(tx, &request_id)?
                        .ok_or_else(|| Error::internal("the request vanished"))
                })?;
                let _ = input_digest;
                let _ = configuration;
                receipt(&row)
            }
            sdk::ChildOutcome::Refused { error, .. } => {
                let row = if auth_requests > 0 {
                    // A known, complete authentication failure is not a
                    // pre-admission refusal: the operation was already
                    // admitted. Keep the real (empty) usage and refuse the
                    // result.
                    shared.store.write_tx(|tx| {
                        let row = durable::request_in(tx, &request_id)?
                            .ok_or_else(|| refused("invalid_request", "the request is unknown"))?;
                        durable::settle_request_in(
                            tx,
                            &row,
                            durable::STATE_RESULT_REFUSED,
                            None,
                            None,
                            Some(error.to_json().to_string().as_str()),
                            &billing.to_json().to_string(),
                            auth_requests,
                            inference_requests,
                            now,
                        )?;
                        durable::request_in(tx, &request_id)?
                            .ok_or_else(|| Error::internal("the request vanished"))
                    })?
                } else {
                    self.settle_refused(shared, &request_id, *error)?
                };
                receipt(&row)
            }
            sdk::ChildOutcome::Unknown { error, .. } => {
                let row = shared.store.write_tx(|tx| {
                    let row = durable::request_in(tx, &request_id)?
                        .ok_or_else(|| refused("invalid_request", "the request is unknown"))?;
                    durable::settle_request_in(
                        tx,
                        &row,
                        durable::STATE_UNKNOWN,
                        None,
                        None,
                        Some(error.to_json().to_string().as_str()),
                        &billing.to_json().to_string(),
                        auth_requests,
                        inference_requests,
                        now,
                    )?;
                    durable::request_in(tx, &request_id)?
                        .ok_or_else(|| Error::internal("the request vanished"))
                })?;
                let _ = evidence;
                receipt(&row)
            }
        }
    }

    fn settle_refused(
        &self,
        shared: &Arc<Shared>,
        request_id: &str,
        error: ClosedModelTurnErrorV1,
    ) -> Result<durable::RequestRow> {
        let now = now_ms();
        let request_id = request_id.to_string();
        shared.store.write_tx(move |tx| {
            let row = durable::request_in(tx, &request_id)?
                .ok_or_else(|| refused("invalid_request", "the request is unknown"))?;
            durable::settle_request_in(
                tx,
                &row,
                durable::STATE_REFUSED,
                None,
                None,
                Some(error.to_json().to_string().as_str()),
                &NativeBillingProjectionV1::NOT_DISPATCHED
                    .to_json()
                    .to_string(),
                0,
                0,
                now,
            )?;
            durable::request_in(tx, &request_id)?
                .ok_or_else(|| Error::internal("the request vanished"))
        })
    }

    fn mark_unknown(
        &self,
        shared: &Arc<Shared>,
        request_id: &str,
        auth_requests: i64,
        inference_requests: i64,
        _usage: i64,
    ) -> Result<durable::RequestRow> {
        let now = now_ms();
        let request_id = request_id.to_string();
        shared.store.write_tx(move |tx| {
            durable::note_unknown_in(
                tx,
                &request_id,
                auth_requests,
                inference_requests,
                &ClosedModelTurnErrorV1::outcome_unknown()
                    .to_json()
                    .to_string(),
                &NativeBillingProjectionV1::UNKNOWN.to_json().to_string(),
                now,
            )?;
            durable::request_in(tx, &request_id)?
                .ok_or_else(|| Error::internal("the request vanished"))
        })
    }

    fn result(&self, caller: &RuntimeCaller, args: &Value) -> Result<Value> {
        let parsed = dto::parse_result_args(args)?;
        let evidence = self.measured(caller)?;
        let shared = self.shared()?;
        let row = shared
            .store
            .native_model_request(&parsed.request_id)?
            .ok_or_else(|| refused("invalid_request", "the model request selector is unknown"))?;
        if row.workspace_id != evidence.workspace_id || row.agent_uid != evidence.agent_uid {
            return Err(refused(
                "principal_required",
                "the model request does not belong to this principal",
            ));
        }
        let task = shared
            .store
            .native_model_task_by_id(&row.durable_task_id)?
            .ok_or_else(|| refused("runtime_revoked", "the model task is unknown"))?;
        if task.session_binding != evidence.session_binding || task.agent_uid != evidence.agent_uid
        {
            return Err(refused(
                "runtime_revoked",
                "the model task does not belong to this runtime",
            ));
        }
        receipt(&row)
    }

    // -- durable task binding ----------------------------------------------

    pub(crate) fn bind_task(
        &self,
        caller: &RuntimeCaller,
        binding: &NativeModelTaskBinding,
    ) -> Result<NativeModelTaskGuard> {
        for (value, what) in [
            (&binding.client_message_id, "client message"),
            (&binding.server_message_id, "server message"),
            (&binding.turn_token, "turn token"),
        ] {
            if value.is_empty() || value.len() > 200 || value.bytes().any(|b| b.is_ascii_control())
            {
                return Err(refused(
                    "invalid_request",
                    &format!("the {what} binding is malformed"),
                ));
            }
        }
        let evidence = self.measured(caller)?;
        let shared = self.shared()?;
        let message = shared
            .store
            .native_model_message(&binding.client_message_id)?
            .ok_or_else(|| refused("principal_required", "the durable message does not exist"))?;
        let message_agent = shared
            .store
            .agent(&message.alias)
            .map_err(|_| refused("principal_required", "the durable message owner is unknown"))?;
        if message_agent.uid != evidence.agent_uid {
            return Err(refused(
                "principal_required",
                "the durable message belongs to another agent",
            ));
        }
        if !matches!(message.state.as_str(), "queued" | "submitting" | "running") {
            let _ = shared.store.event_public(
                &message.alias,
                "native_task_binding_refused",
                serde_json::json!({
                    "message": binding.client_message_id,
                    "state": message.state,
                }),
            );
            return Err(refused(
                "runtime_revoked",
                "the durable message is no longer running",
            ));
        }
        if let Some(turn) = &message.turn_id {
            if turn != &binding.turn_token {
                return Err(refused(
                    "runtime_revoked",
                    "the durable message belongs to another turn",
                ));
            }
        }
        let workspace = evidence.workspace_id.clone();
        let agent_uid = evidence.agent_uid.clone();
        let session_binding = evidence.session_binding.clone();
        let client_message_id = binding.client_message_id.clone();
        let server_message_id = binding.server_message_id.clone();
        let turn_token = binding.turn_token.clone();
        let candidate = durable::TaskRow {
            durable_task_id: uuid::Uuid::new_v4().to_string(),
            workspace_id: workspace.clone(),
            agent_uid: agent_uid.clone(),
            session_binding: session_binding.clone(),
            client_message_id: client_message_id.clone(),
            server_message_id,
            turn_token: turn_token.clone(),
            state: durable::TASK_ACTIVE.to_string(),
            created_ms: now_ms(),
        };
        let task = shared.store.write_tx(move |tx| {
            let live = durable::bind_task_in(tx, &candidate)?;
            if live.state != durable::TASK_ACTIVE {
                return Err(refused("runtime_revoked", "the model task is fenced"));
            }
            Ok(live)
        })?;
        if task.agent_uid != agent_uid || task.session_binding != session_binding {
            return Err(refused(
                "principal_required",
                "the durable message belongs to another runtime",
            ));
        }
        let key = Self::runtime_key(&evidence.runtime_id, &evidence.session_binding);
        {
            let mut active = self
                .active
                .lock()
                .map_err(|_| Error::internal("native model task lock poisoned"))?;
            if let Some(existing) = active.get(&key) {
                if existing.durable_task_id != task.durable_task_id {
                    return Err(refused(
                        "concurrency_limit",
                        "another model turn is already active on this runtime",
                    ));
                }
            }
            active.insert(key.clone(), task.clone());
        }
        Ok(NativeModelTaskGuard {
            shared: Arc::downgrade(&shared),
            active: Arc::clone(&self.active),
            task_id: task.durable_task_id,
            runtime_key: key,
            fenced: Mutex::new(false),
        })
    }

    // -- trusted human source link -----------------------------------------

    pub(crate) fn apply_human_attempt(
        &self,
        attempt: &VerifiedHumanAttemptV1,
        input: &BoundedSecretInputV1,
    ) -> Result<Value> {
        if attempt.provider() != PROVIDER || attempt.connection_id().is_some() {
            return Err(refused(
                "unsupported_transport",
                "the native model source request is not a supported provider link",
            ));
        }
        let shared = self.shared()?;
        let workspace = shared.store.connection_workspace_id()?;
        let vault = shared
            .platform_custody
            .encrypted()
            .cloned()
            .ok_or_else(|| {
                refused(
                    "auth_unavailable",
                    "the daemon has no protected encrypted custody",
                )
            })?;
        let route = self.route(PROVIDER)?.clone();
        let kind = match attempt.shape() {
            Some("native-model-api-key") => "api_key",
            Some("native-model-devin-credentials") => "credentials",
            _ => {
                return Err(refused(
                    "unsupported_content",
                    "the native model source submission is not supported",
                ))
            }
        };
        match attempt.verb() {
            "verify" if input.kind() != SecretKind::None => {
                return Err(refused(
                    "invalid_request",
                    "a verification request carries no credential",
                ));
            }
            "enroll" | "rotate" => {
                let expected = if kind == "api_key" {
                    SecretKind::NativeProviderApiKey
                } else {
                    SecretKind::NativeProviderDevinCredentials
                };
                if input.kind() != expected {
                    return Err(refused(
                        "invalid_request",
                        "the submitted credential does not match the requested shape",
                    ));
                }
            }
            "verify" => {
                // The initial qualified profile has no reviewed human
                // authentication-only adapter for this provider. Decrypting
                // custody proves the material exists, not that the provider
                // accepted the sign-in, so a presence check must never complete
                // a verification attempt as verified. Refusing here changes no
                // source or verification state.
                return Err(refused(
                    "unsupported_verification",
                    "this profile has no reviewed sign-in verification for the native model source",
                ));
            }
            _ => {
                return Err(refused(
                    "invalid_request",
                    "the credential request verb is unsupported",
                ))
            }
        }

        let (bytes, origin_hint) = match kind {
            "api_key" => {
                let text = std::str::from_utf8(input.expose())
                    .map_err(|_| refused("invalid_request", "the native model key is unusable"))?;
                let trimmed = text.trim();
                if trimmed.is_empty()
                    || trimmed.len() > 8192
                    || trimmed
                        .bytes()
                        .any(|b| b.is_ascii_control() || b == b' ' || b == b'\t')
                {
                    return Err(refused(
                        "invalid_request",
                        "the native model key is unusable",
                    ));
                }
                (Zeroizing::new(trimmed.as_bytes().to_vec()), None)
            }
            "credentials" => {
                let text = std::str::from_utf8(input.expose()).map_err(|_| {
                    refused("unsupported_content", "the credential file is not UTF-8")
                })?;
                if text.len() > 65_536 {
                    return Err(refused(
                        "payload_too_large",
                        "the credential file exceeds its bound",
                    ));
                }
                let parsed = parse_devin_credentials(text)?;
                (Zeroizing::new(parsed.0), parsed.1)
            }
            _ => unreachable!(),
        };

        let key_id = vault
            .key_id()
            .ok_or_else(|| refused("auth_unavailable", "protected custody is locked"))?;
        let rotated = attempt.verb() == "rotate";
        let existing =
            if rotated {
                Some(shared.store.native_model_rotation_source(
                    attempt.target_id(),
                    PROVIDER,
                    attempt.expected_credential_revision().ok_or_else(|| {
                        refused("invalid_request", "the source revision is required")
                    })?,
                )?)
            } else {
                None
            };
        let account = existing
            .as_ref()
            .map(|source| source.account.clone())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let origin = match &origin_hint {
            Some(origin) => route.require_origin(origin)?,
            None => existing
                .as_ref()
                .map(|source| source.origin.clone())
                .unwrap_or_else(|| route.default_origin.clone()),
        };
        let source_id = existing
            .as_ref()
            .map(|source| source.source_id.clone())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let revision = existing
            .as_ref()
            .map(|source| source.revision + 1)
            .unwrap_or(1);
        let catalog_revision = i64::try_from(route.catalog_revision)
            .map_err(|_| Error::internal("catalog revision out of range"))?;
        let model_uid_map = model_uid_map(&route)?;
        crate::platform::refuse_leak(
            "native model source",
            &format!(
                "{source_id} {account} {origin} {} {model_uid_map}",
                revision
            ),
            &bytes,
        )
        .map_err(|_| {
            refused(
                "invalid_request",
                "the native model source would disclose its own key",
            )
        })?;
        // Authenticated evidence first: no custody or metadata write happens
        // unless the live owner/profile chain answers with its real epoch. The
        // ledger's `database_epoch` is never the checkpoint's owner epoch — an
        // owner restart advances the owner epoch without restoring or
        // incrementing the database.
        let (evidence, owner_epoch) = self.management_owner_evidence(&workspace)?;
        attempt.require_management_context(
            &evidence.workspace_id,
            evidence.custody_domain,
            evidence.database_epoch,
            evidence.owner_epoch,
            &evidence.supervisor_generation,
        )?;
        let identity = ExpectedIdentity {
            workspace_id: workspace.clone(),
            connection_id: source_id.clone(),
            provider: PROVIDER.to_string(),
            account: account.clone(),
            credential_revision: u64::try_from(revision)
                .map_err(|_| Error::internal("the source revision is out of range"))?,
            exchange: SOURCE_EXCHANGE.to_string(),
            key_id,
        };
        let reference = vault.write_row(&identity, bytes.as_slice())?;
        let now = now_ms();
        let source_id_for_tx = source_id.clone();
        let workspace_for_tx = workspace.clone();
        let origin_for_tx = origin.clone();
        let account_for_tx = account.clone();
        let model_uid_map_for_tx = model_uid_map.clone();
        let reference_for_tx = reference.clone();
        let key_id_for_tx = key_id.to_vec();
        let action = if rotated { "rotated" } else { "linked" };
        let existing_revision = existing.as_ref().map(|source| source.revision);
        // The exact row this commit produces. The checkpoint binds this
        // binding, so it is derived before the mutation and verified against
        // the row that actually commits.
        let next = durable::SourceRow {
            source_id: source_id.clone(),
            workspace_id: workspace.clone(),
            provider: PROVIDER.to_string(),
            account: account.clone(),
            revision,
            origin: origin.clone(),
            origin_kind: if kind == "credentials" {
                durable::ORIGIN_CREDENTIALS.to_string()
            } else {
                durable::ORIGIN_PINNED.to_string()
            },
            catalog_revision,
            model_uid_map: model_uid_map.clone(),
            vault_reference: reference.clone(),
            key_id: key_id.to_vec(),
            state: durable::SOURCE_ACTIVE.to_string(),
            created_ms: existing
                .as_ref()
                .map(|source| source.created_ms)
                .unwrap_or(now),
            updated_ms: now,
        };
        let binding = material_digest_for_runtime(&next, &workspace)?;
        let digest = Digest::try_from(binding.clone())
            .map_err(|_| Error::internal("source binding digest is malformed"))?;
        // One commit and one external acknowledgement. A lost acknowledgement
        // fences protected admission and answers `outcome_unknown`; the
        // mutation is never re-run.
        let stored = shared.store.protected_management_commit(
            &workspace,
            owner_epoch,
            digest,
            move |tx| {
                if let Some(expected) = existing_revision {
                    durable::rotate_source_in(
                        tx,
                        &workspace_for_tx,
                        &source_id_for_tx,
                        expected,
                        &account_for_tx,
                        &origin_for_tx,
                        if kind == "credentials" {
                            durable::ORIGIN_CREDENTIALS
                        } else {
                            durable::ORIGIN_PINNED
                        },
                        catalog_revision,
                        &model_uid_map_for_tx,
                        &reference_for_tx,
                        &key_id_for_tx,
                        now,
                    )?;
                } else {
                    durable::insert_source_in(tx, &next)?;
                }
                durable::source_history_in(
                    tx,
                    &source_id_for_tx,
                    revision,
                    action,
                    &format!("human:{}", attempt.request_id()),
                    now,
                )?;
                let stored = durable::source_in(tx, &workspace_for_tx, &source_id_for_tx)?
                    .ok_or_else(|| Error::internal("the native model source vanished"))?;
                if material_digest_for_runtime(&stored, &workspace_for_tx)? != binding {
                    return Err(Error::internal(
                        "the native model source binding changed under its commit",
                    ));
                }
                Ok(stored)
            },
        )?;
        Ok(json!({
            "source_id": stored.source_id,
            "revision": stored.revision.to_string(),
            "state": stored.state,
            "verification": "saved",
        }))
    }

    /// The trusted owner hook that fences a source for new preparation and
    /// admission. The private child is per-turn, so no cached key survives.
    pub(crate) fn fence_source(
        &self,
        workspace_id: &str,
        source_id: &str,
        owner_epoch: Revision,
        state: &str,
    ) -> Result<()> {
        let state = match state {
            "locked" => durable::SOURCE_LOCKED,
            "revoked" => durable::SOURCE_REVOKED,
            _ => {
                return Err(refused(
                    "invalid_request",
                    "the source state is unsupported",
                ))
            }
        };
        let shared = self.shared()?;
        let workspace = workspace_id.to_string();
        let id = source_id.to_string();
        let action = if state == durable::SOURCE_REVOKED {
            "revoked"
        } else {
            "locked"
        };
        let now = now_ms();
        let state = state.to_string();
        // Preserve the management nonce's already-checked owner epoch. A later
        // owner change may leave an unknown fenced commitment, never success
        // under a freshly rebound epoch.
        let mut expected = shared
            .store
            .native_model_source(&workspace, &id)?
            .ok_or_else(|| refused("auth_unavailable", "the model source is unknown"))?;
        expected.state = state.clone();
        let binding = material_digest_for_runtime(&expected, &workspace)?;
        let digest = Digest::try_from(binding.clone())
            .map_err(|_| Error::internal("source binding digest is malformed"))?;
        let workspace_for_tx = workspace.clone();
        shared
            .store
            .protected_management_commit(&workspace, owner_epoch, digest, move |tx| {
                let current = durable::source_in(tx, &workspace_for_tx, &id)?
                    .ok_or_else(|| refused("auth_unavailable", "the model source is unknown"))?;
                durable::set_source_state_in(tx, &workspace_for_tx, &id, &state, now)?;
                durable::source_history_in(tx, &id, current.revision, action, "owner", now)?;
                let fenced = durable::source_in(tx, &workspace_for_tx, &id)?
                    .ok_or_else(|| Error::internal("the native model source vanished"))?;
                if material_digest_for_runtime(&fenced, &workspace_for_tx)? != binding {
                    // Never commit a fence that a concurrent rotation made
                    // stale; the transaction rolls back, nothing was applied.
                    return Err(refused(
                        "stale_configuration",
                        "the native model source changed under its commit",
                    ));
                }
                Ok(())
            })
    }
}

/// A held turn binding. Dropping it fences the task for new preparation; the
/// adapter may also call [`Self::fence`] explicitly on settle/interrupt.
pub(crate) struct NativeModelTaskGuard {
    shared: Weak<Shared>,
    active: Arc<Mutex<HashMap<String, durable::TaskRow>>>,
    task_id: String,
    runtime_key: String,
    fenced: Mutex<bool>,
}

impl NativeModelTaskGuard {
    pub(crate) fn fence(&self, reason: &str) {
        let mut fenced = match self.fenced.lock() {
            Ok(fenced) => fenced,
            Err(_) => return,
        };
        if *fenced {
            return;
        }
        *fenced = true;
        if let Some(shared) = self.shared.upgrade() {
            let task_id = self.task_id.clone();
            let reason = reason.to_string();
            let now = now_ms();
            let _ = shared
                .store
                .write_tx(move |tx| durable::fence_task_in(tx, &task_id, &reason, now));
        }
        if let Ok(mut active) = self.active.lock() {
            if active
                .get(&self.runtime_key)
                .map(|task| task.durable_task_id == self.task_id)
                .unwrap_or(false)
            {
                active.remove(&self.runtime_key);
            }
        }
    }
}

impl Drop for NativeModelTaskGuard {
    fn drop(&mut self) {
        self.fence("the native model turn ended");
    }
}

/// Remove the in-flight marker on every exit path.
struct InflightGuard {
    inflight: Arc<Mutex<HashSet<String>>>,
    request_id: String,
}

impl Drop for InflightGuard {
    fn drop(&mut self) {
        if let Ok(mut inflight) = self.inflight.lock() {
            inflight.remove(&self.request_id);
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn role_of(role: &str) -> RoleV1 {
    if role == "master" {
        RoleV1::Master
    } else {
        RoleV1::Worker
    }
}

fn is_canonical_uuid(value: &str) -> bool {
    let b = value.as_bytes();
    b.len() == 36
        && b.iter().enumerate().all(|(i, c)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                *c == b'-'
            } else {
                c.is_ascii_digit() || (b'a'..=b'f').contains(c)
            }
        })
}

fn destination(origin: &str) -> String {
    format!("{origin}/exa.api_server_pb.ApiServerService/GetChatMessage")
}

fn approved_tool_bindings(route: &catalog::RouteFacts) -> Result<String> {
    let mut items: Vec<Value> = Vec::with_capacity(route.approved_tools.len());
    for tool in &route.approved_tools {
        items.push(json!({
            "name": tool.name,
            "declaration_digest": tool.declaration_digest()?,
            "schema_digest": tool.schema_digest()?,
        }));
    }
    items.sort_by(|left, right| {
        left["name"]
            .as_str()
            .unwrap_or("")
            .cmp(right["name"].as_str().unwrap_or(""))
    });
    serde_json::to_string(&cadence_core::connection_dispatch::canonical_value(
        &json!({
            "v": 1,
            "tools": items,
            "set_digest": route.approved_tools_digest,
        }),
    ))
    .map_err(|_| Error::internal("the approved tool set could not be encoded"))
}

fn canonical_declaration(tool: &NativeToolV1) -> Result<String> {
    serde_json::to_string(&cadence_core::connection_dispatch::canonical_value(
        &tool.to_json(),
    ))
    .map_err(|_| Error::internal("the tool declaration could not be encoded"))
}

fn authorize_tools(
    messages: &[NativeMessageV1],
    evidence: &NativeModelRuntimeEvidence,
    route: &catalog::RouteFacts,
) -> Result<Vec<NativeToolV1>> {
    use std::collections::HashMap;
    let observed: HashMap<&str, &str> = evidence
        .approved_tools
        .iter()
        .map(|tool| (tool.name.as_str(), tool.schema_digest.as_str()))
        .collect();
    let approved: HashMap<&str, &NativeToolV1> = route
        .approved_tools
        .iter()
        .map(|tool| (tool.name.as_str(), tool))
        .collect();
    let check = |declared: &NativeToolV1| -> Result<()> {
        let expected = approved.get(declared.name.as_str()).ok_or_else(|| {
            refused(
                "operation_not_granted",
                "a declared tool is not in the approved loaded set",
            )
        })?;
        if canonical_declaration(declared)? != canonical_declaration(expected)? {
            return Err(refused(
                "operation_not_granted",
                &format!(
                    "a declared tool does not match its approved declaration: {}",
                    declared.name
                ),
            ));
        }
        let schema = declared.schema_digest()?;
        let seen = observed.get(declared.name.as_str()).ok_or_else(|| {
            refused(
                "operation_not_granted",
                "a declared tool was not observed in the loaded registry",
            )
        })?;
        if schema != *seen {
            return Err(refused(
                "operation_not_granted",
                "a declared tool schema does not match the observed registry",
            ));
        }
        Ok(())
    };
    for message in messages {
        for declared in message.declared_tools() {
            check(declared)?;
        }
    }
    let active = dto::active_tools(messages)?;
    for declared in &active {
        check(declared)?;
    }
    Ok(active)
}

fn operation_input_digest(
    binding: &Value,
    configuration: &durable::ConfigurationRow,
    max_output_tokens: u32,
    tool_choice: &str,
) -> Result<String> {
    dto::material_digest(
        "cadence-native-model-input-v1",
        &json!({
            "v": 1,
            "binding": binding,
            "configuration_id": configuration.configuration_id,
            "configuration_revision": configuration.revision,
            "model": configuration.model,
            "model_uid": configuration.model_uid,
            "effort": configuration.effort,
            "provider": configuration.provider,
            "source_id": configuration.source_id,
            "source_revision": configuration.source_revision,
            "account": configuration.account,
            "origin": configuration.origin,
            "sdk_graph_digest": configuration.sdk_graph_digest,
            "snapshot_id": configuration.snapshot_id,
            "loaded_receipt_id": configuration.loaded_receipt_id,
            "max_output_tokens": max_output_tokens,
            "tool_choice": tool_choice,
        }),
    )
}

fn commitment_digest(
    input_digest: &str,
    source: &durable::SourceRow,
    configuration: &durable::ConfigurationRow,
    max_output_tokens: u32,
    tool_choice: &str,
    evidence: &NativeModelRuntimeEvidence,
) -> Result<String> {
    dto::material_digest(
        "cadence-native-model-commitment-v1",
        &json!({
            "v": 1,
            "input_digest": input_digest,
            "source_id": source.source_id,
            "source_revision": source.revision,
            "account": source.account,
            "origin": configuration.origin,
            "destination": configuration.destination,
            "model_uid": configuration.model_uid,
            "model": configuration.model,
            "effort": configuration.effort,
            "max_output_tokens": max_output_tokens,
            "tool_choice": tool_choice,
            "sdk_graph_digest": evidence.sdk_graph_digest,
            "configuration_id": configuration.configuration_id,
            "configuration_revision": configuration.revision,
            "snapshot_id": configuration.snapshot_id,
            "loaded_receipt_id": configuration.loaded_receipt_id,
            "runtime_generation": evidence.runtime_generation,
            "owner_epoch": evidence.owner_epoch,
        }),
    )
}

fn runtime_binding_digest(
    evidence: &NativeModelRuntimeEvidence,
    snapshot_id: &str,
    loaded_receipt_id: &str,
) -> String {
    cadence_core::connection_dispatch::material_digest_for_domain(
        "cadence-native-model-runtime-binding-v1",
        &json!({
            "v": 1,
            "workspace_id": evidence.workspace_id,
            "agent_uid": evidence.agent_uid,
            "session_binding": evidence.session_binding,
            "runtime_id": evidence.runtime_id,
            "runtime_generation": evidence.runtime_generation,
            "owner_epoch": evidence.owner_epoch,
            "os_uid": evidence.os_uid,
            "node_pid": evidence.node_pid,
            "node_starttime": evidence.node_starttime,
            "container_id": evidence.container_id,
            "image_digest": evidence.image_digest,
            "profile_digest": evidence.profile_digest,
            "manifest_digest": evidence.manifest_digest,
            "sdk_graph_digest": evidence.sdk_graph_digest,
            "snapshot_id": snapshot_id,
            "loaded_receipt_id": loaded_receipt_id,
        }),
    )
}

fn material_digest_for_runtime(source: &durable::SourceRow, workspace_id: &str) -> Result<String> {
    Ok(
        cadence_core::connection_dispatch::material_digest_for_domain(
            "cadence-native-model-source-v1",
            &json!({
                "v": 1,
                "workspace_id": workspace_id,
                "source_id": source.source_id,
                "provider": source.provider,
                "account": source.account,
                "revision": source.revision,
                "origin": source.origin,
                "origin_kind": source.origin_kind,
                "catalog_revision": source.catalog_revision,
                "model_uid_map": source.model_uid_map,
                "state": source.state,
            }),
        ),
    )
}

/// Reject a completed assistant that cannot be projected: wrong identity,
/// unauthorized tool calls, invalid arguments, or a truncated tool turn.
fn authorize_result(
    assistant: &NativeAssistantV1,
    messages: &[NativeMessageV1],
    evidence: &NativeModelRuntimeEvidence,
    configuration: &durable::ConfigurationRow,
    route: &catalog::RouteFacts,
    parsed: &InvokeArgsV1,
) -> Result<()> {
    if assistant.api != PROVIDER
        || assistant.provider != PROVIDER
        || assistant.model != configuration.model
    {
        return Err(refused(
            "result_refused",
            "the model result identity is not the configured route",
        ));
    }
    // The configuration itself was frozen from this route; re-resolving here
    // only proves the executable route still matches it.
    let resolved = route.resolve(
        &configuration.model,
        Some(configuration.effort.as_str()),
        Some(configuration.origin.as_str()),
    )?;
    if resolved.model_uid != configuration.model_uid || resolved.effort != configuration.effort {
        return Err(refused(
            "route_unavailable",
            "the model route changed under the frozen configuration",
        ));
    }
    let active = authorize_tools(messages, evidence, route)?;
    let active_names: HashSet<&str> = active.iter().map(|tool| tool.name.as_str()).collect();
    let mut tool_calls = 0usize;
    for call in assistant.tool_calls() {
        tool_calls += 1;
        if call.arguments_error.is_some() {
            return Err(refused(
                "result_refused",
                "the model returned a malformed tool call",
            ));
        }
        if !active_names.contains(call.name.as_str()) {
            return Err(refused(
                "operation_not_granted",
                "the model requested a tool that is not active and approved",
            ));
        }
        let declared = active
            .iter()
            .find(|tool| tool.name == call.name)
            .ok_or_else(|| {
                refused(
                    "operation_not_granted",
                    "the model requested an unknown tool",
                )
            })?;
        dto::validate_integer_json(&call.arguments, 0)?;
        let _ = declared;
    }
    if assistant.stop_reason == "length" && tool_calls > 0 {
        return Err(refused(
            "result_refused",
            "a length-truncated result cannot execute tools",
        ));
    }
    if assistant.stop_reason == "toolUse" && tool_calls == 0 {
        return Err(refused(
            "result_refused",
            "the model reported tool use without a tool call",
        ));
    }
    if parsed.tool_choice == dto::ToolChoiceV1::None && tool_calls > 0 {
        return Err(refused(
            "result_refused",
            "the model returned a tool call when tools were disabled",
        ));
    }
    Ok(())
}

fn model_uid_map(route: &catalog::RouteFacts) -> Result<String> {
    let mut families = Vec::with_capacity(route.families.len());
    for family in &route.families {
        let mut levels = serde_json::Map::new();
        for (level, uid) in &family.thinking_level_map {
            levels.insert(level.clone(), json!(uid));
        }
        families.push(json!({
            "model_id": family.model_id,
            "family_uid": family.family_uid,
            "label": family.family_label,
            "reasoning": family.reasoning,
            "levels": Value::Object(levels),
            "variants": family.variants.iter().map(|variant| json!({
                "model_uid": variant.model_uid,
                "max_context_tokens": variant.max_context_tokens,
                "max_output_tokens": variant.max_output_tokens,
            })).collect::<Vec<_>>(),
        }));
    }
    serde_json::to_string(&cadence_core::connection_dispatch::canonical_value(
        &json!({
            "v": 1,
            "families": families,
        }),
    ))
    .map_err(|_| Error::internal("the model uid map could not be encoded"))
}

/// `~/.local/share/devin/credentials.toml`-shaped content, submitted by the
/// human on the isolated form. Only the provider key and its server URL are
/// read; nothing is executed and no path/environment is consulted.
fn parse_devin_credentials(text: &str) -> Result<(Vec<u8>, Option<String>)> {
    let mut key: Option<String> = None;
    let mut origin: Option<String> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('[') {
            continue;
        }
        let Some((name, value)) = line.split_once('=') else {
            return Err(refused(
                "unsupported_content",
                "the credential file is not a supported Devin credentials file",
            ));
        };
        let name = name.trim();
        let value = value.trim();
        if !value.starts_with('"') || !value.ends_with('"') || value.len() < 2 {
            continue;
        }
        let value = &value[1..value.len() - 1];
        if value.contains('"')
            || value.contains('\\')
            || value.bytes().any(|b| b.is_ascii_control())
        {
            return Err(refused(
                "unsupported_content",
                "the credential file is not a supported Devin credentials file",
            ));
        }
        match name {
            "windsurf_api_key" | "api_key" => {
                if key.is_none() && !value.is_empty() && value.len() <= 8192 {
                    key = Some(value.to_string());
                }
            }
            "api_server_url" if origin.is_none() && !value.is_empty() && value.len() <= 200 => {
                origin = Some(value.to_string());
            }
            _ => {}
        }
    }
    let key = key.ok_or_else(|| {
        refused(
            "unsupported_content",
            "the credential file carries no Devin API key",
        )
    })?;
    if key.bytes().any(|b| b == b' ' || b == b'\t') {
        return Err(refused(
            "invalid_request",
            "the native model key is unusable",
        ));
    }
    Ok((key.into_bytes(), origin))
}

fn receipt(row: &durable::RequestRow) -> Result<Value> {
    let state = match row.state.as_str() {
        durable::STATE_PREPARED => "prepared",
        durable::STATE_COMMITTED => "committed",
        durable::STATE_DONE => "done",
        durable::STATE_REFUSED => "refused",
        durable::STATE_UNKNOWN => "unknown",
        durable::STATE_RESULT_REFUSED => "done",
        _ => "unknown",
    };
    let assistant = if row.state == durable::STATE_DONE {
        match &row.assistant_json {
            Some(json_text) if json_text.len() <= MAX_ARTIFACT_BYTES => {
                let value: Value = serde_json::from_str(json_text)
                    .map_err(|_| Error::internal("the model result is unreadable"))?;
                // Re-validate on every read: a stored row is not authority.
                NativeAssistantV1::parse(&value)?.to_json()
            }
            _ => Value::Null,
        }
    } else {
        Value::Null
    };
    let usage = match &row.usage_json {
        Some(json_text) if json_text.len() <= MAX_ARTIFACT_BYTES => serde_json::from_str(json_text)
            .map_err(|_| Error::internal("the model usage is unreadable"))?,
        _ => Value::Null,
    };
    let error = match &row.error_json {
        Some(json_text) if json_text.len() <= MAX_ARTIFACT_BYTES => serde_json::from_str(json_text)
            .map_err(|_| Error::internal("the model error is unreadable"))?,
        _ => Value::Null,
    };
    let billing: Value = serde_json::from_str(&row.billing_json)
        .map_err(|_| Error::internal("the model billing projection is unreadable"))?;
    let value = json!({
        "v": 1,
        "request_id": row.request_id,
        "input_digest": row.input_digest,
        "state": state,
        "assistant": assistant,
        "usage": usage,
        "error": error,
        "billing": billing,
    });
    let bytes = serde_json::to_vec(&value)
        .map_err(|_| Error::internal("the model receipt could not be encoded"))?;
    if bytes.len() > dto::MAX_RECEIPT_BYTES {
        return Err(refused(
            "payload_too_large",
            "the model receipt exceeds its bound",
        ));
    }
    Ok(value)
}

// ---------------------------------------------------------------------------
// Shared accessors
// ---------------------------------------------------------------------------

impl Shared {
    /// The installed native model broker, if this daemon runs the protected
    /// native model route. Absent is a truthful refusal: no unprotected or
    /// substitute backend answers in its place.
    pub(crate) fn native_model(&self) -> Result<&Arc<NativeModelBroker>> {
        self.native_model.get().ok_or_else(|| {
            refused(
                "runtime_unqualified",
                "the native model broker is not installed on this daemon",
            )
        })
    }
}
