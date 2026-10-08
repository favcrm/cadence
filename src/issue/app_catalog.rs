//! CAD-630 foundation: explicit single-workspace installation catalog.
//! No daemon/HTTP routing, execution or grant translation is enabled here.
mod appslock;
mod fs;
pub mod workspace;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::ops::Deref;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::issue::{app::Record, model, Pm};
use fs::Root;

const CATALOG: &str = ".apps/catalog.yaml";
const PENDING: &str = ".apps/pending.yaml";
const RECORD_CAP: u64 = 64 * 1024;
const CATALOG_CAP: u64 = 4 * 1024 * 1024;
const MAX_INSTALLATIONS: usize = 1024;
const MAX_INVENTORY_ENTRIES: usize = 4096;
const JOURNAL_CAP: u64 = 32 * 1024 * 1024;

/// A validated immutable identity; an app name is never its fallback.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct InstallationId(String);

impl InstallationId {
    pub fn parse(value: &str) -> Result<Self> {
        if value.is_empty()
            || value.len() > 128
            || !value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        {
            return Err(Error::rejected("invalid installation ID"));
        }
        Ok(Self(value.into()))
    }
}
impl Deref for InstallationId {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}
impl PartialEq<&str> for InstallationId {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}
impl<'de> Deserialize<'de> for InstallationId {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        Self::parse(&String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
enum Storage {
    Legacy { project: String, name: String },
    Workspace,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    app: String,
    project: Option<String>,
    storage: Storage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    bundle_revision: Option<String>,
}
impl Entry {
    fn paths(&self, id: &InstallationId) -> (PathBuf, PathBuf) {
        match &self.storage {
            Storage::Legacy { project, name } => {
                let base = Path::new(project).join("apps");
                (base.join(name), base.join(format!("{name}.yaml")))
            }
            Storage::Workspace => {
                let base = Path::new(".apps/installations").join(&**id);
                let bundle = match &self.bundle_revision {
                    Some(revision) => base.join("revisions").join(revision).join("bundle"),
                    None => base.join("bundle"),
                };
                (bundle, base.join("record.yaml"))
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Catalog {
    schema: u32,
    workspace: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_migration: Option<String>,
    installations: BTreeMap<InstallationId, Entry>,
}
impl Default for Catalog {
    fn default() -> Self {
        Self {
            schema: 1,
            workspace: "default".into(),
            last_migration: None,
            installations: BTreeMap::new(),
        }
    }
}

#[derive(Debug)]
pub struct Installation {
    pub install_id: InstallationId,
    pub project: Option<String>,
}

/// Operator-only legacy inventory/backfill; no grant or effect is rewritten.
pub fn migrate(pm: &Pm, state_dir: &Path) -> Result<Catalog> {
    crate::rollout::require_operator(state_dir, "app catalog migration")?;
    migrate_authorized(pm)
}

/// Daemon backend; the caller has already proved its connection is operator.
pub(crate) fn migrate_authorized(pm: &Pm) -> Result<Catalog> {
    let _apps = appslock::acquire(pm)?;
    let _lock = pm.lock()?;
    migrate_locked(pm)
}

fn migrate_locked(pm: &Pm) -> Result<Catalog> {
    migrate_delivering(pm, |_| Ok(()))
}
fn migrate_delivering(pm: &Pm, delivery: impl FnOnce(&Journal) -> Result<()>) -> Result<Catalog> {
    let root = Root::open(&pm.dir)?;
    no_pending(&root)?;
    let before_catalog = root.read(Path::new(CATALOG), CATALOG_CAP)?;
    let old: Catalog = before_catalog
        .as_deref()
        .map(decode)
        .transpose()?
        .unwrap_or_default();
    old.validate()?;
    let id = uuid::Uuid::new_v4().simple().to_string();
    let mut records = Vec::new();
    for (project, name, before) in legacy_records(&root)? {
        let mut parsed = record(&before, &name)?;
        let after = if parsed.install_id.is_empty() {
            parsed.install_id = uuid::Uuid::new_v4().simple().to_string();
            yaml(&parsed)?
        } else {
            before.clone()
        };
        records.push(Snapshot {
            project,
            name,
            before_hash: hash(&before),
            after_hash: hash(&after),
            before,
            after,
        });
    }
    let mut journal = Journal {
        schema: 1,
        id: id.clone(),
        before_catalog,
        after_catalog: String::new(),
        records,
    };
    let candidate = journal.candidate(&root)?;
    if old.installations == candidate.installations
        && journal.records.iter().all(|r| r.before == r.after)
        && journal.before_catalog.is_some()
    {
        return Ok(old);
    }
    journal.after_catalog = yaml(&candidate)?;
    journal.verify(&root)?;
    let text = yaml(&journal)?;
    if text.len() as u64 > JOURNAL_CAP {
        return Err(Error::rejected("app migration journal exceeds its limit"));
    }
    root.mkdir(Path::new(".apps"))?;
    root.mkdir(Path::new(".apps/migrations"))?;
    let backup = journal_path(&id);
    if root.kind(&backup)?.is_some() {
        return Err(Error::rejected("migration journal already exists"));
    }
    root.put(&backup, &text)?;
    root.put(
        Path::new(PENDING),
        &yaml(&Pending {
            schema: 1,
            journal: id,
        })?,
    )?;
    journal.apply_delivering(&root, Recovery::Resume, delivery).map_err(|error| {
        Error::internal(format!("app catalog publication or git delivery incomplete ({}); retained migration {}; explicitly run `cadence app catalog migration-recover {}` to resume, or add `--rollback` to roll back", error.kind(), journal.id, journal.id))
    })?;
    Ok(candidate)
}

#[derive(Clone, Copy)]
pub enum Recovery {
    Resume,
    Rollback,
}

pub fn recover(pm: &Pm, state_dir: &Path, id: &str, mode: Recovery) -> Result<()> {
    crate::rollout::require_operator(state_dir, "app catalog recovery")?;
    recover_authorized(pm, id, mode)
}

pub(crate) fn recover_authorized(pm: &Pm, id: &str, mode: Recovery) -> Result<()> {
    let _apps = appslock::acquire(pm)?;
    let _lock = pm.lock()?;
    recover_locked(pm, id, mode)
}
fn recover_locked(pm: &Pm, id: &str, mode: Recovery) -> Result<()> {
    recover_delivering(pm, id, mode, |_| Ok(()))
}
fn recover_delivering(
    pm: &Pm,
    id: &str,
    mode: Recovery,
    delivery: impl FnOnce(&Journal) -> Result<()>,
) -> Result<()> {
    journal_id(id)?;
    let root = Root::open(&pm.dir)?;
    let journal: Journal = decode(&required(&root, &journal_path(id), JOURNAL_CAP)?)?;
    if journal.id != id {
        return Err(Error::rejected(
            "migration journal identity does not match its filename",
        ));
    }
    journal.verify(&root)?;
    if root.read(Path::new(PENDING), RECORD_CAP)?.is_none() {
        root.put(
            Path::new(PENDING),
            &yaml(&Pending {
                schema: 1,
                journal: id.into(),
            })?,
        )?;
    }
    journal.apply_delivering(&root, mode, delivery)
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pending {
    schema: u32,
    journal: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    project: String,
    name: String,
    before: String,
    after: String,
    before_hash: String,
    after_hash: String,
}
impl Snapshot {
    fn entry(&self) -> Entry {
        Entry {
            app: self.name.clone(),
            project: Some(self.project.clone()),
            storage: Storage::Legacy {
                project: self.project.clone(),
                name: self.name.clone(),
            },
            bundle_revision: None,
        }
    }
    fn path(&self) -> PathBuf {
        Path::new(&self.project)
            .join("apps")
            .join(format!("{}.yaml", self.name))
    }
    fn identity(&self) -> Result<InstallationId> {
        model::check_key(&self.project)?;
        if !model::valid_tag(&self.name)
            || self.before.len() as u64 > RECORD_CAP
            || self.after.len() as u64 > RECORD_CAP
            || hash(&self.before) != self.before_hash
            || hash(&self.after) != self.after_hash
        {
            return Err(Error::rejected(
                "invalid migration source path, size or fingerprint",
            ));
        }
        let mut before = record(&self.before, &self.name)?;
        let after = record(&self.after, &self.name)?;
        let id = InstallationId::parse(&after.install_id)?;
        if before.install_id.is_empty() {
            before.install_id = after.install_id.clone();
        } else if self.before != self.after {
            return Err(Error::rejected(
                "migration cannot rewrite a nonempty installation identity",
            ));
        }
        if serde_yaml::to_value(before).map_err(|e| Error::internal(e.to_string()))?
            != serde_yaml::to_value(after).map_err(|e| Error::internal(e.to_string()))?
        {
            return Err(Error::rejected(
                "migration may only backfill the missing installation ID",
            ));
        }
        Ok(id)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    schema: u32,
    id: String,
    before_catalog: Option<String>,
    after_catalog: String,
    records: Vec<Snapshot>,
}
impl Journal {
    fn candidate(&self, root: &Root) -> Result<Catalog> {
        if self.schema != 1
            || self.records.len() > MAX_INSTALLATIONS
            || self
                .before_catalog
                .as_ref()
                .is_some_and(|s| s.len() as u64 > CATALOG_CAP)
            || self.after_catalog.len() as u64 > CATALOG_CAP
        {
            return Err(Error::rejected(
                "unsupported app migration journal schema or size",
            ));
        }
        journal_id(&self.id)?;
        let old: Catalog = self
            .before_catalog
            .as_deref()
            .map(decode)
            .transpose()?
            .unwrap_or_default();
        old.validate()?;
        let mut catalog = Catalog {
            last_migration: Some(self.id.clone()),
            ..Catalog::default()
        };
        // Existing workspace records are not backfilled or rewritten. Their
        // immutable identity is re-proved, and optional links are preserved.
        for (id, entry) in &old.installations {
            if entry.storage == Storage::Workspace {
                old.installation(root, id)?;
                catalog.installations.insert(id.clone(), entry.clone());
            }
        }
        let mut aliases = HashSet::new();
        for snapshot in &self.records {
            let id = snapshot.identity()?;
            if !aliases.insert((&snapshot.project, &snapshot.name))
                || catalog.installations.insert(id, snapshot.entry()).is_some()
            {
                return Err(Error::rejected(
                    "duplicate installation identity or migration source",
                ));
            }
        }
        catalog.validate()?;
        Ok(catalog)
    }
    fn verify(&self, root: &Root) -> Result<()> {
        let candidate = self.candidate(root)?;
        let staged: Catalog = decode(&self.after_catalog)?;
        if staged != candidate || self.after_catalog.len() as u64 > CATALOG_CAP {
            return Err(Error::rejected(
                "migration catalog does not match its complete source inventory",
            ));
        }
        if let Some(text) = root.read(Path::new(PENDING), RECORD_CAP)? {
            let pending: Pending = decode(&text)?;
            if pending.schema != 1 || pending.journal != self.id {
                return Err(Error::rejected("another app migration is pending"));
            }
        }
        let current = root.read(Path::new(CATALOG), CATALOG_CAP)?;
        if current != self.before_catalog && current.as_deref() != Some(self.after_catalog.as_str())
        {
            return Err(Error::rejected(
                "catalog changed after migration staging; retained journal is not safe to apply",
            ));
        }
        let observed: HashSet<_> = legacy_records(root)?
            .into_iter()
            .map(|(project, name, _)| (project, name))
            .collect();
        let expected: HashSet<_> = self
            .records
            .iter()
            .map(|s| (s.project.clone(), s.name.clone()))
            .collect();
        if observed != expected {
            return Err(Error::rejected(
                "legacy installation inventory changed after staging",
            ));
        }
        for source in &self.records {
            let current = required(root, &source.path(), RECORD_CAP)?;
            if current != source.before && current != source.after {
                return Err(Error::rejected(
                    "app record changed after staging; retained journal is not safe to apply",
                ));
            }
        }
        Ok(())
    }
    fn target<'a>(&'a self, source: &'a Snapshot, mode: Recovery) -> &'a str {
        match mode {
            Recovery::Resume => &source.after,
            Recovery::Rollback => &source.before,
        }
    }
    fn apply_delivering(
        &self,
        root: &Root,
        mode: Recovery,
        delivery: impl FnOnce(&Journal) -> Result<()>,
    ) -> Result<()> {
        self.verify(root)?;
        for source in &self.records {
            let target = self.target(source, mode);
            if required(root, &source.path(), RECORD_CAP)? != target {
                root.put(&source.path(), target)?;
            }
        }
        self.verify(root)?;
        for source in &self.records {
            if required(root, &source.path(), RECORD_CAP)? != self.target(source, mode) {
                return Err(Error::rejected(
                    "record changed during app migration publication",
                ));
            }
        }
        let target = match mode {
            Recovery::Resume => Some(self.after_catalog.as_str()),
            Recovery::Rollback => self.before_catalog.as_deref(),
        };
        match target {
            Some(text) => root.put(Path::new(CATALOG), text)?,
            None => root.remove(Path::new(CATALOG))?,
        }
        for source in &self.records {
            if required(root, &source.path(), RECORD_CAP)? != self.target(source, mode) {
                return Err(Error::rejected(
                    "record changed before migration completion",
                ));
            }
        }
        if root.read(Path::new(CATALOG), CATALOG_CAP)?.as_deref() != target {
            return Err(Error::rejected(
                "catalog changed before migration completion",
            ));
        }
        delivery(self)?;
        root.remove(Path::new(PENDING))?;
        Ok(())
    }
}

fn hash(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}
fn yaml<T: Serialize>(value: &T) -> Result<String> {
    serde_yaml::to_string(value).map_err(|e| Error::internal(e.to_string()))
}
fn journal_path(id: &str) -> PathBuf {
    Path::new(".apps/migrations").join(format!("{id}.yaml"))
}

impl Catalog {
    fn validate(&self) -> Result<()> {
        if self.schema != 1
            || self.workspace != "default"
            || self.installations.len() > MAX_INSTALLATIONS
        {
            return Err(Error::rejected(
                "unsupported app catalog schema, workspace or size",
            ));
        }
        if let Some(id) = &self.last_migration {
            journal_id(id)?;
        }
        let mut aliases = HashSet::new();
        for entry in self.installations.values() {
            if !model::valid_tag(&entry.app) {
                return Err(Error::rejected("invalid catalog app name"));
            }
            if let Some(project) = &entry.project {
                model::check_key(project)?;
            }
            if let Some(revision) = &entry.bundle_revision {
                if entry.storage != Storage::Workspace
                    || revision.len() != 64
                    || !revision
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
                {
                    return Err(Error::rejected("invalid workspace bundle revision"));
                }
            }
            if let Storage::Legacy { project, name } = &entry.storage {
                model::check_key(project)?;
                if name != &entry.app
                    || entry.project.as_ref() != Some(project)
                    || !aliases.insert((project, name))
                {
                    return Err(Error::rejected(
                        "divergent or duplicate legacy installation alias",
                    ));
                }
            }
        }
        Ok(())
    }
    fn installation(&self, root: &Root, id: &InstallationId) -> Result<Installation> {
        self.validate()?;
        let entry = self
            .installations
            .get(id)
            .ok_or_else(|| Error::rejected("unknown installation ID"))?;
        let (bundle, file) = entry.paths(id);
        root.dir(&bundle)?;
        let record = record(&required(root, &file, RECORD_CAP)?, &entry.app)?;
        if record.install_id != **id {
            return Err(Error::rejected(
                "catalog installation was removed, replaced or changed identity",
            ));
        }
        Ok(Installation {
            install_id: id.clone(),
            project: entry.project.clone(),
        })
    }
    fn require_current(&self, root: &Root) -> Result<()> {
        no_pending(root)?;
        let current: Catalog = decode(&required(root, Path::new(CATALOG), CATALOG_CAP)?)?;
        current.validate()?;
        if current != *self {
            return Err(Error::rejected(
                "cached catalog generation is no longer published",
            ));
        }
        Ok(())
    }
    pub fn load(root: &Path) -> Result<Self> {
        Self::load_observed(root, || {})
    }
    fn load_observed(root: &Path, observed: impl FnOnce()) -> Result<Self> {
        let root = Root::open(root)?;
        no_pending(&root)?;
        let catalog: Self = decode(&required(&root, Path::new(CATALOG), CATALOG_CAP)?)?;
        catalog.validate()?;
        observed();
        for id in catalog.installations.keys() {
            catalog.installation(&root, id)?;
        }
        catalog.require_current(&root)?;
        Ok(catalog)
    }
    #[cfg(feature = "test-seam")]
    #[doc(hidden)]
    pub fn load_with_observer(root: &Path, observed: impl FnOnce()) -> Result<Self> {
        Self::load_observed(root, observed)
    }
    pub fn resolve_id(&self, root: &Path, id: &str) -> Result<Installation> {
        self.resolve_observed(root, id, || {})
    }
    fn resolve_observed(
        &self,
        root: &Path,
        id: &str,
        observed: impl FnOnce(),
    ) -> Result<Installation> {
        let id = InstallationId::parse(id)?;
        let root = Root::open(root)?;
        self.require_current(&root)?;
        observed();
        let installation = self.installation(&root, &id)?;
        self.require_current(&root)?;
        Ok(installation)
    }
    #[cfg(feature = "test-seam")]
    #[doc(hidden)]
    pub fn resolve_id_with_observer(
        &self,
        root: &Path,
        id: &str,
        observed: impl FnOnce(),
    ) -> Result<Installation> {
        self.resolve_observed(root, id, observed)
    }
    pub fn resolve_legacy(&self, root: &Path, project: &str, name: &str) -> Result<Installation> {
        model::check_key(project)?;
        if !model::valid_tag(name) {
            return Err(Error::rejected("invalid legacy app name"));
        }
        let id = self
            .installations
            .iter()
            .find_map(|(id, entry)| match &entry.storage {
                Storage::Legacy {
                    project: p,
                    name: n,
                } if p == project && n == name => Some(id),
                _ => None,
            })
            .ok_or_else(|| Error::rejected("unknown exact legacy installation alias"))?;
        self.resolve_id(root, id)
    }
}

// Value's Mapping visitor rejects duplicate YAML keys, including nested maps.
fn decode<T: serde::de::DeserializeOwned>(text: &str) -> Result<T> {
    let value: serde_yaml::Value =
        serde_yaml::from_str(text).map_err(|e| Error::rejected(format!("catalog YAML: {e}")))?;
    serde_yaml::from_value(value).map_err(|e| Error::rejected(format!("catalog schema: {e}")))
}
fn required(root: &Root, path: &Path, cap: u64) -> Result<String> {
    root.read(path, cap)?
        .ok_or_else(|| Error::rejected(format!("catalog source is missing: {}", path.display())))
}
fn record(text: &str, name: &str) -> Result<Record> {
    let record: Record = decode(text)?;
    if record.schema != 1 || record.app != name {
        return Err(Error::rejected(
            "catalog record schema or app name does not match",
        ));
    }
    Ok(record)
}
fn no_pending(root: &Root) -> Result<()> {
    if root
        .read(Path::new(".apps/upgrade-pending.yaml"), RECORD_CAP)?
        .is_some()
    {
        return Err(Error::rejected("workspace upgrade is pending; use app catalog upgrade-recover with its installation ID and request ID"));
    }
    if root
        .read(Path::new(".apps/install-pending.yaml"), RECORD_CAP)?
        .is_some()
    {
        return Err(Error::rejected("workspace installation is pending; use app catalog recover with the retained installation ID"));
    }
    if root.read(Path::new(PENDING), RECORD_CAP)?.is_some() {
        return Err(Error::rejected(
            "app catalog publication is pending; recover its retained journal first",
        ));
    }
    Ok(())
}
fn journal_id(id: &str) -> Result<()> {
    if id.len() != 32
        || !id
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err(Error::rejected("invalid app migration journal ID"));
    }
    Ok(())
}
fn legacy_records(root: &Root) -> Result<Vec<(String, String, String)>> {
    let mut records = Vec::new();
    let mut budget = MAX_INVENTORY_ENTRIES;
    for project in root.list(Path::new(""), &mut budget)? {
        if project.starts_with('.') || model::check_key(&project).is_err() {
            continue;
        }
        match root.kind(Path::new(&project))? {
            Some(libc::S_IFLNK) => {
                return Err(Error::rejected("symlinked legacy project is refused"))
            }
            Some(libc::S_IFDIR) => {}
            _ => continue,
        }
        let apps = Path::new(&project).join("apps");
        let Some(kind) = root.kind(&apps)? else {
            continue;
        };
        if kind != libc::S_IFDIR {
            return Err(Error::rejected("legacy apps must be a real directory"));
        }
        let mut folders = HashSet::new();
        let mut files = HashSet::new();
        for leaf in root.list(&apps, &mut budget)? {
            let kind = root.kind(&apps.join(&leaf))?;
            if kind == Some(libc::S_IFLNK) {
                return Err(Error::rejected("symlinked legacy app is refused"));
            }
            if kind == Some(libc::S_IFDIR) && model::valid_tag(&leaf) {
                folders.insert(leaf);
            } else if kind == Some(libc::S_IFREG) {
                if let Some(name) = leaf.strip_suffix(".yaml") {
                    if !model::valid_tag(name) {
                        return Err(Error::rejected("invalid legacy record name"));
                    }
                    files.insert(name.to_string());
                }
            }
        }
        if folders != files {
            return Err(Error::rejected(
                "legacy app folder and record inventory diverge",
            ));
        }
        let mut names: Vec<_> = files.into_iter().collect();
        names.sort();
        for name in names {
            let text = required(root, &apps.join(format!("{name}.yaml")), RECORD_CAP)?;
            record(&text, &name)?;
            records.push((project.clone(), name, text));
        }
    }
    Ok(records)
}
