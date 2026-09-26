//! CAD-535: `cadence job` — moved verbatim from src/main.rs.

use super::*;

use cadence_agent::proc::BoundedError;

#[derive(Subcommand)]
pub(crate) enum JobAction {
    /// Create a job — bookkeeping, not spawning. Requires a registered
    /// PM (any endpoint kind; an inbox alias collects notifications for
    /// `cadence inbox`) and a readable spec file. Writes the job plus
    /// one default task `<job>-t1` covering the spec.
    New {
        /// Owning PM agent — the job's group root.
        #[arg(long)]
        pm: String,
        /// Spec/brief file — hashed at creation for drift detection.
        #[arg(long)]
        spec: PathBuf,
        /// Client idempotency key; same id + same spec hash dedupes.
        #[arg(long)]
        job: Option<String>,
        #[arg(long)]
        title: Option<String>,
        /// Board issue this job tracks (`<PREFIX>-<n>`, grammar only —
        /// the daemon never reads the board filesystem).
        #[arg(long)]
        issue: Option<String>,
        /// Repo root the job's worktrees live under.
        #[arg(long)]
        repo: Option<PathBuf>,
        /// Base branch/SHA QA is relative to.
        #[arg(long)]
        base_ref: Option<String>,
        /// Automatic revision cycles before a revise verdict escalates
        /// the task to blocked.
        #[arg(long, default_value_t = 2)]
        max_revisions: i64,
        /// Silence budget for turns this job's kickoffs start — over it
        /// the PM gets a `turn_stalled` notice (0 disables).
        #[arg(long)]
        stall_secs: Option<u64>,
        /// Title for the default `<job>-t1` task.
        #[arg(long)]
        task_title: Option<String>,
        /// Worktree name (`.cadence/wt/<name>`) scoped onto `<job>-t1`.
        #[arg(long)]
        task_worktree: Option<String>,
        /// Branch scoped onto `<job>-t1`.
        #[arg(long)]
        task_branch: Option<String>,
        /// Base revision scoped onto `<job>-t1`.
        #[arg(long)]
        task_base_sha: Option<String>,
        /// Assignee scoped onto `<job>-t1` — the PM or a group member.
        #[arg(long)]
        task_assignee: Option<String>,
    },
    /// List jobs — non-terminal by default; `--all` widens and a
    /// `--state` selection does too (it names states the default
    /// hides). Repeatable — any of the values.
    #[command(after_long_help = cadence_agent::filter::GRAMMAR)]
    List {
        /// draft open done failed cancelled; repeatable — any of them.
        #[arg(long, value_delimiter = ',')]
        state: Vec<String>,
        #[arg(long)]
        all: bool,
        /// Sort by id state title pm issue created updated; `-KEY`
        /// descending.
        #[arg(long, allow_hyphen_values = true)]
        sort: Option<String>,
        /// Keep only the first N rows.
        #[arg(long)]
        limit: Option<usize>,
        /// Keep only these keys in each row (comma-joined).
        #[arg(long, value_delimiter = ',')]
        fields: Vec<String>,
        /// Output is JSON already — accepted for grammar parity.
        #[arg(long)]
        json: bool,
    },
    /// Show a job: tasks with live kickoff state, drift flags, latest
    /// verdicts.
    Show { job: String },
    /// The job-scoped event view — every event any alias row recorded
    /// for this job. Default page is the newest 50; `--after` pages
    /// forward like `cadence events`.
    Events {
        job: String,
        /// Return events after this cursor; omit for the newest page.
        #[arg(long)]
        after: Option<i64>,
        /// Seconds to wait for new events per request (0-30).
        #[arg(long, default_value_t = 0)]
        wait: u64,
        #[arg(long)]
        follow: bool,
    },
    /// Dispatch a task: enqueue its kickoff to the assignee at a new
    /// revision. Legal from draft/revising and — once the live kickoff
    /// ended without completing — dispatched/running. A live kickoff
    /// makes this an idempotent retry of the same revision.
    Dispatch {
        task: String,
        /// Reassign to another group member (bumps the revision).
        #[arg(long)]
        to: Option<String>,
        /// Claim `agent ready` for the worker first — same operator
        /// claim as `send --ready`; no-op on non-pty endpoints.
        #[arg(long)]
        ready: bool,
        /// Force the ready claim past a busy probe verdict.
        #[arg(long, requires = "ready")]
        force: bool,
        /// Explicit kickoff message id (default: deterministic
        /// cadence-dispatch:<task>:r<n>).
        #[arg(long)]
        message: Option<String>,
    },
    /// Record a QA verdict bound to the task's reported commit.
    /// The reviewer is the verified caller (CAD-372): the agent whose
    /// pane or managed endpoint runs this command, or `operator` from
    /// an operator shell outside every pane. It is never a flag or an
    /// env value. The reviewer can never be the assignee.
    #[command(group = clap::ArgGroup::new("verdict").required(true).args(["pass", "revise", "blocked"]))]
    Verdict {
        task: String,
        /// The commit this verdict judges — must equal the task's
        /// reported head_sha.
        #[arg(long)]
        sha: String,
        /// Approve the revision — task moves to verified.
        #[arg(long)]
        pass: bool,
        /// Request changes — task re-dispatches until max_revisions,
        /// then escalates to blocked.
        #[arg(long)]
        revise: bool,
        /// Stop the task — blocked until an operator reopens it.
        #[arg(long)]
        blocked: bool,
        /// Optional check of who this caller is (`operator` outside a
        /// pane, else the pane's alias). It is never sent: the daemon
        /// derives the reviewer from the connection (CAD-372), and a
        /// mismatch is refused here before anything is written.
        #[arg(long)]
        reviewer: Option<String>,
        /// Evidence file — commands run, outputs, artifact paths.
        #[arg(long)]
        evidence: Option<PathBuf>,
        /// Message id that carried the QA report, if any.
        #[arg(long)]
        message: Option<String>,
        /// Pin the revision this verdict names — a stale value rejects.
        #[arg(long)]
        revision: Option<i64>,
        /// Skip the worktree verification — the opt-out is recorded on
        /// the verdict.
        #[arg(long)]
        no_verify_worktree: bool,
        /// Skip posting the `qa-verdict` commit status to the PR head.
        #[arg(long)]
        no_status: bool,
        /// Post the status to this PR number instead of discovering the
        /// open PR on the task's branch.
        #[arg(long)]
        pr: Option<u64>,
    },
    /// Accept a verified task — records the merge claim. Cadence never
    /// runs git merges itself.
    Accept {
        task: String,
        /// The merge commit once it lands — recorded as evidence.
        #[arg(long)]
        merged_sha: Option<String>,
    },
    /// Cancel every non-terminal task and the job. Queued kickoffs are
    /// cancelled in the same transaction; running ones finish alone.
    /// Agents are never stopped by a job.
    Cancel { job: String },
    /// Close a job — legal only when every task is done.
    Close { job: String },
    /// Manage a job's tasks.
    Task {
        #[command(subcommand)]
        action: TaskAction,
    },
}

