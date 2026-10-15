//! Unix anchored filesystem access for the web-publish freezer.
//!
//! Directory descriptors keep descendant lookup rooted at the originally
//! opened directory even if its pathname or an ancestor is renamed.
use std::ffi::{CStr, CString};
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

pub(crate) enum Child {
    Directory(File),
    Regular(File, u64),
    Other,
}

pub(crate) fn open_root(path: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    if !file.metadata()?.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a directory",
        ));
    }
    Ok(file)
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

pub(crate) fn entries(dir: &File) -> io::Result<Vec<String>> {
    // fdopendir owns its descriptor, so enumerate a duplicate and retain the
    // original descriptor for openat calls throughout this directory walk.
    let duplicate = dir.try_clone()?.into_raw_fd();
    // SAFETY: `duplicate` is a newly-owned descriptor and fdopendir takes
    // ownership on success. On failure, ownership remains with this function.
    let stream = unsafe { libc::fdopendir(duplicate) };
    if stream.is_null() {
        let error = io::Error::last_os_error();
        // SAFETY: fdopendir failed, so `duplicate` was not consumed.
        unsafe { libc::close(duplicate) };
        return Err(error);
    }
    let stream = DirectoryStream(stream);
    let mut names = Vec::new();
    loop {
        set_errno(0);
        // SAFETY: `stream` owns a valid DIR pointer; readdir's result is used
        // only until the next call, and d_name is NUL-terminated by libc.
        let entry = unsafe { libc::readdir(stream.0) };
        if entry.is_null() {
            let errno = get_errno();
            if errno != 0 {
                return Err(io::Error::from_raw_os_error(errno));
            }
            break;
        }
        // SAFETY: a successful readdir returns a live dirent with NUL-ended
        // d_name, valid until the next readdir call.
        let bytes = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if bytes == b"." || bytes == b".." {
            continue;
        }
        let name = std::str::from_utf8(bytes)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "non-UTF-8 entry name"))?;
        names.push(name.to_owned());
    }
    names.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    Ok(names)
}

pub(crate) fn open_child(parent: &File, name: &str) -> io::Result<Child> {
    let name = CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid entry name"))?;
    // O_NOFOLLOW prevents a final-component symlink race; using the parent's
    // descriptor prevents ancestor pathname replacement from escaping root.
    // O_NONBLOCK means opening a raced FIFO cannot block before fstat rejects it.
    // SAFETY: parent is live, name is NUL-free, and openat returns a new fd.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful openat returned a uniquely-owned descriptor.
    let file = unsafe { File::from_raw_fd(fd) };
    let metadata = file.metadata()?;
    if metadata.is_dir() {
        Ok(Child::Directory(file))
    } else if metadata.is_file() {
        Ok(Child::Regular(file, metadata.len()))
    } else {
        Ok(Child::Other)
    }
}

struct DirectoryStream(*mut libc::DIR);

impl Drop for DirectoryStream {
    fn drop(&mut self) {
        // SAFETY: this guard uniquely owns the DIR pointer from fdopendir.
        unsafe { libc::closedir(self.0) };
    }
}

#[cfg(target_os = "linux")]
fn set_errno(value: i32) {
    // SAFETY: libc exposes the calling thread's errno slot.
    unsafe { *libc::__errno_location() = value };
}
#[cfg(target_os = "linux")]
fn get_errno() -> i32 {
    // SAFETY: libc exposes the calling thread's errno slot.
    unsafe { *libc::__errno_location() }
}
#[cfg(target_os = "macos")]
fn set_errno(value: i32) {
    // SAFETY: libc exposes the calling thread's errno slot.
    unsafe { *libc::__error() = value };
}
#[cfg(target_os = "macos")]
fn get_errno() -> i32 {
    // SAFETY: libc exposes the calling thread's errno slot.
    unsafe { *libc::__error() }
}
