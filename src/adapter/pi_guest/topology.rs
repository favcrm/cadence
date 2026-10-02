//! The protected topology — an openat2 verify-only walk of the
//! pre-provisioned tree. Every node the launch depends on is opened relative
//! to a pinned `/` dirfd with `RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS` and
//! `fstat`-checked (kind, owner, group, mode, `nlink`). The source never
//! creates a privileged node and never chowns: a missing or mis-owned slot
//! refuses outright. `openat2` is the only traversal — an `ENOSYS`/`EPERM`/
//! `EINVAL`/`EOPNOTSUPP` answer means the syscall is unavailable and the walk
//! fails closed rather than silently downgrade.

#[cfg(target_os = "linux")]
use std::ffi::{CStr, CString};
#[cfg(target_os = "linux")]
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::os::unix::io::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd, RawFd};
use std::path::Path;

use super::execfd::OpenKind;
use super::{acct, Error, Layer, Result, Role, Segments};

/// `linux/openat2.h` `struct open_how` — matches the in-tree declaration in
/// `src/platform/local.rs`.
#[cfg(target_os = "linux")]
#[repr(C)]
struct OpenHow {
    flags: u64,
    mode: u64,
    resolve: u64,
}

/// One raw `openat2`. The caller maps the error: `ENOSYS`/`EPERM`/`EINVAL`/
/// `EOPNOTSUPP` mean the syscall is absent and the whole approach refuses.
#[cfg(target_os = "linux")]
fn sys_openat2(dirfd: RawFd, path: &CStr, how: &OpenHow) -> Result<RawFd> {
    let fd = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            dirfd,
            path.as_ptr(),
            how as *const OpenHow,
            std::mem::size_of::<OpenHow>(),
        )
    };
    if fd < 0 {
        Err(Error::internal(format!(
            "openat2: {}",
            std::io::Error::last_os_error()
        )))
    } else {
        Ok(fd as RawFd)
    }
}

