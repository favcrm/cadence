//! `issue sync` — bring a cloned tracker level with `origin`: fetch,
//! rebase the local commits on top, lint the result, push. The store
//! shape makes most concurrent writes merge cleanly (comments and
//! artifacts are create-only, new issues are new folders); when a
//! rebase cannot finish automatically — real conflicts, or a clean
//! merge that fails lint — the tree is restored exactly as found and
//! the report says why, per path.

use std::path::Path;
use std::process::Command;

use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::issue::{git, hooks, lint, Pm};

/// Which side of a conflict wins — named from the operator's seat:
/// `Ours` keeps the local commit's content, `Theirs` takes the fetched
/// remote's. During `git rebase` the flags invert: the commits being
/// replayed are "theirs" and the upstream base is "ours".
#[derive(Clone, Copy)]
pub enum Resolve {
    Ours,
    Theirs,
}

impl Resolve {
    /// The `git checkout` flag that selects this side mid-rebase.
    fn checkout_flag(self) -> &'static str {
        match self {
            Resolve::Ours => "--theirs",
            Resolve::Theirs => "--ours",
        }
    }
}

/// A git probe that returns `None` on failure — for existence checks
/// (`rev-parse --verify`, `merge-base --is-ancestor`) where the answer
/// is data, not an error.
fn probe(dir: &Path, args: &[&str]) -> Option<String> {
    let out = crate::reaper::output(Command::new("git").arg("-C").arg(dir).args(args)).ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// `git rebase --continue`/`--skip`/`--abort` with `GIT_EDITOR=true` —
/// a resolved conflict never opens an editor. `core.hooksPath=/dev/null`
/// keeps the tracker's own hooks out of the sequencer's replays: the
/// post-commit hook would otherwise background-push mid-sequence,
/// leaking commits the lint abort or `--no-push` never meant to send.
const NO_HOOKS: &str = "core.hooksPath=/dev/null";

fn rebase_step(dir: &Path, step: &str) -> Result<String> {
    let out = crate::reaper::output(
        Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", NO_HOOKS, "rebase", step])
            .env("GIT_EDITOR", "true"),
    )
    .map_err(|_| Error::rejected("`git` is required and was not found on PATH"))?;
    if !out.status.success() {
        return Err(Error::rejected(format!(
            "git rebase {step} failed in {}: {}",
            dir.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// True while a rebase sequence is in progress.
fn rebase_in_progress(git_dir: &Path) -> bool {
    git_dir.join("rebase-merge").exists() || git_dir.join("rebase-apply").exists()
}

/// Currently-unmerged paths in the working tree.
fn unmerged(dir: &Path) -> Vec<String> {
    probe(dir, &["diff", "--name-only", "--diff-filter=U"])
        .map(|t| t.lines().map(str::to_string).collect())
        .unwrap_or_default()
}

/// `rev-list --left-right --count upstream...HEAD` → (ahead, behind);
/// `(0, 0)` without an upstream — "behind" is meaningless until one
/// exists.
fn ahead_behind(dir: &Path, upstream: Option<&str>) -> (u64, u64) {
    let Some(up) = upstream else { return (0, 0) };
    let counts = probe(
        dir,
        &[
            "rev-list",
            "--left-right",
            "--count",
            &format!("{up}...HEAD"),
        ],
    )
    .unwrap_or_else(|| "0\t0".to_string());
    let mut parts = counts.split_whitespace();
    let parse = |p: Option<&str>| p.and_then(|n| n.parse().ok()).unwrap_or(0);
    // Left of `...` counts upstream-only commits (behind), right counts
    // HEAD-only ones (ahead).
    let behind = parse(parts.next());
    let ahead = parse(parts.next());
    (ahead, behind)
}

/// The subject of the newest commit on `ref_` touching `path` — the
/// "who moved this file" line in a conflict report.
fn touch_subject(dir: &Path, ref_: &str, path: &str) -> Option<String> {
    probe(dir, &["log", "-1", "--format=%s", ref_, "--", path]).filter(|s| !s.is_empty())
}

/// Per-path conflict detail: the file plus the local and remote commit
/// subjects that last touched it. `local_ref` is `REBASE_HEAD` when a
/// rebase is in progress (the local commit being applied right now),
/// else the pre-rebase HEAD.
fn conflict_detail(dir: &Path, local_ref: &str, upstream: &str) -> Vec<Value> {
    unmerged(dir)
        .iter()
        .map(|path| {
            json!({
                "path": path,
                "local": touch_subject(dir, local_ref, path),
                "remote": touch_subject(dir, upstream, path),
            })
        })
        .collect()
}

/// Undo a completed rebase — `reset --hard` to the recorded pre-rebase
/// head. Safe only because the clean-tree precondition guarantees no
/// uncommitted work exists to lose.
fn undo_rebase(dir: &Path, pre_head: &str) -> Result<()> {
    git(dir, &["reset", "--hard", pre_head])?;
    Ok(())
}

/// The rebase-in-progress markers a sync refuses to trample, plus a
/// merge in progress.
fn in_progress_markers(git_dir: &Path) -> Vec<String> {
    ["rebase-merge", "rebase-apply", "MERGE_HEAD"]
        .iter()
        .map(|m| git_dir.join(m))
        .filter(|p| p.exists())
        .map(|p| p.display().to_string())
        .collect()
}

/// Paths `HEAD` and `upstream` would conflict on, probed with a
/// read-only `git merge-tree --write-tree` (no working-tree changes).
/// With `--name-only` the output's first paragraph is the written
/// tree's oid followed by one path per conflicted file; a later
/// paragraph holds the informational messages. A clean merge lists
/// just the oid.
fn would_conflicts(dir: &Path, upstream: &str) -> Vec<String> {
    let out = crate::reaper::output(Command::new("git").arg("-C").arg(dir).args([
        "merge-tree",
        "--write-tree",
        "--name-only",
        "HEAD",
        upstream,
    ]));
    let Ok(out) = out else { return vec![] };
    let text = String::from_utf8_lossy(&out.stdout);
    text.split("\n\n")
        .next()
        .unwrap_or_default()
        .lines()
        .skip(1) // the written tree's oid
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

/// Drive the resolve loop: take each unmerged path whole from the
/// chosen side, continue; a commit that goes empty is skipped, and a
/// `--continue` that stops at the next conflicting commit just loops
/// until done — that "could not apply" error is the normal signal,
/// not a failure. Bounded by a generous iteration cap — a pathological
/// case errors out rather than spinning.
fn resolve_rebase(dir: &Path, git_dir: &Path, side: Resolve) -> Result<()> {
    let flag = side.checkout_flag();
    for _ in 0..200 {
        let conflicts = unmerged(dir);
        for path in &conflicts {
            if git(dir, &["checkout", flag, "--", path]).is_err() {
                // The chosen side deleted the file — taking it whole
                // means removing it.
                git(dir, &["rm", "-q", "--", path])?;
            } else {
                git(dir, &["add", "--", path])?;
            }
        }
        // Resolution must clear the markers — the same set still
        // unmerged means checkout+rm made no progress; bail.
        if !conflicts.is_empty() && unmerged(dir) == conflicts {
            return Err(Error::rejected(format!(
                "could not resolve conflicts on: {}",
                conflicts.join(", ")
            )));
        }
        let outcome = rebase_step(dir, "--continue");
        if !rebase_in_progress(git_dir) {
            return outcome.map(|_| ());
        }
        // Still mid-sequence: a continue error with unmerged paths is
        // the next commit's conflict (the loop resolves it); with a
        // clean tree the resolved commit went empty and is skipped.
        if outcome.is_err() && unmerged(dir).is_empty() {
            rebase_step(dir, "--skip")?;
            if !rebase_in_progress(git_dir) {
                return Ok(());
            }
        }
    }
    Err(Error::rejected(
        "rebase did not converge after 200 resolved commits — aborting",
    ))
}

/// `issue sync [--no-push] [--dry-run] [--resolve ours|theirs]`.
/// `ok:false` reports are complete syncs that could not finish — the
/// tree is back to its pre-sync state; precondition violations are
/// refusals naming the offending paths.
pub fn run(pm: &Pm, push: bool, dry_run: bool, resolve: Option<Resolve>) -> Result<Value> {
    // The writers' lock — nothing may land in the tracker mid-sync.
    let _lock = pm.lock()?;
    let dir = &pm.dir;
    let git_dir = hooks::git_dir(dir).ok_or_else(|| {
        Error::rejected(format!(
            "{} is not a git repository — `cadence issue init` creates one",
            dir.display()
        ))
    })?;
    git(dir, &["remote", "get-url", "origin"]).map_err(|_| {
        Error::rejected(format!(
            "{} has no `origin` remote — nothing to sync against",
            dir.display()
        ))
    })?;
    let markers = in_progress_markers(&git_dir);
    if !markers.is_empty() {
        return Err(Error::rejected(format!(
            "a rebase or merge is already in progress — finish or abort it \
             first: {}",
            markers.join(", ")
        )));
    }
    let dirty = git(dir, &["status", "--porcelain"])?;
    if !dirty.is_empty() {
        return Err(Error::rejected(format!(
            "working tree is not clean — sync must start untouched:\n{dirty}"
        )));
    }
    let branch = git(dir, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    git(dir, &["fetch", "origin"])?;
    let upstream = format!("origin/{branch}");
    let has_upstream = probe(
        dir,
        &["rev-parse", "--verify", "--quiet", upstream.as_str()],
    )
    .is_some();
    let (mut ahead, mut behind) = ahead_behind(dir, has_upstream.then_some(upstream.as_str()));

    if dry_run {
        let would_conflict = if behind > 0 {
            would_conflicts(dir, &upstream)
        } else {
            vec![]
        };
        return Ok(json!({
            "ok": true, "dry_run": true, "changed": false,
            "branch": branch,
            "upstream": has_upstream.then_some(upstream.clone()),
            "fetched": true,
            "ahead": ahead, "behind": behind,
            "would_conflict": would_conflict,
        }));
    }

    let pre_head = git(dir, &["rev-parse", "HEAD"])?;
    let mut rebased = false;
    // `origin/<branch>` already an ancestor → nothing to rebase; any
    // other shape (behind or diverged) replays the local commits.
    let needs_rebase =
        has_upstream && probe(dir, &["merge-base", "--is-ancestor", &upstream, "HEAD"]).is_none();
    if needs_rebase {
        match git(dir, &["-c", NO_HOOKS, "rebase", &upstream]) {
            // `ahead` is still the pre-rebase count: a strictly-behind
            // rebase just fast-forwards and replays nothing.
            Ok(_) => rebased = ahead > 0,
            Err(rebase_err) => {
                if unmerged(dir).is_empty() {
                    let _ = rebase_step(dir, "--abort");
                    return Err(rebase_err);
                }
                let conflicts = conflict_detail(dir, "REBASE_HEAD", &upstream);
                match resolve {
                    None => {
                        let _ = rebase_step(dir, "--abort");
                        let restored =
                            probe(dir, &["rev-parse", "HEAD"]).is_some_and(|h| h == pre_head);
                        return Ok(json!({
                            "ok": false, "aborted": "conflict",
                            "branch": branch, "upstream": upstream,
                            "fetched": true, "ahead": ahead, "behind": behind,
                            "rebased": false, "pushed": false,
                            "tree_restored": restored,
                            "conflicts": conflicts,
                            "hint": "re-run with `--resolve ours|theirs` to take \
                                     one side whole, or resolve the paths by hand",
                        }));
                    }
                    Some(side) => {
                        if let Err(e) = resolve_rebase(dir, &git_dir, side) {
                            let _ = rebase_step(dir, "--abort");
                            return Err(e);
                        }
                        rebased = true;
                    }
                }
            }
        }
    }

    // The merged result must still pass lint — a component one host
    // removed can leave the other host's issues dangling.
    let lint_report = lint::run(pm, None)?;
    if lint_report["ok"].as_bool() != Some(true) {
        if rebased {
            undo_rebase(dir, &pre_head)?;
        }
        let restored = probe(dir, &["rev-parse", "HEAD"]).is_some_and(|h| h == pre_head);
        return Ok(json!({
            "ok": false, "aborted": "lint",
            "branch": branch, "upstream": has_upstream.then_some(upstream.clone()),
            "fetched": true, "ahead": ahead, "behind": behind,
            "rebased": rebased, "pushed": false,
            "tree_restored": restored,
            "lint": lint_report,
        }));
    }

    let mut pushed = false;
    let mut push_error = Value::Null;
    if push {
        // `pushed` means the remote actually moved — an up-to-date
        // push succeeds without changing anything.
        let before = probe(dir, &["rev-parse", &upstream]);
        match git(dir, &["push", "origin", &branch]) {
            Ok(_) => pushed = probe(dir, &["rev-parse", &upstream]) != before,
            Err(e) => push_error = json!(e.to_string()),
        }
    }
    // Post-state counts: a push levels upstream to HEAD; --no-push
    // leaves `ahead` showing what remains local.
    (ahead, behind) = ahead_behind(dir, has_upstream.then_some(upstream.as_str()));
    Ok(json!({
        "ok": push_error.is_null(),
        "branch": branch,
        "upstream": has_upstream.then_some(upstream),
        "fetched": true,
        "ahead": ahead, "behind": behind,
        "rebased": rebased, "pushed": pushed,
        "push_error": push_error,
        "lint": {"ok": true},
    }))
}
