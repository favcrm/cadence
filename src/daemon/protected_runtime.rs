//! CAD-1085 r7 — the supervisor-side protected runtime coordinator.
//!
//! This is the object that ties the *supervisor* (uid 21000) to the root-owned
//! `native-oci-agent/1` authority, and it is the only place a protected Pi
//! runtime is started, attached and stopped:
//!
//! 1. [`prepare_store`] runs **before** any boot-time authorizing write. It
//!    validates the root-owned profile/public-private split, checks the durable
//!    core identity against the profile (one truth, never a second), and
//!    installs the fail-closed owner checkpoint writer. A missing owner keeps
//!    metadata readable while protected *success* stays impossible.
//! 2. [`initialize`] runs after the `Arc<Shared>` exists. It starts the
//!    supervisor-owned grant listener and broker-fence listener, fences any
//!    runtime that a previous supervisor generation left behind (a restart
//!    discards transient witnesses), and installs the coordinator as both the
//!    `ProtectedConnections` owner client and the per-runtime operations router.
//! 3. [`ProtectedRuntimeCoordinator::open_console_for_agent`] performs the r5
//!    cold bootstrap for one agent — `prepare_runtime` (owner allocates and
//!    launches the pinned installer), bind the operations listener, register
//!    the prepared intent, wait for the installer's one-use consume, `start`,
//!    wait for the measured observer, `recheck`, then the bounded owned-console
//!    attachment. Nothing here invents readiness: a phase that is not `ready`
//!    is a refusal.
//! 4. Every operation caller is matched to the exact registered closed handle
//!    (runtime id + generation + stable uid + session + OS uid); the model-turn
//!    methods additionally require the measured Node pid/starttime. Multiple
//!    agents, runtimes and generations cannot cross.
//!
//! There is no host same-UID fallback, no caller-selected PID/URL/command, no
//! generic signer and no unprotected "managed Pi" path under this profile: a
//! failed protected bootstrap refuses.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock, Weak};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::protected_runtime::console::{self, OwnerConsole};
use crate::protected_runtime::ops::{
    AgentEnrollment, OpsFamily, OpsMethod, RuntimeCaller, RuntimeOpsHandler, RuntimeOpsListener,
};
use crate::protected_runtime::{
    attestation, CurrentEligibility, LocalProfile, RootOwnerClient, RuntimeStatus,
};
use cadence_core::connection_dispatch::ledger::{CheckpointAckV1, CheckpointV1, LedgerRecordV1};
use cadence_core::connection_dispatch::primitives::{Counter, Digest, Revision};

use crate::store::{
    connection_dispatch::{BootAuthority, ExternalFloorV1},
    Store,
};

use super::protected_connections::{OwnerEligibility, ProtectedOwnerClient};
use super::supervisor_grant::{BrokerFenceListener, BrokerFenceTarget, GrantCore, GrantListener};
use super::Shared;

/// How long the coordinator waits for the installer's one-use consume and for
/// the measured observer to reach `ready`. Both stages are asynchronous
/// (installer process / guest registry) but absolutely bounded.
const CONSUME_BUDGET: Duration = Duration::from_secs(20);
const READY_BUDGET: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(100);

/// Upper bound on the selector registry file.
const MAX_REGISTRY_BYTES: u64 = 1024 * 1024;

// ---------------------------------------------------------------------------
// Live owner channel
// ---------------------------------------------------------------------------

/// The authenticated owner channel plus the process-local supervisor
/// generation. A lost/ambiguous exchange drops the client so the *next* call
/// performs a fresh `hello_v1`; mutations are never retried here.
struct OwnerLink {
    profile: LocalProfile,
    generation: String,
    client: RwLock<Option<Arc<RootOwnerClient>>>,
}

impl OwnerLink {
    fn new(profile: LocalProfile, generation: String) -> Arc<Self> {
        Arc::new(Self {
            profile,
            generation,
            client: RwLock::new(None),
        })
    }

    /// The live authenticated owner client, connecting on first use. The owner
    /// being down is an error — never a synthesized epoch or readiness.
    fn client(&self) -> Result<Arc<RootOwnerClient>> {
        if let Some(client) = self
            .client
            .read()
            .ok()
            .and_then(|guard| guard.as_ref().cloned())
        {
            return Ok(client);
        }
        let client = Arc::new(RootOwnerClient::connect(
            &self.profile,
            self.generation.clone(),
        )?);
        if let Ok(mut guard) = self.client.write() {
            *guard = Some(Arc::clone(&client));
        }
        Ok(client)
    }

    fn invalidate(&self) {
        if let Ok(mut guard) = self.client.write() {
            *guard = None;
        }
    }

    /// A read-only exchange, retried ONCE after a transport/epoch failure.
    /// Reads are idempotent; mutations use [`Self::client`] directly and are
    /// never replayed.
    fn read<T>(&self, f: impl Fn(&RootOwnerClient) -> Result<T>) -> Result<T> {
        let client = self.client()?;
        match f(&client) {
            Ok(value) => Ok(value),
            Err(first) => {
                self.invalidate();
                let fresh = self.client()?;
                f(&fresh).map_err(|second| {
                    if matches!(second.kind(), "unknown") {
                        Error::unknown(format!(
                            "owner recheck failed after reconnect: {} / {}",
                            first.kind(),
                            second.kind()
                        ))
                    } else {
                        second
                    }
                })
            }
        }
    }
}

struct OwnerCheckpoint {
    link: Arc<OwnerLink>,
}

impl crate::store::connection_dispatch::ProtectedCheckpointWriter for OwnerCheckpoint {
    fn context(
        &self,
        workspace_id: &str,
        database_epoch: Revision,
    ) -> Result<crate::store::connection_dispatch::ProtectedCheckpointContext> {
        let witness = self.link.read(|client| client.prestart_eligibility())?;
        if witness.workspace_id != workspace_id
            || witness.database_epoch != database_epoch.get()
            || witness.restore_state != "stable"
        {
            return Err(Error::invalid(
                "runtime_unqualified",
                "the live owner does not authorize this core history",
            ));
        }
        Ok(
            crate::store::connection_dispatch::ProtectedCheckpointContext {
                owner_epoch: Revision::new(witness.owner_epoch).map_err(|_| {
                    Error::invalid("runtime_unqualified", "the owner epoch is malformed")
                })?,
                minimum_sequence: Counter::new(witness.min_sequence).map_err(|_| {
                    Error::invalid("runtime_unqualified", "the owner minimum is malformed")
                })?,
                minimum_digest: Digest::try_from(witness.min_digest).map_err(|_| {
                    Error::invalid("runtime_unqualified", "the owner minimum is malformed")
                })?,
            },
        )
    }

    /// Called AFTER the writer transaction commits and the DB lock is released.
    /// A lost acknowledgement is `outcome_unknown`; there is no replay and no
    /// callback retry. The transport `runtime_generation` field is not
    /// authority (the owner validates the canonical checkpoint, which binds the
    /// exact runtime generation through `runtime_binding_digest`), so the fixed
    /// positive carrier value is sent and nothing more is claimed.
    fn checkpoint(
        &self,
        checkpoint: &CheckpointV1,
        prefix: &[LedgerRecordV1],
    ) -> Result<CheckpointAckV1> {
        let client = self.link.client()?;
        client.checkpoint(checkpoint, prefix, 1)
    }

    fn acknowledged_head(&self, checkpoint: &CheckpointV1) -> Result<CheckpointAckV1> {
        self.link.client()?.checkpoint_receipt(checkpoint)
    }
}

// ---------------------------------------------------------------------------
// Prepared protection (before boot-time authorizing writes)
// ---------------------------------------------------------------------------

/// The validated profile + owner channel produced by [`prepare_store`] and
/// consumed by [`initialize`]. Private fields; nothing here is caller built.
pub(crate) struct PreparedProtection {
    profile: LocalProfile,
    supervisor_generation: String,
    owner: Arc<OwnerLink>,
}

/// What a protected boot may do after the store is open but before recovery.
/// `MetadataOnly` keeps encrypted metadata readable and refuses recovery and
/// protected admission; it is never a silent legacy/permit upgrade.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PreparedBoot {
    /// The owner's external witness matches this profile and the local ledger
    /// contains the external floor: guarded recovery may run with this epoch.
    Ready {
        owner_epoch: u64,
        floor_sequence: u64,
        floor_digest: String,
    },
    /// Readable metadata only. No recovery, no protected success. `code` is a
    /// fixed closed reason.
    MetadataOnly { code: &'static str },
}

