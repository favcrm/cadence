//! One fixed owner-selected, path-only configuration for daemon and artifacts.
//! This never opens a key or vault database and never grants Connection authority.
use super::encrypted_fs::Directories;
use super::encrypted_sqlite::EncryptedCustodyConfig;
use crate::error::{Error, Result};
use std::ffi::OsStr;
use std::fs::File;
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

pub(crate) const RECORD: &str = "custody-encrypted.json";
const LIMIT: u64 = 4096;
fn refused() -> Error {
    Error::invalid(
        "storage_unavailable",
        "custody path configuration unavailable",
    )
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    key_file: PathBuf,
    #[serde(default)]
    vault_dir: Option<PathBuf>,
}

/// Keep the admitted namespace descriptors and record fd for the caller's
/// entire operation. No key bytes, crypto state, grant or recovery ledger.
pub(crate) struct Config {
    pub(crate) paths: EncryptedCustodyConfig,
    state: Directories,
    key_parent: Directories,
    vault: Option<Directories>,
    record: File,
    state_path: PathBuf,
}
impl Config {
    pub(crate) fn check(&self) -> Result<()> {
        self.state.check().map_err(|_| refused())?;
        self.key_parent.check().map_err(|_| refused())?;
        if let Some(vault) = &self.vault {
            vault.check().map_err(|_| refused())?;
        }
        let current = self
            .state
            .stat(OsStr::new(RECORD))
            .map_err(|_| refused())?
            .ok_or_else(refused)?;
        let pinned = self.record.metadata().map_err(|_| refused())?;
        #[allow(clippy::unnecessary_cast)]
        let same = current.st_dev as u64 == pinned.dev() && current.st_ino as u64 == pinned.ino();
        if !same
            || !pinned.is_file()
            || pinned.uid() != unsafe { libc::geteuid() }
            || pinned.mode() & 0o7777 != 0o600
            || pinned.nlink() != 1
            || pinned.len() > LIMIT
        {
            return Err(refused());
        }
        Ok(())
    }

    /// Compare the actual artifact tree, not all ancestors of the key.
    /// A reusable key-parent root refuses; a fresh disjoint sibling bundle
    /// can pass. The caller must separately reserve/publish exclusively.
    pub(crate) fn refuse_output(&self, output: &Path) -> Result<()> {
        self.check()?;
        let output = resolve_output(output)?;
        for protected in [
            self.paths.key_file.as_path(),
            self.paths.vault_dir.as_path(),
            self.state_path.join(RECORD).as_path(),
        ] {
            if output.starts_with(protected) || protected.starts_with(&output) {
                return Err(Error::invalid(
                    "invalid_store",
                    "artifact output overlaps custody source",
                ));
            }
        }
        Ok(())
    }
}

fn absolute(path: &Path) -> Result<PathBuf> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().map_err(|_| refused())?.join(path)
    };
    if path
        .components()
        .any(|c| !matches!(c, Component::RootDir | Component::Normal(_)))
    {
        return Err(refused());
    }
    Ok(path)
}
fn private_directory(dirs: &Directories) -> Result<()> {
    let meta = dirs.last().metadata().map_err(|_| refused())?;
    if meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o7777 != 0o700 {
        return Err(refused());
    }
    Ok(())
}

/// Missing configuration is allowed only without encrypted storage evidence.
/// Read-only metadata, no migration/writer, credential use or vault inspection.
fn missing_is_legacy(state: &Directories, state_path: &Path) -> Result<bool> {
    if state
        .stat(OsStr::new("vault"))
        .map_err(|_| refused())?
        .is_some()
    {
        return Ok(false);
    }
    let Some(meta) = state
        .stat(OsStr::new("cadence.sqlite3"))
        .map_err(|_| refused())?
    else {
        return Ok(true);
    };
    if meta.st_mode & libc::S_IFMT != libc::S_IFREG {
        return Err(refused());
    }
    let core =
        crate::store::open_read_only(&state_path.join("cadence.sqlite3")).map_err(|_| refused())?;
    // Pre-Connections stores have no credential references to classify. Leave
    // their schema validation/migration to the existing core entrypoints.
    let has_credentials: bool = core
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='platform_credentials')",
            [],
            |row| row.get(0),
        )
        .map_err(|_| refused())?;
    if !has_credentials {
        state.check().map_err(|_| refused())?;
        return Ok(true);
    }
    let mut stmt = core
        .prepare("SELECT custody FROM platform_credentials")
        .map_err(|_| refused())?;
    let mut rows = stmt.query([]).map_err(|_| refused())?;
    while let Some(row) = rows.next().map_err(|_| refused())? {
        let tag = match row.get_ref(0).map_err(|_| refused())? {
            rusqlite::types::ValueRef::Text(bytes) if bytes.len() <= 128 => {
                std::str::from_utf8(bytes).map_err(|_| refused())?
            }
            _ => return Err(refused()),
        };
        // Reserve the whole encrypted family, including malformed/future tags.
        // Other opaque references remain untouched: legacy dispatch still
        // refuses unknown backends, and exports discard credential authority.
        if tag.starts_with("encrypted-sqlite") {
            return Ok(false);
        }
    }
    state.check().map_err(|_| refused())?;
    Ok(true)
}

