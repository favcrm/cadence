//! Durable, repository-local ownership for cadence-managed checkouts.
//!
//! Issue refs remain authoritative for development lanes; this ledger records
//! setup/release state and identifies review/validation trees. Inventory is
//! read-only. Unknown worktrees are never adopted or deleted implicitly.

use std::fs::{self, OpenOptions};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::issue::finish;
use crate::issue::time;
use crate::worktree::{self, layout};

const LEDGER: &str = "managed-checkouts.json";
const LOCK: &str = "managed-checkouts.lock";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Checkout {
    pub repo: String,
    pub purpose: String,
    pub tool: String,
    pub owner: String,
    pub path: String,
    pub branch: Option<String>,
    /// Exact commit selected at setup/adoption. Development HEADs may advance;
    /// review and validation HEADs remain pinned to this object.
    pub pinned_sha: String,
    #[serde(default)]
    pub base_sha: Option<String>,
    pub state: String,
    pub issue: Option<String>,
    pub retention_reason: Option<String>,
    pub release_reason: Option<String>,
    #[serde(default)]
    pub release_artifacts: Vec<String>,
    #[serde(default)]
    pub rollback_artifacts: Vec<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Ledger {
    schema: u32,
    records: Vec<Checkout>,
}

impl Default for Ledger {
    fn default() -> Self {
        Self {
            schema: 1,
            records: Vec::new(),
        }
    }
}

fn canonical_root(repo: &Path) -> Result<PathBuf> {
    let root = worktree::main_root(repo)?.canonicalize()?;
    Ok(root)
}

fn metadata_dir(root: &Path, create: bool) -> Result<PathBuf> {
    let dir = root.join(".cadence");
    match fs::symlink_metadata(&dir) {
        Ok(meta) if meta.file_type().is_symlink() || !meta.is_dir() => {
            return Err(Error::rejected(format!(
                "{} is not a real .cadence directory — refusing lifecycle metadata",
                dir.display()
            )))
        }
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && create => fs::create_dir_all(&dir)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(dir),
        Err(e) => return Err(e.into()),
    }
    Ok(dir)
}

fn ledger_path(root: &Path, create: bool) -> Result<PathBuf> {
    Ok(metadata_dir(root, create)?.join(LEDGER))
}

/// A managed destination component must be a real directory, or absent so
/// Git creates real directories itself. A symlinked `.cadence/wt` (or any
/// ancestor below the canonical root) would make `git worktree add` create
/// the checkout outside the repository, where activation would then record
/// a second canonical identity.
fn ensure_real_directory(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() || !meta.is_dir() => Err(Error::rejected(
            format!(
                "{} is a symbolic link, not a real worktree directory — refusing to create a managed checkout outside the repository layout",
                path.display()
            ),
        )),
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

fn validate_destination_parent(root: &Path, parent: &Path) -> Result<()> {
    let Ok(suffix) = parent.strip_prefix(root) else {
        return ensure_real_directory(parent);
    };
    let mut current = root.to_path_buf();
    for component in suffix.components() {
        current.push(component);
        ensure_real_directory(&current)?;
    }
    Ok(())
}

/// Preflight for a managed `git worktree add` that does not run `begin`:
/// recovery branches must refuse a destination whose existing parents are
/// symlinks or non-directories before Git creates anything.
pub(crate) fn validate_creation_parent(repo: &Path, path: &Path) -> Result<()> {
    let root = canonical_root(repo)?;
    match path.parent() {
        Some(parent) => validate_destination_parent(&root, parent),
        None => Ok(()),
    }
}

fn read_ledger(root: &Path) -> Result<Ledger> {
    let path = ledger_path(root, false)?;
    let meta = match fs::symlink_metadata(&path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Ledger::default()),
        Err(e) => return Err(e.into()),
    };
    if meta.file_type().is_symlink() || !meta.is_file() {
        return Err(Error::rejected(format!(
            "{} is not a regular lifecycle ledger",
            path.display()
        )));
    }
    let bytes = fs::read(&path)?;
    let ledger: Ledger = serde_json::from_slice(&bytes)
        .map_err(|e| Error::rejected(format!("{} is unreadable: {e}", path.display())))?;
    if ledger.schema != 1 {
        return Err(Error::rejected(format!(
            "{} has unsupported schema {}",
            path.display(),
            ledger.schema
        )));
    }
    Ok(ledger)
}

struct LedgerLock {
    _file: fs::File,
}

impl LedgerLock {
    fn acquire(root: &Path) -> Result<(PathBuf, Self)> {
        let dir = metadata_dir(root, true)?;
        let path = dir.join(LOCK);
        if fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(Error::rejected(format!(
                "{} is a symlink — refusing lifecycle lock",
                path.display()
            )));
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&path)?;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        use std::os::unix::io::AsRawFd;
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok((dir, Self { _file: file }))
    }
}

fn write_ledger(dir: &Path, ledger: &Ledger) -> Result<()> {
    let path = dir.join(LEDGER);
    if fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(Error::rejected(format!(
            "{} is a symlink — refusing lifecycle ledger write",
            path.display()
        )));
    }
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let tmp = dir.join(format!(".{LEDGER}.{}-{nonce}.tmp", std::process::id()));
    let bytes = serde_json::to_vec_pretty(ledger)?;
    let mut created = false;
    let mut write = || -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&tmp)?;
        created = true;
        use std::io::Write;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&tmp, &path)?;
        OpenOptions::new().read(true).open(dir)?.sync_all()?;
        Ok(())
    };
    if let Err(e) = write() {
        if created {
            let _ = fs::remove_file(&tmp);
        }
        return Err(e);
    }
    Ok(())
}

fn path_key(path: &Path) -> String {
    let resolved = path
        .canonicalize()
        .unwrap_or_else(|_| finish::lexical_path(path));
    resolved.to_string_lossy().into_owned()
}

fn record_path_matches(record: &Checkout, key: &str) -> bool {
    path_key(Path::new(&record.path)) == key
}

