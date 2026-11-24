//! Process-local adapter port for protected consoles.

use std::path::Path;
use std::sync::{Arc, OnceLock, Weak};

use crate::error::{Error, Result};
use crate::protected_runtime::console::OwnerConsole;
use crate::protected_runtime::RuntimeCaller;

/// An attachment whose caller was derived from a live owner recheck.
pub struct AttachedConsole {
    pub(crate) console: OwnerConsole,
    pub(crate) generation_tag: String,
    pub(crate) runtime_id: String,
    pub(crate) caller: RuntimeCaller,
}

/// The subject comes from the daemon's durable agent row.
pub struct ConsoleSubject<'a> {
    pub alias: &'a str,
    pub agent_uid: &'a str,
    pub created: f64,
}

pub trait NativeTaskGuard: Send {
    fn fence(&self, reason: &str);
}

pub trait ProtectedConsoleProvider: Send + Sync {
    fn open_console(&self, subject: ConsoleSubject<'_>, stderr_log: &Path) -> Result<AttachedConsole>;
    fn bind_native_task(
        &self,
        caller: &RuntimeCaller,
        client_message_id: &str,
        claim: &crate::store::NativeTurnClaim,
        turn_token: &str,
    ) -> Result<Box<dyn NativeTaskGuard>>;
    fn stop_agent(&self, agent_uid: &str, reason: &str);
}

static CONSOLE_PROVIDER: OnceLock<Weak<dyn ProtectedConsoleProvider>> = OnceLock::new();

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub fn install_console_provider(
    _owner: &crate::protected_runtime::owner_link::OwnerLink,
    provider: &Arc<dyn ProtectedConsoleProvider>,
) -> Result<()> {
    CONSOLE_PROVIDER
        .set(Arc::downgrade(provider))
        .map_err(|_| Error::internal("the protected console provider is already installed"))
}

pub fn console_provider() -> Result<Arc<dyn ProtectedConsoleProvider>> {
    CONSOLE_PROVIDER
        .get()
        .and_then(|weak| weak.upgrade())
        .ok_or_else(|| Error::invalid("runtime_unqualified", "no protected runtime coordinator is installed"))
}

/// An identity copied only from an owner-issued qualified runtime. Fields are
/// private so an adapter provider cannot manufacture an attached caller.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub struct VerifiedConsoleSubject {
    runtime_id: String,
    agent_uid: String,
    session_binding: String,
    generation: u64,
    os_uid: u32,
    node_pid: u32,
    node_starttime: u64,
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
impl VerifiedConsoleSubject {
    pub fn from_qualified(qualified: &crate::protected_runtime::qualified::QualifiedRuntime) -> Self {
        let identity = qualified.identity();
        Self {
            runtime_id: identity.runtime_id.clone(),
            agent_uid: identity.agent_uid.clone(),
            session_binding: identity.session_binding.clone(),
            generation: identity.runtime_generation,
            os_uid: identity.os_uid,
            node_pid: identity.node_pid,
            node_starttime: identity.node_starttime,
        }
    }
}

/// Attach only using an authenticated owner client and an owner-rechecked
/// subject; selector strings alone cannot mint a runtime caller.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub fn attach_console(
    client: &crate::protected_runtime::RootOwnerClient,
    subject: VerifiedConsoleSubject,
    recipient_nonce: &str,
    stderr_log: &Path,
) -> Result<AttachedConsole> {
    let open = client.open_console(&subject.runtime_id, subject.generation, recipient_nonce)?;
    if open.node_pid != subject.node_pid || open.node_starttime != subject.node_starttime {
        return Err(Error::rejected("owner console names a different measured guest"));
    }
    let console = crate::protected_runtime::console::connect(
        &open.path,
        &open.token,
        &subject.runtime_id,
        subject.generation,
        open.node_pid,
        Some(stderr_log.to_path_buf()),
    )?;
    Ok(AttachedConsole {
        console,
        generation_tag: format!("{}-{}", subject.runtime_id, subject.generation),
        runtime_id: subject.runtime_id.clone(),
        caller: RuntimeCaller::registered(
            subject.runtime_id,
            subject.agent_uid,
            subject.session_binding,
            subject.generation,
            subject.os_uid,
            subject.node_pid,
            subject.node_starttime,
        ),
    })
}
