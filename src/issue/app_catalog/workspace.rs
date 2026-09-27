//! Operator-owned workspace installation transport. Execution is deliberately absent.
use super::*;
use crate::issue::{app, workflow, write};
use serde_json::{json, Value};

const INSTALL_PENDING: &str = ".apps/install-pending.yaml";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct InstallJournal {
    schema: u32,
    id: InstallationId,
    before_catalog: Option<String>,
    after_catalog: String,
    record: String,
    files: BTreeMap<String, String>,
}

fn journal_path(id: &InstallationId) -> PathBuf {
    Path::new(".apps/install-journals").join(format!("{}.yaml", &**id))
}
fn bundle_digest(files: &BTreeMap<String, String>) -> String {
    let mut digest = Sha256::new();
    for (name, body) in files {
        digest.update((name.len() as u64).to_be_bytes());
        digest.update(name.as_bytes());
        digest.update((body.len() as u64).to_be_bytes());
        digest.update(body.as_bytes());
    }
    format!("sha256:{:x}", digest.finalize())
}

fn validate_source_transport(source: &str) -> Result<()> {
    if Path::new(source).is_absolute() {
        return Ok(());
    }
    let remote = source.contains("://") || source.starts_with("git@");
    let credential = source.contains('?')
        || source.contains('#')
        || source.contains('\n')
        || source.contains('\r');
    let userinfo = source
        .split_once("://")
        .and_then(|(_, rest)| rest.split('/').next())
        .and_then(|authority| authority.rsplit_once('@').map(|(user, _)| user));
    if remote
        && (credential
            || userinfo.is_some_and(|user| {
                source.starts_with("https://")
                    || source.starts_with("http://")
                    || user.contains(':')
            }))
    {
        return Err(Error::rejected("credential-bearing source URLs are refused; use a trusted credential-free repository URL or local checkout"));
    }
    if !source.starts_with("https://")
        && !source.starts_with("ssh://")
        && !source.starts_with("git@")
    {
        return Err(Error::rejected(
            "workspace Git sources admit plain HTTPS or SSH repository URLs",
        ));
    }
    Ok(())
}

fn snapshot(root: &Root, base: &Path, source: bool) -> Result<BTreeMap<String, String>> {
    let mut budget = MAX_INVENTORY_ENTRIES;
    let mut files = BTreeMap::new();
    let mut bytes = 0usize;
    let mut add = |name: String, path: PathBuf| -> Result<()> {
        if files.len() >= 128 {
            return Err(Error::rejected("app bundle exceeds its file limit"));
        }
        let text = required(root, &path, crate::issue::plan::MAX_PLAN_BYTES as u64)?;
        bytes += text.len();
        if bytes > app::MAX_APP_BYTES as usize {
            return Err(Error::rejected(
                "app bundle exceeds its aggregate byte limit",
            ));
        }
        files.insert(name, text);
        Ok(())
    };
    for name in root.list(base, &mut budget)? {
        let path = base.join(&name);
        if source && name == ".git" {
            continue;
        }
        match root.kind(&path)? {
            Some(libc::S_IFDIR)
                if matches!(name.as_str(), "workflows" | "rubrics" | "templates") =>
            {
                for leaf in root.list(&path, &mut budget)? {
                    if leaf.starts_with('.')
                        || (name == "workflows"
                            && (!leaf.ends_with(".md")
                                || !model::valid_tag(leaf.trim_end_matches(".md"))))
                    {
                        return Err(Error::rejected(
                            "workflows must be named Markdown files and all bundle entries must be visible flat text files",
                        ));
                    }
                    add(format!("{name}/{leaf}"), path.join(leaf))?;
                }
            }
            Some(libc::S_IFREG) if name == "app.md" => add(name, path)?,
            _ => return Err(Error::rejected("unsupported or unsafe app bundle entry")),
        }
    }
    Ok(files)
}

