use super::*;

/// Why a hosted lane was not released when no release was attempted: the
/// observation is guidance only and never authorizes cleanup.
pub fn refusal<H: CoordHttp>(
    coordinator: &Coordinator<H>,
    issue: Option<&str>,
    selected: Option<&Path>,
) -> Error {
    let observation = issue.and_then(|id| observe(coordinator, id, selected).ok().flatten());
    let detail = match observation {
        Some(LaneState::Dirty) => "the lane has uncommitted work",
        Some(LaneState::CleanMerged) => "the lane was observed clean and its work has evidence of being merged",
        Some(LaneState::CleanUnverified) => {
            "the lane was observed clean but its merge state cannot be confirmed; it may contain work"
        }
        None => "the lane state cannot be confirmed",
    };
    Error::rejected(format!(
        "Hosted issue finish did not release the lane; {detail}. Nothing was removed. \
         Release needs this machine's recorded lane for the issue, a finished assignment, \
         a clean idle worktree and a merged branch (pushing alone does not release it). If \
         the lane cannot be bound, verify yourself that it is merged, that nothing is using \
         it and that you do not need its ignored files (removal deletes them), then run \
         `git worktree remove <path>` and `git branch -d <branch>` from the repository; a \
         squash-merged branch needs `-D`, only after you have verified the merge"
    ))
}

/// How hosted release asks GitHub for merged PRs: `(owner/repo, head branch)`
/// to the `gh pr list --json number,headRefOid,baseRefName,mergedAt` array, or
/// `None` when it could not ask. Tests pass a fake through [`release_with`].
pub type PrLookup<'a> = &'a dyn Fn(&str, &str) -> Option<Value>;

/// `issue finish <ID>` on a hosted org (CAD-2178): release this machine's
/// lane for the issue through the native finish guard, with no `--force`.
/// The lane is bound from the coordinator ledger exactly as [`refusal`]
/// observes it, and the assignment must have ended — a running or blocked
/// assignment is a turn inside the lane.
pub fn release<H: CoordHttp>(
    coordinator: &Coordinator<H>,
    issue: &str,
    selected: Option<&Path>,
    state_dir: &Path,
) -> Result<Value> {
    release_with(
        coordinator,
        issue,
        selected,
        state_dir,
        &crate::issue::finish::gh_merged_prs,
    )
}

/// [`release`] with the merged-PR lookup supplied (the test seam).
pub fn release_with<H: CoordHttp>(
    coordinator: &Coordinator<H>,
    issue: &str,
    selected: Option<&Path>,
    state_dir: &Path,
    prs: PrLookup<'_>,
) -> Result<Value> {
    release_bound(coordinator, issue, selected, None, state_dir, prs)
}

/// Release only the lane bound to this exact confirmed cancellation.
pub fn release_cancelled<H: CoordHttp>(
    coordinator: &Coordinator<H>,
    assignment: &str,
    state_dir: &Path,
) -> Result<Value> {
    let lock = remote_cli::private_lock(
        &coordinator.ledger,
        "local-assign",
        Instant::now() + Duration::from_secs(5),
    )?;
    let ledger = Ledger::load(&coordinator.ledger, &coordinator.org_id)?;
    let mut matches = ledger
        .attempts
        .values()
        .filter(|attempt| attempt.assignment_id.as_deref() == Some(assignment));
    let Some(attempt) = matches.next() else {
        return Err(Error::rejected(
            "the cancelled assignment has no local lane record",
        ));
    };
    if matches.next().is_some()
        || attempt.state != "cancelled"
        || !attempt.fenced
        || attempt.reassign_operation_id.is_some()
        || attempt.reassign_fingerprint.is_some()
        || attempt.reassign_expected_generation.is_some()
    {
        return Err(Error::rejected(
            "the assignment is not uniquely recorded as cancelled",
        ));
    }
    let issue = attempt.issue_id.clone();
    drop(lock);
    release_bound(
        coordinator,
        &issue,
        None,
        Some(assignment),
        state_dir,
        &crate::issue::finish::gh_merged_prs,
    )
}

fn release_bound<H: CoordHttp>(
    coordinator: &Coordinator<H>,
    issue: &str,
    selected: Option<&Path>,
    assignment: Option<&str>,
    state_dir: &Path,
    prs: PrLookup<'_>,
) -> Result<Value> {
    let Some(lane) = bound_lane(coordinator, issue, selected, assignment)
        .ok()
        .flatten()
    else {
        if assignment.is_some() {
            return Err(Error::rejected(
                "the cancelled assignment does not have a uniquely bound local lane; nothing was removed",
            ));
        }
        return Err(refusal(coordinator, Some(issue), selected));
    };
    if !lane.ended {
        return Err(Error::rejected(format!(
            "Hosted issue finish did not release the lane for {issue}; its assignment has not \
             ended. If the actor is gone, cancel it with \
             `cadence --org <workspace> agent reassign {} --cancel --release-lane`; \
             cancellation does not stop the actor. Nothing was removed",
            lane.unfinished.as_deref().unwrap_or("<assignment>")
        )));
    }
    let _assignment_locks = lane
        .assignments
        .iter()
        .map(|id| coordinator.assignment_lock(id))
        .collect::<Result<Vec<_>>>()?;
    let lock_and_validate = || {
        let lock = remote_cli::private_lock(
            &coordinator.ledger,
            "local-assign",
            Instant::now() + Duration::from_secs(5),
        )?;
        let ledger = Ledger::load(&coordinator.ledger, &coordinator.org_id)?;
        if bound_lane_from_ledger(&ledger, issue, selected, assignment)?.as_ref() != Some(&lane) {
            return Err(Error::rejected(
                "the hosted lane binding changed before release; nothing was removed",
            ));
        }
        Ok(lock)
    };
    let mut result = crate::issue::finish::release_hosted_lane(
        issue,
        &lane.root,
        &lane.path,
        &lane.branch,
        state_dir,
        prs,
        &lock_and_validate,
    )?;
    if let Some(guard) = lane.guarded.as_ref() {
        if let Err(error) = clear_released_guard(coordinator, issue, &lane.path, guard) {
            let previous = result["warning"].as_str().unwrap_or("");
            let separator = if previous.is_empty() { "" } else { "; " };
            result["warning"] = json!(format!(
                "{previous}{separator}the lane was removed, but its local compatibility guard \
                 could not be cleared: {error}; inspect it, then run \
                 `cadence --org <workspace> agent reconcile-release {}`",
                guard.assignment
            ));
        }
    }
    Ok(result)
}

/// Clear only a compatibility guard for an already absent, unregistered lane.
/// This does not remove a branch or establish that the work was merged.
pub fn reconcile_absent_guard<H: CoordHttp>(
    coordinator: &Coordinator<H>,
    assignment: &str,
) -> Result<Value> {
    coordinator.with_ledger(|ledger| {
        let mut rows = ledger
            .attempts
            .values_mut()
            .filter(|attempt| attempt.assignment_id.as_deref() == Some(assignment));
        let Some(attempt) = rows.next() else {
            return Err(Error::rejected("the assignment has no local lane record"));
        };
        if rows.next().is_some()
            || !attempt.retired_lane_guard
            || !attempt_ended(&attempt.state)
            || attempt.reassign_operation_id.is_some()
            || attempt.reassign_fingerprint.is_some()
            || attempt.reassign_expected_generation.is_some()
        {
            return Err(Error::rejected(
                "the assignment does not have one confirmed terminal lane guard",
            ));
        }
        let (Some(worktree), Some(lane), Some(lane_claim), Some(lane_generation)) = (
            attempt.worktree.as_deref(),
            attempt.lane.as_deref(),
            attempt.lane_claim.as_deref(),
            attempt.lane_generation,
        ) else {
            return Err(Error::rejected("the recorded lane binding is incomplete"));
        };
        let current_binding = attempt.claim_id.as_deref() == Some(lane_claim)
            && attempt.generation == Some(lane_generation);
        let cancelled_binding = attempt.state == "cancelled"
            && attempt.fenced
            && attempt.claim_id.is_none()
            && lane_generation.checked_add(1) == attempt.generation;
        if !(current_binding || cancelled_binding) {
            return Err(Error::rejected("the recorded lane binding is inconsistent"));
        }
        let path = Path::new(worktree);
        if recorded_lane_present(worktree) {
            return Err(Error::rejected("the lane exists or cannot be inspected"));
        }
        let Some(repo) = path.parent().and_then(Path::parent).and_then(Path::parent) else {
            return Err(Error::rejected("the recorded lane path has no repository root"));
        };
        if layout::worktree_dir(repo, lane) != path {
            return Err(Error::rejected("the recorded lane is outside the expected layout"));
        }
        let root = worktree::main_root(repo)?;
        let expected = layout::worktree_dir(&root, lane);
        let listing = crate::issue::finish::git(&root, &["worktree", "list", "--porcelain", "-z"])?;
        if listing.split('\0').filter_map(|entry| entry.strip_prefix("worktree ")).any(|entry| {
            let registered = Path::new(entry);
            crate::issue::finish::same_path(registered, path)
                || crate::issue::finish::same_path(registered, &expected)
        }) || recorded_lane_present(worktree) {
            return Err(Error::rejected("the lane is still present or registered"));
        }
        let issue = attempt.issue_id.clone();
        let path = worktree.to_string();
        attempt.worktree = None;
        attempt.lane = None;
        attempt.lane_claim = None;
        attempt.lane_generation = None;
        attempt.retired_lane_guard = false;
        Ok(json!({
            "assignment_id": assignment,
            "issue": issue,
            "worktree": path,
            "guard_reconciled": true,
            "removed_worktree": false,
            "deleted_branch": false,
            "merge_evidence_verified": false,
            "warning": "The worktree was already absent; no deletion or merge verification was performed."
        }))
    })
}

fn clear_released_guard<H: CoordHttp>(
    coordinator: &Coordinator<H>,
    issue: &str,
    path: &Path,
    guard: &GuardedLane,
) -> Result<()> {
    coordinator.with_ledger(|ledger| {
        let mut matches = ledger.attempts.values_mut().filter(|attempt| {
            attempt.assignment_id.as_deref() == Some(guard.assignment.as_str())
                && attempt.issue_id == issue
        });
        let Some(attempt) = matches.next() else {
            return Err(Error::rejected("the recorded assignment disappeared"));
        };
        if matches.next().is_some()
            || !attempt.retired_lane_guard
            || !attempt.fenced
            || attempt.generation != Some(guard.generation)
            || attempt.state != guard.state
            || attempt.claim_id != guard.claim
            || attempt.lane_claim.as_deref() != Some(guard.lane_claim.as_str())
            || attempt.lane_generation != Some(guard.lane_generation)
            || attempt.lane.as_deref() != Some(guard.lane.as_str())
            || attempt.worktree.as_deref().map(Path::new) != Some(path)
            || attempt.reassign_operation_id.is_some()
            || attempt.reassign_fingerprint.is_some()
            || attempt.reassign_expected_generation.is_some()
        {
            return Err(Error::rejected("the recorded lane binding changed"));
        }
        if !matches!(fs::symlink_metadata(path), Err(error) if error.kind() == std::io::ErrorKind::NotFound) {
            return Err(Error::rejected("the released path exists or cannot be inspected"));
        }
        attempt.worktree = None;
        attempt.lane = None;
        attempt.lane_claim = None;
        attempt.lane_generation = None;
        attempt.retired_lane_guard = false;
        Ok(())
    })
}

#[derive(PartialEq, Eq)]
struct BoundLane {
    root: PathBuf,
    path: PathBuf,
    branch: String,
    ended: bool,
    unfinished: Option<String>,
    assignments: Vec<String>,
    guarded: Option<GuardedLane>,
}

#[derive(PartialEq, Eq)]
struct GuardedLane {
    assignment: String,
    generation: u64,
    state: String,
    claim: Option<String>,
    lane_claim: String,
    lane_generation: u64,
    lane: String,
}

enum LaneState {
    Dirty,
    CleanMerged,
    CleanUnverified,
}

fn bound_lane<H: CoordHttp>(
    coordinator: &Coordinator<H>,
    issue: &str,
    selected: Option<&Path>,
    assignment: Option<&str>,
) -> Result<Option<BoundLane>> {
    let lock = remote_cli::private_lock(
        &coordinator.ledger,
        "local-assign",
        Instant::now() + Duration::from_secs(5),
    )?;
    let ledger = Ledger::load(&coordinator.ledger, &coordinator.org_id)?;
    drop(lock);
    bound_lane_from_ledger(&ledger, issue, selected, assignment)
}