/// The `cadence job` tree — thin RPC wrappers. Validation, transitions
/// and notifications live in the daemon/store so every caller (CLI,
/// agent pane, operator) sees the same rules. `pane`/`by` carry the
/// caller's identity claim: inside a cadence pane `CADENCE_ALIAS` is
/// the actor; outside, `operator`.
pub(super) fn run_job(state_dir: &Path, action: &JobAction) -> Result<i32> {
    let pane = std::env::var("CADENCE_ALIAS").ok();
    // CAD-384: the daemon attributes the caller itself — an agent's pane
    // as that agent, the proven operator as `operator`. Send only what
    // names the caller, never an `operator` default.
    let by = pane.clone();
    let rpc = |method: &str, params: Value| client::rpc(state_dir, method, params);
    match action {
        JobAction::New {
            pm,
            spec,
            job,
            title,
            issue,
            repo,
            base_ref,
            max_revisions,
            stall_secs,
            task_title,
            task_worktree,
            task_branch,
            task_base_sha,
            task_assignee,
        } => {
            // Canonicalize + hash client-side: the daemon stores the
            // path/hash and never needs the board or spec filesystem.
            let spec_path = spec.canonicalize().map_err(|_| {
                Error::rejected(format!("Spec file {} is unreadable", spec.display()))
            })?;
            let bytes = std::fs::read(&spec_path)?;
            use sha2::{Digest, Sha256};
            let spec_sha256 = format!("{:x}", Sha256::digest(&bytes));
            let repo = repo
                .as_ref()
                .map(|r| r.canonicalize().unwrap_or_else(|_| r.clone()));
            print_json(&rpc(
                "job_new",
                json!({"pm": pm, "spec": spec_path, "spec_sha256": spec_sha256,
                       "job": job, "title": title, "issue": issue,
                       "repo": repo, "base_ref": base_ref,
                       "max_revisions": max_revisions,
                       "stall_secs": stall_secs,
                       "task_title": task_title,
                       "task_worktree": task_worktree,
                       "task_branch": task_branch,
                       "task_base_sha": task_base_sha,
                       "task_assignee": task_assignee}),
            )?);
        }
        JobAction::List {
            state,
            all,
            sort,
            limit,
            fields,
            json: _,
        } => {
            let mut out = rpc("job_list", json!({"states": state, "all": all}))?;
            shape_rows(
                &mut out,
                "jobs",
                sort.as_deref(),
                &[
                    ("id", "id"),
                    ("state", "state"),
                    ("title", "title"),
                    ("pm", "pm"),
                    ("issue", "issue"),
                    ("created", "created"),
                    ("updated", "updated"),
                ],
                "id",
                *limit,
                fields,
            )?;
            print_json(&out);
        }
        JobAction::Show { job } => {
            print_json(&rpc("job_show", json!({"job": job}))?);
        }
        JobAction::Events {
            job,
            after,
            wait,
            follow,
        } => {
            // Same default as `cadence events`: no --after means the
            // newest page, then forward paging from its cursor.
            let mut cursor = match after {
                Some(cursor) => *cursor,
                None => {
                    let page = rpc("job_events", json!({"job": job, "tail": true}))?;
                    print_json(&page);
                    if !*follow {
                        return Ok(0);
                    }
                    page.get("cursor").and_then(Value::as_i64).unwrap_or(0)
                }
            };
            loop {
                let page = rpc(
                    "job_events",
                    json!({"job": job, "after": cursor,
                           "wait": if *follow { 25 } else { *wait }}),
                )?;
                let empty = page
                    .get("events")
                    .and_then(Value::as_array)
                    .is_some_and(Vec::is_empty);
                if !empty || !*follow {
                    print_json(&page);
                }
                cursor = page.get("cursor").and_then(Value::as_i64).unwrap_or(cursor);
                if !*follow {
                    return Ok(0);
                }
            }
        }
        JobAction::Dispatch {
            task,
            to,
            ready,
            force,
            message,
        } => {
            // --ready is the same operator claim as `send --ready`:
            // resolve the assignee (explicit --to or the stored one),
            // claim the pty gate if there is one, then dispatch. The
            // claim probes the pane — --force overrides a busy verdict.
            if *ready {
                let assignee = match to {
                    Some(a) => Some(a.clone()),
                    None => rpc("task_show", json!({"task": task}))?["task"]["assignee"]
                        .as_str()
                        .map(str::to_string),
                };
                if let Some(assignee) = assignee {
                    let show = rpc("agent_show", json!({"alias": assignee}))?;
                    let agent = &show["agent"];
                    if registry::ready_gate(
                        agent["provider"].as_str().unwrap_or_default(),
                        agent["endpoint_kind"].as_str().unwrap_or_default(),
                    ) {
                        rpc(
                            "agent_ready",
                            json!({"alias": assignee, "by": pane, "force": force}),
                        )?;
                    }
                }
            }
            print_json(&rpc(
                "task_dispatch",
                json!({"task": task, "to": to, "message": message,
                       "by": by}),
            )?);
        }
        JobAction::Verdict {
            task,
            sha,
            pass,
            revise,
            blocked,
            reviewer,
            evidence,
            message,
            revision,
            no_verify_worktree,
            no_status,
            pr,
        } => {
            let verdict = if *pass {
                "pass"
            } else if *revise {
                "revise"
            } else if *blocked {
                "blocked"
            } else {
                return Err(Error::rejected(
                    "job verdict needs one of --pass, --revise, --blocked",
                ));
            };
            // `--reviewer` only states who the caller believes it is;
            // the daemon records the verified connection's identity.
            if let Some(claimed) = reviewer {
                let expected = pane.as_deref().unwrap_or("operator");
                if claimed != expected {
                    return Err(Error::rejected(format!(
                        "--reviewer '{claimed}' is not this caller ('{expected}'): the \
                         reviewer is the verified connection (CAD-372) — run the \
                         verdict from the reviewer's own pane, or from an operator \
                         shell for 'operator'"
                    )));
                }
            }
            let evidence = evidence.as_ref().map(std::fs::read_to_string).transpose()?;

            // Worktree verification runs client-side in the job's repo
            // before anything is written; the same task/job fetch feeds
            // the status bridge below. It only gates a verdict that
            // could land — a stale sha or a task not in review stays
            // the store's own rejection.
            let view = rpc("task_show", json!({"task": task}))?["task"].clone();
            let scoped = view["worktree"].is_string() && view["branch"].is_string();
            let landable = view["state"].as_str() == Some("review")
                && view["head_sha"].as_str() == Some(sha.to_ascii_lowercase().as_str());
            let job = if scoped || view["branch"].is_string() {
                rpc("job_show", json!({"job": view["job"]}))?["job"].clone()
            } else {
                Value::Null
            };
            let mut verify = Value::Null;
            if *no_verify_worktree {
                verify = json!({"checked": [], "skipped": [{"check": "worktree verification",
                        "reason": "opted out via --no-verify-worktree"}]});
            } else if scoped && landable {
                let repo = verdict_repo(&job, &view)?;
                verify = verify_worktree(&repo, &view, sha)?;
            }

            let mut out = rpc(
                "task_verdict",
                json!({"task": task, "sha": sha, "verdict": verdict,
                       "evidence": evidence, "message": message,
                       "revision": revision, "verify": verify}),
            )?;

            // The qa-verdict bridge never decides the verdict — every
            // failure reports {posted: false, reason} alongside the
            // committed verdict.
            out["status"] = if *no_status {
                json!({"posted": false, "reason": "skipped via --no-status"})
            } else {
                let job = if job.is_null() {
                    rpc("job_show", json!({"job": view["job"]}))?["job"].clone()
                } else {
                    job
                };
                let revision = out["verdict"]["revision"].as_i64().unwrap_or(0);
                verdict_status_post(&job, &view, sha, verdict, revision, *pr)
            };
            print_json(&out);
        }
        JobAction::Accept { task, merged_sha } => {
            print_json(&rpc(
                "task_accept",
                json!({"task": task, "merged_sha": merged_sha, "by": by}),
            )?);
        }
        JobAction::Cancel { job } => {
            print_json(&rpc("job_cancel", json!({"job": job, "by": by}))?);
        }
        JobAction::Close { job } => {
            print_json(&rpc("job_close", json!({"job": job, "by": by}))?);
        }
        JobAction::Task { action } => match action {
            TaskAction::Add {
                job,
                task,
                title,
                assignee,
                spec,
                accept,
                worktree,
                branch,
                base_sha,
            } => {
                let spec = spec
                    .as_ref()
                    .map(|s| s.canonicalize().unwrap_or_else(|_| s.clone()));
                print_json(&rpc(
                    "task_new",
                    json!({"job": job, "task": task, "title": title,
                           "assignee": assignee, "spec": spec,
                           "acceptance": accept, "worktree": worktree,
                           "branch": branch, "base_sha": base_sha}),
                )?);
            }
            TaskAction::Show { task } => {
                print_json(&rpc("task_show", json!({"task": task}))?);
            }
            TaskAction::Sha { task, sha } => {
                print_json(&rpc(
                    "task_sha",
                    json!({"task": task, "sha": sha, "by": by}),
                )?);
            }
            TaskAction::Fail { task, reason } => {
                print_json(&rpc(
                    "task_fail",
                    json!({"task": task, "reason": reason, "by": by}),
                )?);
            }
            TaskAction::Reopen { task } => {
                print_json(&rpc("task_reopen", json!({"task": task}))?);
            }
            TaskAction::Cancel { task } => {
                print_json(&rpc("task_cancel", json!({"task": task, "by": by}))?);
            }
        },
    }
    Ok(0)
}

