//! Held immutable executable custody. Reuses managed-Pi openat2/hash helpers;
//! adds the selected static x86_64 policy without changing helper/Node policy.
use super::{refused, Deadline, Result, CARRIER, CLIENT, OBSERVER};
use crate::adapter::pi_guest::{
    execfd::{sha256_fd, OpenKind},
    topology::open_at2,
};
use std::fs::File;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::Path;
const MAX_EXE: u64 = 128 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Stamp {
    dev: u64,
    ino: u64,
    size: u64,
    uid: u32,
    gid: u32,
    mode: u32,
    links: u64,
    mtime: (i64, i64),
    ctime: (i64, i64),
}
pub(super) fn stamp(file: &File) -> Result<Stamp> {
    let m = file.metadata().map_err(|_| refused())?;
    Ok(Stamp {
        dev: m.dev(),
        ino: m.ino(),
        size: m.size(),
        uid: m.uid(),
        gid: m.gid(),
        mode: m.mode(),
        links: m.nlink(),
        mtime: (m.mtime(), m.mtime_nsec()),
        ctime: (m.ctime(), m.ctime_nsec()),
    })
}
pub(super) fn correspond(file: &File, selected: &Stamp) -> Result<()> {
    if stamp(file)? != *selected {
        return Err(refused());
    }
    Ok(())
}
fn no_file_caps(file: &File) -> Result<()> {
    let name = c"security.capability";
    let rc = unsafe { libc::fgetxattr(file.as_raw_fd(), name.as_ptr(), std::ptr::null_mut(), 0) };
    if rc >= 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::ENODATA) {
        return Err(refused());
    }
    Ok(())
}
pub(super) fn static_elf(file: &File, size: u64) -> Result<()> {
    let mut h = [0u8; 64];
    file.read_exact_at(&mut h, 0).map_err(|_| refused())?;
    if &h[..4] != b"\x7fELF" || h[4..7] != [2,1,1] || h[7] != 0
        || u16::from_le_bytes(h[16..18].try_into().unwrap()) != 2 // ET_EXEC only
        || u16::from_le_bytes(h[18..20].try_into().unwrap()) != 62
        || u32::from_le_bytes(h[20..24].try_into().unwrap()) != 1
        || u16::from_le_bytes(h[52..54].try_into().unwrap()) != 64
    {
        return Err(refused());
    }
    let off = u64::from_le_bytes(h[32..40].try_into().unwrap());
    let entry = u16::from_le_bytes(h[54..56].try_into().unwrap());
    let count = u16::from_le_bytes(h[56..58].try_into().unwrap());
    if off < 64
        || entry != 56
        || count == 0
        || count > 128
        || off
            .checked_add(u64::from(count) * 56)
            .is_none_or(|end| end > size)
    {
        return Err(refused());
    }
    let mut load = false;
    for i in 0..u64::from(count) {
        let mut p = [0u8; 56];
        file.read_exact_at(&mut p, off + i * 56)
            .map_err(|_| refused())?;
        let kind = u32::from_le_bytes(p[..4].try_into().unwrap());
        // No interpreter or dynamic segment/relocation startup closure.
        if kind == 3 || kind == 2 {
            return Err(refused());
        }
        if kind == 1 {
            let start = u64::from_le_bytes(p[8..16].try_into().unwrap());
            let bytes = u64::from_le_bytes(p[32..40].try_into().unwrap());
            let memory = u64::from_le_bytes(p[40..48].try_into().unwrap());
            if bytes > memory || start.checked_add(bytes).is_none_or(|end| end > size) {
                return Err(refused());
            }
            load = true;
        }
    }
    if !load {
        return Err(refused());
    }
    Ok(())
}
pub(super) fn measure(
    file: &File,
    expected: [u8; 32],
    uid: u32,
    deadline: Deadline,
) -> Result<Stamp> {
    deadline.check()?;
    measure_mode(file, expected, uid, uid, 0o755, deadline)
}
fn measure_mode(
    file: &File,
    expected: [u8; 32],
    uid: u32,
    gid: u32,
    mode: u32,
    deadline: Deadline,
) -> Result<Stamp> {
    deadline.check()?;
    let before = stamp(file)?;
    if expected == [0; 32]
        || before.size > MAX_EXE
        || before.size < 64
        || before.mode != libc::S_IFREG | mode
        || before.uid != uid
        || before.gid != gid
        || before.links != 1
    {
        return Err(refused());
    }
    no_file_caps(file)?;
    static_elf(file, before.size)?;
    let digest = sha256_fd(file, before.size).map_err(|_| refused())?;
    deadline.check()?;
    no_file_caps(file)?;
    if digest != expected || stamp(file)? != before {
        return Err(refused());
    }
    Ok(before)
}

