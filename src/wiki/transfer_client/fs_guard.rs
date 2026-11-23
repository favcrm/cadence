//! POSIX/`DirFd` filesystem confinement, symlink refusal, private staging, and export anchoring for `wiki_transfer`.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::ffi::CString;
use std::fs::{self, File};
use std::io::{Read, Seek};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::path::{Component, Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::fd_fs::{self, DirFd};

use super::ExportEntry;

pub(super) fn refused_path(path: &Path, why: &str) -> Error {
    Error::rejected(format!(
        "wiki cp refused '{}': {why} — nothing was sent",
        path.display()
    ))
}

pub(super) fn guard_local(path: &Path, _write: bool) -> Result<PathBuf> {
    let abs = lexical_abs(path)?;
    if crate::master::caller_is_master() {
        let cwd = lexical_abs(&std::env::current_dir().map_err(|e| {
            Error::rejected(format!("wiki cp cannot resolve the working directory: {e}"))
        })?)?;
        if !abs.starts_with(&cwd) {
            return Err(Error::rejected(format!(
                "wiki cp refused '{}': outside the registered working directory",
                path.display()
            )));
        }
    }
    Ok(abs)
}

pub(super) fn lexical_abs(path: &Path) -> Result<PathBuf> {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut out = PathBuf::new();
    for component in abs.components() {
        match component {
            Component::RootDir => out.push(component),
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    return Err(Error::rejected(format!(
                        "wiki cp refused '{}'",
                        path.display()
                    )));
                }
            }
            Component::Normal(seg) => out.push(seg),
            Component::Prefix(_) => {
                return Err(Error::rejected(format!(
                    "wiki cp refused '{}'",
                    path.display()
                )))
            }
        }
    }
    Ok(out)
}

pub(super) fn symlink_meta(path: &Path) -> Result<fs::Metadata> {
    fs::symlink_metadata(path)
        .map_err(|e| Error::rejected(format!("wiki cp cannot stat {}: {e}", path.display())))
}

pub(super) fn path_utf8(path: &Path) -> Result<String> {
    path.to_str()
        .map(str::to_string)
        .ok_or_else(|| Error::rejected("wiki cp refused a non-UTF-8 path"))
}

/// Metadata of `path` after every existing ancestor, including the leaf,
/// has been opened with `O_NOFOLLOW`. A missing path is an error. This
/// is the import admission check: nothing is hashed or listed first.
pub(super) fn contained_source_meta(path: &Path) -> Result<fs::Metadata> {
    let file = open_contained(path)?;
    file.metadata()
        .map_err(|e| Error::rejected(format!("wiki cp cannot stat {}: {e}", path.display())))
}

