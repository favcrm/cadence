//! `issue finish` orchestration.

use super::*;

/// What one `issue finish <ID>` does — the CLI flags, and each sweep
/// row's (which never forces).
pub(crate) struct FinishArgs<'a> {
    pub force: bool,
    pub keep_branch: bool,
    pub remote: bool,
    /// Finish exactly this open worktree ref — `--worktree`, and each
    /// sweep row. Required when the issue has several.
    pub worktree: Option<&'a Path>,
    /// `--worktree` only (CAD-274): a recorded directory that is
    /// already gone closes its worktree/branch refs in one tracker
    /// commit and touches no git state — the branch, if any, is kept.
    /// The sweep never sets it: its missing-dir rows stay open.
    pub close_if_gone: bool,
}

struct FinishCtx<'a> {
    pm: &'a Pm,
    id: &'a str,
    args: &'a FinishArgs<'a>,
    actor: &'a str,
    state_dir: &'a Path,
}

struct FinishProbe {
    target: Target,
    ev: Evidence,
    refs_only: bool,
    keep_branch: bool,
    remote: bool,
    remote_covered: bool,
    merged_by: Value,
    merged_how_present: bool,
    tip: Option<String>,
    overridden: Vec<String>,
}

enum ProbeOutcome {
    EarlyReturn(Value),
    Ready(Box<FinishProbe>),
}

struct RemoteFinishOutcome {
    deleted: bool,
    note: Value,
    lifecycle_warning: Option<String>,
    force: bool,
}

struct RemovalOutcome {
    removed_worktree: bool,
    branch_kept: bool,
    deleted_branch: bool,
    branch_note: Value,
    disposition: String,
    release_warning: Option<String>,
}

fn already_finished_json(id: &str) -> Value {
    json!({"issue": id, "finished": false, "reason": "worktree already finished"})
}

fn probe_finish_unlocked(ctx: &FinishCtx<'_>, view: &DaemonView) -> Result<ProbeOutcome> {
    let target = match resolve(ctx.pm, ctx.id, ctx.args.worktree)? {
        Resolve::Nothing => {
            return Err(Error::rejected(format!(
                "{}: no worktree/branch refs recorded — nothing to finish",
                ctx.id
            )))
        }
        Resolve::Finished => return Ok(ProbeOutcome::EarlyReturn(already_finished_json(ctx.id))),
        Resolve::Target(t) => *t,
    };
    if target.wt_dir.is_none()
        && !target.branch.is_empty()
        && ctx.args.keep_branch
        && !ctx.args.remote
    {
        return Ok(ProbeOutcome::EarlyReturn(json!({
            "issue": ctx.id,
            "finished": false,
            "branch": target.branch,
            "kept_branch": true,
            "deleted_branch": false,
            "removed_worktree": false,
            "reason": "no managed checkout path; branch/ref retained; use issue start with recorded lane/name to reattach before finishing"
        })));
    }
    // A gone directory under `--worktree` only closes refs: nothing
    // is removed or deleted, so no branch or remote is touched and
    // survivability has nothing to protect.
    let refs_only = ctx.args.close_if_gone && target.wt_dir.as_deref().is_some_and(|d| !d.is_dir());
    let keep_branch = ctx.args.keep_branch || refs_only;
    let remote = ctx.args.remote && !refs_only;
    if refs_only {
        let path = target.wt_dir.as_deref().ok_or_else(|| {
            Error::rejected(format!("issue {} has no path for refs-only finish", ctx.id))
        })?;
        verify_refs_only_ownership(
            &target.root,
            path,
            ctx.id,
            (!target.branch.is_empty()).then_some(target.branch.as_str()),
        )?;
    } else if let Some(path) = target.wt_dir.as_deref() {
        let _ = verify_checkout_ownership(
            &target.root,
            path,
            ctx.id,
            (!target.branch.is_empty()).then_some(target.branch.as_str()),
        )?;
    } else if !target.branch.is_empty() && (!keep_branch || remote) {
        return Err(Error::rejected(format!(
            "issue {} has no managed checkout path for branch {}; refusing deletion",
            ctx.id, target.branch
        )));
    }
    let ev = evidence(&target, remote);
    let (merged_how, tip, _) = survivability(&ev, &target.root, &target.branch);
    // The --remote coverage decision is probe data too — `gh` runs
    // unlocked. Merge evidence must cover the FETCHED remote tip;
    // survivability's "pushed" is not enough (origin can hold commits
    // no evidence covers — another agent pushing ahead — and an
    // unmerged-but-pushed branch's last copy IS that remote).
    let remote_covered = remote
        && ev.remote_cmp
        && merged_how.is_some()
        && ev
            .remote_tip
            .as_deref()
            .zip(ev.into.as_deref())
            .is_some_and(|(r, into)| merge_rule(&target.root, &target.branch, r, into).is_some());
    // The per-worktree guard + dirty + survivability, evaluated once.
    // `--force` records every block it bypasses; without it the first
    // block refuses, naming its pid or message id.
    let check = inspect(view, ctx.state_dir, &target, &ev);
    let mut overridden = Vec::new();
    for b in check.blocks {
        if refs_only && matches!(b.tag.as_str(), "not-started" | "unmerged-unpushed") {
            continue;
        }
        let safety_probe_failed = matches!(
            b.tag.as_str(),
            "process-enumeration-failed"
                | "daemon-unreachable"
                | "agent-check-inconclusive"
                | "activity-check-failed"
        ) || b.tag.starts_with("dirty-check-failed:");
        if safety_probe_failed && !refs_only {
            return Err(Error::rejected(format!(
                "{} — failed safety enumeration cannot be overridden with --force",
                b.reason
            )));
        }
        if ctx.args.force {
            overridden.push(b.tag);
        } else {
            return Err(Error::rejected(b.reason));
        }
    }
    let merged_by = merged_how.map_or(Value::Null, |h| json!(h));
    let merged_how_present = merged_how.is_some();
    Ok(ProbeOutcome::Ready(Box::new(FinishProbe {
        target,
        ev,
        refs_only,
        keep_branch,
        remote,
        remote_covered,
        merged_by,
        merged_how_present,
        tip,
        overridden,
    })))
}

