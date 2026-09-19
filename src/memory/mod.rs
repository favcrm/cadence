//! Project memory — reviewed, scoped facts shared across agents.
//!
//! One fact per file at `<pm>/<project>/memory/<slug>.md`: YAML
//! frontmatter (id/type/status/scope/source/confidence/created/
//! verified_at/supersedes/author) plus a body holding the fact (≤5
//! lines), a `**Why:**` line and a `**How to apply:**` line.
//!
//! Workers `propose`; only a group root, an inbox PM or a plain
//! operator terminal may `accept`/`reject`/`supersede` — the guard is
//! enforced here in the guarded write path, not in the CLI. Every
//! write is exactly one tracker commit with `Actor:` and `Memory:`
//! trailers (never `Issue:` — memory writes are not issue writes).
//!
//! Only `accepted` memories match a dispatch: `scope.project` or any
//! of components/path-globs/tags/providers intersecting the dispatch
//! context. `cadence dispatch` renders the top matches into a lessons
//! file and names it in the kickoff; the briefing lists accepted
//! project-wide `rule`s.

pub mod cli;

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::client;
use crate::error::{Error, Result};
use crate::issue::{board, git, history, parse, project, time, write, Pm};
use crate::proc;

pub const TYPES: &[&str] = &["rule", "gotcha", "decision", "recipe"];
pub const STATUSES: &[&str] = &["proposed", "accepted", "rejected", "superseded"];
pub const CONFIDENCES: &[&str] = &["low", "medium", "high"];

/// Dispatch injection caps — the lessons file stays a quick scan.
pub const LESSON_MAX_ENTRIES: usize = 12;
pub const LESSON_MAX_BYTES: usize = 4 * 1024;

/// `scope:` — every axis is optional; an empty scope matches only via
/// `project: true` (which is itself one of the axes).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Scope {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub components: Vec<String>,
    /// Globs relative to a project repo root (`src/adapter/**`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub providers: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Project-wide: matches every dispatch in the project.
    #[serde(default, skip_serializing_if = "is_false")]
    pub project: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// Memory file frontmatter. `id` is the slug = filename stem.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Front {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub status: String,
    #[serde(default)]
    pub scope: Scope,
    /// Issue id, note path or commit this fact was learned from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    pub confidence: String,
    pub created: String,
    /// Last time a human/PM re-checked the fact against reality.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified_at: Option<String>,
    /// Slug this memory replaces (set on the new file by `supersede`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes: Option<String>,
    /// CADENCE_ALIAS of the proposer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
}

/// A loaded memory file.
#[derive(Clone, Debug)]
pub struct Memory {
    pub project: String,
    pub front: Front,
    pub body: String,
    pub path: PathBuf,
}

/// Slug grammar: `[a-z0-9][a-z0-9-]{0,47}`, no trailing `-`.
pub fn valid_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug.len() <= 48
        && slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !slug.starts_with('-')
        && !slug.ends_with('-')
        && !slug.contains("--")
}

pub fn check_slug(slug: &str) -> Result<String> {
    if valid_slug(slug) {
        Ok(slug.to_string())
    } else {
        Err(Error::rejected(format!(
            "Invalid memory slug '{slug}' — 1-48 lowercase letters, digits or single hyphens"
        )))
    }
}

/// `<pm>/<key>/memory/` — created lazily by `propose`.
pub fn memory_dir(pm: &Pm, key: &str) -> PathBuf {
    pm.dir.join(key).join("memory")
}

/// `---\n<yaml>\n---\n<body>` — same fence grammar as issue files.
pub fn parse_memory(text: &str) -> Result<(Front, String)> {
    let (yaml, body) =
        parse::split_front(text).map_err(|e| Error::rejected(format!("memory {e}")))?;
    let front: Front = serde_yaml::from_str(yaml)
        .map_err(|e| Error::rejected(format!("memory frontmatter is not valid YAML: {e}")))?;
    Ok((front, body.to_string()))
}

/// The body contract: a fact block (≤5 non-empty lines) followed by
/// `**Why:**` and `**How to apply:**` markers. Returns
/// `(fact_lines, why, how)`; sections may span multiple lines.
fn body_parts(body: &str) -> (Vec<String>, String, String) {
    let mut fact = Vec::new();
    let mut why = Vec::new();
    let mut how = Vec::new();
    let mut section = 0; // 0 = fact, 1 = why, 2 = how
    for line in body.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("**Why:**") {
            section = 1;
            if !rest.trim().is_empty() {
                why.push(rest.trim().to_string());
            }
            continue;
        }
        if let Some(rest) = t.strip_prefix("**How to apply:**") {
            section = 2;
            if !rest.trim().is_empty() {
                how.push(rest.trim().to_string());
            }
            continue;
        }
        match section {
            0 => {
                if !t.is_empty() || !fact.is_empty() {
                    fact.push(line.to_string());
                }
            }
            1 => why.push(line.to_string()),
            _ => how.push(line.to_string()),
        }
    }
    let trim = |v: &mut Vec<String>| {
        while v.first().is_some_and(|l| l.trim().is_empty()) {
            v.remove(0);
        }
        while v.last().is_some_and(|l| l.trim().is_empty()) {
            v.pop();
        }
    };
    trim(&mut fact);
    trim(&mut why);
    trim(&mut how);
    (fact, why.join("\n"), how.join("\n"))
}

