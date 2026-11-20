//! Descriptor-pinned Unix filesystem primitives (`openat`, `O_NOFOLLOW`, RAII
//! directory handles and iterators).
//!
//! Isolates raw POSIX `libc` filesystem `unsafe` blocks behind a small audited
//! surface so storage and domain modules stay free of raw descriptor handling.

#![deny(clippy::undocumented_unsafe_blocks)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

#[cfg(unix)]
mod unix {
    use std::ffi::{CStr, CString};
    use std::fs::File;
    use std::io;
    use std::mem::MaybeUninit;
    use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    /// An owned directory file descriptor opened with `O_DIRECTORY | O_CLOEXEC`.
    ///
    /// All child operations execute relative to this descriptor, preventing
    /// ancestor path replacement races after the handle is opened.
    pub struct DirFd(OwnedFd);

    impl DirFd {
        /// Open the filesystem root (`/`) as a pinned directory descriptor.
        pub fn open_root() -> io::Result<Self> {
            // SAFETY: `c"/"` is a static NUL-terminated path and `libc::open`
            // with `O_RDONLY | O_DIRECTORY | O_CLOEXEC` requires no mode arg.
            let fd = unsafe {
                libc::open(
                    c"/".as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
                )
            };
            owned_fd_from_rc(fd).map(Self)
        }

        /// Open `path` as a directory, following a symlink at `path` itself.
        ///
        /// Intended only for operator-configured root directories (such as a PM
        /// root); every child operation on the returned handle enforces
        /// `O_NOFOLLOW`.
        pub fn open_dir(path: &Path) -> io::Result<Self> {
            let c_path = CString::new(path.as_os_str().as_bytes())
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
            // SAFETY: `c_path` is a live NUL-terminated C string and `open`
            // with read-only flags takes no mode argument.
            let fd = unsafe {
                libc::open(
                    c_path.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
                )
            };
            owned_fd_from_rc(fd).map(Self)
        }