fn revalidate_live_process_use(
    wt_dir: Option<&Path>,
    refs_only: bool,
    force: bool,
    overridden: &mut Vec<String>,
) -> Result<Vec<u32>> {
    // Local live-use is rechecked at the destructive boundary. Daemon and
    // tracker bindings were probed unlocked above; new tracker refs make the
    // stale-probe check fail, while process cwd/FD use can change without a
    // tracker write.
    let mut excused_pids: Vec<u32> = Vec::new();
    if refs_only {
        return Ok(excused_pids);
    }
    if let Some(d) = wt_dir.filter(|d| d.is_dir()) {
        let scanned = process_use_under(d);
        if let Ok(uses) = &scanned {
            excused_pids = uses.excused.clone();
        }
        match scanned {
            Err(e) => {
                return Err(Error::rejected(format!(
                    "Cannot revalidate process cwd/open-fd use for {} ({e}) — refusing deletion; --force cannot override failed enumeration",
                    d.display()
                )))
            }
            Ok(uses) if uses.enumeration_error.is_some() => {
                return Err(Error::rejected(format!(
                    "Cannot fully revalidate process cwd/open-fd use for {} ({}) — refusing deletion; --force cannot override failed enumeration",
                    d.display(),
                    uses.enumeration_error
                        .as_deref()
                        .unwrap_or("unknown process enumeration failure")
                )))
            }
            Ok(uses) if !uses.cwd.is_empty() || !uses.fd.is_empty() => {
                let pid = uses
                    .cwd
                    .first()
                    .or_else(|| uses.fd.first())
                    .copied()
                    .unwrap_or(0);
                let tag = if uses.cwd.contains(&pid) {
                    "proc-cwd"
                } else {
                    "proc-open-fd"
                };
                let reason = format!(
                    "Process {pid} ({}) began using {} during finish — close it before deletion",
                    comm_of(pid),
                    d.display()
                );
                if force {
                    if !overridden.iter().any(|old| old == tag) {
                        overridden.push(tag.to_string());
                    }
                } else {
                    return Err(Error::rejected(reason));
                }
            }
            Ok(_) => {}
        }
    }
    Ok(excused_pids)
}