/// The one-line fact used in lessons files and list views.
pub fn fact_line(body: &str) -> String {
    body_parts(body).0.first().cloned().unwrap_or_default()
}

/// The first `**How to apply:**` line — briefings and lessons carry it.
pub fn apply_line(body: &str) -> String {
    let (_, _, how) = body_parts(body);
    how.lines().next().unwrap_or_default().trim().to_string()
}

/// Body-contract errors — shared by `propose`/`accept --edit` and lint.
/// One fact stays a fact — bounded in lines AND bytes so a stuffed
/// paragraph can't slip past the line cap.
pub const FACT_MAX_BYTES: usize = 512;

fn lint_body(slug: &str, body: &str, err: &mut dyn FnMut(String)) {
    let (fact, why, how) = body_parts(body);
    let fact_lines = fact.iter().filter(|l| !l.trim().is_empty()).count();
    if fact_lines == 0 {
        err(format!("{slug}: missing fact (the lines before **Why:**)"));
    } else if fact_lines > 5 {
        err(format!("{slug}: fact is {fact_lines} lines — the cap is 5"));
    }
    let fact_bytes: usize = fact.iter().map(|l| l.len()).sum();
    if fact_bytes > FACT_MAX_BYTES {
        err(format!(
            "{slug}: fact is {fact_bytes} bytes — the cap is {FACT_MAX_BYTES}"
        ));
    }
    if why.trim().is_empty() {
        err(format!("{slug}: missing '**Why:**' section"));
    }
    if how.trim().is_empty() {
        err(format!("{slug}: missing '**How to apply:**' section"));
    }
}

/// Read one memory file; skips (returns None) anything not a real file.
fn load_file(path: &Path, key: &str) -> Result<Option<Memory>> {
    if !board::is_real_file(path) {
        return Ok(None);
    }
    let text = std::fs::read_to_string(path)?;
    let (front, body) = parse_memory(&text)?;
    Ok(Some(Memory {
        project: key.to_string(),
        front,
        body,
        path: path.to_path_buf(),
    }))
}

/// Every memory in one project, sorted by slug.
pub fn load_project(pm_dir: &Path, key: &str) -> Result<Vec<Memory>> {
    let dir = pm_dir.join(key).join("memory");
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.ends_with(".md") || name.starts_with('.') {
                continue;
            }
            if let Some(m) = load_file(&entry.path(), key)? {
                out.push(m);
            }
        }
    }
    out.sort_by(|a, b| a.front.id.cmp(&b.front.id));
    Ok(out)
}

/// Every memory across every project.
pub fn load_all(pm_dir: &Path) -> Result<Vec<Memory>> {
    let mut out = Vec::new();
    for p in project::list(pm_dir)? {
        out.extend(load_project(pm_dir, &p.key)?);
    }
    Ok(out)
}

/// `load_project` that keeps going past a broken file — returns the
/// good memories plus one `<project>/<file>: <error>` string per
/// failure, so callers can surface instead of swallowing them.
pub fn load_project_report(pm_dir: &Path, key: &str) -> (Vec<Memory>, Vec<String>) {
    let dir = pm_dir.join(key).join("memory");
    let (mut out, mut errors) = (Vec::new(), Vec::new());
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.ends_with(".md") || name.starts_with('.') {
                continue;
            }
            match load_file(&entry.path(), key) {
                Ok(Some(m)) => out.push(m),
                Ok(None) => {}
                Err(e) => errors.push(format!("{key}/{name}: {e}")),
            }
        }
    }
    out.sort_by(|a, b| a.front.id.cmp(&b.front.id));
    (out, errors)
}

/// `load_all` in the same error-collecting shape.
pub fn load_all_report(pm_dir: &Path) -> (Vec<Memory>, Vec<String>) {
    let (mut out, mut errors) = (Vec::new(), Vec::new());
    match project::list(pm_dir) {
        Ok(projects) => {
            for p in projects {
                let (mems, errs) = load_project_report(pm_dir, &p.key);
                out.extend(mems);
                errors.extend(errs);
            }
        }
        Err(e) => errors.push(e.to_string()),
    }
    (out, errors)
}

