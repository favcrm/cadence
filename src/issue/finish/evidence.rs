//! Branch evidence and survivability for `issue finish`.

use super::*;

/// Where the target's branch stands for survivability — one
/// `rev-parse` + `merge_rule` pair feeds both the finish guard and
/// the `--merged` sweep's candidate filter. The variants carry the
/// tip they were computed against so a `branch -D` can be bound to
/// exactly the commit the evidence covered.
pub(crate) enum Branch {
    /// No branch ref recorded, or the recorded ref no longer exists.
    Gone,
    /// CAD-275: zero commits beyond the commit the branch was cut
    /// from — the lane has not started, so nothing on it is merged
    /// (its tip sitting on the default branch proves nothing). Never
    /// a sweep candidate; an explicit finish needs `--force`. `tip`
    /// is deletable: the branch holds no work.
    NotStarted { tip: String },
    /// The ref exists but `merge_rule` does not land — the tip's
    /// commits are covered by no evidence and the branch is never
    /// deleted.
    Open { tip: String },
    /// Merged into the repo's default branch — `merged_by` names
    /// how, `tip` is the commit the evidence was verified against.
    Merged { how: &'static str, tip: String },
}

/// Everything the merge/push evidence was computed AGAINST, recorded
/// in the unlocked probe so the locked phase can prove nothing moved
/// without re-running `gh` or `fetch` while holding writers out:
/// the branch tip, the default ref (name AND commit — either can
/// change mid-probe), and the remote-tracking tip. `remote_tip` is
/// fetch-fresh under `--remote`, else the tracking ref as it stands.
/// `remote_cmp` is false only when a `--remote` fetch could not
/// prove the remote ref — there is no freshness to defend, the note
/// explains, and the delete is skipped. `remote_note` is that note.
pub(crate) struct Evidence {
    pub(crate) state: Branch,
    pub(crate) into: Option<String>,
    pub(crate) into_sha: Option<String>,
    pub(crate) remote_tip: Option<String>,
    pub(crate) remote_cmp: bool,
    pub(crate) remote_note: Option<String>,
}

/// The branch's tip once: `rev-parse refs/heads/<branch>`.
pub(crate) fn branch_tip(root: &Path, branch: &str) -> Option<String> {
    if branch.is_empty() {
        return None;
    }
    git(
        root,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    )
    .ok()
}

/// Where `branch` was cut: the oldest entry of its reflog — `branch:
/// Created from …` for every lane `issue start` mints. `None` when
/// the reflog is gone (expired or disabled): the merge rules decide.
pub(crate) fn fork_point(root: &Path, branch: &str) -> Option<String> {
    let log = git(
        root,
        &[
            "reflog",
            "show",
            "--format=%H",
            &format!("refs/heads/{branch}"),
        ],
    )
    .ok()?;
    log.lines()
        .last()
        .map(str::to_string)
        .filter(|s| !s.is_empty())
}

/// CAD-275: zero commits beyond `git merge-base <tip> <fork point>`
/// — the branch still sits where it was cut (or behind it). Measured
/// against the fork point, not the default branch: a lane merged by
/// fast-forward also has no commits beyond the default branch, yet it
/// did start.
pub(crate) fn not_started(root: &Path, branch: &str, tip: &str) -> bool {
    let Some(fork) = fork_point(root, branch) else {
        return false;
    };
    git(root, &["rev-list", "--count", &format!("{fork}..{tip}")]).is_ok_and(|n| n == "0")
}

/// The tracking tip once: `rev-parse refs/remotes/origin/<branch>`.
pub(crate) fn tracking_tip(root: &Path, branch: &str) -> Option<String> {
    git(
        root,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/remotes/origin/{branch}"),
        ],
    )
    .ok()
}

/// Resolve every input the finish decision needs — all in the
/// unlocked probe. Under `--remote` the tracking ref is fetched
/// first: the remote gate and the delete's lease must evaluate the
/// server's CURRENT tip, never a stale view. A fetch that cannot
/// prove the remote ref (unreachable, or the branch gone there)
/// yields no tip — the delete is skipped and `remote_note` says why;
/// a stale tracking ref is never read as evidence.
pub(crate) fn evidence(t: &Target, remote: bool) -> Evidence {
    let tip = branch_tip(&t.root, &t.branch);
    let into = default_ref(&t.root);
    let into_sha = into
        .as_deref()
        .and_then(|d| git(&t.root, &["rev-parse", "--verify", "--quiet", d]).ok());
    let state = match tip {
        None => Branch::Gone,
        Some(tip) if not_started(&t.root, &t.branch, &tip) => Branch::NotStarted { tip },
        Some(tip) => match into
            .as_deref()
            .and_then(|d| merge_rule(&t.root, &t.branch, &tip, d))
        {
            Some(how) => Branch::Merged { how, tip },
            None => Branch::Open { tip },
        },
    };
    let (remote_tip, remote_cmp, remote_note) = if t.branch.is_empty() {
        (None, false, None)
    } else if remote {
        // An explicit refspec, not a bare branch name: the tracking ref
        // updates even when `remote.origin.fetch` does not map it (a
        // single-branch clone), so the gate and the lease never read a
        // stale tip. `--` ends option parsing before any tracker value.
        let refspec = format!("+refs/heads/{0}:refs/remotes/origin/{0}", t.branch);
        match git_out(&t.root, &["fetch", "--", "origin", &refspec], &[]) {
            Ok(o) if o.status.success() => (tracking_tip(&t.root, &t.branch), true, None),
            Ok(o) if String::from_utf8_lossy(&o.stderr).contains("couldn't find remote ref") => (
                None,
                false,
                Some(format!(
                    "no 'origin/{}' on the remote — nothing deleted",
                    t.branch
                )),
            ),
            _ => (
                None,
                false,
                Some(format!(
                    "cannot reach origin to refresh 'origin/{}' — remote kept",
                    t.branch
                )),
            ),
        }
    } else {
        (tracking_tip(&t.root, &t.branch), true, None)
    };
    Evidence {
        state,
        into,
        into_sha,
        remote_tip,
        remote_cmp,
        remote_note,
    }
}