/// Open `path` by walking from the filesystem root with `openat` and
/// `O_NOFOLLOW` on every component. The returned fd is the only handle;
/// a later path open is not used for source bytes.
pub(super) fn open_contained(path: &Path) -> Result<File> {
    refuse_symlink_chain(path)?;
    let mut current = open_root_dir()?;
    let mut cur = PathBuf::new();
    cur.push(Component::RootDir);
    let abs = lexical_abs(path)?;
    let mut leaf: Option<File> = None;
    for component in abs.components() {
        let Component::Normal(seg) = component else {
            if matches!(component, Component::RootDir) {
                continue;
            }
            return Err(refused_path(path, "unsupported path"));
        };
        cur.push(seg);
        let is_last = cur == abs;
        let name = CString::new(os_str_bytes(seg)).map_err(|_| {
            Error::rejected(format!(
                "wiki cp refused '{}': path contains an interior nul — nothing was sent",
                path.display()
            ))
        })?;
        // Ancestors are directories, so they must not use O_NONBLOCK
        // (some kernels return ENOTSUP). The leaf uses O_NONBLOCK so a
        // fifo cannot stall admission; a directory leaf that a kernel
        // refuses that way is retried below. Symlinks stay ELOOP.
        if is_last {
            let opened = match current.open_nofollow_fd(&name) {
                Ok(fd) => File::from(fd),
                Err(err) if err.raw_os_error() == Some(libc::ENOTSUP) => {
                    match current.open_child_dir(&name) {
                        Ok(dir) => dir.into_file(),
                        Err(retry_err) => {
                            if retry_err.raw_os_error() == Some(libc::ELOOP)
                                || is_symlink_at(&current, &name)
                            {
                                return Err(refused_path(&cur, "symlink"));
                            }
                            return Err(Error::rejected(format!(
                                "wiki cp cannot read {}: {retry_err}",
                                cur.display()
                            )));
                        }
                    }
                }
                Err(err) => {
                    // Linux reports a symlink as ELOOP. A kernel that returns the
                    // link descriptor is still refused by fstat below.
                    if err.raw_os_error() == Some(libc::ELOOP) || is_symlink_at(&current, &name) {
                        return Err(refused_path(&cur, "symlink"));
                    }
                    return Err(Error::rejected(format!(
                        "wiki cp cannot read {}: {err}",
                        cur.display()
                    )));
                }
            };
            if is_symlink_fd(opened.as_fd()) {
                return Err(refused_path(&cur, "symlink"));
            }
            leaf = Some(opened);
        } else {
            let child = match current.open_child_dir(&name) {
                Ok(dir) => dir,
                Err(err) => {
                    if err.raw_os_error() == Some(libc::ELOOP) || is_symlink_at(&current, &name) {
                        return Err(refused_path(&cur, "symlink"));
                    }
                    return Err(Error::rejected(format!(
                        "wiki cp cannot read {}: {err}",
                        cur.display()
                    )));
                }
            };
            if is_symlink_dir(&child) {
                return Err(refused_path(&cur, "symlink"));
            }
            current = child;
        }
    }
    let file = leaf.unwrap_or_else(|| current.into_file());
    let meta = file
        .metadata()
        .map_err(|e| Error::rejected(format!("wiki cp cannot stat {}: {e}", path.display())))?;
    if meta.file_type().is_symlink() {
        return Err(refused_path(path, "symlink"));
    }
    if !meta.is_file() && !meta.is_dir() {
        return Err(refused_path(path, "not a regular file or directory"));
    }
    Ok(file)
}

fn open_root_dir() -> Result<DirFd> {
    DirFd::open_root()
        .map_err(|_| Error::rejected("wiki cp cannot open the filesystem root — nothing was sent"))
}

fn os_str_bytes(seg: &std::ffi::OsStr) -> &[u8] {
    use std::os::unix::ffi::OsStrExt;
    seg.as_bytes()
}

fn is_symlink_fd(fd: BorrowedFd<'_>) -> bool {
    fd_fs::fstat(fd).is_ok_and(|st| (st.st_mode & libc::S_IFMT) == libc::S_IFLNK)
}

fn is_symlink_dir(dir: &DirFd) -> bool {
    dir.stat()
        .is_ok_and(|st| (st.st_mode & libc::S_IFMT) == libc::S_IFLNK)
}

fn is_symlink_at(dir: &DirFd, name: &std::ffi::CStr) -> bool {
    dir.fstatat_nofollow(name)
        .is_ok_and(|st| (st.st_mode & libc::S_IFMT) == libc::S_IFLNK)
}

/// Names in a directory fd from [`open_contained`]. `.` and `..` are omitted.
/// The caller's path is not opened again.
pub(super) fn list_nofollow_dir(file: File, path: &Path) -> Result<Vec<std::ffi::OsString>> {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let st = fd_fs::fstat(file.as_fd())
        .map_err(|err| Error::rejected(format!("wiki cp cannot read {}: {err}", path.display())))?;
    let dir = DirFd::from_verified_dir(OwnedFd::from(file), &st).ok_or_else(|| {
        Error::rejected(format!(
            "wiki cp cannot read {}: not a directory",
            path.display()
        ))
    })?;
    let iter = dir
        .read_dir()
        .map_err(|err| Error::rejected(format!("wiki cp cannot read {}: {err}", path.display())))?;
    let mut names = Vec::new();
    for entry in iter {
        let name = entry.map_err(|err| {
            Error::rejected(format!("wiki cp cannot read {}: {err}", path.display()))
        })?;
        names.push(OsStr::from_bytes(name.to_bytes()).to_os_string());
    }
    Ok(names)
}

