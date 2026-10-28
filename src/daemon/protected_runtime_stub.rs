//! Explicitly refusing protected-runtime stub for hosts other than Linux
//! x86_64. The real coordinator provisions root-owned OCI consoles and needs
//! Linux x86_64 kernel facilities; on any other target every entry point
//! refuses instead of falling back to a host same-UID process.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::protected_runtime::console::OwnerConsole;
use crate::protected_runtime::qualified::CurrentReader;
use crate::protected_runtime::RuntimeCaller;
use crate::store::Store;

use super::Shared;

pub(crate) struct PreparedProtection;

/// Mirrors the real boot decision; only the refusal-free variants exist so a
/// caller cannot match a protected success on an unsupported host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PreparedBoot {
    Ready {
        owner_epoch: u64,
        floor_sequence: u64,
        floor_digest: String,
    },
    MetadataOnly {
        code: &'static str,
    },
}

pub(crate) struct ProtectedRuntimeCoordinator;

/// Mirrors the real profile/core management context. On this host no
/// coordinator can exist, so the only producer refuses.
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
    /// No coordinator can exist on this host; shutdown has nothing to stop.
    pub(crate) fn stop_all(&self, _reason: &str) {}

    pub(crate) fn management_profile_evidence(&self) -> Result<ManagementProfileEvidence> {
        Err(refused())
    }

    pub(crate) fn measure_native_model_caller(
        &self,
        _caller: &RuntimeCaller,
    ) -> Result<super::native_model_turn::NativeModelRuntimeEvidence> {
        Err(refused())
    }

    pub(crate) fn native_model_admission_reader(
        &self,
        _evidence: &super::native_model_turn::NativeModelRuntimeEvidence,
    ) -> Result<CurrentReader<'_>> {
        Err(refused())
    }
}

impl Shared {
    pub(crate) fn protected_runtime(&self) -> Result<&Arc<ProtectedRuntimeCoordinator>> {
        self.protected_runtime
            .get()
            .ok_or_else(|| Error::invalid("runtime_unqualified", "protected runtime not installed"))
    }

    /// No protected coordinator can be installed on this host, so legacy
    /// management stays available exactly as in an ordinary deployment.
    pub(super) fn refuse_legacy_protected_authority(
        &self,
        _method: &str,
        _params: &Value,
        _peer_pid: u32,
    ) -> Result<()> {
        Ok(())
    }

    pub(crate) fn protected_entry_origin(&self) -> Option<&str> {
        None
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
        Ok(json!({"entry": {"configured": false, "protected": false}}))
    }
}

pub(crate) struct AttachedConsole {
    pub(crate) console: OwnerConsole,
    pub(crate) generation_tag: String,
    pub(crate) runtime_id: String,
    pub(crate) caller: RuntimeCaller,
}

pub(crate) struct ConsoleSubject<'a> {
    pub(crate) alias: &'a str,
    pub(crate) agent_uid: &'a str,
    pub(crate) created: f64,
}

pub(crate) trait NativeTaskGuard: Send {
    fn fence(&self, reason: &str);
}

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

    fn stop_agent(&self, agent_uid: &str, reason: &str);
}

fn refused() -> Error {
    Error::rejected("the protected runtime requires Linux x86_64")
}

/// Ordinary mode (no profile) stays ordinary; a configured profile refuses.
pub(crate) fn prepare_profile(profile_path: Option<&Path>) -> Result<Option<PreparedProtection>> {
    match profile_path {
        None => Ok(None),
        Some(_) => Err(refused()),
    }
}

pub(crate) fn install_store_gate(
    _store: &Store,
    _prepared: &PreparedProtection,
) -> Result<PreparedBoot> {
    Err(refused())
}

pub(crate) fn default_profile_path() -> Result<Option<PathBuf>> {
    Ok(None)
}

pub(crate) fn initialize(
    _shared: &Arc<Shared>,
    prepared: Option<PreparedProtection>,
) -> Result<()> {
    match prepared {
        None => Ok(()),
        Some(_) => Err(refused()),
    }
}

pub(crate) fn console_provider() -> Result<Arc<dyn ProtectedConsoleProvider>> {
    Err(refused())
}
