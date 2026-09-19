//! `cadence session start` / `cadence session end` — the SESSION.md
//! bookends as two verbs.
//!
//! `start` is the morning gate: host, binary, daemon, board, reconcile
//! and inbox checks print `ok|warn|fail` with a one-line remedy and the
//! process exits 0 (all ok), 1 (warnings) or 2 (failures) — the PM's
//! first command of a day is the same every time. Read-only by
//! default; `--fix` performs only the reversible fixes (`daemon start`,
//! `ui start`, `ui tailscale start` when sharing is persisted). It
//! never restarts a running daemon and never removes anything.
//!
//! `end` is the evening sweep: the merged-worktree finish (CAD-93's
//! `issue finish --merged` — feature-checked by parsing the argv so
//! the call appears the day it lands and reads "not available" until
//! then), `agent stop` for agents idle past `--idle-secs` with nothing
//! queued and no running message, `agent gc --older-than 1h`, a host
//! sweep (orphaned test processes are reported, never killed), and a
//! handoff note under `<state>/sessions/<date>-end.md`. `--dry-run`
//! prints the plan. It never stops a busy agent and never stops the
//! daemon while work is live.
//!
//! Every check is composed from the existing implementations — doctor,
//! `daemon_info`, `ui status`, overview's needs-me rows, the tracker
//! views — never re-computed here. Every subprocess goes through
//! [`crate::proc::run_bounded`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde_json::{json, Value};

use crate::adapter::registry;
use crate::client;
use crate::doctor;
use crate::error::{Error, Result};
use crate::issue::{self, board, project, time as itime};
use crate::overview;
use crate::proc::run_bounded;
use crate::ui;

const GIT_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Sev {
    Ok,
    Warn,
    Fail,
}

impl Sev {
    fn name(self) -> &'static str {
        match self {
            Sev::Ok => "ok",
            Sev::Warn => "warn",
            Sev::Fail => "fail",
        }
    }
}

/// One reported line: the check, its worst finding, and the fix.
struct Row {
    name: &'static str,
    sev: Sev,
    detail: String,
    remedy: Option<String>,
    /// What `--fix` did, when it acted.
    fixed: Option<String>,
    /// Sub-findings inside one named check (reconcile rows).
    items: Vec<String>,
}

impl Row {
    fn new(name: &'static str) -> Self {
        Self {
            name,
            sev: Sev::Ok,
            detail: String::new(),
            remedy: None,
            fixed: None,
            items: Vec::new(),
        }
    }

    fn ok(mut self, detail: impl Into<String>) -> Self {
        self.sev = Sev::Ok;
        self.detail = detail.into();
        self
    }

    fn warn(mut self, detail: impl Into<String>, remedy: impl Into<String>) -> Self {
        self.sev = Sev::Warn;
        self.detail = detail.into();
        let r = remedy.into();
        if !r.is_empty() {
            self.remedy = Some(r);
        }
        self
    }

    fn fail(mut self, detail: impl Into<String>, remedy: impl Into<String>) -> Self {
        self.sev = Sev::Fail;
        self.detail = detail.into();
        let r = remedy.into();
        if !r.is_empty() {
            self.remedy = Some(r);
        }
        self
    }

    fn json(&self) -> Value {
        json!({
            "name": self.name, "severity": self.sev.name(),
            "detail": self.detail, "remedy": self.remedy,
            "fixed": self.fixed, "items": self.items,
        })
    }
}

/// The host scan with the command's cwd — `Scan::host` defaults to
/// the process cwd, which is the same thing in practice but the
/// explicit override keeps the option field honest.
fn host_scan_for(state_dir: PathBuf, cwd: PathBuf) -> doctor::host::Scan {
    let mut scan = doctor::host::Scan::host(&state_dir);
    scan.cwd = cwd;
    scan
}

/// A `doctor --host` report as one Row: worst level wins, each non-ok
/// check becomes an item with its remedy.
fn host_row(name: &'static str, report: &Value) -> Row {
    let mut row = Row::new(name);
    let level = match report["level"].as_str() {
        Some("fail") => Sev::Fail,
        Some("warn") => Sev::Warn,
        _ => Sev::Ok,
    };
    let checks = report["checks"].as_array().cloned().unwrap_or_default();
    let mut worst_detail = String::new();
    let mut worst_remedy = None;
    for c in &checks {
        let cl = c["level"].as_str().unwrap_or("ok");
        if cl == "ok" {
            continue;
        }
        let cname = c["name"].as_str().unwrap_or("?");
        let detail = c["detail"].as_str().unwrap_or_default();
        let remedy = c["remedy"].as_str().unwrap_or_default();
        row.items.push(if remedy.is_empty() {
            format!("{cname}: {detail}")
        } else {
            format!("{cname}: {detail} — {remedy}")
        });
        if worst_detail.is_empty() || (cl == "fail" && level == Sev::Fail) {
            worst_detail = format!("{cname}: {detail}");
            worst_remedy = if remedy.is_empty() {
                None
            } else {
                Some(remedy.to_string())
            };
        }
    }
    row.sev = level;
    row.detail = if worst_detail.is_empty() {
        "host clean".to_string()
    } else {
        worst_detail
    };
    row.remedy = worst_remedy;
    row
}

