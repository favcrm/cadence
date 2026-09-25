//! ADR 0007 T1 — the agent-uid boundary: `cadence agent-uid provision`
//! and its audit (`cadence agent-uid doctor`, surfaced on `doctor
//! --host` as the `agent-uid` row).
//!
//! Provision is the §5 install script as code: the `cadence-agent`
//! account, the `cadence` and `cadence-launch` groups, `/opt/cadence`
//! and `/var/lib/cadence` with the §3 modes, and the setuid helper.
//! It is idempotent, refuses to run as anything but real root, and
//! prints every action under `--dry-run`. It never writes under the
//! operator's home, never writes a `safe.directory`/`include.path`
//! exception for `/var/lib/cadence`, and never touches sudoers — the
//! action set simply has no verb for any of those.
//!
//! The audit checks every artifact plus §4 rule 2's negative
//! assertions: no ACL under the operator's home reachable by the agent
//! principals, and no uid-1000 git config that would carry
//! `safe.directory`/`include.path` covering `/var/lib/cadence`.
//!
//! Every read and write goes through [`View`]/[`Host`], so tests drive
//! the whole lane against a fixture host in a temp dir — nothing here
//! ever runs `useradd`, `install` or `setfacl` on the real machine in
//! a test.

pub mod audit;
#[doc(hidden)]
pub mod fixture;
pub mod provision;
pub mod runbook;

use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};

// The identity constants are spelled out in `cadence-agent-exec`'s
// `policy` module as well; the audit re-derives them from the host so
// a drift between this list and the helper's fixed sources is itself
// a finding.
pub const AGENT_USER: &str = "cadence-agent";
pub const SHARED_GROUP: &str = "cadence";
pub const LAUNCH_GROUP: &str = "cadence-launch";
/// The uid-1000 seat the boundary protects. `agent-uid` verbs take
/// `--operator` for other hosts; every default assumes this one.
pub const OPERATOR_USER: &str = "ubuntu";

pub const AGENT_HOME: &str = "/home/cadence-agent";
pub const NOLOGIN: &str = "/usr/sbin/nologin";
pub const OPT_ROOT: &str = "/opt/cadence";
pub const LIBEXEC_DIR: &str = "/opt/cadence/libexec";
pub const HELPER_DEST: &str = "/opt/cadence/libexec/cadence-agent-exec";
pub const VAR_LIB: &str = "/var/lib/cadence";
pub const LANES_DIR: &str = "/var/lib/cadence/lanes";
pub const REPOS_DIR: &str = "/var/lib/cadence/repos";
pub const OPT_BIN: &str = "/opt/cadence/bin";
pub const OPT_RELEASES: &str = "/opt/cadence/releases";

/// The repos tree carries one default ACL entry — §5's
/// `setfacl -m d:g:cadence:rwX` — so every store a lane creates starts
/// group-shared.
pub const REPOS_DEFAULT_ACL_PERMS: u8 = 0b111; // rwX

/// The operator-home ACL sweep is bounded the way every watchdog walk
/// here is: past this many entries the check reports a lower bound and
/// warns rather than claiming the negative.
pub const HOME_ACL_WALK_BUDGET: usize = 100_000;

/// A passwd entry, resolved through the view.
#[derive(Clone, Debug)]
pub struct User {
    pub name: String,
    pub uid: u32,
    pub gid: u32,
    pub home: String,
    pub shell: String,
    /// `Some(false)` would mean an unlocked password — shadow is only
    /// readable as root, so `None` is "cannot tell from this seat".
    pub locked: Option<bool>,
}

/// A group entry; `members` is the supplementary member list only —
/// primary-gid membership shows up on the user side.
#[derive(Clone, Debug)]
pub struct Group {
    pub name: String,
    pub gid: u32,
    pub members: BTreeSet<String>,
}