/// One-line summary of load errors for stderr: `N memory file(s)
/// failed to load; first: <msg>` — None when clean.
pub fn load_errors_line(errors: &[String]) -> Option<String> {
    if errors.is_empty() {
        return None;
    }
    Some(format!(
        "memory: {} file(s) failed to load; first: {}",
        errors.len(),
        errors[0]
    ))
}

/// Resolve `<slug>` to `(project, memory)`. `--project` pins the
/// project; without it every project's memory dir is scanned and an
/// ambiguous or missing slug is an error.
pub fn find(pm: &Pm, flag: Option<&str>, slug: &str) -> Result<(project::Project, Memory)> {
    check_slug(slug)?;
    let projects = project::list(&pm.dir)?;
    let candidates: Vec<&project::Project> = match flag {
        Some(key) => vec![projects
            .iter()
            .find(|p| p.key == key)
            .ok_or_else(|| project::unknown_project(key, &pm.dir))?],
        None => projects.iter().collect(),
    };
    let mut hits = Vec::new();
    for p in candidates {
        if let Some(m) = load_file(&memory_dir(pm, &p.key).join(format!("{slug}.md")), &p.key)? {
            hits.push((p.clone(), m));
        }
    }
    match hits.len() {
        0 => Err(Error::rejected(format!(
            "Unknown memory '{slug}' — `cadence memory ls` lists what exists"
        ))),
        1 => Ok(hits.pop().unwrap()),
        _ => Err(Error::rejected(format!(
            "'{slug}' exists in several projects — pass --project"
        ))),
    }
}

/// Curator gate for accept/reject/supersede: outside a cadence pane
/// any operator may curate; inside one, only a group root (no
/// `upstream`) may. A worker pane fails closed — the daemon answering
/// is what proves PM status, so an unreachable daemon also refuses.
fn require_curator(state_dir: &Path) -> Result<()> {
    let Ok(alias) = std::env::var("CADENCE_ALIAS") else {
        return Ok(());
    };
    if alias.is_empty() {
        return Ok(());
    }
    let show = client::rpc(state_dir, "agent_show", json!({"alias": alias})).map_err(|_| {
        Error::rejected(format!(
            "'{alias}' is in a cadence pane but the daemon is unreachable — \
             cannot verify PM status for memory curation"
        ))
    })?;
    let agent = &show["agent"];
    if agent["params"]["upstream"].is_null() {
        Ok(())
    } else {
        Err(Error::rejected(format!(
            "'{alias}' is a cadence worker — accept/reject/supersede is the PM's \
             job; `cadence memory propose` records the lesson instead"
        )))
    }
}

/// One tracker commit for a memory write: subject plus `Memory:` and
/// `Actor:` trailers — deliberately no `Issue:` trailer, memory writes
/// are not issue writes. Returns whether a commit object was actually
/// created — `Pm::commit` no-ops on an empty staged diff (e.g. a
/// verify that re-stamps the same second), which callers report
/// honestly as `committed: false`.
fn commit_mem(pm: &Pm, slug: &str, subject: &str, actor: &str) -> Result<bool> {
    let who = write::actor_who(actor, None);
    let before = git(&pm.dir, &["rev-parse", "HEAD"]).unwrap_or_default();
    pm.commit(&format!("{subject}\n\nMemory: {slug}\nActor: {who}\n"))?;
    let after = git(&pm.dir, &["rev-parse", "HEAD"]).unwrap_or_default();
    Ok(!before.is_empty() && before != after)
}

fn save_mem(mem: &Memory) -> Result<()> {
    let tmp = mem.path.with_extension("md.tmp");
    std::fs::write(&tmp, parse::render(&mem.front, &mem.body)?)?;
    std::fs::rename(&tmp, &mem.path)?;
    Ok(())
}

/// Frontmatter + body validation used by every write and by lint.
/// `components` is the project's declared list (empty = anything goes).
fn check_front(mem: &Memory, components: &[String]) -> Result<()> {
    let f = &mem.front;
    if f.id != mem.path.file_stem().unwrap_or_default().to_string_lossy() {
        return Err(Error::rejected(format!(
            "memory id '{}' must match its filename",
            f.id
        )));
    }
    check_slug(&f.id)?;
    if !TYPES.contains(&f.kind.as_str()) {
        return Err(Error::rejected(format!(
            "Unknown memory type '{}' — one of {}",
            f.kind,
            TYPES.join(" ")
        )));
    }
    if !STATUSES.contains(&f.status.as_str()) {
        return Err(Error::rejected(format!(
            "Unknown memory status '{}' — one of {}",
            f.status,
            STATUSES.join(" ")
        )));
    }
    if !CONFIDENCES.contains(&f.confidence.as_str()) {
        return Err(Error::rejected(format!(
            "Unknown confidence '{}' — one of {}",
            f.confidence,
            CONFIDENCES.join(" ")
        )));
    }
    for c in &f.scope.components {
        if !components.is_empty() && !components.iter().any(|d| d == c) {
            return Err(Error::rejected(format!(
                "Unknown component '{c}' — {} declares: {}",
                mem.project,
                components.join(", ")
            )));
        }
    }
    // `**` recursion in glob_match is exponential on adversarial
    // patterns (`**a**a**a**`) — bound the shape a scope may carry.
    for pat in &f.scope.paths {
        if pat.len() > 200 || pat.matches("**").count() > 2 {
            return Err(Error::rejected(format!(
                "path scope '{pat}' is too complex — ≤200 chars, ≤2 `**` segments"
            )));
        }
    }
    let mut errs = Vec::new();
    lint_body(&f.id, &mem.body, &mut |e| errs.push(e));
    if let Some(first) = errs.into_iter().next() {
        return Err(Error::rejected(first));
    }
    Ok(())
}

