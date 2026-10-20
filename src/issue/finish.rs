//! `cadence issue finish <ID>` — remove an issue's recorded worktree
//! and branch once the work has landed. The guard is per worktree,
//! not per agent: it refuses only while the worktree is actually in
//! use — a live message whose dispatch recorded this worktree (the
//! issue's `message` refs, or a job task's recorded worktree), an
//! unreconciled `unknown` message bound the same way or held by a
//! registered agent whose cwd is on the worktree, a pane
//! process tree with cwd inside it, any process standing in it, or
//! any non-ignored file modified within `ACTIVE_WINDOW` (CAD-275) —
//! plus a dirty worktree (ignored paths don't count), a branch that
//! has not started (no commits beyond where it was cut — never
//! "merged", CAD-275) and a branch whose work survives nowhere —
//! merged into the repo's default branch by ancestry,
//! patch-equivalent commits, a squash merge, or a merged PR, or
//! pushed. A busy owner working ELSEWHERE is not a reason. `--force`
//! overrides each and is recorded as a `Forced:` trailer on the
//! finish commit. Refs are kept as history, marked `closed: true`
//! (a branch finish leaves standing keeps its ref open, CAD-145);
//! the issue's status is untouched — status follows the job or the
//! PM. An issue with several open worktree refs needs `--worktree
//! <path>` to name one (CAD-274); a named directory already gone only
//! closes its refs. `issue finish --merged` sweeps every open
//! worktree ref whose branch is merged and whose guard passes; an
//! unstarted branch is `skipped(not started)`. A recorded directory
//! that is already missing, or a branch checked out at a different
//! live path, is skipped for reconciliation instead of finished — the
//! sweep does not close those refs or delete those branches.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

use serde_json::{json, Value};
use tempfile::TempDir;

use crate::adapter::pty;
use crate::client;
use crate::error::{Error, Result};
use crate::issue::model::{self, Front, Ref};
use crate::issue::process_use::{process_use_under, ProcessUseProbe};
use crate::issue::{board, project, start, write, Pm};
use crate::proc::{run_bounded, BoundedError};
use crate::worktree::layout;

/// Every git/gh probe in this file runs through the bounded runner —
/// user repos can be slow or locked and finish must not stall.
mod activity;
mod evidence;
mod git;
mod guard;
mod resolve;
mod run;
mod sweep;

use activity::*;
use evidence::*;
use git::*;
use guard::*;
use resolve::*;

