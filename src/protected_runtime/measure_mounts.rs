//! Finite, held-descriptor mount provenance for the owned guest.
//!
//! The two owner socket file binds were the original finite surface. Docker
//! also generates exactly three engine metadata files (`resolv.conf`,
//! `hostname`, `hosts`) and mounts them read-only over `/etc`. This module
//! proves *that exact finite layout* and refuses everything else:
//!
//! - the engine data root comes only from the fixed `GET /v1.44/info` request
//!   on the already configured root-controlled Docker socket; the returned
//!   `DockerRootDir` is opened component by component with `O_PATH |
//!   O_NOFOLLOW` and every ancestor must be uid 0 and not group/other writable;
//! - the three metadata paths are derived only as
//!   `<engine>/containers/<exact 64-hex inspected id>/<name>` and the inspect
//!   `ResolvConfPath`/`HostnamePath`/`HostsPath` fields must equal them;
//! - source and guest descriptors are held and compared by device, inode and
//!   type; the guest descriptor's `STATX_MNT_ID` must name the exact parsed
//!   `/proc/<pid>/mountinfo` record, and the source mount id must differ from
//!   the guest bind mount id;
//! - the full guest mount table is matched against the fixed Linux Docker 29
//!   baseline plus the four producer tmpfs mounts and the five proven files;
//!   known optional masked/read-only records are accepted only with their exact
//!   filesystem, root and read-only flag, and every other record refuses;
//! - each accepted metadata byte sequence is bounded and validated against the
//!   exact Docker 29 generated output (no unknown comment or directive);
//! - the inspected id/labels/pid and the live starttime are re-checked before
//!   and after the proof, and every call re-opens and re-reads fresh metadata.
//!
//! Errors are fixed category strings and never contain host bytes.

use std::collections::BTreeMap;
use std::ffi::CString;
use std::fs::File;
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd};
use std::path::{Component, Path, PathBuf};

use crate::error::{Error, Result};

use super::docker::DockerClient;
use super::peer;
use super::profile::LocalProfile;
use super::spec::{self, RuntimeSpec};

/// One guest mount table is expected to stay far below this.
const MAX_MOUNTINFO_BYTES: usize = 1024 * 1024;
/// Docker's generated metadata files are tiny; anything larger is not the
/// producer output this profile approved.
const MAX_METADATA_BYTES: usize = 16 * 1024;
const MAX_ENGINE_ROOT_BYTES: usize = 4096;

fn proof_unavailable() -> Error {
    Error::unknown("mount proof unavailable")
}

fn provenance_refused() -> Error {
    Error::rejected("mount provenance refused")
}

fn identity_refused() -> Error {
    Error::rejected("mount identity refused")
}

fn writable_refused() -> Error {
    Error::rejected("mount writable refused")
}

fn unexpected_refused() -> Error {
    Error::rejected("mount unexpected refused")
}

fn stacked_refused() -> Error {
    Error::rejected("mount stacked refused")
}

fn content_refused() -> Error {
    Error::rejected("mount content refused")
}

fn stale_refused() -> Error {
    Error::rejected("mount stale process refused")
}

/// The trusted engine data root: a held directory descriptor opened from the
/// fixed v1.44 `/info` fact, never from caller input.
pub(crate) struct EngineMountRoot {
    dir: File,
    path: PathBuf,
}

