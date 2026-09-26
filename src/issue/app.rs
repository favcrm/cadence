//! Apps (CAD-547): an app is a folder that bundles the workflows for
//! one kind of work — `app.md` (frontmatter `app`, `title`, `version`,
//! `needs.connections: [<slot>…]`; the body is the guide agents read),
//! `workflows/*.md` (the same plan templates CAD-487 ships) and
//! optional flat `rubrics/` and `templates/` dirs. A1 carries nothing
//! else — `records`, `actions`, `ui`, `settings` stay gated for later
//! stages.
//!
//! `cadence app install <path|git-url> --project <key>` copies a
//! verified bundle to `<pm>/<project>/apps/<name>/` and writes the
//! install record beside it at `apps/<name>.yaml` — source (a git
//! install pins the commit SHA), when, by whom, and each slot's
//! binding. An installed app lands **unapproved**: nothing in it runs
//! until the operator's `cadence app approve` records its structural
//! digest, the same gate-and-digest model `workflow approve` uses.
//!
//! The digest covers what the operator reviewed: the app name, the
//! declared slots, each slot's effective binding (an `app set` rebind
//! is a structural change — it re-gates), the `app.md` guide body, each
//! workflow's CAD-487 gate digest (its wording stays wording-free,
//! exactly like a stored workflow), and a content hash of every
//! rubric/template file. A wording-only workflow edit keeps approval;
//! a slot, binding, guide, rubric or template change does not —
//! `plan propose --workflow <app>/<wf>` refuses `app_unapproved` until
//! the operator re-approves.
//!
//! Slots: `needs.connections` declares names; each binds to a
//! connection name (`cadence app set <app> <slot>=<connection>`),
//! defaulting to the built-in `local` (CAD-546's connector — the name
//! is the contract, not its implementation). A workflow step names the
//! slot in `uses:` — never a service directly — and install refuses a
//! `uses:` the app did not declare.
//!
//! Install is safe on hostile input: the bundle is never executed, no
//! symlinks are followed (any link refuses), only the known top-level
//! entries are read, every file is bounded UTF-8 text, and a git
//! source is cloned into a temp dir and pinned to its HEAD SHA before
//! any byte is looked at.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::issue::model;
use crate::issue::parse;
use crate::issue::{board, plan, project, workflow, write, Pm};

/// `<pm>/<project>/apps/` — beside PROJECT.md and `workflows/`.
pub const DIR: &str = "apps";

/// `app.md` — the one required file of an app folder.
pub const MANIFEST: &str = "app.md";

/// The built-in connection a slot binds when nothing else is recorded —
/// CAD-546's local connector. The name is the contract; whether the
/// connector is registered yet is a `doctor` finding, not an install
/// refusal.
pub const LOCAL_CONNECTION: &str = "local";

/// `app.md` frontmatter keys v0 knows. Anything else refuses — the same
/// fail-loud rule the workflow and plan parsers apply.
const MANIFEST_KEYS: &[&str] = &["app", "title", "version", "needs", "summary"];

/// Frontmatter keys the later stages reserve — refused with their own
/// message so a bundle carrying one names the stage, not "unknown key".
const GATED_KEYS: &[&str] = &["records", "actions", "ui", "settings", "actors"];

/// The only directories an app folder may carry at top level.
const TOP_DIRS: &[&str] = &["workflows", "rubrics", "templates"];

/// Largest single file in a bundle — workflows render to plans, so the
/// plan cap applies; the same bound keeps every other file small.
const MAX_FILE_BYTES: u64 = plan::MAX_PLAN_BYTES as u64;

/// Most files a bundle may carry.
const MAX_FILES: usize = 128;

/// Largest bundle, all files together — an A1 app is text.
const MAX_APP_BYTES: u64 = 2 * 1024 * 1024;

/// The `app.md` definition: the frontmatter fields plus the body, the
/// guide agents read. `needs.connections` declares the slots the app's
/// workflows may name in their `uses:` lines; `summary` is the optional
/// one-line purpose the board shows people (wording — the guide's bytes
/// are what the digest covers, so adding one never re-gates an app).
#[derive(Clone, Debug)]
pub struct Manifest {
    pub app: String,
    pub title: String,
    pub version: String,
    pub connections: Vec<String>,
    pub summary: Option<String>,
    pub guide: String,
}

/// `apps/<name>.yaml` — the install record beside the content folder:
/// where the bundle came from (a git install pins the commit SHA), when
/// and by whom, and each slot's binding. In `bindings`, `null` is an
/// explicit unbind and an absent slot is the `local` default.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub schema: u32,
    pub app: String,
    pub source: Source,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub bindings: BTreeMap<String, Option<String>>,
    pub installed_at: String,
    pub installed_by: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

/// Where an installed bundle came from. `git` records the exact commit
/// the clone pinned — a branch or tag name is never trusted later.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum Source {
    Path { path: String },
    Git { url: String, sha: String },
}

impl Record {
    /// The connection a slot runs against: the recorded binding, an
    /// explicit unbind (`None`), or the `local` default.
    pub fn binding(&self, slot: &str) -> Option<&str> {
        match self.bindings.get(slot) {
            Some(Some(conn)) => Some(conn),
            Some(None) => None,
            None => Some(LOCAL_CONNECTION),
        }
    }
}

/// An app or workflow name — tag-shaped, like the workflow names beside
/// it (`apps/<app>/workflows/<wf>.md` addresses a run as `<app>/<wf>`).
fn check_name(name: &str, what: &str) -> Result<()> {
    if !model::valid_tag(name) {
        return Err(Error::rejected(format!(
            "{what} '{name}' — 1-32 lowercase letters, digits or hyphens"
        )));
    }
    Ok(())
}

/// `<pm>/<project>/apps/` — `project` is a key, never a path fragment,
/// and a symlinked `apps/` is refused rather than followed.
fn dir_of(pm_dir: &Path, project: &str) -> Result<PathBuf> {
    model::check_key(project)?;
    let dir = pm_dir.join(project).join(DIR);
    if dir.symlink_metadata().is_ok_and(|m| m.is_symlink()) {
        return Err(Error::rejected(format!(
            "{project}/{DIR}/ is a symlink — the tracker never follows links"
        )));
    }
    Ok(dir)
}

/// `apps/<name>/` — the content folder; must exist as a real directory.
fn app_dir(pm_dir: &Path, project: &str, name: &str) -> Result<PathBuf> {
    check_name(name, "app name")?;
    let dir = dir_of(pm_dir, project)?.join(name);
    match dir.symlink_metadata() {
        Ok(m) if m.is_symlink() => Err(Error::rejected(format!(
            "{} is a symlink — the tracker never follows links",
            dir.display()
        ))),
        Ok(m) if m.is_dir() => Ok(dir),
        _ => Err(Error::rejected(format!(
            "no app '{name}' installed in {project} — `cadence app ls \
             --project {project}` lists them"
        ))),
    }
}

/// `apps/<name>.yaml` — the install record.
fn record_file(pm_dir: &Path, project: &str, name: &str) -> Result<PathBuf> {
    check_name(name, "app name")?;
    Ok(dir_of(pm_dir, project)?.join(format!("{name}.yaml")))
}

/// Read the install record: real file only, schema-checked, and the
/// record must name the folder it claims.
pub fn read_record(pm_dir: &Path, project: &str, name: &str) -> Result<Record> {
    let file = record_file(pm_dir, project, name)?;
    if file.symlink_metadata().is_ok_and(|m| m.is_symlink()) {
        return Err(Error::rejected(format!(
            "{} is a symlink — the tracker never follows links",
            file.display()
        )));
    }
    let text = std::fs::read_to_string(&file).map_err(|_| {
        Error::rejected(format!(
            "app '{name}' in {project} has no install record {} — it was not \
             installed by `cadence app install` (or was removed by hand)",
            file.display()
        ))
    })?;
    let record: Record = serde_yaml::from_str(&text).map_err(|e| {
        Error::rejected(format!(
            "{} is not a valid install record: {e}",
            file.display()
        ))
    })?;
    if record.schema != 1 {
        return Err(Error::rejected(format!(
            "{}: schema {} — this cadence reads schema 1",
            file.display(),
            record.schema
        )));
    }
    if record.app != name {
        return Err(Error::rejected(format!(
            "{} names app '{}' — the file is for '{name}'",
            file.display(),
            record.app
        )));
    }
    Ok(record)
}

/// Write the install record (deterministic YAML, 0644).
fn write_record(file: &Path, record: &Record) -> Result<()> {
    let text = serde_yaml::to_string(record)
        .map_err(|e| Error::internal(format!("install record: {e}")))?;
    write_verified(file, &text)?;
    Ok(())
}