/// Spawn `prog args` in `cwd`, capture output, kill after 10s — the
/// short timeout every verdict check gets. Ok(stdout) on exit 0; the
/// Err string carries stderr/exit/spawn/timeout.
pub(super) fn run_capped(
    prog: &str,
    args: &[String],
    cwd: &Path,
) -> std::result::Result<String, String> {
    let mut cmd = Command::new(prog);
    cmd.args(args).current_dir(cwd);
    let out = match cadence_agent::proc::run_bounded(&mut cmd, Duration::from_secs(10)) {
        Ok(out) => out,
        Err(BoundedError::TimedOut { .. }) => return Err(format!("{prog} timed out after 10s")),
        Err(e) => return Err(format!("{prog}: {e}")),
    };
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(format!(
            "{prog} exited {}: {}",
            out.status.code().unwrap_or(-1),
            stderr
        ))
    }
}

pub(super) fn run_git(cwd: &Path, args: &[&str]) -> std::result::Result<String, String> {
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    run_capped("git", &args, cwd)
}

/// Every `gh` invocation in the verdict bridge goes through here so
/// tests can put a fake `gh` first on PATH.
pub(super) fn gh(cwd: &Path, args: &[String]) -> std::result::Result<String, String> {
    run_capped("gh", args, cwd)
}