/// Read the authenticated external floor before taking the store's read lock.
/// Management may establish enablement; execution must already have it.
fn current_owner_authority(
    store: &Store,
    profile: &LocalProfile,
    owner: &OwnerLink,
    require_enabled: bool,
) -> Result<(
    crate::protected_runtime::client::PrestartEligibility,
    BootAuthority,
)> {
    let witness = owner.client()?.prestart_eligibility()?;
    if witness.workspace_id != profile.workspace_id()
        || witness.database_epoch != profile.database_epoch()
        || witness.restore_state != "stable"
    {
        return Err(Error::invalid(
            "runtime_unqualified",
            "the live owner does not authorize this database lineage",
        ));
    }
    let floor = ExternalFloorV1 {
        workspace_id: witness.workspace_id.clone(),
        min_sequence: witness.min_sequence,
        min_digest: Digest::try_from(witness.min_digest.clone())
            .map_err(|_| Error::invalid("runtime_unqualified", "the owner floor is malformed"))?,
    };
    let authority = if require_enabled {
        store.protected_boot_preflight(&floor)?
    } else {
        store.protected_history_preflight(&floor)?
    };
    Ok((witness, authority))
}

impl PreparedProtection {
    /// Decide the boot authority from the authenticated owner witness and the
    /// actual local ledger. A floor of 0 (fresh deployment) is `Ready` for an
    /// empty local chain: that is how a truthful human initial-enable happens
    /// without a chicken/egg owner-less state. It never treats a mismatch or a
    /// missing owner as `Ready`.
    pub(crate) fn boot_authority(&self, store: &Store) -> PreparedBoot {
        match current_owner_authority(store, &self.profile, &self.owner, false) {
            Ok((witness, BootAuthority::Ready)) => PreparedBoot::Ready {
                owner_epoch: witness.owner_epoch,
                floor_sequence: witness.min_sequence,
                floor_digest: witness.min_digest,
            },
            Ok((_, BootAuthority::MetadataOnly(code))) => PreparedBoot::MetadataOnly { code },
            Err(_) => PreparedBoot::MetadataOnly {
                code: "owner_unavailable",
            },
        }
    }

    /// The authenticated prestart witness / ledger floor for the protected
    /// store open and recovery. It must match this profile's workspace and
    /// lineage epoch, and the owner must not be restoring/fenced.
    pub(crate) fn prestart_eligibility(
        &self,
    ) -> Result<crate::protected_runtime::client::PrestartEligibility> {
        let witness = self.owner.client()?.prestart_eligibility()?;
        if witness.workspace_id != self.profile.workspace_id()
            || witness.database_epoch != self.profile.database_epoch()
            || witness.restore_state != "stable"
        {
            return Err(Error::rejected(
                "owner prestart witness does not match this protected profile/lineage",
            ));
        }
        Ok(witness)
    }
}

/// Validate the operator-provisioned protected profile and install the
/// fail-closed checkpoint writer before boot-time authorizing writes.
///
/// `None` is the explicit, labeled ordinary daemon: no profile, no protected
/// authority claim, and the pre-existing unprotected native behavior keeps its
/// own separate label. `Some(path)` must be the root-owned profile; a profile
/// that exists but cannot be validated refuses startup rather than silently
/// running unprotected.
pub(crate) fn prepare_profile(profile_path: Option<&Path>) -> Result<Option<PreparedProtection>> {
    let Some(path) = profile_path else {
        // Ordinary mode is valid ONLY when no protected deployment is
        // configured. A missing profile while running as the reserved
        // supervisor uid or beside a root-owned owner tree is a downgrade
        // attempt, not an ordinary daemon: refuse.
        if reserved_supervisor_process() {
            return Err(Error::rejected(
                "a configured protected deployment has no root-owned profile — ordinary mode refuses",
            ));
        }
        return Ok(None);
    };
    let profile = LocalProfile::load_from(path)?;
    // This process must be the pinned supervisor.
    let facts = crate::protected_runtime::local_supervisor_facts(&profile)?;
    validate_private_layout(&profile)?;
    let owner = OwnerLink::new(profile.clone(), facts.generation.clone());
    // Eager connect is best effort: the checkpoint writer (not startup) stays
    // fail-closed until the owner is live, so encrypted metadata remains
    // readable in the meantime.
    let _ = owner.client();
    Ok(Some(PreparedProtection {
        profile,
        supervisor_generation: facts.generation,
        owner,
    }))
}

/// Install the protected store gate BEFORE any authorizing/recovery write.
///
/// Must run on an unrecovered permitted coordination store. It does not enable
/// the separate sealed-store `OpenMode::Protected` constructor. It verifies
/// durable core identity and installs the checkpoint writer.
/// Until the owner channel is live the writer refuses; metadata reads stay
/// available and an authorizing success is impossible.
pub(crate) fn install_store_gate(
    store: &Store,
    prepared: &PreparedProtection,
) -> Result<PreparedBoot> {
    validate_core_identity(store, &prepared.profile)?;
    store.install_protected_checkpoint(Arc::new(OwnerCheckpoint {
        link: Arc::clone(&prepared.owner),
    }))?;
    // The composite gate: the permitted latchless Legacy store stays
    // `protected=false`; real protected admission requires this live owner
    // witness + the ledger floor, decided before any guarded recovery.
    Ok(prepared.boot_authority(store))
}

/// The kickoff-named combined hook: validate the operator-provisioned profile
/// and install the fail-closed store gate. `None` is the explicit, labeled
/// ordinary daemon (nothing installed). An active profile that cannot be
/// validated refuses rather than silently running unprotected.
pub(crate) fn prepare_store(
    store: &Store,
    profile_path: Option<&Path>,
) -> Result<Option<PreparedProtection>> {
    let prepared = prepare_profile(profile_path)?;
    if let Some(prepared) = prepared.as_ref() {
        let _boot = install_store_gate(store, prepared)?;
    }
    Ok(prepared)
}

/// The compiled-in root-owned profile, if one is present. Absent means the
/// explicit ordinary daemon; present-but-invalid is refused by
/// [`prepare_profile`], never treated as absent. No environment variable, PM
/// row or model field can redirect this path.
pub(crate) fn default_profile_path() -> Result<Option<PathBuf>> {
    match std::fs::symlink_metadata(crate::protected_runtime::PROFILE_PATH) {
        Ok(_) => Ok(Some(PathBuf::from(crate::protected_runtime::PROFILE_PATH))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(Error::rejected(
            "the protected profile path cannot be inspected",
        )),
    }
}

/// The private authority subtree and the supervisor/owner socket directories
/// must have the exact reviewed shape. Never weakens a private mode.
fn validate_private_layout(profile: &LocalProfile) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let state = profile.state_dir();
    let meta = std::fs::symlink_metadata(state)
        .map_err(|_| Error::rejected("owner state directory is missing"))?;
    if !meta.is_dir() || meta.is_symlink() {
        return Err(Error::rejected(
            "owner state directory must be a real directory",
        ));
    }
    if meta.uid() != 0 || meta.mode() & 0o077 != 0 {
        return Err(Error::rejected(
            "owner state directory must be root-owned 0700",
        ));
    }
    let run = profile.run_dir();
    let meta = std::fs::symlink_metadata(run)
        .map_err(|_| Error::rejected("owner run directory is missing"))?;
    if !meta.is_dir() || meta.is_symlink() {
        return Err(Error::rejected(
            "owner run directory must be a real directory",
        ));
    }
    if meta.uid() != 0 || meta.gid() != profile.supervisor().gid {
        return Err(Error::rejected(
            "owner run directory must be root-owned in the supervisor group",
        ));
    }
    let mode = meta.mode() & 0o777;
    if mode & 0o007 != 0 || mode & 0o200 == 0 {
        return Err(Error::rejected(
            "owner run directory must be a root-owned supervisor-writable 0770",
        ));
    }
    Ok(())
}

