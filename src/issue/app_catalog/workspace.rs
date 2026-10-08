//! Operator-owned workspace installation transport. Execution is deliberately absent.
use super::*;
use crate::issue::{app, app_view, workflow, write};
use crate::store::Store;
use serde_json::{json, Value};

/// CAD-1194: the install-check source form that names a host built-in.
const BUILTIN_SOURCE_PREFIX: &str = "builtin:";
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
/// CAD-1129: the bundle digest the explorer's catalog check and
/// update-check compute — `pub(crate)` so the daemon's explorer RPC
/// shares it.
pub(crate) fn bundle_digest(files: &BTreeMap<String, String>) -> String {
    let mut digest = Sha256::new();
    for (name, body) in files {
        digest.update((name.len() as u64).to_be_bytes());
        digest.update(name.as_bytes());
        digest.update((body.len() as u64).to_be_bytes());
        digest.update(body.as_bytes());
    }
    format!("sha256:{:x}", digest.finalize())
}

/// Create `dir`'s parent chain under `root`, one validated prefix at a
/// time — `Root::mkdir` is descriptor-relative `mkdirat` and only makes a
/// leaf, so a nested member like `screens/<tag>/<leaf>` needs `screens/`
/// then `screens/<tag>/` created first. Each intermediate is itself
/// created through `Root::mkdir` (O_NOFOLLOW descriptor-relative), never
/// `create_dir_all` — a symlinked or non-dir prefix refuses at `dir()`.
fn mkdir_parents(root: &Root, dir: &Path) -> Result<()> {
    let mut cur = PathBuf::new();
    for part in dir.components() {
        let std::path::Component::Normal(_) = part else {
            continue;
        };
        cur.push(part);
        root.mkdir(&cur)?;
    }
    Ok(())
}

/// The lexical grammar a journaled bundle member's rel-path must satisfy
/// — the flat `app.md`/`workflows/<tag>.md`/`rubrics|templates/<leaf>`
/// plus CAD-1006's `screens/<tag>/<leaf>` (the one depth-3 member: a
/// tag-named package dir holding `screens.json` or a
/// `<stem>.<js|css|svg|json>` leaf). `Path::components` already drops
/// `.`/`..`/slashes, so a member that survives the components grammar is
/// normal by construction.
fn member_path_ok(name: &str) -> bool {
    let path = Path::new(name);
    let parts: Vec<_> = path.components().collect();
    let normal = |i: usize| -> Option<&str> {
        match parts.get(i) {
            Some(std::path::Component::Normal(n)) => n.to_str(),
            _ => None,
        }
    };
    if name == "app.md"
        || name == crate::issue::app_chat::FILE
        || name == crate::issue::app_assistant::FILE
    {
        return true;
    }
    match (parts.len(), normal(0)) {
        (2, Some(top)) if matches!(top, "workflows" | "rubrics" | "templates" | "views") => {
            normal(1).is_some_and(|leaf| {
                !leaf.starts_with('.')
                    && match top {
                        // workflows are tag-named Markdown.
                        "workflows" => {
                            leaf.ends_with(".md") && model::valid_tag(leaf.trim_end_matches(".md"))
                        }
                        // views/ holds exactly the app-views/v1 descriptor.
                        "views" => leaf == crate::issue::app_view::FILE,
                        _ => true,
                    }
            })
        }
        // CAD-1129: assets/<leaf>.svg — the catalog's flat SVG assets.
        (2, Some("assets")) => normal(1).is_some_and(|leaf| {
            !leaf.starts_with('.') && leaf.len() <= 128 && leaf.ends_with(".svg")
        }),
        // screens/<tag>/<leaf> — tag-validated dir + package leaf grammar.
        (3, Some("screens")) => {
            normal(1).is_some_and(model::valid_tag)
                && normal(2).is_some_and(|leaf| {
                    !leaf.starts_with('.') && crate::issue::app_screen_pkg::leaf_ok(leaf)
                })
        }
        _ => false,
    }
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
        let text = required(root, &path, app::file_cap(&name))?;
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
            // CAD-1006: screens/<tag>/<leaf> — the one nested level. The
            // `<tag>` is a tag-named package dir; each leaf is `screens.json`
            // or a `<stem>.<js|css|svg|json>` file per the package grammar.
            Some(libc::S_IFDIR) if name.as_str() == "screens" => {
                for tag in root.list(&path, &mut budget)? {
                    if tag.starts_with('.') || !model::valid_tag(&tag) {
                        return Err(Error::rejected(
                            "a screen package is a tag-named screens/<tag>/ directory",
                        ));
                    }
                    let tagdir = path.join(&tag);
                    if root.kind(&tagdir)? != Some(libc::S_IFDIR) {
                        return Err(Error::rejected("screens/<tag> must be a directory"));
                    }
                    for leaf in root.list(&tagdir, &mut budget)? {
                        if leaf.starts_with('.') || !crate::issue::app_screen_pkg::leaf_ok(&leaf) {
                            return Err(Error::rejected(
                                "a screen asset is <stem>.<js|css|svg|json>, or screens.json",
                            ));
                        }
                        add(format!("screens/{tag}/{leaf}"), tagdir.join(leaf))?;
                    }
                }
            }
            Some(libc::S_IFDIR)
                if matches!(
                    name.as_str(),
                    "workflows" | "rubrics" | "templates" | "views" | "assets"
                ) =>
            {
                for leaf in root.list(&path, &mut budget)? {
                    if leaf.starts_with('.')
                        || (name == "workflows"
                            && (!leaf.ends_with(".md")
                                || !model::valid_tag(leaf.trim_end_matches(".md"))))
                        // views/ holds exactly one file — the filename
                        // pins the descriptor contract version.
                        || (name == "views" && leaf != app_view::FILE)
                        || (name == "assets" && (!leaf.ends_with(".svg") || leaf.len() > 128))
                    {
                        return Err(Error::rejected(
                            "workflows must be named Markdown files, views/ holds exactly app-views-v1.json, assets flat *.svg, and all bundle entries must be visible flat text files",
                        ));
                    }
                    add(format!("{name}/{leaf}"), path.join(leaf))?;
                }
            }
            Some(libc::S_IFREG)
                if name == "app.md"
                    || name == crate::issue::app_chat::FILE
                    || name == crate::issue::app_assistant::FILE =>
            {
                add(name, path)?
            }
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
    if text.is_none() {
        let legacy = legacy_records(root)?;
        if !legacy.is_empty() {
            return Err(legacy_unmigrated(&legacy));
        }
    }
    for id in catalog.installations.keys() {
        catalog.require_current(root)?;
        catalog.installation(root, id)?;
    }
    Ok((catalog, text))
}