/// `owner/name` from a GitHub remote URL (`git@github.com:o/n.git`,
/// `https://github.com/o/n`, `ssh://git@github.com/o/n`) — None for
/// any other host.
pub(super) fn github_slug(url: &str) -> Option<String> {
    let rest = url.split_once("github.com")?.1;
    let rest = rest.strip_prefix([':', '/'])?;
    let slug = rest.trim_end_matches('/').trim_end_matches(".git");
    let mut parts = slug.split('/');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(owner), Some(name), None) if !owner.is_empty() && !name.is_empty() => {
            Some(format!("{owner}/{name}"))
        }
        _ => None,
    }
}

/// The repo the verdict checks run in: the job's recorded `repo`, or —
/// when the job lost it — the main repo resolved from the task's
/// worktree (`<repo>/.cadence/wt/<name>` shares the object store).
/// Neither existing is a rejection, not a skip: verification was on by
/// default and could not run.
pub(super) fn verdict_repo(job: &Value, task: &Value) -> Result<PathBuf> {
    if let Some(repo) = job["repo"].as_str() {
        return Ok(PathBuf::from(repo));
    }
    if let Some(wt) = task["worktree"].as_str().map(PathBuf::from) {
        if wt.is_dir() {
            if let Ok(common) = run_git(
                &wt,
                &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            ) {
                if let Some(root) = Path::new(common.trim()).parent() {
                    return Ok(root.to_path_buf());
                }
            }
        }
        return Err(Error::rejected(format!(
            "job '{}' has no repo and the worktree {} cannot resolve \
             one — cannot verify the worktree \
             (`--no-verify-worktree` bypasses)",
            task["job"].as_str().unwrap_or("?"),
            wt.display()
        )));
    }
    Err(Error::rejected(format!(
        "job '{}' has no repo — cannot verify the worktree \
         (`--no-verify-worktree` bypasses)",
        task["job"].as_str().unwrap_or("?")
    )))
}

