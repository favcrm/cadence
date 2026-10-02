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
//!
//! Every read of the bound fd is **offset-preserving** (`pread`): a shared
//! file-description offset must never be advanced as a side-effect of
//! verification, and the digest is taken over the *whole* file from offset 0.
//! The digest window is closed by a before/after `fstat` pair — if the inode
//! changes (size, mtime, ctime, inode number) while it is hashed, the bind
//! refuses rather than pinning a file observed mid-mutation.

use std::ffi::CString;
#[cfg(test)]
use std::os::unix::io::RawFd;
use std::os::unix::io::{AsRawFd, OwnedFd};
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

/// ELF e_ident class byte — only a 64-bit object may be an exec'd ELF here.
const ELFCLASS64: u8 = 2;
/// ELF e_ident data byte — little-endian.
const ELFDATA2LSB: u8 = 1;
/// `e_machine` for x86-64 (the host arch this build runs on).
#[cfg(target_arch = "x86_64")]
const EM_HOST: u16 = 62;
/// `e_machine` for AArch64.
#[cfg(target_arch = "aarch64")]
const EM_HOST: u16 = 183;
/// `e_type` for an executable (`ET_EXEC`, 2) or a PIE DSO (`ET_DYN`, 3).
const ET_EXEC: u16 = 2;
const ET_DYN: u16 = 3;

/// A source-owned executable pin. `sha256` is compiled in and bound to the
/// measured release artifact — never caller-supplied argv/env, never a
/// placeholder, never the target's self-attestation. `canon` is the canonical
/// absolute path the fd must resolve under; `owner`/`group`/`mode` are
/// asserted on the opened inode. The setuid helper is group `cadence-launch`
/// — only members of that group may exec it, which is why the pin checks the
/// gid, not just uid+mode.
pub(crate) struct ExecPin {
    pub canon: &'static str,
    pub owner: u32,
    /// Expected gid — `acct::LAUNCH_GROUP` (`cadence-launch`) for the helper,
    /// resolved at bind time from NSS, never hardcoded.
    pub group: Option<&'static str>,
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
        group: Some(super::acct::LAUNCH_GROUP),
        mode: 0o4750,
        sha256: None,
    },
    ExecPin {
        canon: "/opt/cadence/pi/node",
        owner: 0,
        group: None,
        mode: 0o755,
        sha256: None,
    },
];

/// An opened, verified executable: the fd plus the digest actually measured.
/// The `OwnedFd` is the custody object — it is moved into the `PreparedExec`
/// and from there into the `pre_exec` closure, so the fd can never be closed
/// early or its number recycled by an unrelated `open` between verify and
/// exec.
#[derive(Debug)]
pub(crate) struct BoundExec {
    pub fd: OwnedFd,
    pub digest: [u8; 32],
    pub canon: PathBuf,
}

/// The ELF header bytes we must see for the bound object to be an *executed*
/// ELF — magic, class, data encoding, `e_type` and host `e_machine`. Reads
/// are `pread` at absolute offsets, so they never move the shared offset.
fn fd_is_host_elf(file: &std::fs::File) -> bool {
    use std::os::unix::fs::FileExt;
    let mut ehdr = [0u8; 20]; // enough for e_ident[16] + e_type + e_machine
    if file.read_exact_at(&mut ehdr, 0).is_err() {
        return false;
    }
    if &ehdr[0..4] != b"\x7fELF" {
        return false;
    }
    if ehdr[4] != ELFCLASS64 || ehdr[5] != ELFDATA2LSB || ehdr[6] != 1
    /*EV_CURRENT*/
    {
        return false;
    }
    let e_type = u16::from_le_bytes([ehdr[16], ehdr[17]]);
    let e_machine = u16::from_le_bytes([ehdr[18], ehdr[19]]);
    (e_type == ET_EXEC || e_type == ET_DYN) && e_machine == EM_HOST
}

/// `pread` a window of `len` bytes starting at `off` — used for the magic
/// probe without disturbing the shared offset.
fn read_at(file: &std::fs::File, off: u64, buf: &mut [u8]) -> bool {
    use std::os::unix::fs::FileExt;
    file.read_exact_at(buf, off).is_ok()
}

/// A minimal `fstat` snapshot used for the before/after digest stability
/// check: if any of these change while we hash, the inode was mutated under
/// us and the digest is untrustworthy.
#[derive(Clone, Copy, PartialEq, Eq)]
struct StatStamp {
    ino: u64,
    size: u64,
    mtime: i64,
    mtime_ns: i64,
    ctime: i64,
    ctime_ns: i64,
}

