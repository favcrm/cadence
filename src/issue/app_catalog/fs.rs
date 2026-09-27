//! Descriptor-relative reads: a swapped ancestor cannot redirect the catalog.
use std::ffi::{CStr, CString};
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd};
use std::os::unix::{
    ffi::OsStrExt,
    fs::{MetadataExt, PermissionsExt},
};
use std::path::{Component, Path};

use crate::error::{Error, Result};

pub(super) struct Root(File);

fn name(bytes: &[u8]) -> std::io::Result<CString> {
    CString::new(bytes).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "catalog path contains a NUL",
        )
    })
}

fn open_at(parent: &File, value: &[u8], flags: i32) -> std::io::Result<File> {
    let value = name(value)?;
    let fd = unsafe { libc::openat(parent.as_raw_fd(), value.as_ptr(), flags) };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

const DIRECTORY: i32 = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_DIRECTORY | libc::O_CLOEXEC;

impl Root {
    pub(super) fn open(path: &Path) -> Result<Self> {
        let start = if path.is_absolute() { "/" } else { "." };
        let c = name(start.as_bytes())?;
        let fd = unsafe { libc::open(c.as_ptr(), DIRECTORY) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let mut dir = unsafe { File::from_raw_fd(fd) };
        for part in path.components() {
            match part {
                Component::Normal(n) => dir = open_at(&dir, n.as_bytes(), DIRECTORY)?,
                Component::RootDir | Component::CurDir => {}
                _ => return Err(Error::rejected("catalog root contains an unsafe component")),
            }
        }
        Ok(Self(dir))
    }

    pub(super) fn dir(&self, path: &Path) -> std::io::Result<File> {
        // A dup shares directory offsets; reopen the pinned dot so repeated
        // inventory scans cannot inherit a prior readdir cursor.
        let mut dir = open_at(&self.0, b".", DIRECTORY)?;
        for part in path.components() {
            let Component::Normal(n) = part else {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "catalog path must stay below its workspace",
                ));
            };
            dir = open_at(&dir, n.as_bytes(), DIRECTORY)?;
        }
        Ok(dir)
    }

    fn parent(&self, path: &Path) -> std::io::Result<(File, CString)> {
        let leaf = path.file_name().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "catalog path has no file name",
            )
        })?;
        Ok((
            self.dir(path.parent().unwrap_or(Path::new("")))?,
            name(leaf.as_bytes())?,
        ))
    }

    pub(super) fn kind(&self, path: &Path) -> Result<Option<libc::mode_t>> {
        let (parent, leaf) = match self.parent(path) {
            Ok(p) => p,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        let rc = unsafe {
            libc::fstatat(
                parent.as_raw_fd(),
                leaf.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if rc < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::NotFound {
                return Ok(None);
            }
            return Err(error.into());
        }
        Ok(Some(unsafe { stat.assume_init() }.st_mode & libc::S_IFMT))
    }

    pub(super) fn read(&self, path: &Path, cap: u64) -> Result<Option<String>> {
        let (parent, leaf) = match self.parent(path) {
            Ok(p) => p,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC;
        let fd = unsafe { libc::openat(parent.as_raw_fd(), leaf.as_ptr(), flags) };
        if fd < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::NotFound {
                return Ok(None);
            }
            return Err(error.into());
        }
        let file = unsafe { File::from_raw_fd(fd) };
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.nlink() != 1 || metadata.len() > cap {
            return Err(Error::rejected(
                "catalog requires a bounded regular file without hard links",
            ));
        }
        let mut bytes = Vec::new();
        file.take(cap + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > cap {
            return Err(Error::rejected("catalog file exceeds its limit"));
        }
        String::from_utf8(bytes)
            .map(Some)
            .map_err(|_| Error::rejected("catalog files must be UTF-8"))
    }

    pub(super) fn list(&self, path: &Path, budget: &mut usize) -> Result<Vec<String>> {
        struct Directory(*mut libc::DIR);
        impl Drop for Directory {
            fn drop(&mut self) {
                unsafe {
                    libc::closedir(self.0);
                }
            }
        }
        let fd = self.dir(path)?.into_raw_fd();
        let raw = unsafe { libc::fdopendir(fd) };
        if raw.is_null() {
            unsafe {
                libc::close(fd);
            }
            return Err(std::io::Error::last_os_error().into());
        }
        let dir = Directory(raw);
        let mut names = Vec::new();
        loop {
            #[cfg(target_os = "linux")]
            let errno = unsafe { libc::__errno_location() };
            #[cfg(target_os = "macos")]
            let errno = unsafe { libc::__error() };
            unsafe {
                *errno = 0;
            }
            let next = unsafe { libc::readdir(dir.0) };
            if next.is_null() {
                if unsafe { *errno } != 0 {
                    return Err(std::io::Error::last_os_error().into());
                }
                break;
            }
            let bytes = unsafe { CStr::from_ptr((*next).d_name.as_ptr()) }.to_bytes();
            if bytes == b"." || bytes == b".." {
                continue;
            }
            *budget = budget
                .checked_sub(1)
                .ok_or_else(|| Error::rejected("catalog global inventory limit exceeded"))?;
            names.push(
                std::str::from_utf8(bytes)
                    .map_err(|_| Error::rejected("catalog names must be UTF-8"))?
                    .to_string(),
            );
        }
        names.sort();
        Ok(names)
    }

    pub(super) fn mkdir(&self, path: &Path) -> Result<()> {
        let (parent, leaf) = self.parent(path)?;
        if unsafe { libc::mkdirat(parent.as_raw_fd(), leaf.as_ptr(), 0o700) } < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::AlreadyExists {
                return Err(error.into());
            }
        }
        self.dir(path)?;
        parent.sync_all()?;
        Ok(())
    }

    pub(super) fn put(&self, path: &Path, text: &str) -> Result<()> {
        let (parent, leaf) = self.parent(path)?;
        let mode = match open_at(
            &parent,
            leaf.to_bytes(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        ) {
            Ok(file) => {
                let meta = file.metadata()?;
                if !meta.is_file() || meta.nlink() != 1 {
                    return Err(Error::rejected(
                        "catalog refuses a non-regular or hard-linked target",
                    ));
                }
                meta.mode() & 0o777
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0o600,
            Err(e) => return Err(e.into()),
        };
        let temp = name(format!(".tmp-{}", uuid::Uuid::new_v4().simple()).as_bytes())?;
        let flags =
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        let fd = unsafe { libc::openat(parent.as_raw_fd(), temp.as_ptr(), flags, 0o600) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let mut file = unsafe { File::from_raw_fd(fd) };
        let result = (|| {
            file.write_all(text.as_bytes())?;
            file.set_permissions(std::fs::Permissions::from_mode(mode))?;
            file.sync_all()?;
            if unsafe {
                libc::renameat(
                    parent.as_raw_fd(),
                    temp.as_ptr(),
                    parent.as_raw_fd(),
                    leaf.as_ptr(),
                )
            } < 0
            {
                return Err(std::io::Error::last_os_error().into());
            }
            parent.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            unsafe {
                libc::unlinkat(parent.as_raw_fd(), temp.as_ptr(), 0);
            }
        }
        result
    }

    pub(super) fn remove(&self, path: &Path) -> Result<()> {
        let (parent, leaf) = self.parent(path)?;
        if unsafe { libc::unlinkat(parent.as_raw_fd(), leaf.as_ptr(), 0) } < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::NotFound {
                return Err(error.into());
            }
        }
        parent.sync_all()?;
        Ok(())
    }
}