fn print_row(r: &Row) {
    let mut line = format!("{:<9} {:<4} {}", r.name, r.sev.name(), r.detail);
    if let Some(rem) = &r.remedy {
        line.push_str(&format!(" — {rem}"));
    }
    println!("{line}");
    for i in &r.items {
        println!("           · {i}");
    }
    if let Some(f) = &r.fixed {
        println!("           fixed: {f}");
    }
}

// ---------- shared probes ----------

fn git(repo: &Path, args: &[&str]) -> Result<String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(repo).args(args);
    let out = run_bounded(&mut cmd, GIT_TIMEOUT).map_err(|e| {
        Error::internal(format!("git {} in {}: {e}", args.join(" "), repo.display()))
    })?;
    if !out.status.success() {
        return Err(Error::internal(format!(
            "git {} in {}: {}",
            args.join(" "),
            repo.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Agent rows plus each one's `agent_show` — the snapshot both verbs
/// read. Empty when the daemon is unreachable.
struct Fleet {
    reachable: bool,
    info: Option<Value>,
    agents: Vec<Value>,
    shows: HashMap<String, Value>,
}

fn fleet(state_dir: &Path) -> Fleet {
    let reachable = client::rpc(state_dir, "health", json!({})).is_ok();
    let info = client::rpc(state_dir, "daemon_info", json!({})).ok();
    let agents = client::rpc(state_dir, "agent_list", json!({}))
        .ok()
        .and_then(|v| v["agents"].as_array().cloned())
        .unwrap_or_default();
    let mut shows = HashMap::new();
    for a in &agents {
        if let Some(alias) = a["alias"].as_str() {
            if let Ok(show) = client::rpc(state_dir, "agent_show", json!({"alias": alias})) {
                shows.insert(alias.to_string(), show);
            }
        }
    }
    Fleet {
        reachable,
        info,
        agents,
        shows,
    }
}

/// The in-flight message for one agent — `running`/`submitting`/
/// `submitted` — as `(id, first-line, age_secs)`.
fn running_msg(show: &Value, now: i64) -> Option<(String, String, i64)> {
    show["messages"].as_array()?.iter().find_map(|m| {
        if !matches!(
            m["state"].as_str().unwrap_or_default(),
            "running" | "submitting" | "submitted"
        ) {
            return None;
        }
        let started = m["started"]
            .as_f64()
            .or(m["created"].as_f64())
            .unwrap_or(now as f64) as i64;
        let head = m["body"]
            .as_str()
            .unwrap_or_default()
            .lines()
            .next()
            .unwrap_or_default()
            .chars()
            .take(60)
            .collect::<String>();
        Some((
            m["id"].as_str().unwrap_or_default().to_string(),
            head,
            (now - started).max(0),
        ))
    })
}

/// Tracker projects and views for `scope` (`None` = all projects), plus
/// each project's locally-declared repo checkouts for the fs scans.
struct Scope {
    projects: Vec<project::Project>,
    views: Vec<board::View>,
    /// `(project key, github slug if the remote is github, local path)`.
    repos: Vec<(String, Option<String>, PathBuf)>,
    /// Issue id (uppercased) → status, for the worktree-name match.
    issue_status: HashMap<String, String>,
}

fn scope(project: Option<&str>) -> Scope {
    let mut out = Scope {
        projects: Vec::new(),
        views: Vec::new(),
        repos: Vec::new(),
        issue_status: HashMap::new(),
    };
    let Ok(pm) = issue::Pm::open_default() else {
        return out;
    };
    let all = project::list(&pm.dir).unwrap_or_default();
    let mut projects = all.clone();
    if let Some(want) = project {
        projects.retain(|p| p.key == want);
    }
    for p in &projects {
        for r in &p.repos {
            let Some(path) = r.path.as_deref().map(project::expand_home) else {
                continue;
            };
            let slug = r
                .remote
                .as_deref()
                .map(project::normalize_remote)
                .and_then(|n| n.strip_prefix("github.com/").map(str::to_string));
            out.repos.push((p.key.clone(), slug, path));
        }
    }
    let issues = board::load_all(&pm.dir, None).unwrap_or_default();
    let views = board::views(&pm.config.notes_dir(), issues);
    for v in views {
        if project.is_some() && v.issue.project != project.unwrap_or_default() {
            continue;
        }
        out.issue_status
            .insert(v.issue.front.id.to_uppercase(), v.status.clone());
        out.views.push(v);
    }
    // All projects — `build_repo_match` must find the build's repo
    // even when `--project` narrowed the checks above.
    out.projects = all;
    out
}

/// The `cadence/<wt-name>` open-PR branch names, lowercased, plus the
/// full per-slug `prs` payload for the handoff — one `gh` fetch shared
/// with the overview cache.
fn gh_open(state_dir: &Path, sc: &Scope) -> (Vec<String>, HashMap<String, Value>) {
    let mut slugs: Vec<String> = sc.repos.iter().filter_map(|(_, s, _)| s.clone()).collect();
    slugs.sort();
    slugs.dedup();
    let (repos, _state) = overview::github_repos(state_dir, &slugs);
    let mut branches = Vec::new();
    for data in repos.values() {
        for pr in data["prs"].as_array().cloned().unwrap_or_default() {
            if let Some(h) = pr["headRefName"].as_str() {
                branches.push(h.to_lowercase());
            }
        }
    }
    (branches, repos)
}

/// `.cadence/wt/*` dirs under each repo checkout, skipping tool-owned
/// review trees (the `.cadence-review-tree` marker `cadence review`
/// leaves) and names without a `<prefix>-<n>-` issue stem.
fn worktree_dirs(repos: &[(String, Option<String>, PathBuf)]) -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    for (_, _, root) in repos {
        let wt = root.join(".cadence").join("wt");
        let Ok(entries) = std::fs::read_dir(&wt) else {
            continue;
        };
        for e in entries.flatten() {
            if !e.path().is_dir() {
                continue;
            }
            if e.path().join(".cadence-review-tree").is_file() {
                continue;
            }
            if let Some(name) = e.file_name().to_str() {
                out.push((root.clone(), name.to_string()));
            }
        }
    }
    out
}

/// `<prefix>-<num>` issue id out of a `cad-70-review-verb` worktree
/// name — `cadence/<name>` branches and `.cadence/wt/<name>` dirs share
/// the stem.
fn issue_stem(name: &str) -> Option<String> {
    let mut parts = name.splitn(3, '-');
    let prefix = parts.next()?;
    let num: u64 = parts.next()?.parse().ok()?;
    if prefix.is_empty() || !prefix.chars().all(|c| c.is_ascii_lowercase()) {
        return None;
    }
    Some(format!("{}-{}", prefix.to_uppercase(), num))
}

// ---------- session start ----------

pub struct StartOptions {
    pub project: Option<String>,
    pub json: bool,
    pub fix: bool,
    pub cwd: PathBuf,
    pub state_dir: PathBuf,
}

pub fn run_start(opts: &StartOptions) -> Result<i32> {
    let mut rows: Vec<Row> = Vec::new();
    let mut fixes: Vec<String> = Vec::new();

    // ---- host: the CAD-72 watchdog — disk, WAL, pipes, orphans,
    // temp dirs, stale worktrees in one read-only scan ----
    let sc = scope(opts.project.as_deref());
    let host_scan = doctor::host::run(&host_scan_for(opts.state_dir.clone(), opts.cwd.clone()));
    let host = host_row("host", &host_scan);
    rows.push(host);

    // ---- binary vs main ----
    let mut bin = Row::new("binary");
    let repo_for_binary = match &opts.project {
        Some(_) => sc.repos.first().map(|(_, _, p)| p.clone()),
        None => overview::build_repo_match_pub(&sc.projects).map(|(_, r)| r),
    };
    match repo_for_binary {
        Some(repo) => {
            let d = overview::drift_of(&repo, overview::BUILD_COMMIT);
            if d["known"].as_bool() == Some(true) {
                let n = d["count"].as_i64().unwrap_or(0);
                bin = if n > 0 {
                    bin.warn(
                        format!(
                            "binary is {n} commit(s) behind {}",
                            d["ref"].as_str().unwrap_or("main")
                        ),
                        "git pull && cargo build --release --features ui",
                    )
                } else {
                    bin.ok(format!(
                        "at {}",
                        &overview::BUILD_COMMIT[..10.min(overview::BUILD_COMMIT.len())]
                    ))
                };
            } else {
                bin = bin.ok(d["reason"].as_str().unwrap_or("cannot tell").to_string());
            }
        }
        None => bin = bin.ok("no repo matches this build — drift unknown"),
    }
    rows.push(bin);

    // ---- daemon ----
    let mut fl = fleet(&opts.state_dir);
    if !fl.reachable && opts.fix {
        match client::daemon_start(&opts.state_dir) {
            Ok(v) => {
                fixes.push(format!(
                    "daemon start → {}",
                    v["state"].as_str().unwrap_or("?")
                ));
                fl = fleet(&opts.state_dir);
            }
            Err(e) => fixes.push(format!("daemon start failed: {e}")),
        }
    }
    let mut drow = Row::new("daemon");
    if !fl.reachable {
        drow = drow.fail("unreachable", "cadence daemon start");
    } else {
        match &fl.info {
            Some(info) => {
                let dc = info["build_commit"].as_str().unwrap_or("unknown");
                if dc != "unknown" && dc != overview::BUILD_COMMIT {
                    drow = drow.warn(
                        format!("daemon build {dc} != binary {}", overview::BUILD_COMMIT),
                        "daemon behind — run `cadence daemon restart`",
                    );
                } else {
                    let up = itime::now_epoch() - info["started_at"].as_i64().unwrap_or(0);
                    drow = drow.ok(format!("up {}m, {} agents", up / 60, fl.agents.len()));
                }
            }
            None => {
                drow = drow.warn(
                    "daemon predates daemon_info — build unreadable",
                    "cadence daemon restart",
                );
            }
        }
    }
    if let Some(f) = fixes.last().filter(|f| f.starts_with("daemon")) {
        drow.fixed = Some(f.clone());
    }
    rows.push(drow);

    // ---- board ----
    let mut board = Row::new("board");
    let ui_opts = ui::persisted_opts(&opts.state_dir);
    let port = ui_opts.port.unwrap_or(3010);
    if ui::detached_pid(&opts.state_dir).is_none() && opts.fix {
        match ui::run_cli(
            &opts.state_dir,
            &ui::UiAction::Start {
                flags: ui::UiFlags::default(),
                reset: false,
            },
        ) {
            Ok(_) => {
                fixes.push("ui start".to_string());
                board.fixed = Some("ui start".to_string());
            }
            Err(e) => fixes.push(format!("ui start failed: {e}")),
        }
    }
    match ui::detached_pid(&opts.state_dir) {
        None => {
            board = if board.fixed.is_some() {
                board.warn("ui start ran but no pid yet", "cadence ui status")
            } else {
                board.warn("board down", "cadence ui start")
            };
        }
        Some(pid) => {
            let mut detail = format!("pid {pid}, :{port}");
            if ui::health(&opts.state_dir).is_none() {
                detail.push_str(", health probe failed");
            }
            if let Some(ts) = &ui_opts.tailscale {
                let live = ui::serve_has_target(&ts.target).unwrap_or(false);
                if live {
                    detail.push_str(&format!(", shared {}", ts.url()));
                } else if opts.fix {
                    match ui::run_cli(
                        &opts.state_dir,
                        &ui::UiAction::Tailscale {
                            action: ui::TailscaleAction::Start {
                                port: ts.https_port,
                                read_only: ui_opts.read_only,
                            },
                        },
                    ) {
                        Ok(_) => {
                            board.fixed = Some(format!("ui tailscale start → {}", ts.url()));
                            detail.push_str(&format!(", shared {}", ts.url()));
                        }
                        Err(e) => detail.push_str(&format!(", tailscale fix failed: {e}")),
                    }
                } else {
                    detail.push_str(", tailscale mapping not live");
                }
            }
            let unhealthy = ui::health(&opts.state_dir).is_none();
            let ts_missing = ui_opts.tailscale.is_some()
                && board.fixed.is_none()
                && !ui::serve_has_target(&ui_opts.tailscale.as_ref().unwrap().target)
                    .unwrap_or(false);
            board = if unhealthy {
                board.warn(detail, "cadence ui status")
            } else if ts_missing {
                board.warn(detail, "cadence ui tailscale start")
            } else {
                board.ok(detail)
            };
        }
    }
    rows.push(board);

    // ---- reconcile + inbox ----
    let mut recon = Row::new("reconcile");
    let mut inbox = Row::new("inbox");
    let mut inbox_warns = Vec::new();
    if !fl.reachable {
        recon = recon.fail(
            "cannot inspect agents — daemon unreachable",
            "cadence daemon start",
        );
        inbox = inbox.fail("cannot read mailboxes — daemon unreachable", "");
    } else {
        // Overview computes the shared needs-me rows once (gh cache
        // included); inbox rows split out into their own check.
        let pm_dir = issue::default_dir().unwrap_or_default();
        let view = overview::overview(&opts.state_dir, &pm_dir);
        let needs = view["needs_me"].as_array().cloned().unwrap_or_default();
        // Hard failures: a fenced agent and an unknown message both
        // mean a turn's outcome is unaccounted for — the gate refuses
        // go until a human reconciles them. Everything else warns.
        let mut recon_fail = false;
        for n in &needs {
            let kind = n["kind"].as_str().unwrap_or_default();
            let line = format!(
                "{} — {}",
                n["title"].as_str().unwrap_or_default(),
                n["command"].as_str().unwrap_or_default()
            );
            if kind == "inbox_unread" {
                inbox_warns.push(line);
            } else {
                if kind == "fenced" {
                    recon_fail = true;
                }
                recon.items.push(line);
            }
        }
        // Unknown messages — a fence the overview rows don't name.
        for a in &fl.agents {
            let alias = a["alias"].as_str().unwrap_or_default();
            let show = fl.shows.get(alias).cloned().unwrap_or_default();
            let mut named = false;
            for m in show["messages"].as_array().cloned().unwrap_or_default() {
                if m["state"].as_str() == Some("unknown") {
                    named = true;
                    recon_fail = true;
                    let head = m["body"]
                        .as_str()
                        .unwrap_or_default()
                        .lines()
                        .next()
                        .unwrap_or_default()
                        .chars()
                        .take(50)
                        .collect::<String>();
                    recon.items.push(format!(
                        "agent {alias}: unknown message {} ({head}) — cadence message reconcile {} --status interrupted",
                        m["id"].as_str().unwrap_or("?"),
                        m["id"].as_str().unwrap_or("?")
                    ));
                }
            }
            if !named && show["unknown"].as_i64().unwrap_or(0) > 0 {
                recon_fail = true;
                recon.items.push(format!(
                    "agent {alias}: unknown message(s) — cadence agent show {alias}"
                ));
            }
        }
        // `doing` issues with no live owner.
        for v in &sc.views {
            if v.status != "doing" {
                continue;
            }
            let id = &v.issue.front.id;
            match v.issue.front.owner.as_deref() {
                None => recon.items.push(format!(
                    "{id} doing with no owner — cadence issue set {id} owner=<alias>"
                )),
                Some(owner) => {
                    let live = fl.agents.iter().any(|a| {
                        a["alias"].as_str() == Some(owner)
                            && !a["dead"].as_bool().unwrap_or(false)
                            && !matches!(
                                a["state"].as_str().unwrap_or_default(),
                                "stopped" | "stopping" | "offline"
                            )
                    });
                    if !live {
                        recon.items.push(format!(
                            "{id} doing but owner {owner} is not live — resume or reassign"
                        ));
                    }
                }
            }
        }
        // Open PR on a cadence/<wt> branch whose local worktree is gone
        // (only checked where the PR's repo has a declared checkout —
        // the worktree may legitimately live on another host).
        let (pr_branches, gh_repos) = gh_open(&opts.state_dir, &sc);
        for (slug, data) in &gh_repos {
            let Some((_, _, root)) = sc
                .repos
                .iter()
                .find(|(_, s, _)| s.as_deref() == Some(slug.as_str()))
            else {
                continue;
            };
            for pr in data["prs"].as_array().cloned().unwrap_or_default() {
                let Some(head) = pr["headRefName"].as_str() else {
                    continue;
                };
                let Some(name) = head.strip_prefix("cadence/") else {
                    continue;
                };
                if !root.join(".cadence").join("wt").join(name).exists() {
                    recon.items.push(format!(
                        "PR #{} branch {head} — no .cadence/wt/{name} locally",
                        pr["number"].as_i64().unwrap_or(0)
                    ));
                }
            }
        }
        // `.cadence/wt/*` with neither an open PR nor an open issue.
        for (root, name) in worktree_dirs(&sc.repos) {
            let branch = format!("cadence/{name}");
            let has_pr = pr_branches.contains(&branch);
            let open_issue = issue_stem(&name)
                .and_then(|id| sc.issue_status.get(&id))
                .is_some_and(|s| !matches!(s.as_str(), "done" | "dropped"));
            if !has_pr && !open_issue {
                recon.items.push(format!(
                    "orphan worktree {}/.cadence/wt/{name} — no open PR or issue; \
                     inspect, then `git -C {} worktree remove .cadence/wt/{name}`",
                    root.display(),
                    root.display()
                ));
            }
        }
        if !recon.items.is_empty() {
            recon.sev = if recon_fail { Sev::Fail } else { Sev::Warn };
            recon.detail = format!("{} item(s)", recon.items.len());
        } else {
            recon = recon.ok("nothing stale");
        }
        if inbox_warns.is_empty() {
            inbox = inbox.ok("no unread");
        } else {
            inbox = Row {
                name: "inbox",
                sev: Sev::Warn,
                detail: format!("{} mailbox(es)", inbox_warns.len()),
                remedy: None,
                fixed: None,
                items: inbox_warns,
            };
        }
    }
    rows.push(recon);
    rows.push(inbox);

    finish_start(rows, opts.json)
}

fn finish_start(rows: Vec<Row>, json_out: bool) -> Result<i32> {
    let worst = rows.iter().map(|r| r.sev).max().unwrap_or(Sev::Ok);
    let exit = match worst {
        Sev::Ok => 0,
        Sev::Warn => 1,
        Sev::Fail => 2,
    };
    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "kind": "session-start",
                "go": worst != Sev::Fail,
                "checks": rows.iter().map(|r| r.json()).collect::<Vec<_>>(),
            }))
            .unwrap_or_default()
        );
    } else {
        let label = match worst {
            Sev::Ok => "GO",
            Sev::Warn => "GO (with warnings)",
            Sev::Fail => "NO-GO",
        };
        println!("session start — {label}");
        for r in &rows {
            print_row(r);
        }
    }
    Ok(exit)
}

