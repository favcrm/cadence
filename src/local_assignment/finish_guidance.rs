use super::*;

/// A hosted finish is always a refusal. Local observations are guidance only;
/// they never authorize cleanup or a local-tracker operation.
pub fn refusal<H: CoordHttp>(
    coordinator: &Coordinator<H>,
    issue: Option<&str>,
    selected: Option<&Path>,
) -> Error {
    // CAD-2195: repo-less work has no lane to release, so a lane observation
    // would be an invented answer. Say what is true and leave the close to the
    // ticket's own acceptance rules.
    if issue.is_some_and(|id| repo_less(coordinator, id)) {
        return Error::rejected(
            "Hosted issue finish has no lane to release: this issue's local assignment is \
             repo-less work with no checkout. Report its result and close the ticket under \
             the project's own acceptance rules",
        );
    }
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
        "Hosted issue finish does not release lanes; {detail}; leave it for the reconcile sweep"
    ))
}

/// Every local attempt for the issue is repo-less work that opened no lane.
/// A single dev attempt, or any lane, makes this false.
fn repo_less<H: CoordHttp>(coordinator: &Coordinator<H>, issue: &str) -> bool {
    let Ok(lock) = remote_cli::private_lock(
        &coordinator.ledger,
        "local-assign",
        Instant::now() + Duration::from_secs(5),
    ) else {
        return false;
    };
    let ledger = match Ledger::load(&coordinator.ledger, &coordinator.org_id) {
        Ok(ledger) => ledger,
        Err(_) => return false,
    };
    drop(lock);
    let mut found = false;
    for attempt in ledger.attempts.values().filter(|a| a.issue_id == issue) {
        found = true;
        if attempt.lane.is_some()
            || attempt.worktree.is_some()
            || attempt
                .work_kind
                .as_deref()
                .and_then(WorkKind::parse)
                .is_none_or(|kind| !kind.is_non_code())
        {
            return false;
        }
    }
    found
}

enum LaneState {
    Dirty,
    CleanMerged,
    CleanUnverified,
}

fn observe<H: CoordHttp>(
    coordinator: &Coordinator<H>,
    issue: &str,
    selected: Option<&Path>,
) -> Result<Option<LaneState>> {
    let lock = remote_cli::private_lock(
        &coordinator.ledger,
        "local-assign",
        Instant::now() + Duration::from_secs(5),
    )?;
    let ledger = Ledger::load(&coordinator.ledger, &coordinator.org_id)?;
    drop(lock);
    let mut lane = None;
    for attempt in ledger.attempts.values().filter(|a| a.issue_id == issue) {
        if attempt.worktree.is_none() && attempt.claim_id.is_none() {
            continue;
        }
        if attempt.fenced
            || attempt.claim_id.is_none()
            || attempt.lane_claim != attempt.claim_id
            || attempt.generation.is_none()
            || attempt.lane_generation != attempt.generation
        {
            return Ok(None);
        }
        let Some(candidate) = attempt.worktree.as_ref().zip(attempt.lane.as_ref()) else {
            return Ok(None);
        };
        if lane.is_some_and(|previous| previous != candidate) {
            return Ok(None);
        }
        lane = Some(candidate);
    }
    let Some((path, lane)) = lane else {
        return Ok(None);
    };
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
    // `git cherry` ignores merge commits, which can contain unique resolution
    // work even when every non-merge commit is patch-equivalent upstream.
    if evidence == Some("cherry")
        && !git(&root, &["rev-list", "--merges", &format!("{into}..{tip}")])?.is_empty()
    {
        return Ok(Some(LaneState::CleanUnverified));
    }
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
            clean.contains("reconcile sweep")
                && clean.contains("clean")
                && clean.contains("merged"),
            "{clean}"
        );
        fs::write(lane.join("file"), "new work\n").unwrap();
        let dirty = refusal(&coord, Some("CAD-1"), None).to_string();
        assert!(
            dirty.contains("reconcile sweep") && dirty.contains("uncommitted"),
            "{dirty}"
        );
        git(&lane, &["add", "file"]);
        git(&lane, &["commit", "-m", "work"]);
        let unmerged = refusal(&coord, Some("CAD-1"), None).to_string();
        assert!(
            unmerged.contains("reconcile sweep") && unmerged.contains("cannot be confirmed"),
            "{unmerged}"
        );
        git(&repo, &["merge", "--ff-only", "cadence/test"]);
        git(&repo, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
        let merged = refusal(&coord, Some("CAD-1"), None).to_string();
        assert!(
            merged.contains("reconcile sweep")
                && merged.contains("clean")
                && merged.contains("merged"),
            "{merged}"
        );
        let wrong_lane = refusal(&coord, Some("CAD-1"), Some(&repo)).to_string();
        assert!(wrong_lane.contains("cannot be confirmed"), "{wrong_lane}");
        let missing = refusal(&coord, Some("CAD-2"), None).to_string();
        assert!(
            missing.contains("reconcile sweep") && missing.contains("cannot"),
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
}