fn validate_record(root: &Path, record: &Checkout) -> Result<()> {
    if record.repo != root.to_string_lossy().as_ref() {
        return Err(Error::rejected(
            "lifecycle record repo does not match canonical repo",
        ));
    }
    if record.owner.trim().is_empty()
        || record.purpose.trim().is_empty()
        || record.tool.trim().is_empty()
    {
        return Err(Error::rejected(
            "lifecycle record requires explicit purpose, tool and owner",
        ));
    }
    if record.pinned_sha.len() != 40 || !record.pinned_sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(Error::rejected(
            "pinned_sha must be a full 40-hex commit SHA",
        ));
    }
    if let Some(branch) = &record.branch {
        if branch.is_empty() || branch.starts_with('-') {
            return Err(Error::rejected("branch must be a non-option ref name"));
        }
    }
    if !Path::new(&record.path).is_absolute() {
        return Err(Error::rejected("lifecycle checkout path must be absolute"));
    }
    let checkout = path_key(Path::new(&record.path));
    for artifact in record
        .release_artifacts
        .iter()
        .chain(&record.rollback_artifacts)
    {
        let path = Path::new(artifact);
        if !path.is_absolute() {
            return Err(Error::rejected(format!(
                "declared artifact path must be absolute: {artifact}"
            )));
        }
        let name = path.file_name().ok_or_else(|| {
            Error::rejected(format!(
                "declared artifact path has no file name: {artifact}"
            ))
        })?;
        let parent = path
            .parent()
            .ok_or_else(|| {
                Error::rejected(format!("declared artifact path has no parent: {artifact}"))
            })?
            .canonicalize()
            .map_err(|e| {
                Error::rejected(format!(
                    "declared artifact parent is not resolvable: {artifact}: {e}"
                ))
            })?;
        let resolved = parent.join(name);
        let artifact_key = match path.canonicalize() {
            Ok(path) => path,
            Err(_)
                if fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink()) =>
            {
                return Err(Error::rejected(format!(
                    "declared artifact symlink cannot be resolved: {artifact}"
                )));
            }
            Err(_) => resolved,
        };
        if artifact_key.starts_with(&checkout) {
            return Err(Error::rejected(format!(
                "declared artifact {artifact} is inside the disposable checkout"
            )));
        }
    }
    Ok(())
}

fn same_identity(a: &Checkout, b: &Checkout) -> bool {
    a.repo == b.repo
        && a.purpose == b.purpose
        && a.tool == b.tool
        && a.path == b.path
        && a.branch == b.branch
        && a.issue == b.issue
}

fn ensure_released_path_absent(root: &Path, key: &str) -> Result<()> {
    match fs::symlink_metadata(key) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Ok(_) => {
            return Err(Error::rejected(format!(
                "released checkout {key} still exists; refusing a new lifecycle generation"
            )))
        }
        Err(error) => {
            return Err(Error::rejected(format!(
                "cannot verify released checkout {key} is absent: {error}"
            )))
        }
    }
    let listed = finish::git(root, &["worktree", "list", "--porcelain"])?;
    if listed
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .any(|path| path_key(Path::new(path)) == key)
    {
        return Err(Error::rejected(format!(
            "released checkout {key} is still registered by Git; refusing a new lifecycle generation"
        )));
    }
    Ok(())
}

/// Write a pending record before a new checkout is created. A crash after this
/// point is visible as `preparing`, never permission to reclaim the path.
pub fn begin(repo: &Path, mut record: Checkout) -> Result<()> {
    let root = canonical_root(repo)?;
    if let Some(parent) = Path::new(&record.path).parent() {
        validate_destination_parent(&root, parent)?;
    }
    record.repo = root.to_string_lossy().into_owned();
    record.path = path_key(Path::new(&record.path));
    record.state = "preparing".into();
    validate_record(&root, &record)?;
    let (dir, _lock) = LedgerLock::acquire(&root)?;
    let mut ledger = read_ledger(&root)?;
    let matches: Vec<usize> = ledger
        .records
        .iter()
        .enumerate()
        .filter_map(|(index, existing)| {
            record_path_matches(existing, &record.path).then_some(index)
        })
        .collect();
    if matches.len() > 1 {
        return Err(Error::rejected(format!(
            "multiple lifecycle ownership records name {}; refusing setup",
            record.path
        )));
    }
    if let Some(index) = matches.first().copied() {
        let existing = &ledger.records[index];
        if !same_identity(existing, &record) {
            return Err(Error::rejected(format!(
                "{} already has lifecycle ownership metadata for a different repo, purpose, tool or branch",
                record.path
            )));
        }
        if matches!(existing.state.as_str(), "retained" | "releasing") {
            return Err(Error::rejected(format!(
                "{} is recorded as {}; managed setup cannot implicitly reactivate it",
                record.path, existing.state
            )));
        }
        let new_generation = existing.state == "released";
        if new_generation {
            ensure_released_path_absent(&root, &record.path)?;
        }
        let existing = &mut ledger.records[index];
        existing.state = "preparing".into();
        existing.owner = record.owner;
        existing.pinned_sha = record.pinned_sha;
        existing.base_sha = record.base_sha;
        existing.retention_reason = None;
        existing.release_reason = None;
        if new_generation {
            existing.created_at = record.created_at;
            existing.release_artifacts.clear();
            existing.rollback_artifacts.clear();
        }
        existing.updated_at = time::now_epoch();
    } else {
        ledger.records.push(record);
    }
    write_ledger(&dir, &ledger)
}

/// Register a native issue lane or mark a newly-created managed checkout
/// active. An existing path is reusable only when every identity field agrees.
pub fn activate(repo: &Path, mut record: Checkout) -> Result<()> {
    let root = canonical_root(repo)?;
    let requested = Path::new(&record.path);
    if let Some(parent) = requested.parent() {
        validate_destination_parent(&root, parent)?;
    }
    match fs::symlink_metadata(requested) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err(Error::rejected(format!(
                "{} is a symbolic link, not a real checkout — refusing activation outside the repository layout",
                requested.display()
            )))
        }
        Ok(meta) if !meta.is_dir() => {
            return Err(Error::rejected(format!(
                "{} is not a real checkout directory — refusing activation",
                requested.display()
            )))
        }
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    record.repo = root.to_string_lossy().into_owned();
    record.path = path_key(Path::new(&record.path));
    validate_record(&root, &record)?;
    let (dir, _lock) = LedgerLock::acquire(&root)?;
    let mut ledger = read_ledger(&root)?;
    match ledger.records.iter_mut().find(|r| r.path == record.path) {
        Some(existing) if same_identity(existing, &record) => {
            if matches!(
                existing.state.as_str(),
                "retained" | "released" | "releasing"
            ) {
                return Err(Error::rejected(format!(
                    "{} is recorded as {}; refusing implicit activation",
                    record.path, existing.state
                )));
            }
            if existing.pinned_sha != record.pinned_sha
                && matches!(existing.purpose.as_str(), "review" | "validation")
            {
                return Err(Error::rejected(format!(
                    "pinned checkout {} is recorded at {}, not {}",
                    record.path, existing.pinned_sha, record.pinned_sha
                )));
            }
            existing.state = "active".into();
            existing.owner = record.owner;
            if !matches!(existing.purpose.as_str(), "review" | "validation") {
                existing.pinned_sha = record.pinned_sha;
                existing.base_sha = record.base_sha;
            }
            existing.retention_reason = None;
            existing.release_reason = None;
            existing.updated_at = time::now_epoch();
        }
        Some(_) => {
            return Err(Error::rejected(format!(
                "{} is already managed for a different repo, purpose, owner or branch",
                record.path
            )))
        }
        None => {
            record.state = "active".into();
            record.created_at = time::now_epoch();
            record.updated_at = record.created_at;
            ledger.records.push(record);
        }
    }
    write_ledger(&dir, &ledger)
}