fn bound_lane_from_ledger(
    ledger: &Ledger,
    issue: &str,
    selected: Option<&Path>,
    assignment: Option<&str>,
) -> Result<Option<BoundLane>> {
    let requested = if let Some(id) = assignment {
        let Some(attempt) = ledger.attempts.values().find(|attempt| {
            attempt.issue_id == issue && attempt.assignment_id.as_deref() == Some(id)
        }) else {
            return Ok(None);
        };
        let Some(path) = attempt.worktree.as_deref() else {
            return Ok(None);
        };
        let path = Path::new(path);
        if selected.is_some_and(|selected| !crate::issue::finish::same_path(path, selected)) {
            return Ok(None);
        }
        Some(path)
    } else {
        selected
    };
    let mut lane = None;
    let mut ended = true;
    let mut unfinished = None;
    let mut guarded = None;
    let mut matched_assignment = assignment.is_none();
    let mut assignments = Vec::new();
    for attempt in ledger.attempts.values().filter(|a| a.issue_id == issue) {
        if attempt.worktree.is_none() && attempt.claim_id.is_none() {
            if attempt.retired_lane_guard && attempt.lane.is_some() {
                return Ok(None);
            }
            continue;
        }
        if let Some(requested) = requested {
            let Some(path) = attempt.worktree.as_deref() else {
                return Ok(None);
            };
            if !crate::issue::finish::same_path(Path::new(path), requested) {
                continue;
            }
        }
        let Some(id) = attempt.assignment_id.as_ref() else {
            return Ok(None);
        };
        if !assignments.contains(id) {
            assignments.push(id.clone());
        }
        // A confirmed cancellation advances the generation and clears the
        // active claim. Only its retained immediately preceding lane binding
        // is usable for cleanup; it never becomes execution authority.
        let row_ended = matches!(attempt.state.as_str(), "result" | "failed" | "cancelled");
        let current_binding = attempt.claim_id.is_some()
            && attempt.lane_claim == attempt.claim_id
            && attempt.generation.is_some()
            && attempt.lane_generation == attempt.generation;
        let cancelled_binding = attempt.state == "cancelled"
            && attempt.retired_lane_guard
            && attempt.fenced
            && attempt.claim_id.is_none()
            && attempt.lane_claim.is_some()
            && attempt.generation.is_some()
            && attempt
                .lane_generation
                .and_then(|generation| generation.checked_add(1))
                == attempt.generation;
        if (attempt.fenced && !row_ended)
            || (row_ended
                && (attempt.reassign_operation_id.is_some()
                    || attempt.reassign_fingerprint.is_some()
                    || attempt.reassign_expected_generation.is_some()))
            || !(current_binding || cancelled_binding)
        {
            return Ok(None);
        }
        let Some(candidate) = attempt.worktree.as_ref().zip(attempt.lane.as_ref()) else {
            return Ok(None);
        };
        if lane.is_some_and(|previous| previous != candidate) {
            return Ok(None);
        }
        if attempt.retired_lane_guard {
            let (Some(id), Some(generation), Some(lane_claim), Some(lane_generation)) = (
                attempt.assignment_id.as_ref(),
                attempt.generation,
                attempt.lane_claim.as_ref(),
                attempt.lane_generation,
            ) else {
                return Ok(None);
            };
            if guarded.is_some() {
                return Ok(None);
            }
            guarded = Some(GuardedLane {
                assignment: id.clone(),
                generation,
                state: attempt.state.clone(),
                claim: attempt.claim_id.clone(),
                lane_claim: lane_claim.clone(),
                lane_generation,
                lane: candidate.1.clone(),
            });
        }
        if assignment.is_some_and(|id| attempt.assignment_id.as_deref() == Some(id)) {
            if matched_assignment || attempt.state != "cancelled" || !attempt.fenced {
                return Ok(None);
            }
            matched_assignment = true;
        }
        if !row_ended {
            let Some(id) = attempt.assignment_id.as_ref() else {
                return Ok(None);
            };
            if unfinished.as_ref().is_some_and(|previous| previous != id) {
                return Ok(None);
            }
            unfinished = Some(id.clone());
        }
        lane = Some(candidate);
        ended &= matches!(attempt.state.as_str(), "result" | "failed" | "cancelled");
    }
    if !matched_assignment {
        return Ok(None);
    }
    let Some((path, lane)) = lane else {
        return Ok(None);
    };
    assignments.sort_unstable();
    let path = Path::new(path);
    if selected.is_some_and(|requested| !crate::issue::finish::same_path(path, requested)) {
        return Ok(None);
    }
    let root = worktree::main_root(path)?;
    if layout::worktree_dir(&root, lane) != path {
        return Ok(None);
    }
    let branch = layout::branch(lane);
    worktree::validate_registered_branch(&root, path, &branch)?;
    Ok(Some(BoundLane {
        root,
        path: path.to_path_buf(),
        branch,
        ended,
        unfinished,
        assignments,
        guarded,
    }))
}

