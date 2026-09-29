//! Operator-owned workspace installation transport. Execution is deliberately absent.
use super::*;
use crate::issue::{app, workflow, write};
use serde_json::{json, Value};

const INSTALL_PENDING: &str = ".apps/install-pending.yaml";
const UPGRADE_PENDING: &str = ".apps/upgrade-pending.yaml";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct UpgradeJournal {
    schema: u32,
    id: InstallationId,
    request_id: String,
    source: String,
    expected_digest: String,
    expected_generation: String,
    expected_new_digest: String,
    before_catalog: String,
    after_catalog: String,
    before_record: String,
    after_record: String,
    files: BTreeMap<String, String>,
    diff: Value,
    compatibility: Value,
}

fn upgrade_journal_path(id: &InstallationId, request: &str) -> PathBuf {
    Path::new(".apps/upgrade-journals")
        .join(&**id)
        .join(format!("{request}.yaml"))
}

fn structural_diff(old: &BTreeMap<String, String>, new: &BTreeMap<String, String>) -> Value {
    let added: Vec<_> = new.keys().filter(|name| !old.contains_key(*name)).collect();
    let removed: Vec<_> = old.keys().filter(|name| !new.contains_key(*name)).collect();
    let changed: Vec<_> = new
        .iter()
        .filter_map(|(name, text)| old.get(name).filter(|before| *before != text).map(|_| name))
        .collect();
    json!({"added":added,"removed":removed,"changed":changed})
}

pub(crate) struct UpgradeRequest<'a> {
    pub(crate) id: &'a str,
    pub(crate) source: &'a str,
    pub(crate) expected_digest: &'a str,
    pub(crate) expected_generation: &'a str,
    pub(crate) expected_new_digest: &'a str,
    pub(crate) request_id: &'a str,
}

fn committed_upgrade_repeat(
    pm: &Pm,
    root: &Root,
    catalog: &Catalog,
    id: &InstallationId,
    request: &UpgradeRequest<'_>,
    new_digest: Option<&str>,
) -> Result<Option<Value>> {
    let Some(text) = root.read(&upgrade_journal_path(id, request.request_id), JOURNAL_CAP)? else {
        return Ok(None);
    };
    let journal: UpgradeJournal = decode(&text)?;
    if &journal.id != id
        || request.id != &**id
        || journal.request_id != request.request_id
        || journal.source != request.source
        || journal.expected_digest != request.expected_digest
        || journal.expected_generation != request.expected_generation
        || journal.expected_new_digest != request.expected_new_digest
        || bundle_digest(&journal.files) != request.expected_new_digest
        || new_digest.is_some_and(|digest| bundle_digest(&journal.files) != digest)
    {
        return Err(Error::rejected(
            "upgrade request ID was reused for different material",
        ));
    }
    // The worktree can contain the after-catalog when Git delivery failed.
    // Only a journal present at HEAD proves that this request committed.
    let committed_journal = crate::reaper::output(
        std::process::Command::new("git")
            .arg("-C")
            .arg(&pm.dir)
            .arg("show")
            .arg(format!(
                "HEAD:{}",
                upgrade_journal_path(id, request.request_id).display()
            )),
    )?;
    if !committed_journal.status.success() || committed_journal.stdout != text.as_bytes() {
        return Err(Error::rejected("workspace upgrade is staged; use app catalog upgrade-recover with its installation ID and request ID"));
    }
    let before: Catalog = decode(&journal.before_catalog)?;
    let after: Catalog = decode(&journal.after_catalog)?;
    before.validate()?;
    after.validate()?;
    let old_entry = before
        .installations
        .get(id)
        .ok_or_else(|| Error::rejected("retained upgrade journal lacks the old installation"))?;
    let new_entry = after
        .installations
        .get(id)
        .ok_or_else(|| Error::rejected("retained upgrade journal lacks the new installation"))?;
    let mut changed = before.clone();
    changed.installations.insert(id.clone(), new_entry.clone());
    if journal.schema != 1
        || changed != after
        || hash(&yaml(&before)?) != journal.expected_generation
        || old_entry.app != new_entry.app
        || old_entry.storage != Storage::Workspace
        || new_entry.storage != Storage::Workspace
        || old_entry.project.is_some()
        || new_entry.project.is_some()
        || new_entry.bundle_revision.as_deref()
            != Some(journal.expected_new_digest.trim_start_matches("sha256:"))
    {
        return Err(Error::rejected(
            "retained upgrade journal changes installation authority",
        ));
    }
    if catalog.installations.get(id) != Some(new_entry) {
        if required(root, Path::new(CATALOG), CATALOG_CAP)? == journal.before_catalog {
            return Err(Error::rejected("workspace upgrade is staged; use app catalog upgrade-recover with its installation ID and request ID"));
        }
        return Err(Error::rejected(
            "retained upgrade journal diverges from the published installation",
        ));
    }
    let (bundle, _) = new_entry.paths(id);
    if bundle_digest(&snapshot(root, &bundle, false)?) != journal.expected_new_digest {
        return Err(Error::rejected(
            "retained upgrade bundle differs from the committed digest",
        ));
    }
    let mut row = describe(root, catalog, id)?;
    row["committed"] = json!(true);
    row["idempotent"] = json!(true);
    row["structural_diff"] = journal.diff;
    row["compatibility"] = journal.compatibility;
    Ok(Some(row))
}

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

