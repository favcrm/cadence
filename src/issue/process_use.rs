//! Read-only process-use scanning shared by checkout and scratch cleanup.
//!
//! Owns the proc-view completeness proof, cwd/open-FD enumeration and scan
//! failures. Callers decide whether to delete; an incomplete scan never proves
//! that a checkout is unused.

use std::path::{Path, PathBuf};

#[cfg(target_os = "linux")]
use std::ffi::CString;
#[cfg(target_os = "linux")]
use std::fs::File;
#[cfg(target_os = "linux")]
use std::io::{self, Read};
#[cfg(target_os = "linux")]
use std::os::fd::{AsRawFd, FromRawFd};
#[cfg(target_os = "linux")]
use std::os::unix::ffi::OsStrExt;
#[cfg(target_os = "linux")]
use std::os::unix::fs::{FileTypeExt, MetadataExt};

use crate::error::{Error, Result};

#[cfg(test)]
thread_local! {
    static PROCESS_PROC_ROOT: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) struct ProcessProcRootGuard(Option<PathBuf>);

#[cfg(test)]
impl Drop for ProcessProcRootGuard {
    fn drop(&mut self) {
        PROCESS_PROC_ROOT.with(|root| *root.borrow_mut() = self.0.take());
    }
}

#[cfg(test)]
pub(crate) fn use_process_proc_root_for_test(root: PathBuf) -> ProcessProcRootGuard {
    let previous = PROCESS_PROC_ROOT.with(|current| current.borrow_mut().replace(root));
    ProcessProcRootGuard(previous)
}

fn process_proc_root() -> PathBuf {
    #[cfg(test)]
    if let Some(root) = PROCESS_PROC_ROOT.with(|root| root.borrow().clone()) {
        return root;
    }
    // Release builds reject `test-seam`; integration checks can isolate proc
    // enumeration without weakening the production `/proc` path.
    #[cfg(feature = "test-seam")]
    if let Some(root) = std::env::var_os("CADENCE_TEST_PROC_ROOT") {
        return PathBuf::from(root);
    }
    PathBuf::from("/proc")
}

#[cfg(target_os = "linux")]
fn decode_mountinfo_path(value: &str) -> Option<PathBuf> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'\\' {
            decoded.push(bytes[i]);
            i += 1;
            continue;
        }
        let digits = bytes.get(i + 1..i + 4)?;
        let escaped = match digits {
            b"040" => b' ',
            b"011" => b'\t',
            b"012" => b'\n',
            b"134" => b'\\',
            _ => return None,
        };
        decoded.push(escaped);
        i += 4;
    }
    if decoded.contains(&0) {
        return None;
    }
    String::from_utf8(decoded).ok().map(PathBuf::from)
}