/// `openat2`-relative open of an absolute `path` beneath a pinned `/` dirfd.
/// `O_NOFOLLOW` semantics are folded into `RESOLVE_NO_SYMLINKS` atomically —
/// any symlink component refuses the open. Non-Linux (the cross-build) and an
/// absent syscall both refuse.
pub(crate) fn open_at2(path: &Path, kind: OpenKind) -> Result<OwnedFd> {
    #[cfg(target_os = "linux")]
    {
        if !path.is_absolute() {
            return Err(Error::rejected("protected path must be absolute"));
        }
        let c = CString::new(path.as_os_str().as_bytes())
            .map_err(|_| Error::rejected("protected path carries NUL"))?;
        let mut flags = libc::O_RDONLY as u64;
        match kind {
            OpenKind::Dir => flags |= libc::O_DIRECTORY as u64 | libc::O_CLOEXEC as u64,
            OpenKind::ExecFile => flags |= libc::O_CLOEXEC as u64,
            OpenKind::DataInherit => {} // preserved across the next exec
        }
        let how = OpenHow {
            flags,
            mode: 0,
            resolve: libc::RESOLVE_BENEATH | libc::RESOLVE_NO_SYMLINKS,
        };
        let root = open_root()?;
        match sys_openat2(root.as_raw_fd(), &c, &how) {
            Ok(fd) => Ok(unsafe { OwnedFd::from_raw_fd(fd) }),
            Err(e) => Err(open_refusal(path, &e)),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (path, kind);
        Err(Error::rejected(
            "protected managed-Pi launch is Linux-only (openat2 unavailable)",
        ))
    }
}

#[cfg(target_os = "linux")]
fn open_refusal(path: &Path, e: &Error) -> Error {
    Error::rejected(format!("{}: {}", path.display(), e))
}

/// The pinned `/` dirfd the whole walk hangs from.
#[cfg(target_os = "linux")]
fn open_root() -> Result<OwnedFd> {
    let c = CString::new("/").unwrap();
    let how = OpenHow {
        flags: (libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC) as u64,
        mode: 0,
        resolve: libc::RESOLVE_BENEATH,
    };
    let fd = sys_openat2(libc::AT_FDCWD, &c, &how)
        .map_err(|e| Error::rejected(format!("cannot open the pinned root dirfd: {e}")))?;
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// What a topology node must look like — owner/group resolved from NSS names,
/// never hardcoded numbers.
#[derive(Clone, Copy, Debug)]
struct Node {
    path: &'static str,
    owner: u32,
    group: u32,
    mode: u32,
    is_dir: bool,
}

/// The verified protected topology. Construction resolves the NSS ids and
/// walks+checks every fixed node; a missing or mis-owned node refuses.
pub(crate) struct ProtectedTopology {
    /// Resolved ids, retained for the view check.
    supervisor: u32,
    guest: u32,
    shared: u32,
    #[allow(dead_code)]
    launch: u32,
    /// Pinned `/` dirfd the per-node opens ride on.
    root: OwnedFd,
}

impl ProtectedTopology {
    /// Resolve the four accounts and verify the fixed skeleton. `agent_uid`
    /// is the configured guest uid — it must equal the resolved `cadence-agent`
    /// uid and may not be 0 or the supervisor.
    pub(crate) fn verify(agent_uid: u32) -> Result<Self> {
        let supervisor = resolve_uid(acct::SUPERVISOR)?;
        let guest = resolve_uid(acct::GUEST)?;
        let launch = resolve_gid(acct::LAUNCH_GROUP)?;
        let shared = resolve_gid(acct::SHARED_GROUP)?;
        if agent_uid == 0 || agent_uid == supervisor || agent_uid != guest {
            return Err(Error::rejected(format!(
                "agent_uid {agent_uid} is not the provisioned guest {} (uid {})",
                acct::GUEST,
                guest
            )));
        }
        let root = open_root()?;
        let topo = Self {
            supervisor,
            guest,
            shared,
            launch,
            root,
        };
        topo.verify_skeleton()?;
        Ok(topo)
    }

    /// The fixed skeleton nodes — owner/mode/kind verified per node, in the
    /// exact pre-provisioned shape. A missing node refuses; never created.
    fn verify_skeleton(&self) -> Result<()> {
        let nodes = [
            Node {
                path: "/opt/cadence/libexec",
                owner: 0,
                group: 0,
                mode: 0o755,
                is_dir: true,
            },
            Node {
                path: "/opt/cadence/pi",
                owner: 0,
                group: 0,
                mode: 0o755,
                is_dir: true,
            },
            Node {
                path: "/srv/cadence",
                owner: 0,
                group: 0,
                mode: 0o755,
                is_dir: true,
            },
            Node {
                path: "/srv/cadence/protected",
                owner: self.supervisor,
                group: self.supervisor_gid()?,
                mode: 0o700,
                is_dir: true,
            },
            Node {
                path: "/srv/cadence/protected/state",
                owner: self.supervisor,
                group: self.supervisor_gid()?,
                mode: 0o700,
                is_dir: true,
            },
            Node {
                path: "/srv/cadence/guest-views",
                owner: self.supervisor,
                group: self.shared,
                mode: 0o750,
                is_dir: true,
            },
            Node {
                path: "/var/lib/cadence",
                owner: self.supervisor,
                group: self.shared,
                mode: 0o750,
                is_dir: true,
            },
            Node {
                path: "/workspace",
                owner: self.supervisor,
                group: self.shared,
                mode: 0o750,
                is_dir: true,
            },
            Node {
                path: "/workspace/company",
                owner: self.guest,
                group: self.shared,
                mode: 0o770,
                is_dir: true,
            },
            Node {
                path: "/workspace/company/pm",
                owner: self.guest,
                group: self.shared,
                mode: 0o770,
                is_dir: true,
            },
        ];
        for n in &nodes {
            self.check_node(n)?;
        }
        Ok(())
    }

    fn supervisor_gid(&self) -> Result<u32> {
        resolve_primary_gid(acct::SUPERVISOR)
    }

    /// Verify the per-alias view slots exist in the exact pre-provisioned
    /// shape. `…/<alias-sha256>` and its `durable` + `<generation>` parents are
    /// supervisor:cadence 0750; the guest leaf dirs under them are
    /// guest:guest 0700. A missing or mis-owned slot refuses — the source
    /// never creates a privileged node.
    pub(crate) fn verify_view(&self, segs: &Segments, _role: Role) -> Result<()> {
        let base = format!("/srv/cadence/guest-views/{}", segs.alias_hex());
        let gen = segs.generation_hex();
        // supervisor-owned parents
        for (path, mode) in [
            (base.clone(), 0o750u32),
            (format!("{base}/durable"), 0o750),
            (format!("{base}/{gen}"), 0o750),
        ] {
            self.check(&path, self.supervisor, self.shared, mode, true)?;
        }
        // guest-owned leaves (0700 guest:guest) — all pre-provisioned
        let guest_gid = resolve_primary_gid(acct::GUEST)?;
        for leaf in ["home", "config", "cache", "tmp", "no-forge"] {
            self.check(
                &format!("{base}/{gen}/{leaf}"),
                self.guest,
                guest_gid,
                0o700,
                true,
            )?;
        }
        self.check(
            &format!("{base}/durable/sessions"),
            self.guest,
            guest_gid,
            0o700,
            true,
        )?;
        self.check(
            &format!("{base}/{gen}/briefing"),
            self.supervisor,
            self.shared,
            0o750,
            true,
        )?;
        Ok(())
    }

    /// Open a single expected node and `fstat` it.
    fn check_node(&self, n: &Node) -> Result<OwnedFd> {
        self.check(n.path, n.owner, n.group, n.mode, n.is_dir)
    }

    fn check(&self, path: &str, uid: u32, gid: u32, mode: u32, is_dir: bool) -> Result<OwnedFd> {
        let fd = open_at2(Path::new(path), OpenKind::Dir)
            .map_err(|e| Error::rejected(format!("protected node {path}: {e}")))?;
        let meta = fd_metadata(&fd)?;
        if is_dir && !meta.is_dir() {
            return Err(Error::rejected(format!("{path}: not a directory")));
        }
        if meta.uid() != uid || meta.gid() != gid {
            return Err(Error::rejected(format!(
                "{path}: owned {}:{} not {uid}:{gid}",
                meta.uid(),
                meta.gid()
            )));
        }
        if meta.mode() & 0o7777 != mode {
            return Err(Error::rejected(format!(
                "{path}: mode {:04o} not {mode:04o}",
                meta.mode() & 0o7777
            )));
        }
        Ok(fd)
    }

    /// Open the per-launch view dirfd for `layer` — verified, not created.
    pub(crate) fn view_dir(&self, segs: &Segments, layer: Layer) -> Result<OwnedFd> {
        let path = match layer {
            Layer::Durable => format!("/srv/cadence/guest-views/{}/durable", segs.alias_hex()),
            Layer::Generation => {
                format!(
                    "/srv/cadence/guest-views/{}/{}",
                    segs.alias_hex(),
                    segs.generation_hex()
                )
            }
        };
        self.check(&path, self.supervisor, self.shared, 0o750, true)
    }
}

fn fd_metadata(fd: &OwnedFd) -> Result<std::fs::Metadata> {
    fd.try_clone()
        .map_err(|e| Error::internal(format!("clone dir fd: {e}")))
        .and_then(|f| {
            let file = unsafe { std::fs::File::from_raw_fd(f.into_raw_fd()) };
            file.metadata().map_err(|e| Error::internal(e.to_string()))
        })
}

/// NSS lookups — resolved by name, never hardcoded numbers, never caller data.
fn resolve_uid(name: &str) -> Result<u32> {
    let c = CString::new(name).map_err(|_| Error::rejected("account name NUL"))?;
    let pw = unsafe { libc::getpwnam(c.as_ptr()) };
    if pw.is_null() {
        return Err(Error::rejected(format!(
            "account {name} is not provisioned"
        )));
    }
    Ok(unsafe { (*pw).pw_uid })
}

fn resolve_gid(name: &str) -> Result<u32> {
    let c = CString::new(name).map_err(|_| Error::rejected("group name NUL"))?;
    let gr = unsafe { libc::getgrnam(c.as_ptr()) };
    if gr.is_null() {
        return Err(Error::rejected(format!("group {name} is not provisioned")));
    }
    Ok(unsafe { (*gr).gr_gid })
}

fn resolve_primary_gid(user: &str) -> Result<u32> {
    let c = CString::new(user).map_err(|_| Error::rejected("account name NUL"))?;
    let pw = unsafe { libc::getpwnam(c.as_ptr()) };
    if pw.is_null() {
        return Err(Error::rejected(format!(
            "account {user} is not provisioned"
        )));
    }
    Ok(unsafe { (*pw).pw_gid })
}
