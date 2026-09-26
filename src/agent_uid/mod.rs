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

#[cfg(unix)]
use std::os::fd::AsRawFd;

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

/// `View::read_file`'s bound — config reads (gitconfig, `include.path`
/// targets) are attacker-influenceable files read as root: never more
/// than this many bytes, never a fifo, never through a symlink.
pub const READ_CAP: u64 = 1 << 20;

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
    /// True when the path *itself* is a symlink — the lstat verdict.
    /// No verb in this lane ever follows one: a symlink anywhere in a
    /// §5 path is a refusal (ADR §7), never a redirect.
    pub is_symlink: bool,
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
    /// Every account on the host — uid-collision checks need the
    /// whole passwd map, not just the named lookups.
    fn users(&self) -> io::Result<Vec<User>>;
    /// The user's full group vector — primary gid plus every
    /// supplementary membership (`getgrouplist(3)`). Memberships
    /// granted outside the spec groups still reach the agent domain,
    /// so the audit folds them into the principal set.
    fn member_gids(&self, user: &User) -> io::Result<Vec<u32>>;
    /// Read a config file bounded: a regular file only, at most
    /// [`READ_CAP`] bytes, never opened through a symlink — an
    /// include target can name a fifo and the audit must not hang
    /// as root.
    fn read_file(&self, path: &str) -> io::Result<Vec<u8>>;
    /// Where a symlink points — `readlink(2)`, the link itself only.
    /// The git-config sweep resolves links it refuses to *open* so a
    /// symlinked include into the agent domain still flags.
    fn read_link(&self, path: &str) -> io::Result<PathBuf>;
    /// Parse one gitconfig file with git's own parser —
    /// `git config --file <path> --no-includes --list --null` — under
    /// a scrubbed, inert env (ambient scopes nullified — git follows
    /// their includes even under `--no-includes`), so the audit can
    /// never diverge from the grammar git applies at use time.
    /// `path` is logical; the view maps it under its root. Includes
    /// are *not* resolved by git (`--no-includes`): the caller walks
    /// the include graph itself so every hop gets the bounded/
    /// no-follow vetting. Returns canonical `section[.sub].key →
    /// value` pairs; a valueless key reports `"true"` (git's
    /// implicit-bool).
    fn git_config(&self, path: &str) -> io::Result<Vec<(String, String)>>;
    /// The file's sha256, streamed — the helper's byte-compare reads
    /// a multi-MB binary, so it hashes instead of loading.
    fn file_sha256(&self, path: &str) -> io::Result<[u8; 32]>;
    /// Vet a helper *source* path and return its sha256 in one
    /// fd-atomic step: `O_NOFOLLOW`, regular file, trusted owner, not
    /// group/other-writable — then hash the opened fd, so a path swap
    /// between vetting and hashing cannot smuggle other bytes in.
    /// `src` is a real filesystem path (not root-relative).
    fn helper_source(&self, src: &Path) -> io::Result<[u8; 32]>;
    /// Every entry under `dir` (symlinks not followed), budgeted —
    /// `(paths, truncated)`.
    fn walk(&self, dir: &str, budget: usize) -> io::Result<(Vec<String>, bool)>;
    fn env(&self, key: &str) -> Option<String>;
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
    /// Uids allowed to own a path component provision writes
    /// *through*: `{0}` plus the operator's uid once `cli` resolves
    /// it. The agent's uid is never admitted — an agent-owned
    /// intermediate directory means the agent controls a prefix of
    /// the path, and every write under it is refused.
    trusted_uids: BTreeSet<u32>,
}

impl LiveHost {
    pub fn new() -> Self {
        LiveHost {
            root: PathBuf::from("/"),
            trusted_uids: BTreeSet::from([0]),
        }
    }