/// Update the lifecycle state for a recorded checkout. This records release or
/// retention; it never removes a checkout, branch, artifact or tracker ref.
pub fn transition(repo: &Path, path: &Path, state: &str, reason: Option<&str>) -> Result<()> {
    if !matches!(
        state,
        "active"
            | "preparing"
            | "setup-failed"
            | "releasing"
            | "released"
            | "retained"
            | "cleanup-failed"
    ) {
        return Err(Error::rejected(format!(
            "unsupported checkout state '{state}'"
        )));
    }
    if state == "releasing" {
        return Err(Error::rejected(
            "release state requires a guarded release transaction",
        ));
    }
    if matches!(
        state,
        "setup-failed" | "released" | "retained" | "cleanup-failed"
    ) && reason.is_none_or(|value| value.trim().is_empty())
    {
        return Err(Error::rejected(format!(
            "checkout state '{state}' requires a reason"
        )));
    }
    let root = canonical_root(repo)?;
    let key = path_key(path);
    let (dir, _lock) = LedgerLock::acquire(&root)?;
    let mut ledger = read_ledger(&root)?;
    let matches: Vec<usize> = ledger
        .records
        .iter()
        .enumerate()
        .filter_map(|(index, record)| (record.path == key).then_some(index))
        .collect();
    if matches.len() != 1 {
        return Err(Error::rejected(format!(
            "{} must have exactly one managed checkout record to change state; found {}",
            key,
            matches.len()
        )));
    }
    let record = &mut ledger.records[matches[0]];
    if record.state == "releasing" {
        return Err(Error::rejected(format!(
            "{} is recorded as releasing or release-interrupted; only explicit checkout release with a reason may resolve it",
            key
        )));
    }
    let terminal_change = match record.state.as_str() {
        "retained" => !matches!(state, "retained" | "released"),
        "released" => state != "released",
        _ => false,
    };
    if terminal_change {
        return Err(Error::rejected(format!(
            "{} is recorded as {}; refusing to overwrite its terminal lifecycle state",
            key, record.state
        )));
    }
    record.state = state.to_string();
    record.updated_at = time::now_epoch();
    if matches!(state, "retained") {
        record.retention_reason = reason.map(str::to_string);
    }
    if matches!(state, "released" | "releasing" | "cleanup-failed") {
        record.release_reason = reason.map(str::to_string);
    }
    write_ledger(&dir, &ledger)
}

pub fn release(repo: &Path, path: &Path, reason: &str) -> Result<()> {
    if reason.trim().is_empty() {
        return Err(Error::rejected("checkout release requires a reason"));
    }
    let root = canonical_root(repo)?;
    let key = path_key(path);
    let (dir, _lock) = LedgerLock::acquire(&root)?;
    let mut ledger = read_ledger(&root)?;
    let matches: Vec<usize> = ledger
        .records
        .iter()
        .enumerate()
        .filter_map(|(index, record)| record_path_matches(record, &key).then_some(index))
        .collect();
    if matches.len() != 1 {
        return Err(Error::rejected(format!(
            "{} must have exactly one managed checkout record to release; found {}",
            path.display(),
            matches.len()
        )));
    }
    let record = &mut ledger.records[matches[0]];
    validate_record(&root, record)?;
    if record.path != key {
        return Err(Error::rejected(format!(
            "checkout path {} differs from recorded path {}; refusing moved or noncanonical ownership",
            path.display(),
            record.path
        )));
    }
    if record.state == "released" {
        return Ok(());
    }
    if !matches!(
        record.state.as_str(),
        "active" | "preparing" | "setup-failed" | "releasing" | "retained" | "cleanup-failed"
    ) {
        return Err(Error::rejected(format!(
            "{} has unsupported lifecycle state {}; refusing explicit release",
            path.display(),
            record.state
        )));
    }
    record.state = "released".into();
    record.release_reason = Some(reason.to_string());
    record.updated_at = time::now_epoch();
    write_ledger(&dir, &ledger)
}

/// Explicitly adopt a pre-existing linked checkout. The caller must already
/// supply the repo, path, owner, purpose and pinned SHA; validation proves the
/// checkout belongs to this Git common directory and is at that exact SHA.
pub fn adopt(repo: &Path, mut record: Checkout) -> Result<()> {
    let root = canonical_root(repo)?;
    let (dir, _lock) = LedgerLock::acquire(&root)?;
    let path = Path::new(&record.path)
        .canonicalize()
        .map_err(|e| Error::rejected(format!("cannot resolve checkout {}: {e}", record.path)))?;
    if path == root || worktree::main_root(&path)?.canonicalize()? != root {
        return Err(Error::rejected(format!(
            "{} is not a linked checkout of {}",
            path.display(),
            root.display()
        )));
    }
    let head = finish::git(&path, &["rev-parse", "--verify", "HEAD^{commit}"])?;
    if head != record.pinned_sha {
        return Err(Error::rejected(format!(
            "checkout HEAD {head} does not match the supplied pinned SHA {}",
            record.pinned_sha
        )));
    }
    let branch = finish::git_branch(&path)?;
    if branch != record.branch {
        return Err(Error::rejected(format!(
            "checkout branch {:?} does not match supplied branch {:?}",
            branch, record.branch
        )));
    }
    let listed = finish::git(&root, &["worktree", "list", "--porcelain"])?;
    if !listed
        .lines()
        .any(|line| line == format!("worktree {}", path.display()))
    {
        return Err(Error::rejected(
            "checkout is not registered by git worktree list",
        ));
    }
    record.repo = root.to_string_lossy().into_owned();
    record.path = path.to_string_lossy().into_owned();
    record.state = "active".into();
    record.created_at = time::now_epoch();
    record.updated_at = record.created_at;
    validate_record(&root, &record)?;
    let mut ledger = read_ledger(&root)?;
    if ledger
        .records
        .iter()
        .any(|existing| record_path_matches(existing, &record.path))
    {
        return Err(Error::rejected(format!(
            "{} already has a lifecycle record",
            record.path
        )));
    }
    ledger.records.push(record);
    write_ledger(&dir, &ledger)
}