impl EngineMountRoot {
    pub(crate) fn from_docker(docker: &DockerClient) -> Result<Self> {
        let info = docker.info().map_err(|_| proof_unavailable())?;
        let raw = info
            .get("DockerRootDir")
            .and_then(|value| value.as_str())
            .ok_or_else(proof_unavailable)?;
        if raw.is_empty()
            || raw.len() > MAX_ENGINE_ROOT_BYTES
            || raw.as_bytes().contains(&0)
            || !raw.starts_with('/')
        {
            return Err(proof_unavailable());
        }
        let path = PathBuf::from(raw);
        let mut components = path.components();
        if components.next() != Some(Component::RootDir) {
            return Err(proof_unavailable());
        }
        let mut held = open_root()?;
        let root_stat = fstat(&held)?;
        if root_stat.st_uid != 0 || root_stat.st_mode & 0o022 != 0 {
            return Err(proof_unavailable());
        }
        for component in components {
            let Component::Normal(name) = component else {
                // `.`/`..`/a second root component is not a canonical data
                // root; refuse rather than normalize it.
                return Err(proof_unavailable());
            };
            let name = name.to_str().ok_or_else(proof_unavailable)?;
            if name.is_empty() {
                return Err(proof_unavailable());
            }
            held = open_dir_at(&held, name)?;
            let stat = fstat(&held)?;
            if stat.st_uid != 0 || stat.st_mode & 0o022 != 0 {
                return Err(proof_unavailable());
            }
        }
        Ok(Self { dir: held, path })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// The absolute engine path for one container resource. Callers only ever
    /// pass a validated 64-hex id and one of the three fixed file names.
    fn resource_path(&self, id: &str, name: &str) -> PathBuf {
        self.path.join("containers").join(id).join(name)
    }

    /// Open `<engine>/containers/<id>/<name>` through the held root, holding
    /// every ancestor descriptor and refusing symlinks/non-root ancestors.
    fn open_metadata(&self, id: &str, name: &str) -> Result<HeldFile> {
        let containers = open_dir_at(&self.dir, "containers")?;
        require_root_dir(&containers)?;
        let container = open_dir_at(&containers, id)?;
        require_root_dir(&container)?;
        let file = open_file_at(
            &container,
            name,
            libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )?;
        let stat = fstat(&file)?;
        require_metadata_stat(&stat)?;
        Ok(HeldFile {
            path: self.resource_path(id, name),
            file,
            stat,
        })
    }
}

struct HeldFile {
    path: PathBuf,
    file: File,
    stat: libc::stat,
}

fn open_root() -> Result<File> {
    let path = CString::new("/").map_err(|_| proof_unavailable())?;
    let fd = unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if fd < 0 {
        return Err(proof_unavailable());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn open_dir_at(parent: &File, name: &str) -> Result<File> {
    let name = CString::new(name).map_err(|_| proof_unavailable())?;
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if fd < 0 {
        return Err(proof_unavailable());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn open_file_at(parent: &File, name: &str, flags: i32) -> Result<File> {
    let name = CString::new(name).map_err(|_| proof_unavailable())?;
    let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags) };
    if fd < 0 {
        return Err(proof_unavailable());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn fstat(file: &File) -> Result<libc::stat> {
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(file.as_raw_fd(), &mut stat) } != 0 {
        return Err(proof_unavailable());
    }
    Ok(stat)
}

fn require_root_dir(file: &File) -> Result<()> {
    let stat = fstat(file)?;
    if stat.st_uid != 0 || stat.st_mode & 0o022 != 0 {
        return Err(proof_unavailable());
    }
    Ok(())
}

fn require_metadata_stat(stat: &libc::stat) -> Result<()> {
    if stat.st_mode & libc::S_IFMT != libc::S_IFREG {
        return Err(identity_refused());
    }
    if stat.st_uid != 0 || stat.st_gid != 0 || stat.st_mode & 0o7777 != 0o644 {
        return Err(identity_refused());
    }
    if stat.st_size <= 0 || stat.st_size as usize > MAX_METADATA_BYTES {
        return Err(content_refused());
    }
    Ok(())
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn statx_mount_id(file: &File) -> Result<u64> {
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
    if result != 0 || stat.stx_mask & libc::STATX_MNT_ID == 0 || stat.stx_mnt_id == 0 {
        return Err(proof_unavailable());
    }
    Ok(stat.stx_mnt_id)
}

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
fn statx_mount_id(_file: &File) -> Result<u64> {
    Err(proof_unavailable())
}

fn filesystem_name(file: &File) -> Result<&'static str> {
    let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstatfs(file.as_raw_fd(), &mut stat) } != 0 {
        return Err(proof_unavailable());
    }
    match stat.f_type as u64 {
        0xEF53 => Ok("ext4"),
        0x5846_5342 => Ok("xfs"),
        0x9123_683E => Ok("btrfs"),
        0x2FC1_2FC1 => Ok("zfs"),
        0x794C_7630 => Ok("overlay"),
        0x0102_1994 => Ok("tmpfs"),
        _ => Err(proof_unavailable()),
    }
}

/// Open `/proc/<pid>` once and hold it. A held proc directory cannot be
/// redirected to a recycled numeric pid, so every later task-object read
/// (`stat`, `root`, `mountinfo`) goes through `openat` on this descriptor.
fn open_proc_dir(pid: u32) -> Result<File> {
    let path = CString::new(format!("/proc/{pid}")).map_err(|_| proof_unavailable())?;
    let fd = unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(proof_unavailable());
    }
    let file = unsafe { File::from_raw_fd(fd) };
    let stat = fstat(&file)?;
    if stat.st_mode & libc::S_IFMT != libc::S_IFDIR {
        return Err(proof_unavailable());
    }
    Ok(file)
}

fn read_relative(dir: &File, name: &str, max: usize) -> Result<Vec<u8>> {
    let mut file = open_file_at(dir, name, libc::O_RDONLY | libc::O_CLOEXEC)?;
    let mut bytes = Vec::new();
    file.by_ref()
        .take(max as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| proof_unavailable())?;
    if bytes.is_empty() || bytes.len() > max {
        return Err(proof_unavailable());
    }
    Ok(bytes)
}

/// `/proc/<pid>/stat` field 22 (starttime), read through the held proc dir and
/// never by reopening the numeric pid.
fn proc_starttime_at(dir: &File) -> Result<u64> {
    let bytes = read_relative(dir, "stat", 4096).map_err(|_| stale_refused())?;
    let text = std::str::from_utf8(&bytes).map_err(|_| stale_refused())?;
    let (_, rest) = text.rsplit_once(')').ok_or_else(stale_refused)?;
    let fields: Vec<&str> = rest.split_whitespace().collect();
    fields
        .get(19)
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or_else(stale_refused)
}

fn open_guest_root(proc_dir: &File) -> Result<File> {
    // `root` is a magic symlink into the guest mount namespace; following it
    // relative to the held proc directory is the point. No `O_NOFOLLOW` here,
    // and no numeric `/proc/<pid>` reopen.
    let file = open_file_at(
        proc_dir,
        "root",
        libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC,
    )?;
    let stat = fstat(&file)?;
    if stat.st_mode & libc::S_IFMT != libc::S_IFDIR {
        return Err(proof_unavailable());
    }
    Ok(file)
}

fn open_guest_path(root: &File, target: &str, readable: bool) -> Result<File> {
    let relative = target.strip_prefix('/').ok_or_else(proof_unavailable)?;
    let components: Vec<&str> = relative.split('/').collect();
    if components.is_empty() {
        return Err(proof_unavailable());
    }
    let mut held = root.try_clone().map_err(|_| proof_unavailable())?;
    for (index, name) in components.iter().enumerate() {
        if name.is_empty() || *name == "." || *name == ".." {
            return Err(proof_unavailable());
        }
        let last = index + 1 == components.len();
        let flags = if last {
            let mut flags = libc::O_NOFOLLOW | libc::O_CLOEXEC;
            flags |= if readable {
                libc::O_RDONLY
            } else {
                libc::O_PATH
            };
            flags
        } else {
            libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC
        };
        held = open_file_at(&held, name, flags)?;
    }
    Ok(held)
}

fn open_guest_metadata(root: &File, target: &str) -> Result<File> {
    let file = open_guest_path(root, target, true)?;
    let stat = fstat(&file)?;
    if stat.st_mode & libc::S_IFMT != libc::S_IFREG {
        return Err(identity_refused());
    }
    Ok(file)
}

fn open_absolute_nofollow(path: &Path) -> Result<File> {
    let mut components = path.components().peekable();
    if components.next() != Some(Component::RootDir) {
        return Err(proof_unavailable());
    }
    let mut held = open_root()?;
    while let Some(component) = components.next() {
        let Component::Normal(name) = component else {
            return Err(proof_unavailable());
        };
        let name = name.to_str().ok_or_else(proof_unavailable)?;
        if name.is_empty() {
            return Err(proof_unavailable());
        }
        let last = components.peek().is_none();
        let flags = if last {
            libc::O_PATH | libc::O_NOFOLLOW | libc::O_CLOEXEC
        } else {
            libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC
        };
        held = open_file_at(&held, name, flags)?;
    }
    Ok(held)
}

fn open_socket_source(path: &Path) -> Result<HeldFile> {
    let file = open_absolute_nofollow(path)?;
    let stat = fstat(&file)?;
    if stat.st_mode & libc::S_IFMT != libc::S_IFSOCK {
        return Err(identity_refused());
    }
    Ok(HeldFile {
        path: path.to_path_buf(),
        file,
        stat,
    })
}

fn require_same_inode(source: &libc::stat, guest: &libc::stat) -> Result<()> {
    if source.st_dev != guest.st_dev
        || source.st_ino != guest.st_ino
        || (source.st_mode & libc::S_IFMT) != (guest.st_mode & libc::S_IFMT)
    {
        return Err(identity_refused());
    }
    Ok(())
}

#[derive(Debug)]
struct MountRecord {
    id: u64,
    root: Vec<u8>,
    target: Vec<u8>,
    options: Vec<Vec<u8>>,
    filesystem: Vec<u8>,
    readonly: bool,
}

fn parse_u64(raw: &[u8]) -> Option<u64> {
    if raw.is_empty() || !raw.iter().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    std::str::from_utf8(raw).ok()?.parse().ok()
}

fn is_dash(field: &[u8]) -> bool {
    field.len() == 1 && field[0] == b'-'
}

/// `mountpoint`/`root` escape space, tab, newline and backslash as `\040`,
/// `\011`, `\012` and `\134`. Accept exactly those and refuse every other
/// backslash sequence instead of treating it as literal text.
fn unescape(raw: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(raw.len());
    let mut index = 0;
    while index < raw.len() {
        if raw[index] == b'\\' {
            if index + 4 > raw.len() {
                return Err(proof_unavailable());
            }
            let digits = &raw[index + 1..index + 4];
            if !digits.iter().all(|byte| (b'0'..=b'7').contains(byte)) {
                return Err(proof_unavailable());
            }
            let value = u16::from(digits[0] - b'0') * 64
                + u16::from(digits[1] - b'0') * 8
                + u16::from(digits[2] - b'0');
            if !matches!(value, 0x20 | 0x09 | 0x0a | 0x5c) {
                return Err(proof_unavailable());
            }
            out.push(value as u8);
            index += 4;
        } else {
            out.push(raw[index]);
            index += 1;
        }
    }
    Ok(out)
}

fn comma_fields(raw: &[u8]) -> Vec<Vec<u8>> {
    raw.split(|byte| *byte == b',')
        .map(|field| field.to_vec())
        .collect()
}

fn parse_mountinfo(bytes: &[u8]) -> Result<Vec<MountRecord>> {
    let mut records = Vec::new();
    for line in bytes.split(|byte| *byte == b'\n') {
        if line.is_empty() {
            continue;
        }
        let fields: Vec<&[u8]> = line
            .split(|byte| *byte == b' ')
            .filter(|field| !field.is_empty())
            .collect();
        if fields.len() < 10 {
            return Err(proof_unavailable());
        }
        let separator = fields
            .iter()
            .position(|field| is_dash(field))
            .ok_or_else(proof_unavailable)?;
        if separator < 6 || fields.len() < separator + 4 {
            return Err(proof_unavailable());
        }
        if fields[separator + 1..].iter().any(|field| is_dash(field)) {
            return Err(proof_unavailable());
        }
        let id = parse_u64(fields[0]).ok_or_else(proof_unavailable)?;
        if id == 0 {
            return Err(proof_unavailable());
        }
        let root = unescape(fields[3])?;
        let target = unescape(fields[4])?;
        let options = comma_fields(fields[5]);
        let filesystem = fields[separator + 1].to_vec();
        // The per-mount read-only state is field 6 only. A read-only
        // superblock does not make this bind mount read-only, and a missing or
        // contradictory rw/ro token is not a state this profile accepts.
        let read_only = options
            .iter()
            .filter(|option| option.as_slice() == &b"ro"[..])
            .count();
        let writable = options
            .iter()
            .filter(|option| option.as_slice() == &b"rw"[..])
            .count();
        if read_only + writable != 1 {
            return Err(proof_unavailable());
        }
        records.push(MountRecord {
            id,
            root,
            target,
            options,
            filesystem,
            readonly: read_only == 1,
        });
    }
    Ok(records)
}

fn read_guest_mountinfo(proc_dir: &File) -> Result<Vec<u8>> {
    read_relative(proc_dir, "mountinfo", MAX_MOUNTINFO_BYTES)
}

/// The mount table `root` for a file bind mount is the source path relative to
/// the source mount's own root. Compute it from the held source's mount id and
/// the actual host mount table instead of guessing absolute path prefixes.
fn expected_bind_root(host: &[MountRecord], source: &File, source_path: &Path) -> Result<Vec<u8>> {
    let mount_id = statx_mount_id(source)?;
    let record = host
        .iter()
        .find(|record| record.id == mount_id)
        .ok_or_else(proof_unavailable)?;
    let mount_point = std::str::from_utf8(&record.target).map_err(|_| proof_unavailable())?;
    let root = std::str::from_utf8(&record.root).map_err(|_| proof_unavailable())?;
    let relative = source_path
        .strip_prefix(Path::new(mount_point))
        .map_err(|_| proof_unavailable())?;
    let joined = Path::new(root).join(relative);
    let joined = joined.to_str().ok_or_else(proof_unavailable)?;
    Ok(joined.as_bytes().to_vec())
}

struct FsRule(Vec<&'static str>);

impl FsRule {
    fn exact(name: &'static str) -> Self {
        Self(vec![name])
    }

    fn any(names: &[&'static str]) -> Self {
        Self(names.to_vec())
    }

    fn matches(&self, filesystem: &[u8]) -> bool {
        self.0.iter().any(|name| name.as_bytes() == filesystem)
    }
}

struct ExpectedMount {
    filesystem: FsRule,
    root: Vec<u8>,
    readonly: bool,
    required: bool,
    extra_options: &'static [&'static [u8]],
}

impl ExpectedMount {
    fn matches(&self, record: &MountRecord) -> bool {
        self.filesystem.matches(&record.filesystem)
            && record.root == self.root
            && record.readonly == self.readonly
            && self
                .extra_options
                .iter()
                .all(|wanted| record.options.iter().any(|got| got.as_slice() == *wanted))
    }
}

const RW_TMPFS_OPTIONS: &[&[u8]] = &[b"rw", b"nosuid", b"nodev"];

fn insert_expected(
    expected: &mut BTreeMap<Vec<u8>, ExpectedMount>,
    target: &str,
    mount: ExpectedMount,
) {
    expected.insert(target.as_bytes().to_vec(), mount);
}

fn simple_mount(
    filesystem: &'static str,
    root: &str,
    readonly: bool,
    required: bool,
) -> ExpectedMount {
    ExpectedMount {
        filesystem: FsRule::exact(filesystem),
        root: root.as_bytes().to_vec(),
        readonly,
        required,
        extra_options: &[],
    }
}

fn file_mount(filesystem: &'static str, root: Vec<u8>) -> ExpectedMount {
    ExpectedMount {
        filesystem: FsRule::exact(filesystem),
        root,
        readonly: true,
        required: true,
        extra_options: &[],
    }
}

/// Exact Linux Docker 29 mount baseline, four producer tmpfs mounts, five
/// proven files and the finite masked/read-only records Docker may create when
/// the configured target exists. Nothing else can be added.
fn expected_mounts(
    id: &str,
    metadata: Option<&BTreeMap<&'static str, HeldFile>>,
    ops_source: &HeldFile,
    attest_source: &HeldFile,
    host: &[MountRecord],
) -> Result<BTreeMap<Vec<u8>, ExpectedMount>> {
    let mut expected = BTreeMap::new();

    // Fixed Docker/runc baseline.
    insert_expected(&mut expected, "/", simple_mount("overlay", "/", true, true));
    insert_expected(
        &mut expected,
        "/proc",
        simple_mount("proc", "/", false, true),
    );
    insert_expected(
        &mut expected,
        "/dev",
        simple_mount("tmpfs", "/", false, true),
    );
    insert_expected(
        &mut expected,
        "/dev/pts",
        simple_mount("devpts", "/", false, true),
    );
    insert_expected(
        &mut expected,
        "/sys",
        simple_mount("sysfs", "/", true, true),
    );
    insert_expected(
        &mut expected,
        "/sys/fs/cgroup",
        ExpectedMount {
            filesystem: FsRule::exact("cgroup2"),
            root: format!("/system.slice/docker-{id}.scope").into_bytes(),
            readonly: true,
            required: true,
            extra_options: &[],
        },
    );
    insert_expected(
        &mut expected,
        "/dev/mqueue",
        simple_mount("mqueue", "/", false, true),
    );
    insert_expected(
        &mut expected,
        "/dev/shm",
        simple_mount("tmpfs", "/", false, true),
    );

    // The exact four producer tmpfs mounts.
    for target in ["/run", "/tmp", "/workspace", "/home/agent"] {
        insert_expected(
            &mut expected,
            target,
            ExpectedMount {
                filesystem: FsRule::exact("tmpfs"),
                root: b"/".to_vec(),
                readonly: false,
                required: true,
                extra_options: RW_TMPFS_OPTIONS,
            },
        );
    }

    // The two owned socket files.
    for (target, source) in [
        (spec::OPS_TARGET, ops_source),
        (spec::ATTEST_TARGET, attest_source),
    ] {
        let filesystem = filesystem_name(&source.file)?;
        let root = expected_bind_root(host, &source.file, &source.path)?;
        insert_expected(&mut expected, target, file_mount(filesystem, root));
    }

    // The three engine-generated metadata files, only in the all-canonical
    // producer shape. The all-empty shape creates no `/etc` mounts at all.
    if let Some(sources) = metadata {
        for (name, target) in METADATA {
            let source = sources.get(name).ok_or_else(proof_unavailable)?;
            let filesystem = filesystem_name(&source.file)?;
            let root = expected_bind_root(host, &source.file, &source.path)?;
            insert_expected(&mut expected, target, file_mount(filesystem, root));
        }
    }

    // Docker's fixed read-only process paths. They exist whenever the pinned
    // image has the target; the exact record is fixed when present.
    for (target, root) in [
        ("/proc/bus", "/bus"),
        ("/proc/fs", "/fs"),
        ("/proc/irq", "/irq"),
        ("/proc/sys", "/sys"),
        ("/proc/sysrq-trigger", "/sysrq-trigger"),
    ] {
        insert_expected(
            &mut expected,
            target,
            simple_mount("proc", root, true, false),
        );
    }

    // Configured masked directories: a read-only tmpfs over the target.
    for target in ["/proc/acpi", "/proc/asound", "/proc/scsi", "/sys/firmware"] {
        insert_expected(
            &mut expected,
            target,
            simple_mount("tmpfs", "/", true, false),
        );
    }
    // Configured masked files: a bind of the host `/dev/null`; the containing
    // filesystem is the host's `/dev` (tmpfs or devtmpfs). It stays writable.
    for target in [
        "/proc/kcore",
        "/proc/keys",
        "/proc/latency_stats",
        "/proc/timer_list",
        "/proc/timer_stats",
        "/proc/sched_debug",
    ] {
        insert_expected(
            &mut expected,
            target,
            ExpectedMount {
                filesystem: FsRule::any(&["tmpfs", "devtmpfs"]),
                root: b"/null".to_vec(),
                readonly: false,
                required: false,
                extra_options: &[],
            },
        );
    }

    Ok(expected)
}

/// Check the actual guest mount table against the finite allowlist. Required
/// records must be present, known optional records must match exactly, and any
/// other record refuses. Read-only expectations are reported distinctly.
fn verify_mount_table(
    records: &[MountRecord],
    expected: &BTreeMap<Vec<u8>, ExpectedMount>,
) -> Result<()> {
    let mut by_target: BTreeMap<&[u8], &MountRecord> = BTreeMap::new();
    for record in records {
        if by_target.insert(record.target.as_slice(), record).is_some() {
            return Err(stacked_refused());
        }
    }
    for (target, wanted) in expected {
        match by_target.get(target.as_slice()) {
            Some(record) => {
                if !wanted.matches(record) {
                    if wanted.readonly && record.readonly != wanted.readonly {
                        return Err(writable_refused());
                    }
                    return Err(unexpected_refused());
                }
            }
            None if wanted.required => return Err(proof_unavailable()),
            None => {}
        }
    }
    for record in records {
        if !expected.contains_key(record.target.as_slice()) {
            return Err(unexpected_refused());
        }
    }
    Ok(())
}

fn require_mount_identity(guest: &File, source: &File, record: &MountRecord) -> Result<()> {
    if statx_mount_id(guest)? != record.id {
        return Err(identity_refused());
    }
    if statx_mount_id(source)? == record.id {
        return Err(identity_refused());
    }
    Ok(())
}

fn read_metadata(file: &mut File, before: &libc::stat) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    file.by_ref()
        .take(MAX_METADATA_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| proof_unavailable())?;
    if bytes.len() > MAX_METADATA_BYTES {
        return Err(content_refused());
    }
    let after = fstat(file)?;
    if after.st_dev != before.st_dev
        || after.st_ino != before.st_ino
        || after.st_size != before.st_size
        || after.st_mtime != before.st_mtime
        || after.st_mtime_nsec != before.st_mtime_nsec
        || after.st_ctime != before.st_ctime
        || after.st_ctime_nsec != before.st_ctime_nsec
    {
        return Err(content_refused());
    }
    Ok(bytes)
}

const METADATA: [(&str, &str); 3] = [
    ("resolv.conf", "/etc/resolv.conf"),
    ("hostname", "/etc/hostname"),
    ("hosts", "/etc/hosts"),
];

/// The two exhaustive producer shapes for the generated metadata files. The
/// exact producer sends `Config.NetworkDisabled=true` together with
/// `HostConfig.NetworkMode=none`; Docker then leaves `ResolvConfPath`,
/// `HostnamePath` and `HostsPath` empty and creates no `/etc` mounts, so the
/// pinned image's empty regular files remain on the read-only root mount.
/// Without `NetworkDisabled` Docker instead generates the three engine files
/// and bind-mounts them. There is no partial form.
#[derive(Clone, Copy, PartialEq, Eq)]
enum MetadataEvidence {
    Engine,
    Empty,
}

fn metadata_evidence(
    inspect: &serde_json::Value,
    paths: &BTreeMap<&'static str, PathBuf>,
) -> Result<MetadataEvidence> {
    let mut empty = 0usize;
    let mut engine = 0usize;
    for (field, name) in [
        ("ResolvConfPath", "resolv.conf"),
        ("HostnamePath", "hostname"),
        ("HostsPath", "hosts"),
    ] {
        let got = inspect
            .get(field)
            .and_then(|value| value.as_str())
            .ok_or_else(proof_unavailable)?;
        if got.is_empty() {
            empty += 1;
        } else if Some(got) == paths.get(name).and_then(|path| path.to_str()) {
            engine += 1;
        } else {
            return Err(provenance_refused());
        }
    }
    if empty == 3 {
        Ok(MetadataEvidence::Empty)
    } else if engine == 3 {
        Ok(MetadataEvidence::Engine)
    } else {
        // Partial/mixed evidence (for example two empty and one canonical
        // path) is never a producer output.
        Err(provenance_refused())
    }
}

/// The all-empty producer shape: the three `/etc` targets are ordinary empty
/// root-owned files on the same read-only guest root mount. Any generated
/// bind mount would have failed the exhaustive table already; this holds the
/// guest file descriptors and proves stable zero bytes and the root mount id.
fn verify_empty_metadata(guest_root: &File, root_record: &MountRecord, target: &str) -> Result<()> {
    let mut file = open_guest_path(guest_root, target, true)?;
    let before = fstat(&file)?;
    if before.st_mode & libc::S_IFMT != libc::S_IFREG || before.st_uid != 0 {
        return Err(identity_refused());
    }
    if before.st_size != 0 {
        return Err(content_refused());
    }
    if statx_mount_id(&file)? != root_record.id {
        return Err(identity_refused());
    }
    let bytes = read_metadata(&mut file, &before)?;
    if !bytes.is_empty() {
        return Err(content_refused());
    }
    Ok(())
}

fn is_lower_hex_64(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn is_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
            }
        })
}

