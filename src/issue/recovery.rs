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
        "marker": marker_facts(pm)?,
        "snapshot": snapshot_facts(pm)?,
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
    // The eligible and the protected sets come from the TRACKER, never
    // from the caller's review: a hand-written review must not be able
    // to make a protected path look like the crashed writer's.
    let report = pm.interruption_report()?;
    let leftovers = report.paths;
    let foreign = report.foreign_paths;
    let started = report.crash_started_at;
    let selected = select_paths(pm, &leftovers, &foreign, paths, started)?;
    require_staged_content(pm, choice, &selected)?;
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

/// The outcome of an execution, successful or not.
///
/// A failed execution still leaves an attempt: what it observed, what
/// it changed, and what it could not determine. The operator can
/// confirm it with `recover status --attempt <file>` — a failure is
/// never a state nobody can look at afterwards.
pub struct Execution {
    pub attempt: Value,
    pub error: Option<Error>,
}

/// The execution, with the attempt always produced.
pub fn execute_detailed(
    pm: &Pm,
    review: &Value,
    decision: &Value,
    choice: Choice,
    paths: &[String],
) -> Execution {
    let mut landed: Option<Outcome> = None;
    match execute_inner(pm, review, decision, choice, paths, &mut landed) {
        Ok(attempt) => Execution {
            attempt,
            error: None,
        },
        Err(e) => {
            let attempt = failure_attempt(pm, review, choice, paths, &e, landed.as_ref());
            Execution {
                attempt,
                error: Some(e),
            }
        }
    }
}

/// The attempt a failed execution leaves behind: what was observed,
/// what was changed, and what is unknown. It never claims success.
fn failure_attempt(
    pm: &Pm,
    review: &Value,
    choice: Choice,
    paths: &[String],
    error: &Error,
    landed: Option<&Outcome>,
) -> Value {
    let bound = review.get("state").and_then(Value::as_str).unwrap_or("");
    let remaining = pm
        .interruption_report()
        .map(|r| r.paths)
        .unwrap_or_default();
    let (outcome, committed, message) = match landed {
        Some(o) => (
            "applied_but_unconfirmed",
            o.committed.clone().map(Value::from).unwrap_or(Value::Null),
            format!(
                "the change was applied ({}), but the outcome could not be confirmed: {error}. \
                 The state below is what a fresh observation shows; re-review before doing \
                 anything else. A blind retry could repeat the change.",
                o.message
            ),
        ),
        None => (
            "failed_before_any_change",
            Value::Null,
            format!(
                "the recovery was refused before it changed anything: {error}. Nothing was \
                 applied, so a retry is safe once the reason is dealt with."
            ),
        ),
    };
    json!({
        "state": bound,
        "choice": choice.as_str(),
        "paths": paths,
        "resolved": false,
        "recovery_success": false,
        "outcome": outcome,
        "committed": committed,
        "message": message,
        "error": {
            "kind": error.kind(),
            "code": error.code(),
            "message": error.to_string(),
        },
        "remaining": remaining,
        "next_action": "inspect the attempt and the current state before doing anything else; \
                        a blind retry of the same request is not safe",
    })
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
    match execute_detailed(pm, review, decision, choice, paths) {
        Execution {
            attempt,
            error: None,
        } => Ok(attempt),
        Execution { error: Some(e), .. } => Err(e),
    }
}