fn current(root: &Root) -> Result<(Catalog, Option<String>)> {
    no_pending(root)?;
    let text = root.read(Path::new(CATALOG), CATALOG_CAP)?;
    let catalog: Catalog = text.as_deref().map(decode).transpose()?.unwrap_or_default();
    catalog.validate()?;
    if text.is_none() && !legacy_records(root)?.is_empty() {
        return Err(Error::rejected("legacy installations require explicit `cadence app catalog migrate` before workspace installation"));
    }
    for id in catalog.installations.keys() {
        catalog.require_current(root)?;
        catalog.installation(root, id)?;
    }
    Ok((catalog, text))
}

pub(crate) fn install(pm: &Pm, state: &Path, source: &str) -> Result<Value> {
    let _ = state; // Caller must pass the daemon's connection-bound operator gate.
    if !Path::new(source).is_absolute() && !source.contains("://") && !source.starts_with("git@") {
        return Err(Error::rejected("workspace local source must be an absolute path; the CLI resolves caller-relative paths"));
    }
    validate_source_transport(source)?;
    let (source_dir, provenance, _temporary) = app::resolve_source(source)?;
    if source_dir.starts_with(pm.dir.canonicalize()?) {
        return Err(Error::rejected(
            "the tracker cannot be its own installation source",
        ));
    }
    let (agents, agent_sources) = workflow::known_agents(&pm.dir, None, &[]);
    let source_root = Root::open(&source_dir)?;
    let files = snapshot(&source_root, Path::new(""), true)?;
    let validated = app::validate_texts(
        files
            .iter()
            .map(|(name, text)| (name.clone(), text.clone()))
            .collect(),
        &agents,
        &agent_sources,
    )?;
    let _lock = pm.lock()?;
    let root = Root::open(&pm.dir)?;
    let (mut catalog, before_catalog) = current(&root)?;
    for (id, entry) in &catalog.installations {
        if entry.storage == Storage::Workspace && entry.app == validated.manifest.app {
            return Err(Error::rejected(format!("app '{}' already has workspace installation {}; replacement/upgrade must explicitly target that ID and is not supported in this increment", entry.app, &**id)));
        }
    }
    let id = InstallationId::parse(&uuid::Uuid::new_v4().simple().to_string())?;
    let record = Record {
        schema: 1,
        app: validated.manifest.app.clone(),
        install_id: id.to_string(),
        source: provenance,
        bindings: BTreeMap::new(),
        team: BTreeMap::new(),
        installed_at: crate::issue::time::iso(crate::issue::time::now_epoch()),
        installed_by: "operator".into(),
        updated_at: None,
    };
    catalog.installations.insert(
        id.clone(),
        Entry {
            app: record.app.clone(),
            project: None,
            storage: Storage::Workspace,
        },
    );
    catalog.validate()?;
    let journal = InstallJournal {
        schema: 1,
        id: id.clone(),
        before_catalog,
        after_catalog: yaml(&catalog)?,
        record: yaml(&record)?,
        files,
    };
    let text = yaml(&journal)?;
    if text.len() as u64 > JOURNAL_CAP {
        return Err(Error::rejected(
            "workspace install journal exceeds its limit",
        ));
    }
    root.mkdir(Path::new(".apps"))?;
    root.mkdir(Path::new(".apps/install-journals"))?;
    root.put(&journal_path(&id), &text)?;
    root.put(Path::new(INSTALL_PENDING), &id)?;
    let foreign = apply(pm, &root, &journal).map_err(|error| {
        Error::internal(format!("workspace app publication or git delivery incomplete ({}); retained installation {}; explicitly run `cadence app catalog recover {}`", error.kind(), &*id, &*id))
    })?;
    let mut row = describe(&root, &catalog, &id)?;
    row["committed"] = json!(true);
    row["foreign_files"] = json!(foreign);
    row["notes"] = json!(validated.notes);
    row["secret_warnings"] = crate::secret::warnings_json(&validated.secret_warnings);
    Ok(row)
}

