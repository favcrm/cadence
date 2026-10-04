//! Helper-private protected profile: all inherited FDs have already been closed.
//! Only this module opens Node, and only its owned descriptor crosses the seal.
use crate::protected_pi_profile::{Profile, NODE_DIGEST, NODE_PATH};
use sha2::{Digest, Sha256};
use std::ffi::CString;
use std::fs::File;
use std::io;
use std::os::unix::fs::{FileExt, MetadataExt};
use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd};

/// No observed candidate CLI layout elects a reviewed immutable JS graph.
const PRIVATE_PI_CLI: Option<&str> = None;

/// Kept inside the helper; no public constructor, argv field or inherited fd.
struct PrivateView {
    cwd: OwnedFd,
    env: Vec<CString>,
    session: Option<String>,
}
#[derive(PartialEq, Eq)]
#[allow(dead_code)] // Only unavailable private authority may select the role.
enum PrivateRole {
    Master,
    Worker,
}
struct ViewAuthority {
    supervisor: u32,
    guest: u32,
    guest_gid: u32,
    shared: u32,
    alias: String,
    role: PrivateRole,
    model: String,
}
fn private_authority(profile: &Profile) -> io::Result<ViewAuthority> {
    // Selectors are never proof. No release/namespace/current-grant backend
    // exists here, and VerifiedGrant alone is explicitly NOT launch authority.
    let _ = (profile.alias_sha256(), profile.generation());
    Err(io::Error::new(
        io::ErrorKind::PermissionDenied,
        "protected private graph/view/namespace/grant/current authority unavailable",
    ))
}
fn private_view(profile: &Profile) -> io::Result<PrivateView> {
    let authority = private_authority(profile)?;
    if profile.routing().no_session() != (authority.role == PrivateRole::Master)
        || profile.routing().model() != Some(authority.model.as_str())
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "private role/model routing mismatch",
        ));
    }
    let guest = super::account_named(super::policy::AGENT_USER).ok_or_else(|| {
        io::Error::new(io::ErrorKind::PermissionDenied, "fixed guest unavailable")
    })?;
    let supervisor = super::account_named("cadence-supervisor").ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            "fixed supervisor unavailable",
        )
    })?;
    if guest.uid != authority.guest
        || guest.gid != authority.guest_gid
        || supervisor.uid != authority.supervisor
        || super::group_named(super::policy::SHARED_GROUP) != Some(authority.shared)
        || authority.guest_gid == 0
        || authority.shared == 0
        || authority.supervisor == 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "private/fixed NSS identity mismatch",
        ));
    }
    let alias_digest: [u8; 32] = Sha256::digest(authority.alias.as_bytes()).into();
    let expected_alias: String = alias_digest.iter().map(|b| format!("{b:02x}")).collect();
    if expected_alias != profile.alias_sha256()
        || authority.guest == 0
        || authority.supervisor == authority.guest
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "private profile identity mismatch",
        ));
    }
    let base = format!("/srv/cadence/guest-views/{}", profile.alias_sha256());
    let view = format!("{base}/{}", profile.generation());
    let mut policy = vec![
        (
            "/srv/cadence/guest-views".to_owned(),
            authority.supervisor,
            authority.shared,
            0o750,
        ),
        (base.clone(), authority.supervisor, authority.shared, 0o750),
        (
            format!("{base}/durable"),
            authority.supervisor,
            authority.shared,
            0o750,
        ),
        (view.clone(), authority.supervisor, authority.shared, 0o750),
        (
            format!("{base}/durable/sessions"),
            authority.guest,
            authority.guest_gid,
            0o700,
        ),
        (
            "/workspace".to_owned(),
            authority.supervisor,
            authority.shared,
            0o750,
        ),
        (
            "/workspace/company".to_owned(),
            authority.guest,
            authority.shared,
            0o770,
        ),
    ];
    for leaf in ["home", "config", "cache", "tmp", "no-forge"] {
        policy.push((
            format!("{view}/{leaf}"),
            authority.guest,
            authority.guest_gid,
            0o700,
        ));
    }
    for leaf in ["home", "config", "cache", "tmp", "no-forge"] {
        let _verified = open_view_dir(&format!("{view}/{leaf}"), &policy)?;
    }
    let _sessions = open_view_dir(&format!("{base}/durable/sessions"), &policy)?;
    let cwd = open_view_dir("/workspace/company", &policy)?;
    let env = derived_env(&view, &authority.alias)?;
    Ok(PrivateView {
        cwd,
        env,
        session: Some(format!("{base}/durable/sessions/session.json")),
    })
}
fn derived_env(view: &str, alias: &str) -> io::Result<Vec<CString>> {
    // Finite helper-derived set. No getenv, caller --env or provider prefix.
    [
        format!("HOME={view}/home"),
        format!("PI_CODING_AGENT_DIR={view}/config"),
        format!("XDG_CACHE_HOME={view}/cache"),
        format!("TMPDIR={view}/tmp"),
        format!("GH_CONFIG_DIR={view}/no-forge"),
        format!("GIT_CONFIG_GLOBAL={view}/config/gitconfig"),
        "GIT_TERMINAL_PROMPT=0".into(),
        "PI_OFFLINE=1".into(),
        "CADENCE_SOCKET=/var/lib/cadence/cadence.sock".into(),
        format!("CADENCE_ALIAS={alias}"),
        "CADENCE_PM_DIR=/workspace/company/pm".into(),
        "PATH=/opt/cadence/bin:/usr/bin:/bin".into(),
    ]
    .into_iter()
    .map(|s| {
        CString::new(s).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "private env NUL"))
    })
    .collect()
}
fn open_view_dir(path: &str, policy: &[(String, u32, u32, u32)]) -> io::Result<OwnedFd> {
    let root = File::open("/")?;
    let mut dir: OwnedFd = root.into();
    let mut walked = String::new();
    for part in path
        .strip_prefix('/')
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "private path"))?
        .split('/')
    {
        if part.is_empty() || part == "." || part == ".." {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "private path segment",
            ));
        }
        let child = open_child(dir.as_raw_fd(), part, true)?;
        walked.push('/');
        walked.push_str(part);
        let m = File::from(child.try_clone()?).metadata()?;
        let valid =
            if let Some((_, uid, gid, mode)) = policy.iter().find(|(p, _, _, _)| p == &walked) {
                m.uid() == *uid && m.gid() == *gid && m.mode() & 0o7777 == *mode
            } else {
                m.uid() == 0 && m.mode() & 0o022 == 0
            };
        if !m.is_dir() || !valid {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "private view ancestor custody",
            ));
        }
        dir = child;
    }
    Ok(dir)
}
/// Real production preparation used by run, also called by the independent check.
/// There is no test override, marker, signer, boolean or caller-chosen pin port.
pub(super) fn prepare(profile: Profile) -> io::Result<OwnedLaunch> {
    let cli = PRIVATE_PI_CLI.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            "protected release/immutable Pi graph pin unavailable",
        )
    })?;
    if NODE_DIGEST.is_none() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "protected Node pin unavailable",
        ));
    }
    let view = private_view(&profile)?;
    super::protected_effect(); // Actual privately-owned Node-open attempt site.
    let node = open_node()?;
    // Disable writable/config-discovered extensions. Any future approved
    // immutable extension set is private graph policy, never caller -e paths.
    let mut argv = vec![
        NODE_PATH.to_owned(),
        cli.to_owned(),
        "--no-extensions".into(),
    ];
    argv.extend(profile.routing().tokens());
    // Workers' session paths are private policy-derived, never caller argv.
    if !profile.routing().no_session() {
        argv.extend([
            "--session".into(),
            view.session
                .as_ref()
                .ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "private worker session unavailable",
                    )
                })?
                .clone(),
        ]);
    }
    OwnedLaunch::new(profile, node, view.cwd, argv, view.env)
}
#[repr(C)]
struct OpenHow {
    flags: u64,
    mode: u64,
    resolve: u64,
}
fn open_child(parent: i32, name: &str, directory: bool) -> io::Result<OwnedFd> {
    let name =
        CString::new(name).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path NUL"))?;
    let how = OpenHow {
        flags: (libc::O_RDONLY
            | libc::O_CLOEXEC
            | libc::O_NOFOLLOW
            | if directory {
                libc::O_DIRECTORY
            } else {
                libc::O_NONBLOCK
            }) as u64,
        mode: 0,
        resolve: libc::RESOLVE_BENEATH | libc::RESOLVE_NO_SYMLINKS,
    };
    let fd = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            parent,
            name.as_ptr(),
            &how as *const OpenHow,
            std::mem::size_of::<OpenHow>(),
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd as i32) })
}
fn open_node() -> io::Result<File> {
    let expected = NODE_DIGEST.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Node release pin unavailable",
        )
    })?;
    let root = CString::new("/").unwrap();
    let fd = unsafe {
        libc::open(
            root.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut directory = unsafe { OwnedFd::from_raw_fd(fd) };
    let parts: Vec<_> = NODE_PATH
        .strip_prefix('/')
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Node path not fixed absolute"))?
        .split('/')
        .collect();
    for (i, part) in parts.iter().enumerate() {
        let m = File::from(directory.try_clone()?).metadata()?;
        if !m.is_dir() || m.uid() != 0 || m.mode() & 0o022 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "mutable/non-root Node ancestor",
            ));
        }
        if part.is_empty() || *part == "." || *part == ".." {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "noncanonical fixed Node path",
            ));
        }
        let leaf = i + 1 == parts.len();
        let child = open_child(directory.as_raw_fd(), part, !leaf)?;
        if leaf {
            return verify_file(File::from(child), 0, 0o755, expected);
        }
        directory = child;
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        "empty Node path",
    ))
}
#[derive(PartialEq, Eq)]
struct FileIdentity {
    dev: u64,
    ino: u64,
    size: u64,
    uid: u32,
    gid: u32,
    mode: u32,
    nlink: u64,
    mtime: i64,
    mtime_ns: i64,
    ctime: i64,
    ctime_ns: i64,
}
fn identity(m: &std::fs::Metadata) -> FileIdentity {
    FileIdentity {
        dev: m.dev(),
        ino: m.ino(),
        size: m.size(),
        uid: m.uid(),
        gid: m.gid(),
        mode: m.mode(),
        nlink: m.nlink(),
        mtime: m.mtime(),
        mtime_ns: m.mtime_nsec(),
        ctime: m.ctime(),
        ctime_ns: m.ctime_nsec(),
    }
}
/// Offset-zero, bounded and metadata-stable; private test seam is measurement,
/// not production eligibility. The production caller above supplies fixed pins.
fn verify_file(file: File, owner: u32, mode: u32, expected: [u8; 32]) -> io::Result<File> {
    let before = file.metadata()?;
    if !before.is_file()
        || before.uid() != owner
        || before.mode() & 0o7777 != mode
        || before.nlink() != 1
        || !(64..=512 * 1024 * 1024).contains(&before.size())
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Node type/owner/mode/link/size",
        ));
    }
    let mut header = [0u8; 20];
    file.read_exact_at(&mut header, 0)?;
    #[cfg(target_arch = "x86_64")]
    let host = 62u16;
    #[cfg(target_arch = "aarch64")]
    let host = 183u16;
    if &header[..4] != b"\x7fELF"
        || header[4..7] != [2, 1, 1]
        || ![2, 3].contains(&u16::from_le_bytes([header[16], header[17]]))
        || u16::from_le_bytes([header[18], header[19]]) != host
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Node host ELF identity",
        ));
    }
    let mut hash = Sha256::new();
    let mut at = 0;
    let mut buf = [0u8; 65536];
    while at < before.size() {
        let want = std::cmp::min(buf.len() as u64, before.size() - at) as usize;
        let n = file.read_at(&mut buf[..want], at)?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "Node truncated",
            ));
        }
        hash.update(&buf[..n]);
        at += n as u64;
    }
    let digest: [u8; 32] = hash.finalize().into();
    if digest != expected || identity(&before) != identity(&file.metadata()?) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Node digest or metadata drift",
        ));
    }
    Ok(file)
}
/// Owning all CString backing/pointers and private fds, built before the seal.
/// Never an arbitrary descriptor supplied through the helper CLI.
pub(super) struct OwnedLaunch {
    profile: Profile,
    node: File,
    cwd: OwnedFd,
    argv: Box<[CString]>,
    env: Box<[CString]>,
    argv_p: Vec<*const libc::c_char>,
    env_p: Vec<*const libc::c_char>,
}
impl OwnedLaunch {
    fn new(
        profile: Profile,
        node: File,
        cwd: OwnedFd,
        argv: Vec<String>,
        env: Vec<CString>,
    ) -> io::Result<Self> {
        let argv: Box<[CString]> = argv
            .into_iter()
            .map(|s| {
                CString::new(s).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "argv NUL"))
            })
            .collect::<io::Result<Vec<_>>>()?
            .into_boxed_slice();
        let env = env.into_boxed_slice();
        let ptrs = |s: &[CString]| {
            s.iter()
                .map(|v| v.as_ptr())
                .chain(std::iter::once(std::ptr::null()))
                .collect()
        };
        let argv_p = ptrs(&argv);
        let env_p = ptrs(&env);
        Ok(Self {
            profile,
            node,
            cwd,
            argv,
            env,
            argv_p,
            env_p,
        })
    }
    /// Called ONLY after run proves UID/GID/groups and the capability/NNP seal.
    /// The CLOEXEC Node fd stays open until execveat consumes the inode; no
    /// broad inherited fd survives. Error captures errno before RAII cleanup.
    pub(super) fn exec(self) -> io::Result<()> {
        // Recheck the real private current gate immediately after the seal;
        // a stale preparation fact never authorizes descriptor execution.
        let _current = private_authority(&self.profile)?;
        self.exec_inode()
    }
    fn exec_inode(self) -> io::Result<()> {
        // Keep all owned backing visibly live across both syscalls.
        let _backing = (&self.argv, &self.env);
        super::protected_effect();
        if unsafe { libc::fchdir(self.cwd.as_raw_fd()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        super::protected_effect();
        let rc = unsafe {
            libc::syscall(
                libc::SYS_execveat,
                self.node.as_raw_fd(),
                c"".as_ptr(),
                self.argv_p.as_ptr(),
                self.env_p.as_ptr(),
                libc::AT_EMPTY_PATH,
            )
        };
        let error = io::Error::last_os_error();
        if rc == -1 {
            Err(error)
        } else {
            Err(io::Error::other("execveat unexpectedly returned"))
        }
    }
}
#[cfg(test)]
#[path = "tests/protected_mechanics.rs"]
mod tests;