/// `memory propose` — workers and operators alike. `--from` accepts a
/// full memory file (frontmatter kept verbatim; status is still reset
/// to `proposed`) or a bare body; `-m` is a bare body inline.
#[allow(clippy::too_many_arguments)]
pub fn propose(
    pm: &Pm,
    key: &str,
    kind: &str,
    scope: &Scope,
    source: Option<&str>,
    confidence: Option<&str>,
    from: Option<&Path>,
    text: Option<&str>,
    slug: Option<&str>,
    actor: &str,
) -> Result<Value> {
    let projects = project::list(&pm.dir)?;
    let proj = projects
        .iter()
        .find(|p| p.key == key)
        .ok_or_else(|| project::unknown_project(key, &pm.dir))?;
    let (mut front, body) = match (from, text) {
        (Some(file), None) => {
            let raw = std::fs::read_to_string(file)
                .map_err(|_| Error::rejected(format!("Cannot read {}", file.display())))?;
            match parse_memory(&raw) {
                Ok((f, b)) => (Some(f), b),
                Err(_) => (None, raw),
            }
        }
        (None, Some(t)) => (None, t.to_string()),
        (None, None) => {
            return Err(Error::rejected(
                "propose needs content — `-m \"<body>\"` or `--from <file>`",
            ))
        }
        (Some(_), Some(_)) => return Err(Error::rejected("propose takes --from or -m, not both")),
    };
    let slug = match slug {
        Some(s) => check_slug(s)?,
        None => {
            let base = fact_line(&body);
            // slugify can leave dash runs — collapse them before the
            // grammar check rather than erroring on a derived slug.
            let s: String = crate::issue::start::slugify(&base)
                .split('-')
                .filter(|p| !p.is_empty())
                .collect::<Vec<_>>()
                .join("-");
            check_slug(if s.is_empty() || s == "work" {
                "lesson"
            } else {
                &s
            })?
        }
    };
    let now = time::iso(time::now_epoch());
    let mut front = front.take().unwrap_or_else(|| Front {
        id: slug.clone(),
        kind: kind.to_string(),
        status: "proposed".to_string(),
        scope: scope.clone(),
        source: source.map(str::to_string),
        confidence: confidence.unwrap_or("medium").to_string(),
        created: now.clone(),
        verified_at: None,
        supersedes: None,
        author: None,
    });
    front.id = slug.clone();
    front.status = "proposed".to_string();
    front.author = Some(write::actor_who(actor, None));
    let path = memory_dir(pm, key).join(format!("{slug}.md"));
    if path.exists() {
        return Err(Error::rejected(format!(
            "Memory '{slug}' already exists — `memory show {slug}` reads it"
        )));
    }
    let mem = Memory {
        project: key.to_string(),
        front,
        body,
        path,
    };
    check_front(&mem, &proj.components)?;
    let _lock = pm.lock()?;
    std::fs::create_dir_all(memory_dir(pm, key))?;
    save_mem(&mem)?;
    let committed = commit_mem(pm, &slug, &format!("{key}/memory/{slug}: proposed"), actor)?;
    Ok(json!({"project": key, "slug": slug, "status": "proposed",
              "path": mem.path, "committed": committed}))
}

/// Shared accept/reject: curator-gated status transition.
fn review(
    pm: &Pm,
    flag: Option<&str>,
    slug: &str,
    status: &str,
    edit: Option<&str>,
    actor: &str,
    state_dir: &Path,
) -> Result<Value> {
    require_curator(state_dir)?;
    let (proj, mut mem) = find(pm, flag, slug)?;
    if let Some(body) = edit {
        mem.body = body.to_string();
    }
    mem.front.status = status.to_string();
    if status == "accepted" {
        mem.front.verified_at = Some(time::iso(time::now_epoch()));
    }
    check_front(&mem, &proj.components)?;
    let _lock = pm.lock()?;
    save_mem(&mem)?;
    let committed = commit_mem(
        pm,
        slug,
        &format!("{}/memory/{slug}: {status}", proj.key),
        actor,
    )?;
    Ok(json!({"project": proj.key, "slug": slug, "status": status,
              "committed": committed}))
}