#[cfg(target_os = "linux")]
fn validate_proc_mount_options(options: &[&str]) -> std::result::Result<(), String> {
    let mut hidepid_seen = false;
    for option in options.iter().flat_map(|options| options.split(',')) {
        if option.is_empty() {
            return Err("mountinfo contains an empty mount option".into());
        }
        if option == "hidepid" {
            return Err("proc mount has an unparseable hidepid option".into());
        }
        if let Some(value) = option.strip_prefix("hidepid=") {
            if hidepid_seen {
                return Err("proc mount has ambiguous duplicate hidepid options".into());
            }
            hidepid_seen = true;
            if value != "0" {
                return Err(format!(
                    "proc mount restricts process visibility with hidepid={value}"
                ));
            }
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
struct ProcView {
    root: File,
    canonical: PathBuf,
    scan_root: PathBuf,
    synthetic: bool,
}

#[cfg(target_os = "linux")]
fn proc_view_error(path: &Path, reason: impl std::fmt::Display) -> Error {
    Error::rejected(format!(
        "cannot prove complete procfs visibility for {}: {reason}",
        path.display()
    ))
}

#[cfg(target_os = "linux")]
impl ProcView {
    fn open(proc_root: &Path) -> Result<Self> {
        let canonical = proc_root.canonicalize().map_err(|error| {
            proc_view_error(proc_root, format!("cannot resolve proc root: {error}"))
        })?;
        let c_path = CString::new(canonical.as_os_str().as_bytes())
            .map_err(|_| proc_view_error(&canonical, "proc root contains NUL"))?;
        let fd = unsafe {
            libc::open(
                c_path.as_ptr(),
                libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if fd < 0 {
            return Err(proc_view_error(
                &canonical,
                format!("cannot open proc root: {}", io::Error::last_os_error()),
            ));
        }
        let root = unsafe { File::from_raw_fd(fd) };
        let root_meta = root.metadata().map_err(|error| {
            proc_view_error(&canonical, format!("cannot inspect opened root: {error}"))
        })?;
        if !root_meta.is_dir() {
            return Err(proc_view_error(
                &canonical,
                "opened root is not a directory",
            ));
        }
        let scan_root = PathBuf::from(format!("/proc/self/fd/{}", root.as_raw_fd()));
        let anchor_meta = std::fs::metadata(&scan_root).map_err(|error| {
            proc_view_error(
                &canonical,
                format!(
                    "cannot resolve held root through {}: {error}",
                    scan_root.display()
                ),
            )
        })?;
        if !anchor_meta.is_dir()
            || anchor_meta.dev() != root_meta.dev()
            || anchor_meta.ino() != root_meta.ino()
        {
            return Err(proc_view_error(
                &canonical,
                "anchored scan path does not identify the opened root directory",
            ));
        }
        let root_is_nonprocfs =
            fstatfs_type(&root).is_ok_and(|kind| kind != libc::PROC_SUPER_MAGIC);
        let synthetic = root_is_nonprocfs
            && canonical != Path::new("/proc")
            && (cfg!(test) || cfg!(feature = "test-seam"));
        Ok(Self {
            root,
            canonical,
            scan_root,
            synthetic,
        })
    }

    fn open_at(&self, relative: &str, flags: libc::c_int) -> io::Result<File> {
        let relative = CString::new(relative)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "proc path contains NUL"))?;
        let fd = unsafe { libc::openat(self.root.as_raw_fd(), relative.as_ptr(), flags) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    fn verify_proof_fd(
        &self,
        file: &File,
        root_dev: u64,
        root_mount_id: u64,
        label: &str,
        require_regular: bool,
    ) -> Result<()> {
        let metadata = file.metadata().map_err(|error| {
            proc_view_error(&self.canonical, format!("cannot inspect {label}: {error}"))
        })?;
        if require_regular && !metadata.is_file() {
            return Err(proc_view_error(
                &self.canonical,
                format!("{label} is not a regular file"),
            ));
        }
        if !require_regular && !metadata.file_type().is_symlink() {
            return Err(proc_view_error(
                &self.canonical,
                format!("{label} is not a namespace symlink"),
            ));
        }
        if metadata.dev() != root_dev {
            return Err(proc_view_error(
                &self.canonical,
                format!("{label} is on a different filesystem from the held proc root"),
            ));
        }
        let fs_type = fstatfs_type(file).map_err(|error| {
            proc_view_error(
                &self.canonical,
                format!("cannot inspect {label} filesystem: {error}"),
            )
        })?;
        if !self.synthetic && fs_type != libc::PROC_SUPER_MAGIC {
            return Err(proc_view_error(
                &self.canonical,
                format!("{label} is not on procfs"),
            ));
        }
        let mount_id = statx_mount_id(file).map_err(|error| {
            proc_view_error(
                &self.canonical,
                format!("cannot prove {label} mount identity: {error}"),
            )
        })?;
        if mount_id != root_mount_id {
            return Err(proc_view_error(
                &self.canonical,
                format!("{label} is not on the held proc-root mount"),
            ));
        }
        Ok(())
    }

    fn open_proof_file(&self, relative: &str, root_dev: u64, root_mount_id: u64) -> Result<File> {
        let file = self
            .open_at(
                relative,
                libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK,
            )
            .map_err(|error| {
                proc_view_error(
                    &self.canonical,
                    format!("cannot open {relative} relative to held root: {error}"),
                )
            })?;
        self.verify_proof_fd(&file, root_dev, root_mount_id, relative, true)?;
        Ok(file)
    }

    fn read_namespace_link(
        &self,
        relative: &str,
        root_dev: u64,
        root_mount_id: u64,
    ) -> Result<Option<u64>> {
        let file = self
            .open_at(relative, libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .map_err(|error| {
                proc_view_error(
                    &self.canonical,
                    format!("cannot open {relative} relative to held root: {error}"),
                )
            })?;
        self.verify_proof_fd(&file, root_dev, root_mount_id, relative, false)?;
        let empty = b"\0";
        let mut bytes = [0u8; 64];
        let count = unsafe {
            libc::readlinkat(
                file.as_raw_fd(),
                empty.as_ptr().cast(),
                bytes.as_mut_ptr().cast(),
                bytes.len(),
            )
        };
        if count < 0 {
            let error = io::Error::last_os_error();
            // CAD-1845: newer kernels (7.0.0-1014) answer the same read with
            // EACCES instead of an empty target. Only a link owned by another
            // uid gets the CAD-1196 meaning; any other failure refuses.
            let foreign = file
                .metadata()
                .is_ok_and(|metadata| metadata.uid() != unsafe { libc::geteuid() });
            if error.raw_os_error() == Some(libc::EACCES) && foreign {
                return Ok(None);
            }
            return Err(proc_view_error(
                &self.canonical,
                format!("cannot read {relative} through held link: {error}"),
            ));
        }
        let count = count as usize;
        if count == bytes.len() {
            return Err(proc_view_error(
                &self.canonical,
                format!("{relative} namespace link was truncated"),
            ));
        }
        // CAD-1196: the kernel answers a read of another uid's ns link with
        // a successful, zero-length target instead of EACCES. That is
        // "not readable by us", reported as None; any other target must be
        // canonical.
        if count == 0 {
            return Ok(None);
        }
        let target = std::str::from_utf8(&bytes[..count]).map_err(|error| {
            proc_view_error(
                &self.canonical,
                format!("{relative} has a non-UTF-8 target: {error}"),
            )
        })?;
        parse_pid_namespace_link(target).map(Some).ok_or_else(|| {
            proc_view_error(
                &self.canonical,
                format!("{relative} is not a canonical pid namespace link"),
            )
        })
    }

    fn read_status_at(&self, relative: &str) -> io::Result<String> {
        let flags = libc::O_RDONLY
            | libc::O_CLOEXEC
            | libc::O_NOFOLLOW
            | if self.synthetic { 0 } else { libc::O_NONBLOCK };
        let mut file = self.open_at(relative, flags)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() && !(self.synthetic && metadata.file_type().is_fifo()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "process status is not a regular file",
            ));
        }
        let mut status = String::new();
        file.read_to_string(&mut status)?;
        Ok(status)
    }

    fn read_pid_status(&self, pid: u32) -> io::Result<String> {
        self.read_status_at(&format!("{pid}/status"))
    }

    fn validate_completeness(&self) -> Result<()> {
        let root_meta = self.root.metadata().map_err(|error| {
            proc_view_error(
                &self.canonical,
                format!("cannot inspect held root: {error}"),
            )
        })?;
        if !root_meta.is_dir() {
            return Err(proc_view_error(
                &self.canonical,
                "held root is not a directory",
            ));
        }
        let root_fs_type = fstatfs_type(&self.root).map_err(|error| {
            proc_view_error(
                &self.canonical,
                format!("cannot inspect held root filesystem: {error}"),
            )
        })?;
        if (!self.synthetic && root_fs_type != libc::PROC_SUPER_MAGIC)
            || (self.synthetic && root_fs_type == libc::PROC_SUPER_MAGIC)
        {
            return Err(proc_view_error(
                &self.canonical,
                "held root filesystem does not match its proc-view adapter",
            ));
        }
        let root_mount_id = statx_mount_id(&self.root).map_err(|error| {
            proc_view_error(
                &self.canonical,
                format!("cannot prove held root mount identity: {error}"),
            )
        })?;

        let mut mountinfo =
            self.open_proof_file("self/mountinfo", root_meta.dev(), root_mount_id)?;
        let mut mountinfo_text = String::new();
        mountinfo
            .read_to_string(&mut mountinfo_text)
            .map_err(|error| {
                proc_view_error(
                    &self.canonical,
                    format!("cannot read held self/mountinfo: {error}"),
                )
            })?;
        validate_proc_mountinfo(
            &mountinfo_text,
            &self.canonical,
            root_mount_id,
            self.synthetic,
        )?;

        let release = if self.synthetic {
            let mut file =
                self.open_proof_file("self/kernel-release", root_meta.dev(), root_mount_id)?;
            let mut release = String::new();
            file.read_to_string(&mut release).map_err(|error| {
                proc_view_error(
                    &self.canonical,
                    format!("cannot read synthetic kernel release: {error}"),
                )
            })?;
            release
        } else {
            kernel_release().map_err(|error| {
                proc_view_error(
                    &self.canonical,
                    format!("cannot read kernel release: {error}"),
                )
            })?
        };
        if !supported_kernel_abi(&release) {
            return Err(proc_view_error(
                &self.canonical,
                format!("kernel release {release:?} has no frozen proc proof ABI"),
            ));
        }

        let mut stat = self.open_proof_file("2/stat", root_meta.dev(), root_mount_id)?;
        let mut stat_text = String::new();
        stat.read_to_string(&mut stat_text).map_err(|error| {
            proc_view_error(
                &self.canonical,
                format!("cannot read held PID 2 stat: {error}"),
            )
        })?;
        if !is_initial_kernel_task(&stat_text) {
            return Err(proc_view_error(
                &self.canonical,
                "PID 2 is not a live kthreadd kernel thread in the initial PID namespace",
            ));
        }
        // The caller's own link is always readable; an unreadable one proves
        // nothing and refuses.
        let Some(self_pid_ns) =
            self.read_namespace_link("self/ns/pid", root_meta.dev(), root_mount_id)?
        else {
            return Err(proc_view_error(
                &self.canonical,
                "caller's own pid namespace link is empty",
            ));
        };
        match self.read_namespace_link("2/ns/pid", root_meta.dev(), root_mount_id)? {
            Some(task_pid_ns) if task_pid_ns != self_pid_ns => {
                return Err(proc_view_error(
                    &self.canonical,
                    "caller and PID 2 are not in the same PID namespace",
                ));
            }
            Some(_) => {}
            None => {
                // CAD-1196: PID 2 is root's, so an unprivileged caller reads
                // its ns link as empty (or EACCES, CAD-1845). The same fact is proven without it:
                // PID 2 was just verified as kthreadd (PF_KTHREAD, parent 0),
                // which exists only in the initial PID namespace, so this
                // proc is the initial namespace's view; and the caller's own
                // NSpid has a single entry, so the caller is in that same
                // namespace as seen by this proc.
                let status = self.read_status_at("self/status").map_err(|error| {
                    proc_view_error(
                        &self.canonical,
                        format!("cannot read held self/status: {error}"),
                    )
                })?;
                if !status_has_single_nspid(&status) {
                    return Err(proc_view_error(
                        &self.canonical,
                        "caller is not provably in the PID namespace of this proc view",
                    ));
                }
            }
        }
        Ok(())
    }
}

#[cfg(target_os = "linux")]
fn fstatfs_type(file: &File) -> io::Result<libc::c_long> {
    let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstatfs(file.as_raw_fd(), &mut stat) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(stat.f_type as libc::c_long)
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn statx_mount_id(file: &File) -> io::Result<u64> {
    let empty = b"\0";
    let mut stat: libc::statx = unsafe { std::mem::zeroed() };
    let result = unsafe {
        libc::statx(
            file.as_raw_fd(),
            empty.as_ptr().cast(),
            libc::AT_EMPTY_PATH | libc::AT_SYMLINK_NOFOLLOW,
            libc::STATX_MNT_ID,
            &mut stat,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    if stat.stx_mask & libc::STATX_MNT_ID == 0 || stat.stx_mnt_id == 0 {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "statx did not report STATX_MNT_ID",
        ));
    }
    Ok(stat.stx_mnt_id)
}

#[cfg(all(target_os = "linux", not(target_env = "gnu")))]
fn statx_mount_id(_file: &File) -> io::Result<u64> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "this libc target has no verified statx mount-id ABI",
    ))
}

#[cfg(target_os = "linux")]
fn parse_pid_namespace_link(target: &str) -> Option<u64> {
    let value = target.strip_prefix("pid:[")?.strip_suffix(']')?;
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let inode = value.parse::<u64>().ok()?;
    (inode != 0 && inode.to_string() == value).then_some(inode)
}

/// `NSpid:` lists the task's pid in each namespace from the proc mount's
/// down to its own; exactly one entry means the task lives in the proc
/// mount's namespace.
#[cfg(target_os = "linux")]
fn status_has_single_nspid(status: &str) -> bool {
    let mut lines = status
        .lines()
        .filter_map(|line| line.strip_prefix("NSpid:"));
    let (Some(line), None) = (lines.next(), lines.next()) else {
        return false;
    };
    let mut ids = line.split_whitespace();
    matches!(
        (ids.next().map(str::parse::<u32>), ids.next()),
        (Some(Ok(pid)), None) if pid != 0
    )
}

/// What the kernel's ptrace read-access rule is judged against.
#[cfg(target_os = "linux")]
#[derive(Clone, Copy)]
struct Caller {
    euid: u32,
    egid: u32,
    /// `CapPrm` of the caller; `None` when it could not be read, which
    /// disables the capability explanation (fail closed).
    cap_prm: Option<u64>,
}

#[cfg(target_os = "linux")]
fn status_cap_prm(status: &str) -> Option<u64> {
    let value = status
        .lines()
        .find_map(|line| line.strip_prefix("CapPrm:"))?;
    u64::from_str_radix(value.trim(), 16).ok()
}

/// CAD-1196: does the kernel's own access rule for `/proc/<pid>/{cwd,fd}`
/// (ptrace read access) explain a PermissionDenied for this caller? The
/// kernel itself marks the cases: it gives the process's `cwd` entry to the
/// process's uid/gid only while the process is dumpable and (for a foreign
/// uid) otherwise to root, so an entry owned by neither the caller's
/// effective uid nor gid is a foreign or non-dumpable process. The other
/// case is a target whose permitted capabilities exceed the caller's.
/// Status credentials are never trusted for this: a PermissionDenied that
/// neither the entry's ownership nor the capabilities explain is an
/// unexplained gap and still refuses.
#[cfg(target_os = "linux")]
fn policy_denies_read(status: &str, caller: Caller, cwd_entry_owner: Option<(u32, u32)>) -> bool {
    let extra_caps = match (status_cap_prm(status), caller.cap_prm) {
        (Some(target), Some(own)) => target & !own != 0,
        _ => false,
    };
    extra_caps || cwd_entry_owner.is_some_and(|(uid, gid)| uid != caller.euid || gid != caller.egid)
}

#[cfg(target_os = "linux")]
fn kernel_release() -> io::Result<String> {
    let mut uts: libc::utsname = unsafe { std::mem::zeroed() };
    if unsafe { libc::uname(&mut uts) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let bytes: Vec<u8> = uts
        .release
        .iter()
        .take_while(|byte| **byte != 0)
        .map(|byte| *byte as u8)
        .collect();
    if bytes.len() == uts.release.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "uname release is not NUL-terminated",
        ));
    }
    String::from_utf8(bytes).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

#[cfg(target_os = "linux")]
fn supported_kernel_abi(release: &str) -> bool {
    const SUPPORTED: &[(u32, u32)] = &[
        (5, 8),
        (5, 10),
        (5, 15),
        (6, 1),
        (6, 6),
        (6, 8),
        (6, 11),
        (6, 12),
        (6, 14),
        (6, 17),
        // CAD-1967: the InstaBox CI kernel. It presents the same proof
        // inputs as the supported 7.0 runner — kthreadd as PID 2 with
        // PF_KTHREAD and PPid 0, a single full procfs root, the same
        // ns/pid answers — and its one extra submount sits under
        // /proc/sys, which `sensitive_proc_submount` does not treat as
        // sensitive. The host acceptance tests prove the enumeration
        // itself on that kernel; an unexplained denial still fails closed.
        (6, 18),
        (7, 0),
    ];
    let Some((major, rest)) = release.split_once('.') else {
        return false;
    };
    if major.is_empty() || !major.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    let Some(minor) = rest.split(['.', '-']).next() else {
        return false;
    };
    if minor.is_empty() || !minor.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    let (Ok(major), Ok(minor)) = (major.parse::<u32>(), minor.parse::<u32>()) else {
        return false;
    };
    SUPPORTED.contains(&(major, minor))
}

#[cfg(target_os = "linux")]
fn is_initial_kernel_task(stat: &str) -> bool {
    const PF_KTHREAD: u64 = 0x0020_0000;
    let Some((pid, record)) = stat.split_once(' ') else {
        return false;
    };
    if pid.parse::<u32>().ok() != Some(2) {
        return false;
    }
    let Some(record) = record.strip_prefix('(') else {
        return false;
    };
    let Some(close) = record.rfind(')') else {
        return false;
    };
    if &record[..close] != "kthreadd" {
        return false;
    }
    let fields: Vec<&str> = record[close + 1..].split_whitespace().collect();
    if fields.len() < 7 || fields[0].len() != 1 {
        return false;
    }
    let Some(state) = fields[0].chars().next() else {
        return false;
    };
    let Some(parent) = fields[1].parse::<u32>().ok() else {
        return false;
    };
    let Some(flags) = fields[6].parse::<u64>().ok() else {
        return false;
    };
    valid_proc_state(state)
        && !matches!(state, 'Z' | 'X' | 'x')
        && parent == 0
        && flags & PF_KTHREAD != 0
}

#[cfg(target_os = "linux")]
fn sensitive_proc_submount(root: &Path, mount_point: &Path) -> bool {
    let Ok(relative) = mount_point.strip_prefix(root) else {
        return false;
    };
    let Some(std::path::Component::Normal(first)) = relative.components().next() else {
        return false;
    };
    let first = first.as_bytes();
    first == b"self"
        || first == b"thread-self"
        || (!first.is_empty() && first.iter().all(u8::is_ascii_digit))
}

#[cfg(target_os = "linux")]
fn validate_proc_mountinfo(
    text: &str,
    canonical: &Path,
    root_mount_id: u64,
    synthetic: bool,
) -> Result<()> {
    let mut exact_mounts = 0usize;
    let mut mount_ids = std::collections::HashSet::new();
    for (line_index, line) in text.lines().enumerate() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let separators: Vec<usize> = fields
            .iter()
            .enumerate()
            .filter_map(|(index, field)| (*field == "-").then_some(index))
            .collect();
        let Some(mount_id) = fields.first().and_then(|field| field.parse::<u64>().ok()) else {
            return Err(proc_view_error(
                canonical,
                format!("malformed mountinfo line {}", line_index + 1),
            ));
        };
        if line.is_empty()
            || fields.len() < 10
            || mount_id == 0
            || fields[1].parse::<u64>().is_err()
            || separators.len() != 1
        {
            return Err(proc_view_error(
                canonical,
                format!("malformed mountinfo line {}", line_index + 1),
            ));
        }
        let separator = separators[0];
        let device = fields[2].split_once(':');
        if separator < 6
            || fields.len() < separator + 4
            || !mount_ids.insert(mount_id)
            || device.is_none_or(|(major, minor)| {
                major.parse::<u64>().is_err() || minor.parse::<u64>().is_err()
            })
        {
            return Err(proc_view_error(
                canonical,
                format!("malformed mountinfo line {}", line_index + 1),
            ));
        }
        let mount_root = decode_mountinfo_path(fields[3]);
        let mount_point = decode_mountinfo_path(fields[4]);
        let (Some(mount_root), Some(mount_point)) = (mount_root, mount_point) else {
            return Err(proc_view_error(
                canonical,
                format!(
                    "malformed escaped path on mountinfo line {}",
                    line_index + 1
                ),
            ));
        };
        if !mount_point.is_absolute()
            || (fields[separator + 1] != "nsfs" && !mount_root.is_absolute())
        {
            return Err(proc_view_error(
                canonical,
                format!("non-absolute mount path on line {}", line_index + 1),
            ));
        }
        if fields[5].split(',').any(str::is_empty)
            || fields[separator + 3].split(',').any(str::is_empty)
        {
            return Err(proc_view_error(
                canonical,
                format!("malformed mount options on line {}", line_index + 1),
            ));
        }
        if sensitive_proc_submount(canonical, &mount_point) {
            return Err(proc_view_error(
                canonical,
                format!(
                    "sensitive proc subtree is overmounted at {}",
                    mount_point.display()
                ),
            ));
        }
        if mount_point == canonical {
            exact_mounts += 1;
            if exact_mounts != 1 {
                return Err(proc_view_error(canonical, "ambiguous stacked proc mounts"));
            }
            let expected_mount_id = if synthetic { 1 } else { root_mount_id };
            if mount_id != expected_mount_id {
                return Err(proc_view_error(
                    canonical,
                    "mountinfo root ID does not match the held root mount",
                ));
            }
            if mount_root != Path::new("/") || fields[separator + 1] != "proc" {
                return Err(proc_view_error(
                    canonical,
                    "mount is not a full procfs root",
                ));
            }
            validate_proc_mount_options(&[fields[5], fields[separator + 3]])
                .map_err(|reason| proc_view_error(canonical, reason))?;
        }
    }
    if exact_mounts != 1 {
        return Err(proc_view_error(
            canonical,
            "mountinfo has no unique full procfs root",
        ));
    }
    Ok(())
}