fn begin_finish_release(
    id: &str,
    force: bool,
    target: &Target,
    probe: &FinishProbe,
    keep_for_remote: bool,
) -> Result<Option<crate::worktree::lifecycle::ReleaseGuard>> {
    let branch = target.branch.as_str();
    let root = &target.root;
    let will_remove_worktree = target.wt_dir.as_deref().is_some_and(|d| d.is_dir());
    let will_delete_branch =
        !probe.keep_branch && !branch.is_empty() && !keep_for_remote && probe.tip.is_some();
    let will_delete_remote = probe.remote
        && !branch.is_empty()
        && probe.ev.remote_note.is_none()
        && probe.ev.remote_tip.is_some()
        && (probe.remote_covered || force);
    if probe.refs_only {
        let path = target.wt_dir.as_deref().ok_or_else(|| {
            Error::rejected(format!("issue {id} has no path for refs-only finish"))
        })?;
        Ok(Some(crate::worktree::lifecycle::begin_refs_only_finish(
            root,
            path,
            id,
            (!branch.is_empty()).then_some(branch),
            &format!("issue finish {id} refs-only closure"),
        )?))
    } else if will_remove_worktree || will_delete_branch || will_delete_remote {
        let path = target.wt_dir.as_deref().ok_or_else(|| {
            Error::rejected(format!(
                "issue {id} has no managed checkout path for branch {branch}; refusing deletion"
            ))
        })?;
        verify_checkout_ownership(root, path, id, (!branch.is_empty()).then_some(branch))?;
        Ok(Some(crate::worktree::lifecycle::begin_release(
            root,
            path,
            id,
            (!branch.is_empty()).then_some(branch),
            &format!("issue finish {id} for branch {branch}"),
        )?))
    } else {
        Ok(None)
    }
}

fn remove_worktree_and_branch(
    target: &Target,
    probe: &FinishProbe,
    force: bool,
    keep_for_remote: bool,
    lifecycle_release: &mut Option<crate::worktree::lifecycle::ReleaseGuard>,
) -> Result<RemovalOutcome> {
    let root = &target.root;
    let branch = target.branch.as_str();
    // Removal: the worktree first (frees the branch), then the branch
    // — and only the exact tip the evidence covered: a tip that moved
    // (or was never covered) keeps its commits, so an uncovered branch
    // is never `branch -D`'d even under --force.
    let mut removed_worktree = false;
    if !probe.refs_only {
        if let Some(d) = target.wt_dir.as_deref().filter(|d| d.is_dir()) {
            let wt_str = d.to_string_lossy().into_owned();
            let mut args = vec!["worktree", "remove"];
            if force {
                args.push("--force");
            }
            args.push("--");
            args.push(&wt_str);
            if let Err(e) = git(root, &args) {
                let reason = format!("git worktree remove {} failed: {e}", d.display());
                if let Some(release) = lifecycle_release.as_mut() {
                    let _ = release.cleanup_failed(&reason);
                }
                return Err(Error::rejected(reason));
            }
            removed_worktree = true;
        }
    }
    let mut deleted_branch = false;
    let mut branch_note = Value::Null;
    if probe.refs_only && branch_tip(root, branch).is_some() {
        branch_note = json!("kept: the worktree dir was already gone — refs closed only");
    }
    if !probe.keep_branch && !branch.is_empty() {
        if keep_for_remote {
            branch_note = json!(format!(
                "kept: 'origin/{branch}' is not covered by merge evidence — \
                 deleting the local branch would leave one stray copy"
            ));
        } else {
            match &probe.tip {
                Some(tip) => {
                    deleted_branch = delete_branch_at(root, branch, tip);
                    if !deleted_branch {
                        branch_note = json!(format!(
                            "branch tip moved during finish — kept (its \
                             evidence covered {tip})"
                        ));
                    }
                }
                None => {
                    // tip=None: either the branch is already gone
                    // (nothing to say) or its tip has commits no
                    // evidence covers — never deleted, even --force.
                    if git(
                        root,
                        &[
                            "rev-parse",
                            "--verify",
                            "--quiet",
                            &format!("refs/heads/{branch}"),
                        ],
                    )
                    .is_ok()
                    {
                        branch_note =
                            json!("branch tip has commits no merge/push evidence covers — kept");
                    }
                }
            }
        }
    }
    // One tracker commit marks the refs closed — kept as history. A
    // branch this finish left standing (`--keep-branch`, kept for its
    // remote, an uncovered or moved tip) keeps its ref open, so the
    // surviving work stays on the board and finishable (CAD-145). A
    // refs-only close of a missing checkout closes only the worktree ref;
    // any surviving branch ref remains open for recovery.
    let branch_kept = !deleted_branch && !branch.is_empty() && branch_tip(root, branch).is_some();
    let disposition = if branch_kept {
        format!("checkout released; branch {branch} retained")
    } else {
        format!("checkout released; branch {branch} removed or already missing")
    };
    // The lifecycle transaction ends at the destructive boundary, before any
    // tracker write can fail after the checkout has already been removed.
    let release_warning = if probe.refs_only {
        None
    } else {
        lifecycle_release
            .take()
            .and_then(|mut release| release.released(&disposition).err())
            .map(|e| e.to_string())
    };
    Ok(RemovalOutcome {
        removed_worktree,
        branch_kept,
        deleted_branch,
        branch_note,
        disposition,
        release_warning,
    })
}

