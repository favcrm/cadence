//! CAD-580: the wiki v1 store — the knowledge layer's durable tree.
//!
//! Layout under the vault root ([`vault_dir`] — `<pm>/wiki` today,
//! CAD-584's `<home>/vault` when the new layout lands):
//!
//! ```text
//! global/                     operator-written shared knowledge
//! projects/<key>/             a project team's pages (agents write
//!                             only their own project's)
//! agents/<alias>/knowledge/   the agent's own pages (self-written)
//! agents/<alias>/profile/     READ-ONLY view of <pm>/agents/<slug>/*
//!                             (writes go through agent_file_write)
//! agents/<alias>/memory/      READ-ONLY view of the memory store's
//!                             lessons, grouped by project
//! users/<handle>/             private to the named board user;
//!                             the operator administers
//! .trash/                     `wiki rm` landings (not API-addressable)
//! .blobs/                     content-addressed binary store
//! ```
//!
//! Text files live IN the tracker git: one commit per write with an
//! `Actor:` trailer and `if_rev` optimistic concurrency (the same
//! FNV-1a token [`crate::issue::write::issue_rev`] computes). Binary
//! files are content-addressed into `.blobs/` (gitignored); the
//! tracker carries a `<name>.blob` pointer file (`{sha256,size,mime,
//! name}`) so a blob page has history and shows in `ls`.
//!
//! The caller is never a request field — the daemon derives it from
//! the connection ([`Caller`]) and [`allowed`] — the ONE allowlist —
//! decides per path prefix. Every refusal names the rule.
//!
//! Path safety ([`normalize`]): NFC, then refuse absolute paths, `.`
//! and `..`, empty segments, `\`, control bytes, `%`, dot-prefixed
//! names (which also bars `.trash`/`.blobs`), and `.blob` suffixes
//! (pointer files are managed, never user-written). The filesystem
//! layer additionally refuses any symlinked component.

use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use unicode_normalization::UnicodeNormalization;

use crate::error::{Error, Result};
use crate::issue::Pm;

/// `if_rev` value meaning "the page does not exist yet" — a
/// create-only write. Any real content rev is `fnv1a:<16 hex>`.
pub const ABSENT: &str = "none";

/// Largest accepted `search` answer.
const SEARCH_CAP: usize = 200;
/// Largest accepted `history` answer.
const HISTORY_CAP: usize = 50;
/// Longest single path segment (bytes) — the filesystem's own cap.
const SEG_CAP: usize = 255;
/// Longest normalized path (bytes).
const PATH_CAP: usize = 2048;
/// Deepest a normalized path may nest.
const DEPTH_CAP: usize = 32;
/// Largest text page `read`/`write` will touch (64 MiB — generous,
/// still bounded).
const TEXT_CAP: u64 = 64 * 1024 * 1024;
/// Largest profile view file (matches master's cap order).
const VIEW_CAP: u64 = 1_048_576;

/// Who the daemon derived from the connection — never a request
/// field. `wiki_as` on an operator connection produces the same
/// variants for the board's relayed callers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Caller {
    /// Positive operator proof.
    Operator,
    /// A registered agent; `project` is its cwd's tracker project
    /// ([`crate::issue::project::key_for_cwd`]) — `None` when the cwd
    /// is no project's repo.
    Agent { alias: String, project: Option<String> },
    /// A named board session's handle (the board relays
    /// `user:<author>`).
    User(String),
    /// An unattributed board peer: reads the shared tree only.
    Public,
}

impl Caller {
    /// The `Actor:` trailer line this caller stamps — the same label
    /// `Actor: <who>` carries everywhere else.
    pub fn actor(&self) -> String {
        match self {
            Caller::Operator => "operator".to_string(),
            Caller::Agent { alias, .. } => alias.clone(),
            Caller::User(u) => format!("{u} (ui)"),
            Caller::Public => "anonymous (ui)".to_string(),
        }
    }
}

/// What an op wants to do — reads are `ls`/`read`/`search`/`history`;
/// everything that lands a commit is `Write`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Read,
    Write,
}

impl Op {
    fn as_str(self) -> &'static str {
        match self {
            Op::Read => "read",
            Op::Write => "write",
        }
    }
}

/// The ONE access allowlist: caller × op × path prefix. Everything
/// not listed is refused; every refusal names why. The table test at
/// the bottom of this file is its specification — a change to the
/// policy is a change to the table.
///
/// `segs` are normalized segments (empty = the root, for `ls`).
pub fn allowed(caller: &Caller, op: Op, segs: &[&str]) -> Result<()> {
    let verb = || {
        format!(
            "wiki {} '/{}' refused",
            op.as_str(),
            segs.join("/")
        )
    };
    let no = |why: &str| -> Result<()> {
        Err(Error::rejected(format!("{}: {why}", verb())))
    };
    let top = segs.first().copied().unwrap_or("");
    match top {
        "" => match op {
            Op::Read => Ok(()),
            Op::Write => no("the wiki root is not writable"),
        },
        "global" => match op {
            Op::Read => Ok(()),
            Op::Write => match caller {
                Caller::Operator => Ok(()),
                _ => no("global/ is operator-written; agents and users keep their own areas"),
            },
        },
        "projects" => {
            if segs.len() == 1 {
                return match op {
                    Op::Read => Ok(()),
                    Op::Write => no("projects/ itself is not writable — write inside a project"),
                };
            }
            match op {
                Op::Read => Ok(()),
                Op::Write => match caller {
                    Caller::Operator => Ok(()),
                    Caller::Agent { alias, project }
                        if project.as_deref() == Some(segs[1]) =>
                    {
                        let _ = alias;
                        Ok(())
                    }
                    Caller::Agent { project, .. } => no(&format!(
                        "agents write only their own project's folder (this agent's \
                         project: {})",
                        project.as_deref().unwrap_or("none — cwd is no project repo")
                    )),
                    _ => no("only the operator and the project's own agents write projects/"),
                },
            }
        }
        "agents" => {
            if segs.len() < 3 {
                // `agents/` and `agents/<a>/` — listing folders.
                return match op {
                    Op::Read => Ok(()),
                    Op::Write => {
                        no("agent folders are layout — write under knowledge/ instead")
                    }
                };
            }
            let area = segs[2];
            match area {
                "profile" => match op {
                    Op::Read => Ok(()),
                    Op::Write => no(
                        "profiles are a read-only view of the agent's files — write \
                         through `cadence master edit` (agent_file_write)",
                    ),
                },
                "memory" => match op {
                    Op::Read => Ok(()),
                    Op::Write => no(
                        "memory/ is a read-only view of the memory store — propose \
                         through `cadence memory`",
                    ),
                },
                "knowledge" => match op {
                    Op::Read => Ok(()),
                    Op::Write => match caller {
                        Caller::Operator => Ok(()),
                        Caller::Agent { alias, .. } if alias == segs[1] => Ok(()),
                        Caller::Agent { .. } => {
                            no("an agent writes only its own knowledge/")
                        }
                        _ => no("only the operator and the agent itself write knowledge/"),
                    },
                },
                _ => no(&format!(
                    "agents/<a>/ holds profile/, knowledge/ and memory/ — not '{area}/'"
                )),
            }
        }
        "users" => {
            if segs.len() == 1 {
                return match (op, caller) {
                    (Op::Read, Caller::Operator | Caller::User(_)) => Ok(()),
                    (Op::Read, _) => no("users/ is private — only a member sees own entry"),
                    (Op::Write, _) => no("users/ itself is not writable"),
                };
            }
            match caller {
                Caller::Operator => Ok(()),
                Caller::User(u) if u == segs[1] => Ok(()),
                Caller::User(_) => no("users/<u>/ is private to its member"),
                _ => no("users/<u>/ is private — the member and the operator may enter"),
            }
        }
        other => no(&format!(
            "'{other}/' is not a wiki root — the roots are global/, projects/<key>/, \
             agents/<alias>/ and users/<u>/"
        )),
    }
}