/// Cwd and open-descriptor holders under a managed checkout. Any process that
/// cannot be fully inspected leaves the scan incomplete; cleanup never infers
/// that a process lacks an inherited or transferred checkout descriptor from
/// its UID or path permissions.
pub(crate) struct ProcessUse {
    pub cwd: Vec<u32>,
    pub fd: Vec<u32>,
    pub enumeration_error: Option<String>,
    /// CAD-1209: pids whose cwd/fd the kernel hid from this caller and
    /// `policy_denies_read` explained (root's, capability-bearing,
    /// non-dumpable). They are not inspected; finish reports them.
    pub excused: Vec<u32>,
}

pub(crate) type ProcessUseProbe = dyn Fn(&Path) -> Result<ProcessUse>;

#[cfg(target_os = "linux")]
fn proc_id_values(status: &str, key: &str, count: usize) -> Option<Vec<u32>> {
    let line = status.lines().find_map(|line| line.strip_prefix(key))?;
    line.split_whitespace()
        .take(count)
        .map(str::parse::<u32>)
        .collect::<std::result::Result<Vec<_>, _>>()
        .ok()
        .filter(|values| values.len() == count)
}

#[cfg(target_os = "linux")]
fn valid_proc_state(state: char) -> bool {
    matches!(
        state,
        'R' | 'S' | 'D' | 'T' | 't' | 'Z' | 'X' | 'x' | 'K' | 'W' | 'P' | 'I'
    )
}

