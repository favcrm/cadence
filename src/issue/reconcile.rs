//! CAD-754 `issue reconcile` — close the gap between merge reality
//! and tracker status. `doing`/`review` is a claims ledger: work that
//! landed keeps its status until someone sweeps it (on 2026-09-28,
//! 86 of 101 `doing` leaves and 13 of 14 `review` items already had
//! merged PRs). The sweep classifies every `doing`/`review` LEAF
//! issue — containers derive — and marks `done` the ones whose
//! recorded work provably merged, in the same commit shape as
//! `mark_done_on_merge` (CAD-449), one per issue naming the evidence.
//!
//! Evidence channels, in order of authority:
//! - a recorded `pr:` ref (`gh pr view <url>`) that reports merged
//! - a `branch`/worktree ref whose branch a merged GitHub PR covered
//!   (`gh pr list --head`), head-commit checked like `pr_merged`
//! - a branch tip `merge_rule` proves landed on the default branch
//!   (ancestry | cherry | patch — the no-PR paths `finish` trusts)
//!
//! A close additionally requires: no OPEN PR on any of the issue's
//! branches or pr refs (a post-merge revision in flight means the
//! ticket is still live work), and — when a merge timestamp is
//! known — no claim recorded after it (a re-claim means a second
//! increment started). When merge evidence exists but GitHub cannot
//! be checked, the issue is `held`, never closed on a guess.
//!
//! After the flips, `finish --merged` sweeps the worktree refs the
//! same merge evidence already proved — `reconcile` is the status
//! half of the same reality check. The daemon tick (watch.rs) calls
//! [`run_daemon`], which skips the finish sweep: probing worktree
//! liveness goes through `client::rpc`, which the daemon must not
//! issue to itself.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde_json::{json, Value};

use crate::error::Result;
use crate::issue::model;
use crate::issue::time;
use crate::issue::{board, finish, project, write, Pm};
use crate::proc::run_bounded;
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
enum GhProbe {
    /// The repo has no GitHub origin — PR channels do not apply.
    NoRemote,
    /// `gh` failed or answered garbage — PR state is UNKNOWN.
    Unreachable,
    Ok(Vec<Pr>),
}

/// The `gh pr list --head` lookup seam — tests inject answers.
type PrLookup<'a> = &'a dyn Fn(&Path, &str) -> GhProbe;
/// The `gh pr view <url>` lookup seam for `ref pr:` targets.
type PrView<'a> = &'a dyn Fn(&Path, &str) -> GhProbe;