/// The `app.md` contract: frontmatter is a mapping of
/// [`MANIFEST_KEYS`] — `app` (the folder/install name, tag-shaped),
/// `title`, `version`, and `needs.connections` a distinct list of
/// tag-shaped slots. Later-stage keys refuse with their stage named.
pub fn parse_manifest(text: &str) -> Result<Manifest> {
    let (yaml, body) = parse::split_front(text).map_err(|e| {
        Error::rejected(format!(
            "{e} — app.md needs frontmatter: app, title, version, needs"
        ))
    })?;
    let meta: serde_yaml::Value = serde_yaml::from_str(yaml)
        .map_err(|e| Error::rejected(format!("app.md frontmatter: {e}")))?;
    let serde_yaml::Value::Mapping(map) = meta else {
        return Err(Error::rejected(
            "app.md frontmatter is not a mapping — app, title, version, needs",
        ));
    };
    for key in map.keys() {
        let k = key.as_str().unwrap_or_default();
        if GATED_KEYS.contains(&k) {
            return Err(Error::rejected(format!(
                "app.md frontmatter key '{k}' is a later stage (A2/A3) — v0 installs \
                 app.md + workflows/ + optional rubrics/, templates/ only"
            )));
        }
        if !MANIFEST_KEYS.contains(&k) {
            return Err(Error::rejected(format!(
                "app.md frontmatter key '{k}' is unknown — v0 knows {}; anything else \
                 can never be installed",
                MANIFEST_KEYS.join(", ")
            )));
        }
    }
    let get = |k: &str| map.get(serde_yaml::Value::String(k.to_string()));
    let need_str = |k: &str, what: &str| -> Result<String> {
        match get(k).and_then(|v| v.as_str()) {
            Some(s) if !s.trim().is_empty() => Ok(s.trim().to_string()),
            _ => Err(Error::rejected(format!("app.md needs `{k}:` — {what}"))),
        }
    };
    let app = need_str("app", "the app name — the folder it installs as")?;
    check_name(&app, "app name")?;
    let title = need_str("title", "one line naming the app")?;
    let version = need_str("version", "a version string like 0.1.0")?;
    for (k, v, cap) in [("title", &title, 120), ("version", &version, 40)] {
        if v.chars().count() > cap || v.chars().any(char::is_control) {
            return Err(Error::rejected(format!(
                "app.md `{k}:` — ≤{cap} chars, no control characters"
            )));
        }
    }
    // `summary:` is optional wording — the board's one-line purpose.
    let summary = match get("summary") {
        None | Some(serde_yaml::Value::Null) => None,
        Some(serde_yaml::Value::String(s)) => {
            let s = s.trim();
            if s.is_empty() {
                None
            } else if s.chars().count() > 160 || s.chars().any(char::is_control) {
                return Err(Error::rejected(
                    "app.md `summary:` — ≤160 chars, no control characters",
                ));
            } else {
                Some(s.to_string())
            }
        }
        Some(_) => {
            return Err(Error::rejected(
                "app.md `summary:` is one line of text — what the app is for",
            ))
        }
    };
    let mut connections = Vec::new();
    if let Some(needs) = get("needs") {
        let serde_yaml::Value::Mapping(needs) = needs else {
            return Err(Error::rejected(
                "app.md `needs:` is a mapping — v0 knows `needs.connections`",
            ));
        };
        for key in needs.keys() {
            let k = key.as_str().unwrap_or_default();
            if k != "connections" {
                return Err(Error::rejected(format!(
                    "app.md `needs.{k}` is unknown — v0 knows `needs.connections`"
                )));
            }
        }
        if let Some(list) = needs.get(serde_yaml::Value::String("connections".to_string())) {
            let serde_yaml::Value::Sequence(list) = list else {
                return Err(Error::rejected(
                    "app.md `needs.connections` is a list of slot names — [publish, cms]",
                ));
            };
            for item in list {
                let Some(slot) = item.as_str() else {
                    return Err(Error::rejected(
                        "app.md `needs.connections` entries are slot names — [publish, cms]",
                    ));
                };
                check_name(slot, "connection slot")?;
                if connections.iter().any(|s| s == slot) {
                    return Err(Error::rejected(format!(
                        "connection slot '{slot}' is declared twice"
                    )));
                }
                connections.push(slot.to_string());
            }
        }
    }
    Ok(Manifest {
        app,
        title,
        version,
        connections,
        summary,
        guide: body.to_string(),
    })
}

/// The verified file list of an app folder: `rel-path → source path`.
/// The folder is never followed past a symlink — every entry is checked
/// lstat-style — and only the known top-level entries are read at all.
/// `workflows/` holds flat `*.md` files (the stem a tag-shaped name);
/// `rubrics/` and `templates/` are flat text dirs. Everything else —
/// a nested dir, a fifo/socket/device, a dotfile, a name that is not
/// UTF-8, an oversize file — refuses.
fn bundle_files(root: &Path) -> Result<Vec<(String, PathBuf)>> {
    let meta = root
        .symlink_metadata()
        .map_err(|e| Error::rejected(format!("cannot stat app source {}: {e}", root.display())))?;
    if meta.is_symlink() {
        return Err(Error::rejected(format!(
            "app source {} is a symlink — an app is a real folder",
            root.display()
        )));
    }
    if !meta.is_dir() {
        return Err(Error::rejected(format!(
            "app source {} is not a folder",
            root.display()
        )));
    }
    let mut files: Vec<(String, PathBuf)> = Vec::new();
    let mut dirs: Vec<(String, PathBuf)> = Vec::new();
    let mut manifest = false;
    let entry_err = |name: &str, why: &str| -> Error {
        Error::rejected(format!("app source entry '{name}': {why}"))
    };
    for entry in std::fs::read_dir(root)?.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            return Err(Error::rejected(
                "app source carries a file name that is not UTF-8",
            ));
        };
        if name.starts_with('.') {
            // A clone's `.git` is never app content; other dotfiles are
            // refused outright — nothing in an app is hidden.
            if name == ".git" {
                continue;
            }
            return Err(entry_err(name, "dotfiles are not app content"));
        }
        let ft = entry
            .file_type()
            .map_err(|e| Error::rejected(format!("cannot stat app source entry '{name}': {e}")))?;
        if ft.is_symlink() {
            return Err(entry_err(
                name,
                "a symlink — an app folder holds real files only",
            ));
        }
        if name == MANIFEST {
            if !ft.is_file() {
                return Err(entry_err(name, "app.md is a regular file"));
            }
            manifest = true;
            files.push((name.to_string(), entry.path()));
            continue;
        }
        if TOP_DIRS.contains(&name) {
            if !ft.is_dir() {
                return Err(entry_err(name, "expected a directory"));
            }
            dirs.push((name.to_string(), entry.path()));
            continue;
        }
        return Err(Error::rejected(format!(
            "app source entry '{name}' — v0 knows app.md, workflows/, rubrics/, \
             templates/; everything else refuses"
        )));
    }
    if !manifest {
        return Err(Error::rejected(format!(
            "app source {} has no app.md — frontmatter `app`, `title`, `version` \
             and the agent guide are required",
            root.display()
        )));
    }
    if !dirs.iter().any(|(d, _)| d == "workflows") {
        return Err(Error::rejected(
            "an app needs workflows/ — the workflows it bundles; an app with \
             none installs nothing runnable",
        ));
    }
    for (top, dir) in &dirs {
        for entry in std::fs::read_dir(dir)?.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                return Err(Error::rejected(format!(
                    "{top}/ carries a file name that is not UTF-8"
                )));
            };
            if name.starts_with('.') {
                return Err(entry_err(
                    &format!("{top}/{name}"),
                    "dotfiles are not app content",
                ));
            }
            let ft = entry
                .file_type()
                .map_err(|e| Error::rejected(format!("cannot stat '{top}/{name}': {e}")))?;
            if ft.is_symlink() {
                return Err(entry_err(
                    &format!("{top}/{name}"),
                    "a symlink — an app folder holds real files only",
                ));
            }
            if ft.is_dir() {
                return Err(entry_err(
                    &format!("{top}/{name}"),
                    "v0 app dirs are flat — no nested folders",
                ));
            }
            if !ft.is_file() {
                return Err(entry_err(&format!("{top}/{name}"), "not a regular file"));
            }
            if top == "workflows" {
                let Some(stem) = name.strip_suffix(".md") else {
                    return Err(entry_err(
                        &format!("{top}/{name}"),
                        "workflows are plan-template files ending in .md",
                    ));
                };
                check_name(stem, "workflow name")?;
            }
            files.push((format!("{top}/{name}"), entry.path()));
        }
    }
    if files.len() > MAX_FILES {
        return Err(Error::rejected(format!(
            "app carries {} files — at most {MAX_FILES}",
            files.len()
        )));
    }
    let mut total = 0u64;
    for (rel, path) in &files {
        let len = path.symlink_metadata().map(|m| m.len()).unwrap_or(0);
        if len > MAX_FILE_BYTES {
            return Err(Error::rejected(format!(
                "{rel} is {len} bytes — a file is at most {MAX_FILE_BYTES}"
            )));
        }
        total += len;
        if total > MAX_APP_BYTES {
            return Err(Error::rejected(format!(
                "app is over {MAX_APP_BYTES} bytes of content — split it"
            )));
        }
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(files)
}

/// The `uses:` slots a workflow names: each `uses:` metadata value is a
/// static slot list — `[a, b]` or `a, b` — never a placeholder (the
/// slot contract must be checkable at install).
fn workflow_slots(text: &str) -> Result<Vec<String>> {
    let (_, body) = parse::split_front(text)?;
    let metas = workflow::ticket_meta(body)?;
    let mut slots = Vec::new();
    for meta in metas {
        for (key, value) in meta {
            if key != "uses" {
                continue;
            }
            if value.contains("{{") {
                return Err(Error::rejected(
                    "a `uses:` slot is static — never a `{{input}}` placeholder",
                ));
            }
            for tok in value
                .trim_start_matches('[')
                .trim_end_matches(']')
                .split(',')
                .map(str::trim)
                .filter(|t| !t.is_empty())
            {
                slots.push(tok.to_string());
            }
        }
    }
    Ok(slots)
}

/// What `validate` returns on success: the parsed manifest, the
/// verified file texts (`rel-path → text`, sorted), the secret guard's
/// warnings and the workflow checks' notes.
struct Validated {
    manifest: Manifest,
    files: Vec<(String, String)>,
    secret_warnings: Vec<crate::secret::Finding>,
    notes: Vec<String>,
}

