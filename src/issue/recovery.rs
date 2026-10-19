//! CAD-1601: the operator recovery flow for an interrupted tracker write.
//!
//! A crashed writer leaves the tracker mid-transaction. The crash guard
//! refuses every write that touches the leftovers, so that work stays
//! unusable until somebody decides what the leftovers mean. Before this
//! module the only remedy was to read `git status` by hand and commit or
//! discard the paths yourself — while the tracker refused everything else
//! the crashed writer had left behind.
//!
//! This module is the bounded, safe version of that. It is four steps:
//!
//! 1. **Review** — a complete, read-only look at the interrupted state:
//!    the crashed writer's own paths, the protected foreign paths, the
//!    full staged and unstaged content, and a binding to the exact state
//!    that was inspected.
//! 2. **Decide** — a positively authorized preview of one choice (keep,
//!    discard, leave) over one explicit set of paths, bound to that
//!    review.
//! 3. **Execute** — the one mutation. It re-checks the binding under the
//!    real tracker lock, so a state that moved since the review refuses
//!    instead of acting on a stale decision.
//! 4. **Confirm** — a truthful account of what happened and what is
//!    still unresolved.
//!
//! It never guesses. An attribution it cannot prove is reported as
//! unknown, and a path that is not provably the crashed writer's is
//! refused however explicit the operator's confirmation is. It never
//! auto-commits, auto-discards, deletes a fence by age, bulk resets the
//! tracker, or force-unlocks anything.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::issue::git;
use crate::issue::pmlock::LockState;
use crate::issue::Pm;

#[cfg(test)]
#[path = "recovery_tests.rs"]
mod recovery_tests;

/// What the operator decided to do with the crashed writer's work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    /// Commit the inspected staged content, exactly as it was staged.
    Keep,
    /// Restore the selected paths to their committed state.
    Discard,
    /// Leave the interrupted transaction as it is.
    Leave,
}

impl Choice {
    pub fn parse(s: &str) -> Result<Choice> {
        match s {
            "keep" => Ok(Choice::Keep),
            "discard" => Ok(Choice::Discard),
            "leave" => Ok(Choice::Leave),
            other => Err(Error::rejected(format!(
                "unknown recovery choice '{other}' — use keep, discard or leave"
            ))),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Choice::Keep => "keep",
            Choice::Discard => "discard",
            Choice::Leave => "leave",
        }
    }
}

/// The state binding: a digest over everything a recovery decision
/// depends on. A change in any of it invalidates the review.
const BINDING_VERSION: &str = "cadence-recovery-binding-v1";

/// Step 1 — the complete, read-only review of the interrupted state.
///
/// A review is not an authorization: it says what the tracker holds, and
/// binds that state so a later decision cannot act on something else.
pub fn review(pm: &Pm, blocked_operation: Option<&str>) -> Result<Value> {
    let report = match pm.lock_state() {
        LockState::Held { holder } => {
            return Err(Error::busy(format!(
                "the tracker is held by a live writer ({holder}); an interrupted write cannot \
                 be reviewed while a writer is active. Wait for that writer to finish, then \
                 review again. A retry can help once the lock is released."
            )));
        }
        LockState::LegacyUnknown { holder } => {
            return Err(Error::gate_coded(
                "legacy_write_lock",
                format!(
                    "the tracker's write lock is a legacy file ({holder}) that this build did \
                     not write; an older cadence writer may be live, or may have crashed \
                     holding it. This build will not write beside it and will not remove it, \
                     and retrying will not help. The rollout owner must clear it during a \
                     quiescent migration, after every pre-flock cadence process is stopped."
                ),
            ));
        }
        LockState::IoUnknown(why) => {
            return Err(Error::internal(format!(
                "the tracker's write-lock state could not be read ({why}); the interrupted \
                 state is unknown, so no recovery can be authorized. Restore readable \
                 evidence, then review again. A retry is not safe authority."
            )));
        }
        LockState::Free => {
            return Err(Error::rejected(
                "the tracker has no interrupted write to recover — the write lock is free and \
                 no crashed writer left work behind. If you expected a recovery, check the \
                 tracker path; a retry will not help.",
            ));
        }
        LockState::Interrupted { .. } => pm.interruption_report()?,
    };
    if report.paths.is_empty() {
        return Err(Error::rejected(
            "the interrupted state changed while it was being reviewed — the crashed writer's \
             leftovers are no longer there. Review again to see the current state. A retry of \
             this review may help.",
        ));
    }
    if !report.hazards.is_empty() {
        return Err(Error::rejected(format!(
            "the tracker has a Git-global state that a path-local recovery cannot settle: {}. \
             Resolve the Git operation itself first (finish or abort the merge/rebase, or clear \
             the stale index.lock), then review again. A retry of this recovery will not help.",
            report.hazards.join(", ")
        )));
    }
    let leftovers = report.paths.clone();
    let foreign = report.foreign.clone();
    let foreign_paths = report.foreign_paths.clone();
    let binding = state_binding(pm, &leftovers, &foreign_paths)?;
    let mut out = json!({
        "state": binding,
        "tracker": pm.dir.display().to_string(),
        "git_dir": git_dir_of(pm)?.display().to_string(),
        "head": git(&pm.dir, &["rev-parse", "HEAD"]).unwrap_or_default(),
        "leftovers": leftovers,
        "foreign": foreign,
        "foreign_paths": foreign_paths,
        "foreign_count": foreign.len(),
        "named": report.named,
        "hazards": report.hazards,
        "marker": marker_facts(pm),
        "snapshot": snapshot_facts(pm),
        "paths": path_facts(pm, &leftovers)?,
    });
    if let Some(op) = blocked_operation {
        out["blocked_operation"] = json!(op);
    }
    Ok(out)
}

