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
    let probe = match resolve(pm, id, args.worktree)? {
        Resolve::Nothing => {
            return Err(Error::rejected(format!(
                "{id}: no worktree/branch refs recorded — nothing to finish"
            )))
        }
        Resolve::Finished => {
            return Ok(json!({"issue": id, "finished": false,
                             "reason": "worktree already finished"}))
        }
        Resolve::Target(t) => t,
    };
    if probe.wt_dir.is_none() && !probe.branch.is_empty() && args.keep_branch && !args.remote {
        return Ok(json!({
            "issue": id,
            "finished": false,
            "branch": probe.branch,
            "kept_branch": true,
            "deleted_branch": false,
            "removed_worktree": false,
            "reason": "no managed checkout path; branch/ref retained; use issue start with recorded lane/name to reattach before finishing"
        }));
    }
    // A gone directory under `--worktree` only closes refs: nothing
    // is removed or deleted, so no branch or remote is touched and
    // survivability has nothing to protect.
    let refs_only = args.close_if_gone && probe.wt_dir.as_deref().is_some_and(|d| !d.is_dir());
    let keep_branch = args.keep_branch || refs_only;
    let remote = args.remote && !refs_only;
    if refs_only {
        let path = probe.wt_dir.as_deref().ok_or_else(|| {
            Error::rejected(format!("issue {id} has no path for refs-only finish"))
        })?;
        verify_refs_only_ownership(
            &probe.root,
            path,
            id,
            (!probe.branch.is_empty()).then_some(probe.branch.as_str()),
        )?;
    } else if let Some(path) = probe.wt_dir.as_deref() {
        let _ = verify_checkout_ownership(
            &probe.root,
            path,
            id,
            (!probe.branch.is_empty()).then_some(probe.branch.as_str()),
        )?;
    } else if !probe.branch.is_empty() && (!keep_branch || remote) {
        return Err(Error::rejected(format!(
            "issue {id} has no managed checkout path for branch {}; refusing deletion",
            probe.branch
        )));
    }
    let ev = evidence(&probe, remote);
    let (merged_how, tip, _) = survivability(&ev, &probe);
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
            .is_some_and(|(r, into)| merge_rule(&probe.root, &probe.branch, r, into).is_some());
    // The per-worktree guard + dirty + survivability, evaluated once.
    // `--force` records every block it bypasses; without it the first
    // block refuses, naming its pid or message id.
    let check = inspect(view, state_dir, &probe, &ev);
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
        if force {
            overridden.push(b.tag);
        } else {
            return Err(Error::rejected(b.reason));
        }
    }

    // Phase 2 — the decision, under the pm lock (CAD-1191: lock 1).
    // Re-resolve so a tracker write that landed mid-probe is seen, then
    // prove every input the probe's evidence depended on is unmoved — all
    // local `rev-parse`s — and record the release intent in the lifecycle
    // ledger. The lock is returned before anything is removed; lock 2
    // below only closes the refs.
    let lock = pm.lock()?;
    let t = match resolve(pm, id, args.worktree)? {
        // A concurrent finish closed the pair mid-probe — the same
        // idempotent answer the unlocked probe would have given.
        Resolve::Finished => {
            return Ok(json!({"issue": id, "finished": false,
                             "reason": "worktree already finished"}))
        }
        Resolve::Nothing => {
            return Err(Error::rejected(format!(
                "{id}: worktree/branch refs vanished during finish — retry"
            )))
        }
        Resolve::Target(t) => t,
    };
    // A retargeted pair or a newly recorded message ref means the
    // probe is stale — even --force does not retarget a probe.
    if let Some(reason) = stale_probe_reason(&probe, &t) {
        return Err(Error::rejected(reason));
    }
    // Any input the evidence was computed against moving mid-probe —
    // the branch tip, the default ref, or the tracking ref a
    // concurrent fetch updated — also makes it stale. A remote-side
    // move the tracking ref cannot see is covered by the leased
    // delete below, not this check.
    if let Some(reason) = stale_evidence_reason(&t.root, &t.branch, &ev) {
        return Err(Error::rejected(reason));
    }
    // Local live-use is rechecked at the destructive boundary. Daemon and
    // tracker bindings were probed unlocked above; new tracker refs make the
    // stale-probe check fail, while process cwd/FD use can change without a
    // tracker write.
    let mut excused_pids: Vec<u32> = Vec::new();
    if !refs_only {
        if let Some(d) = t.wt_dir.as_deref().filter(|d| d.is_dir()) {
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
                        d.display(), uses.enumeration_error.as_deref().unwrap_or("unknown process enumeration failure")
                    )))
                }
                Ok(uses) if !uses.cwd.is_empty() || !uses.fd.is_empty() => {
                    let pid = uses.cwd.first().or_else(|| uses.fd.first()).copied().unwrap_or(0);
                    let tag = if uses.cwd.contains(&pid) { "proc-cwd" } else { "proc-open-fd" };
                    let reason = format!(
                        "Process {pid} ({}) began using {} during finish — close it before deletion",
                        comm_of(pid), d.display()
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
    }
    let merged_by = merged_how.map_or(Value::Null, |h| json!(h));
    let dir = t.dir.clone();
    let wt_dir = t.wt_dir.clone();
    let wt_name = t.wt_name.clone();
    let branch = t.branch.clone();
    let root = t.root.clone();
    let cargo_target = t.cargo_target.clone();

    // "Pushed" was the local branch's only evidence — under --remote,
    // keeping the uncovered remote is what makes deleting the local
    // safe; if the remote is kept AND the local's commits are its
    // only other copy story, keep both (the row explains; --force
    // overrides). A plain finish never deletes the remote, so the
    // pushed evidence stands on its own and the local still goes.
    let keep_for_remote = remote
        && ev.remote_tip.is_some()
        && !remote_covered
        && !force
        && merged_how.is_none()
        && tip.is_some();
    let will_remove_worktree = wt_dir.as_deref().is_some_and(|d| d.is_dir());
    let will_delete_branch =
        !keep_branch && !branch.is_empty() && !keep_for_remote && tip.is_some();
    let will_delete_remote = remote
        && !branch.is_empty()
        && ev.remote_note.is_none()
        && ev.remote_tip.is_some()
        && (remote_covered || force);
    let mut lifecycle_release = if refs_only {
        let path = wt_dir.as_deref().ok_or_else(|| {
            Error::rejected(format!("issue {id} has no path for refs-only finish"))
        })?;
        Some(crate::worktree::lifecycle::begin_refs_only_finish(
            &root,
            path,
            id,
            (!branch.is_empty()).then_some(branch.as_str()),
            &format!("issue finish {id} refs-only closure"),
        )?)
    } else if will_remove_worktree || will_delete_branch || will_delete_remote {
        let path = wt_dir.as_deref().ok_or_else(|| {
            Error::rejected(format!(
                "issue {id} has no managed checkout path for branch {branch}; refusing deletion"
            ))
        })?;
        verify_checkout_ownership(
            &root,
            path,
            id,
            (!branch.is_empty()).then_some(branch.as_str()),
        )?;
        Some(crate::worktree::lifecycle::begin_release(
            &root,
            path,
            id,
            (!branch.is_empty()).then_some(branch.as_str()),
            &format!("issue finish {id} for branch {branch}"),
        )?)
    } else {
        None
    };

    drop(lock);
    crate::issue::lockseam::outside_lock("finish-remove");

    // Removal: the worktree first (frees the branch), then the branch
    // — and only the exact tip the evidence covered: a tip that moved
    // (or was never covered) keeps its commits, so an uncovered branch
    // is never `branch -D`'d even under --force.
    let mut removed_worktree = false;
    if !refs_only {
        if let Some(d) = wt_dir.as_deref().filter(|d| d.is_dir()) {
            let target = d.to_string_lossy().into_owned();
            let mut args = vec!["worktree", "remove"];
            if force {
                args.push("--force");
            }
            args.push("--");
            args.push(&target);
            if let Err(e) = git(&root, &args) {
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
    if refs_only && branch_tip(&root, &branch).is_some() {
        branch_note = json!("kept: the worktree dir was already gone — refs closed only");
    }
    if !keep_branch && !branch.is_empty() {
        if keep_for_remote {
            branch_note = json!(format!(
                "kept: 'origin/{branch}' is not covered by merge evidence — \
                 deleting the local branch would leave one stray copy"
            ));
        } else {
            match tip {
                Some(tip) => {
                    deleted_branch = delete_branch_at(&root, &branch, &tip);
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
                        &root,
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
    let branch_kept = !deleted_branch && !branch.is_empty() && branch_tip(&root, &branch).is_some();
    let disposition = if branch_kept {
        format!("checkout released; branch {branch} retained")
    } else {
        format!("checkout released; branch {branch} removed or already missing")
    };
    // The lifecycle transaction ends at the destructive boundary, before any
    // tracker write can fail after the checkout has already been removed.
    let release_warning = if refs_only {
        None
    } else {
        lifecycle_release
            .take()
            .and_then(|mut release| release.released(&disposition).err())
            .map(|e| e.to_string())
    };
    // CAD-1191 lock 2: close the refs on a fresh read of the issue, so a
    // tracker write that landed during the removal is kept. The refs are
    // matched by path, exactly as before.
    let lock = pm.lock()?;
    let (mut front, body) = write::load_front(&dir)?;
    for r in &mut front.refs {
        if (r.kind == "worktree"
            && wt_dir
                .as_deref()
                .is_some_and(|d| r.path.as_deref() == Some(d.to_string_lossy().as_ref())))
            || (r.kind == "branch"
                && !branch.is_empty()
                && !branch_kept
                && r.path.as_deref() == Some(&branch))
        {
            r.closed = Some(true);
        }
    }
    // CAD-454: the front write is committed only if the tracker commit
    // lands — a refused commit puts the file back so the refs stay
    // open on disk, not just in memory.
    let front_file = dir.join("issue.md");
    let front_prev = std::fs::read(&front_file).ok();
    write::save_front(&dir, &front, &body)?;
    let what = if branch.is_empty() {
        wt_name.as_deref().unwrap_or("worktree").to_string()
    } else {
        branch.clone()
    };
    let subject = if actor.is_empty() {
        format!("{id}: finish {what}")
    } else {
        format!("{id}: finish {what} ({actor})")
    };
    let mut trailers = format!("Issue: {id}\nActor: {}\n", write::actor_who(actor, None));
    if force {
        trailers.push_str("Forced: true\n");
    }
    if let Err(e) = pm.commit(
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
            .and_then(|mut release| release.released(&disposition).err())
            .map(|e| e.to_string())
    } else {
        release_warning
    };
    // The commit phase is done — the pm lock goes back before the
    // remote delete: a `push` can take seconds and the lock's spin
    // deadline is 15s. The lease below, not lock ordering, is what
    // makes the delete safe.
    drop(lock);

    let mut remote_deleted = false;
    let mut remote_note = Value::Null;
    if remote {
        if branch.is_empty() {
            remote_note = json!("no branch ref — nothing deleted");
        } else if let Some(note) = &ev.remote_note {
            // The fetch could not prove the remote ref — keep it and
            // say why; a stale tracking ref never stands in.
            remote_note = json!(note);
        } else {
            match ev.remote_tip.as_deref() {
                None => {
                    remote_note = json!(format!("no 'origin/{branch}' — nothing deleted"));
                }
                Some(expect) => {
                    if !remote_covered && !force {
                        remote_note = json!(
                            "remote branch kept: its tip is not covered by merge \
                             evidence — pass --force to delete it anyway"
                        );
                    } else {
                        // Leased on the fetched tip: a remote-side move
                        // since the fetch refuses the push instead of
                        // deleting commits this host never saw.
                        match remote_delete(&root, &branch, expect) {
                            Ok(_) => {
                                remote_deleted = true;
                                if !remote_covered {
                                    overridden.push("remote-delete-uncovered".to_string());
                                }
                            }
                            Err(e) => remote_note = json!(e.to_string()),
                        }
                    }
                }
            }
        }
    }

    // Accounting only — `git worktree remove` takes the worktree dir
    // and nothing else. `cargo_target_exists` is a literal check on
    // the recorded path after the removal, emitted only when a target
    // was recorded: a target inside the worktree reports false while
    // the shared dep cache it linked into survives untouched (rm
    // unlinks symlinks; it never follows them).
    let mut out = json!({
        "issue": front.id,
        "finished": true,
        "worktree": wt_dir,
        "branch": branch,
        "cargo_target": cargo_target,
        "removed_worktree": removed_worktree,
        "deleted_branch": deleted_branch,
        "branch_note": branch_note,
        "kept_branch": keep_branch,
        "remote_deleted": remote_deleted,
        "remote_note": remote_note,
        "forced": force,
        "overrode": overridden,
        // CAD-1209: processes the kernel hid from this caller (root's,
        // capability-bearing, non-dumpable) and `policy_denies_read`
        // excused. Not inspected, so the audit trail names them.
        "unscanned_processes": {"count": excused_pids.len(), "pids": excused_pids},
        "merged_by": merged_by,
        "not_started": matches!(ev.state, Branch::NotStarted { .. }),
        "refs_only": refs_only,
        "checkout_disposition": disposition,
        "lifecycle_warning": lifecycle_warning,
        "status": front.status,
    });
    if let Some(target) = &cargo_target {
        // `symlink_metadata` — a recorded path that is itself a
        // symlink reports its own presence, not its target's.
        out["cargo_target_exists"] = json!(Path::new(target).symlink_metadata().is_ok());
    }
    Ok(out)
}