/// `job verdict` worktree verification — bind the judged sha to the
/// task's worktree and branch: it resolves to a commit, it is the tip
/// of the branch, the task's base is an ancestor, the worktree is
/// clean, and `origin/<branch>` equals it (the commit is pushed).
/// Each failed check rejects naming the check and both values; checks
/// that cannot apply land in `skipped` with the reason.
pub(super) fn verify_worktree(repo: &Path, task: &Value, sha: &str) -> Result<Value> {
    let branch = task["branch"].as_str().unwrap_or_default();
    let mut checked: Vec<&str> = Vec::new();
    let mut skipped: Vec<Value> = Vec::new();
    let fail = |check: &str, detail: String| {
        Error::rejected(format!("worktree verify — {check}: {detail}"))
    };

    let resolved = run_git(
        repo,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{sha}^{{commit}}"),
        ],
    )
    .map_err(|e| {
        fail(
            "commit",
            format!(
                "{sha} does not resolve to a commit in {} ({e})",
                repo.display()
            ),
        )
    })?
    .trim()
    .to_string();
    checked.push("commit");

    let tip = run_git(
        repo,
        &["rev-parse", "--verify", &format!("refs/heads/{branch}")],
    )
    .map_err(|e| {
        fail(
            "branch tip",
            format!("branch {branch} does not resolve ({e})"),
        )
    })?
    .trim()
    .to_string();
    if tip != resolved {
        return Err(fail(
            "branch tip",
            format!("branch {branch} is at {tip}, judged sha is {resolved}"),
        ));
    }
    checked.push("branch tip");

    match task["base_sha"].as_str() {
        Some(base) => {
            run_git(repo, &["merge-base", "--is-ancestor", base, &resolved]).map_err(|_| {
                fail(
                    "base ancestor",
                    format!("{base} is not an ancestor of {resolved}"),
                )
            })?;
            checked.push("base ancestor");
        }
        None => skipped.push(json!({"check": "base ancestor",
            "reason": "task has no base_sha"})),
    }

    // `task.worktree` is the scope claim `.cadence/wt/<name>` —
    // `issue start` stores the bare name; an absolute path is honored
    // as recorded.
    let wt = task["worktree"].as_str().map(PathBuf::from).map(|p| {
        if p.is_absolute() {
            p
        } else {
            cadence_agent::worktree::layout::worktree_dir(repo, p)
        }
    });
    match wt {
        Some(dir) if dir.is_dir() => {
            let dirty = run_git(&dir, &["status", "--porcelain"]).map_err(|e| {
                fail(
                    "worktree clean",
                    format!("git status in {}: {e}", dir.display()),
                )
            })?;
            if !dirty.trim().is_empty() {
                return Err(fail(
                    "worktree clean",
                    format!(
                        "{} has uncommitted changes: {}",
                        dir.display(),
                        dirty.lines().take(3).collect::<Vec<_>>().join("; ")
                    ),
                ));
            }
            checked.push("worktree clean");
        }
        Some(dir) => skipped.push(json!({"check": "worktree clean",
            "reason": format!("worktree directory {} is absent", dir.display())})),
        None => skipped.push(json!({"check": "worktree clean",
            "reason": "task has no worktree"})),
    }

    match run_git(repo, &["remote", "get-url", "origin"]) {
        Err(_) => skipped.push(json!({"check": "pushed",
            "reason": "repo has no origin"})),
        Ok(_) => {
            let remote = run_git(
                repo,
                &[
                    "rev-parse",
                    "--verify",
                    &format!("refs/remotes/origin/{branch}"),
                ],
            )
            .map_err(|_| {
                fail(
                    "pushed",
                    format!("origin/{branch} does not resolve — {resolved} is not pushed"),
                )
            })?
            .trim()
            .to_string();
            if remote != resolved {
                return Err(fail(
                    "pushed",
                    format!("origin/{branch} is {remote}, judged sha is {resolved}"),
                ));
            }
            checked.push("pushed");
        }
    }

    Ok(json!({"checked": checked, "skipped": skipped}))
}