fn execute_inner(
    pm: &Pm,
    review: &Value,
    decision: &Value,
    choice: Choice,
    paths: &[String],
    landed: &mut Option<Outcome>,
) -> Result<Value> {
    let bound = require_str(review, "state")?;
    // The eligible and the protected sets come from the TRACKER, never
    // from the caller's review: a hand-written review must not be able
    // to make a protected path look like the crashed writer's.
    let report = pm.interruption_report()?;
    let leftovers = report.paths;
    let foreign = report.foreign_paths;
    let started = report.crash_started_at;
    let confirmed = require_str(decision, "confirmation")?;
    let preview_digest = require_str(decision, "preview_digest")?;
    let selected = select_paths(pm, &leftovers, &foreign, paths, started)?;
    require_staged_content(pm, choice, &selected)?;
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
        // The change landed: a later failure must say so, not claim
        // that nothing happened.
        *landed = Some(outcome.clone());
        Ok(outcome)
    })()?; // The guard is released; the report below is a fresh observation.
    let after = pm.interruption_report()?;
    let remaining: Vec<String> = after.paths.clone();
    let resolved = outcome.resolved && remaining.is_empty();
    Ok(json!({
        "state": bound,
        "choice": choice.as_str(),
        "paths": selected,
        "outcome": "applied",
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
    let hazards = after.hazards.clone();
    // EVERY blocker counts: the path-local leftovers AND the
    // Git-global ones. The tracker is not settled while either stands.
    let blockers = !remaining.is_empty() || !hazards.is_empty();
    // What the attempt did, historically. A keep or a discard that
    // landed stays a fact even if a hazard appeared afterwards; what
    // the CURRENT state allows is a separate question.
    let outcome = attempt
        .get("outcome")
        .and_then(Value::as_str)
        .unwrap_or("applied");
    let settled = outcome == "applied"
        && attempt
            .get("recovery_success")
            .and_then(Value::as_bool)
            .unwrap_or(false);
    let mut out = attempt.clone();
    out["remaining"] = json!(remaining);
    out["hazards"] = json!(hazards);
    out["foreign_count"] = json!(after.foreign.len());
    out["tracker_resolved"] = json!(!blockers);
    out["selected_settled"] = json!(settled);
    out["next_action"] = json!(if !settled {
        "the recovery did not settle the selected paths — inspect the attempt and the current \
         state before doing anything else; a blind retry is not safe"
    } else if blockers {
        "the selected paths were settled, but the tracker still has blockers — resolve them \
         and review again; the blocked operation must not be resumed while any blocker remains"
    } else {
        "the interrupted transaction is settled; the blocked operation can be revalidated and \
         resumed deliberately"
    });
    Ok(out)
}

/// What the index holds for a path: a staged blob, a staged deletion,
/// or nothing staged at all.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Staged {
    Blob(String),
    /// The path is in HEAD and not in the index: the crash staged the
    /// removal.
    Deleted,
    /// Nothing staged: the path exists only in the working tree.
    None,
}

fn staged_state(pm: &Pm, path: &str) -> Staged {
    if let Some(blob) = staged_blob(pm, path).ok().flatten() {
        return Staged::Blob(blob);
    }
    if git(&pm.dir, &["cat-file", "-e", &format!("HEAD:{path}")]).is_ok() {
        return Staged::Deleted;
    }
    Staged::None
}

/// A keep commits the STAGED version. A path with nothing staged has
/// no staged version to keep: keeping it would silently stage the
/// working tree's content instead, which is not what was reviewed.
fn require_staged_content(pm: &Pm, choice: Choice, selected: &[String]) -> Result<()> {
    if choice != Choice::Keep {
        return Ok(());
    }
    for path in selected {
        if staged_state(pm, path) == Staged::None {
            return Err(Error::rejected(format!(
                "there is no inspected staged content for {path} — it exists only in the \
                 working tree, and a keep commits the staged version, never the working \
                 tree's. Choose to discard the path, or leave the transaction unresolved. A \
                 retry unchanged will not help."
            )));
        }
    }
    Ok(())
}

/// What a decision would do, shown before it does anything. Every
/// selected path carries the exact resulting state of each layer — the
/// index entry and the working tree — so a destructive choice can
/// actually be reviewed before it is confirmed.
fn preview_for(pm: &Pm, choice: Choice, selected: &[String]) -> Result<Value> {
    let mut effects = Vec::new();
    for path in selected {
        let staged = staged_blob(pm, path)?;
        let worktree = worktree_facts(pm, path)?;
        let head = head_facts(pm, path)?;
        let effect = match choice {
            Choice::Keep => format!(
                "commit the staged content of {path} (index blob {}), leaving the working \
                 tree's own content untouched; the path's committed state becomes the staged \
                 one",
                staged
                    .clone()
                    .unwrap_or_else(|| { "none — the path has no staged content".to_string() })
            ),
            Choice::Discard => format!(
                "restore {path} to its committed state: the index entry and the working \
                 tree's content are both replaced by the committed version. {}",
                head["effect"].as_str().unwrap_or_default()
            ),
            Choice::Leave => format!("leave {path} exactly as it is"),
        };
        effects.push(json!({
            "path": path,
            "staged_blob": staged,
            "worktree": worktree,
            "head": head,
            "effect": effect,
        }));
    }
    Ok(json!({ "effects": effects }))
}

#[derive(Clone)]
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
    crash_started_at: Option<i64>,
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
        // Every ANCESTOR of the path must be a real directory inside the
        // tracker: a symlinked ancestor would take the write out of it
        // even though the path itself is innocent.
        let mut ancestor = pm.dir.clone();
        let parts: Vec<&str> = norm.split('/').collect();
        for part in parts.iter().take(parts.len().saturating_sub(1)) {
            ancestor = ancestor.join(part);
            if let Ok(meta) = std::fs::symlink_metadata(&ancestor) {
                if meta.file_type().is_symlink() {
                    return Err(Error::rejected(format!(
                        "{norm} is under a symlinked directory ({}) — a recovery will not \
                         follow a link out of the tracker. Inspect and resolve it safely \
                         outside this flow, then obtain a new review. A retry unchanged will \
                         not help.",
                        ancestor
                            .strip_prefix(&pm.dir)
                            .unwrap_or(&ancestor)
                            .display()
                    )));
                }
            }
        }
        if !selected.iter().any(|s| s == norm) {
            selected.push(norm.to_string());
        }
    }
    // Every selected path must be PROVABLY the crashed writer's. Being
    // in the blocked set is refusal evidence, not ownership proof: a
    // path that was already staged before the crash took the lock is
    // in it too. The crash's own marker says when it started, and a
    // path it wrote is newer than that.
    let renames = staged_renames(pm)?;
    {
        use std::os::unix::fs::MetadataExt;
        let index_written = std::fs::metadata(index_path(pm)?)
            .map(|m| m.mtime())
            .unwrap_or(0);
        for path in &selected {
            let Some(started) = crash_started_at else {
                return Err(Error::rejected(format!(
                    "the crashed writer's start time is unknown, so {path} cannot be \
                     attributed to it. A recovery may only act on paths the crash \
                     demonstrably wrote. Leave the transaction unresolved for this path, or \
                     settle it by hand. A retry unchanged will not help."
                )));
            };
            // A rename MOVES a file without writing it, so the evidence
            // for its ends is the index the crash wrote, not the file's
            // own timestamp.
            let is_rename_end = renames.iter().any(|(f, t)| f == path || t == path);
            let written = if is_rename_end {
                index_written
            } else {
                std::fs::symlink_metadata(pm.dir.join(path))
                    .map(|m| m.mtime())
                    .unwrap_or(0)
            };
            if written < started {
                return Err(Error::rejected(format!(
                    "{path} cannot be attributed to the crashed writer: it was last written \
                     before the crash began, so it was already there and the refusal that \
                     named it is not proof that the crash made it. A recovery may only act on \
                     the paths the crash demonstrably wrote. Leave the transaction unresolved \
                     for this path, or settle it by hand. A retry unchanged will not help."
                )));
            }
        }
    }
    selected.sort();
    // A staged rename has TWO ends, and both are affected. Selecting
    // one end is not a selection: keeping only the destination would
    // create a file while the source stays, and discarding only one
    // end would leave the other behind. Both or neither.
    for (from, to) in &renames {
        let from_selected = selected.iter().any(|s| s == from);
        let to_selected = selected.iter().any(|s| s == to);
        if from_selected != to_selected {
            return Err(Error::rejected(format!(
                "the interrupted rename of {from} to {to} has two ends, and both are affected: \
                 selecting only one would change what the rename meant and leave the other \
                 end behind. Select both ends, or leave the transaction unresolved. A retry \
                 unchanged will not help."
            )));
        }
    }
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
    h.update(file_digest(&index_path(pm)?)?.as_bytes());
    h.update(b"\0marker\0");
    h.update(marker_facts(pm)?.to_string().as_bytes());
    h.update(b"\0snapshot\0");
    h.update(snapshot_facts(pm)?.to_string().as_bytes());
    h.update(b"\0leftovers\0");
    for p in leftovers {
        h.update(p.as_bytes());
        h.update(b"\0");
        h.update(worktree_facts(pm, p)?.to_string().as_bytes());
        // The ancestors' identity: a symlink replaced between the
        // review and the execute changes the binding, so the recovery
        // refuses instead of writing through it.
        h.update(ancestor_identity(pm, p)?.as_bytes());
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
fn marker_facts(pm: &Pm) -> Result<Value> {
    let path = pm.dir.join(".write.lock");
    match std::fs::symlink_metadata(&path) {
        Ok(meta) => {
            use std::os::unix::fs::MetadataExt;
            Ok(json!({
                "present": true,
                "bytes": file_digest(&path)?,
                "size": meta.len(),
                "mode": meta.mode(),
                "dev": meta.dev(),
                "ino": meta.ino(),
                "content": std::fs::read_to_string(&path).unwrap_or_default(),
            }))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(json!({ "present": false })),
        Err(e) => Err(Error::internal(format!(
            "{} could not be examined ({e}); the interrupted state cannot be inspected, so no \
             recovery can be authorized.",
            path.display()
        ))),
    }
}

/// The dirty snapshot: the attribution baseline. It is never rewritten.
fn snapshot_facts(pm: &Pm) -> Result<Value> {
    let path = git_dir_of(pm)?.join("cadence-write.dirty.v2");
    match std::fs::symlink_metadata(&path) {
        Ok(meta) => {
            use std::os::unix::fs::MetadataExt;
            Ok(json!({
                "present": true,
                "bytes": file_digest(&path)?,
                "size": meta.len(),
                "mtime": meta.mtime(),
                "mtime_nsec": meta.mtime_nsec(),
                "dev": meta.dev(),
                "ino": meta.ino(),
            }))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(json!({ "present": false })),
        Err(e) => Err(Error::internal(format!(
            "{} could not be examined ({e}); the attribution baseline cannot be read, so no \
             recovery can be authorized.",
            path.display()
        ))),
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
            "staged_content": staged_content(pm, path)?,
            "worktree": worktree_facts(pm, path)?,
            "staged_diff": git(&pm.dir, &["diff", "--cached", "--no-color", "--", path])?,
            "unstaged_diff": git(&pm.dir, &["diff", "--no-color", "--", path])?,
        }));
    }
    Ok(Value::Array(out))
}