fn verify_owner_identity(
    profile: &LocalProfile,
    runtime: &RuntimeSpec,
    inspect: &serde_json::Value,
) -> Result<()> {
    let labels = inspect
        .get("Config")
        .and_then(|config| config.get("Labels"))
        .and_then(|labels| labels.as_object())
        .ok_or_else(proof_unavailable)?;
    let wanted = spec::labels(profile, runtime);
    if labels.len() != wanted.len() {
        return Err(provenance_refused());
    }
    for (key, value) in &wanted {
        if labels.get(key).and_then(|got| got.as_str()) != Some(value.as_str()) {
            return Err(provenance_refused());
        }
    }
    Ok(())
}

fn string_array<'a>(value: &'a serde_json::Value, key: &str) -> Result<Vec<&'a str>> {
    value
        .get(key)
        .and_then(|array| array.as_array())
        .ok_or_else(proof_unavailable)?
        .iter()
        .map(|entry| entry.as_str().ok_or_else(proof_unavailable))
        .collect()
}

fn want_string_array(value: &serde_json::Value, key: &str, wanted: &[&str]) -> Result<()> {
    let got = string_array(value, key)?;
    if got.as_slice() != wanted {
        return Err(provenance_refused());
    }
    Ok(())
}

fn verify_producer_config(
    profile: &LocalProfile,
    runtime: &RuntimeSpec,
    inspect: &serde_json::Value,
) -> Result<()> {
    let config = inspect.get("Config").ok_or_else(proof_unavailable)?;
    let host = inspect.get("HostConfig").ok_or_else(proof_unavailable)?;
    if config.get("Hostname").and_then(|value| value.as_str()) != Some(spec::HOSTNAME) {
        return Err(provenance_refused());
    }
    if !config
        .get("Domainname")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .is_empty()
    {
        return Err(provenance_refused());
    }
    want_string_array(host, "Dns", &[spec::DNS_NAMESERVER])?;
    want_string_array(host, "DnsSearch", &[spec::DNS_SEARCH])?;
    want_string_array(host, "DnsOptions", &[spec::DNS_OPTION])?;
    want_string_array(host, "MaskedPaths", &spec::MASKED_PATHS)?;
    want_string_array(host, "ReadonlyPaths", &spec::READONLY_PATHS)?;

    // The exact two socket binds, and no other Bind/Tmpfs/volume syntax.
    let binds = string_array(host, "Binds")?;
    let wanted_binds = spec::socket_binds(runtime)?;
    if binds.len() != wanted_binds.len()
        || !wanted_binds
            .iter()
            .all(|wanted| binds.contains(&wanted.as_str()))
    {
        return Err(provenance_refused());
    }
    let tmpfs = host
        .get("Tmpfs")
        .and_then(|value| value.as_object())
        .ok_or_else(proof_unavailable)?;
    let wanted_tmpfs = spec::tmpfs_mounts(profile, runtime);
    if tmpfs.len() != wanted_tmpfs.len() {
        return Err(provenance_refused());
    }
    for (target, options) in &wanted_tmpfs {
        if tmpfs.get(target).and_then(|got| got.as_str()) != Some(options.as_str()) {
            return Err(provenance_refused());
        }
    }
    for empty in ["Mounts", "Volumes"] {
        match host.get(empty) {
            None | Some(serde_json::Value::Null) => {}
            Some(serde_json::Value::Array(items)) if items.is_empty() => {}
            Some(serde_json::Value::Object(items)) if items.is_empty() => {}
            _ => return Err(provenance_refused()),
        }
    }

    // `Mounts` describes the two socket binds only: Docker does not list the
    // generated `/etc` files or `HostConfig.Tmpfs` there, so anything extra is
    // a volume/`--mount` shape this profile never creates.
    let mounts = inspect
        .get("Mounts")
        .and_then(|value| value.as_array())
        .ok_or_else(proof_unavailable)?;
    if mounts.len() != 2 {
        return Err(provenance_refused());
    }
    let ops = runtime.ops_socket.to_str().ok_or_else(proof_unavailable)?;
    let attest = runtime
        .attest_socket
        .to_str()
        .ok_or_else(proof_unavailable)?;
    let mut seen_ops = false;
    let mut seen_attest = false;
    for mount in mounts {
        if mount.get("Type").and_then(|value| value.as_str()) != Some("bind")
            || mount.get("RW").and_then(|value| value.as_bool()) != Some(false)
        {
            return Err(unexpected_refused());
        }
        let destination = mount
            .get("Destination")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        let source = mount
            .get("Source")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        if destination == spec::OPS_TARGET && source == ops {
            seen_ops = true;
        } else if destination == spec::ATTEST_TARGET && source == attest {
            seen_attest = true;
        } else {
            return Err(provenance_refused());
        }
    }
    if !seen_ops || !seen_attest {
        return Err(provenance_refused());
    }
    Ok(())
}