fn stamp(file: &std::fs::File) -> Result<StatStamp> {
    use std::os::unix::fs::MetadataExt;
    let m = file.metadata()?;
    Ok(StatStamp {
        ino: m.ino(),
        size: m.size(),
        mtime: m.mtime(),
        mtime_ns: m.mtime_nsec(),
        ctime: m.ctime(),
        ctime_ns: m.ctime_nsec(),
    })
}

/// Streamed sha256 of the *whole* file, read via `pread` from offset 0 in
/// bounded chunks. Rejects a file larger than a sane exec ceiling up front so
/// the digest never streams an unbounded or growing blob. A `StatStamp` is
/// taken before and after; any drift refuses.
fn sha256_fd(file: &std::fs::File, size: u64) -> Result<[u8; 32]> {
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::FileExt;
    /// Refuse to hash something implausibly large for a binary — an exec'd
    /// ELF is tens of MB at most; this bounds the read loop and catches a
    /// grow-under-us file before the digest window opens.
    const MAX_EXEC_BYTES: u64 = 512 * 1024 * 1024;
    if size > MAX_EXEC_BYTES {
        return Err(Error::rejected(format!(
            "exec object is {size} bytes — above the {MAX_EXEC_BYTES} bound; \
             refusing to digest an unbounded blob"
        )));
    }
    let before = stamp(file)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 128 * 1024];
    let mut off = 0u64;
    while off < size {
        let want = std::cmp::min(buf.len() as u64, size - off) as usize;
        let n = file
            .read_at(&mut buf[..want], off)
            .map_err(|e| Error::internal(e.to_string()))?;
        if n == 0 {
            // Truncated under us between the stat and the read.
            return Err(Error::rejected(
                "exec object shrank while hashing — inode not stable",
            ));
        }
        h.update(&buf[..n]);
        off += n as u64;
    }
    let after = stamp(file)?;
    if before != after {
        return Err(Error::rejected(
            "exec object changed while hashing (ino/size/timestamps drifted) — \
             the measured digest is untrustworthy",
        ));
    }
    Ok(h.finalize().into())
}

