//! `cadence issue` — the only writer for the PM board. Every verb ends
//! in a single git commit inside the PM dir; reads need no daemon.

use std::io::Read;
use std::path::PathBuf;

use clap::Subcommand;
use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::issue::{
    board, doctor, finish, history, hooks, lint, model, project, retro, start, sync, write, Pm,
};

#[derive(Subcommand)]
pub enum IssueAction {
    /// Create the PM dir skeleton (pm.yaml, README, .gitignore, git
    /// init + first commit) and install the pre-commit/post-commit
    /// hooks. Idempotent; a foreign hook is never overwritten.
    Init,
    /// Read-only tracker health report: root, git repo, remote, hook
    /// state, lint, push lag and the failure-log tail. Non-zero exit
    /// when any check fails.
    Doctor,
    /// Manage projects under the PM dir.
    Project {
        #[command(subcommand)]
        action: ProjectAction,
    },
    /// Create an issue: allocates `<PREFIX>-<n>` under the write lock.
    New {
        /// Issue title.
        title: String,
        /// Project key — else CADENCE_PROJECT, else the cwd's repo.
        #[arg(long)]
        project: Option<String>,
        /// P0..P3 [default: P2].
        #[arg(long)]
        priority: Option<String>,
        /// Parent issue id (sub-issue; depth is two levels).
        #[arg(long)]
        parent: Option<String>,
        /// The epic this issue belongs to — an alias of `--parent`.
        #[arg(long, conflicts_with = "parent")]
        epic: Option<String>,
        /// A tag; repeatable. Checked against the project's `tags:`
        /// list when it declares one.
        #[arg(long = "tag")]
        tags: Vec<String>,
        /// blocked_by target; repeatable.
        #[arg(long = "blocked-by")]
        blocked_by: Vec<String>,
        /// Owner [default: the project's default_owner].
        #[arg(long)]
        owner: Option<String>,
        /// Component label (must be declared by the project).
        #[arg(long)]
        component: Option<String>,
        /// Mint this exact id — for seeds/imports; must match the
        /// project prefix and not exist.
        #[arg(long)]
        id: Option<String>,
    },
    /// List issues — a compact table on a TTY, `--json` for agents.
    /// Filters combine (AND).
    Ls {
        #[arg(long)]
        project: Option<String>,
        /// Filter by status (backlog ready doing review done dropped);
        /// repeatable — any of them.
        #[arg(long)]
        status: Vec<String>,
        /// Only issues carrying this tag; repeatable — all of them.
        #[arg(long = "tag")]
        tags: Vec<String>,
        /// Only the children of this epic.
        #[arg(long)]
        epic: Option<String>,
        #[arg(long)]
        owner: Option<String>,
        #[arg(long)]
        component: Option<String>,
        /// P0..P3.
        #[arg(long)]
        priority: Option<String>,
        /// Only open issues — not done, not dropped.
        #[arg(long)]
        open: bool,
        /// Only computed-ready leaves.
        #[arg(long)]
        ready: bool,
        /// Board state at a git revision — exports the tree at `<rev>`
        /// and lists it read-only; cards report `status_source: file`.
        #[arg(long)]
        at: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// `git log` for the issue folder, parsed: `sha`, `at`, `by`
    /// (the ` (actor)` suffix, else the commit author), `kind`
    /// (`created|set|tag|link|unlink|ref|comment|attach|other`), `summary`
    /// and a `fields` map for `set` entries. Read-only.
    Log {
        /// Issue id (CAD-16).
        id: String,
        /// Max entries [default: 50].
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// Field-level diff of `issue.md` between two revisions — `{field,
    /// from, to}` entries plus body line counts and comment/artifact
    /// files added/removed. `issue diff CAD-16` compares the issue's
    /// newest change with its parent; `diff CAD-16 <rev> --to HEAD`
    /// shows everything that changed since `<rev>`.
    Diff {
        id: String,
        /// From-revision [default: the parent of the newest change].
        rev: Option<String>,
        /// To-revision [default: the newest commit on the issue].
        #[arg(long)]
        to: Option<String>,
    },
    /// For every frontmatter field currently set, the history entry
    /// that last changed it (`value`, `sha`, `at`, `by`).
    Blame { id: String },
    /// Read-only retrospective for one issue: review rounds and
    /// verdicts, blocking findings, flake mentions, timings and merge
    /// evidence — assembled from the tracker, tagged notes, project
    /// repos and the daemon store (read-only). Prints a preview;
    /// nothing is attached, promoted or published. `--json` emits the
    /// `cadence.retro/1` document.
    Retro {
        /// Issue id (CAD-16).
        id: String,
        #[arg(long)]
        json: bool,
    },
    /// Print the exact trailer line for this issue (`Issue: <ID>`) —
    /// agents and hooks append it to code commits without guessing
    /// the format. Refuses an id that does not exist.
    Trailer { id: String },
    /// Start work on an issue: mint `.cadence/wt/<id>-<slug>` on
    /// `cadence/<id>-<slug>` in the project repo, record both refs,
    /// move backlog|ready to doing, print the commit trailer. `--job`
    /// also opens an M3 job scoped to the worktree (needs the daemon).
    Start {
        id: String,
        /// Repo path — else the cwd's repo when it is one of the
        /// project's repos, else the project's only repo.
        #[arg(long)]
        repo: Option<PathBuf>,
        /// Worktree slug — default: the slugified title (≤32 chars).
        #[arg(long)]
        name: Option<String>,
        /// Base ref — else the repo's origin/HEAD, else its current
        /// branch. No fetch.
        #[arg(long)]
        base: Option<String>,
        /// Owner to record when the issue has none — default: the
        /// resolved actor.
        #[arg(long)]
        owner: Option<String>,
        /// Open an M3 job bound to the worktree through `job new` +
        /// `job task add` (daemon required; checked before anything
        /// is created).
        #[arg(long, requires_all = ["pm", "spec"])]
        job: bool,
        /// Owning PM agent for the job.
        #[arg(long, requires = "job")]
        pm: Option<String>,
        /// Job spec file — hashed at creation like `job new`.
        #[arg(long, requires = "job")]
        spec: Option<PathBuf>,
        /// Assignee for the worktree-scoped task.
        #[arg(long, requires = "job")]
        assignee: Option<String>,
        /// Bind the task even when the assignee's pty pane cwd lies
        /// outside every repo of the issue's project (CAD-202). The
        /// override is recorded as an issue comment.
        #[arg(long, requires = "assignee")]
        force: bool,
    },
    /// Finish an issue's worktree: refuse while the worktree is in use
    /// (a live message recorded against it, a pane tree or any process
    /// with cwd inside it), while the worktree is dirty, or while the
    /// branch is neither merged nor pushed — `--force` overrides each
    /// (recorded). Then `git worktree remove`, delete the branch
    /// (`--keep-branch` keeps it, `--remote` deletes the remote one
    /// too) and mark both refs `closed: true` in one commit. The
    /// issue's status is untouched. `--merged` instead sweeps every
    /// open worktree ref in scope whose branch is merged and whose
    /// guard passes, one row per worktree; it never forces.
    Finish {
        /// Issue id — required unless --merged.
        id: Option<String>,
        /// Override the in-use, dirty and unmerged refusals —
        /// recorded on the finish commit and in the output. A branch
        /// whose tip no merge/push evidence covers is still kept, and
        /// a probe made stale mid-finish refuses with "retry" — force
        /// never retargets a stale probe or deletes uncovered work.
        #[arg(long, conflicts_with = "merged")]
        force: bool,
        /// Remove the worktree but keep the local branch — with
        /// --remote the remote branch is still deleted when its
        /// evidence gate passes.
        #[arg(long, conflicts_with = "merged")]
        keep_branch: bool,
        /// Also delete the remote branch — the remote tip is fetched
        /// fresh and must be covered by merge evidence (unmerged or
        /// origin-ahead branches keep both copies, noted in the row;
        /// --force deletes anyway and records it). The delete is
        /// leased on the fetched tip: a remote that moved since
        /// refuses the push rather than losing unseen commits.
        #[arg(long)]
        remote: bool,
        /// Sweep every merged+idle worktree in scope — one row per
        /// open worktree ref: finished | skipped(reason) |
        /// refused(reason). Exits 1 when anything refused.
        #[arg(long, conflicts_with = "id")]
        merged: bool,
        /// Limit the sweep to one project (all projects when omitted).
        #[arg(long, requires = "merged")]
        project: Option<String>,
        /// Print the sweep plan without changing anything.
        #[arg(long, requires = "merged")]
        dry_run: bool,
        /// Emit the sweep rows as JSON.
        #[arg(long, requires = "merged")]
        json: bool,
    },
    /// Show one issue — frontmatter, body, links both ways, comments,
    /// artifacts, activity.
    Show {
        /// Issue id (CAD-16).
        id: String,
        #[arg(long)]
        json: bool,
    },
    /// Replace the level-two Acceptance checklist from a file. The file
    /// must contain at least one explicit checked or unchecked item.
    Acceptance {
        /// Issue id (CAD-16).
        id: String,
        /// Checklist file; every nonblank line must be a checkbox item.
        #[arg(long, value_name = "FILE")]
        from: PathBuf,
    },
    /// Set writable fields: `status priority owner component title
    /// tags`. Several ids make a bulk edit: one commit, and nothing is
    /// written unless every id and pair is valid.
    Set {
        /// `<ID>… key=value…` — ids first; an empty value clears
        /// owner/component/tags, `tags=a,b` replaces the tag list.
        #[arg(required = true)]
        args: Vec<String>,
    },
    /// Add or remove tags: `issue tag <ID>… add|rm <tag>…`. Bulk like
    /// `set`: one commit, all-or-nothing.
    Tag {
        /// `<ID>… add|rm <tag>…`
        #[arg(required = true)]
        args: Vec<String>,
    },
    /// Epics — issues with children — and their progress.
    Epic {
        #[command(subcommand)]
        action: EpicAction,
    },
    /// Add a link: `blocked_by|relates|parent|duplicate_of`.
    Link {
        id: String,
        kind: String,
        target: String,
    },
    /// Remove a link.
    Unlink {
        id: String,
        kind: String,
        target: String,
    },
    /// Add a ref: `pr|commit|note|preview|message|url`. A http(s)
    /// target is stored as `url`, anything else as `path`.
    Ref {
        id: String,
        kind: String,
        target: String,
        #[arg(long)]
        label: Option<String>,
    },
    /// Add a comment — one create-only file named by UTC + author
    /// (`CADENCE_ALIAS`, else `operator`).
    Comment {
        id: String,
        /// Inline comment text.
        #[arg(short = 'm', long, conflicts_with = "file")]
        text: Option<String>,
        /// Read the comment body from a file.
        #[arg(long)]
        file: Option<PathBuf>,
        /// Override the recorded author.
        #[arg(long)]
        author: Option<String>,
        /// Optional kind tag (e.g. `note`).
        #[arg(long)]
        kind: Option<String>,
    },
    /// Attach a file into `artifacts/` (basename only, size cap,
    /// create-only).
    Attach { id: String, file: PathBuf },
    /// Check the whole PM dir: schema, id/folder mismatch, dangling and
    /// cyclic links, depth > 2, oversize artifacts, unknown status/kind.
    /// Non-zero exit on any error.
    Lint {
        #[arg(long)]
        project: Option<String>,
    },
    /// Bring the tracker level with `origin`: fetch, rebase the local
    /// commits on top, lint the result, push. A conflict or lint
    /// failure aborts and leaves the tree exactly as found; the report
    /// names the paths. Needs a clean tree and no rebase in progress.
    Sync {
        /// Rebase and lint but do not push.
        #[arg(long)]
        no_push: bool,
        /// Fetch and report ahead/behind plus the paths that would
        /// conflict — nothing changes.
        #[arg(long)]
        dry_run: bool,
        /// Resolve conflicting paths by taking one side whole:
        /// `ours` keeps the local commit's content, `theirs` takes the
        /// fetched remote's.
        #[arg(long, value_parser = ["ours", "theirs"])]
        resolve: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum EpicAction {
    /// List epics with `total`, per-status counts, `done_ratio`,
    /// `blocked` and the distinct owners of their children.
    Ls {
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// One epic and its children: status, owner, priority, tags.
    Show {
        id: String,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub enum ProjectAction {
    /// Register a project.
    Add {
        /// Project key (folder name), e.g. `cadence`.
        key: String,
        /// Issue id prefix, e.g. `CAD`.
        #[arg(long)]
        prefix: String,
        /// A repo path belonging to the project (repeatable); its
        /// origin remote is recorded for cwd resolution.
        #[arg(long = "repo")]
        repos: Vec<String>,
        /// A component label (repeatable).
        #[arg(long = "component")]
        components: Vec<String>,
        /// A declared tag (repeatable) — with any declared, issues may
        /// only carry tags from the list.
        #[arg(long = "tag")]
        tags: Vec<String>,
        /// Default owner applied by `issue new`.
        #[arg(long)]
        owner: Option<String>,
    },
    /// List registered projects.
    Ls,
}

pub(crate) fn print_json(value: &Value) {
    println!(
        "{}",
        serde_json::to_string_pretty(value).unwrap_or_default()
    );
}

fn open_pm() -> Result<Pm> {
    Pm::open_default()
}

/// `cadence issue …` — returns the process exit code.
pub fn run(action: &IssueAction, state_dir: &std::path::Path) -> Result<i32> {
    match action {
        IssueAction::Init => {
            let dir = crate::issue::default_dir()?;
            let pm = Pm::init(&dir)?;
            let installed = hooks::install(&pm.dir)?;
            print_json(&json!({"pm_dir": pm.dir,
                               "git": hooks::git_dir(&pm.dir).is_some(),
                               "hooks": installed}));
            Ok(0)
        }
        IssueAction::Doctor => {
            let pm = open_pm()?;
            let report = doctor::run(&pm)?;
            if report["ok"].as_bool() == Some(true) {
                print_json(&report);
                Ok(0)
            } else {
                eprintln!(
                    "{}",
                    serde_json::to_string_pretty(&report).unwrap_or_default()
                );
                Ok(1)
            }
        }
        IssueAction::Project { action } => match action {
            ProjectAction::Add {
                key,
                prefix,
                repos,
                components,
                tags,
                owner,
            } => {
                let pm = open_pm()?;
                let out = write::project_add(
                    &pm,
                    key,
                    prefix,
                    repos,
                    components,
                    tags,
                    owner.as_deref(),
                )?;
                print_json(&out);
                Ok(0)
            }
            ProjectAction::Ls => {
                let pm = open_pm()?;
                let projects = project::list(&pm.dir)?;
                print_json(&json!({
                    "projects": projects.iter().map(|p| json!({
                        "key": p.key, "prefix": p.prefix,
                        "components": p.components,
                        "tags": p.tags,
                        "default_owner": p.default_owner,
                        "repos": p.repos.iter().map(|r| json!({
                            "path": r.path, "remote": r.remote,
                        })).collect::<Vec<_>>(),
                    })).collect::<Vec<_>>(),
                }));
                Ok(0)
            }
        },
        IssueAction::New {
            title,
            project,
            priority,
            parent,
            epic,
            tags,
            blocked_by,
            owner,
            component,
            id,
        } => {
            let pm = open_pm()?;
            let cwd = std::env::current_dir()?;
            let out = write::new_issue(
                &pm,
                &cwd,
                project.as_deref(),
                title,
                priority.as_deref(),
                parent.as_deref().or(epic.as_deref()),
                blocked_by,
                owner.as_deref(),
                component.as_deref(),
                tags,
                id.as_deref(),
                "",
            )?;
            print_json(&out);
            Ok(0)
        }
        IssueAction::Ls {
            project,
            status,
            tags,
            epic,
            owner,
            component,
            priority,
            open,
            ready,
            at,
            json: json_flag,
        } => {
            let filter = board::Filter {
                tags: tags.clone(),
                epic: epic.clone(),
                owner: owner.clone(),
                statuses: status.clone(),
                component: component.clone(),
                priority: priority.clone(),
                open: *open,
            };
            filter.validate()?;
            let pm = open_pm()?;
            // An unknown key must not read as an empty project that
            // invites `issue new` into the wrong place. `--at` may name
            // a project that existed only in history, so it is exempt.
            if let (Some(want), None) = (project.as_deref(), at) {
                let keys: Vec<String> =
                    project::list(&pm.dir)?.into_iter().map(|p| p.key).collect();
                if !keys.iter().any(|k| k == want) {
                    return Err(Error::rejected(format!(
                        "unknown project '{want}' — known: {}",
                        keys.join(", ")
                    )));
                }
            }
            let (views, at_meta) = match at {
                Some(rev) => {
                    let (meta, views) = history::ls_at(&pm.dir, rev, project.as_deref())?;
                    (views, Some(meta))
                }
                None => {
                    let issues = board::load_all(&pm.dir, project.as_deref())?;
                    let jobs = crate::client::state_dir()
                        .map(|d| board::fetch_job_outcomes(&d))
                        .unwrap_or_default();
                    (
                        board::views_with_jobs(&pm.config.notes_dir(), issues, &jobs),
                        None,
                    )
                }
            };
            let mut views: Vec<&board::View> = views.iter().collect();
            views.retain(|v| filter.matches(v));
            if *ready {
                views.retain(|v| v.ready);
            }
            if *json_flag {
                let mut out = json!({
                    "issues": views.iter().map(|v| board::card_json(v)).collect::<Vec<_>>(),
                });
                if let Some(meta) = at_meta {
                    out["at"] = meta;
                }
                print_json(&out);
            } else {
                if let Some(meta) = &at_meta {
                    eprintln!(
                        "at {} {}",
                        meta["sha"].as_str().unwrap_or("?"),
                        meta["time"].as_str().unwrap_or("?")
                    );
                }
                print_ls_table(&views);
            }
            Ok(0)
        }
        IssueAction::Log { id, limit } => {
            let pm = open_pm()?;
            let issue = board::find_issue(&pm.dir, id)?;
            print_json(&json!({
                "id": issue.front.id,
                "history": history::log(&pm.dir, &issue, *limit)?,
            }));
            Ok(0)
        }
        IssueAction::Diff { id, rev, to } => {
            let pm = open_pm()?;
            let issue = board::find_issue(&pm.dir, id)?;
            print_json(&history::diff(
                &pm.dir,
                &issue,
                rev.as_deref(),
                to.as_deref(),
            )?);
            Ok(0)
        }
        IssueAction::Blame { id } => {
            let pm = open_pm()?;
            let issue = board::find_issue(&pm.dir, id)?;
            print_json(&history::blame(&pm.dir, &issue)?);
            Ok(0)
        }
        IssueAction::Trailer { id } => {
            let pm = open_pm()?;
            let issue = board::find_issue(&pm.dir, id)?;
            println!("Issue: {}", issue.front.id);
            Ok(0)
        }
        IssueAction::Retro { id, json } => {
            let pm = open_pm()?;
            let v = retro::run(&pm.dir, &pm.config.notes_dir(), state_dir, id)?;
            if *json {
                print_json(&v);
            } else {
                print!("{}", retro::render(&v));
            }
            Ok(0)
        }
        IssueAction::Start {
            id,
            repo,
            name,
            base,
            owner,
            job,
            pm,
            spec,
            assignee,
            force,
        } => {
            let pm_dir = open_pm()?;
            let args = start::StartArgs {
                repo: repo.clone(),
                name: name.clone(),
                base: base.clone(),
                owner: owner.clone(),
                job: if *job {
                    Some(start::JobArgs {
                        pm: pm.clone().unwrap_or_default(),
                        spec: spec.clone().unwrap_or_default(),
                        assignee: assignee.clone(),
                        force: *force,
                    })
                } else {
                    None
                },
            };
            print_json(&start::run(&pm_dir, id, &args, "", state_dir)?);
            Ok(0)
        }
        IssueAction::Finish {
            id,
            force,
            keep_branch,
            remote,
            merged,
            project,
            dry_run,
            json,
        } => {
            let pm_dir = open_pm()?;
            if *merged {
                let out = finish::sweep(
                    &pm_dir,
                    project.as_deref(),
                    *remote,
                    *dry_run,
                    "",
                    state_dir,
                )?;
                if *json {
                    print_json(&out);
                } else {
                    for row in out["rows"].as_array().into_iter().flatten() {
                        let mut line = format!(
                            "{}: {}",
                            row["issue"].as_str().unwrap_or("?"),
                            row["outcome"].as_str().unwrap_or("?")
                        );
                        if let Some(r) = row["reason"].as_str() {
                            line.push_str(&format!("({})", r.lines().next().unwrap_or(r)));
                        }
                        if let Some(wt) = row["worktree"].as_str() {
                            line.push_str(&format!(" — {wt}"));
                        }
                        if let Some(how) = row["merged_by"].as_str() {
                            line.push_str(&format!(" [{how}]"));
                        }
                        println!("{line}");
                    }
                }
                return Ok(if out["refused"].as_u64().unwrap_or(0) > 0 {
                    1
                } else {
                    0
                });
            }
            let Some(id) = id.as_deref() else {
                return Err(Error::rejected(
                    "issue finish needs an id — or --merged to sweep",
                ));
            };
            print_json(&finish::run(
                &pm_dir,
                id,
                *force,
                *keep_branch,
                *remote,
                "",
                state_dir,
                None,
            )?);
            Ok(0)
        }
        IssueAction::Show { id, json } => {
            let pm = open_pm()?;
            model::check_id(id)?;
            let issues = board::load_all(&pm.dir, None)?;
            let jobs = crate::client::state_dir()
                .map(|d| board::fetch_job_outcomes(&d))
                .unwrap_or_default();
            let views = board::views_with_jobs(&pm.config.notes_dir(), issues, &jobs);
            let by_id: std::collections::HashMap<String, &board::View> = views
                .iter()
                .map(|v| (v.issue.front.id.clone(), v))
                .collect();
            let view = by_id.get(id).ok_or_else(|| {
                Error::rejected(format!(
                    "Unknown issue '{id}' — `cadence issue ls` lists what exists"
                ))
            })?;
            if *json {
                print_json(&board::detail_json(&pm.dir, view, &by_id));
            } else {
                print_show(view, &by_id);
            }
            Ok(0)
        }
        IssueAction::Acceptance { id, from } => {
            let pm = open_pm()?;
            print_json(&write::set_acceptance(&pm, id, from, "")?);
            Ok(0)
        }
        IssueAction::Set { args } => {
            // Ids never contain `=`, pairs always do.
            let split = args
                .iter()
                .position(|a| a.contains('='))
                .unwrap_or(args.len());
            let (ids, pairs) = args.split_at(split);
            if let Some(stray) = pairs.iter().find(|p| !p.contains('=')) {
                return Err(Error::rejected(format!(
                    "'{stray}' after a key=value pair — ids come first: \
                     `cadence issue set CAD-16 CAD-17 status=ready`"
                )));
            }
            let pm = open_pm()?;
            let out = write::set_fields(&pm, ids, pairs, "")?;
            print_json(&out);
            // The post-merge reminder: a done issue with an open
            // worktree ref still holds the tree — finish it (or sweep
            // every merged one with `issue finish --merged`).
            if let Some(ids) = out["worktree_open"].as_array() {
                for id in ids.iter().filter_map(|i| i.as_str()) {
                    eprintln!("{id}: worktree open: run cadence issue finish {id}");
                }
            }
            Ok(0)
        }
        IssueAction::Tag { args } => {
            let Some(split) = args.iter().position(|a| a == "add" || a == "rm") else {
                return Err(Error::rejected(
                    "tag needs add or rm — `cadence issue tag CAD-16 CAD-17 add ui`",
                ));
            };
            let (ids, rest) = args.split_at(split);
            let pm = open_pm()?;
            print_json(&write::tag_edit(
                &pm,
                ids,
                rest[0] == "add",
                &rest[1..],
                "",
            )?);
            Ok(0)
        }
        IssueAction::Epic { action } => {
            let pm = open_pm()?;
            let project = match action {
                EpicAction::Ls { project, .. } => project.as_deref(),
                EpicAction::Show { .. } => None,
            };
            // Every project loads so cross-project children count.
            let issues = board::load_all(&pm.dir, None)?;
            let jobs = crate::client::state_dir()
                .map(|d| board::fetch_job_outcomes(&d))
                .unwrap_or_default();
            let views = board::views_with_jobs(&pm.config.notes_dir(), issues, &jobs);
            match action {
                EpicAction::Ls { json, .. } => {
                    let epics = board::epics_json(&views, project);
                    if *json {
                        print_json(&json!({"epics": epics}));
                    } else {
                        print_epics_table(&epics);
                    }
                }
                EpicAction::Show { id, json } => {
                    model::check_id(id)?;
                    let by_id: std::collections::HashMap<String, &board::View> = views
                        .iter()
                        .map(|v| (v.issue.front.id.clone(), v))
                        .collect();
                    let epic = by_id.get(id).ok_or_else(|| {
                        Error::rejected(format!(
                            "Unknown issue '{id}' — `cadence issue epic ls` lists epics"
                        ))
                    })?;
                    if !epic.container {
                        return Err(Error::rejected(format!(
                            "{id} has no children — `cadence issue new --epic {id} \"title\"` \
                             makes it an epic"
                        )));
                    }
                    let kids: Vec<&board::View> = epic
                        .children
                        .iter()
                        .filter_map(|k| by_id.get(k).copied())
                        .collect();
                    if *json {
                        let mut out = board::epic_json(epic, &by_id);
                        out["issues"] = kids.iter().map(|v| board::card_json(v)).collect();
                        print_json(&out);
                    } else {
                        print_epics_table(&[board::epic_json(epic, &by_id)]);
                        println!();
                        print_ls_table(&kids);
                    }
                }
            }
            Ok(0)
        }
        IssueAction::Link { id, kind, target } => {
            let pm = open_pm()?;
            let sd = crate::client::state_dir().ok();
            print_json(&write::link(
                &pm,
                id,
                kind,
                target,
                false,
                None,
                "",
                sd.as_deref(),
            )?);
            Ok(0)
        }
        IssueAction::Unlink { id, kind, target } => {
            let pm = open_pm()?;
            let sd = crate::client::state_dir().ok();
            print_json(&write::link(
                &pm,
                id,
                kind,
                target,
                true,
                None,
                "",
                sd.as_deref(),
            )?);
            Ok(0)
        }
        IssueAction::Ref {
            id,
            kind,
            target,
            label,
        } => {
            let pm = open_pm()?;
            print_json(&write::add_ref(
                &pm,
                id,
                kind,
                target,
                label.as_deref(),
                None,
                None,
                None,
                "",
            )?);
            Ok(0)
        }
        IssueAction::Comment {
            id,
            text,
            file,
            author,
            kind,
        } => {
            let body = match (text, file) {
                (Some(t), _) => t.clone(),
                (None, Some(f)) => std::fs::read_to_string(f)?,
                (None, None) => {
                    if atty_stdin() {
                        return Err(Error::rejected("Provide -m or --file"));
                    }
                    let mut buf = String::new();
                    std::io::stdin().read_to_string(&mut buf)?;
                    buf
                }
            };
            let pm = open_pm()?;
            print_json(&write::add_comment(
                &pm,
                id,
                &body,
                author.as_deref(),
                kind.as_deref(),
                None,
                "",
            )?);
            Ok(0)
        }
        IssueAction::Attach { id, file } => {
            let pm = open_pm()?;
            print_json(&write::attach(&pm, id, file)?);
            Ok(0)
        }
        IssueAction::Lint { project } => {
            let pm = open_pm()?;
            let report = lint::run(&pm, project.as_deref())?;
            if report["ok"].as_bool() == Some(true) {
                print_json(&report);
                Ok(0)
            } else {
                eprintln!(
                    "{}",
                    serde_json::to_string_pretty(&report).unwrap_or_default()
                );
                Ok(1)
            }
        }
        IssueAction::Sync {
            no_push,
            dry_run,
            resolve,
        } => {
            let pm = open_pm()?;
            let side = resolve.as_deref().map(|s| match s {
                "ours" => sync::Resolve::Ours,
                _ => sync::Resolve::Theirs,
            });
            let report = sync::run(&pm, !no_push, *dry_run, side)?;
            if report["ok"].as_bool() == Some(true) {
                print_json(&report);
                Ok(0)
            } else {
                eprintln!(
                    "{}",
                    serde_json::to_string_pretty(&report).unwrap_or_default()
                );
                Ok(1)
            }
        }
    }
}

fn atty_stdin() -> bool {
    unsafe { libc::isatty(libc::STDIN_FILENO) == 1 }
}

/// Print rows as an aligned table — every column padded to its widest
/// cell except the last, which runs free.
fn print_table(rows: &[Vec<String>]) {
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
    let widths: Vec<usize> = (0..cols)
        .map(|i| {
            rows.iter()
                .filter_map(|r| r.get(i))
                .map(|c| c.chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect();
    for row in rows {
        let mut line = String::new();
        for (i, cell) in row.iter().enumerate() {
            if i + 1 == row.len() {
                line.push_str(cell);
            } else {
                let pad = widths[i] - cell.chars().count() + 2;
                line.push_str(cell);
                line.push_str(&" ".repeat(pad));
            }
        }
        println!("{}", line.trim_end());
    }
}

/// Compact human table for `issue ls` on a TTY.
fn print_ls_table(views: &[&board::View]) {
    if views.is_empty() {
        eprintln!("no issues — `cadence issue new \"title\"` creates one");
        return;
    }
    let mut rows = vec![["ID", "STATUS", "PRI", "FLAGS", "OWNER", "TAGS", "TITLE"]
        .map(str::to_string)
        .to_vec()];
    for v in views {
        let f = &v.issue.front;
        let mut flags = String::new();
        if v.blocked {
            flags.push('B');
        }
        if v.status_source != "file" {
            flags.push('~'); // derived
        }
        if v.container {
            flags.push('C');
        }
        rows.push(vec![
            f.id.clone(),
            v.status.clone(),
            f.priority.clone(),
            flags,
            f.owner.clone().unwrap_or_default(),
            f.tags.join(","),
            f.title.clone(),
        ]);
    }
    print_table(&rows);
    eprintln!(
        "{} issues · B=blocked ~=derived-status C=container",
        views.len()
    );
}

/// `issue epic ls` table — one row per epic from `board::epic_json`.
fn print_epics_table(epics: &[Value]) {
    if epics.is_empty() {
        eprintln!("no epics — `cadence issue new --epic <ID> \"title\"` gives an issue children");
        return;
    }
    let mut rows = vec![[
        "EPIC", "STATUS", "DONE", "OPEN", "DOING", "REVIEW", "BLOCKED", "OWNERS", "TITLE",
    ]
    .map(str::to_string)
    .to_vec()];
    for e in epics {
        let n = |k: &str| e["counts"][k].as_u64().unwrap_or(0);
        let live = e["total"].as_u64().unwrap_or(0) - n("dropped");
        let owners: Vec<&str> = e["owners"]
            .as_array()
            .map(|o| o.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        rows.push(vec![
            e["id"].as_str().unwrap_or_default().to_string(),
            e["status"].as_str().unwrap_or_default().to_string(),
            format!(
                "{}/{} {:.0}%",
                n("done"),
                live,
                e["done_ratio"].as_f64().unwrap_or(0.0) * 100.0
            ),
            (n("backlog") + n("ready")).to_string(),
            n("doing").to_string(),
            n("review").to_string(),
            e["blocked"].as_u64().unwrap_or(0).to_string(),
            owners.join(","),
            e["title"].as_str().unwrap_or_default().to_string(),
        ]);
    }
    print_table(&rows);
}

fn print_show(view: &board::View, by_id: &std::collections::HashMap<String, &board::View>) {
    let f = &view.issue.front;
    println!("{} — {}", f.id, f.title);
    println!(
        "status: {} (from {})  priority: {}  project: {}",
        view.status, view.status_source, f.priority, view.issue.project
    );
    if let Some(o) = &f.owner {
        println!("owner: {o}");
    }
    if let Some(c) = &f.component {
        println!("component: {c}");
    }
    if !f.tags.is_empty() {
        println!("tags: {}", f.tags.join(", "));
    }
    if view.ready {
        println!("ready: yes");
    }
    if view.blocked {
        println!("blocked: waits on {}", f.blocked_by.join(", "));
    }
    if let Some(p) = &f.parent {
        let title = by_id
            .get(p)
            .map(|v| v.issue.front.title.as_str())
            .unwrap_or("?");
        println!("parent: {p} — {title}");
    }
    if !view.children.is_empty() {
        println!("children: {}", view.children.join(", "));
    }
    if !view.blocks.is_empty() {
        println!("blocks: {}", view.blocks.join(", "));
    }
    if !f.relates.is_empty() {
        println!("relates: {}", f.relates.join(", "));
    }
    if let Some(d) = &f.duplicate_of {
        println!("duplicate_of: {d}");
    }
    for r in &f.refs {
        let target = r.url.as_deref().or(r.path.as_deref()).unwrap_or_default();
        let label = r.label.as_deref().unwrap_or(target);
        println!("ref {}: {} ({})", r.kind, label, target);
    }
    for (name, size) in &view.issue.artifacts {
        println!("artifact: {name} ({size} B)");
    }
    if view.checks_total > 0 {
        println!("checks: {}/{}", view.checks_done, view.checks_total);
    }
    for c in &view.issue.comments {
        println!("comment {} [{}]:", c.front.author, c.front.at);
        for line in c.body.trim().lines() {
            println!("    {line}");
        }
    }
    if !view.issue.body.trim().is_empty() {
        println!("\n{}", view.issue.body.trim());
    }
}