/// Read a bundle's files as UTF-8 text and validate them: the manifest
/// parses, every workflow passes the CAD-487 `workflow check` (same
/// `check_text`), every `uses:` names a declared slot, and the secret
/// guard runs over each file — an install refuses on any error.
fn validate(root: &Path, agents: &HashSet<String>, agent_sources: &[String]) -> Result<Validated> {
    let paths = bundle_files(root)?;
    let mut files = Vec::with_capacity(paths.len());
    let mut secret_warnings = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    for (rel, path) in paths {
        let bytes = std::fs::read(&path)
            .map_err(|e| Error::rejected(format!("cannot read app file {rel}: {e}")))?;
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return Err(Error::rejected(format!(
                "{rel} is {} bytes — a file is at most {MAX_FILE_BYTES}",
                bytes.len()
            )));
        }
        let text = match String::from_utf8(bytes) {
            Ok(text) => text,
            Err(_) => {
                errors.push(format!(
                    "{rel}: not UTF-8 text — an A1 app carries text only"
                ));
                continue;
            }
        };
        match crate::secret::guard(&format!("app file {rel}"), &text) {
            Ok(findings) => secret_warnings.extend(findings),
            Err(e) => errors.push(format!("{rel}: {e}")),
        }
        files.push((rel, text));
    }
    let manifest = match files.iter().find(|(rel, _)| rel == MANIFEST) {
        Some((_, text)) => match parse_manifest(text) {
            Ok(manifest) => manifest,
            Err(e) => {
                errors.push(e.to_string());
                // Without a manifest the slot check cannot run; report
                // the rest anyway.
                Manifest {
                    app: String::new(),
                    title: String::new(),
                    version: String::new(),
                    connections: vec![],
                    summary: None,
                    guide: String::new(),
                }
            }
        },
        None => Manifest {
            app: String::new(),
            title: String::new(),
            version: String::new(),
            connections: vec![],
            summary: None,
            guide: String::new(),
        },
    };
    let mut notes = Vec::new();
    let mut workflow_count = 0usize;
    for (rel, text) in &files {
        // `bundle_files` admits only `workflows/<tag>.md` under that top.
        if !rel.starts_with("workflows/") {
            continue;
        }
        workflow_count += 1;
        let (errs, ns, _) = workflow::check_text(text, agents, agent_sources);
        errors.extend(errs.into_iter().map(|e| format!("{rel}: {e}")));
        notes.extend(ns.into_iter().map(|n| format!("{rel}: {n}")));
        match workflow_slots(text) {
            Ok(slots) => {
                for slot in slots {
                    if !manifest.connections.iter().any(|s| s == &slot) {
                        errors.push(format!(
                            "{rel}: `uses: {slot}` — the app does not declare that slot; \
                             add it to `needs.connections` in app.md so the binding is \
                             the operator's choice, not the bundle's"
                        ));
                    }
                }
            }
            Err(e) => errors.push(format!("{rel}: {e}")),
        }
    }
    if workflow_count == 0 {
        errors.push("workflows/ holds no *.md workflow".to_string());
    }
    if !errors.is_empty() {
        return Err(Error::rejected(format!(
            "app is not installable — {}",
            errors.join("; ")
        )));
    }
    Ok(Validated {
        manifest,
        files,
        secret_warnings,
        notes,
    })
}

/// `mkdir` that never follows a link: `create_dir` is EEXIST on any
/// existing entry — a planted symlink included — and an existing entry
/// is accepted only when it is a real directory (N5).
fn mkdir_verified(dir: &Path) -> Result<()> {
    match std::fs::create_dir(dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => match dir.symlink_metadata() {
            Ok(m) if m.is_symlink() => Err(Error::rejected(format!(
                "{} is a symlink — the tracker never follows links",
                dir.display()
            ))),
            Ok(m) if m.is_dir() => Ok(()),
            Ok(_) => Err(Error::rejected(format!(
                "{} exists and is not a directory",
                dir.display()
            ))),
            Err(e) => Err(e.into()),
        },
        Err(e) => Err(e.into()),
    }
}

/// Write `text` to `file` without ever following a planted link —
/// `O_NOFOLLOW` makes the open refuse one outright (N5).
fn write_verified(file: &Path, text: &str) -> Result<std::fs::File> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NOFOLLOW);
    }
    let mut f = opts.open(file)?;
    std::io::Write::write_all(&mut f, text.as_bytes())?;
    Ok(f)
}

/// Copy a verified bundle into `target`, 0644 files — no mode bits,
/// exec or otherwise, travel with app content.
fn copy_verified(files: &[(String, String)], target: &Path) -> Result<()> {
    for (rel, text) in files {
        let to = target.join(rel);
        if let Some(parent) = to.parent() {
            mkdir_verified(parent)?;
        }
        let f = write_verified(&to, text)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            f.set_permissions(std::fs::Permissions::from_mode(0o644))?;
        }
    }
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The structural digest `app approve` records and `plan propose
/// --workflow <app>/<wf>` matches: app name, declared slots, each
/// slot's effective binding, the guide body, every workflow's CAD-487
/// gate digest, and a content hash of each rubric/template file — the
/// whole envelope the operator reviewed, computed over the INSTALLED
/// dir (never a source it claims to match). Any structural change —
/// install or hand edit — changes it, which is what re-gates the app.
pub fn digest(pm_dir: &Path, project: &str, name: &str) -> Result<String> {
    digest_over(pm_dir, project, name, &[])
}

/// `digest` with caller-held workflow texts: for each `(rel, text)` in
/// `over`, the entry `wf.<x>=` hashes `text` instead of re-reading the
/// file — a caller that renders a buffer it already read proves the
/// approval covered THOSE bytes, never a second read a co-host writer
/// could have flipped (N2: plan_text renders what it digests).
fn digest_over(pm_dir: &Path, project: &str, name: &str, over: &[(&str, &str)]) -> Result<String> {
    let dir = app_dir(pm_dir, project, name)?;
    let record = read_record(pm_dir, project, name)?;
    let text = std::fs::read_to_string(dir.join(MANIFEST)).map_err(|e| {
        Error::rejected(format!("cannot read {}: {e}", dir.join(MANIFEST).display()))
    })?;
    let manifest = parse_manifest(&text)
        .map_err(|e| Error::rejected(format!("installed {}: {e}", dir.join(MANIFEST).display())))?;
    // The installed layout is walked with the same strict rules as a
    // source — a symlink planted post-install makes the app undigestable
    // rather than silently outside the digest.
    let files = bundle_files(&dir)?;
    let mut keys = format!("app={}\n", manifest.app);
    keys.push_str(&format!("slots={}\n", manifest.connections.join(",")));
    for slot in &manifest.connections {
        keys.push_str(&format!(
            "bind.{slot}={}\n",
            record.binding(slot).unwrap_or("~")
        ));
    }
    keys.push_str(&format!(
        "guide={}\n",
        sha256_hex(manifest.guide.as_bytes())
    ));
    for (rel, path) in &files {
        if rel == MANIFEST {
            continue;
        }
        if let Some(wf) = rel
            .strip_prefix("workflows/")
            .and_then(|n| n.strip_suffix(".md"))
        {
            let text: std::borrow::Cow<'_, str> = match over.iter().find(|(r, _)| r == rel) {
                Some((_, t)) => std::borrow::Cow::Borrowed(*t),
                None => std::borrow::Cow::Owned(
                    std::fs::read_to_string(path)
                        .map_err(|e| Error::rejected(format!("cannot read {rel}: {e}")))?,
                ),
            };
            keys.push_str(&format!("wf.{wf}={}\n", workflow::gate_digest(&text)?));
        } else {
            let bytes = std::fs::read(path)
                .map_err(|e| Error::rejected(format!("cannot read {rel}: {e}")))?;
            keys.push_str(&format!("file.{rel}={}\n", sha256_hex(&bytes)));
        }
    }
    Ok(format!("sha256:{}", sha256_hex(keys.as_bytes())))
}

/// Every recorded app approval: `"<project>/<app>"` → payload. Read
/// straight from the store, read-only — like the workflow approvals —
/// so `ls`/`show`/`check` work with the daemon down; `None` means absent
/// or unreadable, and readers show approval as unknown, never granted.
pub fn fetch_approvals(state_dir: &Path) -> Option<Map<String, Value>> {
    let path = state_dir.join("cadence.sqlite3");
    if !path.exists() {
        return None;
    }
    let conn = crate::store::open_read_only(&path).ok()?;
    let mut stmt = conn
        .prepare("SELECT payload FROM events WHERE alias=? AND kind=? ORDER BY seq")
        .ok()?;
    let rows = stmt
        .query_map(
            rusqlite::params![
                crate::store::APPROVAL_STREAM,
                crate::store::APP_APPROVED_EVENT
            ],
            |r| r.get::<_, String>(0),
        )
        .ok()?;
    let mut out = Map::new();
    for raw in rows.flatten() {
        let Ok(payload) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        if let (Some(p), Some(n)) = (payload["project"].as_str(), payload["name"].as_str()) {
            out.insert(approval_key(p, n), payload);
        }
    }
    Some(out)
}

/// The key an approval is stored under — `"<project>/<app>"`.
pub fn approval_key(project: &str, name: &str) -> String {
    format!("{project}/{name}")
}

/// The Apps board's approval word (CAD-557) — why `approved` is false:
/// `approved`, `changed` (a record exists but the digest moved — any
/// bundle or binding edit re-gates the app), `unapproved` (no record),
/// or `unknown` (the approval store did not read — never shown granted).
fn approval_state(approvals: Option<&Map<String, Value>>, key: &str, digest: &str) -> &'static str {
    let Some(approvals) = approvals else {
        return "unknown";
    };
    match approvals.get(key) {
        Some(p) if p["digest"].as_str() == Some(digest) => "approved",
        Some(_) => "changed",
        None => "unapproved",
    }
}

