//! PR and git probe classification for `issue::reconcile`.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde_json::{json, Value};

use crate::issue::{board, finish, line_times::LineTimes, model, project, time};
use crate::worktree::layout;

/// One GitHub PR row relevant to an issue lane.
#[derive(Clone, Debug, PartialEq)]
pub struct Pr {
    pub number: u64,
    /// `open` | `merged` | `closed` — lowercase from `--state all`.
    pub state: String,
    /// Merge time (epoch seconds) when `state == "merged"`.
    pub merged_at: Option<i64>,
    /// The PR's recorded head commit — the coverage check binds the
    /// merge to exactly this commit, like `finish::pr_merged`.
    pub head_oid: String,
    /// PR base branch — only merges into the repo's default ref count.
    pub base: String,
}

/// What the GitHub probe answered for one branch — distinguishable so
/// a `gh` outage never reads as "no PR".
pub(super) enum GhProbe {
    /// The repo has no GitHub origin — PR channels do not apply.
    NoRemote,
    /// `gh` failed or answered garbage — PR state is UNKNOWN.
    Unreachable,
    Ok(Vec<Pr>),
}

/// The `gh pr list --head` lookup seam — tests inject answers.
pub(super) type PrLookup<'a> = &'a dyn Fn(&Path, &str) -> GhProbe;
/// The `gh pr view <url>` lookup seam for `ref pr:` targets.
pub(super) type PrView<'a> = &'a dyn Fn(&Path, &str) -> GhProbe;

/// Real `gh` lookups (bounded, like `finish::pr_merged`).
pub(super) fn gh_list(root: &Path, branch: &str) -> GhProbe {
    let Ok(url) = finish::git(root, &["remote", "get-url", "origin"]) else {
        return GhProbe::NoRemote;
    };
    if !url.contains("github.com") {
        return GhProbe::NoRemote;
    }
    let mut cmd = Command::new("gh");
    cmd.args([
        "pr",
        "list",
        "--head",
        branch,
        "--state",
        "all",
        "--json",
        "number,state,mergedAt,headRefOid,baseRefName",
        "--limit",
        "10",
    ])
    .current_dir(root);
    let Ok(out) = crate::gh_guard::run(&mut cmd, Duration::from_secs(10)) else {
        return GhProbe::Unreachable;
    };
    if !out.status.success() {
        return GhProbe::Unreachable;
    }
    let Ok(v) = serde_json::from_slice::<Value>(&out.stdout) else {
        return GhProbe::Unreachable;
    };
    let Some(rows) = v.as_array() else {
        return GhProbe::Unreachable;
    };
    GhProbe::Ok(
        rows.iter()
            .filter_map(|r| {
                Some(Pr {
                    number: r["number"].as_u64()?,
                    state: r["state"].as_str()?.to_ascii_lowercase(),
                    merged_at: r["mergedAt"].as_str().and_then(time::parse_iso),
                    head_oid: r["headRefOid"].as_str()?.to_string(),
                    base: r["baseRefName"].as_str()?.to_string(),
                })
            })
            .collect(),
    )
}

/// `gh pr view <url>` — a `ref pr:` target names its PR explicitly.
pub(super) fn gh_view(cwd: &Path, url: &str) -> GhProbe {
    if !url.contains("github.com") {
        return GhProbe::NoRemote;
    }
    let mut cmd = Command::new("gh");
    cmd.args([
        "pr",
        "view",
        url,
        "--json",
        "number,state,mergedAt,headRefOid,baseRefName",
    ])
    .current_dir(cwd);
    let Ok(out) = crate::gh_guard::run(&mut cmd, Duration::from_secs(10)) else {
        return GhProbe::Unreachable;
    };
    if !out.status.success() {
        return GhProbe::Unreachable;
    }
    let Ok(v) = serde_json::from_slice::<Value>(&out.stdout) else {
        return GhProbe::Unreachable;
    };
    (|| {
        Some(Pr {
            number: v["number"].as_u64()?,
            state: v["state"].as_str()?.to_ascii_lowercase(),
            merged_at: v["mergedAt"].as_str().and_then(time::parse_iso),
            head_oid: v["headRefOid"].as_str()?.to_string(),
            base: v["baseRefName"].as_str()?.to_string(),
        })
    })()
    .map(|p| GhProbe::Ok(vec![p]))
    .unwrap_or(GhProbe::Unreachable)
}

