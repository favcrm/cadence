//! History: everything the tracker's own git log can answer about an
//! issue — `log`, `diff`, `blame` and `ls --at` — plus `code_commits`,
//! the same question asked of the project's own repos. Strictly
//! read-only: no lock, no commit, no fetch; a tracker with no remote
//! works. The only stored state is the git history that already exists.

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::issue::model::{self, Front};
use crate::issue::{board, git, hooks, parse, project, time};
use crate::proc::BoundedError;

mod provenance;

#[cfg(test)]
use provenance::{assignment_at, owner_walk, OwnerProof};
pub use provenance::{owner_assignments, owner_changed_at};

/// Front keys in file order — the union over `Front` is fixed, so
/// diff/blame output is stable instead of map-ordered.
const FIELD_ORDER: &[&str] = &[
    "id",
    "title",
    "status",
    "priority",
    "owner",
    "claim",
    "component",
    "tags",
    "paths",
    "parent",
    "blocked_by",
    "relates",
    "duplicate_of",
    "refs",
    "created",
];

/// Every history verb reads the tracker's git log — a PM dir that is
/// not a repo gets this refusal, never a git error dump.
fn require_git(pm_dir: &Path) -> Result<()> {
    if hooks::git_dir(pm_dir).is_none() {
        return Err(Error::rejected(format!(
            "{} is not a git repository — issue history needs the \
             tracker's own log (`cadence issue init` sets one up)",
            pm_dir.display()
        )));
    }
    Ok(())
}

/// `<project>/<ID>` — the pathspec every `git log --` walks.
pub(super) fn issue_rel(project_key: &str, id: &str) -> String {
    format!("{project_key}/{id}")
}

/// One `git log` row for the issue folder.
struct RawCommit {
    /// Full sha — internal `git show` calls.
    full: String,
    /// Abbreviated sha — the `sha` field in output.
    sha: String,
    /// RFC 3339 UTC (`%at` epoch through `time::iso`).
    at: String,
    /// `%an` — commit author name, the last-resort `by`.
    author: String,
    /// `%s` — the raw subject.
    subject: String,
    /// `%(trailers:key=Actor)` — the CAD-42 trailer; empty on
    /// commits written before trailers existed.
    trailer_actor: String,
}

fn raw_log(pm_dir: &Path, rel: &str) -> Result<Vec<RawCommit>> {
    let out = git(
        pm_dir,
        &[
            "log",
            "--format=%H%x1f%h%x1f%at%x1f%an%x1f%s%x1f%(trailers:key=Actor,valueonly,separator=%x2C)",
            "--",
            rel,
        ],
    )?;
    Ok(out
        .lines()
        .filter_map(|line| {
            let mut parts = line.splitn(6, '\x1f');
            let (full, sha, at, author, subject, trailer) = (
                parts.next()?,
                parts.next()?,
                parts.next()?,
                parts.next()?,
                parts.next()?,
                parts.next()?,
            );
            Some(RawCommit {
                full: full.to_string(),
                sha: sha.to_string(),
                at: at
                    .parse::<i64>()
                    .map(time::iso)
                    .unwrap_or_else(|_| at.to_string()),
                author: author.to_string(),
                subject: subject.to_string(),
                trailer_actor: trailer.trim().to_string(),
            })
        })
        .collect())
}

/// A trailing ` (actor)` — the writer appends it for UI writes, and
/// the actor itself may contain parens (`operator (ui)`), so the scan
/// is depth-counted from the end.
fn split_paren(text: &str) -> (String, Option<String>) {
    if !text.ends_with(')') {
        return (text.to_string(), None);
    }
    let bytes = text.as_bytes();
    let mut depth = 0i32;
    for i in (0..bytes.len()).rev() {
        match bytes[i] {
            b')' => depth += 1,
            b'(' => {
                depth -= 1;
                if depth == 0 {
                    if i > 0 && bytes[i - 1] == b' ' {
                        return (
                            text[..i - 1].to_string(),
                            Some(text[i + 1..text.len() - 1].to_string()),
                        );
                    }
                    return (text.to_string(), None);
                }
            }
            _ => {}
        }
    }
    (text.to_string(), None)
}

/// `(kind, summary, by)` for a cadence-shaped subject — `None` for
/// anything else (hand edits, reverts, sync commits, unknown verbs).
pub(super) fn parse_subject(subject: &str, id: &str) -> Option<(&'static str, String, String)> {
    // A bulk write names every id it touched: `CAD-1, CAD-2: set …`.
    let (ids, rest) = subject.split_once(": ")?;
    if !ids.split(", ").any(|i| i == id) {
        return None;
    }
    // CAD-405/360 epic stage moves and plan decisions: the writer names
    // the actor only in the trailer, and a stage note or the ticket
    // count may end in `)` — never paren-parse these.
    for (prefix, kind) in [
        ("stage ", "stage"),
        ("plan approved ", "plan"),
        ("plan rejected ", "plan"),
    ] {
        if rest.starts_with(prefix) {
            return Some((kind, rest.trim_end().to_string(), String::new()));
        }
    }
    let (summary, actor) = split_paren(rest);
    let summary = summary.trim_end().to_string();
    let kind = match summary.split_whitespace().next().unwrap_or("") {
        "created" if summary == "created" => "created",
        "set" => "set",
        "tag" => "tag",
        "link" => "link",
        "unlink" => "unlink",
        "ref" => "ref",
        "comment" => "comment",
        "attach" => "attach",
        // CAD-383: `claim by|refreshed by|take-over by …`, `release by …`.
        "claim" => "claim",
        "release" => "release",
        _ => return None,
    };
    Some((kind, summary, actor.unwrap_or_default()))
}

/// The `by` field, most-truthful first: the `Actor:` trailer (CAD-42
/// commits), then the ` (actor)` subject suffix (API writes), then
/// `comment by <name>` (comments name their author), else the git
/// author. Pre-trailer commits resolve through the same chain minus
/// the first step.
fn actor_of(raw: &RawCommit, id: &str) -> String {
    if !raw.trailer_actor.is_empty() {
        return raw.trailer_actor.clone();
    }
    if let Some((kind, summary, actor)) = parse_subject(&raw.subject, id) {
        if !actor.is_empty() {
            return actor;
        }
        if kind == "comment" {
            if let Some(name) = summary.strip_prefix("comment by ") {
                return name.to_string();
            }
        }
    }
    raw.author.clone()
}

/// One history entry for `issue log` and `GET …/history`.
fn entry(raw: &RawCommit, id: &str) -> Value {
    let by = actor_of(raw, id);
    let Some((kind, summary, _actor)) = parse_subject(&raw.subject, id) else {
        return json!({
            "sha": raw.sha, "at": raw.at, "by": by,
            "kind": "other", "summary": raw.subject,
        });
    };
    let mut e = json!({
        "sha": raw.sha, "at": raw.at,
        "by": by,
        "kind": kind, "summary": summary,
    });
    if kind == "stage" {
        // `stage <from> → <to>[ — <note>]` — the board's stage history.
        let move_ = summary["stage ".len()..].to_string();
        let (arrow, note) = match move_.split_once(" — ") {
            Some((a, n)) => (a.to_string(), Some(n.to_string())),
            None => (move_, None),
        };
        if let Some((from, to)) = arrow.split_once(" → ") {
            e["from"] = json!(from.trim());
            e["to"] = json!(to.trim());
            e["note"] = json!(note);
        }
    }
    if kind == "set" {
        // `set k=v …`; patch writes bare `title`/`body` — keys whose
        // new value the subject does not carry map to null.
        let mut fields = Map::new();
        for tok in summary["set".len()..].split_whitespace() {
            match tok.split_once('=') {
                Some((k, v)) => fields.insert(k.to_string(), json!(v)),
                None => fields.insert(tok.to_string(), Value::Null),
            };
        }
        e["fields"] = Value::Object(fields);
    }
    e
}

/// `issue log <ID>` / `GET /api/issues/<ID>/history` — newest-first
/// parsed entries for every commit that touched the issue folder.
pub fn log(pm_dir: &Path, issue: &board::Issue, limit: usize) -> Result<Vec<Value>> {
    require_git(pm_dir)?;
    let rel = issue_rel(&issue.project, &issue.front.id);
    Ok(raw_log(pm_dir, &rel)?
        .iter()
        .take(limit)
        .map(|r| entry(r, &issue.front.id))
        .collect())
}

