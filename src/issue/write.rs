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
/// reads instead of the git author. `paths` are exactly the files the
/// write touched — they are all the commit stages (CAD-454); the
/// returned list names foreign paths the commit saw but left alone.
pub(crate) fn commit(
    pm: &Pm,
    paths: &[PathBuf],
    message: &str,
    ids: &[&str],
    actor: &str,
) -> Result<Vec<String>> {
    commit_who(pm, paths, message, ids, actor, None)
}

pub(crate) fn commit_who(
    pm: &Pm,
    paths: &[PathBuf],
    message: &str,
    ids: &[&str],
    actor: &str,
    who: Option<&str>,
) -> Result<Vec<String>> {
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
    pm.commit(paths, &format!("{subject}\n\n{trailers}"))
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

/// A tracker file's bytes before this write touches it — `None` when
/// it does not exist. Feeds [`restore_preimage`] when the commit
/// fails, so a refused write leaves neither an index entry (the
/// commit unstages its own paths) nor a half-written file (CAD-454).
fn file_preimage(path: &Path) -> Option<Vec<u8>> {
    std::fs::read(path).ok()
}

/// Undo what a write did to `path`: the old bytes go back where they
/// were; a file this write created is removed.
fn restore_preimage(path: &Path, prev: Option<Vec<u8>>) {
    match prev {
        Some(bytes) => {
            let _ = std::fs::write(path, bytes);
        }
        None => {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Fold the foreign paths a commit saw into the write's JSON result —
/// `foreign_files` is present only when the tracker held files the
/// write did not stage.
pub(crate) fn attach_foreign(out: &mut Value, foreign: &[String]) {
    if !foreign.is_empty() {
        out["foreign_files"] = json!(foreign);
    }
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
    let file = dir.join("issue.md");
    let prev = file_preimage(&file);
    save_front(&dir, &front, &body)?;
    let foreign = match commit(
        pm,
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
    crate::issue::project_new::check_prefix(&prefix)?;
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
    let yaml_path = dir.join("project.yaml");
    std::fs::write(&yaml_path, yaml)?;
    let foreign = match commit(
        pm,
        std::slice::from_ref(&yaml_path),
        &format!("project {key} added"),
        &[],
        "",
    ) {
        Ok(f) => f,
        Err(e) => {
            let _ = std::fs::remove_file(&yaml_path);
            return Err(e);
        }
    };
    let mut out = json!({"project": key, "prefix": project.prefix,
              "path": dir, "committed": true});
    attach_foreign(&mut out, &foreign);
    Ok(out)
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

/// CAD-614: `issue new` always creates a backlog ticket. The master
/// may not pass `--owner` or `--id` (those are operator overrides);
/// any caller passing `--status` other than `backlog` is refused.
/// Refusals name the allowed form.
pub fn master_issue_new_limits(
    caller_is_master: bool,
    owner: Option<&str>,
    explicit_id: Option<&str>,
    status: Option<&str>,
) -> Result<()> {
    if let Some(status) = status {
        if status != "backlog" {
            return Err(Error::rejected(
                "issue new creates a backlog ticket — omit --status or pass --status backlog. \
                 The master cannot move status; that stays the operator's `issue set`",
            ));
        }
    }
    if !caller_is_master {
        return Ok(());
    }
    if owner.is_some() {
        return Err(Error::rejected(
            "the master creates backlog tickets without --owner — omit it; the ticket records actor=master",
        ));
    }
    if explicit_id.is_some() {
        return Err(Error::rejected(
            "the master does not choose ticket ids — omit --id",
        ));
    }
    Ok(())
}

/// `issue new` — one folder + issue.md, under the id lock.
/// `body`, when set, replaces the default title-plus-empty-acceptance
/// body (CAD-614: the master writes it under `master/tmp` and passes
/// `--file`).
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
    body: Option<&str>,
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
    let body = body
        .map(str::to_string)
        .unwrap_or_else(|| format!("{title}\n\n## Acceptance\n\n"));
    if let Some(parent) = parent {
        model::check_id(parent)?;
        // CAD-360: nothing new is parented to a plan epic.
        crate::issue::plan::check_parent_change(&pm.dir, &front, Some(parent))?;
        front.parent = Some(parent.to_string());
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
    let foreign = match commit(
        pm,
        &[dir.join("issue.md")],
        &format!("{id}: created"),
        &[&id],
        actor,
    ) {
        Ok(f) => f,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&dir);
            return Err(e);
        }
    };
    let mut out = json!({"id": id, "project": project.key, "path": dir, "committed": true});
    attach_foreign(&mut out, &foreign);
    Ok(out)
}

/// CAD-359 `plan propose` — the epic (the plan, `plan.state:
/// proposed`) and one child per ticket in `backlog`, with acceptance,
/// sizes, suggested agents (`owner`) and `blocked_by` links, all in ONE
/// tracker commit. Ids are allocated under the lock; any structural
/// refusal removes every folder this call created.
pub fn create_plan(
    pm: &Pm,
    project_key: &str,
    doc: &crate::issue::plan::PlanDoc,
    workflow: Option<&str>,
    actor: &str,
) -> Result<Value> {
    use crate::issue::plan::Dep;
    let project = project::list(&pm.dir)?
        .into_iter()
        .find(|p| p.key == project_key)
        .ok_or_else(|| {
            Error::rejected(format!(
                "Unknown project '{project_key}' — `cadence issue project ls` lists them"
            ))
        })?;
    let proposer = actor_who(actor, None);
    let _lock = pm.lock()?;
    let root = pm.dir.join(&project.key);
    let first = next_id(&root, &project.prefix)?;
    let id_at = |n: u64| format!("{}-{}", project.prefix, first + n);
    let epic = id_at(0);
    let ids: Vec<String> = (0..=doc.tickets.len() as u64).map(id_at).collect();
    let now = time::iso(time::now_epoch());

    let mut epic_front = Front::new(&epic, &doc.title, &now);
    epic_front.plan = Some(model::Plan {
        state: "proposed".to_string(),
        proposed_by: proposer.clone(),
        proposed_at: now.clone(),
        decided_by: None,
        decided_at: None,
        reason: None,
        workflow: workflow.map(str::to_string),
        tickets: ids[1..].to_vec(),
    });
    epic_front.item_type = Some("epic".to_string());
    let mut epic_body = format!("{}\n\n## Goal\n\n{}\n", doc.title, doc.goal);
    if !doc.non_goals.is_empty() {
        epic_body.push_str("\n## Non-goals\n\n");
        for g in &doc.non_goals {
            epic_body.push_str(&format!("- {g}\n"));
        }
    }
    if !doc.intro.is_empty() {
        epic_body.push_str(&format!("\n## Plan\n\n{}\n", doc.intro));
    }
    let mut files: Vec<(PathBuf, Front, String)> = vec![(root.join(&epic), epic_front, epic_body)];
    for (n, ticket) in doc.tickets.iter().enumerate() {
        let id = &ids[n + 1];
        let mut front = Front::new(id, &ticket.title, &now);
        front.parent = Some(epic.clone());
        front.plan_epic = Some(epic.clone());
        front.size = ticket.size.clone();
        front.owner = ticket.agent.clone();
        front.blocked_by = ticket
            .depends_on
            .iter()
            .map(|d| match d {
                Dep::Ticket(k) => ids[k + 1].clone(),
                Dep::Issue(i) => i.clone(),
            })
            .collect();
        let mut body = format!("{}\n", ticket.title);
        if !ticket.description.is_empty() {
            body.push_str(&format!("\n{}\n", ticket.description));
        }
        let body = parse::replace_acceptance(&body, &ticket.acceptance)?;
        files.push((root.join(id), front, body));
    }
    if let Some((dir, _, _)) = files.iter().find(|(dir, _, _)| dir.exists()) {
        return Err(Error::rejected(format!(
            "{} already exists — refusing to overwrite it",
            dir.display()
        )));
    }
    let created = || files.iter().map(|(dir, _, _)| dir);
    let build = || -> Result<Vec<String>> {
        for (dir, front, body) in &files {
            std::fs::create_dir_all(dir.join("comments"))?;
            std::fs::create_dir_all(dir.join("artifacts"))?;
            save_front(dir, front, body)?;
        }
        let issues = board::load_all(&pm.dir, None)?;
        for id in &ids {
            check_structure(&issues, id)?;
        }
        let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
        let paths: Vec<PathBuf> = files
            .iter()
            .map(|(dir, _, _)| dir.join("issue.md"))
            .collect();
        commit(
            pm,
            &paths,
            &format!(
                "{epic}: plan proposed — {} ({} tickets)",
                doc.title,
                doc.tickets.len()
            ),
            &refs,
            actor,
        )
    };
    let foreign = match build() {
        Ok(foreign) => foreign,
        Err(e) => {
            for dir in created() {
                let _ = std::fs::remove_dir_all(dir);
            }
            return Err(e);
        }
    };
    let mut out = json!({
        "epic": epic,
        "project": project.key,
        "title": doc.title,
        "state": "proposed",
        "proposed_by": proposer,
        "tickets": ids[1..],
        "committed": true,
    });
    attach_foreign(&mut out, &foreign);
    Ok(out)
}

/// CAD-360 `plan approve` / `plan reject` — record the operator's
/// decision on a `proposed` plan (state, who, when, why) and, on
/// approval, move its `backlog` tickets to `ready` — one commit. The
/// caller has already proven operator authority (the daemon's
/// connection-bound check); this only writes.
pub fn decide_plan(
    pm: &Pm,
    epic: &str,
    approve: bool,
    by: &str,
    reason: Option<&str>,
) -> Result<Value> {
    let reason = reason.map(str::trim).filter(|r| !r.is_empty());
    if !approve && reason.is_none() {
        return Err(Error::rejected("plan reject needs --reason"));
    }
    if let Some(reason) = reason {
        crate::secret::guard(&format!("{epic}: plan reason"), reason)?;
    }
    let (project, dir) = issue_dir(pm, epic)?;
    let _lock = pm.lock()?;
    let (mut front, body) = load_front(&dir)?;
    let Some(plan) = front.plan.as_mut() else {
        return Err(Error::rejected(format!(
            "{epic} is not a plan — `cadence plan show` needs a proposed plan"
        )));
    };
    if plan.state != "proposed" {
        return Err(Error::rejected(format!(
            "plan {epic} is already {} — only a proposed plan is decided",
            plan.state
        )));
    }
    let state = if approve { "approved" } else { "rejected" };
    plan.state = state.to_string();
    plan.decided_by = Some(by.to_string());
    plan.decided_at = Some(time::iso(time::now_epoch()));
    plan.reason = reason.map(str::to_string);
    let decided = plan.clone();
    let mut ready = vec![];
    let mut writes = vec![(dir.clone(), front.clone(), body)];
    if approve {
        for kid in board::load_all(&pm.dir, Some(&project.key))? {
            if decided.tickets.contains(&kid.front.id) && kid.front.status == "backlog" {
                let mut f = kid.front.clone();
                f.status = "ready".to_string();
                ready.push(f.id.clone());
                writes.push((kid.dir.clone(), f, kid.body.clone()));
            }
        }
    }
    // All or nothing: a failed write or commit restores every file.
    let originals: Vec<(PathBuf, String)> = writes
        .iter()
        .map(|(dir, _, _)| {
            let file = dir.join("issue.md");
            std::fs::read_to_string(&file).map(|text| (file, text))
        })
        .collect::<std::io::Result<_>>()?;
    let restore = || {
        for (file, text) in &originals {
            let _ = atomic_write(file, text);
        }
    };
    let mut ids: Vec<&str> = vec![epic];
    ids.extend(ready.iter().map(String::as_str));
    let subject = if approve {
        format!(
            "{epic}: plan approved by {by} ({} tickets ready)",
            ready.len()
        )
    } else {
        format!("{epic}: plan rejected by {by}")
    };
    let paths: Vec<PathBuf> = writes
        .iter()
        .map(|(dir, _, _)| dir.join("issue.md"))
        .collect();
    let written = writes
        .iter()
        .try_for_each(|(dir, front, body)| save_front(dir, front, body))
        .and_then(|_| commit_who(pm, &paths, &subject, &ids, "", Some(by)));
    let foreign = match written {
        Ok(f) => f,
        Err(e) => {
            restore();
            return Err(e);
        }
    };
    let mut out = json!({
        "epic": epic,
        "project": project.key,
        "state": state,
        "decided_by": decided.decided_by,
        "decided_at": decided.decided_at,
        "reason": decided.reason,
        "ready": ready,
        "committed": true,
    });
    attach_foreign(&mut out, &foreign);
    Ok(out)
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
        if key == "stage" || key == "stage_at" {
            return Err(Error::rejected(
                "an epic's stage is a gate decision — move it with \
                 `cadence issue epic stage <EPIC> <stage>`",
            ));
        }
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
            // CAD-378: advisory planned paths; an empty value clears.
            "paths" => {
                front.paths = crate::issue::areas::parse_paths(value)?;
                changed.push(format!("paths={}", front.paths.join(",")));
                continue;
            }
            // CAD-405: an empty value clears each of these.
            "type" => {
                if !value.is_empty() {
                    model::check_type(value)?;
                }
                front.item_type = (!value.is_empty()).then(|| value.to_string());
            }
            "milestone" => {
                if !value.is_empty() && !model::valid_tag(value) {
                    return Err(Error::rejected(format!(
                        "Invalid milestone '{value}' — 1-32 lowercase letters, digits or hyphens"
                    )));
                }
                front.milestone = (!value.is_empty()).then(|| value.to_string());
            }
            "size" => {
                let size = value.to_ascii_uppercase();
                if !size.is_empty() && !model::SIZES.iter().any(|(s, _)| *s == size) {
                    return Err(Error::rejected(format!(
                        "Unknown size '{value}' — one of S M L"
                    )));
                }
                front.size = (!size.is_empty()).then_some(size);
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
/// Only the staged issues' `issue.md` files reach the commit. Returns
/// the committed ids plus the foreign paths the commit saw; a failed
/// write or commit restores every file it touched (CAD-454).
fn commit_staged(
    pm: &Pm,
    staged: &[Staged],
    summary: &str,
    actor: &str,
) -> Result<(Vec<String>, Vec<String>)> {
    if staged.is_empty() {
        return Ok((vec![], vec![]));
    }
    let originals: Vec<(PathBuf, Option<Vec<u8>>)> = staged
        .iter()
        .map(|s| {
            let file = s.dir.join("issue.md");
            let prev = file_preimage(&file);
            (file, prev)
        })
        .collect();
    let paths: Vec<PathBuf> = originals.iter().map(|(f, _)| f.clone()).collect();
    let ids: Vec<&str> = staged.iter().map(|s| s.id.as_str()).collect();
    let written = staged
        .iter()
        .try_for_each(|s| save_front(&s.dir, &s.front, &s.body))
        .and_then(|_| {
            commit(
                pm,
                &paths,
                &format!("{}: {summary}", ids.join(", ")),
                &ids,
                actor,
            )
        });
    match written {
        Ok(foreign) => Ok((ids.iter().map(|i| i.to_string()).collect(), foreign)),
        Err(e) => {
            for (file, prev) in originals {
                restore_preimage(&file, prev);
            }
            Err(e)
        }
    }
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
        let before = front.status.clone();
        let milestone = front.milestone.clone();
        let item_type = front.item_type.clone();
        changed = apply_pairs(project, front, pairs)?;
        if front.status != before {
            // CAD-360: an unapproved plan's tickets stay in backlog.
            crate::issue::plan::check_status_write(&pm.dir, front, &front.status)?;
        }
        if front.milestone != milestone {
            check_milestone(pm, project, front.milestone.as_deref())?;
        }
        if front.item_type != item_type {
            check_type_change(pm, front)?;
        }
        Ok(true)
    })?;
    let (ids, foreign) = commit_staged(pm, &staged, &format!("set {}", changed.join(" ")), actor)?;
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
    let mut out = json!({"id": ids[0], "ids": ids, "set": changed,
              "worktree_open": worktree_open, "committed": true});
    attach_foreign(&mut out, &foreign);
    Ok(out)
}

/// CAD-449: a merged delivery marks its ticket done — one frontmatter
/// write and one tracker commit, `<ID>: set status=done — <why>`, whose
/// `Actor:` trailer is `actor` (the observer and the delivery). A ticket
/// already `done` or `dropped` is left alone: answers `Some(status)`
/// and writes nothing. `None` means it was marked done. The plan gate
/// every status write passes ([`crate::issue::plan::check_status_write`])
/// applies here too. With `expect`, the ticket is marked only while its
/// status is still that one — a status changed since (the operator
/// reopened or moved it) is left alone and answered like `done`.
///
/// It never waits for the tracker lock: a busy tracker is an error the
/// caller retries (the daemon holds its own lock here). A failed commit
/// leaves nothing behind — `issue.md` is restored, and the commit
/// itself unstages the path, so no later write can commit
/// `status: done` for it.
pub fn mark_done_on_merge(
    pm: &Pm,
    id: &str,
    why: &str,
    actor: &str,
    expect: Option<&str>,
) -> Result<Option<String>> {
    let Some(_lock) = pm.try_lock()? else {
        return Err(Error::rejected(
            "the tracker is locked by another writer (.write.lock)",
        ));
    };
    let (_project, dir) = issue_dir(pm, id)?;
    let (mut front, body) = load_front(&dir)?;
    if matches!(front.status.as_str(), "done" | "dropped")
        || expect.is_some_and(|e| e != front.status)
    {
        return Ok(Some(front.status));
    }
    crate::issue::plan::check_status_write(&pm.dir, &front, "done")?;
    front.status = "done".to_string();
    let file = dir.join("issue.md");
    let original = std::fs::read(&file)?;
    let written = save_front(&dir, &front, &body).and_then(|_| {
        commit(
            pm,
            std::slice::from_ref(&file),
            &format!("{id}: set status=done — {why}"),
            &[id],
            actor,
        )
    });
    if let Err(e) = written {
        let _ = std::fs::write(&file, &original);
        return Err(e);
    }
    Ok(None)
}

/// CAD-405: an issue with a plan or children is an epic — an explicit
/// other type would silently drop it from every epic view.
fn check_type_change(pm: &Pm, front: &Front) -> Result<()> {
    let Some(t) = front.item_type.as_deref().filter(|t| *t != "epic") else {
        return Ok(());
    };
    let id = front.id.as_str();
    let why = if front.plan.is_some() {
        Some("it carries a plan")
    } else if board::load_all(&pm.dir, None)?
        .iter()
        .any(|i| i.front.parent.as_deref() == Some(id))
    {
        Some("it has children")
    } else {
        None
    };
    match why {
        Some(why) => Err(Error::rejected(format!(
            "{id} cannot be type '{t}' — {why}, so it is an epic"
        ))),
        None => Ok(()),
    }
}

/// CAD-405: a milestone the project's `PROJECT.md` declares — any
/// well-formed id when it declares none.
fn check_milestone(pm: &Pm, project: &project::Project, milestone: Option<&str>) -> Result<()> {
    let Some(m) = milestone else { return Ok(()) };
    let cfg = crate::issue::work::load_config(&pm.dir, &project.key)?;
    if !cfg.milestones.is_empty() && !cfg.milestones.iter().any(|d| d.id == m) {
        return Err(Error::rejected(format!(
            "Unknown milestone '{m}' — {} declares: {}",
            project.key,
            cfg.milestones
                .iter()
                .map(|d| d.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }
    Ok(())
}

/// CAD-405 `issue epic stage` — move an epic to another stage: one
/// frontmatter write (`stage`, `stage_at`) and one tracker commit whose
/// `Actor:` trailer is the mover. The move is checked under the PM lock
/// ([`crate::issue::work::check_move`]: one stage forward, any stage
/// back), then `authorize` decides who may make it and returns the
/// actor — the daemon demands the proven operator for a forward move
/// into one of the project's `operator_stages`. A plan epic's first
/// forward move is `cadence plan approve`, an approved plan never moves
/// back before build (the plan owns shape), and a rejected plan never
/// moves. The gate keys apply only as `approvals` allow
/// ([`crate::issue::work::effective`]); a malformed `PROJECT.md`
/// refuses: a gate never guesses.
pub fn move_stage(
    pm: &Pm,
    epic: &str,
    to: &str,
    note: Option<&str>,
    approvals: &crate::issue::work::Approvals,
    authorize: impl FnOnce(&crate::issue::work::Move) -> Result<String>,
) -> Result<Value> {
    use crate::issue::work;
    let note = match note.map(str::trim).filter(|n| !n.is_empty()) {
        Some(n) => {
            crate::secret::guard(&format!("{epic}: stage note"), n)?;
            Some(crate::issue::claim::clean_text("--note", n)?)
        }
        None => None,
    };
    let (project, dir) = issue_dir(pm, epic)?;
    let _lock = pm.lock()?;
    let (mut front, body) = load_front(&dir)?;
    let (cfg, unapproved) = work::effective(
        &project.key,
        work::load_config(&pm.dir, &project.key)?,
        approvals.get(&project.key).map(String::as_str),
    );
    let views = board::views(&pm.config.notes_dir(), board::load_all(&pm.dir, None)?);
    let Some(view) = views.iter().find(|v| v.issue.front.id == epic) else {
        return Err(Error::internal(format!("{epic} missing from reload")));
    };
    let kind = model::item_type(&front, view.container);
    if kind != "epic" {
        return Err(Error::rejected(format!(
            "{epic} is a {kind} — only epics have stages \
             (`cadence issue set {epic} type=epic` makes it one)"
        )));
    }
    work::plan_allows_moves(&front)?;
    let cur = work::stage_of(&front, &cfg, view.status == "done");
    let mv = work::check_move(&cfg, &cur, to, work::floor(&front, &cfg))?;
    let by = authorize(&mv)?;
    let at = time::iso(time::now_epoch());
    front.stage = Some(mv.to.clone());
    front.stage_at = Some(at.clone());
    let file = dir.join("issue.md");
    let original = std::fs::read_to_string(&file)?;
    let mut subject = format!("{epic}: stage {} → {}", mv.from, mv.to);
    if let Some(n) = &note {
        subject.push_str(&format!(" — {n}"));
    }
    let written = save_front(&dir, &front, &body).and_then(|_| {
        commit_who(
            pm,
            std::slice::from_ref(&file),
            &subject,
            &[epic],
            "",
            Some(&by),
        )
    });
    let foreign = match written {
        Ok(f) => f,
        Err(e) => {
            // Nothing half-done: the file goes back to what was committed.
            let _ = atomic_write(&file, &original);
            return Err(e);
        }
    };
    let mut out = json!({
        "epic": epic,
        "project": project.key,
        "from": mv.from,
        "to": mv.to,
        "forward": mv.forward,
        "needs_operator": mv.needs_operator,
        "exit_met": mv.forward.then_some(mv.exit),
        "note": note,
        "by": by,
        "at": at,
        "config_unapproved": unapproved,
        "committed": true,
    });
    attach_foreign(&mut out, &foreign);
    Ok(out)
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
    let (ids, foreign) = commit_staged(
        pm,
        &staged,
        &format!("tag {verb} {}", tags.join(" ")),
        actor,
    )?;
    let mut out =
        json!({"ids": ids, "tag": verb, "tags": tags, "issues": tagged, "committed": true});
    attach_foreign(&mut out, &foreign);
    Ok(out)
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
        crate::issue::plan::check_status_write(&pm.dir, &front, v)?;
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
    let file = dir.join("issue.md");
    let prev = file_preimage(&file);
    save_front(&dir, &front, &body)?;
    let foreign = match commit(
        pm,
        std::slice::from_ref(&file),
        &format!("{id}: set {}", changed.join(" ")),
        &[id],
        actor,
    ) {
        Ok(f) => f,
        Err(e) => {
            restore_preimage(&file, prev);
            return Err(e);
        }
    };
    let warnings = blocked_warnings(pm, id, state_dir)?;
    let mut out = json!({"id": id, "set": changed, "committed": true, "warnings": warnings});
    attach_foreign(&mut out, &foreign);
    Ok(out)
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
            if kind == "parent" {
                // CAD-360: a plan ticket keeps its epic; nothing joins
                // a plan by link.
                crate::issue::plan::check_parent_change(
                    &pm.dir,
                    &front,
                    (!unlink).then_some(target),
                )?;
            }
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
    let file = dir.join("issue.md");
    let prev = file_preimage(&file);
    save_front(&dir, &front, &body)?;
    let foreign = match commit(
        pm,
        std::slice::from_ref(&file),
        &format!("{id}: {verb} {kind} {target}"),
        &[id, target],
        actor,
    ) {
        Ok(f) => f,
        Err(e) => {
            restore_preimage(&file, prev);
            return Err(e);
        }
    };
    let warnings = blocked_warnings(pm, id, state_dir)?;
    let mut out = json!({"id": id, "link": kind, "target": target,
              "unlink": unlink, "committed": true, "warnings": warnings});
    attach_foreign(&mut out, &foreign);
    Ok(out)
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
    let file = dir.join("issue.md");
    let prev = file_preimage(&file);
    save_front(&dir, &front, &body)?;
    let foreign = match commit(
        pm,
        std::slice::from_ref(&file),
        &format!("{id}: ref {kind}"),
        &[id],
        actor,
    ) {
        Ok(f) => f,
        Err(e) => {
            restore_preimage(&file, prev);
            return Err(e);
        }
    };
    let mut out = json!({"id": id, "ref": {"kind": kind, "target": target},
              "committed": true});
    attach_foreign(&mut out, &foreign);
    Ok(out)
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
        let file = dir.join("issue.md");
        let prev = file_preimage(&file);
        save_front(&dir, &front, &body)?;
        if let Err(e) = commit(
            pm,
            std::slice::from_ref(&file),
            &format!("{id}: ref {kind} closed"),
            &[id],
            actor,
        ) {
            restore_preimage(&file, prev);
            return Err(e);
        }
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
    // A failed commit leaves no comment file behind — an orphan would
    // sit foreign and uncommitted under the next writer's eye.
    let foreign = match commit_who(
        pm,
        std::slice::from_ref(&path),
        &format!("{id}: comment by {author}"),
        &[id],
        actor,
        author_opt,
    ) {
        Ok(f) => f,
        Err(e) => {
            let _ = std::fs::remove_file(&path);
            return Err(e);
        }
    };
    let mut out = json!({"id": id, "comment": path.file_name().map(|n| n.to_string_lossy().to_string()),
              "author": author, "committed": true});
    if !secret_warnings.is_empty() {
        out["secret_warnings"] = crate::secret::warnings_json(&secret_warnings);
    }
    attach_foreign(&mut out, &foreign);
    Ok(out)
}

/// CAD-383: write `front` and one comment by `author` in a single tracker
/// commit (`subject` without the id). Call under the PM lock. A failed
/// commit restores `prev` and removes the comment, so a refusal leaves
/// the issue as it was.
#[allow(clippy::too_many_arguments)]
pub(crate) fn commit_front_with_comment(
    pm: &Pm,
    dir: &Path,
    prev: &Front,
    front: &Front,
    body: &str,
    author: &str,
    text: &str,
    subject: &str,
    actor: &str,
) -> Result<String> {
    let id = front.id.as_str();
    crate::issue::claim::check_alias(author, "Claimant")?;
    // CAD-109: a credential-shaped note or reason is refused here too.
    crate::secret::guard(&format!("{id}: comment"), text)?;
    let comments = dir.join("comments");
    if comments.symlink_metadata().is_ok_and(|m| m.is_symlink()) {
        return Err(Error::rejected(format!(
            "{id}: comments/ is a symlink — refusing to write outside the PM dir"
        )));
    }
    std::fs::create_dir_all(&comments)?;
    let epoch = time::now_epoch();
    let meta = model::CommentFront {
        author: author.to_string(),
        at: time::iso(epoch),
        kind: Some("claim".to_string()),
    };
    let rendered = parse::render(&meta, text)?;
    let path = create_exclusive(
        &comments,
        &format!("{}-{author}.md", time::basic(epoch)),
        rendered.as_bytes(),
    )?;
    let committed = save_front(dir, front, body).and_then(|_| {
        commit_who(
            pm,
            &[dir.join("issue.md"), path.clone()],
            &format!("{id}: {subject}"),
            &[id],
            actor,
            Some(author),
        )
    });
    if let Err(e) = committed {
        let _ = save_front(dir, prev, body);
        let _ = std::fs::remove_file(&path);
        return Err(e);
    }
    Ok(path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default())
}

/// Store one already-validated task report (CAD-341) as a create-only
/// file under `reports/`, named by UTC time + agent. Byte-identical
/// content already on the ticket is not written twice: the existing
/// file is returned with `duplicate: true` and nothing is committed.
pub fn add_report(
    pm: &Pm,
    id: &str,
    front: &crate::issue::task_report::Front,
    body: &str,
    actor: &str,
) -> Result<Value> {
    use crate::issue::task_report::DIR;
    let (_project, dir) = issue_dir(pm, id)?;
    let agent = front.agent.clone().unwrap_or_default();
    let kind = front.kind.map(|k| k.as_str()).unwrap_or_default();
    let text = parse::render(front, body)?;
    let _lock = pm.lock()?;
    let reports = dir.join(DIR);
    if reports.symlink_metadata().is_ok_and(|m| m.is_symlink()) {
        return Err(Error::rejected(format!(
            "{id}: {DIR}/ is a symlink — refusing to write outside the PM dir"
        )));
    }
    std::fs::create_dir_all(&reports)?;
    let out = |name: &str, duplicate: bool| {
        json!({"id": id, "report": name, "path": format!("{id}/{DIR}/{name}"),
               "kind": kind, "agent": agent, "committed": !duplicate,
               "duplicate": duplicate})
    };
    for name in crate::issue::task_report::names(&dir) {
        if std::fs::read(reports.join(&name)).is_ok_and(|b| b == text.as_bytes()) {
            return Ok(out(&name, true));
        }
    }
    let path = create_exclusive(
        &reports,
        &format!("{}-{agent}.md", time::basic(time::now_epoch())),
        text.as_bytes(),
    )?;
    // A failed commit must not leave the file behind: a retry would hit
    // the duplicate short-circuit and never commit it.
    let foreign = match commit_who(
        pm,
        std::slice::from_ref(&path),
        &format!("{id}: report {kind} by {agent}"),
        &[id],
        actor,
        Some(&agent),
    ) {
        Ok(f) => f,
        Err(e) => {
            let _ = std::fs::remove_file(&path);
            return Err(e);
        }
    };
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let mut out = out(&name, false);
    attach_foreign(&mut out, &foreign);
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
    let foreign = match commit(
        pm,
        std::slice::from_ref(&path),
        &format!("{id}: attach {name}"),
        &[id],
        actor,
    ) {
        Ok(f) => f,
        Err(e) => {
            let _ = std::fs::remove_file(&path);
            return Err(e);
        }
    };
    let mut out = json!({"id": id, "artifact": path.file_name().map(|n| n.to_string_lossy().to_string()),
              "size": bytes.len(), "committed": true});
    attach_foreign(&mut out, &foreign);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tracker with project `cadence` and one ticket, CAD-1.
    fn tracker() -> (tempfile::TempDir, Pm) {
        let dir = tempfile::tempdir().unwrap();
        let pm = Pm::init(&dir.path().join("pm")).unwrap();
        project_add(&pm, "cadence", "CAD", &[], &[], &[], None).unwrap();
        new_issue(
            &pm,
            dir.path(),
            Some("cadence"),
            "ticket",
            None,
            None,
            &[],
            None,
            None,
            &[],
            None,
            None,
            "t",
        )
        .unwrap();
        (dir, pm)
    }

    /// A pre-commit hook that refuses every commit; remove it to heal.
    fn failing_hook(pm: &Pm) -> PathBuf {
        let hooks = crate::issue::git(&pm.dir, &["rev-parse", "--git-path", "hooks"]).unwrap();
        let hooks = pm.dir.join(hooks);
        std::fs::create_dir_all(&hooks).unwrap();
        let hook = hooks.join("pre-commit");
        std::fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        hook
    }

    /// Nothing staged, nothing changed in the work tree.
    fn clean(pm: &Pm) -> bool {
        crate::issue::git(&pm.dir, &["diff", "--cached", "--quiet"]).is_ok()
            && crate::issue::git(&pm.dir, &["status", "--porcelain"]).is_ok_and(|s| s.is_empty())
    }

    /// CAD-449: a failed done commit restores `issue.md` and its index
    /// entry — the next writer's `git add -A` finds nothing of it.
    #[test]
    fn a_failed_done_write_leaves_nothing_behind() {
        let (_dir, pm) = tracker();
        let (_, dir) = issue_dir(&pm, "CAD-1").unwrap();
        let before = std::fs::read(dir.join("issue.md")).unwrap();
        assert!(clean(&pm));
        let hook = failing_hook(&pm);
        let e = mark_done_on_merge(
            &pm,
            "CAD-1",
            "o/r#1 merged at x",
            "operator (delivery o/r#1)",
            None,
        )
        .unwrap_err();
        assert!(e.to_string().contains("commit"), "{e}");
        assert_eq!(std::fs::read(dir.join("issue.md")).unwrap(), before);
        assert!(clean(&pm), "a failed done write left something staged");
        std::fs::remove_file(hook).unwrap();
        // A status changed since the merge (`expect` no longer holds) is
        // left alone and nothing is written.
        let commits = crate::issue::git(&pm.dir, &["rev-list", "--count", "HEAD"]).unwrap();
        assert_eq!(
            mark_done_on_merge(&pm, "CAD-1", "w", "operator", Some("doing")).unwrap(),
            Some("backlog".to_string())
        );
        assert_eq!(load_front(&dir).unwrap().0.status, "backlog");
        assert_eq!(
            crate::issue::git(&pm.dir, &["rev-list", "--count", "HEAD"]).unwrap(),
            commits
        );
        assert_eq!(
            mark_done_on_merge(
                &pm,
                "CAD-1",
                "o/r#1 merged at x",
                "operator (delivery o/r#1)",
                Some("backlog"),
            )
            .unwrap(),
            None
        );
        assert_eq!(load_front(&dir).unwrap().0.status, "done");
        // Done already: left alone.
        assert_eq!(
            mark_done_on_merge(&pm, "CAD-1", "again", "operator", None).unwrap(),
            Some("done".to_string())
        );
    }

    /// CAD-449: the done write never waits for a busy tracker.
    #[test]
    fn a_busy_tracker_refuses_the_done_write_at_once() {
        let (_dir, pm) = tracker();
        let held = pm.lock().unwrap();
        let t = std::time::Instant::now();
        let e = mark_done_on_merge(&pm, "CAD-1", "w", "operator", None).unwrap_err();
        assert!(e.to_string().contains("locked"), "{e}");
        assert!(t.elapsed() < std::time::Duration::from_secs(2));
        drop(held);
    }

    /// CAD-449: a comment whose commit fails removes its file and its
    /// staging, so no later commit carries it.
    #[test]
    fn a_failed_comment_leaves_nothing_behind() {
        let (_dir, pm) = tracker();
        let hook = failing_hook(&pm);
        assert!(add_comment(&pm, "CAD-1", "hello", Some("w1"), None, None, "w1").is_err());
        assert!(clean(&pm), "a failed comment left something behind");
        std::fs::remove_file(hook).unwrap();
    }

    /// CAD-614: the master cannot stamp owner, id, or a non-backlog
    /// status. Anyone else still can set owner and id; status other
    /// than backlog is refused for every caller.
    #[test]
    fn master_issue_new_refuses_owner_id_and_non_backlog() {
        assert!(master_issue_new_limits(true, None, None, None).is_ok());
        assert!(master_issue_new_limits(true, None, None, Some("backlog")).is_ok());
        let owner = master_issue_new_limits(true, Some("bob"), None, None).unwrap_err();
        assert!(owner.to_string().contains("--owner"), "{owner}");
        assert!(owner.to_string().contains("actor=master"), "{owner}");
        let id = master_issue_new_limits(true, None, Some("CAD-9"), None).unwrap_err();
        assert!(id.to_string().contains("--id"), "{id}");
        let status = master_issue_new_limits(true, None, None, Some("ready")).unwrap_err();
        assert!(status.to_string().contains("backlog"), "{status}");
        assert!(master_issue_new_limits(false, Some("bob"), Some("CAD-9"), None).is_ok());
        let other = master_issue_new_limits(false, None, None, Some("ready")).unwrap_err();
        assert!(other.to_string().contains("backlog"), "{other}");
    }

    /// CAD-614: a create attributed to master records that actor, and
    /// `--file` body text replaces the empty acceptance stub.
    #[test]
    fn master_create_records_actor_and_file_body() {
        let (dir, pm) = tracker();
        new_issue(
            &pm,
            dir.path(),
            Some("cadence"),
            "from the master",
            None,
            None,
            &[],
            None,
            None,
            &[],
            None,
            Some("the note\n"),
            "master",
        )
        .unwrap();
        let md = std::fs::read_to_string(dir.path().join("pm/cadence/CAD-2/issue.md")).unwrap();
        assert!(md.contains("the note"), "{md}");
        let log = crate::issue::git(
            &pm.dir,
            &["log", "-1", "--format=%B", "--", "cadence/CAD-2/issue.md"],
        )
        .unwrap();
        assert!(log.contains("Actor: master"), "{log}");
    }
}
