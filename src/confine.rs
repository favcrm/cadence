//! `cadence confine` (CAD-439): run a command under an OS filesystem
//! sandbox that exposes only the listed paths — read (and execute) for
//! `--read`, everything for `--write`. Every descendant inherits it and
//! nothing inside can lift it.
//!
//! Linux uses Landlock (unprivileged, no user namespace — bubblewrap is
//! refused on hosts that restrict unprivileged user namespaces, as
//! Ubuntu 24.04+ does by default). Other platforms refuse: there is no
//! verified sandbox for them yet, and the master must never start
//! unconfined.
//!
//! The daemon launches the master's provider through this command; the
//! policy the master gets is [`crate::master::confinement`]. A path that
//! does not exist is skipped, never widened to its parent. On Landlock
//! ABI 6+ the process is also scoped: it cannot signal processes outside
//! its domain (the daemon, the operator's shells) nor connect to abstract
//! unix sockets. Network and path-named unix-socket connects are not
//! restricted — the master talks to the daemon socket and the provider's
//! API.
//!
//! **What this is not.** It confines the filesystem, not what the
//! process can ask others to do. A confined process that gets arbitrary
//! exec escapes through any unix-socket service it can reach —
//! `systemd-run --user` (the user bus), a tmux server, an ssh-agent — and
//! runs unconfined there. For the master, the Bash allowlist
//! (`master::CLAUDE_ALLOWED_TOOLS`) is the barrier against arbitrary
//! exec; this sandbox is the backstop for reads (Claude Code's auto-
//! allowed read-only commands, `--file` arguments).

use std::path::PathBuf;

use crate::error::{Error, Result};

/// A filesystem policy: what the confined process may read, and what it
/// may also change. Everything else is denied.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Policy {
    /// Read, list and execute.
    pub read: Vec<PathBuf>,
    /// Every filesystem right the kernel handles.
    pub write: Vec<PathBuf>,
}

impl Policy {
    /// `cadence confine` arguments for this policy, up to (not
    /// including) the `--` that precedes the command.
    pub fn to_args(&self) -> Vec<String> {
        let mut args = Vec::new();
        for (flag, paths) in [("--read", &self.read), ("--write", &self.write)] {
            for p in paths {
                args.push(flag.to_string());
                args.push(p.to_string_lossy().to_string());
            }
        }
        args
    }
}

/// Confine this process to `policy`, then exec `command` (PATH lookup
/// as `execvp`). Returns only on failure.
pub fn exec(policy: &Policy, command: &[String]) -> Result<i32> {
    use std::os::unix::process::CommandExt;
    let Some((program, args)) = command.split_first() else {
        return Err(Error::rejected("cadence confine: no command after --"));
    };
    restrict_self(policy)?;
    let err = std::process::Command::new(program).args(args).exec();
    Err(Error::provider(format!(
        "cadence confine: cannot exec {program}: {err}"
    )))
}

#[cfg(target_os = "linux")]
pub use landlock::abi_version;

/// Ok when this host can confine a process — checked before the master
/// is registered, so an unconfinable host refuses `master start` with
/// the reason instead of launching a provider that dies at once.
pub fn available() -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        if abi_version() >= 1 {
            return Ok(());
        }
        Err(Error::provider(UNAVAILABLE))
    }
    #[cfg(not(target_os = "linux"))]
    {
        Err(Error::provider(UNSUPPORTED))
    }
}

#[cfg(target_os = "linux")]
const UNAVAILABLE: &str = "cadence confine: Landlock is unavailable on this kernel (needs Linux \
     5.13+ with landlock in the LSM list); refusing to run unconfined";
#[cfg(not(target_os = "linux"))]
const UNSUPPORTED: &str = "cadence confine: no filesystem sandbox on this platform (Linux \
     Landlock only); refusing to run unconfined";

#[cfg(target_os = "linux")]
fn restrict_self(policy: &Policy) -> Result<()> {
    landlock::restrict_self(policy)
}

#[cfg(not(target_os = "linux"))]
fn restrict_self(_policy: &Policy) -> Result<()> {
    Err(Error::provider(UNSUPPORTED))
}

#[cfg(target_os = "linux")]
mod landlock {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    use super::Policy;
    use crate::error::{Error, Result};

    const CREATE_RULESET_VERSION: u32 = 1;
    const RULE_PATH_BENEATH: libc::c_int = 1;
    /// ABI 6 scopes.
    const SCOPE_ABSTRACT_UNIX_SOCKET: u64 = 1 << 0;
    const SCOPE_SIGNAL: u64 = 1 << 1;

    const EXECUTE: u64 = 1 << 0;
    const WRITE_FILE: u64 = 1 << 1;
    const READ_FILE: u64 = 1 << 2;
    const READ_DIR: u64 = 1 << 3;
    /// ABI 1: bits 0..=12 (execute … make_sym).
    const ABI1: u64 = (1 << 13) - 1;
    const REFER: u64 = 1 << 13;
    const TRUNCATE: u64 = 1 << 14;
    const IOCTL_DEV: u64 = 1 << 15;
    /// Rights a rule on a non-directory may carry.
    const FILE_RIGHTS: u64 = EXECUTE | WRITE_FILE | READ_FILE | TRUNCATE | IOCTL_DEV;
    const READ_RIGHTS: u64 = EXECUTE | READ_FILE | READ_DIR;