/// Real `gh` lookups (bounded, like `finish::pr_merged`).
fn gh_list(root: &Path, branch: &str) -> GhProbe {
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
    let Ok(out) = run_bounded(&mut cmd, Duration::from_secs(10)) else {
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
fn gh_view(cwd: &Path, url: &str) -> GhProbe {
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
    let Ok(out) = run_bounded(&mut cmd, Duration::from_secs(10)) else {
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
struct Probe {
    id: String,
    status: String,
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
enum Verdict {
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

/// Everything one issue's refs say about its lanes, before evidence.
fn probe(issue: &board::Issue) -> Probe {
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
fn classify(p: &Probe, pr_list: PrLookup<'_>, pr_view: PrView<'_>) -> (Verdict, Value) {
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
    let mut gh_failed = false;
    let mut gh_checked = false;

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
            if let Some(how) = finish::merge_rule(root, branch, t, into) {
                row["local"] = json!(how);
                merged.get_or_insert((how, branch.clone(), None, None));
            } else {
                saw_unmerged = true;
            }
        }
        match pr_list(root, branch) {
            GhProbe::NoRemote => row["prs"] = json!("no-remote"),
            GhProbe::Unreachable => {
                gh_failed = true;
                row["prs"] = json!("unreachable");
            }
            GhProbe::Ok(prs) => {
                gh_checked = true;
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
                gh_checked = true;
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
            GhProbe::Unreachable => gh_failed = true,
            GhProbe::NoRemote => {}
        }
    }

    let detail = json!({"lanes": detail_lanes});
    let verdict = match merged {
        Some((how, lane, pr, _)) => {
            if open_pr.is_some() {
                Verdict::Held("open-pr")
            } else if gh_failed && !gh_checked {
                // Local evidence says merged but GitHub is unverifiable
                // — an open PR or closed-unmerged PR could change the
                // verdict. Never close on a guess.
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

/// `issue reconcile [--project P] [--dry-run] [--limit N]` — the
/// sweep. Leaf issues that look in flight — file `doing`/`review`,
/// or a non-terminal file whose notes/job derive `doing`/`review`
/// (CAD-823) — are marked done when their recorded work merged (one
/// commit per issue, `mark_done_on_merge` semantics: `expect` pins
/// the status it was read at so a mid-sweep reopen is never
/// overwritten). `held`/`stalled` rows are reported, not moved.
/// Then `finish --merged` sweeps the same evidence — worktree refs
/// and branches the merge already covered get cleaned up.
pub fn run(
    pm: &Pm,
    project: Option<&str>,
    dry_run: bool,
    actor: &str,
    state_dir: &Path,
    limit: usize,
) -> Result<Value> {
    run_inner(
        pm,
        project,
        dry_run,
        actor,
        Some(state_dir),
        limit,
        &gh_list,
        &gh_view,
    )
}

/// The daemon tick — same sweep without the finish pass: worktree
/// liveness probes go through `client::rpc`, which the daemon must
/// not call on itself (CAD-754). `limit` caps issues per tick; the
/// next tick takes the rest.
pub fn run_daemon(pm: &Pm, actor: &str, limit: usize) -> Result<Value> {
    run_inner(pm, None, false, actor, None, limit, &gh_list, &gh_view)
}

#[allow(clippy::too_many_arguments)]
fn run_inner(
    pm: &Pm,
    project: Option<&str>,
    dry_run: bool,
    actor: &str,
    state_dir: Option<&Path>,
    limit: usize,
    pr_list: PrLookup<'_>,
    pr_view: PrView<'_>,
) -> Result<Value> {
    let issues = board::load_all(&pm.dir, project)?;
    // Candidacy follows the derived status, not only the file field:
    // a verdict note no longer fakes `done` (CAD-823), so a ticket
    // whose file was never moved off backlog/ready — but whose notes
    // or job derive doing|review — must still reach the probe, else a
    // merged PR strands it at review forever.
    let jobs = state_dir.map(board::fetch_job_outcomes).unwrap_or_default();
    let views = board::views_with_jobs(&pm.config.notes_dir(), issues, &jobs);
    let children: HashSet<&str> = views
        .iter()
        .filter_map(|v| v.issue.front.parent.as_deref())
        .collect();
    let mut rows = Vec::new();
    let mut done = Vec::new();
    let mut held = Vec::new();
    let mut stalled = Vec::new();
    let mut errors = Vec::new();
    let mut classified = 0usize;

    for v in &views {
        let f = &v.issue.front;
        if !matches!(f.status.as_str(), "doing" | "review")
            && !matches!(v.status.as_str(), "doing" | "review")
        {
            continue;
        }
        if children.contains(f.id.as_str()) || f.item_type.as_deref() == Some("epic") {
            continue;
        }
        if limit > 0 && classified >= limit {
            break;
        }
        classified += 1;
        let p = probe(&v.issue);
        let (verdict, detail) = classify(&p, pr_list, pr_view);
        let mut row = json!({"issue": p.id, "status": p.status});
        row["detail"] = detail;
        match verdict {
            Verdict::Skip(why) => {
                row["outcome"] = json!("skipped");
                row["reason"] = json!(why);
            }
            Verdict::Open(why) => {
                row["outcome"] = json!("open");
                row["reason"] = json!(why);
            }
            Verdict::Stalled => {
                row["outcome"] = json!("stalled");
                row["reason"] = json!("pr-closed-unmerged");
                stalled.push(p.id.clone());
            }
            Verdict::Held(why) => {
                row["outcome"] = json!("held");
                row["reason"] = json!(why);
                held.push(p.id.clone());
            }
            Verdict::Close {
                how,
                branch,
                pr,
                merged_at,
            } => {
                let why = match (pr, merged_at) {
                    (Some(n), Some(at)) => format!(
                        "reconcile: {branch} merged ({how}, PR #{n}, {})",
                        time::iso(at)
                    ),
                    (Some(n), None) => format!("reconcile: {branch} merged ({how}, PR #{n})"),
                    _ => format!("reconcile: {branch} merged ({how})"),
                };
                row["merged_by"] = json!(how);
                row["evidence"] = json!(why);
                if dry_run {
                    row["outcome"] = json!("would-close");
                } else {
                    match write::mark_done_on_merge(pm, &p.id, &why, actor, Some(&p.status)) {
                        Ok(None) => {
                            row["outcome"] = json!("closed");
                            done.push(p.id.clone());
                        }
                        Ok(Some(now)) => {
                            row["outcome"] = json!("skipped");
                            row["reason"] = json!(format!("status moved to {now} mid-sweep"));
                        }
                        Err(e) => {
                            row["outcome"] = json!("error");
                            row["reason"] = json!(e.to_string());
                            errors.push(p.id.clone());
                        }
                    }
                }
            }
        }
        rows.push(row);
    }

    // The worktree half: after status flips, sweep every merged open
    // worktree ref in scope — the same merge evidence, one row each.
    // Skipped entirely under the daemon (state_dir None).
    let finish = match state_dir {
        Some(d) => Some(finish::sweep(pm, project, false, dry_run, actor, d)?),
        None => None,
    };

    Ok(json!({
        "dry_run": dry_run,
        "project": project,
        "classified": classified,
        "rows": rows,
        "done": done,
        "held": held,
        "stalled": stalled,
        "errors": errors,
        "finish": finish,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::issue::model::{Claim, Front, Ref};
    use std::fs;
    use tempfile::TempDir;

    /// A pm dir + one project + a git repo wired like a lane path.
    struct Rig {
        _tmp: TempDir,
        pm: Pm,
        repo: PathBuf,
        key: String,
    }

    fn rig() -> Rig {
        let tmp = TempDir::new().unwrap();
        let t = tmp.path();
        let pm_dir = t.join("pm");
        let mut pm = Pm::init(&pm_dir).unwrap();
        // The sweep derives candidacy through the board views, which
        // read the notes dir — keep it a throwaway path, never the
        // default /var/www/agent-notes.
        pm.config.notes_dir = t.join("notes").display().to_string();
        let key = "cad".to_string();
        let pdir = pm_dir.join(&key);
        fs::create_dir_all(&pdir).unwrap();
        fs::write(
            pdir.join("project.yaml"),
            "key: cad\nprefix: CAD\nrepos: []\n",
        )
        .unwrap();
        let repo = t.join("repo");
        fs::create_dir_all(&repo).unwrap();
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .args(args)
                .current_dir(&repo)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?} failed");
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        git(&["init", "-b", "main"]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "t"]);
        fs::write(repo.join("f"), "one").unwrap();
        git(&["add", "f"]);
        git(&["commit", "-m", "one"]);
        Rig {
            _tmp: tmp,
            pm,
            repo,
            key,
        }
    }

    /// One issue folder: status doing, a lane worktree+branch pair
    /// recorded under `repo` like `issue start` mints them.
    fn put_issue(rig: &Rig, n: u32, claim_at: Option<&str>) -> String {
        let id = format!("CAD-{n}");
        let dir = rig.pm.dir.join(&rig.key).join(&id);
        fs::create_dir_all(&dir).unwrap();
        let mut front = Front::new(&id, "t", "2026-09-28T00:00:00Z");
        front.status = "doing".to_string();
        if let Some(at) = claim_at {
            front.claim = Some(Claim {
                by: "op".to_string(),
                at: at.to_string(),
                session: None,
                last_seen: None,
                note: None,
                stale: None,
            });
        }
        front.refs.push(Ref {
            kind: "worktree".to_string(),
            path: Some(rig.repo.join(".cadence/wt/lane").display().to_string()),
            url: None,
            label: None,
            closed: None,
            worktree: None,
            cargo_target: None,
            agent: None,
        });
        front.refs.push(Ref {
            kind: "branch".to_string(),
            path: Some("cadence/lane".to_string()),
            url: None,
            label: None,
            closed: None,
            worktree: None,
            cargo_target: None,
            agent: None,
        });
        write::save_front(&dir, &front, "").unwrap();
        id
    }

    /// `put_issue` with a non-`doing` file status — the note-driven
    /// tickets CAD-823 widened candidacy for.
    fn put_issue_at(rig: &Rig, n: u32, claim_at: Option<&str>, status: &str) -> String {
        let id = put_issue(rig, n, claim_at);
        let dir = rig.pm.dir.join(&rig.key).join(&id);
        let (mut front, body) = write::load_front(&dir).unwrap();
        front.status = status.to_string();
        write::save_front(&dir, &front, &body).unwrap();
        id
    }

    fn git(repo: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?} failed");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// `cadence/lane` with one commit past main.
    fn lane(repo: &Path, merged: bool) {
        git(repo, &["branch", "cadence/lane"]);
        git(repo, &["checkout", "-q", "cadence/lane"]);
        std::fs::write(repo.join("f"), "two").unwrap();
        git(repo, &["commit", "-qam", "two"]);
        git(repo, &["checkout", "-q", "main"]);
        if merged {
            git(repo, &["merge", "-q", "--no-ff", "-m", "m", "cadence/lane"]);
        }
    }

    fn no_gh(_: &Path, _: &str) -> GhProbe {
        GhProbe::NoRemote
    }
    fn no_view(_: &Path, _: &str) -> GhProbe {
        GhProbe::Unreachable
    }

    fn sweep(rig: &Rig, dry: bool, pl: PrLookup<'_>, pv: PrView<'_>) -> Value {
        run_inner(&rig.pm, None, dry, "op", None, 0, pl, pv).unwrap()
    }

    fn status(rig: &Rig, id: &str) -> String {
        let dir = rig.pm.dir.join(&rig.key).join(id);
        let (f, _) = write::load_front(&dir).unwrap();
        f.status
    }

    #[test]
    fn merged_lane_closes() {
        let rig = rig();
        let id = put_issue(&rig, 1, None);
        lane(&rig.repo, true);
        let out = sweep(&rig, false, &no_gh, &no_view);
        assert_eq!(status(&rig, &id), "done", "{out}");
        assert!(out["done"].as_array().unwrap().contains(&json!(id)));
    }

    /// The guard that earns the gate: an unstarted branch's empty
    /// diff reverse-applies, so `merge_rule` alone reports it "patch"
    /// merged — a freshly claimed ticket would read as done. CAD-754's
    /// own lane caught this on the first live dry run.
    #[test]
    fn unstarted_lane_is_not_merged() {
        let rig = rig();
        let id = put_issue(&rig, 12, None);
        git(&rig.repo, &["branch", "cadence/lane"]); // cut, never committed
        let out = sweep(&rig, false, &no_gh, &no_view);
        assert_eq!(status(&rig, &id), "doing", "{out}");
        assert_eq!(out["rows"][0]["outcome"], "open");
        assert_eq!(out["rows"][0]["reason"], "not-started");
    }

    #[test]
    fn unmerged_lane_stays_open() {
        let rig = rig();
        let id = put_issue(&rig, 2, None);
        lane(&rig.repo, false);
        let out = sweep(&rig, false, &no_gh, &no_view);
        assert_eq!(status(&rig, &id), "doing");
        assert_eq!(out["rows"][0]["outcome"], "open");
    }

    #[test]
    fn merged_pr_with_claim_before_merge_closes() {
        let rig = rig();
        let id = put_issue(&rig, 3, Some("2026-09-25T10:00:00Z"));
        lane(&rig.repo, false); // local tip unmerged — gh carries the evidence
        let pl = |_: &Path, _: &str| {
            GhProbe::Ok(vec![Pr {
                number: 42,
                state: "merged".to_string(),
                merged_at: Some(time::parse_iso("2026-09-26T10:00:00Z").unwrap()),
                head_oid: git(&rig.repo, &["rev-parse", "cadence/lane"]),
                base: "main".to_string(),
            }])
        };
        let out = sweep(&rig, false, &pl, &no_view);
        assert_eq!(status(&rig, &id), "done", "{out}");
    }

    #[test]
    fn claim_after_merge_holds() {
        let rig = rig();
        let id = put_issue(&rig, 4, Some("2026-09-27T10:00:00Z"));
        lane(&rig.repo, false);
        let pl = |_: &Path, _: &str| {
            GhProbe::Ok(vec![Pr {
                number: 42,
                state: "merged".to_string(),
                merged_at: Some(time::parse_iso("2026-09-26T10:00:00Z").unwrap()),
                head_oid: git(&rig.repo, &["rev-parse", "cadence/lane"]),
                base: "main".to_string(),
            }])
        };
        let out = sweep(&rig, false, &pl, &no_view);
        assert_eq!(status(&rig, &id), "doing");
        assert_eq!(out["rows"][0]["outcome"], "held");
        assert_eq!(out["rows"][0]["reason"], "post-merge-claim");
    }

    #[test]
    fn open_pr_on_the_branch_holds() {
        let rig = rig();
        let id = put_issue(&rig, 5, None);
        lane(&rig.repo, true); // merged AND an open follow-up PR
        let pl = |_: &Path, _: &str| {
            GhProbe::Ok(vec![
                Pr {
                    number: 42,
                    state: "merged".to_string(),
                    merged_at: Some(time::parse_iso("2026-09-26T10:00:00Z").unwrap()),
                    head_oid: git(&rig.repo, &["rev-parse", "cadence/lane"]),
                    base: "main".to_string(),
                },
                Pr {
                    number: 43,
                    state: "open".to_string(),
                    merged_at: None,
                    head_oid: "x".to_string(),
                    base: "main".to_string(),
                },
            ])
        };
        let out = sweep(&rig, false, &pl, &no_view);
        assert_eq!(status(&rig, &id), "doing");
        assert_eq!(out["rows"][0]["reason"], "open-pr");
    }

    #[test]
    fn claim_with_undated_merge_holds() {
        let rig = rig();
        let id = put_issue(&rig, 6, Some("2026-09-25T10:00:00Z"));
        lane(&rig.repo, true); // ancestry evidence — no mergedAt anywhere
        let out = sweep(&rig, false, &no_gh, &no_view);
        assert_eq!(status(&rig, &id), "doing");
        assert_eq!(out["rows"][0]["reason"], "claim-undated");
    }

    #[test]
    fn closed_unmerged_pr_reports_stalled() {
        let rig = rig();
        let id = put_issue(&rig, 7, None);
        lane(&rig.repo, false);
        let pl = |_: &Path, _: &str| {
            GhProbe::Ok(vec![Pr {
                number: 42,
                state: "closed".to_string(),
                merged_at: None,
                head_oid: "x".to_string(),
                base: "main".to_string(),
            }])
        };
        let out = sweep(&rig, false, &pl, &no_view);
        assert_eq!(status(&rig, &id), "doing");
        assert_eq!(out["rows"][0]["outcome"], "stalled");
    }

    #[test]
    fn dry_run_writes_nothing() {
        let rig = rig();
        let id = put_issue(&rig, 8, None);
        lane(&rig.repo, true);
        let out = sweep(&rig, true, &no_gh, &no_view);
        assert_eq!(status(&rig, &id), "doing");
        assert_eq!(out["rows"][0]["outcome"], "would-close");
    }

    #[test]
    fn ref_pr_url_merged_closes_without_lane() {
        let rig = rig();
        let id = "CAD-9".to_string();
        let dir = rig.pm.dir.join(&rig.key).join(&id);
        fs::create_dir_all(&dir).unwrap();
        let mut front = Front::new(&id, "t", "2026-09-28T00:00:00Z");
        front.status = "review".to_string();
        front.refs.push(Ref {
            kind: "pr".to_string(),
            url: Some("https://github.com/favcrm/cadence/pull/9".to_string()),
            path: None,
            label: None,
            closed: None,
            worktree: None,
            cargo_target: None,
            agent: None,
        });
        write::save_front(&dir, &front, "").unwrap();
        let pv = |_: &Path, url: &str| {
            assert!(url.ends_with("/9"));
            GhProbe::Ok(vec![Pr {
                number: 9,
                state: "merged".to_string(),
                merged_at: Some(1),
                head_oid: "x".to_string(),
                base: "main".to_string(),
            }])
        };
        let out = sweep(&rig, false, &no_gh, &pv);
        assert_eq!(status(&rig, &id), "done", "{out}");
    }

    /// CAD-823 follow-up (Devin Review on #597): a ticket whose file
    /// never left `backlog` still closes when its notes derive
    /// `review` and the lane merged — a verdict can no longer fake
    /// `done`, so candidacy follows the derived status.
    #[test]
    fn merged_lane_closes_note_driven_backlog() {
        let rig = rig();
        let id = put_issue_at(&rig, 20, None, "backlog");
        fs::create_dir_all(rig.pm.config.notes_dir()).unwrap();
        fs::write(
            rig.pm
                .config
                .notes_dir()
                .join("20260928-120000-x-verdict.md"),
            format!("# Verdict: x\n> Issue: `{id}`\n\n## Verdict\npass\n"),
        )
        .unwrap();
        lane(&rig.repo, true);
        let out = sweep(&rig, false, &no_gh, &no_view);
        assert_eq!(status(&rig, &id), "done", "{out}");
        assert!(out["done"].as_array().unwrap().contains(&json!(id)));
    }

    /// Same shape, nothing merged: the note-derived `review` makes it
    /// a candidate but no evidence means no write — the file stays
    /// `backlog`.
    #[test]
    fn unmerged_lane_keeps_note_driven_backlog() {
        let rig = rig();
        let id = put_issue_at(&rig, 21, None, "backlog");
        fs::create_dir_all(rig.pm.config.notes_dir()).unwrap();
        fs::write(
            rig.pm
                .config
                .notes_dir()
                .join("20260928-120000-x-verdict.md"),
            format!("# Verdict: x\n> Issue: `{id}`\n\n## Verdict\npass\n"),
        )
        .unwrap();
        lane(&rig.repo, false);
        let out = sweep(&rig, false, &no_gh, &no_view);
        assert_eq!(status(&rig, &id), "backlog", "{out}");
        assert_eq!(out["rows"][0]["outcome"], "open");
    }

    #[test]
    fn containers_never_classified() {
        let rig = rig();
        let parent = put_issue(&rig, 10, None);
        // A child makes CAD-10 a rollup — its doing is derived.
        let kid = "CAD-11".to_string();
        let dir = rig.pm.dir.join(&rig.key).join(&kid);
        fs::create_dir_all(&dir).unwrap();
        let mut front = Front::new(&kid, "t", "2026-09-28T00:00:00Z");
        front.status = "backlog".to_string();
        front.parent = Some(parent.clone());
        write::save_front(&dir, &front, "").unwrap();
        lane(&rig.repo, true);
        let out = sweep(&rig, false, &no_gh, &no_view);
        assert_eq!(status(&rig, &parent), "doing", "{out}");
        assert_eq!(out["classified"], 0);
    }
}