/// Explicitly reactivate an existing released checkout without changing Git or
/// tracker state. The ledger lock binds validation and the active transition.
pub fn resume(repo: &Path, path: &Path, pinned_sha: &str, reason: &str) -> Result<()> {
    if reason.trim().is_empty() {
        return Err(Error::rejected("checkout resume requires a reason"));
    }
    if pinned_sha.len() != 40 || !pinned_sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(Error::rejected(
            "pinned_sha must be a full 40-hex commit SHA",
        ));
    }

    let root = canonical_root(repo)?;
    let key = path_key(path);
    let (dir, _lock) = LedgerLock::acquire(&root)?;
    let mut ledger = read_ledger(&root)?;
    let matches: Vec<usize> = ledger
        .records
        .iter()
        .enumerate()
        .filter_map(|(index, record)| record_path_matches(record, &key).then_some(index))
        .collect();
    if matches.len() != 1 {
        return Err(Error::rejected(format!(
            "{} must have exactly one managed lifecycle record to resume; found {}",
            path.display(),
            matches.len()
        )));
    }
    let index = matches[0];
    let record = &ledger.records[index];
    validate_record(&root, record)?;
    if record.path != key {
        return Err(Error::rejected(format!(
            "{} does not match the canonical recorded checkout path",
            path.display()
        )));
    }
    match record.state.as_str() {
        "released" => {}
        "retained" => {
            return Err(Error::rejected(format!(
                "{} is retained; explicitly release it before resuming",
                path.display()
            )))
        }
        "releasing" => {
            return Err(Error::rejected(format!(
                "{} has an interrupted release; inspect it and explicitly release it with a reason before resuming",
                path.display()
            )))
        }
        state => {
            return Err(Error::rejected(format!(
                "{} is recorded as {state}; only a released checkout can be resumed",
                path.display()
            )))
        }
    }
    if record
        .release_reason
        .as_deref()
        .is_none_or(|release_reason| release_reason.trim().is_empty())
    {
        return Err(Error::rejected(format!(
            "{} has no explicit release reason; refusing resume",
            path.display()
        )));
    }
    if !matches!(
        record.purpose.as_str(),
        "development" | "review" | "validation" | "agent" | "staging"
    ) {
        return Err(Error::rejected(format!(
            "{} has unsupported lifecycle purpose {}; refusing resume",
            path.display(),
            record.purpose
        )));
    }

    if !path.is_absolute() {
        return Err(Error::rejected(format!(
            "{} is not an absolute canonical checkout path",
            path.display()
        )));
    }
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        Error::rejected(format!(
            "cannot inspect released checkout {}: {error}",
            path.display()
        ))
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(Error::rejected(format!(
            "{} is not a real checkout directory",
            path.display()
        )));
    }
    let canonical_path = path.canonicalize().map_err(|error| {
        Error::rejected(format!(
            "cannot resolve released checkout {}: {error}",
            path.display()
        ))
    })?;
    let canonical_key = canonical_path.to_str().ok_or_else(|| {
        Error::rejected(format!(
            "{} cannot be represented as a canonical lifecycle path",
            path.display()
        ))
    })?;
    if path.as_os_str() != canonical_path.as_os_str() || record.path != canonical_key {
        return Err(Error::rejected(format!(
            "{} is not the exact canonical recorded checkout path",
            path.display()
        )));
    }
    if canonical_path == root || worktree::main_root(&canonical_path)?.canonicalize()? != root {
        return Err(Error::rejected(format!(
            "{} is not a linked checkout of {}",
            canonical_path.display(),
            root.display()
        )));
    }

    let branch = finish::git_branch(&canonical_path)?;
    if branch != record.branch {
        return Err(Error::rejected(format!(
            "checkout branch {:?} does not match recorded branch {:?}",
            branch, record.branch
        )));
    }
    let head = finish::git(&canonical_path, &["rev-parse", "--verify", "HEAD^{commit}"])?;
    if head != pinned_sha {
        return Err(Error::rejected(format!(
            "checkout HEAD {head} does not match the supplied pinned SHA {pinned_sha}"
        )));
    }
    if record.purpose != "development" && head != record.pinned_sha {
        return Err(Error::rejected(format!(
            "{} is pinned at {}, not {head}",
            record.purpose, record.pinned_sha
        )));
    }
    let trees = live_trees(&root)?;
    let registered: Vec<&LiveTree> = trees
        .iter()
        .filter(|tree| tree.path.as_os_str() == canonical_path.as_os_str())
        .collect();
    if registered.len() != 1 {
        return Err(Error::rejected(format!(
            "{} is not registered exactly once at its canonical Git worktree path",
            canonical_path.display()
        )));
    }
    if registered[0].head != head || registered[0].branch != branch {
        return Err(Error::rejected(format!(
            "{} Git worktree registration does not match its current HEAD and branch",
            canonical_path.display()
        )));
    }

    let record = &mut ledger.records[index];
    record.state = "active".into();
    if record.purpose == "development" {
        record.pinned_sha = head;
    }
    record.retention_reason = None;
    record.release_reason = Some(reason.to_string());
    record.updated_at = time::now_epoch();
    validate_record(&root, record)?;
    write_ledger(&dir, &ledger)
}

#[derive(Clone)]
struct LiveTree {
    path: PathBuf,
    head: String,
    branch: Option<String>,
}