/// Step 2 — the decision preview.
///
/// It mutates nothing. It proves the paths are the crashed writer's own,
/// shows exactly what the choice would do, and binds the result to the
/// review it was made from.
pub fn decide(pm: &Pm, review: &Value, choice: Choice, paths: &[String]) -> Result<Value> {
    let bound = require_str(review, "state")?;
    let leftovers = require_str_list(review, "leftovers")?;
    let foreign = require_str_list(review, "foreign_paths")?;
    let selected = select_paths(pm, &leftovers, &foreign, paths)?;
    let current = state_binding(pm, &leftovers, &foreign)?;
    if current != bound {
        return Err(Error::conflict_coded(
            "stale_review",
            "the tracker's interrupted state changed since it was reviewed — the review is \
             stale. Review again, then decide again. Retrying this decision unchanged will \
             not help.",
        ));
    }
    let preview = preview_for(pm, choice, &selected)?;
    let preview_digest = digest_of(&preview.to_string());
    let confirmation = digest_of(&format!(
        "{BINDING_VERSION}\0confirmation\0{bound}\0{}\0{}\0{preview_digest}",
        choice.as_str(),
        selected.join("\0")
    ));
    Ok(json!({
        "state": bound,
        "choice": choice.as_str(),
        "paths": selected,
        "confirmation": confirmation,
        "preview_digest": preview_digest,
        "preview": preview,
    }))
}