#[cfg(target_os = "linux")]
fn proc_status_state(status: &str) -> Option<char> {
    let mut states = status
        .lines()
        .filter_map(|line| line.strip_prefix("State:"));
    let state = states.next()?.split_whitespace().next()?;
    if states.next().is_some() || state.len() != 1 {
        return None;
    }
    let state = state.chars().next()?;
    valid_proc_state(state).then_some(state)
}

// Validate the PID entry only; credentials are never used to skip cwd or FD
// inspection or to infer that a process could not hold a checkout descriptor.
#[cfg(target_os = "linux")]
fn proc_status_is_complete(status: &str) -> bool {
    let credentials_complete = proc_id_values(status, "Uid:", 4).is_some()
        && proc_id_values(status, "Gid:", 4).is_some()
        && status
            .lines()
            .find_map(|line| line.strip_prefix("Groups:"))
            .is_some_and(|groups| {
                groups
                    .split_whitespace()
                    .all(|group| group.parse::<u32>().is_ok())
            });
    let capabilities_complete = status
        .lines()
        .find_map(|line| line.strip_prefix("CapEff:"))
        .is_some_and(|caps| u64::from_str_radix(caps.trim(), 16).is_ok());
    proc_status_state(status).is_some() && credentials_complete && capabilities_complete
}