fn apply(pm: &Pm, root: &Root, journal: &InstallJournal) -> Result<Vec<String>> {
    if journal.schema != 1 || journal.files.len() > 256 || journal.files.is_empty() {
        return Err(Error::rejected("unsupported workspace install journal"));
    }
    let pending = required(root, Path::new(INSTALL_PENDING), RECORD_CAP)?;
    if pending != journal.id.to_string() {
        return Err(Error::rejected("another workspace installation is pending"));
    }
    let staged: Catalog = decode(&journal.after_catalog)?;
    staged.validate()?;
    let entry = staged
        .installations
        .get(&journal.id)
        .ok_or_else(|| Error::rejected("journal missing installation"))?;
    let record = record(&journal.record, &entry.app)?;
    if entry.storage != Storage::Workspace
        || entry.project.is_some()
        || record.install_id != journal.id.to_string()
        || !record.team.is_empty()
        || !record.bindings.is_empty()
    {
        return Err(Error::rejected(
            "workspace journal cannot import authority or change installation identity",
        ));
    }
    let mut expected: Catalog = journal
        .before_catalog
        .as_deref()
        .map(decode)
        .transpose()?
        .unwrap_or_default();
    if expected
        .installations
        .insert(journal.id.clone(), entry.clone())
        .is_some()
        || expected != staged
    {
        return Err(Error::rejected(
            "journal must add exactly one new installation",
        ));
    }
    let observed = root.read(Path::new(CATALOG), CATALOG_CAP)?;
    if observed != journal.before_catalog && observed.as_deref() != Some(&journal.after_catalog) {
        return Err(Error::rejected("catalog diverged after install staging; retained journal requires operator investigation"));
    }
    let (agents, agent_sources) = workflow::known_agents(&pm.dir, None, &[]);
    let validated = app::validate_texts(
        journal
            .files
            .iter()
            .map(|(name, text)| (name.clone(), text.clone()))
            .collect(),
        &agents,
        &agent_sources,
    )?;
    if validated.manifest.app != record.app {
        return Err(Error::rejected(
            "staged manifest identity differs from installation record",
        ));
    }
    for (name, text) in &journal.files {
        let path = Path::new(name);
        let components: Vec<_> = path.components().collect();
        let valid = name == "app.md"
            || (components.len() == 2
                && matches!(components[0],std::path::Component::Normal(n) if n=="workflows" || n=="rubrics" || n=="templates")
                && matches!(components[1],std::path::Component::Normal(n) if n.to_str().is_some_and(|leaf| !leaf.starts_with('.') && (!name.starts_with("workflows/") || (leaf.ends_with(".md") && model::valid_tag(leaf.trim_end_matches(".md")))))));
        if !valid || text.len() as u64 > crate::issue::plan::MAX_PLAN_BYTES as u64 {
            return Err(Error::rejected("unsafe or oversized journal bundle file"));
        }
    }
    root.mkdir(Path::new(".apps/installations"))?;
    let base = Path::new(".apps/installations").join(&*journal.id);
    root.mkdir(&base)?;
    let bundle = base.join("bundle");
    root.mkdir(&bundle)?;
    let mut paths = vec![
        pm.dir.join(CATALOG),
        pm.dir.join(journal_path(&journal.id)),
        pm.dir.join(base.join("record.yaml")),
    ];
    for (name, text) in &journal.files {
        let path = Path::new(name);
        let components: Vec<_> = path.components().collect();
        let valid = name == "app.md"
            || (components.len() == 2
                && matches!(components[0], std::path::Component::Normal(n) if n=="workflows" || n=="rubrics" || n=="templates")
                && matches!(components[1], std::path::Component::Normal(_)));
        if !valid || text.len() as u64 > CATALOG_CAP {
            return Err(Error::rejected("unsafe or oversized journal bundle file"));
        }
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            root.mkdir(&bundle.join(parent))?;
        }
        let target = bundle.join(path);
        match root.read(&target, CATALOG_CAP)? {
            Some(current) if current != *text => {
                return Err(Error::rejected("workspace bundle changed after staging"))
            }
            Some(_) => {}
            None => root.put(&target, text)?,
        }
        paths.push(pm.dir.join(target));
    }
    let target = base.join("record.yaml");
    match root.read(&target, RECORD_CAP)? {
        Some(current) if current != journal.record => {
            return Err(Error::rejected("workspace record changed after staging"))
        }
        Some(_) => {}
        None => root.put(&target, &journal.record)?,
    }
    for (name, text) in &journal.files {
        if required(root, &bundle.join(name), CATALOG_CAP)? != *text {
            return Err(Error::rejected("staged bundle changed before publication"));
        }
    }
    if snapshot(root, &bundle, false)? != journal.files {
        return Err(Error::rejected(
            "staged bundle inventory differs from retained journal",
        ));
    }
    root.put(Path::new(CATALOG), &journal.after_catalog)?;
    // Keep pending on Git failure: the durable journal supports explicit retry;
    // reads refuse rather than report an installation as delivered.
    let foreign = write::commit(
        pm,
        &paths,
        &format!("workspace app {} installed ({})", entry.app, &*journal.id),
        &[],
        "operator",
    )?;
    root.remove(Path::new(INSTALL_PENDING))?;
    Ok(foreign
        .into_iter()
        .filter(|path| path != INSTALL_PENDING)
        .collect())
}