fn live_trees(root: &Path) -> Result<Vec<LiveTree>> {
    let text = finish::git(root, &["worktree", "list", "--porcelain"])?;
    let mut trees = Vec::new();
    let mut path: Option<PathBuf> = None;
    let mut head: Option<String> = None;
    let mut branch: Option<String> = None;
    let push = |trees: &mut Vec<LiveTree>,
                path: &mut Option<PathBuf>,
                head: &mut Option<String>,
                branch: &mut Option<String>| {
        if let (Some(path), Some(head)) = (path.take(), head.take()) {
            trees.push(LiveTree {
                path,
                head,
                branch: branch.take(),
            });
        }
        *branch = None;
    };
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("worktree ") {
            push(&mut trees, &mut path, &mut head, &mut branch);
            path = Some(PathBuf::from(value));
        } else if let Some(value) = line.strip_prefix("HEAD ") {
            head = Some(value.to_string());
        } else if let Some(value) = line.strip_prefix("branch refs/heads/") {
            branch = Some(value.to_string());
        }
    }
    push(&mut trees, &mut path, &mut head, &mut branch);
    Ok(trees)
}

fn dirty(path: &Path) -> Result<bool> {
    let out = finish::git(path, &["status", "--porcelain", "--untracked-files=all"])?;
    Ok(!out.is_empty())
}

fn bytes(path: &Path) -> Value {
    match crate::doctor::host::dir_size(path) {
        (n, false) => json!(n),
        _ => Value::Null,
    }
}

fn branch_exists(root: &Path, branch: Option<&str>) -> bool {
    branch.is_some_and(|b| {
        finish::git(
            root,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/heads/{b}"),
            ],
        )
        .is_ok()
    })
}

fn record_row(root: &Path, record: &Checkout, trees: &[LiveTree]) -> Value {
    let expected = PathBuf::from(&record.path);
    let expected_key = path_key(&expected);
    let here = trees
        .iter()
        .find(|tree| path_key(&tree.path) == expected_key);
    let mut moved = record.branch.as_deref().and_then(|branch| {
        trees
            .iter()
            .find(|tree| tree.branch.as_deref() == Some(branch))
    });
    let mut ambiguous_detached_move = false;
    if moved.is_none() && record.branch.is_none() {
        let candidates: Vec<_> = trees
            .iter()
            .filter(|tree| tree.branch.is_none() && tree.head == record.pinned_sha)
            .collect();
        if candidates.len() == 1 {
            moved = candidates.first().copied();
        } else if candidates.len() > 1 {
            ambiguous_detached_move = true;
        }
    }
    let mut status = record.state.clone();
    let mut reason = match record.state.as_str() {
        "preparing" => "interrupted-setup",
        "setup-failed" => "setup-failed-recoverable",
        "released" => "released-awaiting-explicit-cleanup",
        "retained" => "explicitly-retained",
        "releasing" => "release-interrupted",
        "cleanup-failed" => "cleanup-failed-recoverable",
        _ => "managed-active",
    };
    let mut actual_path = here.map(|tree| tree.path.clone());
    let mut actual_head = here.map(|tree| tree.head.clone());
    let mut dirty_state = None;
    if here.is_none() {
        if let Some(moved) = moved {
            status = "moved".into();
            reason = "branch-checked-out-at-different-path-preserve-commits";
            actual_path = Some(moved.path.clone());
            actual_head = Some(moved.head.clone());
        } else if ambiguous_detached_move {
            status = "unknown".into();
            reason = "detached-checkout-move-ambiguous-preserve-all-candidates";
        } else if matches!(record.state.as_str(), "preparing" | "setup-failed") {
            status = record.state.clone();
            reason = if branch_exists(root, record.branch.as_deref()) {
                "interrupted-setup-checkout-missing-branch-survives-preserve-branch"
            } else {
                "interrupted-setup-checkout-absent-retryable"
            };
        } else if matches!(record.purpose.as_str(), "review" | "validation")
            && matches!(record.state.as_str(), "releasing" | "cleanup-failed")
        {
            status = "unknown".into();
            reason = "tool-release-interrupted-checkout-missing-verify-registration";
        } else if record.state == "released"
            && matches!(record.purpose.as_str(), "review" | "validation")
        {
            status = "released".into();
            reason = "released-tool-checkout-absent";
        } else {
            status = "missing".into();
            reason = if branch_exists(root, record.branch.as_deref()) {
                "checkout-missing-branch-survives-preserve-branch"
            } else {
                "checkout-and-branch-missing-close-refs-only"
            };
        }
    } else if let Some(tree) = here {
        if worktree::main_root(&tree.path)
            .ok()
            .and_then(|p| p.canonicalize().ok())
            .as_deref()
            != Some(root)
        {
            status = "unknown".into();
            reason = "repo-identity-unknown";
        } else if record
            .branch
            .as_deref()
            .is_some_and(|b| tree.branch.as_deref() != Some(b))
        {
            status = "moved".into();
            reason = "branch-mismatch-preserve-checkout";
        } else if matches!(record.purpose.as_str(), "review" | "validation")
            && tree.head != record.pinned_sha
            && record.base_sha.as_deref() != Some(tree.head.as_str())
        {
            status = "unknown".into();
            reason = "pinned-sha-mismatch-preserve-checkout";
        } else {
            match dirty(&tree.path) {
                Ok(true) => {
                    dirty_state = Some(true);
                    let base = record
                        .base_sha
                        .as_deref()
                        .unwrap_or(record.pinned_sha.as_str());
                    let started = tree.head.as_str() != base;
                    let merged = started
                        && record
                            .branch
                            .as_deref()
                            .and_then(|branch| {
                                finish::default_ref(root).and_then(|into| {
                                    finish::merge_rule(root, branch, &tree.head, &into)
                                })
                            })
                            .is_some();
                    if merged {
                        status = "dirty-merged".into();
                        reason = "merged-tip-but-dirty-retain-checkout-and-branch";
                    } else {
                        status = "dirty".into();
                        reason = "dirty-retain-checkout-and-branch";
                    }
                }
                Ok(false) => dirty_state = Some(false),
                Err(_) => {
                    status = "unknown".into();
                    reason = "dirty-probe-failed-refuse-cleanup";
                }
            }
        }
    }
    let size = actual_path
        .as_deref()
        .filter(|p| p.is_dir())
        .map(bytes)
        .unwrap_or(Value::Null);
    let reclaimable = if status == "missing" {
        json!(0)
    } else {
        Value::Null
    };
    json!({
        "status": status,
        "lifecycle_state": record.state,
        "reason_code": reason,
        "repo": record.repo,
        "purpose": record.purpose,
        "tool": record.tool,
        "owner": record.owner,
        "issue": record.issue,
        "path": record.path,
        "actual_path": actual_path.map(|p| p.to_string_lossy().into_owned()),
        "branch": record.branch,
        "pinned_sha": record.pinned_sha,
        "base_sha": record.base_sha,
        "actual_sha": actual_head,
        "dirty": dirty_state,
        "bytes_estimate": size,
        "reclaimable_bytes": reclaimable,
        "retention_reason": record.retention_reason,
        "release_reason": record.release_reason,
        "retained_artifacts": record.release_artifacts,
        "rollback_artifacts": record.rollback_artifacts,
        "created_at": record.created_at,
        "updated_at": record.updated_at,
    })
}

