//! CAD-1085: the native protected-Connection backend.
//!
//! This module is where the trusted native adapter meets the shared planner
//! and the store authority:
//!
//! 1. the live root-owner recheck ([`ProtectedOwnerClient`]) supplies the
//!    current epoch/runtime/loaded-registry evidence — never a boolean, a
//!    caller field or a persisted row;
//! 2. [`Store::protected_commit`] re-reads every authorizing row inside one
//!    writer transaction, plans with the shared pure rule, appends the
//!    reservation to the real protected ledger, CAS-consumes the approval and
//!    inserts the immutable dispatch row;
//! 3. the complete ledger prefix is checkpointed to the external watermark
//!    and only its positive acknowledgement mints the private, non-cloneable,
//!    consuming [`ExecutionPermit`];
//! 4. the permit executes the one typed provider call with NO store lock held;
//! 5. a separate short CAS transaction settles the exact validated outcome.
//!
//! Nothing durable is an execution permit: a recovered `connection_dispatches`
//! row, a replayed invoke or an ambiguous acknowledgement can only produce a
//! status read, never another provider call.
//!
//! The bounded text operation here is the typed `text.generate@1` Connection
//! operation (fixed origin, non-streaming, no redirect/cookies/proxy/retry).
//! It is deliberately separate from the Pi model-turn/guest-bridge transport,
//! whose contract is still pending independent review.

use std::sync::{Arc, Weak};

use cadence_core::connection_dispatch::canonical::{exact_byte_digest, material_digest_value};
use cadence_core::connection_dispatch::dto::{
    AdmissionRequestV1, AgentOperationV1, ConnectionInvokeV1, CredentialRequestArgsV1, CurrencyV1,
    DispatchCommitV1, DispatchReceiptV1, DispatchStateV1, FallbackPolicyV1, FenceStateV1,
    HumanSmtpVerifyAdmissionV1, HumanVerbV1, LoadedReceiptV1, ModelQuoteV1, OperationKindV1,
    OutcomeKindV1, OutcomeResultV1, ParameterPolicyV1, QuoteArgsV1, QuotePriceV1,
    ReasoningBillingV1, ReceiptResultV1, ResolvedDestinationV1, ResolvedMaterialV1, ResultArgsV1,
    RuntimeProfileV1, SendApprovalV1, SettlementV1, SmtpAcceptedKindV1, SmtpAcceptedOutcomeV1,
    SmtpVerifiedKindV1, SmtpVerifiedOutcomeV1, SnapshotArgsV1, SnapshotV1, StageSendArgsV1,
    TextGenerateV1, TextGeneratedKindV1, TextOutcomeV1, UsageV1, ValidatedOutcomeV1,
    SNAPSHOT_LIFETIME_MS,
};
use cadence_core::connection_dispatch::primitives::{
    parse_usd_decimal_to_pico, AgentUid, Counter, Digest, EpochMs, HttpsUrl, Ident, PicoUsd,
    Revision, UuidV1,
};
use cadence_core::connection_dispatch::{
    verify_frozen_dispatch_v1, ErrorCodeV1, GrantStateV1, NativeBindingKindV1,
    NativeRuntimeBindingV1, NativeSessionV1, PrincipalBindingV1, PrincipalKindV1, RequesterV1,
    RuntimeBindingV1, RuntimeFactsV1, UidGrantV1,
};
use serde_json::{json, Value};

use super::credential_entry_rpc::{
    AgentCredentialIntent, BoundedSecretInputV1, EntryContextV1, EntryTargetV1, SecretKind,
    VerifiedHumanAttemptV1, NOT_READY,
};
use super::Shared;
use crate::error::{Error, Result};
use crate::protected_runtime::ops::{OpsMethod, RuntimeCaller};
use crate::store::connection_dispatch::{
    effect_operation_binding, AdmissionFactsV1, CommitOutcome, IssuedSnapshotV1, LoadedSnapshot,
    SnapshotIssueFactsV1,
};

/// How long one provider call may take once the permit exists.
const EXECUTION_DEADLINE: std::time::Duration = std::time::Duration::from_secs(60);
/// Hard cap on one model response body.
const MODEL_RESPONSE_MAX: u64 = 1024 * 1024 + 8192;
/// The reviewed protected model origin. The exact endpoint path comes from
/// the frozen quote's fixed endpoint set; a caller never selects a transport.
const OPENROUTER_BASE: &str = "https://openrouter.ai";

// ---------------------------------------------------------------------------
// Live owner evidence
// ---------------------------------------------------------------------------

/// The live protected runtime facts only the measured root owner can produce:
/// current owner/custody epoch, the enrolled runtime generation, the loaded
/// registry digest and the fence state. Constructed by the root-owner client
/// from authenticated owner RPCs; never from SQLite alone. It carries NO
/// snapshot/receipt ids or expiry — those are issued by this backend from the
/// real guarded evidence and the approved rows.
#[derive(Clone, Debug)]
pub(crate) struct OwnerEligibility {
    pub workspace_id: String,
    /// `local` for native custody.
    pub custody_domain: String,
    pub custody_epoch: Revision,
    pub owner_epoch: Revision,
    pub runtime_generation: Revision,
    pub enrollment_id: UuidV1,
    pub observed_registry_digest: Digest,
    pub fence: FenceStateV1,
    pub runtime_profile: RuntimeProfileV1,
}

impl OwnerEligibility {
    /// Construct the live evidence from one authenticated owner row set. The
    /// owner supplies no snapshot/receipt ids or expiry: issuance belongs to
    /// this backend, from the approved rows.
    pub(crate) fn from_owner_evidence(
        evidence: &crate::protected_runtime::OwnerEvidence,
    ) -> Result<Self> {
        let unreadable = || Error::invalid("runtime_unqualified", "owner evidence is unusable");
        Ok(Self {
            workspace_id: evidence.workspace_id.clone(),
            custody_domain: "local".to_string(),
            custody_epoch: Revision::new(evidence.custody_epoch).map_err(|_| unreadable())?,
            owner_epoch: Revision::new(evidence.owner_epoch).map_err(|_| unreadable())?,
            runtime_generation: Revision::new(evidence.runtime_generation)
                .map_err(|_| unreadable())?,
            enrollment_id: UuidV1::try_from(evidence.enrollment_id.clone())
                .map_err(|_| unreadable())?,
            observed_registry_digest: Digest::try_from(evidence.observed_registry_digest.clone())
                .map_err(|_| unreadable())?,
            fence: if evidence.fenced {
                FenceStateV1::Revoking
            } else {
                FenceStateV1::Clear
            },
            runtime_profile: RuntimeProfileV1::NativeOciAgent1,
        })
    }
}

/// Boot-time protected management evidence: the profile/owner epoch and the
/// enrolled supervisor generation. It is NOT a runtime readiness claim and
/// carries no agent subject or receipt.
#[derive(Clone, Debug)]
pub(crate) struct ManagementEvidence {
    pub workspace_id: String,
    pub custody_domain: String,
    pub database_epoch: Revision,
    pub owner_epoch: Revision,
    pub profile_digest: Digest,
    pub supervisor_generation: String,
    pub fenced: bool,
}

/// The measured coordinator's runtime and guest-independent management
/// evidence. Restore-watermark transport is installed once on the Store.
/// A transport failure is an `Err`, never an epoch value.
pub(crate) trait ProtectedOwnerClient: Send + Sync {
    /// Current eligibility for one EXACT runtime subject, resolved from the
    /// kernel-verified caller. A daemon with several agents cannot reuse one
    /// agent's evidence for another.
    fn current_eligibility(&self, subject: &RuntimeCaller) -> Result<OwnerEligibility>;
    /// Boot-time profile/owner evidence for initialize/unlock/lock and
    /// manifest/policy/grant management. It needs no agent runtime.
    fn management_evidence(&self) -> Result<ManagementEvidence>;
}

// ---------------------------------------------------------------------------
// The backend
// ---------------------------------------------------------------------------

/// The native protected-Connection backend. Held once per daemon through
/// `Shared::protected` (`OnceLock<Arc<ProtectedConnections>>`); it keeps only a
/// `Weak` back-reference, so there is no `Arc` cycle and no duplicate
/// authority cache. All authority still lives in the store and the root owner.
pub(crate) struct ProtectedConnections {
    shared: Weak<Shared>,
    owner: Option<Arc<dyn ProtectedOwnerClient>>,
    /// Per-process quote activation. A restart mints a new value, so a quote
    /// observed before the restart can never authorize inference afterwards.
    process_activation: String,
}

impl ProtectedConnections {
    pub(crate) fn new(shared: &Arc<Shared>, owner: Option<Arc<dyn ProtectedOwnerClient>>) -> Self {
        Self {
            shared: Arc::downgrade(shared),
            owner,
            process_activation: uuid::Uuid::new_v4().simple().to_string(),
        }
    }

    fn shared(&self) -> Result<Arc<Shared>> {
        self.shared
            .upgrade()
            .ok_or_else(|| Error::invalid("outcome_unknown", "the daemon is shutting down"))
    }

    /// The live owner recheck for one EXACT runtime subject plus the durable
    /// authority gate. No store lock is held across the owner RPC: the store
    /// read acquires and releases the writer mutex on its own.
    pub(crate) fn eligibility_for(&self, caller: &RuntimeCaller) -> Result<OwnerEligibility> {
        let owner = self.owner.as_ref().ok_or_else(|| {
            Error::invalid(
                "runtime_unqualified",
                "no qualified protected runtime is current on this daemon",
            )
        })?;
        let eligibility = owner.current_eligibility(caller)?;
        self.check_eligibility(eligibility)
    }

    /// Boot-time management evidence, independent of any agent runtime.
    pub(crate) fn management_evidence(&self) -> Result<ManagementEvidence> {
        let owner = self.owner.as_ref().ok_or_else(|| {
            Error::invalid(
                "runtime_unqualified",
                "no qualified protected runtime is current on this daemon",
            )
        })?;
        let evidence = owner.management_evidence()?;
        if evidence.custody_domain != "local" || evidence.fenced {
            return Err(Error::invalid(
                "runtime_revoked",
                "the protected profile is fenced or revoked",
            ));
        }
        let shared = self.shared()?;
        let authority = shared.store.protected_authority()?;
        if authority.workspace_id != evidence.workspace_id
            || authority.database_epoch != evidence.database_epoch
        {
            return Err(Error::invalid(
                "placement_changed",
                "the protected workspace or database epoch changed",
            ));
        }
        Ok(evidence)
    }

    /// The current immutable material held for one stable agent, without a
    /// live socket caller. Used only to revalidate an already-issued bundle;
    /// it refuses rather than issuing without live owner evidence.
    pub(crate) fn current_material_for_agent(
        &self,
        agent_uid: &str,
    ) -> Result<(SnapshotV1, LoadedReceiptV1)> {
        AgentUid::try_from(agent_uid.to_string()).map_err(|_| {
            Error::invalid("runtime_unqualified", "the agent identity is not current")
        })?;
        let shared = self.shared()?;
        let now = now_epoch_ms()?;
        let loaded = shared
            .store
            .protected_current_snapshot_for_agent(agent_uid, now.get())?
            .ok_or_else(|| {
                Error::invalid(
                    "runtime_unqualified",
                    "no current issued material is held for this agent",
                )
            })?;
        let authority = shared.store.protected_authority()?;
        if !authority.enabled || authority.fenced {
            return Err(Error::invalid(
                "runtime_unqualified",
                "protected Connections are not enabled on this store",
            ));
        }
        if loaded.authorization_revision != authority.authorization_revision {
            return Err(Error::invalid(
                "stale_revision",
                "the protected authorization changed",
            ));
        }
        let manifest_current = match shared
            .store
            .protected_manifest(loaded.snapshot.manifest_id.as_str())?
        {
            Some(manifest) => {
                manifest.material_digest == loaded.snapshot.manifest_digest
                    && manifest.revision == loaded.snapshot.manifest_revision
            }
            None => false,
        };
        if !manifest_current {
            return Err(Error::invalid(
                "loaded_state_mismatch",
                "the approved manifest changed",
            ));
        }
        Ok((loaded.snapshot, loaded.receipt))
    }