/// Step 3 — the one mutation.
///
/// It re-proves the bound under the real tracker lock, so a state that
/// moved since the review refuses rather than acting on a stale
/// decision, and a live writer still refuses it.
pub fn execute(
    pm: &Pm,
    review: &Value,
    decision: &Value,
    choice: Choice,
    paths: &[String],
) -> Result<Value> {
    let bound = require_str(review, "state")?;
    let leftovers = require_str_list(review, "leftovers")?;
    let foreign = require_str_list(review, "foreign_paths")?;
    let confirmed = require_str(decision, "confirmation")?;
    let preview_digest = require_str(decision, "preview_digest")?;
    let selected = select_paths(pm, &leftovers, &foreign, paths)?;
    if selected != require_str_list(decision, "paths")? {
        return Err(Error::rejected(
            "the confirmed path set does not match the paths being recovered — a decision \
             covers exactly the set it previewed. Make a new decision for this set. A retry \
             unchanged will not help.",
        ));
    }
    // The real tracker lock: the same kernel flock every other writer
    // takes. A live writer refuses the recovery rather than racing it.
    // Everything the mutation touches happens under the real tracker
    // lock. The report below is a FRESH observation taken after the
    // guard is released: the same flock cannot be probed while this
    // process holds it.
    let outcome = (|| -> Result<Outcome> {
        let _lock = pm.lock_for_recovery()?;
        let current = state_binding(pm, &leftovers, &foreign)?;
        if current != bound {
            return Err(Error::conflict_coded(
                "stale_review",
                "the tracker's interrupted state changed since it was reviewed — the reviewed \
             state is stale and nothing was changed. Review again, then decide again. A retry \
             of this recovery unchanged will not help.",
            ));
        }
        let preview = preview_for(pm, choice, &selected)?;
        let now_preview = digest_of(&preview.to_string());
        if now_preview != preview_digest {
            return Err(Error::rejected(
                "the confirmed decision does not cover this choice and path set — the preview \
             changed. Make a new decision. A retry unchanged will not help.",
            ));
        }
        let expect = digest_of(&format!(
            "{BINDING_VERSION}\0confirmation\0{bound}\0{}\0{}\0{preview_digest}",
            choice.as_str(),
            selected.join("\0")
        ));
        if expect != confirmed {
            return Err(Error::rejected(
                "the confirmation does not match this choice, path set and reviewed state — the \
             decision was not made for this recovery. Make a new explicit decision. A retry \
             unchanged will not help.",
            ));
        }
        let outcome: Outcome = match choice {
            Choice::Leave => Outcome {
                resolved: false,
                committed: None,
                message: "left unresolved: the crashed writer's work and every blocker stay as \
                          they are. No tracker content was changed and no recovery succeeded."
                    .to_string(),
            },
            Choice::Keep => {
                let message = format!(
                    "{}: keep the staged content of {}",
                    selected.join(", "),
                    selected.join(", ")
                );
                let commit = commit_index_state(pm, &selected, &message)?;
                Outcome {
                    resolved: true,
                    committed: Some(commit),
                    message: "kept the inspected staged content: the committed blobs are the \
                              ones the crashed writer staged, and any unstaged content in the \
                              working tree is untouched."
                        .to_string(),
                }
            }
            Choice::Discard => {
                discard_paths(pm, &selected)?;
                Outcome {
                    resolved: true,
                    committed: None,
                    message: "discarded the selected changes: the index and the working tree \
                              for those paths are back to their committed state, and every \
                              other path is untouched."
                        .to_string(),
                }
            }
        };
        Ok(outcome)
    })()?; // The guard is released; the report below is a fresh observation.
    let after = pm.interruption_report()?;
    let remaining: Vec<String> = after.paths.clone();
    let resolved = outcome.resolved && remaining.is_empty();
    Ok(json!({
        "state": bound,
        "choice": choice.as_str(),
        "paths": selected,
        "resolved": resolved,
        "recovery_success": resolved,
        "committed": outcome.committed,
        "message": outcome.message,
        "remaining": remaining,
        "foreign_count": after.foreign.len(),
        "hazards": after.hazards,
    }))
}

/// Step 4 — the truthful account of an attempt.
pub fn status(pm: &Pm, attempt: &Value) -> Result<Value> {
    let after = pm.interruption_report()?;
    let remaining = after.paths.clone();
    let mut out = attempt.clone();
    out["remaining"] = json!(remaining);
    out["foreign_count"] = json!(after.foreign.len());
    out["hazards"] = json!(after.hazards);
    out["tracker_resolved"] = json!(remaining.is_empty());
    let resolved = out["resolved"].as_bool().unwrap_or(false);
    out["next_action"] = json!(if !resolved {
        "the interrupted transaction is still unresolved — review the remaining paths and \
         decide again, or leave them as they are"
    } else if remaining.is_empty() {
        "the interrupted transaction is settled; the blocked operation can be revalidated and \
         resumed deliberately"
    } else {
        "the selected paths are settled, but other leftovers remain unresolved"
    });
    Ok(out)
}