fn resolved_bundle(
    pm: &Pm,
    source: &str,
) -> Result<(BTreeMap<String, String>, app::Validated, app::Source)> {
    if !Path::new(source).is_absolute() && !source.contains("://") && !source.starts_with("git@") {
        return Err(Error::rejected(
            "workspace local source must be an absolute path",
        ));
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
    Ok((files, validated, provenance))
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
            bundle_revision: None,
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

/// Read-only proposal. The returned new digest is required by `upgrade`.
pub(crate) fn upgrade_check(
    pm: &Pm,
    id: &str,
    source: &str,
    expected_digest: &str,
    expected_generation: &str,
) -> Result<Value> {
    let id = InstallationId::parse(id)?;
    let (files, validated, provenance) = resolved_bundle(pm, source)?;
    let _lock = pm.lock()?;
    let root = Root::open(&pm.dir)?;
    let (catalog, _) = current(&root)?;
    let entry = catalog
        .installations
        .get(&id)
        .ok_or_else(|| Error::rejected("unknown installation ID"))?;
    if entry.storage != Storage::Workspace
        || entry.project.is_some()
        || entry.app != validated.manifest.app
    {
        return Err(Error::rejected(
            "upgrade proposal changes workspace app identity",
        ));
    }
    if hash(&yaml(&catalog)?) != expected_generation {
        return Err(Error::rejected("workspace catalog generation is stale"));
    }
    let (old_bundle, _) = entry.paths(&id);
    let old_files = snapshot(&root, &old_bundle, false)?;
    if bundle_digest(&old_files) != expected_digest {
        return Err(Error::rejected("workspace installation digest is stale"));
    }
    let new_digest = bundle_digest(&files);
    if new_digest == expected_digest {
        return Err(Error::rejected("upgrade bundle is unchanged"));
    }
    Ok(json!({"schema":1,"install_id":&*id,"name":entry.app,
        "version":validated.manifest.version,"source":provenance,
        "expected_digest":expected_digest,"expected_generation":expected_generation,
        "digest":new_digest,"structural_diff":structural_diff(&old_files,&files),
        "committed":false,"approved":false,"notes":validated.notes,
        "secret_warnings":crate::secret::warnings_json(&validated.secret_warnings)}))
}

/// The daemon owns the operator proof. `preflight` runs under the PM lock,
/// which is also the run-creation lock, before any upgrade journal is staged.
pub(crate) fn upgrade(
    pm: &Pm,
    request: &UpgradeRequest<'_>,
    preflight: impl FnOnce(&BTreeMap<String, String>, &app::Manifest) -> Result<Value>,
) -> Result<Value> {
    let id = InstallationId::parse(request.id)?;
    let source = request.source;
    let expected_digest = request.expected_digest;
    let expected_generation = request.expected_generation;
    let request_id = request.request_id;
    crate::proto::identifier(request_id, "upgrade request ID")?;
    if !Path::new(source).is_absolute() && !source.contains("://") && !source.starts_with("git@") {
        return Err(Error::rejected(
            "workspace local source must be an absolute path",
        ));
    }
    validate_source_transport(source)?;
    // Completed request replay must not depend on a mutable/missing source.
    {
        let _lock = pm.lock()?;
        let root = Root::open(&pm.dir)?;
        let (catalog, _) = current(&root)?;
        if let Some(row) = committed_upgrade_repeat(pm, &root, &catalog, &id, request, None)? {
            return Ok(row);
        }
    }
    let (files, validated, provenance) = resolved_bundle(pm, source)?;
    let new_digest = bundle_digest(&files);
    let _lock = pm.lock()?;
    let root = Root::open(&pm.dir)?;
    let (mut catalog, before_catalog) = current(&root)?;
    let before_catalog =
        before_catalog.ok_or_else(|| Error::rejected("workspace catalog is missing"))?;
    if let Some(row) =
        committed_upgrade_repeat(pm, &root, &catalog, &id, request, Some(&new_digest))?
    {
        return Ok(row);
    }
    let journal_path = upgrade_journal_path(&id, request_id);
    let entry = catalog
        .installations
        .get(&id)
        .ok_or_else(|| Error::rejected("unknown installation ID"))?;
    if entry.storage != Storage::Workspace || entry.project.is_some() {
        return Err(Error::rejected(
            "upgrade requires an exact workspace installation ID",
        ));
    }
    if validated.manifest.app != entry.app {
        return Err(Error::rejected(
            "upgrade cannot change the installed app identity",
        ));
    }
    if hash(&yaml(&catalog)?) != expected_generation {
        return Err(Error::rejected("workspace catalog generation is stale"));
    }
    let (old_bundle, record_path) = entry.paths(&id);
    let old_files = snapshot(&root, &old_bundle, false)?;
    if bundle_digest(&old_files) != expected_digest {
        return Err(Error::rejected("workspace installation digest is stale"));
    }
    if new_digest != request.expected_new_digest {
        return Err(Error::rejected(
            "proposed workspace bundle digest changed since upgrade check",
        ));
    }
    if new_digest == expected_digest {
        return Err(Error::rejected("upgrade bundle is unchanged"));
    }
    let compatibility = preflight(&files, &validated.manifest)?;
    let before_record = required(&root, &record_path, RECORD_CAP)?;
    let mut next_record = record(&before_record, &entry.app)?;
    if next_record.install_id != *id {
        return Err(Error::rejected(
            "upgrade record has a different installation ID",
        ));
    }
    next_record.source = provenance;
    next_record.updated_at = Some(crate::issue::time::iso(crate::issue::time::now_epoch()));
    let entry = catalog.installations.get_mut(&id).unwrap();
    entry.bundle_revision = Some(new_digest.trim_start_matches("sha256:").to_string());
    catalog.validate()?;
    let journal = UpgradeJournal {
        schema: 1,
        id: id.clone(),
        request_id: request_id.to_string(),
        source: source.to_string(),
        expected_digest: expected_digest.to_string(),
        expected_generation: expected_generation.to_string(),
        expected_new_digest: request.expected_new_digest.to_string(),
        before_catalog,
        after_catalog: yaml(&catalog)?,
        before_record,
        after_record: yaml(&next_record)?,
        diff: structural_diff(&old_files, &files),
        compatibility,
        files,
    };
    let journal_text = yaml(&journal)?;
    if journal_text.len() as u64 > JOURNAL_CAP {
        return Err(Error::rejected(
            "workspace upgrade journal exceeds its limit",
        ));
    }
    root.mkdir(Path::new(".apps/upgrade-journals"))?;
    root.mkdir(&Path::new(".apps/upgrade-journals").join(&*id))?;
    root.put(&journal_path, &journal_text)?;
    root.put(
        Path::new(UPGRADE_PENDING),
        &yaml(&json!({"install_id":&*id,"request_id":request_id}))?,
    ).map_err(|error| {
        Error::internal(format!("workspace upgrade marker incomplete ({}); retained journal can be resumed with `cadence app catalog upgrade-recover {} --request-id {}`", error.kind(), &*id, request_id))
    })?;
    let foreign = apply_upgrade(pm, &root, &journal).map_err(|error| {
        Error::internal(format!("workspace upgrade delivery incomplete ({}); recover with `cadence app catalog upgrade-recover {} --request-id {}`", error.kind(), &*id, request_id))
    })?;
    let mut row = describe(&root, &catalog, &id)?;
    row["committed"] = json!(true);
    row["foreign_files"] = json!(foreign);
    row["structural_diff"] = journal.diff;
    row["compatibility"] = journal.compatibility;
    Ok(row)
}

fn apply_upgrade(pm: &Pm, root: &Root, journal: &UpgradeJournal) -> Result<Vec<String>> {
    if journal.schema != 1 || journal.files.is_empty() || journal.files.len() > 128 {
        return Err(Error::rejected("unsupported workspace upgrade journal"));
    }
    if bundle_digest(&journal.files) != journal.expected_new_digest {
        return Err(Error::rejected(
            "upgrade journal bundle differs from the reviewed new digest",
        ));
    }
    let pending = required(root, Path::new(UPGRADE_PENDING), RECORD_CAP)?;
    let expected_pending =
        yaml(&json!({"install_id":&*journal.id,"request_id":journal.request_id}))?;
    if pending != expected_pending {
        return Err(Error::rejected("another workspace upgrade is pending"));
    }
    let before: Catalog = decode(&journal.before_catalog)?;
    let after: Catalog = decode(&journal.after_catalog)?;
    before.validate()?;
    after.validate()?;
    let old_entry = before
        .installations
        .get(&journal.id)
        .ok_or_else(|| Error::rejected("upgrade journal lacks the old installation"))?;
    let new_entry = after
        .installations
        .get(&journal.id)
        .ok_or_else(|| Error::rejected("upgrade journal lacks the new installation"))?;
    if old_entry.storage != Storage::Workspace
        || old_entry.project.is_some()
        || new_entry.storage != Storage::Workspace
        || new_entry.project.is_some()
        || old_entry.app != new_entry.app
        || new_entry.bundle_revision.as_deref()
            != Some(bundle_digest(&journal.files).trim_start_matches("sha256:"))
    {
        return Err(Error::rejected(
            "upgrade journal changes installation identity",
        ));
    }
    let mut changed = before.clone();
    changed
        .installations
        .insert(journal.id.clone(), new_entry.clone());
    if changed != after || hash(&yaml(&before)?) != journal.expected_generation {
        return Err(Error::rejected(
            "upgrade journal changes more than one installation",
        ));
    }
    let old = record(&journal.before_record, &old_entry.app)?;
    let next = record(&journal.after_record, &new_entry.app)?;
    if old.install_id != *journal.id
        || next.install_id != *journal.id
        || old.bindings != next.bindings
        || old.team != next.team
        || old.installed_at != next.installed_at
        || old.installed_by != next.installed_by
    {
        return Err(Error::rejected(
            "upgrade journal changes installation authority or identity",
        ));
    }
    let (old_bundle, record_path) = old_entry.paths(&journal.id);
    if bundle_digest(&snapshot(root, &old_bundle, false)?) != journal.expected_digest {
        return Err(Error::rejected(
            "old workspace bundle changed after upgrade staging",
        ));
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
    if validated.manifest.app != old_entry.app {
        return Err(Error::rejected("upgrade manifest changes app identity"));
    }
    for (name, text) in &journal.files {
        let path = Path::new(name);
        let parts: Vec<_> = path.components().collect();
        let valid = name == "app.md"
            || (parts.len() == 2
                && matches!(parts[0], std::path::Component::Normal(n) if n=="workflows" || n=="rubrics" || n=="templates")
                && matches!(parts[1], std::path::Component::Normal(n) if n.to_str().is_some_and(|leaf| !leaf.starts_with('.') && (!name.starts_with("workflows/") || (leaf.ends_with(".md") && model::valid_tag(leaf.trim_end_matches(".md")))))));
        if !valid || text.len() as u64 > crate::issue::plan::MAX_PLAN_BYTES as u64 {
            return Err(Error::rejected(
                "unsafe or oversized upgrade journal bundle file",
            ));
        }
    }
    let observed = required(root, Path::new(CATALOG), CATALOG_CAP)?;
    if observed != journal.before_catalog && observed != journal.after_catalog {
        return Err(Error::rejected(
            "workspace catalog diverged after upgrade staging",
        ));
    }
    let observed_record = required(root, &record_path, RECORD_CAP)?;
    if observed_record != journal.before_record && observed_record != journal.after_record {
        return Err(Error::rejected(
            "workspace record diverged after upgrade staging",
        ));
    }
    let (bundle, _) = new_entry.paths(&journal.id);
    let base = Path::new(".apps/installations").join(&*journal.id);
    root.mkdir(&base.join("revisions"))?;
    root.mkdir(bundle.parent().unwrap())?;
    root.mkdir(&bundle)?;
    let mut paths = vec![
        pm.dir.join(CATALOG),
        pm.dir
            .join(upgrade_journal_path(&journal.id, &journal.request_id)),
        pm.dir.join(&record_path),
    ];
    for (name, text) in &journal.files {
        let path = Path::new(name);
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            root.mkdir(&bundle.join(parent))?;
        }
        let target = bundle.join(path);
        match root.read(&target, CATALOG_CAP)? {
            Some(current) if current != *text => {
                return Err(Error::rejected("new bundle changed after staging"))
            }
            Some(_) => {}
            None => root.put(&target, text)?,
        }
        paths.push(pm.dir.join(target));
    }
    if snapshot(root, &bundle, false)? != journal.files {
        return Err(Error::rejected(
            "new bundle inventory differs from upgrade journal",
        ));
    }
    root.put(&record_path, &journal.after_record)?;
    root.put(Path::new(CATALOG), &journal.after_catalog)?;
    let foreign = write::commit(
        pm,
        &paths,
        &format!(
            "workspace app {} upgraded ({})",
            old_entry.app, &*journal.id
        ),
        &[],
        "operator",
    )?;
    root.remove(Path::new(UPGRADE_PENDING))?;
    Ok(foreign
        .into_iter()
        .filter(|path| path != UPGRADE_PENDING)
        .collect())
}

pub(crate) fn upgrade_recover(pm: &Pm, id: &str, request_id: &str) -> Result<Value> {
    let id = InstallationId::parse(id)?;
    crate::proto::identifier(request_id, "upgrade request ID")?;
    let _lock = pm.lock()?;
    let root = Root::open(&pm.dir)?;
    let journal: UpgradeJournal = decode(&required(
        &root,
        &upgrade_journal_path(&id, request_id),
        JOURNAL_CAP,
    )?)?;
    if journal.id != id || journal.request_id != request_id {
        return Err(Error::rejected(
            "upgrade journal identity differs from filename",
        ));
    }
    let pending_path = Path::new(UPGRADE_PENDING);
    if root.read(pending_path, RECORD_CAP)?.is_none() {
        no_pending(&root)?;
        let observed = required(&root, Path::new(CATALOG), CATALOG_CAP)?;
        if journal.schema != 1
            || bundle_digest(&journal.files) != journal.expected_new_digest
            || (observed != journal.before_catalog && observed != journal.after_catalog)
        {
            return Err(Error::rejected(
                "retained upgrade journal is not safe to resume",
            ));
        }
        root.put(
            pending_path,
            &yaml(&json!({"install_id":&*id,"request_id":request_id}))?,
        )?;
    }
    let foreign = apply_upgrade(pm, &root, &journal)?;
    let catalog = Catalog::load(&pm.dir)?;
    let mut row = describe(&root, &catalog, &id)?;
    row["committed"] = json!(true);
    row["foreign_files"] = json!(foreign);
    row["structural_diff"] = journal.diff;
    row["compatibility"] = journal.compatibility;
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
    let workflows = files
        .iter()
        .filter_map(|(path, text)| {
            path.strip_prefix("workflows/")
                .and_then(|name| name.strip_suffix(".md"))
                .map(|name| (name, text))
        })
        .map(|(name, text)| {
            let inputs = workflow::parse_template(text)
                .ok()
                .map(|template| workflow::inputs_json(&template));
            json!({"name":name,"inputs":inputs})
        })
        .collect::<Vec<_>>();
    Ok(
        json!({"schema":1,"workspace":"default","catalog_generation":hash(&yaml(catalog)?),"install_id":&**id,"name":manifest.app,"title":manifest.title,"version":manifest.version,"summary":manifest.summary,"project":entry.project,"project_link":entry.project,"storage_kind":if entry.storage==Storage::Workspace {"workspace"} else {"legacy"},"digest":bundle_digest(&files),"source":record.source,"installed_at":record.installed_at,"approval":{"state":if entry.storage==Storage::Workspace {"unapproved"} else {"unknown"}},"approved":if entry.storage==Storage::Workspace {json!(false)} else {Value::Null},"executable":false,"execution_note":"catalog execution is unavailable; existing legacy execution paths are unchanged","guide":manifest.guide,"capabilities":serde_json::to_value(&manifest.capabilities).unwrap_or(Value::Null),"connection_slots":manifest.connections,"record":record,"files":files.keys().collect::<Vec<_>>(),"workflows":workflows }),
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

/// Read an immutable installed revision for completed work. The current
/// catalog still proves the installation identity; only exact retained bundle
/// bytes may supply the old app contract. Active runs always use the current
/// runtime snapshot above.
pub(crate) fn with_completed_bundle_snapshot<T>(
    pm: &Pm,
    id: &str,
    digest: &str,
    callback: impl FnOnce(&Value, &BTreeMap<String, String>) -> Result<T>,
) -> Result<T> {
    let id = InstallationId::parse(id)?;
    let revision = digest
        .strip_prefix("sha256:")
        .filter(|s| {
            s.len() == 64
                && s.bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        })
        .ok_or_else(|| Error::rejected("historical bundle digest is invalid"))?;
    let _lock = pm.lock()?;
    let root = Root::open(&pm.dir)?;
    let catalog = Catalog::load(&pm.dir)?;
    no_pending(&root)?;
    let current = describe(&root, &catalog, &id)?;
    let entry = &catalog.installations[&id];
    if entry.storage != Storage::Workspace {
        return Err(Error::rejected(
            "historical bundle needs a workspace installation",
        ));
    }
    let bundle = if current["digest"] == digest {
        entry.paths(&id).0
    } else {
        let base = Path::new(".apps/installations").join(&*id);
        let original = base.join("bundle");
        let old_files = snapshot(&root, &original, false)?;
        if bundle_digest(&old_files) == digest {
            original
        } else {
            base.join("revisions").join(revision).join("bundle")
        }
    };
    let files = snapshot(&root, &bundle, false)?;
    if bundle_digest(&files) != digest {
        return Err(Error::rejected(
            "retained historical bundle differs from its frozen digest",
        ));
    }
    let manifest = app::parse_manifest(
        files
            .get("app.md")
            .ok_or_else(|| Error::rejected("historical bundle manifest is missing"))?,
    )?;
    if manifest.app != entry.app {
        return Err(Error::rejected(
            "historical bundle changes installation identity",
        ));
    }
    callback(&json!({"digest":digest}), &files)
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORKFLOW: &str = "---\ntitle: \"Post: {{topic}}\"\ngoal: \"Publish {{topic}} for {{keyword}}\"\n\
inputs:\n  topic: { ask: \"About what?\" }\n  keyword: { ask: \"Phrase\", optional: true }\n---\n\n\
Why.\n\n## Research {{topic}}\nagent: dev-1\nsize: S\n\nDo it.\n\n### Acceptance\n- [ ] brief written\n\n\
## Write\nagent: dev-2\ndepends_on: 1\n\n### Acceptance\n- [ ] post done\n";
    const MANIFEST_A: &str = "---\napp: fixture-app\ntitle: Fixture\nversion: '1'\nneeds:\n  connections: [cms]\n  capabilities:\n    publication: {schema: 1, capability: text.publish, version: 1, action: publish, resource_kind: connection_account, effect: send}\n---\n\nGuide.\n";
    const MANIFEST_B: &str = "---\napp: fixture-app\ntitle: Fixture\nversion: '2'\nneeds:\n  connections: []\n  capabilities:\n    publication: {schema: 1, capability: text.publish, version: 1, action: publish, resource_kind: connection_account, effect: send}\n    source: {schema: 1, capability: social.read, version: 1, action: list_posts, resource_kind: connection_account, effect: read}\n---\n\nGuide.\n";

    fn bundle(dir: &std::path::Path, manifest: &str) {
        std::fs::create_dir_all(dir.join("workflows")).unwrap();
        std::fs::write(dir.join("app.md"), manifest).unwrap();
        std::fs::write(dir.join("workflows").join("do.md"), WORKFLOW).unwrap();
    }

    fn installed() -> (
        tempfile::TempDir,
        tempfile::TempDir,
        tempfile::TempDir,
        String,
    ) {
        let pm_dir = tempfile::tempdir().unwrap();
        let state_dir = tempfile::tempdir().unwrap();
        // The tracker refuses its own tree as an installation source.
        let sources = tempfile::tempdir().unwrap();
        let pm = Pm::init(pm_dir.path()).unwrap();
        let source = sources.path().join("bundle-a");
        bundle(&source, MANIFEST_A);
        let out = install(&pm, state_dir.path(), source.to_str().unwrap()).unwrap();
        let id = out["install_id"].as_str().unwrap().to_string();
        (pm_dir, state_dir, sources, id)
    }

    /// CAD-585: the show receipt exposes the declared slot contract —
    /// typed capability slots plus untyped legacy slots — exactly as the
    /// installed manifest declares them. Without the projection these
    /// fields are absent and the board cannot match slots to reviewed
    /// provider capabilities.
    #[test]
    fn show_exposes_declared_slot_capabilities_exactly() {
        let (pm_dir, _state, _sources, id) = installed();
        let pm = Pm::at(pm_dir.path()).unwrap();
        let shown = show(&pm, &id).unwrap();
        let publication = &shown["capabilities"]["publication"];
        assert_eq!(publication["capability"], "text.publish");
        assert_eq!(publication["version"], 1);
        assert_eq!(publication["action"], "publish");
        assert_eq!(publication["resource_kind"], "connection_account");
        assert_eq!(publication["effect"], "send");
        assert_eq!(shown["connection_slots"], json!(["cms"]));
    }

    /// The projection derives from the live bundle snapshot, never a
    /// stored record: after an upgrade the receipt reflects the new
    /// manifest, so a stale cached contract cannot survive a rebind.
    #[test]
    fn show_reflects_upgraded_manifest_capabilities() {
        let (pm_dir, _state, sources, id) = installed();
        let pm = Pm::at(pm_dir.path()).unwrap();
        let before = show(&pm, &id).unwrap();
        let source_b = sources.path().join("bundle-b");
        bundle(&source_b, MANIFEST_B);
        let digest = before["digest"].as_str().unwrap();
        let generation = before["catalog_generation"].as_str().unwrap();
        let check =
            upgrade_check(&pm, &id, source_b.to_str().unwrap(), digest, generation).unwrap();
        let new_digest = check["digest"].as_str().unwrap().to_string();
        upgrade(
            &pm,
            &UpgradeRequest {
                id: &id,
                source: source_b.to_str().unwrap(),
                expected_digest: digest,
                expected_generation: generation,
                expected_new_digest: &new_digest,
                request_id: "fixture-upgrade",
            },
            |_, _| Ok(json!({})),
        )
        .unwrap();
        let after = show(&pm, &id).unwrap();
        assert_eq!(after["capabilities"]["source"]["capability"], "social.read");
        assert_eq!(after["capabilities"]["source"]["effect"], "read");
        assert_eq!(after["connection_slots"], json!([]));
    }

    /// Forged or unknown installation IDs never describe a bundle —
    /// the exact-ID parse refuses path traversal and the lookup
    /// refuses anything that is not a live installation.
    #[test]
    fn show_refuses_forged_and_unknown_installation_ids() {
        let (pm_dir, _state, _sources, _id) = installed();
        let pm = Pm::at(pm_dir.path()).unwrap();
        for forged in ["", "install-a", "../catalog", "a/b", "not-a-uuid"] {
            assert!(show(&pm, forged).is_err(), "admitted {forged}");
        }
        assert!(show(&pm, "0123456789abcdef0123456789abcdef").is_err());
    }
}
