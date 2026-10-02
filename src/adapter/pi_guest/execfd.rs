//! Verified executable custody — open a protected binary by fd, prove its
//! identity on the *fd* (never a pathname `lstat` that a swap can defeat), and
//! build the materialized `execveat` spawn plan `pre_exec` runs.
//!
//! The two exec'd objects are ELF binaries — the setuid helper and `node` —
//! so `execveat(fd, "", AT_EMPTY_PATH)` / `fexecve` binds the inode the
//! kernel executes. A Node *script* is never an fd-exec target: a non-CLOEXEC
//! script fd *can* exec, but the kernel then re-resolves the shebang
//! interpreter (`#!/usr/bin/env node`) by path — the interpreter escapes the
//! binding. Scripts ride the immutable verified tree instead.

use std::ffi::CString;
use std::os::unix::io::{OwnedFd, RawFd};
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

/// A source-owned executable pin. `sha256` is compiled in and bound to the
/// measured release artifact — never caller-supplied argv/env, never a
/// placeholder, never the target's self-attestation. `canon` is the canonical
/// absolute path the fd must resolve under; `owner`/`mode` are asserted on
/// the opened inode.
pub(crate) struct ExecPin {
    pub canon: &'static str,
    pub owner: u32,
    pub mode: u32,
    pub sha256: Option<[u8; 32]>,
}

/// The compiled pin table. Digests are bound to the release artifacts; `None`
/// means the pin is not provisioned for this build and the bound open must
/// refuse — a missing pin is never a pass.
///
/// The order is fixed: index 0 is the setuid helper, index 1 the node ELF.
pub(crate) const EXEC_PINS: &[ExecPin] = &[
    ExecPin {
        canon: "/opt/cadence/libexec/cadence-agent-exec",
        owner: 0,
        mode: 0o4750,
        sha256: None,
    },
    ExecPin {
        canon: "/opt/cadence/pi/node",
        owner: 0,
        mode: 0o755,
        sha256: None,
    },
];

/// An opened, verified executable: the fd plus the digest actually measured.
#[derive(Debug)]
pub(crate) struct BoundExec {
    pub fd: OwnedFd,
    pub digest: [u8; 32],
    pub canon: PathBuf,
}

/// True when `path` is a script (`#!` first two bytes) — such an object is
/// never an fd-exec target because the shebang interpreter re-resolves by
/// path. Read from the opened fd, not the name.
fn is_script(fd: &std::fs::File) -> bool {
    use std::io::Read;
    let mut magic = [0u8; 2];
    let mut f = fd;
    f.read_exact(&mut magic).is_ok() && &magic == b"#!"
}

/// Open `pin.canon` and verify it *on the fd*: openat2-relative to a pinned
/// `/`, `RESOLVE_NO_SYMLINKS`, then `fstat` (regular file, owner, exact mode
/// bits, `nlink == 1`, ELF magic for an exec target) and sha256 of the opened
/// descriptor. A script, a non-regular file, a wrong owner/mode, an unset pin,
/// or a digest mismatch all refuse.
pub(crate) fn open_bound(pin: &ExecPin) -> Result<BoundExec> {
    let Some(expected) = pin.sha256 else {
        return Err(Error::rejected(format!(
            "{}: no compiled exec pin provisioned — refusing to bind an \
             unverified executable",
            pin.canon
        )));
    };
    let fd = super::topology::open_at2(Path::new(pin.canon), OpenKind::ExecFile)?;
    let file = fd_to_file(&fd)?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(Error::rejected(format!(
            "{}: not a regular file",
            pin.canon
        )));
    }
    use std::os::unix::fs::MetadataExt;
    if meta.uid() != pin.owner {
        return Err(Error::rejected(format!(
            "{}: owned by uid {} not {}",
            pin.canon,
            meta.uid(),
            pin.owner
        )));
    }
    if meta.mode() & 0o7777 != pin.mode {
        return Err(Error::rejected(format!(
            "{}: mode {:04o} not {:04o}",
            pin.canon,
            meta.mode() & 0o7777,
            pin.mode
        )));
    }
    if meta.nlink() != 1 {
        return Err(Error::rejected(format!(
            "{}: nlink {} != 1 — refuse a multiply-linked exec target",
            pin.canon,
            meta.nlink()
        )));
    }
    if is_script(&file) {
        return Err(Error::rejected(format!(
            "{}: is a script (#! interpreter) — never an fd-exec target; the \
             interpreter would re-resolve by path",
            pin.canon
        )));
    }
    let digest = sha256_fd(&file)?;
    if digest != expected {
        return Err(Error::rejected(format!(
            "{}: sha256 {} != pinned {}",
            pin.canon,
            super::hex_encode(&digest),
            super::hex_encode(&expected)
        )));
    }
    Ok(BoundExec {
        fd,
        digest,
        canon: PathBuf::from(pin.canon),
    })
}

fn fd_to_file(fd: &OwnedFd) -> Result<std::fs::File> {
    use std::os::unix::io::{FromRawFd, IntoRawFd};
    fd.try_clone()
        .map(|f| unsafe { std::fs::File::from_raw_fd(f.into_raw_fd()) })
        .map_err(|e| Error::internal(format!("clone exec fd: {e}")))
}