/// The durable core identity (workspace + database lineage epoch) is the single
/// truth. When it is already bound, the profile must equal it; an empty binding
/// is not silently replaced here — the store path owns that binding.
fn validate_core_identity(store: &Store, profile: &LocalProfile) -> Result<()> {
    let authority = store.protected_authority()?;
    if !authority.workspace_id.is_empty() && authority.workspace_id != profile.workspace_id() {
        return Err(Error::rejected(
            "protected profile workspace does not match the durable core identity",
        ));
    }
    if authority.database_epoch.get() != profile.database_epoch() {
        return Err(Error::rejected(
            "protected profile database epoch does not match the durable core identity",
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Registered runtime handles
// ---------------------------------------------------------------------------

/// One closed, non-serializable runtime binding. Every field is server-derived
/// from the owner's own allocation/measurement; a caller field never creates
/// one.
#[derive(Clone, Debug)]
struct RuntimeHandle {
    alias: String,
    role: String,
    agent_uid: String,
    session_binding: String,
    runtime_id: String,
    operation_id: String,
    generation: u64,
    server_generation: u64,
    owner_epoch: u64,
    os_uid: u32,
    node_pid: u32,
    node_starttime: u64,
    recipient_nonce: String,
    container_id: String,
    loaded_receipt_id: String,
    observed_registry_digest: String,
    /// Not serialized: the accept-thread stop flag and socket path for THIS
    /// process's listener. A restarted supervisor re-derives nothing from it.
    stop: Arc<AtomicBool>,
    ops_path: PathBuf,
}

impl RuntimeHandle {
    fn matches(&self, caller: &RuntimeCaller) -> bool {
        caller.runtime_id() == self.runtime_id
            && caller.runtime_generation() == self.generation
            && caller.agent_uid() == self.agent_uid
            && caller.session_binding() == self.session_binding
            && caller.os_uid() == self.os_uid
    }

    /// Only the measured guest Node process may use the native model-turn
    /// methods; tool children share the uid but are a different pid.
    fn is_measured_node(&self, caller: &RuntimeCaller) -> bool {
        caller.peer_pid() == self.node_pid && caller.peer_starttime() == self.node_starttime
    }

    fn to_json(&self) -> Value {
        json!({
            "v": 1,
            "alias": self.alias,
            "role": self.role,
            "agent_uid": self.agent_uid,
            "session_binding": self.session_binding,
            "runtime_id": self.runtime_id,
            "operation_id": self.operation_id,
            "generation": self.generation,
            "server_generation": self.server_generation,
            "owner_epoch": self.owner_epoch,
            "os_uid": self.os_uid,
            "node_pid": self.node_pid,
            "node_starttime": self.node_starttime,
            "recipient_nonce": self.recipient_nonce,
            "container_id": self.container_id,
            "loaded_receipt_id": self.loaded_receipt_id,
            "observed_registry_digest": self.observed_registry_digest,
        })
    }

    fn from_json(value: &Value) -> Result<Self> {
        let m = crate::protected_runtime::strict::object(
            value,
            &[
                "v",
                "alias",
                "role",
                "agent_uid",
                "session_binding",
                "runtime_id",
                "operation_id",
                "generation",
                "server_generation",
                "owner_epoch",
                "os_uid",
                "node_pid",
                "node_starttime",
                "recipient_nonce",
                "container_id",
                "loaded_receipt_id",
                "observed_registry_digest",
            ],
        )?;
        let s = |key: &str| crate::protected_runtime::strict::string(m, key).map(str::to_string);
        Ok(Self {
            alias: s("alias")?,
            role: s("role")?,
            agent_uid: s("agent_uid")?,
            session_binding: s("session_binding")?,
            runtime_id: s("runtime_id")?,
            operation_id: s("operation_id")?,
            generation: crate::protected_runtime::strict::positive_u64(m, "generation")?,
            server_generation: crate::protected_runtime::strict::positive_u64(
                m,
                "server_generation",
            )?,
            owner_epoch: crate::protected_runtime::strict::positive_u64(m, "owner_epoch")?,
            os_uid: u32::try_from(crate::protected_runtime::strict::positive_u64(m, "os_uid")?)
                .map_err(|_| Error::rejected("registered runtime uid is out of range"))?,
            node_pid: u32::try_from(crate::protected_runtime::strict::positive_u64(
                m, "node_pid",
            )?)
            .map_err(|_| Error::rejected("registered runtime pid is out of range"))?,
            node_starttime: crate::protected_runtime::strict::positive_u64(m, "node_starttime")?,
            recipient_nonce: s("recipient_nonce")?,
            container_id: s("container_id")?,
            loaded_receipt_id: s("loaded_receipt_id")?,
            observed_registry_digest: s("observed_registry_digest")?,
            stop: Arc::new(AtomicBool::new(true)),
            ops_path: PathBuf::new(),
        })
    }
}

/// Every incomplete allocation is fenced, including early bind/transport
/// errors. Success disarms this only after the console and registry attach.
struct RuntimeLaunchGuard<'a> {
    coordinator: &'a ProtectedRuntimeCoordinator,
    prepared: &'a crate::protected_runtime::types::PreparedRuntime,
    armed: bool,
}

impl Drop for RuntimeLaunchGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.coordinator.fence_id(
                &self.prepared.runtime_id,
                &self.prepared.operation_id,
                self.prepared.generation,
                "runtime bootstrap did not complete",
            );
        }
    }
}

/// Tears down a per-runtime operations listener unless the runtime reached its
/// final attached state. Every early `?` after `bind` therefore stops the
/// accept thread and removes the socket.
struct OpsListenerGuard {
    stop: Arc<AtomicBool>,
    path: PathBuf,
    armed: bool,
}

impl Drop for OpsListenerGuard {
    fn drop(&mut self) {
        if self.armed {
            self.stop.store(true, Ordering::SeqCst);
            use std::os::unix::fs::FileTypeExt;
            if let Ok(meta) = std::fs::symlink_metadata(&self.path) {
                if meta.file_type().is_socket() {
                    let _ = std::fs::remove_file(&self.path);
                }
            }
        }
    }
}

/// The typed operations router: the guest's single socket routes to the real
/// backend for the family the method belongs to. The caller identity is the
/// listener-derived [`RuntimeCaller`]; nothing in `args` can change it.
pub(crate) struct RuntimeOpsRouter {
    shared: Weak<Shared>,
}

impl RuntimeOpsHandler for RuntimeOpsRouter {
    fn handle(&self, caller: &RuntimeCaller, method: OpsMethod, args: &Value) -> Result<Value> {
        let shared = self
            .shared
            .upgrade()
            .ok_or_else(|| Error::invalid("runtime_unqualified", "the daemon is shutting down"))?;
        match method.family() {
            OpsFamily::Connection => shared.handle_runtime(caller, method, args),
            OpsFamily::NativeModel => shared.native_model()?.handle_runtime(caller, method, args),
        }
    }
}

// ---------------------------------------------------------------------------
// The coordinator
// ---------------------------------------------------------------------------

/// The supervisor-side coordinator. Held once per daemon through
/// `Shared::protected_runtime`; it keeps only a `Weak<Shared>` (no Arc cycle)
/// and the authenticated owner channel.
pub(crate) struct ProtectedRuntimeCoordinator {
    shared: Weak<Shared>,
    profile: LocalProfile,
    supervisor_generation: String,
    owner: Arc<OwnerLink>,
    state_dir: PathBuf,
    registry: Mutex<BTreeMap<String, RuntimeHandle>>,
    grant: Mutex<Option<GrantCore>>,
    grant_error: Mutex<Option<String>>,
    router: Arc<RuntimeOpsRouter>,
    eligibility: CurrentEligibility,
    closing: AtomicBool,
}

/// The attachment returned for one agent: the private console plus the stable
/// generation tag the adapter stamps on its events, and the closed caller
/// identity the adapter binds a durable model task to.
pub(crate) struct AttachedConsole {
    pub(crate) console: OwnerConsole,
    pub(crate) generation_tag: String,
    pub(crate) runtime_id: String,
    pub(crate) caller: RuntimeCaller,
}

/// The process-global console provider. The protected coordinator is a
/// process-global authority (one daemon per process); the adapter reaches it
/// without threading a new field through every adapter constructor.
static CONSOLE_PROVIDER: OnceLock<Weak<dyn ProtectedConsoleProvider>> = OnceLock::new();

/// One agent's closed console subject. Values come from the durable agent row
/// the daemon already holds; nothing here is a request field.
pub(crate) struct ConsoleSubject<'a> {
    pub(crate) alias: &'a str,
    pub(crate) agent_uid: &'a str,
    pub(crate) created: f64,
}