    #[repr(C)]
    struct RulesetAttr {
        handled_access_fs: u64,
        handled_access_net: u64,
        scoped: u64,
    }

    #[repr(C, packed)]
    struct PathBeneathAttr {
        allowed_access: u64,
        parent_fd: i32,
    }

    /// The kernel's Landlock ABI version; 0 when Landlock is not
    /// compiled in or not enabled at boot.
    pub fn abi_version() -> i64 {
        // SAFETY: the documented version query — null attr, size 0.
        let v = unsafe {
            libc::syscall(
                libc::SYS_landlock_create_ruleset,
                std::ptr::null::<RulesetAttr>(),
                0usize,
                CREATE_RULESET_VERSION,
            )
        };
        v.max(0)
    }

    /// Every filesystem right this ABI version handles.
    fn handled(abi: i64) -> u64 {
        let mut rights = ABI1;
        if abi >= 2 {
            rights |= REFER;
        }
        if abi >= 3 {
            rights |= TRUNCATE;
        }
        if abi >= 5 {
            rights |= IOCTL_DEV;
        }
        rights
    }

    /// The scopes this ABI version enforces: none before ABI 6.
    pub fn scopes(abi: i64) -> u64 {
        if abi >= 6 {
            SCOPE_ABSTRACT_UNIX_SOCKET | SCOPE_SIGNAL
        } else {
            0
        }
    }

    pub fn restrict_self(policy: &Policy) -> Result<()> {
        let abi = abi_version();
        if abi < 1 {
            return Err(Error::provider(super::UNAVAILABLE));
        }
        let handled = handled(abi);
        let attr = RulesetAttr {
            handled_access_fs: handled,
            handled_access_net: 0,
            scoped: scopes(abi),
        };
        // The struct size each ABI knows: `scoped` arrived in ABI 6,
        // `handled_access_net` in ABI 4 — an older kernel refuses a
        // larger struct with a non-zero tail, and ours is zero there.
        let size = std::mem::size_of::<RulesetAttr>();
        // SAFETY: attr is a valid, fully initialised struct of `size`.
        let ruleset = unsafe {
            libc::syscall(
                libc::SYS_landlock_create_ruleset,
                &attr as *const RulesetAttr,
                size,
                0u32,
            )
        };
        if ruleset < 0 {
            return Err(os_err("landlock_create_ruleset"));
        }
        let ruleset = ruleset as libc::c_int;
        let result = (|| {
            for p in &policy.read {
                add_rule(ruleset, p, READ_RIGHTS & handled)?;
            }
            for p in &policy.write {
                add_rule(ruleset, p, handled)?;
            }
            // SAFETY: plain prctl; required before an unprivileged
            // landlock_restrict_self.
            if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0 {
                return Err(os_err("prctl(PR_SET_NO_NEW_PRIVS)"));
            }
            // SAFETY: ruleset is an open landlock ruleset fd.
            if unsafe { libc::syscall(libc::SYS_landlock_restrict_self, ruleset, 0u32) } != 0 {
                return Err(os_err("landlock_restrict_self"));
            }
            Ok(())
        })();
        // SAFETY: closing the fd this function opened.
        unsafe { libc::close(ruleset) };
        result
    }

    /// Allow `rights` beneath `path`. A missing path is skipped; a file
    /// keeps only the rights a file can carry.
    fn add_rule(ruleset: libc::c_int, path: &Path, rights: u64) -> Result<()> {
        let Ok(c) = CString::new(path.as_os_str().as_bytes()) else {
            return Err(Error::rejected(format!(
                "cadence confine: bad path {}",
                path.display()
            )));
        };
        // SAFETY: c is NUL-terminated; O_PATH opens without reading.
        let fd = unsafe { libc::open(c.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) };
        if fd < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::NotFound {
                return Ok(());
            }
            return Err(Error::provider(format!(
                "cadence confine: cannot open {}: {err}",
                path.display()
            )));
        }
        let rights = if path.is_dir() {
            rights
        } else {
            rights & FILE_RIGHTS
        };
        let attr = PathBeneathAttr {
            allowed_access: rights,
            parent_fd: fd,
        };
        // SAFETY: attr is valid for the call; fd is an O_PATH fd.
        let rc = unsafe {
            libc::syscall(
                libc::SYS_landlock_add_rule,
                ruleset,
                RULE_PATH_BENEATH,
                &attr as *const PathBeneathAttr,
                0u32,
            )
        };
        let err = std::io::Error::last_os_error();
        // SAFETY: closing the fd opened above.
        unsafe { libc::close(fd) };
        if rc != 0 {
            return Err(Error::provider(format!(
                "cadence confine: landlock_add_rule {}: {err}",
                path.display()
            )));
        }
        Ok(())
    }

    fn os_err(what: &str) -> Error {
        Error::provider(format!(
            "cadence confine: {what}: {}",
            std::io::Error::last_os_error()
        ))
    }
}