/// `memory accept <slug>` — PM/operator only; stamps verified_at.
pub fn accept(
    pm: &Pm,
    flag: Option<&str>,
    slug: &str,
    edit: Option<&str>,
    actor: &str,
    state_dir: &Path,
) -> Result<Value> {
    review(pm, flag, slug, "accepted", edit, actor, state_dir)
}

/// `memory reject <slug>`.
pub fn reject(
    pm: &Pm,
    flag: Option<&str>,
    slug: &str,
    actor: &str,
    state_dir: &Path,
) -> Result<Value> {
    review(pm, flag, slug, "rejected", None, actor, state_dir)
}

/// `memory supersede <old> <new>` — one commit: old → `superseded`,
/// new gains `supersedes: <old>`; a proposed `new` is accepted by the
/// same act of curation.
pub fn supersede(
    pm: &Pm,
    flag: Option<&str>,
    old: &str,
    new: &str,
    actor: &str,
    state_dir: &Path,
) -> Result<Value> {
    require_curator(state_dir)?;
    let (proj, mut old_mem) = find(pm, flag, old)?;
    let (_, mut new_mem) = find(pm, flag.or(Some(&proj.key)), new)?;
    if old == new {
        return Err(Error::rejected("a memory cannot supersede itself"));
    }
    old_mem.front.status = "superseded".to_string();
    new_mem.front.supersedes = Some(old.to_string());
    if new_mem.front.status == "proposed" {
        new_mem.front.status = "accepted".to_string();
        new_mem.front.verified_at = Some(time::iso(time::now_epoch()));
    }
    check_front(&old_mem, &proj.components)?;
    check_front(&new_mem, &proj.components)?;
    let _lock = pm.lock()?;
    save_mem(&old_mem)?;
    save_mem(&new_mem)?;
    let committed = commit_mem(
        pm,
        new,
        &format!("{}/memory/{new}: supersedes {old}", proj.key),
        actor,
    )?;
    Ok(json!({"project": proj.key, "old": old, "new": new,
              "old_status": "superseded", "new_status": new_mem.front.status,
              "committed": committed}))
}

/// `memory verify <slug>` — re-stamp verified_at. Curator-gated like
/// accept/reject: verified_at is documented as the PM's re-check of a
/// fact, and it feeds ranking and staleness — a worker verify would
/// falsify the attestation and silently un-stale the memory. Workers
/// propose a correction instead.
/// A same-second re-verify changes nothing and commits nothing.
pub fn verify(
    pm: &Pm,
    flag: Option<&str>,
    slug: &str,
    actor: &str,
    state_dir: &Path,
) -> Result<Value> {
    require_curator(state_dir)?;
    let (proj, mut mem) = find(pm, flag, slug)?;
    if mem.front.status != "accepted" {
        return Err(Error::rejected(format!(
            "'{slug}' is {} — only accepted memories are verified",
            mem.front.status
        )));
    }
    let now = time::iso(time::now_epoch());
    if mem.front.verified_at.as_deref() == Some(now.as_str()) {
        return Ok(json!({"project": proj.key, "slug": slug,
                  "verified_at": mem.front.verified_at, "committed": false}));
    }
    mem.front.verified_at = Some(now);
    let _lock = pm.lock()?;
    save_mem(&mem)?;
    let committed = commit_mem(
        pm,
        slug,
        &format!("{}/memory/{slug}: verified", proj.key),
        actor,
    )?;
    Ok(json!({"project": proj.key, "slug": slug,
              "verified_at": mem.front.verified_at, "committed": committed}))
}

// ── Matching ─────────────────────────────────────────────────────

/// What a dispatch/match call knows about the target.
#[derive(Clone, Debug, Default)]
pub struct MatchCtx {
    pub components: Vec<String>,
    /// Concrete repo-relative paths the issue is likely to touch.
    pub paths: Vec<String>,
    pub providers: Vec<String>,
    pub tags: Vec<String>,
}

/// `*` within a path segment, `**` across segments, `?` one char.
/// Unanchored suffixes (`src/**` alone) still require the leading
/// segments to match — globs are anchored at both ends.
pub fn glob_match(pattern: &str, path: &str) -> bool {
    fn inner(p: &[u8], s: &[u8]) -> bool {
        if p.is_empty() {
            return s.is_empty();
        }
        if p.starts_with(b"**") {
            let rest = &p[2..];
            let rest = rest.strip_prefix(b"/").unwrap_or(rest);
            // `**` spans slashes: try every split point of the string.
            return (0..=s.len()).any(|i| inner(rest, &s[i..]));
        }
        match p[0] {
            b'*' => {
                for i in 0..=s.len() {
                    if inner(&p[1..], &s[i..]) {
                        return true;
                    }
                    if i == s.len() || s[i] == b'/' {
                        return false;
                    }
                }
                false
            }
            b'?' => !s.is_empty() && s[0] != b'/' && inner(&p[1..], &s[1..]),
            c => !s.is_empty() && s[0] == c && inner(&p[1..], &s[1..]),
        }
    }
    inner(pattern.as_bytes(), path.as_bytes())
}