/// What a decision would do, shown before it does anything.
fn preview_for(pm: &Pm, choice: Choice, selected: &[String]) -> Result<Value> {
    let mut effects = Vec::new();
    for path in selected {
        let staged = staged_blob(pm, path)?;
        let worktree = worktree_facts(pm, path)?;
        let effect = match choice {
            Choice::Keep => format!(
                "commit the staged content of {path} (index blob {}), leaving the working \
                 tree's own content untouched",
                staged
                    .clone()
                    .unwrap_or_else(|| "none — the path has no staged content".to_string())
            ),
            Choice::Discard => format!(
                "restore {path} to its committed state: the staged content and the working \
                 tree's content are both replaced by the committed version"
            ),
            Choice::Leave => format!("leave {path} exactly as it is"),
        };
        effects.push(json!({
            "path": path,
            "staged_blob": staged,
            "worktree": worktree,
            "effect": effect,
        }));
    }
    Ok(json!({ "effects": effects }))
}

struct Outcome {
    resolved: bool,
    committed: Option<String>,
    message: String,
}

/// The selected paths must be explicit, literal, and provably the
/// crashed writer's own.
fn select_paths(
    pm: &Pm,
    leftovers: &[String],
    foreign: &[String],
    paths: &[String],
) -> Result<Vec<String>> {
    if paths.is_empty() {
        return Err(Error::rejected(
            "no paths were selected — a recovery must name the exact paths it acts on. \
             Review the leftovers and select them explicitly. A retry unchanged will not help.",
        ));
    }
    let foreign_set: BTreeSet<&str> = foreign.iter().map(String::as_str).collect();
    let leftover_set: BTreeSet<&str> = leftovers.iter().map(String::as_str).collect();
    let mut selected: Vec<String> = Vec::new();
    for raw in paths {
        let path = raw.trim();
        if path.is_empty() || path == "." || path == ".." {
            return Err(Error::rejected(format!(
                "'{raw}' is not a specific path — a recovery acts on literal paths only, never \
                 on a directory or a root. Select the exact paths. A retry unchanged will not \
                 help."
            )));
        }
        if path.contains('*')
            || path.contains('?')
            || path.contains('[')
            || path.contains(']')
            || path.contains('\\')
            || path.contains(':')
        {
            return Err(Error::rejected(format!(
                "'{raw}' looks like a Git pattern or an escape, not a literal path — a recovery \
                 acts on literal paths only. Select the exact paths. A retry unchanged will not \
                 help."
            )));
        }
        if path.split('/').any(|c| c == "..") {
            return Err(Error::rejected(format!(
                "'{raw}' escapes the tracker — a recovery acts on paths inside the tracker \
                 only. Select the exact paths. A retry unchanged will not help."
            )));
        }
        let norm = path.trim_start_matches("./").trim_end_matches('/');
        if norm.is_empty() {
            return Err(Error::rejected(format!(
                "'{raw}' is not a specific path — select the exact paths. A retry unchanged \
                 will not help."
            )));
        }
        if foreign_set.contains(norm) {
            return Err(Error::rejected(format!(
                "{norm} is a protected path: it was already there before the crash, or it \
                 cannot be attributed to it. A recovery may not select it. Select only the \
                 paths the review named as the crashed writer's, or leave the transaction \
                 unresolved. A retry unchanged will not help."
            )));
        }
        if !leftover_set.contains(norm) {
            return Err(Error::rejected(format!(
                "{norm} is not one of the crashed writer's leftovers — the review named \
                 {}. A recovery may only select the paths it reviewed. A retry unchanged will \
                 not help.",
                leftovers.join(", ")
            )));
        }
        // A selected path must be a plain file or a missing path, never
        // a symlink: a link can point outside the tracker.
        let abs = pm.dir.join(norm);
        if let Ok(meta) = std::fs::symlink_metadata(&abs) {
            if meta.file_type().is_symlink() {
                return Err(Error::rejected(format!(
                    "{norm} is a symlink — a recovery will not follow a link out of the \
                     tracker. Inspect and resolve it safely outside this flow, then obtain a \
                     new review. A retry unchanged will not help."
                )));
            }
            if meta.is_dir() {
                return Err(Error::rejected(format!(
                    "{norm} is a directory — a recovery acts on literal file paths, not on \
                     directories and the descendants they imply. Select the exact files. A \
                     retry unchanged will not help."
                )));
            }
        }
        if !selected.iter().any(|s| s == norm) {
            selected.push(norm.to_string());
        }
    }
    selected.sort();
    Ok(selected)
}