/// Private export stage in a held directory fd. `Drop` unlinks only the
/// stage name. After `renameat` that name is already gone, so cleanup
/// cannot remove the published file.
pub(super) struct PrivateStage {
    dir: DirFd,
    name: CString,
    pub(super) file: File,
}

impl Drop for PrivateStage {
    fn drop(&mut self) {
        let _ = self.dir.unlink_child(&self.name);
    }
}

impl PrivateStage {
    pub(super) fn create(dir: &DirFd, display: &Path) -> Result<Self> {
        let flags = libc::O_RDWR | libc::O_CREAT | libc::O_EXCL;
        let mut last = None;
        for _ in 0..8 {
            let label = format!(".cadence-transfer-{}", super::hex_encode(stage_nonce()));
            let name = CString::new(label).map_err(|_| {
                Error::rejected(format!(
                    "cannot stage {}: bad stage name",
                    display.display()
                ))
            })?;
            match dir.open_file_nofollow(&name, flags, 0o600) {
                Ok(file) => {
                    let dup = match dir.try_clone() {
                        Ok(dup) => dup,
                        Err(err) => {
                            let _ = dir.unlink_child(&name);
                            drop(file);
                            return Err(Error::rejected(format!(
                                "cannot stage {}: {err}",
                                display.display()
                            )));
                        }
                    };
                    return Ok(Self {
                        dir: dup,
                        name,
                        file,
                    });
                }
                Err(err) if err.raw_os_error() == Some(libc::EEXIST) => {
                    last = Some(err);
                }
                Err(err) => {
                    return Err(Error::rejected(format!(
                        "cannot stage {}: {err}",
                        display.display()
                    )));
                }
            }
        }
        Err(Error::rejected(format!(
            "cannot stage {}: {}",
            display.display(),
            last.map(|err| err.to_string())
                .unwrap_or_else(|| "export stage was not created".to_string())
        )))
    }
}

fn stage_nonce() -> [u8; 16] {
    let mut nonce = [0u8; 16];
    if getrandom::fill(&mut nonce).is_ok() {
        return nonce;
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    let mixed = (u128::from(std::process::id()) << 96) ^ nanos;
    nonce.copy_from_slice(&mixed.to_ne_bytes());
    nonce
}

/// Create-only links the stage under `dest` and leaves an existing name
/// untouched. `--update` renames onto that name. Both operations use the
/// held directory fd, never a reopened path.
pub(super) fn commit_stage(
    stage: &PrivateStage,
    dest: &std::ffi::CStr,
    update: bool,
    display: &Path,
) -> Result<()> {
    let res = if update {
        stage.dir.rename_child(&stage.name, dest)
    } else {
        stage.dir.link_child(&stage.name, dest)
    };
    if let Err(err) = res {
        if !update
            && (err.kind() == std::io::ErrorKind::AlreadyExists
                || err.raw_os_error() == Some(libc::EEXIST))
        {
            return Err(Error::rejected(format!(
                "wiki cp refused to overwrite '{}' — pass --update",
                display.display()
            )));
        }
        return Err(Error::rejected(format!(
            "cannot commit {}: {err}",
            display.display()
        )));
    }
    if !update {
        let _ = stage.dir.unlink_child(&stage.name);
    }
    Ok(())
}

pub(super) fn hash_open_file(file: &mut File) -> Result<(u64, String)> {
    file.rewind()?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];
    let mut size = 0u64;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        size += n as u64;
    }
    Ok((size, super::hex_encode(hasher.finalize())))
}

pub(super) struct ExportAnchor {
    pub(super) dir: DirFd,
    pub(super) root: PathBuf,
    pub(super) single_name: Option<CString>,
}

