//! Descriptor-relative encrypted-custody filesystem admission. These checks
//! establish owner/root-controlled paths, not isolation from the same UID.
use crate::error::{Error, Result};
use std::ffi::CString;
use std::fs::{File, Metadata};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path};

fn refused() -> Error {
    Error::invalid("key_unavailable", "unsafe or unavailable custody path")
}
fn store_refused() -> Error {
    Error::invalid("invalid_store", "unsafe or unavailable custody store")
}
fn name(bytes: &[u8]) -> Result<CString> {
    CString::new(bytes).map_err(|_| refused())
}
fn openat(parent: &File, name: &CString, flags: i32, mode: u32) -> Result<File> {
    // O_NONBLOCK prevents a forged FIFO/device from hanging admission before
    // fstat can refuse it. It has no effect on admitted regular files/dirs.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            mode as libc::mode_t,
        )
    };
    if fd < 0 {
        return Err(refused());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}
fn file_valid(meta: &Metadata) -> bool {
    meta.is_file()
        && meta.uid() == unsafe { libc::geteuid() }
        && meta.mode() & 0o7777 == 0o600
        && meta.nlink() == 1
}
fn statat(parent: &File, name: &CString) -> Result<Option<libc::stat>> {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    let rc = unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if rc == 0 {
        return Ok(Some(unsafe { stat.assume_init() }));
    }
    if std::io::Error::last_os_error().raw_os_error() == Some(libc::ENOENT) {
        return Ok(None);
    }
    Err(store_refused())
}
fn stat_file_valid(stat: &libc::stat) -> bool {
    stat.st_mode & libc::S_IFMT == libc::S_IFREG
        && stat.st_uid == unsafe { libc::geteuid() }
        && stat.st_mode & 0o7777 == 0o600
        && stat.st_nlink == 1
}

