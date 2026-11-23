//! Owner-assignment provenance walking for tracker issue histories.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::{git_bounded, issue_rel, parse_subject};
use crate::issue::{parse, time};
use crate::proc::{run_bounded, BoundedError};

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
pub(super) struct OwnerWalk {
    commits: Vec<OwnerCommit>,
    /// `<project>/<id>` → indices into `commits`, newest-first.
    by_rel: HashMap<String, Vec<usize>>,
}

/// One bounded `git log` over the card files at the pinned `head`.
/// `None` when git cannot answer — the caller reports no provenance.
pub(super) fn owner_walk(
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

pub(super) struct OwnerIdentity<'a> {
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
pub(super) struct OwnerProof<'a> {
    pub(super) pm_dir: &'a Path,
    pub(super) deadline: Instant,
    pub(super) reads_left: usize,
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
    let Ok(receipt) = super::super::canonical_receipt(&raw) else {
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
pub(super) fn assignment_at(
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