/// NFC-normalize and validate an API path. Returned form is
/// `a/b/c` (no leading or trailing `/`); the empty path is the root
/// and valid only for reads.
///
/// Refuses (each names its guard): absolute paths, `.`/`..` and empty
/// segments, `\`, NUL and control bytes, `%` (encoded-traversal bait —
/// a `%2F` here is a literal name, never a decoded slash), segments
/// starting `.` (which bars `.trash`, `.blobs`, `.git`, and dotfiles),
/// `.blob` suffixes (managed pointer files), and over-long/over-deep
/// paths.
pub fn normalize(path: &str) -> Result<String> {
    let bad = |why: &str| -> Result<String> {
        Err(Error::rejected(format!("wiki path '{path}' refused: {why}")))
    };
    if path.len() > PATH_CAP {
        return bad("path is too long");
    }
    if path.starts_with('/') {
        return bad("absolute paths are not allowed");
    }
    // NFC first so a decomposed `..` look-alike cannot slip past the
    // segment grammar, and a name stored NFC compares equal to the
    // same name typed NFD.
    let path: String = path.nfc().collect();
    if path.is_empty() {
        return Ok(String::new());
    }
    let mut segs = Vec::new();
    for seg in path.split('/') {
        if seg.is_empty() {
            return bad("empty segment ('//' or a trailing '/')");
        }
        if seg == "." || seg == ".." {
            return bad("'.' and '..' segments are not allowed");
        }
        if seg.starts_with('.') {
            return bad("dot-prefixed names are not allowed");
        }
        if seg.ends_with(".blob") {
            return bad("'.blob' pointer files are managed, never addressed");
        }
        if seg.len() > SEG_CAP {
            return bad("a path segment is over 255 bytes");
        }
        if seg.chars().any(|c| {
            c == '\\' || c == '%' || c.is_control() || c == '\u{7f}' || c == '\u{202e}'
        }) {
            return bad("segments carry no '\\', '%', control or bidi-override bytes");
        }
        segs.push(seg);
    }
    if segs.len() > DEPTH_CAP {
        return bad("path is too deep");
    }
    Ok(segs.join("/"))
}

/// The vault this tracker serves — [`crate::home::vault_dir`]'s layout
/// reconciled with the daemon's instance-bound tracker: under the new
/// `CADENCE_HOME` layout the vault is `<home>/vault` (the resolver's
/// answer); under the legacy layout it is `<pm.dir>/wiki` — what
/// `vault_dir` resolves whenever `pm.dir` is the env tracker, and the
/// only correct answer when a fixture daemon's `CADENCE_PM_DIR` lives
/// in its provider env rather than this process's.
pub fn vault_dir(pm: &Pm) -> Result<PathBuf> {
    match crate::home::layout()?.source {
        crate::home::Source::Env => crate::home::vault_dir(),
        crate::home::Source::Legacy => Ok(pm.dir.join("wiki")),
    }
}

/// Where blobs live under the vault.
pub fn blobs_dir(vault: &Path) -> PathBuf {
    crate::home::blobs_dir(vault)
}

/// `.trash/` under the vault — `rm` landings, never API-addressable.
fn trash_dir(vault: &Path) -> PathBuf {
    vault.join(".trash")
}

/// The tracker-relative form of a vault path — for git commits and
/// `git log`. `None` when the vault is not inside the tracker
/// (CAD-584's `<home>/vault`; commits then refuse — the tracker git
/// cannot see them — and the caller reports why).
fn git_rel(pm: &Pm, abs: &Path) -> Result<PathBuf> {
    abs.strip_prefix(&pm.dir).map(PathBuf::from).map_err(|_| {
        Error::internal(format!(
            "wiki path {} is outside tracker {}",
            abs.display(),
            pm.dir.display()
        ))
    })
}

/// Resolve a normalized path inside the vault, refusing symlinked
/// components — every existing ancestor and the leaf itself must be
/// a real directory entry. A missing leaf is fine (a write creates
/// it); a missing ancestor is fine (mkdir -p at write).
fn resolve(vault: &Path, norm: &str) -> Result<PathBuf> {
    if vault.symlink_metadata().is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(Error::rejected(format!(
            "wiki vault {} is a symlink — refusing to resolve through it",
            vault.display()
        )));
    }
    let abs = vault.join(norm);
    // Walk the existing ancestors from the vault down; any symlinked
    // component — including a planted `projects/x` pointing out —
    // refuses before the path is touched.
    let mut cur = vault.to_path_buf();
    for seg in Path::new(norm).components() {
        cur = cur.join(seg.as_os_str());
        match cur.symlink_metadata() {
            Ok(m) if m.file_type().is_symlink() => {
                return Err(Error::rejected(format!(
                    "wiki path '{norm}' resolves through a symlink — refused"
                )));
            }
            _ => {}
        }
    }
    Ok(abs)
}

/// Lazily create `.blobs/` and `.trash/`, and extend the tracker's
/// `.gitignore` with `wiki/.blobs/` when the vault lives inside the
/// tracker. Returns `Some(gitignore)` when this call modified it —
/// the caller folds the file into this write's commit.
fn ensure_layout(pm: &Pm) -> Result<Option<PathBuf>> {
    let vault = vault_dir(pm)?;
    std::fs::create_dir_all(blobs_dir(&vault))?;
    std::fs::create_dir_all(trash_dir(&vault))?;
    let gitignore = pm.dir.join(".gitignore");
    let want = vault
        .strip_prefix(&pm.dir)
        .ok()
        .map(|rel| format!("{}/.blobs/", rel.display()));
    let Some(line) = want else {
        return Ok(None);
    };
    let text = std::fs::read_to_string(&gitignore).unwrap_or_default();
    if text.lines().any(|l| l.trim() == line) {
        return Ok(None);
    }
    let mut next = text;
    if !next.is_empty() && !next.ends_with('\n') {
        next.push('\n');
    }
    next.push_str(&line);
    next.push('\n');
    std::fs::write(&gitignore, next)?;
    Ok(Some(gitignore))
}