/// A held native-model task binding. Dropping it ends the binding; `fence`
/// stops new preparation for the turn when it settles or is interrupted.
pub(crate) trait NativeTaskGuard: Send {
    fn fence(&self, reason: &str);
}

/// The adapter-facing contract implemented by the coordinator.
pub(crate) trait ProtectedConsoleProvider: Send + Sync {
    fn open_console(
        &self,
        subject: ConsoleSubject<'_>,
        stderr_log: &Path,
    ) -> Result<AttachedConsole>;

    fn bind_native_task(
        &self,
        caller: &RuntimeCaller,
        client_message_id: &str,
        server_message_id: &str,
        turn_token: &str,
    ) -> Result<Box<dyn NativeTaskGuard>>;

    /// Stop exactly this agent's owned runtime. Best effort from a close path;
    /// the owner fences first and only retires its own labelled container.
    fn stop_agent(&self, agent_uid: &str, reason: &str);
}

/// Install the process-global provider (called once from [`initialize`]).
fn install_console_provider(coordinator: &Arc<ProtectedRuntimeCoordinator>) -> Result<()> {
    let provider: Arc<dyn ProtectedConsoleProvider> = coordinator.clone();
    let weak: Weak<dyn ProtectedConsoleProvider> = Arc::downgrade(&provider);
    CONSOLE_PROVIDER
        .set(weak)
        .map_err(|_| Error::internal("the protected console provider is already installed"))
}

/// The live provider, or a refusal. An active protected profile without a
/// provider refuses — it never falls back to a host process.
pub(crate) fn console_provider() -> Result<Arc<dyn ProtectedConsoleProvider>> {
    CONSOLE_PROVIDER
        .get()
        .and_then(|weak| weak.upgrade())
        .ok_or_else(|| {
            Error::invalid(
                "runtime_unqualified",
                "no protected runtime coordinator is installed",
            )
        })
}

impl ProtectedRuntimeCoordinator {
    fn start(shared: &Arc<Shared>, prepared: PreparedProtection) -> Result<Arc<Self>> {
        let coordinator = Arc::new(Self {
            shared: Arc::downgrade(shared),
            profile: prepared.profile,
            supervisor_generation: prepared.supervisor_generation,
            owner: prepared.owner,
            state_dir: shared.state_dir.clone(),
            registry: Mutex::new(BTreeMap::new()),
            grant: Mutex::new(None),
            grant_error: Mutex::new(None),
            router: Arc::new(RuntimeOpsRouter {
                shared: Arc::downgrade(shared),
            }),
            eligibility: CurrentEligibility::new(),
            closing: AtomicBool::new(false),
        });
        coordinator.start_broker_fence();
        coordinator.start_grant_listener();
        // A supervisor restart discards transient witnesses: a runtime the
        // previous generation started is fenced (its operations socket died
        // with that process), then forgotten. Nothing is adopted.
        coordinator.fence_previous_runtimes();
        Ok(coordinator)
    }

    pub(crate) fn profile_digest(&self) -> &str {
        self.profile.digest()
    }

    fn start_broker_fence(self: &Arc<Self>) {
        let listener = match BrokerFenceListener::listen_production(&self.profile) {
            Ok(listener) => listener,
            Err(e) => {
                self.emit_error("broker fence listener unavailable", Some(e.to_string()));
                return;
            }
        };
        let target: Arc<dyn BrokerFenceTarget> = Arc::clone(self) as Arc<dyn BrokerFenceTarget>;
        let listener = Arc::new(listener);
        std::thread::spawn(move || listener.serve(target));
    }

    fn start_grant_listener(self: &Arc<Self>) {
        let profile = self.profile.clone();
        let listener = match GrantListener::listen_production() {
            Ok(listener) => listener,
            Err(e) => {
                if let Ok(mut slot) = self.grant_error.lock() {
                    *slot = Some(e.to_string());
                }
                self.emit_error("grant listener unavailable", Some(e.to_string()));
                return;
            }
        };
        let core = match GrantCore::production_local(&profile) {
            Ok(core) => core,
            Err(e) => {
                if let Ok(mut slot) = self.grant_error.lock() {
                    *slot = Some(e.to_string());
                }
                self.emit_error("grant core unavailable", Some(e.to_string()));
                return;
            }
        };
        if let Ok(mut slot) = self.grant.lock() {
            *slot = Some(core);
        }
        let me = Arc::clone(self);
        std::thread::spawn(move || {
            while let Ok(stream) = listener.accept_production() {
                let Ok(mut guard) = me.grant.lock() else {
                    return;
                };
                let Some(core) = guard.as_mut() else {
                    return;
                };
                let _ = listener.serve_accepted(&stream, core);
            }
        });
    }

    /// A supervisor restart discards transient witnesses: every runtime a
    /// previous generation started is fenced (its operations socket died with
    /// that process) and its selector is dropped. A failed fence is retried in
    /// the background; the selector never becomes a reusable handle.
    fn fence_previous_runtimes(self: &Arc<Self>) {
        let entries = self.load_registry_file();
        if entries.is_empty() {
            return;
        }
        self.persist_registry();
        let me = Arc::clone(self);
        std::thread::spawn(move || {
            let mut pending = entries;
            for _ in 0..120 {
                if me.closing.load(Ordering::SeqCst) {
                    return;
                }
                pending.retain(|handle| me.fence_handle(handle, "supervisor restart").is_err());
                if pending.is_empty() {
                    return;
                }
                std::thread::sleep(Duration::from_secs(5));
            }
        });
    }