        /// Open `path` as a directory without following a symlink at the final
        /// component.
        pub fn open_nofollow(path: &Path) -> io::Result<Self> {
            let c_path = CString::new(path.as_os_str().as_bytes())
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
            // SAFETY: `c_path` is a live NUL-terminated C string and `open`
            // with read-only flags takes no mode argument.
            let fd = unsafe {
                libc::open(
                    c_path.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            owned_fd_from_rc(fd).map(Self)
        }

        /// Wrap an already-opened descriptor after verifying via `fstat` that
        /// it refers to a directory.
        pub fn from_verified_dir(fd: OwnedFd, stat: &libc::stat) -> Option<Self> {
            if stat.st_mode & libc::S_IFMT == libc::S_IFDIR {
                Some(Self(fd))
            } else {
                None
            }
        }

        /// Convert this pinned directory descriptor into a [`File`] handle.
        pub fn into_file(self) -> File {
            File::from(self.0)
        }

        /// Duplicate this directory descriptor with `FD_CLOEXEC`, avoiding
        /// standard I/O slots 0..=2.
        pub fn try_clone(&self) -> io::Result<Self> {
            duplicate_cloexec(self.0.as_fd()).map(Self)
        }

        /// Inspect metadata for this pinned directory via `fstat`.
        pub fn stat(&self) -> io::Result<libc::stat> {
            fstat(self.0.as_fd())
        }

        /// Inspect metadata for a direct child entry relative to `self`
        /// without following symlinks (`fstatat(..., AT_SYMLINK_NOFOLLOW)`).
        pub fn fstatat_nofollow(&self, name: &CStr) -> io::Result<libc::stat> {
            let mut stat = MaybeUninit::<libc::stat>::uninit();
            // SAFETY: `self.0` is a valid open directory fd, `name` is a valid
            // NUL-terminated C string, and `stat.as_mut_ptr()` points to valid
            // writable storage for `libc::stat`.
            let rc = unsafe {
                libc::fstatat(
                    self.0.as_raw_fd(),
                    name.as_ptr(),
                    stat.as_mut_ptr(),
                    libc::AT_SYMLINK_NOFOLLOW,
                )
            };
            if rc != 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: `libc::fstatat` returned 0, so `stat` is fully initialized.
            Ok(unsafe { stat.assume_init() })
        }

        /// Flush directory metadata changes to storage via `fsync`.
        pub fn sync_all(&self) -> io::Result<()> {
            // SAFETY: `self.0` is a valid open directory fd.
            let rc = unsafe { libc::fsync(self.0.as_raw_fd()) };
            check_zero(rc)
        }

        /// Open an existing child directory relative to `self` without
        /// following symlinks.
        pub fn open_child_dir(&self, name: &CStr) -> io::Result<Self> {
            // SAFETY: `self.0` is a valid open directory fd, `name` is a valid
            // NUL-terminated C string, and no `O_CREAT`/`O_TMPFILE` flag is set.
            let fd = unsafe {
                libc::openat(
                    self.0.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            owned_fd_from_rc(fd).map(Self)
        }

        /// Create a child directory relative to `self` with `mode`.
        pub fn mkdir(&self, name: &CStr, mode: libc::mode_t) -> io::Result<()> {
            // SAFETY: `self.0` is a valid open directory fd and `name` is a
            // valid NUL-terminated C string.
            let rc = unsafe { libc::mkdirat(self.0.as_raw_fd(), name.as_ptr(), mode) };
            check_zero(rc)
        }

        /// Set POSIX permission bits on this pinned directory (`fchmod`).
        pub fn fchmod(&self, mode: libc::mode_t) -> io::Result<()> {
            // SAFETY: `self.0` is a valid open directory descriptor.
            let rc = unsafe { libc::fchmod(self.0.as_raw_fd(), mode) };
            check_zero(rc)
        }

        /// Open a child entry read-only with `O_NOFOLLOW | O_NONBLOCK | O_CLOEXEC`,
        /// returning a raw `OwnedFd` so callers can `fstat` before deciding
        /// whether it is a regular file or directory.
        pub fn open_nofollow_fd(&self, name: &CStr) -> io::Result<OwnedFd> {
            // SAFETY: `self.0` is a valid open directory fd, `name` is a valid
            // NUL-terminated C string, and read-only `openat` takes no mode arg.
            let fd = unsafe {
                libc::openat(
                    self.0.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
                )
            };
            owned_fd_from_rc(fd)
        }

        /// Open a child file read-only with `O_NOFOLLOW | O_NONBLOCK | O_CLOEXEC`.
        pub fn open_readonly_nofollow(&self, name: &CStr) -> io::Result<File> {
            self.open_nofollow_fd(name).map(File::from)
        }

        /// Open or create a child file with `flags | O_NOFOLLOW | O_CLOEXEC`
        /// and permission `mode`.
        pub fn open_file_nofollow(
            &self,
            name: &CStr,
            flags: libc::c_int,
            mode: libc::mode_t,
        ) -> io::Result<File> {
            // `libc::mode_t` is `u16` on macOS and `u32` on Linux; widen to
            // `c_uint` for C variadic promotion without narrowing.
            #[allow(clippy::useless_conversion)]
            let mode_arg: libc::c_uint = mode.into();
            // SAFETY: `self.0` is a valid open directory fd, `name` is a valid
            // NUL-terminated C string, and `mode_arg` is promoted to `c_uint`
            // as required for variadic C arguments on both Linux and macOS.
            let fd = unsafe {
                libc::openat(
                    self.0.as_raw_fd(),
                    name.as_ptr(),
                    flags | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                    mode_arg,
                )
            };
            owned_fd_from_rc(fd).map(File::from)
        }

        /// Atomically rename a child entry within `self`.
        pub fn rename_child(&self, from: &CStr, to: &CStr) -> io::Result<()> {
            let dir = self.0.as_raw_fd();
            // SAFETY: `dir` is a valid open directory fd and `from`/`to` are
            // valid NUL-terminated C strings.
            let rc = unsafe { libc::renameat(dir, from.as_ptr(), dir, to.as_ptr()) };
            check_zero(rc)
        }

        /// Rename `from` inside `self` to `to` inside `dst_dir` without
        /// replacing an existing destination entry.
        ///
        /// Returns `Ok(true)` when renamed, `Ok(false)` when the platform or
        /// filesystem does not support no-replace cross-rename (`ENOSYS`,
        /// `EINVAL`, or `EOPNOTSUPP`), and `Err` on other failures (such as
        /// `EEXIST`).
        pub fn rename_child_to_noreplace(
            &self,
            from: &CStr,
            dst_dir: &DirFd,
            to: &CStr,
        ) -> io::Result<bool> {
            #[cfg(target_os = "linux")]
            // SAFETY: both descriptors are valid open directory fds and both
            // C strings are NUL-terminated.
            let rc = unsafe {
                libc::renameat2(
                    self.0.as_raw_fd(),
                    from.as_ptr(),
                    dst_dir.0.as_raw_fd(),
                    to.as_ptr(),
                    libc::RENAME_NOREPLACE,
                )
            };
            #[cfg(target_vendor = "apple")]
            // SAFETY: both descriptors are valid open directory fds and both
            // C strings are NUL-terminated.
            let rc = unsafe {
                libc::renameatx_np(
                    self.0.as_raw_fd(),
                    from.as_ptr(),
                    dst_dir.0.as_raw_fd(),
                    to.as_ptr(),
                    libc::RENAME_EXCL,
                )
            };
            #[cfg(any(target_os = "linux", target_vendor = "apple"))]
            {
                if rc == 0 {
                    return Ok(true);
                }
                let error = io::Error::last_os_error();
                if matches!(
                    error.raw_os_error(),
                    Some(libc::ENOSYS) | Some(libc::EINVAL) | Some(libc::EOPNOTSUPP)
                ) {
                    return Ok(false);
                }
                Err(error)
            }
            #[cfg(not(any(target_os = "linux", target_vendor = "apple")))]
            {
                let _ = (from, dst_dir, to);
                Ok(false)
            }
        }

        /// Create a hard link `to` pointing to `from` within `self` without
        /// replacing an existing entry (`EEXIST` if `to` already exists).
        pub fn link_child(&self, from: &CStr, to: &CStr) -> io::Result<()> {
            self.link_child_to(from, self, to)
        }

        /// Create a hard link `to` inside `dst_dir` pointing to `from` inside
        /// `self` without replacing an existing entry (`EEXIST` if `to`
        /// already exists).
        pub fn link_child_to(&self, from: &CStr, dst_dir: &DirFd, to: &CStr) -> io::Result<()> {
            // SAFETY: `self.0` and `dst_dir.0` are valid open directory fds and
            // both `from`/`to` are valid NUL-terminated C strings.
            let rc = unsafe {
                libc::linkat(
                    self.0.as_raw_fd(),
                    from.as_ptr(),
                    dst_dir.0.as_raw_fd(),
                    to.as_ptr(),
                    0,
                )
            };
            check_zero(rc)
        }

        /// Unlink a non-directory child entry within `self`.
        pub fn unlink_child(&self, name: &CStr) -> io::Result<()> {
            // SAFETY: `self.0` is a valid open directory fd and `name` is a
            // valid NUL-terminated C string.
            let rc = unsafe { libc::unlinkat(self.0.as_raw_fd(), name.as_ptr(), 0) };
            check_zero(rc)
        }

        /// Remove an empty child directory within `self` without following
        /// symlinks (`unlinkat(..., AT_REMOVEDIR)`).
        pub fn rmdir_child(&self, name: &CStr) -> io::Result<()> {
            // SAFETY: `self.0` is a valid open directory fd and `name` is a
            // valid NUL-terminated C string.
            let rc =
                unsafe { libc::unlinkat(self.0.as_raw_fd(), name.as_ptr(), libc::AT_REMOVEDIR) };
            check_zero(rc)
        }

        /// Enumerate direct child names (excluding `.` and `..`) using a
        /// duplicated directory descriptor so `self` remains usable.
        pub fn read_dir(&self) -> io::Result<DirIter> {
            let scan_fd = duplicate_cloexec(self.0.as_fd())?;
            let raw = scan_fd.as_raw_fd();
            // SAFETY: `raw` is a newly duplicated open directory fd. On success
            // `fdopendir` takes ownership of `raw` (so we disarm `scan_fd` via
            // `into_raw_fd`); on failure `scan_fd` drops and closes `raw`.
            let dir = unsafe { libc::fdopendir(raw) };
            if dir.is_null() {
                return Err(io::Error::last_os_error());
            }
            let _ = std::os::fd::IntoRawFd::into_raw_fd(scan_fd);
            Ok(DirIter(dir))
        }

        /// Create an anonymous (`O_TMPFILE`) read/write inode inside `self`.
        #[cfg(target_os = "linux")]
        pub fn open_tmpfile(&self, mode: libc::mode_t) -> io::Result<File> {
            // SAFETY: `self.0` is a valid open directory fd, `c"."` is a static
            // NUL-terminated C string, and `O_TMPFILE` consumes `mode` (`u32` on Linux).
            let fd = unsafe {
                libc::openat(
                    self.0.as_raw_fd(),
                    c".".as_ptr(),
                    libc::O_TMPFILE | libc::O_RDWR | libc::O_CLOEXEC,
                    mode,
                )
            };
            owned_fd_from_rc(fd).map(File::from)
        }

        /// Materialize an open file inode (`file`) at `name` inside `self`
        /// without replacing an existing entry.
        ///
        /// Tries `linkat(..., AT_EMPTY_PATH)` first and falls back to
        /// `/proc/self/fd/<fd>` when `CAP_DAC_READ_SEARCH` is absent
        /// (`EPERM` / `EINVAL`).
        #[cfg(target_os = "linux")]
        pub fn link_fd(&self, file: &File, name: &CStr) -> io::Result<()> {
            // SAFETY: `file` and `self.0` are valid open descriptors and both
            // C strings are NUL-terminated.
            let mut rc = unsafe {
                libc::linkat(
                    file.as_raw_fd(),
                    c"".as_ptr(),
                    self.0.as_raw_fd(),
                    name.as_ptr(),
                    libc::AT_EMPTY_PATH,
                )
            };
            if rc != 0 {
                let err = io::Error::last_os_error();
                if matches!(err.raw_os_error(), Some(libc::EPERM | libc::EINVAL)) {
                    let procfd = CString::new(format!("/proc/self/fd/{}", file.as_raw_fd()))
                        .map_err(|_| {
                            io::Error::new(io::ErrorKind::InvalidInput, "invalid procfd path")
                        })?;
                    // SAFETY: `procfd` and `name` are valid NUL-terminated C
                    // strings and `self.0` is a valid open directory fd.
                    rc = unsafe {
                        libc::linkat(
                            libc::AT_FDCWD,
                            procfd.as_ptr(),
                            self.0.as_raw_fd(),
                            name.as_ptr(),
                            libc::AT_SYMLINK_FOLLOW,
                        )
                    };
                } else {
                    return Err(err);
                }
            }
            check_zero(rc)
        }
    }

    /// RAII iterator over direct entry names of a [`DirFd`], skipping `.` and `..`.
    pub struct DirIter(*mut libc::DIR);

    impl Drop for DirIter {
        fn drop(&mut self) {
            // SAFETY: `self.0` is uniquely owned by this iterator from `fdopendir`.
            unsafe {
                libc::closedir(self.0);
            }
        }
    }

    impl Iterator for DirIter {
        type Item = io::Result<CString>;

        fn next(&mut self) -> Option<Self::Item> {
            loop {
                clear_errno();
                // SAFETY: `self.0` is a live `DIR*` exclusively borrowed via `&mut self`.
                let entry = unsafe { libc::readdir(self.0) };
                if entry.is_null() {
                    let err = io::Error::last_os_error();
                    if err.raw_os_error().is_some_and(|code| code != 0) {
                        return Some(Err(err));
                    }
                    return None;
                }
                // SAFETY: `readdir` returned a non-null `dirent` pointer valid
                // until the next `readdir` call on `self.0`, with NUL-terminated `d_name`.
                let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
                if matches!(name.to_bytes(), b"." | b"..") {
                    continue;
                }
                return Some(Ok(name.to_owned()));
            }
        }
    }

    /// Query `fstat` for any borrowed file descriptor.
    pub fn fstat(fd: BorrowedFd<'_>) -> io::Result<libc::stat> {
        let mut stat = MaybeUninit::<libc::stat>::uninit();
        // SAFETY: `fd` is a live descriptor and `stat.as_mut_ptr()` points to
        // valid writable storage for `libc::stat`.
        let rc = unsafe { libc::fstat(fd.as_raw_fd(), stat.as_mut_ptr()) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `libc::fstat` returned 0, so `stat` is fully initialized.
        Ok(unsafe { stat.assume_init() })
    }

    /// Return the calling process's effective user ID.
    pub fn geteuid() -> libc::uid_t {
        // SAFETY: `geteuid` is always safe to call and cannot fail.
        unsafe { libc::geteuid() }
    }

    /// Return the calling process's real user ID.
    pub fn getuid() -> libc::uid_t {
        // SAFETY: `getuid` is always safe to call and cannot fail.
        unsafe { libc::getuid() }
    }

    /// Acquire a non-blocking exclusive `flock` on `file`.
    pub fn flock_exclusive_nonblocking(file: &File) -> io::Result<()> {
        // SAFETY: `file` owns a live open descriptor.
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        check_zero(rc)
    }

    /// Set `O_NONBLOCK` on an open descriptor while preserving existing status
    /// flags.
    pub fn set_nonblocking(fd: BorrowedFd<'_>) -> io::Result<()> {
        let raw = fd.as_raw_fd();
        // SAFETY: `raw` is a live descriptor borrowed for this call.
        let flags = unsafe { libc::fcntl(raw, libc::F_GETFL) };
        if flags < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `raw` is a live descriptor and `F_SETFL` accepts the bitwise
        // OR of the current status flags with `O_NONBLOCK`.
        let rc = unsafe { libc::fcntl(raw, libc::F_SETFL, flags | libc::O_NONBLOCK) };
        check_zero(rc)
    }

    fn duplicate_cloexec(fd: BorrowedFd<'_>) -> io::Result<OwnedFd> {
        // SAFETY: `fd` is a valid open descriptor and `F_DUPFD_CLOEXEC` with
        // minimum fd 3 returns a new unowned descriptor or -1 on error.
        let dup = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 3) };
        owned_fd_from_rc(dup)
    }

    fn owned_fd_from_rc(fd: libc::c_int) -> io::Result<OwnedFd> {
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: syscall succeeded and returned a freshly opened, uniquely
        // owned file descriptor (`fd >= 0`).
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    fn check_zero(rc: libc::c_int) -> io::Result<()> {
        if rc == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    fn clear_errno() {
        #[cfg(target_os = "linux")]
        // SAFETY: libc exposes the calling thread's `errno` slot.
        unsafe {
            *libc::__errno_location() = 0;
        }
        #[cfg(target_os = "macos")]
        // SAFETY: libc exposes the calling thread's `errno` slot.
        unsafe {
            *libc::__error() = 0;
        }
    }
}

#[cfg(unix)]
pub use unix::{
    flock_exclusive_nonblocking, fstat, geteuid, getuid, set_nonblocking, DirFd, DirIter,
};

#[cfg(all(test, unix))]
mod tests {
    use super::{flock_exclusive_nonblocking, fstat, DirFd};
    use std::ffi::CString;
    use std::io::Read;
    use std::os::fd::AsFd;
    use std::os::unix::fs::symlink;
    use tempfile::TempDir;

    #[test]
    fn open_nofollow_and_child_refuse_symlinks() {
        let tmp = TempDir::new().unwrap();
        let real_dir = tmp.path().join("real");
        std::fs::create_dir(&real_dir).unwrap();
        std::fs::write(real_dir.join("secret.txt"), b"ok").unwrap();
        let link_dir = tmp.path().join("link-dir");
        symlink(&real_dir, &link_dir).unwrap();
        let link_file = real_dir.join("link-file");
        symlink(real_dir.join("secret.txt"), &link_file).unwrap();

        assert!(DirFd::open_nofollow(&link_dir).is_err());

        // `open_dir` follows a symlink at the configured root while still
        // enforcing `O_NOFOLLOW` on every child operation.
        let via_link = DirFd::open_dir(&link_dir).unwrap();
        assert!(via_link.open_readonly_nofollow(c"link-file").is_err());

        let parent = DirFd::open_nofollow(tmp.path()).unwrap();
        assert!(parent.open_child_dir(c"link-dir").is_err());

        let real = parent.open_child_dir(c"real").unwrap();
        assert!(real.open_readonly_nofollow(c"link-file").is_err());

        let mut file = real.open_readonly_nofollow(c"secret.txt").unwrap();
        let mut buf = Vec::new();
        file.read_to_end(&mut buf).unwrap();
        assert_eq!(buf, b"ok");
    }

    #[test]
    fn read_dir_enumerates_entries_and_excludes_dots() {
        let tmp = TempDir::new().unwrap();
        let dir = DirFd::open_nofollow(tmp.path()).unwrap();
        dir.mkdir(c"sub", 0o700).unwrap();
        let _ = dir
            .open_file_nofollow(
                c"alpha.txt",
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
                0o600,
            )
            .unwrap();

        let mut names: Vec<CString> = dir
            .read_dir()
            .unwrap()
            .map(|entry| entry.unwrap())
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec![
                CString::new("alpha.txt").unwrap(),
                CString::new("sub").unwrap()
            ]
        );

        let fd = dir.open_nofollow_fd(c"sub").unwrap();
        let stat = fstat(fd.as_fd()).unwrap();
        assert!(DirFd::from_verified_dir(fd, &stat).is_some());

        let child_stat = dir.fstatat_nofollow(c"alpha.txt").unwrap();
        assert_eq!(child_stat.st_mode & libc::S_IFMT, libc::S_IFREG);
        dir.link_child(c"alpha.txt", c"beta.txt").unwrap();
        assert!(dir.link_child(c"alpha.txt", c"beta.txt").is_err());
        dir.unlink_child(c"beta.txt").unwrap();
        dir.sync_all().unwrap();

        let file = dir.open_readonly_nofollow(c"alpha.txt").unwrap();
        flock_exclusive_nonblocking(&file).unwrap();
    }

    #[test]
    fn cross_dir_operations_and_permissions() {
        let tmp = TempDir::new().unwrap();
        let root = DirFd::open_nofollow(tmp.path()).unwrap();
        root.mkdir(c"spool", 0o755).unwrap();
        let spool = root.open_child_dir(c"spool").unwrap();
        spool.fchmod(0o700).unwrap();
        #[allow(clippy::useless_conversion)]
        let mode: u32 = spool.stat().unwrap().st_mode.into();
        assert_eq!(mode & 0o777, 0o700);

        let _ = spool
            .open_file_nofollow(
                c"artifact",
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
                0o600,
            )
            .unwrap();
        if !spool
            .rename_child_to_noreplace(c"artifact", &root, c"published")
            .unwrap()
        {
            spool
                .link_child_to(c"artifact", &root, c"published")
                .unwrap();
            spool.unlink_child(c"artifact").unwrap();
        }
        assert!(root.fstatat_nofollow(c"published").is_ok());

        let _ = spool
            .open_file_nofollow(c"dup", libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL, 0o600)
            .unwrap();
        assert!(spool.link_child_to(c"dup", &root, c"published").is_err());
        spool.unlink_child(c"dup").unwrap();
        root.rmdir_child(c"spool").unwrap();
        assert!(root.open_child_dir(c"spool").is_err());
    }
}
