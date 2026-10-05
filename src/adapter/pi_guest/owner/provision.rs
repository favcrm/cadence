//! Root-owned finite view provisioning after real external operation issuance.
//! Never repairs existing ownership, accepts a caller path, copies credentials,
//! follows links, deletes data, or reuses an existing generation. Partial errors
//! leave an unresolved generation; only the root lifecycle owner may retire it.
use super::LaunchPermit;
use crate::adapter::pi_guest::{topology::ProtectedTopology, Segments};
use crate::error::{Error, Result};
use std::ffi::CString;
use std::fs::File;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::MetadataExt;

fn refused() -> Error {
    Error::rejected("protected Pi view provisioning refused/UNKNOWN")
}
#[repr(C)]
struct OpenHow {
    flags: u64,
    mode: u64,
    resolve: u64,
}
fn metadata(fd: &OwnedFd) -> Result<std::fs::Metadata> {
    Ok(File::from(fd.try_clone()?).metadata()?)
}
fn check(fd: &OwnedFd, uid: u32, gid: u32, mode: u32) -> Result<()> {
    let m = metadata(fd)?;
    if !m.is_dir() || m.uid() != uid || m.gid() != gid || m.mode() & 0o7777 != mode {
        return Err(refused());
    }
    Ok(())
}
fn directory(
    parent: &OwnedFd,
    name: &str,
    uid: u32,
    gid: u32,
    mode: u32,
    fresh: bool,
) -> Result<OwnedFd> {
    if name.is_empty() || name.contains('/') || name == "." || name == ".." {
        return Err(refused());
    }
    let name = CString::new(name).map_err(|_| refused())?;
    let created = if unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) } == 0 {
        true
    } else {
        let error = std::io::Error::last_os_error();
        if fresh || error.raw_os_error() != Some(libc::EEXIST) {
            return Err(error.into());
        }
        false
    };
    let how = OpenHow {
        flags: (libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW) as u64,
        mode: 0,
        resolve: libc::RESOLVE_BENEATH | libc::RESOLVE_NO_SYMLINKS | libc::RESOLVE_NO_XDEV,
    };
    let raw = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            parent.as_raw_fd(),
            name.as_ptr(),
            &how,
            std::mem::size_of::<OpenHow>(),
        )
    };
    if raw < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let fd = unsafe { OwnedFd::from_raw_fd(raw as i32) };
    if created {
        // Only our fresh root-owned inode may be changed. Never chown/chmod a
        // caller-substituted or preexisting guest/supervisor-owned object.
        check(&fd, 0, 0, 0o700)?;
        if unsafe { libc::fchown(fd.as_raw_fd(), uid, gid) } != 0
            || unsafe { libc::fchmod(fd.as_raw_fd(), mode) } != 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    check(&fd, uid, gid, mode)?;
    Ok(fd)
}
/// Accepts ONLY a private-origin non-Clone permit, never an Authorized JSON
/// description. Root dispatcher must first admit the actual supervisor handle.
pub(super) fn create(permit: &LaunchPermit) -> Result<()> {
    if unsafe { libc::getuid() } != 0
        || unsafe { libc::geteuid() } != 0
        || unsafe { libc::getegid() } != 0
    {
        return Err(refused());
    }
    permit.recheck()?;
    let launch = permit.describe();
    let topology = ProtectedTopology::verify(launch.guest)?;
    let anchor = topology.view_anchor()?;
    let segments = Segments::new(&launch.alias, &launch.selection.generation)?;
    if segments.alias_hex() != launch.selection.alias_sha256 {
        return Err(refused());
    }
    let alias = directory(
        &anchor,
        &segments.alias_hex(),
        launch.supervisor,
        launch.shared_gid,
        0o750,
        false,
    )?;
    let durable = directory(
        &alias,
        "durable",
        launch.supervisor,
        launch.shared_gid,
        0o750,
        false,
    )?;
    let _sessions = directory(
        &durable,
        "sessions",
        launch.guest,
        launch.guest_gid,
        0o700,
        false,
    )?;
    // Existing generation = refusal, even if perfectly shaped/empty. No cleanup
    // or cached marker can convert a replay into a fresh authorized generation.
    let generation = directory(
        &alias,
        &segments.generation_hex(),
        launch.supervisor,
        launch.shared_gid,
        0o750,
        true,
    )?;
    for leaf in ["home", "config", "cache", "tmp", "no-forge"] {
        directory(
            &generation,
            leaf,
            launch.guest,
            launch.guest_gid,
            0o700,
            true,
        )?;
    }
    directory(
        &generation,
        "briefing",
        launch.supervisor,
        launch.shared_gid,
        0o750,
        true,
    )?;
    topology.verify_view(
        &segments,
        if launch.selection.role == crate::protected_pi_profile::authority::Role::Master {
            crate::adapter::pi_guest::Role::Master
        } else {
            crate::adapter::pi_guest::Role::Worker
        },
    )?;
    permit.recheck()
}