    fn load_registry_file(&self) -> Vec<RuntimeHandle> {
        let path = self.registry_path();
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            return Vec::new();
        };
        use std::os::unix::fs::MetadataExt;
        if !meta.is_file() || meta.is_symlink() || meta.len() > MAX_REGISTRY_BYTES {
            return Vec::new();
        }
        if meta.mode() & 0o077 != 0 || meta.uid() != unsafe { libc::geteuid() } {
            return Vec::new();
        }
        let Ok(raw) = std::fs::read(&path) else {
            return Vec::new();
        };
        let Ok(value) = serde_json::from_slice::<Value>(&raw) else {
            return Vec::new();
        };
        let Some(entries) = value.get("entries").and_then(Value::as_array) else {
            return Vec::new();
        };
        entries
            .iter()
            .filter_map(|entry| RuntimeHandle::from_json(entry).ok())
            .collect()
    }

    fn registry_path(&self) -> PathBuf {
        self.state_dir
            .join("protected-runtime")
            .join("runtime-registry.json")
    }

    fn persist_registry(&self) {
        let path = self.registry_path();
        let Some(parent) = path.parent() else { return };
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
        let entries: Vec<Value> = self
            .registry
            .lock()
            .map(|guard| guard.values().map(RuntimeHandle::to_json).collect())
            .unwrap_or_default();
        let document = json!({"v": 1, "entries": entries});
        let Ok(bytes) = serde_json::to_vec(&document) else {
            return;
        };
        let temporary = path.with_extension("tmp");
        if std::fs::write(&temporary, &bytes).is_err() {
            return;
        }
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600));
        let _ = std::fs::rename(&temporary, &path);
    }

    /// Operator-visible startup diagnostic. There is no event channel for a
    /// pre-ready failure, and a silent refusal would be misleading.
    fn emit_error(&self, what: &str, detail: Option<String>) {
        match detail {
            Some(detail) => eprintln!("cadence: protected runtime: {what}: {detail}"),
            None => eprintln!("cadence: protected runtime: {what}"),
        }
    }

    fn handle_for_caller(&self, caller: &RuntimeCaller) -> Result<RuntimeHandle> {
        let guard = self
            .registry
            .lock()
            .map_err(|_| Error::internal("runtime registry lock poisoned"))?;
        guard
            .values()
            .find(|handle| handle.matches(caller))
            .cloned()
            .ok_or_else(|| {
                Error::invalid(
                    "runtime_unqualified",
                    "the caller is not a registered protected runtime",
                )
            })
    }

    fn handle_for_uid(&self, agent_uid: &str) -> Option<RuntimeHandle> {
        self.registry
            .lock()
            .ok()
            .and_then(|guard| guard.get(agent_uid).cloned())
    }

    fn insert_handle(&self, handle: RuntimeHandle) {
        if let Ok(mut guard) = self.registry.lock() {
            guard.insert(handle.agent_uid.clone(), handle);
        }
        self.persist_registry();
    }

    fn remove_handle(&self, agent_uid: &str) {
        if let Ok(mut guard) = self.registry.lock() {
            guard.remove(agent_uid);
        }
        self.persist_registry();
    }

    /// Open (or reuse) this agent's owned runtime console.
    ///
    /// `stderr_log` receives the bounded private stderr projection; it is
    /// diagnostics, never protocol.
    pub(crate) fn open_console_subject(
        &self,
        alias: &str,
        agent_uid: &str,
        created: f64,
        stderr_log: &Path,
    ) -> Result<AttachedConsole> {
        if self.closing.load(Ordering::SeqCst) {
            return Err(Error::invalid(
                "runtime_unqualified",
                "the protected runtime coordinator is closing",
            ));
        }
        if !is_stable_agent_uid(agent_uid) {
            return Err(Error::rejected(
                "the agent has no stable protected participant uid",
            ));
        }
        let shared = self
            .shared
            .upgrade()
            .ok_or_else(|| Error::invalid("runtime_unqualified", "the daemon is shutting down"))?;
        let (_, authority) =
            current_owner_authority(&shared.store, &self.profile, &self.owner, true)?;
        if let BootAuthority::MetadataOnly(code) = authority {
            return Err(Error::invalid(code, "protected execution is unavailable"));
        }
        // One runtime per agent: a still-ready handle is reused only after a
        // fresh live recheck; anything else is fenced first.
        if let Some(handle) = self.handle_for_uid(agent_uid) {
            if let Ok(console) = self.attach_existing(&handle, stderr_log) {
                return Ok(console);
            }
            let _ = self.fence_handle(&handle, "reopen after a failed recheck");
            self.remove_handle(agent_uid);
        }
        self.start_runtime_for(alias, agent_uid, created, stderr_log)
    }

    fn attach_existing(
        &self,
        handle: &RuntimeHandle,
        stderr_log: &Path,
    ) -> Result<AttachedConsole> {
        let status = self
            .owner
            .read(|client| client.observe(&handle.runtime_id))?;
        if status.phase != "ready" || !status.qualified || status.fenced_reason.is_some() {
            return Err(Error::rejected("registered runtime is not live"));
        }
        if status.agent_uid != handle.agent_uid
            || status.session_binding != handle.session_binding
            || status.generation != handle.generation
            || status.os_uid != handle.os_uid
        {
            return Err(Error::rejected(
                "registered runtime no longer names this exact subject",
            ));
        }
        let qualified = self
            .owner
            .read(|client| client.recheck(&handle.runtime_id, handle.generation))?;
        self.eligibility.refresh(qualified)?;
        self.attach_console(handle, stderr_log)
    }

    fn attach_console(&self, handle: &RuntimeHandle, stderr_log: &Path) -> Result<AttachedConsole> {
        let client = self.owner.client()?;
        let open = client.open_console(
            &handle.runtime_id,
            handle.generation,
            &handle.recipient_nonce,
        )?;
        if open.node_pid != handle.node_pid || open.node_starttime != handle.node_starttime {
            return Err(Error::rejected(
                "owner console names a different measured guest",
            ));
        }
        let console = console::connect(
            &open.path,
            &open.token,
            &handle.runtime_id,
            handle.generation,
            open.node_pid,
            Some(stderr_log.to_path_buf()),
        )?;
        Ok(AttachedConsole {
            console,
            generation_tag: format!("{}-{}", handle.runtime_id, handle.generation),
            runtime_id: handle.runtime_id.clone(),
            caller: RuntimeCaller::registered(
                handle.runtime_id.clone(),
                handle.agent_uid.clone(),
                handle.session_binding.clone(),
                handle.generation,
                handle.os_uid,
                handle.node_pid,
                handle.node_starttime,
            ),
        })
    }

    /// The complete r5 cold bootstrap for one agent. Any stage that does not
    /// reach its measured terminal state fences the obligation and refuses.
    fn start_runtime_for(
        &self,
        alias: &str,
        agent_uid: &str,
        created: f64,
        stderr_log: &Path,
    ) -> Result<AttachedConsole> {
        if self.grant.lock().map(|slot| slot.is_none()).unwrap_or(true) {
            let reason = self
                .grant_error
                .lock()
                .ok()
                .and_then(|guard| guard.clone())
                .unwrap_or_else(|| "the supervisor grant listener is not installed".to_string());
            return Err(Error::invalid("runtime_unqualified", reason));
        }
        let client = self.owner.client()?;
        let session_binding = random_hex32()?;
        let server_generation = server_generation_for(created)?;
        let prepared = client
            .prepare_runtime(&crate::protected_runtime::client::PrepareRequest {
                stable_agent_uid: agent_uid.to_string(),
                session_binding: session_binding.clone(),
                server_generation,
                expected_owner_epoch: client.owner_epoch(),
            })
            .inspect_err(|_| {
                self.owner.invalidate();
            })?;
        let mut launch_guard = RuntimeLaunchGuard {
            coordinator: self,
            prepared: &prepared,
            armed: true,
        };
        // Bind the one per-runtime operations socket BEFORE start: the owner
        // seals this exact listening socket at start_runtime.
        let listener = RuntimeOpsListener::bind(
            &prepared.ops_socket,
            AgentEnrollment {
                runtime_id: prepared.runtime_id.clone(),
                agent_uid: agent_uid.to_string(),
                session_binding: session_binding.clone(),
                runtime_generation: prepared.generation,
                os_uid: prepared.os_uid,
            },
        )?;
        let stop = Arc::new(AtomicBool::new(false));
        let mut ops_guard = OpsListenerGuard {
            stop: Arc::clone(&stop),
            path: prepared.ops_socket.clone(),
            armed: true,
        };
        {
            let handler: Arc<dyn RuntimeOpsHandler> =
                Arc::clone(&self.router) as Arc<dyn RuntimeOpsHandler>;
            let stop_flag = Arc::clone(&stop);
            std::thread::spawn(move || {
                let _ = listener.serve_managed(handler, stop_flag);
            });
        }
        // Register the exact prepared intent with the supervisor's grant core;
        // the owner-launched installer then performs the one-use challenge and
        // install conversation on the private listener. The core lock is
        // released BEFORE waiting: the listener thread needs it to serve that
        // very conversation.
        {
            let mut guard = self
                .grant
                .lock()
                .map_err(|_| Error::internal("grant core lock poisoned"))?;
            let grant = guard.as_mut().ok_or_else(|| {
                Error::invalid("runtime_unqualified", "the grant core is not installed")
            })?;
            grant.register_prepared(&prepared)?;
        }
        client.release_installer(
            &prepared.runtime_id,
            &prepared.operation_id,
            prepared.generation,
        )?;
        self.wait_for_install(&prepared)?;
        let started = client
            .start_runtime(&prepared.runtime_id, prepared.generation)
            .inspect_err(|_| {
                self.owner.invalidate();
            })?;
        if started.generation != prepared.generation || started.node_pid == 0 {
            return Err(Error::unknown(
                "owner start reply does not bind this runtime",
            ));
        }
        self.wait_for_phase(&prepared.runtime_id, "ready", READY_BUDGET)?;
        let qualified = client
            .recheck(&prepared.runtime_id, prepared.generation)
            .inspect_err(|_| {
                self.owner.invalidate();
            })?;
        let identity = qualified.identity();
        if identity.agent_uid != agent_uid
            || identity.session_binding != session_binding
            || identity.runtime_generation != prepared.generation
            || identity.os_uid != prepared.os_uid
            || identity.node_pid != started.node_pid
            || identity.node_starttime != started.node_starttime
        {
            return Err(Error::rejected(
                "owner recheck does not name this exact enrolled subject",
            ));
        }
        let handle = RuntimeHandle {
            alias: alias.to_string(),
            role: if crate::master::is_master(alias) {
                "master".to_string()
            } else {
                "worker".to_string()
            },
            agent_uid: agent_uid.to_string(),
            session_binding,
            runtime_id: prepared.runtime_id.clone(),
            operation_id: prepared.operation_id.clone(),
            generation: prepared.generation,
            server_generation,
            owner_epoch: prepared.owner_epoch,
            os_uid: prepared.os_uid,
            node_pid: identity.node_pid,
            node_starttime: identity.node_starttime,
            recipient_nonce: prepared.recipient_nonce.clone(),
            container_id: identity.container_id.clone(),
            loaded_receipt_id: identity.loaded_receipt_id.clone(),
            observed_registry_digest: identity.loaded_registry_digest.clone(),
            stop: Arc::clone(&stop),
            ops_path: prepared.ops_socket.clone(),
        };
        self.eligibility.refresh(qualified)?;
        let attached = self.attach_console(&handle, stderr_log)?;
        self.insert_handle(handle);
        ops_guard.armed = false;
        launch_guard.armed = false;
        Ok(attached)
    }

    fn wait_for_install(
        &self,
        prepared: &crate::protected_runtime::types::PreparedRuntime,
    ) -> Result<()> {
        use super::supervisor_grant::{
            parse_request, LaunchBinding, Lineage, Pins, Recipient, SupervisorChallenge,
        };
        let image = self.profile.image();
        let expected = SupervisorChallenge {
            launch: LaunchBinding {
                epoch: prepared.owner_epoch,
                request: parse_request(&prepared.launch_request)?,
            },
            recipient: Recipient {
                pid: prepared.recipient_pid,
                starttime: prepared.recipient_starttime.to_string(),
                generation: prepared.recipient_generation.clone(),
                nonce: prepared.recipient_nonce.clone(),
            },
            pins: Pins {
                source: image.source.clone(),
                image: image.reference.clone(),
                helper: image.helper.clone(),
                node: image.node.clone(),
                pi_graph: image.pi_graph.clone(),
                policy: image.policy.clone(),
            },
            lineage: Lineage {
                reference: self.profile.lineage_reference().to_string(),
                database_epoch: self.profile.database_epoch(),
            },
        };
        let deadline = Instant::now() + CONSUME_BUDGET;
        loop {
            if self.closing.load(Ordering::SeqCst) {
                return Err(Error::rejected("the daemon is shutting down"));
            }
            let installed = self
                .grant
                .lock()
                .map_err(|_| Error::internal("grant core lock poisoned"))?
                .as_mut()
                .ok_or_else(|| Error::rejected("the grant core is unavailable"))?
                .take_installed(&prepared.operation_id);
            if let Some(grant) = installed {
                if grant.challenge() != &expected {
                    return Err(Error::rejected(
                        "the acknowledged install does not bind this prepared allocation",
                    ));
                }
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(Error::unknown("the verified install ACK deadline elapsed"));
            }
            std::thread::sleep(POLL);
        }
    }

    fn wait_for_phase(&self, runtime_id: &str, phase: &str, budget: Duration) -> Result<()> {
        let deadline = Instant::now() + budget;
        loop {
            match self.owner.read(|client| client.observe(runtime_id)) {
                Ok(status) => {
                    if status.phase == phase && status.fenced_reason.is_none() {
                        return Ok(());
                    }
                    if status.phase == "revoking" || status.phase == "revoked" {
                        return Err(Error::rejected(
                            "the runtime obligation was fenced before it became ready",
                        ));
                    }
                }
                Err(e) if matches!(e.kind(), "unknown") && Instant::now() < deadline => {}
                Err(e) => return Err(e),
            }
            if Instant::now() >= deadline {
                return Err(Error::unknown(format!(
                    "runtime did not reach '{phase}' before the deadline"
                )));
            }
            std::thread::sleep(POLL);
        }
    }

    fn fence_id(
        &self,
        runtime_id: &str,
        operation_id: &str,
        generation: u64,
        reason: &str,
    ) -> Result<()> {
        if let Ok(mut slot) = self.grant.lock() {
            if let Some(grant) = slot.as_mut() {
                grant.remove_installed(operation_id);
            }
        }
        self.eligibility.invalidate(reason);
        let client = self.owner.client()?;
        let _ = client.fence(runtime_id, generation, reason)?;
        Ok(())
    }

    fn fence_handle(&self, handle: &RuntimeHandle, reason: &str) -> Result<()> {
        let fenced = self.fence_id(
            &handle.runtime_id,
            &handle.operation_id,
            handle.generation,
            reason,
        );
        // Stop this process's accept thread and unlink its socket; the owner
        // retired the container and the console socket itself.
        handle.stop.store(true, Ordering::SeqCst);
        if !handle.ops_path.as_os_str().is_empty() {
            use std::os::unix::fs::FileTypeExt;
            if let Ok(meta) = std::fs::symlink_metadata(&handle.ops_path) {
                if meta.file_type().is_socket() {
                    let _ = std::fs::remove_file(&handle.ops_path);
                }
            }
        }
        fenced
    }

    /// Stop exactly this agent's owned runtime (owner init / agent stop). The
    /// owner fences first, confirms the broker writer fence and retires only
    /// its own labelled container.
    pub(crate) fn stop_agent_now(&self, agent_uid: &str, reason: &str) -> Result<()> {
        let Some(handle) = self.handle_for_uid(agent_uid) else {
            return Ok(());
        };
        let result = self.fence_handle(&handle, reason);
        self.remove_handle(agent_uid);
        result
    }

    /// Fence every live runtime this supervisor started. Called from the
    /// daemon shutdown path; each stop is independent and bounded.
    pub(crate) fn stop_all(&self, reason: &str) {
        self.closing.store(true, Ordering::SeqCst);
        if let Ok(mut slot) = self.grant.lock() {
            if let Some(grant) = slot.as_mut() {
                grant.clear_installed();
            }
        }
        let handles: Vec<RuntimeHandle> = self
            .registry
            .lock()
            .map(|guard| guard.values().cloned().collect())
            .unwrap_or_default();
        for handle in handles {
            let _ = self.fence_handle(&handle, reason);
            self.remove_handle(&handle.agent_uid);
        }
    }

    /// Live owner facts for one already-registered caller, produced only from
    /// an authenticated owner recheck. Never cached, never synthesized.
    fn live_evidence(
        &self,
        handle: &RuntimeHandle,
    ) -> Result<crate::protected_runtime::OwnerEvidence> {
        self.owner
            .read(|client| client.owner_evidence(&handle.runtime_id, handle.generation))
    }

    fn owner_eligibility(&self, handle: &RuntimeHandle) -> Result<OwnerEligibility> {
        let evidence = self.live_evidence(handle)?;
        if evidence.agent_uid != handle.agent_uid
            || evidence.session_binding != handle.session_binding
            || evidence.runtime_generation != handle.generation
            || evidence.os_uid != handle.os_uid
        {
            return Err(Error::rejected(
                "owner evidence does not name the registered subject",
            ));
        }
        OwnerEligibility::from_owner_evidence(&evidence)
    }

    /// The model broker's measured-subject evidence. Requires the caller to be
    /// the measured guest Node process (pid + starttime), not merely the uid,
    /// and a live recheck plus the currently issued snapshot/receipt material.
    pub(crate) fn measure_native_model_caller(
        &self,
        caller: &RuntimeCaller,
    ) -> Result<super::native_model_turn::NativeModelRuntimeEvidence> {
        let handle = self.handle_for_caller(caller)?;
        if !handle.is_measured_node(caller) {
            return Err(Error::invalid(
                "principal_required",
                "only the measured Pi process may prepare a native model turn",
            ));
        }
        let utility = self.live_evidence(&handle)?;
        if utility.phase != "ready" || utility.fenced {
            return Err(Error::invalid(
                "runtime_unqualified",
                "the registered runtime is not currently ready",
            ));
        }
        let shared = self
            .shared
            .upgrade()
            .ok_or_else(|| Error::invalid("runtime_unqualified", "the daemon is shutting down"))?;
        let (snapshot, receipt) = shared.protected()?.current_material(caller)?;
        let approved_tools = self
            .profile
            .registry()
            .approved
            .iter()
            .map(|tool| super::native_model_turn::NativeModelApprovedTool {
                name: tool.name.clone(),
                schema_digest: tool.schema_digest.clone(),
            })
            .collect();
        let graph = self.profile.image().pi_graph.as_str();
        let graph = format!("sha256:{}", graph.strip_prefix("sha256:").unwrap_or(graph));
        Ok(super::native_model_turn::NativeModelRuntimeEvidence {
            workspace_id: self.profile.workspace_id().to_string(),
            agent_uid: handle.agent_uid.clone(),
            agent_alias: handle.alias.clone(),
            session_binding: handle.session_binding.clone(),
            runtime_id: handle.runtime_id.clone(),
            runtime_generation: handle.generation,
            owner_epoch: utility.owner_epoch,
            os_uid: handle.os_uid,
            node_pid: handle.node_pid,
            node_starttime: handle.node_starttime,
            container_id: handle.container_id.clone(),
            image_digest: self.profile.image().digest.clone(),
            profile_digest: self.profile.digest().to_string(),
            manifest_digest: self.profile.registry().registry_digest.clone(),
            sdk_graph_digest: graph,
            approved_tools,
            snapshot_id: snapshot.snapshot_id.as_str().to_string(),
            snapshot_expires_ms: snapshot.expires_at_ms.get(),
            loaded_receipt_id: receipt.loaded_receipt_id.as_str().to_string(),
        })
    }

    /// Revalidate one model evidence bundle against the live owner before a
    /// later stage uses it. Any changed generation/epoch/snapshot/fence refuses.
    pub(crate) fn revalidate_native_model_caller(
        &self,
        evidence: &super::native_model_turn::NativeModelRuntimeEvidence,
    ) -> Result<()> {
        let caller_handle = self
            .registry
            .lock()
            .map_err(|_| Error::internal("runtime registry lock poisoned"))?
            .get(&evidence.agent_uid)
            .cloned()
            .ok_or_else(|| {
                Error::invalid("runtime_unqualified", "the runtime is no longer registered")
            })?;
        if caller_handle.runtime_id != evidence.runtime_id
            || caller_handle.generation != evidence.runtime_generation
            || caller_handle.node_pid != evidence.node_pid
            || caller_handle.node_starttime != evidence.node_starttime
        {
            return Err(Error::invalid(
                "runtime_revoked",
                "the model evidence names a superseded runtime",
            ));
        }
        let qualified = self
            .owner
            .read(|client| client.recheck(&caller_handle.runtime_id, caller_handle.generation))?;
        let live = qualified.identity();
        if live.owner_epoch != evidence.owner_epoch
            || live.runtime_generation != evidence.runtime_generation
            || live.loaded_registry_digest != caller_handle.observed_registry_digest
        {
            return Err(Error::invalid(
                "runtime_revoked",
                "the runtime changed since the model turn was measured",
            ));
        }
        let shared = self
            .shared
            .upgrade()
            .ok_or_else(|| Error::invalid("runtime_unqualified", "the daemon is shutting down"))?;
        let protected = shared.protected()?;
        let (snapshot, receipt) = protected.current_material_for_agent(&evidence.agent_uid)?;
        if snapshot.snapshot_id.as_str() != evidence.snapshot_id
            || snapshot.expires_at_ms.get() != evidence.snapshot_expires_ms
            || receipt.loaded_receipt_id.as_str() != evidence.loaded_receipt_id
        {
            return Err(Error::invalid(
                "stale_configuration",
                "the issued snapshot/receipt changed since measurement",
            ));
        }
        self.eligibility.refresh(qualified)?;
        Ok(())
    }

    /// The actual generation reader, held only across native admission and its
    /// owner ACK, never across private SDK/provider I/O.
    pub(crate) fn native_model_admission_reader(
        &self,
        evidence: &super::native_model_turn::NativeModelRuntimeEvidence,
    ) -> Result<crate::protected_runtime::qualified::CurrentReader<'_>> {
        self.revalidate_native_model_caller(evidence)?;
        let reader = self.eligibility.reader()?;
        let identity = reader.require()?.identity();
        if identity.workspace_id != evidence.workspace_id
            || identity.agent_uid != evidence.agent_uid
            || identity.session_binding != evidence.session_binding
            || identity.runtime_id != evidence.runtime_id
            || identity.runtime_generation != evidence.runtime_generation
            || identity.owner_epoch != evidence.owner_epoch
            || identity.node_pid != evidence.node_pid
            || identity.node_starttime != evidence.node_starttime
        {
            return Err(Error::invalid(
                "runtime_revoked",
                "the admission generation changed",
            ));
        }
        Ok(reader)
    }
}