/// Is `digest` the approved digest for `project/app`? An unreachable
/// daemon is no approval — fail closed, like `plan propose` refusing.
pub fn approved(
    project: &str,
    name: &str,
    digest: &str,
    approvals: Option<&Map<String, Value>>,
) -> bool {
    approvals
        .and_then(|a| a.get(&approval_key(project, name)))
        .and_then(|p| p["digest"].as_str())
        == Some(digest)
}

/// `<app>/<workflow>` → both parts, tag-shaped; `None` for a bare name
/// or a malformed ref (a second `/`, an empty half).
pub fn split_ref(name: &str) -> Option<(&str, &str)> {
    let (app, wf) = name.split_once('/')?;
    if model::valid_tag(app) && model::valid_tag(wf) && !wf.contains('/') {
        Some((app, wf))
    } else {
        None
    }
}

/// The installed `apps/<app>/workflows/<wf>.md` — real file only.
pub fn read_workflow(pm_dir: &Path, project: &str, app: &str, wf: &str) -> Result<String> {
    check_name(wf, "workflow name")?;
    let file = app_dir(pm_dir, project, app)?
        .join("workflows")
        .join(format!("{wf}.md"));
    if file.symlink_metadata().is_ok_and(|m| m.is_symlink()) {
        return Err(Error::rejected(format!(
            "{} is a symlink — the tracker never follows links",
            file.display()
        )));
    }
    std::fs::read_to_string(&file).map_err(|_| {
        Error::rejected(format!(
            "no workflow '{wf}' in app '{app}' ({project}) — `cadence app show {app} \
             --project {project}` lists its workflows"
        ))
    })
}