    fn check_eligibility(&self, eligibility: OwnerEligibility) -> Result<OwnerEligibility> {
        if eligibility.custody_domain != "local" || eligibility.fence != FenceStateV1::Clear {
            return Err(Error::invalid(
                "runtime_revoked",
                "the protected runtime is fenced or revoked",
            ));
        }
        let shared = self.shared()?;
        let authority = shared.store.protected_authority()?;
        if !authority.enabled || authority.fenced || !authority.ever_enabled {
            return Err(Error::invalid(
                "runtime_unqualified",
                "protected Connections are not enabled on this store",
            ));
        }
        if authority.workspace_id != eligibility.workspace_id
            || authority.database_epoch != eligibility.custody_epoch
        {
            return Err(Error::invalid(
                "placement_changed",
                "the protected workspace or database epoch changed",
            ));
        }
        Ok(eligibility)
    }

    /// The exact evidence window one issued snapshot/receipt pair supplies.
    fn loaded_runtime_facts(
        eligibility: &OwnerEligibility,
        loaded: &LoadedSnapshot,
    ) -> RuntimeFactsV1 {
        RuntimeFactsV1 {
            profile: eligibility.runtime_profile,
            runtime_generation: loaded.receipt.runtime_generation,
            enrollment_id: loaded.receipt.enrollment_id.clone(),
            loaded_receipt_id: loaded.receipt.loaded_receipt_id.clone(),
            observed_registry_digest: loaded.receipt.observed_registry_digest.clone(),
            owner_epoch: eligibility.owner_epoch,
            custody_epoch: None,
            fence: eligibility.fence,
            snapshot_issued_ms: loaded.snapshot.issued_at_ms,
            snapshot_expires_ms: loaded.snapshot.expires_at_ms,
            receipt_issued_ms: loaded.receipt.issued_at_ms,
            receipt_expires_ms: loaded.receipt.expires_at_ms,
        }
    }

    /// The evidence window of the human `smtp.verify@1` action. There is no
    /// agent runtime subject: the human entry session is the actual native
    /// session (generation 1), the observed digest is the measured profile
    /// digest from the authenticated management evidence, and the owner epoch
    /// is the live authenticated owner epoch. No guest receipt, guest
    /// generation or synthesized time is borrowed.
    fn human_runtime_facts(
        evidence: &ManagementEvidence,
        now_ms: u64,
        enrollment_id: UuidV1,
        session_id: UuidV1,
    ) -> Result<RuntimeFactsV1> {
        let expires = now_ms
            .checked_add(SNAPSHOT_LIFETIME_MS)
            .ok_or_else(|| Error::invalid("clock_regression", "the clock is unusable"))?;
        Ok(RuntimeFactsV1 {
            profile: RuntimeProfileV1::NativeOciAgent1,
            runtime_generation: Self::human_session_generation()?,
            enrollment_id,
            loaded_receipt_id: session_id,
            observed_registry_digest: evidence.profile_digest.clone(),
            owner_epoch: evidence.owner_epoch,
            custody_epoch: None,
            fence: FenceStateV1::Clear,
            snapshot_issued_ms: EpochMs::new(now_ms)
                .map_err(|_| Error::invalid("clock_regression", "the clock is unusable"))?,
            snapshot_expires_ms: EpochMs::new(expires)
                .map_err(|_| Error::invalid("clock_regression", "the clock is unusable"))?,
            receipt_issued_ms: EpochMs::new(now_ms)
                .map_err(|_| Error::invalid("clock_regression", "the clock is unusable"))?,
            receipt_expires_ms: EpochMs::new(expires)
                .map_err(|_| Error::invalid("clock_regression", "the clock is unusable"))?,
        })
    }

    /// A human entry session is created fresh and is generation 1. It is an
    /// actual native-session fact, not a borrowed guest generation.
    fn human_session_generation() -> Result<Revision> {
        Revision::new(1).map_err(|_| Error::invalid("request_changed", "the session changed"))
    }

    // -- entry context -----------------------------------------------------

    /// The live protected context a human credential request binds. Derived
    /// from the authenticated profile/supervisor/core management evidence and
    /// the durable workspace, never from a live agent runtime or a persisted
    /// readiness flag. This is why the FIRST credential can be enrolled on a
    /// daemon with zero agents/guests; metadata reads stay available while
    /// custody is locked because this only needs the owner evidence and the
    /// store row.
    pub(crate) fn entry_context(&self, target: &EntryTargetV1) -> Result<EntryContextV1> {
        let evidence = self.management_evidence().map_err(|error| {
            if error.code().is_some() {
                error
            } else {
                Error::invalid("request_not_ready", NOT_READY)
            }
        })?;
        let shared = self.shared()?;
        let workspace_id = shared.store.connection_workspace_id()?;
        if workspace_id != evidence.workspace_id {
            return Err(Error::invalid("request_not_ready", NOT_READY));
        }
        let target_agent_uid = match target.agent_alias.as_deref() {
            Some(alias) => Some(
                shared
                    .store
                    .agent_uid_for_alias(alias)?
                    .ok_or_else(|| Error::invalid("request_not_ready", NOT_READY))?,
            ),
            None => target.participant_uid.clone(),
        };
        let not_ready = || Error::invalid("request_not_ready", NOT_READY);
        Ok(EntryContextV1 {
            workspace_id,
            custody_domain: evidence.custody_domain,
            custody_epoch: i64::try_from(evidence.database_epoch.get()).map_err(|_| not_ready())?,
            owner_epoch: i64::try_from(evidence.owner_epoch.get()).map_err(|_| not_ready())?,
            supervisor_generation: evidence.supervisor_generation,
            // No agent registry is loaded for the human management profile.
            loaded_registry_digest: None,
            target_agent_uid,
        })
    }

    // -- runtime operations ------------------------------------------------

    /// The single entry the per-runtime router calls. `caller` is server-derived
    /// from the kernel peer; no field of `args` can name an actor, runtime,
    /// workspace, revision or management authority.
    pub(crate) fn handle_runtime(
        &self,
        caller: &RuntimeCaller,
        method: OpsMethod,
        args: &Value,
    ) -> Result<Value> {
        match method {
            OpsMethod::ConnectionSnapshotV1 => self.op_snapshot(caller, args),
            OpsMethod::ConnectionModelQuoteV1 => self.op_quote(caller, args),
            OpsMethod::ConnectionStageSendV1 => self.op_stage_send(caller, args),
            OpsMethod::ConnectionInvokeV1 => self.op_invoke(caller, args),
            OpsMethod::ConnectionResultV1 => self.op_result(caller, args),
            OpsMethod::ConnectionRequestV1 => self.op_request(caller, args),
            _ => Err(Error::rejected(
                "the protected backend does not serve this operation",
            )),
        }
    }

    /// The CURRENT immutable snapshot/receipt for one exact runtime subject.
    /// Reuses a still-current issued pair, otherwise issues from the single
    /// approved manifest reachable by that agent. Never accepts a caller id.
    pub(crate) fn current_material(
        &self,
        caller: &RuntimeCaller,
    ) -> Result<(SnapshotV1, LoadedReceiptV1)> {
        let eligibility = self.eligibility_for(caller)?;
        let shared = self.shared()?;
        let now = now_epoch_ms()?;
        if let Some(loaded) = shared.store.protected_current_snapshot(
            caller.agent_uid(),
            eligibility.runtime_generation.get(),
            now.get(),
        )? {
            let authority = shared.store.protected_authority()?;
            let manifest_current = match shared
                .store
                .protected_manifest(loaded.snapshot.manifest_id.as_str())?
            {
                Some(manifest) => {
                    manifest.material_digest == loaded.snapshot.manifest_digest
                        && manifest.revision == loaded.snapshot.manifest_revision
                }
                None => false,
            };
            if loaded.authorization_revision == authority.authorization_revision && manifest_current
            {
                return Ok((loaded.snapshot, loaded.receipt));
            }
        }
        let manifest_id = shared
            .store
            .protected_single_approved_manifest()?
            .ok_or_else(|| {
                Error::invalid("capability_unavailable", "the capability is not available")
            })?;
        let issued = self.issue_material(caller, &eligibility, manifest_id)?;
        Ok((issued.snapshot, issued.receipt))
    }

    fn issue_material(
        &self,
        caller: &RuntimeCaller,
        eligibility: &OwnerEligibility,
        manifest_id: Ident,
    ) -> Result<IssuedSnapshotV1> {
        let shared = self.shared()?;
        shared.store.protected_issue_snapshot(SnapshotIssueFactsV1 {
            snapshot_id: random_uuid()?,
            loaded_receipt_id: random_uuid()?,
            now_ms: now_epoch_ms()?,
            agent_uid: agent_uid_of(caller)?,
            session_binding_id: random_uuid()?,
            runtime_generation: eligibility.runtime_generation,
            enrollment_id: eligibility.enrollment_id.clone(),
            observed_registry_digest: eligibility.observed_registry_digest.clone(),
            manifest_id,
        })
    }

    /// Load EXACTLY the issued selector pair the caller named and require it to
    /// describe this live subject and the current authorization revision.
    fn load_for_operation(
        &self,
        caller: &RuntimeCaller,
        eligibility: &OwnerEligibility,
        snapshot_id: &UuidV1,
        receipt_id: &UuidV1,
    ) -> Result<LoadedSnapshot> {
        if eligibility.runtime_generation.get() != caller.runtime_generation() {
            return Err(Error::invalid(
                "placement_changed",
                "the loaded runtime changed since the snapshot was issued",
            ));
        }
        let shared = self.shared()?;
        let loaded = shared
            .store
            .protected_load_snapshot(snapshot_id.as_str(), receipt_id.as_str())?
            .ok_or_else(|| {
                Error::invalid(
                    "loaded_state_mismatch",
                    "the named snapshot or loaded receipt was not issued",
                )
            })?;
        if loaded.snapshot.requester.agent_uid.as_str() != caller.agent_uid()
            || loaded.snapshot.requester.runtime_generation != eligibility.runtime_generation
            || loaded.receipt.runtime_generation != eligibility.runtime_generation
            || loaded.receipt.enrollment_id != eligibility.enrollment_id
            || loaded.receipt.observed_registry_digest != eligibility.observed_registry_digest
        {
            return Err(Error::invalid(
                "loaded_state_mismatch",
                "the loaded runtime does not match the issued receipt",
            ));
        }
        if now_epoch_ms()?.get() >= loaded.snapshot.expires_at_ms.get() {
            return Err(Error::invalid(
                "session_expired",
                "the issued snapshot has expired",
            ));
        }
        if loaded.authorization_revision
            != shared.store.protected_authority()?.authorization_revision
        {
            return Err(Error::invalid(
                "stale_revision",
                "the protected authorization changed after the snapshot was issued",
            ));
        }
        Ok(loaded)
    }

    fn agent_principal(
        &self,
        caller: &RuntimeCaller,
        eligibility: &OwnerEligibility,
        loaded: &LoadedSnapshot,
        key_id: &Ident,
    ) -> Result<DispatchPrincipal> {
        let shared = self.shared()?;
        let authority = shared.store.protected_authority()?;
        let uid = agent_uid_of(caller)?;
        let session_id = ident(caller.session_binding())?;
        let requester = RequesterV1 {
            agent_uid: uid.clone(),
            session_binding_id: loaded.snapshot.requester.session_binding_id.clone(),
            runtime_generation: loaded.snapshot.requester.runtime_generation,
        };
        let principal = PrincipalBindingV1 {
            custody_domain: ident("local")?,
            workspace_id: ident(&authority.workspace_id)?,
            kind: PrincipalKindV1::Agent,
            principal_id: session_id.clone(),
            agent_uid: uid,
            alias: None,
            native_session: Some(NativeSessionV1 {
                session_id,
                generation: eligibility.runtime_generation,
            }),
            session_authority_ref: None,
            session_generation: Some(eligibility.runtime_generation),
            conversation_generation: None,
            enrollment_generation: Revision::new(1).map_err(|_| refused())?,
            binding: RuntimeBindingV1::Native(NativeRuntimeBindingV1 {
                kind: NativeBindingKindV1::Native,
                launch_digest: digest(
                    "cadence-protected-agent-launch-v1",
                    &json!({
                        "workspace_id": authority.workspace_id,
                        "runtime_id": caller.runtime_id(),
                        "runtime_generation": eligibility.runtime_generation.get(),
                        "enrollment_id": eligibility.enrollment_id.as_str(),
                    }),
                )?,
                continuity_digest: digest(
                    "cadence-protected-agent-continuity-v1",
                    &json!({
                        "session": caller.session_binding(),
                        "loaded_receipt_id": loaded.receipt.loaded_receipt_id.as_str(),
                        "runtime_generation": loaded.receipt.runtime_generation.get(),
                    }),
                )?,
                agent_generation: eligibility.runtime_generation,
                owner_generation: eligibility.owner_epoch,
            }),
        };
        Ok(DispatchPrincipal::new(
            caller.clone(),
            requester,
            principal,
            Self::loaded_runtime_facts(eligibility, loaded),
            key_id.clone(),
            loaded.snapshot.manifest_id.clone(),
        ))
    }