/// The binding over everything a decision depends on.
fn state_binding(pm: &Pm, leftovers: &[String], foreign: &[String]) -> Result<String> {
    let mut h = Sha256::new();
    h.update(BINDING_VERSION.as_bytes());
    h.update(b"\0tracker\0");
    h.update(pm.dir.to_string_lossy().as_bytes());
    h.update(b"\0head\0");
    h.update(
        git(&pm.dir, &["rev-parse", "HEAD"])
            .unwrap_or_default()
            .as_bytes(),
    );
    h.update(b"\0index\0");
    h.update(file_digest(&index_path(pm)?).as_bytes());
    h.update(b"\0marker\0");
    h.update(marker_facts(pm).to_string().as_bytes());
    h.update(b"\0snapshot\0");
    h.update(snapshot_facts(pm).to_string().as_bytes());
    h.update(b"\0leftovers\0");
    for p in leftovers {
        h.update(p.as_bytes());
        h.update(b"\0");
        h.update(worktree_facts(pm, p)?.to_string().as_bytes());
        h.update(b"\0");
    }
    h.update(b"\0foreign\0");
    for p in foreign {
        h.update(p.as_bytes());
        h.update(b"\0");
    }
    Ok(format!("{:x}", h.finalize()))
}

fn digest_of(text: &str) -> String {
    let mut h = Sha256::new();
    h.update(text.as_bytes());
    format!("{:x}", h.finalize())
}

fn git_dir_of(pm: &Pm) -> Result<PathBuf> {
    let out = git(&pm.dir, &["rev-parse", "--absolute-git-dir"])?;
    Ok(PathBuf::from(out))
}

fn index_path(pm: &Pm) -> Result<PathBuf> {
    Ok(git_dir_of(pm)?.join("index"))
}

/// The crash marker, as facts: it is evidence, never authority.
fn marker_facts(pm: &Pm) -> Value {
    let path = pm.dir.join(".write.lock");
    match std::fs::symlink_metadata(&path) {
        Ok(meta) => {
            use std::os::unix::fs::MetadataExt;
            json!({
                "present": true,
                "bytes": file_digest(&path),
                "size": meta.len(),
                "mode": meta.mode(),
                "dev": meta.dev(),
                "ino": meta.ino(),
                "content": std::fs::read_to_string(&path).unwrap_or_default(),
            })
        }
        Err(_) => json!({ "present": false }),
    }
}

/// The dirty snapshot: the attribution baseline. It is never rewritten.
fn snapshot_facts(pm: &Pm) -> Value {
    let path = match git_dir_of(pm) {
        Ok(d) => d.join("cadence-write.dirty.v2"),
        Err(_) => return json!({ "present": false }),
    };
    match std::fs::symlink_metadata(&path) {
        Ok(meta) => {
            use std::os::unix::fs::MetadataExt;
            json!({
                "present": true,
                "bytes": file_digest(&path),
                "size": meta.len(),
                "mtime": meta.mtime(),
                "mtime_nsec": meta.mtime_nsec(),
                "dev": meta.dev(),
                "ino": meta.ino(),
            })
        }
        Err(_) => json!({ "present": false }),
    }
}

/// The facts of one path: what the index holds, what the working tree
/// holds, and the complete content of both.
fn path_facts(pm: &Pm, paths: &[String]) -> Result<Value> {
    let mut out = Vec::new();
    for path in paths {
        out.push(json!({
            "path": path,
            "staged_blob": staged_blob(pm, path)?,
            "worktree": worktree_facts(pm, path)?,
            "staged_diff": git(&pm.dir, &["diff", "--cached", "--no-color", "--", path]).ok(),
            "unstaged_diff": git(&pm.dir, &["diff", "--no-color", "--", path]).ok(),
        }));
    }
    Ok(Value::Array(out))
}

/// The blob the index holds for a path: what a keep would commit.
fn staged_blob(pm: &Pm, path: &str) -> Result<Option<String>> {
    let out = git(&pm.dir, &["ls-files", "--stage", "--", path])?;
    let first = out.lines().next().unwrap_or("").trim().to_string();
    if first.is_empty() {
        return Ok(None);
    }
    Ok(first
        .split_whitespace()
        .nth(1)
        .map(str::to_string)
        .filter(|b| !b.is_empty()))
}