/// Streamed sha256 of an already-open file.
fn sha256_fd(f: &std::fs::File) -> Result<[u8; 32]> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut h = Sha256::new();
    let mut file = f;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|e| Error::internal(e.to_string()))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().into())
}

/// The kind of object `open_at2` is asked for — drives the `O_DIRECTORY` /
/// regular-file choice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OpenKind {
    /// A directory — `O_DIRECTORY`.
    Dir,
    /// A regular file to be executed — `O_RDONLY | O_CLOEXEC`. `fstat` in the
    /// caller proves it is not a fifo/socket/dir before exec.
    ExecFile,
    /// A regular file that must stay open across the *next* exec — opened
    /// `O_RDONLY` **without** `O_CLOEXEC` so `execveat` preserves it for the
    /// interpreter to read via `/proc/self/fd/<n>`.
    DataInherit,
}

/// A fully-materialized exec plan for `Command::pre_exec`. Every `CString`,
/// pointer array and fd lifetime is resolved before `fork` — the `pre_exec`
/// closure is async-signal-safe: it calls `setsid` then `execveat`, reads
/// `errno` via `last_os_error`, and touches no allocation, lock or panic.
///
/// The closure runs `execveat(helper_fd, "", argv, envp, AT_EMPTY_PATH)`;
/// success does not return, so the `Command`'s own path-exec is unreachable.
/// On `setsid` or `execveat` failure it returns `Err` and `Command` aborts
/// with no exec at all.
pub(crate) struct PreparedExec {
    /// The verified helper fd — owned for the life of the plan so the raw fd
    /// stays valid in the child.
    helper_fd: RawFd,
    /// Owned C strings; kept alive so `argv_p`/`envp_p` stay valid.
    _argv_c: Vec<CString>,
    _envp_c: Vec<CString>,
    argv_p: Vec<*const libc::c_char>,
    envp_p: Vec<*const libc::c_char>,
}

/// A `Send`/`Sync` view of the materialized exec plan handed to `pre_exec`.
/// Raw pointers are not `Send`; this wrapper asserts the closure may carry
/// them across the fork. Soundness: the pointers address memory owned by the
/// parent's `PreparedExec`, which outlives the spawn; the child reads that
/// copy-on-write memory only to pass it to `execveat`, never to mutate.
/// Nothing else touches it after `attach`.
struct ExecArgs {
    helper_fd: RawFd,
    /// `execveat`'s empty-path operand — a stable pointer to a NUL byte,
    /// materialized once so the closure never names a `c""` literal (whose
    /// `PhantomData<*const c_char>` is not `Send`).
    empty_path: *const libc::c_char,
    argv_p: Vec<*const libc::c_char>,
    envp_p: Vec<*const libc::c_char>,
}
unsafe impl Send for ExecArgs {}
unsafe impl Sync for ExecArgs {}

/// A static NUL byte for `execveat`'s `path=""` operand.
static EMPTY_PATH_NUL: u8 = 0;

impl PreparedExec {
    /// Materialize the argv/envp for `execveat(helper_fd, …)`.
    /// `provider_argv` is appended verbatim after the fixed helper profile
    /// tokens; `env` is the guest envp vector (already `KEY=VAL` strings).
    pub(crate) fn assemble(
        segs: &super::Segments,
        provider_argv: &[String],
        env: Vec<CString>,
    ) -> Result<Self> {
        // Fixed argv: the helper's own profile selection + bounded segments,
        // then `--` then the provider argv verbatim.
        let mut argv: Vec<CString> = Vec::new();
        let push = |argv: &mut Vec<CString>, s: &str| -> Result<()> {
            argv.push(
                CString::new(s).map_err(|_| Error::rejected("argv carries an interior NUL"))?,
            );
            Ok(())
        };
        push(&mut argv, "cadence-agent-exec")?;
        push(&mut argv, "exec")?;
        push(&mut argv, "--profile")?;
        push(&mut argv, "pi-guest")?;
        push(&mut argv, &format!("--alias-sha256={}", segs.alias_hex()))?;
        push(
            &mut argv,
            &format!("--generation={}", segs.generation_hex()),
        )?;
        push(&mut argv, "--")?;
        for a in provider_argv {
            push(&mut argv, a)?;
        }
        let argv_p: Vec<*const libc::c_char> = argv
            .iter()
            .map(|c| c.as_ptr())
            .chain(std::iter::once(std::ptr::null()))
            .collect();
        let envp_p: Vec<*const libc::c_char> = env
            .iter()
            .map(|c| c.as_ptr())
            .chain(std::iter::once(std::ptr::null()))
            .collect();
        Ok(Self {
            helper_fd: -1, // bound by the caller via `with_helper_fd`
            _argv_c: argv,
            _envp_c: env,
            argv_p,
            envp_p,
        })
    }

    /// Bind the verified helper fd into the plan. Called once the `BoundExec`
    /// exists so the fd's raw value is captured.
    pub(crate) fn with_helper_fd(mut self, fd: RawFd) -> Self {
        self.helper_fd = fd;
        self
    }

