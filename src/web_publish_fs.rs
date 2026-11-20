//! Unix anchored filesystem access for the web-publish freezer.
//!
//! Directory descriptors keep descendant lookup rooted at the originally
//! opened directory even if its pathname or an ancestor is renamed.
#![forbid(unsafe_code)]

use std::ffi::CString;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use crate::fd_fs::{fstat, DirFd};

pub(crate) type Dir = DirFd;

pub(crate) enum Child {
    Directory(Dir),
    Regular(File, u64),
    Other,
}

pub(crate) fn open_root(path: &Path) -> io::Result<Dir> {
    let dir = DirFd::open_nofollow(path)?;
    let stat = dir.stat()?;
    if stat.st_mode & libc::S_IFMT != libc::S_IFDIR {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a directory",
        ));
    }
    Ok(dir)
}

pub(crate) fn open_regular(path: &Path) -> io::Result<(File, u64)> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    Ok((file, metadata.len()))
}

pub(crate) fn entries(dir: &Dir) -> io::Result<Vec<String>> {
    let mut names = Vec::new();
    for entry in dir.read_dir()? {
        let entry = entry?;
        let name = std::str::from_utf8(entry.as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "non-UTF-8 entry name"))?;
        names.push(name.to_owned());
    }
    names.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    Ok(names)
}

pub(crate) fn open_child(parent: &Dir, name: &str) -> io::Result<Child> {
    let name = CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid entry name"))?;
    let fd = parent.open_nofollow_fd(&name)?;
    let stat = fstat(fd.as_fd())?;
    match stat.st_mode & libc::S_IFMT {
        libc::S_IFDIR => Ok(Child::Directory(
            DirFd::from_verified_dir(fd, &stat)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "not a directory"))?,
        )),
        libc::S_IFREG => {
            let file = File::from(fd);
            let len = file.metadata()?.len();
            Ok(Child::Regular(file, len))
        }
        _ => Ok(Child::Other),
    }
}