// The coordinator itself is the broker-fence target: the owner calls it while
// this supervisor is blocked in `owner.fence`, on the listener's own thread.
impl BrokerFenceTarget for ProtectedRuntimeCoordinator {
    #[cfg(target_os = "linux")]
    fn verify_owner_callback(
        &self,
        stream: &std::os::unix::net::UnixStream,
        owner_epoch: u64,
    ) -> Result<()> {
        let client = self
            .owner
            .client
            .read()
            .map_err(|_| Error::rejected("owner channel state is unavailable"))?
            .as_ref()
            .cloned()
            .ok_or_else(|| Error::rejected("owner channel is not authenticated"))?;
        client.verify_owner_callback(stream, owner_epoch)
    }

    fn fence_generation(
        &self,
        runtime_id: &str,
        owner_epoch: u64,
        generation: u64,
        reason: &str,
    ) -> Result<()> {
        let nonce = random_hex32()?;
        let _ = self
            .eligibility
            .fence_generation(runtime_id, owner_epoch, generation, &nonce)?;
        // Never deadlock an installer holding GrantCore while it waits for the
        // owner: a busy marker fence is refused, so the owner stays revoking.
        let mut slot = self
            .grant
            .try_lock()
            .map_err(|_| Error::rejected("grant marker fencing is busy"))?;
        if let Some(grant) = slot.as_mut() {
            grant.remove_runtime_installed(runtime_id, generation);
        }
        let _ = reason;
        Ok(())
    }
}

