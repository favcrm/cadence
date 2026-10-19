//! History: everything the tracker's own git log can answer about an
//! issue — `log`, `diff`, `blame` and `ls --at` — plus `code_commits`,
//! the same question asked of the project's own repos. Strictly
//! read-only: no lock, no commit, no fetch; a tracker with no remote
//! works. The only stored state is the git history that already exists.

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

use crate::error::{Error, Result};
use crate::issue::model::{self, Front};
use crate::issue::{board, git, hooks, parse, project, time};
use crate::proc::{run_bounded, BoundedError};

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
fn issue_rel(project_key: &str, id: &str) -> String {
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
fn parse_subject(subject: &str, id: &str) -> Option<(&'static str, String, String)> {
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
struct TempExport(PathBuf);

impl TempExport {
    fn create() -> Result<Self> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path =
            std::env::temp_dir().join(format!("cadence-issue-at-{}-{nanos}", std::process::id()));
        std::fs::create_dir(&path)?;
        Ok(Self(path))
    }
}

impl Drop for TempExport {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
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
            .arg(&export.0)
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
    let issues = board::load_all(&export.0, project_key)?;
    // A notes dir that never exists — `derive`/`chain` return empty,
    // so status falls through to the file field exactly.
    let views = board::views(&export.0.join(".no-notes"), issues);
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

/// CAD-383: when the issue's `owner:` line last changed — the claim age
/// of an issue owned before claims were recorded: the author time of
/// the newest tracker commit whose diff adds or removes an `owner:`
/// line in its `issue.md`, one bounded `git log -G`. For one issue's
/// claim check; `status`/`overview` read every issue's from the cached
/// [`crate::issue::line_times::LineTimes`] instead (CAD-403).
pub fn owner_changed_at(
    pm_dir: &Path,
    project_key: &str,
    id: &str,
    timeout: Duration,
) -> Option<i64> {
    let rel = format!("{}/issue.md", issue_rel(project_key, id));
    let out = git_bounded(
        pm_dir,
        &["log", "-1", "--format=%at", "-G", "^owner:", "--", &rel],
        timeout,
    )
    .ok()?;
    out.trim().parse().ok()
}

/// AOS-274: the recorded assignment of a caller's issues, for My work.
/// One bounded, literal-path file-history walk follows HEAD's first-parent
/// lineage. Every candidate compares parsed owners before and after:
/// YAML formatting and subject tokens cannot establish a handoff, and a
/// branch-local roundtrip does not reset an unchanged merged owner.
/// Causal commit order, not author time, resolves same-second transitions.
///
/// Every file's entry is the newest commit whose `owner:` becomes the
/// caller's: `{kind: 'assigned'|'initial_owner', actor, at,
/// self_assigned}` — `initial_owner` only when the file is proven absent
/// at the parent or the commit is a true root, `assigned` otherwise.
/// The entry is `null` — never a guess — when the
/// transition cannot be proven: no tracker repo or failed walk, an
/// uncommitted or never-committed owner, an unparseable `issue.md` in
/// the scan, a transition buried past the bounds, a corrupt or
/// unreadable object, or a commit with no trustworthy actor (the git
/// author alone never proves a person). `self_assigned` is true only
/// when the commit's `Activity-Receipt:` trailer `human.sub` is the
/// caller's own verified subject exactly (never a sanitization or
/// alias match) while the recorded owner IS that subject's canonical
/// handle — `operator`, agents, display spellings and receipt-free
/// commits can never be "self".
///
/// `cards` is `(issue_rel, recorded owner)` per owned card already
/// filtered to the caller's readable projects, scanned in the
/// caller's order so the shared read budget spends identically on
/// identical requests; `subject` is the caller's exact verified
/// platform subject when the session carries one (`None` for the
/// shared operator or any unverifiable caller, so `self_assigned` is
/// then always false); `timeout` is ONE deadline shared by the HEAD
/// pin, the log walk and every git read the proofs make, and
/// `show_budget` bounds how many of those reads run at all. A read
/// that cannot finish inside its bounds leaves that provenance
/// `null` — an absent answer, never a refusal or a half-proven
/// tenure: an entry appears only after its whole before/after chain
/// verified at the pinned HEAD.
pub fn owner_assignments(
    pm_dir: &Path,
    cards: &[(String, &str)],
    subject: Option<&str>,
    timeout: Duration,
    show_budget: usize,
) -> HashMap<String, Value> {
    if cards.is_empty() {
        return HashMap::new();
    }
    // Every asked card answers — `null` IS the unproven answer, so a
    // caller never distinguishes "walk failed" from "not asked".
    let null_entries = || {
        cards
            .iter()
            .map(|(rel, _)| (rel.clone(), Value::Null))
            .collect::<HashMap<String, Value>>()
    };
    let deadline = Instant::now() + timeout;
    // Pin the revision the whole proof walks: the candidate log and
    // every before/after `issue.md` read name THIS commit, so a HEAD
    // that moves mid-read cannot mix two tenures into one answer.
    let Some(head) = provenance_rev(pm_dir, deadline) else {
        return null_entries();
    };
    let Some(walk) = owner_walk(pm_dir, &head, cards, deadline) else {
        return null_entries();
    };
    let identity = subject.map(|subject| OwnerIdentity {
        subject,
        handle: crate::ident::sanitized(subject),
    });
    let mut proof = OwnerProof {
        pm_dir,
        deadline,
        reads_left: show_budget,
    };
    // Input card order — identical requests spend the shared budget
    // identically, so the same rows are never unavailable by
    // HashMap iteration order.
    let entries: HashMap<String, Value> = cards
        .iter()
        .map(|(rel, owner)| {
            (
                rel.clone(),
                assignment_at(&mut proof, &head, rel, owner, identity.as_ref(), &walk),
            )
        })
        .collect();
    // The pinned revision must still be HEAD to publish: a write that
    // landed mid-scan mixed owners no single proof covers, so the
    // whole answer is unproven rather than half-old.
    match provenance_rev(pm_dir, deadline) {
        Some(now) if now == head => entries,
        _ => null_entries(),
    }
}

/// The tracker's pinned HEAD commit for one provenance read — the rev
/// the walk and every before/after proof is taken at, compared again
/// before the answer is published. `None` when git cannot answer
/// within the shared deadline (no repo, no HEAD, timeout).
fn provenance_rev(pm_dir: &Path, deadline: Instant) -> Option<String> {
    let (ok, out) = provenance_git(
        pm_dir,
        &["rev-parse", "--verify", "--quiet", "HEAD^{commit}"],
        deadline,
    )
    .ok()?;
    let head = out.trim().to_string();
    (ok && !head.is_empty()).then_some(head)
}

/// `git -C <pm_dir> <args>` within what is left of `deadline` — the
/// one clock the pin, the log walk and every `cat-file`/`show` share,
/// so the whole provenance read is bounded by `timeout` total plus
/// `show_budget` git calls, never by per-command sums. `Ok` carries
/// the exit status' success and stdout.
fn provenance_git(
    pm_dir: &Path,
    args: &[&str],
    deadline: Instant,
) -> std::result::Result<(bool, String), String> {
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return Err("owner provenance: out of time".to_string());
    }
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(pm_dir).args(args);
    match run_bounded(&mut cmd, left) {
        Ok(out) => String::from_utf8(out.stdout)
            .map(|text| (out.status.success(), text))
            .map_err(|e| format!("owner provenance: invalid UTF-8: {e}")),
        Err(BoundedError::Spawn(e)) => Err(format!("owner provenance: spawn git: {e}")),
        Err(e) => Err(format!("owner provenance: {e}")),
    }
}

/// Bound file-changing candidates per issue; non-owner edits still count,
/// so long histories become unavailable rather than skipping transitions.
const ASSIGNMENT_WALKS_PER_FILE: usize = 8;

/// One scoped history row. The git author is deliberately absent:
/// only the explicit actor and canonical human receipt can name a person.
struct OwnerCommit {
    /// Full sha — the `git show` rev.
    full: String,
    /// RFC 3339 UTC author time (`%at` through `time::iso`).
    at: String,
    /// `%s` — the raw subject, source of the ` (actor)` suffix.
    subject: String,
    /// `%(trailers:key=Actor)` — the recorded actor, else empty.
    actor: String,
    /// `%(trailers:key=Activity-Receipt)` — the CAD-1374 receipt JSON
    /// public board writes carry, else empty.
    receipt: String,
}

/// The parsed walk: every file-changing commit once (`commits`,
/// newest-first) and, per wanted file, the indices into it — commit
/// metadata is shared, never re-parsed or cloned per file.
struct OwnerWalk {
    commits: Vec<OwnerCommit>,
    /// `<project>/<id>` → indices into `commits`, newest-first.
    by_rel: HashMap<String, Vec<usize>>,
}

/// One bounded `git log` over the card files at the pinned `head`.
/// `None` when git cannot answer — the caller reports no provenance.
fn owner_walk(
    pm_dir: &Path,
    head: &str,
    cards: &[(String, &str)],
    deadline: Instant,
) -> Option<OwnerWalk> {
    let mut args: Vec<Cow<'_, str>> = vec![
        "-c".into(),
        "core.quotePath=false".into(),
        "log".into(),
        "--first-parent".into(),
        "--diff-merges=first-parent".into(),
        "--no-renames".into(),
        "--max-count=2000".into(),
        "--format=%x00%H%x1f%at%x1f%s%x1f%(trailers:key=Actor,valueonly,separator=%x1e)%x1f%(trailers:key=Activity-Receipt,valueonly,separator=%x1e)".into(),
        "--name-only".into(),
        head.into(),
        "--".into(),
    ];
    for (rel, _) in cards {
        // `:(literal)` — these are exact scoped file paths, never glob
        // patterns, even if a project key or id carried magic chars.
        args.push(format!(":(literal){rel}/issue.md").into());
    }
    let (ok, out) = provenance_git(
        pm_dir,
        &args.iter().map(Cow::as_ref).collect::<Vec<_>>(),
        deadline,
    )
    .ok()?;
    if !ok {
        return None;
    }
    let wanted: HashMap<&str, ()> = cards.iter().map(|(rel, _)| (rel.as_str(), ())).collect();
    let mut walk = OwnerWalk {
        commits: Vec::new(),
        by_rel: HashMap::new(),
    };
    let mut current = None;
    for line in out.lines() {
        if let Some(header) = line.strip_prefix('\x00') {
            let mut f = header.splitn(5, '\x1f');
            current = match (f.next(), f.next(), f.next(), f.next(), f.next()) {
                (Some(full), Some(at), Some(subject), Some(actor), Some(receipt)) => {
                    let index = walk.commits.len();
                    walk.commits.push(OwnerCommit {
                        full: full.to_string(),
                        at: at
                            .parse::<i64>()
                            .map(time::iso)
                            .unwrap_or_else(|_| at.to_string()),
                        subject: subject.to_string(),
                        // More than one Actor trailer: the first wins —
                        // the writers emit exactly one.
                        actor: actor
                            .trim()
                            .split('\x1e')
                            .next()
                            .unwrap_or_default()
                            .to_string(),
                        receipt: receipt.trim().to_string(),
                    });
                    Some(index)
                }
                // A malformed header attaches its file list to nothing.
                _ => None,
            };
        } else if let (Some(index), false) = (current, line.is_empty()) {
            if let Some(rel) = line.strip_suffix("/issue.md") {
                if wanted.contains_key(rel) {
                    walk.by_rel.entry(rel.to_string()).or_default().push(index);
                }
            }
        }
    }
    Some(walk)
}