#[cfg(target_os = "linux")]
fn inspect_process_status(status: &str) -> io::Result<bool> {
    let state = proc_status_state(status).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "process status has a missing or malformed State field",
        )
    })?;
    if !proc_status_is_complete(status) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "process status has incomplete credentials",
        ));
    }
    Ok(!matches!(state, 'Z' | 'X'))
}

#[cfg(target_os = "linux")]
struct PendingPidError {
    kind: io::ErrorKind,
    only_gone_resolves: bool,
    operation: &'static str,
    message: String,
}

#[cfg(target_os = "linux")]
fn remember_pid_error(
    pending: &mut Option<PendingPidError>,
    what: &'static str,
    error: io::Error,
    allow_zombie_resolution: bool,
) {
    let kind = error.kind();
    let only_gone_resolves = kind == io::ErrorKind::NotFound && !allow_zombie_resolution;
    match pending {
        None => {
            *pending = Some(PendingPidError {
                kind,
                only_gone_resolves,
                operation: what,
                message: error.to_string(),
            });
        }
        Some(previous)
            if previous.kind == io::ErrorKind::NotFound && kind != io::ErrorKind::NotFound =>
        {
            *previous = PendingPidError {
                kind,
                only_gone_resolves,
                operation: what,
                message: error.to_string(),
            };
        }
        Some(previous)
            if previous.kind == io::ErrorKind::NotFound && kind == io::ErrorKind::NotFound =>
        {
            previous.only_gone_resolves |= only_gone_resolves;
        }
        Some(_) => {}
    }
}