/// The truthful profile/core management context. Managerial actions have no
/// agent runtime subject, so this deliberately carries no runtime generation,
/// enrollment or loaded receipt: activation cannot depend on the activation it
/// is trying to create. `database_epoch`/`owner_epoch` are live authoritative
/// values (profile lineage + authenticated owner epoch), not synthesized times.
#[derive(Clone, Debug)]
pub(crate) struct ManagementProfileEvidence {
    pub workspace_id: String,
    pub custody_domain: &'static str,
    pub database_epoch: u64,
    pub owner_epoch: u64,
    pub profile_digest: String,
    pub supervisor_generation: String,
    pub fenced: bool,
}

impl ProtectedRuntimeCoordinator {
    pub(crate) fn management_profile_evidence(&self) -> Result<ManagementProfileEvidence> {
        let shared = self
            .shared
            .upgrade()
            .ok_or_else(|| Error::invalid("runtime_unqualified", "the daemon is shutting down"))?;
        let (witness, current) =
            current_owner_authority(&shared.store, &self.profile, &self.owner, false)?;
        if let BootAuthority::MetadataOnly(code) = current {
            return Err(Error::invalid(
                code,
                "protected management history is unavailable",
            ));
        }
        let authority = shared.store.protected_authority()?;
        if !authority.workspace_id.is_empty()
            && authority.workspace_id != self.profile.workspace_id()
        {
            return Err(Error::rejected(
                "durable core identity does not match the protected profile",
            ));
        }
        let owner_epoch = witness.owner_epoch;
        Ok(ManagementProfileEvidence {
            workspace_id: self.profile.workspace_id().to_string(),
            custody_domain: "local",
            database_epoch: self.profile.database_epoch(),
            owner_epoch,
            profile_digest: self.profile.digest().to_string(),
            supervisor_generation: self.supervisor_generation.clone(),
            fenced: authority.fenced,
        })
    }
}

impl ProtectedOwnerClient for ProtectedRuntimeCoordinator {
    /// Live, subject-specific owner evidence for one operations caller.
    fn current_eligibility(&self, subject: &RuntimeCaller) -> Result<OwnerEligibility> {
        let handle = self.handle_for_caller(subject)?;
        self.owner_eligibility(&handle)
    }

    /// Truthful profile-level management evidence (no agent runtime subject,
    /// no fabricated receipt). The backend uses this for bootstrap management.
    fn management_evidence(&self) -> Result<super::protected_connections::ManagementEvidence> {
        let evidence = self.management_profile_evidence()?;
        Ok(super::protected_connections::ManagementEvidence {
            workspace_id: evidence.workspace_id,
            custody_domain: evidence.custody_domain.to_string(),
            database_epoch: Revision::new(evidence.database_epoch)
                .map_err(|_| Error::internal("profile database epoch is not canonical"))?,
            owner_epoch: Revision::new(evidence.owner_epoch)
                .map_err(|_| Error::internal("owner epoch is not canonical"))?,
            profile_digest: Digest::try_from(evidence.profile_digest)
                .map_err(|_| Error::internal("profile digest is not canonical"))?,
            supervisor_generation: evidence.supervisor_generation,
            fenced: evidence.fenced,
        })
    }
}

struct BoundModelTask {
    guard: super::native_model_turn::NativeModelTaskGuard,
}