struct OwnerIdentity<'a> {
    subject: &'a str,
    handle: String,
}

enum OwnerAt {
    Present(Option<String>),
    Absent,
    Unresolved,
    Unavailable,
}

/// One deadline and command budget for every file proof, including
/// tree lookups and root checks. No child command bypasses this gate.
struct OwnerProof<'a> {
    pm_dir: &'a Path,
    deadline: Instant,
    reads_left: usize,
}

impl OwnerProof<'_> {
    fn git(&mut self, args: &[&str]) -> Option<(bool, String)> {
        self.reads_left = self.reads_left.checked_sub(1)?;
        provenance_git(self.pm_dir, args, self.deadline).ok()
    }

    fn owner_at(&mut self, rev: &str, rel_file: &str) -> OwnerAt {
        let Some((ok, tree)) =
            self.git(&["--literal-pathspecs", "ls-tree", "-z", rev, "--", rel_file])
        else {
            return OwnerAt::Unavailable;
        };
        if !ok {
            return OwnerAt::Unresolved;
        }
        if tree.is_empty() {
            return OwnerAt::Absent;
        }
        if !tree.starts_with("100644 blob ") && !tree.starts_with("100755 blob ") {
            return OwnerAt::Unavailable;
        }
        // A tree entry proves the file exists. A missing/corrupt blob
        // is unavailable, never evidence that the file was absent.
        let spec = format!("{rev}:{rel_file}");
        match self.git(&["show", &spec]) {
            Some((true, text)) => match parse::parse_issue(&text) {
                Ok((front, _)) => OwnerAt::Present(front.owner),
                Err(_) => OwnerAt::Unavailable,
            },
            _ => OwnerAt::Unavailable,
        }
    }

    fn is_root(&mut self, sha: &str) -> bool {
        match self.git(&["cat-file", "commit", sha]) {
            Some((true, body)) => !body
                .lines()
                .take_while(|line| !line.is_empty())
                .any(|line| line.starts_with("parent ")),
            _ => false,
        }
    }
}

