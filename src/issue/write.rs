//! The write side — the only writer for issue folders. Every op takes
//! the PM lock, mutates files (`issue.md` via temp+rename; comments and
//! artifacts create-only), then makes one git commit. The HTTP API in
//! `ui.rs` calls these same functions with the derived board caller —
//! `actor = "operator (ui)"`, or a pane's alias (CAD-254).

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::issue::model::{self, Front, Ref};
use crate::issue::{board, parse, project, time, Pm};

/// `Actor:` trailer resolution, in order: an explicit `--author`/`--by`
/// (`who`), the API actor string (`actor`), `CADENCE_ALIAS`, else
/// `operator`. The same chain feeds `issue comment`'s author fallback.
pub(crate) fn actor_who(actor: &str, who: Option<&str>) -> String {
    who.map(str::to_string)
        .or_else(|| (!actor.is_empty()).then(|| actor.to_string()))
        .or_else(|| std::env::var("CADENCE_ALIAS").ok())
        .filter(|w| !w.is_empty())
        .unwrap_or_else(|| "operator".to_string())
}

/// Commit with the actor named when one is given — API writes show as
/// `CAD-16: set status=review (operator (ui))`; CLI writes pass "" and
/// keep the bare subject. Every commit also carries trailers after a
/// blank line: `Issue: <ID>` once per issue the write touches (links
/// record both ends) and `Actor: <who>` — the truthful actor history
/// reads instead of the git author.
pub(crate) fn commit(pm: &Pm, message: &str, ids: &[&str], actor: &str) -> Result<()> {
    commit_who(pm, message, ids, actor, None)
}

fn commit_who(pm: &Pm, message: &str, ids: &[&str], actor: &str, who: Option<&str>) -> Result<()> {
    let subject = if actor.is_empty() {
        message.to_string()
    } else {
        format!("{message} ({actor})")
    };
    let mut trailers = String::new();
    for id in ids {
        trailers.push_str(&format!("Issue: {id}\n"));
    }
    trailers.push_str(&format!("Actor: {}\n", actor_who(actor, who)));
    pm.commit(&format!("{subject}\n\n{trailers}"))
}

/// Content hash of `issue.md` — the optimistic-concurrency token the
/// write API calls `if_rev`. FNV-1a: no deps, stable across versions.
pub fn issue_rev(dir: &Path) -> Result<String> {
    let bytes = std::fs::read(dir.join("issue.md"))?;
    Ok(rev_bytes(&bytes))
}

fn rev_bytes(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("fnv1a:{h:016x}")
}

/// Optimistic-concurrency check, run under the write lock. Returns a
/// conflict payload (the route maps it to 409) or None.
fn check_rev(dir: &Path, if_rev: Option<&str>) -> Result<Option<Value>> {
    let Some(want) = if_rev else {
        return Ok(None);
    };
    let cur = issue_rev(dir)?;
    if want != cur {
        return Ok(Some(json!({"conflict": "if_rev", "current_rev": cur})));
    }
    Ok(None)
}

/// Lint parity at write time: an issue whose status is ready|doing|
/// review while a blocked_by target is unfinished succeeds but the
/// response carries this warning.
fn blocked_warnings(pm: &Pm, id: &str, state_dir: Option<&Path>) -> Result<Vec<String>> {
    let issues = board::load_all(&pm.dir, None)?;
    let jobs = state_dir.map(board::fetch_job_outcomes).unwrap_or_default();
    let views = board::views_with_jobs(&pm.config.notes_dir(), issues, &jobs);
    let Some(v) = views.iter().find(|v| v.issue.front.id == id) else {
        return Ok(vec![]);
    };
    if !(v.blocked && matches!(v.status.as_str(), "ready" | "doing" | "review")) {
        return Ok(vec![]);
    }
    let open: Vec<String> = v
        .issue
        .front
        .blocked_by
        .iter()
        .filter(|b| {
            views
                .iter()
                .find(|o| &o.issue.front.id == *b)
                .map(|o| !matches!(o.status.as_str(), "done" | "dropped"))
                .unwrap_or(true)
        })
        .cloned()
        .collect();
    Ok(vec![format!(
        "{id} is {} but still waits on {}",
        v.status,
        open.join(", ")
    )])
}