fn commit_closed_refs(
    ctx: &FinishCtx<'_>,
    target: &Target,
    removal: &RemovalOutcome,
    refs_only: bool,
    lifecycle_release: &mut Option<crate::worktree::lifecycle::ReleaseGuard>,
) -> Result<(Front, Option<String>)> {
    // CAD-1191 lock 2: close the refs on a fresh read of the issue, so a
    // tracker write that landed during the removal is kept. The refs are
    // matched by path, exactly as before.
    let _lock = ctx.pm.lock()?;
    let (mut front, body) = write::load_front(&target.dir)?;
    for r in &mut front.refs {
        if (r.kind == "worktree"
            && target
                .wt_dir
                .as_deref()
                .is_some_and(|d| r.path.as_deref() == Some(d.to_string_lossy().as_ref())))
            || (r.kind == "branch"
                && !target.branch.is_empty()
                && !removal.branch_kept
                && r.path.as_deref() == Some(&target.branch))
        {
            r.closed = Some(true);
        }
    }
    // CAD-454: the front write is committed only if the tracker commit
    // lands — a refused commit puts the file back so the refs stay
    // open on disk, not just in memory.
    let front_file = target.dir.join("issue.md");
    let front_prev = std::fs::read(&front_file).ok();
    write::save_front(&target.dir, &front, &body)?;
    let what = if target.branch.is_empty() {
        target.wt_name.as_deref().unwrap_or("worktree").to_string()
    } else {
        target.branch.clone()
    };
    let subject = if ctx.actor.is_empty() {
        format!("{}: finish {what}", ctx.id)
    } else {
        format!("{}: finish {what} ({})", ctx.id, ctx.actor)
    };
    let mut trailers = format!(
        "Issue: {}\nActor: {}\n",
        ctx.id,
        write::actor_who(ctx.actor, None)
    );
    if ctx.args.force {
        trailers.push_str("Forced: true\n");
    }
    if let Err(e) = ctx.pm.commit(
        std::slice::from_ref(&front_file),
        &format!("{subject}\n\n{trailers}"),
    ) {
        match &front_prev {
            Some(bytes) => {
                let _ = std::fs::write(&front_file, bytes);
            }
            None => {
                let _ = std::fs::remove_file(&front_file);
            }
        }
        return Err(e);
    }
    let lifecycle_warning = if refs_only {
        lifecycle_release
            .take()
            .and_then(|mut release| release.released(&removal.disposition).err())
            .map(|e| e.to_string())
    } else {
        removal.release_warning.clone()
    };
    Ok((front, lifecycle_warning))
}