/// The identity of every ancestor of a path, from the tracker root
/// down. A recovery must not write through a symlinked ancestor, and a
/// symlink that is REPLACED between the review and the execute has to
/// change the binding — so the ancestors' identity is part of it.
fn ancestor_identity(pm: &Pm, path: &str) -> Result<String> {
    use std::os::unix::fs::MetadataExt;
    let mut out = String::new();
    let mut dir = pm.dir.clone();
    let parts: Vec<&str> = path.split('/').collect();
    for part in parts.iter().take(parts.len().saturating_sub(1)) {
        dir = dir.join(part);
        match std::fs::symlink_metadata(&dir) {
            Ok(meta) => {
                out.push_str(&format!(
                    "{part}:{}:{}:{}:{};",
                    meta.dev(),
                    meta.ino(),
                    meta.mode(),
                    meta.file_type().is_symlink()
                ));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                out.push_str(&format!("{part}:absent;"));
            }
            Err(e) => {
                return Err(Error::internal(format!(
                    "{} could not be examined ({e}); the interrupted state cannot be \
                     inspected, so no recovery can be authorized.",
                    dir.display()
                )))
            }
        }
    }
    Ok(out)
}

/// The staged renames the crashed writer left: (from, to) pairs. Both
/// ends are affected, so a recovery must select both or neither.
fn staged_renames(pm: &Pm) -> Result<Vec<(String, String)>> {
    let out = crate::reaper::output(
        Command::new("git")
            .arg("--no-optional-locks")
            .arg("-C")
            .arg(&pm.dir)
            .args(["status", "--porcelain", "-z"]),
    )
    .map_err(|e| Error::internal(format!("git status could not be run: {e}")))?;
    if !out.status.success() {
        return Err(Error::internal(format!(
            "git status failed in {}: {}",
            pm.dir.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut fields = text.split('\0');
    let mut renames = Vec::new();
    while let Some(rec) = fields.next() {
        if rec.len() < 4 {
            continue;
        }
        let code = &rec[..2];
        if !(code.starts_with('R') || code.starts_with('C')) {
            continue;
        }
        let to = rec[3..].to_string();
        let from = fields.next().unwrap_or_default().to_string();
        renames.push((from, to));
    }
    Ok(renames)
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
                match std::fs::read(&abs) {
                    Ok(bytes) => Some(bytes),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                    Err(e) => {
                        return Err(Error::internal(format!(
                            "{path} could not be read ({e}); its content cannot be inspected, \
                             so no recovery can be authorized. Restore readable evidence, then \
                             review again."
                        )))
                    }
                }
            } else {
                None
            };
            Ok(json!({
                "kind": kind,
                "size": meta.len(),
                "mode": meta.mode(),
                "exists": true,
                "bytes": content.as_ref().map(|c| format!("{:x}", Sha256::digest(c))),
                "content": content_value(content.as_deref()),
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

/// The bytes of a file, in a form a reviewer can actually read: the
/// text when it is text, and the hex when it is not. A binary leftover
/// is never summarized away — the review has to be able to show the
/// whole thing before anyone decides to keep or discard it.
fn content_value(bytes: Option<&[u8]>) -> Value {
    let Some(bytes) = bytes else {
        return Value::Null;
    };
    match std::str::from_utf8(bytes) {
        Ok(text) => json!({ "utf8": text, "binary": false }),
        Err(_) => {
            let mut hex = String::with_capacity(bytes.len() * 2);
            for b in bytes {
                hex.push_str(&format!("{b:02x}"));
            }
            json!({ "hex": hex, "binary": true })
        }
    }
}

/// The content the index holds for a path: what a keep would commit,
/// in a form a reviewer can read. A binary blob is shown in hex, not
/// summarized away.
fn staged_content(pm: &Pm, path: &str) -> Result<Value> {
    let Some(blob) = staged_blob(pm, path).ok().flatten() else {
        return Ok(Value::Null);
    };
    let bytes = git_raw(pm, &["cat-file", "blob", &blob])?;
    Ok(content_value(Some(&bytes)))
}

/// Git's raw output, without the trimming the text helper does: a
/// binary blob's bytes must survive intact.
fn git_raw(pm: &Pm, args: &[&str]) -> Result<Vec<u8>> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(&pm.dir);
    for a in args {
        cmd.arg(a);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = crate::reaper::spawn(&mut cmd)
        .map_err(|_| Error::internal("`git` is required and was not found on PATH"))?;
    let out = child
        .wait_with_output()
        .map_err(|e| Error::internal(format!("git could not be run: {e}")))?;
    if !out.status.success() {
        return Err(Error::internal(format!(
            "git {} failed in {}: {}",
            args.join(" "),
            pm.dir.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(out.stdout)
}

/// The committed state of a path: what a discard would restore it to.
fn head_facts(pm: &Pm, path: &str) -> Result<Value> {
    // Whether the path is in HEAD is answered by the TREE, not by
    // `cat-file -e`: that fails both for a path that is not there and
    // for an object it cannot read, and the second must not look like
    // the first.
    let listing = match git(&pm.dir, &["ls-tree", "HEAD", "--", path]) {
        Ok(l) => l,
        Err(e) => {
            if git(&pm.dir, &["rev-parse", "--verify", "HEAD"]).is_err() {
                String::new()
            } else {
                return Err(Error::internal(format!(
                    "the committed state of {path} could not be read ({e}); the interrupted \
                     state cannot be inspected, so no recovery can be authorized."
                )));
            }
        }
    };
    if listing.trim().is_empty() {
        return Ok(json!({
            "present": false,
            "effect": "the path is not in HEAD: the staged addition and the working-tree file \
                       are both removed, and the path is absent afterwards",
        }));
    }
    let blob = git(&pm.dir, &["rev-parse", &format!("HEAD:{path}")])?;
    let mode = listing
        .split_whitespace()
        .next()
        .map(str::to_string)
        .unwrap_or_default();
    let content = git_raw(pm, &["cat-file", "blob", &format!("HEAD:{path}")]).ok();
    Ok(json!({
        "present": true,
        "blob": blob,
        "mode": mode,
        "content": content_value(content.as_deref()),
        "effect": "the index entry and the working-tree file are both replaced by this \
                   committed version",
    }))
}

fn file_digest(path: &Path) -> Result<String> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(format!("{:x}", Sha256::digest(&bytes))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok("absent".to_string()),
        Err(e) => Err(Error::internal(format!(
            "{} could not be read ({e}); the interrupted state cannot be inspected, so no \
             recovery can be authorized. Restore readable evidence, then review again.",
            path.display()
        ))),
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
    // The tree this commit would carry, before the hook runs.
    let expected = run_git(pm, &env, &[Os::new("write-tree")], None)?;
    // The pre-commit hook, exactly as git would run it, against the
    // content this commit is about to carry.
    run_pre_commit_hook(pm, &git_dir, &tmp_index)?;
    // A hook may change the index — a formatter that stages, a hook
    // that adds a path. The commit must carry exactly what was
    // reviewed: HEAD plus the confirmed staged entries, and nothing
    // else. So the tree is compared, not trusted.
    let tree = run_git(pm, &env, &[Os::new("write-tree")], None)?;
    if tree != expected {
        return Err(Error::rejected(
            "the tracker's pre-commit hook changed the content that was about to be committed. \
             Nothing was committed. Review again so the confirmed content is what the hook \
             sees; a retry unchanged will not help.",
        ));
    }
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
                // The worktree's copy has to be gone for the previewed
                // result to be true. A removal that fails is a partial
                // outcome, not a completed one.
                if let Err(e) = std::fs::remove_file(&abs) {
                    if std::fs::symlink_metadata(&abs).is_ok() {
                        return Err(Error::internal(format!(
                            "the discard of {path} removed the staged addition but its \
                             working-tree file could not be removed ({e}). The change is \
                             PARTLY applied: the index entry is gone and the file is still \
                             there. Nothing was committed, the crash's marker and snapshot are \
                             intact, and the file must be removed by hand before this path is \
                             left alone."
                        )));
                    }
                }
            }
        }
    }
    Ok(())
}

/// Run the repository's effective pre-commit hook against the content
/// the recovery is about to commit. A hook that refuses refuses the
/// recovery: the check is not weakened by committing a different way.
fn run_pre_commit_hook(pm: &Pm, git_dir: &Path, tmp_index: &Path) -> Result<()> {
    let Some(hook) = effective_hook(pm) else {
        return Ok(());
    };
    let Ok(meta) = std::fs::metadata(&hook) else {
        return Ok(());
    };
    if !meta.is_file() {
        return Ok(());
    }
    {
        use std::os::unix::fs::PermissionsExt;
        // Git ignores a hook that is not executable; so does the
        // recovery, or it would run a check the ordinary commit path
        // never runs.
        if meta.permissions().mode() & 0o111 == 0 {
            return Ok(());
        }
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

/// The hook Git would run for this repository: `core.hooksPath` when
/// it is set (a relative path is relative to the repository root),
/// otherwise the repository's hooks directory. An ordinary commit runs
/// that hook, so the recovery must run the same one — the recovery
/// commits through `commit-tree`, which runs no hook of its own.
fn effective_hook(pm: &Pm) -> Option<PathBuf> {
    let dir = match git(&pm.dir, &["config", "--get", "core.hooksPath"]) {
        Ok(p) if !p.trim().is_empty() => {
            let p = PathBuf::from(p.trim());
            if p.is_absolute() {
                p
            } else {
                pm.dir.join(p)
            }
        }
        _ => {
            let rel = git(&pm.dir, &["rev-parse", "--git-path", "hooks"]).ok()?;
            let p = PathBuf::from(rel);
            if p.is_absolute() {
                p
            } else {
                pm.dir.join(p)
            }
        }
    };
    Some(dir.join("pre-commit"))
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
