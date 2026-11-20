//! Descriptor-relative reads: a swapped ancestor cannot redirect the catalog.
#![forbid(unsafe_code)]

use std::ffi::CString;
use std::io::{Read, Write};
use std::os::unix::{
    ffi::OsStrExt,
    fs::{MetadataExt, PermissionsExt},
};
use std::path::{Component, Path};

use crate::error::{Error, Result};
use crate::fd_fs::DirFd;

pub(in crate::issue) struct Root(DirFd);

fn name(bytes: &[u8]) -> std::io::Result<CString> {
    CString::new(bytes).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "catalog path contains a NUL",
        )
    })
}

impl Root {
    pub(in crate::issue) fn open(path: &Path) -> Result<Self> {
        let mut dir = if path.is_absolute() {
            DirFd::open_root()?
        } else {
            DirFd::open_nofollow(Path::new("."))?
        };
        for part in path.components() {
            match part {
                Component::Normal(n) => dir = dir.open_child_dir(&name(n.as_bytes())?)?,
                Component::RootDir | Component::CurDir => {}
                _ => return Err(Error::rejected("catalog root contains an unsafe component")),
            }
        }
        Ok(Self(dir))
    }

    pub(super) fn dir(&self, path: &Path) -> std::io::Result<DirFd> {
        // Reopen the pinned dot so repeated inventory scans cannot share a
        // prior readdir cursor.
        let mut dir = self.0.open_child_dir(c".")?;
        for part in path.components() {
            let Component::Normal(n) = part else {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "catalog path must stay below its workspace",
                ));
            };
            dir = dir.open_child_dir(&name(n.as_bytes())?)?;
        }
        Ok(dir)
    }

    fn parent(&self, path: &Path) -> std::io::Result<(DirFd, CString)> {
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
        match parent.fstatat_nofollow(&leaf) {
            Ok(stat) => Ok(Some(stat.st_mode & libc::S_IFMT)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    pub(in crate::issue) fn read(&self, path: &Path, cap: u64) -> Result<Option<String>> {
        let (parent, leaf) = match self.parent(path) {
            Ok(p) => p,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let file = match parent.open_readonly_nofollow(&leaf) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.nlink() > 1 {
            return Err(Error::rejected(
                "catalog requires a bounded regular file without hard links",
            ));
        }
        if metadata.len() > cap {
            return Err(Error::rejected(format!(
                "catalog file {path:?} exceeds its {cap}-byte limit"
            )));
        }
        if metadata.nlink() == 0 {
            return Err(Error::invalid(
                super::CATALOG_NOT_CURRENT,
                "catalog file changed during read",
            ));
        }
        let mut bytes = Vec::new();
        file.take(cap + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > cap {
            return Err(Error::rejected(format!(
                "catalog file {path:?} exceeds its {cap}-byte limit"
            )));
        }
        String::from_utf8(bytes)
            .map(Some)
            .map_err(|_| Error::rejected("catalog files must be UTF-8"))
    }

    pub(super) fn list(&self, path: &Path, budget: &mut usize) -> Result<Vec<String>> {
        let dir = self.dir(path)?;
        let mut names = Vec::new();
        for entry in dir.read_dir()? {
            let entry = entry?;
            *budget = budget
                .checked_sub(1)
                .ok_or_else(|| Error::rejected("catalog global inventory limit exceeded"))?;
            names.push(
                std::str::from_utf8(entry.as_bytes())
                    .map_err(|_| Error::rejected("catalog names must be UTF-8"))?
                    .to_string(),
            );
        }
        names.sort();
        Ok(names)
    }

    pub(super) fn mkdir(&self, path: &Path) -> Result<()> {
        let (parent, leaf) = self.parent(path)?;
        if let Err(error) = parent.mkdir(&leaf, 0o700) {
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
        let mode = match parent.open_readonly_nofollow(&leaf) {
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
        let mut file = parent.open_file_nofollow(
            &temp,
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
            0o600,
        )?;
        let result = (|| {
            file.write_all(text.as_bytes())?;
            file.set_permissions(std::fs::Permissions::from_mode(mode))?;
            file.sync_all()?;
            parent.rename_child(&temp, &leaf)?;
            parent.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            let _ = parent.unlink_child(&temp);
        }
        result
    }

    pub(super) fn remove(&self, path: &Path) -> Result<()> {
        let (parent, leaf) = self.parent(path)?;
        if let Err(error) = parent.unlink_child(&leaf) {
            if error.kind() != std::io::ErrorKind::NotFound {
                return Err(error.into());
            }
        }
        parent.sync_all()?;
        Ok(())
    }
}