/// What `lstat` says about a path under the root, minus the noise.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Meta {
    pub uid: u32,
    pub gid: u32,
    /// Permission and special bits only (`0o7777` mask).
    pub mode: u32,
    pub is_dir: bool,
    pub is_file: bool,
}

/// One POSIX ACL entry — `e_tag`/`e_perm` decoded; ids are numeric
/// because the xattr format carries numbers, not names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Principal {
    UserObj,
    User(u32),
    GroupObj,
    Group(u32),
    Mask,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AclEntry {
    /// True for `default:` (directory inheritance) entries.
    pub default: bool,
    pub tag: Principal,
    /// r=4 w=2 x=1 bits.
    pub perms: u8,
}

/// The read surface provision and the audit share. All paths are
/// logical absolute paths (`/var/lib/cadence/...`); a view with a fake
/// root maps them itself.
pub trait View {
    fn user(&self, name: &str) -> io::Result<Option<User>>;
    fn group(&self, name: &str) -> io::Result<Option<Group>>;
    /// Reverse lookups for ACL interpretation — an xattr carries the
    /// number, the report wants the name.
    fn user_name(&self, uid: u32) -> io::Result<Option<String>>;
    fn group_name(&self, gid: u32) -> io::Result<Option<String>>;
    fn stat(&self, path: &str) -> io::Result<Option<Meta>>;
    /// Access + default ACL entries at `path`; empty when the file
    /// carries only its mode.
    fn acls(&self, path: &str) -> io::Result<Vec<AclEntry>>;
    /// False when the platform cannot report ACLs at all — the audit
    /// says so instead of claiming a clean sweep it never did.
    fn acl_supported(&self) -> bool {
        true
    }
    fn read_file(&self, path: &str) -> io::Result<Vec<u8>>;
    /// Every entry under `dir` (symlinks not followed), budgeted —
    /// `(paths, truncated)`.
    fn walk(&self, dir: &str, budget: usize) -> io::Result<(Vec<String>, bool)>;
    fn env(&self, key: &str) -> Option<String>;
    /// All env vars with `prefix` — `GIT_CONFIG_KEY_`* enumeration.
    fn env_prefixed(&self, prefix: &str) -> Vec<(String, String)>;
}

/// The write surface provision adds. Each method is the *verb* the
/// §5 script names — `install -d` semantics, `usermod -aG`, `setfacl`.
pub trait Host: View {
    /// Read this host through the shared read surface — Rust's
    /// `&mut dyn Host` → `&dyn View` upcast helper.
    fn as_view(&self) -> &dyn View;
    fn create_group(&mut self, name: &str, system: bool) -> io::Result<()>;
    fn create_user(&mut self, spec: &NewUser) -> io::Result<()>;
    /// `usermod -d <home> -s <shell> -g <group> <name>` — repair drift
    /// on an account that already exists.
    fn repair_user(&mut self, spec: &NewUser) -> io::Result<()>;
    fn add_member(&mut self, group: &str, user: &str) -> io::Result<()>;
    fn mkdir(&mut self, path: &str) -> io::Result<()>;
    /// `chown owner:group path && chmod mode path` — mode carries the
    /// special bits (setgid dirs, the helper's setuid).
    fn set_meta(&mut self, path: &str, owner: &str, group: &str, mode: u32) -> io::Result<()>;
    /// `install -o owner -g group -m mode <src> <dest>` — copies bytes,
    /// then applies meta. `src` is a real filesystem path, not
    /// root-relative: the helper is read out of the build tree.
    fn install(
        &mut self,
        src: &Path,
        dest: &str,
        owner: &str,
        group: &str,
        mode: u32,
    ) -> io::Result<()>;
    /// `setfacl -m d:g:<group>:<perms>` — provision's only ACL write.
    fn set_default_group_acl(&mut self, path: &str, group: &str, perms: u8) -> io::Result<()>;
}

pub struct NewUser {
    pub name: String,
    pub home: String,
    pub shell: String,
    pub primary_group: String,
}