/// Write `text` to `path` atomically (temp file + rename).
fn atomic_write(path: &Path, text: &str) -> Result<()> {
    let tmp = path.with_extension("md.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Create a file exclusively; on a name collision try `-2`, `-3`…
/// before the extension. Returns the created path.
fn create_exclusive(dir: &Path, name: &str, bytes: &[u8]) -> Result<PathBuf> {
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) => (s.to_string(), format!(".{e}")),
        None => (name.to_string(), String::new()),
    };
    for n in 0..100 {
        let candidate = if n == 0 {
            name.to_string()
        } else {
            format!("{stem}-{n}{ext}")
        };
        let path = dir.join(&candidate);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                use std::io::Write;
                file.write_all(bytes)?;
                return Ok(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Err(Error::rejected(format!(
        "Could not create a unique name for '{name}' under {}",
        dir.display()
    )))
}

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
fn check_component(project: &project::Project, component: &str) -> Result<()> {
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
fn split_tags(value: &str) -> Vec<String> {
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
pub fn set_acceptance(pm: &Pm, id: &str, source: &Path, actor: &str) -> Result<Value> {
    let input = std::fs::read_to_string(source).map_err(|e| {
        Error::rejected(format!(
            "Cannot read acceptance input {}: {e}",
            source.display()
        ))
    })?;
    let items = parse::parse_acceptance_input(&input)?;
    let (_project, dir) = issue_dir(pm, id)?;
    let _lock = pm.lock()?;
    let (front, body) = load_front(&dir)?;
    let body = parse::replace_acceptance(&body, &items)?;
    save_front(&dir, &front, &body)?;
    commit(pm, &format!("{id}: acceptance replaced"), &[id], actor)?;
    Ok(json!({
        "id": id,
        "acceptance": items
            .iter()
            .map(parse::AcceptanceItem::to_json)
            .collect::<Vec<_>>(),
        "committed": true,
    }))
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
    model::check_key(key)?;
    let tags = model::normalize_tags(tags)?;
    let prefix = prefix.to_ascii_uppercase();
    if prefix.is_empty()
        || prefix.len() > 8
        || !prefix
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
        || !prefix
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_uppercase())
    {
        return Err(Error::rejected(format!(
            "Invalid prefix '{prefix}' — uppercase letters/digits starting with a letter"
        )));
    }
    if project::list(&pm.dir)?.iter().any(|p| p.key == key) {
        return Err(Error::rejected(format!(
            "Project '{key}' already exists — edit {} directly",
            pm.dir.join(key).join("project.yaml").display()
        )));
    }
    let _lock = pm.lock()?;
    let mut repo_entries = Vec::new();
    for repo in repos {
        let path = project::expand_home(repo);
        let canonical = path.canonicalize().unwrap_or(path.clone());
        // Store the path as given (~/ keeps it portable); remote is
        // discovered from the repo's own config.
        let remote = std::process::Command::new("git")
            .arg("-C")
            .arg(&canonical)
            .args(["config", "--get", "remote.origin.url"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|r| !r.is_empty())
            .map(|r| project::normalize_remote(&r));
        repo_entries.push(project::Repo {
            path: Some(repo.clone()),
            remote,
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
    };
    let dir = pm.dir.join(key);
    if dir.symlink_metadata().is_ok_and(|m| m.is_symlink()) {
        return Err(Error::rejected(format!(
            "{key}/ is a symlink — refusing to write outside the PM dir"
        )));
    }
    std::fs::create_dir_all(&dir)?;
    let yaml = serde_yaml::to_string(&project)
        .map_err(|e| Error::internal(format!("project.yaml: {e}")))?;
    std::fs::write(dir.join("project.yaml"), yaml)?;
    commit(pm, &format!("project {key} added"), &[], "")?;
    Ok(json!({"project": key, "prefix": project.prefix,
              "path": dir, "committed": true}))
}

/// Allocate the next id under the write lock: `<PREFIX>-<max+1>`.
pub(crate) fn next_id(dir: &Path, prefix: &str) -> Result<u64> {
    let mut max = 0u64;
    for entry in std::fs::read_dir(dir)?.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if let Some(n) = name
            .strip_prefix(&format!("{prefix}-"))
            .and_then(|s| s.parse::<u64>().ok())
        {
            max = max.max(n);
        }
    }
    Ok(max + 1)
}

/// `issue new` — one folder + issue.md, under the id lock.
#[allow(clippy::too_many_arguments)]
pub fn new_issue(
    pm: &Pm,
    cwd: &Path,
    project_flag: Option<&str>,
    title: &str,
    priority: Option<&str>,
    parent: Option<&str>,
    blocked_by: &[String],
    owner: Option<&str>,
    component: Option<&str>,
    tags: &[String],
    explicit_id: Option<&str>,
    actor: &str,
) -> Result<Value> {
    let project = project::resolve(&pm.dir, project_flag, cwd)?;
    let priority = priority.unwrap_or("P2");
    model::check_priority(priority)?;
    if let Some(id) = explicit_id {
        model::check_id(id)?;
        let want = format!("{}-", project.prefix);
        if !id.starts_with(&want) {
            return Err(Error::rejected(format!(
                "--id '{id}' does not match project prefix '{want}'"
            )));
        }
    }
    if let Some(component) = component {
        check_component(&project, component)?;
    }
    let tags = check_tags(&project, tags)?;
    let _lock = pm.lock()?;
    let id = match explicit_id {
        Some(id) => id.to_string(),
        None => format!(
            "{}-{}",
            project.prefix,
            next_id(&pm.dir.join(&project.key), &project.prefix)?
        ),
    };
    let dir = pm.dir.join(&project.key).join(&id);
    if dir.exists() {
        return Err(Error::rejected(format!(
            "Issue '{id}' already exists at {}",
            dir.display()
        )));
    }
    let mut front = Front::new(&id, title, &time::iso(time::now_epoch()));
    front.priority = priority.to_string();
    // Owner is intent — `default_owner` stays a project.yaml hint for
    // later dispatch; it is never stamped onto new issues.
    front.owner = owner.map(str::to_string);
    front.component = component.map(str::to_string);
    front.tags = tags;
    front.blocked_by = blocked_by.to_vec();
    for dep in &front.blocked_by {
        model::check_id(dep)?;
    }
    let body = format!("{title}\n\n## Acceptance\n\n");
    if let Some(parent) = parent {
        front.parent = Some(model::check_id(parent)?);
    }
    std::fs::create_dir_all(dir.join("comments"))?;
    std::fs::create_dir_all(dir.join("artifacts"))?;
    save_front(&dir, &front, &body)?;
    // Structural checks run after the folder exists so failures leave
    // no half-built issue (the lock serialises the whole write).
    let issues = board::load_all(&pm.dir, None)?;
    if let Err(e) = check_structure(&issues, &id) {
        let _ = std::fs::remove_dir_all(&dir);
        return Err(e);
    }
    commit(pm, &format!("{id}: created"), &[&id], actor)?;
    Ok(json!({"id": id, "project": project.key, "path": dir, "committed": true}))
}

/// Reject shapes lint would flag, at write time: self-links, missing
/// targets, parent cycles and depth > 2, blocked_by cycles.
pub(crate) fn check_structure(issues: &[board::Issue], id: &str) -> Result<()> {
    let by_id: HashMap<&str, &board::Issue> =
        issues.iter().map(|i| (i.front.id.as_str(), i)).collect();
    let this = by_id
        .get(id)
        .ok_or_else(|| Error::internal("new issue missing from reload"))?;
    for dep in &this.front.blocked_by {
        if dep == &this.front.id {
            return Err(Error::rejected(format!("{id} cannot block itself")));
        }
        if !by_id.contains_key(dep.as_str()) {
            return Err(Error::rejected(format!(
                "blocked_by target '{dep}' does not exist — \
                 `cadence issue new \"title\"` creates it first"
            )));
        }
    }
    // blocked_by cycle: walk targets; a return to id is a cycle.
    let mut seen = HashSet::new();
    let mut stack: Vec<&str> = this.front.blocked_by.iter().map(String::as_str).collect();
    while let Some(cur) = stack.pop() {
        if cur == id {
            return Err(Error::rejected(format!(
                "blocked_by would create a cycle through {id} — \
                 reorder the dependency instead"
            )));
        }
        if seen.insert(cur) {
            if let Some(node) = by_id.get(cur) {
                stack.extend(node.front.blocked_by.iter().map(String::as_str));
            }
        }
    }
    check_parent(&by_id, this)
}

/// Parent rules: target exists, not self, no cycle, depth stays ≤ 2.
fn check_parent(by_id: &HashMap<&str, &board::Issue>, this: &board::Issue) -> Result<()> {
    let Some(parent) = &this.front.parent else {
        return Ok(());
    };
    let id = this.front.id.as_str();
    if parent == id {
        return Err(Error::rejected(format!("{id} cannot be its own parent")));
    }
    let Some(parent_issue) = by_id.get(parent.as_str()) else {
        return Err(Error::rejected(format!(
            "parent '{parent}' does not exist — create it with \
             `cadence issue new` first"
        )));
    };
    if parent_issue.front.parent.is_some() {
        return Err(Error::rejected(format!(
            "{parent} is itself a sub-issue — depth is two levels; \
             parent {id} under a root issue instead"
        )));
    }
    if by_id
        .values()
        .any(|i| i.front.parent.as_deref() == Some(id))
    {
        return Err(Error::rejected(format!(
            "{id} has children — making it a sub-issue would exceed depth 2"
        )));
    }
    // Cycle guard for hand-edited files (writes can never make one).
    let mut cur = parent_issue;
    while let Some(up) = &cur.front.parent {
        if up == id {
            return Err(Error::rejected(format!(
                "parent link would cycle through {id}"
            )));
        }
        match by_id.get(up.as_str()) {
            Some(next) => cur = next,
            None => break,
        }
    }
    Ok(())
}

/// Apply `key=value` pairs to one front in memory; returns the
/// `key=value` summary tokens. Nothing is written here, so a bulk edit
/// can validate every issue before it touches a file.
fn apply_pairs(
    project: &project::Project,
    front: &mut Front,
    pairs: &[String],
) -> Result<Vec<String>> {
    let mut changed = Vec::new();
    for kv in pairs {
        let (key, value) = kv
            .split_once('=')
            .ok_or_else(|| Error::rejected(format!("set pair '{kv}' is not key=value")))?;
        if !model::SETTABLE.contains(&key) {
            return Err(Error::rejected(format!(
                "'{key}' is not settable — one of {}",
                model::SETTABLE.join(" ")
            )));
        }
        match key {
            "status" => {
                model::check_status(value)?;
                front.status = value.to_string();
            }
            "priority" => {
                model::check_priority(value)?;
                front.priority = value.to_string();
            }
            "title" => {
                if value.is_empty() {
                    return Err(Error::rejected("title cannot be empty"));
                }
                front.title = value.to_string();
            }
            "owner" => front.owner = (!value.is_empty()).then(|| value.to_string()),
            "component" => {
                if !value.is_empty() {
                    check_component(project, value)?;
                }
                front.component = (!value.is_empty()).then(|| value.to_string());
            }
            "tags" => {
                front.tags = check_tags(project, &split_tags(value))?;
                changed.push(format!("tags={}", front.tags.join(",")));
                continue;
            }
            _ => unreachable!(),
        }
        changed.push(format!("{key}={value}"));
    }
    Ok(changed)
}

/// One issue of a bulk edit, loaded and edited in memory.
struct Staged {
    id: String,
    dir: PathBuf,
    front: Front,
    body: String,
}

/// Load every id (each once, in the order given) and run `edit` on its
/// front; `edit` answers whether it changed anything, and unchanged
/// issues drop out of the batch. Any unknown id or rejected edit fails
/// the whole batch before a single file is written. Call under the PM
/// lock.
fn stage(
    pm: &Pm,
    ids: &[String],
    mut edit: impl FnMut(&project::Project, &mut Front) -> Result<bool>,
) -> Result<Vec<Staged>> {
    let mut staged: Vec<Staged> = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();
    for id in ids {
        if !seen.insert(id) {
            continue;
        }
        let (project, dir) = issue_dir(pm, id)?;
        let (mut front, body) = load_front(&dir)?;
        let changed = edit(&project, &mut front).map_err(|e| match e {
            Error::Rejected(m) if ids.len() > 1 => Error::rejected(format!("{id}: {m}")),
            other => other,
        })?;
        if !changed {
            continue;
        }
        staged.push(Staged {
            id: id.clone(),
            dir,
            front,
            body,
        });
    }
    Ok(staged)
}

/// Write the staged fronts and make the one commit that names them all
/// — subject `<ID>[, <ID>…]: <summary>`, one `Issue:` trailer per id.
fn commit_staged(pm: &Pm, staged: &[Staged], summary: &str, actor: &str) -> Result<Vec<String>> {
    for s in staged {
        save_front(&s.dir, &s.front, &s.body)?;
    }
    let ids: Vec<&str> = staged.iter().map(|s| s.id.as_str()).collect();
    commit(pm, &format!("{}: {summary}", ids.join(", ")), &ids, actor)?;
    Ok(ids.iter().map(|i| i.to_string()).collect())
}

/// `issue set <ID>… key=value…` — the writable frontmatter fields, on
/// one issue or several. All-or-nothing: every id and every pair is
/// validated before any file changes, and the batch is one commit.
pub fn set_fields(pm: &Pm, ids: &[String], pairs: &[String], actor: &str) -> Result<Value> {
    if ids.is_empty() || pairs.is_empty() {
        return Err(Error::rejected(
            "set needs an id and key=value pairs — e.g. `cadence issue set CAD-16 status=doing`",
        ));
    }
    let _lock = pm.lock()?;
    let mut changed = Vec::new();
    let staged = stage(pm, ids, |project, front| {
        changed = apply_pairs(project, front, pairs)?;
        Ok(true)
    })?;
    let ids = commit_staged(pm, &staged, &format!("set {}", changed.join(" ")), actor)?;
    // The post-merge reminder (CAD-94): when this set marks an issue
    // done while a worktree ref is still open, the CLI prints the
    // one-line `issue finish` hint for each of these ids.
    let worktree_open: Vec<&String> = if pairs.iter().any(|p| {
        p.split_once('=').is_some_and(|(k, v)| {
            k.trim().eq_ignore_ascii_case("status") && v.trim().eq_ignore_ascii_case("done")
        })
    }) {
        staged
            .iter()
            .filter(|s| {
                s.front
                    .refs
                    .iter()
                    .any(|r| r.kind == "worktree" && r.closed != Some(true))
            })
            .map(|s| &s.id)
            .collect()
    } else {
        Vec::new()
    };
    Ok(json!({"id": ids[0], "ids": ids, "set": changed,
              "worktree_open": worktree_open, "committed": true}))
}

/// `issue tag <ID>… add|rm <tag>…` — add or remove tags on one issue or
/// several, same all-or-nothing batch as `set`. Adding a tag an issue
/// already has, or removing one it lacks, leaves that issue unchanged;
/// a batch that changes nothing is refused.
pub fn tag_edit(pm: &Pm, ids: &[String], add: bool, tags: &[String], actor: &str) -> Result<Value> {
    let verb = if add { "add" } else { "rm" };
    if ids.is_empty() || tags.is_empty() {
        return Err(Error::rejected(
            "tag needs ids, add|rm and tags — e.g. `cadence issue tag CAD-16 CAD-17 add ui`",
        ));
    }
    let tags = model::normalize_tags(tags)?;
    let _lock = pm.lock()?;
    let staged = stage(pm, ids, |project, front| {
        let before = front.tags.clone();
        if add {
            let mut all = before.clone();
            all.extend(tags.iter().cloned());
            front.tags = check_tags(project, &all)?;
        } else {
            front.tags.retain(|t| !tags.contains(t));
        }
        Ok(front.tags != before)
    })?;
    if staged.is_empty() {
        return Err(Error::rejected(format!(
            "tag {verb} {} changes nothing — `cadence issue show <ID>` lists tags",
            tags.join(" ")
        )));
    }
    let tagged: Vec<Value> = staged
        .iter()
        .map(|s| json!({"id": s.id, "tags": s.front.tags}))
        .collect();
    let ids = commit_staged(
        pm,
        &staged,
        &format!("tag {verb} {}", tags.join(" ")),
        actor,
    )?;
    Ok(json!({"ids": ids, "tag": verb, "tags": tags, "issues": tagged, "committed": true}))
}

/// The HTTP PATCH surface: typed fields instead of `key=value` pairs,
/// `body` replaces the markdown body (frontmatter untouched), `if_rev`
/// optimistic concurrency, and a derived status refuses `status`.
pub struct IssuePatch {
    pub status: Option<String>,
    pub priority: Option<String>,
    /// `Some("")` clears owner.
    pub owner: Option<String>,
    /// `Some("")` clears component.
    pub component: Option<String>,
    pub title: Option<String>,
    pub body: Option<String>,
    /// Replaces the tag list; `Some([])` clears it.
    pub tags: Option<Vec<String>>,
}

pub fn patch_issue(
    pm: &Pm,
    id: &str,
    patch: &IssuePatch,
    if_rev: Option<&str>,
    actor: &str,
    state_dir: Option<&Path>,
) -> Result<Value> {
    let (project, dir) = issue_dir(pm, id)?;
    let _lock = pm.lock()?;
    if let Some(conflict) = check_rev(&dir, if_rev)? {
        return Ok(conflict);
    }
    if patch.status.is_some() {
        // A derived status is not file-writable: roll-up containers,
        // job-bound and note-driven issues refuse with the reason.
        let issues = board::load_all(&pm.dir, None)?;
        let jobs = state_dir.map(board::fetch_job_outcomes).unwrap_or_default();
        let views = board::views_with_jobs(&pm.config.notes_dir(), issues, &jobs);
        let src = views
            .iter()
            .find(|v| v.issue.front.id == id)
            .map(|v| v.status_source)
            .unwrap_or("file");
        if src != "file" {
            return Ok(json!({
                "id": id,
                "conflict": "status_derived",
                "status_source": src,
                "reason": format!(
                    "{id} status is derived from {src} — the file field is not authoritative"
                ),
            }));
        }
    }
    let (mut front, mut body) = load_front(&dir)?;
    let mut changed = Vec::new();
    if let Some(v) = &patch.status {
        model::check_status(v)?;
        front.status = v.clone();
        changed.push(format!("status={v}"));
    }
    if let Some(v) = &patch.priority {
        model::check_priority(v)?;
        front.priority = v.clone();
        changed.push(format!("priority={v}"));
    }
    if let Some(v) = &patch.title {
        if v.is_empty() {
            return Err(Error::rejected("title cannot be empty"));
        }
        front.title = v.clone();
        changed.push("title".to_string());
    }
    if let Some(v) = &patch.owner {
        front.owner = (!v.is_empty()).then(|| v.clone());
        changed.push(format!("owner={v}"));
    }
    if let Some(v) = &patch.component {
        if !v.is_empty() {
            check_component(&project, v)?;
        }
        front.component = (!v.is_empty()).then(|| v.clone());
        changed.push(format!("component={v}"));
    }
    if let Some(v) = &patch.tags {
        front.tags = check_tags(&project, v)?;
        changed.push(format!("tags={}", front.tags.join(",")));
    }
    if let Some(v) = &patch.body {
        body = v.clone();
        changed.push("body".to_string());
    }
    if changed.is_empty() {
        return Err(Error::rejected(
            "nothing to patch — send at least one field",
        ));
    }
    save_front(&dir, &front, &body)?;
    commit(
        pm,
        &format!("{id}: set {}", changed.join(" ")),
        &[id],
        actor,
    )?;
    let warnings = blocked_warnings(pm, id, state_dir)?;
    Ok(json!({"id": id, "set": changed, "committed": true, "warnings": warnings}))
}

/// `issue link` / `issue unlink`. `blocked_by`/`relates` are list
/// fields; `parent`/`duplicate_of` are scalars.
#[allow(clippy::too_many_arguments)]
pub fn link(
    pm: &Pm,
    id: &str,
    kind: &str,
    target: &str,
    unlink: bool,
    if_rev: Option<&str>,
    actor: &str,
    state_dir: Option<&Path>,
) -> Result<Value> {
    model::check_link_kind(kind)?;
    model::check_id(target)?;
    let (_project, dir) = issue_dir(pm, id)?;
    let _lock = pm.lock()?;
    if let Some(conflict) = check_rev(&dir, if_rev)? {
        return Ok(conflict);
    }
    let (mut front, body) = load_front(&dir)?;
    let verb = if unlink { "unlink" } else { "link" };
    match kind {
        "blocked_by" | "relates" => {
            let list = if kind == "blocked_by" {
                &mut front.blocked_by
            } else {
                &mut front.relates
            };
            if unlink {
                if !list.iter().any(|t| t == target) {
                    return Err(Error::rejected(format!(
                        "{id} has no {kind} link to {target} — \
                         `cadence issue show {id}` lists links"
                    )));
                }
                list.retain(|t| t != target);
            } else {
                if target == id {
                    return Err(Error::rejected(format!("{id} cannot link to itself")));
                }
                board::find_issue(&pm.dir, target)?; // must exist
                if list.iter().any(|t| t == target) {
                    return Err(Error::rejected(format!(
                        "{id} already links {kind} {target} — nothing to do"
                    )));
                }
                list.push(target.to_string());
            }
        }
        "parent" | "duplicate_of" => {
            let slot = if kind == "parent" {
                &mut front.parent
            } else {
                &mut front.duplicate_of
            };
            if unlink {
                if slot.as_deref() != Some(target) {
                    return Err(Error::rejected(format!(
                        "{id} has {kind}={} — not {target}",
                        slot.as_deref().unwrap_or("none")
                    )));
                }
                *slot = None;
            } else {
                if target == id {
                    return Err(Error::rejected(format!("{id} cannot link to itself")));
                }
                board::find_issue(&pm.dir, target)?;
                *slot = Some(target.to_string());
            }
        }
        _ => unreachable!(),
    }
    // Structural check before the file is written — a rejected link
    // leaves the folder untouched.
    let issues = board::load_all(&pm.dir, None)?;
    let mut preview: Vec<board::Issue> = issues;
    if let Some(this) = preview.iter_mut().find(|i| i.front.id == id) {
        this.front = front.clone();
    }
    check_structure(&preview, id)?;
    save_front(&dir, &front, &body)?;
    commit(
        pm,
        &format!("{id}: {verb} {kind} {target}"),
        &[id, target],
        actor,
    )?;
    let warnings = blocked_warnings(pm, id, state_dir)?;
    Ok(json!({"id": id, "link": kind, "target": target,
              "unlink": unlink, "committed": true, "warnings": warnings}))
}

/// `issue ref <ID> <kind> <url-or-path> [--label x]`. A scheme makes it
/// `url:`; everything else is a `path:` (previews store publish paths).
/// `worktree` scopes a `message` ref to the pair it was dispatched
/// against; other kinds leave it `None`.
#[allow(clippy::too_many_arguments)]
pub fn add_ref(
    pm: &Pm,
    id: &str,
    kind: &str,
    target: &str,
    label: Option<&str>,
    worktree: Option<&str>,
    agent: Option<&str>,
    if_rev: Option<&str>,
    actor: &str,
) -> Result<Value> {
    model::check_ref_kind(kind)?;
    model::check_ref_value(target)?;
    let (_project, dir) = issue_dir(pm, id)?;
    let _lock = pm.lock()?;
    if let Some(conflict) = check_rev(&dir, if_rev)? {
        return Ok(conflict);
    }
    let (mut front, body) = load_front(&dir)?;
    let is_url = target.starts_with("http://") || target.starts_with("https://");
    let r = Ref {
        kind: kind.to_string(),
        url: is_url.then(|| target.to_string()),
        path: (!is_url).then(|| target.to_string()),
        label: label.map(str::to_string),
        closed: None,
        worktree: worktree.map(str::to_string),
        cargo_target: None,
        agent: agent.map(str::to_string),
    };
    front.refs.push(r);
    save_front(&dir, &front, &body)?;
    commit(pm, &format!("{id}: ref {kind}"), &[id], actor)?;
    Ok(json!({"id": id, "ref": {"kind": kind, "target": target},
              "committed": true}))
}

/// Mark every open `<kind>` ref naming `target` closed — kept as
/// history but no longer counted by `issue finish`. A dispatch whose
/// send failed leaves its pre-recorded `message` ref orphaned;
/// closing it keeps the attempt's history without letting it read as
/// a live binding (or a "dispatch recorded during finish" stale
/// reason) to a concurrent finish.
pub fn close_ref(pm: &Pm, id: &str, kind: &str, target: &str, actor: &str) -> Result<()> {
    let (_project, dir) = issue_dir(pm, id)?;
    let _lock = pm.lock()?;
    let (mut front, body) = load_front(&dir)?;
    let mut hit = false;
    for r in &mut front.refs {
        if r.kind == kind && r.path.as_deref() == Some(target) && r.closed != Some(true) {
            r.closed = Some(true);
            hit = true;
        }
    }
    if hit {
        save_front(&dir, &front, &body)?;
        commit(pm, &format!("{id}: ref {kind} closed"), &[id], actor)?;
    }
    Ok(())
}

/// `issue comment <ID> -m|--file` — one create-only file under
/// `comments/`, named by UTC time + author.
pub fn add_comment(
    pm: &Pm,
    id: &str,
    body: &str,
    author: Option<&str>,
    kind: Option<&str>,
    if_rev: Option<&str>,
    actor: &str,
) -> Result<Value> {
    let (_project, dir) = issue_dir(pm, id)?;
    let author_opt = author;
    let author = author_opt
        .map(str::to_string)
        .or_else(|| std::env::var("CADENCE_ALIAS").ok())
        .unwrap_or_else(|| "operator".to_string());
    if author.is_empty()
        || author.len() > 64
        || !author
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(Error::rejected(format!(
            "Bad comment author '{author}' — letters, digits, '-' or '_'"
        )));
    }
    if body.trim().is_empty() {
        return Err(Error::rejected("Comment body is empty — pass -m or --file"));
    }
    // CAD-109: a credential-shaped body is refused before anything is
    // written; warn-only findings ride along in the result.
    let secret_warnings = crate::secret::guard(&format!("{id}: comment"), body)?;
    let _lock = pm.lock()?;
    if let Some(conflict) = check_rev(&dir, if_rev)? {
        return Ok(conflict);
    }
    let comments = dir.join("comments");
    if comments.symlink_metadata().is_ok_and(|m| m.is_symlink()) {
        return Err(Error::rejected(format!(
            "{id}: comments/ is a symlink — refusing to write outside the PM dir"
        )));
    }
    std::fs::create_dir_all(&comments)?;
    let epoch = time::now_epoch();
    let front = crate::issue::model::CommentFront {
        author: author.clone(),
        at: time::iso(epoch),
        kind: kind.map(str::to_string),
    };
    let text = parse::render(&front, body)?;
    let path = create_exclusive(
        &comments,
        &format!("{}-{author}.md", time::basic(epoch)),
        text.as_bytes(),
    )?;
    commit_who(
        pm,
        &format!("{id}: comment by {author}"),
        &[id],
        actor,
        author_opt,
    )?;
    let mut out = json!({"id": id, "comment": path.file_name().map(|n| n.to_string_lossy().to_string()),
              "author": author, "committed": true});
    if !secret_warnings.is_empty() {
        out["secret_warnings"] = crate::secret::warnings_json(&secret_warnings);
    }
    Ok(out)
}

/// `issue attach <ID> <file>` — copy into `artifacts/` (basename only),
/// create-only, under `artifact_max_bytes`.
pub fn attach(pm: &Pm, id: &str, file: &Path) -> Result<Value> {
    let meta = std::fs::metadata(file)
        .map_err(|e| Error::rejected(format!("Cannot read {}: {e}", file.display())))?;
    if !meta.is_file() {
        return Err(Error::rejected(format!("{} is not a file", file.display())));
    }
    if meta.len() > pm.config.artifact_max_bytes {
        return Err(Error::rejected(format!(
            "{} is {} bytes — over the {} cap; link it instead: \
             `cadence issue ref {id} url <location>`",
            file.display(),
            meta.len(),
            pm.config.artifact_max_bytes
        )));
    }
    let name = file
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .ok_or_else(|| Error::rejected("Attachment needs a plain file name"))?;
    let bytes = std::fs::read(file)?;
    attach_bytes(pm, id, &name, &bytes, true, "")
}

/// Attach raw bytes as `artifacts/<name>` — the upload route's path.
/// Names follow the read grammar so every stored file is fetchable.
/// With `autorename` a collision becomes `<stem>-N<ext>` (CLI parity);
/// without it an existing name is a conflict the route maps to 409.
pub fn attach_bytes(
    pm: &Pm,
    id: &str,
    name: &str,
    bytes: &[u8],
    autorename: bool,
    actor: &str,
) -> Result<Value> {
    let (_project, dir) = issue_dir(pm, id)?;
    if !model::valid_artifact_name(name) {
        return Err(Error::rejected(format!(
            "Bad artifact name '{name}' — [A-Za-z0-9._-]{{1,120}}, no leading dot"
        )));
    }
    if bytes.len() as u64 > pm.config.artifact_max_bytes {
        return Err(Error::rejected(format!(
            "{name} is {} bytes — over the {} cap; link it instead",
            bytes.len(),
            pm.config.artifact_max_bytes
        )));
    }
    let _lock = pm.lock()?;
    let artifacts = dir.join("artifacts");
    if artifacts.symlink_metadata().is_ok_and(|m| m.is_symlink()) {
        return Err(Error::rejected(format!(
            "{id}: artifacts/ is a symlink — refusing to write outside the PM dir"
        )));
    }
    std::fs::create_dir_all(&artifacts)?;
    let path = if autorename {
        create_exclusive(&artifacts, name, bytes)?
    } else {
        let target = artifacts.join(name);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)
        {
            Ok(mut file) => {
                file.write_all(bytes)?;
                target
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                return Ok(json!({"id": id, "conflict": "exists", "artifact": name}));
            }
            Err(e) => return Err(e.into()),
        }
    };
    commit(pm, &format!("{id}: attach {name}"), &[id], actor)?;
    Ok(
        json!({"id": id, "artifact": path.file_name().map(|n| n.to_string_lossy().to_string()),
              "size": bytes.len(), "committed": true}),
    )
}