/// Resolve `<rev>` to a commit sha. Rejects what `rev-parse` cannot
/// resolve and anything not reachable from `HEAD` — a foreign object
/// is "unrelated" to this tracker's history.
fn resolve_rev(pm_dir: &Path, rev: &str) -> Result<String> {
    let sha = git(
        pm_dir,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{rev}^{{commit}}"),
        ],
    )
    .map_err(|_| {
        Error::rejected(format!(
            "Unknown revision '{rev}' — does not resolve to a commit"
        ))
    })?;
    let ancestor = crate::reaper::status(Command::new("git").arg("-C").arg(pm_dir).args([
        "merge-base",
        "--is-ancestor",
        &sha,
        "HEAD",
    ]));
    match ancestor {
        Ok(s) if s.success() => Ok(sha),
        _ => Err(Error::rejected(format!(
            "Revision '{rev}' is not part of this tracker's history — \
             `cadence issue log` lists what is"
        ))),
    }
}

/// `git show -s --format=%at <sha>` → RFC 3339 UTC.
fn commit_time(pm_dir: &Path, sha: &str) -> Result<String> {
    let epoch = git(pm_dir, &["show", "-s", "--format=%at", sha])?;
    Ok(epoch
        .parse::<i64>()
        .map(time::iso)
        .unwrap_or_else(|_| epoch.to_string()))
}

/// `git show <rev>:<file>` — `None` when the path does not exist at
/// that revision (pre-creation revs included).
fn file_at(pm_dir: &Path, rev: &str, rel_file: &str) -> Option<String> {
    crate::reaper::output(
        Command::new("git")
            .arg("-C")
            .arg(pm_dir)
            .args(["show", &format!("{rev}:{rel_file}")]),
    )
    .ok()
    .filter(|o| o.status.success())
    .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
}

/// Frontmatter as a JSON map — `skip_serializing_if` drops unset
/// fields, matching what the file literally carries. Absent fields
/// compare as `null`.
fn front_map(front: &Front) -> Map<String, Value> {
    match serde_json::to_value(front) {
        Ok(Value::Object(m)) => m,
        _ => Map::new(),
    }
}

/// `(fields, body, state)` for issue.md at one rev — `state` is
/// `absent` when the file does not exist there, `unparseable` when it
/// exists but is not valid frontmatter (hand edits).
fn issue_at<'a>(pm_dir: &Path, rev: &str, rel_file: &str) -> (Map<String, Value>, String, &'a str) {
    let Some(text) = file_at(pm_dir, rev, rel_file) else {
        return (Map::new(), String::new(), "absent");
    };
    match parse::parse_issue(&text) {
        Ok((front, body)) => (front_map(&front), body, "present"),
        Err(_) => (Map::new(), text, "unparseable"),
    }
}

/// Added/removed line counts between two bodies — a multiset
/// difference on lines; good enough for "how much did the body move".
fn line_delta(from: &str, to: &str) -> (usize, usize) {
    fn counts(s: &str) -> HashMap<&str, usize> {
        let mut m = HashMap::new();
        for l in s.lines() {
            *m.entry(l).or_insert(0) += 1;
        }
        m
    }
    let (a, b) = (counts(from), counts(to));
    let added: usize = b
        .iter()
        .map(|(l, n)| n.saturating_sub(*a.get(l).unwrap_or(&0)))
        .sum();
    let removed: usize = a
        .iter()
        .map(|(l, n)| n.saturating_sub(*b.get(l).unwrap_or(&0)))
        .sum();
    (added, removed)
}

/// `git diff --name-status <from> <to> -- <dir>` → `(added, removed,
/// modified)` basenames under the directory.
fn file_delta(
    pm_dir: &Path,
    from: &str,
    to: &str,
    rel_dir: &str,
) -> (Vec<String>, Vec<String>, Vec<String>) {
    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut modified = Vec::new();
    let Ok(out) = git(pm_dir, &["diff", "--name-status", from, to, "--", rel_dir]) else {
        return (added, removed, modified);
    };
    let base = |p: &str| p.rsplit('/').next().unwrap_or(p).to_string();
    for line in out.lines() {
        let mut fields = line.split('\t');
        let Some(status) = fields.next() else {
            continue;
        };
        match status.as_bytes().first() {
            Some(b'A') | Some(b'C') => {
                if let Some(p) = fields.next_back() {
                    added.push(base(p));
                }
            }
            Some(b'D') => {
                if let Some(p) = fields.next() {
                    removed.push(base(p));
                }
            }
            Some(b'M') => {
                if let Some(p) = fields.next() {
                    modified.push(base(p));
                }
            }
            // R100\told\tnew — a rename removes one name and adds one.
            Some(b'R') => {
                if let Some(p) = fields.next() {
                    removed.push(base(p));
                }
                if let Some(p) = fields.next() {
                    added.push(base(p));
                }
            }
            _ => {}
        }
    }
    (added, removed, modified)
}

/// `issue diff <ID> [<rev>] [--to <rev>]` — field-level frontmatter
/// changes plus body line counts and comment/artifact file deltas.
/// Default compares the issue's newest change with its parent; a bare
/// `<rev>` means "what changed since <rev>" against the newest change.
pub fn diff(
    pm_dir: &Path,
    issue: &board::Issue,
    rev: Option<&str>,
    to: Option<&str>,
) -> Result<Value> {
    require_git(pm_dir)?;
    let id = &issue.front.id;
    let rel_dir = issue_rel(&issue.project, id);
    let rel_file = format!("{rel_dir}/issue.md");
    let to_sha = match to {
        Some(r) => resolve_rev(pm_dir, r)?,
        None => git(pm_dir, &["log", "-1", "--format=%H", "--", &rel_dir])
            .map_err(|_| Error::rejected(format!("{id} has no commits yet — nothing to diff")))?,
    };
    let from_sha = match rev {
        Some(r) => Some(resolve_rev(pm_dir, r)?),
        // The parent of `to`; a root commit diffs against "absent".
        None => git(
            pm_dir,
            &["rev-parse", "--verify", "--quiet", &format!("{to_sha}^")],
        )
        .ok(),
    };
    let (from_map, from_body, from_state) = match &from_sha {
        Some(r) => issue_at(pm_dir, r, &rel_file),
        None => (Map::new(), String::new(), "absent"),
    };
    let (to_map, to_body, to_state) = issue_at(pm_dir, &to_sha, &rel_file);
    let mut fields = Vec::new();
    for k in FIELD_ORDER {
        let (a, b) = (
            from_map.get(*k).cloned().unwrap_or(Value::Null),
            to_map.get(*k).cloned().unwrap_or(Value::Null),
        );
        if a != b {
            fields.push(json!({"field": k, "from": a, "to": b}));
        }
    }
    let (added, removed) = line_delta(&from_body, &to_body);
    // A root `to` diffs against the empty tree — files its commit
    // added under comments/artifacts still count as added.
    const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
    let delta_from = from_sha.as_deref().unwrap_or(EMPTY_TREE);
    let (c_added, c_removed, c_modified) =
        file_delta(pm_dir, delta_from, &to_sha, &format!("{rel_dir}/comments"));
    let (a_added, a_removed, a_modified) =
        file_delta(pm_dir, delta_from, &to_sha, &format!("{rel_dir}/artifacts"));
    let rev_json = |sha: &str, state: &str| -> Result<Value> {
        Ok(json!({"sha": sha, "at": commit_time(pm_dir, sha)?, "issue": state}))
    };
    Ok(json!({
        "id": id,
        "from": match &from_sha {
            Some(s) => rev_json(s, from_state)?,
            None => json!({"sha": null, "issue": "absent"}),
        },
        "to": rev_json(&to_sha, to_state)?,
        "fields": fields,
        "body_changed": from_body != to_body,
        "body_added_lines": added,
        "body_removed_lines": removed,
        "comments": {"added": c_added, "removed": c_removed, "modified": c_modified},
        "artifacts": {"added": a_added, "removed": a_removed, "modified": a_modified},
    }))
}