/// The working tree's own state for a path.
fn worktree_facts(pm: &Pm, path: &str) -> Result<Value> {
    let abs = pm.dir.join(path);
    match std::fs::symlink_metadata(&abs) {
        Ok(meta) => {
            use std::os::unix::fs::MetadataExt;
            let kind = if meta.file_type().is_symlink() {
                "symlink"
            } else if meta.is_dir() {
                "directory"
            } else {
                "file"
            };
            let content = if kind == "file" {
                std::fs::read(&abs).ok()
            } else {
                None
            };
            Ok(json!({
                "kind": kind,
                "size": meta.len(),
                "mode": meta.mode(),
                "exists": true,
                "bytes": content.as_ref().map(|c| format!("{:x}", Sha256::digest(c))),
                "content": content
                    .as_ref()
                    .and_then(|c| String::from_utf8(c.clone()).ok()),
                "link_target": if kind == "symlink" {
                    std::fs::read_link(&abs).ok().map(|p| p.display().to_string())
                } else {
                    None
                },
            }))
        }
        Err(_) => Ok(json!({ "kind": "absent", "exists": false })),
    }
}

fn file_digest(path: &Path) -> String {
    match std::fs::read(path) {
        Ok(bytes) => format!("{:x}", Sha256::digest(&bytes)),
        Err(_) => "absent".to_string(),
    }
}

/// Commit the INDEX state of `paths` — not the working tree.
///
/// A keep must commit exactly what the crashed writer staged. The
/// ordinary commit path stages the working tree's content first, which
/// would silently commit the unstaged version instead, so the recovery
/// builds the tree from the index itself, in a temporary index, and
/// advances HEAD only if it is still the reviewed one.
fn commit_index_state(pm: &Pm, paths: &[String], message: &str) -> Result<String> {
    let git_dir = git_dir_of(pm)?;
    let head = git(&pm.dir, &["rev-parse", "HEAD"])?;
    let tmp_index = git_dir.join(format!("cadence-recovery-index-{}", std::process::id()));
    let _ = std::fs::remove_file(&tmp_index);
    let env = [("GIT_INDEX_FILE", tmp_index.as_os_str())];
    // The commit's tree: HEAD plus the selected index entries.
    run_git(pm, &env, &[Os::new("read-tree"), Os::new(&head)], None)?;
    for path in paths {
        let entry = git(&pm.dir, &["ls-files", "--stage", "--", path])?;
        if entry.trim().is_empty() {
            // Not in the index: a staged deletion, or a path that only
            // exists in the working tree. It must not enter the commit.
            run_git(
                pm,
                &env,
                &[
                    Os::new("update-index"),
                    Os::new("--force-remove"),
                    Os::new("--"),
                    Os::new(path),
                ],
                None,
            )?;
        } else {
            run_git(
                pm,
                &env,
                &[Os::new("update-index"), Os::new("--index-info")],
                Some(entry.as_bytes()),
            )?;
        }
    }
    // The pre-commit hook, exactly as git would run it, against the
    // content this commit is about to carry.
    run_pre_commit_hook(pm, &git_dir, &tmp_index)?;
    let tree = run_git(pm, &env, &[Os::new("write-tree")], None)?;
    let commit = run_git(
        pm,
        &[],
        &[
            Os::new("-c"),
            Os::new("user.name=cadence"),
            Os::new("-c"),
            Os::new("user.email=cadence@localhost"),
            Os::new("commit-tree"),
            Os::new(&tree),
            Os::new("-p"),
            Os::new(&head),
            Os::new("-m"),
            Os::new(message),
        ],
        None,
    )?;
    // HEAD advances only if it is still the reviewed one.
    run_git(
        pm,
        &[],
        &[
            Os::new("update-ref"),
            Os::new("HEAD"),
            Os::new(&commit),
            Os::new(&head),
        ],
        None,
    )?;
    let _ = std::fs::remove_file(&tmp_index);
    Ok(commit)
}

