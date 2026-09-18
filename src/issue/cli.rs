//! `cadence issue` — the only writer for the PM board. Every verb ends
//! in a single git commit inside the PM dir; reads need no daemon.

use std::io::Read;
use std::path::PathBuf;

use clap::Subcommand;
use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::issue::{board, doctor, hooks, lint, model, project, write, Pm};

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
    Ls {
        #[arg(long)]
        project: Option<String>,
        /// Filter by status (backlog ready doing review done dropped).
        #[arg(long)]
        status: Option<String>,
        /// Only computed-ready leaves.
        #[arg(long)]
        ready: bool,
        #[arg(long)]
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
    /// Set writable fields: `status priority owner component title`.
    Set {
        id: String,
        /// key=value pairs; empty value clears owner/component.
        pairs: Vec<String>,
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
        #[arg(short = 'm', conflicts_with = "file")]
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
        /// Default owner applied by `issue new`.
        #[arg(long)]
        owner: Option<String>,
    },
    /// List registered projects.
    Ls,
}

fn print_json(value: &Value) {
    println!(
        "{}",
        serde_json::to_string_pretty(value).unwrap_or_default()
    );
}

fn open_pm() -> Result<Pm> {
    Pm::open_default()
}

/// `cadence issue …` — returns the process exit code.
pub fn run(action: &IssueAction) -> Result<i32> {
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
                owner,
            } => {
                let pm = open_pm()?;
                let out =
                    write::project_add(&pm, key, prefix, repos, components, owner.as_deref())?;
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
                parent.as_deref(),
                blocked_by,
                owner.as_deref(),
                component.as_deref(),
                id.as_deref(),
                "",
            )?;
            print_json(&out);
            Ok(0)
        }
        IssueAction::Ls {
            project,
            status,
            ready,
            json: json_flag,
        } => {
            let pm = open_pm()?;
            let issues = board::load_all(&pm.dir, project.as_deref())?;
            let views = board::views(&pm.config.notes_dir(), issues);
            let mut views: Vec<&board::View> = views.iter().collect();
            if let Some(status) = status {
                model::check_status(status)?;
                views.retain(|v| v.status == *status);
            }
            if *ready {
                views.retain(|v| v.ready);
            }
            if *json_flag {
                print_json(&json!({
                    "issues": views.iter().map(|v| board::card_json(v)).collect::<Vec<_>>(),
                }));
            } else {
                print_ls_table(&views);
            }
            Ok(0)
        }
        IssueAction::Show { id, json } => {
            let pm = open_pm()?;
            model::check_id(id)?;
            let issues = board::load_all(&pm.dir, None)?;
            let views = board::views(&pm.config.notes_dir(), issues);
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
        IssueAction::Set { id, pairs } => {
            let pm = open_pm()?;
            print_json(&write::set_fields(&pm, id, pairs, "")?);
            Ok(0)
        }
        IssueAction::Link { id, kind, target } => {
            let pm = open_pm()?;
            print_json(&write::link(&pm, id, kind, target, false, None, "")?);
            Ok(0)
        }
        IssueAction::Unlink { id, kind, target } => {
            let pm = open_pm()?;
            print_json(&write::link(&pm, id, kind, target, true, None, "")?);
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
    }
}

fn atty_stdin() -> bool {
    unsafe { libc::isatty(libc::STDIN_FILENO) == 1 }
}

/// Compact human table for `issue ls` on a TTY.
fn print_ls_table(views: &[&board::View]) {
    if views.is_empty() {
        eprintln!("no issues — `cadence issue new \"title\"` creates one");
        return;
    }
    let mut rows: Vec<[String; 6]> = Vec::new();
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
        rows.push([
            f.id.clone(),
            v.status.clone(),
            f.priority.clone(),
            flags,
            f.owner.clone().unwrap_or_default(),
            f.title.clone(),
        ]);
    }
    let widths = |i: usize| rows.iter().map(|r| r[i].chars().count()).max().unwrap_or(0);
    for row in &rows {
        println!(
            "{:<w0$}  {:<w1$}  {:<w2$}  {:<w3$}  {:<w4$}  {}",
            row[0],
            row[1],
            row[2],
            row[3],
            row[4],
            row[5],
            w0 = widths(0),
            w1 = widths(1),
            w2 = widths(2),
            w3 = widths(3),
            w4 = widths(4),
        );
    }
    eprintln!(
        "{} issues · B=blocked ~=derived-status C=container",
        rows.len()
    );
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