/// `issue blame <ID>` — for every frontmatter field currently set,
/// the history entry that last changed it. Derived by walking commits
/// newest-first and comparing parsed frontmatter snapshots — never
/// raw `git blame` line output, which cannot see field semantics.
pub fn blame(pm_dir: &Path, issue: &board::Issue) -> Result<Value> {
    require_git(pm_dir)?;
    let id = &issue.front.id;
    let rel_dir = issue_rel(&issue.project, id);
    let rel_file = format!("{rel_dir}/issue.md");
    let current = front_map(&issue.front);
    let mut attributed: Map<String, Value> = Map::new();
    for raw in raw_log(pm_dir, &rel_dir)? {
        if FIELD_ORDER
            .iter()
            .all(|k| !current.contains_key(*k) || attributed.contains_key(*k))
        {
            break;
        }
        let (cur, _, _) = issue_at(pm_dir, &raw.full, &rel_file);
        // The parent's map — empty for the creating commit and for any
        // rev where issue.md did not exist yet.
        let prev = file_at(pm_dir, &format!("{}^", raw.full), &rel_file)
            .and_then(|t| parse::parse_issue(&t).ok().map(|(f, _)| front_map(&f)))
            .unwrap_or_default();
        for k in FIELD_ORDER {
            if attributed.contains_key(*k) || !current.contains_key(*k) {
                continue;
            }
            let (a, b) = (
                prev.get(*k).cloned().unwrap_or(Value::Null),
                cur.get(*k).cloned().unwrap_or(Value::Null),
            );
            if a != b {
                attributed.insert(
                    k.to_string(),
                    json!({
                        "field": k, "value": current[*k],
                        "sha": raw.sha, "at": raw.at,
                        "by": actor_of(&raw, id),
                    }),
                );
            }
        }
    }
    // Anything the walk could not attribute (never committed, or the
    // field appeared without a commit touching the path) still reports
    // — with nulls instead of a guessed sha.
    let fields: Vec<Value> = FIELD_ORDER
        .iter()
        .filter(|k| current.contains_key(**k))
        .map(|k| {
            attributed.get(*k).cloned().unwrap_or_else(|| {
                json!({
                    "field": k, "value": current[*k],
                    "sha": null, "at": null, "by": null,
                })
            })
        })
        .collect();
    Ok(json!({"id": id, "fields": fields}))
}

/// RAII temp dir for `git archive` exports — always removed, errors
/// included. `ls --at` promises the temp export is gone afterwards.
/// Created owner-only: the exported tree is a tracker's private content,
/// and the ambient umask must never add group or other access to it.
struct TempExport(tempfile::TempDir);

impl TempExport {
    fn create() -> Result<Self> {
        let dir = tempfile::Builder::new()
            .prefix("cadence-issue-at-")
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()?;
        Ok(Self(dir))
    }
}

/// `issue ls --at <rev>` — the board as it was at one revision:
/// `git archive` the tree into a temp dir, load it through the same
/// loader, then discard it. Job and note derivation is a property of
/// *now*, so every card reports `status_source: "file"` (or `rollup`
/// for a container — that is the tree's own truth, not now-state).
pub fn ls_at(
    pm_dir: &Path,
    rev: &str,
    project_key: Option<&str>,
) -> Result<(Value, Vec<board::View>)> {
    require_git(pm_dir)?;
    if let Some(key) = project_key {
        model::check_key(key)?;
    }
    let sha = resolve_rev(pm_dir, rev)?;
    let at = json!({"sha": sha, "time": commit_time(pm_dir, &sha)?});
    let export = TempExport::create()?;
    let mut archive = crate::reaper::spawn(
        Command::new("git")
            .arg("-C")
            .arg(pm_dir)
            .args(["archive", &sha])
            .stdout(Stdio::piped()),
    )
    .map_err(|_| Error::rejected("`git` is required and was not found on PATH"))?;
    let tar = crate::reaper::spawn(
        Command::new("tar")
            .args(["-x", "-C"])
            .arg(export.0.path())
            .stdin(archive.stdout.take().expect("piped stdout"))
            .stdout(Stdio::null())
            .stderr(Stdio::piped()),
    )
    .map_err(|_| Error::rejected("`tar` is required and was not found on PATH"))?;
    let tar_out = tar.wait_with_output()?;
    let git_status = archive.wait()?;
    if !git_status.success() || !tar_out.status.success() {
        return Err(Error::rejected(format!(
            "Could not export {rev} — {}",
            String::from_utf8_lossy(&tar_out.stderr).trim()
        )));
    }
    let issues = board::load_all(export.0.path(), project_key)?;
    // A notes dir that never exists — `derive`/`chain` return empty,
    // so status falls through to the file field exactly.
    let views = board::views(&export.0.path().join(".no-notes"), issues);
    Ok((at, views))
}

/// `<id>` whole-word in a code-commit subject — `(CAD-47)`, `CAD-47:`
/// match; `CAD-479`, `XCAD-47`, `CAD-47-x` do not (`-`/`_` join words).
fn subject_mentions(subject: &str, id: &str) -> bool {
    let joiner = |c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_';
    subject.match_indices(id).any(|(i, _)| {
        let before = subject[..i].chars().next_back();
        let after = subject[i + id.len()..].chars().next();
        !before.is_some_and(joiner) && !after.is_some_and(joiner)
    })
}

/// `git log --all -n <scan>` under `dir` with a hard timeout — project
/// repos are user paths, not ours, so a slow or locked repo must not
/// stall a detail read. Returns the log text or a skip reason.
pub(super) fn git_bounded(
    dir: &Path,
    args: &[&str],
    timeout: Duration,
) -> std::result::Result<String, String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(dir).args(args);
    match crate::proc::run_bounded(&mut cmd, timeout) {
        Ok(out) if out.status.success() => Ok(String::from_utf8_lossy(&out.stdout).to_string()),
        Ok(_) => Err("git log failed".to_string()),
        Err(BoundedError::Spawn(e)) => Err(format!("spawn: {e}")),
        Err(e) => Err(e.to_string()),
    }
}

/// The ref the repo treats as its default branch: `origin/HEAD`'s
/// target, else a local `main`, else whatever is checked out.
fn default_ref(dir: &Path) -> String {
    let t = Duration::from_secs(2);
    if let Ok(out) = git_bounded(
        dir,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
        t,
    ) {
        return out.trim().to_string();
    }
    match git_bounded(
        dir,
        &["rev-parse", "--verify", "--quiet", "refs/heads/main"],
        t,
    ) {
        Ok(_) => "refs/heads/main".to_string(),
        Err(_) => "HEAD".to_string(),
    }
}

/// A subject without its trailing ` (#<n>)` squash-merge suffix.
fn normalized_subject(subject: &str) -> &str {
    let s = subject.trim();
    let bare = s.strip_suffix(')').and_then(|rest| {
        let (head, num) = rest.rsplit_once(" (#")?;
        (!num.is_empty() && num.bytes().all(|b| b.is_ascii_digit())).then_some(head)
    });
    bare.unwrap_or(s).trim()
}

const MAX_CACHED_REPOS: usize = 16;
const MAX_CACHED_ISSUES_PER_REPO: usize = 256;
const MAX_LOOSE_REFS: usize = 512;

struct CachedRepoLog {
    digest: [u8; 32],
    log: Arc<String>,
    default_ref: Option<String>,
    by_issue: HashMap<String, Vec<Value>>,
    last_used: Instant,
}

static REPO_COMMIT_CACHE: LazyLock<Mutex<HashMap<PathBuf, CachedRepoLog>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Resolve `(git_dir, common_dir)` for a repository checkout or worktree.
pub(crate) fn resolve_git_paths(dir: &Path) -> Option<(PathBuf, PathBuf)> {
    let dot_git = dir.join(".git");
    let git_dir = if dot_git.is_dir() {
        dot_git
    } else if dot_git.is_file() {
        let text = std::fs::read_to_string(&dot_git).ok()?;
        let raw = text
            .lines()
            .find_map(|line| line.strip_prefix("gitdir:"))?
            .trim();
        let path = Path::new(raw);
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            dir.join(path)
        }
    } else {
        hooks::git_dir(dir)?
    };
    if !git_dir.is_dir() {
        return None;
    }
    let commondir_file = git_dir.join("commondir");
    let common_dir = if commondir_file.is_file() {
        let raw = std::fs::read_to_string(&commondir_file).ok()?;
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            git_dir.clone()
        } else {
            let path = Path::new(trimmed);
            if path.is_absolute() {
                path.to_path_buf()
            } else {
                git_dir.join(path)
            }
        }
    } else {
        git_dir.clone()
    };
    Some((git_dir, common_dir))
}

