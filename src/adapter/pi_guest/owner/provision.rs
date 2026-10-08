//! Root-owned finite view provisioning after real external operation issuance.
//! Never repairs existing ownership, accepts a caller path, copies credentials,
//! follows links, deletes data, or reuses an existing generation. Partial errors
//! leave an unresolved generation; only the root lifecycle owner may retire it.
use super::{current, Epoch, LaunchPermit};
use crate::adapter::pi_guest::{topology::ProtectedTopology, Segments};
use crate::error::{Error, Result};
use crate::installer_bundle::constructor::{RetiredPiFamily, RuntimeProof};
use crate::protected_pi_profile::authority::Selection;
use std::cell::Cell;
use std::ffi::CString;
use std::fs::File;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::MetadataExt;
use std::time::Instant;

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
fn open_directory(parent: &OwnedFd, name: &str) -> Result<OwnedFd> {
    if name.is_empty() || name.contains('/') || name == "." || name == ".." {
        return Err(refused());
    }
    let name = CString::new(name).map_err(|_| refused())?;
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
    Ok(unsafe { OwnedFd::from_raw_fd(raw as i32) })
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
    let cname = CString::new(name).map_err(|_| refused())?;
    let created = if unsafe { libc::mkdirat(parent.as_raw_fd(), cname.as_ptr(), 0o700) } == 0 {
        true
    } else {
        let error = std::io::Error::last_os_error();
        if fresh || error.raw_os_error() != Some(libc::EEXIST) {
            return Err(error.into());
        }
        false
    };
    let fd = open_directory(parent, name)?;
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

#[derive(Clone, Copy, PartialEq, Eq)]
struct DirectoryIdentity {
    device: (u32, u32),
    inode: u64,
    mount: u64,
    uid: u32,
    gid: u32,
    mode: u32,
    flags: libc::c_ulong,
}
#[derive(Clone, Copy, PartialEq, Eq)]
struct DirectoryStamp {
    identity: DirectoryIdentity,
    links: u32,
    mtime: (i64, u32),
    ctime: (i64, u32),
}
fn stamp(fd: &OwnedFd) -> Result<DirectoryStamp> {
    let mut stat: libc::statx = unsafe { std::mem::zeroed() };
    let fields = libc::STATX_BASIC_STATS | libc::STATX_MNT_ID;
    let mut fs: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe {
        libc::statx(
            fd.as_raw_fd(),
            c"".as_ptr(),
            libc::AT_EMPTY_PATH | libc::AT_SYMLINK_NOFOLLOW,
            fields,
            &mut stat,
        )
    } != 0
        || stat.stx_mask & fields != fields
        || stat.stx_mode as u32 & libc::S_IFMT != libc::S_IFDIR
        || stat.stx_mnt_id == 0
        || unsafe { libc::fstatvfs(fd.as_raw_fd(), &mut fs) } != 0
        || fs.f_flag & libc::ST_RDONLY != 0
        || fs.f_flag & libc::ST_NOSUID == 0
        || fs.f_flag & libc::ST_NODEV == 0
    {
        return Err(refused());
    }
    Ok(DirectoryStamp {
        identity: DirectoryIdentity {
            device: (stat.stx_dev_major, stat.stx_dev_minor),
            inode: stat.stx_ino,
            mount: stat.stx_mnt_id,
            uid: stat.stx_uid,
            gid: stat.stx_gid,
            mode: stat.stx_mode as u32 & 0o7777,
            flags: fs.f_flag,
        },
        links: stat.stx_nlink,
        mtime: (stat.stx_mtime.tv_sec, stat.stx_mtime.tv_nsec),
        ctime: (stat.stx_ctime.tv_sec, stat.stx_ctime.tv_nsec),
    })
}
#[derive(Clone, Copy)]
enum Isolation {
    Live,
    Unknown,
    Completed(DirectoryStamp),
}
/// Minted ONLY below, by authentic fresh provisioning. No caller path/FD,
/// descriptor, boolean or retirement factory; Drop never modifies the view.
pub(crate) struct GenerationView {
    topology: ProtectedTopology,
    anchor: OwnedFd,
    parent: OwnedFd,
    generation: OwnedFd,
    anchor_identity: DirectoryIdentity,
    parent_identity: DirectoryIdentity,
    original: DirectoryStamp,
    selection: Selection,
    operation: String,
    proof: RuntimeProof,
    epoch: Epoch,
    isolation: Cell<Isolation>,
}
impl GenerationView {
    fn current(&self, until: Instant) -> Result<()> {
        if Instant::now() >= until
            || unsafe { libc::getuid() } != 0
            || unsafe { libc::geteuid() } != 0
            || unsafe { libc::getgid() } != 0
            || unsafe { libc::getegid() } != 0
            || current(&self.proof, until)? != self.epoch
            || Instant::now() >= until
        {
            return Err(refused());
        }
        Ok(())
    }
    fn correspondence(&self, expected: DirectoryStamp) -> Result<()> {
        let anchor = self.topology.view_anchor()?;
        if stamp(&self.anchor)?.identity != self.anchor_identity
            || stamp(&anchor)?.identity != self.anchor_identity
        {
            return Err(refused());
        }
        let parent = open_directory(&anchor, &self.selection.alias_sha256)?;
        if stamp(&self.parent)?.identity != self.parent_identity
            || stamp(&parent)?.identity != self.parent_identity
        {
            return Err(refused());
        }
        let named = open_directory(&parent, &self.selection.generation)?;
        if stamp(&self.generation)? != expected || stamp(&named)? != expected {
            return Err(refused());
        }
        Ok(())
    }
    fn require(&self, family: &RetiredPiFamily<'_>, until: Instant) -> Result<()> {
        family.require(&self.selection, &self.operation, until)?;
        self.current(until)
    }
    pub(crate) fn isolate(&self, family: &RetiredPiFamily<'_>, until: Instant) -> Result<()> {
        match self.isolation.get() {
            Isolation::Completed(_) => return self.require_isolated(family, until),
            Isolation::Unknown => return Err(refused()),
            Isolation::Live => self.isolation.set(Isolation::Unknown),
        }
        // Burn BEFORE proof checks. Wrong proof/current/correspondence causes
        // zero chmod and permanent UNKNOWN, not a matching-proof retry.
        self.require(family, until)?;
        self.correspondence(self.original)?;
        self.require(family, until)?;
        self.correspondence(self.original)?;
        if Instant::now() >= until {
            return Err(refused());
        }
        #[cfg(all(debug_assertions, feature = "test-seam"))]
        crate::adapter::pi_guest::diag_seam::before_fchmod(&self.selection)?;
        if unsafe { libc::fchmod(self.generation.as_raw_fd(), 0o700) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let completed = stamp(&self.generation)?;
        let mut identity = self.original.identity;
        identity.mode = 0o700;
        if completed.identity != identity
            || completed.links != self.original.links
            || completed.mtime != self.original.mtime
        {
            return Err(refused());
        }
        self.correspondence(completed)?;
        self.require(family, until)?;
        if Instant::now() >= until {
            return Err(refused());
        }
        self.isolation.set(Isolation::Completed(completed));
        Ok(())
    }
    /// N1 diagnostic: read-only; true while this view is sticky Unknown.
    #[cfg(all(debug_assertions, feature = "test-seam"))]
    #[allow(dead_code)] // read by the native harness
    pub(crate) fn observe_unknown(&self) -> bool {
        matches!(self.isolation.get(), Isolation::Unknown)
    }
    /// Completion is private state plus exact original witness/correspondence,
    /// NEVER an observed0700 directory or a helper-only exited/retired boolean.
    pub(crate) fn require_isolated(
        &self,
        family: &RetiredPiFamily<'_>,
        until: Instant,
    ) -> Result<()> {
        let Isolation::Completed(completed) = self.isolation.get() else {
            return Err(refused());
        };
        let result = (|| {
            self.require(family, until)?;
            self.correspondence(completed)?;
            self.require(family, until)?;
            if Instant::now() >= until {
                return Err(refused());
            }
            Ok(())
        })();
        if result.is_err() {
            self.isolation.set(Isolation::Unknown);
        }
        result
    }
}
/// Accepts ONLY a private-origin non-Clone permit, never an Authorized JSON
/// description. Root dispatcher must first admit the actual supervisor handle.
pub(super) fn create(permit: &LaunchPermit) -> Result<GenerationView> {
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
    permit.recheck()?;
    let original = stamp(&generation)?;
    let anchor_identity = stamp(&anchor)?.identity;
    let parent_identity = stamp(&alias)?.identity;
    if original.identity.uid != launch.supervisor
        || original.identity.gid != launch.shared_gid
        || original.identity.mode != 0o750
        || anchor_identity.uid != launch.supervisor
        || anchor_identity.gid != launch.shared_gid
        || anchor_identity.mode != 0o750
        || parent_identity.uid != launch.supervisor
        || parent_identity.gid != launch.shared_gid
        || parent_identity.mode != 0o750
    {
        return Err(refused());
    }
    let view = GenerationView {
        topology,
        anchor,
        parent: alias,
        generation,
        anchor_identity,
        parent_identity,
        original,
        selection: launch.selection.clone(),
        operation: launch.operation.clone(),
        proof: permit.proof.clone(),
        epoch: permit.epoch.clone(),
        isolation: Cell::new(Isolation::Live),
    };
    view.correspondence(original)?;
    permit.recheck()?;
    Ok(view)
}