fn maybe_delete_remote_branch(
    root: &Path,
    branch: &str,
    probe: &FinishProbe,
    force: bool,
    overridden: &mut Vec<String>,
) -> (bool, Value) {
    let mut remote_deleted = false;
    let mut remote_note = Value::Null;
    if !probe.remote {
        return (remote_deleted, remote_note);
    }
    if branch.is_empty() {
        remote_note = json!("no branch ref — nothing deleted");
    } else if let Some(note) = &probe.ev.remote_note {
        // The fetch could not prove the remote ref — keep it and
        // say why; a stale tracking ref never stands in.
        remote_note = json!(note);
    } else {
        match probe.ev.remote_tip.as_deref() {
            None => {
                remote_note = json!(format!("no 'origin/{branch}' — nothing deleted"));
            }
            Some(expect) => {
                if !probe.remote_covered && !force {
                    remote_note = json!(
                        "remote branch kept: its tip is not covered by merge \
                         evidence — pass --force to delete it anyway"
                    );
                } else {
                    // Leased on the fetched tip: a remote-side move
                    // since the fetch refuses the push instead of
                    // deleting commits this host never saw.
                    match remote_delete(root, branch, expect) {
                        Ok(_) => {
                            remote_deleted = true;
                            if !probe.remote_covered {
                                overridden.push("remote-delete-uncovered".to_string());
                            }
                        }
                        Err(e) => remote_note = json!(e.to_string()),
                    }
                }
            }
        }
    }
    (remote_deleted, remote_note)
}

fn build_finish_output(
    front: &Front,
    target: &Target,
    probe: &FinishProbe,
    removal: &RemovalOutcome,
    excused_pids: &[u32],
    remote_outcome: RemoteFinishOutcome,
) -> Value {
    let RemoteFinishOutcome {
        deleted: remote_deleted,
        note: remote_note,
        lifecycle_warning,
        force,
    } = remote_outcome;
    // Accounting only — `git worktree remove` takes the worktree dir
    // and nothing else. `cargo_target_exists` is a literal check on
    // the recorded path after the removal, emitted only when a target
    // was recorded: a target inside the worktree reports false while
    // the shared dep cache it linked into survives untouched (rm
    // unlinks symlinks; it never follows them).
    let mut out = json!({
        "issue": front.id,
        "finished": true,
        "worktree": target.wt_dir,
        "branch": target.branch,
        "cargo_target": target.cargo_target,
        "removed_worktree": removal.removed_worktree,
        "deleted_branch": removal.deleted_branch,
        "branch_note": removal.branch_note,
        "kept_branch": probe.keep_branch,
        "remote_deleted": remote_deleted,
        "remote_note": remote_note,
        "forced": force,
        "overrode": probe.overridden,
        // CAD-1209: processes the kernel hid from this caller (root's,
        // capability-bearing, non-dumpable) and `policy_denies_read`
        // excused. Not inspected, so the audit trail names them.
        "unscanned_processes": {"count": excused_pids.len(), "pids": excused_pids},
        "merged_by": probe.merged_by,
        "not_started": matches!(probe.ev.state, Branch::NotStarted { .. }),
        "refs_only": probe.refs_only,
        "checkout_disposition": removal.disposition,
        "lifecycle_warning": lifecycle_warning,
        "status": front.status,
    });
    if let Some(t_path) = &target.cargo_target {
        // `symlink_metadata` — a recorded path that is itself a
        // symlink reports its own presence, not its target's.
        out["cargo_target_exists"] = json!(Path::new(t_path).symlink_metadata().is_ok());
    }
    out
}