fn collect_loose_refs(root: &Path, cur: &Path, out: &mut Vec<(PathBuf, PathBuf)>) -> Option<()> {
    let entries = std::fs::read_dir(cur).ok()?;
    for entry in entries {
        let entry = entry.ok()?;
        let path = entry.path();
        let meta = std::fs::symlink_metadata(&path).ok()?;
        if meta.file_type().is_symlink() {
            return None;
        }
        if meta.is_dir() {
            collect_loose_refs(root, &path, out)?;
        } else if meta.is_file() {
            if out.len() >= MAX_LOOSE_REFS {
                return None;
            }
            let rel = path.strip_prefix(root).ok()?.to_path_buf();
            out.push((rel, path));
        }
    }
    Some(())
}

fn hash_show_ref(dir: &Path) -> Option<[u8; 32]> {
    let out = git_bounded(dir, &["show-ref", "--head"], Duration::from_secs(2)).ok()?;
    Some(Sha256::digest(out.as_bytes()).into())
}

fn repo_ref_digest(dir: &Path, git_dir: &Path, common_dir: &Path) -> Option<[u8; 32]> {
    if common_dir.join("reftable").exists() {
        return hash_show_ref(dir);
    }
    let head_bytes = std::fs::read(git_dir.join("HEAD")).ok()?;
    let refs_root = common_dir.join("refs");
    let mut loose = Vec::new();
    if refs_root.is_dir() && collect_loose_refs(&refs_root, &refs_root, &mut loose).is_none() {
        return hash_show_ref(dir);
    }
    loose.sort_by(|a, b| a.0.cmp(&b.0));

    let mut hasher = Sha256::new();
    hasher.update(b"HEAD\0");
    hasher.update(&head_bytes);

    let packed_path = common_dir.join("packed-refs");
    if packed_path.is_file() {
        let packed = std::fs::read(&packed_path).ok()?;
        hasher.update(b"PACKED\0");
        hasher.update(&packed);
    }

    for (rel, full) in loose {
        let bytes = std::fs::read(&full).ok()?;
        hasher.update(b"REF\0");
        hasher.update(rel.to_string_lossy().as_bytes());
        hasher.update(b"\0");
        hasher.update(&bytes);
    }
    Some(hasher.finalize().into())
}

fn extract_code_hits<'a>(log: &'a str, id: &str) -> Vec<(&'a str, Value)> {
    log.lines()
        .filter_map(|line| {
            let mut parts = line.splitn(6, '\x1f');
            let (full, sha, at, author, subject, issues) = (
                parts.next()?,
                parts.next()?,
                parts.next()?,
                parts.next()?,
                parts.next()?,
                parts.next()?,
            );
            let tagged = issues.split(',').any(|t| t.trim() == id);
            (tagged || subject_mentions(subject, id)).then(|| {
                (
                    full,
                    json!({
                        "repo": "",
                        "sha": sha,
                        "at": at
                            .parse::<i64>()
                            .map(time::iso)
                            .unwrap_or_else(|_| at.to_string()),
                        "author": author,
                        "subject": subject,
                    }),
                )
            })
        })
        .take(40)
        .collect()
}

/// Tag `(full sha, commit)` candidates from one repo with `on_default`
/// and drop each not-on-default commit that has a same-subject twin on
/// the default branch. One `rev-list --no-walk <candidates> --not
/// <default>` names the commits the default branch does not reach.
fn dedupe_stale_with_default(dir: &Path, default: &str, hits: Vec<(&str, Value)>) -> Vec<Value> {
    if hits.is_empty() {
        return Vec::new();
    }
    let mut args = vec!["rev-list", "--no-walk"];
    args.extend(hits.iter().map(|(full, _)| *full));
    args.extend(["--not", default]);
    let off_default = git_bounded(dir, &args, Duration::from_secs(5)).ok();
    let mut commits: Vec<Value> = hits
        .into_iter()
        .map(|(full, mut c)| {
            c["on_default"] = match &off_default {
                Some(off) => json!(!off.lines().any(|l| l.trim() == full)),
                None => Value::Null,
            };
            c
        })
        .collect();
    let landed: Vec<String> = commits
        .iter()
        .filter(|c| c["on_default"] == json!(true))
        .map(|c| normalized_subject(c["subject"].as_str().unwrap_or_default()).to_string())
        .collect();
    commits.retain(|c| {
        c["on_default"] != json!(false)
            || !landed
                .iter()
                .any(|s| s == normalized_subject(c["subject"].as_str().unwrap_or_default()))
    });
    commits
}

const CODE_LOG_ARGS: &[&str] = &[
    "log",
    "--all",
    "-n",
    "2000",
    "--format=%H%x1f%h%x1f%at%x1f%an%x1f%s%x1f%(trailers:key=Issue,valueonly,separator=%x2C)",
];

fn stamp_repo_label(mut rows: Vec<Value>, label: &str) -> Vec<Value> {
    for row in &mut rows {
        row["repo"] = Value::String(label.to_string());
    }
    rows
}