/// Union semantics per the kickoff: a memory applies when ANY scope
/// axis matches the context.
fn applies(scope: &Scope, ctx: &MatchCtx) -> bool {
    scope.project
        || scope.components.iter().any(|c| ctx.components.contains(c))
        || scope
            .paths
            .iter()
            .any(|g| ctx.paths.iter().any(|p| glob_match(g, p)))
        || scope.tags.iter().any(|t| ctx.tags.contains(t))
        || scope.providers.iter().any(|p| ctx.providers.contains(p))
}

fn type_rank(kind: &str) -> u8 {
    match kind {
        "rule" => 0,
        "gotcha" => 1,
        "recipe" => 2,
        _ => 3,
    }
}

fn confidence_rank(confidence: &str) -> u8 {
    match confidence {
        "high" => 0,
        "medium" => 1,
        _ => 2,
    }
}

/// Accepted memories that apply to `ctx`, ranked: type
/// (rule>gotcha>recipe>decision), confidence (high first), newest
/// verified_at first.
pub fn match_memories(memories: &[Memory], ctx: &MatchCtx) -> Vec<Memory> {
    let mut hits: Vec<Memory> = memories
        .iter()
        .filter(|m| m.front.status == "accepted" && applies(&m.front.scope, ctx))
        .cloned()
        .collect();
    hits.sort_by(|a, b| {
        (
            type_rank(&a.front.kind),
            confidence_rank(&a.front.confidence),
            b.front.verified_at.clone().unwrap_or_default(),
            &a.front.id,
        )
            .cmp(&(
                type_rank(&b.front.kind),
                confidence_rank(&b.front.confidence),
                a.front.verified_at.clone().unwrap_or_default(),
                &b.front.id,
            ))
    });
    hits
}

/// Paths the issue's recorded code commits touched — the path-scope
/// input for issue matching. Absent commits/repos simply yield none.
fn issue_paths(pm_dir: &Path, issue: &board::Issue) -> Vec<String> {
    let mut paths = Vec::new();
    let (commits, _) = history::code_commits(pm_dir, issue);
    for c in commits {
        let (Some(repo), Some(sha)) = (c["repo"].as_str(), c["sha"].as_str()) else {
            continue;
        };
        let dir = project::expand_home(repo);
        let out = git_bounded(
            &dir,
            &["show", "--format=", "--name-only", sha],
            Duration::from_secs(5),
        )
        .unwrap_or_default();
        paths.extend(out.lines().map(str::to_string));
    }
    // Explicit commit refs too — a ref path is a sha in a project repo.
    for r in issue.front.refs.iter().filter(|r| r.kind == "commit") {
        let Some(sha) = r.path.as_deref() else {
            continue;
        };
        let Ok(projects) = project::list(pm_dir) else {
            break;
        };
        for p in projects.iter().filter(|p| p.key == issue.project) {
            for repo in &p.repos {
                let Some(path) = &repo.path else { continue };
                let dir = project::expand_home(path);
                if git_bounded(
                    &dir,
                    &["cat-file", "-e", &format!("{sha}^{{commit}}")],
                    Duration::from_secs(5),
                )
                .is_err()
                {
                    continue;
                }
                if let Ok(out) = git_bounded(
                    &dir,
                    &["show", "--format=", "--name-only", sha],
                    Duration::from_secs(5),
                ) {
                    paths.extend(out.lines().map(str::to_string));
                }
            }
        }
    }
    paths.sort();
    paths.dedup();
    paths
}