/// The git source resolved and pinned: clone into a private temp dir
/// (never executed, never trusted past the SHA we record), answer the
/// clone root and the commit SHA `rev-parse HEAD` reads.
fn clone_git(url: &str, into: &Path) -> Result<String> {
    let out = crate::reaper::output(
        std::process::Command::new("timeout")
            .arg("120")
            .arg("git")
            .args(["clone", "--quiet", "--no-tags", "--depth", "1", "--"])
            .arg(url)
            .arg(into)
            .env("GIT_TERMINAL_PROMPT", "0")
            // `--` keeps a dash-led source a repository name, never a
            // switch; transports are pinned to https/ssh/file (a local
            // dir installs as a path, never through clone) — no `ext::`
            // or other transport ever runs (N3).
            .env("GIT_ALLOW_PROTOCOL", "https:ssh:file"),
    )
    .map_err(|e| Error::rejected(format!("git clone of '{url}' failed to run: {e}")))?;
    if !out.status.success() {
        return Err(Error::rejected(format!(
            "git clone of '{url}' failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let head = crate::reaper::output(
        std::process::Command::new("git")
            .arg("-C")
            .arg(into)
            .args(["rev-parse", "HEAD"]),
    )
    .map_err(|e| Error::rejected(format!("git rev-parse in the clone failed: {e}")))?;
    let sha = String::from_utf8_lossy(&head.stdout).trim().to_string();
    if !head.status.success()
        || sha.len() != 40
        || !sha
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(Error::rejected(format!(
            "git clone of '{url}' gave no commit SHA — the install pins one; \
             the clone may be empty"
        )));
    }
    Ok(sha)
}

/// Resolve `<path|git-url>` to `(dir to scan, source record)`. An
/// existing directory is a path install; anything else is tried as a
/// git URL (a mistyped path fails the clone with a clear error). The
/// returned TempDir, when `Some`, owns the clone — keep it alive until
/// the copy has landed.
fn resolve_source(source: &str) -> Result<(PathBuf, Source, Option<tempfile::TempDir>)> {
    let path = Path::new(source);
    if path.symlink_metadata().is_ok_and(|m| m.is_symlink()) {
        return Err(Error::rejected(format!(
            "app source {source} is a symlink — an app is a real folder"
        )));
    }
    if path.is_dir() {
        let canon = path
            .canonicalize()
            .map_err(|e| Error::rejected(format!("cannot resolve app source {source}: {e}")))?;
        return Ok((
            canon.clone(),
            Source::Path {
                path: canon.display().to_string(),
            },
            None,
        ));
    }
    if path.exists() {
        return Err(Error::rejected(format!(
            "app source {source} is a file — it names a folder or a git url"
        )));
    }
    let tmp = tempfile::tempdir()?;
    let clone_dir = tmp.path().join("clone");
    let sha = clone_git(source, &clone_dir)?;
    Ok((
        clone_dir,
        Source::Git {
            url: source.to_string(),
            sha,
        },
        Some(tmp),
    ))
}

/// `cadence app install <path|git-url> --project <key>`: verify the
/// bundle (every workflow through the same `workflow check`), copy it
/// to `<pm>/<key>/apps/<name>/`, write `apps/<name>.yaml`, and land it
/// UNAPPROVED — one tracker commit, `Actor:` recorded. Nothing in the
/// bundle executes; a git source pins its HEAD SHA into the record.
pub fn install(
    pm: &Pm,
    project_key: &str,
    source: &str,
    state_dir: &Path,
    actor: &str,
) -> Result<Value> {
    model::check_key(project_key)?;
    if !project::list(&pm.dir)?.iter().any(|p| p.key == project_key) {
        return Err(project::unknown_project(project_key, &pm.dir));
    }
    let (src_dir, src, _tmp) = resolve_source(source)?;
    // The tracker is never its own source — `apps/` inside it included.
    let canon_pm = pm.dir.canonicalize().unwrap_or_else(|_| pm.dir.clone());
    if src_dir.starts_with(&canon_pm) {
        return Err(Error::rejected(format!(
            "app source {source} is inside the tracker {} — install a copy \
             from outside it",
            pm.dir.display()
        )));
    }
    let daemon_agents = daemon_aliases(state_dir);
    let (agents, agent_sources) =
        workflow::known_agents(&pm.dir, Some(project_key), &daemon_agents);
    let Validated {
        manifest,
        files,
        secret_warnings,
        notes,
    } = validate(&src_dir, &agents, &agent_sources)?;
    let name = manifest.app.clone();
    let apps = dir_of(&pm.dir, project_key)?;
    let target = apps.join(&name);
    let record_path = apps.join(format!("{name}.yaml"));
    let _lock = pm.lock()?;
    for path in [&target, &record_path] {
        match path.symlink_metadata() {
            Ok(m) if m.is_symlink() => {
                return Err(Error::rejected(format!(
                    "{} is a symlink — the tracker never follows links",
                    path.display()
                )));
            }
            Ok(_) => {
                return Err(Error::rejected(format!(
                    "app '{name}' is already installed in {project_key} — `cadence \
                     app update {name} --project {project_key}` replaces it"
                )));
            }
            Err(_) => {}
        }
    }
    // mkdir, never create_dir_all: a leaf planted between the check and
    // here — a symlink included — is EEXIST, not a dir the copy writes
    // into; then what exists is verified a real directory (N5).
    mkdir_verified(&apps)?;
    mkdir_verified(&target)?;
    match target.symlink_metadata() {
        Ok(m) if m.is_dir() && !m.is_symlink() => {}
        _ => {
            let _ = std::fs::remove_dir_all(&target);
            return Err(Error::rejected(format!(
                "{} was moved or replaced during install — refusing",
                target.display()
            )));
        }
    }
    let rollback = |keep_record: bool| {
        let _ = std::fs::remove_dir_all(&target);
        if !keep_record {
            let _ = std::fs::remove_file(&record_path);
        }
    };
    if let Err(e) = copy_verified(&files, &target) {
        rollback(false);
        return Err(e);
    }
    let now = crate::issue::time::iso(crate::issue::time::now_epoch());
    let record = Record {
        schema: 1,
        app: name.clone(),
        source: src.clone(),
        bindings: BTreeMap::new(),
        installed_at: now,
        installed_by: write::actor_who(actor, None),
        updated_at: None,
    };
    if let Err(e) = write_record(&record_path, &record) {
        rollback(false);
        return Err(e);
    }
    let label = match &src {
        Source::Path { path } => format!("path {path}"),
        Source::Git { url, sha } => format!("git {url} @ {}", &sha[..12]),
    };
    let foreign = match write::commit(
        pm,
        &[target.clone(), record_path.clone()],
        &format!("{project_key}/{DIR}/{name}: app installed ({label})"),
        &[],
        actor,
    ) {
        Ok(foreign) => foreign,
        Err(e) => {
            rollback(false);
            return Err(e);
        }
    };
    let digest = digest(&pm.dir, project_key, &name)?;
    let mut out = json!({
        "project": project_key,
        "name": name,
        "path": target,
        "title": manifest.title,
        "version": manifest.version,
        "connections": manifest.connections,
        "workflows": files.iter().filter_map(|(rel, _)| rel
            .strip_prefix("workflows/").and_then(|n| n.strip_suffix(".md"))
            .map(str::to_string)).collect::<Vec<_>>(),
        "digest": digest,
        "approved": false,
        "note": "unapproved — nothing in the app runs until the operator's \
                 `cadence app approve`",
        "source": serde_json::to_value(&src).unwrap_or(Value::Null),
        "committed": true,
        "notes": notes,
    });
    if !secret_warnings.is_empty() {
        out["secret_warnings"] = crate::secret::warnings_json(&secret_warnings);
    }
    write::attach_foreign(&mut out, &foreign);
    Ok(out)
}

/// The diff `app update` prints — `added`/`removed`/`changed` rel-paths
/// plus a unified `patch` (`git diff --no-index`, `<app>` as both roots)
/// capped at 64 KiB.
fn diff_report(name: &str, old_dir: &Path, new_files: &[(String, String)]) -> Result<Value> {
    let old: BTreeMap<String, String> = bundle_files(old_dir)?
        .into_iter()
        .filter_map(|(rel, path)| std::fs::read_to_string(&path).ok().map(|t| (rel, t)))
        .collect();
    let new: BTreeMap<String, String> = new_files.iter().cloned().collect();
    let added: Vec<&String> = new.keys().filter(|k| !old.contains_key(*k)).collect();
    let removed: Vec<&String> = old.keys().filter(|k| !new.contains_key(*k)).collect();
    let changed: Vec<&String> = new
        .iter()
        .filter(|(k, v)| old.get(*k).is_some_and(|o| o != *v))
        .map(|(k, _)| k)
        .collect();
    let tmp = tempfile::tempdir()?;
    let (old_root, new_root) = (tmp.path().join("old"), tmp.path().join("new"));
    for (rel, text) in &old {
        let to = old_root.join(rel);
        std::fs::create_dir_all(to.parent().unwrap())?;
        std::fs::write(to, text)?;
    }
    for (rel, text) in &new {
        let to = new_root.join(rel);
        std::fs::create_dir_all(to.parent().unwrap())?;
        std::fs::write(to, text)?;
    }
    let out = crate::reaper::output(
        std::process::Command::new("git")
            .arg("-C")
            .arg(tmp.path())
            .args(["diff", "--no-index", "--", "old", "new"]),
    )?;
    // --no-index exits 1 on differences, 2+ on a real failure.
    if !matches!(out.status.code(), Some(0) | Some(1)) {
        return Err(Error::internal(format!(
            "git diff --no-index failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let mut patch = String::from_utf8_lossy(&out.stdout).to_string();
    patch = patch.replace("a/old/", &format!("a/{name}/"));
    patch = patch.replace("b/new/", &format!("b/{name}/"));
    const CAP: usize = 64 * 1024;
    if patch.len() > CAP {
        let mut cap = CAP;
        while !patch.is_char_boundary(cap) {
            cap -= 1;
        }
        patch.truncate(cap);
        patch.push_str("\n… patch truncated at 64 KiB");
    }
    Ok(json!({
        "added": added,
        "removed": removed,
        "changed": changed,
        "patch": patch,
    }))
}

/// `cadence app update <app> [path|git-url] --project <key>` — replace
/// the installed bundle from the recorded (or a given) source, after
/// the same verification install runs. Prints the diff; bindings that
/// still name declared slots survive, the rest are dropped (named in
/// the output). Any structural change — a slot, a binding carried over
/// or not, a workflow gate key, a guide/rubric/template byte — changes
/// the digest, which is what re-requires approval: nothing extra to
/// revoke.
pub fn update(
    pm: &Pm,
    project_key: &str,
    name: &str,
    source_arg: Option<&str>,
    state_dir: &Path,
    actor: &str,
) -> Result<Value> {
    model::check_key(project_key)?;
    check_name(name, "app name")?;
    let dir = app_dir(&pm.dir, project_key, name)?;
    let record = read_record(&pm.dir, project_key, name)?;
    let old_digest = digest(&pm.dir, project_key, name)?;
    let old_version = read_manifest(&pm.dir, project_key, name)
        .map(|m| m.version)
        .unwrap_or_default();
    let (src_dir, src, _tmp) = match source_arg {
        Some(arg) => resolve_source(arg)?,
        None => match &record.source {
            Source::Path { path } => resolve_source(path)?,
            Source::Git { url, .. } => resolve_source(url)?,
        },
    };
    if src_dir.starts_with(pm.dir.canonicalize().unwrap_or_else(|_| pm.dir.clone())) {
        return Err(Error::rejected(format!(
            "app source {} is inside the tracker — update from outside it",
            src_dir.display()
        )));
    }
    let daemon_agents = daemon_aliases(state_dir);
    let (agents, agent_sources) =
        workflow::known_agents(&pm.dir, Some(project_key), &daemon_agents);
    let Validated {
        manifest,
        files,
        secret_warnings,
        notes,
    } = validate(&src_dir, &agents, &agent_sources)?;
    if manifest.app != name {
        return Err(Error::rejected(format!(
            "the bundle names app '{}' — `app update {name}` replaces '{name}' \
             only; install '{}' separately",
            manifest.app, manifest.app
        )));
    }
    let diff = diff_report(name, &dir, &files)?;
    // Slot bindings carry over only where the new manifest still
    // declares the slot; dropped ones are named, not silently kept.
    let mut bindings = record.bindings.clone();
    let dropped: Vec<String> = bindings
        .keys()
        .filter(|k| !manifest.connections.iter().any(|s| s == *k))
        .cloned()
        .collect();
    for slot in &dropped {
        bindings.remove(slot);
    }
    let _lock = pm.lock()?;
    // Replace: the dir is our own verified layout — remove the files
    // the diff says are gone, then write the new set. A failure mid-way
    // leaves a partially-updated dir that fails digest — unusable until
    // the operator re-runs update or remove, never silently approved.
    for rel in diff["removed"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str())
    {
        let _ = std::fs::remove_file(dir.join(rel));
    }
    // A top dir the update emptied is removed too — an empty rubrics/
    // is lint-clean but dead weight.
    for top in TOP_DIRS {
        let sub = dir.join(top);
        if sub.is_dir()
            && std::fs::read_dir(&sub)
                .map(|mut e| e.next().is_none())
                .unwrap_or(false)
            && !files
                .iter()
                .any(|(rel, _)| rel.starts_with(&format!("{top}/")))
        {
            let _ = std::fs::remove_dir(&sub);
        }
    }
    copy_verified(&files, &dir)?;
    let mut record = record;
    record.source = src;
    record.bindings = bindings;
    record.updated_at = Some(crate::issue::time::iso(crate::issue::time::now_epoch()));
    let record_path = record_file(&pm.dir, project_key, name)?;
    write_record(&record_path, &record)?;
    let label = match &record.source {
        Source::Path { path } => format!("path {path}"),
        Source::Git { url, sha } => format!("git {url} @ {}", &sha[..12]),
    };
    let foreign = write::commit(
        pm,
        &[dir.clone(), record_path],
        &format!("{project_key}/{DIR}/{name}: app updated ({label})"),
        &[],
        actor,
    )?;
    let digest = digest(&pm.dir, project_key, name)?;
    let approvals = fetch_approvals(state_dir);
    let approved = approved(project_key, name, &digest, approvals.as_ref());
    let mut out = json!({
        "project": project_key,
        "name": name,
        "version": {"from": old_version, "to": manifest.version},
        "diff": diff,
        "dropped_bindings": dropped,
        "digest": digest,
        "approved": approved,
        "gate_changed": old_digest != digest,
        "committed": true,
        "notes": notes,
    });
    if old_digest != digest {
        out["unapproved"] = json!(
            "structural change — `cadence app approve {name} --project {project_key}` \
             before anything in the app runs"
        );
    }
    if !secret_warnings.is_empty() {
        out["secret_warnings"] = crate::secret::warnings_json(&secret_warnings);
    }
    write::attach_foreign(&mut out, &foreign);
    Ok(out)
}

/// Epic ids of plans a `plan propose --workflow <name>/…` created that
/// are still open — `state` not `rejected` and the epic not
/// done/dropped. The tracker provenance (`plan.workflow`) is the record;
/// the daemon's `plan_proposed` events are the fallback for epics whose
/// frontmatter lost the field to an older binary's rewrite.
fn open_plan_epics(pm_dir: &Path, project_key: &str, name: &str, state_dir: &Path) -> Vec<String> {
    let mut open: Vec<String> = Vec::new();
    let is_open = |front: &model::Front| -> bool {
        let Some(plan) = &front.plan else {
            return false;
        };
        plan.state != "rejected" && !matches!(front.status.as_str(), "done" | "dropped")
    };
    // `<app>/<wf>` only — a stored workflow that happens to share the
    // app's name (`plan.workflow = "studio"`, no slash) is not its plan.
    let from_app = |wf: &str| split_ref(wf).is_some_and(|(app, _)| app == name);
    if let Ok(issues) = board::load_all(pm_dir, Some(project_key)) {
        for issue in &issues {
            if let Some(wf) = issue
                .front
                .plan
                .as_ref()
                .and_then(|p| p.workflow.as_deref())
            {
                if from_app(wf) && is_open(&issue.front) {
                    open.push(issue.front.id.clone());
                }
            }
        }
    }
    // Fallback provenance: `plan_proposed` payloads carry `app`.
    let db = state_dir.join("cadence.sqlite3");
    if db.exists() {
        if let Ok(conn) = crate::store::open_read_only(&db) {
            if let Ok(mut stmt) =
                conn.prepare("SELECT payload FROM events WHERE alias=? AND kind=? ORDER BY seq")
            {
                if let Ok(rows) = stmt.query_map(
                    rusqlite::params![crate::store::Store::DAEMON_STREAM, "plan_proposed"],
                    |r| r.get::<_, String>(0),
                ) {
                    for raw in rows.flatten() {
                        let Ok(payload) = serde_json::from_str::<Value>(&raw) else {
                            continue;
                        };
                        if payload["app"].as_str() != Some(name)
                            || payload["project"].as_str() != Some(project_key)
                        {
                            continue;
                        }
                        let Some(epic) = payload["epic"].as_str() else {
                            continue;
                        };
                        if model::check_id(epic).is_err() || open.iter().any(|id| id == epic) {
                            continue;
                        }
                        // Only an epic that still exists and is still
                        // open blocks removal.
                        if let Ok((front, _)) = parse::parse_issue(
                            &std::fs::read_to_string(
                                pm_dir.join(project_key).join(epic).join("issue.md"),
                            )
                            .unwrap_or_default(),
                        ) {
                            if is_open(&front) {
                                open.push(epic.to_string());
                            }
                        }
                    }
                }
            }
        }
    }
    open.sort();
    open.dedup();
    open
}

/// `cadence app remove <app> --project <key>` — delete the content
/// folder and its install record in one tracker commit. Refuses while a
/// plan proposed from the app is still open — an open plan's tickets
/// keep their provenance readable.
pub fn remove(
    pm: &Pm,
    project_key: &str,
    name: &str,
    state_dir: &Path,
    actor: &str,
) -> Result<Value> {
    model::check_key(project_key)?;
    let dir = app_dir(&pm.dir, project_key, name)?;
    let record_path = record_file(&pm.dir, project_key, name)?;
    let open = open_plan_epics(&pm.dir, project_key, name, state_dir);
    if !open.is_empty() {
        return Err(Error::rejected(format!(
            "app '{name}' has open plans ({}) — decide or finish them first \
             (`cadence plan ls --project {project_key}`)",
            open.join(", ")
        )));
    }
    let _lock = pm.lock()?;
    std::fs::remove_dir_all(&dir)?;
    if record_path.exists() {
        std::fs::remove_file(&record_path)?;
    }
    let foreign = write::commit(
        pm,
        &[dir, record_path],
        &format!("{project_key}/{DIR}/{name}: app removed"),
        &[],
        actor,
    )?;
    let mut out = json!({
        "project": project_key,
        "name": name,
        "removed": true,
        "committed": true,
    });
    write::attach_foreign(&mut out, &foreign);
    Ok(out)
}

/// `cadence app set <app> <slot>=<connection> […] --project <key>` —
/// record slot bindings in the install record; `<slot>=` unbinds
/// explicitly (neither bound nor defaulted — `doctor` reports it). A
/// slot the manifest does not declare refuses; a connection name the
/// daemon does not register lands with a warning (it may not exist yet
/// — `doctor` reports it as unknown). Because bindings are in the gate
/// digest, every `set` re-gates the app.
pub fn set(
    pm: &Pm,
    project_key: &str,
    name: &str,
    bindings: &[String],
    state_dir: &Path,
    actor: &str,
) -> Result<Value> {
    if bindings.is_empty() {
        return Err(Error::rejected(
            "app set needs <slot>=<connection> — `cadence app show` lists the slots",
        ));
    }
    model::check_key(project_key)?;
    let _ = app_dir(&pm.dir, project_key, name)?;
    let manifest = read_manifest(&pm.dir, project_key, name)?;
    let mut record = read_record(&pm.dir, project_key, name)?;
    let known = known_connections(state_dir);
    let mut warnings = Vec::new();
    for binding in bindings {
        let Some((slot, conn)) = binding.split_once('=') else {
            return Err(Error::rejected(format!(
                "app set takes <slot>=<connection> — got '{binding}'"
            )));
        };
        check_name(slot, "connection slot")?;
        if !manifest.connections.iter().any(|s| s == slot) {
            return Err(Error::rejected(format!(
                "app '{name}' declares no slot '{slot}' — `needs.connections` has: {}",
                if manifest.connections.is_empty() {
                    "none".to_string()
                } else {
                    manifest.connections.join(", ")
                }
            )));
        }
        if conn.is_empty() {
            record.bindings.insert(slot.to_string(), None);
            continue;
        }
        crate::proto::identifier(conn, "connection name")?;
        if let Some(known) = &known {
            if !known.contains(conn) {
                warnings.push(format!(
                    "connection '{conn}' is not registered — the daemon knows: {}; \
                     `cadence doctor` reports slots bound to connections that do \
                     not exist",
                    known.iter().cloned().collect::<Vec<_>>().join(", ")
                ));
            }
        }
        record
            .bindings
            .insert(slot.to_string(), Some(conn.to_string()));
    }
    let _lock = pm.lock()?;
    let record_path = record_file(&pm.dir, project_key, name)?;
    write_record(&record_path, &record)?;
    let foreign = write::commit(
        pm,
        &[record_path],
        &format!("{project_key}/{DIR}/{name}: connection bindings set"),
        &[],
        actor,
    )?;
    let digest = digest(&pm.dir, project_key, name)?;
    let approvals = fetch_approvals(state_dir);
    let effective: Map<String, Value> = manifest
        .connections
        .iter()
        .map(|slot| {
            (
                slot.clone(),
                record.binding(slot).map_or(Value::Null, |c| json!(c)),
            )
        })
        .collect();
    let mut out = json!({
        "project": project_key,
        "name": name,
        "bindings": effective,
        "digest": digest,
        "approved": approved(project_key, name, &digest, approvals.as_ref()),
        "committed": true,
        "warnings": warnings,
    });
    write::attach_foreign(&mut out, &foreign);
    Ok(out)
}

/// The connection names the daemon registers, best-effort — `local` is
/// always in the set (the built-in contract, CAD-546 or not). `None`
/// when the daemon is unreachable; callers then report "not verified"
/// rather than "unknown".
fn known_connections(state_dir: &Path) -> Option<HashSet<String>> {
    let v = crate::client::rpc(state_dir, "daemon_info", json!({})).ok()?;
    let mut set: HashSet<String> = v["connections"]
        .as_array()?
        .iter()
        .filter_map(|c| c.as_str().map(str::to_string))
        .collect();
    set.insert(LOCAL_CONNECTION.to_string());
    Some(set)
}

/// The manifest of an installed app (`apps/<name>/app.md`).
pub fn read_manifest(pm_dir: &Path, project: &str, name: &str) -> Result<Manifest> {
    let file = app_dir(pm_dir, project, name)?.join(MANIFEST);
    if file.symlink_metadata().is_ok_and(|m| m.is_symlink()) {
        return Err(Error::rejected(format!(
            "{} is a symlink — the tracker never follows links",
            file.display()
        )));
    }
    let text = std::fs::read_to_string(&file)
        .map_err(|e| Error::rejected(format!("cannot read {}: {e}", file.display())))?;
    parse_manifest(&text)
}

/// The daemon's registered agent aliases, best-effort — install/update
/// resolve `agent:` against them like `workflow add` does.
fn daemon_aliases(state_dir: &Path) -> Vec<String> {
    crate::client::rpc(state_dir, "agent_list", json!({}))
        .ok()
        .and_then(|v| v["agents"].as_array().cloned())
        .map(|a| {
            a.iter()
                .filter_map(|a| a["alias"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Every installed app of `project_key` (or all projects), one row per
/// `apps/<name>/` dir — plus one `error` row per stray entry (a symlink,
/// an orphan `<name>.yaml` with no folder, a folder with no record).
pub fn ls(pm: &Pm, project_key: Option<&str>, state_dir: &Path) -> Result<Value> {
    let projects = project::list(&pm.dir)?;
    let projects: Vec<&project::Project> = match project_key {
        Some(key) => {
            let found: Vec<&project::Project> = projects.iter().filter(|p| p.key == key).collect();
            if found.is_empty() {
                return Err(project::unknown_project(key, &pm.dir));
            }
            found
        }
        None => projects.iter().collect(),
    };
    let approvals = fetch_approvals(state_dir);
    let mut rows = Vec::new();
    for p in projects {
        let dir = pm.dir.join(&p.key).join(DIR);
        if dir.symlink_metadata().is_ok_and(|m| m.is_symlink()) {
            rows.push(json!({
                "project": p.key, "name": null,
                "error": "apps/ is a symlink — the tracker never follows links",
            }));
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut seen_records: HashSet<String> = HashSet::new();
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let Ok(ft) = e.file_type() else {
                continue;
            };
            if ft.is_symlink() {
                rows.push(json!({
                    "project": p.key, "name": name,
                    "error": "symlink — the tracker never follows links",
                }));
                continue;
            }
            if ft.is_dir() {
                rows.push(describe(&pm.dir, &p.key, &name, approvals.as_ref()));
                continue;
            }
            if let Some(stem) = name.strip_suffix(".yaml") {
                seen_records.insert(stem.to_string());
                continue; // the record's own row is the app dir's
            }
            rows.push(json!({
                "project": p.key, "name": name,
                "error": "not an app — apps/ holds <name>/ folders and <name>.yaml records",
            }));
        }
        // A record without its folder — a half-removed app.
        for stem in seen_records {
            if !dir.join(&stem).is_dir() {
                rows.push(json!({
                    "project": p.key, "name": stem,
                    "error": "install record without its app folder — remove \
                              the stray file or reinstall",
                }));
            }
        }
    }
    rows.sort_by(|a, b| {
        (
            a["project"].as_str().unwrap_or(""),
            a["name"].as_str().unwrap_or(""),
        )
            .cmp(&(
                b["project"].as_str().unwrap_or(""),
                b["name"].as_str().unwrap_or(""),
            ))
    });
    Ok(json!({"apps": rows, "count": rows.len()}))
}

/// One installed app's summary row — used by `ls` and the board list.
fn describe(
    pm_dir: &Path,
    project: &str,
    name: &str,
    approvals: Option<&Map<String, Value>>,
) -> Value {
    let mut row = json!({"project": project, "name": name});
    let record = match read_record(pm_dir, project, name) {
        Ok(record) => record,
        Err(e) => {
            row["error"] = json!(e.to_string());
            return row;
        }
    };
    let manifest = match read_manifest(pm_dir, project, name) {
        Ok(manifest) => manifest,
        Err(e) => {
            row["error"] = json!(e.to_string());
            return row;
        }
    };
    let workflows: Vec<String> = match bundle_files(&app_dir_or(pm_dir, project, name)) {
        Ok(files) => files
            .iter()
            .filter_map(|(rel, _)| {
                rel.strip_prefix("workflows/")
                    .and_then(|n| n.strip_suffix(".md"))
                    .map(str::to_string)
            })
            .collect(),
        Err(e) => {
            row["error"] = json!(e.to_string());
            return row;
        }
    };
    row["title"] = json!(manifest.title);
    row["version"] = json!(manifest.version);
    row["summary"] = json!(manifest.summary);
    row["workflows"] = json!(workflows);
    // The card's primary action (CAD-563 r2): the app's first workflow
    // (sorted, so the choice is deterministic) and the human label the
    // board names the action with.
    let mut names = workflows.clone();
    names.sort();
    if let Some(first) = names.first() {
        let label = std::fs::read_to_string(
            app_dir_or(pm_dir, project, name)
                .join("workflows")
                .join(format!("{first}.md")),
        )
        .ok()
        .and_then(|text| workflow::parse_template(&text).ok())
        .and_then(|tpl| tpl.label);
        row["primary"] = json!({"workflow": first, "label": label});
    }
    row["connections"] = json!(manifest
        .connections
        .iter()
        .map(|slot| json!({
            "slot": slot,
            "bound": record.binding(slot),
        }))
        .collect::<Vec<_>>());
    match digest(pm_dir, project, name) {
        Ok(d) => {
            row["digest"] = json!(d);
            row["approved"] = match approvals {
                Some(a) => json!(approved(project, name, &d, Some(a))),
                None => json!("unknown — daemon unreachable"),
            };
            row["approval"] = json!(approval_state(approvals, &approval_key(project, name), &d));
        }
        Err(e) => row["error"] = json!(e.to_string()),
    }
    row["source"] = serde_json::to_value(&record.source).unwrap_or(Value::Null);
    row["installed_at"] = json!(record.installed_at);
    row["installed_by"] = json!(record.installed_by);
    row["updated_at"] = json!(record.updated_at);
    row
}

/// `apps/<name>/` without the "must be installed" wording — for
/// internal walkers that already know it exists.
fn app_dir_or(pm_dir: &Path, project: &str, name: &str) -> PathBuf {
    pm_dir.join(project).join(DIR).join(name)
}

/// `cadence app show <app> --project <key>` — the manifest, the guide,
/// each workflow's summary (the same detail `workflow show` gives),
/// declared slots with their effective bindings, the install record,
/// the digest and the approval state.
pub fn show(pm: &Pm, project_key: &str, name: &str, state_dir: &Path) -> Result<Value> {
    model::check_key(project_key)?;
    if !project::list(&pm.dir)?.iter().any(|p| p.key == project_key) {
        return Err(project::unknown_project(project_key, &pm.dir));
    }
    let mut out = describe(
        &pm.dir,
        project_key,
        name,
        fetch_approvals(state_dir).as_ref(),
    );
    if out.get("error").is_some() {
        return Ok(out);
    }
    let dir = app_dir(&pm.dir, project_key, name)?;
    let manifest = read_manifest(&pm.dir, project_key, name)?;
    let daemon_agents = daemon_aliases(state_dir);
    let (agents, agent_sources) =
        workflow::known_agents(&pm.dir, Some(project_key), &daemon_agents);
    let mut workflows = Vec::new();
    let mut rubrics = Vec::new();
    for (rel, path) in bundle_files(&dir)? {
        if let Some(rb) = rel
            .strip_prefix("rubrics/")
            .and_then(|n| n.strip_suffix(".md"))
        {
            // A file verified real at scan could still be swapped for a
            // link before the read — re-check the leaf, like
            // `read_workflow` and `board_rows` do.
            if path
                .symlink_metadata()
                .map(|m| !m.is_file())
                .unwrap_or(true)
            {
                continue;
            }
            rubrics.push(json!({
                "name": rb,
                "body": std::fs::read_to_string(&path).unwrap_or_default(),
            }));
            continue;
        }
        let Some(wf) = rel
            .strip_prefix("workflows/")
            .and_then(|n| n.strip_suffix(".md"))
            .map(str::to_string)
        else {
            continue;
        };
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let (errors, notes, doc) = workflow::check_text(&text, &agents, &agent_sources);
        let tpl = workflow::parse_template(&text).ok();
        workflows.push(json!({
            "name": format!("{name}/{wf}"),
            "ok": errors.is_empty(),
            "errors": errors,
            "notes": notes,
            "title": doc.as_ref().map(|d| d.title.clone()),
            // The workflow's human label (`label:`, wording) — the
            // board's primary action names it; the app's title when the
            // workflow never declared one.
            "label": tpl.as_ref().and_then(|t| t.label.clone()),
            "tickets": doc.as_ref().map(|d| d.tickets.len()),
            "inputs": tpl.as_ref().map(workflow::inputs_json),
            // The steps, in order, from the canonical render: each
            // title (input names stand in for values) and the agent
            // expression — an input name when the ticket's `agent:` is
            // exactly `{{input}}`, which is how the board maps a run's
            // owners back to the team inputs.
            "steps": doc.as_ref().map(|d| d.tickets.iter().map(|t| json!({
                "title": t.title,
                "agent": t.agent,
                "size": t.size,
            })).collect::<Vec<_>>()),
            "distinct": tpl.as_ref().map(|t| t.distinct.clone()),
            "uses": workflow_slots(&text).unwrap_or_default(),
        }));
    }
    let record = read_record(&pm.dir, project_key, name)?;
    out["summary"] = json!(manifest.summary);
    out["guide"] = json!(manifest.guide);
    out["workflows"] = json!(workflows);
    out["rubrics"] = json!(rubrics);
    out["record"] = serde_json::to_value(&record).unwrap_or(Value::Null);
    Ok(out)
}

/// `plan propose --workflow <app>/<wf>` resolution — the daemon's read:
/// the app must be installed (record included), its current digest must
/// match the operator's recorded approval (`app_unapproved` otherwise),
/// then the workflow renders exactly as a stored one would.
pub fn plan_text(
    pm_dir: &Path,
    app_approvals: &std::collections::HashMap<String, Value>,
    project: &str,
    app: &str,
    wf: &str,
    provided: &BTreeMap<String, String>,
) -> Result<String> {
    let _ = app_dir(pm_dir, project, app)?;
    let _ = read_record(pm_dir, project, app)?;
    let text = read_workflow(pm_dir, project, app, wf)?;
    // The digest covers THIS buffer for the rendered workflow — the
    // bytes the operator approved are the bytes rendered, even if a
    // co-host writer flips the file after this one read (N2).
    let rel = format!("workflows/{wf}.md");
    let digest = digest_over(pm_dir, project, app, &[(rel.as_str(), text.as_str())])?;
    let ok = app_approvals
        .get(&approval_key(project, app))
        .and_then(|p| p["digest"].as_str())
        == Some(digest.as_str());
    if !ok {
        return Err(Error::invalid(
            "app_unapproved",
            format!(
                "app '{app}' in {project} is not approved for its current structure \
                 ({digest}) — a slot, binding, guide, rubric, template or workflow \
                 gate change resets it; the operator re-approves with `cadence app \
                 approve {app} --project {project}`"
            ),
        ));
    }
    workflow::render(&text, provided)
}

/// `app approve`'s view of the INSTALLED folder — the same strict scan
/// and checks a fresh source passes, so a hand edit after install
/// cannot smuggle content past the operator's review.
pub fn check_installed(
    pm_dir: &Path,
    project: &str,
    name: &str,
    agents: &HashSet<String>,
    agent_sources: &[String],
) -> Result<Vec<String>> {
    let dir = app_dir(pm_dir, project, name)?;
    let _ = read_record(pm_dir, project, name)?;
    Ok(validate(&dir, agents, agent_sources)?.notes)
}

/// The board's New-run rows for `project`'s app workflows — one per
/// `apps/<app>/workflows/<wf>.md`, named `<app>/<wf>`, carrying the
/// APP's approval state (approval is whole-app; there is no
/// per-workflow approval). An app that fails its installed checks lists
/// its workflows with the error attached — the board shows why it
/// cannot run rather than hiding it.
pub fn board_rows(pm_dir: &Path, project: &str, state_dir: &Path) -> Vec<Value> {
    let mut rows = Vec::new();
    let dir = pm_dir.join(project).join(DIR);
    if dir.symlink_metadata().is_ok_and(|m| m.is_symlink()) {
        return rows;
    }
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return rows;
    };
    let approvals = fetch_approvals(state_dir);
    let daemon_agents = daemon_aliases(state_dir);
    let (agents, agent_sources) = workflow::known_agents(pm_dir, Some(project), &daemon_agents);
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let Ok(ft) = e.file_type() else {
            continue;
        };
        if !ft.is_dir() || !model::valid_tag(&name) {
            continue;
        }
        let checked = check_installed(pm_dir, project, &name, &agents, &agent_sources)
            .map_err(|e| e.to_string());
        let app_digest = digest(pm_dir, project, &name);
        let approved = match &app_digest {
            Ok(d) => match &approvals {
                Some(a) => json!(approved(project, &name, d, Some(a))),
                None => json!("unknown — daemon unreachable"),
            },
            Err(_) => Value::Null,
        };
        // The listing goes through bundle_files — the same no-links walk
        // install and approve enforce — so a symlinked workflows/ dir (or
        // any planted link) becomes the app's error row instead of
        // leaking the foreign dir's file names onto the board (N1).
        match bundle_files(&e.path()) {
            Ok(files) => {
                for (rel, path) in &files {
                    let Some(stem) = rel
                        .strip_prefix("workflows/")
                        .and_then(|n| n.strip_suffix(".md"))
                    else {
                        continue;
                    };
                    // A file verified real at scan could still be swapped
                    // for a link before the read — re-check the leaf.
                    if path
                        .symlink_metadata()
                        .map(|m| !m.is_file())
                        .unwrap_or(true)
                    {
                        continue;
                    }
                    let text = std::fs::read_to_string(path).unwrap_or_default();
                    let (_, _, doc) = workflow::check_text(&text, &agents, &agent_sources);
                    let tpl = workflow::parse_template(&text).ok();
                    let mut row = json!({
                        "project": project,
                        "name": format!("{name}/{stem}"),
                        "app": name,
                        "title": doc.as_ref().map(|d| d.title.clone()),
                        "tickets": doc.map(|d| d.tickets.len()),
                        "inputs": tpl.as_ref().map(workflow::inputs_json),
                        "approved": approved,
                        "digest": app_digest.as_ref().ok(),
                    });
                    if let Err(why) = &checked {
                        row["error"] = json!(why);
                    }
                    rows.push(row);
                }
            }
            Err(why) => {
                // One row still names the broken app — the board shows
                // why it cannot run rather than hiding it.
                let mut row = json!({
                    "project": project,
                    "name": name,
                    "app": name,
                    "title": Value::Null,
                    "tickets": Value::Null,
                    "inputs": Value::Null,
                    "approved": approved,
                    "digest": app_digest.as_ref().ok(),
                });
                row["error"] = match &checked {
                    Err(checked_why) => json!(checked_why),
                    Ok(_) => json!(why.to_string()),
                };
                rows.push(row);
            }
        }
    }
    rows
}

/// `issue lint`'s view of `<pm>/<key>/apps/` — each `<name>/` is an app
/// folder with a parseable `app.md`, each `<name>.yaml` a well-formed
/// record that names it; anything else is a warning, a symlink an
/// error, like the workflows dir's own lint.
pub fn lint_dir(
    dir: &Path,
    project_key: &str,
    err: &mut dyn FnMut(String),
    warn: &mut dyn FnMut(String),
) {
    if dir.symlink_metadata().is_ok_and(|m| m.is_symlink()) {
        err(format!(
            "{project_key}/{DIR}: symlink — the board never follows links"
        ));
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut names: Vec<String> = Vec::new();
    let mut records: Vec<String> = Vec::new();
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if e.file_type().map(|t| t.is_symlink()).unwrap_or(false) {
            err(format!(
                "{project_key}/{DIR}/{name}: symlink — the board never follows links"
            ));
            continue;
        }
        if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            if !model::valid_tag(&name) {
                warn(format!(
                    "{project_key}/{DIR}/{name}: not an app name — [a-z0-9-], ≤32"
                ));
                continue;
            }
            names.push(name);
            continue;
        }
        if let Some(stem) = name.strip_suffix(".yaml") {
            records.push(stem.to_string());
            continue;
        }
        warn(format!(
            "{project_key}/{DIR}/{name}: not an app — apps/ holds <name>/ folders \
             and <name>.yaml records"
        ));
    }
    for name in &names {
        let app_path = dir.join(name);
        let md = app_path.join(MANIFEST);
        if md.symlink_metadata().is_ok_and(|m| m.is_symlink()) {
            err(format!(
                "{project_key}/{DIR}/{name}/app.md: symlink — the board never follows links"
            ));
            continue;
        }
        match std::fs::read_to_string(&md) {
            Err(_) => err(format!("{project_key}/{DIR}/{name}: no app.md")),
            Ok(text) => match parse_manifest(&text) {
                Err(e) => err(format!("{project_key}/{DIR}/{name}/app.md: {e}")),
                Ok(m) if m.app != *name => warn(format!(
                    "{project_key}/{DIR}/{name}/app.md: declares app '{}' — the folder \
                     and record name it",
                    m.app
                )),
                Ok(_) => {}
            },
        }
        if !records.iter().any(|r| r == name) {
            warn(format!(
                "{project_key}/{DIR}/{name}: no install record {name}.yaml — it was \
                 not installed by `cadence app install`"
            ));
        }
    }
    for stem in &records {
        if names.iter().any(|n| n == stem) {
            continue;
        }
        warn(format!(
            "{project_key}/{DIR}/{stem}.yaml: install record without its app folder"
        ));
    }
}

/// `cadence doctor`'s app report — per installed app, every declared
/// slot's binding health: `unbound` (explicitly unbound), `unknown` when
/// the connection name isn't registered (`known` is the daemon's
/// connection set — `None` when unreachable, reported as
/// "unavailable", never as "unknown connection"), plus slots bound
/// that the manifest no longer declares.
pub fn doctor(pm_dir: &Path, known: Option<&HashSet<String>>) -> Value {
    let mut rows = Vec::new();
    let Ok(projects) = project::list(pm_dir) else {
        return json!({"apps": [], "note": "no readable PM projects"});
    };
    for p in projects {
        let apps_dir = pm_dir.join(&p.key).join(DIR);
        if apps_dir.symlink_metadata().is_ok_and(|m| m.is_symlink()) {
            rows.push(json!({
                "project": p.key, "app": null,
                "error": "apps/ is a symlink — never followed",
            }));
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&apps_dir) else {
            continue;
        };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if !e.file_type().map(|t| t.is_dir()).unwrap_or(false) || !model::valid_tag(&name) {
                continue;
            }
            let (manifest, record) = match (
                read_manifest(pm_dir, &p.key, &name),
                read_record(pm_dir, &p.key, &name),
            ) {
                (Ok(m), Ok(r)) => (m, r),
                (a, b) => {
                    let why = a
                        .err()
                        .or(b.err())
                        .map(|e| e.to_string())
                        .unwrap_or_default();
                    rows.push(json!({"project": p.key, "app": name, "error": why}));
                    continue;
                }
            };
            let mut unbound = Vec::new();
            let mut unknown = Vec::new();
            let mut ok = Vec::new();
            for slot in &manifest.connections {
                match record.binding(slot) {
                    None => unbound.push(slot.clone()),
                    Some(conn) => match known {
                        None => ok.push(
                            json!({"slot": slot, "connection": conn, "verified": "unavailable"}),
                        ),
                        Some(known) if known.contains(conn) => {
                            ok.push(json!({"slot": slot, "connection": conn}))
                        }
                        Some(_) => unknown.push(json!({"slot": slot, "connection": conn})),
                    },
                }
            }
            let stray: Vec<&String> = record
                .bindings
                .keys()
                .filter(|k| !manifest.connections.iter().any(|s| s == *k))
                .collect();
            let mut row = json!({
                "project": p.key,
                "app": name,
                "slots_ok": ok,
                "unbound": unbound,
                "unknown_connection": unknown,
            });
            if !stray.is_empty() {
                row["stray_bindings"] = json!(stray);
            }
            if known.is_none() {
                row["connection_check"] = json!("unavailable — daemon unreachable");
            }
            rows.push(row);
        }
    }
    json!({"apps": rows, "count": rows.len()})
}

#[cfg(test)]
mod tests {
    use super::*;

    const APP_MD: &str = "---\napp: content-studio\ntitle: Content studio\nversion: 0.1.0\n\
needs:\n  connections: [publish]\n---\n\n# Guide\n\nHow to run the studio.\n";

    #[test]
    fn manifest_parses_fields_and_slots() {
        let m = parse_manifest(APP_MD).unwrap();
        assert_eq!(m.app, "content-studio");
        assert_eq!(m.title, "Content studio");
        assert_eq!(m.version, "0.1.0");
        assert_eq!(m.connections, vec!["publish".to_string()]);
        assert_eq!(m.summary, None, "no summary is fine");
        assert!(m.guide.contains("Guide"));
    }

    /// CAD-563: `summary:` is the optional one-line purpose the board
    /// shows — accepted and trimmed; a non-string or an oversize one
    /// refuses like every other manifest field.
    #[test]
    fn manifest_summary_is_optional_wording() {
        let with = APP_MD.replace(
            "version: 0.1.0",
            "version: 0.1.0\nsummary:  Get a post written, checked and published.  ",
        );
        let m = parse_manifest(&with).unwrap();
        assert_eq!(
            m.summary.as_deref(),
            Some("Get a post written, checked and published.")
        );
        // A blank summary reads as none — a `summary:` line left empty
        // is not a refusal.
        let blank = APP_MD.replace("version: 0.1.0", "version: 0.1.0\nsummary: '  '");
        assert_eq!(parse_manifest(&blank).unwrap().summary, None);
        for bad in [
            APP_MD.replace("version: 0.1.0", "version: 0.1.0\nsummary: [a, b]"),
            APP_MD.replace(
                "version: 0.1.0",
                &format!("version: 0.1.0\nsummary: '{}'", "x".repeat(161)),
            ),
        ] {
            assert!(parse_manifest(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn manifest_refuses_gated_and_unknown_keys() {
        for bad in [
            "---\napp: x\ntitle: t\nversion: v\nrecords: {}\n---\n\nbody\n",
            "---\napp: x\ntitle: t\nversion: v\nbogus: 1\n---\n\nbody\n",
            "---\napp: x\ntitle: t\nversion: v\nneeds: {other: [a]}\n---\n\nbody\n",
        ] {
            assert!(parse_manifest(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn manifest_requires_fields_and_good_names() {
        for bad in [
            "---\ntitle: t\nversion: v\n---\n\nbody\n",
            "---\napp: 'Bad Name'\ntitle: t\nversion: v\n---\n\nbody\n",
            "---\napp: x\ntitle: t\n---\n\nbody\n",
        ] {
            assert!(parse_manifest(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn split_ref_accepts_only_app_slash_workflow() {
        assert_eq!(split_ref("app/wf"), Some(("app", "wf")));
        assert_eq!(split_ref("wf"), None);
        assert_eq!(split_ref("a/b/c"), None);
        assert_eq!(split_ref("a/"), None);
        assert_eq!(split_ref("/b"), None);
        assert_eq!(split_ref("A/b"), None);
    }
}