#[cfg(target_os = "linux")]
fn process_status(view: &ProcView, pid: u32) -> io::Result<bool> {
    inspect_process_status(&view.read_pid_status(pid)?)
}

#[cfg(target_os = "linux")]
fn pid_is_gone_or_dead(view: &ProcView, pid: u32, allow_zombie: bool) -> io::Result<bool> {
    let proc_dir = view.scan_root.join(pid.to_string());
    match std::fs::symlink_metadata(&proc_dir) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(true),
        Err(error) => return Err(error),
        Ok(metadata) if !metadata.is_dir() => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "numeric proc entry is not a directory",
            ));
        }
        Ok(_) => {}
    }
    if allow_zombie {
        Ok(!process_status(view, pid)?)
    } else {
        Ok(false)
    }
}

#[cfg(target_os = "linux")]
fn aggregate_pid_error(use_: &mut ProcessUse, pid: u32, pending: Option<PendingPidError>) {
    if let Some(error) = pending {
        use_.enumeration_error.get_or_insert_with(|| {
            format!(
                "cannot {} for process {pid} during cleanup: {}",
                error.operation, error.message
            )
        });
    }
}

#[cfg(target_os = "linux")]
fn finish_pid_scan(
    view: &ProcView,
    pid: u32,
    use_: &mut ProcessUse,
    mut pending: Option<PendingPidError>,
) {
    if let Some(allow_zombie) = pending
        .as_ref()
        .filter(|error| error.kind == io::ErrorKind::NotFound)
        .map(|error| !error.only_gone_resolves)
    {
        match pid_is_gone_or_dead(view, pid, allow_zombie) {
            Ok(true) => {
                pending = None;
                use_.cwd.retain(|holder| *holder != pid);
                use_.fd.retain(|holder| *holder != pid);
            }
            Ok(false) => {}
            Err(error) => {
                remember_pid_error(&mut pending, "confirm process disappearance", error, true)
            }
        }
    }
    aggregate_pid_error(use_, pid, pending);
}

