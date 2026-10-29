//! Project metadata, frontmatter and acceptance helpers.

use super::*;

/// Locate an issue folder by id. The id's prefix must match the
/// project's prefix — ids cannot smuggle across projects. Symlinks are
/// never followed: a linked folder or issue.md does not exist as far
/// as the writer is concerned.
pub(crate) fn issue_dir(pm: &Pm, id: &str) -> Result<(project::Project, PathBuf)> {
    model::check_id(id)?;
    for project in project::list(&pm.dir)? {
        let dir = pm.dir.join(&project.key).join(id);
        if board::is_real_dir(&dir) && board::is_real_file(&dir.join("issue.md")) {
            return Ok((project, dir));
        }
    }
    Err(Error::rejected(format!(
        "Unknown issue '{id}' — `cadence issue ls` lists what exists"
    )))
}

pub(crate) fn load_front(dir: &Path) -> Result<(Front, String)> {
    let text = std::fs::read_to_string(dir.join("issue.md"))?;
    parse::parse_issue(&text)
}

/// Lint parity for the component field on every write path (`new`,
/// `set`, HTTP PATCH): the value must be one the issue's project
/// declares — a project with no component list accepts any.
pub(super) fn check_component(project: &project::Project, component: &str) -> Result<()> {
    if !project.components.is_empty() && !project.components.iter().any(|c| c == component) {
        return Err(Error::rejected(format!(
            "Unknown component '{component}' — {} declares: {}",
            project.key,
            project.components.join(", ")
        )));
    }
    Ok(())
}

/// The same rule for tags: well-formed, sorted, de-duplicated, capped —
/// and, when the project declares a `tags:` list, drawn from it.
pub(crate) fn check_tags(project: &project::Project, tags: &[String]) -> Result<Vec<String>> {
    let tags = model::normalize_tags(tags)?;
    if !project.tags.is_empty() {
        if let Some(unknown) = tags
            .iter()
            .find(|t| !project.tags.contains(t) && !model::system_tag(t))
        {
            return Err(Error::rejected(format!(
                "Unknown tag '{unknown}' — {} declares: {}",
                project.key,
                project.tags.join(", ")
            )));
        }
    }
    Ok(tags)
}

