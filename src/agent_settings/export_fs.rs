//! Descriptor-anchored `.pi/agents` and `.claude/agents` export filesystem helpers.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::ffi::CString;
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use super::GEN_MARKER;
use crate::error::{Error, Result};
use crate::fd_fs::{self, DirFd};

pub(super) enum ExportRead {
    Missing,
    Keep,
    Body(String),
}

pub(super) fn safe_export_id(id: &str) -> bool {
    !id.is_empty()
        && id != "."
        && id != ".."
        && !id.contains('/')
        && !id.contains('\\')
        && !id.contains('\0')
}

fn export_component(name: &str) -> Result<CString> {
    CString::new(name).map_err(|_| Error::rejected("export path contains an interior nul"))
}

pub(super) fn open_export_root(pm_dir: &Path) -> Result<Option<DirFd>> {
    match DirFd::open_dir(pm_dir) {
        Ok(dir) => Ok(Some(dir)),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
        Err(err) if err.kind() == ErrorKind::InvalidInput => {
            Err(Error::rejected("PM directory contains an interior nul"))
        }
        Err(err) => Err(Error::from(err)),
    }
}

fn kind_at(dir: &DirFd, name: &str) -> Result<AtExport> {
    let c_name = export_component(name)?;
    let st = match dir.fstatat_nofollow(&c_name) {
        Ok(st) => st,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(AtExport::Missing),
        Err(err) => return Err(Error::from(err)),
    };
    Ok(match st.st_mode & libc::S_IFMT {
        mode if mode == libc::S_IFLNK => AtExport::Symlink,
        mode if mode == libc::S_IFDIR => AtExport::Dir,
        _ => AtExport::Other,
    })
}

enum AtExport {
    Missing,
    Symlink,
    Dir,
    Other,
}

fn open_export_dir(dir: &DirFd, name: &str) -> Result<DirFd> {
    let c_name = export_component(name)?;
    dir.open_child_dir(&c_name)
        .map_err(|_| Error::rejected(format!("{name} is a symlink or not a directory")))
}

fn mkdir_export(dir: &DirFd, name: &str) -> Result<()> {
    let c_name = export_component(name)?;
    match dir.mkdir(&c_name, 0o755) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == ErrorKind::AlreadyExists => Ok(()),
        Err(err) => Err(Error::from(err)),
    }
}

fn open_or_make(parent: &DirFd, name: &str, create: bool) -> Result<Option<DirFd>> {
    match kind_at(parent, name)? {
        AtExport::Missing if !create => Ok(None),
        AtExport::Missing => {
            mkdir_export(parent, name)?;
            Ok(Some(open_export_dir(parent, name)?))
        }
        AtExport::Dir => Ok(Some(open_export_dir(parent, name)?)),
        AtExport::Symlink | AtExport::Other => Err(Error::rejected(format!(
            "{name} is a symlink or not a directory; refusing to follow it"
        ))),
    }
}

pub(super) fn walk_export_dir(root: &DirFd, harness: &str, create: bool) -> Result<Option<DirFd>> {
    let top = if harness == "claude" {
        ".claude"
    } else {
        ".pi"
    };
    let Some(first) = open_or_make(root, top, create)? else {
        return Ok(None);
    };
    open_or_make(&first, "agents", create)
}

fn read_regular_at(dir: &DirFd, name: &str) -> Result<Option<String>> {
    let c_name = export_component(name)?;
    let mut file = match dir.open_readonly_nofollow(&c_name) {
        Ok(file) => file,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
        Err(_) => {
            return Err(Error::rejected(format!(
                "{name} is a symlink or cannot be opened"
            )));
        }
    };
    let meta = file.metadata()?;
    if !meta.file_type().is_file() {
        return Err(Error::rejected(format!("{name} is not a regular file")));
    }
    let mut text = String::new();
    Read::take(&mut file, 1024 * 1024).read_to_string(&mut text)?;
    Ok(Some(text))
}