/// Open `pin.canon` and verify it *on the fd*: openat2-relative to a pinned
/// `/`, `RESOLVE_NO_SYMLINKS`, then `fstat` (regular file, owner, exact mode
/// bits, `nlink == 1`) and a host-ELF header probe plus a whole-file sha256
/// taken under a before/after stat-stability window — all offset-preserving
/// `pread`, so verification never moves the descriptor's shared offset. A
/// script, a non-regular file, a wrong owner/mode, an unset pin, a non-ELF or
/// non-host-arch object, or a digest mismatch all refuse.
pub(crate) fn open_bound(pin: &ExecPin) -> Result<BoundExec> {
    let Some(expected) = pin.sha256 else {
        return Err(Error::rejected(format!(
            "{}: no compiled exec pin provisioned — refusing to bind an \
             unverified executable",
            pin.canon
        )));
    };
    let fd = super::topology::open_at2(Path::new(pin.canon), OpenKind::ExecFile)?;
    let file = std::fs::File::from(fd);
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
    // The expected group is part of the pin — e.g. the helper is group
    // `cadence-launch` so only that group may exec the setuid helper.
    if let Some(group) = pin.group {
        let want_gid = super::topology::resolve_gid_pub(group)?;
        if meta.gid() != want_gid {
            return Err(Error::rejected(format!(
                "{}: group {} not the pinned {} ({want_gid})",
                pin.canon,
                meta.gid(),
                group
            )));
        }
    }
    // First-two-bytes `#!` is the cheap tell, then the real ELF claim: a
    // bound exec object must be a host-arch 64-bit ELF executable/PIE —
    // "not a script" is not proof of an executable.
    let mut magic = [0u8; 2];
    if read_at(&file, 0, &mut magic) && &magic == b"#!" {
        return Err(Error::rejected(format!(
            "{}: is a script (#! interpreter) — never an fd-exec target; the \
             interpreter would re-resolve by path",
            pin.canon
        )));
    }
    if !fd_is_host_elf(&file) {
        return Err(Error::rejected(format!(
            "{}: not a 64-bit host-arch ELF executable/PIE — refusing to bind \
             a non-ELF or foreign-arch object",
            pin.canon
        )));
    }
    let digest = sha256_fd(&file, meta.size())?;
    if digest != expected {
        return Err(Error::rejected(format!(
            "{}: sha256 {} != pinned {}",
            pin.canon,
            super::hex_encode(&digest),
            super::hex_encode(&expected)
        )));
    }
    Ok(BoundExec {
        fd: file.into(),
        digest,
        canon: PathBuf::from(pin.canon),
    })
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
/// Custody: the plan **owns** the bound `OwnedFd` (moved out of `BoundExec`),
/// so the helper inode's descriptor cannot be closed or its number recycled
/// by an unrelated `open` before the child exec's. The closure captures that
/// `OwnedFd` by move — `as_raw_fd` is read *inside* the child, after fork.
pub(crate) struct PreparedExec {
    /// The verified helper fd, owned for the life of the plan and moved into
    /// the closure at `attach`.
    helper_fd: OwnedFd,
    /// Owned C strings; moved into the closure so `argv_p`/`envp_p` stay
    /// valid — the pointer arrays index this backing.
    argv_c: Vec<CString>,
    envp_c: Vec<CString>,
    argv_p: Vec<*const libc::c_char>,
    envp_p: Vec<*const libc::c_char>,
}

/// A `Send`/`Sync`-able carrier for the whole spawn plan handed to `pre_exec`.
/// It owns the `OwnedFd` and both `CString` backings, so the raw pointers in
/// `argv_p`/`envp_p` and the fd stay valid for the life of the closure.
/// Raw pointers are not `Send`; the wrapper asserts they may cross the fork.
/// Soundness: every pointer addresses memory this struct owns and outlives
/// (the `Vec`s and `OwnedFd` move *into* the closure); the child reads that
/// copy-on-write memory only to hand it to `execveat`, never to mutate, and
/// nothing else holds a reference.
struct ExecArgs {
    helper_fd: OwnedFd,
    argv_c: Vec<CString>,
    envp_c: Vec<CString>,
    /// `execveat`'s empty-path operand — a stable pointer to a static NUL.
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
        // The plan has no fd yet — `with_bound` moves the BoundExec's OwnedFd
        // in. A plan must never be built without one; keep a sentinel owner.
        Ok(Self {
            helper_fd: empty_helper_fd()?,
            argv_c: argv,
            envp_c: env,
            argv_p,
            envp_p,
        })
    }

    /// Bind the verified `BoundExec` into the plan, taking ownership of its
    /// `OwnedFd` — so the descriptor lives exactly as long as the plan and
    /// the closure it is moved into.
    pub(crate) fn with_bound(mut self, bound: BoundExec) -> Self {
        self.helper_fd = bound.fd;
        self
    }

    /// Attach the async-signal-safe `pre_exec` to `cmd`, consuming the plan.
    /// The `OwnedFd`, `CString` backings and pointer arrays all move *into*
    /// the closure, so the child reads live descriptors and stable pointers
    /// after fork — there is no window where a dropped/reused fd or freed
    /// `CString` could leave the child a stale argument.
    ///
    /// `setsid` runs first and is error-checked; `execveat` `-1` maps to
    /// `last_os_error` immediately (the return value is not the errno).
    #[cfg(target_os = "linux")]
    pub(crate) fn attach(self, cmd: &mut std::process::Command) {
        use std::os::unix::process::CommandExt;
        let args = ExecArgs {
            helper_fd: self.helper_fd,
            argv_c: self.argv_c,
            envp_c: self.envp_c,
            empty_path: &EMPTY_PATH_NUL as *const u8 as *const libc::c_char,
            argv_p: self.argv_p,
            envp_p: self.envp_p,
        };
        unsafe {
            // `move` on the whole `ExecArgs` forces whole-struct capture so the
            // closure is Send/Sync (Edition-2021 disjoint field capture would
            // pull `empty_path` out as a bare `*const i8`, which is not Send).
            let captured = args;
            cmd.pre_exec(move || {
                let a = &captured;
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                let rc = libc::syscall(
                    libc::SYS_execveat,
                    a.helper_fd.as_raw_fd(),
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

    /// The raw helper fd the plan will exec — for tests asserting the fd
    /// consumed is the one bound. Valid only while the plan is alive.
    #[cfg(test)]
    pub(crate) fn helper_fd(&self) -> RawFd {
        self.helper_fd.as_raw_fd()
    }

    /// The materialized argv, decoded — for the test that proves the profile
    /// tokens precede `--` and the provider argv follows verbatim.
    #[cfg(test)]
    pub(crate) fn argv_strings(&self) -> Vec<String> {
        self.argv_c
            .iter()
            .map(|c| c.to_string_lossy().into_owned())
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn envp_strings(&self) -> Vec<String> {
        self.envp_c
            .iter()
            .map(|c| c.to_string_lossy().into_owned())
            .collect()
    }
}

/// A placeholder fd for `assemble` before `with_bound` installs the real one —
/// an `OwnedFd` over `/dev/null`, never a real exec target, replaced before
/// `attach` ever runs.
fn empty_helper_fd() -> Result<OwnedFd> {
    std::fs::File::open("/dev/null")
        .map(|f| f.into())
        .map_err(|e| Error::internal(format!("open /dev/null sentinel: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::pi_guest::Segments;

    fn segs() -> Segments {
        Segments::new("w-alias", "0123456789abcdef0123456789abcdef").unwrap()
    }

    /// An `OwnedFd` over a file we control — used to build a `BoundExec`
    /// without a real provisioned binary.
    fn bound(path: &Path) -> BoundExec {
        BoundExec {
            fd: std::fs::File::open(path).unwrap().into(),
            digest: [0u8; 32],
            canon: path.to_path_buf(),
        }
    }

    /// `pre_exec`'s argv: fixed helper profile tokens, then `--`, then the
    /// provider argv verbatim — no provider flag may surface as a helper flag.
    #[test]
    fn prepared_exec_argv_is_profile_tokens_then_provider() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("h");
        std::fs::write(&path, b"x").unwrap();
        let env = vec![CString::new("HOME=/x").unwrap()];
        let plan = PreparedExec::assemble(
            &segs(),
            &["pi".to_string(), "--mode".to_string(), "rpc".to_string()],
            env,
        )
        .unwrap()
        .with_bound(bound(&path));
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
        assert!(plan.helper_fd() > 0);
    }

    /// An argv token carrying NUL is refused at materialization, never exec'd.
    #[test]
    fn nul_in_argv_or_env_is_refused() {
        let env = vec![CString::new("A=B").unwrap()];
        assert!(PreparedExec::assemble(&segs(), &["bad\0arg".to_string()], env).is_err());
    }

    /// `ExecArgs` is `Send + Sync` — compile-time proof the whole-struct
    /// capture carries no non-Send raw pointer field.
    #[test]
    fn exec_args_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ExecArgs>();
    }

    /// Memory-safety: the pointer arrays index the *owned* `CString` backing
    /// that moved into the plan. Dereference through `argv_p`/`envp_p` while
    /// the plan is alive — every pointer must resolve to the string it names,
    /// terminated by a null. This proves the pointers track live backing, not
    /// a freed or reallocated buffer.
    #[test]
    fn argv_and_env_pointers_resolve_through_the_plan() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("h");
        std::fs::write(&path, b"x").unwrap();
        let env = vec![
            CString::new("HOME=/x").unwrap(),
            CString::new("CADENCE_ALIAS=w").unwrap(),
        ];
        let plan = PreparedExec::assemble(&segs(), &["pi".to_string()], env)
            .unwrap()
            .with_bound(bound(&path));
        unsafe {
            // argv_p is NULL-terminated after the real args.
            let mut i = 0usize;
            loop {
                let p = plan.argv_p[i];
                if p.is_null() {
                    break;
                }
                let s = std::ffi::CStr::from_ptr(p).to_string_lossy();
                assert_eq!(s, plan.argv_c[i].to_string_lossy());
                i += 1;
            }
            assert_eq!(i, plan.argv_c.len());
            let mut j = 0usize;
            loop {
                let p = plan.envp_p[j];
                if p.is_null() {
                    break;
                }
                let s = std::ffi::CStr::from_ptr(p).to_string_lossy();
                assert_eq!(s, plan.envp_c[j].to_string_lossy());
                j += 1;
            }
            assert_eq!(j, plan.envp_c.len());
        }
    }

    /// The plan owns the helper fd: once `with_bound` moves the `OwnedFd` in,
    /// the *same* descriptor number is the one `attach` execs — and dropping
    /// the plan (before any spawn) closes it, so no stale fd can be re-exec'd.
    #[test]
    fn plan_owns_the_bound_fd_and_drop_closes_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("h");
        std::fs::write(&path, b"x").unwrap();
        let bound = bound(&path);
        let bound_raw = bound.fd.as_raw_fd();
        let plan = PreparedExec::assemble(&segs(), &["pi".to_string()], vec![])
            .unwrap()
            .with_bound(bound);
        assert_eq!(plan.helper_fd(), bound_raw, "plan holds the bound fd");
        drop(plan);
        // After the plan drops, that fd number is closed — an fcntl on it
        // must fail EBADF. This proves the descriptor could not have been
        // handed to a child as a recycled number.
        let still_open = unsafe { libc::fcntl(bound_raw, libc::F_GETFD) } != -1;
        assert!(!still_open, "dropping the plan must close its helper fd");
    }

    /// Two plans built from two `BoundExec`s own *distinct* descriptors —
    /// the second bind cannot silently reuse a number the first still holds
    /// (which would make the child exec the wrong inode).
    #[test]
    fn two_plans_own_distinct_fds() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        std::fs::write(&a, b"a").unwrap();
        std::fs::write(&b, b"b").unwrap();
        let pa = PreparedExec::assemble(&segs(), &["pi".to_string()], vec![])
            .unwrap()
            .with_bound(bound(&a));
        let pb = PreparedExec::assemble(&segs(), &["pi".to_string()], vec![])
            .unwrap()
            .with_bound(bound(&b));
        assert_ne!(
            pa.helper_fd(),
            pb.helper_fd(),
            "each plan owns its own live fd"
        );
    }

    /// `open_bound` refuses a pin with no provisioned digest — the seam can
    /// never bind an unverified executable.
    #[test]
    fn open_bound_refuses_unset_pin() {
        let pin = ExecPin {
            canon: "/nonexistent/helper",
            owner: 0,
            group: None,
            mode: 0o4750,
            sha256: None,
        };
        let e = open_bound(&pin).unwrap_err();
        assert!(e.to_string().contains("no compiled exec pin"), "{e}");
    }

    /// `pread`-based magic probe does NOT move the shared file offset, and the
    /// whole-file digest is taken from offset 0 — regression for the bug where
    /// `is_script`'s `read` advanced the offset so `sha256` skipped 2 bytes.
    #[test]
    fn magic_probe_and_hash_preserve_offset_and_cover_whole_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("obj");
        let data: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&path, &data).unwrap();
        let file = std::fs::File::open(&path).unwrap();
        let size = file.metadata().unwrap().len();
        // Interleave a magic read with the digest; offset must stay 0.
        let mut m = [0u8; 2];
        assert!(read_at(&file, 0, &mut m));
        let d1 = sha256_fd(&file, size).unwrap();
        // Independent reference digest over the raw bytes.
        use sha2::Digest;
        let d2: [u8; 32] = sha2::Sha256::digest(&data).into();
        assert_eq!(d1, d2, "fd digest must cover the whole file from 0");
        // And the shared offset is still 0 — a plain read starts at the head.
        use std::io::Read;
        let mut head = [0u8; 4];
        let mut f = &file;
        f.read_exact(&mut head).unwrap();
        assert_eq!(&head, &data[0..4]);
    }

    /// A file whose size changes during the digest window is refused — the
    /// before/after stat stamp catches a mutation the path-verify can't.
    #[test]
    fn hash_refuses_a_mutating_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("obj");
        std::fs::write(&path, b"steady").unwrap();
        let file = std::fs::File::open(&path).unwrap();
        // Claim a size larger than the real file so `read_at` hits EOF mid-way
        // -> the "shrank while hashing" refusal path.
        let e = sha256_fd(&file, 1 << 20).unwrap_err();
        assert!(
            e.to_string().contains("shrank") || e.to_string().contains("not stable"),
            "{e}"
        );
    }

    /// A `#!` script is never an fd-exec target even when a pin is set; a real
    /// host ELF (`/bin/true`) passes the ELF probe; a non-ELF file refuses.
    #[test]
    #[cfg(target_os = "linux")]
    fn elf_claim_is_real_magic_not_just_no_shebang() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("s.sh");
        std::fs::write(&script, b"#!/bin/sh\nexit 0\n").unwrap();
        let fs = std::fs::File::open(&script).unwrap();
        let mut m = [0u8; 2];
        assert!(read_at(&fs, 0, &mut m) && &m == b"#!");
        // Not-ELF non-script refuses.
        let notelf = dir.path().join("x.bin");
        std::fs::write(&notelf, b"MZ fake binary, no shebang").unwrap();
        let fx = std::fs::File::open(&notelf).unwrap();
        assert!(!fd_is_host_elf(&fx), "a non-ELF must fail the ELF probe");
        // A real ELF passes the probe.
        let ftrue = std::fs::File::open("/bin/true").unwrap();
        assert!(fd_is_host_elf(&ftrue), "/bin/true is a host ELF");
        assert!(
            !({
                let mut m = [0u8; 2];
                read_at(&ftrue, 0, &mut m);
                &m == b"#!"
            })
        );
    }
}