#[derive(Clone, Copy)]
pub(super) enum Artifact {
    Client,
    Carrier,
    Observer,
    Constructor,
    Recipient,
    Helper,
}
impl Artifact {
    fn path(self) -> &'static str {
        match self {
            Self::Client => CLIENT,
            Self::Carrier => CARRIER,
            Self::Observer => OBSERVER,
            Self::Constructor => "/opt/protected/bin/cadence-root-constructor",
            Self::Recipient => "/opt/protected/bin/cadence-enrolled-recipient",
            Self::Helper => "/opt/protected/bin/cadence-agent-exec",
        }
    }
}
pub(super) struct HeldArtifact {
    file: File,
    dirs: Vec<File>,
    stamps: Vec<Stamp>,
    expected: [u8; 32],
    kind: Artifact,
}
const DIRS: &[&str] = &["/", "/opt", "/opt/protected", "/opt/protected/bin"];
impl HeldArtifact {
    pub(super) fn open(kind: Artifact, expected: [u8; 32], deadline: Deadline) -> Result<Self> {
        if expected == [0; 32] {
            return Err(refused());
        }
        let mut dirs = Vec::new();
        let mut stamps = Vec::new();
        for path in DIRS {
            let f = if *path == "/" {
                File::open(path).map_err(|_| refused())?
            } else {
                File::from(open_at2(Path::new(path), OpenKind::Dir).map_err(|_| refused())?)
            };
            let m = stamp(&f)?;
            if m.uid != 0 || m.gid != 0 || m.mode != libc::S_IFDIR | 0o755 {
                return Err(refused());
            }
            dirs.push(f);
            stamps.push(m);
        }
        let file = File::from(
            open_at2(Path::new(kind.path()), OpenKind::ExecFile).map_err(|_| refused())?,
        );
        stamps.push(match kind {
            Artifact::Helper => measure_mode(&file, expected, 0, 21000, 0o4750, deadline)?,
            _ => measure(&file, expected, 0, deadline)?,
        });
        Ok(Self {
            file,
            dirs,
            stamps,
            expected,
            kind,
        })
    }
    pub(super) fn recheck(&self, deadline: Deadline) -> Result<()> {
        let current = Self::open(self.kind, self.expected, deadline)?;
        if current.stamps != self.stamps || stamp(&self.file)? != *self.stamps.last().unwrap() {
            return Err(refused());
        }
        for (file, old) in self.dirs.iter().zip(&self.stamps) {
            if stamp(file)? != *old {
                return Err(refused());
            }
        }
        Ok(())
    }
    pub(super) fn fd(&self) -> std::os::fd::RawFd {
        self.file.as_raw_fd()
    }
    pub(super) fn selected_stamp(&self) -> &Stamp {
        self.stamps.last().unwrap()
    }
    pub(super) fn snapshot(&self) -> &[Stamp] {
        &self.stamps
    }
    pub(super) fn self_correspondence(&self, deadline: Deadline) -> Result<()> {
        let live = File::open("/proc/self/exe").map_err(|_| refused())?;
        if measure(&live, self.expected, 0, deadline)? != *self.selected_stamp() {
            return Err(refused());
        }
        self.recheck(deadline)
    }
    // Drop all held directories BEFORE close_range. No File destructor may
    // close a recycled descriptor after closure/exec; keep only the target FD.
    pub(super) fn live_correspondence(&self, pid: u32, deadline: Deadline) -> Result<()> {
        let file = File::open(format!("/proc/{pid}/exe")).map_err(|_| refused())?;
        let live = match self.kind {
            Artifact::Helper => measure_mode(&file, self.expected, 0, 21000, 0o4750, deadline)?,
            _ => measure(&file, self.expected, 0, deadline)?,
        };
        if &live != self.selected_stamp() {
            return Err(refused());
        }
        self.recheck(deadline)
    }
    pub(super) fn into_exec(self) -> File {
        self.file
    }
}