/// Read-only inventory for one repository. It performs no deletion, tracker
/// mutation, lifecycle transition or notification. New checkout deletion is
/// explicitly report-only; an unknown/foreign tree has no reclaim estimate.
pub fn inventory(repo: &Path) -> Result<Value> {
    let root = canonical_root(repo)?;
    let ledger = read_ledger(&root)?;
    let trees = live_trees(&root)?;
    let mut rows = Vec::new();
    let mut known = std::collections::HashSet::new();
    for record in &ledger.records {
        known.insert(path_key(Path::new(&record.path)));
        rows.push(record_row(&root, record, &trees));
    }
    for tree in trees
        .iter()
        .filter(|tree| path_key(&tree.path) != path_key(&root))
    {
        let key = path_key(&tree.path);
        if known.contains(&key) {
            continue;
        }
        let (status, reason, dirty_state) = match dirty(&tree.path) {
            Ok(true) => ("unmanaged", "unmanaged-dirty-inventory-only", Some(true)),
            Ok(false) => (
                "unmanaged",
                "unmanaged-inventory-only-explicit-adoption-required",
                Some(false),
            ),
            Err(_) => (
                "unknown",
                "unmanaged-dirty-probe-failed-inventory-only",
                None,
            ),
        };
        rows.push(json!({
            "status": status,
            "reason_code": reason,
            "repo": root,
            "purpose": "unknown",
            "tool": Value::Null,
            "owner": Value::Null,
            "issue": Value::Null,
            "path": tree.path,
            "actual_path": tree.path,
            "branch": tree.branch,
            "pinned_sha": Value::Null,
            "actual_sha": tree.head,
            "dirty": dirty_state,
            "bytes_estimate": bytes(&tree.path),
            "reclaimable_bytes": Value::Null,
            "retained_artifacts": [],
            "rollback_artifacts": [],
            "created_at": Value::Null,
            "updated_at": Value::Null,
        }));
    }
    let mut counts = serde_json::Map::new();
    let mut lifecycle_counts = serde_json::Map::new();
    for row in &rows {
        let key = row["status"].as_str().unwrap_or("unknown").to_string();
        let n = counts.get(&key).and_then(Value::as_u64).unwrap_or(0) + 1;
        counts.insert(key, json!(n));
        if let Some(state) = row["lifecycle_state"].as_str() {
            let n = lifecycle_counts
                .get(state)
                .and_then(Value::as_u64)
                .unwrap_or(0)
                + 1;
            lifecycle_counts.insert(state.to_string(), json!(n));
        }
    }
    Ok(json!({
        "schema": "cadence.worktree-inventory/1",
        "repo": root,
        "cleanup_mode": "report-only",
        "dry_run": true,
        "counts": counts,
        "lifecycle_counts": lifecycle_counts,
        "resources": rows,
    }))
}

pub struct CheckoutSpec<'a> {
    pub repo: &'a Path,
    pub purpose: &'a str,
    pub tool: &'a str,
    pub owner: &'a str,
    pub path: &'a Path,
    pub branch: Option<&'a str>,
    pub pinned_sha: &'a str,
    pub issue: Option<&'a str>,
}

/// Build a timestamped record for a new managed checkout.
pub fn new_record(spec: CheckoutSpec<'_>) -> Checkout {
    let now = time::now_epoch();
    Checkout {
        repo: spec.repo.to_string_lossy().into_owned(),
        purpose: spec.purpose.to_string(),
        tool: spec.tool.to_string(),
        owner: spec.owner.to_string(),
        path: path_key(spec.path),
        branch: spec.branch.map(str::to_string),
        pinned_sha: spec.pinned_sha.to_string(),
        base_sha: None,
        state: "preparing".into(),
        issue: spec.issue.map(str::to_string),
        retention_reason: None,
        release_reason: None,
        release_artifacts: Vec::new(),
        rollback_artifacts: Vec::new(),
        created_at: now,
        updated_at: now,
    }
}

/// Declare artifacts that are outside the disposable checkout and must be
/// retained with its release/rollback record.
pub fn declare_artifacts(
    repo: &Path,
    path: &Path,
    release: Vec<String>,
    rollback: Vec<String>,
) -> Result<()> {
    let root = canonical_root(repo)?;
    let key = path_key(path);
    let (dir, _lock) = LedgerLock::acquire(&root)?;
    let mut ledger = read_ledger(&root)?;
    let record = ledger
        .records
        .iter_mut()
        .find(|r| r.path == key)
        .ok_or_else(|| Error::rejected(format!("{} has no managed checkout record", key)))?;
    record.release_artifacts = release;
    record.rollback_artifacts = rollback;
    validate_record(&root, record)?;
    record.updated_at = time::now_epoch();
    write_ledger(&dir, &ledger)
}

/// Return the unique ownership record for a checkout path, if one exists.
pub fn managed_record(repo: &Path, path: &Path) -> Result<Option<Checkout>> {
    let root = canonical_root(repo)?;
    let key = path_key(path);
    let records = read_ledger(&root)?.records;
    let mut matches = records
        .into_iter()
        .filter(|record| record_path_matches(record, &key));
    let Some(record) = matches.next() else {
        return Ok(None);
    };
    if matches.next().is_some() {
        return Err(Error::rejected(format!(
            "multiple lifecycle ownership records name {}; refusing cleanup",
            path.display()
        )));
    }
    validate_record(&root, &record)?;
    if record.path != key {
        return Err(Error::rejected(format!(
            "checkout path {} differs from recorded path {}; refusing moved or noncanonical ownership",
            path.display(),
            record.path
        )));
    }
    Ok(Some(record))
}

/// Holds the repository lifecycle lock through destructive cleanup and its
/// final state. A legacy checkout without a record still holds the lock to fence
/// adoption; concurrent retention and release cannot cross the deletion boundary.
pub struct ReleaseGuard {
    root: PathBuf,
    dir: PathBuf,
    key: Option<String>,
    _lock: LedgerLock,
}