/// Survivability for `t`'s branch from recorded `evidence`:
/// `merged_by` names the merge evidence; `tip` is the commit a
/// `branch -D` may delete — `Some` only when evidence (merge rule or
/// a pushed remote ref) covers it. A tip with uncovered commits
/// yields the `unmerged-unpushed` block and NO deletable tip — the
/// branch is never deleted.
pub(crate) fn survivability(
    ev: &Evidence,
    t: &Target,
) -> (Option<&'static str>, Option<String>, Option<Block>) {
    match &ev.state {
        Branch::Merged { how, tip } => (Some(*how), Some(tip.clone()), None),
        // No work on the branch — deleting it loses nothing. Whether
        // the lane may be finished at all is `inspect`'s `not-started`
        // block, not survivability.
        Branch::NotStarted { tip } => (None, Some(tip.clone()), None),
        Branch::Open { tip } => {
            let pushed = ev
                .remote_tip
                .as_deref()
                .is_some_and(|r| ancestor(&t.root, tip, r));
            if pushed {
                // Every commit on the branch is on the remote —
                // deleting the local ref loses nothing.
                (None, Some(tip.clone()), None)
            } else {
                (
                    None,
                    None,
                    Some(Block {
                        tag: "unmerged-unpushed".to_string(),
                        reason: format!(
                            "Branch '{}' is neither merged into the default \
                             branch nor pushed — its work would be lost. Merge or \
                             push it, or pass --force",
                            t.branch
                        ),
                    }),
                )
            }
        }
        Branch::Gone => (None, None, None),
    }
}

/// Phase-2 staleness for the probe's evidence — all local
/// `rev-parse`s, no fetch/gh/rpc under the lock. Any input the
/// probe's evidence depended on moving mid-probe makes the probe
/// stale: refuse with "retry" rather than re-run network checks
/// while holding writers out. A remote-side move the tracking ref
/// cannot see is covered by the leased delete instead.
pub(crate) fn stale_evidence_reason(root: &Path, branch: &str, ev: &Evidence) -> Option<String> {
    if !branch.is_empty() {
        let tip_now = branch_tip(root, branch);
        let ev_tip = match &ev.state {
            Branch::Merged { tip, .. } | Branch::Open { tip } | Branch::NotStarted { tip } => {
                Some(tip.clone())
            }
            Branch::Gone => None,
        };
        if tip_now != ev_tip {
            return Some(format!("branch '{branch}' tip moved during finish — retry"));
        }
        if ev.remote_cmp && tracking_tip(root, branch) != ev.remote_tip {
            return Some(format!("'origin/{branch}' moved during finish — retry"));
        }
    }
    let into_now = default_ref(root);
    if into_now != ev.into {
        return Some("the default branch changed during finish — retry".to_string());
    }
    let into_sha_now = into_now
        .as_deref()
        .and_then(|d| git(root, &["rev-parse", "--verify", "--quiet", d]).ok());
    if into_sha_now != ev.into_sha {
        return Some("the default branch moved during finish — retry".to_string());
    }
    None
}

/// Atomically delete `branch` iff its tip is exactly `tip` —
/// `update-ref -d <ref> <tip>` is the compare-and-delete: a tip that
/// moved (or a ref that vanished) fails and keeps its commits.
pub(crate) fn delete_branch_at(root: &Path, branch: &str, tip: &str) -> bool {
    git(
        root,
        &["update-ref", "-d", &format!("refs/heads/{branch}"), tip],
    )
    .is_ok()
}

/// Delete `origin/<branch>` leased on `expect` — the tip the probe
/// proved covered. `--force-with-lease=<ref>:<expect>` makes the
/// server refuse when its ref moved since the fetch: an origin-ahead
/// branch is never deleted by a stale view. The empty-src refspec
/// deletes the ref.
pub(crate) fn remote_delete(root: &Path, branch: &str, expect: &str) -> Result<()> {
    git(
        root,
        &[
            "push",
            &format!("--force-with-lease=refs/heads/{branch}:{expect}"),
            "--",
            "origin",
            &format!(":refs/heads/{branch}"),
        ],
    )
    .map(|_| ())
}

/// Phase-2 staleness: a retargeted pair or a newly recorded in-scope
/// message ref means the unlocked probe is stale — even --force does
/// not retarget a probe. Returns the retry reason when stale.
pub(crate) fn stale_probe_reason(probe: &Target, t: &Target) -> Option<String> {
    if t.wt_dir != probe.wt_dir || t.branch != probe.branch {
        Some(format!(
            "{}'s worktree/branch refs changed during finish — retry",
            t.front.id
        ))
    } else if !t.msg_refs.is_subset(&probe.msg_refs) {
        Some("A dispatch was recorded during finish — retry".to_string())
    } else {
        None
    }
}