/// Restore the selected paths to their committed state: the index entry
/// and the working tree's content both go back to HEAD.
fn discard_paths(pm: &Pm, paths: &[String]) -> Result<()> {
    for path in paths {
        let in_head = git(&pm.dir, &["cat-file", "-e", &format!("HEAD:{path}")]).is_ok();
        if in_head {
            run_git(
                pm,
                &[],
                &[
                    Os::new("restore"),
                    Os::new("--source=HEAD"),
                    Os::new("--staged"),
                    Os::new("--worktree"),
                    Os::new("--"),
                    Os::new(path),
                ],
                None,
            )?;
        } else {
            // A file the crash added: remove the staged addition and
            // the working tree's copy, so both are absent.
            run_git(
                pm,
                &[],
                &[
                    Os::new("rm"),
                    Os::new("--cached"),
                    Os::new("--force"),
                    Os::new("--ignore-unmatch"),
                    Os::new("--"),
                    Os::new(path),
                ],
                None,
            )?;
            let abs = pm.dir.join(path);
            if abs.exists() || std::fs::symlink_metadata(&abs).is_ok() {
                let _ = std::fs::remove_file(&abs);
            }
        }
    }
    Ok(())
}

/// Run the repository's own pre-commit hook against the content the
/// recovery is about to commit. A hook that refuses refuses the
/// recovery: the check is not weakened by committing a different way.
fn run_pre_commit_hook(pm: &Pm, git_dir: &Path, tmp_index: &Path) -> Result<()> {
    let hook = git_dir.join("hooks").join("pre-commit");
    let Ok(meta) = std::fs::metadata(&hook) else {
        return Ok(());
    };
    if !meta.is_file() {
        return Ok(());
    }
    let mut cmd = Command::new(&hook);
    cmd.current_dir(&pm.dir)
        .env("GIT_DIR", git_dir)
        .env("GIT_INDEX_FILE", tmp_index)
        .env("GIT_WORK_TREE", &pm.dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = crate::reaper::output(&mut cmd)
        .map_err(|e| Error::internal(format!("the pre-commit hook could not be run: {e}")))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(Error::rejected(format!(
            "the tracker's pre-commit hook refused the recovered content: {}. Nothing was \
             committed. Fix what the hook reports and review again; a retry unchanged will \
             not help.",
            err.trim().lines().take(20).collect::<Vec<_>>().join("; ")
        )));
    }
    Ok(())
}

/// A git argument that may be a flag or a value.
#[derive(Clone)]
struct Os(String);
impl Os {
    fn new(s: &str) -> Os {
        Os(s.to_string())
    }
}
impl AsRef<std::ffi::OsStr> for Os {
    fn as_ref(&self) -> &std::ffi::OsStr {
        self.0.as_ref()
    }
}

fn run_git(
    pm: &Pm,
    env: &[(&str, &std::ffi::OsStr)],
    args: &[Os],
    stdin: Option<&[u8]>,
) -> Result<String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(&pm.dir);
    for (k, v) in env {
        cmd.env(k, v);
    }
    for a in args {
        cmd.arg(a.as_ref());
    }
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    if stdin.is_some() {
        cmd.stdin(Stdio::piped());
    } else {
        cmd.stdin(Stdio::null());
    }
    let mut child = crate::reaper::spawn(&mut cmd)
        .map_err(|_| Error::internal("`git` is required and was not found on PATH"))?;
    if let Some(input) = stdin {
        use std::io::Write;
        if let Some(mut pipe) = child.stdin.take() {
            let _ = pipe.write_all(input);
        }
    }
    let out = child
        .wait_with_output()
        .map_err(|e| Error::internal(format!("git could not be run: {e}")))?;
    if !out.status.success() {
        return Err(Error::internal(format!(
            "git {} failed in {}: {}",
            args.iter()
                .map(|a| a.0.clone())
                .collect::<Vec<_>>()
                .join(" "),
            pm.dir.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn require_str(value: &Value, key: &str) -> Result<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| {
            Error::rejected(format!(
                "the reviewed state is missing '{key}' — review again. A retry unchanged will \
                 not help."
            ))
        })
}

fn require_str_list(value: &Value, key: &str) -> Result<Vec<String>> {
    let list = value.get(key).and_then(Value::as_array).ok_or_else(|| {
        Error::rejected(format!(
            "the reviewed state is missing '{key}' — review again. A retry unchanged will not \
             help."
        ))
    })?;
    Ok(list
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect())
}