    fn op_snapshot(&self, caller: &RuntimeCaller, args: &Value) -> Result<Value> {
        let args: SnapshotArgsV1 = parse_dto(args)?;
        if args.v != 1 {
            return Err(invalid_request());
        }
        let eligibility = self.eligibility_for(caller)?;
        let issued = self.issue_material(caller, &eligibility, args.manifest_id)?;
        Ok(json!({
            "snapshot": issued.snapshot,
            "loaded_receipt": issued.receipt,
        }))
    }

    fn op_quote(&self, caller: &RuntimeCaller, args: &Value) -> Result<Value> {
        let args: QuoteArgsV1 = parse_dto(args)?;
        if args.v != 1 {
            return Err(invalid_request());
        }
        let eligibility = self.eligibility_for(caller)?;
        let loaded = self.load_for_operation(
            caller,
            &eligibility,
            &args.snapshot_id,
            &args.loaded_receipt_id,
        )?;
        let shared = self.shared()?;
        let authority = shared.store.protected_authority()?;
        let connection = shared
            .store
            .protected_connection_row(args.connection_id.as_str())?
            .ok_or_else(|| Error::invalid("not_found", "the selector is unavailable"))?;
        if connection.platform != OPENROUTER_PROVIDER {
            return Err(Error::invalid(
                "operation_not_granted",
                "the Connection does not grant this operation",
            ));
        }
        let binding = loaded
            .snapshot
            .connection_bindings
            .iter()
            .find(|binding| binding.connection_id == args.connection_id)
            .ok_or_else(|| {
                Error::invalid(
                    "loaded_state_mismatch",
                    "the Connection is not bound to the issued snapshot",
                )
            })?;
        if binding.credential_revision != args.expected_credential_revision
            || binding.authorization_revision != args.expected_authorization_revision
            || binding.credential_revision.get() as i64 != connection.credential_revision
        {
            return Err(Error::invalid(
                "stale_revision",
                "the credential or authorization revision changed",
            ));
        }
        let policy_row = shared
            .store
            .protected_policy_row(args.connection_id.as_str())?
            .ok_or_else(|| {
                Error::invalid(
                    "operation_not_granted",
                    "the Connection has no reviewed protected policy",
                )
            })?;
        if policy_row.state != "active"
            || !policy_row.policy.active
            || policy_row.credential_revision != connection.credential_revision
            || !policy_row
                .policy
                .allowed_operations
                .contains(&OperationKindV1::TextGenerate)
        {
            return Err(Error::invalid(
                "operation_not_granted",
                "the Connection does not grant this operation",
            ));
        }
        if !policy_row.policy.allowed_models.contains(&args.model) {
            return Err(Error::invalid(
                "operation_not_granted",
                "the Connection does not grant this model",
            ));
        }
        let capability = loaded
            .snapshot
            .capabilities
            .iter()
            .find(|capability| capability.capability_id == args.capability_id)
            .ok_or_else(|| {
                Error::invalid("capability_unavailable", "the capability is not available")
            })?;
        if capability.operation != OperationKindV1::TextGenerate
            || capability.connection_scope != args.connection_id
        {
            return Err(Error::invalid(
                "capability_unavailable",
                "the capability is not available",
            ));
        }
        let grants = shared.store.protected_grants(
            caller.agent_uid(),
            &connection.platform,
            &connection.account,
        )?;
        let grant = exactly_one_grant(&grants, caller.agent_uid())?;
        let role_policy = shared.store.protected_role_model_policy()?;
        cadence_core::connection_dispatch::pi_model_decision(
            grant.role,
            &args.model,
            role_policy.as_ref(),
        )
        .map_err(|_| {
            Error::invalid(
                "operation_not_granted",
                "the model is not allowed for this agent role",
            )
        })?;
        let scope = policy_row
            .policy
            .data_scope(&args.data_scope_id)
            .ok_or_else(|| {
                Error::invalid("data_policy_refused", "the data scope is not approved")
            })?;
        if scope.source == cadence_core::connection_dispatch::dto::DataScopeSourceV1::CallerText
            && !scope.allow_caller_text
        {
            return Err(Error::invalid(
                "data_policy_refused",
                "the data scope is not approved",
            ));
        }
        // The metadata read has its OWN finite allowance, checked before any
        // network I/O.
        let now = now_epoch_ms()?.get();
        let (principal_reads, workspace_reads) = shared
            .store
            .protected_quote_counts(caller.session_binding(), now)?;
        if principal_reads >= QUOTE_READS_PER_PRINCIPAL
            || workspace_reads >= QUOTE_READS_PER_WORKSPACE
        {
            return Err(Error::invalid(
                "rate_limit",
                "the metadata read rate limit is reached",
            ));
        }
        let metadata = fetch_openrouter_metadata(args.model.as_str())?;
        let observed = std::cmp::max(now, authority.clock_floor_ms.get());
        let expires = observed
            .checked_add(SNAPSHOT_LIFETIME_MS)
            .ok_or_else(|| Error::invalid("clock_regression", "the clock is unusable"))?;
        let quote = ModelQuoteV1 {
            v: 1,
            quote_id: random_uuid()?,
            connection_id: args.connection_id.clone(),
            credential_revision: binding.credential_revision,
            authorization_revision: binding.authorization_revision,
            policy_revision: Revision::new(policy_row.policy_revision as u64)
                .map_err(|_| refused())?,
            provider_profile: policy_row.policy.provider_profile.clone(),
            routing_profile: ident(OPENROUTER_ROUTING)?,
            model: args.model.clone(),
            endpoints: vec![
                HttpsUrl::try_from(OPENROUTER_ENDPOINT.to_string()).map_err(|_| refused())?
            ],
            fallback: FallbackPolicyV1::Disallowed,
            parameters: ParameterPolicyV1::Required,
            service_tier: ident("default")?,
            metadata_digest: Digest::try_from(exact_byte_digest(&metadata.entry_bytes))
                .map_err(|_| refused())?,
            metadata_observed_ms: EpochMs::new(observed)
                .map_err(|_| Error::invalid("clock_regression", "the clock is unusable"))?,
            metadata_expires_ms: EpochMs::new(expires)
                .map_err(|_| Error::invalid("clock_regression", "the clock is unusable"))?,
            context_bound: Counter::new(metadata.context_bound).map_err(|_| {
                Error::invalid("pricing_unavailable", "the model bound is unusable")
            })?,
            max_output_bound: Counter::new(MODEL_MAX_OUTPUT as u64).map_err(|_| refused())?,
            price: metadata.price,
            privacy_policy: ident("provider-published")?,
            max_cost_micros: args.max_cost_micros,
        };
        cadence_core::connection_dispatch::dto::validate_model_quote_v1(&quote)
            .map_err(|_| Error::invalid("pricing_unavailable", "the quote is unusable"))?;
        shared.store.protected_put_quote(
            quote.clone(),
            caller.session_binding().to_string(),
            self.process_activation.clone(),
        )?;
        Ok(json!({"quote": quote}))
    }

    fn op_stage_send(&self, caller: &RuntimeCaller, args: &Value) -> Result<Value> {
        let args: StageSendArgsV1 = parse_dto(args)?;
        if args.v != 1 {
            return Err(invalid_request());
        }
        let eligibility = self.eligibility_for(caller)?;
        let loaded = self.load_for_operation(
            caller,
            &eligibility,
            &args.snapshot_id,
            &args.loaded_receipt_id,
        )?;
        let shared = self.shared()?;
        let authority = shared.store.protected_authority()?;
        let connection = shared
            .store
            .protected_connection_row(args.connection_id.as_str())?
            .ok_or_else(|| Error::invalid("not_found", "the selector is unavailable"))?;
        if connection.platform != SMTP_PROVIDER {
            return Err(Error::invalid(
                "operation_not_granted",
                "the Connection does not grant this operation",
            ));
        }
        let binding = loaded
            .snapshot
            .connection_bindings
            .iter()
            .find(|binding| binding.connection_id == args.connection_id)
            .ok_or_else(|| {
                Error::invalid(
                    "loaded_state_mismatch",
                    "the Connection is not bound to the issued snapshot",
                )
            })?;
        if binding.credential_revision.get() as i64 != connection.credential_revision {
            return Err(Error::invalid(
                "stale_revision",
                "the credential revision changed",
            ));
        }
        let policy_row = shared
            .store
            .protected_policy_row(args.connection_id.as_str())?
            .ok_or_else(|| {
                Error::invalid(
                    "operation_not_granted",
                    "the Connection has no reviewed protected policy",
                )
            })?;
        if policy_row.state != "active"
            || !policy_row.policy.active
            || !policy_row
                .policy
                .allowed_operations
                .contains(&OperationKindV1::SmtpSend)
        {
            return Err(Error::invalid(
                "operation_not_granted",
                "the Connection does not grant this operation",
            ));
        }
        let capability = loaded
            .snapshot
            .capabilities
            .iter()
            .find(|capability| capability.capability_id == args.capability_id)
            .ok_or_else(|| {
                Error::invalid("capability_unavailable", "the capability is not available")
            })?;
        if capability.operation != OperationKindV1::SmtpSend
            || capability.connection_scope != args.connection_id
        {
            return Err(Error::invalid(
                "capability_unavailable",
                "the capability is not available",
            ));
        }
        let grants = shared.store.protected_grants(
            caller.agent_uid(),
            &connection.platform,
            &connection.account,
        )?;
        exactly_one_grant(&grants, caller.agent_uid())?;
        // The source is the EXISTING human-decided effect. Model text can
        // never approve it; a missing/declined/wrong row refuses.
        let effect = shared
            .store
            .effect_by_id(args.source_id.as_str())?
            .ok_or_else(|| {
                Error::invalid("approval_required", "a current human approval is required")
            })?;
        if effect.state != "decided"
            || effect.decision.is_none()
            || effect.platform != connection.platform
            || effect.account != connection.account
            || effect.source_hash.as_deref() != Some(args.expected_source_digest.as_str())
        {
            return Err(Error::invalid(
                "approval_required",
                "a current human approval is required",
            ));
        }
        let message = message_from_effect(&effect)?;
        let projection = self.smtp_projection(&connection)?;
        let content_digest = effect
            .source_hash
            .clone()
            .unwrap_or_else(|| exact_byte_digest(effect.input.to_string().as_bytes()));
        let body = crate::platform::smtp::assemble_frozen(
            &projection.sender_name,
            &projection.sender,
            &projection.host,
            &message,
            &content_digest,
        )?;
        let body_digest =
            Digest::try_from(exact_byte_digest(body.as_bytes())).map_err(|_| refused())?;
        let destination = ResolvedDestinationV1 {
            kind: cadence_core::connection_dispatch::dto::DestinationKindV1::Smtp,
            host: ident(&projection.host)?,
            port: Some(projection.port),
            path: None,
        };
        let destination_digest =
            cadence_core::connection_dispatch::dto::destination_digest_for(&destination);
        let key_id = ident(&connection.custody)?;
        let principal = self.agent_principal(caller, &eligibility, &loaded, &key_id)?;
        let principal_digest = principal_binding_digest(&principal.principal)?;
        let approval_id = ident(&format!(
            "apr-{}",
            exact_byte_digest(
                format!(
                    "{}|{}|{}|{}|{}",
                    caller.session_binding(),
                    args.source_id.as_str(),
                    args.connection_id.as_str(),
                    binding.credential_revision.get(),
                    binding.authorization_revision.get(),
                )
                .as_bytes()
            )
            .trim_start_matches("sha256:")
        ))?;
        let approval = SendApprovalV1 {
            approval_id: approval_id.clone(),
            connection_id: args.connection_id.clone(),
            source_id: args.source_id.clone(),
            source_digest: args.expected_source_digest.clone(),
            action: ident("smtp.send")?,
            principal_digest,
            credential_revision: binding.credential_revision,
            authorization_revision: binding.authorization_revision,
            policy_revision: Revision::new(policy_row.policy_revision as u64)
                .map_err(|_| refused())?,
            body_digest,
            destination_digest,
            policy_digest: Digest::try_from(policy_row.policy_digest.clone())
                .map_err(|_| refused())?,
            human_decision_ref: args.source_id.clone(),
            consumed_dispatch_id: None,
        };
        let principal_key = caller.session_binding().to_string();
        let source_id = args.source_id.clone();
        let staged = shared.store.protected_management_commit(
            &authority.workspace_id,
            eligibility.owner_epoch,
            digest(
                "cadence-protected-management-binding-v1",
                &json!({
                    "kind": "stage_send",
                    "connection_id": args.connection_id.as_str(),
                    "source_id": args.source_id.as_str(),
                    "runtime_generation": eligibility.runtime_generation.get(),
                }),
            )?,
            |tx| {
                crate::store::connection_dispatch::put_approval_in(
                    tx,
                    &approval,
                    &principal_key,
                    &message.to,
                    body.as_bytes(),
                )?;
                let stored = crate::store::connection_dispatch::approval_material_in(
                    tx,
                    approval.approval_id.as_str(),
                )?
                .ok_or_else(|| {
                    Error::invalid("approval_required", "a current human approval is required")
                })?;
                let operation_binding = effect_operation_binding(
                    &authority.workspace_id,
                    &approval.principal_digest,
                    args.connection_id.as_str(),
                    binding.credential_revision,
                    binding.authorization_revision,
                    approval.policy_revision,
                    source_id.as_str(),
                    &stored.0.source_digest,
                    &stored.0.body_digest,
                );
                crate::store::effect_bind_in(tx, source_id.as_str(), operation_binding.as_str())?;
                Ok(stored.0)
            },
        )?;
        Ok(json!({"approval": staged}))
    }

