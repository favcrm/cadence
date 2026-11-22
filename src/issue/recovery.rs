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

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::collections::BTreeSet;

use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::issue::git;
use crate::issue::pmlock::LockState;
use crate::issue::Pm;

mod facts;
mod git_ops;

#[cfg(test)]
use facts::index_path;
use facts::{
    digest_of, git_dir_of, head_facts, marker_facts, path_facts, snapshot_facts, staged_blob,
    staged_renames, state_binding, worktree_facts,
};
use git_ops::{commit_index_state, discard_paths, require_str, require_str_list};
#[cfg(test)]
use git_ops::{materialize_index, remove_file_no_follow};

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
pub(super) const BINDING_VERSION: &str = "cadence-recovery-binding-v1";

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
    if report.sampled {
        return Err(Error::rejected(
            "the crashed writer's leftovers are too many to review as a complete set: the scan \
             stopped at its cap, so the names are a sample. A recovery will not authorize a \
             keep or discard from a sample — a path it did not list could be left behind or \
             changed without being reviewed. Settle this transaction by hand. A retry will not \
             help.",
        ));
    }
    if !report.snapshot_known {
        return Err(Error::rejected(
            "the tracker's acquisition snapshot is missing, or it is not one this build wrote, \
             so no path can be proven to be the crashed writer's. A recovery will not guess: \
             settle this transaction by hand. A retry will not help.",
        ));
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
    if !report.hazards.is_empty() {
        return Err(Error::rejected(format!(
            "the tracker has a Git-global state that a path-local recovery cannot settle: {}. \
             Resolve the Git operation itself first (finish or abort the merge/rebase, or clear \
             the stale index.lock), then review and decide again. A retry will not help.",
            report.hazards.join(", ")
        )));
    }
    let leftovers = report.paths;
    let foreign = report.foreign_paths;
    let selected = select_paths(pm, &leftovers, &foreign, paths)?;
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
        Some(o) if o.partial => (
            "partially_applied",
            o.committed.clone().map(Value::from).unwrap_or(Value::Null),
            format!(
                "the recovery PARTLY applied ({}) and did not finish: {error}. The state below \
                 is what a fresh observation shows. The removed preimages are named, so the \
                 discarded content stays recoverable from the object store. Re-review before \
                 doing anything else; a blind retry is not safe.",
                o.message
            ),
        ),
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
        "removed_blobs": landed.map(|o| o.removed.clone()).unwrap_or_default(),
        "worktree_preimages": landed
            .map(|o| o.worktree_preimages.clone())
            .unwrap_or_default(),
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
    if !report.hazards.is_empty() {
        return Err(Error::rejected(format!(
            "the tracker has a Git-global state that a path-local recovery cannot settle: {}. \
             Resolve the Git operation itself first (finish or abort the merge/rebase, or clear \
             the stale index.lock), then review and decide again. A retry will not help.",
            report.hazards.join(", ")
        )));
    }
    let reviewed: Vec<String> = review
        .get("leftovers")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let gone: Vec<&String> = paths
        .iter()
        .filter(|p| {
            let n = p.trim().trim_start_matches("./").trim_end_matches('/');
            reviewed.iter().any(|r| r == n) && !leftovers.iter().any(|l| l == n)
        })
        .collect();
    if !gone.is_empty() {
        return Err(Error::conflict_coded(
            "stale_review",
            format!(
                "{} was reviewed as the crashed writer's leftover but is no longer in the \
                 tracker's leftover set — the state changed, or another writer settled it. The \
                 review is stale and nothing was changed by this request: review again. A retry \
                 unchanged will not help.",
                gone.iter()
                    .map(|p| p.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ));
    }
    let confirmed = require_str(decision, "confirmation")?;
    let preview_digest = require_str(decision, "preview_digest")?;
    let selected = select_paths(pm, &leftovers, &foreign, paths)?;
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
                partial: false,
                removed: Vec::new(),
                worktree_preimages: Vec::new(),
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
                match commit_index_state(pm, &selected, &message) {
                    Ok(commit) => Outcome {
                        resolved: true,
                        committed: Some(commit),
                        partial: false,
                        removed: Vec::new(),
                        worktree_preimages: Vec::new(),
                        message: "kept the inspected staged content: the committed blobs are the \
                                  ones the crashed writer staged, and any unstaged content in the \
                                  working tree is untouched."
                            .to_string(),
                    },
                    Err(e) => {
                        // A refusal before any commit is not a landing;
                        // an internal failure can be a commit whose ref
                        // update went unconfirmed. Only the second is
                        // recorded as possibly applied.
                        if matches!(e.kind(), "internal" | "unknown") {
                            *landed = Some(Outcome {
                                resolved: false,
                                committed: None,
                                partial: true,
                                removed: Vec::new(),
                                worktree_preimages: Vec::new(),
                                message: "the keep may have been applied: the commit could not \
                                          be confirmed. Inspect the tracker before doing anything \
                                          else."
                                    .to_string(),
                            });
                        }
                        return Err(e);
                    }
                }
            }
            Choice::Discard => {
                // The preimages, captured BEFORE anything is changed:
                // the staged blobs and the working tree's own RAW bytes
                // (no Git filters, so the blob reproduces exactly what
                // the discard would destroy). A capture that fails
                // refuses the discard: without it a partial failure
                // could not be recovered.
                let removed: Vec<String> = selected
                    .iter()
                    .filter_map(|p| staged_blob(pm, p).ok().flatten())
                    .collect();
                let mut worktree_preimages: Vec<Value> = Vec::new();
                for p in &selected {
                    // A path absent from the working tree (a staged
                    // deletion) has no worktree layer to preserve.
                    let state = match std::fs::symlink_metadata(pm.dir.join(p)) {
                        Ok(m) => Some(m),
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                        Err(e) => {
                            return Err(Error::internal(format!(
                                "the state of {p} could not be read ({e}); nothing was \
                                 discarded. A retry is safe once the filesystem is readable."
                            )))
                        }
                    };
                    let Some(meta) = state else { continue };
                    if !meta.is_file() {
                        continue;
                    }
                    let blob = git(&pm.dir, &["hash-object", "-w", "--no-filters", "--", p])
                        .map_err(|e| {
                            Error::internal(format!(
                                "the working-tree content of {p} could not be saved before the \
                                 discard ({e}); NOTHING was discarded. A retry is safe once the \
                                 object store is writable."
                            ))
                        })?;
                    worktree_preimages.push(json!({"path": p, "blob": blob.trim()}));
                }
                match discard_paths(pm, &selected) {
                    Ok(()) => Outcome {
                        resolved: true,
                        committed: None,
                        partial: false,
                        removed,
                        worktree_preimages,
                        message: "discarded the selected changes: the index and the working tree \
                                  for those paths are back to their committed state, and every \
                                  other path is untouched."
                            .to_string(),
                    },
                    Err(e) => {
                        // The change began: the attempt must say so, and
                        // it must keep the preimages.
                        *landed = Some(Outcome {
                            resolved: false,
                            committed: None,
                            partial: true,
                            removed,
                            worktree_preimages,
                            message: "the discard PARTLY applied: some selected paths are back \
                                      to their committed state and the rest were not touched."
                                .to_string(),
                        });
                        return Err(e);
                    }
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
    // Two questions, two answers: the selected paths' completion, and
    // the tracker's settlement (no leftover and no hazard).
    let selected_settled = selected.iter().all(|p| !remaining.contains(p));
    let recovery_success =
        outcome.resolved && selected_settled && remaining.is_empty() && after.hazards.is_empty();
    let resolved = recovery_success;
    Ok(json!({
        "state": bound,
        "choice": choice.as_str(),
        "paths": selected,
        "outcome": "applied",
        "resolved": resolved,
        "selected_settled": selected_settled,
        "recovery_success": recovery_success,
        "committed": outcome.committed,
        "removed_blobs": outcome.removed,
        "worktree_preimages": outcome.worktree_preimages,
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
    // The selected paths' completion and the tracker's settlement are
    // two questions: an attempt that settled its own paths is not a
    // success while another blocker stands.
    let selected_settled = outcome == "applied"
        && attempt
            .get("selected_settled")
            .and_then(Value::as_bool)
            .unwrap_or(false);
    let recovery_success = selected_settled && !blockers;
    let mut out = attempt.clone();
    out["remaining"] = json!(remaining);
    out["hazards"] = json!(hazards);
    out["foreign_count"] = json!(after.foreign.len());
    out["tracker_resolved"] = json!(!blockers);
    out["selected_settled"] = json!(selected_settled);
    out["recovery_success"] = json!(recovery_success);
    out["next_action"] = json!(if !selected_settled {
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
    /// The change began but did not finish. The attempt must say
    /// PARTLY applied then, never "nothing was applied".
    partial: bool,
    /// The staged blobs a discard removed: the preimages that keep a
    /// partial discard recoverable.
    removed: Vec<String>,
    /// The working tree's own bytes a discard removed, saved as blobs
    /// in the object store: the index preimage alone does not make an
    /// unstaged or untracked file recoverable.
    worktree_preimages: Vec<Value>,
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
        // A committed entry that is not a regular file has no restore
        // path this flow can honour: refuse it at selection rather than
        // preview a restoration it cannot apply.
        if let Ok(entry) = git(&pm.dir, &["ls-tree", "HEAD", "--", norm]) {
            let entry = entry.trim();
            let mode = entry.split_whitespace().next().unwrap_or_default();
            if !entry.is_empty() && !mode.starts_with("100") {
                return Err(Error::rejected(format!(
                    "{norm} is a {mode} entry in the committed tree, not a regular file — a \
                     recovery restores regular files only. Restore it by hand and leave the \
                     transaction unresolved. A retry unchanged will not help."
                )));
            }
        }
        if !selected.iter().any(|s| s == norm) {
            selected.push(norm.to_string());
        }
    }
    // Every selected path is PROVABLY the crashed writer's: attribution
    // comes from the acquisition snapshot, in `classify`. Being in the
    // blocked set is refusal evidence, not ownership proof — a path
    // already dirty before the crash took the lock is in the snapshot,
    // and `classify` puts it in the protected set, so it never reaches
    // this function. A timestamp is not ownership evidence.
    let renames = staged_renames(pm)?;
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
