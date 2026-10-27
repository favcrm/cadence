//! Artifact namespace ownership. Mutations address retained directory
//! descriptors, never an independent post-admission pathname walk. SQLite
//! needs a pathname: Linux hands over the retained descriptor itself, other
//! supported Unix re-proves the admitted canonical path on every use.
//! Publication is exclusive linkat, cleanup is inode/owner checked unlinkat.
use crate::error::{Error, Result};
use crate::platform::ControlledDirectories;
use std::collections::BTreeMap;
use std::ffi::{CString, OsStr, OsString};
use std::fs::File;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

fn refused() -> Error {
    Error::invalid(
        "invalid_store",
        "artifact namespace unavailable or occupied",
    )
}
fn leaf(name: &OsStr) -> Result<CString> {
    if name.is_empty() || matches!(name.as_bytes(), b"." | b"..") || name.as_bytes().contains(&b'/')
    {
        return Err(refused());
    }
    CString::new(name.as_bytes()).map_err(|_| refused())
}
#[derive(Clone)]
struct Identity {
    dev: u64,
    ino: u64,
    // Pin the actual allocated inode until all SQLite phases have closed.
    // Otherwise delete/recreate could recycle its numeric inode and fool
    // cleanup ownership. Never close this duplicate while SQLite holds locks.
    _file: std::sync::Arc<File>,
}