/// The actor a proven owner transition may name: the `Actor:` trailer,
/// else the ` (actor)` subject suffix of a cadence-shaped subject.
/// The git author is never trusted — it is a machine property, not a
/// recorded person — so its absence leaves the entry `null`.
fn trusted_actor(commit: &OwnerCommit, id: &str) -> Option<String> {
    if !commit.actor.is_empty() {
        return Some(commit.actor.clone());
    }
    parse_subject(&commit.subject, id)
        .map(|(_, _, actor)| actor)
        .filter(|actor| !actor.is_empty())
}

/// Only a canonical recorded human context can prove self-assignment;
/// display aliases, partial/duplicated receipts and the shared operator
/// spelling cannot. Reuse the writer's validator rather than accepting
/// a second, weaker receipt shape for this projection.
fn self_assigned(commit: &OwnerCommit, owner: &str, identity: Option<&OwnerIdentity<'_>>) -> bool {
    let Some(identity) = identity else {
        return false;
    };
    if owner == "operator"
        || owner != identity.handle
        || !crate::ident::is_handle(owner, crate::ident::HANDLE_MAX)
        || commit.receipt.contains('\x1e')
    {
        return false;
    }
    let Ok(raw) = serde_json::from_str::<Value>(&commit.receipt) else {
        return false;
    };
    let Ok(receipt) = super::canonical_receipt(&raw) else {
        return false;
    };
    receipt.pointer("/human/sub").and_then(Value::as_str) == Some(identity.subject)
}