/// `issue finish <ID> [--worktree P] [--force] [--keep-branch] [--remote]` — JSON like
/// the other issue verbs. Runs in two phases so the pm lock is never
/// held across a daemon RPC, a `fetch` or a `gh` call: the resolve +
/// probe (daemon view, /proc scans, git and gh evidence, the remote
/// fetch) happen unlocked, then the lock covers only the commit
/// phase — a fresh resolve for staleness, LOCAL `rev-parse`s proving
/// the evidence's inputs did not move, the removals, and the tracker
/// write. `shared` lets a sweep reuse one enumeration for every
/// issue instead of re-issuing `agent_list` + `task_show`×N per row;
/// a binding added mid-sweep still lands as a stale probe.
pub(crate) fn run(
    pm: &Pm,
    id: &str,
    args: &FinishArgs,
    actor: &str,
    state_dir: &Path,
    shared: Option<&DaemonView>,
) -> Result<Value> {
    let ctx = FinishCtx {
        pm,
        id,
        args,
        actor,
        state_dir,
    };
    let force = args.force;
    // Phase 1 — the unlocked probe. Nothing here holds the pm lock:
    // one enumeration + the merge/remote evidence (incl. the
    // `--remote` fetch and `gh`) all resolve before any lock.
    let owned;
    let view = match shared {
        Some(v) => v,
        None => {
            owned = daemon_view(state_dir);
            &owned
        }
    };
    let mut probe = match probe_finish_unlocked(&ctx, view)? {
        ProbeOutcome::EarlyReturn(out) => return Ok(out),
        ProbeOutcome::Ready(p) => *p,
    };

    // Phase 2 — the decision, under the pm lock (CAD-1191: lock 1).
    // Re-resolve so a tracker write that landed mid-probe is seen, then
    // prove every input the probe's evidence depended on is unmoved — all
    // local `rev-parse`s — and record the release intent in the lifecycle
    // ledger. The lock is returned before anything is removed; lock 2
    // below only closes the refs.
    let lock = pm.lock()?;
    let target = match resolve(pm, id, args.worktree)? {
        // A concurrent finish closed the pair mid-probe — the same
        // idempotent answer the unlocked probe would have given.
        Resolve::Finished => return Ok(already_finished_json(id)),
        Resolve::Nothing => {
            return Err(Error::rejected(format!(
                "{id}: worktree/branch refs vanished during finish — retry"
            )))
        }
        Resolve::Target(t) => *t,
    };
    // A retargeted pair or a newly recorded message ref means the
    // probe is stale — even --force does not retarget a probe.
    if let Some(reason) = stale_probe_reason(&probe.target, &target) {
        return Err(Error::rejected(reason));
    }
    // Any input the evidence was computed against moving mid-probe —
    // the branch tip, the default ref, or the tracking ref a
    // concurrent fetch updated — also makes it stale. A remote-side
    // move the tracking ref cannot see is covered by the leased
    // delete below, not this check.
    if let Some(reason) = stale_evidence_reason(&target.root, &target.branch, &probe.ev) {
        return Err(Error::rejected(reason));
    }
    let excused_pids = revalidate_live_process_use(
        target.wt_dir.as_deref(),
        probe.refs_only,
        force,
        &mut probe.overridden,
    )?;

    // "Pushed" was the local branch's only evidence — under --remote,
    // keeping the uncovered remote is what makes deleting the local
    // safe; if the remote is kept AND the local's commits are its
    // only other copy story, keep both (the row explains; --force
    // overrides). A plain finish never deletes the remote, so the
    // pushed evidence stands on its own and the local still goes.
    let keep_for_remote = probe.remote
        && probe.ev.remote_tip.is_some()
        && !probe.remote_covered
        && !force
        && !probe.merged_how_present
        && probe.tip.is_some();
    let mut lifecycle_release = begin_finish_release(id, force, &target, &probe, keep_for_remote)?;

    drop(lock);
    crate::issue::lockseam::outside_lock("finish-remove");

    let removal = remove_worktree_and_branch(
        &target,
        &probe,
        force,
        keep_for_remote,
        &mut lifecycle_release,
    )?;
    let (front, lifecycle_warning) = commit_closed_refs(
        &ctx,
        &target,
        &removal,
        probe.refs_only,
        &mut lifecycle_release,
    )?;
    // The commit phase is done — the pm lock has been released before the
    // remote delete: a `push` can take seconds and the lock's spin
    // deadline is 15s. The lease below, not lock ordering, is what
    // makes the delete safe.
    let mut overridden = std::mem::take(&mut probe.overridden);
    let (remote_deleted, remote_note) =
        maybe_delete_remote_branch(&target.root, &target.branch, &probe, force, &mut overridden);
    probe.overridden = overridden;

    Ok(build_finish_output(
        &front,
        &target,
        &probe,
        &removal,
        &excused_pids,
        RemoteFinishOutcome {
            deleted: remote_deleted,
            note: remote_note,
            lifecycle_warning,
            force,
        },
    ))
}