/// Reads of one fd entry before a link that keeps vanishing under a still
/// present entry counts as an incomplete scan.
#[cfg(target_os = "linux")]
const FD_REREAD_ATTEMPTS: u32 = 8;

#[cfg(target_os = "linux")]
fn inspect_pid(view: &ProcView, dir: &Path, pid: u32, use_: &mut ProcessUse, caller: Caller) {
    let proc_dir = view.scan_root.join(pid.to_string());
    let mut pending = None;
    match std::fs::symlink_metadata(&proc_dir) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return,
        Err(error) => {
            remember_pid_error(&mut pending, "inspect proc entry", error, false);
            finish_pid_scan(view, pid, use_, pending);
            return;
        }
        Ok(metadata) if !metadata.is_dir() => {
            remember_pid_error(
                &mut pending,
                "inspect proc entry",
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "numeric proc entry is not a directory",
                ),
                false,
            );
            finish_pid_scan(view, pid, use_, pending);
            return;
        }
        Ok(_) => {}
    }
    let status = view
        .read_pid_status(pid)
        .and_then(|status| inspect_process_status(&status).map(|live| (status, live)));
    let status = match status {
        Ok((_, false)) => return,
        Ok((status, true)) => status,
        Err(error) => {
            remember_pid_error(&mut pending, "establish process identity", error, true);
            finish_pid_scan(view, pid, use_, pending);
            return;
        }
    };
    // CAD-1196: on a normal host many processes are unreadable to an
    // unprivileged caller by kernel policy (root's, capability-bearing,
    // non-dumpable). PermissionDenied that `policy_denies_read` explains is
    // that policy, not an incomplete view; any other error, and a
    // PermissionDenied nothing explains, still leaves the scan incomplete.
    let cwd_entry_owner = std::fs::symlink_metadata(proc_dir.join("cwd"))
        .ok()
        .map(|metadata| (metadata.uid(), metadata.gid()));
    let excused = std::cell::Cell::new(false);
    let unreadable_by_policy = |error: &io::Error| {
        let denied = error.kind() == io::ErrorKind::PermissionDenied
            && policy_denies_read(&status, caller, cwd_entry_owner);
        if denied {
            excused.set(true);
        }
        denied
    };

    match std::fs::read_link(proc_dir.join("cwd")) {
        Ok(cwd) if cwd.starts_with(dir) => use_.cwd.push(pid),
        Ok(_) => {}
        Err(error) if unreadable_by_policy(&error) => {}
        Err(error) => remember_pid_error(&mut pending, "inspect cwd", error, true),
    }
    match std::fs::read_dir(proc_dir.join("fd")) {
        Ok(fds) => {
            let mut holds = false;
            for fd in fds {
                let fd = match fd {
                    Ok(fd) => fd,
                    Err(error) => {
                        remember_pid_error(
                            &mut pending,
                            "enumerate file descriptors",
                            error,
                            false,
                        );
                        continue;
                    }
                };
                let path = fd.path();
                // CAD-1558: a live process that closes fd N and reopens the
                // same number between our readlink and lstat leaves an entry
                // whose link we could not read. The new descriptor may point
                // into the checkout, so it is read again (bounded), never
                // skipped; still unreadable after the bound refuses.
                let mut attempts = 0;
                loop {
                    attempts += 1;
                    match std::fs::read_link(&path) {
                        Ok(target) if target.starts_with(dir) => holds = true,
                        Ok(_) => {}
                        Err(error) if error.kind() == io::ErrorKind::NotFound => {
                            match std::fs::symlink_metadata(&path) {
                                Err(metadata_error)
                                    if metadata_error.kind() == io::ErrorKind::NotFound => {}
                                Err(metadata_error) => remember_pid_error(
                                    &mut pending,
                                    "confirm file descriptor entry disappearance",
                                    metadata_error,
                                    false,
                                ),
                                Ok(_) if attempts < FD_REREAD_ATTEMPTS => continue,
                                Ok(_) => remember_pid_error(
                                    &mut pending,
                                    "inspect file descriptor",
                                    error,
                                    false,
                                ),
                            }
                        }
                        Err(error) if unreadable_by_policy(&error) => {}
                        Err(error) => remember_pid_error(
                            &mut pending,
                            "inspect file descriptor",
                            error,
                            false,
                        ),
                    }
                    break;
                }
                if holds {
                    break;
                }
            }
            if holds {
                use_.fd.push(pid);
            }
        }
        Err(error) if unreadable_by_policy(&error) => {}
        Err(error) => remember_pid_error(&mut pending, "enumerate file descriptors", error, true),
    }
    if excused.get() && !use_.excused.contains(&pid) {
        use_.excused.push(pid);
    }
    finish_pid_scan(view, pid, use_, pending);
}