// ---------- session end ----------

pub struct EndOptions {
    pub project: Option<String>,
    pub json: bool,
    pub dry_run: bool,
    pub force_finish: bool,
    pub idle_secs: u64,
    pub cwd: PathBuf,
    pub state_dir: PathBuf,
    /// `issue finish --merged` parsed in main.rs when this build's CLI
    /// accepts it — `None` means the sweep isn't on this binary yet.
    pub merged_finish: Option<crate::issue::cli::IssueAction>,
}

pub fn run_end(opts: &EndOptions) -> Result<i32> {
    let now = itime::now_epoch();
    let sc = scope(opts.project.as_deref());
    let fl = fleet(&opts.state_dir);
    let mut rows: Vec<Row> = Vec::new();
    let mut done = EndActions::default();
    let mut failures = 0u32;

    // ---- merged-worktree sweep ----
    let candidates = merged_worktree_candidates(&sc.repos);
    let mut sweep = Row::new("finish");
    match &opts.merged_finish {
        None => {
            sweep = sweep.ok(format!(
                "issue finish --merged not available — {} merged candidate(s) listed",
                candidates.len()
            ));
            for c in &candidates {
                sweep.items.push(format!(
                    "{} ({}) — would finish",
                    c["worktree"].as_str().unwrap_or("?"),
                    c["branch"].as_str().unwrap_or("?"),
                ));
            }
        }
        Some(action) => {
            if opts.dry_run {
                sweep.detail = format!(
                    "would run `cadence issue finish --merged{}` — {} candidate(s)",
                    if opts.force_finish { " --force" } else { "" },
                    candidates.len()
                );
                for c in &candidates {
                    sweep.items.push(format!(
                        "{} ({})",
                        c["worktree"].as_str().unwrap_or("?"),
                        c["branch"].as_str().unwrap_or("?")
                    ));
                }
            } else {
                match issue::cli::run(action, &opts.state_dir) {
                    Ok(0) => {
                        sweep = sweep.ok(format!("{} candidate(s)", candidates.len()));
                        done.finished = candidates
                            .iter()
                            .filter_map(|c| c["worktree"].as_str().map(str::to_string))
                            .collect();
                    }
                    Ok(code) => {
                        sweep = sweep.fail(format!("issue finish --merged exited {code}"), "");
                        failures += 1;
                    }
                    Err(e) => {
                        sweep = sweep.fail(format!("issue finish --merged failed: {e}"), "");
                        failures += 1;
                    }
                }
            }
        }
    }
    rows.push(sweep);

    // ---- idle agents ----
    let mut idle_row = Row::new("agents");
    let mut stop_candidates: Vec<String> = Vec::new();
    let mut busy = 0u32;
    if fl.reachable {
        for a in &fl.agents {
            let alias = a["alias"].as_str().unwrap_or_default();
            let provider = a["provider"].as_str().unwrap_or_default();
            let kind = a["endpoint_kind"].as_str().unwrap_or_default();
            if !registry::has_actor(provider, kind) {
                continue;
            }
            let show = fl.shows.get(alias).cloned().unwrap_or_default();
            let queued = show["queued"].as_i64().unwrap_or(0);
            let running = running_msg(&show, now);
            if running.is_some() {
                busy += 1;
                continue;
            }
            if a["state"].as_str() != Some("idle")
                || a["dead"].as_bool().unwrap_or(false)
                || queued > 0
            {
                continue;
            }
            let updated = a["updated"].as_f64().unwrap_or(now as f64) as i64;
            if now - updated < opts.idle_secs as i64 {
                continue;
            }
            // Never stop a busy pane: a live pty endpoint must probe idle.
            if kind == "pty" && a["endpoint"].is_string() {
                let idle = client::rpc(&opts.state_dir, "agent_probe", json!({"alias": alias}))
                    .map(|p| p["idle"].as_bool().unwrap_or(false))
                    .unwrap_or(false);
                if !idle {
                    busy += 1;
                    continue;
                }
            }
            stop_candidates.push(alias.to_string());
        }
    }
    if !fl.reachable {
        idle_row = idle_row.warn("daemon unreachable — nothing stopped", "");
    } else if stop_candidates.is_empty() {
        idle_row = idle_row.ok(format!(
            "nothing idle past {}s ({busy} busy)",
            opts.idle_secs
        ));
    } else {
        idle_row = Row {
            name: "agents",
            sev: Sev::Ok,
            detail: format!(
                "{} agent(s) idle > {}s{}",
                stop_candidates.len(),
                opts.idle_secs,
                if opts.dry_run { " (dry-run)" } else { "" }
            ),
            remedy: None,
            fixed: None,
            items: stop_candidates.clone(),
        };
        if !opts.dry_run {
            for alias in &stop_candidates {
                match client::rpc(&opts.state_dir, "agent_stop", json!({"alias": alias})) {
                    Ok(_) => done.stopped.push(alias.clone()),
                    Err(e) => {
                        idle_row.items.push(format!("stop {alias} failed: {e}"));
                        failures += 1;
                    }
                }
            }
        }
    }
    rows.push(idle_row);

    // ---- agent gc ----
    let mut gc_row = Row::new("gc");
    if !fl.reachable {
        gc_row = gc_row.ok("skipped — daemon unreachable");
    } else if opts.dry_run {
        let cands: Vec<String> = fl
            .agents
            .iter()
            .filter(|a| {
                a["endpoint"].is_null()
                    && matches!(a["state"].as_str(), Some("attention" | "stopped"))
                    && now - a["updated"].as_f64().unwrap_or(now as f64) as i64 > 3600
            })
            .filter_map(|a| a["alias"].as_str().map(str::to_string))
            .collect();
        gc_row = gc_row.ok(format!("would sweep {} dead agent(s)", cands.len()));
        gc_row.items = cands;
    } else {
        match client::rpc(&opts.state_dir, "agent_gc", json!({"older_than": 3600})) {
            Ok(v) => {
                done.gc_removed = v["removed"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|a| a.as_str().map(str::to_string))
                    .collect();
                gc_row = gc_row.ok(format!("removed {}", done.gc_removed.len()));
            }
            Err(e) => {
                gc_row = gc_row.fail(format!("agent gc failed: {e}"), "");
                failures += 1;
            }
        }
    }
    rows.push(gc_row);

    // ---- host sweep: the CAD-72 watchdog again — orphans are
    // reported, never killed; disk state rides along ----
    let host_scan = doctor::host::run(&host_scan_for(opts.state_dir.clone(), opts.cwd.clone()));
    let mut sweep_row = host_row("sweep", &host_scan);
    if sweep_row.detail == "host clean" {
        sweep_row.detail = "clean".to_string();
    }
    // Orphan pids deserve their own lines — the checklist's "five hung
    // test binaries" are named, not counted.
    for c in host_scan["checks"].as_array().cloned().unwrap_or_default() {
        if c["name"].as_str() != Some("orphans") {
            continue;
        }
        for o in c["value"]["pids"].as_array().cloned().unwrap_or_default() {
            sweep_row.items.push(format!(
                "orphan pid {} — {} ({})",
                o["pid"],
                o["head"].as_str().unwrap_or_default(),
                o["reasons"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|r| r.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    rows.push(sweep_row);

    // ---- handoff ----
    let (_, gh_repos) = gh_open(&opts.state_dir, &sc);
    let handoff_path = match write_handoff(opts, &fl, &sc, &gh_repos, &done, now) {
        Ok(p) => p,
        Err(e) => {
            rows.push(Row::new("handoff").fail(format!("{e}"), ""));
            failures += 1;
            PathBuf::new()
        }
    };
    if !handoff_path.as_os_str().is_empty() {
        rows.push(Row::new("handoff").ok(handoff_path.display().to_string()));
    }

    let worst = rows.iter().map(|r| r.sev).max().unwrap_or(Sev::Ok);
    let mut exit = match worst {
        Sev::Ok => 0,
        Sev::Warn => 1,
        Sev::Fail => 2,
    };
    if failures > 0 {
        exit = 2;
    }
    if opts.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "kind": "session-end",
                "dry_run": opts.dry_run,
                "steps": rows.iter().map(|r| r.json()).collect::<Vec<_>>(),
                "stop_candidates": stop_candidates,
                "stopped": done.stopped,
                "gc_removed": done.gc_removed,
                "handoff": handoff_path,
            }))
            .unwrap_or_default()
        );
    } else {
        println!(
            "session end{}",
            if opts.dry_run { " — dry run" } else { "" }
        );
        for r in &rows {
            print_row(r);
        }
    }
    Ok(exit)
}

