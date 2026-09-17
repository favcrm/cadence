//! The write side — the only writer for issue folders. Every op takes
//! the PM lock, mutates files (`issue.md` via temp+rename; comments and
//! artifacts create-only), then makes one git commit.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::issue::model::{self, Front, Ref};
use crate::issue::{board, parse, project, time, Pm};

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
fn issue_dir(pm: &Pm, id: &str) -> Result<(project::Project, PathBuf)> {
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

fn load_front(dir: &Path) -> Result<(Front, String)> {
    let text = std::fs::read_to_string(dir.join("issue.md"))?;
    parse::parse_issue(&text)
}

fn save_front(dir: &Path, front: &Front, body: &str) -> Result<()> {
    atomic_write(&dir.join("issue.md"), &parse::render(front, body)?)
}

/// `project add` — create `<pm>/<key>/project.yaml`.
#[allow(clippy::too_many_arguments)]
pub fn project_add(
    pm: &Pm,
    key: &str,
    prefix: &str,
    repos: &[String],
    components: &[String],
    owner: Option<&str>,
) -> Result<Value> {
    model::check_key(key)?;
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
        default_owner: owner.map(str::to_string),
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
    pm.commit(&format!("project {key} added"))?;
    Ok(json!({"project": key, "prefix": project.prefix,
              "path": dir, "committed": true}))
}

/// Allocate the next id under the write lock: `<PREFIX>-<max+1>`.
fn next_id(dir: &Path, prefix: &str) -> Result<u64> {
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
    explicit_id: Option<&str>,
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
        if !project.components.is_empty() && !project.components.iter().any(|c| c == component) {
            return Err(Error::rejected(format!(
                "Unknown component '{component}' — {} declares: {}",
                project.key,
                project.components.join(", ")
            )));
        }
    }
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
    pm.commit(&format!("{id}: created"))?;
    Ok(json!({"id": id, "project": project.key, "path": dir, "committed": true}))
}

/// Reject shapes lint would flag, at write time: self-links, missing
/// targets, parent cycles and depth > 2, blocked_by cycles.
fn check_structure(issues: &[board::Issue], id: &str) -> Result<()> {
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

/// `issue set <ID> key=value…` — the writable frontmatter fields.
pub fn set_fields(pm: &Pm, id: &str, pairs: &[String]) -> Result<Value> {
    if pairs.is_empty() {
        return Err(Error::rejected(
            "set needs key=value pairs — e.g. `cadence issue set CAD-16 status=doing`",
        ));
    }
    let (_project, dir) = issue_dir(pm, id)?;
    let _lock = pm.lock()?;
    let (mut front, body) = load_front(&dir)?;
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
            "component" => front.component = (!value.is_empty()).then(|| value.to_string()),
            _ => unreachable!(),
        }
        changed.push(format!("{key}={value}"));
    }
    save_front(&dir, &front, &body)?;
    pm.commit(&format!("{id}: set {}", changed.join(" ")))?;
    Ok(json!({"id": id, "set": changed, "committed": true}))
}

/// `issue link` / `issue unlink`. `blocked_by`/`relates` are list
/// fields; `parent`/`duplicate_of` are scalars.
pub fn link(pm: &Pm, id: &str, kind: &str, target: &str, unlink: bool) -> Result<Value> {
    model::check_link_kind(kind)?;
    model::check_id(target)?;
    let (_project, dir) = issue_dir(pm, id)?;
    let _lock = pm.lock()?;
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
    pm.commit(&format!("{id}: {verb} {kind} {target}"))?;
    Ok(json!({"id": id, "link": kind, "target": target,
              "unlink": unlink, "committed": true}))
}

/// `issue ref <ID> <kind> <url-or-path> [--label x]`. A scheme makes it
/// `url:`; everything else is a `path:` (previews store publish paths).
pub fn add_ref(pm: &Pm, id: &str, kind: &str, target: &str, label: Option<&str>) -> Result<Value> {
    model::check_ref_kind(kind)?;
    let (_project, dir) = issue_dir(pm, id)?;
    let _lock = pm.lock()?;
    let (mut front, body) = load_front(&dir)?;
    let is_url = target.starts_with("http://") || target.starts_with("https://");
    let r = Ref {
        kind: kind.to_string(),
        url: is_url.then(|| target.to_string()),
        path: (!is_url).then(|| target.to_string()),
        label: label.map(str::to_string),
    };
    front.refs.push(r);
    save_front(&dir, &front, &body)?;
    pm.commit(&format!("{id}: ref {kind}"))?;
    Ok(json!({"id": id, "ref": {"kind": kind, "target": target},
              "committed": true}))
}

/// `issue comment <ID> -m|--file` — one create-only file under
/// `comments/`, named by UTC time + author.
pub fn add_comment(
    pm: &Pm,
    id: &str,
    body: &str,
    author: Option<&str>,
    kind: Option<&str>,
) -> Result<Value> {
    let (_project, dir) = issue_dir(pm, id)?;
    let author = author
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
    let _lock = pm.lock()?;
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
    pm.commit(&format!("{id}: comment by {author}"))?;
    Ok(
        json!({"id": id, "comment": path.file_name().map(|n| n.to_string_lossy().to_string()),
              "author": author, "committed": true}),
    )
}

/// `issue attach <ID> <file>` — copy into `artifacts/` (basename only),
/// create-only, under `artifact_max_bytes`.
pub fn attach(pm: &Pm, id: &str, file: &Path) -> Result<Value> {
    let (_project, dir) = issue_dir(pm, id)?;
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
        .filter(|n| !n.is_empty() && !n.starts_with('.'))
        .ok_or_else(|| Error::rejected("Attachment needs a plain file name"))?;
    let bytes = std::fs::read(file)?;
    let _lock = pm.lock()?;
    let artifacts = dir.join("artifacts");
    if artifacts.symlink_metadata().is_ok_and(|m| m.is_symlink()) {
        return Err(Error::rejected(format!(
            "{id}: artifacts/ is a symlink — refusing to write outside the PM dir"
        )));
    }
    std::fs::create_dir_all(&artifacts)?;
    let path = create_exclusive(&artifacts, &name, &bytes)?;
    pm.commit(&format!("{id}: attach {name}"))?;
    Ok(
        json!({"id": id, "artifact": path.file_name().map(|n| n.to_string_lossy().to_string()),
              "size": meta.len(), "committed": true}),
    )
}