/// The real machine. `root` stays `/` outside tests; nothing on this
/// type is reachable by an agent — provision runs it as root, the
/// audit reads through it.
pub struct LiveHost {
    root: PathBuf,
}

impl LiveHost {
    pub fn new() -> Self {
        LiveHost {
            root: PathBuf::from("/"),
        }
    }

    /// The logical path `/a/b` on the real disk: `<root>/a/b`.
    fn phys(&self, path: &str) -> PathBuf {
        self.root.join(path.trim_start_matches('/'))
    }

    fn logical(&self, phys: &Path) -> String {
        match phys.strip_prefix(&self.root) {
            Ok(rel) => format!("/{}", rel.display()),
            Err(_) => phys.display().to_string(),
        }
    }
}

impl Default for LiveHost {
    fn default() -> Self {
        Self::new()
    }
}

fn command(prog: &str, args: &[&str]) -> io::Result<()> {
    let mut cmd = std::process::Command::new(prog);
    cmd.args(args).stdin(std::process::Stdio::null());
    let out = crate::reaper::output(&mut cmd)?;
    if out.status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "{} {}: {}",
            prog,
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )))
    }
}

#[cfg(unix)]
fn passwd_entry(name: &str) -> Option<User> {
    use std::ffi::CString;
    let name = CString::new(name).ok()?;
    let pw = unsafe { libc::getpwnam(name.as_ptr()) };
    if pw.is_null() {
        return None;
    }
    let pw = unsafe { *pw };
    let s = |p: *const libc::c_char| {
        if p.is_null() {
            String::new()
        } else {
            unsafe { std::ffi::CStr::from_ptr(p) }
                .to_string_lossy()
                .into_owned()
        }
    };
    Some(User {
        name: s(pw.pw_name),
        uid: pw.pw_uid,
        gid: pw.pw_gid,
        home: s(pw.pw_dir),
        shell: s(pw.pw_shell),
        locked: shadow_locked(pw.pw_name),
    })
}

/// The password-lock bit lives in /etc/shadow, readable only to root —
/// `None` means "this seat cannot tell", not "unlocked".
#[cfg(target_os = "linux")]
fn shadow_locked(name: *const libc::c_char) -> Option<bool> {
    let sp = unsafe { libc::getspnam(name) };
    if sp.is_null() {
        return None;
    }
    let pw = unsafe { std::ffi::CStr::from_ptr((*sp).sp_pwdp) }.to_string_lossy();
    Some(pw.starts_with('!') || pw.starts_with('*'))
}

#[cfg(all(unix, not(target_os = "linux")))]
fn shadow_locked(_name: *const libc::c_char) -> Option<bool> {
    None
}

#[cfg(unix)]
fn group_entry(name: &str) -> Option<Group> {
    use std::ffi::CString;
    let name = CString::new(name).ok()?;
    let gr = unsafe { libc::getgrnam(name.as_ptr()) };
    if gr.is_null() {
        return None;
    }
    Some(group_from(unsafe { *gr }))
}

#[cfg(unix)]
fn group_from(gr: libc::group) -> Group {
    let s = |p: *const libc::c_char| {
        if p.is_null() {
            String::new()
        } else {
            unsafe { std::ffi::CStr::from_ptr(p) }
                .to_string_lossy()
                .into_owned()
        }
    };
    let mut members = BTreeSet::new();
    let mut i = 0;
    loop {
        let p = unsafe { *gr.gr_mem.add(i) };
        if p.is_null() {
            break;
        }
        members.insert(s(p));
        i += 1;
    }
    Group {
        name: s(gr.gr_name),
        gid: gr.gr_gid,
        members,
    }
}

impl View for LiveHost {
    fn user(&self, name: &str) -> io::Result<Option<User>> {
        #[cfg(unix)]
        {
            Ok(passwd_entry(name))
        }
        #[cfg(not(unix))]
        {
            let _ = name;
            Ok(None)
        }
    }