impl ReleaseGuard {
    fn finish(&mut self, state: &str, reason: &str) -> Result<()> {
        let Some(key) = self.key.as_deref() else {
            return Ok(());
        };
        if reason.trim().is_empty() {
            return Err(Error::rejected(format!(
                "checkout state '{state}' requires a reason"
            )));
        }
        let mut ledger = read_ledger(&self.root)?;
        let record = ledger
            .records
            .iter_mut()
            .find(|record| record.path == key)
            .ok_or_else(|| Error::rejected(format!("{key} has no managed ownership record")))?;
        if record.state != "releasing" {
            return Err(Error::rejected(format!(
                "{key} changed lifecycle state during release; refusing to record {state}"
            )));
        }
        record.state = state.to_string();
        record.updated_at = time::now_epoch();
        if state == "retained" {
            record.retention_reason = Some(reason.to_string());
        } else {
            record.release_reason = Some(reason.to_string());
        }
        write_ledger(&self.dir, &ledger)
    }

    pub fn released(&mut self, reason: &str) -> Result<()> {
        self.finish("released", reason)
    }

    pub fn retained(&mut self, reason: &str) -> Result<()> {
        self.finish("retained", reason)
    }

    pub fn cleanup_failed(&mut self, reason: &str) -> Result<()> {
        self.finish("cleanup-failed", reason)
    }
}

fn validate_refs_only_finish_target(root: &Path, path: &Path, branch: Option<&str>) -> Result<()> {
    let lexical = finish::lexical_path(path);
    if !path.is_absolute() || path != lexical.as_path() {
        return Err(Error::rejected(format!(
            "refs-only finish path {} is not canonical; refusing closure",
            path.display()
        )));
    }
    let parent = path.parent().ok_or_else(|| {
        Error::rejected(format!(
            "refs-only finish path {} has no parent; refusing closure",
            path.display()
        ))
    })?;
    let canonical_parent = parent.canonicalize().map_err(|error| {
        Error::rejected(format!(
            "cannot verify refs-only finish parent {}: {error}",
            parent.display()
        ))
    })?;
    if canonical_parent != parent {
        return Err(Error::rejected(format!(
            "refs-only finish path {} has a symlinked parent alias; refusing closure",
            path.display()
        )));
    }
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Ok(_) => {
            return Err(Error::rejected(format!(
                "refs-only finish path {} reappeared; refusing closure",
                path.display()
            )))
        }
        Err(error) => {
            return Err(Error::rejected(format!(
                "cannot verify refs-only finish path {} is absent: {error}",
                path.display()
            )))
        }
    }
    if let Some(branch) = branch.filter(|branch| !branch.is_empty()) {
        if let Some(live) = finish::registered_branch_path(root, branch)?
            .filter(|live| !finish::same_path(live, path))
        {
            return Err(Error::rejected(format!(
                "issue-owned branch {branch} is checked out at {}, not {}; refusing refs-only finish",
                live.display(),
                path.display()
            )));
        }
    }
    Ok(())
}

fn begin_release_transaction(
    repo: &Path,
    path: &Path,
    reason: &str,
    refused: String,
    allow_legacy_absence: bool,
    allow_already_released: bool,
    expected: impl FnOnce(Option<&Checkout>) -> Result<bool>,
) -> Result<ReleaseGuard> {
    if reason.trim().is_empty() {
        return Err(Error::rejected("checkout release requires a reason"));
    }
    let root = canonical_root(repo)?;
    let key = path_key(path);
    let (dir, lock) = LedgerLock::acquire(&root)?;
    let mut ledger = read_ledger(&root)?;
    let matches: Vec<usize> = ledger
        .records
        .iter()
        .enumerate()
        .filter_map(|(index, record)| record_path_matches(record, &key).then_some(index))
        .collect();
    if matches.len() > 1 {
        return Err(Error::rejected(format!(
            "multiple lifecycle ownership records name {}; refusing release",
            path.display()
        )));
    }
    let Some(index) = matches.first().copied() else {
        if !allow_legacy_absence {
            return Err(Error::rejected(format!(
                "{} has no managed ownership record; refusing deletion",
                path.display()
            )));
        }
        if !expected(None)? {
            return Err(Error::rejected(refused));
        }
        return Ok(ReleaseGuard {
            root,
            dir,
            key: None,
            _lock: lock,
        });
    };
    let record = &ledger.records[index];
    validate_record(&root, record)?;
    if record.path != key {
        return Err(Error::rejected(format!(
            "checkout path {} differs from recorded path {}; refusing moved or noncanonical ownership",
            path.display(),
            record.path
        )));
    }
    if record.state == "released" && allow_already_released {
        if !expected(Some(record))? {
            return Err(Error::rejected(format!(
                "{refused} (lifecycle state released)"
            )));
        }
        return Ok(ReleaseGuard {
            root,
            dir,
            key: None,
            _lock: lock,
        });
    }
    if record.state != "active" {
        return Err(Error::rejected(format!(
            "{refused} (lifecycle state {})",
            record.state
        )));
    }
    if !expected(Some(record))? {
        return Err(Error::rejected(refused));
    }
    let record = &mut ledger.records[index];
    record.state = "releasing".into();
    record.release_reason = Some(reason.to_string());
    record.updated_at = time::now_epoch();
    write_ledger(&dir, &ledger)?;
    Ok(ReleaseGuard {
        root,
        dir,
        key: Some(key),
        _lock: lock,
    })
}

/// Authorize a development release and hold the ledger lock across deletion.
/// Missing records are allowed only for legacy issue-ref-authorized checkouts;
/// the held lock serializes any concurrent explicit adoption.
pub fn begin_release(
    repo: &Path,
    path: &Path,
    issue: &str,
    branch: Option<&str>,
    reason: &str,
) -> Result<ReleaseGuard> {
    begin_release_transaction(
        repo,
        path,
        reason,
        format!(
            "checkout ownership for {} changed before release of issue {issue}; refusing deletion",
            path.display()
        ),
        true,
        false,
        |record| {
            Ok(record.is_none_or(|record| {
                record.purpose == "development"
                    && record.issue.as_deref() == Some(issue)
                    && record.branch.as_deref() == branch
            }))
        },
    )
}