/// CAD-1186 legacy rule: while the workspace catalog does not exist, ANY
/// unmigrated `<project>/apps/<name>` installation blocks every workspace
/// install (not only a same-named one), because a first workspace install
/// would create a catalog that does not account for them. Once `migrate` has
/// catalogued them they carry their own install IDs and never block a
/// workspace install of the same name. The message names this rule and the
/// next command.
fn legacy_unmigrated(legacy: &[(String, String, String)]) -> Error {
    let mut names: Vec<String> = legacy.iter().map(|(p, n, _)| format!("{p}/{n}")).collect();
    names.sort();
    names.dedup();
    let shown = names.iter().take(5).cloned().collect::<Vec<_>>().join(", ");
    let more = names.len().saturating_sub(5);
    let more = if more > 0 {
        format!(" and {more} more")
    } else {
        String::new()
    };
    Error::rejected(format!("unmigrated legacy project installation(s) ({shown}{more}) block every workspace install until they are catalogued; run `cadence app catalog migrate` first, then repeat this command. After migration a legacy install never blocks a workspace install of the same name"))
}

fn same_name_workspace(catalog: &Catalog, app: &str) -> Result<()> {
    for (id, entry) in &catalog.installations {
        if entry.storage == Storage::Workspace && entry.app == app {
            return Err(Error::rejected(format!("app '{}' already has workspace installation {}; replacement/upgrade must explicitly target that ID and is not supported in this increment", entry.app, &**id)));
        }
    }
    Ok(())
}

/// The one digest gate shared by install and the board relay. A malformed pin
/// is refused as such; a well-formed pin must equal the resolved bytes.
fn check_expected_digest(expected: &str, actual: &str) -> Result<()> {
    let hex = expected.strip_prefix("sha256:").unwrap_or("");
    if hex.len() != 64 || !hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return Err(Error::rejected(
            "expected digest must be 'sha256:' followed by 64 lowercase hex digits, as returned by `cadence app catalog install-check`",
        ));
    }
    if expected != actual {
        return Err(Error::rejected(format!("bundle digest {actual} differs from the expected digest {expected}; the source changed since `cadence app catalog install-check`. Nothing was installed; re-run install-check and confirm the new digest")));
    }
    Ok(())
}

/// Read-only install proposal (CAD-1186). Resolves and validates exactly as
/// `install` does and returns the digest `--expected-digest` must carry. It
/// writes nothing: no catalog, journal or lock file is created.
///
/// CAD-1194: `builtin:<catalog_id>` names a host built-in, so the board's
/// built-in install pins a digest through this same check.
pub(crate) fn install_check(pm: &Pm, source: &str) -> Result<Value> {
    let (files, validated, provenance) = match source.strip_prefix(BUILTIN_SOURCE_PREFIX) {
        Some(catalog_id) => builtin_bundle(pm, catalog_id)?,
        None => resolved_bundle(pm, source)?,
    };
    optimistic(pm, || {
        let root = Root::open(&pm.dir)?;
        let (catalog, _) = current(&root)?;
        same_name_workspace(&catalog, &validated.manifest.app)
    })?;
    Ok(json!({"schema":1,"name":validated.manifest.app,
        "version":validated.manifest.version,"source":provenance,
        "digest":bundle_digest(&files),
        "files":files.keys().collect::<Vec<_>>(),
        "committed":false,"compatibility":validated.compatibility,"notes":validated.notes,
        "secret_warnings":crate::secret::warnings_json(&validated.secret_warnings)}))
}

pub(crate) fn install(
    pm: &Pm,
    state: &Path,
    source: &str,
    expected_digest: Option<&str>,
) -> Result<Value> {
    let _ = state; // Caller must pass the daemon's connection-bound operator gate.
    let (files, validated, provenance) = resolved_bundle(pm, source)?;
    install_resolved(pm, files, validated, provenance, expected_digest)
}