    fn group(&self, name: &str) -> io::Result<Option<Group>> {
        #[cfg(unix)]
        {
            Ok(group_entry(name))
        }
        #[cfg(not(unix))]
        {
            let _ = name;
            Ok(None)
        }
    }

    fn user_name(&self, uid: u32) -> io::Result<Option<String>> {
        #[cfg(unix)]
        {
            let pw = unsafe { libc::getpwuid(uid) };
            if pw.is_null() {
                return Ok(None);
            }
            Ok(Some(
                unsafe { std::ffi::CStr::from_ptr((*pw).pw_name) }
                    .to_string_lossy()
                    .into_owned(),
            ))
        }
        #[cfg(not(unix))]
        {
            let _ = uid;
            Ok(None)
        }
    }

    fn group_name(&self, gid: u32) -> io::Result<Option<String>> {
        #[cfg(unix)]
        {
            let gr = unsafe { libc::getgrgid(gid) };
            if gr.is_null() {
                return Ok(None);
            }
            Ok(Some(
                unsafe { std::ffi::CStr::from_ptr((*gr).gr_name) }
                    .to_string_lossy()
                    .into_owned(),
            ))
        }
        #[cfg(not(unix))]
        {
            let _ = gid;
            Ok(None)
        }
    }

    fn stat(&self, path: &str) -> io::Result<Option<Meta>> {
        let phys = self.phys(path);
        let Ok(md) = std::fs::symlink_metadata(&phys) else {
            return Ok(None);
        };
        use std::os::unix::fs::MetadataExt;
        Ok(Some(Meta {
            uid: md.uid(),
            gid: md.gid(),
            mode: md.mode() & 0o7777,
            is_dir: md.is_dir(),
            is_file: md.is_file(),
        }))
    }

    fn acls(&self, path: &str) -> io::Result<Vec<AclEntry>> {
        #[cfg(target_os = "linux")]
        {
            acl_xattrs(&self.phys(path))
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = path;
            Ok(Vec::new())
        }
    }

    fn acl_supported(&self) -> bool {
        cfg!(target_os = "linux")
    }

    fn read_file(&self, path: &str) -> io::Result<Vec<u8>> {
        std::fs::read(self.phys(path))
    }

    fn walk(&self, dir: &str, budget: usize) -> io::Result<(Vec<String>, bool)> {
        let mut out = Vec::new();
        let mut truncated = false;
        let mut stack = vec![self.phys(dir)];
        while let Some(dir) = stack.pop() {
            let entries = match std::fs::read_dir(&dir) {
                Ok(e) => e,
                Err(_) => continue,
            };
            for ent in entries.flatten() {
                if out.len() >= budget {
                    truncated = true;
                    break;
                }
                let phys = ent.path();
                out.push(self.logical(&phys));
                if ent.file_type().is_ok_and(|t| t.is_dir()) {
                    stack.push(phys);
                }
            }
            if truncated {
                break;
            }
        }
        Ok((out, truncated))
    }

    fn env(&self, key: &str) -> Option<String> {
        std::env::var_os(key).map(|v| v.to_string_lossy().into_owned())
    }

    fn env_prefixed(&self, prefix: &str) -> Vec<(String, String)> {
        std::env::vars_os()
            .filter(|(k, _)| k.to_string_lossy().starts_with(prefix))
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.to_string_lossy().into_owned(),
                )
            })
            .collect()
    }
}

impl Host for LiveHost {
    fn as_view(&self) -> &dyn View {
        self
    }

    fn create_group(&mut self, name: &str, system: bool) -> io::Result<()> {
        if system {
            command("groupadd", &["--system", name])
        } else {
            command("groupadd", &[name])
        }
    }