/// The open PR to post on: `--pr` names it, else the open PR whose
/// head branch is the task's. Returns `(number, head_sha)` — the head
/// check against the judged sha happens in the caller.
pub(super) fn lookup_pr(
    repo: &Path,
    slug: &str,
    branch: &str,
    pr: Option<u64>,
) -> std::result::Result<(i64, String), String> {
    match pr {
        Some(n) => {
            let out = gh(
                repo,
                &[
                    "pr".to_string(),
                    "view".to_string(),
                    n.to_string(),
                    "--repo".to_string(),
                    slug.to_string(),
                    "--json".to_string(),
                    "number,headRefOid".to_string(),
                ],
            )?;
            let v: Value = serde_json::from_str(&out)
                .map_err(|e| format!("gh pr view {n}: unreadable response ({e})"))?;
            let head = v["headRefOid"]
                .as_str()
                .ok_or_else(|| format!("gh pr view {n}: no headRefOid in response"))?
                .to_string();
            Ok((n as i64, head))
        }
        None => {
            let out = gh(
                repo,
                &[
                    "pr".to_string(),
                    "list".to_string(),
                    "--repo".to_string(),
                    slug.to_string(),
                    "--head".to_string(),
                    branch.to_string(),
                    "--state".to_string(),
                    "open".to_string(),
                    "--json".to_string(),
                    "number,headRefOid".to_string(),
                ],
            )?;
            let v: Value = serde_json::from_str(&out)
                .map_err(|e| format!("gh pr list: unreadable response ({e})"))?;
            match v.as_array().and_then(|prs| prs.first()) {
                Some(pr) => Ok((
                    pr["number"].as_i64().unwrap_or(0),
                    pr["headRefOid"].as_str().unwrap_or_default().to_string(),
                )),
                None => Err(format!("no open PR for branch {branch}")),
            }
        }
    }
}