impl ExportAnchor {
    /// Hold the directory that will contain published names. A single file
    /// whose destination is not already a real directory is published in
    /// its parent; every other plan publishes inside `dest`.
    pub(super) fn open(dest: &Path, scope: &str, entries: &[ExportEntry]) -> Result<Self> {
        let single = entries.len() == 1 && entries[0].path.is_empty();
        if single && !super::is_real_dir(dest) {
            let parent = match dest.parent() {
                Some(parent) if !parent.as_os_str().is_empty() => parent,
                _ => Path::new("/"),
            };
            let leaf = dest
                .file_name()
                .ok_or_else(|| refused_path(dest, "unsupported path"))?;
            return Ok(Self {
                dir: hold_export_dir(parent)?,
                root: parent.to_path_buf(),
                single_name: Some(leaf_name(leaf, dest)?),
            });
        }
        let single_name = if single {
            let base = scope
                .rsplit('/')
                .next()
                .filter(|name| !name.is_empty())
                .unwrap_or("export");
            Some(leaf_name(std::ffi::OsStr::new(base), dest)?)
        } else {
            None
        };
        Ok(Self {
            dir: hold_export_dir(dest)?,
            root: dest.to_path_buf(),
            single_name,
        })
    }
}

/// Walk `path` from `/` with the same `O_NOFOLLOW` directory opens as
/// [`open_contained`]. Missing components are created with `mkdirat` on
/// the held parent. The returned fd, not the path, is the destination.
fn hold_export_dir(path: &Path) -> Result<DirFd> {
    let abs = lexical_abs(path)?;
    let mut current = open_root_dir()?;
    let mut built = PathBuf::new();
    built.push(Component::RootDir);
    for component in abs.components() {
        let Component::Normal(seg) = component else {
            if matches!(component, Component::RootDir) {
                continue;
            }
            return Err(refused_path(path, "unsupported path"));
        };
        let name = component_cstr(seg, path)?;
        built.push(seg);
        current = open_or_create_dir(&current, &name, &built)?;
    }
    Ok(current)
}

pub(super) fn hold_relative_dir(root: &DirFd, rel: &Path) -> Result<DirFd> {
    if rel.as_os_str().is_empty() {
        return root.try_clone().map_err(|err| {
            Error::rejected(format!("wiki cp cannot hold export directory: {err}"))
        });
    }
    let mut current = root
        .try_clone()
        .map_err(|err| Error::rejected(format!("wiki cp cannot hold export directory: {err}")))?;
    let mut built = PathBuf::new();
    for component in rel.components() {
        let Component::Normal(seg) = component else {
            return Err(refused_path(rel, "unsupported path"));
        };
        let bytes = os_str_bytes(seg);
        if bytes.is_empty()
            || bytes == b"."
            || bytes == b".."
            || bytes.starts_with(b".")
            || bytes.contains(&b'/')
            || bytes.contains(&0)
        {
            return Err(refused_path(rel, "unsupported path"));
        }
        let name = component_cstr(seg, rel)?;
        built.push(seg);
        current = open_or_create_dir(&current, &name, &built)?;
    }
    Ok(current)
}

enum DirFail {
    Missing,
    Symlink,
    NotDir,
    Io(std::io::Error),
}

fn open_dir_at(parent: &DirFd, name: &std::ffi::CStr) -> std::result::Result<DirFd, DirFail> {
    let dir = match parent.open_child_dir(name) {
        Ok(dir) => dir,
        Err(err) => {
            if err.raw_os_error() == Some(libc::ELOOP) || is_symlink_at(parent, name) {
                return Err(DirFail::Symlink);
            }
            if err.raw_os_error() == Some(libc::ENOENT) {
                return Err(DirFail::Missing);
            }
            if err.raw_os_error() == Some(libc::ENOTDIR) {
                return Err(DirFail::NotDir);
            }
            return Err(DirFail::Io(err));
        }
    };
    match dir.stat() {
        Ok(st) if (st.st_mode & libc::S_IFMT) == libc::S_IFLNK => Err(DirFail::Symlink),
        Ok(st) if (st.st_mode & libc::S_IFMT) == libc::S_IFDIR => Ok(dir),
        Ok(_) => Err(DirFail::NotDir),
        Err(err) => Err(DirFail::Io(err)),
    }
}