    fn op_invoke(&self, caller: &RuntimeCaller, args: &Value) -> Result<Value> {
        let invoke: ConnectionInvokeV1 = parse_dto(args)?;
        if invoke.v != 1 {
            return Err(invalid_request());
        }
        let eligibility = self.eligibility_for(caller)?;
        let loaded = self.load_for_operation(
            caller,
            &eligibility,
            &invoke.snapshot_id,
            &invoke.loaded_receipt_id,
        )?;
        let shared = self.shared()?;
        let connection = shared
            .store
            .protected_connection_row(invoke.connection_id.as_str())?
            .ok_or_else(|| Error::invalid("not_found", "the selector is unavailable"))?;
        let binding = loaded
            .snapshot
            .connection_bindings
            .iter()
            .find(|binding| binding.connection_id == invoke.connection_id)
            .ok_or_else(|| {
                Error::invalid(
                    "loaded_state_mismatch",
                    "the Connection is not bound to the issued snapshot",
                )
            })?;
        if binding.credential_revision != invoke.expected_credential_revision
            || binding.authorization_revision != invoke.expected_authorization_revision
            || binding.credential_revision.get() as i64 != connection.credential_revision
        {
            return Err(Error::invalid(
                "stale_revision",
                "the credential or authorization revision changed",
            ));
        }
        let policy_row = shared
            .store
            .protected_policy_row(invoke.connection_id.as_str())?
            .ok_or_else(|| {
                Error::invalid(
                    "operation_not_granted",
                    "the Connection has no reviewed protected policy",
                )
            })?;
        let key_id = ident(&connection.custody)?;
        let principal = self.agent_principal(caller, &eligibility, &loaded, &key_id)?;
        let principal_digest = principal_binding_digest(&principal.principal)?;
        let material = match &invoke.operation {
            AgentOperationV1::TextGenerate(operation) => {
                let quote = shared
                    .store
                    .protected_quote(operation.quote_id.as_str())?
                    .ok_or_else(|| {
                        Error::invalid("pricing_unavailable", "the quote is unavailable")
                    })?;
                if shared
                    .store
                    .protected_quote_activation(operation.quote_id.as_str())?
                    .as_deref()
                    != Some(self.process_activation.as_str())
                {
                    return Err(Error::invalid(
                        "price_changed",
                        "the quote was observed by a previous process",
                    ));
                }
                if quote.connection_id != invoke.connection_id
                    || quote.credential_revision != binding.credential_revision
                    || quote.authorization_revision != binding.authorization_revision
                    || quote.policy_revision.get() as i64 != policy_row.policy_revision
                    || quote.model != operation.model
                    || quote.max_cost_micros != operation.max_cost_micros
                {
                    return Err(Error::invalid("price_changed", "the quote is stale"));
                }
                let endpoint = quote
                    .endpoints
                    .first()
                    .ok_or_else(|| Error::invalid("pricing_unavailable", "the quote is unusable"))?
                    .as_str();
                ProviderMaterial {
                    destination: https_destination(endpoint)?,
                    body: text_request_body(operation)?,
                    recipient: None,
                    approval_id: None,
                    key_id: key_id.clone(),
                    smtp_test_ca: None,
                }
            }
            AgentOperationV1::SmtpSend(operation) => {
                if connection.platform != SMTP_PROVIDER {
                    return Err(Error::invalid(
                        "operation_not_granted",
                        "the Connection does not grant this operation",
                    ));
                }
                let (approval, consumed, recipient, body) = shared
                    .store
                    .protected_approval_material(operation.approved_send_id.as_str())?
                    .ok_or_else(|| {
                        Error::invalid("approval_required", "a current human approval is required")
                    })?;
                if consumed.is_some()
                    || approval.connection_id != invoke.connection_id
                    || approval.credential_revision != binding.credential_revision
                    || approval.authorization_revision != binding.authorization_revision
                    || approval.policy_revision.get() as i64 != policy_row.policy_revision
                    || approval.principal_digest != principal_digest
                {
                    return Err(Error::invalid(
                        "approval_required",
                        "a current human approval is required",
                    ));
                }
                let projection = self.smtp_projection(&connection)?;
                ProviderMaterial {
                    destination: ResolvedDestinationV1 {
                        kind: cadence_core::connection_dispatch::dto::DestinationKindV1::Smtp,
                        host: ident(&projection.host)?,
                        port: Some(projection.port),
                        path: None,
                    },
                    body,
                    recipient: Some(recipient),
                    approval_id: Some(approval.approval_id.clone()),
                    key_id: key_id.clone(),
                    smtp_test_ca: shared.smtp_test_ca.clone(),
                }
            }
            // No reviewed human test-send record producer exists in this
            // profile, so the tag refuses truthfully rather than inventing a
            // record or running a second approval UI.
            AgentOperationV1::SmtpTestSend(_) => {
                return Err(Error::invalid(
                    "unsupported_verification",
                    "this profile has no reviewed human test-send record",
                ))
            }
        };
        Ok(invoke_response(
            self.dispatch(principal, invoke, loaded, material)?,
        ))
    }

    fn op_result(&self, caller: &RuntimeCaller, args: &Value) -> Result<Value> {
        let args: ResultArgsV1 = parse_dto(args)?;
        if args.v != 1 {
            return Err(invalid_request());
        }
        let eligibility = self.eligibility_for(caller)?;
        let shared = self.shared()?;
        let principal_key = caller.session_binding().to_string();
        let row = shared.store.protected_reconcile(
            args.dispatch_id.as_str(),
            args.request_id.as_str(),
            &principal_key,
        )?;
        let Some(row) = row else {
            return Err(Error::invalid("not_found", "the selector is unavailable"));
        };
        if row.commit.runtime_generation != eligibility.runtime_generation {
            return Err(Error::invalid(
                "placement_changed",
                "the loaded runtime changed since the dispatch",
            ));
        }
        // Cached results are re-authorized against the CURRENT grant/policy.
        shared.store.protected_read_authorized(
            &principal_key,
            Some(caller.agent_uid()),
            row.commit.connection_id.as_str(),
        )?;
        Ok(json!({
            "dispatch_id": row.commit.dispatch_id,
            "request_id": row.commit.request_id,
            "state": row.state,
            "receipt": row.receipt,
        }))
    }

    fn op_request(&self, caller: &RuntimeCaller, args: &Value) -> Result<Value> {
        let args: CredentialRequestArgsV1 = parse_dto(args)?;
        if args.v != 1 {
            return Err(invalid_request());
        }
        let eligibility = self.eligibility_for(caller)?;
        if eligibility.runtime_generation.get() != caller.runtime_generation() {
            return Err(Error::invalid(
                "placement_changed",
                "the loaded runtime changed",
            ));
        }
        let provider =
            crate::platform::connections::provider_identifier(&args.provider, "Provider")?;
        let verb = match args.verb {
            HumanVerbV1::Enroll => crate::store::credential_requests::CredentialVerb::Enroll,
            HumanVerbV1::Rotate => crate::store::credential_requests::CredentialVerb::Rotate,
            HumanVerbV1::Verify => crate::store::credential_requests::CredentialVerb::Verify,
        };
        if args.scopes.len() > 16
            || args.scopes.iter().any(|scope| {
                scope.is_empty()
                    || scope.len() > 64
                    || scope.bytes().any(|b| !(0x21..=0x7e).contains(&b))
            })
        {
            return Err(invalid_request());
        }
        let shape = match verb {
            crate::store::credential_requests::CredentialVerb::Enroll => Some(
                self.shared()?
                    .connection_descriptor(&provider)?
                    .enrollment_shapes
                    .first()
                    .cloned()
                    .ok_or_else(|| {
                        Error::invalid(
                            "capability_unavailable",
                            "the provider has no reviewed enrollment shape",
                        )
                    })?,
            ),
            _ => None,
        };
        let intent = AgentCredentialIntent {
            provider,
            verb,
            connection_id: args.connection_id,
            shape,
            scopes: args.scopes,
            expected_credential_revision: None,
            expected_authorization_revision: None,
        };
        let now = now_epoch_ms()?.get() as i64;
        let shared = self.shared()?;
        // The card must land in the chat this turn actually came from, so the
        // conversation is the broker's measured live task and its durable
        // message. The random runtime session binding is never a chat, and no
        // caller field can name one.
        let conversation = shared.native_model()?.conversation_for_turn(caller)?;
        let request = super::credential_entry_rpc::create_agent_credential_request(
            shared.as_ref(),
            caller.agent_uid(),
            &conversation,
            intent,
            now,
        )?;
        Ok(json!({
            "request": request.to_json(now),
            "status": request.status,
        }))
    }

    fn smtp_projection(
        &self,
        connection: &crate::store::connection_dispatch::ConnectionRow,
    ) -> Result<crate::platform::smtp::SmtpProjection> {
        let shared = self.shared()?;
        let record = shared
            .store
            .connection_credential(&connection.connection_id)?
            .ok_or_else(|| Error::invalid("not_found", "the credential is unavailable"))?;
        self.decode_smtp_projection(&record)
    }

    /// Decode the non-secret SMTP projection from exactly this credential
    /// record. The custody witness binds the record's own credential revision,
    /// key id and AAD, so a rotation cannot open its replacement ciphertext as
    /// the current record (or vice versa).
    fn decode_smtp_projection(
        &self,
        record: &crate::store::CredentialRecord,
    ) -> Result<crate::platform::smtp::SmtpProjection> {
        let shared = self.shared()?;
        let operation = shared.custody_operation()?;
        let witness = operation.witness(super::custody_rpc::CustodyUse::Connections, record)?;
        let bytes = shared.platform_custody.load_encrypted(&witness)?;
        let (_, projection) = crate::platform::smtp::custody_decode(&bytes)?;
        Ok(projection)
    }

    // -- the dispatch path -------------------------------------------------