    fn create_user(&mut self, spec: &NewUser) -> io::Result<()> {
        command(
            "useradd",
            &[
                "--system",
                "-m",
                "-d",
                &spec.home,
                "-s",
                &spec.shell,
                "-g",
                &spec.primary_group,
                &spec.name,
            ],
        )
    }

    fn add_member(&mut self, group: &str, user: &str) -> io::Result<()> {
        command("usermod", &["-aG", group, user])
    }

    fn repair_user(&mut self, spec: &NewUser) -> io::Result<()> {
        command(
            "usermod",
            &[
                "-d",
                &spec.home,
                "-s",
                &spec.shell,
                "-g",
                &spec.primary_group,
                &spec.name,
            ],
        )
    }

    fn mkdir(&mut self, path: &str) -> io::Result<()> {
        std::fs::create_dir_all(self.phys(path))
    }

    fn set_meta(&mut self, path: &str, owner: &str, group: &str, mode: u32) -> io::Result<()> {
        let owner_uid = self
            .user(owner)?
            .map(|u| u.uid)
            .ok_or_else(|| io::Error::other(format!("no such user: {owner}")))?;
        let group_gid = self
            .group(group)?
            .map(|g| g.gid)
            .ok_or_else(|| io::Error::other(format!("no such group: {group}")))?;
        let phys = self.phys(path);
        use std::os::unix::ffi::OsStrExt;
        let c = std::ffi::CString::new(phys.as_os_str().as_bytes())
            .map_err(|_| io::Error::other("path carries NUL"))?;
        if unsafe { libc::chown(c.as_ptr(), owner_uid, group_gid) } != 0 {
            return Err(io::Error::last_os_error());
        }
        if unsafe { libc::chmod(c.as_ptr(), mode as libc::mode_t) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    fn install(
        &mut self,
        src: &Path,
        dest: &str,
        owner: &str,
        group: &str,
        mode: u32,
    ) -> io::Result<()> {
        let phys = self.phys(dest);
        std::fs::copy(src, &phys)?;
        self.set_meta(dest, owner, group, mode)
    }

    fn set_default_group_acl(&mut self, path: &str, group: &str, perms: u8) -> io::Result<()> {
        const RWX: [&str; 8] = ["---", "--x", "-w-", "-wx", "r--", "r-x", "rw-", "rwx"];
        command(
            "setfacl",
            &[
                "-m",
                &format!("d:g:{group}:{}", RWX[(perms & 7) as usize]),
                &self.phys(path).display().to_string(),
            ],
        )
    }
}

/// POSIX ACL xattr decoding — the format is `a_version` (u32 LE, ==2)
/// then entries of `{ e_tag: u16, e_perm: u16, e_id: u32 }`, all LE.
/// `e_id` is a uid for USER, a gid for GROUP, and `0xffffffff` for the
/// owner/group/mask/other tags.
#[cfg(target_os = "linux")]
fn acl_xattrs(phys: &Path) -> io::Result<Vec<AclEntry>> {
    let mut out = Vec::new();
    for (name, default) in [
        ("system.posix_acl_access", false),
        ("system.posix_acl_default", true),
    ] {
        out.extend(read_acl_xattr(phys, name, default)?);
    }
    Ok(out)
}

#[cfg(target_os = "linux")]
fn read_acl_xattr(phys: &Path, name: &str, default: bool) -> io::Result<Vec<AclEntry>> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(phys.as_os_str().as_bytes())
        .map_err(|_| io::Error::other("path carries NUL"))?;
    let attr = std::ffi::CString::new(name).map_err(|_| io::Error::other("attr carries NUL"))?;
    let size = unsafe { libc::lgetxattr(path.as_ptr(), attr.as_ptr(), std::ptr::null_mut(), 0) };
    if size < 0 {
        // ENODATA: no such xattr — the file carries only its mode.
        return Ok(Vec::new());
    }
    let mut buf = vec![0u8; size as usize];
    let got = unsafe {
        libc::lgetxattr(
            path.as_ptr(),
            attr.as_ptr(),
            buf.as_mut_ptr() as *mut _,
            buf.len(),
        )
    };
    if got < 0 {
        return Err(io::Error::last_os_error());
    }
    buf.truncate(got as usize);
    Ok(parse_acl(&buf, default))
}

#[cfg(target_os = "linux")]
fn parse_acl(buf: &[u8], default: bool) -> Vec<AclEntry> {
    const VERSION: u32 = 2;
    if buf.len() < 4 || u32::from_le_bytes(buf[..4].try_into().unwrap()) != VERSION {
        return Vec::new();
    }
    let mut out = Vec::new();
    for chunk in buf[4..].as_chunks::<8>().0 {
        let tag = u16::from_le_bytes(chunk[0..2].try_into().unwrap());
        let perms = u16::from_le_bytes(chunk[2..4].try_into().unwrap()) as u8 & 7;
        let id = u32::from_le_bytes(chunk[4..8].try_into().unwrap());
        let tag = match tag {
            0x01 => Principal::UserObj,
            0x02 => Principal::User(id),
            0x04 => Principal::GroupObj,
            0x08 => Principal::Group(id),
            0x10 => Principal::Mask,
            0x20 => Principal::Other,
            _ => continue,
        };
        out.push(AclEntry {
            default,
            tag,
            perms,
        });
    }
    out
}

/// The boundary the negative assertions protect. Resolved from the
/// view so the checks work mid-provision (before the uid exists, only
/// the named-principal half can be tested).
#[derive(Clone, Debug)]
pub struct AgentPrincipals {
    /// Resolved uid/gids once the account exists; empty before.
    pub uid: Option<u32>,
    pub gids: Vec<u32>,
    /// Group names that grant the agent side — membership or an ACL
    /// naming one reaches the agent domain.
    pub group_names: [&'static str; 2],
}

impl AgentPrincipals {
    pub fn resolve(view: &dyn View) -> io::Result<AgentPrincipals> {
        let uid = view.user(AGENT_USER)?.map(|u| u.uid);
        let mut gids = Vec::new();
        for name in [AGENT_USER, SHARED_GROUP] {
            if let Some(g) = view.group(name)? {
                gids.push(g.gid);
            }
        }
        Ok(AgentPrincipals {
            uid,
            gids,
            group_names: [AGENT_USER, SHARED_GROUP],
        })
    }