pub fn begin_refs_only_finish(
    repo: &Path,
    path: &Path,
    issue: &str,
    branch: Option<&str>,
    reason: &str,
) -> Result<ReleaseGuard> {
    let root = canonical_root(repo)?;
    begin_release_transaction(
        &root,
        path,
        reason,
        format!(
            "refs-only finish ownership for {} changed before closing issue {issue}; refusing closure",
            path.display()
        ),
        true,
        true,
        |record| {
            validate_refs_only_finish_target(&root, path, branch)?;
            Ok(record.is_none_or(|record| {
                record.purpose == "development"
                    && record.issue.as_deref() == Some(issue)
                    && record.branch.as_deref() == branch
            }))
        },
    )
}

/// Authorize a review-tree release while binding it to the exact tool owner
/// and pinned revision, then hold the ledger lock through removal.
pub fn begin_review_release(
    repo: &Path,
    path: &Path,
    tool: &str,
    owner: &str,
    pinned_sha: &str,
    reason: &str,
) -> Result<ReleaseGuard> {
    begin_release_transaction(
        repo,
        path,
        reason,
        format!(
            "review checkout ownership for {} changed before release; refusing deletion",
            path.display()
        ),
        false,
        false,
        |record| {
            Ok(record.is_some_and(|record| {
                record.purpose == "review"
                    && record.tool == tool
                    && record.owner == owner
                    && record.pinned_sha == pinned_sha
            }))
        },
    )
}

pub struct ReclaimGuard {
    _lock: LedgerLock,
}

fn validate_target_reclaim_at_root(
    root: &Path,
    path: &Path,
    issue: &str,
    branch: &str,
) -> Result<()> {
    if !path.is_absolute() {
        return Err(Error::rejected(format!(
            "target reclaim path {} is not absolute",
            path.display()
        )));
    }
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        Error::rejected(format!(
            "cannot inspect target reclaim checkout {}: {error}",
            path.display()
        ))
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(Error::rejected(format!(
            "target reclaim checkout {} is not a real directory",
            path.display()
        )));
    }
    let canonical = path.canonicalize()?;
    if canonical.as_os_str() != path.as_os_str()
        || canonical == root
        || worktree::main_root(&canonical)?.canonicalize()? != root
    {
        return Err(Error::rejected(format!(
            "target reclaim checkout {} is not its canonical linked path in {}",
            path.display(),
            root.display()
        )));
    }
    if finish::git_branch(&canonical)?.as_deref() != Some(branch) {
        return Err(Error::rejected(format!(
            "target reclaim checkout {} is not on issue-owned branch {branch}",
            path.display()
        )));
    }
    worktree::validate_registered_branch(root, &canonical, branch)?;

    let key = canonical.to_string_lossy().into_owned();
    let ledger = read_ledger(root)?;
    let matches: Vec<&Checkout> = ledger
        .records
        .iter()
        .filter(|record| record_path_matches(record, &key))
        .collect();
    if matches.len() > 1 {
        return Err(Error::rejected(format!(
            "multiple lifecycle ownership records name {}; refusing target reclaim",
            path.display()
        )));
    }
    if let Some(record) = matches.first().copied() {
        validate_record(root, record)?;
        if record.path != key {
            return Err(Error::rejected(format!(
                "target reclaim path {} differs from recorded path {}; refusing moved or noncanonical ownership",
                path.display(),
                record.path
            )));
        }
        if record.purpose != "development"
            || record.issue.as_deref() != Some(issue)
            || record.branch.as_deref() != Some(branch)
        {
            return Err(Error::rejected(format!(
                "target reclaim lifecycle ownership for {} does not match development issue {issue}, branch {branch}",
                path.display()
            )));
        }
        if record.state != "active" {
            let reason = record
                .retention_reason
                .as_deref()
                .or(record.release_reason.as_deref())
                .unwrap_or("no recorded lifecycle reason");
            return Err(Error::rejected(format!(
                "target reclaim refused: {} is recorded as {} ({reason})",
                path.display(),
                record.state
            )));
        }
    }
    Ok(())
}

pub fn validate_target_reclaim(repo: &Path, path: &Path, issue: &str, branch: &str) -> Result<()> {
    let root = canonical_root(repo)?;
    validate_target_reclaim_at_root(&root, path, issue, branch)
}

pub fn begin_target_reclaim(
    repo: &Path,
    path: &Path,
    issue: &str,
    branch: &str,
) -> Result<ReclaimGuard> {
    let root = canonical_root(repo)?;
    let (_dir, lock) = LedgerLock::acquire(&root)?;
    validate_target_reclaim_at_root(&root, path, issue, branch)?;
    Ok(ReclaimGuard { _lock: lock })
}

/// Whether a lifecycle record exists for this path (read-only).
pub fn contains(repo: &Path, path: &Path) -> Result<bool> {
    Ok(managed_record(repo, path)?.is_some())
}

/// Find a matching interrupted or active checkout that its creating tool may
/// safely resume. Released/retained records are never implicit reuse permits.
pub fn recoverable_record(
    repo: &Path,
    path: &Path,
    purpose: &str,
    tool: &str,
    branch: Option<&str>,
    issue: Option<&str>,
) -> Result<Option<Checkout>> {
    let root = canonical_root(repo)?;
    let key = path_key(path);
    let repo_key = root.to_string_lossy().into_owned();
    let ledger = read_ledger(&root)?;
    let record = ledger.records.into_iter().find(|record| {
        record.repo == repo_key
            && record.path == key
            && record.purpose == purpose
            && record.tool == tool
            && record.branch.as_deref() == branch
            && record.issue.as_deref() == issue
            && matches!(
                record.state.as_str(),
                "preparing" | "setup-failed" | "cleanup-failed" | "active"
            )
    });
    if let Some(record) = &record {
        validate_record(&root, record)?;
    }
    Ok(record)
}

/// Verify that a particular tool and owner own this exact path and revision.
pub fn owns(repo: &Path, path: &Path, tool: &str, owner: &str, pinned_sha: &str) -> Result<bool> {
    let root = canonical_root(repo)?;
    let key = path_key(path);
    let ledger = read_ledger(&root)?;
    Ok(ledger.records.iter().any(|record| {
        record.path == key
            && record.tool == tool
            && record.owner == owner
            && record.pinned_sha == pinned_sha
            && record.state == "active"
    }))
}

/// The native layout name remains the one source of development checkout paths.
pub fn development_path(repo: &Path, name: &str) -> PathBuf {
    layout::worktree_dir(repo, name)
}