    /// Admit, commit, checkpoint, execute once and settle one typed agent
    /// operation. The permit lives only on this stack frame.
    pub(crate) fn dispatch(
        &self,
        principal: DispatchPrincipal,
        invoke: ConnectionInvokeV1,
        loaded: LoadedSnapshot,
        material: ProviderMaterial,
    ) -> Result<InvokeOutcome> {
        let eligibility = self.eligibility_for(&principal.caller)?;
        Self::cross_check_runtime(&principal, &eligibility)?;
        let shared = self.shared()?;
        let request = AdmissionRequestV1::Agent(invoke.clone());
        let resolved = material.resolved()?;
        let facts = AdmissionFactsV1 {
            dispatch_id: random_uuid()?,
            request_id: random_uuid()?,
            now_ms: now_epoch_ms()?,
            requester: principal.requester.clone(),
            principal: principal.principal.clone(),
            runtime: principal.runtime.clone(),
            resolved,
            manifest_id: principal.manifest_id.clone(),
            key_id: principal.key_id.clone(),
            approval_id: material.approval_id.clone(),
            loaded_snapshot: Some(loaded),
        };
        let commit = match shared.store.protected_commit(facts, request)? {
            CommitOutcome::Duplicate(status) => {
                let row = shared.store.protected_reconcile(
                    status.dispatch_id.as_str(),
                    status.request_id.as_str(),
                    principal.principal_key(),
                )?;
                return Ok(InvokeOutcome::Duplicate {
                    dispatch_id: status.dispatch_id.as_str().to_string(),
                    request_id: status.request_id.as_str().to_string(),
                    state: status.state,
                    receipt: row.and_then(|row| row.receipt),
                });
            }
            CommitOutcome::Committed(commit) => commit,
        };
        self.finish(shared, eligibility, commit, invoke, material)
    }

    /// Human verification runs the same admission/checkpoint/execute/settle
    /// path, but the request is `smtp.verify@1` and the principal is the
    /// claimed human attempt. It authenticates once and sends no mail.
    pub(crate) fn verify(
        &self,
        attempt: &VerifiedHumanAttemptV1,
        material: ProviderMaterial,
        evidence: ManagementEvidence,
    ) -> Result<SettlementV1> {
        let verification_request_id = attempt
            .verification_request_id()
            .ok_or_else(|| Error::invalid("request_changed", "the verification record changed"))?;
        let connection_id = attempt
            .connection_id()
            .ok_or_else(|| Error::invalid("request_changed", "the verification target changed"))?;
        let credential_revision = Revision::new(
            attempt
                .expected_credential_revision()
                .ok_or_else(|| Error::invalid("request_changed", "the revision changed"))?
                as u64,
        )
        .map_err(|_| Error::invalid("request_changed", "the revision changed"))?;
        let authorization_revision = Revision::new(
            attempt
                .expected_authorization_revision()
                .ok_or_else(|| Error::invalid("request_changed", "the revision changed"))?
                as u64,
        )
        .map_err(|_| Error::invalid("request_changed", "the revision changed"))?;
        let shared = self.shared()?;
        let (requester, principal) =
            human_principal(attempt, evidence.owner_epoch, &evidence.workspace_id)?;
        let now = now_epoch_ms()?;
        let verification_uuid = UuidV1::try_from(verification_request_id.to_string())
            .map_err(|_| Error::invalid("request_changed", "the request id changed"))?;
        // The claimed human attempt and the server verification id are the
        // actual, non-authorizing selectors this verification binds.
        let enrollment_id = UuidV1::try_from(attempt.attempt_id().to_string())
            .map_err(|_| Error::invalid("request_changed", "the attempt id changed"))?;
        let request = AdmissionRequestV1::HumanSmtpVerify(HumanSmtpVerifyAdmissionV1 {
            v: 1,
            connection_id: ident(connection_id)?,
            expected_credential_revision: credential_revision,
            expected_authorization_revision: authorization_revision,
            verification_request_id: verification_uuid.clone(),
        });
        let facts = AdmissionFactsV1 {
            dispatch_id: random_uuid()?,
            request_id: verification_uuid.clone(),
            now_ms: now,
            requester,
            principal,
            runtime: Self::human_runtime_facts(
                &evidence,
                now.get(),
                enrollment_id,
                verification_uuid,
            )?,
            resolved: material.resolved()?,
            manifest_id: None,
            key_id: material.key_id.clone(),
            approval_id: None,
            loaded_snapshot: None,
        };
        let commit = match shared.store.protected_commit(facts, request)? {
            CommitOutcome::Duplicate(status) => {
                return Err(Error::invalid(
                    if status.state == DispatchStateV1::Committed {
                        "request_not_ready"
                    } else {
                        "already_submitted"
                    },
                    "the verification request was already used",
                ))
            }
            CommitOutcome::Committed(commit) => commit,
        };
        self.finish_human(shared, evidence.owner_epoch, commit, material)
    }

    fn cross_check_runtime(
        principal: &DispatchPrincipal,
        eligibility: &OwnerEligibility,
    ) -> Result<()> {
        if principal.runtime.owner_epoch != eligibility.owner_epoch
            || principal.runtime.runtime_generation != eligibility.runtime_generation
            || principal.runtime.observed_registry_digest != eligibility.observed_registry_digest
            || principal.runtime.enrollment_id != eligibility.enrollment_id
            || principal.runtime.fence != FenceStateV1::Clear
        {
            return Err(Error::invalid(
                "placement_changed",
                "the loaded runtime changed since the snapshot was issued",
            ));
        }
        Ok(())
    }

    // -- commit → checkpoint → permit → execute → settle --------------------

    fn finish(
        &self,
        shared: Arc<Shared>,
        eligibility: OwnerEligibility,
        commit: DispatchCommitV1,
        invoke: ConnectionInvokeV1,
        material: ProviderMaterial,
    ) -> Result<InvokeOutcome> {
        let permit = match self.permit(&shared, eligibility.owner_epoch, &commit, material) {
            Ok(permit) => permit,
            Err(error) => {
                // No acknowledgement means no provider I/O. The reservation
                // stays held and the durable row is readable as `unknown`.
                let _ = shared.store.protected_settle(commit, unknown_outcome());
                return Err(error);
            }
        };
        let outcome = permit.execute(&shared, &invoke);
        let settlement = shared.store.protected_settle(commit, outcome)?;
        Ok(InvokeOutcome::Settled(settlement))
    }

    fn finish_human(
        &self,
        shared: Arc<Shared>,
        owner_epoch: Revision,
        commit: DispatchCommitV1,
        material: ProviderMaterial,
    ) -> Result<SettlementV1> {
        let mut permit = match self.permit(&shared, owner_epoch, &commit, material) {
            Ok(permit) => permit,
            Err(error) => {
                let _ = shared.store.protected_settle(commit, unknown_outcome());
                return Err(error);
            }
        };
        let outcome = permit.execute_human_verify(&shared);
        shared.store.protected_settle(commit, outcome)
    }

    /// Acknowledge the committed head and mint the one-shot permit.
    /// Nothing durable can reconstruct this value.
    ///
    /// A verification call has zero metered cost, so its checkpoint binds the
    /// same committed head but measures no spend.
    fn permit(
        &self,
        shared: &Arc<Shared>,
        owner_epoch: Revision,
        commit: &DispatchCommitV1,
        material: ProviderMaterial,
    ) -> Result<ExecutionPermit> {
        shared.store.protected_acknowledge_commit(
            commit,
            owner_epoch,
            runtime_binding_digest(commit),
        )?;
        Ok(ExecutionPermit {
            commit: commit.clone(),
            material,
            consumed: false,
        })
    }
}

/// What one invoke answers. A duplicate is a status, never a second charge or
/// a second provider call.
#[allow(dead_code)]
pub(crate) enum InvokeOutcome {
    Duplicate {
        dispatch_id: String,
        request_id: String,
        state: DispatchStateV1,
        receipt: Option<DispatchReceiptV1>,
    },
    Settled(SettlementV1),
}

/// The trusted dispatch principal. Constructed only by the verified runtime
/// socket adapter from the kernel peer facts, the loaded snapshot/receipt and
/// the enrolled stable agent UID; every field is copied from real evidence.
pub(crate) struct DispatchPrincipal {
    /// The kernel-verified runtime subject this principal was derived from.
    caller: RuntimeCaller,
    requester: RequesterV1,
    principal: PrincipalBindingV1,
    runtime: RuntimeFactsV1,
    key_id: Ident,
    manifest_id: Option<Ident>,
}

impl DispatchPrincipal {
    pub(crate) fn new(
        caller: RuntimeCaller,
        requester: RequesterV1,
        principal: PrincipalBindingV1,
        runtime: RuntimeFactsV1,
        key_id: Ident,
        manifest_id: Ident,
    ) -> Self {
        Self {
            caller,
            requester,
            principal,
            runtime,
            key_id,
            manifest_id: Some(manifest_id),
        }
    }
    pub(crate) fn principal_key(&self) -> &str {
        self.principal.principal_id.as_str()
    }
}

/// The exact provider material one dispatch binds. `body` is frozen before
/// admission and is the only payload that may reach the provider.
pub(crate) struct ProviderMaterial {
    pub destination: ResolvedDestinationV1,
    pub body: Vec<u8>,
    pub recipient: Option<String>,
    pub approval_id: Option<Ident>,
    pub key_id: Ident,
    pub smtp_test_ca: Option<Vec<u8>>,
}

impl ProviderMaterial {
    fn resolved(&self) -> Result<ResolvedMaterialV1> {
        Ok(ResolvedMaterialV1 {
            destination: self.destination.clone(),
            provider_body_digest: Digest::try_from(exact_byte_digest(&self.body))
                .map_err(|_| refused())?,
        })
    }
}

/// The one-shot permit. Not `Clone`, consumed by value; the fields are the
/// frozen commit plus the exact bytes it bound. Nothing reconstructs it from a
/// durable row.
struct ExecutionPermit {
    commit: DispatchCommitV1,
    material: ProviderMaterial,
    consumed: bool,
}

impl ExecutionPermit {
    fn execute(mut self, shared: &Shared, invoke: &ConnectionInvokeV1) -> ValidatedOutcomeV1 {
        if self.consumed {
            return unknown_outcome();
        }
        self.consumed = true;
        if verify_frozen_dispatch_v1(
            &self.commit,
            &self.material.body,
            &self.commit.frozen_identity,
        )
        .is_err()
        {
            return refused_outcome(ErrorCodeV1::DigestMismatch);
        }
        match &invoke.operation {
            AgentOperationV1::TextGenerate(operation) => {
                execute_text(shared, &self.commit, operation, &self.material)
            }
            AgentOperationV1::SmtpSend(_) | AgentOperationV1::SmtpTestSend(_) => {
                execute_smtp_send(shared, &self.commit, &self.material)
            }
        }
    }

    fn execute_human_verify(&mut self, shared: &Shared) -> ValidatedOutcomeV1 {
        if self.consumed {
            return unknown_outcome();
        }
        self.consumed = true;
        if verify_frozen_dispatch_v1(
            &self.commit,
            &self.material.body,
            &self.commit.frozen_identity,
        )
        .is_err()
        {
            return refused_outcome(ErrorCodeV1::DigestMismatch);
        }
        execute_smtp_verify(shared, &self.commit, &self.material)
    }
}

// ---------------------------------------------------------------------------
// Provider execution (never holds a store lock)
// ---------------------------------------------------------------------------

fn execute_smtp_verify(
    shared: &Shared,
    commit: &DispatchCommitV1,
    material: &ProviderMaterial,
) -> ValidatedOutcomeV1 {
    let envelope = match load_smtp_envelope(shared, commit) {
        Ok(envelope) => envelope,
        Err(_) => return refused_outcome(ErrorCodeV1::CustodyLocked),
    };
    let deadline = crate::platform::OpDeadline::in_seconds(EXECUTION_DEADLINE.as_secs());
    let addresses = match public_smtp_addresses(&envelope.host, envelope.port, &deadline) {
        Ok(addresses) => addresses,
        Err(_) => return refused_outcome(ErrorCodeV1::DestinationRefused),
    };
    match crate::platform::smtp::verify_login(
        &envelope,
        &addresses,
        material.smtp_test_ca.as_deref(),
        &deadline,
    ) {
        Ok(()) => done_verified(),
        // AUTH was refused: a definitive pre-execution refusal, no mail sent.
        Err(failure) if failure.code == crate::platform::smtp::FailureCode::AuthFailed => {
            refused_outcome(ErrorCodeV1::ProviderRefused)
        }
        // Interrupted/ambiguous AUTH is unknown and is never retried.
        Err(_) => unknown_outcome(),
    }
}