/// The qa-verdict bridge — post the `qa-verdict` commit status on the
/// PR head that equals the judged sha (the same API call and context
/// as `scripts/qa-verdict.sh`; the script stays the manual path).
/// Posting never decides the verdict: a missing `gh`, no PR, a moved
/// head, or an API error is reported `{posted: false, reason}` while
/// the committed verdict stands.
pub(super) fn verdict_status_post(
    job: &Value,
    task: &Value,
    sha: &str,
    verdict: &str,
    revision: i64,
    pr: Option<u64>,
) -> Value {
    let reason = |r: String| json!({"posted": false, "reason": r});
    let branch = match task["branch"].as_str() {
        Some(b) => b.to_string(),
        None => return reason("task has no branch".to_string()),
    };
    let repo = match job["repo"].as_str() {
        Some(r) => PathBuf::from(r),
        None => return reason("job has no repo".to_string()),
    };
    let origin = match run_git(&repo, &["remote", "get-url", "origin"]) {
        Ok(u) => u.trim().to_string(),
        Err(_) => return reason("repo has no origin".to_string()),
    };
    let slug = match github_slug(&origin) {
        Some(s) => s,
        None => return reason(format!("origin '{origin}' is not a GitHub remote")),
    };
    let (number, head) = match lookup_pr(&repo, &slug, &branch, pr) {
        Ok(found) => found,
        Err(r) => return reason(r),
    };
    if head != sha {
        return reason(format!("pr head {head} is not the judged sha {sha}"));
    }
    let state = if verdict == "pass" {
        "success"
    } else {
        "failure"
    };
    let task_id = task["id"].as_str().unwrap_or("?");
    let description: String = format!("{verdict} — {task_id} r{revision}")
        .chars()
        .take(140)
        .collect();
    match gh(
        &repo,
        &[
            "api".to_string(),
            "--method".to_string(),
            "POST".to_string(),
            format!("repos/{slug}/statuses/{sha}"),
            "-f".to_string(),
            "context=qa-verdict".to_string(),
            "-f".to_string(),
            format!("state={state}"),
            "-f".to_string(),
            format!("description={description}"),
        ],
    ) {
        Ok(_) => json!({"posted": true, "pr": number, "sha": sha}),
        Err(e) => reason(format!("gh post failed: {e}")),
    }
}

pub(super) fn run(state_dir: PathBuf, action: JobAction) -> Result<i32> {
    run_job(&state_dir, &action)
}