pub(crate) use activity::{comm_of, daemon_view, lane_in_use, with_probe_timeout, DaemonView};
pub(crate) use evidence::{branch_tip, not_started};
pub(crate) use git::{default_ref, git, git_branch, merge_rule};
pub(crate) use guard::cwd_holder_aliases;
pub(crate) use run::{run, FinishArgs};
pub(crate) use sweep::{lexical_path, registered_branch_path, same_path, sweep, sweep_selected};

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn repo() -> TempDir {
        let tmp = TempDir::new().unwrap();
        let r = tmp.path();
        git(r, &["init", "-b", "main"]).unwrap();
        git(r, &["config", "user.email", "t@t"]).unwrap();
        git(r, &["config", "user.name", "t"]).unwrap();
        std::fs::write(r.join("f"), "one").unwrap();
        git(r, &["add", "f"]).unwrap();
        git(r, &["commit", "-m", "one"]).unwrap();
        tmp
    }

    fn target(id: &str) -> Target {
        Target {
            front: Front::new(id, "t", "now"),
            dir: PathBuf::new(),
            wt_dir: None,
            wt_name: None,
            branch: "side".to_string(),
            root: PathBuf::new(),
            msg_refs: HashSet::new(),
            cargo_target: None,
        }
    }

    /// CAD-275: "not started" is measured from where the branch was
    /// cut, not from the default branch — a lane fast-forwarded into
    /// main also has no commits beyond main, yet it did start.
    #[test]
    fn not_started_measures_from_the_fork_point() {
        let tmp = repo();
        let r = tmp.path();
        git(r, &["branch", "lane"]).unwrap();
        let tip = git(r, &["rev-parse", "lane"]).unwrap();
        assert!(not_started(r, "lane", &tip), "cut and untouched");
        git(r, &["checkout", "-q", "lane"]).unwrap();
        std::fs::write(r.join("f"), "two").unwrap();
        git(r, &["commit", "-qam", "two"]).unwrap();
        git(r, &["checkout", "-q", "main"]).unwrap();
        git(r, &["merge", "-q", "--ff-only", "lane"]).unwrap();
        let tip = git(r, &["rev-parse", "lane"]).unwrap();
        assert!(ancestor(r, &tip, "main"));
        assert!(!not_started(r, "lane", &tip), "fast-forwarded work started");
        // No reflog (expired or disabled): no fork point, and the
        // merge rules decide — never a guess of "not started".
        git(r, &["-c", "core.logAllRefUpdates=false", "branch", "bare"]).unwrap();
        assert!(fork_point(r, "bare").is_none());
        assert!(!not_started(r, "bare", &tip));
    }

    #[test]
    fn fmt_age_reads_like_a_person() {
        assert_eq!(fmt_age(Duration::from_secs(0)), "0s");
        assert_eq!(fmt_age(Duration::from_secs(45)), "45s");
        assert_eq!(fmt_age(Duration::from_secs(12 * 60 + 3)), "12m03s");
        assert_eq!(fmt_age(ACTIVE_WINDOW), "30m");
    }

    #[test]
    fn delete_branch_at_is_compare_and_delete() {
        let tmp = repo();
        let r = tmp.path();
        git(r, &["branch", "side"]).unwrap();
        let tip = git(r, &["rev-parse", "side"]).unwrap();
        // A second commit the evidence pretended to cover — deleting
        // against IT must refuse and keep the real tip's commits.
        std::fs::write(r.join("f"), "two").unwrap();
        git(r, &["commit", "-am", "two"]).unwrap();
        let other = git(r, &["rev-parse", "main"]).unwrap();
        assert_ne!(tip, other);
        assert!(!delete_branch_at(r, "side", &other));
        assert_eq!(git(r, &["rev-parse", "side"]).unwrap(), tip);
        // The exact covered tip deletes; the ref is then gone.
        assert!(delete_branch_at(r, "side", &tip));
        assert!(git(r, &["rev-parse", "--verify", "--quiet", "side"]).is_err());
        // Deleting an absent ref is a no-op, not a crash.
        assert!(!delete_branch_at(r, "side", &tip));
    }

    /// A repo with a bare `origin` remote — the fixture for the
    /// leased-delete unit test.
    fn repo_with_origin() -> (TempDir, TempDir) {
        let tmp = repo();
        let r = tmp.path();
        git(r, &["branch", "side"]).unwrap();
        let bare = TempDir::new().unwrap();
        git(bare.path(), &["init", "--bare", "-b", "main"]).unwrap();
        git(
            r,
            &["remote", "add", "origin", bare.path().to_str().unwrap()],
        )
        .unwrap();
        git(r, &["push", "-q", "origin", "main", "side"]).unwrap();
        git(r, &["fetch", "-q", "origin"]).unwrap();
        (tmp, bare)
    }

    #[test]
    fn remote_delete_is_leased_on_the_expected_tip() {
        let (tmp, bare) = repo_with_origin();
        let r = tmp.path();
        let tip = git(r, &["rev-parse", "side"]).unwrap();
        // The expected tip deletes the remote ref.
        assert!(remote_delete(r, "side", &tip).is_ok());
        assert!(
            git(bare.path(), &["rev-parse", "--verify", "--quiet", "side"]).is_err(),
            "remote branch must be gone"
        );
        // Re-push, then advance the remote PAST the proven tip (the
        // mid-finish move — another agent pushing ahead): the leased
        // push must refuse and the new commits must survive.
        git(r, &["push", "-q", "origin", "side"]).unwrap();
        std::fs::write(r.join("f"), "more").unwrap();
        git(r, &["commit", "-qam", "remote ahead"]).unwrap();
        let ahead = git(r, &["rev-parse", "main"]).unwrap();
        git(r, &["push", "-q", "origin", "main:side"]).unwrap();
        assert_eq!(
            git(bare.path(), &["rev-parse", "side"]).unwrap(),
            ahead,
            "fixture: the remote must hold the advanced tip"
        );
        // The stale tracking view the probe had proven covered.
        git(r, &["update-ref", "refs/remotes/origin/side", &tip]).unwrap();
        assert!(
            remote_delete(r, "side", &tip).is_err(),
            "a remote that moved past the proven tip must refuse"
        );
        assert_eq!(
            git(bare.path(), &["rev-parse", "side"]).unwrap(),
            ahead,
            "the unseen commits must still be on the remote"
        );
    }

    /// CAD-144: a single-branch clone's configured refspec does not map
    /// the lane, so a bare `fetch origin <b>` leaves the tracking ref
    /// stale. The explicit refspec must refresh it — the coverage gate
    /// and the lease read the server's current tip.
    #[test]
    fn remote_fetch_refreshes_tracking_on_a_single_branch_clone() {
        let (tmp, _bare) = repo_with_origin();
        let r = tmp.path();
        let stale = git(r, &["rev-parse", "side"]).unwrap();
        std::fs::write(r.join("f"), "ahead").unwrap();
        git(r, &["commit", "-qam", "ahead"]).unwrap();
        let ahead = git(r, &["rev-parse", "main"]).unwrap();
        git(r, &["push", "-q", "origin", "main:side"]).unwrap();
        git(
            r,
            &[
                "config",
                "remote.origin.fetch",
                "+refs/heads/main:refs/remotes/origin/main",
            ],
        )
        .unwrap();
        git(r, &["update-ref", "refs/remotes/origin/side", &stale]).unwrap();
        let mut t = target("CAD-1");
        t.root = r.to_path_buf();
        let ev = evidence(&t, true);
        assert!(ev.remote_cmp, "{:?}", ev.remote_note);
        assert_eq!(ev.remote_tip.as_deref(), Some(ahead.as_str()));
    }

    /// CAD-144: a branch value shaped like an option never reaches git
    /// as one — even past `resolve`'s refusal, the fetch passes it only
    /// inside a refspec after `--`.
    #[test]
    fn remote_fetch_never_passes_the_branch_as_an_option() {
        let (tmp, _bare) = repo_with_origin();
        let r = tmp.path();
        let marker = r.join("upload-pack-ran");
        let mut t = target("CAD-1");
        t.root = r.to_path_buf();
        t.branch = format!("--upload-pack=touch {}", marker.display());
        let ev = evidence(&t, true);
        assert!(!marker.exists(), "--upload-pack ran: {:?}", ev.remote_note);
        assert!(!ev.remote_cmp && ev.remote_tip.is_none());
    }

    #[test]
    fn stale_evidence_flags_only_mid_probe_moves() {
        let tmp = repo();
        let r = tmp.path();
        git(r, &["branch", "side"]).unwrap();
        let mut t = target("CAD-1");
        t.root = r.to_path_buf();
        // No remote configured — tracking tip is absent, compared.
        let ev = evidence(&t, false);
        assert!(stale_evidence_reason(r, "side", &ev).is_none());
        // Branch tip moved mid-probe — stale.
        std::fs::write(r.join("f"), "two").unwrap();
        git(r, &["commit", "-qam", "two"]).unwrap();
        git(r, &["branch", "-f", "side", "HEAD"]).unwrap();
        let moved = stale_evidence_reason(r, "side", &ev).unwrap();
        assert!(moved.contains("tip moved"), "{moved}");
        // A tracking-ref move is stale only when remote_cmp says the
        // probe proved it — a fetch that could not prove the remote
        // skips the compare.
        git(r, &["update-ref", "refs/remotes/origin/side", "HEAD"]).unwrap();
        assert!(stale_evidence_reason(r, "side", &ev).is_some());
        // Re-probe fresh (everything matches now), then clear
        // remote_cmp — the same tracking state must not refuse.
        let mut ev_no_cmp = evidence(&t, false);
        assert!(stale_evidence_reason(r, "side", &ev_no_cmp).is_none());
        ev_no_cmp.remote_cmp = false;
        ev_no_cmp.remote_tip = None;
        assert!(
            stale_evidence_reason(r, "side", &ev_no_cmp).is_none(),
            "an unproven remote must not produce a stale-tracking refusal"
        );
    }

    #[test]
    fn stale_probe_flags_only_mid_probe_changes() {
        let probe = target("CAD-1");
        // Identical re-resolve — fresh.
        assert!(stale_probe_reason(&probe, &target("CAD-1")).is_none());
        // Retargeted worktree — stale.
        let mut t = target("CAD-1");
        t.wt_dir = Some(PathBuf::from("/elsewhere"));
        assert!(stale_probe_reason(&probe, &t)
            .unwrap()
            .contains("changed during finish"));
        // Retargeted branch — stale.
        let mut t = target("CAD-1");
        t.branch = "other".to_string();
        assert!(stale_probe_reason(&probe, &t).is_some());
        // A message ref recorded mid-probe — stale.
        let mut t = target("CAD-1");
        t.msg_refs.insert("new-msg".to_string());
        assert!(stale_probe_reason(&probe, &t)
            .unwrap()
            .contains("dispatch was recorded"));
        // A ref that vanished mid-probe is NOT stale — the probe saw
        // strictly more than exists now, so it erred conservative.
        let mut probe2 = target("CAD-1");
        probe2.msg_refs.insert("gone".to_string());
        assert!(stale_probe_reason(&probe2, &target("CAD-1")).is_none());
    }
}