fn execute_smtp_send(
    shared: &Shared,
    commit: &DispatchCommitV1,
    material: &ProviderMaterial,
) -> ValidatedOutcomeV1 {
    let envelope = match load_smtp_envelope(shared, commit) {
        Ok(envelope) => envelope,
        Err(_) => return refused_outcome(ErrorCodeV1::CustodyLocked),
    };
    let Some(recipient) = material.recipient.as_deref() else {
        return refused_outcome(ErrorCodeV1::DestinationRefused);
    };
    let Ok(body) = std::str::from_utf8(&material.body) else {
        return refused_outcome(ErrorCodeV1::DigestMismatch);
    };
    let deadline = crate::platform::OpDeadline::in_seconds(EXECUTION_DEADLINE.as_secs());
    let addresses = match public_smtp_addresses(&envelope.host, envelope.port, &deadline) {
        Ok(addresses) => addresses,
        Err(_) => return refused_outcome(ErrorCodeV1::DestinationRefused),
    };
    match crate::platform::smtp::send_frozen(
        &envelope,
        recipient,
        body,
        &addresses,
        material.smtp_test_ca.as_deref(),
    ) {
        Ok(crate::platform::smtp::SmtpOutcome::Accepted { .. }) => ValidatedOutcomeV1 {
            v: 1,
            kind: OutcomeKindV1::Done,
            usage: None,
            result: Some(OutcomeResultV1::SmtpAccepted(SmtpAcceptedOutcomeV1 {
                kind: SmtpAcceptedKindV1::SmtpAccepted,
                accepted: true,
            })),
            refused_code: None,
        },
        // Definitively not submitted before the message data was accepted.
        Ok(crate::platform::smtp::SmtpOutcome::NotSubmitted { .. })
        | Ok(crate::platform::smtp::SmtpOutcome::Rejected { .. }) => {
            refused_outcome(ErrorCodeV1::ProviderRefused)
        }
        // Deferred/uncertain: the server may hold the message.
        Ok(_) => unknown_outcome(),
        Err(_) => unknown_outcome(),
    }
}

fn execute_text(
    shared: &Shared,
    commit: &DispatchCommitV1,
    operation: &TextGenerateV1,
    material: &ProviderMaterial,
) -> ValidatedOutcomeV1 {
    let _ = operation;
    let endpoint = match material.destination.https_endpoint() {
        Some(endpoint) if endpoint.starts_with(OPENROUTER_BASE) => endpoint,
        _ => return refused_outcome(ErrorCodeV1::DestinationRefused),
    };
    let key = match load_openrouter_key(shared, commit) {
        Ok(key) => key,
        Err(_) => return refused_outcome(ErrorCodeV1::CustodyLocked),
    };
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(EXECUTION_DEADLINE))
        .max_redirects(0)
        .http_status_as_error(false)
        .build()
        .into();
    let mut response = match agent
        .post(&endpoint)
        .header("authorization", &format!("Bearer {}", key.as_str()))
        .header("content-type", "application/json")
        .send(material.body.as_slice())
    {
        Ok(response) => response,
        Err(_) => return unknown_outcome(),
    };
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        // 4xx is a definitive pre-execution refusal; 5xx may have executed.
        return if (400..500).contains(&status) {
            refused_outcome(ErrorCodeV1::ProviderRefused)
        } else {
            unknown_outcome()
        };
    }
    let bytes = match response
        .body_mut()
        .with_config()
        .limit(MODEL_RESPONSE_MAX)
        .read_to_vec()
    {
        Ok(bytes) => bytes,
        Err(_) => return unknown_outcome(),
    };
    // Strict, bounded parse. Anything surprising is `unknown`: the provider
    // may have billed the call, so the reservation is retained.
    let Some((text, usage)) = parse_model_response(&bytes) else {
        return unknown_outcome();
    };
    let Some(model) = commit.model.clone() else {
        return unknown_outcome();
    };
    // The response text must not carry the credential or a known fragment.
    if crate::platform::refuse_leak("protected model response", &text, key.as_bytes()).is_err() {
        // Billed but withholdable: keep the reservation (unknown), never leak.
        return unknown_outcome();
    }
    ValidatedOutcomeV1 {
        v: 1,
        kind: OutcomeKindV1::Done,
        usage: Some(usage),
        result: Some(OutcomeResultV1::TextGenerated(TextOutcomeV1 {
            kind: TextGeneratedKindV1::TextGenerated,
            model,
            text,
        })),
        refused_code: None,
    }
}

fn parse_model_response(bytes: &[u8]) -> Option<(String, UsageV1)> {
    let value: Value = serde_json::from_slice(bytes).ok()?;
    let root = value.as_object()?;
    let choices = root.get("choices")?.as_array()?;
    let first = choices.first()?.as_object()?;
    let message = first.get("message")?.as_object()?;
    let text = message.get("content")?.as_str()?.to_string();
    if text.len() > 1024 * 1024 {
        return None;
    }
    let usage = root.get("usage")?.as_object()?;
    let input = usage.get("prompt_tokens")?.as_u64()?;
    let output = usage.get("completion_tokens")?.as_u64()?;
    let total = usage
        .get("total_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(input.checked_add(output)?);
    let reasoning = usage
        .get("completion_tokens_details")
        .and_then(Value::as_object)
        .and_then(|details| details.get("reasoning_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    Some((
        text,
        UsageV1 {
            input_tokens: Counter::new(input).ok()?,
            output_tokens: Counter::new(output).ok()?,
            reasoning_tokens: Counter::new(reasoning).ok()?,
            total_tokens: Counter::new(total).ok()?,
        },
    ))
}

// ---------------------------------------------------------------------------
// Secret materialization (inside the consuming adapter only)
// ---------------------------------------------------------------------------

fn load_smtp_envelope(
    shared: &Shared,
    commit: &DispatchCommitV1,
) -> Result<crate::platform::smtp::SmtpEnvelope> {
    let record = shared
        .store
        .connection_credential(commit.connection_id.as_str())?
        .ok_or_else(|| Error::invalid("not_found", "the credential is unavailable"))?;
    let operation = shared.custody_operation()?;
    let witness = operation.witness(super::custody_rpc::CustodyUse::Connections, &record)?;
    let bytes = shared.platform_custody.load_encrypted(&witness)?;
    let (envelope, projection) = crate::platform::smtp::custody_decode(&bytes)?;
    if projection.host.is_empty() {
        return Err(Error::internal("the stored SMTP transport is unusable"));
    }
    Ok(envelope)
}

fn load_openrouter_key(
    shared: &Shared,
    commit: &DispatchCommitV1,
) -> Result<zeroize::Zeroizing<String>> {
    let record = shared
        .store
        .connection_credential(commit.connection_id.as_str())?
        .ok_or_else(|| Error::invalid("not_found", "the credential is unavailable"))?;
    let operation = shared.custody_operation()?;
    let witness = operation.witness(super::custody_rpc::CustodyUse::Connections, &record)?;
    let bytes = shared.platform_custody.load_encrypted(&witness)?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| Error::invalid("custody_locked", "the stored credential is unusable"))?
        .trim()
        .to_string();
    if text.is_empty() {
        return Err(Error::invalid(
            "custody_locked",
            "the stored credential is unusable",
        ));
    }
    Ok(zeroize::Zeroizing::new(text))
}

/// Resolve an enrolled SMTP host and require every answer to be globally
/// routable. The returned set is what the session connects to, so a later
/// re-resolution cannot move the socket.
fn public_smtp_addresses(
    host: &str,
    port: u16,
    deadline: &crate::platform::OpDeadline,
) -> Result<Vec<std::net::SocketAddr>> {
    let addresses = crate::platform::smtp::resolve_host(host, port)?;
    if addresses.is_empty() || addresses.iter().any(|address| !public_ip(address.ip())) {
        return Err(Error::invalid(
            "destination_refused",
            "the destination is not approved",
        ));
    }
    if deadline.expired() {
        return Err(Error::invalid(
            "destination_refused",
            "the destination is not approved",
        ));
    }
    Ok(addresses)
}

/// Globally routable unicast only: not loopback, private, link-local, CGNAT,
/// documentation, benchmarking, multicast, reserved or unspecified. The same
/// predicate the git egress guard uses.
fn public_ip(ip: std::net::IpAddr) -> bool {
    use std::net::IpAddr;
    match ip {
        IpAddr::V4(v) => {
            let o = v.octets();
            !(v.is_unspecified()
                || v.is_loopback()
                || v.is_private()
                || v.is_link_local()
                || v.is_broadcast()
                || v.is_multicast()
                || v.is_documentation()
                || o[0] == 0
                || (o[0] == 100 && (64..=127).contains(&o[1]))
                || (o[0] == 198 && (18..=19).contains(&o[1]))
                || o[0] >= 240)
        }
        IpAddr::V6(v) => {
            if let Some(v4) = v.to_ipv4_mapped() {
                return public_ip(IpAddr::V4(v4));
            }
            let s = v.segments();
            !(v.is_unspecified()
                || v.is_loopback()
                || v.is_multicast()
                || (s[0] & 0xfe00) == 0xfc00
                || (s[0] & 0xffc0) == 0xfe80
                || (s[0] == 0x2001 && s[1] == 0x0db8)
                || (s[0] == 0x0064 && s[1] == 0xff9b))
        }
    }
}

// ---------------------------------------------------------------------------
// Human credential mutation
// ---------------------------------------------------------------------------

impl ProtectedConnections {
    /// The single trusted mutation behind an authenticated human attempt.
    /// Enrollment/rotation writes the encrypted custody row and the credential
    /// record; verification runs the real AUTH-only provider check through the
    /// same authority path. A verification never mints send authority.
    pub(crate) fn apply_human_attempt(
        &self,
        attempt: &VerifiedHumanAttemptV1,
        input: &BoundedSecretInputV1,
    ) -> Result<Value> {
        // The native-model source is a broker-private purpose, not a business
        // Connection: it never creates a Connection/wallet/vault row here and
        // never uses business funding.
        if crate::store::credential_requests::is_native_model_shape(attempt.shape()) {
            let shared = self.shared()?;
            return shared.native_model()?.apply_human_attempt(attempt, input);
        }
        let evidence = self.management_evidence()?;
        attempt.require_management_context(
            &evidence.workspace_id,
            &evidence.custody_domain,
            evidence.database_epoch.get(),
            evidence.owner_epoch.get(),
            &evidence.supervisor_generation,
        )?;
        match attempt.verb() {
            "verify" => {
                let material = self.verify_material(attempt)?;
                let settlement = self.verify(attempt, material, evidence)?;
                verification_result(settlement)
            }
            "enroll" | "rotate" => self.enroll(attempt, input, evidence),
            _ => Err(Error::invalid(
                "invalid_request",
                "the credential request verb is unsupported",
            )),
        }
    }

