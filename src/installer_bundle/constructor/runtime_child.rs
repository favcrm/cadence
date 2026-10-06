//! Fixed runtime Client entry and UID21000 accepted-FD proxy. This child is
//! transport/application only. Root keeps every positive kernel/caller factory.
use super::super::{carrier, files, fixed_arguments, refused, Deadline, Result};
use super::{
    custody,
    private_wire::{self, Domain, Packet},
};
use std::fs::File;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::net::{UnixDatagram, UnixListener};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
static CONTROL: OnceLock<Arc<UnixDatagram>> = OnceLock::new();
pub(crate) fn control() -> Option<Arc<UnixDatagram>> {
    CONTROL.get().cloned()
}
/// Fixed diagnostic continuation of the actual admitted Client. Compilation
/// exposes no issuer: CONTROL was installed only after Boot, image/self, seal
/// and the real parent socket checks in entry(). No ordinary daemon can enter.
#[cfg(all(
    debug_assertions,
    feature = "test-seam",
    target_os = "linux",
    target_arch = "x86_64"
))]
pub(crate) fn retained_opening_replay(
    store: &crate::store::Store,
) -> Result<std::convert::Infallible> {
    let control = control().ok_or_else(refused)?;
    let deadline = Deadline(Instant::now() + Duration::from_secs(30));
    let parent = || -> Result<()> {
        deadline.check()?;
        let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
        let mut len = std::mem::size_of_val(&cred) as libc::socklen_t;
        if unsafe {
            libc::getsockopt(
                control.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut cred as *mut libc::ucred).cast(),
                &mut len,
            )
        } != 0
            || len as usize != std::mem::size_of_val(&cred)
            || cred.uid != 0
            || cred.gid != 0
            || cred.pid != unsafe { libc::getppid() }
        {
            return Err(refused());
        }
        Ok(())
    };
    let next = || -> Result<Packet> {
        loop {
            parent()?;
            if let Some(packet) = private_wire::receive_child(&control)? {
                return Ok(packet);
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    };
    parent()?;
    private_wire::send(
        &control,
        &Packet::StoreReplayPrepared { version: 1 },
        &[],
        deadline,
    )?;
    if !matches!(next()?, Packet::StoreReplayBegin { version: 1 }) {
        return Err(refused());
    }
    // Root's opening/WAL/current checks finished BEFORE Begin. No current,
    // flush or recovery is inserted into the author's file comparison range.
    crate::store::seal::owner_guard::native_retained_opening_replay(store)?;
    parent()?;
    private_wire::send(
        &control,
        &Packet::StoreReplayChecked { version: 1 },
        &[],
        deadline,
    )?;
    if !matches!(next()?, Packet::StoreReplayStop { version: 1 }) {
        return Err(refused());
    }
    parent()?;
    // Never resume business emitters/Serving, or drop SQLite into a checkpoint.
    // Root must still independently observe this own child's clean exit/reap.
    unsafe { libc::_exit(0) }
}
pub(super) fn try_entry() -> Option<Result<()>> {
    let mut ty = 0i32;
    let mut n = std::mem::size_of_val(&ty) as libc::socklen_t;
    if unsafe {
        libc::getsockopt(
            3,
            libc::SOL_SOCKET,
            libc::SO_TYPE,
            (&mut ty as *mut i32).cast(),
            &mut n,
        )
    } != 0
        || ty != libc::SOCK_DGRAM
    {
        return None;
    }
    Some(entry())
}
fn entry() -> Result<()> {
    fixed_arguments()?;
    let shared = crate::adapter::pi_guest::topology::resolve_gid_pub(
        crate::adapter::pi_guest::acct::SHARED_GROUP,
    )?;
    let mut group = 0u32;
    if shared == 0
        || shared == 21000
        || unsafe { libc::getgroups(1, &mut group) } != 1
        || group != shared
    {
        return Err(refused());
    }
    if std::env::vars_os().next().is_some()
        || carrier::ids()? != [21000; 6]
        || unsafe { libc::prctl(libc::PR_GET_SECUREBITS, 0, 0, 0, 0) } != 239
        || unsafe { libc::prctl(libc::PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) } != 1
        || unsafe { libc::prctl(libc::PR_GET_SECCOMP, 0, 0, 0, 0) } != 2
    {
        return Err(refused());
    }
    let status = custody::status(std::process::id())?;
    if custody::field(&status, "TracerPid:")? != "0" || custody::field(&status, "Threads:")? != "1"
    {
        return Err(refused());
    }
    for key in ["CapInh:", "CapPrm:", "CapEff:", "CapBnd:", "CapAmb:"] {
        if custody::field(&status, key)? != "0000000000000000" {
            return Err(refused());
        }
    }
    let c = Arc::new(unsafe { UnixDatagram::from_raw_fd(3) });
    c.set_nonblocking(true).map_err(|_| refused())?;
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of_val(&cred) as libc::socklen_t;
    if unsafe {
        libc::getsockopt(
            c.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast(),
            &mut len,
        )
    } != 0
        || len as usize != std::mem::size_of_val(&cred)
        || cred.uid != 0
        || cred.gid != 0
        || cred.pid != unsafe { libc::getppid() }
    {
        return Err(refused());
    }
    let deadline = Deadline::new();
    let packet = loop {
        deadline.check()?;
        if let Some(p) = private_wire::receive(&c)? {
            break p;
        }
        std::thread::sleep(Duration::from_millis(1));
    };
    let (
        Packet::Boot {
            version: 1,
            image_attestation,
        },
        fds,
    ) = packet
    else {
        return Err(refused());
    };
    if !fds.is_empty() {
        return Err(refused());
    }
    // Independent image authentication and SELF executable corroboration do
    // not create a RootProof, owned caller, Pi operation or Store grant.
    let now = super::context::runtime_ms()?;
    let image = super::authenticate_image_attestation(&image_attestation, now, None, now)?;
    files::measure(
        &File::open("/proc/self/exe").map_err(|_| refused())?,
        super::pin(&image.artifacts.client)?,
        0,
        deadline,
    )?;
    CONTROL.set(c.clone()).map_err(|_| refused())?;
    for (fd, domain, path) in [
        (4, Domain::Pi, "/run/cadence/private/pi-launch.sock"),
        (5, Domain::Store, "/run/cadence/private/store-owner.sock"),
    ] {
        let listener = unsafe { UnixListener::from_raw_fd(fd) };
        if listener.local_addr().map_err(|_| refused())?.as_pathname()
            != Some(std::path::Path::new(path))
        {
            return Err(refused());
        }
        listener.set_nonblocking(true).map_err(|_| refused())?;
        let ctl = c.clone();
        std::thread::spawn(move || loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    if private_wire::send(
                        &ctl,
                        &Packet::Accepted { version: 1, domain },
                        &[stream.as_raw_fd()],
                        Deadline(Instant::now() + Duration::from_secs(10)),
                    )
                    .is_err()
                    {
                        unsafe { libc::_exit(125) }
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(1))
                }
                Err(_) => unsafe { libc::_exit(125) },
            }
        });
    }
    let provider_env = crate::adapter::ProviderEnv::default();
    // Match the helper's fixed mutable company PM leaf; /workspace itself
    // remains supervisor-owned, non-writable to the guest. This path routes
    // application data, never identity/authority or a model-policy bypass.
    provider_env.set("CADENCE_PM_DIR", "/workspace/company/pm");
    provider_env.set("CADENCE_STATE_DIR", "/srv/cadence/protected/store");
    crate::daemon::serve_with(
        std::path::Path::new("/srv/cadence/protected/store"),
        crate::daemon::ServeOptions {
            agent_uid: Some(21001),
            provider_env,
            shared_socket: Some(("/srv/cadence/guest-views/cadence.sock".into(), shared)),
            ..crate::daemon::ServeOptions::default()
        },
    )
}
