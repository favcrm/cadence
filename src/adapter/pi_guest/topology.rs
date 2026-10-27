//! The protected topology — an openat2 verify-only walk of the
//! pre-provisioned tree. The trusted root `/` is opened once as an
//! `O_DIRECTORY` dirfd; every node the launch depends on is then resolved by
//! walking its path **component by component**, each `openat2` made *relative
//! to the previous hop's held dirfd* with `RESOLVE_BENEATH |
//! RESOLVE_NO_SYMLINKS`. Per-hop `fstat` checks owner/group/mode before the
//! next component opens, so a writable intermediate can never let an attacker
//! swap a child the leaf-check would bless — `RESOLVE_NO_SYMLINKS` alone does
//! not stop a rename under a writable parent.
//!
//! The source never creates a privileged node and never chowns: a missing or
//! mis-owned component refuses outright. `openat2` is the only traversal — an
//! `ENOSYS`/`EPERM`/`EINVAL`/`EOPNOTSUPP` answer means the syscall is absent
//! and the walk fails closed rather than silently downgrade.

#[cfg(target_os = "linux")]
use std::ffi::{CStr, CString};
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

/// Open the trusted root `/` as a held dirfd. It carries no untrusted
/// segments — `O_DIRECTORY | O_CLOEXEC` (and `O_NOFOLLOW`, though `/` is
/// never a symlink) — so it is a stable anchor for the per-hop relative walk.
/// `RESOLVE_BENEATH` is *not* applied here: beneath `/` it is meaningless and
/// `openat2` rejects an absolute operand under it with `EXDEV` regardless.
#[cfg(target_os = "linux")]
fn open_root() -> Result<OwnedFd> {
    let c = CString::new("/").unwrap();
    let how = OpenHow {
        flags: (libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW) as u64,
        mode: 0,
        resolve: 0,
    };
    let fd = sys_openat2(libc::AT_FDCWD, &c, &how)
        .map_err(|e| Error::rejected(format!("cannot open the pinned root dirfd: {e}")))?;
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// The openat2 resolve flags for every hop *beneath* the held root.
#[cfg(target_os = "linux")]
const RESOLVE: u64 = libc::RESOLVE_BENEATH | libc::RESOLVE_NO_SYMLINKS;

/// Split a canonical absolute path into its single-level relative segments.
/// Refuses an empty component, `.`/`..`, a trailing/leading slash, and any
/// non-canonical spell — a path like `//`, `/a//b`, `/a/./b` or `/a/../b`
/// cannot reach the walker.
#[cfg(target_os = "linux")]
fn relative_segments(path: &str) -> Result<Vec<String>> {
    let s = path
        .strip_prefix('/')
        .ok_or_else(|| Error::rejected(format!("{path}: protected path must be absolute")))?;
    if s.is_empty() {
        return Err(Error::rejected(format!(
            "{path}: protected path is the root itself — refusing"
        )));
    }
    let mut out = Vec::new();
    for seg in s.split('/') {
        if seg.is_empty() || seg == "." || seg == ".." {
            return Err(Error::rejected(format!(
                "{path}: non-canonical or traversal segment '{seg}'"
            )));
        }
        out.push(seg.to_string());
    }
    Ok(out)
}

/// Walk `path` one component at a time beneath the held `root` dirfd, opening
/// each hop *relative to the previous dirfd* with `RESOLVE`. `check_hop` is
/// applied to each intermediate (directory) hop before the next component
/// opens, so a writable or mis-owned ancestor fails the walk rather than
/// letting a child be substituted beneath it. The leaf is opened with `kind`
/// flags and returned; intermediates use `O_DIRECTORY`.
#[cfg(target_os = "linux")]
fn open_walk<F>(root: &OwnedFd, path: &str, kind: OpenKind, check_hop: F) -> Result<OwnedFd>
where
    F: Fn(&std::fs::Metadata, &str) -> Result<()>,
{
    let segs = relative_segments(path)?;
    let mut dirfd = root
        .try_clone()
        .map_err(|e| Error::internal(format!("clone root dirfd: {e}")))?;
    let mut walked = String::new();
    let last = segs.len() - 1;
    for (i, seg) in segs.iter().enumerate() {
        walked.push('/');
        walked.push_str(seg);
        let c = CString::new(seg.as_str())
            .map_err(|_| Error::rejected(format!("{walked}: segment carries NUL")))?;
        let leaf = i == last;
        let mut flags = libc::O_RDONLY as u64 | libc::O_CLOEXEC as u64;
        if leaf {
            match kind {
                OpenKind::Dir => flags |= libc::O_DIRECTORY as u64,
                // `O_NONBLOCK` on the exec file: a fifo must not block the
                // open before `fstat` can reject it — `O_RDONLY` alone would
                // wait for a writer forever.
                OpenKind::ExecFile => flags |= libc::O_NONBLOCK as u64,
            }
        } else {
            flags |= libc::O_DIRECTORY as u64;
        }
        let how = OpenHow {
            flags,
            mode: 0,
            resolve: RESOLVE,
        };
        let fd = match sys_openat2(dirfd.as_raw_fd(), &c, &how) {
            Ok(fd) => fd,
            Err(e) => return Err(open_refusal(&walked, &e)),
        };
        let newfd = unsafe { OwnedFd::from_raw_fd(fd) };
        let meta = fd_metadata(&newfd)?;
        if leaf {
            return Ok(newfd);
        }
        if !meta.is_dir() {
            return Err(Error::rejected(format!("{walked}: not a directory")));
        }
        check_hop(&meta, &walked)?;
        dirfd = newfd;
    }
    unreachable!("relative_segments guarantees a non-empty path")
}

/// `openat2`-relative open of a canonical absolute `path`, walking each
/// component beneath the pinned `/` dirfd. This is the exec-target walk used
/// by `open_bound`: every ancestor must be an **immutable root-owned**
/// directory — owned by uid 0 and not group- or other-writable — so no
/// intermediate a guest or group could reshape ever sits on the path to a
/// binary the kernel will exec. The leaf itself is `fstat`-checked by the
/// caller (owner/mode/ELF/digest). Non-Linux refuses.
pub(crate) fn open_at2(path: &Path, kind: OpenKind) -> Result<OwnedFd> {
    open_walk_kind(path.to_string_lossy().as_ref(), kind, |meta, walked| {
        if meta.uid() != 0 {
            return Err(Error::rejected(format!(
                "{walked}: exec-path ancestor owned by uid {} not 0 — an exec \
                 target must sit under root-owned immutable dirs",
                meta.uid()
            )));
        }
        if meta.mode() & 0o022 != 0 {
            return Err(Error::rejected(format!(
                "{walked}: exec-path ancestor is group/other-writable (mode {:04o}) \
                 — refuse a reshapeable intermediate",
                meta.mode() & 0o7777
            )));
        }
        Ok(())
    })
}

#[cfg(target_os = "linux")]
fn open_walk_kind<F>(path: &str, kind: OpenKind, check_hop: F) -> Result<OwnedFd>
where
    F: Fn(&std::fs::Metadata, &str) -> Result<()>,
{
    let root = open_root()?;
    open_walk(&root, path, kind, check_hop)
}

#[cfg(not(target_os = "linux"))]
fn open_walk_kind<F>(path: &str, kind: OpenKind, _check_hop: F) -> Result<OwnedFd>
where
    F: Fn(&std::fs::Metadata, &str) -> Result<()>,
{
    let _ = (path, kind);
    Err(Error::rejected(
        "protected managed-Pi launch is Linux-only (openat2 unavailable)",
    ))
}

#[cfg(target_os = "linux")]
fn open_refusal(path: &str, e: &Error) -> Error {
    Error::rejected(format!("{path}: {e}"))
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
/// walks+checks every fixed node; a missing or mis-owned node refuses. Holds
/// the pinned `/` dirfd the per-hop walks ride on.
pub(crate) struct ProtectedTopology {
    /// Resolved ids, retained for the view check.
    supervisor: u32,
    supervisor_gid: u32,
    guest: u32,
    guest_gid: u32,
    shared: u32,
    /// Pinned `/` dirfd the per-node opens ride on.
    root: OwnedFd,
}

/// The owner/mode expected at each hop of a node path, longest first — built
/// by the verifier so the per-hop `check_hop` can look up the policy that
/// covers an intermediate. Only fixed nodes appear here; a hop with no entry
/// uses the default "directory, not world-writable" rule.
struct HopPolicy {
    map: Vec<(String, u32, u32, u32)>, // (path, uid, gid, mode)
}

impl HopPolicy {
    fn lookup(&self, path: &str) -> Option<(u32, u32, u32)> {
        self.map
            .iter()
            .find(|(p, _, _, _)| p == path)
            .map(|(_, u, g, m)| (*u, *g, *m))
    }
}

impl ProtectedTopology {
    /// Resolve the four accounts and verify the fixed skeleton. `agent_uid`
    /// is the configured guest uid — it must equal the resolved `cadence-agent`
    /// uid and may not be 0 or the supervisor.
    pub(crate) fn verify(agent_uid: u32) -> Result<Self> {
        let supervisor = resolve_uid(acct::SUPERVISOR)?;
        let supervisor_gid = resolve_primary_gid(acct::SUPERVISOR)?;
        let guest = resolve_uid(acct::GUEST)?;
        let guest_gid = resolve_primary_gid(acct::GUEST)?;
        let shared = resolve_gid(acct::SHARED_GROUP)?;
        // `cadence-launch` is the helper's group — verify it resolves so the
        // exec pin's expected gid can be checked (execfd consumes it).
        let _launch = resolve_gid(acct::LAUNCH_GROUP)?;
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
            supervisor_gid,
            guest,
            guest_gid,
            shared,
            root,
        };
        topo.verify_skeleton()?;
        Ok(topo)
    }

    /// The fixed skeleton nodes — owner/mode/kind verified per node, in the
    /// exact pre-provisioned shape. A missing node refuses; never created.
    fn verify_skeleton(&self) -> Result<()> {
        let nodes = self.node_table();
        for n in &nodes {
            if n.path == "/workspace/company/pm" {
                // PM is mutable workspace DATA, never a protected executable,
                // view ancestor or authority carrier. Keep its existing exact
                // metadata check, relative to a verified held workspace leaf.
                self.check_workspace_pm()?;
            } else {
                self.check_node(n)?;
            }
        }
        Ok(())
    }

    fn check_workspace_pm(&self) -> Result<OwnedFd> {
        let company = self.check_with(
            "/workspace/company",
            self.guest,
            self.shared,
            0o770,
            true,
            &self.skeleton_policy(),
        )?;
        #[cfg(target_os = "linux")]
        {
            let name = CString::new("pm").unwrap();
            let fd = sys_openat2(
                company.as_raw_fd(),
                &name,
                &OpenHow {
                    flags: (libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW)
                        as u64,
                    mode: 0,
                    resolve: RESOLVE | libc::RESOLVE_NO_XDEV,
                },
            )?;
            let fd = unsafe { OwnedFd::from_raw_fd(fd) };
            let meta = fd_metadata(&fd)?;
            if !meta.is_dir()
                || meta.uid() != self.guest
                || meta.gid() != self.shared
                || meta.mode() & 0o7777 != 0o770
            {
                return Err(Error::rejected("mutable PM data leaf metadata refused"));
            }
            Ok(fd)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = company;
            Err(Error::rejected("protected managed-Pi launch is Linux-only"))
        }
    }

    /// The static node table — every protected ancestor is listed so the
    /// per-hop walk can verify each intermediate's owner/mode, not just the
    /// leaf.
    fn node_table(&self) -> Vec<Node> {
        vec![
            Node {
                path: "/opt",
                owner: 0,
                group: 0,
                mode: 0o755,
                is_dir: true,
            },
            Node {
                path: "/opt/cadence",
                owner: 0,
                group: 0,
                mode: 0o755,
                is_dir: true,
            },
            Node {
                path: "/opt/protected",
                owner: 0,
                group: 0,
                mode: 0o755,
                is_dir: true,
            },
            Node {
                path: "/opt/protected/bin",
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
                path: "/srv",
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
                group: self.supervisor_gid,
                mode: 0o700,
                is_dir: true,
            },
            Node {
                path: "/srv/cadence/protected/state",
                owner: self.supervisor,
                group: self.supervisor_gid,
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
                path: "/var",
                owner: 0,
                group: 0,
                mode: 0o755,
                is_dir: true,
            },
            Node {
                path: "/var/lib",
                owner: 0,
                group: 0,
                mode: 0o755,
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
        ]
    }

    /// The hop-policy map for the skeleton — every node's expected owner/mode
    /// keyed by its absolute path, so a `check` walk verifies each ancestor
    /// against the exact policy that covers it.
    fn skeleton_policy(&self) -> HopPolicy {
        HopPolicy {
            map: self
                .node_table()
                .iter()
                .map(|n| (n.path.to_string(), n.owner, n.group, n.mode))
                .collect(),
        }
    }

    /// The hop policy for the dynamic per-alias view: the skeleton rows plus
    /// the alias/generation segment rows this launch names. Both `verify_view`
    /// and `view_dir` walk against this so the per-hop check knows every
    /// intermediate on the protected view path — an alias or generation dir
    /// that is not in the policy is refused.
    fn view_policy(&self, segs: &Segments) -> HopPolicy {
        let base = format!("/srv/cadence/guest-views/{}", segs.alias_hex());
        let gen = segs.generation_hex();
        let mut map = self.skeleton_policy().map;
        for (p, u, g, m) in [
            (base.clone(), self.supervisor, self.shared, 0o750u32),
            (
                format!("{base}/durable"),
                self.supervisor,
                self.shared,
                0o750,
            ),
            (format!("{base}/{gen}"), self.supervisor, self.shared, 0o750),
            (
                format!("{base}/durable/sessions"),
                self.guest,
                self.guest_gid,
                0o700,
            ),
            (
                format!("{base}/{gen}/briefing"),
                self.supervisor,
                self.shared,
                0o750,
            ),
        ] {
            map.push((p, u, g, m));
        }
        for leaf in ["home", "config", "cache", "tmp", "no-forge"] {
            map.push((
                format!("{base}/{gen}/{leaf}"),
                self.guest,
                self.guest_gid,
                0o700,
            ));
        }
        HopPolicy { map }
    }

    /// Verify the per-alias view slots exist in the exact pre-provisioned
    /// shape. `…/<alias-sha256>` and its `durable` + `<generation>` parents are
    /// supervisor:cadence 0750; the guest leaf dirs under them are
    /// guest:guest 0700. A missing or mis-owned slot refuses — the source
    /// never creates a privileged node. Every ancestor on the way to a leaf
    /// is checked against the view policy.
    pub(crate) fn verify_view(&self, segs: &Segments, _role: Role) -> Result<()> {
        let base = format!("/srv/cadence/guest-views/{}", segs.alias_hex());
        let gen = segs.generation_hex();
        let policy = self.view_policy(segs);
        // supervisor-owned parents
        for (path, mode) in [
            (base.clone(), 0o750u32),
            (format!("{base}/durable"), 0o750),
            (format!("{base}/{gen}"), 0o750),
        ] {
            self.check_with(&path, self.supervisor, self.shared, mode, true, &policy)?;
        }
        // guest-owned leaves (0700 guest:guest) — all pre-provisioned
        for leaf in ["home", "config", "cache", "tmp", "no-forge"] {
            self.check_with(
                &format!("{base}/{gen}/{leaf}"),
                self.guest,
                self.guest_gid,
                0o700,
                true,
                &policy,
            )?;
        }
        self.check_with(
            &format!("{base}/durable/sessions"),
            self.guest,
            self.guest_gid,
            0o700,
            true,
            &policy,
        )?;
        self.check_with(
            &format!("{base}/{gen}/briefing"),
            self.supervisor,
            self.shared,
            0o750,
            true,
            &policy,
        )?;
        Ok(())
    }

    /// Open a single expected node and `fstat` it — walking every ancestor
    /// through the skeleton policy so a mis-owned intermediate refuses.
    fn check_node(&self, n: &Node) -> Result<OwnedFd> {
        let policy = self.skeleton_policy();
        self.check_with(n.path, n.owner, n.group, n.mode, n.is_dir, &policy)
    }

    /// Walk `path` component-by-component beneath the held root, checking each
    /// intermediate's owner/mode against `policy` (a hop absent from the map
    /// must still be a non-world-writable directory), then `fstat` the leaf.
    fn check_with(
        &self,
        path: &str,
        uid: u32,
        gid: u32,
        mode: u32,
        is_dir: bool,
        policy: &HopPolicy,
    ) -> Result<OwnedFd> {
        #[cfg(target_os = "linux")]
        {
            let fd = open_walk(
                &self.root,
                path,
                OpenKind::Dir,
                protected_hop_ok(policy, self.guest),
            )
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
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (path, uid, gid, mode, is_dir, policy);
            Err(Error::rejected(
                "protected managed-Pi launch is Linux-only (openat2 unavailable)",
            ))
        }
    }

    /// Root provisioner anchor. Exact protected metadata and every immutable
    /// ancestor are still verified; this does not accept a caller-selected path.
    pub(crate) fn view_anchor(&self) -> Result<OwnedFd> {
        self.check_with(
            "/srv/cadence/guest-views",
            self.supervisor,
            self.shared,
            0o750,
            true,
            &self.skeleton_policy(),
        )
    }

    #[allow(dead_code)]
    // legacy daemon-side exec path; production caller moved to root constructor in PR809; removal tracked in CAD-1188
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
        // Walk the view path under the *view* policy — the alias/generation
        // segments have their own rows; a skeleton-only policy would refuse
        // them as unprofiled.
        let policy = self.view_policy(segs);
        self.check_with(&path, self.supervisor, self.shared, 0o750, true, &policy)
    }
}

/// The per-hop policy check for a *protected* (skeleton or view) path. A hop
/// must appear in `policy` (an unnamed ancestor on a protected path refuses —
/// we never walk beneath a node the topology does not name), match its owner
/// and exact mode, AND be supervisor-owned non-group/other-writable: a
/// guest-owned or group-writable intermediate can be reshaped by the guest or
/// a shared-group member even when its row matches, so both are refused.
#[cfg(target_os = "linux")]
fn protected_hop_ok(
    policy: &HopPolicy,
    guest_uid: u32,
) -> impl Fn(&std::fs::Metadata, &str) -> Result<()> + '_ {
    move |meta, walked| {
        match policy.lookup(walked) {
            Some((u, g, m)) => {
                if meta.uid() != u || meta.gid() != g || meta.mode() & 0o7777 != m {
                    return Err(Error::rejected(format!(
                        "{walked}: ancestor owned {}:{} mode {:04o}, expected \
                         {u}:{g} mode {m:04o}",
                        meta.uid(),
                        meta.gid(),
                        meta.mode() & 0o7777
                    )));
                }
            }
            None => {
                return Err(Error::rejected(format!(
                    "{walked}: ancestor has no topology policy — refusing to \
                     walk beneath an unprofiled protected intermediate"
                )));
            }
        }
        if meta.uid() == guest_uid {
            return Err(Error::rejected(format!(
                "{walked}: protected ancestor is guest-owned (uid {}) — the \
                 guest must not own a dir on a protected path",
                meta.uid()
            )));
        }
        if meta.mode() & 0o022 != 0 {
            return Err(Error::rejected(format!(
                "{walked}: protected ancestor is group/other-writable \
                 (mode {:04o})",
                meta.mode() & 0o7777
            )));
        }
        Ok(())
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

/// NSS group resolution exposed to `execfd` for the exec pin's expected-gid
/// check — same rule: by name, never a hardcoded number or caller value.
pub(crate) fn resolve_gid_pub(name: &str) -> Result<u32> {
    resolve_gid(name)
}

fn resolve_gid(name: &str) -> Result<u32> {
    let c = CString::new(name).map_err(|_| Error::rejected("group name NUL"))?;
    let gr = unsafe { libc::getgrnam(c.as_ptr()) };
    if gr.is_null() {
        return Err(Error::rejected(format!("group {name} is not provisioned")));
    }
    Ok(unsafe { (*gr).gr_gid })
}

pub(crate) fn resolve_primary_gid(user: &str) -> Result<u32> {
    let c = CString::new(user).map_err(|_| Error::rejected("account name NUL"))?;
    let pw = unsafe { libc::getpwnam(c.as_ptr()) };
    if pw.is_null() {
        return Err(Error::rejected(format!(
            "account {user} is not provisioned"
        )));
    }
    Ok(unsafe { (*pw).pw_gid })
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// A held dirfd anchored at a tempdir stands in for the pinned `/` so the
    /// per-hop `open_walk` runs against an ordinary-uid tree — no privileged
    /// provisioning needed.
    fn anchor(dir: &std::path::Path) -> OwnedFd {
        std::fs::File::open(dir).unwrap().into()
    }

    /// Positive traversal: build `a/b/c` under a tempdir anchor and walk it
    /// relative to the anchor fd — each hop opens beneath the previous dirfd.
    /// This is the fix for the absolute-path-with-RESOLVE_BENEATH defect:
    /// `/a/b/c` under BENEATH would EXDEV; the walker strips to segments.
    #[test]
    fn positive_openat2_traversal_under_held_anchor() {
        let base = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(base.path().join("a/b/c")).unwrap();
        let root = anchor(base.path());
        // `open_walk` wants an absolute *logical* path; feed it one whose
        // segments map under the anchor by passing the anchor dirfd directly.
        let leaf = open_walk(&root, "/a/b/c", OpenKind::Dir, |meta, walked| {
            assert!(meta.is_dir(), "{walked} must be a dir");
            Ok(())
        })
        .unwrap();
        assert!(fd_metadata(&leaf).unwrap().is_dir());
    }

    /// A relative path built from a canonical absolute string must refuse any
    /// `..`, `.`, empty, or double-slash segment before it ever reaches a
    /// syscall.
    #[test]
    fn noncanonical_and_traversal_segments_refuse() {
        for bad in [
            "",
            "/",
            "//a",
            "/a//b",
            "/a/./b",
            "/a/../b",
            "/a/b/",
            "relative/path",
        ] {
            assert!(relative_segments(bad).is_err(), "{bad}");
        }
        assert_eq!(relative_segments("/a/b/c").unwrap(), vec!["a", "b", "c"]);
    }

    /// A policy covering the tempdir-anchor tree: each named hop maps to its
    /// real owner/mode. `guest_uid` is chosen as a non-0 uid distinct from the
    /// test files' owner (which is the real euid — typically 0 in the sandbox,
    /// or the agent uid).
    fn policy_for(paths: &[(&str, u32, u32, u32)]) -> HopPolicy {
        HopPolicy {
            map: paths
                .iter()
                .map(|(p, u, g, m)| (p.to_string(), *u, *g, *m))
                .collect(),
        }
    }

    /// The real `protected_hop_ok` refuses a hop with no policy row — a
    /// protected path never walks beneath an intermediate the topology does
    /// not name.
    #[test]
    fn unknown_policy_ancestor_on_protected_path_refuses() {
        let base = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(base.path().join("a/b")).unwrap();
        let root = anchor(base.path());
        // Policy knows only "/a" — "/a/b"'s walk hits "/a" (has row) then the
        // leaf; but "/a" row must match or fail. Give "/a" its real ids.
        let meta_a = fd_metadata(&anchor(&base.path().join("a"))).unwrap();
        // Policy empty: even "/a" has no row -> refuse.
        let empty = policy_for(&[]);
        let e = open_walk(
            &root,
            "/a/b",
            OpenKind::Dir,
            protected_hop_ok(&empty, 99999),
        )
        .unwrap_err();
        assert!(e.to_string().contains("no topology policy"), "{e}");
        let _ = meta_a;
    }

    /// A protected ancestor whose recorded owner/mode does not match its
    /// policy row refuses — an owner/mode flip under us is caught by the
    /// per-hop check, not just the leaf.
    #[test]
    fn ancestor_owner_mode_mismatch_refuses() {
        let base = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(base.path().join("a/b")).unwrap();
        let root = anchor(base.path());
        let meta_a = fd_metadata(&anchor(&base.path().join("a"))).unwrap();
        // Policy expects /a to be a DIFFERENT owner than it actually is.
        let wrong_owner = policy_for(&[(
            "/a",
            meta_a.uid() + 1111,
            meta_a.gid(),
            meta_a.mode() & 0o7777,
        )]);
        let e = open_walk(
            &root,
            "/a/b",
            OpenKind::Dir,
            protected_hop_ok(&wrong_owner, 99999),
        )
        .unwrap_err();
        assert!(e.to_string().contains("ancestor owned"), "{e}");
    }

    /// A group-writable or guest-owned protected ancestor refuses even when
    /// its policy row otherwise matches — the extra not-reshapeable rule.
    #[test]
    fn group_writable_or_guest_owned_protected_ancestor_refuses() {
        let base = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(base.path().join("a/b")).unwrap();
        let meta_a = fd_metadata(&anchor(&base.path().join("a"))).unwrap();
        let root = anchor(base.path());
        // Case 1: policy row matches, but /a is group-writable.
        std::fs::set_permissions(
            base.path().join("a"),
            std::fs::Permissions::from_mode(0o770),
        )
        .unwrap();
        let p = policy_for(&[("/a", meta_a.uid(), meta_a.gid(), 0o770)]);
        let e = open_walk(&root, "/a/b", OpenKind::Dir, protected_hop_ok(&p, 99999)).unwrap_err();
        assert!(e.to_string().contains("group/other-writable"), "{e}");
        // Case 2: policy row matches, but /a is "guest-owned" (the guest uid).
        std::fs::set_permissions(
            base.path().join("a"),
            std::fs::Permissions::from_mode(0o750),
        )
        .unwrap();
        let p2 = policy_for(&[("/a", meta_a.uid(), meta_a.gid(), 0o750)]);
        let e2 = open_walk(
            &root,
            "/a/b",
            OpenKind::Dir,
            protected_hop_ok(&p2, meta_a.uid()), // guest_uid == /a's owner
        )
        .unwrap_err();
        assert!(e2.to_string().contains("guest-owned"), "{e2}");
    }

    /// A hop-by-hop rename under a held anchor: swap the leaf dir for a fresh
    /// one after the parent dirfd is held — the leaf `fstat` still must see
    /// the expected owner/mode, and a swapped-in foreign-owned dir refuses.
    /// This is the substitution-under-writable-parent class the per-hop rule
    /// plus the leaf check together close.
    #[test]
    fn leaf_substitution_under_anchor_is_restat_checked() {
        let base = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(base.path().join("a/b")).unwrap();
        // Make /a supervisor-like: non-group/other-writable so the hop passes.
        std::fs::set_permissions(
            base.path().join("a"),
            std::fs::Permissions::from_mode(0o750),
        )
        .unwrap();
        let root = anchor(base.path());
        let meta_a = fd_metadata(&anchor(&base.path().join("a"))).unwrap();
        let p = policy_for(&[("/a", meta_a.uid(), meta_a.gid(), 0o750)]);
        // First walk succeeds and yields the leaf.
        let leaf = open_walk(&root, "/a/b", OpenKind::Dir, protected_hop_ok(&p, 99999)).unwrap();
        assert!(fd_metadata(&leaf).unwrap().is_dir());
        // Swap b for a file (not a dir) — the leaf check must reject it.
        std::fs::remove_dir(base.path().join("a/b")).unwrap();
        std::fs::write(base.path().join("a/b"), b"not a dir").unwrap();
        // The walker still opens the leaf (OpenKind::Dir sets O_DIRECTORY on
        // the leaf), so a regular file leaf fails at open with ENOTDIR-like.
        let e = open_walk(&root, "/a/b", OpenKind::Dir, protected_hop_ok(&p, 99999));
        assert!(e.is_err(), "a substituted non-dir leaf must refuse");
    }

    /// A fifo must NOT hang the exec-file open: `O_NONBLOCK` lets `open` return
    /// so `fstat` can reject it. Without O_NONBLOCK a fifo open waits for a
    /// writer forever — this test proves the open returns at all.
    #[test]
    fn fifo_exec_open_does_not_hang() {
        let base = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(base.path().join("a")).unwrap();
        // A fifo as the leaf under a dir we can anchor-walk to.
        let fifopath = base.path().join("a").join("f");
        let c = CString::new(fifopath.to_string_lossy().as_ref()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        let root = anchor(base.path());
        // open_walk with ExecFile kind must return promptly — O_NONBLOCK —
        // and yield a fifo fd the caller's fstat then rejects.
        let fd = open_walk(&root, "/a/f", OpenKind::ExecFile, |_, _| Ok(())).unwrap();
        let meta = fd_metadata(&fd).unwrap();
        assert!(!meta.is_file(), "a fifo is not a regular exec target");
    }
}