pub(crate) fn process_use_under(dir: &Path) -> Result<ProcessUse> {
    process_use_under_from_proc_root(dir, &process_proc_root())
}

#[cfg(target_os = "linux")]
pub(crate) fn process_use_under_from_proc_root(dir: &Path, proc_root: &Path) -> Result<ProcessUse> {
    let dir = dir.canonicalize().map_err(|error| {
        Error::rejected(format!(
            "cannot resolve live-use path {}: {error}",
            dir.display()
        ))
    })?;
    let view = ProcView::open(proc_root)?;
    let completeness_error = view
        .validate_completeness()
        .err()
        .map(|error| error.to_string());
    let complete = completeness_error.is_none();
    let mut use_ = ProcessUse {
        cwd: Vec::new(),
        fd: Vec::new(),
        enumeration_error: completeness_error,
        excused: Vec::new(),
    };
    let procs = match std::fs::read_dir(&view.scan_root) {
        Ok(procs) => procs,
        Err(error) => {
            use_.enumeration_error
                .get_or_insert_with(|| format!("cannot enumerate anchored proc entries: {error}"));
            return Ok(use_);
        }
    };
    let me = std::process::id();
    let caller = Caller {
        euid: unsafe { libc::geteuid() },
        egid: unsafe { libc::getegid() },
        cap_prm: view
            .read_status_at("self/status")
            .ok()
            .and_then(|status| status_cap_prm(&status)),
    };
    for entry in procs {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                use_.enumeration_error.get_or_insert_with(|| {
                    format!("cannot enumerate anchored proc entries: {error}")
                });
                continue;
            }
        };
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        if complete && pid == me {
            continue;
        }
        inspect_pid(&view, &dir, pid, &mut use_, caller);
    }
    Ok(use_)
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn process_use_under_from_proc_root(dir: &Path, proc_root: &Path) -> Result<ProcessUse> {
    let _ = (dir, proc_root);
    Err(Error::rejected(
        "process cwd/open-fd enumeration requires a verified Linux proc view; refusing deletion",
    ))
}

#[cfg(test)]
mod tests;