pub(super) fn read_export(pm_dir: &Path, harness: &str, id: &str) -> ExportRead {
    if !safe_export_id(id) {
        return ExportRead::Keep;
    }
    let Ok(Some(root)) = open_export_root(pm_dir) else {
        return ExportRead::Missing;
    };
    let dir = match walk_export_dir(&root, harness, false) {
        Ok(Some(dir)) => dir,
        Ok(None) => return ExportRead::Missing,
        Err(_) => return ExportRead::Keep,
    };
    match read_regular_at(&dir, &format!("{id}.md")) {
        Ok(None) => ExportRead::Missing,
        Ok(Some(text)) if text.starts_with(&format!("---\n# {GEN_MARKER} ")) => {
            ExportRead::Body(text)
        }
        Ok(Some(_)) | Err(_) => ExportRead::Keep,
    }
}

pub(super) fn export_sync_unknown(error: Error) -> Error {
    if error.kind() == "unknown" {
        return error;
    }
    Error::unknown(format!(
        "profile sync may have changed agent exports; inspect `cadence settings agents show` \
         in the same workspace before repeating: {error}"
    ))
}

pub(super) fn write_export_at(dir: &DirFd, name: &str, text: &str) -> Result<()> {
    let cname = export_component(name)?;
    match dir.open_file_nofollow(&cname, libc::O_RDWR | libc::O_NONBLOCK, 0) {
        Ok(mut file) => {
            let metadata = file.metadata()?;
            if !metadata.is_file() || metadata.nlink() != 1 {
                return Err(Error::rejected("export is not a private regular file"));
            }
            if fd_fs::flock_exclusive_nonblocking(&file).is_err() {
                return Err(Error::rejected("export is busy; retry sync"));
            }
            let mut previous = String::new();
            Read::take(&mut file, 1024 * 1024).read_to_string(&mut previous)?;
            if !previous.starts_with(&format!("---\n# {GEN_MARKER} ")) {
                return Err(Error::rejected(format!(
                    "{name} is unmanaged; refusing to write it"
                )));
            }
            // Write the validated inode, never a replacement at `name`. Preserve
            // the ownership header through partial writes; truncate only afterward.
            file.seek(SeekFrom::Start(0))?;
            return (|| -> Result<()> {
                file.write_all(text.as_bytes())?;
                file.set_len(text.len() as u64)?;
                file.sync_all()?;
                let current = dir
                    .open_readonly_nofollow(&cname)
                    .map_err(|_| Error::rejected("export destination changed during sync"))?
                    .metadata()?;
                if current.dev() != metadata.dev() || current.ino() != metadata.ino() {
                    return Err(Error::rejected("export destination changed during sync"));
                }
                Ok(())
            })()
            .map_err(export_sync_unknown);
        }
        Err(err) if err.kind() == ErrorKind::NotFound => {}
        Err(_) => return Err(Error::rejected("export is a symlink or cannot be opened")),
    }
    let tmp = format!(
        ".{}.{}.tmp",
        name.trim_end_matches(".md"),
        uuid::Uuid::new_v4().simple()
    );
    let ctmp = export_component(&tmp)?;
    let mut file = dir
        .open_file_nofollow(&ctmp, libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL, 0o644)
        .map_err(|_| Error::rejected("export temp could not be created exclusively"))?;
    let written = file
        .write_all(text.as_bytes())
        .and_then(|_| file.sync_all());
    drop(file);
    if written.is_err() {
        let _ = dir.unlink_child(&ctmp);
        return Err(Error::rejected("export write failed"));
    }
    // Publish only if the destination is still absent; never replace an entry
    // another writer created after the absence check.
    let linked = dir.link_child(&ctmp, &cname);
    let _ = dir.unlink_child(&ctmp);
    if linked.is_err() {
        return Err(Error::rejected("export destination appeared during sync"));
    }
    if dir.sync_all().is_err() {
        return Err(export_sync_unknown(Error::rejected(
            "export directory sync failed",
        )));
    }
    Ok(())
}