/// The one journaled install apply, shared by a source install and a
/// built-in install: the digest gate runs first (before the lock, the
/// catalog read or any write), then the same-name refusal, the journal
/// and the apply.
fn install_resolved(
    pm: &Pm,
    files: BTreeMap<String, String>,
    validated: app::Validated,
    provenance: app::Source,
    expected_digest: Option<&str>,
) -> Result<Value> {
    // Refuse a changed bundle before the lock, the catalog read or any write.
    if let Some(expected) = expected_digest {
        check_expected_digest(expected, &bundle_digest(&files))?;
    }
    let _lock = pm.lock()?;
    let root = Root::open(&pm.dir)?;
    let (mut catalog, before_catalog) = current(&root)?;
    same_name_workspace(&catalog, &validated.manifest.app)?;
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
        removed: None,
        restore_after: None,
        purge_after: None,
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
    let (entry_name, old_files) = optimistic(pm, || {
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
        // Published state must still be what was read.
        catalog.require_current(&root)?;
        let name = entry.app.clone();
        Ok((name, old_files))
    })?;
    let new_digest = bundle_digest(&files);
    if new_digest == expected_digest {
        return Err(Error::rejected("upgrade bundle is unchanged"));
    }
    Ok(json!({"schema":1,"install_id":&*id,"name":entry_name,
        "version":validated.manifest.version,"source":provenance,
        "expected_digest":expected_digest,"expected_generation":expected_generation,
        "digest":new_digest,"structural_diff":structural_diff(&old_files,&files),
        "committed":false,"approved":false,"compatibility":validated.compatibility,"notes":validated.notes,
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
        if !member_path_ok(name) || text.len() as u64 > app::file_cap(name) {
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
            mkdir_parents(root, &bundle.join(parent))?;
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
        if !member_path_ok(name) || text.len() as u64 > app::file_cap(name) {
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
        if !member_path_ok(name) || text.len() as u64 > CATALOG_CAP {
            return Err(Error::rejected("unsafe or oversized journal bundle file"));
        }
        let path = Path::new(name);
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            mkdir_parents(root, &bundle.join(parent))?;
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

/// CAD-1129: re-export `describe` so the explorer's `app_home` reads
/// one install's row under the catalog, the same shape `show` returns.
/// The daemon opens its own PM descriptor — `Root` stays catalog-
/// private.
pub(crate) fn describe_id(pm: &Pm, catalog: &Catalog, id: &InstallationId) -> Result<Value> {
    let root = Root::open(&pm.dir)?;
    describe(&root, catalog, id)
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
    let compatibility = match app::compat_host() {
        Ok(host) => manifest.requires.report(&host),
        Err(error) => manifest.requires.unknown_report(&error.to_string()),
    };
    let reread: Record = decode(&required(root, &path, RECORD_CAP)?)?;
    if serde_yaml::to_value(&reread).map_err(|e| Error::internal(e.to_string()))?
        != serde_yaml::to_value(&record).map_err(|e| Error::internal(e.to_string()))?
    {
        return Err(Error::invalid(
            super::RECORD_CHANGED,
            "installation record changed during inspection",
        ));
    }
    // CAD-864: the installed descriptor rides the receipt as validated
    // data — re-parsed from the bundle snapshot every read, so a
    // hand-edited installed file that no longer meets the contract
    // surfaces as this read's refusal rather than stale served bytes.
    // The read re-proves the pairing too: a descriptor file whose
    // manifest no longer declares `needs.views`, or one naming another
    // app, is a tampered install — refuse rather than serve it.
    let (view_descriptor, view_descriptor_digest) = match files.get(app_view::REL_PATH) {
        Some(text) => {
            if manifest.view_contract.as_deref() != Some(app_view::CONTRACT) {
                return Err(Error::rejected(format!(
                    "installed bundle carries {} that its manifest does not declare",
                    app_view::REL_PATH
                )));
            }
            let descriptor = app_view::parse_descriptor(text)
                .map_err(|e| Error::rejected(format!("installed {}: {e}", app_view::REL_PATH)))?;
            if descriptor.app != manifest.app {
                return Err(Error::rejected(format!(
                    "installed {} names app '{}' — this installation is '{}'",
                    app_view::REL_PATH,
                    descriptor.app,
                    manifest.app
                )));
            }
            (descriptor.raw, json!(format!("sha256:{}", hash(text))))
        }
        None => {
            if manifest.view_contract.is_some() {
                return Err(Error::rejected(format!(
                    "installed manifest declares `needs.views` but {} is absent",
                    app_view::REL_PATH
                )));
            }
            (Value::Null, Value::Null)
        }
    };
    let workflows = files
        .iter()
        .filter_map(|(path, text)| {
            path.strip_prefix("workflows/")
                .and_then(|name| name.strip_suffix(".md"))
                .map(|name| (name, text))
        })
        .map(|(name, text)| {
            let template = workflow::parse_template(text).ok();
            let inputs = template.as_ref().map(workflow::inputs_json);
            // CAD-1123 HP2/HP3: what a host-drawn approval slot needs, all
            // from the installed bundle: the workflow's own label and the
            // capability slots a run of it freezes quotes for.
            let label = template.as_ref().and_then(|t| t.label.clone());
            let distinct = template
                .as_ref()
                .map(|t| t.distinct.clone())
                .unwrap_or_default();
            let capability_slots = template
                .as_ref()
                .map(|t| t.capability_slots.clone())
                .unwrap_or_default();
            // CAD-1123: the digest a run snapshot records as its
            // workflow `source_digest`, so a reader can name the run's
            // workflow without the snapshot carrying the file name.
            let source_digest = crate::store::app_runs::artifact_digest(text.as_bytes());
            // CAD-1171: the run form reads `execution` — a host workflow
            // runs in-process for the operator's click (no PM/worker).
            let execution = template
                .as_ref()
                .map(|t| t.execution.as_str())
                .unwrap_or("agent");
            json!({"name":name,"inputs":inputs,"source_digest":source_digest,"label":label,"capability_slots":capability_slots,"distinct":distinct,"execution":execution})
        })
        .collect::<Vec<_>>();
    Ok(
        json!({"schema":1,"workspace":"default","catalog_generation":hash(&yaml(catalog)?),"install_id":&**id,"name":manifest.app,"title":manifest.title,"version":manifest.version,"summary":manifest.summary,"project":entry.project,"project_link":entry.project,"storage_kind":if entry.storage==Storage::Workspace {"workspace"} else {"legacy"},"digest":bundle_digest(&files),"view_descriptor":view_descriptor,"view_descriptor_digest":view_descriptor_digest,"compatibility":compatibility,"source":record.source,"installed_at":record.installed_at,"removed":record.removed,"restore_after":record.restore_after,"purge_after":record.purge_after,"approval":{"state":if entry.storage==Storage::Workspace {"unapproved"} else {"unknown"}},"approved":if entry.storage==Storage::Workspace {json!(false)} else {Value::Null},"executable":false,"execution_note":"catalog execution is unavailable; existing legacy execution paths are unchanged","guide":manifest.guide,"capabilities":serde_json::to_value(&manifest.capabilities).map_err(|e| Error::internal(format!("installed slot contract is not serializable: {e}")))?,"connection_slots":manifest.connections,"listing":manifest.listing.as_ref().map(|l| l.value.clone()),"record":record,"files":files.keys().collect::<Vec<_>>(),"workflows":workflows }),
    )
}
/// CAD-1189: catalog reads take no lock. Every writer journals first
/// (`*-pending.yaml`) and publishes `catalog.yaml` atomically, so a read
/// that races one fails a freshness check; it is retried a few times and
/// then answers `busy` (retryable), never a mix of old and new state.
const READ_ATTEMPTS: u32 = 3;

fn is_pending(error: &Error) -> bool {
    matches!(
        error.code(),
        Some(super::PENDING_UPGRADE | super::PENDING_INSTALL | super::PENDING_PUBLICATION)
    )
}

fn changed_underneath(error: &Error) -> bool {
    matches!(
        error.code(),
        Some(
            super::CATALOG_NOT_CURRENT
                | super::RECORD_CHANGED
                | super::ADMISSION_CHANGED
                | super::LEGACY_CHANGED
        )
    )
}

fn optimistic<T>(pm: &Pm, mut read: impl FnMut() -> Result<T>) -> Result<T> {
    let mut attempt = 1;
    loop {
        match read() {
            // A pending journal is transient only while a live writer holds
            // the PM write flock. With no holder it is a crashed leftover:
            // one more read (the writer may just have finished), then the
            // original recovery refusal, never `busy`.
            Err(error) if is_pending(&error) && !pm.write_lock_held() => return read(),
            Err(error) if is_pending(&error) || changed_underneath(&error) => {
                if attempt >= READ_ATTEMPTS {
                    return Err(Error::busy(format!(
                        "app catalog kept changing while it was read; retry ({error})"
                    )));
                }
                let jitter = (uuid::Uuid::new_v4().as_u128() % 20) as u64;
                std::thread::sleep(std::time::Duration::from_millis(
                    20 * attempt as u64 + jitter,
                ));
                attempt += 1;
            }
            other => return other,
        }
    }
}

/// Legacy entries (`<project>/apps/<name>`) are rewritten in place by the
/// `app` writers under the PM lock, with no journal and an unchanged
/// catalog, so no catalog freshness check can see a torn read. Those
/// writers bump a seqlock counter (`Pm::begin_legacy_write`): odd while
/// they rewrite, the next even value after. A read accepts its result only
/// when the counter was even before it and unchanged after it. An odd
/// counter with no live writer is a crash leftover: that one attempt reads
/// under the PM lock (the old behaviour) instead of reporting busy forever.
/// A read of workspace entries only is unaffected by the counter apart from
/// a spurious retry.
fn read_catalog<T>(pm: &Pm, mut read: impl FnMut() -> Result<T>) -> Result<T> {
    optimistic(pm, || {
        let before = match pm.legacy_generation() {
            Some(g) if g % 2 == 0 => g,
            _ if !pm.write_lock_held() => {
                let _lock = pm.lock()?;
                return read();
            }
            _ => {
                return Err(Error::invalid(
                    super::LEGACY_CHANGED,
                    "legacy app files changed during read",
                ))
            }
        };
        let result = read();
        if pm.legacy_generation() != Some(before) {
            return Err(Error::invalid(
                super::LEGACY_CHANGED,
                "legacy app files changed during read",
            ));
        }
        result
    })
}

pub fn list(pm: &Pm) -> Result<Value> {
    read_catalog(pm, || {
        let catalog = Catalog::load(&pm.dir)?;
        let root = Root::open(&pm.dir)?;
        let rows = catalog
            .installations
            .keys()
            .map(|id| describe(&root, &catalog, id))
            .collect::<Result<Vec<_>>>()?;
        catalog.require_current(&root)?;
        Ok(json!(rows))
    })
}
pub fn show(pm: &Pm, id: &str) -> Result<Value> {
    let id = InstallationId::parse(id)?;
    read_catalog(pm, || {
        let catalog = Catalog::load(&pm.dir)?;
        let root = Root::open(&pm.dir)?;
        describe(&root, &catalog, &id)
    })
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
#[track_caller]
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
    // CAD-1129 H5: a soft-removed install admits no runtime snapshot —
    // run creation, dispatch and consent checks all stop here.
    if description["removed"].as_i64().is_some() {
        return Err(Error::rejected(
            "installation is removed — restore it before any app action",
        ));
    }
    let (bundle, _) = catalog.installations[&id].paths(&id);
    let files = snapshot(&root, &bundle, false)?;
    if bundle_digest(&files) != description["digest"].as_str().unwrap_or("") {
        return Err(Error::invalid(
            super::ADMISSION_CHANGED,
            "installation changed during runtime admission",
        ));
    }
    callback(&description, &files)
}

/// CAD-1212: test seam for `with_runtime_read`. A check installs a hook that
/// runs after `describe` and before the bundle `snapshot` (once per retry
/// attempt), so it can modify a bundle file at exactly the point the digest
/// compare guards. Compiled to nothing outside `cfg(test)`.
pub(crate) mod read_seam {
    #[cfg(test)]
    use std::cell::RefCell;

    #[cfg(test)]
    type Hook = Box<dyn FnMut()>;

    #[cfg(test)]
    thread_local! {
        static HOOK: RefCell<Option<Hook>> = const { RefCell::new(None) };
    }

    #[inline]
    pub(super) fn between_describe_and_snapshot() {
        #[cfg(test)]
        HOOK.with(|h| {
            if let Some(f) = h.borrow_mut().as_mut() {
                f();
            }
        });
    }

    /// Install (or clear) the callback for this thread.
    #[cfg(test)]
    pub(crate) fn set_hook(hook: Option<Hook>) {
        HOOK.with(|h| *h.borrow_mut() = hook);
    }
}

/// CAD-1189: lock-free runtime read for callbacks that write nothing. It
/// makes the same checks as `with_runtime_snapshot` (pending journal,
/// catalog generation, record re-read, bundle-digest compare) under the
/// optimistic retry, but takes no PM lock. A callback that creates or
/// changes state must use `with_runtime_snapshot`, which the PM lock orders
/// against install, upgrade, remove and revoke.
pub(crate) fn with_runtime_read<T>(
    pm: &Pm,
    id: &str,
    callback: impl FnOnce(&Value, &BTreeMap<String, String>) -> Result<T>,
) -> Result<T> {
    let id = InstallationId::parse(id)?;
    let (description, files) = read_catalog(pm, || {
        let root = Root::open(&pm.dir)?;
        let catalog = Catalog::load(&pm.dir)?;
        no_pending(&root)?;
        let description = describe(&root, &catalog, &id)?;
        read_seam::between_describe_and_snapshot();
        // CAD-1129 H5: the lock-free read funnel refuses a soft-removed
        // install exactly like `with_runtime_snapshot`.
        if description["removed"].as_i64().is_some() {
            return Err(Error::rejected(
                "installation is removed — restore it before any app action",
            ));
        }
        let (bundle, _) = catalog.installations[&id].paths(&id);
        let files = snapshot(&root, &bundle, false)?;
        if bundle_digest(&files) != description["digest"].as_str().unwrap_or("") {
            return Err(Error::invalid(
                super::ADMISSION_CHANGED,
                "installation changed during runtime admission",
            ));
        }
        Ok((description, files))
    })?;
    callback(&description, &files)
}

/// Read an immutable installed revision for completed work. The current
/// catalog still proves the installation identity; only exact retained bundle
/// bytes may supply the old app contract. Active runs always use the current
/// runtime snapshot above.
#[track_caller]
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
    // A soft-removed install admits no historical snapshot either —
    // completed-work receipts stop with the live install.
    if current["removed"].as_i64().is_some() {
        return Err(Error::rejected(
            "installation is removed — restore it before any app action",
        ));
    }
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

/// CAD-1129: the host's embedded built-in bundle `catalog_id`, resolved
/// and validated like a source bundle. The bytes come from
/// `include_str!`, never a checkout or a caller path.
fn builtin_bundle(
    pm: &Pm,
    catalog_id: &str,
) -> Result<(BTreeMap<String, String>, app::Validated, app::Source)> {
    let entry =
        super::builtin::get(catalog_id)?.ok_or_else(|| Error::rejected("unknown catalog entry"))?;
    let files = entry.files;
    let (agents, agent_sources) = workflow::known_agents(&pm.dir, None, &[]);
    let validated = app::validate_texts(
        files
            .iter()
            .map(|(name, text)| (name.clone(), text.clone()))
            .collect(),
        &agents,
        &agent_sources,
    )?;
    let provenance = app::Source::Builtin {
        id: entry.id.to_string(),
        digest: bundle_digest(&files),
    };
    Ok((files, validated, provenance))
}

/// CAD-1129/1194: install the host's embedded built-in bundle
/// `catalog_id` — `app_workspace_install_entry` with `source`
/// pre-resolved by the daemon. The same journaled apply and the same
/// digest gate as `install`; `expected_digest` is the pin the
/// `builtin:<id>` install-check returned and is not optional here, so a
/// board install of a built-in always consents to checked bytes. A
/// second workspace install of the same app refuses.
pub(crate) fn install_builtin(pm: &Pm, catalog_id: &str, expected_digest: &str) -> Result<Value> {
    let (files, validated, provenance) = builtin_bundle(pm, catalog_id)?;
    install_resolved(pm, files, validated, provenance, Some(expected_digest))
}

/// CAD-1129 H4: re-check the install's recorded `source` upstream —
/// never a caller-chosen URL. For `Source::Git` the same URL + `dir`
/// re-resolves `HEAD` to a commit and re-fetches; `Source::Builtin`
/// answers "current" until a later built-in catalog version ships.
/// A `Source::Path` is a local checkout — no upstream to check.
pub(crate) fn update_check(pm: &Pm, install_id: &str) -> Result<Value> {
    let id = InstallationId::parse(install_id)?;
    let _lock = pm.lock()?;
    let root = Root::open(&pm.dir)?;
    let catalog = Catalog::load(&pm.dir)?;
    let entry = catalog
        .installations
        .get(&id)
        .ok_or_else(|| Error::rejected("unknown installation ID"))?;
    if entry.storage != Storage::Workspace {
        return Err(Error::rejected(
            "update check needs a workspace installation",
        ));
    }
    let desc = describe(&root, &catalog, &id)?;
    let current_digest = desc["digest"].as_str().unwrap_or_default().to_string();
    let record: Record = serde_json::from_value(desc["record"].clone())
        .map_err(|e| Error::internal(format!("installation record is not readable: {e}")))?;
    let source = record.source;
    match source {
        app::Source::Builtin { id: cat_id, digest } => {
            let builtin = crate::issue::app_catalog::builtin::get(&cat_id)?
                .ok_or_else(|| Error::rejected("the built-in catalog no longer ships this app"))?;
            let files = builtin.files;
            let new_digest = bundle_digest(&files);
            let validated = app::validate_texts(
                files.iter().map(|(n, t)| (n.clone(), t.clone())).collect(),
                &Default::default(),
                &[],
            )?;
            let has_update = new_digest != digest;
            Ok(json!({"schema":1,"install_id":&*id,"source":"builtin",
                "catalog_id":cat_id,"has_update":has_update,
                "current_digest":digest,"digest":new_digest,
                "version":validated.manifest.version}))
        }
        app::Source::Git { url, sha, dir } => {
            let dir = dir.unwrap_or_default();
            let selected = crate::issue::app_source::SelectedGitSource::new(&url, &sha, &dir)?;
            let bundle = crate::issue::app_source::resolve(&selected)?;
            let validated = app::validate_texts(
                bundle
                    .files
                    .iter()
                    .map(|(n, t)| (n.clone(), t.clone()))
                    .collect(),
                &Default::default(),
                &[],
            )?;
            let new_digest = bundle_digest(&bundle.files);
            let has_update = new_digest != current_digest;
            Ok(json!({"schema":1,"install_id":&*id,"source":"git",
                "url":url,"sha":sha,"dir":dir,"has_update":has_update,
                "current_digest":current_digest,"digest":new_digest,
                "version":validated.manifest.version}))
        }
        app::Source::Path { path } => Ok(json!({"schema":1,"install_id":&*id,
            "source":"path","path":path,"has_update":false,
            "note":"a local checkout is not upstream-tracked — check its own files"})),
    }
}

/// CAD-1129 H5: preview a soft remove — the install's name, the
/// `personal_data` flag and the data it keeps. Per-kind counts live
/// in the store's own tables; the daemon's RPC reads those directly.
/// Writes nothing; the operator confirms before `remove` commits.
pub(crate) fn remove_preview(pm: &Pm, install_id: &str) -> Result<Value> {
    let id = InstallationId::parse(install_id)?;
    let _lock = pm.lock()?;
    let root = Root::open(&pm.dir)?;
    let catalog = Catalog::load(&pm.dir)?;
    let entry = catalog
        .installations
        .get(&id)
        .ok_or_else(|| Error::rejected("unknown installation ID"))?;
    let desc = describe(&root, &catalog, &id)?;
    let _record: Record = serde_json::from_value(desc["record"].clone())
        .map_err(|e| Error::internal(format!("installation record is not readable: {e}")))?;
    // `listing` is a manifest field; `describe` doesn't carry it —
    // re-read the bundle's manifest for the personal-data flag.
    let app_md = {
        let (bundle, _) = catalog.installations[&id].paths(&id);
        let files = snapshot(&root, &bundle, false)?;
        files.get("app.md").cloned()
    };
    let listing = app_md
        .and_then(|md| app::parse_manifest(&md).ok())
        .and_then(|m| m.listing);
    let personal_data = listing
        .as_ref()
        .and_then(|l| l.value["data"]["personal"].as_bool())
        .unwrap_or(false)
        || listing
            .as_ref()
            .and_then(|l| l.value["data"]["contacts"].as_bool())
            .unwrap_or(false);
    Ok(json!({
        "schema":1,"install_id":&*id,"name":entry.app,
        "title":desc["title"],"personal_data":personal_data,
        "keeps":{"data":listing.as_ref().map(|l| l.value["data"]["keeps"].clone())},
        "generation":desc["catalog_generation"],"digest":desc["digest"],
    }))
}

/// CAD-1129 H5: journaled soft remove — mark the record, revoke
/// consent, cancel queued publishes. The install stays in the catalog
/// under `Removed` until a 30-day restore; after `restore_after` the
/// restore refuses. Run creation refuses on the `removed` mark.
pub(crate) fn remove(
    pm: &Pm,
    store: &Store,
    install_id: &str,
    expected_generation: &str,
    expected_digest: &str,
    request_id: &str,
) -> Result<Value> {
    let id = InstallationId::parse(install_id)?;
    crate::proto::identifier(request_id, "remove request ID")?;
    let _lock = pm.lock()?;
    let root = Root::open(&pm.dir)?;
    let catalog = Catalog::load(&pm.dir)?;
    let entry = catalog
        .installations
        .get(&id)
        .ok_or_else(|| Error::rejected("unknown installation ID"))?;
    if hash(&yaml(&catalog)?) != expected_generation {
        return Err(Error::rejected("workspace catalog generation is stale"));
    }
    let desc = describe(&root, &catalog, &id)?;
    if desc["digest"].as_str() != Some(expected_digest) {
        return Err(Error::rejected("workspace installation digest is stale"));
    }
    let mut record: Record = serde_json::from_value(desc["record"].clone())
        .map_err(|e| Error::internal(format!("installation record is not readable: {e}")))?;
    if record.removed.is_some() {
        return Ok(json!({"schema":1,"install_id":&*id,"state":"removed",
            "removed":record.removed,"restore_after":record.restore_after,
            "request_id":request_id,"replayed":true}));
    }
    let now = crate::issue::time::now_epoch();
    record.removed = Some(now);
    record.restore_after = Some(now + 30 * 24 * 3600);
    // Consent revocation rides the same record write — the digest is
    // frozen, so revoke binds exactly these bytes.
    store.app_install_revoke(install_id, expected_digest, "remove")?;
    // Cancel queued publishes the store still holds for this install.
    let cancelled = store.social_publish_cancel_install(&id)?;
    let record_path = Path::new(".apps/installations")
        .join(&*id)
        .join("record.yaml");
    root.put(&record_path, &yaml(&record)?)?;
    let foreign = write::commit(
        pm,
        &[pm.dir.join(&record_path)],
        &format!("workspace app {} removed ({})", entry.app, &*id),
        &[],
        "operator",
    )?;
    let _ = store.event_public(
        "cadence",
        "app_workspace_removed",
        json!({"install_id":&*id,"app":entry.app,"request_id":request_id}),
    );
    Ok(json!({"schema":1,"install_id":&*id,"state":"removed",
        "removed":now,"restore_after":record.restore_after,
        "cancelled_publishes":cancelled,"request_id":request_id,
        "foreign_files":foreign}))
}

/// CAD-1129 H5: clear the soft-remove mark and re-consent the same
/// digest. Refused after `restore_after`; the record write replays
/// idempotently. The operator's restore re-approves the exact bytes
/// already on disk — no re-install.
pub(crate) fn restore(pm: &Pm, install_id: &str) -> Result<Value> {
    let id = InstallationId::parse(install_id)?;
    let _lock = pm.lock()?;
    let root = Root::open(&pm.dir)?;
    let catalog = Catalog::load(&pm.dir)?;
    let entry = catalog
        .installations
        .get(&id)
        .ok_or_else(|| Error::rejected("unknown installation ID"))?;
    let desc = describe(&root, &catalog, &id)?;
    let mut record: Record = serde_json::from_value(desc["record"].clone())
        .map_err(|e| Error::internal(format!("installation record is not readable: {e}")))?;
    if record.removed.is_none() {
        return Ok(json!({"schema":1,"install_id":&*id,"state":"live",
            "digest":desc["digest"],"restored":false}));
    }
    let now = crate::issue::time::now_epoch();
    if record.restore_after.is_some_and(|after| now > after) {
        return Err(Error::rejected(
            "the restore window closed — this install can only be removed",
        ));
    }
    record.removed = None;
    record.restore_after = None;
    let record_path = Path::new(".apps/installations")
        .join(&*id)
        .join("record.yaml");
    root.put(&record_path, &yaml(&record)?)?;
    let foreign = write::commit(
        pm,
        &[pm.dir.join(&record_path)],
        &format!("workspace app {} restored ({})", entry.app, &*id),
        &[],
        "operator",
    )?;
    Ok(json!({"schema":1,"install_id":&*id,"state":"live",
        "digest":desc["digest"],"restored":true,"foreign_files":foreign}))
}

#[cfg(test)]
mod cad1189_acceptance;
#[cfg(test)]
mod cad1254_acceptance;

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
        let out = install(&pm, state_dir.path(), source.to_str().unwrap(), None).unwrap();
        let id = out["install_id"].as_str().unwrap().to_string();
        (pm_dir, state_dir, sources, id)
    }

    /// CAD-1234: a hold over 1 s through `with_runtime_snapshot` reports the
    /// caller's line, not a line inside this file; a same-process busy
    /// refusal during the hold names the same site.
    #[test]
    fn a_long_hold_through_a_helper_names_the_outer_call_site() {
        let (pm_dir, _state, _sources, id) = installed();
        let pm = Pm::at(pm_dir.path()).unwrap();
        crate::issue::pmlock::HOLD_LINES.with(|l| l.borrow_mut().clear());
        let call_line = line!() + 1;
        with_runtime_snapshot(&pm, &id, |_, _| {
            let err = pm
                .lock_for(std::time::Duration::from_millis(200))
                .unwrap_err();
            assert_eq!(err.code(), Some("resource_busy"), "{err}");
            assert!(
                err.to_string().contains(&format!(
                    "at=src/issue/app_catalog/workspace.rs:{call_line}"
                )),
                "{err}"
            );
            std::thread::sleep(std::time::Duration::from_millis(1100));
            Ok(())
        })
        .unwrap();
        let lines = crate::issue::pmlock::HOLD_LINES.with(|l| l.borrow().clone());
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(
            lines[0].ends_with(&format!(
                "at=src/issue/app_catalog/workspace.rs:{call_line}"
            )),
            "{}",
            lines[0]
        );
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

    /// CAD-1189: reads and runtime admission answer while another handle
    /// holds the PM write lock. Before, each waited out the 15 s lock
    /// deadline and failed `resource_busy`.
    #[test]
    fn reads_and_admission_do_not_wait_for_the_pm_lock() {
        let (pm_dir, _state, _sources, id) = installed();
        let pm = Pm::at(pm_dir.path()).unwrap();
        let _held = pm.lock().unwrap();
        let other = Pm::at(pm_dir.path()).unwrap();
        assert!(other.try_lock().unwrap().is_none(), "lock is really held");
        let started = std::time::Instant::now();
        assert_eq!(list(&other).unwrap().as_array().unwrap().len(), 1);
        let shown = show(&other, &id).unwrap();
        let digest = shown["digest"].as_str().unwrap().to_string();
        let seen = with_runtime_read(&other, &id, |row, _| Ok(row["digest"].clone())).unwrap();
        assert_eq!(seen, json!(digest));
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }

    /// CAD-1212: the seam fires between `describe` and the bundle
    /// `snapshot`; a bundle edit there trips the digest compare, so the
    /// read ends `busy` after its retries and the callback never runs.
    #[test]
    fn runtime_read_refuses_a_bundle_edited_after_describe() {
        let (pm_dir, _state, _sources, id) = installed();
        let pm = Pm::at(pm_dir.path()).unwrap();
        let app_md = pm_dir.path().join(".apps").join(&id).join("app.md");
        let app_md = if app_md.exists() {
            app_md
        } else {
            walk_find(pm_dir.path(), "app.md").expect("installed app.md")
        };
        let hits = std::rc::Rc::new(std::cell::Cell::new(0));
        let seen = hits.clone();
        read_seam::set_hook(Some(Box::new(move || {
            seen.set(seen.get() + 1);
            let mut text = std::fs::read_to_string(&app_md).unwrap();
            text.push_str(&format!("\nedit {}\n", seen.get()));
            std::fs::write(&app_md, text).unwrap();
        })));
        let ran = std::cell::Cell::new(false);
        let err = with_runtime_read(&pm, &id, |_, _| {
            ran.set(true);
            Ok(())
        })
        .unwrap_err();
        read_seam::set_hook(None);
        assert_eq!(err.code(), Some("resource_busy"), "{err:?}");
        assert!(err.to_string().contains("runtime admission"), "{err}");
        assert_eq!(hits.get(), READ_ATTEMPTS, "one hook call per attempt");
        assert!(!ran.get(), "callback ran on a torn read");
    }

    fn walk_find(dir: &Path, name: &str) -> Option<PathBuf> {
        for entry in std::fs::read_dir(dir).ok()? {
            let path = entry.ok()?.path();
            if path.is_dir() {
                if path.file_name().is_some_and(|n| n == ".git") {
                    continue;
                }
                if let Some(found) = walk_find(&path, name) {
                    return Some(found);
                }
            } else if path.file_name().is_some_and(|n| n == name) {
                return Some(path);
            }
        }
        None
    }

    /// CAD-1212: the retry classifier matches codes, not message text.
    #[test]
    fn retry_classifier_matches_codes_not_text() {
        let reworded = Error::invalid(super::super::PENDING_UPGRADE, "totally different words");
        assert!(is_pending(&reworded));
        assert!(!is_pending(&Error::rejected(
            "workspace upgrade is pending; use app catalog upgrade-recover"
        )));
        let changed = Error::invalid(super::super::ADMISSION_CHANGED, "reworded");
        assert!(changed_underneath(&changed));
        assert!(!changed_underneath(&Error::rejected(
            "installation changed during runtime admission"
        )));
    }

    /// A crashed leftover journal (no live writer) keeps the recovery
    /// refusal; it is never reported as retryable `busy`.
    #[test]
    fn leftover_pending_journal_is_not_busy() {
        let (pm_dir, _state, _sources, id) = installed();
        let pm = Pm::at(pm_dir.path()).unwrap();
        std::fs::write(pm_dir.path().join(UPGRADE_PENDING), "x: 1\n").unwrap();
        for err in [list(&pm).unwrap_err(), show(&pm, &id).unwrap_err()] {
            assert_eq!(err.kind(), "rejected", "{err:?}");
        }
    }

    /// CAD-1202: an entry with `Storage::Legacy` answers `list`, `show`
    /// and `with_runtime_read` while another handle holds the PM lock. A
    /// crash leftover (odd counter, no live writer) still answers, through
    /// the locked path; a live writer mid-rewrite is retryable `busy`.
    #[test]
    fn legacy_entries_answer_without_the_pm_lock() {
        let pm_dir = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let sources = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let pm = Pm::init(pm_dir.path()).unwrap();
        crate::issue::write::project_add(
            &pm,
            "legacy",
            "LEG",
            &[repo.path().display().to_string()],
            &[],
            &[],
            None,
        )
        .unwrap();
        let src = sources.path().join("fixture-app");
        bundle(&src, MANIFEST_A);
        crate::issue::app::install(&pm, "legacy", src.to_str().unwrap(), state.path(), "t")
            .unwrap();
        crate::issue::app_catalog::migrate_authorized(&pm).unwrap();
        let rows = list(&pm).unwrap();
        let row = &rows.as_array().unwrap()[0];
        assert_eq!(row["storage_kind"], "legacy", "{row}");
        let id = row["install_id"].as_str().unwrap().to_string();

        let gen_file = pm_dir.path().join(".git/cadence-legacy-generation");
        let held = pm.lock().unwrap();
        let other = Pm::at(pm_dir.path()).unwrap();
        assert!(other.try_lock().unwrap().is_none(), "lock is really held");
        let started = std::time::Instant::now();
        assert_eq!(list(&other).unwrap().as_array().unwrap().len(), 1);
        assert_eq!(show(&other, &id).unwrap()["storage_kind"], "legacy");
        with_runtime_read(&other, &id, |row, _| Ok(row["digest"].clone())).unwrap();
        assert!(started.elapsed() < std::time::Duration::from_secs(5));

        // Odd while a live writer holds the lock: retryable busy.
        std::fs::write(&gen_file, "7").unwrap();
        let err = list(&other).unwrap_err();
        assert_eq!(err.kind(), "busy", "{err:?}");
        drop(held);
        // Odd with no live writer: a crash leftover, read under the lock.
        assert_eq!(list(&other).unwrap().as_array().unwrap().len(), 1);
    }
}