/// `.cadence/wt/*` entries whose branch is already merged into the
/// repo's default ref — the read-only half of the finish sweep. The
/// mutation itself is `issue finish --merged`; this list only plans.
fn merged_worktree_candidates(repos: &[(String, Option<String>, PathBuf)]) -> Vec<Value> {
    let mut out = Vec::new();
    for (_, _, root) in repos {
        let Ok(list) = git(root, &["worktree", "list", "--porcelain"]) else {
            continue;
        };
        let default = git(
            root,
            &["symbolic-ref", "refs/remotes/origin/HEAD", "--short"],
        )
        .ok()
        .or_else(|| git(root, &["symbolic-ref", "--short", "HEAD"]).ok());
        let Some(default) = default else { continue };
        let mut cur_path = String::new();
        let mut cur_branch = String::new();
        for line in list.lines().chain(std::iter::once("")) {
            if line.is_empty() {
                let name = Path::new(&cur_path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or_default()
                    .to_string();
                if cur_path.contains("/.cadence/wt/")
                    && !cur_branch.is_empty()
                    && !Path::new(&cur_path).join(".cadence-review-tree").is_file()
                {
                    let merged = git(
                        root,
                        &["merge-base", "--is-ancestor", &cur_branch, &default],
                    )
                    .is_ok()
                    .then_some("ancestry")
                    .or_else(|| {
                        git(root, &["cherry", &default, &cur_branch])
                            .ok()
                            .filter(|m| !m.lines().any(|l| l.starts_with('+')))
                            .map(|_| "cherry")
                    });
                    if let Some(how) = merged {
                        out.push(json!({
                            "repo": root, "worktree": cur_path, "branch": cur_branch,
                            "merged_by": how,
                            "issue": issue_stem(&name),
                        }));
                    }
                }
                cur_path.clear();
                cur_branch.clear();
                continue;
            }
            if let Some(p) = line.strip_prefix("worktree ") {
                cur_path = p.to_string();
            } else if let Some(b) = line.strip_prefix("branch refs/heads/") {
                cur_branch = b.to_string();
            }
        }
    }
    out
}

/// What `session end` applied — reported on screen and in the handoff.
#[derive(Default)]
struct EndActions {
    stopped: Vec<String>,
    gc_removed: Vec<String>,
    finished: Vec<String>,
}

/// The end-of-day note: open PRs with head + verdict, live turns,
/// queued work, issues in review, and what the next session does first.
fn write_handoff(
    opts: &EndOptions,
    fl: &Fleet,
    sc: &Scope,
    gh_repos: &HashMap<String, Value>,
    done: &EndActions,
    now: i64,
) -> Result<PathBuf> {
    let (y, mo, d, h, mi, s) = itime::utc_parts(now);
    let mut md = format!("# session end — {y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z\n\n");

    md.push_str("## open PRs\n");
    let mut any_pr = false;
    for (slug, data) in gh_repos {
        for pr in data["prs"].as_array().cloned().unwrap_or_default() {
            any_pr = true;
            let rollup = pr["statusCheckRollup"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            let verdict = overview::verdict_state_pub(&rollup).unwrap_or_else(|| "none".into());
            let checks = if overview::checks_green_pub(&rollup) {
                "green"
            } else {
                "not-green"
            };
            md.push_str(&format!(
                "- {slug}#{} {} — head {}, verdict {}, checks {}\n",
                pr["number"].as_i64().unwrap_or(0),
                pr["title"].as_str().unwrap_or(""),
                pr["headRefOid"]
                    .as_str()
                    .unwrap_or("?")
                    .chars()
                    .take(10)
                    .collect::<String>(),
                verdict,
                checks,
            ));
        }
    }
    if !any_pr {
        md.push_str("- none\n");
    }

    md.push_str("\n## running turns\n");
    let mut any_run = false;
    let mut queued_lines = String::new();
    for a in &fl.agents {
        let alias = a["alias"].as_str().unwrap_or_default();
        let show = fl.shows.get(alias).cloned().unwrap_or_default();
        if let Some((id, head, age)) = running_msg(&show, now) {
            any_run = true;
            md.push_str(&format!("- {alias}: {id} ({age}s) {head}\n"));
        }
        let queued = show["queued"].as_i64().unwrap_or(0);
        if queued > 0 {
            queued_lines.push_str(&format!("- {alias}: {queued} queued\n"));
        }
    }
    if !any_run {
        md.push_str("- none\n");
    }
    md.push_str("\n## queued kickoffs\n");
    md.push_str(if queued_lines.is_empty() {
        "- none\n"
    } else {
        &queued_lines
    });

    md.push_str("\n## issues in review\n");
    let review: Vec<&str> = sc
        .views
        .iter()
        .filter(|v| v.status == "review")
        .map(|v| v.issue.front.id.as_str())
        .collect();
    if review.is_empty() {
        md.push_str("- none\n");
    } else {
        for id in review {
            md.push_str(&format!("- {id}\n"));
        }
    }

    md.push_str("\n## done this run\n");
    md.push_str(&format!(
        "- stopped: {}\n",
        if done.stopped.is_empty() {
            "none".into()
        } else {
            done.stopped.join(", ")
        }
    ));
    md.push_str(&format!(
        "- gc removed: {}\n",
        if done.gc_removed.is_empty() {
            "none".into()
        } else {
            done.gc_removed.join(", ")
        }
    ));
    md.push_str(&format!(
        "- worktrees finished: {}\n",
        if done.finished.is_empty() {
            "none".into()
        } else {
            done.finished.join(", ")
        }
    ));
    if opts.force_finish {
        md.push_str("- finish ran with --force (recorded)\n");
    }
    if opts.dry_run {
        md.push_str("- dry run — nothing above was applied\n");
    }

    md.push_str("\n## next session first\n");
    let pm_dir = issue::default_dir().unwrap_or_default();
    let view = overview::overview(&opts.state_dir, &pm_dir);
    let needs = view["needs_me"].as_array().cloned().unwrap_or_default();
    if needs.is_empty() {
        md.push_str("- nothing queued on a human\n");
    } else {
        for n in needs.iter().take(5) {
            md.push_str(&format!(
                "- {} — {}\n",
                n["title"].as_str().unwrap_or(""),
                n["command"].as_str().unwrap_or("")
            ));
        }
    }
    let live = fl
        .agents
        .iter()
        .filter(|a| !a["dead"].as_bool().unwrap_or(false))
        .count();
    md.push_str(&format!(
        "\n## fleet\n- daemon {}: {} agent(s) live\n",
        if fl.reachable {
            "reachable"
        } else {
            "unreachable"
        },
        live
    ));
    if fl.reachable && live == 0 {
        md.push_str("- no live agents — `cadence daemon stop` is safe\n");
    } else if fl.reachable {
        md.push_str("- agents live — daemon left running\n");
    }

    let dir = opts.state_dir.join("sessions");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{y:04}{mo:02}{d:02}-end.md"));
    std::fs::write(&path, &md)?;
    Ok(path)
}