/// `a,b` → `[a, b]`; blanks between commas are dropped, so `tags=`
/// clears.
pub(crate) fn split_tags(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

pub(crate) fn save_front(dir: &Path, front: &Front, body: &str) -> Result<()> {
    atomic_write(&dir.join("issue.md"), &parse::render(front, body)?)
}

/// issue acceptance <ID> --from <file> — replace the issue's unique
/// level-two Acceptance section, or append one when it is absent.
/// Validation happens before the PM lock and the body is saved atomically
/// under that lock, so malformed input and duplicate sections leave the
/// tracker unchanged.
#[track_caller]
pub fn set_acceptance(pm: &Pm, id: &str, source: &Path, actor: &str) -> Result<Value> {
    let input = std::fs::read_to_string(source).map_err(|e| {
        Error::rejected(format!(
            "Cannot read acceptance input {}: {e}",
            source.display()
        ))
    })?;
    let items = parse::parse_acceptance_input(&input)?;
    let (_project, dir) = issue_dir(pm, id)?;
    let file = dir.join("issue.md");
    let lock = pm.lock_for_paths(std::slice::from_ref(&file))?;
    let (front, body) = load_front(&dir)?;
    let body = parse::replace_acceptance(&body, &items)?;
    let prev = file_preimage(&file);
    save_front(&dir, &front, &body)?;
    let foreign = match commit_scoped(
        pm,
        &lock,
        std::slice::from_ref(&file),
        &format!("{id}: acceptance replaced"),
        &[id],
        actor,
    ) {
        Ok(f) => f,
        Err(e) => {
            restore_preimage(&file, prev);
            return Err(e);
        }
    };
    let mut out = json!({
        "id": id,
        "acceptance": items
            .iter()
            .map(parse::AcceptanceItem::to_json)
            .collect::<Vec<_>>(),
        "committed": true,
    });
    attach_foreign(&mut out, &foreign);
    Ok(out)
}

/// `<key>/` inside the PM dir, refusing a symlinked project directory: a
/// tracker edit must never land outside it.
pub(super) fn project_dir(pm: &Pm, key: &str) -> Result<PathBuf> {
    let dir = pm.dir.join(key);
    if dir.symlink_metadata().is_ok_and(|m| m.is_symlink()) {
        return Err(Error::rejected(format!(
            "{key}/ is a symlink — refusing to write outside the PM dir"
        )));
    }
    Ok(dir)
}

/// Serialize `project` to `path` — the one place a project record is
/// written, so `project add` and a policy write produce the same file.
pub(super) fn write_project_yaml(path: &Path, project: &project::Project) -> Result<()> {
    let yaml = serde_yaml::to_string(project)
        .map_err(|e| Error::internal(format!("project.yaml: {e}")))?;
    std::fs::write(path, yaml)?;
    Ok(())
}

/// `project add` — create `<pm>/<key>/project.yaml`.
#[allow(clippy::too_many_arguments)]
pub fn project_add(
    pm: &Pm,
    key: &str,
    prefix: &str,
    repos: &[String],
    components: &[String],
    tags: &[String],
    owner: Option<&str>,
) -> Result<Value> {
    // `agents` is reserved for the agent files (CAD-358).
    crate::issue::project_new::check_key(key)?;
    let tags = model::normalize_tags(tags)?;
    let prefix = prefix.to_ascii_uppercase();
    cadence_core::issue::check_prefix(&prefix)?;
    let lock = pm.lock_for_paths(std::slice::from_ref(&pm.dir.join(key)))?;
    let projects = project::list(&pm.dir)?;
    if projects.iter().any(|p| p.key == key) {
        return Err(Error::rejected(format!(
            "Project '{key}' already exists — update a registered remote with \
             `cadence issue project set-remote {key} --from <old> --to <new>`"
        )));
    }
    if let Some(other) = projects.iter().find(|p| p.prefix == prefix) {
        return Err(Error::rejected(format!(
            "Prefix {prefix} is project '{}''s — pass another --prefix",
            other.key
        )));
    }
    let mut repo_entries = Vec::new();
    for repo in repos {
        let path = project::expand_home(repo);
        let canonical = path.canonicalize().unwrap_or(path.clone());
        // Store the path as given (~/ keeps it portable); remote is
        // discovered from the repo's own config.
        let remote = crate::reaper::output(
            std::process::Command::new("git")
                .arg("-C")
                .arg(&canonical)
                .args(["config", "--get", "remote.origin.url"]),
        )
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|r| !r.is_empty())
        .map(|r| project::normalize_remote(&r));
        repo_entries.push(project::Repo {
            path: Some(repo.clone()),
            remote,
            base: None,
        });
    }
    let project = project::Project {
        key: key.to_string(),
        prefix,
        repos: repo_entries,
        components: components.to_vec(),
        tags,
        default_owner: owner.map(str::to_string),
        build: None,
        memory: None,
        intake: None,
        // AOS-274: identity is minted once, and every project records an
        // explicit policy. Only workspace-visible is written until the
        // isolation gate passes.
        id: Some(project::new_id()),
        policy: Some(project::ProjectPolicy::workspace_visible()),
    };
    let dir = project_dir(pm, key)?;
    std::fs::create_dir_all(&dir)?;
    let yaml_path = dir.join("project.yaml");
    let prev = file_preimage(&yaml_path);
    write_project_yaml(&yaml_path, &project)?;
    let foreign = match commit_scoped(
        pm,
        &lock,
        std::slice::from_ref(&yaml_path),
        &format!("project {key} added"),
        &[],
        "",
    ) {
        Ok(f) => f,
        Err(e) => {
            restore_preimage(&yaml_path, prev);
            return Err(e);
        }
    };
    let mut out = json!({"project": key, "prefix": project.prefix,
              "path": dir, "committed": true});
    attach_foreign(&mut out, &foreign);
    Ok(out)
}

/// The one refusal sentence for private, shared by every writer so the CLI,
/// the daemon and the board cannot describe the gate differently.
pub const PRIVATE_UNAVAILABLE: &str =
    "private projects are not available yet — the isolation gate \
     has not passed; only workspace-visible can be recorded";

/// May this build record `private` (CAD-1950, AOS-274 / SEC-006)?
///
/// The isolation gate stays closed until CAD-1950 records, per enforcement
/// path, a test proving a non-granted caller is served nothing:
/// `project_policy::readable` and `refused` for listings and single rows,
/// `retain_overview` for the overview payload, and `reads_every_project`
/// for the shared live stream (which is one un-filterable model, so a
/// caller who cannot read every project is refused live updates rather
/// than served filtered frames).
///
/// It is one function on purpose: `set_project_policy`, `project new` and
/// the board's Settings page read the same answer, so flipping it is one
/// place and the UI cannot offer a write the tracker will refuse.
pub fn private_available() -> bool {
    false
}