fn verify_resolv_conf(bytes: &[u8]) -> Result<()> {
    // Docker's Generate always ends the file with a newline. A missing final
    // LF is not the producer byte stream even if the visible lines match.
    if !bytes.ends_with(b"\n") {
        return Err(content_refused());
    }
    const HEADER: [&str; 3] = [
        "# Generated by Docker Engine.",
        "# This file can be edited; Docker Engine will not make further changes once it",
        "# has been modified.",
    ];
    const BASED_ON: [&str; 2] = [
        "# Based on host file: '/etc/resolv.conf' (legacy)",
        "# Based on host file: '/run/systemd/resolve/resolv.conf' (legacy)",
    ];
    let text = std::str::from_utf8(bytes).map_err(|_| content_refused())?;
    let mut lines: Vec<&str> = text.split('\n').collect();
    if lines.last() == Some(&"") {
        lines.pop();
    }
    let minimum = 3 + 1 + 1 + 1 + 1 + 1 + 1 + 1;
    if lines.len() < minimum || !lines.starts_with(&HEADER) {
        return Err(content_refused());
    }
    let mut cursor = 3;
    if lines.get(cursor) != Some(&"") {
        return Err(content_refused());
    }
    cursor += 1;
    if lines.get(cursor) != Some(&"nameserver 127.0.0.1") {
        return Err(content_refused());
    }
    cursor += 1;
    if lines.get(cursor) != Some(&"options ndots:0") {
        return Err(content_refused());
    }
    cursor += 1;
    if lines.get(cursor) != Some(&"") {
        return Err(content_refused());
    }
    cursor += 1;
    match lines.get(cursor) {
        Some(line) if BASED_ON.contains(line) => cursor += 1,
        _ => return Err(content_refused()),
    }
    if lines.get(cursor) != Some(&"# Overrides: [nameservers search options]") {
        return Err(content_refused());
    }
    cursor += 1;
    if lines.get(cursor) != Some(&"# Option ndots from: override") {
        return Err(content_refused());
    }
    cursor += 1;
    if cursor != lines.len() {
        return Err(content_refused());
    }
    Ok(())
}