/// Content rev of the file at `abs` — `fnv1a:<16 hex>` like
/// [`crate::issue::write::issue_rev`], or [`ABSENT`] when missing.
fn rev_of(abs: &Path) -> Result<String> {
    match std::fs::read(abs) {
        Ok(bytes) => Ok(rev_bytes(&bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(ABSENT.to_string()),
        Err(e) => Err(e.into()),
    }
}

fn rev_bytes(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("fnv1a:{h:016x}")
}

/// The path shape: a real vault path, or one of the two read-only
/// views. Checked after `normalize`+`allowed` — `agents/<a>/x/` never
/// reaches here with `x` outside the three areas.
#[derive(Debug, Clone, PartialEq, Eq)]
enum View {
    Real,
    Profile { alias: String },
    Memory { alias: String },
}

fn classify(segs: &[&str]) -> View {
    if segs.len() >= 3 && segs[0] == "agents" {
        match segs[2] {
            "profile" => return View::Profile { alias: segs[1].to_string() },
            "memory" => return View::Memory { alias: segs[1].to_string() },
            _ => {}
        }
    }
    View::Real
}

/// One `ls` entry.
#[derive(Debug, Clone)]
struct Entry {
    name: String,
    path: String,
    kind: &'static str,
    size: Option<u64>,
    mime: Option<String>,
}

impl Entry {
    fn json(&self) -> Value {
        json!({
            "name": self.name,
            "path": self.path,
            "kind": self.kind,
            "size": self.size,
            "mime": self.mime,
        })
    }
}

/// `agents/` dir names across both stores: the vault's `agents/`
/// (knowledge) plus `<pm>/agents/` (profile/memory sources).
fn agent_dirs(pm: &Pm, vault: &Path) -> Result<BTreeSet<String>> {
    let mut out = BTreeSet::new();
    for dir in [vault.join("agents"), pm.dir.join("agents")] {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        };
        for e in entries.flatten() {
            if e.file_type().is_ok_and(|t| t.is_dir() && !t.is_symlink()) {
                if let Some(name) = e.file_name().to_str() {
                    if !name.starts_with('.') {
                        out.insert(name.to_string());
                    }
                }
            }
        }
    }
    Ok(out)
}

/// Real-dir listing of `dir` (a vault dir): files, dirs and blob
/// pointers (collapsed to their logical names).
fn read_dir_entries(dir: &Path, base: &str) -> Result<Vec<Entry>> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e.into()),
    };
    for e in entries.flatten() {
        let Some(name) = e.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if name.starts_with('.') {
            continue;
        }
        let ft = match e.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        if ft.is_symlink() {
            continue;
        }
        let path = if base.is_empty() {
            name.clone()
        } else {
            format!("{base}/{name}")
        };
        if ft.is_dir() {
            seen.insert(name.clone());
            out.push(Entry {
                name,
                path,
                kind: "dir",
                size: None,
                mime: None,
            });
        } else if let Some(logical) = name.strip_suffix(".blob") {
            // A blob pointer file: the entry is the logical name.
            if logical.is_empty() || !seen.insert(logical.to_string()) {
                continue;
            }
            let meta = blob_meta(&e.path()).unwrap_or_default();
            out.push(Entry {
                name: logical.to_string(),
                path: if base.is_empty() {
                    logical.to_string()
                } else {
                    format!("{base}/{logical}")
                },
                kind: "blob",
                size: meta.get("size").and_then(Value::as_u64),
                mime: meta
                    .get("mime")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            });
        } else if ft.is_file() {
            seen.insert(name.clone());
            let size = e.metadata().ok().map(|m| m.len());
            out.push(Entry {
                name,
                path,
                kind: "text",
                size,
                mime: None,
            });
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// The JSON a `<name>.blob` pointer file carries.
fn blob_meta(pointer: &Path) -> Result<Value> {
    let text = std::fs::read_to_string(pointer)?;
    Ok(serde_json::from_str(&text).unwrap_or_else(|_| json!({})))
}

/// `wiki ls` — `path` a dir (or "" for the root). Entries are the
/// caller-visible names: blob pointer files appear once, under the
/// logical name.
pub fn ls(pm: &Pm, caller: &Caller, path: &str) -> Result<Value> {
    let norm = normalize(path)?;
    let segs: Vec<&str> = if norm.is_empty() {
        Vec::new()
    } else {
        norm.split('/').collect()
    };
    allowed(caller, Op::Read, &segs)?;
    let vault = vault_dir(pm)?;

    // The root: the fixed top-level folders.
    if segs.is_empty() {
        let entries: Vec<Value> = ["agents", "global", "projects", "users"]
            .iter()
            .map(|name| {
                Entry {
                    name: name.to_string(),
                    path: name.to_string(),
                    kind: "dir",
                    size: None,
                    mime: None,
                }
                .json()
            })
            .collect();
        return Ok(json!({"path": "", "entries": entries}));
    }

    match classify(&segs) {
        View::Profile { alias } => {
            // `agents/<a>/profile` → the live agent dir's files.
            let dir = pm.dir.join("agents").join(&alias);
            if segs.len() == 3 {
                let mut out = Vec::new();
                if let Ok(entries) = std::fs::read_dir(&dir) {
                    for e in entries.flatten() {
                        let Some(name) = e.file_name().to_str().map(str::to_string) else {
                            continue;
                        };
                        if name.starts_with('.') {
                            continue;
                        }
                        let Ok(meta) = e.path().symlink_metadata() else {
                            continue;
                        };
                        if meta.file_type().is_symlink() || !meta.is_file() {
                            continue;
                        }
                        let base = format!("agents/{alias}/profile");
                        out.push(
                            Entry {
                                name: name.clone(),
                                path: format!("{base}/{name}"),
                                kind: "text",
                                size: Some(meta.len()),
                                mime: None,
                            }
                            .json(),
                        );
                    }
                }
                return Ok(json!({"path": norm, "entries": out}));
            }
            return Err(Error::rejected(format!(
                "wiki ls '{norm}' refused: profile/ is flat — read the file itself"
            )));
        }
        View::Memory { alias } => {
            let base = format!("agents/{alias}/memory");
            if segs.len() == 3 {
                // Project dirs holding lessons this agent authored.
                let mut out = Vec::new();
                for p in memory_projects(pm, &alias)? {
                    out.push(
                        Entry {
                            path: format!("{base}/{p}"),
                            name: p,
                            kind: "dir",
                            size: None,
                            mime: None,
                        }
                        .json(),
                    );
                }
                return Ok(json!({"path": norm, "entries": out}));
            }
            if segs.len() == 4 {
                let project = segs[3];
                let mut out = Vec::new();
                for slug in memory_slugs(pm, &alias, project)? {
                    out.push(
                        Entry {
                            path: format!("{base}/{project}/{slug}.md"),
                            name: format!("{slug}.md"),
                            kind: "text",
                            size: None,
                            mime: None,
                        }
                        .json(),
                    );
                }
                return Ok(json!({"path": norm, "entries": out}));
            }
            return Err(Error::rejected(format!(
                "wiki ls '{norm}' refused: memory/ is <project>/<lesson>.md deep"
            )));
        }
        View::Real => {}
    }

    match segs[..] {
        [a] if a == "agents" => {
            let mut out = Vec::new();
            for alias in agent_dirs(pm, &vault)? {
                out.push(
                    Entry {
                        path: format!("agents/{alias}"),
                        name: alias,
                        kind: "dir",
                        size: None,
                        mime: None,
                    }
                    .json(),
                );
            }
            return Ok(json!({"path": norm, "entries": out}));
        }
        ["agents", alias] => {
            // The three layout areas always show — knowledge/ may not
            // exist in the vault yet but is part of the contract.
            let out: Vec<Value> = ["knowledge", "memory", "profile"]
                .iter()
                .map(|name| {
                    Entry {
                        name: name.to_string(),
                        path: format!("agents/{alias}/{name}"),
                        kind: "dir",
                        size: None,
                        mime: None,
                    }
                    .json()
                })
                .collect();
            return Ok(json!({"path": norm, "entries": out}));
        }
        ["users"] => {
            // Operator sees every member dir; a member sees only own;
            // allowed() already refused agents/public.
            let mut names = Vec::new();
            if let Ok(entries) = std::fs::read_dir(vault.join("users")) {
                for e in entries.flatten() {
                    if e.file_type().is_ok_and(|t| t.is_dir() && !t.is_symlink()) {
                        if let Some(name) = e.file_name().to_str() {
                            if !name.starts_with('.') {
                                names.push(name.to_string());
                            }
                        }
                    }
                }
            }
            if let Caller::User(u) = caller {
                names.retain(|n| n == u);
            }
            names.sort();
            let entries: Vec<Value> = names
                .into_iter()
                .map(|name| {
                    Entry {
                        path: format!("users/{name}"),
                        name,
                        kind: "dir",
                        size: None,
                        mime: None,
                    }
                    .json()
                })
                .collect();
            return Ok(json!({"path": norm, "entries": entries}));
        }
        _ => {}
    }

    let abs = resolve(&vault, &norm)?;
    match abs.symlink_metadata() {
        Ok(m) if m.is_dir() => {
            let entries = read_dir_entries(&abs, &norm)?
                .iter()
                .map(Entry::json)
                .collect::<Vec<_>>();
            Ok(json!({"path": norm, "entries": entries}))
        }
        Ok(m) if m.is_file() => Err(Error::rejected(format!(
            "wiki ls '{norm}' refused: a file, not a directory"
        ))),
        Ok(_) => Err(Error::rejected(format!(
            "wiki ls '{norm}' refused: not a directory"
        ))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(Error::rejected(format!(
            "wiki ls '{norm}': no such directory"
        ))),
        Err(e) => Err(e.into()),
    }
}

/// The memory store's project keys whose lessons `alias` authored.
fn memory_projects(pm: &Pm, alias: &str) -> Result<Vec<String>> {
    let mut out = BTreeSet::new();
    for m in crate::memory::load_all(&pm.dir)? {
        if m.front.author.as_deref() == Some(alias) {
            out.insert(m.project.clone());
        }
    }
    Ok(out.into_iter().collect())
}

/// Lesson slugs `alias` authored in `project`.
fn memory_slugs(pm: &Pm, alias: &str, project: &str) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for m in crate::memory::load_all(&pm.dir)? {
        if m.project == project && m.front.author.as_deref() == Some(alias) {
            out.push(m.front.id.clone());
        }
    }
    out.sort();
    Ok(out)
}

/// The memory record for `agents/<a>/memory/<proj>/<slug>.md`.
fn memory_file(pm: &Pm, alias: &str, project: &str, slug: &str) -> Result<PathBuf> {
    let slug = slug.strip_suffix(".md").unwrap_or(slug);
    for m in crate::memory::load_all(&pm.dir)? {
        if m.project == project && m.front.id == slug {
            if m.front.author.as_deref() != Some(alias) {
                return Err(Error::rejected(format!(
                    "wiki read 'agents/{alias}/memory/{project}/{slug}.md' refused: \
                     lesson '{slug}' is not {alias}'s (author: {})",
                    m.front.author.as_deref().unwrap_or("unknown")
                )));
            }
            return Ok(m.path.clone());
        }
    }
    Err(Error::rejected(format!(
        "wiki read 'agents/{alias}/memory/{project}/{slug}.md': no such lesson"
    )))
}

/// A profile view file's real path — `<pm>/agents/<alias>/<name>`,
/// symlink-refused, cap-checked.
fn profile_file(pm: &Pm, alias: &str, name: &str) -> Result<PathBuf> {
    let path = pm.dir.join("agents").join(alias).join(name);
    match path.symlink_metadata() {
        Ok(m) if m.file_type().is_symlink() => Err(Error::rejected(format!(
            "wiki profile '{alias}/{name}' is a symlink — agent files are never read \
             through links"
        ))),
        Ok(m) if m.is_file() => Ok(path),
        Ok(_) => Err(Error::rejected(format!(
            "wiki profile 'agents/{alias}/profile/{name}': not a file"
        ))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(Error::rejected(format!(
            "wiki read 'agents/{alias}/profile/{name}': no such file"
        ))),
        Err(e) => Err(e.into()),
    }
}

/// `wiki read` — text as `{kind:"text",text,rev}`; a blob page as
/// `{kind:"blob",sha256,size,mime,name,rev}` (rev = the pointer's);
/// a directory as `{kind:"dir"}`. View paths read their source.
pub fn read(pm: &Pm, caller: &Caller, path: &str) -> Result<Value> {
    let norm = normalize(path)?;
    if norm.is_empty() {
        return Err(Error::rejected("wiki read '' refused: the root is a directory"));
    }
    let segs: Vec<&str> = norm.split('/').collect();
    allowed(caller, Op::Read, &segs)?;
    let vault = vault_dir(pm)?;

    match classify(&segs) {
        View::Profile { alias } => {
            if segs.len() != 4 {
                return Err(Error::rejected(format!(
                    "wiki read '{norm}' refused: profile/ reads are profile/<file>"
                )));
            }
            let file = profile_file(pm, &alias, segs[3])?;
            let meta = std::fs::metadata(&file)?;
            if meta.len() > VIEW_CAP {
                return Err(Error::rejected(format!(
                    "wiki read '{norm}' refused: file over the {VIEW_CAP}-byte cap"
                )));
            }
            let text = std::fs::read_to_string(&file).map_err(|e| {
                Error::rejected(format!("wiki read '{norm}': {e}"))
            })?;
            return Ok(json!({
                "path": norm,
                "kind": "text",
                "text": text,
                "rev": rev_of(&file)?,
            }));
        }
        View::Memory { alias } => {
            if segs.len() != 5 {
                return Err(Error::rejected(format!(
                    "wiki read '{norm}' refused: memory/ reads are \
                     memory/<project>/<lesson>.md"
                )));
            }
            let file = memory_file(pm, &alias, segs[3], segs[4])?;
            let text = std::fs::read_to_string(&file).map_err(|e| {
                Error::rejected(format!("wiki read '{norm}': {e}"))
            })?;
            return Ok(json!({
                "path": norm,
                "kind": "text",
                "text": text,
                "rev": rev_of(&file)?,
            }));
        }
        View::Real => {}
    }

    let abs = resolve(&vault, &norm)?;
    match abs.symlink_metadata() {
        Ok(m) if m.is_dir() => Ok(json!({"path": norm, "kind": "dir"})),
        Ok(m) if m.is_file() => {
            if m.len() > TEXT_CAP {
                return Err(Error::rejected(format!(
                    "wiki read '{norm}' refused: file over the {TEXT_CAP}-byte cap"
                )));
            }
            let text = std::fs::read_to_string(&abs).map_err(|_| {
                Error::rejected(format!(
                    "wiki read '{norm}' refused: not UTF-8 text (is it a blob?)"
                ))
            })?;
            Ok(json!({
                "path": norm,
                "kind": "text",
                "text": text,
                "rev": rev_of(&abs)?,
            }))
        }
        Ok(_) => Err(Error::rejected(format!("wiki read '{norm}': not a file"))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // A blob page: <path>.blob holds the pointer.
            let pointer = vault.join(format!("{norm}.blob"));
            match pointer.symlink_metadata() {
                Ok(m) if m.is_file() && !m.file_type().is_symlink() => {
                    let meta = blob_meta(&pointer)?;
                    Ok(json!({
                        "path": norm,
                        "kind": "blob",
                        "sha256": meta.get("sha256").cloned().unwrap_or(Value::Null),
                        "size": meta.get("size").cloned().unwrap_or(Value::Null),
                        "mime": meta.get("mime").cloned().unwrap_or(Value::Null),
                        "name": meta.get("name").cloned().unwrap_or(Value::Null),
                        "rev": rev_of(&pointer)?,
                    }))
                }
                _ => Err(Error::rejected(format!("wiki read '{norm}': no such file"))),
            }
        }
        Err(e) => Err(e.into()),
    }
}

/// `wiki write` — a text page. `if_rev` is the optimistic-concurrency
/// token: `ABSENT` ("none") requires a missing page, a `fnv1a:` rev
/// must equal the current content's. Returns `{rev}` on success, or
/// `{conflict: "if_rev", current_rev}` on a stale token — never a
/// lost update.
pub fn write(
    pm: &Pm,
    caller: &Caller,
    path: &str,
    text: &str,
    if_rev: Option<&str>,
) -> Result<Value> {
    let norm = normalize(path)?;
    if norm.is_empty() {
        return Err(Error::rejected("wiki write '' refused: the root is a directory"));
    }
    let segs: Vec<&str> = norm.split('/').collect();
    allowed(caller, Op::Write, &segs)?;
    if let View::Real = classify(&segs) {
    } else {
        unreachable!("allowed() refused the views before here")
    }
    if text.len() as u64 > TEXT_CAP {
        return Err(Error::rejected(format!(
            "wiki write '{norm}' refused: text over the {TEXT_CAP}-byte cap"
        )));
    }
    let warnings = crate::secret::guard(&format!("wiki/{norm}"), text)?;
    let vault = vault_dir(pm)?;
    let abs = resolve(&vault, &norm)?;
    let _lock = pm.lock()?;
    let gitignore = ensure_layout(pm)?;

    let cur = rev_of(&abs)?;
    if let Some(want) = if_rev {
        if want != cur {
            return Ok(json!({"conflict": "if_rev", "current_rev": cur, "path": norm}));
        }
    }
    if abs.symlink_metadata().is_ok_and(|m| m.is_dir()) {
        return Err(Error::rejected(format!(
            "wiki write '{norm}' refused: a directory exists there"
        )));
    }
    if let Some(parent) = abs.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let before = std::fs::read(&abs).ok();
    let tmp = abs.with_extension("wiki.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, &abs)?;

    let mut paths = vec![abs.clone()];
    if let Some(g) = gitignore {
        paths.push(g);
    }
    let subject = format!("wiki: write {norm}");
    let actor = caller.actor();
    let committed = crate::issue::write::commit_who(
        pm,
        &paths,
        &subject,
        &[],
        &actor,
        None,
    );
    if let Err(e) = committed {
        restore(&abs, before);
        return Err(e);
    }
    let mut out = json!({
        "path": norm,
        "rev": rev_of(&abs)?,
        "committed": true,
        "foreign_files": committed.unwrap_or_default(),
    });
    if !warnings.is_empty() {
        out["secret_warnings"] = crate::secret::warnings_json(&warnings);
    }
    Ok(out)
}

/// Put `before` back at `abs` (or remove what this write created) —
/// a refused commit leaves neither an index entry nor a half-write.
fn restore(abs: &Path, before: Option<Vec<u8>>) {
    match before {
        Some(bytes) => {
            let _ = std::fs::write(abs, bytes);
        }
        None => {
            let _ = std::fs::remove_file(abs);
        }
    }
}

/// `wiki mkdir` — creates the dir plus a `.gitkeep` (invisible to
/// `ls`; git cannot track an empty dir) and commits it.
pub fn mkdir(pm: &Pm, caller: &Caller, path: &str) -> Result<Value> {
    let norm = normalize(path)?;
    if norm.is_empty() {
        return Err(Error::rejected("wiki mkdir '' refused: the root exists"));
    }
    let segs: Vec<&str> = norm.split('/').collect();
    allowed(caller, Op::Write, &segs)?;
    if classify(&segs) != View::Real {
        unreachable!()
    }
    let vault = vault_dir(pm)?;
    let abs = resolve(&vault, &norm)?;
    let _lock = pm.lock()?;
    let gitignore = ensure_layout(pm)?;
    match abs.symlink_metadata() {
        Ok(m) if m.is_dir() => return Ok(json!({"path": norm, "created": false})),
        Ok(_) => {
            return Err(Error::rejected(format!(
                "wiki mkdir '{norm}' refused: a file exists there"
            )))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    std::fs::create_dir_all(&abs)?;
    let keep = abs.join(".gitkeep");
    std::fs::write(&keep, "")?;
    let mut paths = vec![keep];
    if let Some(g) = gitignore {
        paths.push(g);
    }
    let actor = caller.actor();
    crate::issue::write::commit_who(
        pm,
        &paths,
        &format!("wiki: mkdir {norm}"),
        &[],
        &actor,
        None,
    )?;
    Ok(json!({"path": norm, "created": true}))
}

/// `wiki mv` — rename a page, dir or blob page. Both endpoints pass
/// the write ACL; the destination must not exist. A `from` under
/// `.trash/` restores — operator only (trash is operator-managed).
pub fn mv(pm: &Pm, caller: &Caller, from: &str, to: &str) -> Result<Value> {
    let norm_to = normalize(to)?;
    if norm_to.is_empty() {
        return Err(Error::rejected("wiki mv refused: destination is the root"));
    }
    let segs_to: Vec<&str> = norm_to.split('/').collect();
    let vault = vault_dir(pm)?;

    // A `.trash/<stamp>/<orig>` source is a restore: operator only.
    let restoring = from.starts_with(".trash/");
    let (src_abs, from_label) = if restoring {
        if !matches!(caller, Caller::Operator) {
            return Err(Error::rejected(
                "wiki mv refused: restoring from .trash/ is the operator's",
            ));
        }
        let rel = from.trim_start_matches(".trash/");
        let norm_rel = normalize(rel)?;
        if norm_rel.is_empty() {
            return Err(Error::rejected("wiki mv refused: empty trash path"));
        }
        (
            trash_dir(&vault).join(&norm_rel),
            format!(".trash/{norm_rel}"),
        )
    } else {
        let norm_from = normalize(from)?;
        if norm_from.is_empty() {
            return Err(Error::rejected("wiki mv refused: source is the root"));
        }
        let segs_from: Vec<&str> = norm_from.split('/').collect();
        allowed(caller, Op::Write, &segs_from)?;
        if classify(&segs_from) != View::Real {
            unreachable!()
        }
        (resolve(&vault, &norm_from)?, norm_from)
    };
    allowed(caller, Op::Write, &segs_to)?;
    if classify(&segs_to) != View::Real {
        unreachable!()
    }
    let dst_abs = resolve(&vault, &norm_to)?;

    // Never move a dir into itself or its own descendant.
    if dst_abs.starts_with(&src_abs) {
        return Err(Error::rejected(format!(
            "wiki mv '{from_label}' → '{norm_to}' refused: cannot move a directory \
             into itself"
        )));
    }
    match src_abs.symlink_metadata() {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // A blob page moves its pointer file.
            let pointer = vault.join(format!("{from_label}.blob"));
            if !restoring && pointer.is_file() {
                return mv_real(pm, caller, &pointer, &vault.join(format!("{norm_to}.blob")), &from_label, &norm_to);
            }
            return Err(Error::rejected(format!(
                "wiki mv '{from_label}': no such page"
            )));
        }
        Err(e) => return Err(e.into()),
    }
    mv_real(pm, caller, &src_abs, &dst_abs, &from_label, &norm_to)
}

fn mv_real(
    pm: &Pm,
    caller: &Caller,
    src: &Path,
    dst: &Path,
    from_label: &str,
    to: &str,
) -> Result<Value> {
    let vault = vault_dir(pm)?;
    let dst_pointer = vault.join(format!("{to}.blob"));
    if dst.symlink_metadata().is_ok() || dst_pointer.symlink_metadata().is_ok() {
        return Err(Error::rejected(format!(
            "wiki mv '{from_label}' → '{to}' refused: destination exists"
        )));
    }
    let _lock = pm.lock()?;
    let gitignore = ensure_layout(pm)?;
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::rename(src, dst)?;
    let mut paths = vec![src.to_path_buf(), dst.to_path_buf()];
    if let Some(g) = gitignore {
        paths.push(g);
    }
    let actor = caller.actor();
    if let Err(e) = crate::issue::write::commit_who(
        pm,
        &paths,
        &format!("wiki: mv {from_label} → {to}"),
        &[],
        &actor,
        None,
    ) {
        let _ = std::fs::rename(dst, src);
        return Err(e);
    }
    Ok(json!({"from": from_label, "to": to, "moved": true}))
}

/// `wiki rm` — move a page, dir or blob page to
/// `.trash/<stamp>-<nonce>/<orig>`, returning the restore path a
/// `wiki mv` from it takes. The blob bytes stay in `.blobs/`
/// (content-addressed; unreferenced blobs are GC work, not this op).
pub fn rm(pm: &Pm, caller: &Caller, path: &str) -> Result<Value> {
    let norm = normalize(path)?;
    if norm.is_empty() {
        return Err(Error::rejected("wiki rm '' refused: the root stays"));
    }
    let segs: Vec<&str> = norm.split('/').collect();
    allowed(caller, Op::Write, &segs)?;
    if classify(&segs) != View::Real {
        unreachable!()
    }
    let vault = vault_dir(pm)?;
    let abs = resolve(&vault, &norm)?;
    // What moves: the page itself, or its blob pointer file.
    let src = match abs.symlink_metadata() {
        Ok(_) => abs,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let pointer = vault.join(format!("{norm}.blob"));
            match pointer.symlink_metadata() {
                Ok(m) if m.is_file() && !m.file_type().is_symlink() => pointer,
                _ => return Err(Error::rejected(format!("wiki rm '{norm}': no such page"))),
            }
        }
        Err(e) => return Err(e.into()),
    };
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let nonce = &uuid::Uuid::new_v4().simple().to_string()[..8];
    let rel = src.strip_prefix(&vault).unwrap_or(&src);
    let dst = trash_dir(&vault)
        .join(format!("{stamp}-{nonce}"))
        .join(rel);
    let restore = format!(".trash/{stamp}-{nonce}/{}", rel.display());
    let _lock = pm.lock()?;
    let gitignore = ensure_layout(pm)?;
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::rename(&src, &dst)?;
    let mut paths = vec![src.clone(), dst.clone()];
    if let Some(g) = gitignore {
        paths.push(g);
    }
    let actor = caller.actor();
    if let Err(e) = crate::issue::write::commit_who(
        pm,
        &paths,
        &format!("wiki: rm {norm} → {restore}"),
        &[],
        &actor,
        None,
    ) {
        let _ = std::fs::rename(&dst, &src);
        return Err(e);
    }
    Ok(json!({"path": norm, "trash": restore, "removed": true}))
}

/// The daemon's name for an accepted upload's staging dir —
/// `<state>/wiki-uploads/`. `put_blob` accepts a `tmp` only under it.
pub const UPLOAD_DIR: &str = "wiki-uploads";

/// sha256 of a file, lowercase hex.
fn sha256_file(path: &Path) -> Result<String> {
    use sha2::Digest;
    let mut f = std::fs::File::open(path)?;
    let mut h = sha2::Sha256::new();
    std::io::copy(&mut f, &mut h)?;
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// A first-block MIME guess: magic bytes for the kinds the board
/// streams; UTF-8 → text/plain; else octet-stream. The pointer file
/// records it; the board's serve rules decide inline vs attachment.
fn sniff_mime(head: &[u8]) -> &'static str {
    match head {
        [0x89, b'P', b'N', b'G', ..] => "image/png",
        [0xff, 0xd8, 0xff, ..] => "image/jpeg",
        [b'G', b'I', b'F', b'8', ..] => "image/gif",
        [b'R', b'I', b'F', b'F', ..] if head.len() >= 12 && &head[8..12] == b"WEBP" => {
            "image/webp"
        }
        [b'%', b'P', b'D', b'F', ..] => "application/pdf",
        [b'P', b'K', 0x03, 0x04, ..] => "application/zip",
        [0x00, 0x00, 0x00, ..] if head.len() >= 12 && &head[4..8] == b"ftyp" => "video/mp4",
        [0x1a, 0x45, 0xdf, 0xa3, ..] => "video/webm",
        _ => {
            if std::str::from_utf8(head).is_ok() {
                let text = std::str::from_utf8(head).unwrap_or_default();
                let lower = text.trim_start().to_lowercase();
                if lower.starts_with("<?xml") && lower.contains("<svg")
                    || lower.starts_with("<svg")
                {
                    "image/svg+xml"
                } else if lower.starts_with("<!doctype html") || lower.starts_with("<html") {
                    "text/html"
                } else {
                    "text/plain"
                }
            } else {
                "application/octet-stream"
            }
        }
    }
}

/// `wiki_put_blob` — verify a staged upload and land it: re-hash the
/// tmp file (the caller's `sha256` is advisory and must match),
/// enforce the cap BEFORE the move, sniff MIME, secret-scan text
/// content, then move the file into `.blobs/<sha256>` and commit the
/// `<path>.blob` pointer. The tracker write lock is taken only for
/// the pointer's commit — never while the upload is being checked.
///
/// `tmp` must sit under `<state_dir>/wiki-uploads/` — a tmp anywhere
/// else is refused (the daemon never renames an arbitrary caller
/// path into the vault).
pub fn put_blob(
    pm: &Pm,
    state_dir: &Path,
    caller: &Caller,
    path: &str,
    tmp: &Path,
    sha256: Option<&str>,
) -> Result<Value> {
    let norm = normalize(path)?;
    if norm.is_empty() {
        return Err(Error::rejected("wiki put_blob '' refused: the root is a directory"));
    }
    let segs: Vec<&str> = norm.split('/').collect();
    allowed(caller, Op::Write, &segs)?;
    if classify(&segs) != View::Real {
        unreachable!()
    }
    let vault = vault_dir(pm)?;

    // The tmp must live under <state>/wiki-uploads/ — canonicalized
    // so `..` and symlink tricks cannot escape the check.
    let uploads = state_dir.join(UPLOAD_DIR);
    let uploads_canon = uploads
        .canonicalize()
        .unwrap_or_else(|_| uploads.clone());
    let tmp_canon = tmp
        .canonicalize()
        .map_err(|e| Error::rejected(format!("wiki put_blob tmp {}: {e}", tmp.display())))?;
    if tmp_canon.parent() != Some(uploads_canon.as_path()) {
        return Err(Error::rejected(format!(
            "wiki put_blob refused: tmp must be a file directly under {}",
            uploads.display()
        )));
    }
    let meta = tmp_canon.symlink_metadata()?;
    if !meta.is_file() || meta.file_type().is_symlink() {
        return Err(Error::rejected("wiki put_blob refused: tmp is not a regular file"));
    }
    let cap = pm.config.wiki.max_upload_bytes;
    if meta.len() > cap {
        let _ = std::fs::remove_file(&tmp_canon);
        return Err(Error::rejected(format!(
            "wiki put_blob '{norm}' refused: {} bytes over the {cap}-byte cap",
            meta.len()
        )));
    }
    let actual = sha256_file(&tmp_canon)?;
    if let Some(want) = sha256 {
        if !want.eq_ignore_ascii_case(&actual) {
            let _ = std::fs::remove_file(&tmp_canon);
            return Err(Error::rejected(format!(
                "wiki put_blob '{norm}' refused: sha256 mismatch (got {actual})"
            )));
        }
    }
    // MIME sniff + secret scan on text content. Read a bounded head
    // for sniffing; the scan runs on the full text when it is one.
    let mut head = vec![0u8; 8192.min(meta.len() as usize)];
    std::fs::File::open(&tmp_canon)?.read_exact(&mut head)?;
    let mime = sniff_mime(&head);
    let mut secret_warnings = Vec::new();
    if mime == "text/plain" && meta.len() <= TEXT_CAP {
        if let Ok(text) = std::fs::read_to_string(&tmp_canon) {
            secret_warnings = crate::secret::guard(&format!("wiki/{norm}"), &text)?;
        }
    }

    // Land the blob — rename within the vault's filesystem, else
    // copy+unlink across mounts (state dir and vault may differ).
    ensure_layout(pm)?;
    let dest = blobs_dir(&vault).join(&actual);
    if !dest.exists() {
        match std::fs::rename(&tmp_canon, &dest) {
            Ok(()) => {}
            Err(_) => {
                std::fs::copy(&tmp_canon, &dest)?;
                let _ = std::fs::remove_file(&tmp_canon);
            }
        }
    } else {
        let _ = std::fs::remove_file(&tmp_canon);
    }

    // The pointer file — a normal tracker text write under the lock.
    let pointer = vault.join(format!("{norm}.blob"));
    let _lock = pm.lock()?;
    let gitignore = ensure_layout(pm)?;
    if let Some(parent) = pointer.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if resolve(&vault, &norm)?.symlink_metadata().is_ok() {
        // A text page already sits at the logical name.
        let _ = std::fs::remove_file(&dest);
        return Err(Error::rejected(format!(
            "wiki put_blob '{norm}' refused: a text page exists there — rm it first"
        )));
    }
    let name = norm.rsplit('/').next().unwrap_or(&norm).to_string();
    let pointer_text = serde_json::to_string_pretty(&json!({
        "sha256": actual,
        "size": meta.len(),
        "mime": mime,
        "name": name,
    }))?;
    let before = std::fs::read(&pointer).ok();
    std::fs::write(&pointer, &pointer_text)?;
    let mut paths = vec![pointer.clone()];
    if let Some(g) = gitignore {
        paths.push(g);
    }
    let actor = caller.actor();
    if let Err(e) = crate::issue::write::commit_who(
        pm,
        &paths,
        &format!("wiki: put_blob {norm}"),
        &[],
        &actor,
        None,
    ) {
        restore(&pointer, before);
        return Err(e);
    }
    let mut out = json!({
        "path": norm,
        "kind": "blob",
        "sha256": actual,
        "size": meta.len(),
        "mime": mime,
        "name": name,
        "rev": rev_of(&pointer)?,
    });
    if !secret_warnings.is_empty() {
        out["secret_warnings"] = crate::secret::warnings_json(&secret_warnings);
    }
    Ok(out)
}

/// `wiki search` — case-insensitive substring match over the vault's
/// text files under `base` ("" = everywhere), each match ACL-checked
/// for this caller. Pointer files and dot-dirs never match.
pub fn search(pm: &Pm, caller: &Caller, q: &str, base: &str) -> Result<Value> {
    if q.is_empty() {
        return Err(Error::rejected("wiki search refused: empty query"));
    }
    let norm = normalize(base)?;
    let segs: Vec<&str> = if norm.is_empty() {
        Vec::new()
    } else {
        norm.split('/').collect()
    };
    allowed(caller, Op::Read, &segs)?;
    let vault = vault_dir(pm)?;
    let root = if segs.is_empty() {
        vault.clone()
    } else {
        resolve(&vault, &norm)?
    };
    let needle = q.to_lowercase();
    let mut matches = Vec::new();
    let mut stack = vec![root];
    while let Some(dir) = stack.pop() {
        if matches.len() >= SEARCH_CAP {
            break;
        }
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for e in entries.flatten() {
            if matches.len() >= SEARCH_CAP {
                break;
            }
            let path = e.path();
            let Ok(meta) = path.symlink_metadata() else {
                continue;
            };
            if meta.file_type().is_symlink() {
                continue;
            }
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            if meta.is_dir() {
                stack.push(path);
                continue;
            }
            if !meta.is_file() || name.ends_with(".blob") {
                continue;
            }
            let rel = match path.strip_prefix(&vault) {
                Ok(r) => r.to_string_lossy().replace('\\', "/"),
                Err(_) => continue,
            };
            let rel_segs: Vec<&str> = rel.split('/').collect();
            if allowed(caller, Op::Read, &rel_segs).is_err() {
                continue;
            }
            if meta.len() > TEXT_CAP {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            for (i, line) in text.lines().enumerate() {
                if line.to_lowercase().contains(&needle) {
                    matches.push(json!({
                        "path": rel,
                        "line": i + 1,
                        "text": line.trim(),
                    }));
                    if matches.len() >= SEARCH_CAP {
                        break;
                    }
                }
            }
        }
    }
    matches.sort_by(|a, b| {
        a["path"]
            .as_str()
            .unwrap_or("")
            .cmp(b["path"].as_str().unwrap_or(""))
            .then(a["line"].as_u64().cmp(&b["line"].as_u64()))
    });
    Ok(json!({"q": q, "base": norm, "matches": matches}))
}

/// `wiki history` — `git log` for a page, mapped to its tracker
/// path: real pages `wiki/<path>` (plus a blob's pointer), profile
/// views `agents/<a>/<file>`, memory views `<proj>/memory/<slug>.md`.
pub fn history(pm: &Pm, caller: &Caller, path: &str, limit: usize) -> Result<Value> {
    let norm = normalize(path)?;
    if norm.is_empty() {
        return Err(Error::rejected("wiki history '' refused: the root has no file log"));
    }
    let segs: Vec<&str> = norm.split('/').collect();
    allowed(caller, Op::Read, &segs)?;
    let vault = vault_dir(pm)?;
    let rels: Vec<PathBuf> = match classify(&segs) {
        View::Profile { alias } => {
            if segs.len() != 4 {
                return Err(Error::rejected(format!(
                    "wiki history '{norm}' refused: profile logs are profile/<file>"
                )));
            }
            vec![PathBuf::from("agents").join(&alias).join(segs[3])]
        }
        View::Memory { alias } => {
            if segs.len() != 5 {
                return Err(Error::rejected(format!(
                    "wiki history '{norm}' refused: memory logs are \
                     memory/<project>/<lesson>.md"
                )));
            }
            let file = memory_file(pm, &alias, segs[3], segs[4])?;
            vec![file.strip_prefix(&pm.dir).unwrap_or(&file).to_path_buf()]
        }
        View::Real => {
            // The page and its pointer: a blob's log rides the pointer
            // file; a text page's rides the file itself. Either or both
            // may exist — `git log` over both keeps a rm'd page's story.
            vec![
                git_rel(pm, &resolve(&vault, &norm)?)?,
                git_rel(pm, &vault.join(format!("{norm}.blob")))?,
            ]
        }
    };
    let mut args: Vec<std::ffi::OsString> = vec![
        "-C".into(),
        pm.dir.clone().into_os_string(),
        "log".into(),
        format!("-{}", limit.min(HISTORY_CAP)).into(),
        "-z".into(),
        "--format=%H%x1f%at%x1f%B%x1e".into(),
        "--".into(),
    ];
    for rel in &rels {
        args.push(rel.clone().into_os_string());
    }
    let out = Command::new("git").args(&args).output()?;
    if !out.status.success() {
        return Err(Error::internal(format!(
            "git log failed: {}",
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut commits = Vec::new();
    for rec in text.split('\u{1e}') {
        let rec = rec.trim_start_matches('\0');
        if rec.is_empty() {
            continue;
        }
        let mut parts = rec.splitn(3, '\u{1f}');
        let (sha, at, body) = (
            parts.next().unwrap_or("").to_string(),
            parts.next().unwrap_or("0").to_string(),
            parts.next().unwrap_or(""),
        );
        let actor = body
            .lines()
            .find_map(|l| l.strip_prefix("Actor: "))
            .unwrap_or("")
            .to_string();
        let subject = body.lines().next().unwrap_or("").to_string();
        commits.push(json!({
            "sha": sha,
            "at": at.parse::<u64>().unwrap_or(0),
            "subject": subject,
            "actor": actor,
        }));
    }
    Ok(json!({"path": norm, "commits": commits}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segs(path: &str) -> Vec<&str> {
        if path.is_empty() {
            Vec::new()
        } else {
            path.split('/').collect()
        }
    }

    fn agent() -> Caller {
        Caller::Agent {
            alias: "swe-1".to_string(),
            project: Some("cadence".to_string()),
        }
    }

    fn agentless() -> Caller {
        Caller::Agent {
            alias: "swe-1".to_string(),
            project: None,
        }
    }

    /// The allowlist, row by row: every (caller, op, path) the policy
    /// names. Each row pins a decision — a policy change edits THIS
    /// table, and a missing row is a gap the test catches.
    #[test]
    fn allowlist_table() {
        let op = Caller::Operator;
        let public = Caller::Public;
        let user = Caller::User("fable".to_string());
        let user2 = Caller::User("other".to_string());
        // (caller, op, path, allow?)
        let table: &[(&Caller, Op, &str, bool)] = &[
            // The root: listable, never writable.
            (&op, Op::Read, "", true),
            (&public, Op::Read, "", true),
            (&op, Op::Write, "", false),
            // global/: read by all, written by the operator only.
            (&op, Op::Read, "global/notes.md", true),
            (&agent(), Op::Read, "global/notes.md", true),
            (&public, Op::Read, "global/notes.md", true),
            (&user, Op::Read, "global/notes.md", true),
            (&op, Op::Write, "global/notes.md", true),
            (&agent(), Op::Write, "global/notes.md", false),
            (&user, Op::Write, "global/notes.md", false),
            (&public, Op::Write, "global/notes.md", false),
            // projects/<key>/: all read; write = operator + owning agent.
            (&op, Op::Read, "projects/cadence/a.md", true),
            (&public, Op::Read, "projects/cadence/a.md", true),
            (&agent(), Op::Write, "projects/cadence/a.md", true),
            (&agent(), Op::Write, "projects/other/a.md", false),
            (&agentless(), Op::Write, "projects/cadence/a.md", false),
            (&user, Op::Write, "projects/cadence/a.md", false),
            (&public, Op::Write, "projects/cadence/a.md", false),
            (&op, Op::Write, "projects", false),
            (&agent(), Op::Read, "projects", true),
            // agents/<a>/profile/: read by all; written by NOBODY via
            // the wiki — profile writes go through agent_file_write.
            (&public, Op::Read, "agents/swe-1/profile/SOUL.md", true),
            (&agent(), Op::Read, "agents/other/profile/AGENT.md", true),
            (&op, Op::Write, "agents/swe-1/profile/SOUL.md", false),
            (&agent(), Op::Write, "agents/swe-1/profile/SOUL.md", false),
            // agents/<a>/memory/: read-only view for everyone.
            (&public, Op::Read, "agents/swe-1/memory/cadence/x.md", true),
            (&op, Op::Write, "agents/swe-1/memory/cadence/x.md", false),
            (&agent(), Op::Write, "agents/swe-1/memory/cadence/x.md", false),
            // agents/<a>/knowledge/: all read; self + operator write.
            (&public, Op::Read, "agents/swe-1/knowledge/n.md", true),
            (&agent(), Op::Read, "agents/other/knowledge/n.md", true),
            (&agent(), Op::Write, "agents/swe-1/knowledge/n.md", true),
            (&agent(), Op::Write, "agents/swe-2/knowledge/n.md", false),
            (&user, Op::Write, "agents/swe-1/knowledge/n.md", false),
            (&op, Op::Write, "agents/swe-1/knowledge/n.md", true),
            // unknown agents/<a>/ subdirs refuse outright.
            (&agent(), Op::Read, "agents/swe-1/secrets/x", false),
            (&op, Op::Write, "agents/swe-1/secrets/x", false),
            // users/<u>/: private — the member and the operator only.
            (&op, Op::Read, "users", true),
            (&user, Op::Read, "users", true),
            (&agent(), Op::Read, "users", false),
            (&public, Op::Read, "users", false),
            (&op, Op::Read, "users/fable/diary.md", true),
            (&user, Op::Read, "users/fable/diary.md", true),
            (&user, Op::Write, "users/fable/diary.md", true),
            (&user2, Op::Read, "users/fable/diary.md", false),
            (&user2, Op::Write, "users/fable/diary.md", false),
            (&agent(), Op::Read, "users/fable/diary.md", false),
            (&agent(), Op::Write, "users/fable/diary.md", false),
            (&public, Op::Read, "users/fable/diary.md", false),
            // Unknown roots refuse — for every caller, both ops.
            (&op, Op::Read, "etc/passwd", false),
            (&op, Op::Write, "etc/passwd", false),
            (&agent(), Op::Read, "random/x", false),
            (&public, Op::Read, "wiki/x", false),
        ];
        for (caller, op, path, want) in table {
            let got = allowed(caller, *op, &segs(path));
            assert_eq!(
                got.is_ok(),
                *want,
                "{caller:?} {op:?} '{path}' → {got:?}, want {want}"
            );
        }
    }

    #[test]
    fn normalize_refuses_traversal() {
        // Every refused shape, each naming its guard.
        for bad in [
            "..",
            "../x",
            "a/../b",
            "a/./b",
            "/abs",
            "/a/b",
            "a//b",
            "a/b/",
            "a\\b",
            "a\0b",
            "a%2Fb",
            "a%2fb",
            "100%legit",
            ".hidden",
            ".trash/x",
            ".blobs/deadbeef",
            ".git/config",
            "a/.x",
            "x.blob",
            "a/x.blob",
            "a\u{202e}b",
            "a\tb",
            "a\nb",
        ] {
            assert!(normalize(bad).is_err(), "'{bad}' must refuse");
        }
    }

    #[test]
    fn normalize_accepts_good_names() {
        for (raw, want) in [
            ("", ""),
            ("global/notes.md", "global/notes.md"),
            ("projects/cadence/spec v2.md", "projects/cadence/spec v2.md"),
            ("agents/swe-1/knowledge/日本語.md", "agents/swe-1/knowledge/日本語.md"),
        ] {
            assert_eq!(normalize(raw).unwrap(), want, "'{raw}'");
        }
        // NFC: the NFD form of é normalizes to the NFC byte string.
        let nfd = "projects/p/cafe\u{301}.md";
        assert_eq!(normalize(nfd).unwrap(), "projects/p/caf\u{e9}.md");
    }

    #[test]
    fn actor_names() {
        assert_eq!(Caller::Operator.actor(), "operator");
        assert_eq!(agent().actor(), "swe-1");
        assert_eq!(Caller::User("f".into()).actor(), "f (ui)");
        assert_eq!(Caller::Public.actor(), "anonymous (ui)");
    }

    #[test]
    fn sniff_table() {
        assert_eq!(sniff_mime(&[0x89, b'P', b'N', b'G', 0, 0]), "image/png");
        assert_eq!(sniff_mime(b"%PDF-1.7"), "application/pdf");
        assert_eq!(sniff_mime(b"<svg xmlns='x'></svg>"), "image/svg+xml");
        assert_eq!(sniff_mime(b"<!DOCTYPE html><html>"), "text/html");
        assert_eq!(sniff_mime(b"hello world"), "text/plain");
        assert_eq!(sniff_mime(&[0xde, 0xad, 0xbe, 0xef]), "application/octet-stream");
    }
}