/// Replace a project's recorded access policy (AOS-274).
///
/// Takes the tracker lock, re-reads the project, keeps its identity, bumps
/// the policy version and commits `project.yaml` only. `Private` is refused
/// until [`private_available`] answers yes — `workspace-visible` is the only
/// visibility this build may record.
pub fn set_project_policy(
    pm: &Pm,
    key: &str,
    visibility: project::Visibility,
    members: &[String],
    actor: &str,
) -> Result<Value> {
    // Validate before any join. A remote key is caller input; `../…` or a
    // slash must never become a path outside the tracker.
    let key = crate::issue::model::check_key(key)?;
    if visibility == project::Visibility::Private && !private_available() {
        return Err(Error::rejected(PRIVATE_UNAVAILABLE));
    }
    let dir = project_dir(pm, &key)?;
    let lock = pm.lock_for_paths(std::slice::from_ref(&dir))?;
    let yaml_path = dir.join("project.yaml");
    if !crate::issue::board::is_real_file(&yaml_path) {
        return Err(Error::rejected(format!(
            "no project '{key}' — register it with `cadence issue project add`"
        )));
    }
    let mut project = project::load(&yaml_path)?;
    let grants = members
        .iter()
        .map(|member| project::Grant::parse(member))
        .collect::<Result<Vec<_>>>()?;
    let mut policy = project.policy.clone().unwrap_or_default();
    policy.version = policy.version.saturating_add(1);
    policy.visibility = visibility;
    policy.members = grants;
    project.policy = Some(policy.clone());
    if project.id.is_none() {
        project.id = Some(project::new_id());
    }
    let prev = file_preimage(&yaml_path);
    write_project_yaml(&yaml_path, &project)?;
    let foreign = match commit_scoped(
        pm,
        &lock,
        std::slice::from_ref(&yaml_path),
        &format!("project {key} policy version {}", policy.version),
        &[],
        actor,
    ) {
        Ok(f) => f,
        Err(e) => {
            // A refused commit leaves the record exactly as it was — the
            // rule every other write in this module follows, and the one
            // this verb was missing.
            restore_preimage(&yaml_path, prev);
            return Err(e);
        }
    };
    let mut out = json!({
        "project": key,
        "policy": serde_json::to_value(&policy)
            .map_err(|e| Error::internal(format!("policy: {e}")))?,
        "committed": true,
    });
    attach_foreign(&mut out, &foreign);
    Ok(out)
}

/// `host/owner/repo` after [`project::normalize_remote`]. A git URL is
/// fine; extra path segments, empty parts and `.` / `..` are not.
fn parse_remote(raw: &str) -> Result<String> {
    let norm = project::normalize_remote(raw);
    let parts: Vec<&str> = norm.split('/').collect();
    let ok = parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && *p != "." && *p != ".." && !p.contains(char::is_whitespace));
    if !ok {
        return Err(Error::rejected(format!(
            "malformed repository remote '{raw}' — expected host/owner/repo (a git URL is fine)"
        )));
    }
    Ok(norm)
}

fn remotes_equal(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// CAD-1870: update one registered `repos[].remote` after a transfer or
/// rename. `--from` must uniquely match the current recorded remote;
/// `--to` must not already belong to another project (or another row).
/// A no-op (`--from` equals `--to` equals the record) writes nothing.
pub fn set_project_remote(pm: &Pm, key: &str, from: &str, to: &str, actor: &str) -> Result<Value> {
    let key = crate::issue::model::check_key(key)?;
    let from_norm = parse_remote(from)?;
    let to_norm = parse_remote(to)?;
    let dir = project_dir(pm, &key)?;
    let lock = pm.lock_for_paths(std::slice::from_ref(&dir))?;
    let yaml_path = dir.join("project.yaml");
    if !crate::issue::board::is_real_file(&yaml_path) {
        return Err(Error::rejected(format!(
            "no project '{key}' — register it with `cadence issue project add`"
        )));
    }
    let mut project = project::load(&yaml_path)?;
    let matches: Vec<usize> = project
        .repos
        .iter()
        .enumerate()
        .filter_map(|(i, r)| {
            let remote = r.remote.as_deref()?;
            remotes_equal(&project::normalize_remote(remote), &from_norm).then_some(i)
        })
        .collect();
    let idx = match matches.as_slice() {
        [] => {
            return Err(Error::rejected(format!(
                "project '{key}' has no registered remote {from_norm} — \
                 pass --from matching one recorded repos[].remote"
            )));
        }
        [idx] => *idx,
        _ => {
            return Err(Error::rejected(format!(
                "project '{key}' has more than one repository recorded as {from_norm} — \
                 refusing an ambiguous update"
            )));
        }
    };
    if remotes_equal(&from_norm, &to_norm) {
        return Ok(json!({
            "project": key,
            "from": from_norm,
            "to": to_norm,
            "changed": false,
            "committed": false,
        }));
    }
    let projects = project::list(&pm.dir)?;
    if let Some(other) = projects.iter().find(|p| {
        p.repos.iter().enumerate().any(|(i, r)| {
            let Some(remote) = r.remote.as_deref() else {
                return false;
            };
            if !remotes_equal(&project::normalize_remote(remote), &to_norm) {
                return false;
            }
            !(p.key == key && i == idx)
        })
    }) {
        return Err(Error::rejected(format!(
            "{to_norm} is already project '{}' — one repo, one project",
            other.key
        )));
    }
    project.repos[idx].remote = Some(to_norm.clone());
    let prev = file_preimage(&yaml_path);
    write_project_yaml(&yaml_path, &project)?;
    let foreign = match commit_scoped(
        pm,
        &lock,
        std::slice::from_ref(&yaml_path),
        &format!("project {key} remote {from_norm} -> {to_norm}"),
        &[],
        actor,
    ) {
        Ok(f) => f,
        Err(e) => {
            restore_preimage(&yaml_path, prev);
            return Err(e);
        }
    };
    let mut out = json!({
        "project": key,
        "from": from_norm,
        "to": to_norm,
        "changed": true,
        "committed": true,
    });
    attach_foreign(&mut out, &foreign);
    Ok(out)
}