pub(super) struct ArtifactDir {
    pin: ControlledDirectories,
    path: PathBuf,
    // Identity of the admitted directory. Linux uses it for the one-time
    // /proc admission proof; the other supported Unix re-proves the pathname
    // view against it on every use.
    dir_dev: u64,
    dir_ino: u64,
    owned: BTreeMap<OsString, Identity>,
    committed: bool,
    created: bool,
}
impl ArtifactDir {
    pub(super) fn open(path: &Path, fresh: bool) -> Result<Self> {
        // Stock Unix SQLite needs a pathname. Linux can address the retained
        // descriptor; the other supported Unix host re-proves its admitted
        // canonical path before every use. Unknown hosts keep refusing.
        if !cfg!(any(target_os = "linux", target_os = "macos")) {
            return Err(refused());
        }
        let path = crate::platform::encrypted_config::resolve_output(path)?;
        let (pin, created) =
            ControlledDirectories::artifact(&path, fresh).map_err(|_| refused())?;
        let meta = pin.last().metadata()?;
        if meta.uid() != unsafe { libc::geteuid() } {
            return Err(refused());
        }
        let result = Self {
            pin,
            path,
            dir_dev: meta.dev(),
            dir_ino: meta.ino(),
            owned: BTreeMap::new(),
            committed: false,
            created,
        };
        // Prove the pathname view names the pinned directory before any file
        // is reserved. Linux checks only here; its per-use views stay
        // stat-free because /proc names the retained descriptor itself.
        result.admit_view()?;
        result.check()?;
        Ok(result)
    }
    pub(super) fn check(&self) -> Result<()> {
        self.pin.check().map_err(|_| refused())
    }
    /// The pathname stock SQLite receives. Linux names the retained
    /// descriptor, so no per-use stat is needed. Other supported Unix uses
    /// the admitted canonical path and re-proves it still names the pinned
    /// directory on every call, so a replaced, renamed-away or deleted
    /// output refuses instead of silently redirecting a later open.
    pub(super) fn view(&self) -> Result<PathBuf> {
        #[cfg(not(target_os = "linux"))]
        self.recheck_view()?;
        Ok(self.view_path())
    }
    /// One-time proof that the pathname view names the pinned directory,
    /// run before any file is reserved.
    #[cfg(target_os = "linux")]
    fn admit_view(&self) -> Result<()> {
        let proc = std::fs::metadata(self.view_path())?;
        if proc.dev() != self.dir_dev || proc.ino() != self.dir_ino {
            return Err(refused());
        }
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    fn admit_view(&self) -> Result<()> {
        self.recheck_view()
    }
    #[cfg(not(target_os = "linux"))]
    fn recheck_view(&self) -> Result<()> {
        let meta = std::fs::metadata(&self.path)?;
        if !meta.is_dir() || meta.dev() != self.dir_dev || meta.ino() != self.dir_ino {
            return Err(refused());
        }
        Ok(())
    }
    #[cfg(target_os = "linux")]
    fn view_path(&self) -> PathBuf {
        PathBuf::from(format!("/proc/self/fd/{}", self.pin.last().as_raw_fd()))
    }
    #[cfg(not(target_os = "linux"))]
    fn view_path(&self) -> PathBuf {
        self.path.clone()
    }
    pub(super) fn public_path(&self, name: &OsStr) -> PathBuf {
        self.path.join(name)
    }
    pub(super) fn file_path(&self, name: &OsStr) -> Result<PathBuf> {
        leaf(name)?;
        self.check()?;
        Ok(self.view()?.join(name))
    }
    pub(super) fn create(&mut self, name: &OsStr) -> Result<File> {
        self.check()?;
        let encoded = leaf(name)?;
        let raw = unsafe {
            libc::openat(
                self.pin.last().as_raw_fd(),
                encoded.as_ptr(),
                libc::O_RDWR
                    | libc::O_CREAT
                    | libc::O_EXCL
                    | libc::O_NOFOLLOW
                    | libc::O_CLOEXEC
                    | libc::O_NONBLOCK,
                0o600,
            )
        };
        if raw < 0 {
            return Err(refused());
        }
        let file = unsafe { File::from_raw_fd(raw) };
        let meta = file.metadata()?;
        if !meta.is_file()
            || meta.uid() != unsafe { libc::geteuid() }
            || meta.mode() & 0o7777 != 0o600
            || meta.nlink() != 1
        {
            return Err(refused());
        }
        self.owned.insert(
            name.to_os_string(),
            Identity {
                dev: meta.dev(),
                ino: meta.ino(),
                _file: std::sync::Arc::new(file.try_clone()?),
            },
        );
        Ok(file)
    }
    #[allow(clippy::unnecessary_cast)] // Unix stat widths differ
    fn same(&self, name: &OsStr, identity: &Identity) -> Result<bool> {
        Ok(self.pin.stat(name)?.is_some_and(|s| {
            s.st_dev as u64 == identity.dev
                && s.st_ino as u64 == identity.ino
                && s.st_mode & libc::S_IFMT == libc::S_IFREG
                && s.st_uid == unsafe { libc::geteuid() }
                && s.st_mode & 0o7777 == 0o600
        }))
    }
    /// Reserve all derived SQLite journal names before each write phase.
    /// A previously engine-deleted name is exclusively reallocated, never
    /// assumed ours by a stem/glob. A changed existing inode refuses.
    pub(super) fn sqlite_sidecars(&mut self, db: &OsStr) -> Result<()> {
        for suffix in ["-journal", "-wal", "-shm"] {
            let mut name = db.to_os_string();
            name.push(suffix);
            match self.pin.stat(&name)? {
                None => {
                    drop(self.create(&name)?);
                }
                Some(_) => {
                    let identity = self.owned.get(&name).cloned().ok_or_else(refused)?;
                    if !self.same(&name, &identity)? {
                        return Err(refused());
                    }
                }
            }
        }
        Ok(())
    }
    pub(super) fn remove_owned(&mut self, name: &OsStr) -> Result<()> {
        self.check()?;
        let Some(identity) = self.owned.get(name).cloned() else {
            return Err(refused());
        };
        if self.pin.stat(name)?.is_none() {
            self.owned.remove(name);
            return Ok(());
        }
        if !self.same(name, &identity)? {
            return Err(refused());
        }
        let encoded = leaf(name)?;
        if unsafe { libc::unlinkat(self.pin.last().as_raw_fd(), encoded.as_ptr(), 0) } != 0 {
            return Err(refused());
        }
        self.owned.remove(name);
        Ok(())
    }
    pub(super) fn publish(&mut self, temp: &OsStr, final_name: &OsStr) -> Result<()> {
        self.check()?;
        let identity = self.owned.get(temp).cloned().ok_or_else(refused)?;
        if !self.same(temp, &identity)? {
            return Err(refused());
        }
        let from = leaf(temp)?;
        let to = leaf(final_name)?;
        if unsafe {
            libc::linkat(
                self.pin.last().as_raw_fd(),
                from.as_ptr(),
                self.pin.last().as_raw_fd(),
                to.as_ptr(),
                0,
            )
        } != 0
        {
            return Err(refused());
        }
        self.owned.insert(final_name.to_os_string(), identity);
        self.remove_owned(temp)?;
        self.pin.last().sync_all()?;
        Ok(())
    }
    pub(super) fn clean_sidecars(&mut self, db: &OsStr) -> Result<()> {
        for suffix in ["-journal", "-wal", "-shm"] {
            let mut name = db.to_os_string();
            name.push(suffix);
            self.remove_owned(&name)?;
        }
        Ok(())
    }
    pub(super) fn commit(&mut self) -> Result<()> {
        self.check()?;
        self.pin.last().sync_all()?;
        self.committed = true;
        Ok(())
    }
}
impl Drop for ArtifactDir {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        // No prospective-name cleanup, no recursive deletion, no stem scan.
        let names: Vec<_> = self.owned.keys().cloned().collect();
        for name in names {
            let _ = self.remove_owned(&name);
        }
        if self.created {
            let _ = self.pin.remove_empty_last();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    include!("namespace/tests.rs");
}