    fn enroll(
        &self,
        attempt: &VerifiedHumanAttemptV1,
        input: &BoundedSecretInputV1,
        profile_evidence: ManagementEvidence,
    ) -> Result<Value> {
        let shared = self.shared()?;
        // Bind this authorizing mutation to the actual authenticated
        // profile/owner evidence, re-read for this attempt. A revoked,
        // missing or forked owner refuses before any ciphertext or core row
        // lands, and no live agent runtime is needed for a first enrollment.
        let vault = shared.platform_custody.encrypted().ok_or_else(|| {
            Error::invalid(
                "custody_locked",
                "this daemon has no protected encrypted custody profile — an \
                 operator must provision the isolated custody key and unlock it \
                 before a protected credential can be saved",
            )
        })?;
        let key_id = vault.key_id().ok_or_else(|| {
            Error::invalid(
                "custody_locked",
                "protected custody is locked — unlock it before a credential can be saved",
            )
        })?;
        let rotated = attempt.verb() == "rotate";
        let existing = match attempt.connection_id() {
            Some(connection_id) => Some(
                shared
                    .store
                    .connection_credential(connection_id)?
                    .ok_or_else(|| Error::invalid("request_changed", "the connection changed"))?,
            ),
            None => None,
        };
        let (base, revision) = match (&existing, rotated) {
            (Some(record), true) => {
                if let Some(expected) = attempt.expected_credential_revision() {
                    if record.credential_revision != expected as u64 {
                        return Err(Error::invalid("stale_revision", "the credential changed"));
                    }
                }
                let next = record
                    .credential_revision
                    .checked_add(1)
                    .ok_or_else(|| Error::invalid("state_conflict", "the revision is exhausted"))?;
                (record.clone(), next)
            }
            (Some(_), false) => {
                return Err(Error::invalid(
                    "request_changed",
                    "the connection is already enrolled",
                ))
            }
            (None, true) => {
                return Err(Error::invalid("request_changed", "the connection changed"))
            }
            (None, false) => {
                if attempt.expected_credential_revision().is_some() {
                    return Err(Error::invalid("stale_revision", "the credential changed"));
                }
                (synthetic_record(attempt), 1)
            }
        };
        if base.platform != attempt.provider() {
            return Err(Error::invalid("request_changed", "the provider changed"));
        }
        let (bytes, exchange, screen) = match input.kind() {
            SecretKind::SmtpPassword => {
                // A fresh enrollment is the only path where the human typed a
                // transport. Rotation inherits the pinned, already-admitted
                // transport: the current ciphertext is opened under its own
                // credential revision/key id/AAD and only the password is
                // replaced. No model, caller or browser can repoint it.
                let enrollment = match (input.transport(), rotated) {
                    (Some(transport), false) => crate::platform::smtp::SmtpEnrollment {
                        host: transport.host().to_string(),
                        port: transport.port(),
                        tls_mode: transport.tls_mode().to_string(),
                        username: transport.username().to_string(),
                        secret: input.expose().to_vec(),
                        sender: transport.sender().to_string(),
                        sender_name: transport.sender_name().to_string(),
                    },
                    (Some(_), true) => {
                        return Err(Error::invalid(
                            "invalid_request",
                            "SMTP rotation inherits the pinned transport",
                        ))
                    }
                    (None, true) => {
                        let current = self.decode_smtp_projection(&base)?;
                        let secret = std::str::from_utf8(input.expose()).map_err(|_| {
                            Error::invalid("invalid_request", "the credential is unusable")
                        })?;
                        crate::platform::smtp::overlay_rotate(
                            &current,
                            &json!({ "secret": secret }),
                        )?
                    }
                    (None, false) => {
                        return Err(Error::invalid(
                            "invalid_request",
                            "the SMTP transport is missing",
                        ))
                    }
                };
                let bytes = crate::platform::smtp::custody_bytes(&enrollment)?;
                crate::platform::refuse_leak(
                    "protected SMTP enrollment",
                    &smtp_enrollment_metadata(&enrollment),
                    &enrollment.secret,
                )?;
                (bytes, "smtp".to_string(), enrollment.secret)
            }
            SecretKind::OpenRouterApiKey => {
                let text = std::str::from_utf8(input.expose())
                    .map_err(|_| Error::invalid("invalid_request", "the credential is unusable"))?;
                if text.trim() != text {
                    return Err(Error::invalid(
                        "invalid_request",
                        "the credential is unusable",
                    ));
                }
                (
                    input.expose().to_vec(),
                    "token".to_string(),
                    input.expose().to_vec(),
                )
            }
            SecretKind::NativeProviderApiKey | SecretKind::NativeProviderDevinCredentials => {
                return Err(Error::invalid(
                    "invalid_request",
                    "native provider sources cannot become business Connections",
                ))
            }
            SecretKind::None => {
                return Err(Error::invalid(
                    "invalid_request",
                    "the credential is missing",
                ))
            }
        };
        let workspace_id = shared.store.connection_workspace_id()?;
        // The secret is screened against every piece of metadata that will be
        // persisted BEFORE any of it lands.
        crate::platform::refuse_leak(
            "protected credential record",
            &format!(
                "{} {} {} {}",
                base.connection_id, base.platform, base.account, exchange
            ),
            &screen,
        )?;
        let identity = crate::platform::encrypted_sqlite::ExpectedIdentity {
            workspace_id,
            connection_id: base.connection_id.clone(),
            provider: base.platform.clone(),
            account: base.account.clone(),
            credential_revision: revision,
            exchange: exchange.clone(),
            key_id,
        };
        let reference = vault.write_row(&identity, &bytes)?;
        let scopes = if attempt.scopes().is_empty() {
            default_scopes(attempt.provider())
        } else {
            attempt.scopes().to_vec()
        };
        let record = crate::store::CredentialRecord {
            connection_id: base.connection_id,
            credential_revision: revision,
            platform: base.platform,
            account: base.account,
            scopes,
            fingerprint: crate::secret::fingerprint(&screen),
            custody: reference,
            exchange,
            enrolled_at: crate::store::now(),
            by: if rotated {
                "human:rotate".to_string()
            } else {
                "human:enroll".to_string()
            },
        };
        shared
            .store
            .platform_enroll(&record, rotated, Some("protected-encrypted"))?;
        // The credential revision change is authorizing: its journaled head
        // must be acknowledged by the external floor before this reports
        // success. A lost acknowledgement is outcome_unknown, never a retried
        // mutation. The checkpoint carries the real authenticated owner epoch
        // (not the database epoch) so an owner restart still acknowledges.
        let workspace_id = shared.store.connection_workspace_id()?;
        if workspace_id != profile_evidence.workspace_id {
            return Err(Error::invalid(
                "placement_changed",
                "the protected workspace changed",
            ));
        }
        shared.store.protected_management_commit(
            &workspace_id,
            profile_evidence.owner_epoch,
            digest(
                "cadence-protected-credential-mutation-v1",
                &json!({
                    "connection_id": record.connection_id,
                    "credential_revision": record.credential_revision,
                    "provider": record.platform,
                    "verb": if rotated { "rotate" } else { "enroll" },
                }),
            )?,
            |_tx| Ok(()),
        )?;
        Ok(json!({
            "connection_id": record.connection_id,
            "credential_revision": record.credential_revision,
            "provider": record.platform,
            "status": if rotated { "rotated" } else { "saved" },
            "custody": "protected-encrypted",
        }))
    }

    fn verify_material(&self, attempt: &VerifiedHumanAttemptV1) -> Result<ProviderMaterial> {
        let shared = self.shared()?;
        let connection_id = attempt
            .connection_id()
            .ok_or_else(|| Error::invalid("request_changed", "the verification target changed"))?;
        let record = shared
            .store
            .connection_credential(connection_id)?
            .ok_or_else(|| Error::invalid("request_changed", "the connection changed"))?;
        if record.platform != "smtp" {
            return Err(Error::invalid(
                "unsupported_verification",
                "the provider has no reviewed verification adapter",
            ));
        }
        let projection = {
            let operation = shared.custody_operation()?;
            let witness =
                operation.witness(super::custody_rpc::CustodyUse::Connections, &record)?;
            let bytes = shared.platform_custody.load_encrypted(&witness)?;
            let (_, projection) = crate::platform::smtp::custody_decode(&bytes)?;
            projection
        };
        let verification_request_id = attempt
            .verification_request_id()
            .ok_or_else(|| Error::invalid("request_changed", "the verification record changed"))?;
        let body = serde_json::to_vec(&json!({
            "kind": "smtp.verify@1",
            "verification_request_id": verification_request_id,
        }))
        .map_err(|e| Error::internal(e.to_string()))?;
        Ok(ProviderMaterial {
            destination: ResolvedDestinationV1 {
                kind: cadence_core::connection_dispatch::DestinationKindV1::Smtp,
                host: ident(&projection.host)?,
                port: Some(projection.port),
                path: None,
            },
            body,
            recipient: None,
            approval_id: None,
            key_id: ident(&record.custody)?,
            smtp_test_ca: shared.smtp_test_ca.clone(),
        })
    }
}

/// A completed dispatch is not necessarily a successful provider verification.
/// Preserve definitive refusal and ambiguity after the durable settlement.
fn verification_result(settlement: SettlementV1) -> Result<Value> {
    if !settlement.fence
        && settlement.receipt.state == DispatchStateV1::Done
        && settlement.receipt.validate().is_ok()
        && matches!(
            &settlement.receipt.result,
            Some(ReceiptResultV1::SmtpVerified(result)) if result.verified
        )
    {
        return Ok(json!({"receipt": settlement.receipt, "fence": false}));
    }
    if !settlement.fence
        && settlement.receipt.state == DispatchStateV1::Refused
        && settlement.receipt.validate().is_ok()
    {
        return Err(Error::invalid(
            "provider_refused",
            "SMTP authentication was refused",
        ));
    }
    Err(Error::OutcomeUnknown(
        "SMTP verification outcome is unknown".to_string(),
    ))
}

fn smtp_enrollment_metadata(enrollment: &crate::platform::smtp::SmtpEnrollment) -> String {
    json!({
        "host": &enrollment.host,
        "port": enrollment.port,
        "tls_mode": &enrollment.tls_mode,
        "username": &enrollment.username,
        "sender": &enrollment.sender,
        "sender_name": &enrollment.sender_name,
    })
    .to_string()
}

fn default_scopes(provider: &str) -> Vec<String> {
    match provider {
        "smtp" => vec![crate::platform::smtp::SCOPE_EMAIL_SEND.to_string()],
        _ => Vec::new(),
    }
}

fn synthetic_record(attempt: &VerifiedHumanAttemptV1) -> crate::store::CredentialRecord {
    crate::store::CredentialRecord {
        connection_id: format!("conn-{}", uuid::Uuid::new_v4().simple()),
        credential_revision: 0,
        platform: attempt.provider().to_string(),
        account: attempt.target_id().to_string(),
        scopes: Vec::new(),
        fingerprint: String::new(),
        custody: String::new(),
        exchange: String::new(),
        enrolled_at: crate::store::now(),
        by: String::new(),
    }
}

fn human_principal(
    attempt: &VerifiedHumanAttemptV1,
    owner_epoch: Revision,
    workspace_id: &str,
) -> Result<(RequesterV1, PrincipalBindingV1)> {
    let human_uid = human_agent_uid(attempt.entry_session_binding())?;
    // The human entry session is created fresh for this attempt: its session,
    // session-authority, enrollment and agent generations are all the actual
    // first generation, not a borrowed agent runtime generation.
    let session_generation = ProtectedConnections::human_session_generation()?;
    let requester = RequesterV1 {
        agent_uid: human_uid.clone(),
        session_binding_id: UuidV1::try_from(attempt.attempt_id().to_string())
            .map_err(|_| Error::invalid("request_changed", "the attempt id changed"))?,
        runtime_generation: session_generation,
    };
    let principal = PrincipalBindingV1 {
        custody_domain: ident("local")?,
        workspace_id: ident(workspace_id)?,
        kind: PrincipalKindV1::Human,
        principal_id: ident(attempt.entry_session_binding())?,
        agent_uid: human_uid,
        alias: None,
        native_session: Some(NativeSessionV1 {
            session_id: ident(attempt.entry_session_binding())?,
            generation: session_generation,
        }),
        session_authority_ref: None,
        session_generation: Some(session_generation),
        conversation_generation: None,
        enrollment_generation: session_generation,
        binding: RuntimeBindingV1::Native(NativeRuntimeBindingV1 {
            kind: NativeBindingKindV1::Native,
            launch_digest: digest(
                "cadence-protected-human-launch-v1",
                &json!({
                    "workspace": workspace_id,
                    "runtime_generation": session_generation.get(),
                    "owner_epoch": owner_epoch.get(),
                }),
            )?,
            continuity_digest: digest(
                "cadence-protected-human-continuity-v1",
                &json!({
                    "workspace": workspace_id,
                    "session": attempt.entry_session_binding(),
                    "request": attempt.request_id(),
                }),
            )?,
            agent_generation: session_generation,
            owner_generation: owner_epoch,
        }),
    };
    Ok((requester, principal))
}

/// A stable, non-authorizing agent-shaped selector for a human entry session.
/// It is never matched against a UID grant (human-only operations carry none);
/// it exists because the shared principal DTO always names an `agt-` id.
fn human_agent_uid(session_binding: &str) -> Result<AgentUid> {
    use sha2::{Digest as _, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"cadence-human-principal-v1");
    hasher.update(session_binding.as_bytes());
    let hex = hasher.finalize();
    let text: String = hex.iter().take(16).map(|b| format!("{b:02x}")).collect();
    AgentUid::try_from(format!("agt-{text}"))
        .map_err(|_| Error::invalid("request_changed", "the principal changed"))
}

// ---------------------------------------------------------------------------
// Shared hooks
// ---------------------------------------------------------------------------