/// `commits` for an issue's detail: for each repo the project lists,
/// the newest commits on any ref whose message carries an
/// `Issue: <ID>` trailer or names the id as a whole word in the
/// subject (the `(CAD-47)` convention) — `{repo, sha, at, author,
/// subject, on_default}`. Read-only `git log --all` bounded to the
/// newest 2000 commits per repo, ≤40 matches per repo before dedupe,
/// ≤20 merged; a missing or non-git path is skipped and reported in
/// `commits_skipped`, never an error.
///
/// `--all` also walks stale remote-tracking refs, so a merged issue
/// would list its squash commit and the old branch tip. `on_default`
/// marks what the default branch reaches (`null` when that could not
/// be read); a not-on-default commit whose normalized subject has an
/// `on_default` twin is dropped. Default-branch commits list first,
/// then newest first.
pub fn code_commits(pm_dir: &Path, issue: &board::Issue) -> (Vec<Value>, Vec<Value>) {
    let Ok(projects) = project::list(pm_dir) else {
        return (Vec::new(), Vec::new());
    };
    let Some(proj) = projects.iter().find(|p| p.key == issue.project) else {
        return (Vec::new(), Vec::new());
    };
    let id = issue.front.id.as_str();
    let mut commits = Vec::new();
    let mut skipped = Vec::new();
    for repo in &proj.repos {
        let label = repo
            .path
            .clone()
            .or_else(|| repo.remote.clone())
            .unwrap_or_default();
        let Some(path) = &repo.path else {
            skipped.push(json!({"repo": label, "reason": "no local path"}));
            continue;
        };
        let dir = project::expand_home(path);
        let Some((git_dir, common_dir)) = dir.is_dir().then(|| resolve_git_paths(&dir)).flatten()
        else {
            skipped.push(json!({"repo": label, "reason": "not a git repo"}));
            continue;
        };
        let Some(digest) = repo_ref_digest(&dir, &git_dir, &common_dir) else {
            match git_bounded(&dir, CODE_LOG_ARGS, Duration::from_secs(5)) {
                Err(reason) => skipped.push(json!({"repo": label, "reason": reason})),
                Ok(log) => {
                    let hits = extract_code_hits(&log, id);
                    if !hits.is_empty() {
                        let dref = default_ref(&dir);
                        let rows = dedupe_stale_with_default(&dir, &dref, hits);
                        commits.extend(stamp_repo_label(rows, &label));
                    }
                }
            }
            continue;
        };

        let mut cached_log: Option<Arc<String>> = None;
        let mut cached_default: Option<String> = None;
        let mut cached_rows: Option<Vec<Value>> = None;
        if let Ok(mut cache) = REPO_COMMIT_CACHE.lock() {
            if let Some(entry) = cache.get_mut(&dir) {
                if entry.digest == digest {
                    entry.last_used = Instant::now();
                    if let Some(rows) = entry.by_issue.get(id) {
                        cached_rows = Some(rows.clone());
                    } else {
                        cached_log = Some(Arc::clone(&entry.log));
                        cached_default = entry.default_ref.clone();
                    }
                }
            }
        }
        if let Some(rows) = cached_rows {
            commits.extend(stamp_repo_label(rows, &label));
            continue;
        }

        let log = match cached_log {
            Some(log) => log,
            None => match git_bounded(&dir, CODE_LOG_ARGS, Duration::from_secs(5)) {
                Err(reason) => {
                    skipped.push(json!({"repo": label, "reason": reason}));
                    continue;
                }
                Ok(raw) => Arc::new(raw),
            },
        };

        let hits = extract_code_hits(&log, id);
        let (rows, resolved_default) = if hits.is_empty() {
            (Vec::new(), cached_default)
        } else {
            let dref = cached_default.unwrap_or_else(|| default_ref(&dir));
            let rows = dedupe_stale_with_default(&dir, &dref, hits);
            (rows, Some(dref))
        };

        if let Ok(mut cache) = REPO_COMMIT_CACHE.lock() {
            let now = Instant::now();
            let same_digest = cache.get(&dir).is_some_and(|e| e.digest == digest);
            if !same_digest {
                if !cache.contains_key(&dir) && cache.len() >= MAX_CACHED_REPOS {
                    if let Some(oldest) = cache
                        .iter()
                        .min_by_key(|(_, entry)| entry.last_used)
                        .map(|(k, _)| k.clone())
                    {
                        cache.remove(&oldest);
                    }
                }
                cache.insert(
                    dir.clone(),
                    CachedRepoLog {
                        digest,
                        log: Arc::clone(&log),
                        default_ref: resolved_default.clone(),
                        by_issue: HashMap::new(),
                        last_used: now,
                    },
                );
            }
            if let Some(entry) = cache.get_mut(&dir) {
                entry.last_used = now;
                if entry.default_ref.is_none() {
                    entry.default_ref = resolved_default;
                }
                if entry.by_issue.len() >= MAX_CACHED_ISSUES_PER_REPO {
                    entry.by_issue.clear();
                }
                entry.by_issue.insert(id.to_string(), rows.clone());
            }
        }
        commits.extend(stamp_repo_label(rows, &label));
    }
    commits.sort_by(|a, b| {
        let off = |c: &Value| c["on_default"] == json!(false);
        (off(a), b["at"].as_str()).cmp(&(off(b), a["at"].as_str()))
    });
    commits.truncate(20);
    (commits, skipped)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actor_parens() {
        let (rest, actor) = split_paren("set status=review (operator (ui))");
        assert_eq!(rest, "set status=review");
        assert_eq!(actor.as_deref(), Some("operator (ui)"));
        assert_eq!(split_paren("comment by fable-cc").1, None);
        assert_eq!(split_paren("wip (draft)").1, Some("draft".to_string()));
    }

    #[test]
    fn subject_normalizing() {
        assert_eq!(
            normalized_subject(" feat: x (CAD-1) (#42) "),
            "feat: x (CAD-1)"
        );
        assert_eq!(normalized_subject("feat: x (CAD-1)"), "feat: x (CAD-1)");
        assert_eq!(normalized_subject("fix (#)"), "fix (#)");
        assert_eq!(normalized_subject("fix (#4a)"), "fix (#4a)");
    }

    #[test]
    fn subject_kinds() {
        let (k, s, a) =
            parse_subject("CAD-1: set status=doing owner=me (operator (ui))", "CAD-1").unwrap();
        assert_eq!(k, "set");
        assert_eq!(s, "set status=doing owner=me");
        assert_eq!(a, "operator (ui)");
        assert!(parse_subject("CAD-1: created", "CAD-1").is_some());
        assert!(parse_subject("CAD-2: created", "CAD-1").is_none()); // other issue
        assert!(parse_subject("Revert \"CAD-1: created\"", "CAD-1").is_none());
        assert!(parse_subject("wip", "CAD-1").is_none());
        // Prefixed but unknown verb → not cadence-shaped.
        assert!(parse_subject("CAD-1: frobnicate", "CAD-1").is_none());
    }

    /// CAD-432: a stage move is a `stage` entry with from, to and note —
    /// the note may end in `)` and is never read as the actor, which
    /// comes from the trailer; plan decisions are `plan` entries.
    #[test]
    fn stage_and_plan_entries() {
        let raw = |subject: &str| RawCommit {
            full: "x".into(),
            sha: "abc1234".into(),
            at: "2026-09-18T00:00:00Z".into(),
            author: "cadence".into(),
            subject: subject.into(),
            trailer_actor: "pane-1".into(),
        };
        let e = entry(
            &raw("D-1: stage build → verify — tasks done (all 3)"),
            "D-1",
        );
        assert_eq!(e["kind"], "stage", "{e}");
        assert_eq!(e["by"], "pane-1");
        assert_eq!(
            (&e["from"], &e["to"], &e["note"]),
            (
                &json!("build"),
                &json!("verify"),
                &json!("tasks done (all 3)")
            )
        );
        let e = entry(&raw("D-1: stage verify → build"), "D-1");
        assert_eq!((&e["to"], &e["note"]), (&json!("build"), &Value::Null));
        let e = entry(
            &raw("D-1: plan approved by operator (2 tickets ready)"),
            "D-1",
        );
        assert_eq!(e["kind"], "plan", "{e}");
        assert_eq!(e["summary"], "plan approved by operator (2 tickets ready)");
        assert_eq!(
            entry(&raw("D-1: plan rejected by operator"), "D-1")["kind"],
            "plan"
        );
        assert_eq!(entry(&raw("D-1: staged"), "D-1")["kind"], "other");
    }

    #[test]
    fn entry_fields_map() {
        let raw = RawCommit {
            full: "x".into(),
            sha: "abc1234".into(),
            at: "2026-09-18T00:00:00Z".into(),
            author: "cadence".into(),
            subject: "CAD-1: set status=review body (operator (ui))".into(),
            trailer_actor: String::new(),
        };
        let e = entry(&raw, "CAD-1");
        assert_eq!(e["kind"], "set");
        assert_eq!(e["by"], "operator (ui)");
        assert_eq!(e["fields"]["status"], "review");
        assert!(e["fields"]["body"].is_null());
        // A trailer beats the subject actor and the git author.
        let raw = RawCommit {
            trailer_actor: "fable-cc".into(),
            author: "cadence".into(),
            ..raw
        };
        assert_eq!(entry(&raw, "CAD-1")["by"], "fable-cc");
    }

    #[test]
    fn subject_whole_word() {
        assert!(subject_mentions("fix (X-1) edge", "X-1"));
        assert!(subject_mentions("X-1: the fix", "X-1"));
        assert!(subject_mentions("backport X-1 to stable", "X-1"));
        assert!(!subject_mentions("wip X-12", "X-1"));
        assert!(!subject_mentions("X-1x thing", "X-1"));
        assert!(!subject_mentions("fix-X-1 thing", "X-1"));
        assert!(!subject_mentions("unrelated", "X-1"));
    }

    /// CAD-2184: `ls_at`'s `git archive` export root is owner-only even
    /// when the caller runs with a permissive umask, and it is gone when
    /// the listing returns. The child process is isolated: only its own
    /// umask changes, never the suite's.
    #[test]
    fn historical_export_is_private_and_cleaned_up_under_umask_0002() {
        const CHILD: &str = "CADENCE_TEST_HISTORY_UMASK_CHILD";
        const TEST: &str =
            "issue::history::tests::historical_export_is_private_and_cleaned_up_under_umask_0002";
        if std::env::var_os(CHILD).is_some() {
            let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            let root = tempfile::tempdir().unwrap();
            let scratch = root.path().join("scratch");
            std::fs::create_dir(&scratch).unwrap();
            // A plain creation proves the permissive umask is in effect.
            assert_eq!(mode(&scratch), 0o775);

            let pm = root.path().join("pm");
            std::fs::create_dir_all(pm.join("demo/D-1")).unwrap();
            std::fs::write(pm.join("demo/project.yaml"), "key: demo\nprefix: D\n").unwrap();
            std::fs::write(
                pm.join("demo/D-1/issue.md"),
                "---\nid: D-1\ntitle: t\nstatus: doing\npriority: P2\n\
                 created: '2026-01-01T00:00:00Z'\n---\nbody\n",
            )
            .unwrap();
            let git = |args: &[&str]| {
                let out = Command::new("git")
                    .arg("-C")
                    .arg(&pm)
                    .args(["-c", "user.name=test", "-c", "user.email=test@test"])
                    .args(args)
                    .output()
                    .unwrap();
                assert!(out.status.success(), "git {args:?}: {out:?}");
            };
            git(&["init", "-q"]);
            git(&["add", "-A"]);
            git(&["commit", "-qm", "D-1: created"]);

            // Everything `ls_at` stages lands in this child-only TMPDIR.
            std::env::set_var("TMPDIR", &scratch);

            let export = TempExport::create().unwrap();
            assert_eq!(mode(export.0.path()), 0o700);
            let exported = export.0.path().to_path_buf();
            drop(export);
            assert!(!exported.exists(), "the export scratch outlived its owner");
            assert!(std::fs::read_dir(&scratch).unwrap().next().is_none());

            let (at, views) = ls_at(&pm, "HEAD", None).unwrap();
            assert_eq!(at["sha"].as_str().unwrap().len(), 40);
            assert_eq!(views.len(), 1);
            assert_eq!(views[0].issue.front.id, "D-1");
            assert_eq!(views[0].status, "doing");
            assert!(
                std::fs::read_dir(&scratch).unwrap().next().is_none(),
                "`ls_at` left its export scratch behind"
            );
            return;
        }

        let output = Command::new("sh")
            .arg("-c")
            .arg("umask 0002 && exec \"$0\" --exact \"$1\" --test-threads=1")
            .arg(std::env::current_exe().unwrap())
            .arg(TEST)
            // The inherited environment may name another repository
            // (`GIT_DIR`/`GIT_WORK_TREE`) or a user config that would
            // redirect both the fixture and the real `ls_at` git calls.
            // This child gets an explicit allowlist only.
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("TMPDIR", std::env::temp_dir())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success() && String::from_utf8_lossy(&output.stdout).contains("1 passed"),
            "stdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn code_commits_cache_invalidates_when_a_new_commit_lands() {
        let root = tempfile::tempdir().unwrap();
        let pm = root.path().join("pm");
        let repo = root.path().join("repo");
        std::fs::create_dir_all(pm.join("demo/CAD-99")).unwrap();
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(
            pm.join("demo/project.yaml"),
            format!(
                "key: demo\nprefix: CAD\nrepos:\n- path: {}\n",
                repo.display()
            ),
        )
        .unwrap();
        std::fs::write(
            pm.join("demo/CAD-99/issue.md"),
            "---\nid: CAD-99\ntitle: t\nstatus: doing\npriority: P2\n\
             created: '2026-01-01T00:00:00Z'\n---\nbody\n",
        )
        .unwrap();
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(["-c", "user.name=test", "-c", "user.email=test@test"])
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}: {out:?}");
        };
        git(&["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("a.txt"), "one\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", "feat: first (CAD-99)"]);

        let issues = board::load_all(&pm, Some("demo")).unwrap();
        let issue = issues.iter().find(|i| i.front.id == "CAD-99").unwrap();

        let (first, skipped) = code_commits(&pm, issue);
        assert!(skipped.is_empty(), "{skipped:?}");
        assert_eq!(first.len(), 1, "{first:?}");
        // Warm cache hit returns identical result.
        let (cached, _) = code_commits(&pm, issue);
        assert_eq!(cached.len(), 1, "{cached:?}");

        // Second commit immediately after must invalidate via ref digest.
        std::fs::write(repo.join("b.txt"), "two\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-qm", "feat: second (CAD-99)"]);

        let (after, skipped) = code_commits(&pm, issue);
        assert!(skipped.is_empty(), "{skipped:?}");
        assert_eq!(after.len(), 2, "{after:?}");
    }

    /// A real tracker repo for `owner_assignments`: issues under
    /// `demo/`, commits with author dates and `Actor:`/
    /// `Activity-Receipt:` trailers set exactly like the writers do.
    mod owner_assignments_tests {
        use super::owner_assignments;
        use crate::issue::time;
        use serde_json::{json, Value};
        use std::collections::HashMap;
        use std::path::Path;
        use std::process::Command;
        use std::time::Duration;

        /// `{project}/{id}` → provenance, at the real timeout/budget —
        /// `subject` is the caller's verified platform subject, `None`
        /// for the shared operator session.
        fn run_as(
            dir: &Path,
            cards: &[(&str, &str)],
            subject: Option<&str>,
        ) -> HashMap<String, Value> {
            owner_assignments(
                dir,
                &cards
                    .iter()
                    .map(|(r, o)| (r.to_string(), *o))
                    .collect::<Vec<_>>(),
                subject,
                Duration::from_secs(20),
                128,
            )
        }

        fn run(dir: &Path, cards: &[(&str, &str)]) -> HashMap<String, Value> {
            run_as(dir, cards, None)
        }

        fn human_receipt(sub: &str, id: &str) -> Value {
            json!({
                "schema_version": 1,
                "company": "fixture-company",
                "human": {
                    "company": "fixture-company",
                    "sub": sub,
                    "session": "fixture-session",
                    "role": "member",
                },
                "action": "issue_created",
                "resource": format!("issue:{id}"),
                "reason": "synthetic assignment fixture",
                "request_id": "00000000-0000-4000-8000-000000000001",
                "correlation_id": "00000000-0000-4000-8000-000000000001",
                "causation_id": null,
                "event_id": "00000000-0000-4000-8000-000000000002",
                "at": 1000.0,
                "outcome": "committed",
                "digest": format!("sha256:{}", "0".repeat(64)),
            })
        }

        struct Tracker(tempfile::TempDir);

        impl Tracker {
            fn new() -> Self {
                let t = Tracker(tempfile::tempdir().unwrap());
                t.git(&["init", "-q"]);
                t
            }

            fn dir(&self) -> &Path {
                self.0.path()
            }

            fn git(&self, args: &[&str]) {
                let out = Command::new("git")
                    .arg("-C")
                    .arg(self.dir())
                    .args(["-c", "user.name=test", "-c", "user.email=test@test"])
                    .args(args)
                    .output()
                    .unwrap();
                assert!(out.status.success(), "git {args:?}: {out:?}");
            }

            /// Rewrite `demo/<id>/issue.md` with `owner`, then commit it
            /// with subject `subject`, trailers as one more paragraph,
            /// at `at` (author and committer dates alike).
            fn commit(
                &self,
                id: &str,
                owner: Option<&str>,
                subject: &str,
                trailers: &str,
                at: i64,
            ) {
                let owner_line = owner.map(|o| format!("owner: {o}\n")).unwrap_or_default();
                let text = format!(
                    "---\nid: {id}\ntitle: t\nstatus: doing\npriority: P2\n{owner_line}created: \"2026-01-01T00:00:00Z\"\n---\nbody\n"
                );
                let dir = self.dir().join("demo").join(id);
                std::fs::create_dir_all(&dir).unwrap();
                std::fs::write(dir.join("issue.md"), text).unwrap();
                self.git(&["add", "-A"]);
                let date = format!("@{at} +0000");
                let msg = if trailers.is_empty() {
                    subject.to_string()
                } else {
                    format!("{subject}\n\n{trailers}")
                };
                let out = Command::new("git")
                    .arg("-C")
                    .arg(self.dir())
                    .args(["-c", "user.name=test", "-c", "user.email=test@test"])
                    .args(["commit", "-q", "-m", &msg])
                    .env("GIT_AUTHOR_DATE", &date)
                    .env("GIT_COMMITTER_DATE", &date)
                    .output()
                    .unwrap();
                assert!(out.status.success(), "{out:?}");
            }
        }

        /// Created unowned, then `set owner=` — 'assigned' by the
        /// recorded trailer actor at that commit's author time.
        #[test]
        fn later_assignment_reports_the_assigning_commit() {
            let t = Tracker::new();
            t.commit("D-1", None, "D-1: created", "Actor: avery", 1_000);
            t.commit(
                "D-1",
                Some("mira"),
                "D-1: set owner=mira",
                "Actor: avery",
                2_000,
            );
            let out = run(t.dir(), &[("demo/D-1", "mira")]);
            let a = &out["demo/D-1"];
            assert_eq!(
                a,
                &json!({
                    "kind": "assigned",
                    "actor": "avery",
                    "at": time::iso(2_000),
                    "self_assigned": false,
                }),
                "{a}"
            );
        }

        #[test]
        fn unreadable_parent_blob_is_not_a_creation_assignment() {
            let t = Tracker::new();
            t.commit("D-1", Some("avery"), "D-1: created", "Actor: avery", 1_000);
            t.commit(
                "D-1",
                Some("mira"),
                "D-1: set owner=mira",
                "Actor: avery",
                2_000,
            );
            let revision = |spec: &str| {
                let out = Command::new("git")
                    .arg("-C")
                    .arg(t.dir())
                    .args(["rev-parse", "--verify", spec])
                    .output()
                    .unwrap();
                assert!(out.status.success(), "{out:?}");
                String::from_utf8(out.stdout).unwrap().trim().to_string()
            };
            let head = revision("HEAD");
            let blob = revision("HEAD^:demo/D-1/issue.md");
            let deadline = std::time::Instant::now() + Duration::from_secs(20);
            let cards = vec![("demo/D-1".to_string(), "mira")];
            let walk = super::super::owner_walk(t.dir(), &head, &cards, deadline).unwrap();
            // Object loss after the walk must not turn an unreadable
            // existing file into a proven creation-time assignment.
            std::fs::remove_file(
                t.dir()
                    .join(".git/objects")
                    .join(&blob[..2])
                    .join(&blob[2..]),
            )
            .unwrap();
            let mut proof = super::super::OwnerProof {
                pm_dir: t.dir(),
                deadline,
                reads_left: 128,
            };
            let assignment =
                super::super::assignment_at(&mut proof, &head, "demo/D-1", "mira", None, &walk);
            assert_eq!(assignment, Value::Null);
        }

        /// Created carrying the owner — 'initial_owner' from the
        /// creation commit. `self_assigned` needs the caller's
        /// verified subject AND the commit's `Activity-Receipt`: the
        /// `Actor:` trailer spelling alone never proves self.
        #[test]
        fn initial_owner_and_self_assignment() {
            let t = Tracker::new();
            t.commit(
                "D-1",
                Some("avery"),
                "D-1: created",
                &format!(
                    "Actor: avery\nActivity-Receipt: {}",
                    human_receipt("avery", "D-1")
                ),
                1_000,
            );
            t.commit("D-2", Some("mira"), "D-2: created", "Actor: avery", 2_000);
            let out = run_as(
                t.dir(),
                &[("demo/D-1", "avery"), ("demo/D-2", "mira")],
                Some("avery"),
            );
            assert_eq!(out["demo/D-1"]["kind"], "initial_owner");
            assert_eq!(
                out["demo/D-1"]["self_assigned"], true,
                "{}",
                out["demo/D-1"]
            );
            assert_eq!(out["demo/D-2"]["kind"], "initial_owner");
            assert_eq!(
                out["demo/D-2"]["self_assigned"], false,
                "{}",
                out["demo/D-2"]
            );
        }

        /// An `Actor:` trailer that merely SPELLS the owner — no
        /// `Activity-Receipt` at all — can never be "self", even when
        /// the caller's subject matches: receipts are the only proven
        /// person.
        #[test]
        fn an_actor_spelling_without_a_receipt_is_never_self() {
            let t = Tracker::new();
            t.commit("D-1", Some("mira"), "D-1: created", "Actor: mira", 1_000);
            let out = run_as(t.dir(), &[("demo/D-1", "mira")], Some("mira"));
            assert_eq!(out["demo/D-1"]["kind"], "initial_owner");
            assert_eq!(
                out["demo/D-1"]["self_assigned"], false,
                "{}",
                out["demo/D-1"]
            );
        }

        /// A no-op owner + title edit that names owner in the subject
        /// (`kind other`, the `issue edit` shape) never resets the
        /// handoff — and neither does an `owner:`-line rewrite whose
        /// parsed value is unchanged. Provenance is the before/after
        /// parse, never the subject token or the line diff alone.
        #[test]
        fn a_noop_edit_subject_cannot_reset_the_handoff() {
            let t = Tracker::new();
            t.commit("D-1", None, "D-1: created", "Actor: avery", 1_000);
            t.commit(
                "D-1",
                Some("mira"),
                "D-1: set owner=mira",
                "Actor: avery",
                2_000,
            );
            let path = t.dir().join("demo/D-1/issue.md");
            let text = std::fs::read_to_string(&path)
                .unwrap()
                .replace("\ntitle: t\n", "\ntitle: changed\n");
            std::fs::write(path, text).unwrap();
            t.git(&["add", "-A"]);
            t.git(&[
                "commit",
                "-qm",
                "D-1: edit owner=mira title=changed",
                "-m",
                "Actor: operator",
            ]);
            // The owner line rewritten quoted — the same parsed owner —
            // IS an owner:-touching commit but is no transition.
            std::fs::write(
                t.dir().join("demo/D-1/issue.md"),
                "---\nid: D-1\ntitle: changed\nstatus: doing\npriority: P2\nowner: \"mira\"\ncreated: \"2026-01-01T00:00:00Z\"\n---\nbody\n",
            )
            .unwrap();
            t.git(&["add", "-A"]);
            t.git(&["commit", "-qm", "D-1: requote owner", "-m", "Actor: jordan"]);
            let out = run(t.dir(), &[("demo/D-1", "mira")]);
            let a = &out["demo/D-1"];
            assert_eq!(a["actor"], "avery", "{a}");
            assert_eq!(a["at"], time::iso(2_000), "{a}");
        }

        #[test]
        fn tenure_includes_multiline_owner_changes() {
            let t = Tracker::new();
            t.commit(
                "D-1",
                Some("\n  mira"),
                "D-1: created",
                "Actor: avery",
                1_000,
            );
            t.commit(
                "D-1",
                Some("\n  jordan"),
                "D-1: set owner=jordan",
                "Actor: riley",
                2_000,
            );
            t.commit(
                "D-1",
                Some("\n  mira"),
                "D-1: set owner=mira",
                "Actor: taylor",
                3_000,
            );
            let out = run(t.dir(), &[("demo/D-1", "mira")]);
            assert_eq!(
                out["demo/D-1"],
                json!({
                    "kind": "assigned",
                    "actor": "taylor",
                    "at": time::iso(3_000),
                    "self_assigned": false,
                })
            );
        }

        #[test]
        fn tenure_ignores_a_branch_local_owner_roundtrip() {
            let t = Tracker::new();
            t.commit("D-1", Some("mira"), "D-1: created", "Actor: avery", 1_000);
            t.git(&["checkout", "-qb", "side"]);
            t.commit(
                "D-1",
                Some("jordan"),
                "D-1: set owner=jordan",
                "Actor: riley",
                2_000,
            );
            t.commit(
                "D-1",
                Some("mira"),
                "D-1: set owner=mira",
                "Actor: taylor",
                3_000,
            );
            let path = t.dir().join("demo/D-1/issue.md");
            let text = std::fs::read_to_string(&path)
                .unwrap()
                .replace("\ntitle: t\n", "\ntitle: side edit\n");
            std::fs::write(path, text).unwrap();
            t.git(&["add", "-A"]);
            t.git(&["commit", "-qm", "D-1: edit title", "-m", "Actor: riley"]);
            t.git(&["checkout", "-"]);
            t.commit("D-2", Some("avery"), "D-2: created", "Actor: avery", 4_000);
            t.git(&[
                "merge",
                "--no-ff",
                "-qm",
                "Merge side\n\nActor: merge-fixture",
                "side",
            ]);
            let out = run(t.dir(), &[("demo/D-1", "mira")]);
            assert_eq!(out["demo/D-1"]["kind"], "initial_owner", "{out:?}");
            assert_eq!(out["demo/D-1"]["actor"], "avery", "{out:?}");
            assert_eq!(out["demo/D-1"]["at"], time::iso(1_000), "{out:?}");
        }

        /// Away and back inside one author second: ordered commit
        /// identity decides, so tenure and actor come from the newest
        /// transition — not the timestamp or the older claim.
        #[test]
        fn away_and_back_in_the_same_second_returns_the_newest() {
            let t = Tracker::new();
            t.commit("D-1", Some("mira"), "D-1: created", "Actor: avery", 5_000);
            t.commit(
                "D-1",
                Some("jordan"),
                "D-1: set owner=jordan",
                "Actor: mira",
                5_000,
            );
            t.commit(
                "D-1",
                Some("mira"),
                "D-1: set owner=mira",
                "Actor: jordan",
                5_000,
            );
            let out = run(t.dir(), &[("demo/D-1", "mira")]);
            let a = &out["demo/D-1"];
            assert_eq!(a["kind"], "assigned", "{a}");
            assert_eq!(a["actor"], "jordan", "{a}");
            assert_eq!(a["at"], time::iso(5_000), "{a}");
            assert_eq!(a["self_assigned"], false, "{a}");
        }

        /// No `Actor:` trailer and no `(actor)` suffix: the git author
        /// alone never proves a person, so the entry is `null` — while a
        /// sibling file in the same walk still resolves.
        #[test]
        fn unrecorded_actor_is_null_not_the_git_author() {
            let t = Tracker::new();
            t.commit("D-1", None, "D-1: created", "", 1_000);
            t.commit("D-1", Some("mira"), "D-1: set owner=mira", "", 2_000);
            t.commit("D-2", None, "D-2: created", "", 1_500);
            t.commit(
                "D-2",
                Some("mira"),
                "D-2: set owner=mira",
                "Actor: jordan",
                2_500,
            );
            let out = run(t.dir(), &[("demo/D-1", "mira"), ("demo/D-2", "mira")]);
            assert!(out["demo/D-1"].is_null(), "{}", out["demo/D-1"]);
            assert_eq!(out["demo/D-2"]["actor"], "jordan", "{}", out["demo/D-2"]);
        }

        /// An owner that is not in HEAD (uncommitted working-tree
        /// change, or a value no commit ever wrote) reports `null`.
        #[test]
        fn uncommitted_or_unknown_owner_is_null() {
            let t = Tracker::new();
            t.commit("D-1", None, "D-1: created", "Actor: avery", 1_000);
            // Working tree carries an owner HEAD never did.
            std::fs::write(
                t.dir().join("demo/D-1/issue.md"),
                "---\nid: D-1\ntitle: t\nstatus: doing\npriority: P2\nowner: mira\ncreated: \"x\"\n---\nbody\n",
            )
            .unwrap();
            let out = run(t.dir(), &[("demo/D-1", "mira")]);
            assert!(out["demo/D-1"].is_null(), "{}", out["demo/D-1"]);
            // A never-written owner is null too.
            t.git(&["checkout", "-q", "--", "demo/D-1/issue.md"]);
            let out = run(t.dir(), &[("demo/D-1", "nobody")]);
            assert!(out["demo/D-1"].is_null(), "{}", out["demo/D-1"]);
        }

        /// The walk is scoped to the asked files only: an unlisted
        /// issue's owner commits never appear in the answer.
        #[test]
        fn the_walk_is_scoped_to_the_asked_files() {
            let t = Tracker::new();
            t.commit("D-1", None, "D-1: created", "Actor: avery", 1_000);
            t.commit(
                "D-1",
                Some("mira"),
                "D-1, D-2: set owner=mira",
                "Actor: jordan",
                2_000,
            );
            // D-2 was never asked for: no entry, and its commits stay out.
            let out = run(t.dir(), &[("demo/D-1", "mira")]);
            assert_eq!(out.len(), 1);
            assert_eq!(out["demo/D-1"]["actor"], "jordan", "{}", out["demo/D-1"]);
            assert!(!out.contains_key("demo/D-2"));
        }

        /// A public board write's `Activity-Receipt:` trailer carries
        /// the verified subject: `self_assigned` follows it even when
        /// the `Actor:` trailer is a display string, and a receipt
        /// naming ANOTHER subject refuses self even when the actor
        /// string coincidentally equals the owner.
        #[test]
        fn receipt_sub_decides_self_assignment() {
            let t = Tracker::new();
            t.commit(
                "D-1",
                Some("mira"),
                "D-1: created",
                &format!(
                    "Actor: Mira Chen <mira@x> (board)\nActivity-Receipt: {}",
                    human_receipt("mira", "D-1")
                ),
                1_000,
            );
            // The same actor text but a receipt for someone else.
            t.commit(
                "D-2",
                Some("mira"),
                "D-2: created",
                &format!(
                    "Actor: mira\nActivity-Receipt: {}",
                    human_receipt("jordan", "D-2")
                ),
                2_000,
            );
            let out = run_as(
                t.dir(),
                &[("demo/D-1", "mira"), ("demo/D-2", "mira")],
                Some("mira"),
            );
            assert_eq!(
                out["demo/D-1"]["self_assigned"], true,
                "{}",
                out["demo/D-1"]
            );
            assert_eq!(
                out["demo/D-2"]["self_assigned"], false,
                "{}",
                out["demo/D-2"]
            );
        }

        /// `owner: operator` can never be a self-assignment — it is the
        /// shared authority, so even a recorded `Actor: operator`
        /// reports the handoff without `self_assigned`.
        #[test]
        fn the_operator_authority_is_never_self() {
            let t = Tracker::new();
            t.commit("D-1", None, "D-1: created", "Actor: avery", 1_000);
            t.commit(
                "D-1",
                Some("operator"),
                "D-1: set owner=operator",
                "Actor: operator",
                2_000,
            );
            let out = run(t.dir(), &[("demo/D-1", "operator")]);
            let a = &out["demo/D-1"];
            assert_eq!(a["kind"], "assigned", "{a}");
            assert_eq!(a["actor"], "operator", "{a}");
            assert_eq!(a["self_assigned"], false, "{a}");
        }

        #[test]
        fn invalid_or_duplicated_receipts_never_claim_self() {
            let t = Tracker::new();
            for (index, kind) in ["partial", "missing_role", "duplicate"].iter().enumerate() {
                let id = format!("D-{}", index + 1);
                let mut receipt = human_receipt("mira", &id);
                let claim = match *kind {
                    "partial" => json!({"human": {"sub": "mira"}}).to_string(),
                    "missing_role" => {
                        receipt["human"].as_object_mut().unwrap().remove("role");
                        receipt.to_string()
                    }
                    _ => format!("{receipt}\nActivity-Receipt: {receipt}"),
                };
                t.commit(
                    &id,
                    Some("mira"),
                    &format!("{id}: created"),
                    &format!("Actor: mira\nActivity-Receipt: {claim}"),
                    1_000 + index as i64,
                );
                let rel = format!("demo/{id}");
                let out = run_as(t.dir(), &[(&rel, "mira")], Some("mira"));
                assert_eq!(out[&rel]["actor"], "mira", "{kind}: {out:?}");
                assert_eq!(out[&rel]["self_assigned"], false, "{kind}: {out:?}");
            }
        }

        #[test]
        fn a_colliding_handle_does_not_prove_the_same_person() {
            let t = Tracker::new();
            let subject = "mira/fixture";
            let other_subject = "mira@fixture";
            let owner = crate::ident::sanitized(subject);
            assert_eq!(owner, crate::ident::sanitized(other_subject));
            t.commit(
                "D-1",
                Some(&owner),
                "D-1: created",
                &format!(
                    "Actor: mira\nActivity-Receipt: {}",
                    human_receipt(other_subject, "D-1")
                ),
                1_000,
            );
            let out = run_as(t.dir(), &[("demo/D-1", &owner)], Some(subject));
            assert_eq!(out["demo/D-1"]["self_assigned"], false, "{out:?}");
        }

        /// No tracker repo at all: every asked card reports `null`,
        /// never an error and never a fabricated entry.
        #[test]
        fn a_repo_without_git_reports_null() {
            let dir = tempfile::tempdir().unwrap();
            let out = run(dir.path(), &[("demo/D-1", "mira")]);
            assert!(out["demo/D-1"].is_null(), "{out:?}");
        }
    }
}