pub(crate) fn recover(pm: &Pm, state: &Path, id: &str) -> Result<Value> {
    let _ = state; // The daemon proves the calling connection before this backend.
    let id = InstallationId::parse(id)?;
    let _lock = pm.lock()?;
    let root = Root::open(&pm.dir)?;
    let journal: InstallJournal = decode(&required(&root, &journal_path(&id), JOURNAL_CAP)?)?;
    if journal.id != id {
        return Err(Error::rejected("journal identity differs from filename"));
    }
    let foreign = apply(pm, &root, &journal)?;
    let catalog = Catalog::load(&pm.dir)?;
    let mut row = describe(&root, &catalog, &id)?;
    row["committed"] = json!(true);
    row["foreign_files"] = json!(foreign);
    Ok(row)
}

fn describe(root: &Root, catalog: &Catalog, id: &InstallationId) -> Result<Value> {
    catalog.require_current(root)?;
    catalog.installation(root, id)?;
    let entry = &catalog.installations[id];
    let (bundle, path) = entry.paths(id);
    let record = record(&required(root, &path, RECORD_CAP)?, &entry.app)?;
    let files = snapshot(root, &bundle, false)?;
    let manifest = app::parse_manifest(
        files
            .get("app.md")
            .ok_or_else(|| Error::rejected("installed bundle has no manifest"))?,
    )?;
    if manifest.app != entry.app || manifest.app != record.app {
        return Err(Error::rejected(
            "installed manifest identity differs from catalog and record",
        ));
    }
    catalog.require_current(root)?;
    let reread: Record = decode(&required(root, &path, RECORD_CAP)?)?;
    if serde_yaml::to_value(&reread).map_err(|e| Error::internal(e.to_string()))?
        != serde_yaml::to_value(&record).map_err(|e| Error::internal(e.to_string()))?
    {
        return Err(Error::rejected(
            "installation record changed during inspection",
        ));
    }
    Ok(
        json!({"schema":1,"workspace":"default","catalog_generation":hash(&yaml(catalog)?),"install_id":&**id,"name":manifest.app,"title":manifest.title,"version":manifest.version,"summary":manifest.summary,"project":entry.project,"project_link":entry.project,"storage_kind":if entry.storage==Storage::Workspace {"workspace"} else {"legacy"},"digest":bundle_digest(&files),"source":record.source,"installed_at":record.installed_at,"approval":{"state":if entry.storage==Storage::Workspace {"unapproved"} else {"unknown"}},"approved":if entry.storage==Storage::Workspace {json!(false)} else {Value::Null},"executable":false,"execution_note":"catalog execution is unavailable; existing legacy execution paths are unchanged","guide":manifest.guide,"record":record,"files":files.keys().collect::<Vec<_>>() }),
    )
}
pub fn list(pm: &Pm) -> Result<Value> {
    let _lock = pm.lock()?;
    let catalog = Catalog::load(&pm.dir)?;
    let root = Root::open(&pm.dir)?;
    let rows = catalog
        .installations
        .keys()
        .map(|id| describe(&root, &catalog, id))
        .collect::<Result<Vec<_>>>()?;
    catalog.require_current(&root)?;
    Ok(json!(rows))
}
pub fn show(pm: &Pm, id: &str) -> Result<Value> {
    let _lock = pm.lock()?;
    let catalog = Catalog::load(&pm.dir)?;
    let id = InstallationId::parse(id)?;
    let root = Root::open(&pm.dir)?;
    describe(&root, &catalog, &id)
}