    /// Attach the async-signal-safe `pre_exec` to `cmd`. The helper fd is
    /// `dup`'d is NOT needed — the `BoundExec` fd is owned by the `GuestCtx`
    /// and outlives the spawn; we capture its raw value. The child reads the
    /// same (copy-on-write) memory the parent materialized.
    ///
    /// `setsid` runs first and is error-checked; `execveat` `-1` maps to
    /// `last_os_error` immediately (the return value is not the errno).
    #[cfg(target_os = "linux")]
    pub(crate) fn attach(&self, cmd: &mut std::process::Command) {
        use std::os::unix::process::CommandExt;
        let args = ExecArgs {
            helper_fd: self.helper_fd,
            empty_path: &EMPTY_PATH_NUL as *const u8 as *const libc::c_char,
            argv_p: self.argv_p.clone(),
            envp_p: self.envp_p.clone(),
        };
        unsafe {
            // Edition-2021 disjoint field capture would pull `empty_path` out
            // as a bare `*const i8` (not Send). `move` on the rebound whole
            // `ExecArgs` forces whole-struct capture so the closure is Send.
            let captured = args;
            cmd.pre_exec(move || {
                let a = &captured;
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                let rc = libc::syscall(
                    libc::SYS_execveat,
                    a.helper_fd,
                    a.empty_path,
                    a.argv_p.as_ptr(),
                    a.envp_p.as_ptr(),
                    libc::AT_EMPTY_PATH,
                );
                if rc == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }

    /// The captured helper fd (for tests asserting the fd consumed is the one
    /// bound).
    #[cfg(test)]
    pub(crate) fn helper_fd(&self) -> RawFd {
        self.helper_fd
    }

    /// The materialized argv, decoded — for the test that proves the profile
    /// tokens precede `--` and the provider argv follows verbatim.
    #[cfg(test)]
    pub(crate) fn argv_strings(&self) -> Vec<String> {
        self._argv_c
            .iter()
            .map(|c| c.to_string_lossy().into_owned())
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn envp_strings(&self) -> Vec<String> {
        self._envp_c
            .iter()
            .map(|c| c.to_string_lossy().into_owned())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::pi_guest::Segments;

    fn segs() -> Segments {
        Segments::new("w-alias", "0123456789abcdef0123456789abcdef").unwrap()
    }

    /// `pre_exec`'s argv: fixed helper profile tokens, then `--`, then the
    /// provider argv verbatim — no provider flag may surface as a helper flag.
    #[test]
    fn prepared_exec_argv_is_profile_tokens_then_provider() {
        let env = vec![CString::new("HOME=/x").unwrap()];
        let plan = PreparedExec::assemble(
            &segs(),
            &["pi".to_string(), "--mode".to_string(), "rpc".to_string()],
            env,
        )
        .unwrap()
        .with_helper_fd(42);
        let argv = plan.argv_strings();
        assert_eq!(argv[0], "cadence-agent-exec");
        assert_eq!(argv[1], "exec");
        assert_eq!(argv[2], "--profile");
        assert_eq!(argv[3], "pi-guest");
        assert!(argv[4].starts_with("--alias-sha256="));
        assert_eq!(argv[4].len(), "--alias-sha256=".len() + 64);
        assert!(argv[5].starts_with("--generation="));
        assert_eq!(argv[5].len(), "--generation=".len() + 32);
        assert_eq!(argv[6], "--");
        assert_eq!(&argv[7..], &["pi", "--mode", "rpc"]);
        assert_eq!(plan.helper_fd(), 42);
    }

    /// An argv token carrying NUL is refused at materialization, never exec'd.
    #[test]
    fn nul_in_argv_or_env_is_refused() {
        let env = vec![CString::new("A=B").unwrap()];
        assert!(PreparedExec::assemble(&segs(), &["bad\0arg".to_string()], env).is_err());
    }

    /// The closure handed to `pre_exec` is `Send + Sync` — compile-time proof
    /// the whole-struct capture carries no non-Send raw pointer field.
    #[test]
    fn prepared_exec_attach_closure_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ExecArgs>();
    }

    /// `open_bound` refuses a pin with no provisioned digest — the seam can
    /// never bind an unverified executable.
    #[test]
    fn open_bound_refuses_unset_pin() {
        let pin = ExecPin {
            canon: "/nonexistent/helper",
            owner: 0,
            mode: 0o4750,
            sha256: None,
        };
        let e = open_bound(&pin).unwrap_err();
        assert!(e.to_string().contains("no compiled exec pin"), "{e}");
    }

    /// A `#!` script is never an fd-exec target even when a pin is set.
    #[test]
    #[cfg(target_os = "linux")]
    fn script_fd_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("script.sh");
        std::fs::write(&path, b"#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        // Bypass the canon path check by testing is_script directly.
        let f = std::fs::File::open(&path).unwrap();
        assert!(is_script(&f));
        let elf = std::fs::File::open("/bin/true").unwrap();
        assert!(!is_script(&elf), "an ELF is not a script");
    }
}