/// `git` bounded through `proc::run_bounded` — returns trimmed stdout.
fn git_bounded(dir: &Path, args: &[&str], timeout: Duration) -> Result<String> {
    let out = proc::run_bounded(
        std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args),
        timeout,
    )
    .map_err(|e| Error::rejected(format!("git {} in {}: {e}", args.join(" "), dir.display())))?;
    if !out.status.success() {
        return Err(Error::rejected(format!(
            "git {} failed in {}: {}",
            args.join(" "),
            dir.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Match context for an issue: component, frontmatter tags,
/// recorded-commit paths, and the target worker's provider.
pub fn issue_ctx(pm: &Pm, issue: &board::Issue, provider: Option<&str>) -> Result<MatchCtx> {
    Ok(MatchCtx {
        components: issue.front.component.clone().into_iter().collect(),
        paths: issue_paths(&pm.dir, issue),
        providers: provider.map(|p| vec![p.to_string()]).unwrap_or_default(),
        tags: issue.front.tags.clone(),
    })
}

/// The dispatch/match surface: accepted memories applying to an issue.
pub fn match_for_issue(
    pm: &Pm,
    issue: &board::Issue,
    provider: Option<&str>,
) -> Result<Vec<Memory>> {
    let ctx = issue_ctx(pm, issue, provider)?;
    Ok(match_memories(
        &load_project(&pm.dir, &issue.project)?,
        &ctx,
    ))
}

// ── Lessons rendering (dispatch) ─────────────────────────────────

/// Render the lessons file for a dispatch: each entry is the slug, the
/// one-line fact and the how-to-apply. Capped at `LESSON_MAX_ENTRIES`
/// memories and `LESSON_MAX_BYTES` total — a truncated tail is noted.
/// Returns `(text, slugs)`; empty input yields an empty string.
pub fn render_lessons(matched: &[Memory]) -> (String, Vec<String>) {
    let mut out = String::from("# Lessons — matched project memories\n\n");
    let mut slugs = Vec::new();
    let mut omitted = 0usize;
    for m in matched.iter().take(LESSON_MAX_ENTRIES) {
        let (fact, _why, how) = body_parts(&m.body);
        let fact = fact.join(" ").trim().to_string();
        let how = how.lines().next().unwrap_or_default().trim().to_string();
        let entry = format!(
            "- `{}` ({}): {}\n  apply: {}\n",
            m.front.id, m.front.kind, fact, how
        );
        if out.len() + entry.len() > LESSON_MAX_BYTES {
            omitted += 1;
            continue;
        }
        out.push_str(&entry);
        slugs.push(m.front.id.clone());
    }
    let extra = matched.len().saturating_sub(LESSON_MAX_ENTRIES) + omitted;
    if extra > 0 {
        out.push_str(&format!(
            "\n({extra} more matched — `cadence memory ls` lists them)\n"
        ));
    }
    if slugs.is_empty() {
        return (String::new(), vec![]);
    }
    (out, slugs)
}

/// Accepted project-wide `rule`s — the briefing's memory section.
/// Broken files are skipped, not fatal; callers surface the errors.
pub fn project_rules(pm: &Pm, key: &str) -> (Vec<Memory>, Vec<String>) {
    let (mems, errors) = load_project_report(&pm.dir, key);
    let mut rules: Vec<Memory> = mems
        .into_iter()
        .filter(|m| m.front.status == "accepted" && m.front.scope.project && m.front.kind == "rule")
        .collect();
    rules.sort_by(|a, b| a.front.id.cmp(&b.front.id));
    (rules, errors)
}

// ── Staleness ────────────────────────────────────────────────────

/// `YYYY-MM-DD[THH:MM:SSZ]` → epoch seconds; needed to bound the
/// staleness window without chrono. Byte-sliced — a non-ASCII or
/// short timestamp is "unknown" (None), never a panic.
fn iso_epoch(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    let n = |i: usize, j: usize| -> Option<i64> {
        let slice = b.get(i..j)?;
        if slice.iter().all(|c| c.is_ascii_digit()) {
            std::str::from_utf8(slice).ok()?.parse().ok()
        } else {
            None
        }
    };
    let (y, m, d) = (n(0, 4)?, n(5, 7)?, n(8, 10)?);
    let (hh, mm, ss) = if b.len() >= 19 {
        (n(11, 13)?, n(14, 16)?, n(17, 19)?)
    } else {
        (0, 0, 0)
    };
    // days-from-civil (Howard Hinnant) → seconds.
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(days * 86400 + hh * 3600 + mm * 60 + ss)
}

/// Stale accepted memories: `verified_at` missing or older than the
/// `days` window, or path globs matching files changed in a project
/// repo after `verified_at` (the changed-file scan stays inside the
/// window). Informational — `verify` is the refresh. Broken files are
/// skipped and returned as the second tuple element.
pub fn stale(pm: &Pm, days: u64) -> (Vec<Value>, Vec<String>) {
    let mut out = Vec::new();
    let (mems, errors) = load_all_report(&pm.dir);
    let since_epoch = time::now_epoch() - (days as i64) * 86400;
    for m in mems {
        if m.front.status != "accepted" {
            continue;
        }
        let verified = m
            .front
            .verified_at
            .as_deref()
            .and_then(iso_epoch)
            .unwrap_or(0);
        if verified < since_epoch {
            out.push(json!({
                "project": m.project, "slug": m.front.id,
                "verified_at": m.front.verified_at,
                "reason": "not verified within the window",
                "changed": [],
            }));
            continue;
        }
        if m.front.scope.paths.is_empty() {
            continue;
        }
        // Changes count only after verified_at AND inside the window.
        let bound = verified.max(since_epoch);
        let since = time::iso(bound.max(0));
        let Some(proj) = project::list(&pm.dir)
            .unwrap_or_default()
            .into_iter()
            .find(|p| p.key == m.project)
        else {
            continue;
        };
        for repo in &proj.repos {
            let Some(path) = &repo.path else { continue };
            let dir = project::expand_home(path);
            if !dir.is_dir() {
                continue;
            }
            let mut args: Vec<String> = vec![
                "log".into(),
                format!("--since={since}"),
                "--format=".into(),
                "--name-only".into(),
                "--".into(),
            ];
            args.extend(m.front.scope.paths.iter().cloned());
            let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
            let Ok(log) = git_bounded(&dir, &arg_refs, Duration::from_secs(10)) else {
                continue;
            };
            let changed: Vec<&str> = log.lines().filter(|l| !l.is_empty()).collect();
            if !changed.is_empty() {
                out.push(json!({
                    "project": m.project, "slug": m.front.id,
                    "verified_at": m.front.verified_at,
                    "reason": "paths changed after verified_at",
                    "repo": path,
                    "changed": changed.iter().take(10).collect::<Vec<_>>(),
                }));
                break;
            }
        }
    }
    (out, errors)
}

// ── Lint ─────────────────────────────────────────────────────────

/// Validate every memory file in one project's `memory/` dir —
/// invoked from `issue lint` (which owns the PM-wide report) and by
/// `memory lint`. `err`/`warn` feed the caller's accumulators.
pub fn lint_dir(
    dir: &Path,
    proj: &project::Project,
    err: &mut dyn FnMut(String),
    warn: &mut dyn FnMut(String),
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut slugs = Vec::new();
    let mut mems = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.ends_with(".md") || name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        if path.symlink_metadata().is_ok_and(|m| m.is_symlink()) {
            err(format!(
                "{}/memory/{name}: symlink — the board never follows links",
                proj.key
            ));
            continue;
        }
        let slug = name.trim_end_matches(".md");
        if !valid_slug(slug) {
            err(format!("{}/memory/{name}: bad slug grammar", proj.key));
            continue;
        }
        match load_file(&path, &proj.key) {
            Ok(Some(m)) => {
                if m.front.id != slug {
                    err(format!(
                        "{}/memory/{name}: frontmatter id '{}' does not match filename",
                        proj.key, m.front.id
                    ));
                }
                slugs.push(slug.to_string());
                mems.push(m);
            }
            Ok(None) => {}
            // A file that cannot even be parsed is a warning, not a
            // commit-blocking error: every read path already skips and
            // reports it (load_errors/memory_errors/lessons_error), so
            // erroring here would let one stray file brick every
            // tracker commit — including `issue start` mid-dispatch.
            Err(e) => warn(format!("{}/memory/{name}: {e}", proj.key)),
        }
    }
    for m in &mems {
        for e in check_front(m, &proj.components).err().into_iter() {
            err(format!("{}/memory/{}.md: {e}", m.project, m.front.id));
        }
        let mut body_errs = Vec::new();
        lint_body(&m.front.id, &m.body, &mut |e| body_errs.push(e));
        for e in body_errs {
            err(format!("{}/memory/{}.md: {e}", m.project, m.front.id));
        }
        if let Some(target) = &m.front.supersedes {
            if !slugs.contains(target) {
                err(format!(
                    "{}/memory/{}.md: supersedes '{target}' which does not exist",
                    m.project, m.front.id
                ));
            }
        }
        if m.front.status == "accepted"
            && m.front.scope.paths.is_empty()
            && !m.front.scope.project
            && m.front.scope.components.is_empty()
            && m.front.scope.tags.is_empty()
            && m.front.scope.providers.is_empty()
        {
            warn(format!(
                "{}/memory/{}.md: accepted but has no scope — it matches nothing",
                m.project, m.front.id
            ));
        }
    }
}

/// Card/list payload for `ls` and the UI.
pub fn card_json(m: &Memory) -> Value {
    json!({
        "project": m.project,
        "slug": m.front.id,
        "type": m.front.kind,
        "status": m.front.status,
        "confidence": m.front.confidence,
        "scope": {
            "project": m.front.scope.project,
            "components": m.front.scope.components,
            "paths": m.front.scope.paths,
            "providers": m.front.scope.providers,
            "tags": m.front.scope.tags,
        },
        "source": m.front.source,
        "author": m.front.author,
        "created": m.front.created,
        "verified_at": m.front.verified_at,
        "supersedes": m.front.supersedes,
        "fact": fact_line(&m.body),
        "path": m.path,
    })
}

pub fn detail_json(m: &Memory) -> Value {
    let mut v = card_json(m);
    v["body"] = json!(m.body);
    v
}