    /// The operator's uid joins the trusted set once resolved —
    /// `/var/lib/cadence` is operator-owned by spec.
    pub fn trust_uid(&mut self, uid: u32) {
        self.trusted_uids.insert(uid);
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

/// Root's toolbelt: `groupadd`/`useradd`/`usermod`/`setfacl` run by
/// absolute path under a scrubbed environment — provision executes as
/// uid 0 with an otherwise-inherited env, so neither PATH resolution
/// nor env-carried config may reach the child.
fn command(prog: &str, args: &[&str]) -> io::Result<()> {
    let resolved = ["/usr/sbin", "/sbin", "/usr/bin", "/bin"]
        .iter()
        .map(|d| format!("{d}/{prog}"))
        .find(|p| Path::new(p).is_file())
        .unwrap_or_else(|| prog.to_string());
    let mut cmd = std::process::Command::new(&resolved);
    cmd.args(args)
        .stdin(std::process::Stdio::null())
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin");
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

/// Parse one config file with *git itself* — the audit never reads
/// the grammar a second time (statements after `]` on a section
/// line, quoted `\`-continuations: git is the reference parser, a
/// hand-rolled reader keeps diverging from it). One open does the
/// whole gate (no-follow, regular-file, ≤`READ_CAP`); git is then
/// pointed at `/dev/stdin` — that fd, never the path again — so a
/// swapped path cannot feed it an unvetted file.
///
/// The spawn's environment is scrubbed and *inert*: fixed PATH,
/// `HOME=/`, and both scope files forced to `/dev/null`. Git loads
/// the ambient global/system scopes for its own machinery even
/// under `--file`, and follows their `include.path` regardless of
/// `--no-includes` — a fifo'd include in the operator's real config
/// would hang the audit, an over-cap one would read unbounded. With
/// the scopes nullified the only bytes git can read are the fd's.
/// `~` stays literal in listed values — expansion is the audit's
/// own job, done with the operator home during resolution.
///
/// `git config --list` only ever *reads*: it runs no hooks or
/// aliases (those fire on repo command dispatch, which this is
/// not), evaluates no `core.fsmonitor`/`core.pager` (not a tty, not
/// a worktree command), and executes no config value.
fn git_config_file(phys: &Path, logical: &str) -> io::Result<Vec<(String, String)>> {
    let f = bounded_open(phys, logical)?;
    // The pre-open length is a snapshot — re-check after git reads,
    // so a grow-during-parse can't smuggle past the cap.
    let checked = f.try_clone()?;
    let git = ["/usr/bin/git", "/bin/git", "/usr/local/bin/git"]
        .iter()
        .find(|p| Path::new(p).is_file())
        .ok_or_else(|| {
            io::Error::other("no git binary at a trusted path — config cannot be verified")
        })?;
    let mut cmd = std::process::Command::new(git);
    cmd.args([
        "config",
        "--file",
        "/dev/stdin",
        "--no-includes",
        "--list",
        "--null",
    ])
    .stdin(std::process::Stdio::from(f))
    .env_clear()
    .env("PATH", "/usr/bin:/bin")
    .env("HOME", "/")
    .env("GIT_CONFIG_NOSYSTEM", "1")
    .env("GIT_CONFIG_SYSTEM", "/dev/null")
    .env("GIT_CONFIG_GLOBAL", "/dev/null")
    .env("XDG_CONFIG_HOME", "/dev/null")
    .env("GIT_TERMINAL_PROMPT", "0")
    .env("GIT_PAGER", "cat")
    .env("LC_ALL", "C")
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped());
    // `reaper::output` forces stdin to null — spawn + wait directly so
    // the vetted fd stays git's stdin.
    let out = crate::reaper::spawn(&mut cmd)?.wait_with_output()?;
    if checked.metadata().map(|m| m.len()).unwrap_or(u64::MAX) > READ_CAP {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{logical}: exceeds the {READ_CAP}-byte config bound"),
        ));
    }
    if !out.status.success() {
        return Err(io::Error::other(format!(
            "git config --file {logical}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    // `--null` records: `key\nvalue\0`; an implicit-true key is a bare
    // `key\0`. Git has already canonicalized `section[.sub].key`.
    let mut entries = Vec::new();
    for rec in out.stdout.split(|b| *b == 0) {
        if rec.is_empty() {
            continue;
        }
        let rec = String::from_utf8_lossy(rec);
        match rec.split_once('\n') {
            Some((k, v)) => entries.push((k.to_string(), v.to_string())),
            None => entries.push((rec.into_owned(), "true".to_string())),
        }
    }
    Ok(entries)
}

/// libc's `*mut c_char` fields → owned String — the shared extractor
/// for passwd/group entries.
#[cfg(unix)]
unsafe fn cstr_field(p: *const libc::c_char) -> String {
    if p.is_null() {
        String::new()
    } else {
        unsafe { std::ffi::CStr::from_ptr(p) }
            .to_string_lossy()
            .into_owned()
    }
}

#[cfg(unix)]
fn user_from(pw: &libc::passwd, locked: Option<bool>) -> User {
    User {
        name: unsafe { cstr_field(pw.pw_name) },
        uid: pw.pw_uid,
        gid: pw.pw_gid,
        home: unsafe { cstr_field(pw.pw_dir) },
        shell: unsafe { cstr_field(pw.pw_shell) },
        locked,
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
    Some(user_from(&pw, shadow_locked(pw.pw_name)))
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
    let mut members = BTreeSet::new();
    let mut i = 0;
    loop {
        let p = unsafe { *gr.gr_mem.add(i) };
        if p.is_null() {
            break;
        }
        members.insert(unsafe { cstr_field(p) });
        i += 1;
    }
    Group {
        name: unsafe { cstr_field(gr.gr_name) },
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
            is_symlink: md.file_type().is_symlink(),
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

    fn users(&self) -> io::Result<Vec<User>> {
        #[cfg(unix)]
        {
            let mut out = Vec::new();
            unsafe { libc::setpwent() };
            loop {
                let pw = unsafe { libc::getpwent() };
                if pw.is_null() {
                    break;
                }
                // `locked` stays None here — shadow lookups per
                // account buy nothing for a uid-collision sweep.
                out.push(user_from(unsafe { &*pw }, None));
            }
            unsafe { libc::endpwent() };
            Ok(out)
        }
        #[cfg(not(unix))]
        {
            Ok(Vec::new())
        }
    }

    fn member_gids(&self, user: &User) -> io::Result<Vec<u32>> {
        #[cfg(unix)]
        {
            use std::ffi::CString;
            // libc's `getgrouplist` signature follows the platform:
            // Darwin's buffer and base gid are `int`, elsewhere
            // `gid_t` — the casts below are real on one side of the
            // fence and no-ops on the other.
            #[cfg(target_vendor = "apple")]
            type GrouplistGid = libc::c_int;
            #[cfg(not(target_vendor = "apple"))]
            type GrouplistGid = libc::gid_t;
            let name = CString::new(user.name.clone())
                .map_err(|_| io::Error::other("user name carries NUL"))?;
            let mut count: libc::c_int = 0;
            #[allow(clippy::unnecessary_cast)] // gid_t→c_int on Darwin
            let basegid = user.gid as GrouplistGid;
            unsafe { libc::getgrouplist(name.as_ptr(), basegid, std::ptr::null_mut(), &mut count) };
            let mut buf = vec![0 as GrouplistGid; count.max(0) as usize + 1];
            let mut gids = vec![user.gid];
            // A membership that grows between calls makes the second
            // call fail with the needed size in `n` — grow and retry,
            // bounded, rather than audit a truncated group vector.
            #[allow(clippy::unnecessary_cast)]
            for _ in 0..4 {
                let mut n = buf.len() as libc::c_int;
                let got =
                    unsafe { libc::getgrouplist(name.as_ptr(), basegid, buf.as_mut_ptr(), &mut n) };
                if got >= 0 {
                    gids.extend(buf[..n.max(0) as usize].iter().map(|g| *g as u32));
                    break;
                }
                buf.resize(n.max(0) as usize + 1, 0);
            }
            gids.sort_unstable();
            gids.dedup();
            Ok(gids)
        }
        #[cfg(not(unix))]
        {
            Ok(vec![user.gid])
        }
    }

    fn read_file(&self, path: &str) -> io::Result<Vec<u8>> {
        bounded_read(&self.phys(path), path)
    }

    fn read_link(&self, path: &str) -> io::Result<PathBuf> {
        std::fs::read_link(self.phys(path))
    }

    fn file_sha256(&self, path: &str) -> io::Result<[u8; 32]> {
        sha256_path(&self.phys(path))
    }

    fn git_config(&self, path: &str) -> io::Result<Vec<(String, String)>> {
        git_config_file(&self.phys(path), path)
    }

    #[cfg(unix)]
    fn helper_source(&self, src: &Path) -> io::Result<[u8; 32]> {
        let acls = self.acls(&src.display().to_string())?;
        let f = open_verified_source(src, &self.trusted_uids, &acls)?;
        sha256_file(&f)
    }

    #[cfg(not(unix))]
    fn helper_source(&self, src: &Path) -> io::Result<[u8; 32]> {
        let _ = src;
        Err(io::Error::other("helper_source: unix only"))
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

    #[cfg(unix)]
    fn mkdir(&mut self, path: &str) -> io::Result<()> {
        let phys = self.phys(path);
        let name = phys
            .file_name()
            .ok_or_else(|| io::Error::other(format!("{path}: no final component")))?;
        // The parent chain is walked fd-pinned — a symlinked
        // intermediate refuses, it is never followed.
        let pfd = open_pinned_dir(phys.parent(), &self.trusted_uids, true)?;
        if let Err(e) = mkdirat(pfd.as_raw_fd(), name, 0o755) {
            // Idempotent: an existing directory is fine, anything
            // else (a symlink, a file) fails the verify-open.
            if e.kind() == io::ErrorKind::AlreadyExists {
                openat_dir(pfd.as_raw_fd(), name)?;
                return Ok(());
            }
            return Err(e);
        }
        Ok(())
    }

    #[cfg(not(unix))]
    fn mkdir(&mut self, path: &str) -> io::Result<()> {
        let _ = path;
        Err(io::Error::other("mkdir: unix only"))
    }

    #[cfg(unix)]
    fn set_meta(&mut self, path: &str, owner: &str, group: &str, mode: u32) -> io::Result<()> {
        let owner_uid = self
            .user(owner)?
            .map(|u| u.uid)
            .ok_or_else(|| io::Error::other(format!("no such user: {owner}")))?;
        let group_gid = self
            .group(group)?
            .map(|g| g.gid)
            .ok_or_else(|| io::Error::other(format!("no such group: {group}")))?;
        let fd = open_pinned_dir(Some(&self.phys(path)), &self.trusted_uids, false)?;
        if unsafe { libc::fchown(fd.as_raw_fd(), owner_uid, group_gid) } != 0 {
            return Err(io::Error::last_os_error());
        }
        if unsafe { libc::fchmod(fd.as_raw_fd(), mode as libc::mode_t) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    #[cfg(not(unix))]
    fn set_meta(&mut self, path: &str, owner: &str, group: &str, mode: u32) -> io::Result<()> {
        let _ = (path, owner, group, mode);
        Err(io::Error::other("set_meta: unix only"))
    }

    #[cfg(unix)]
    fn install(
        &mut self,
        src: &Path,
        dest: &str,
        owner: &str,
        group: &str,
        mode: u32,
    ) -> io::Result<()> {
        if !src.is_absolute() {
            return Err(io::Error::other(format!(
                "{}: the helper source must be an absolute path — under sudo the cwd is untrusted",
                src.display()
            )));
        }
        let acls = self.acls(&src.display().to_string())?;
        let mut srcf = open_verified_source(src, &self.trusted_uids, &acls)?;
        let owner_uid = self
            .user(owner)?
            .map(|u| u.uid)
            .ok_or_else(|| io::Error::other(format!("no such user: {owner}")))?;
        let group_gid = self
            .group(group)?
            .map(|g| g.gid)
            .ok_or_else(|| io::Error::other(format!("no such group: {group}")))?;
        let phys = self.phys(dest);
        let name = phys
            .file_name()
            .ok_or_else(|| io::Error::other(format!("{dest}: no final component")))?;
        let pfd = open_pinned_dir(phys.parent(), &self.trusted_uids, true)?;
        let dfd = openat_file(pfd.as_raw_fd(), name)?;
        let destf = std::fs::File::from(dfd);
        copy_verified(&mut srcf, &destf)?;
        if unsafe { libc::fchown(destf.as_raw_fd(), owner_uid, group_gid) } != 0 {
            return Err(io::Error::last_os_error());
        }
        if unsafe { libc::fchmod(destf.as_raw_fd(), mode as libc::mode_t) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    #[cfg(not(unix))]
    fn install(
        &mut self,
        src: &Path,
        dest: &str,
        owner: &str,
        group: &str,
        mode: u32,
    ) -> io::Result<()> {
        let _ = (src, dest, owner, group, mode);
        Err(io::Error::other("install: unix only"))
    }

    #[cfg(unix)]
    fn set_default_group_acl(&mut self, path: &str, group: &str, perms: u8) -> io::Result<()> {
        // setfacl has no fd form — pin the directory first, then hand
        // the tool the fd's procfs alias so a name swap between our
        // check and its run cannot redirect the write.
        let phys = self.phys(path);
        let fd = open_pinned_dir(Some(&phys), &self.trusted_uids, false)?;
        const RWX: [&str; 8] = ["---", "--x", "-w-", "-wx", "r--", "r-x", "rw-", "rwx"];
        let target = if cfg!(target_os = "linux") {
            format!("/proc/{}/fd/{}", std::process::id(), fd.as_raw_fd())
        } else {
            phys.display().to_string()
        };
        command(
            "setfacl",
            &[
                "-m",
                &format!("d:g:{group}:{}", RWX[(perms & 7) as usize]),
                &target,
            ],
        )
    }

    #[cfg(not(unix))]
    fn set_default_group_acl(&mut self, path: &str, group: &str, perms: u8) -> io::Result<()> {
        let _ = (path, group, perms);
        Err(io::Error::other("set_default_group_acl: unix only"))
    }
}

// ---------- fd-pinned writes (unix) ----------
//
// Provision writes paths under an operator-writable root —
// `/var/lib/cadence` is uid-1000 after stage A, and today's agents
// share that uid — so a planted symlink between assess and apply is a
// real channel, not a theoretical one. Every write verb opens its
// target the same way: `openat(O_NOFOLLOW|O_DIRECTORY)` per component
// from the host root, every intermediate owned by a trusted uid, then
// `fchown`/`fchmod`/`mkdirat`/`openat` act on the pinned fd — never on
// the path.

#[cfg(unix)]
fn c_name(name: &std::ffi::OsStr) -> io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(name.as_bytes())
        .map_err(|_| io::Error::other("path component carries NUL"))
}

/// `open(2)` a directory — `O_NOFOLLOW` refuses a symlinked final
/// component instead of following it.
#[cfg(unix)]
fn open_dir_fd(path: &Path) -> io::Result<std::os::unix::io::OwnedFd> {
    use std::os::unix::io::FromRawFd;
    let c = c_name(path.as_os_str())?;
    let fd = unsafe {
        libc::open(
            c.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { std::os::unix::io::OwnedFd::from_raw_fd(fd) })
}

/// `openat(2)` one path component — `O_DIRECTORY|O_NOFOLLOW`.
#[cfg(unix)]
fn openat_dir(
    dirfd: std::os::unix::io::RawFd,
    name: &std::ffi::OsStr,
) -> io::Result<std::os::unix::io::OwnedFd> {
    use std::os::unix::io::FromRawFd;
    let c = c_name(name)?;
    let fd = unsafe {
        libc::openat(
            dirfd,
            c.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { std::os::unix::io::OwnedFd::from_raw_fd(fd) })
}

#[cfg(unix)]
fn openat_file(
    dirfd: std::os::unix::io::RawFd,
    name: &std::ffi::OsStr,
) -> io::Result<std::os::unix::io::OwnedFd> {
    use std::os::unix::io::FromRawFd;
    let c = c_name(name)?;
    // O_NONBLOCK: a planted fifo opens without hanging, then fstat
    // refuses it.
    let fd = unsafe {
        libc::openat(
            dirfd,
            c.as_ptr(),
            libc::O_RDWR
                | libc::O_CREAT
                | libc::O_TRUNC
                | libc::O_NOFOLLOW
                | libc::O_CLOEXEC
                | libc::O_NONBLOCK,
            0o600,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let fd = unsafe { std::os::unix::io::OwnedFd::from_raw_fd(fd) };
    let f = std::fs::File::from(fd);
    if !f.metadata()?.is_file() {
        return Err(io::Error::other(format!(
            "{}: exists and is not a regular file — refusing",
            name.to_string_lossy()
        )));
    }
    Ok(f.into())
}

#[cfg(unix)]
fn mkdirat(
    dirfd: std::os::unix::io::RawFd,
    name: &std::ffi::OsStr,
    mode: libc::mode_t,
) -> io::Result<()> {
    let c = c_name(name)?;
    if unsafe { libc::mkdirat(dirfd, c.as_ptr(), mode) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(unix)]
fn fd_uid(fd: &std::os::unix::io::OwnedFd) -> io::Result<u32> {
    use std::os::unix::io::AsRawFd;
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(fd.as_raw_fd(), &mut st) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(st.st_uid)
}

/// Open `dir` pinned through its whole chain from `/`: every
/// component must be a real directory — `O_NOFOLLOW` refuses a
/// symlink — and every *intermediate* must be owned by a trusted uid.
/// `check_last_owner`: `true` when `dir` is the parent we will create
/// or install under (its final component is an intermediate of the
/// real target — the owner check applies); `false` when `dir` itself
/// is the target (its owner may be the spec's — e.g. agent-owned
/// `repos` — or is about to be repaired by the caller).
/// The returned fd *is* `dir`; writes that act on it cannot be
/// redirected by renaming the path afterwards.
#[cfg(unix)]
fn open_pinned_dir(
    dir: Option<&Path>,
    trusted: &BTreeSet<u32>,
    check_last_owner: bool,
) -> io::Result<std::os::unix::io::OwnedFd> {
    use std::os::unix::io::AsRawFd;
    let dir = dir.ok_or_else(|| io::Error::other("path has no parent chain"))?;
    let mut comps: Vec<&std::ffi::OsStr> = Vec::new();
    for c in dir.components() {
        match c {
            std::path::Component::RootDir => {}
            std::path::Component::Normal(name) => comps.push(name),
            _ => {
                return Err(io::Error::other(format!(
                    "{}: non-normal component — refusing",
                    dir.display()
                )))
            }
        }
    }
    let mut fd = open_dir_fd(Path::new("/"))?;
    if !trusted.contains(&fd_uid(&fd)?) {
        return Err(io::Error::other("/ is owned by an untrusted uid"));
    }
    let mut walked = PathBuf::from("/");
    for (i, name) in comps.iter().enumerate() {
        fd = openat_dir(fd.as_raw_fd(), name)?;
        walked.push(name);
        if (check_last_owner || i + 1 < comps.len()) && !trusted.contains(&fd_uid(&fd)?) {
            return Err(io::Error::other(format!(
                "{}: owned by an untrusted uid — refusing to descend",
                walked.display()
            )));
        }
    }
    Ok(fd)
}

/// The helper-source gate (C2): the operator hands us a *path* — open
/// it `O_NOFOLLOW`, then judge the fd: regular file only (the
/// `O_NONBLOCK` keeps a fifo's open from hanging), owner in the
/// trusted set (root or the operator — never the agent uid), and not
/// writable by group/other. Anything else refuses rather than copies.
/// `acls` are the file's POSIX access entries — grants the mode bits
/// do not show (I6).
#[cfg(unix)]
pub(crate) fn open_verified_source(
    src: &Path,
    trusted: &BTreeSet<u32>,
    acls: &[AclEntry],
) -> io::Result<std::fs::File> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let f = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(src)?;
    let md = f.metadata()?;
    check_source(
        src,
        md.is_file(),
        md.mode() & 0o7777,
        md.uid(),
        trusted,
        acls,
    )?;
    Ok(f)
}

/// What a vetted helper source must look like — shared so the fixture
/// applies the identical rule to its (possibly forged) source meta.
/// POSIX ACLs are part of the vet: a `u:cadence-agent:rw` entry on a
/// root-owned 0644 source lets the agent write what installs as the
/// setuid helper, and the mode bits say nothing about it. Refuse any
/// *effective* write grant (entry perms masked by the ACL mask) to a
/// non-root principal; read-only and root-bound grants are inert.
#[cfg(unix)]
pub(crate) fn check_source(
    src: &Path,
    is_file: bool,
    mode: u32,
    uid: u32,
    trusted: &BTreeSet<u32>,
    acls: &[AclEntry],
) -> io::Result<()> {
    if !is_file {
        return Err(io::Error::other(format!(
            "{}: helper source is not a regular file — refusing",
            src.display()
        )));
    }
    if mode & 0o022 != 0 {
        return Err(io::Error::other(format!(
            "{}: helper source is group/other-writable ({mode:04o}) — refusing",
            src.display()
        )));
    }
    if !trusted.contains(&uid) {
        return Err(io::Error::other(format!(
            "{}: helper source owned by untrusted uid {uid} — refusing",
            src.display()
        )));
    }
    let mask = acls
        .iter()
        .find(|e| !e.default && e.tag == Principal::Mask)
        .map(|e| e.perms)
        .unwrap_or(0b111);
    for e in acls.iter().filter(|e| !e.default) {
        if e.perms & mask & 0b010 == 0 {
            continue;
        }
        match e.tag {
            Principal::User(u) if u != 0 => {
                return Err(io::Error::other(format!(
                    "{}: helper source ACL grants uid {u} write — refusing",
                    src.display()
                )))
            }
            Principal::Group(g) if g != 0 => {
                return Err(io::Error::other(format!(
                    "{}: helper source ACL grants gid {g} write — refusing",
                    src.display()
                )))
            }
            _ => {}
        }
    }
    Ok(())
}

/// Copy `src` to `dest` hashing both ends: what left the source is
/// provably what the destination holds — on the opened fds, so a
/// path swap mid-copy cannot redirect the install.
#[cfg(unix)]
pub(crate) fn copy_verified(src: &mut std::fs::File, dest: &std::fs::File) -> io::Result<()> {
    use sha2::{Digest, Sha256};
    use std::io::{Read, Seek, SeekFrom, Write};
    let mut w = dest;
    let mut src_hash = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = src.read(&mut buf)?;
        if n == 0 {
            break;
        }
        src_hash.update(&buf[..n]);
        w.write_all(&buf[..n])?;
    }
    w.sync_all()?;
    w.seek(SeekFrom::Start(0))?;
    let mut dest_hash = Sha256::new();
    loop {
        let n = w.read(&mut buf)?;
        if n == 0 {
            break;
        }
        dest_hash.update(&buf[..n]);
    }
    if src_hash.finalize() != dest_hash.finalize() {
        return Err(io::Error::other(
            "installed bytes differ from the verified source — refusing",
        ));
    }
    Ok(())
}

/// Bounded, no-follow config read: `O_NOFOLLOW` refuses a symlinked
/// path outright; `O_NONBLOCK` plus the regular-file check keeps a
/// fifo from hanging the audit as root; [`READ_CAP`] keeps a huge
/// include target from OOMing it.
///
/// `bounded_open` is the gate; `bounded_read` adds the post-open
/// length enforcement.
#[cfg(unix)]
pub(crate) fn bounded_open(phys: &Path, logical: &str) -> io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    let f = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(phys)?;
    let md = f.metadata()?;
    if !md.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{logical}: not a regular file — refusing to read"),
        ));
    }
    if md.len() > READ_CAP {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{logical}: exceeds the {READ_CAP}-byte config bound"),
        ));
    }
    Ok(f)
}

#[cfg(unix)]
pub(crate) fn bounded_read(phys: &Path, logical: &str) -> io::Result<Vec<u8>> {
    use std::io::Read;
    let f = bounded_open(phys, logical)?;
    let mut buf = Vec::new();
    f.take(READ_CAP + 1).read_to_end(&mut buf)?;
    if buf.len() as u64 > READ_CAP {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{logical}: exceeds the {READ_CAP}-byte config bound"),
        ));
    }
    Ok(buf)
}

#[cfg(not(unix))]
pub(crate) fn bounded_open(_phys: &Path, logical: &str) -> io::Result<std::fs::File> {
    let _ = logical;
    Err(io::Error::other("bounded_open: unix only"))
}

#[cfg(not(unix))]
pub(crate) fn bounded_read(_phys: &Path, logical: &str) -> io::Result<Vec<u8>> {
    let _ = logical;
    Err(io::Error::other("bounded_read: unix only"))
}

/// Streamed sha256 of an already-open file.
#[cfg(unix)]
pub(crate) fn sha256_file(mut f: &std::fs::File) -> io::Result<[u8; 32]> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().into())
}

/// Streamed sha256 of a path — `O_NOFOLLOW`, regular files only. Used
/// for the helper byte-compare at assess time.
#[cfg(unix)]
pub(crate) fn sha256_path(phys: &Path) -> io::Result<[u8; 32]> {
    use std::os::unix::fs::OpenOptionsExt;
    let f = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(phys)?;
    if !f.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{}: not a regular file", phys.display()),
        ));
    }
    sha256_file(&f)
}

#[cfg(not(unix))]
pub(crate) fn sha256_path(phys: &Path) -> io::Result<[u8; 32]> {
    let bytes = std::fs::read(phys)?;
    use sha2::{Digest, Sha256};
    Ok(Sha256::digest(&bytes).into())
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
        let user = view.user(AGENT_USER)?;
        let uid = user.as_ref().map(|u| u.uid);
        let mut gids = BTreeSet::new();
        for name in [AGENT_USER, SHARED_GROUP] {
            if let Some(g) = view.group(name)? {
                gids.insert(g.gid);
            }
        }
        // Every supplementary membership grants the agent too — a
        // `usermod -aG docker cadence-agent` is invisible unless the
        // whole group vector is enumerated.
        if let Some(u) = &user {
            for gid in view.member_gids(u)? {
                gids.insert(gid);
            }
        }
        Ok(AgentPrincipals {
            uid,
            gids: gids.into_iter().collect(),
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