/// The one card's `assignment` value: the newest candidate commit
/// whose parsed `owner:` becomes the card's owner, verified against
/// the file before and after — all at the pinned `head`. Scans
/// newest-first by commit order, so the newest real transition wins
/// even when commits share an author second. Any scan step that
/// cannot complete (budget, deadline, unparseable file, inconsistent
/// owner, unproven parent) is `null` — a partially proven tenure is
/// never emitted.
fn assignment_at(
    proof: &mut OwnerProof<'_>,
    head: &str,
    rel: &str,
    owner: &str,
    identity: Option<&OwnerIdentity<'_>>,
    walk: &OwnerWalk,
) -> Value {
    let rel_file = format!("{rel}/issue.md");
    let id = rel.rsplit('/').next().unwrap_or_default();
    // An uncommitted owner has no recorded assignment to point at.
    match proof.owner_at(head, &rel_file) {
        OwnerAt::Present(current) if current.as_deref() == Some(owner) => {}
        _ => return Value::Null,
    }
    let indices = walk.by_rel.get(rel).map(Vec::as_slice).unwrap_or(&[]);
    for &index in indices.iter().take(ASSIGNMENT_WALKS_PER_FILE) {
        let commit = &walk.commits[index];
        if commit.full != head {
            match proof.owner_at(&commit.full, &rel_file) {
                OwnerAt::Present(current) if current.as_deref() == Some(owner) => {}
                _ => return Value::Null,
            }
        }
        let parent = format!("{}^", commit.full);
        let initial = match proof.owner_at(&parent, &rel_file) {
            // Same owner before and after: a quoting rewrite or a
            // moved line, not a new assignment. Continue the tenure.
            OwnerAt::Present(previous) if previous.as_deref() == Some(owner) => continue,
            OwnerAt::Present(_) => false,
            OwnerAt::Absent => true,
            OwnerAt::Unresolved if proof.is_root(&commit.full) => true,
            _ => return Value::Null,
        };
        let Some(actor) = trusted_actor(commit, id) else {
            return Value::Null;
        };
        return json!({
            "kind": if initial { "initial_owner" } else { "assigned" },
            "actor": actor,
            "at": commit.at,
            "self_assigned": self_assigned(commit, owner, identity),
        });
    }
    Value::Null
}

/// `git log --all -n <scan>` under `dir` with a hard timeout — project
/// repos are user paths, not ours, so a slow or locked repo must not
/// stall a detail read. Returns the log text or a skip reason.
fn git_bounded(
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

/// Tag `(full sha, commit)` candidates from one repo with `on_default`
/// and drop each not-on-default commit that has a same-subject twin on
/// the default branch. One `rev-list --no-walk <candidates> --not
/// <default>` names the commits the default branch does not reach.
fn dedupe_stale(dir: &Path, hits: Vec<(&str, Value)>) -> Vec<Value> {
    if hits.is_empty() {
        return Vec::new();
    }
    let default = default_ref(dir);
    let mut args = vec!["rev-list", "--no-walk"];
    args.extend(hits.iter().map(|(full, _)| *full));
    args.extend(["--not", default.as_str()]);
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
        if !dir.is_dir() || hooks::git_dir(&dir).is_none() {
            skipped.push(json!({"repo": label, "reason": "not a git repo"}));
            continue;
        }
        let out = git_bounded(
            &dir,
            &[
                "log",
                "--all",
                "-n",
                "2000",
                "--format=%H%x1f%h%x1f%at%x1f%an%x1f%s%x1f%(trailers:key=Issue,valueonly,separator=%x2C)",
            ],
            Duration::from_secs(5),
        );
        match out {
            Err(reason) => skipped.push(json!({"repo": label, "reason": reason})),
            Ok(log) => {
                let hits: Vec<(&str, Value)> = log
                    .lines()
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
                                    "repo": label,
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
                    .collect();
                commits.append(&mut dedupe_stale(&dir, hits));
            }
        }
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