/// Per-issue classification inputs, gathered before any write.
pub(super) struct Probe {
    pub(super) id: String,
    pub(super) status: String,
    claim_at: Option<i64>,
    /// `(branch, repo root)` — one per branch ref + implied worktree
    /// branch, deduped; roots resolved via `assumed_root`/`repo_identity`.
    lanes: Vec<(String, PathBuf)>,
    /// `ref pr:` urls — evidence without a local lane.
    pr_urls: Vec<String>,
    /// A root GitHub could run in for url probes (any lane's root).
    any_root: Option<PathBuf>,
}

/// Resolved verdict for one issue — `reason` is the human/log string.
pub(super) enum Verdict {
    Close {
        how: &'static str,
        branch: String,
        pr: Option<u64>,
        merged_at: Option<i64>,
    },
    /// Merged evidence exists but a live signal blocks the flip.
    Held(&'static str),
    /// PRs exist, all closed unmerged, nothing merged — parked work.
    Stalled,
    /// Real work in flight — leave alone.
    Open(&'static str),
    /// Nothing to classify.
    Skip(&'static str),
}

/// A branch's tip — `(tip, is_local)`. Local `refs/heads` first, else
/// the remote-tracking tip (the pushed state `finish --remote`
/// defends; `merge_rule` decides what either is covered by).
fn lane_tip(root: &Path, branch: &str) -> Option<(String, bool)> {
    finish::branch_tip(root, branch)
        .map(|t| (t, true))
        .or_else(|| {
            finish::git(
                root,
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    &format!("refs/remotes/origin/{branch}"),
                ],
            )
            .ok()
            .map(|t| (t, false))
        })
}

/// The note-driven arm's freshness check (CAD-823): a notes-derived
/// `doing`/`review` admits a non-terminal file only when the newest
/// tagged note is at least as fresh as the `status:` line's last
/// recorded write. A status move after the note — the operator
/// parking or re-scoping — outranks a stale note. `None` for `times`
/// (the history walk failed) means "cannot tell" — don't widen on a
/// guess; `None` for `status_at` means no write was ever recorded,
/// so the note is the freshest signal.
pub(super) fn note_fresh(times: Option<&LineTimes>, v: &board::View) -> bool {
    let Some(times) = times else { return false };
    let Some(at) = times.status_at(&v.issue.project, &v.issue.front.id) else {
        return true;
    };
    v.chain
        .last()
        .and_then(|n| time::parse_iso(&n.at))
        .is_some_and(|note_at| note_at >= at)
}

/// Everything one issue's refs say about its lanes, before evidence.
pub(super) fn probe(issue: &board::Issue) -> Probe {
    let front = &issue.front;
    let mut lanes: Vec<(String, PathBuf)> = Vec::new();
    let mut roots: Vec<PathBuf> = Vec::new();
    let mut push_root = |p: Option<PathBuf>| {
        if let Some(p) = p {
            if !roots.iter().any(|r| r == &p) {
                roots.push(p);
            }
        }
    };
    // Repo roots from every recorded worktree (open or closed — the
    // path shape holds the root even when the dir is gone).
    for r in front.refs.iter().filter(|r| r.kind == "worktree") {
        let Some(p) = r.path.as_deref() else { continue };
        let d = Path::new(p);
        if d.is_dir() {
            if let Some((root, _)) = project::repo_identity(d) {
                push_root(Some(root));
                continue;
            }
        }
        push_root(layout::assumed_root(d));
    }
    let mut branch = |name: &str| {
        if model::check_ref_value(name).is_err() {
            return;
        }
        for root in &roots {
            if !lanes.iter().any(|(b, _)| b == name) {
                lanes.push((name.to_string(), root.clone()));
            }
        }
    };
    // Branch refs first — open and closed alike (a finished lane's
    // delivery still counts; the evidence check decides, not the ref
    // state). Then the implied branch of every worktree ref.
    for r in front.refs.iter().filter(|r| r.kind == "branch") {
        if let Some(name) = r.path.as_deref() {
            branch(name);
        }
    }
    for wt in front.refs.iter().filter(|r| r.kind == "worktree") {
        if let Some(name) = Path::new(wt.path.as_deref().unwrap_or(""))
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
        {
            branch(&layout::branch(&name));
        }
    }
    let pr_urls = front
        .refs
        .iter()
        .filter(|r| r.kind == "pr")
        .filter_map(|r| r.url.clone().or_else(|| r.path.clone()))
        .filter(|u| u.starts_with("http"))
        .collect();
    Probe {
        id: front.id.clone(),
        status: front.status.clone(),
        claim_at: front.claim.as_ref().and_then(|c| time::parse_iso(&c.at)),
        lanes,
        pr_urls,
        any_root: roots.first().cloned(),
    }
}

/// Did merged PR `pr` cover `tip`? Same rule as `finish::pr_merged`:
/// the recorded head IS the tip, or the tip is its ancestor (local
/// branch behind the merged head). Coverage is provable only against
/// a local object — a deleted branch accepts the merge at face value:
/// there is no unprotected work left to lose.
fn covers(root: &Path, tip: Option<&str>, pr: &Pr) -> bool {
    match tip {
        None => true,
        Some(t) => {
            t == pr.head_oid
                || finish::git(root, &["cat-file", "-e", &pr.head_oid]).is_ok()
                    && finish::git(root, &["merge-base", "--is-ancestor", t, &pr.head_oid]).is_ok()
        }
    }
}

/// The full evidence pass for one issue's lanes and pr refs.
pub(super) fn classify(p: &Probe, pr_list: PrLookup<'_>, pr_view: PrView<'_>) -> (Verdict, Value) {
    if p.lanes.is_empty() && p.pr_urls.is_empty() {
        return (Verdict::Skip("no-refs"), json!({}));
    }
    let mut detail_lanes = Vec::new();
    let mut merged: Option<(&'static str, String, Option<u64>, Option<i64>)> = None;
    let mut open_pr: Option<u64> = None;
    let mut closed_pr = false;
    let mut merged_at: Option<i64> = None;
    let mut saw_unmerged = false;
    let mut saw_not_started = false;
    // Every ref whose GitHub probe could not answer (CAD-1755): one
    // successful probe never vouches for another ref's PR state.
    let mut unverified: Vec<String> = Vec::new();

    for (branch, root) in &p.lanes {
        let into = finish::default_ref(root);
        let tip_pair = lane_tip(root, branch);
        let tip = tip_pair.as_ref().map(|(t, _)| t.clone());
        let mut row = json!({
            "branch": branch,
            "tip": tip,
            "local": Value::Null,
            "prs": Value::Null,
        });
        // CAD-754: an unstarted lane must never read as merged — an
        // empty diff reverse-applies, so `merge_rule` would call a
        // pristine branch "patch"-merged. `finish` gates this through
        // `Branch::NotStarted`; the same check runs here first. A
        // remote-only tip has no reflog to fork from — `rev-list`
        // counts its contribution over the default branch instead.
        let started = match (&tip_pair, &into) {
            (Some((t, true)), _) => !finish::not_started(root, branch, t),
            (Some((t, false)), Some(into)) => {
                finish::git(root, &["rev-list", "--count", &format!("{into}..{t}")])
                    .is_ok_and(|n| n != "0")
            }
            _ => true,
        };
        if !started {
            row["local"] = json!("not-started");
            saw_not_started = true;
        } else if let (Some(t), Some(into)) = (&tip, &into) {
            // Local merge evidence — ancestry | cherry | patch | pr.
            if let Some(how) = finish::merged_claim(root, branch, t, into) {
                row["local"] = json!(how);
                merged.get_or_insert((how, branch.clone(), None, None));
            } else {
                saw_unmerged = true;
            }
        }
        match pr_list(root, branch) {
            GhProbe::NoRemote => row["prs"] = json!("no-remote"),
            GhProbe::Unreachable => {
                unverified.push(branch.clone());
                row["prs"] = json!("unreachable");
            }
            GhProbe::Ok(prs) => {
                row["prs"] = json!(prs
                    .iter()
                    .map(|pr| json!({"number": pr.number, "state": pr.state}))
                    .collect::<Vec<_>>());
                for pr in prs.iter().filter(|pr| pr.state == "open") {
                    open_pr.get_or_insert(pr.number);
                }
                closed_pr |= prs.iter().any(|pr| pr.state == "closed");
                let base_ok = |pr: &&Pr| {
                    into.as_deref()
                        .map(|i| pr.base == i.strip_prefix("origin/").unwrap_or(i))
                        .unwrap_or(true)
                };
                if let Some(pr) = prs
                    .iter()
                    .filter(|pr| pr.state == "merged")
                    .filter(base_ok)
                    .find(|pr| covers(root, tip.as_deref(), pr))
                {
                    merged_at = merged_at.max(pr.merged_at);
                    merged.get_or_insert(("pr", branch.clone(), Some(pr.number), pr.merged_at));
                }
            }
        }
        detail_lanes.push(row);
    }
    for url in &p.pr_urls {
        let cwd = p.any_root.clone().unwrap_or_else(|| {
            p.lanes
                .first()
                .map(|(_, r)| r.clone())
                .unwrap_or_else(|| PathBuf::from("."))
        });
        match pr_view(&cwd, url) {
            GhProbe::Ok(prs) => {
                for pr in prs {
                    match pr.state.as_str() {
                        "open" => {
                            open_pr.get_or_insert(pr.number);
                        }
                        "closed" => closed_pr = true,
                        "merged" => {
                            merged_at = merged_at.max(pr.merged_at);
                            merged.get_or_insert((
                                "pr",
                                url.clone(),
                                Some(pr.number),
                                pr.merged_at,
                            ));
                        }
                        _ => {}
                    }
                }
            }
            GhProbe::Unreachable => unverified.push(url.clone()),
            GhProbe::NoRemote => {}
        }
    }

    let mut detail = json!({"lanes": detail_lanes});
    if !unverified.is_empty() {
        detail["unverified"] = json!(unverified);
    }
    let verdict = match merged {
        Some((how, lane, pr, _)) => {
            if open_pr.is_some() {
                Verdict::Held("open-pr")
            } else if !unverified.is_empty() {
                // Merge evidence exists but some ref's PR state is
                // unverifiable — an open follow-up PR or a
                // closed-unmerged PR there could change the verdict.
                // Never close on a guess; `detail.unverified` names them.
                Verdict::Held("pr-state-unverified")
            } else if let Some(claim) = p.claim_at {
                match merged_at {
                    Some(at) if claim > at => Verdict::Held("post-merge-claim"),
                    None => Verdict::Held("claim-undated"),
                    _ => Verdict::Close {
                        how,
                        branch: lane,
                        pr,
                        merged_at,
                    },
                }
            } else {
                Verdict::Close {
                    how,
                    branch: lane,
                    pr,
                    merged_at,
                }
            }
        }
        None => {
            if open_pr.is_some() {
                Verdict::Open("pr-open")
            } else if closed_pr {
                Verdict::Stalled
            } else if saw_unmerged {
                Verdict::Open("unmerged")
            } else if saw_not_started {
                Verdict::Open("not-started")
            } else {
                Verdict::Skip("no-evidence")
            }
        }
    };
    (verdict, detail)
}
