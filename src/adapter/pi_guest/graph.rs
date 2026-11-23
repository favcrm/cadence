//! Verify the complete elected immutable Node/Pi/approved-extension tree.
//! A CLI hash alone never blesses modules discovered at runtime. Inventory is
//! exact (no extra leaves/symlinks/mount overlays), held by dirfd and read-only.
use crate::protected_pi_profile::{
    authority::{refused, ImageProfile},
    IMAGE_ROOT,
};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::CString;
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{FileExt, MetadataExt};

#[derive(PartialEq, Eq)]
struct Stamp {
    dev: u64,
    ino: u64,
    size: u64,
    mode: u32,
    uid: u32,
    gid: u32,
    links: u64,
    mt: i64,
    mn: i64,
    ct: i64,
    cn: i64,
}
fn stamp(file: &File) -> io::Result<Stamp> {
    let m = file.metadata()?;
    Ok(Stamp {
        dev: m.dev(),
        ino: m.ino(),
        size: m.size(),
        mode: m.mode(),
        uid: m.uid(),
        gid: m.gid(),
        links: m.nlink(),
        mt: m.mtime(),
        mn: m.mtime_nsec(),
        ct: m.ctime(),
        cn: m.ctime_nsec(),
    })
}
#[repr(C)]
struct OpenHow {
    flags: u64,
    mode: u64,
    resolve: u64,
}
fn child(parent: &File, name: &str, directory: bool) -> io::Result<File> {
    if name.is_empty() || name == "." || name == ".." || name.contains('/') {
        return Err(refused());
    }
    let name = CString::new(name).map_err(|_| refused())?;
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
        resolve: libc::RESOLVE_BENEATH | libc::RESOLVE_NO_SYMLINKS | libc::RESOLVE_NO_XDEV,
    };
    let fd = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            parent.as_raw_fd(),
            name.as_ptr(),
            &how,
            std::mem::size_of::<OpenHow>(),
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(fd as i32) })
}
fn readonly(file: &File) -> io::Result<()> {
    let mut fs: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstatvfs(file.as_raw_fd(), &mut fs) } != 0 || fs.f_flag & libc::ST_RDONLY == 0
    {
        return Err(refused());
    }
    Ok(())
}
fn dir(file: &File) -> io::Result<()> {
    let m = file.metadata()?;
    if !m.is_dir() || m.uid() != 0 || m.mode() & 0o7777 != 0o755 {
        return Err(refused());
    }
    readonly(file)
}
fn root() -> io::Result<File> {
    let mut held = File::open("/")?;
    for part in IMAGE_ROOT.strip_prefix('/').ok_or_else(refused)?.split('/') {
        let before = held.metadata()?;
        if !before.is_dir() || before.uid() != 0 || before.mode() & 0o022 != 0 {
            return Err(refused());
        }
        // Root image may be its own read-only mount. No crossings below it.
        let part = CString::new(part).map_err(|_| refused())?;
        let how = OpenHow {
            flags: (libc::O_RDONLY | libc::O_CLOEXEC | libc::O_DIRECTORY) as u64,
            mode: 0,
            resolve: libc::RESOLVE_BENEATH | libc::RESOLVE_NO_SYMLINKS,
        };
        let fd = unsafe {
            libc::syscall(
                libc::SYS_openat2,
                held.as_raw_fd(),
                part.as_ptr(),
                &how,
                std::mem::size_of::<OpenHow>(),
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        held = unsafe { File::from_raw_fd(fd as i32) };
    }
    dir(&held)?;
    Ok(held)
}
fn hash(file: &File, size: u64) -> io::Result<[u8; 32]> {
    let mut hash = Sha256::new();
    let mut at = 0;
    let mut buf = [0; 65536];
    while at < size {
        let n = file.read_at(&mut buf[..std::cmp::min(size - at, 65536) as usize], at)?;
        if n == 0 {
            return Err(refused());
        }
        hash.update(&buf[..n]);
        at += n as u64;
    }
    Ok(hash.finalize().into())
}
pub(crate) struct Graph {
    profile: ImageProfile,
    held: Vec<(File, Stamp)>,
    node: File,
}
impl Graph {
    pub(crate) fn open(profile: &ImageProfile) -> io::Result<Self> {
        profile.validate()?;
        let root = root()?;
        let files: BTreeMap<_, _> = profile.files.iter().map(|f| (f.path.as_str(), f)).collect();
        let mut directories = BTreeSet::new();
        for name in files.keys() {
            let mut prefix = String::new();
            let parts: Vec<_> = name.split('/').collect();
            for part in &parts[..parts.len() - 1] {
                if !prefix.is_empty() {
                    prefix.push('/');
                }
                prefix.push_str(part);
                directories.insert(prefix.clone());
            }
        }
        let mut found = BTreeSet::new();
        let mut held = Vec::new();
        let mut node = None;
        let mut pending = vec![(String::new(), root)];
        let mut count = 0;
        while let Some((prefix, directory)) = pending.pop() {
            dir(&directory)?;
            let before = stamp(&directory)?;
            // /proc is used solely to enumerate a held dirfd, never as a script
            // path/authority. Every entry is reopened beneath that dirfd.
            for entry in std::fs::read_dir(format!("/proc/self/fd/{}", directory.as_raw_fd()))? {
                count += 1;
                if count > 32768 {
                    return Err(refused());
                }
                let entry = entry?;
                let name = entry.file_name().into_string().map_err(|_| refused())?;
                let path = if prefix.is_empty() {
                    name.clone()
                } else {
                    format!("{prefix}/{name}")
                };
                if directories.contains(&path) {
                    pending.push((path, child(&directory, &name, true)?));
                    continue;
                }
                let pin = files.get(path.as_str()).ok_or_else(refused)?;
                let file = child(&directory, &name, false)?;
                let m = file.metadata()?;
                readonly(&file)?;
                if !m.is_file()
                    || m.uid() != 0
                    || m.mode() & 0o7777 != pin.mode
                    || m.nlink() != 1
                    || m.size() != pin.size
                {
                    return Err(refused());
                }
                let before = stamp(&file)?;
                if hash(&file, pin.size)? != pin.sha256 || stamp(&file)? != before {
                    return Err(refused());
                }
                if path == "node" {
                    node = Some(file.try_clone()?);
                }
                if !found.insert(path) {
                    return Err(refused());
                }
                held.push((file, before));
            }
            if stamp(&directory)? != before {
                return Err(refused());
            }
            held.push((directory, before));
        }
        if found.len() != files.len() {
            return Err(refused());
        }
        Ok(Self {
            profile: profile.clone(),
            held,
            node: node.ok_or_else(refused)?,
        })
    }
    pub(crate) fn node(&self) -> io::Result<File> {
        self.node.try_clone()
    }
    pub(crate) fn recheck(&self) -> io::Result<()> {
        for (file, before) in &self.held {
            readonly(file)?;
            if &stamp(file)? != before {
                return Err(refused());
            }
        }
        // Rewalk canonical paths: held immutable files alone do not prove Node
        // resolves the same complete JS tree after preparation.
        let current = Self::open(&self.profile)?;
        if stamp(&current.node)? != stamp(&self.node)? {
            return Err(refused());
        }
        Ok(())
    }
}