impl Shared {
    /// The protected backend, if the daemon installed one.
    pub(crate) fn protected(&self) -> Result<&Arc<ProtectedConnections>> {
        self.protected
            .get()
            .ok_or_else(|| Error::invalid("runtime_unqualified", NOT_READY))
    }

    /// The live protected context one human credential request binds.
    pub(crate) fn protected_entry_context(&self, target: &EntryTargetV1) -> Result<EntryContextV1> {
        self.protected()?.entry_context(target)
    }

    /// The single trusted human credential mutation behind a claimed attempt.
    pub(crate) fn apply_human_credential_attempt(
        &self,
        attempt: &VerifiedHumanAttemptV1,
        input: &BoundedSecretInputV1,
    ) -> Result<Value> {
        self.protected()?.apply_human_attempt(attempt, input)
    }

    /// The single per-runtime operation entry the router calls. The caller is
    /// server-derived; nothing in `args` can name another actor/runtime.
    pub(crate) fn handle_runtime(
        &self,
        caller: &RuntimeCaller,
        method: OpsMethod,
        args: &Value,
    ) -> Result<Value> {
        self.protected()?.handle_runtime(caller, method, args)
    }

    /// Install the backend after the `Arc<Shared>` exists (two-phase init, so
    /// the backend can hold a `Weak` back-reference and there is no cycle).
    pub(crate) fn install_protected(
        self: &Arc<Self>,
        owner: Option<Arc<dyn ProtectedOwnerClient>>,
    ) -> Result<()> {
        let backend = Arc::new(ProtectedConnections::new(self, owner));
        self.protected
            .set(backend)
            .map_err(|_| Error::internal("the protected backend is already installed"))
    }
}

// ---------------------------------------------------------------------------
// Runtime operation helpers
// ---------------------------------------------------------------------------

const OPENROUTER_PROVIDER: &str = "openrouter";
const OPENROUTER_ROUTING: &str = "openrouter-fixed";
const OPENROUTER_METADATA_URL: &str = "https://openrouter.ai/api/v1/models";
const OPENROUTER_ENDPOINT: &str = "https://openrouter.ai/api/v1/chat/completions";
const SMTP_PROVIDER: &str = "smtp";
const QUOTE_READS_PER_PRINCIPAL: u64 = 10;
const QUOTE_READS_PER_WORKSPACE: u64 = 30;
const MODEL_MAX_OUTPUT: u32 = 8192;
const METADATA_MAX_BYTES: u64 = 2 * 1024 * 1024;
const METADATA_DEADLINE: std::time::Duration = std::time::Duration::from_secs(20);

fn invalid_request() -> Error {
    Error::invalid("invalid_request", "the operation is malformed")
}

fn pricing_unavailable() -> Error {
    Error::invalid("pricing_unavailable", "the model price is unavailable")
}

fn destination_refused() -> Error {
    Error::invalid("destination_refused", "the destination is not approved")
}

fn approval_required() -> Error {
    Error::invalid("approval_required", "a current human approval is required")
}

fn operation_not_granted() -> Error {
    Error::invalid(
        "operation_not_granted",
        "the Connection does not grant this operation",
    )
}

fn parse_dto<T: serde::de::DeserializeOwned>(args: &Value) -> Result<T> {
    serde_json::from_value(args.clone()).map_err(|_| invalid_request())
}

fn agent_uid_of(caller: &RuntimeCaller) -> Result<AgentUid> {
    AgentUid::try_from(caller.agent_uid().to_string())
        .map_err(|_| Error::invalid("principal_required", "the runtime identity is invalid"))
}

fn exactly_one_grant<'a>(grants: &'a [UidGrantV1], uid: &str) -> Result<&'a UidGrantV1> {
    let mut matching = grants
        .iter()
        .filter(|grant| grant.agent_uid.as_str() == uid);
    let Some(grant) = matching.next() else {
        return Err(operation_not_granted());
    };
    if matching.next().is_some() || grant.state != GrantStateV1::Active {
        return Err(operation_not_granted());
    }
    Ok(grant)
}

fn principal_binding_digest(principal: &PrincipalBindingV1) -> Result<Digest> {
    Ok(material_digest_value(
        cadence_core::connection_dispatch::CONNECTION_DISPATCH_DOMAIN,
        &serde_json::to_value(principal).map_err(|_| refused())?,
    ))
}

fn message_from_effect(
    effect: &crate::store::EffectRow,
) -> Result<crate::platform::smtp::SmtpMessage> {
    let object = effect.input.as_object().ok_or_else(approval_required)?;
    let field = |name: &str| object.get(name).and_then(Value::as_str).map(str::to_string);
    let to = field("to")
        .or_else(|| field("recipient"))
        .ok_or_else(approval_required)?;
    let subject = field("subject").ok_or_else(approval_required)?;
    let html = field("html").unwrap_or_default();
    let text = field("text").unwrap_or_default();
    if html.is_empty() && text.is_empty() {
        return Err(approval_required());
    }
    let unsubscribe_url = field("unsubscribe_url").unwrap_or_default();
    let idempotency_key = effect.source_hash.clone().filter(|hash| {
        let key = hash.strip_prefix("sha256:").unwrap_or(hash);
        !key.is_empty()
            && key.len() <= 128
            && key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    });
    Ok(crate::platform::smtp::SmtpMessage {
        to,
        subject,
        html,
        text,
        unsubscribe_url,
        idempotency_key,
    })
}

fn text_request_body(operation: &TextGenerateV1) -> Result<Vec<u8>> {
    let mut messages = Vec::with_capacity(operation.messages.len());
    for message in &operation.messages {
        messages.push(json!({
            "role": serde_json::to_value(message.role).map_err(|_| invalid_request())?,
            "content": message.content,
        }));
    }
    serde_json::to_vec(&json!({
        "model": operation.model.as_str(),
        "messages": messages,
        "max_tokens": operation.max_output_tokens.get(),
        "stream": false,
    }))
    .map_err(|_| invalid_request())
}

fn https_destination(endpoint: &str) -> Result<ResolvedDestinationV1> {
    let rest = endpoint
        .strip_prefix("https://")
        .ok_or_else(destination_refused)?;
    let (host, path) = match rest.split_once('/') {
        Some((host, path)) => (host, format!("/{path}")),
        None => (rest, "/".to_string()),
    };
    if host.is_empty() || host.contains(['@', ':']) {
        return Err(destination_refused());
    }
    let destination = ResolvedDestinationV1 {
        kind: cadence_core::connection_dispatch::dto::DestinationKindV1::Https,
        host: ident(host)?,
        port: None,
        path: Some(path),
    };
    cadence_core::connection_dispatch::dto::validate_resolved_destination_v1(&destination)
        .map_err(|_| destination_refused())?;
    Ok(destination)
}

fn invoke_response(outcome: InvokeOutcome) -> Value {
    match outcome {
        InvokeOutcome::Settled(settlement) => json!({
            "dispatch_id": settlement.receipt.dispatch_id,
            "request_id": settlement.receipt.request_id,
            "state": settlement.receipt.state,
            "receipt": settlement.receipt,
        }),
        InvokeOutcome::Duplicate {
            dispatch_id,
            request_id,
            state,
            receipt,
        } => json!({
            "dispatch_id": dispatch_id,
            "request_id": request_id,
            "state": state,
            "receipt": receipt,
        }),
    }
}

struct OpenRouterMetadata {
    entry_bytes: Vec<u8>,
    context_bound: u64,
    price: QuotePriceV1,
}

/// Fetch ONLY the configured public catalog metadata. No prompt, no
/// credential, no redirect, cookie, proxy or retry. Any surprise refuses.
fn fetch_openrouter_metadata(model: &str) -> Result<OpenRouterMetadata> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(METADATA_DEADLINE))
        .max_redirects(0)
        .http_status_as_error(false)
        .build()
        .into();
    let mut response = agent
        .get(OPENROUTER_METADATA_URL)
        .call()
        .map_err(|_| pricing_unavailable())?;
    if response.status().as_u16() != 200 {
        return Err(pricing_unavailable());
    }
    let bytes = response
        .body_mut()
        .with_config()
        .limit(METADATA_MAX_BYTES)
        .read_to_vec()
        .map_err(|_| pricing_unavailable())?;
    let root: Value = serde_json::from_slice(&bytes).map_err(|_| pricing_unavailable())?;
    let entries = root
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(pricing_unavailable)?;
    let entry = entries
        .iter()
        .find(|entry| entry.get("id").and_then(Value::as_str) == Some(model))
        .ok_or_else(pricing_unavailable)?;
    let pricing = entry
        .get("pricing")
        .and_then(Value::as_object)
        .ok_or_else(pricing_unavailable)?;
    let rate = |name: &str| -> Result<PicoUsd> {
        match pricing.get(name) {
            None | Some(Value::Null) => Ok(PicoUsd::zero()),
            Some(Value::String(text)) => {
                parse_usd_decimal_to_pico(text).map_err(|_| pricing_unavailable())
            }
            Some(_) => Err(pricing_unavailable()),
        }
    };
    let prompt = rate("prompt")?;
    let completion = rate("completion")?;
    let request = rate("request")?;
    if prompt.get() == 0 || completion.get() == 0 {
        return Err(pricing_unavailable());
    }
    let context_bound = entry
        .get("context_length")
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
        .ok_or_else(pricing_unavailable)?;
    let entry_bytes = serde_json::to_vec(entry).map_err(|_| pricing_unavailable())?;
    Ok(OpenRouterMetadata {
        entry_bytes,
        context_bound,
        price: QuotePriceV1 {
            currency: CurrencyV1::Usd,
            prompt_pico_per_token: prompt,
            completion_pico_per_token: completion,
            request_pico: request,
            reasoning_billing: ReasoningBillingV1::IncludedInCompletion,
        },
    })
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

fn refused() -> Error {
    Error::invalid(
        "storage_unavailable",
        "the protected authority is unavailable",
    )
}

fn ident(value: &str) -> Result<Ident> {
    Ident::try_from(value.to_string())
        .map_err(|_| Error::invalid("invalid_request", "a protected selector is malformed"))
}

fn digest(domain: &str, value: &Value) -> Result<Digest> {
    Digest::try_from(cadence_core::connection_dispatch::material_digest_for_domain(domain, value))
        .map_err(|_| refused())
}

fn random_uuid() -> Result<UuidV1> {
    UuidV1::try_from(uuid::Uuid::new_v4().to_string())
        .map_err(|_| Error::invalid("invalid_request", "a dispatch id is malformed"))
}

fn now_epoch_ms() -> Result<EpochMs> {
    let millis = crate::store::connection_dispatch::now_ms()?;
    EpochMs::new(millis as u64)
        .map_err(|_| Error::invalid("clock_regression", "the clock is unusable"))
}

fn runtime_binding_digest(commit: &DispatchCommitV1) -> Digest {
    Digest::try_from(
        cadence_core::connection_dispatch::material_digest_for_domain(
            "cadence-protected-runtime-binding-v1",
            &json!({
                "dispatch_id": commit.dispatch_id.as_str(),
                "connection_id": commit.connection_id.as_str(),
                "runtime_generation": commit.runtime_generation.get(),
                "enrollment_id": commit.enrollment_id.as_str(),
                "ledger_sequence": commit.ledger_sequence.get(),
            }),
        ),
    )
    .unwrap_or_else(|_| commit.ledger_digest.clone())
}

fn unknown_outcome() -> ValidatedOutcomeV1 {
    ValidatedOutcomeV1 {
        v: 1,
        kind: OutcomeKindV1::Unknown,
        usage: None,
        result: None,
        refused_code: None,
    }
}

fn refused_outcome(code: ErrorCodeV1) -> ValidatedOutcomeV1 {
    ValidatedOutcomeV1 {
        v: 1,
        kind: OutcomeKindV1::Refused,
        usage: None,
        result: None,
        refused_code: Some(code),
    }
}

fn done_verified() -> ValidatedOutcomeV1 {
    ValidatedOutcomeV1 {
        v: 1,
        kind: OutcomeKindV1::Done,
        usage: None,
        result: Some(OutcomeResultV1::SmtpVerified(SmtpVerifiedOutcomeV1 {
            kind: SmtpVerifiedKindV1::SmtpVerified,
            verified: true,
        })),
        refused_code: None,
    }
}

#[cfg(test)]
#[path = "protected_connections_epoch_acceptance_20261027.rs"]
mod protected_connections_epoch_acceptance_20261027;