// Keep every parent pinned for the open manager's lifetime. Revalidation uses
// fstatat rather than opening/closing another DB fd: POSIX closes would release
// SQLite's process-scoped locks. The initial DB probe stays open until AFTER
// the SQLite connection closes.
pub(crate) struct Directories {
    handles: Vec<File>,
    names: Vec<CString>,
    private_descendants: bool,
}
impl Directories {
    pub(crate) fn open(path: &Path, create_last: bool) -> Result<(Self, bool)> {
        Self::open_mode(path, create_last, false, false, false)
    }
    /// Artifact directories allocate every missing component exclusively;
    /// existing components receive the same owner-controlled admission.
    pub(crate) fn artifact(path: &Path, fresh: bool) -> Result<(Self, bool)> {
        Self::open_mode(path, true, true, fresh, true)
    }
    /// Legacy metadata/artifact compatibility below a private owner ancestor.
    /// This does not relax key/config admission, which calls strict `open`.
    pub(crate) fn legacy_metadata(path: &Path) -> Result<(Self, bool)> {
        Self::open_mode(path, false, false, false, true)
    }
    fn open_mode(
        path: &Path,
        create_last: bool,
        parents: bool,
        fresh: bool,
        private_descendants: bool,
    ) -> Result<(Self, bool)> {
        if !path.is_absolute() {
            return Err(refused());
        }
        let components: Vec<_> = path.components().collect();
        if components.len() < 2
            || components
                .iter()
                .any(|c| !matches!(c, Component::RootDir | Component::Normal(_)))
        {
            return Err(refused());
        }
        let root = name(b"/")?;
        let fd = unsafe {
            libc::open(
                root.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if fd < 0 {
            return Err(refused());
        }
        let mut dirs = Self {
            handles: vec![unsafe { File::from_raw_fd(fd) }],
            names: Vec::new(),
            private_descendants,
        };
        let mut private_ancestor = false;
        let mut private_child = false;
        let mut created = false;
        for (i, component) in components.iter().enumerate().skip(1) {
            let Component::Normal(part) = component else {
                return Err(refused());
            };
            let part = name(part.as_bytes())?;
            let parent = dirs.handles.last().ok_or_else(refused)?;
            let final_component = i == components.len() - 1;
            let absent = statat(parent, &part)?.is_none();
            if fresh && final_component && !absent {
                return Err(store_refused());
            }
            if (parents || (create_last && final_component)) && absent {
                let rc = unsafe { libc::mkdirat(parent.as_raw_fd(), part.as_ptr(), 0o700) };
                if rc != 0 {
                    return Err(store_refused());
                }
                created |= final_component;
                parent.sync_all().map_err(|_| {
                    Error::invalid("outcome_unknown", "custody parent durability unknown")
                })?;
            }
            let opened = openat(parent, &part, libc::O_RDONLY | libc::O_DIRECTORY, 0)?;
            let meta = opened.metadata().map_err(|_| refused())?;
            let uid = unsafe { libc::geteuid() };
            if !meta.is_dir() || (meta.uid() != uid && meta.uid() != 0) {
                return Err(refused());
            }
            if private_child {
                if meta.uid() != uid || meta.mode() & 0o7777 != 0o700 {
                    return Err(refused());
                }
                private_child = false;
            } else if meta.mode() & 0o022 != 0
                && !(private_descendants && private_ancestor && meta.uid() == uid)
            {
                if i == 1
                    && part.as_bytes() == b"tmp"
                    && meta.uid() == 0
                    && meta.mode() & 0o1000 != 0
                {
                    private_child = true;
                } else {
                    return Err(refused());
                }
            }
            private_ancestor |= meta.uid() == uid && meta.mode() & 0o7777 == 0o700;
            dirs.names.push(part);
            dirs.handles.push(opened);
        }
        if private_child {
            return Err(refused());
        }
        dirs.check()?;
        Ok((dirs, created))
    }
    /// Remove only this still-pinned final directory and only if empty.
    /// Caller must prove it exclusively created that component.
    pub(crate) fn remove_empty_last(&self) -> Result<()> {
        self.check()?;
        let parent = &self.handles[self.handles.len() - 2];
        let leaf = self.names.last().ok_or_else(store_refused)?;
        if unsafe { libc::unlinkat(parent.as_raw_fd(), leaf.as_ptr(), libc::AT_REMOVEDIR) } != 0 {
            return Err(store_refused());
        }
        parent.sync_all().map_err(|_| store_refused())
    }
    pub(crate) fn last(&self) -> &File {
        self.handles.last().expect("root descriptor always present")
    }
    /// Metadata-only inspection; never opens the configured key/vault leaf.
    pub(crate) fn stat(&self, leaf: &std::ffi::OsStr) -> Result<Option<libc::stat>> {
        statat(self.last(), &name(leaf.as_bytes())?)
    }
    /// Only the fixed path-only record is opened here, never a key file.
    /// Absence is distinguished from dangling symlinks and inspection errors.
    pub(crate) fn private_record(&self, leaf: &std::ffi::OsStr) -> Result<Option<File>> {
        let leaf = name(leaf.as_bytes())?;
        if statat(self.last(), &leaf)?.is_none() {
            return Ok(None);
        }
        let file = openat(self.last(), &leaf, libc::O_RDONLY, 0)?;
        if !file_valid(&file.metadata().map_err(|_| refused())?) {
            return Err(refused());
        }
        self.check()?;
        Ok(Some(file))
    }
    #[allow(clippy::unnecessary_cast)] // stat field widths differ across Unix hosts
    pub(crate) fn check(&self) -> Result<()> {
        let mut private_ancestor = false;
        for (i, part) in self.names.iter().enumerate() {
            let current = statat(&self.handles[i], part)?.ok_or_else(store_refused)?;
            let pinned = self.handles[i + 1]
                .metadata()
                .map_err(|_| store_refused())?;
            if current.st_dev as u64 != pinned.dev()
                || current.st_ino as u64 != pinned.ino()
                || current.st_mode as u32 != pinned.mode()
                || current.st_uid != pinned.uid()
                || !pinned.is_dir()
            {
                return Err(store_refused());
            }
            let uid = unsafe { libc::geteuid() };
            let sticky_tmp = i == 0
                && part.as_bytes() == b"tmp"
                && pinned.uid() == 0
                && pinned.mode() & 0o1000 != 0;
            if (pinned.uid() != uid && pinned.uid() != 0)
                || (!sticky_tmp
                    && pinned.mode() & 0o022 != 0
                    && !(self.private_descendants && private_ancestor && pinned.uid() == uid))
            {
                return Err(store_refused());
            }
            private_ancestor |= pinned.uid() == uid && pinned.mode() & 0o7777 == 0o700;
            if i > 0
                && self.names[0].as_bytes() == b"tmp"
                && i == 1
                && (pinned.uid() != uid || pinned.mode() & 0o7777 != 0o700)
            {
                return Err(store_refused());
            }
        }
        Ok(())
    }
}

pub(super) fn owner_file(path: &Path) -> Result<File> {
    let parent = path.parent().ok_or_else(refused)?;
    let (dirs, _) = Directories::open(parent, false)?;
    let directory = dirs.last().metadata().map_err(|_| refused())?;
    if directory.uid() != unsafe { libc::geteuid() } || directory.mode() & 0o7777 != 0o700 {
        return Err(refused());
    }
    let final_name = name(path.file_name().ok_or_else(refused)?.as_bytes())?;
    let file = openat(dirs.last(), &final_name, libc::O_RDONLY, 0)?;
    if !file_valid(&file.metadata().map_err(|_| refused())?) {
        return Err(refused());
    }
    dirs.check()?;
    Ok(file)
}

pub(super) struct VaultFiles {
    dirs: Directories,
    pub(super) db: File,
}
impl VaultFiles {
    pub(super) fn open(path: &Path, initialize: bool) -> Result<Self> {
        let (dirs, _) = Directories::open(path, initialize).map_err(|e| {
            if e.code() == Some("outcome_unknown") {
                e
            } else {
                store_refused()
            }
        })?;
        let meta = dirs.last().metadata().map_err(|_| store_refused())?;
        if meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o7777 != 0o700 {
            return Err(store_refused());
        }
        let db_name = name(b"custody.sqlite3")?;
        for artifact in [b"custody.sqlite3-wal".as_slice(), b"custody.sqlite3-shm"] {
            if statat(dirs.last(), &name(artifact)?)?.is_some() {
                return Err(Error::invalid(
                    "unsupported_format",
                    "custody journal topology unsupported",
                ));
            }
        }
        if initialize {
            for artifact in [b"custody.sqlite3".as_slice(), b"custody.sqlite3-journal"] {
                if statat(dirs.last(), &name(artifact)?)?.is_some() {
                    return Err(Error::invalid("conflict", "custody store already exists"));
                }
            }
        } else if let Some(journal) = statat(dirs.last(), &name(b"custody.sqlite3-journal")?)? {
            if !stat_file_valid(&journal) {
                return Err(store_refused());
            }
        }
        let db = openat(
            dirs.last(),
            &db_name,
            libc::O_RDWR
                | if initialize {
                    libc::O_CREAT | libc::O_EXCL
                } else {
                    0
                },
            0o600,
        )
        .map_err(|_| store_refused())?;
        if !file_valid(&db.metadata().map_err(|_| store_refused())?) {
            return Err(store_refused());
        }
        let files = Self { dirs, db };
        files.check()?;
        Ok(files)
    }
    #[allow(clippy::unnecessary_cast)] // stat field widths differ across Unix hosts
    pub(super) fn check(&self) -> Result<()> {
        self.dirs.check()?;
        let directory = self.dirs.last().metadata().map_err(|_| store_refused())?;
        if directory.uid() != unsafe { libc::geteuid() } || directory.mode() & 0o7777 != 0o700 {
            return Err(store_refused());
        }
        let db = self.db.metadata().map_err(|_| store_refused())?;
        let current =
            statat(self.dirs.last(), &name(b"custody.sqlite3")?)?.ok_or_else(store_refused)?;
        if !file_valid(&db)
            || !stat_file_valid(&current)
            || current.st_dev as u64 != db.dev()
            || current.st_ino as u64 != db.ino()
        {
            return Err(store_refused());
        }
        for artifact in [b"custody.sqlite3-wal".as_slice(), b"custody.sqlite3-shm"] {
            if statat(self.dirs.last(), &name(artifact)?)?.is_some() {
                return Err(store_refused());
            }
        }
        if let Some(journal) = statat(self.dirs.last(), &name(b"custody.sqlite3-journal")?)? {
            if !stat_file_valid(&journal) {
                return Err(store_refused());
            }
        }
        Ok(())
    }
    pub(super) fn check_connection(&self, conn: &rusqlite::Connection) -> Result<()> {
        self.check()?;
        let mut moved: libc::c_int = 1;
        // The stock Unix VFS checks its ACTUAL open database inode against the
        // path. Failure/unsupported control refuses rather than assuming safety.
        let code = unsafe {
            rusqlite::ffi::sqlite3_file_control(
                conn.handle(),
                c"main".as_ptr(),
                rusqlite::ffi::SQLITE_FCNTL_HAS_MOVED,
                (&mut moved as *mut libc::c_int).cast(),
            )
        };
        if code != rusqlite::ffi::SQLITE_OK || moved != 0 {
            return Err(store_refused());
        }
        Ok(())
    }
    pub(super) fn sync(&self) -> Result<()> {
        self.db
            .sync_all()
            .and_then(|_| self.dirs.last().sync_all())
            .map_err(|_| Error::invalid("outcome_unknown", "custody filesystem durability unknown"))
    }
}