    /// Is this ACL entry a grant into the agent domain? Only named
    /// user/group entries count — owner/group-object/other are the
    /// file's own mode in another spelling, and the mask bounds them.
    pub fn grants_agent(&self, entry: &AclEntry) -> bool {
        match entry.tag {
            Principal::User(uid) => self.uid.is_some_and(|u| u == uid),
            Principal::Group(gid) => self.gids.contains(&gid),
            _ => false,
        }
    }
}

/// `provision` and `agent-uid doctor` share the refusal gate: only a
/// real root may apply the plan. Both real and effective uid must be
/// 0 — a setuid binary's euid-0/ruid-user shape does not count as the
/// operator sitting down as root.
pub fn enforce_root(real_uid: u32, effective_uid: u32) -> crate::Result<()> {
    if real_uid == 0 && effective_uid == 0 {
        Ok(())
    } else {
        Err(crate::Error::rejected(format!(
            "agent-uid provision must run as root (uid={real_uid} euid={effective_uid})"
        )))
    }
}

/// The caller's (real, effective) uid pair for [`enforce_root`].
#[cfg(unix)]
pub fn caller_uids() -> (u32, u32) {
    unsafe { (libc::getuid(), libc::geteuid()) }
}

/// Non-unix has no uids — the pair is impossible, so `enforce_root`
/// refuses the verb entirely.
#[cfg(not(unix))]
pub fn caller_uids() -> (u32, u32) {
    (u32::MAX, u32::MAX)
}