impl NativeTaskGuard for BoundModelTask {
    fn fence(&self, reason: &str) {
        self.guard.fence(reason);
    }
}

impl ProtectedConsoleProvider for ProtectedRuntimeCoordinator {
    fn open_console(
        &self,
        subject: ConsoleSubject<'_>,
        stderr_log: &Path,
    ) -> Result<AttachedConsole> {
        self.open_console_subject(
            subject.alias,
            subject.agent_uid,
            subject.created,
            stderr_log,
        )
    }

    fn bind_native_task(
        &self,
        caller: &RuntimeCaller,
        client_message_id: &str,
        server_message_id: &str,
        turn_token: &str,
    ) -> Result<Box<dyn NativeTaskGuard>> {
        let shared = self
            .shared
            .upgrade()
            .ok_or_else(|| Error::invalid("runtime_unqualified", "the daemon is shutting down"))?;
        let broker = shared.native_model()?;
        let binding = super::native_model_turn::NativeModelTaskBinding {
            client_message_id: client_message_id.to_string(),
            server_message_id: server_message_id.to_string(),
            turn_token: turn_token.to_string(),
        };
        let guard = broker.bind_task(caller, &binding)?;
        Ok(Box::new(BoundModelTask { guard }))
    }

    fn stop_agent(&self, agent_uid: &str, reason: &str) {
        // The owner dance is blocking; never hold the adapter's close path on
        // it, and never pretend success.
        if let Err(e) = self.stop_agent_now(agent_uid, reason) {
            self.emit_error("owned runtime stop was not confirmed", Some(e.to_string()));
        }
    }
}

// ---------------------------------------------------------------------------
// Parent hooks / Shared accessors
// ---------------------------------------------------------------------------

impl Shared {
    /// The protected runtime coordinator, when this daemon runs the reviewed
    /// `native-oci-agent/1` profile. An absent value means either the labeled
    /// ordinary daemon or a startup that failed before installation (which
    /// never falls back to a host process).
    pub(crate) fn protected_runtime(&self) -> Result<&Arc<ProtectedRuntimeCoordinator>> {
        self.protected_runtime
            .get()
            .ok_or_else(|| Error::invalid("runtime_unqualified", "protected runtime not installed"))
    }

    /// The protected profile accepts authority changes only through its
    /// one-use, checkpointed management paths. Legacy writers stay available
    /// to ordinary deployments; they cannot acknowledge rollbackable changes
    /// to protected credentials or grants.
    pub(super) fn refuse_legacy_protected_authority(
        &self,
        method: &str,
        params: &Value,
        peer_pid: u32,
    ) -> Result<()> {
        if self.protected_runtime.get().is_none()
            || !matches!(
                method,
                "platform_enroll"
                    | "platform_rotate"
                    | "platform_revoke"
                    | "platform_grant"
                    | "platform_ungrant"
                    | "platform_default_set"
                    | "connection_create"
                    | "connection_rotate"
                    | "connection_revoke"
                    | "connection_test"
                    | "connection_custody_initialize"
                    | "connection_custody_unlock"
                    | "connection_custody_lock"
            )
        {
            return Ok(());
        }
        self.operator_connection("protected credential management", params, peer_pid)?;
        Err(Error::invalid(
            "protected_management_required",
            "use cadence connection manage for one-use checkpointed human management",
        ))
    }

    /// The loaded root-pinned entry origin, not an execution-readiness claim.
    pub(crate) fn protected_entry_origin(&self) -> Option<&str> {
        self.protected_runtime
            .get()
            .map(|coordinator| coordinator.profile.operator_origin())
    }

    pub(super) fn rpc_credential_entry_config(
        &self,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        self.operator_connection("credential entry configuration", params, peer_pid)?;
        if !matches!(params, Value::Object(object) if object.is_empty()) {
            return Err(Error::invalid(
                "invalid_request",
                "expected an empty metadata request",
            ));
        }
        let Some(coordinator) = self.protected_runtime.get() else {
            return Ok(json!({"entry": {"configured": false, "protected": false}}));
        };
        Ok(json!({"entry": {
            "configured": true,
            "protected": true,
            "origin": coordinator.profile.operator_origin(),
            "profile_digest": coordinator.profile.digest(),
            "supervisor_generation": coordinator.supervisor_generation,
        }}))
    }
}

/// Install the coordinator after the `Arc<Shared>` exists. This replaces the
/// parent's unconditional `install_protected(None)` call: exactly one of the
/// two installs runs, and an active profile can never remain owner-less.
pub(crate) fn initialize(shared: &Arc<Shared>, prepared: Option<PreparedProtection>) -> Result<()> {
    let Some(prepared) = prepared else {
        return shared.install_protected(None);
    };
    let coordinator = ProtectedRuntimeCoordinator::start(shared, prepared)?;
    install_console_provider(&coordinator)?;
    shared.install_protected(Some(
        Arc::clone(&coordinator) as Arc<dyn ProtectedOwnerClient>
    ))?;
    shared
        .protected_runtime
        .set(Arc::clone(&coordinator))
        .map_err(|_| Error::internal("the protected runtime coordinator is already installed"))?;
    // The bounded native model broker is constructed only after the
    // coordinator is ready, and before any model operation can be routed.
    // `new` fails closed on a present-but-invalid/non-root-owned model config.
    let broker =
        super::native_model_turn::NativeModelBroker::new(shared, Arc::clone(&coordinator))?;
    shared
        .native_model
        .set(broker)
        .map_err(|_| Error::internal("the native model broker is already installed"))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

fn reserved_supervisor_process() -> bool {
    (unsafe { libc::geteuid() }) == crate::daemon::supervisor_grant::SUPERVISOR_UID
}

fn is_stable_agent_uid(value: &str) -> bool {
    let Some(rest) = value.strip_prefix("agt-") else {
        return false;
    };
    rest.len() == 32
        && rest
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

fn random_hex32() -> Result<String> {
    let raw = attestation::random32()?;
    Ok(attestation::hex32(&raw))
}

/// The durable server generation for one agent registration. The registration
/// instant is a real server fact: a re-registered alias gets a new row, a new
/// stable uid and a new value. It is never a caller field.
fn server_generation_for(created: f64) -> Result<u64> {
    let millis = created * 1000.0;
    if !millis.is_finite() || millis < 1.0 || millis >= u64::MAX as f64 {
        return Err(Error::rejected(
            "the agent registration has no durable server generation",
        ));
    }
    Ok(millis.round() as u64)
}

/// The live status of one registered runtime (diagnostics/readiness display).
pub(crate) fn runtime_status(
    coordinator: &ProtectedRuntimeCoordinator,
    runtime_id: &str,
) -> Result<RuntimeStatus> {
    coordinator.owner.read(|client| client.observe(runtime_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_agent_uid_shape() {
        assert!(is_stable_agent_uid("agt-0123456789abcdef0123456789abcdef"));
        assert!(!is_stable_agent_uid("agt-0123"));
        assert!(!is_stable_agent_uid("AGT-0123456789abcdef0123456789abcdef"));
        assert!(!is_stable_agent_uid("worker"));
    }

    #[test]
    fn registry_round_trips_and_rejects_extra_fields() {
        let handle = RuntimeHandle {
            alias: "worker".into(),
            role: "worker".into(),
            agent_uid: "agt-0123456789abcdef0123456789abcdef".into(),
            session_binding: "ab".repeat(16),
            runtime_id: "11111111-1111-4111-8111-111111111111".into(),
            operation_id: "33333333-3333-4333-8333-333333333333".into(),
            generation: 7,
            server_generation: 1_700_000_000_000,
            owner_epoch: 3,
            os_uid: 21001,
            node_pid: 42,
            node_starttime: 99,
            recipient_nonce: "cd".repeat(16),
            container_id: "a".repeat(64),
            loaded_receipt_id: "22222222-2222-4222-8222-222222222222".into(),
            observed_registry_digest: format!("sha256:{}", "e".repeat(64)),
            stop: Arc::new(AtomicBool::new(true)),
            ops_path: PathBuf::new(),
        };
        let value = handle.to_json();
        let back = RuntimeHandle::from_json(&value).unwrap();
        assert_eq!(back.runtime_id, handle.runtime_id);
        let mut extra = value.clone();
        extra["caller_uid"] = json!(0);
        assert!(RuntimeHandle::from_json(&extra).is_err());
    }
}
