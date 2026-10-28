//! Explicitly refusing protected-runtime stub for hosts other than Linux
//! x86_64. The real coordinator provisions root-owned OCI consoles and needs
//! Linux x86_64 kernel facilities; on any other target every entry point
//! refuses instead of falling back to a host same-UID process.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::error::{Error, Result};
use crate::protected_runtime::console::OwnerConsole;
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