/// `pm_root` supplies an already-known daemon root, never directory discovery.
/// Artifacts have explicit core/app inventory and compare their actual output
/// tree independently; they do not open a tracker to discover credentials.
pub(crate) fn load(state_dir: &Path, pm_root: Option<&Path>) -> Result<Option<Config>> {
    let state_path = absolute(state_dir)?;
    match std::fs::symlink_metadata(&state_path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(refused()),
        Ok(_) => {}
    }
    let (state, _) = Directories::legacy_metadata(&state_path).map_err(|_| refused())?;
    let Some(file) = state
        .private_record(OsStr::new(RECORD))
        .map_err(|_| refused())?
    else {
        return if missing_is_legacy(&state, &state_path)? {
            Ok(None)
        } else {
            Err(refused())
        };
    };
    // A present encrypted config always receives the strict chain policy;
    // legacy-only classification above never becomes crypto admission.
    let (state, _) = Directories::open(&state_path, false).map_err(|_| refused())?;
    private_directory(&state)?;
    if file.metadata().map_err(|_| refused())?.len() > LIMIT {
        return Err(refused());
    }
    let mut bytes = Vec::new();
    (&file)
        .take(LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| refused())?;
    if bytes.len() as u64 > LIMIT {
        return Err(refused());
    }
    let record: Record = serde_json::from_slice(&bytes).map_err(|_| refused())?;
    if !record.key_file.is_absolute() {
        return Err(refused());
    }
    let key_file = absolute(&record.key_file)?;
    let vault_dir = absolute(&record.vault_dir.unwrap_or_else(|| state_path.join("vault")))?;
    if vault_dir != state_path.join("vault") || key_file.starts_with(&state_path)
        || key_file.components().any(|c| matches!(c, Component::Normal(part)
            if matches!(part.to_str(), Some("backups" | "exports" | "app-records" | "workspace-apps")))) {
        return Err(refused());
    }
    let (key_parent, _) =
        Directories::open(key_file.parent().ok_or_else(refused)?, false).map_err(|_| refused())?;
    private_directory(&key_parent)?;
    // Only stat the reserved key name. Missing/short/wrong key bytes do not
    // require unlock and never permit an artifact to occupy this object.
    if let Some(key) = key_parent
        .stat(key_file.file_name().ok_or_else(refused)?)
        .map_err(|_| refused())?
    {
        if key.st_mode & libc::S_IFMT != libc::S_IFREG
            || key.st_uid != unsafe { libc::geteuid() }
            || key.st_nlink != 1
        {
            return Err(refused());
        }
    }
    if let Some(pm) = pm_root {
        let pm = absolute(pm)?;
        // Both chains refuse symlinks, so absolute names identify admitted
        // namespace paths; pin the known PM chain when it exists.
        if pm.exists() {
            let (pm_pin, _) = Directories::open(&pm, false).map_err(|_| refused())?;
            pm_pin.check().map_err(|_| refused())?;
        }
        if key_file.starts_with(pm) {
            return Err(refused());
        }
    }
    let vault = if state
        .stat(OsStr::new("vault"))
        .map_err(|_| refused())?
        .is_some()
    {
        let (vault, _) = Directories::open(&vault_dir, false).map_err(|_| refused())?;
        private_directory(&vault)?;
        Some(vault)
    } else {
        None
    };
    let config = Config {
        paths: EncryptedCustodyConfig {
            key_file,
            vault_dir,
        },
        state,
        key_parent,
        vault,
        record: file,
        state_path,
    };
    config.check()?;
    Ok(Some(config))
}

// Classification only. The subsequent output namespace must remain pinned;
// this alone is not permission to perform pathname replacement or cleanup.
pub(crate) fn resolve_output(path: &Path) -> Result<PathBuf> {
    let absolute = absolute(path)?;
    let mut ancestor = absolute.as_path();
    let mut missing = Vec::new();
    loop {
        match std::fs::symlink_metadata(ancestor) {
            Ok(_) => break,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                missing.push(ancestor.file_name().ok_or_else(refused)?.to_os_string());
                ancestor = ancestor.parent().ok_or_else(refused)?;
            }
            Err(_) => return Err(refused()),
        }
    }
    let mut result = ancestor.canonicalize().map_err(|_| refused())?;
    for part in missing.into_iter().rev() {
        result.push(part);
    }
    Ok(result)
}