/// Delivery happens before pending is removed, under the same PM lock.
fn commit_migration(pm: &Pm, journal: &Journal) -> Result<Vec<String>> {
    let mut paths = vec![pm.dir.join(super::journal_path(&journal.id))];
    // A first publication whose Git delivery failed is not tracked. Its
    // rollback removes the file, so there is no catalog path to stage.
    // Existing tracked catalogs must still be included to commit deletion.
    if Root::open(&pm.dir)?.kind(Path::new(CATALOG))?.is_some()
        || !super::super::git(&pm.dir, &["ls-files", "--", CATALOG])?.is_empty()
    {
        paths.push(pm.dir.join(CATALOG));
    }
    paths.extend(
        journal
            .records
            .iter()
            .map(|record| pm.dir.join(record.path())),
    );
    Ok(write::commit(
        pm,
        &paths,
        &format!("workspace app catalog migration {}", journal.id),
        &[],
        "operator",
    )?
    .into_iter()
    .filter(|path| path != PENDING)
    .collect())
}
pub(crate) fn migrate(pm: &Pm) -> Result<Value> {
    let _lock = pm.lock()?;
    let mut foreign = None;
    let catalog = migrate_delivering(pm, |journal| {
        foreign = Some(commit_migration(pm, journal)?);
        Ok(())
    })?;
    let foreign = match foreign {
        Some(foreign) => foreign,
        None => write::commit(
            pm,
            &[pm.dir.join(CATALOG)],
            "workspace app catalog unchanged",
            &[],
            "operator",
        )?,
    };
    Ok(
        json!({"schema":1,"workspace":"default","catalog_generation":hash(&yaml(&catalog)?),"journal_id":catalog.last_migration,"installations":catalog.installations.len(),"committed":true,"foreign_files":foreign,"executable":false}),
    )
}
pub(crate) fn migration_recover(pm: &Pm, id: &str, rollback: bool) -> Result<Value> {
    let _lock = pm.lock()?;
    let mut foreign = Vec::new();
    recover_delivering(
        pm,
        id,
        if rollback {
            Recovery::Rollback
        } else {
            Recovery::Resume
        },
        |journal| {
            foreign = commit_migration(pm, journal)?;
            Ok(())
        },
    )?;
    Ok(
        json!({"schema":1,"workspace":"default","journal_id":id,"rollback":rollback,"committed":true,"foreign_files":foreign,"executable":false}),
    )
}

/// Runtime admission uses the same descriptor-confined installation lookup.
/// The callback runs under PM -> SQLite lock ordering, never the reverse.
pub(crate) fn with_runtime_snapshot<T>(
    pm: &Pm,
    id: &str,
    callback: impl FnOnce(&Value, &BTreeMap<String, String>) -> Result<T>,
) -> Result<T> {
    let id = InstallationId::parse(id)?;
    let _lock = pm.lock()?;
    let root = Root::open(&pm.dir)?;
    let catalog = Catalog::load(&pm.dir)?;
    no_pending(&root)?;
    let description = describe(&root, &catalog, &id)?;
    let (bundle, _) = catalog.installations[&id].paths(&id);
    let files = snapshot(&root, &bundle, false)?;
    if bundle_digest(&files) != description["digest"].as_str().unwrap_or("") {
        return Err(Error::rejected(
            "installation changed during runtime admission",
        ));
    }
    callback(&description, &files)
}