fn open_or_create_dir(parent: &DirFd, name: &std::ffi::CStr, display: &Path) -> Result<DirFd> {
    match open_dir_at(parent, name) {
        Ok(dir) => Ok(dir),
        Err(DirFail::Missing) => {
            let created = match parent.mkdir(name, 0o777) {
                Ok(()) => true,
                Err(err) if err.raw_os_error() == Some(libc::EEXIST) => false,
                Err(err) => {
                    return Err(Error::rejected(format!(
                        "wiki cp cannot create {}: {err}",
                        display.display()
                    )));
                }
            };
            let child = match open_dir_at(parent, name) {
                Ok(dir) => dir,
                Err(DirFail::Symlink) => return Err(refused_path(display, "symlink")),
                Err(DirFail::NotDir) => return Err(refused_path(display, "not a directory")),
                Err(DirFail::Missing) => {
                    return Err(Error::rejected(format!(
                        "wiki cp cannot create {}",
                        display.display()
                    )))
                }
                Err(DirFail::Io(err)) => {
                    return Err(Error::rejected(format!(
                        "wiki cp cannot create {}: {err}",
                        display.display()
                    )))
                }
            };
            if created {
                parent.sync_all().map_err(|err| {
                    Error::rejected(format!("wiki cp cannot sync {}: {err}", display.display()))
                })?;
            }
            Ok(child)
        }
        Err(DirFail::Symlink) => Err(refused_path(display, "symlink")),
        Err(DirFail::NotDir) => Err(refused_path(display, "not a directory")),
        Err(DirFail::Io(err)) => Err(Error::rejected(format!(
            "wiki cp cannot open {}: {err}",
            display.display()
        ))),
    }
}

fn component_cstr(seg: &std::ffi::OsStr, display: &Path) -> Result<CString> {
    CString::new(os_str_bytes(seg)).map_err(|_| {
        Error::rejected(format!(
            "wiki cp refused '{}': path contains an interior nul — nothing was sent",
            display.display()
        ))
    })
}

pub(super) fn leaf_name(seg: &std::ffi::OsStr, display: &Path) -> Result<CString> {
    let bytes = os_str_bytes(seg);
    if bytes.is_empty() || bytes == b"." || bytes == b".." || bytes.contains(&0) {
        return Err(refused_path(display, "unsupported path"));
    }
    component_cstr(seg, display)
}

pub(super) fn refuse_export_leaf(
    dir: &DirFd,
    name: &std::ffi::CStr,
    display: &Path,
    update: bool,
) -> Result<()> {
    let st = match dir.fstatat_nofollow(name) {
        Ok(st) => st,
        Err(err) if err.raw_os_error() == Some(libc::ENOENT) => return Ok(()),
        Err(err) => {
            return Err(Error::rejected(format!(
                "wiki cp cannot stat {}: {err}",
                display.display()
            )));
        }
    };
    let kind = st.st_mode & libc::S_IFMT;
    if kind == libc::S_IFLNK {
        return Err(refused_path(display, "symlink"));
    }
    if kind != libc::S_IFREG {
        return Err(refused_path(display, "not a regular file"));
    }
    if !update {
        return Err(Error::rejected(format!(
            "wiki cp refused to overwrite '{}' — pass --update",
            display.display()
        )));
    }
    Ok(())
}

fn refuse_symlink_chain(path: &Path) -> Result<()> {
    let mut cur = PathBuf::new();
    if path.is_absolute() {
        cur.push(Component::RootDir);
    }
    let parent = path.parent().unwrap_or(path);
    for component in parent.components() {
        match component {
            Component::RootDir => {}
            Component::CurDir => continue,
            Component::ParentDir => {
                return Err(refused_path(path, "path escapes its parent"));
            }
            Component::Normal(_) => cur.push(component),
            Component::Prefix(_) => return Err(refused_path(path, "unsupported path")),
        }
        if cur.as_os_str().is_empty() {
            continue;
        }
        if let Ok(meta) = fs::symlink_metadata(&cur) {
            if meta.file_type().is_symlink() {
                return Err(refused_path(&cur, "symlink"));
            }
        }
    }
    Ok(())
}