fn observe<H: CoordHttp>(
    coordinator: &Coordinator<H>,
    issue: &str,
    selected: Option<&Path>,
) -> Result<Option<LaneState>> {
    let Some(BoundLane {
        root, path, branch, ..
    }) = bound_lane(coordinator, issue, selected, None)?
    else {
        return Ok(None);
    };
    let path = path.as_path();
    use crate::issue::finish::{git, merge_rule};
    if !git(
        path,
        &["status", "--porcelain=v1", "--untracked-files=normal"],
    )?
    .is_empty()
    {
        return Ok(Some(LaneState::Dirty));
    }
    let tip = git(path, &["rev-parse", "--verify", "HEAD^{commit}"])?;
    let into = match git(
        &root,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    ) {
        Ok(into) => into,
        Err(_) => return Ok(Some(LaneState::CleanUnverified)),
    };
    git(
        &root,
        &["rev-parse", "--verify", &format!("{into}^{{commit}}")],
    )?;
    let evidence = merge_rule(&root, &branch, &tip, &into);
    Ok(Some(if evidence.is_some() {
        LaneState::CleanMerged
    } else {
        LaneState::CleanUnverified
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::cell::Cell;
    use std::os::unix::fs::PermissionsExt;
    use std::rc::Rc;

    struct FakeRemote(Rc<Cell<usize>>);

    impl CoordHttp for FakeRemote {
        fn send(&mut self, _: &str, _: &str, _: Option<&Value>) -> Result<Reply> {
            self.0.set(self.0.get() + 1);
            Err(Error::rejected("unexpected remote request"))
        }
    }

    fn git(dir: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    #[test]
    fn hosted_finish_distinguishes_merged_dirty_and_uncertain_without_tracker_or_remote_writes() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let auth = tmp.path().join("auth");
        fs::create_dir(&repo).unwrap();
        fs::create_dir(&auth).unwrap();
        fs::set_permissions(&auth, fs::Permissions::from_mode(0o700)).unwrap();
        git(&repo, &["init", "-b", "main"]);
        git(&repo, &["config", "user.email", "test@example.invalid"]);
        git(&repo, &["config", "user.name", "Synthetic"]);
        fs::write(repo.join("file"), "base\n").unwrap();
        git(&repo, &["add", "file"]);
        git(&repo, &["commit", "-m", "base"]);
        git(&repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        git(
            &repo,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/main",
            ],
        );
        let lane = repo.join(".cadence/wt/test");
        fs::create_dir_all(lane.parent().unwrap()).unwrap();
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-b",
                "cadence/test",
                lane.to_str().unwrap(),
                "main",
            ],
        );
        let mut ledger = Ledger::load(&auth, "ws_test").unwrap();
        let attempt: Attempt = serde_json::from_value(json!({
            "operation_id": "op_test", "issue_id": "CAD-1", "action": "start", "target": null,
            "assignment_id": "hla_test", "generation": 1, "claim_id": "hlc_test",
            "state": "result", "fenced": false, "reassign_operation_id": null,
            "worktree": lane, "lane": "test", "lane_claim": "hlc_test", "lane_generation": 1
        }))
        .unwrap();
        ledger.attempts.insert("test".into(), attempt);
        ledger.save(&auth, "ws_test").unwrap();
        let calls = Rc::new(Cell::new(0));
        let coord = Coordinator::new(FakeRemote(calls.clone()), "ws_test".into(), auth).unwrap();

        let clean = refusal(&coord, Some("CAD-1"), None).to_string();
        assert!(
            clean.contains("did not release")
                && clean.contains("clean")
                && clean.contains("merged"),
            "{clean}"
        );
        fs::write(lane.join("file"), "new work\n").unwrap();
        let dirty = refusal(&coord, Some("CAD-1"), None).to_string();
        assert!(
            dirty.contains("did not release") && dirty.contains("uncommitted"),
            "{dirty}"
        );
        git(&lane, &["add", "file"]);
        git(&lane, &["commit", "-m", "work"]);
        let unmerged = refusal(&coord, Some("CAD-1"), None).to_string();
        assert!(
            unmerged.contains("did not release") && unmerged.contains("cannot be confirmed"),
            "{unmerged}"
        );
        git(&repo, &["merge", "--ff-only", "cadence/test"]);
        git(&repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        let merged = refusal(&coord, Some("CAD-1"), None).to_string();
        assert!(
            merged.contains("did not release")
                && merged.contains("clean")
                && merged.contains("merged"),
            "{merged}"
        );
        let wrong_lane = refusal(&coord, Some("CAD-1"), Some(&repo)).to_string();
        assert!(wrong_lane.contains("cannot be confirmed"), "{wrong_lane}");
        let missing = refusal(&coord, Some("CAD-2"), None).to_string();
        assert!(
            missing.contains("did not release") && missing.contains("cannot"),
            "{missing}"
        );
        fs::write(repo.join("main-only"), "main\n").unwrap();
        git(&repo, &["add", "main-only"]);
        git(&repo, &["commit", "-m", "main"]);
        fs::write(lane.join("lane-only"), "lane\n").unwrap();
        git(&lane, &["add", "lane-only"]);
        git(&lane, &["commit", "-m", "lane"]);
        let lane_commit = git(&lane, &["rev-parse", "HEAD"]);
        git(&lane, &["merge", "--no-ff", "--no-commit", "main"]);
        fs::write(lane.join("merge-only"), "unmerged resolution\n").unwrap();
        git(&lane, &["add", "merge-only"]);
        git(&lane, &["commit", "-m", "merge with resolution"]);
        git(&repo, &["cherry-pick", &lane_commit]);
        git(&repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        let merge_only = refusal(&coord, Some("CAD-1"), None).to_string();
        assert!(merge_only.contains("cannot be confirmed"), "{merge_only}");
        ledger.attempts.get_mut("test").unwrap().fenced = true;
        ledger.save(&coord.ledger, "ws_test").unwrap();
        let fenced = refusal(&coord, Some("CAD-1"), None).to_string();
        assert!(fenced.contains("cannot be confirmed"), "{fenced}");
        assert_eq!(
            calls.get(),
            0,
            "hosted refusal must not contact or mutate a tracker"
        );
        assert!(lane.exists(), "hosted refusal must not remove the lane");
    }

    // Independently authored CAD-2203 destructive-release acceptance; do not weaken.
    struct ReleaseCancelReply;

    impl CoordHttp for ReleaseCancelReply {
        fn send(&mut self, method: &str, path: &str, body: Option<&Value>) -> Result<Reply> {
            assert_eq!(method, "POST");
            assert!(path.ends_with("/hla_test/reassign"), "{path}");
            let body = body.expect("cancellation request body");
            assert_eq!(body["organization_id"], "ws_test");
            assert_eq!(body["expected_generation"], 1);
            assert_eq!(body["cancel"], true);
            Ok(Reply {
                status: 200,
                body: json!({
                    "version": VERSION,
                    "assignment_id": "hla_test",
                    "organization_id": "ws_test",
                    "issue_id": "CAD-1",
                    "action": "start",
                    "execution": "local",
                    "state": "cancelled",
                    "generation": 2,
                    "claim_id": null,
                    "target": null,
                    "created_by_subject_id": "usr_test",
                    "client_agent_id": null,
                    "guidance": null,
                    "reported_head_sha": null,
                    "text": null
                }),
            })
        }
    }

    struct CancelReleaseFixture {
        _tmp: tempfile::TempDir,
        repo: PathBuf,
        lane: PathBuf,
        auth: PathBuf,
        state: PathBuf,
    }

    impl CancelReleaseFixture {
        fn new(merged: bool) -> Self {
            let tmp = tempfile::tempdir().unwrap();
            let repo = tmp.path().join("repo");
            let auth = tmp.path().join("auth");
            let state = tmp.path().join("state");
            for dir in [&repo, &auth, &state] {
                fs::create_dir(dir).unwrap();
                fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).unwrap();
            }
            git(&repo, &["init", "-b", "main"]);
            git(&repo, &["config", "user.email", "test@example.invalid"]);
            git(&repo, &["config", "user.name", "Synthetic"]);
            git(&repo, &["config", "commit.gpgsign", "false"]);
            fs::write(repo.join("file"), "base\n").unwrap();
            git(&repo, &["add", "file"]);
            git(&repo, &["commit", "-m", "base"]);
            git(&repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
            git(
                &repo,
                &[
                    "symbolic-ref",
                    "refs/remotes/origin/HEAD",
                    "refs/remotes/origin/main",
                ],
            );
            let lane = repo.join(".cadence/wt/test");
            fs::create_dir_all(lane.parent().unwrap()).unwrap();
            git(
                &repo,
                &[
                    "worktree",
                    "add",
                    "-b",
                    "cadence/test",
                    lane.to_str().unwrap(),
                    "main",
                ],
            );
            fs::write(lane.join("file"), "lane work\n").unwrap();
            git(&lane, &["add", "file"]);
            git(&lane, &["commit", "-m", "lane work"]);
            if merged {
                git(&repo, &["merge", "--ff-only", "cadence/test"]);
                git(&repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
            }
            let past = std::time::SystemTime::now() - Duration::from_secs(2 * 3600);
            fs::OpenOptions::new()
                .write(true)
                .open(lane.join("file"))
                .unwrap()
                .set_modified(past)
                .unwrap();
            let mut ledger = Ledger::load(&auth, "ws_test").unwrap();
            let attempt: Attempt = serde_json::from_value(json!({
                "operation_id": "op_test", "issue_id": "CAD-1", "action": "start",
                "target": null, "assignment_id": "hla_test", "generation": 1,
                "claim_id": "hlc_test", "state": "running", "fenced": false,
                "reassign_operation_id": null, "worktree": lane, "lane": "test",
                "lane_claim": "hlc_test", "lane_generation": 1
            }))
            .unwrap();
            ledger
                .attempts
                .insert(attempt_key("CAD-1", "start", None), attempt);
            ledger.save(&auth, "ws_test").unwrap();
            Self {
                _tmp: tmp,
                repo,
                lane,
                auth,
                state,
            }
        }

        fn cancel(&self) -> Coordinator<ReleaseCancelReply> {
            let mut coord =
                Coordinator::new(ReleaseCancelReply, "ws_test".into(), self.auth.clone()).unwrap();
            let result = coord.reassign("hla_test", None, true).unwrap();
            assert_eq!(result["state"], "cancelled");
            assert_eq!(result["generation"], 2);
            assert_eq!(result["stopped"], false);
            coord
        }

        fn assert_intact(&self, tip: &str) {
            assert!(self.lane.is_dir(), "lane directory was removed");
            assert!(git(&self.repo, &["worktree", "list", "--porcelain"])
                .contains(self.lane.to_str().unwrap()));
            assert_eq!(git(&self.repo, &["rev-parse", "cadence/test"]), tip);
        }

        fn assert_gone(&self) {
            assert!(!self.lane.exists(), "lane directory still exists");
            assert!(!git(&self.repo, &["worktree", "list", "--porcelain"])
                .contains(self.lane.to_str().unwrap()));
            assert!(git(&self.repo, &["branch", "--list", "cadence/test"]).is_empty());
        }
    }

    #[test]
    fn cancellation_then_release_removes_merged_clean_idle_lane() {
        let fx = CancelReleaseFixture::new(true);
        let coord = fx.cancel();
        let result = release_cancelled(&coord, "hla_test", &fx.state)
            .expect("confirmed cancellation should release the merged clean idle lane");
        assert_eq!(result["finished"], true);
        assert_eq!(result["removed_worktree"], true);
        assert_eq!(result["deleted_branch"], true);
        fx.assert_gone();
    }

    #[test]
    fn cancellation_then_release_refuses_unmerged_lane_without_deleting_it() {
        let fx = CancelReleaseFixture::new(false);
        let tip = git(&fx.repo, &["rev-parse", "cadence/test"]);
        let coord = fx.cancel();
        let error = release_cancelled(&coord, "hla_test", &fx.state)
            .expect_err("an unmerged lane must not be deleted")
            .to_string();
        assert!(error.contains("not shown merged"), "{error}");
        fx.assert_intact(&tip);
    }

    #[test]
    fn restarting_same_issue_keeps_cancelled_lane_recoverable() {
        let fx = CancelReleaseFixture::new(true);
        let tip = git(&fx.repo, &["rev-parse", "cadence/test"]);
        let coord = fx.cancel();
        let calls = Rc::new(Cell::new(0));
        let mut starter =
            Coordinator::new(FakeRemote(calls.clone()), "ws_test".into(), fx.auth.clone()).unwrap();
        let error = starter
            .start_here("CAD-1", &fx.repo, None, &mut |_| {
                unreachable!("no local alias is bound")
            })
            .expect_err("starting again must not erase an unreleased cancelled lane")
            .to_string();
        assert!(error.contains("not released"), "{error}");
        assert_eq!(calls.get(), 0, "restart must not queue another assignment");
        fx.assert_intact(&tip);
        release_cancelled(&coord, "hla_test", &fx.state)
            .expect("the original cancellation binding must remain releasable");
        fx.assert_gone();
    }

    #[test]
    fn newer_claim_cannot_overwrite_pending_cancellation_lane() {
        struct ObserveNewClaim {
            auth: PathBuf,
            claim: Value,
        }

        impl CoordHttp for ObserveNewClaim {
            fn send(&mut self, method: &str, path: &str, body: Option<&Value>) -> Result<Reply> {
                assert_eq!(method, "POST");
                assert!(path.ends_with("/hla_test/reassign"), "{path}");
                let body = body.expect("cancellation request body");
                assert_eq!(body["cancel"], true);
                assert_eq!(body["expected_generation"], 1);
                let observer = Coordinator::new(
                    FakeRemote(Rc::new(Cell::new(0))),
                    "ws_test".into(),
                    self.auth.clone(),
                )?;
                let mut current = self.claim.clone();
                current["generation"] = json!(1);
                current["claim_id"] = json!("hlc_test");
                let current = parse_assignment(&current)?;
                assert!(
                    observer.lane_for(&current, None).is_err(),
                    "a current claim must not activate while its cancellation is pending"
                );
                let ledger = Ledger::load(&self.auth, "ws_test")?;
                let attempt = &ledger.attempts[&attempt_key("CAD-1", "start", None)];
                assert!(attempt.retired_lane_guard);
                assert_eq!(attempt.lane_claim.as_deref(), Some("hlc_test"));
                assert_eq!(attempt.lane_generation, Some(1));
                assert!(attempt.worktree.is_some());
                let claim = parse_assignment(&self.claim)?;
                observer.remember_discovered(&claim)?;
                Err(Error::unknown("synthetic lost cancellation response"))
            }
        }

        let fx = CancelReleaseFixture::new(true);
        let tip = git(&fx.repo, &["rev-parse", "cadence/test"]);
        let claim = json!({
            "version": VERSION,
            "assignment_id": "hla_test",
            "organization_id": "ws_test",
            "issue_id": "CAD-1",
            "action": "start",
            "execution": "local",
            "state": "running",
            "generation": 2,
            "claim_id": "hlc_new_claim",
            "target": "usr_test",
            "created_by_subject_id": "usr_test",
            "client_agent_id": "worker",
            "guidance": null,
            "reported_head_sha": null,
            "text": null
        });
        let mut coord = Coordinator::new(
            ObserveNewClaim {
                auth: fx.auth.clone(),
                claim: claim.clone(),
            },
            "ws_test".into(),
            fx.auth.clone(),
        )
        .unwrap();
        let error = coord.reassign("hla_test", None, true).unwrap_err();
        assert_eq!(error.kind(), "unknown");

        let claimed = parse_assignment(&claim).unwrap();
        coord
            .reserve_kickoff(&claimed)
            .expect("the observed newer claim must remain active");
        let error = coord
            .lane_for(&claimed, None)
            .expect_err("a newer claim must not overwrite the retained lane")
            .to_string();
        assert!(error.contains("not released"), "{error}");
        fx.assert_intact(&tip);
        let hidden = fx.lane.with_file_name("temporarily-absent-test");
        fs::rename(&fx.lane, &hidden).unwrap();
        let absent_result = coord.lane_for(&claimed, None);
        fs::rename(&hidden, &fx.lane).unwrap();
        assert!(
            absent_result.is_err(),
            "an absent guarded lane must not be overwritten by a newer claim"
        );
        fx.assert_intact(&tip);

        let ledger = Ledger::load(&fx.auth, "ws_test").unwrap();
        let attempt = ledger
            .attempts
            .get(&attempt_key("CAD-1", "start", None))
            .unwrap();
        assert_eq!(attempt.worktree.as_deref(), fx.lane.to_str());
        assert_eq!(attempt.lane.as_deref(), Some("test"));
        assert_eq!(attempt.lane_claim.as_deref(), Some("hlc_test"));
        assert_eq!(attempt.lane_generation, Some(1));
        assert!(release_cancelled(&coord, "hla_test", &fx.state).is_err());
        fx.assert_intact(&tip);
    }

    #[test]
    fn cancellation_marker_survives_refusal_and_clears_after_release() {
        fn persisted_attempt(auth: &Path) -> Value {
            let path = auth.join("local-assign-ws_test.json");
            let ledger: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
            ledger["attempts"][&attempt_key("CAD-1", "start", None)].clone()
        }

        struct CheckPendingMarker {
            auth: PathBuf,
        }

        impl CoordHttp for CheckPendingMarker {
            fn send(&mut self, method: &str, path: &str, body: Option<&Value>) -> Result<Reply> {
                let attempt = persisted_attempt(&self.auth);
                assert_eq!(attempt["retired_lane_guard"], true);
                assert_eq!(attempt["lane_claim"], "hlc_test");
                assert_eq!(attempt["lane_generation"], 1);
                let loaded = Ledger::load(&self.auth, "ws_test")?;
                let loaded_attempt = loaded
                    .attempts
                    .get(&attempt_key("CAD-1", "start", None))
                    .unwrap();
                assert!(loaded_attempt.retired_lane_guard);
                ReleaseCancelReply.send(method, path, body)
            }
        }

        for merged in [false, true] {
            let fx = CancelReleaseFixture::new(merged);
            let initial = persisted_attempt(&fx.auth);
            assert!(initial.get("retired_lane_guard").is_none());
            let mut coord = Coordinator::new(
                CheckPendingMarker {
                    auth: fx.auth.clone(),
                },
                "ws_test".into(),
                fx.auth.clone(),
            )
            .unwrap();
            coord.reassign("hla_test", None, true).unwrap();
            let confirmed = persisted_attempt(&fx.auth);
            assert_eq!(confirmed["retired_lane_guard"], true);
            assert_eq!(confirmed["lane_claim"], "hlc_test");
            assert_eq!(confirmed["lane_generation"], 1);

            let result = release_cancelled(&coord, "hla_test", &fx.state);
            let after = persisted_attempt(&fx.auth);
            let loaded = Ledger::load(&fx.auth, "ws_test").unwrap();
            let attempt = loaded
                .attempts
                .get(&attempt_key("CAD-1", "start", None))
                .unwrap();
            if merged {
                result.expect("merged clean idle lane should release");
                assert!(after.get("retired_lane_guard").is_none());
                assert!(!attempt.retired_lane_guard);
                assert!(attempt.worktree.is_none());
                assert!(attempt.lane.is_none());
                assert!(attempt.lane_claim.is_none());
                assert!(attempt.lane_generation.is_none());
                fx.assert_gone();
            } else {
                let error = result
                    .expect_err("unmerged lane must not release")
                    .to_string();
                assert!(error.contains("not shown merged"), "{error}");
                assert_eq!(after["retired_lane_guard"], true);
                assert!(attempt.retired_lane_guard);
                assert_eq!(attempt.worktree.as_deref(), fx.lane.to_str());
                assert_eq!(attempt.lane.as_deref(), Some("test"));
                assert_eq!(attempt.lane_claim.as_deref(), Some("hlc_test"));
                assert_eq!(attempt.lane_generation, Some(1));
                assert!(fx.lane.is_dir());
            }
        }
    }

    #[test]
    fn definite_cancellation_refusal_clears_fresh_guard_but_unknown_keeps_it() {
        struct RefusedOrUnknown {
            status: u16,
            code: &'static str,
        }

        impl CoordHttp for RefusedOrUnknown {
            fn send(&mut self, method: &str, path: &str, body: Option<&Value>) -> Result<Reply> {
                assert_eq!(method, "POST");
                assert!(path.ends_with("/hla_test/reassign"), "{path}");
                let body = body.expect("cancellation request body");
                assert_eq!(body["cancel"], true);
                assert_eq!(body["expected_generation"], 1);
                Ok(Reply {
                    status: self.status,
                    body: json!({"code": self.code}),
                })
            }
        }

        for (status, code, definite) in [
            (400, "invalid_request", true),
            (404, "not_found", true),
            (413, "payload_too_large", true),
            (500, "unavailable", false),
        ] {
            let fx = CancelReleaseFixture::new(true);
            let tip = git(&fx.repo, &["rev-parse", "cadence/test"]);
            let mut coord = Coordinator::new(
                RefusedOrUnknown { status, code },
                "ws_test".into(),
                fx.auth.clone(),
            )
            .unwrap();
            let error = coord.reassign("hla_test", None, true).unwrap_err();
            if !definite {
                assert_eq!(error.kind(), "unknown");
            }
            fx.assert_intact(&tip);
            let ledger = Ledger::load(&fx.auth, "ws_test").unwrap();
            let attempt = ledger
                .attempts
                .get(&attempt_key("CAD-1", "start", None))
                .unwrap();
            assert_eq!(attempt.state, "running");
            assert_eq!(attempt.claim_id.as_deref(), Some("hlc_test"));
            assert_eq!(attempt.worktree.as_deref(), fx.lane.to_str());
            assert_eq!(attempt.lane.as_deref(), Some("test"));
            assert_eq!(attempt.lane_claim.as_deref(), Some("hlc_test"));
            assert_eq!(attempt.lane_generation, Some(1));
            if definite {
                assert!(!attempt.retired_lane_guard, "status {status}");
                assert!(attempt.reassign_operation_id.is_none(), "status {status}");
                assert!(attempt.reassign_fingerprint.is_none(), "status {status}");
                assert!(
                    attempt.reassign_expected_generation.is_none(),
                    "status {status}"
                );
            } else {
                assert!(attempt.retired_lane_guard);
                assert!(attempt.reassign_operation_id.is_some());
                assert_eq!(attempt.reassign_fingerprint.as_deref(), Some("cancel"));
                assert_eq!(attempt.reassign_expected_generation, Some(1));
            }
        }
    }

    #[test]
    fn reconcile_only_clears_guard_for_absent_unregistered_lane() {
        let fx = CancelReleaseFixture::new(false);
        let tip = git(&fx.repo, &["rev-parse", "cadence/test"]);
        let coord = fx.cancel();

        assert!(reconcile_absent_guard(&coord, "hla_test").is_err());
        fx.assert_intact(&tip);
        let ledger = Ledger::load(&fx.auth, "ws_test").unwrap();
        assert!(ledger.attempts[&attempt_key("CAD-1", "start", None)].retired_lane_guard);

        fs::remove_dir_all(&fx.lane).unwrap();
        assert!(
            git(&fx.repo, &["worktree", "list", "--porcelain"]).contains(fx.lane.to_str().unwrap())
        );
        assert!(reconcile_absent_guard(&coord, "hla_test").is_err());
        let ledger_path = fx.auth.join("local-assign-ws_test.json");
        let before = fs::read(&ledger_path).unwrap();
        let calls = Rc::new(Cell::new(0));
        let mut starter =
            Coordinator::new(FakeRemote(calls.clone()), "ws_test".into(), fx.auth.clone()).unwrap();
        assert!(
            starter.queue("CAD-1", "start", None).is_err(),
            "a registered guarded lane must block a replacement even if its path is absent"
        );
        assert_eq!(calls.get(), 0, "replacement must not reach the board");
        assert_eq!(fs::read(&ledger_path).unwrap(), before);
        let ledger = Ledger::load(&fx.auth, "ws_test").unwrap();
        let attempt = &ledger.attempts[&attempt_key("CAD-1", "start", None)];
        assert!(attempt.retired_lane_guard);
        assert_eq!(attempt.worktree.as_deref(), fx.lane.to_str());
        assert_eq!(git(&fx.repo, &["rev-parse", "cadence/test"]), tip);

        git(&fx.repo, &["worktree", "prune", "--expire", "now"]);
        assert!(!git(&fx.repo, &["worktree", "list", "--porcelain"])
            .contains(fx.lane.to_str().unwrap()));
        let reconciled = reconcile_absent_guard(&coord, "hla_test")
            .expect("absent unregistered lane should be reconcilable");
        assert_eq!(reconciled["guard_reconciled"], true);
        assert_eq!(reconciled["removed_worktree"], false);
        assert_eq!(reconciled["deleted_branch"], false);
        assert_eq!(reconciled["merge_evidence_verified"], false);
        let ledger = Ledger::load(&fx.auth, "ws_test").unwrap();
        let attempt = &ledger.attempts[&attempt_key("CAD-1", "start", None)];
        assert!(!attempt.retired_lane_guard);
        assert!(attempt.worktree.is_none());
        assert!(attempt.lane.is_none());
        assert!(attempt.lane_claim.is_none());
        assert!(attempt.lane_generation.is_none());
        assert_eq!(git(&fx.repo, &["rev-parse", "cadence/test"]), tip);
    }

    #[test]
    fn observed_cancellation_with_lost_reply_requires_original_replay_before_release() {
        struct ObserveThenLoseReply {
            auth: PathBuf,
            operation: Option<String>,
        }

        impl CoordHttp for ObserveThenLoseReply {
            fn send(&mut self, method: &str, path: &str, body: Option<&Value>) -> Result<Reply> {
                assert_eq!(method, "POST");
                assert!(path.ends_with("/hla_test/reassign"), "{path}");
                let request = body.expect("cancellation request body");
                assert_eq!(request["cancel"], true);
                assert_eq!(request["expected_generation"], 1);
                let operation = request["operation_id"].as_str().unwrap();
                if let Some(original) = &self.operation {
                    assert_eq!(operation, original);
                    return ReleaseCancelReply.send(method, path, body);
                }
                self.operation = Some(operation.to_owned());
                let observer = Coordinator::new(
                    FakeRemote(Rc::new(Cell::new(0))),
                    "ws_test".into(),
                    self.auth.clone(),
                )?;
                let observed = parse_assignment(&json!({
                    "version": VERSION,
                    "assignment_id": "hla_test",
                    "organization_id": "ws_test",
                    "issue_id": "CAD-1",
                    "action": "start",
                    "execution": "local",
                    "state": "cancelled",
                    "generation": 2,
                    "claim_id": null,
                    "target": null,
                    "created_by_subject_id": "usr_test",
                    "client_agent_id": null,
                    "guidance": null,
                    "reported_head_sha": null,
                    "text": null
                }))?;
                observer.remember_discovered(&observed)?;
                Err(Error::unknown("synthetic lost cancellation response"))
            }
        }

        let fx = CancelReleaseFixture::new(true);
        let tip = git(&fx.repo, &["rev-parse", "cadence/test"]);
        let mut coord = Coordinator::new(
            ObserveThenLoseReply {
                auth: fx.auth.clone(),
                operation: None,
            },
            "ws_test".into(),
            fx.auth.clone(),
        )
        .unwrap();
        let error = coord.reassign("hla_test", None, true).unwrap_err();
        assert_eq!(error.kind(), "unknown");
        let ledger = Ledger::load(&fx.auth, "ws_test").unwrap();
        let attempt = &ledger.attempts[&attempt_key("CAD-1", "start", None)];
        assert_eq!(attempt.state, "cancelled");
        assert_eq!(attempt.generation, Some(2));
        assert!(attempt.retired_lane_guard);
        assert!(attempt.reassign_operation_id.is_some());
        assert_eq!(attempt.reassign_expected_generation, Some(1));
        assert!(release(&coord, "CAD-1", None, &fx.state).is_err());
        assert!(release_cancelled(&coord, "hla_test", &fx.state).is_err());
        fx.assert_intact(&tip);

        coord.reassign("hla_test", None, true).unwrap();
        let result = release_cancelled(&coord, "hla_test", &fx.state)
            .expect("the confirmed original cancellation may release the merged idle lane");
        assert_eq!(result["removed_worktree"], true);
        assert_eq!(result["deleted_branch"], true);
        fx.assert_gone();
    }

    #[test]
    fn pending_cancellation_survives_terminal_observation_and_blocks_replacement() {
        struct ObserveTerminalThenLoseReply {
            auth: PathBuf,
            terminal: &'static str,
            operation: Option<String>,
        }

        impl CoordHttp for ObserveTerminalThenLoseReply {
            fn send(&mut self, method: &str, path: &str, body: Option<&Value>) -> Result<Reply> {
                assert_eq!(method, "POST");
                assert!(path.ends_with("/hla_test/reassign"), "{path}");
                let request = body.expect("cancellation request body");
                assert_eq!(request["cancel"], true);
                assert_eq!(request["expected_generation"], 1);
                let operation = request["operation_id"].as_str().unwrap();
                if let Some(original) = &self.operation {
                    assert_eq!(operation, original);
                    return ReleaseCancelReply.send(method, path, body);
                }
                self.operation = Some(operation.to_owned());
                let observer = Coordinator::new(
                    FakeRemote(Rc::new(Cell::new(0))),
                    "ws_test".into(),
                    self.auth.clone(),
                )?;
                let observed = parse_assignment(&json!({
                    "version": VERSION,
                    "assignment_id": "hla_test",
                    "organization_id": "ws_test",
                    "issue_id": "CAD-1",
                    "action": "start",
                    "execution": "local",
                    "state": self.terminal,
                    "generation": 1,
                    "claim_id": "hlc_test",
                    "target": null,
                    "created_by_subject_id": "usr_test",
                    "client_agent_id": "worker",
                    "guidance": null,
                    "reported_head_sha": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "text": "terminal observation"
                }))?;
                observer.remember_discovered(&observed)?;
                Err(Error::unknown("synthetic lost cancellation response"))
            }
        }

        for terminal in ["result", "failed"] {
            let fx = CancelReleaseFixture::new(true);
            let tip = git(&fx.repo, &["rev-parse", "cadence/test"]);
            let mut coord = Coordinator::new(
                ObserveTerminalThenLoseReply {
                    auth: fx.auth.clone(),
                    terminal,
                    operation: None,
                },
                "ws_test".into(),
                fx.auth.clone(),
            )
            .unwrap();
            let error = coord.reassign("hla_test", None, true).unwrap_err();
            assert_eq!(error.kind(), "unknown");
            let ledger = Ledger::load(&fx.auth, "ws_test").unwrap();
            let attempt = &ledger.attempts[&attempt_key("CAD-1", "start", None)];
            assert_eq!(attempt.state, terminal);
            assert!(attempt.retired_lane_guard);
            assert!(attempt.reassign_operation_id.is_some());
            assert_eq!(attempt.worktree.as_deref(), fx.lane.to_str());
            assert_eq!(attempt.lane.as_deref(), Some("test"));
            assert_eq!(attempt.lane_claim.as_deref(), Some("hlc_test"));
            assert_eq!(attempt.lane_generation, Some(1));
            assert!(release(&coord, "CAD-1", None, &fx.state).is_err());
            fx.assert_intact(&tip);

            let hidden = fx.lane.with_file_name("temporarily-absent-test");
            fs::rename(&fx.lane, &hidden).unwrap();
            let calls = Rc::new(Cell::new(0));
            let mut starter =
                Coordinator::new(FakeRemote(calls.clone()), "ws_test".into(), fx.auth.clone())
                    .unwrap();
            let replacement = starter.queue("CAD-1", "start", None);
            fs::rename(&hidden, &fx.lane).unwrap();
            assert!(
                replacement.is_err(),
                "{terminal}: replacement must be refused"
            );
            assert_eq!(
                calls.get(),
                0,
                "{terminal}: no replacement may reach the board"
            );
            let ledger = Ledger::load(&fx.auth, "ws_test").unwrap();
            let attempt = &ledger.attempts[&attempt_key("CAD-1", "start", None)];
            assert_eq!(attempt.state, terminal);
            assert!(attempt.retired_lane_guard);
            assert!(attempt.reassign_operation_id.is_some());
            assert_eq!(attempt.worktree.as_deref(), fx.lane.to_str());
            assert_eq!(attempt.lane_claim.as_deref(), Some("hlc_test"));
            assert_eq!(attempt.lane_generation, Some(1));
            fx.assert_intact(&tip);

            coord.reassign("hla_test", None, true).unwrap();
            let released = release_cancelled(&coord, "hla_test", &fx.state)
                .expect("confirmed cancellation should release the merged lane");
            assert_eq!(released["removed_worktree"], true);
            assert_eq!(released["deleted_branch"], true);
            fx.assert_gone();
        }
    }

    #[test]
    fn pending_cancellation_refuses_changed_intent_but_replays_original() {
        struct LoseThenConfirm {
            calls: Rc<Cell<usize>>,
            operation: Option<String>,
        }

        impl CoordHttp for LoseThenConfirm {
            fn send(&mut self, method: &str, path: &str, body: Option<&Value>) -> Result<Reply> {
                assert_eq!(method, "POST");
                assert!(path.ends_with("/hla_test/reassign"), "{path}");
                let request = body.expect("reassignment request body");
                assert_eq!(request["cancel"], true);
                assert_eq!(request["expected_generation"], 1);
                let operation = request["operation_id"].as_str().unwrap();
                self.calls.set(self.calls.get() + 1);
                if let Some(original) = &self.operation {
                    assert_eq!(operation, original, "replay changed the operation id");
                    return ReleaseCancelReply.send(method, path, body);
                }
                self.operation = Some(operation.to_owned());
                Err(Error::unknown("synthetic lost cancellation response"))
            }
        }

        let fx = CancelReleaseFixture::new(true);
        let tip = git(&fx.repo, &["rev-parse", "cadence/test"]);
        let calls = Rc::new(Cell::new(0));
        let mut coord = Coordinator::new(
            LoseThenConfirm {
                calls: calls.clone(),
                operation: None,
            },
            "ws_test".into(),
            fx.auth.clone(),
        )
        .unwrap();
        let first = coord.reassign("hla_test", None, true).unwrap_err();
        assert_eq!(first.kind(), "unknown");
        assert_eq!(calls.get(), 1);
        let ledger_path = fx.auth.join("local-assign-ws_test.json");
        let before = fs::read(&ledger_path).unwrap();
        assert!(
            coord
                .reassign("hla_test", Some("usr_other"), false)
                .is_err(),
            "a different intent must not replace an unresolved cancellation"
        );
        assert_eq!(calls.get(), 1, "changed intent must not reach the board");
        assert_eq!(fs::read(&ledger_path).unwrap(), before);
        fx.assert_intact(&tip);

        coord.reassign("hla_test", None, true).unwrap();
        assert_eq!(calls.get(), 2);
        let result = release_cancelled(&coord, "hla_test", &fx.state)
            .expect("confirmed replay should release the merged idle lane");
        assert_eq!(result["removed_worktree"], true);
        assert_eq!(result["deleted_branch"], true);
        fx.assert_gone();
    }

    #[test]
    fn concurrent_cancellation_replays_serialize_guard_reconciliation() {
        use std::sync::mpsc;

        struct FirstRefusal {
            entered: mpsc::Sender<()>,
            proceed: mpsc::Receiver<()>,
        }

        impl CoordHttp for FirstRefusal {
            fn send(&mut self, method: &str, path: &str, body: Option<&Value>) -> Result<Reply> {
                assert_eq!(method, "POST");
                assert!(path.ends_with("/hla_test/reassign"), "{path}");
                assert_eq!(body.unwrap()["cancel"], true);
                self.entered.send(()).unwrap();
                self.proceed.recv().unwrap();
                Ok(Reply {
                    status: 400,
                    body: json!({"code": "invalid_request"}),
                })
            }
        }

        struct SecondUnknown {
            entered: mpsc::Sender<()>,
            proceed: mpsc::Receiver<()>,
        }

        impl CoordHttp for SecondUnknown {
            fn send(&mut self, method: &str, path: &str, body: Option<&Value>) -> Result<Reply> {
                assert_eq!(method, "POST");
                assert!(path.ends_with("/hla_test/reassign"), "{path}");
                assert_eq!(body.unwrap()["cancel"], true);
                self.entered.send(()).unwrap();
                self.proceed.recv().unwrap();
                Err(Error::unknown("synthetic lost response"))
            }
        }

        let fx = CancelReleaseFixture::new(true);
        let tip = git(&fx.repo, &["rev-parse", "cadence/test"]);
        let (first_entered_tx, first_entered_rx) = mpsc::channel();
        let (first_proceed_tx, first_proceed_rx) = mpsc::channel();
        let first_auth = fx.auth.clone();
        let first = std::thread::spawn(move || {
            let mut coord = Coordinator::new(
                FirstRefusal {
                    entered: first_entered_tx,
                    proceed: first_proceed_rx,
                },
                "ws_test".into(),
                first_auth,
            )
            .unwrap();
            coord
                .reassign("hla_test", None, true)
                .unwrap_err()
                .code()
                .map(str::to_owned)
        });
        first_entered_rx
            .recv_timeout(Duration::from_secs(3))
            .unwrap();

        let (waiting_tx, waiting_rx) = mpsc::channel();
        let (second_entered_tx, second_entered_rx) = mpsc::channel();
        let (second_proceed_tx, second_proceed_rx) = mpsc::channel();
        let second_auth = fx.auth.clone();
        let second = std::thread::spawn(move || {
            let mut coord = Coordinator::new(
                SecondUnknown {
                    entered: second_entered_tx,
                    proceed: second_proceed_rx,
                },
                "ws_test".into(),
                second_auth,
            )
            .unwrap();
            crate::remote_cli::CREDENTIAL_WAITING.with(|waiting| {
                *waiting.borrow_mut() = Some(waiting_tx);
            });
            coord
                .reassign("hla_test", None, true)
                .unwrap_err()
                .kind()
                .to_owned()
        });
        waiting_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        assert!(matches!(
            second_entered_rx.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        let ledger = Ledger::load(&fx.auth, "ws_test").unwrap();
        let attempt = &ledger.attempts[&attempt_key("CAD-1", "start", None)];
        assert!(attempt.retired_lane_guard);
        assert!(attempt.reassign_operation_id.is_some());
        fx.assert_intact(&tip);

        first_proceed_tx.send(()).unwrap();
        assert_eq!(first.join().unwrap().as_deref(), Some("invalid_request"));
        second_entered_rx
            .recv_timeout(Duration::from_secs(3))
            .unwrap();
        let ledger = Ledger::load(&fx.auth, "ws_test").unwrap();
        let attempt = &ledger.attempts[&attempt_key("CAD-1", "start", None)];
        assert!(attempt.retired_lane_guard);
        assert!(attempt.reassign_operation_id.is_some());
        assert_eq!(attempt.worktree.as_deref(), fx.lane.to_str());
        second_proceed_tx.send(()).unwrap();
        assert_eq!(second.join().unwrap(), "unknown");
        let ledger = Ledger::load(&fx.auth, "ws_test").unwrap();
        let attempt = &ledger.attempts[&attempt_key("CAD-1", "start", None)];
        assert!(attempt.retired_lane_guard);
        assert!(attempt.reassign_operation_id.is_some());
        fx.assert_intact(&tip);
    }

    #[test]
    fn confirmed_guard_blocks_new_reassignment_until_release() {
        let fx = CancelReleaseFixture::new(true);
        let tip = git(&fx.repo, &["rev-parse", "cadence/test"]);
        let coord = fx.cancel();
        let ledger_path = fx.auth.join("local-assign-ws_test.json");
        let before = fs::read(&ledger_path).unwrap();
        let calls = Rc::new(Cell::new(0));
        let mut retry =
            Coordinator::new(FakeRemote(calls.clone()), "ws_test".into(), fx.auth.clone()).unwrap();
        assert!(
            retry
                .reassign("hla_test", Some("usr_other"), false)
                .is_err(),
            "a confirmed unreleased guard must block a fresh reassignment"
        );
        assert_eq!(calls.get(), 0, "reassignment must not reach the board");
        assert_eq!(fs::read(&ledger_path).unwrap(), before);
        fx.assert_intact(&tip);
        let released = release_cancelled(&coord, "hla_test", &fx.state)
            .expect("the original confirmed cancellation must remain releasable");
        assert_eq!(released["removed_worktree"], true);
        assert_eq!(released["deleted_branch"], true);
        fx.assert_gone();
    }

    #[test]
    fn definite_refusal_without_lane_allows_a_different_reassignment_intent() {
        struct RefuseRequests(Rc<std::cell::RefCell<Vec<Value>>>);

        impl CoordHttp for RefuseRequests {
            fn send(&mut self, method: &str, path: &str, body: Option<&Value>) -> Result<Reply> {
                assert_eq!(method, "POST");
                assert!(path.ends_with("/hla_test/reassign"), "{path}");
                self.0
                    .borrow_mut()
                    .push(body.expect("request body").clone());
                Ok(Reply {
                    status: 400,
                    body: json!({"code": "invalid_request"}),
                })
            }
        }

        for cancel_first in [false, true] {
            let fx = CancelReleaseFixture::new(true);
            let tip = git(&fx.repo, &["rev-parse", "cadence/test"]);
            let mut ledger = Ledger::load(&fx.auth, "ws_test").unwrap();
            let attempt = ledger
                .attempts
                .get_mut(&attempt_key("CAD-1", "start", None))
                .unwrap();
            attempt.state = "queued".to_string();
            attempt.claim_id = None;
            attempt.worktree = None;
            attempt.lane = None;
            attempt.lane_claim = None;
            attempt.lane_generation = None;
            ledger.save(&fx.auth, "ws_test").unwrap();

            let requests = Rc::new(std::cell::RefCell::new(Vec::new()));
            let mut coord = Coordinator::new(
                RefuseRequests(requests.clone()),
                "ws_test".into(),
                fx.auth.clone(),
            )
            .unwrap();
            let first = if cancel_first {
                coord.reassign("hla_test", None, true)
            } else {
                coord.reassign("hla_test", Some("usr_other"), false)
            };
            assert_eq!(first.unwrap_err().code(), Some("invalid_request"));
            let second = if cancel_first {
                coord.reassign("hla_test", Some("usr_other"), false)
            } else {
                coord.reassign("hla_test", None, true)
            };
            assert_eq!(second.unwrap_err().code(), Some("invalid_request"));

            let requests = requests.borrow();
            assert_eq!(requests.len(), 2, "changed intent must reach the board");
            let first_id = requests[0]["operation_id"].as_str().unwrap();
            let second_id = requests[1]["operation_id"].as_str().unwrap();
            assert_ne!(first_id, second_id);
            assert_eq!(requests[0]["expected_generation"], 1);
            assert_eq!(requests[1]["expected_generation"], 1);
            if cancel_first {
                assert_eq!(requests[0]["cancel"], true);
                assert_eq!(requests[1]["target"], "usr_other");
            } else {
                assert_eq!(requests[0]["target"], "usr_other");
                assert_eq!(requests[1]["cancel"], true);
            }
            let ledger = Ledger::load(&fx.auth, "ws_test").unwrap();
            let attempt = &ledger.attempts[&attempt_key("CAD-1", "start", None)];
            assert!(attempt.reassign_operation_id.is_none());
            assert!(attempt.reassign_fingerprint.is_none());
            assert!(attempt.reassign_expected_generation.is_none());
            assert!(!attempt.retired_lane_guard);
            fx.assert_intact(&tip);
        }
    }

    #[test]
    fn unknown_cancellation_before_lane_reservation_blocks_activation() {
        struct LoseCancellation;

        impl CoordHttp for LoseCancellation {
            fn send(&mut self, method: &str, path: &str, body: Option<&Value>) -> Result<Reply> {
                assert_eq!(method, "POST");
                assert!(path.ends_with("/hla_test/reassign"), "{path}");
                let request = body.expect("cancellation request body");
                assert_eq!(request["cancel"], true);
                assert_eq!(request["expected_generation"], 1);
                Err(Error::unknown("synthetic lost cancellation response"))
            }
        }

        let fx = CancelReleaseFixture::new(true);
        let tip = git(&fx.repo, &["rev-parse", "cadence/test"]);
        let mut ledger = Ledger::load(&fx.auth, "ws_test").unwrap();
        let attempt = ledger
            .attempts
            .get_mut(&attempt_key("CAD-1", "start", None))
            .unwrap();
        attempt.worktree = None;
        attempt.lane = None;
        attempt.lane_claim = None;
        attempt.lane_generation = None;
        ledger.save(&fx.auth, "ws_test").unwrap();
        let mut coord =
            Coordinator::new(LoseCancellation, "ws_test".into(), fx.auth.clone()).unwrap();
        let error = coord.reassign("hla_test", None, true).unwrap_err();
        assert_eq!(error.kind(), "unknown");

        let ledger_path = fx.auth.join("local-assign-ws_test.json");
        let before = fs::read(&ledger_path).unwrap();
        let ledger = Ledger::load(&fx.auth, "ws_test").unwrap();
        let attempt = &ledger.attempts[&attempt_key("CAD-1", "start", None)];
        assert!(attempt.retired_lane_guard);
        assert!(attempt.reassign_operation_id.is_some());
        assert_eq!(attempt.reassign_expected_generation, Some(1));
        assert!(attempt.worktree.is_none());
        assert!(attempt.lane.is_none());
        let claimed = parse_assignment(&json!({
            "version": VERSION,
            "assignment_id": "hla_test",
            "organization_id": "ws_test",
            "issue_id": "CAD-1",
            "action": "start",
            "execution": "local",
            "state": "running",
            "generation": 1,
            "claim_id": "hlc_test",
            "target": "usr_test",
            "created_by_subject_id": "usr_test",
            "client_agent_id": "worker",
            "guidance": null,
            "reported_head_sha": null,
            "text": null
        }))
        .unwrap();
        assert!(
            coord.lane_for(&claimed, None).is_err(),
            "activation must not reserve a lane while cancellation is unresolved"
        );
        assert_eq!(fs::read(&ledger_path).unwrap(), before);
        fx.assert_intact(&tip);
    }

    #[test]
    fn cancellation_waits_for_lane_open_and_preserves_the_recorded_binding() {
        use std::sync::{mpsc, Arc, Mutex};

        struct ClaimReply(Value);

        impl CoordHttp for ClaimReply {
            fn send(&mut self, method: &str, path: &str, body: Option<&Value>) -> Result<Reply> {
                assert_eq!(method, "POST");
                assert!(path.ends_with("/hla_test/claim"), "{path}");
                assert_eq!(body.unwrap()["expected_generation"], 1);
                Ok(Reply {
                    status: 200,
                    body: self.0.clone(),
                })
            }
        }

        let fx = CancelReleaseFixture::new(true);
        git(
            &fx.repo,
            &["remote", "add", "origin", fx.repo.to_str().unwrap()],
        );
        let mut ledger = Ledger::load(&fx.auth, "ws_test").unwrap();
        let client = ledger.client_agent_id.clone();
        let attempt = ledger
            .attempts
            .get_mut(&attempt_key("CAD-1", "start", None))
            .unwrap();
        attempt.worktree = None;
        attempt.lane = None;
        attempt.lane_claim = None;
        attempt.lane_generation = None;
        ledger.save(&fx.auth, "ws_test").unwrap();
        let claim = json!({
            "version": VERSION,
            "assignment_id": "hla_test",
            "organization_id": "ws_test",
            "issue_id": "CAD-1",
            "action": "start",
            "execution": "local",
            "state": "running",
            "generation": 1,
            "claim_id": "hlc_test",
            "target": "usr_test",
            "created_by_subject_id": "usr_test",
            "client_agent_id": client,
            "guidance": null,
            "reported_head_sha": null,
            "text": null
        });
        let (opening_tx, opening_rx) = mpsc::channel();
        let (continue_tx, continue_rx) = mpsc::channel();
        let activation_auth = fx.auth.clone();
        let activation_repo = fx.repo.clone();
        let activation = std::thread::spawn(move || {
            let claimed = parse_assignment(&claim).unwrap();
            let mut coord =
                Coordinator::new(ClaimReply(claim), "ws_test".into(), activation_auth).unwrap();
            let continue_rx = Mutex::new(continue_rx);
            coord.before_lane_open = Some(Arc::new(move || {
                opening_tx.send(()).unwrap();
                continue_rx.lock().unwrap().recv().unwrap();
            }));
            coord
                .activate(&claimed, &activation_repo, None, "synthetic", &mut |_| {
                    Ok(json!({}))
                })
                .unwrap()
        });
        opening_rx.recv_timeout(Duration::from_secs(3)).unwrap();

        let (waiting_tx, waiting_rx) = mpsc::channel();
        let cancellation_auth = fx.auth.clone();
        let cancellation = std::thread::spawn(move || {
            let mut coord =
                Coordinator::new(ReleaseCancelReply, "ws_test".into(), cancellation_auth).unwrap();
            crate::remote_cli::CREDENTIAL_WAITING.with(|waiting| {
                *waiting.borrow_mut() = Some(waiting_tx);
            });
            coord.reassign("hla_test", None, true).unwrap()
        });
        waiting_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        let ledger = Ledger::load(&fx.auth, "ws_test").unwrap();
        let attempt = &ledger.attempts[&attempt_key("CAD-1", "start", None)];
        assert!(
            attempt.worktree.is_none(),
            "checkout must not yet be recorded"
        );
        assert_eq!(attempt.lane_claim.as_deref(), Some("hlc_test"));
        assert_eq!(attempt.lane_generation, Some(1));

        continue_tx.send(()).unwrap();
        let activated = activation.join().unwrap();
        let cancelled = cancellation.join().unwrap();
        assert_eq!(cancelled["state"], "cancelled");
        let path = PathBuf::from(activated["worktree"].as_str().unwrap());
        let lane = activated["lane"].as_str().unwrap();
        assert!(path.is_dir());
        fs::remove_file(path.join(".env")).unwrap();
        let ledger = Ledger::load(&fx.auth, "ws_test").unwrap();
        let attempt = &ledger.attempts[&attempt_key("CAD-1", "start", None)];
        assert!(attempt.retired_lane_guard);
        assert_eq!(attempt.worktree.as_deref(), path.to_str());
        assert_eq!(attempt.lane.as_deref(), Some(lane));
        assert_eq!(attempt.lane_claim.as_deref(), Some("hlc_test"));
        assert_eq!(attempt.lane_generation, Some(1));

        fs::write(path.join("file"), "new lane work\n").unwrap();
        git(&path, &["add", "file"]);
        git(&path, &["commit", "-m", "new lane work"]);
        let branch = format!("cadence/{lane}");
        git(&fx.repo, &["merge", "--ff-only", &branch]);
        git(
            &fx.repo,
            &["update-ref", "refs/remotes/origin/main", "HEAD"],
        );
        let past = std::time::SystemTime::now() - Duration::from_secs(2 * 3600);
        fs::OpenOptions::new()
            .write(true)
            .open(path.join("file"))
            .unwrap()
            .set_modified(past)
            .unwrap();
        let coord = Coordinator::new(
            FakeRemote(Rc::new(Cell::new(0))),
            "ws_test".into(),
            fx.auth.clone(),
        )
        .unwrap();
        let result = release_cancelled(&coord, "hla_test", &fx.state)
            .expect("confirmed cancellation must release the exact merged lane");
        assert_eq!(result["removed_worktree"], true);
        assert_eq!(result["deleted_branch"], true);
        assert!(!path.exists());
        assert!(git(&fx.repo, &["branch", "--list", &branch]).is_empty());
    }

    #[test]
    fn session_lane_open_finishes_before_cancellation_and_remains_releasable() {
        use std::sync::{mpsc, Arc, Mutex};

        struct SessionList(Value);

        impl CoordHttp for SessionList {
            fn send(&mut self, method: &str, path: &str, _: Option<&Value>) -> Result<Reply> {
                assert_eq!(method, "GET");
                let rows = if path.ends_with("?state=running") {
                    vec![self.0.clone()]
                } else {
                    Vec::new()
                };
                Ok(Reply {
                    status: 200,
                    body: json!({"version": VERSION, "assignments": rows}),
                })
            }
        }

        let fx = CancelReleaseFixture::new(true);
        git(
            &fx.repo,
            &["remote", "add", "origin", fx.repo.to_str().unwrap()],
        );
        let mut ledger = Ledger::load(&fx.auth, "ws_test").unwrap();
        let attempt = ledger
            .attempts
            .get_mut(&attempt_key("CAD-1", "start", None))
            .unwrap();
        attempt.worktree = None;
        attempt.lane = None;
        attempt.lane_claim = None;
        attempt.lane_generation = None;
        ledger.save(&fx.auth, "ws_test").unwrap();
        let claim = json!({
            "version": VERSION,
            "assignment_id": "hla_test",
            "organization_id": "ws_test",
            "issue_id": "CAD-1",
            "action": "start",
            "execution": "local",
            "state": "running",
            "generation": 1,
            "claim_id": "hlc_test",
            "target": "usr_test",
            "created_by_subject_id": "usr_test",
            "client_agent_id": "session-ap_test",
            "guidance": null,
            "reported_head_sha": null,
            "text": null
        });
        let (opening_tx, opening_rx) = mpsc::channel();
        let (continue_tx, continue_rx) = mpsc::channel();
        let session_auth = fx.auth.clone();
        let session_repo = fx.repo.clone();
        let session = std::thread::spawn(move || {
            let mut coord = Coordinator::open(
                SessionList(claim),
                "ws_test".into(),
                session_auth,
                Some("usr_test".into()),
                None,
                String::new(),
            )
            .unwrap();
            let continue_rx = Mutex::new(continue_rx);
            coord.before_lane_open = Some(Arc::new(move || {
                opening_tx.send(()).unwrap();
                continue_rx.lock().unwrap().recv().unwrap();
            }));
            coord
                .start_session("CAD-1", &session_repo, None, "ap_test", None)
                .unwrap()
        });
        opening_rx.recv_timeout(Duration::from_secs(3)).unwrap();

        let (waiting_tx, waiting_rx) = mpsc::channel();
        let cancellation_auth = fx.auth.clone();
        let cancellation = std::thread::spawn(move || {
            let mut coord =
                Coordinator::new(ReleaseCancelReply, "ws_test".into(), cancellation_auth).unwrap();
            crate::remote_cli::CREDENTIAL_WAITING.with(|waiting| {
                *waiting.borrow_mut() = Some(waiting_tx);
            });
            coord.reassign("hla_test", None, true).unwrap()
        });
        waiting_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        let ledger = Ledger::load(&fx.auth, "ws_test").unwrap();
        let attempt = &ledger.attempts[&attempt_key("CAD-1", "start", None)];
        assert!(
            attempt.worktree.is_none(),
            "session checkout is not recorded yet"
        );
        assert_eq!(attempt.lane_claim.as_deref(), Some("hlc_test"));
        assert_eq!(attempt.lane_generation, Some(1));

        continue_tx.send(()).unwrap();
        let started = session.join().unwrap();
        let cancelled = cancellation.join().unwrap();
        assert_eq!(cancelled["state"], "cancelled");
        let path = PathBuf::from(started["worktree"].as_str().unwrap());
        let lane = started["lane"].as_str().unwrap();
        assert!(path.is_dir());
        fs::remove_file(path.join(".env")).unwrap();
        let ledger = Ledger::load(&fx.auth, "ws_test").unwrap();
        let attempt = &ledger.attempts[&attempt_key("CAD-1", "start", None)];
        assert!(attempt.retired_lane_guard);
        assert_eq!(attempt.worktree.as_deref(), path.to_str());
        assert_eq!(attempt.lane.as_deref(), Some(lane));
        assert_eq!(attempt.lane_claim.as_deref(), Some("hlc_test"));
        assert_eq!(attempt.lane_generation, Some(1));

        fs::write(path.join("file"), "session lane work\n").unwrap();
        git(&path, &["add", "file"]);
        git(&path, &["commit", "-m", "session lane work"]);
        let branch = format!("cadence/{lane}");
        git(&fx.repo, &["merge", "--ff-only", &branch]);
        git(
            &fx.repo,
            &["update-ref", "refs/remotes/origin/main", "HEAD"],
        );
        let past = std::time::SystemTime::now() - Duration::from_secs(2 * 3600);
        fs::OpenOptions::new()
            .write(true)
            .open(path.join("file"))
            .unwrap()
            .set_modified(past)
            .unwrap();
        let coord = Coordinator::new(
            FakeRemote(Rc::new(Cell::new(0))),
            "ws_test".into(),
            fx.auth.clone(),
        )
        .unwrap();
        let result = release_cancelled(&coord, "hla_test", &fx.state)
            .expect("confirmed cancellation must release the exact merged session lane");
        assert_eq!(result["removed_worktree"], true);
        assert_eq!(result["deleted_branch"], true);
        assert!(!path.exists());
        assert!(git(&fx.repo, &["branch", "--list", &branch]).is_empty());
    }

    #[test]
    fn confirmed_cancellation_distinguishes_unreserved_and_partially_reserved_lanes() {
        for partial in [false, true] {
            let fx = CancelReleaseFixture::new(true);
            let tip = git(&fx.repo, &["rev-parse", "cadence/test"]);
            let mut ledger = Ledger::load(&fx.auth, "ws_test").unwrap();
            let attempt = ledger
                .attempts
                .get_mut(&attempt_key("CAD-1", "start", None))
                .unwrap();
            attempt.worktree = None;
            if !partial {
                attempt.lane = None;
                attempt.lane_claim = None;
                attempt.lane_generation = None;
            }
            ledger.save(&fx.auth, "ws_test").unwrap();

            let mut coord =
                Coordinator::new(ReleaseCancelReply, "ws_test".into(), fx.auth.clone()).unwrap();
            let cancelled = coord.reassign("hla_test", None, true).unwrap();
            assert_eq!(cancelled["state"], "cancelled");
            assert_eq!(cancelled["generation"], 2);
            assert!(cancelled.get("release").is_none());
            let ledger = Ledger::load(&fx.auth, "ws_test").unwrap();
            let attempt = &ledger.attempts[&attempt_key("CAD-1", "start", None)];
            assert_eq!(attempt.retired_lane_guard, partial);
            assert!(attempt.reassign_operation_id.is_none());
            assert!(attempt.reassign_fingerprint.is_none());
            assert!(attempt.reassign_expected_generation.is_none());
            assert!(attempt.worktree.is_none());
            if partial {
                assert_eq!(attempt.lane.as_deref(), Some("test"));
                assert_eq!(attempt.lane_claim.as_deref(), Some("hlc_test"));
                assert_eq!(attempt.lane_generation, Some(1));
                let calls = Rc::new(Cell::new(0));
                let mut starter =
                    Coordinator::new(FakeRemote(calls.clone()), "ws_test".into(), fx.auth.clone())
                        .unwrap();
                let before = fs::read(fx.auth.join("local-assign-ws_test.json")).unwrap();
                assert!(starter.queue("CAD-1", "start", None).is_err());
                assert_eq!(calls.get(), 0, "replacement must not contact the board");
                assert_eq!(
                    fs::read(fx.auth.join("local-assign-ws_test.json")).unwrap(),
                    before
                );
            } else {
                assert!(attempt.lane.is_none());
                assert!(attempt.lane_claim.is_none());
                assert!(attempt.lane_generation.is_none());
            }
            assert!(release_cancelled(&coord, "hla_test", &fx.state).is_err());
            fx.assert_intact(&tip);
        }
    }

    #[test]
    fn hosted_release_rebinds_only_the_matching_active_generic_lifecycle() {
        use crate::worktree::lifecycle::{self, CheckoutSpec};

        for case in [
            "matching",
            "foreign-tool",
            "wrong-branch",
            "other-issue",
            "retained",
        ] {
            let fx = CancelReleaseFixture::new(true);
            let tip = git(&fx.repo, &["rev-parse", "cadence/test"]);
            let tool = if case == "foreign-tool" {
                "another tool"
            } else {
                "cadence agent worktree"
            };
            let branch = if case == "wrong-branch" {
                "cadence/other"
            } else {
                "cadence/test"
            };
            let issue = if case == "other-issue" {
                Some("CAD-2")
            } else {
                None
            };
            let record = lifecycle::new_record(CheckoutSpec {
                repo: &fx.repo,
                purpose: "development",
                tool,
                owner: "synthetic",
                path: &fx.lane,
                branch: Some(branch),
                pinned_sha: &tip,
                issue,
            });
            lifecycle::activate(&fx.repo, record).unwrap();
            if case == "retained" {
                lifecycle::transition(&fx.repo, &fx.lane, "retained", Some("keep this lane"))
                    .unwrap();
            }
            let before = lifecycle::managed_record(&fx.repo, &fx.lane)
                .unwrap()
                .unwrap();
            let coord = fx.cancel();
            let result = release_cancelled(&coord, "hla_test", &fx.state);
            if case == "matching" {
                let released = result.expect("matching managed hosted lane should release");
                assert_eq!(released["removed_worktree"], true);
                assert_eq!(released["deleted_branch"], true);
                fx.assert_gone();
                let after = lifecycle::managed_record(&fx.repo, &fx.lane)
                    .unwrap()
                    .unwrap();
                assert_eq!(after.issue.as_deref(), Some("CAD-1"));
                assert_eq!(after.state, "released");
            } else {
                assert!(
                    result.is_err(),
                    "{case}: incompatible lifecycle must refuse"
                );
                fx.assert_intact(&tip);
                let after = lifecycle::managed_record(&fx.repo, &fx.lane)
                    .unwrap()
                    .unwrap();
                assert_eq!(
                    serde_json::to_value(&after).unwrap(),
                    serde_json::to_value(&before).unwrap(),
                    "{case}: refusing release must not rebind the lifecycle record"
                );
            }
        }
    }

    #[test]
    fn guarded_discovered_assignment_cannot_be_bypassed_by_queue() {
        let fx = CancelReleaseFixture::new(true);
        let tip = git(&fx.repo, &["rev-parse", "cadence/test"]);
        fx.cancel();
        let mut ledger = Ledger::load(&fx.auth, "ws_test").unwrap();
        let attempt = ledger
            .attempts
            .remove(&attempt_key("CAD-1", "start", None))
            .unwrap();
        ledger
            .attempts
            .insert("discovered:hla_test".into(), attempt);
        ledger.save(&fx.auth, "ws_test").unwrap();
        let ledger_path = fx.auth.join("local-assign-ws_test.json");
        let before = fs::read(&ledger_path).unwrap();
        let calls = Rc::new(Cell::new(0));
        let mut coord =
            Coordinator::new(FakeRemote(calls.clone()), "ws_test".into(), fx.auth.clone()).unwrap();
        assert!(
            coord.queue("CAD-1", "start", None).is_err(),
            "a guarded discovered row must block replacement"
        );
        assert_eq!(calls.get(), 0, "replacement must not reach the board");
        assert_eq!(fs::read(&ledger_path).unwrap(), before);
        fx.assert_intact(&tip);
    }

    #[test]
    fn exact_lane_selection_releases_only_the_selected_assignment() {
        let fx = CancelReleaseFixture::new(true);
        let other = fx.repo.join(".cadence/wt/other");
        git(
            &fx.repo,
            &[
                "worktree",
                "add",
                "-b",
                "cadence/other",
                other.to_str().unwrap(),
                "main",
            ],
        );
        fs::write(other.join("file"), "other merged work\n").unwrap();
        git(&other, &["add", "file"]);
        git(&other, &["commit", "-m", "other lane work"]);
        git(&fx.repo, &["merge", "--ff-only", "cadence/other"]);
        git(
            &fx.repo,
            &["update-ref", "refs/remotes/origin/main", "HEAD"],
        );
        let past = std::time::SystemTime::now() - Duration::from_secs(2 * 3600);
        fs::OpenOptions::new()
            .write(true)
            .open(other.join("file"))
            .unwrap()
            .set_modified(past)
            .unwrap();
        let first_tip = git(&fx.repo, &["rev-parse", "cadence/test"]);
        let second_tip = git(&fx.repo, &["rev-parse", "cadence/other"]);
        let coord = fx.cancel();
        let mut ledger = Ledger::load(&fx.auth, "ws_test").unwrap();
        let first = ledger.attempts[&attempt_key("CAD-1", "start", None)].clone();
        let mut second = first.clone();
        second.assignment_id = Some("hla_other".into());
        second.worktree = Some(other.to_str().unwrap().into());
        second.lane = Some("other".into());
        second.lane_claim = Some("hlc_other".into());
        ledger
            .attempts
            .insert("discovered:hla_other".into(), second);
        let mut active = first;
        active.assignment_id = Some("hla_active".into());
        active.state = "running".into();
        active.generation = Some(1);
        active.claim_id = Some("hlc_active".into());
        active.lane_claim = Some("hlc_active".into());
        active.lane_generation = Some(1);
        active.fenced = false;
        active.retired_lane_guard = false;
        ledger
            .attempts
            .insert("discovered:hla_active".into(), active);
        ledger.save(&fx.auth, "ws_test").unwrap();

        assert!(
            release(&coord, "CAD-1", Some(&fx.lane), &fx.state).is_err(),
            "an active assignment on the selected lane must block release"
        );
        fx.assert_intact(&first_tip);
        assert!(other.is_dir());
        assert_eq!(git(&fx.repo, &["rev-parse", "cadence/other"]), second_tip);

        let mut ledger = Ledger::load(&fx.auth, "ws_test").unwrap();
        ledger.attempts.remove("discovered:hla_active");
        ledger.save(&fx.auth, "ws_test").unwrap();
        let first_result = release(&coord, "CAD-1", Some(&fx.lane), &fx.state)
            .expect("the selected first lane should release independently");
        assert_eq!(first_result["removed_worktree"], true);
        assert_eq!(first_result["deleted_branch"], true);
        fx.assert_gone();
        assert!(other.is_dir());
        assert_eq!(git(&fx.repo, &["rev-parse", "cadence/other"]), second_tip);

        let second_result = release_cancelled(&coord, "hla_other", &fx.state)
            .expect("the exact second assignment should remain releasable");
        assert_eq!(second_result["removed_worktree"], true);
        assert_eq!(second_result["deleted_branch"], true);
        assert!(!other.exists());
        assert!(git(&fx.repo, &["branch", "--list", "cadence/other"]).is_empty());
    }

    #[test]
    fn protected_ignored_file_created_before_final_lock_refuses_without_transition() {
        use crate::worktree::lifecycle::{self, CheckoutSpec};

        let fx = CancelReleaseFixture::new(true);
        let tip = git(&fx.repo, &["rev-parse", "cadence/test"]);
        let exclude = fx.repo.join(".git/info/exclude");
        fs::write(&exclude, ".env\n").unwrap();
        let record = lifecycle::new_record(CheckoutSpec {
            repo: &fx.repo,
            purpose: "development",
            tool: "cadence agent worktree",
            owner: "synthetic",
            path: &fx.lane,
            branch: Some("cadence/test"),
            pinned_sha: &tip,
            issue: Some("CAD-1"),
        });
        lifecycle::activate(&fx.repo, record).unwrap();
        let before = lifecycle::managed_record(&fx.repo, &fx.lane)
            .unwrap()
            .unwrap();
        assert_eq!(before.state, "active");
        let coord = fx.cancel();
        let protected = fx.lane.join(".env");
        let created = protected.clone();
        crate::issue::finish::BEFORE_FINAL_LOCK.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                fs::write(created, "operator secret\n").unwrap();
            }));
        });
        let error = release_cancelled(&coord, "hla_test", &fx.state)
            .expect_err("late protected ignored data must block deletion")
            .to_string();
        assert!(error.contains("ignored"), "{error}");
        assert_eq!(fs::read_to_string(&protected).unwrap(), "operator secret\n");
        fx.assert_intact(&tip);
        let after = lifecycle::managed_record(&fx.repo, &fx.lane)
            .unwrap()
            .unwrap();
        assert_eq!(after.state, "active");
        assert_eq!(
            serde_json::to_value(&after).unwrap(),
            serde_json::to_value(&before).unwrap(),
            "late refusal must not rebind or transition ownership"
        );
    }

    #[test]
    fn queue_response_coalesces_watcher_custody_without_losing_create_key() {
        struct WatchBeforeCreateReply {
            auth: PathBuf,
            lane: PathBuf,
            create_key: Rc<std::cell::RefCell<Option<String>>>,
        }

        impl CoordHttp for WatchBeforeCreateReply {
            fn send(&mut self, method: &str, path: &str, body: Option<&Value>) -> Result<Reply> {
                assert_eq!(method, "POST");
                assert!(path.ends_with("/local-assignments"), "{path}");
                let body = body.expect("create request body");
                *self.create_key.borrow_mut() =
                    Some(body["idempotency_key"].as_str().unwrap().into());
                let observer = Coordinator::new(
                    FakeRemote(Rc::new(Cell::new(0))),
                    "ws_test".into(),
                    self.auth.clone(),
                )?;
                let claimed = parse_assignment(&json!({
                    "version": VERSION, "assignment_id": "hla_test",
                    "organization_id": "ws_test", "issue_id": "CAD-1",
                    "action": "start", "execution": "local", "state": "running",
                    "generation": 1, "claim_id": "hlc_test", "target": "usr_test",
                    "created_by_subject_id": "usr_test", "client_agent_id": "worker",
                    "guidance": null, "reported_head_sha": null, "text": null
                }))?;
                observer.remember_discovered(&claimed)?;
                observer.with_ledger(|ledger| {
                    let attempt = ledger.attempts.get_mut("discovered:hla_test").unwrap();
                    attempt.worktree = Some(self.lane.to_str().unwrap().into());
                    attempt.lane = Some("test".into());
                    attempt.lane_claim = Some("hlc_test".into());
                    attempt.lane_generation = Some(1);
                    Ok(())
                })?;
                Ok(Reply {
                    status: 200,
                    body: json!({
                        "version": VERSION, "assignment_id": "hla_test",
                        "organization_id": "ws_test", "issue_id": "CAD-1",
                        "action": "start", "execution": "local", "state": "waiting",
                        "generation": 1, "claim_id": null, "target": null,
                        "created_by_subject_id": "usr_test", "client_agent_id": null,
                        "guidance": null, "reported_head_sha": null, "text": null
                    }),
                })
            }
        }

        let fx = CancelReleaseFixture::new(true);
        let tip = git(&fx.repo, &["rev-parse", "cadence/test"]);
        let mut ledger = Ledger::load(&fx.auth, "ws_test").unwrap();
        ledger.attempts.clear();
        ledger.save(&fx.auth, "ws_test").unwrap();
        let create_key = Rc::new(std::cell::RefCell::new(None));
        let mut coord = Coordinator::new(
            WatchBeforeCreateReply {
                auth: fx.auth.clone(),
                lane: fx.lane.clone(),
                create_key: create_key.clone(),
            },
            "ws_test".into(),
            fx.auth.clone(),
        )
        .unwrap();
        coord.queue("CAD-1", "start", None).unwrap();
        let ledger = Ledger::load(&fx.auth, "ws_test").unwrap();
        let rows: Vec<_> = ledger
            .attempts
            .values()
            .filter(|attempt| attempt.assignment_id.as_deref() == Some("hla_test"))
            .collect();
        assert_eq!(
            rows.len(),
            1,
            "watcher and create response must not duplicate custody"
        );
        let attempt = rows[0];
        assert_eq!(
            Some(attempt.operation_id.as_str()),
            create_key.borrow().as_deref()
        );
        assert_eq!(attempt.state, "running");
        assert_eq!(attempt.generation, Some(1));
        assert_eq!(attempt.claim_id.as_deref(), Some("hlc_test"));
        assert_eq!(attempt.worktree.as_deref(), fx.lane.to_str());
        assert_eq!(attempt.lane.as_deref(), Some("test"));
        assert_eq!(attempt.lane_claim.as_deref(), Some("hlc_test"));
        assert_eq!(attempt.lane_generation, Some(1));
        fx.assert_intact(&tip);
    }

    #[test]
    fn cross_action_coalescing_preserves_each_create_key_across_lost_reply_and_retries() {
        struct CoalescingBoard {
            original_action: &'static str,
            requests: Rc<std::cell::RefCell<Vec<Value>>>,
            lost_other: bool,
        }

        impl CoordHttp for CoalescingBoard {
            fn send(&mut self, method: &str, path: &str, body: Option<&Value>) -> Result<Reply> {
                assert_eq!(method, "POST");
                assert!(path.ends_with("/local-assignments"), "{path}");
                let request = body.expect("create request body").clone();
                let action = request["action"].as_str().unwrap().to_owned();
                self.requests.borrow_mut().push(request);
                if action != self.original_action && !self.lost_other {
                    self.lost_other = true;
                    return Err(Error::unknown("synthetic lost second-create response"));
                }
                Ok(Reply {
                    status: 200,
                    body: json!({
                        "version": VERSION, "assignment_id": "hla_test",
                        "organization_id": "ws_test", "issue_id": "CAD-1",
                        "action": self.original_action, "execution": "local",
                        "state": "waiting", "generation": 1, "claim_id": null,
                        "target": null, "created_by_subject_id": "usr_test",
                        "client_agent_id": null, "guidance": null,
                        "reported_head_sha": null, "text": null
                    }),
                })
            }
        }

        for (first, second) in [("start", "dispatch"), ("dispatch", "start")] {
            let fx = CancelReleaseFixture::new(true);
            let tip = git(&fx.repo, &["rev-parse", "cadence/test"]);
            let mut ledger = Ledger::load(&fx.auth, "ws_test").unwrap();
            ledger.attempts.clear();
            ledger.save(&fx.auth, "ws_test").unwrap();
            let requests = Rc::new(std::cell::RefCell::new(Vec::new()));
            let mut coord = Coordinator::new(
                CoalescingBoard {
                    original_action: first,
                    requests: requests.clone(),
                    lost_other: false,
                },
                "ws_test".into(),
                fx.auth.clone(),
            )
            .unwrap();
            assert_eq!(
                coord.queue("CAD-1", first, None).unwrap().assignment_id,
                "hla_test"
            );
            let lost = coord.queue("CAD-1", second, None).err().unwrap();
            assert_eq!(lost.kind(), "unknown");
            assert_eq!(
                coord.queue("CAD-1", second, None).unwrap().assignment_id,
                "hla_test"
            );
            assert_eq!(
                coord.queue("CAD-1", first, None).unwrap().assignment_id,
                "hla_test"
            );
            assert_eq!(
                coord.queue("CAD-1", second, None).unwrap().assignment_id,
                "hla_test"
            );

            let requests = requests.borrow();
            assert_eq!(requests.len(), 5);
            let keys: Vec<_> = requests
                .iter()
                .map(|body| body["idempotency_key"].as_str().unwrap())
                .collect();
            assert_ne!(
                keys[0], keys[1],
                "different create requests need different keys"
            );
            assert_eq!(
                keys[1], keys[2],
                "lost second-create response must replay its key"
            );
            assert_eq!(keys[0], keys[3], "first request must retain its own key");
            assert_eq!(keys[1], keys[4], "second request must retain its own key");
            assert_eq!(requests[0]["action"], first);
            assert_eq!(requests[1]["action"], second);
            let ledger = Ledger::load(&fx.auth, "ws_test").unwrap();
            assert_eq!(
                ledger
                    .attempts
                    .values()
                    .filter(|attempt| attempt.assignment_id.as_deref() == Some("hla_test"))
                    .count(),
                1
            );
            fx.assert_intact(&tip);
        }
    }

    #[test]
    fn guarded_cancellation_blocks_cross_action_queue_before_the_board() {
        struct LoseCancellation;

        impl CoordHttp for LoseCancellation {
            fn send(&mut self, method: &str, path: &str, body: Option<&Value>) -> Result<Reply> {
                assert_eq!(method, "POST");
                assert!(path.ends_with("/hla_test/reassign"), "{path}");
                assert_eq!(body.unwrap()["cancel"], true);
                Err(Error::unknown("synthetic lost cancellation response"))
            }
        }

        for pending in [true, false] {
            let fx = CancelReleaseFixture::new(true);
            let tip = git(&fx.repo, &["rev-parse", "cadence/test"]);
            if pending {
                let mut cancel =
                    Coordinator::new(LoseCancellation, "ws_test".into(), fx.auth.clone()).unwrap();
                assert_eq!(
                    cancel.reassign("hla_test", None, true).unwrap_err().kind(),
                    "unknown"
                );
            } else {
                fx.cancel();
            }
            let ledger_path = fx.auth.join("local-assign-ws_test.json");
            let before = fs::read(&ledger_path).unwrap();
            let calls = Rc::new(Cell::new(0));
            let mut coord =
                Coordinator::new(FakeRemote(calls.clone()), "ws_test".into(), fx.auth.clone())
                    .unwrap();
            assert!(
                coord.queue("CAD-1", "dispatch", None).is_err(),
                "cross-action create must not bypass a guarded cancellation"
            );
            assert_eq!(calls.get(), 0, "guarded create must not contact the board");
            assert_eq!(fs::read(&ledger_path).unwrap(), before);
            fx.assert_intact(&tip);
        }
    }

    #[test]
    fn terminal_coalesced_alias_cannot_discard_an_existing_lane() {
        for state in ["result", "failed"] {
            let fx = CancelReleaseFixture::new(true);
            let tip = git(&fx.repo, &["rev-parse", "cadence/test"]);
            let mut ledger = Ledger::load(&fx.auth, "ws_test").unwrap();
            let attempt = ledger
                .attempts
                .get_mut(&attempt_key("CAD-1", "start", None))
                .unwrap();
            attempt.state = state.to_string();
            ledger.create_aliases.insert(
                attempt_key("CAD-1", "dispatch", None),
                CreateAlias {
                    operation_id: "op_dispatch_original".into(),
                    assignment_id: "hla_test".into(),
                },
            );
            ledger.save(&fx.auth, "ws_test").unwrap();
            let ledger_path = fx.auth.join("local-assign-ws_test.json");
            let before = fs::read(&ledger_path).unwrap();
            let calls = Rc::new(Cell::new(0));
            let mut coord =
                Coordinator::new(FakeRemote(calls.clone()), "ws_test".into(), fx.auth.clone())
                    .unwrap();
            assert!(
                coord.queue("CAD-1", "dispatch", None).is_err(),
                "{state}: alias create must not discard the terminal lane binding"
            );
            assert_eq!(calls.get(), 0, "{state}: no create may reach the board");
            assert_eq!(fs::read(&ledger_path).unwrap(), before);
            fx.assert_intact(&tip);
        }
    }

    #[test]
    fn equivalent_legacy_canonical_rows_preserve_both_create_receipts_and_release() {
        let fx = CancelReleaseFixture::new(true);
        let mut ledger = Ledger::load(&fx.auth, "ws_test").unwrap();
        let original = ledger.attempts[&attempt_key("CAD-1", "start", None)].clone();
        let start_operation = original.operation_id.clone();
        let mut duplicate = original;
        duplicate.action = "dispatch".into();
        duplicate.operation_id = "op_legacy_dispatch".into();
        ledger
            .attempts
            .insert(attempt_key("CAD-1", "dispatch", None), duplicate);
        ledger.save(&fx.auth, "ws_test").unwrap();

        let coord = fx.cancel();
        let ledger = Ledger::load(&fx.auth, "ws_test").unwrap();
        let rows: Vec<_> = ledger
            .attempts
            .values()
            .filter(|attempt| attempt.assignment_id.as_deref() == Some("hla_test"))
            .collect();
        assert_eq!(
            rows.len(),
            1,
            "equivalent rows must share one custody record"
        );
        let aliases: Vec<_> = ledger
            .create_aliases
            .values()
            .filter(|alias| alias.assignment_id == "hla_test")
            .collect();
        assert_eq!(
            aliases.len(),
            1,
            "the other request must remain an alias receipt"
        );
        let saved: std::collections::BTreeSet<_> = [
            rows[0].operation_id.as_str(),
            aliases[0].operation_id.as_str(),
        ]
        .into_iter()
        .collect();
        let expected: std::collections::BTreeSet<_> =
            [start_operation.as_str(), "op_legacy_dispatch"]
                .into_iter()
                .collect();
        assert_eq!(
            saved, expected,
            "both original create operation IDs must survive"
        );
        assert_eq!(rows[0].worktree.as_deref(), fx.lane.to_str());
        assert_eq!(rows[0].lane_claim.as_deref(), Some("hlc_test"));
        assert_eq!(rows[0].lane_generation, Some(1));

        let result = release_cancelled(&coord, "hla_test", &fx.state)
            .expect("equivalent legacy custody must remain releasable");
        assert_eq!(result["removed_worktree"], true);
        assert_eq!(result["deleted_branch"], true);
        fx.assert_gone();
    }

    #[test]
    fn duplicate_assignment_custody_must_agree_before_cancellation() {
        for case in ["equivalent", "claim", "generation", "lane", "pending"] {
            let fx = CancelReleaseFixture::new(true);
            let tip = git(&fx.repo, &["rev-parse", "cadence/test"]);
            let mut ledger = Ledger::load(&fx.auth, "ws_test").unwrap();
            let mut duplicate = ledger.attempts[&attempt_key("CAD-1", "start", None)].clone();
            duplicate.operation_id = "op_discovered".into();
            match case {
                "claim" => duplicate.claim_id = Some("hlc_other".into()),
                "generation" => duplicate.generation = Some(2),
                "lane" => duplicate.lane = Some("other".into()),
                "pending" => {
                    duplicate.reassign_operation_id = Some("op_other".into());
                    duplicate.reassign_fingerprint = Some("cancel".into());
                    duplicate.reassign_expected_generation = Some(1);
                }
                _ => {}
            }
            ledger
                .attempts
                .insert("discovered:hla_test".into(), duplicate);
            ledger.save(&fx.auth, "ws_test").unwrap();
            if case == "equivalent" {
                let coord = fx.cancel();
                let ledger = Ledger::load(&fx.auth, "ws_test").unwrap();
                assert_eq!(
                    ledger
                        .attempts
                        .values()
                        .filter(|attempt| attempt.assignment_id.as_deref() == Some("hla_test"))
                        .count(),
                    1
                );
                let result = release_cancelled(&coord, "hla_test", &fx.state)
                    .expect("equivalent duplicates must retain a releasable binding");
                assert_eq!(result["removed_worktree"], true);
                assert_eq!(result["deleted_branch"], true);
                fx.assert_gone();
            } else {
                let calls = Rc::new(Cell::new(0));
                let mut coord =
                    Coordinator::new(FakeRemote(calls.clone()), "ws_test".into(), fx.auth.clone())
                        .unwrap();
                assert!(coord.reassign("hla_test", None, true).is_err(), "{case}");
                assert_eq!(
                    calls.get(),
                    0,
                    "{case}: conflicting custody reached the board"
                );
                fx.assert_intact(&tip);
            }
        }
    }

    #[test]
    fn ambiguous_duplicate_create_keys_refuse_cancellation_without_mutation() {
        let fx = CancelReleaseFixture::new(true);
        let tip = git(&fx.repo, &["rev-parse", "cadence/test"]);
        let mut ledger = Ledger::load(&fx.auth, "ws_test").unwrap();
        let mut duplicate = ledger.attempts[&attempt_key("CAD-1", "start", None)].clone();
        duplicate.operation_id = "op_other_create".into();
        ledger
            .attempts
            .insert("another-canonical-key".into(), duplicate);
        ledger.save(&fx.auth, "ws_test").unwrap();
        let ledger_path = fx.auth.join("local-assign-ws_test.json");
        let before = fs::read(&ledger_path).unwrap();
        let calls = Rc::new(Cell::new(0));
        let mut coord =
            Coordinator::new(FakeRemote(calls.clone()), "ws_test".into(), fx.auth.clone()).unwrap();
        assert!(
            coord.reassign("hla_test", None, true).is_err(),
            "ambiguous create receipts must refuse cancellation"
        );
        assert_eq!(calls.get(), 0, "ambiguous receipt must not reach the board");
        assert_eq!(fs::read(&ledger_path).unwrap(), before);
        fx.assert_intact(&tip);
    }
}