fn verify_hostname(bytes: &[u8]) -> Result<()> {
    let wanted = format!("{}\n", spec::HOSTNAME);
    if bytes == wanted.as_bytes() {
        Ok(())
    } else {
        Err(content_refused())
    }
}

fn verify_hosts(bytes: &[u8]) -> Result<()> {
    // Docker's `etchosts.Record.WriteTo` emits `IP\tHOSTS\n` and nothing else.
    // The guest is IPv4-only or carries the full six standard records; blank
    // lines, extra whitespace and extra records are not the producer output.
    const IPV4_ONLY: &[u8] = b"127.0.0.1\tlocalhost\n";
    const FULL: &[u8] = b"127.0.0.1\tlocalhost\n\
::1\tlocalhost ip6-localhost ip6-loopback\n\
fe00::\tip6-localnet\n\
ff00::\tip6-mcastprefix\n\
ff02::1\tip6-allnodes\n\
ff02::2\tip6-allrouters\n";
    if bytes == IPV4_ONLY || bytes == FULL {
        Ok(())
    } else {
        Err(content_refused())
    }
}

/// Verify the complete protected mount proof for one owned guest. Called after
/// start and again on every live recheck with a fresh inspect document. The
/// engine directory descriptor is retained, but every container directory,
/// metadata descriptor, mount table and content read is fresh.
pub(crate) fn verify_owned_mounts(
    engine: &EngineMountRoot,
    profile: &LocalProfile,
    runtime: &RuntimeSpec,
    inspect: &serde_json::Value,
    pid: u32,
    starttime: u64,
) -> Result<()> {
    if pid == 0 || peer::starttime(pid) != Some(starttime) {
        return Err(stale_refused());
    }
    let id = inspect
        .get("Id")
        .and_then(|value| value.as_str())
        .filter(|value| is_lower_hex_64(value))
        .ok_or_else(proof_unavailable)?;
    let state = inspect.get("State").ok_or_else(proof_unavailable)?;
    if state.get("Running").and_then(|value| value.as_bool()) != Some(true)
        || state.get("Pid").and_then(|value| value.as_u64()) != Some(u64::from(pid))
    {
        return Err(stale_refused());
    }
    if !is_uuid(&runtime.runtime_id) {
        return Err(provenance_refused());
    }
    let wanted_ops = profile
        .run_dir()
        .join("runtimes")
        .join(&runtime.runtime_id)
        .join("ops.sock");
    let wanted_attest = profile
        .run_dir()
        .join("runtimes")
        .join(&runtime.runtime_id)
        .join("attest.sock");
    if runtime.ops_socket != wanted_ops || runtime.attest_socket != wanted_attest {
        return Err(provenance_refused());
    }
    verify_owner_identity(profile, runtime, inspect)?;
    verify_producer_config(profile, runtime, inspect)?;

    // Hold the task object once. A recycled numeric pid cannot redirect these
    // later reads because they are relative to this descriptor, and the task
    // starttime is required to still match before and after the proof.
    let proc_dir = open_proc_dir(pid)?;
    if proc_starttime_at(&proc_dir)? != starttime {
        return Err(stale_refused());
    }

    let mut paths = BTreeMap::new();
    for (name, _) in METADATA {
        paths.insert(name, engine.resource_path(id, name));
    }
    let evidence = metadata_evidence(inspect, &paths)?;
    let mut sources = BTreeMap::new();
    if evidence == MetadataEvidence::Engine {
        for (name, _) in METADATA {
            sources.insert(name, engine.open_metadata(id, name)?);
        }
    }

    let ops_source = open_socket_source(&runtime.ops_socket)?;
    let attest_source = open_socket_source(&runtime.attest_socket)?;

    let guest_root = open_guest_root(&proc_dir)?;
    let ops_guest = open_guest_path(&guest_root, spec::OPS_TARGET, false)?;
    require_same_inode(&ops_source.stat, &fstat(&ops_guest)?)?;
    let attest_guest = open_guest_path(&guest_root, spec::ATTEST_TARGET, false)?;
    require_same_inode(&attest_source.stat, &fstat(&attest_guest)?)?;

    let host_bytes = std::fs::read("/proc/self/mountinfo").map_err(|_| proof_unavailable())?;
    if host_bytes.len() > MAX_MOUNTINFO_BYTES {
        return Err(proof_unavailable());
    }
    let host = parse_mountinfo(&host_bytes)?;
    let guest_records = parse_mountinfo(&read_guest_mountinfo(&proc_dir)?)?;
    let expected = match evidence {
        MetadataEvidence::Engine => {
            expected_mounts(id, Some(&sources), &ops_source, &attest_source, &host)?
        }
        MetadataEvidence::Empty => expected_mounts(id, None, &ops_source, &attest_source, &host)?,
    };
    verify_mount_table(&guest_records, &expected)?;

    let by_target: BTreeMap<&[u8], &MountRecord> = guest_records
        .iter()
        .map(|record| (record.target.as_slice(), record))
        .collect();
    for (target, source) in [
        (spec::OPS_TARGET, &ops_source),
        (spec::ATTEST_TARGET, &attest_source),
    ] {
        let record = *by_target
            .get(target.as_bytes())
            .ok_or_else(proof_unavailable)?;
        let guest = if target == spec::OPS_TARGET {
            &ops_guest
        } else {
            &attest_guest
        };
        require_mount_identity(guest, &source.file, record)?;
    }

    match evidence {
        MetadataEvidence::Engine => {
            let mut guests = BTreeMap::new();
            for (name, target) in METADATA {
                let guest = open_guest_metadata(&guest_root, target)?;
                let guest_stat = fstat(&guest)?;
                let source = sources.get(name).ok_or_else(proof_unavailable)?;
                require_same_inode(&source.stat, &guest_stat)?;
                let record = *by_target
                    .get(target.as_bytes())
                    .ok_or_else(proof_unavailable)?;
                require_mount_identity(&guest, &source.file, record)?;
                guests.insert(name, (guest, guest_stat));
            }
            let (resolv_file, resolv_stat) = guests
                .get_mut("resolv.conf")
                .ok_or_else(proof_unavailable)?;
            let resolv = read_metadata(resolv_file, resolv_stat)?;
            verify_resolv_conf(&resolv)?;
            let (hostname_file, hostname_stat) =
                guests.get_mut("hostname").ok_or_else(proof_unavailable)?;
            let hostname = read_metadata(hostname_file, hostname_stat)?;
            verify_hostname(&hostname)?;
            let (hosts_file, hosts_stat) = guests.get_mut("hosts").ok_or_else(proof_unavailable)?;
            let hosts = read_metadata(hosts_file, hosts_stat)?;
            verify_hosts(&hosts)?;
        }
        MetadataEvidence::Empty => {
            let root_record = *by_target.get(&b"/"[..]).ok_or_else(proof_unavailable)?;
            for (_, target) in METADATA {
                verify_empty_metadata(&guest_root, root_record, target)?;
            }
        }
    }

    if proc_starttime_at(&proc_dir)? != starttime {
        return Err(stale_refused());
    }
    Ok(())
}

#[cfg(all(test, target_os = "linux"))]
#[path = "measure_mount_metadata_acceptance_20261027.rs"]
mod measure_mount_metadata_acceptance_20261027;
