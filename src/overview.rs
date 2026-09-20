//! The Overview slice — `GET /api/overview` and `cadence overview`.
//!
//! One derived answer to "what is waiting on a human or the PM right
//! now, and with which command" plus "is what we merged actually
//! running". Nothing is stored: agents and approvals come from daemon
//! RPCs, review/blocked/project rows from the tracker, PR and CI state
//! from `gh` behind a 60 s cache in the state dir, deploy drift from
//! git walks bounded through `proc::run_bounded`. Every source degrades
//! — an unreachable daemon, a missing tracker, or a failing `gh`
//! narrows the screen instead of failing it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde_json::{json, Value};

use crate::adapter::registry;
use crate::client;
use crate::issue::{self, board, project};
use crate::proc::run_bounded;

/// Git identity baked in by build.rs — `unknown` when git or a repo
/// was absent at build time, which every consumer reads as "cannot
/// tell", never as zero.
pub const BUILD_COMMIT: &str = env!("CADENCE_BUILD_COMMIT");
pub const BUILD_TIME: &str = env!("CADENCE_BUILD_TIME");
pub const BUILD_REMOTE: &str = env!("CADENCE_BUILD_REMOTE");
pub const BUILD_ROOT: &str = env!("CADENCE_BUILD_ROOT");

/// `gh` results are cached this long in the state dir — the board and
/// the CLI share one cache, and GitHub is the only source allowed to
/// be a network call.
const GH_CACHE_SECS: i64 = 60;
const GH_TIMEOUT: Duration = Duration::from_secs(20);
const GIT_TIMEOUT: Duration = Duration::from_secs(15);
/// `git log` subjects surfaced in the drift tile.
const DRIFT_SUBJECTS: usize = 20;

fn now_epoch() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// `YYYY-MM-DDTHH:MM:SSZ` → epoch — the shape `issue::time::iso`
/// writes and the shape `gh` returns for `updatedAt`.
fn parse_iso(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() != 20 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[19] != b'Z' {
        return None;
    }
    let num = |from: usize, to: usize| -> Option<i64> { s.get(from..to)?.parse().ok() };
    let (y, mo, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (h, mi, sec) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    // Days-from-civil (Howard Hinnant).
    let y = if mo <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(days * 86400 + h * 3600 + mi * 60 + sec)
}

/// `(#123)` at the end of a squash-merge subject → the PR number.
fn pr_number(subject: &str) -> Option<u64> {
    let tail = subject.trim_end();
    let inner = tail.strip_suffix(')')?.rsplit("(#").next()?;
    let n: u64 = inner.parse().ok()?;
    Some(n)
}

/// Every rollup entry other than the `qa-verdict` gate must be a pass:
/// CheckRun → COMPLETED + SUCCESS/SKIPPED/NEUTRAL, StatusContext →
/// SUCCESS. An empty rollup means no CI configured — treated green;
/// the human gate is the verdict.
fn checks_green(rollup: &[Value]) -> bool {
    rollup.iter().all(|e| {
        let name = e["context"].as_str().or(e["name"].as_str()).unwrap_or("");
        if name.eq_ignore_ascii_case("qa-verdict") {
            return true;
        }
        if e["__typename"].as_str() == Some("CheckRun") || e["conclusion"].is_string() {
            let done = e["status"].as_str() == Some("COMPLETED") || e["status"].is_null();
            let ok = matches!(
                e["conclusion"].as_str(),
                Some("SUCCESS" | "SKIPPED" | "NEUTRAL")
            );
            return done && ok;
        }
        e["state"].as_str() == Some("SUCCESS")
    })
}

/// The `qa-verdict` commit status on the rollup — "SUCCESS", "FAILURE",
/// "PENDING", or `None` when nobody posted one.
fn verdict_state(rollup: &[Value]) -> Option<String> {
    for e in rollup {
        let name = e["context"].as_str().or(e["name"].as_str()).unwrap_or("");
        if name.eq_ignore_ascii_case("qa-verdict") {
            return e["state"]
                .as_str()
                .or(e["conclusion"].as_str())
                .map(str::to_string);
        }
    }
    None
}

/// Command templates for needs-me rows — every `cadence …` shape the
/// screen can emit. The Cli-parse test in main.rs runs each through
/// `Cli::try_parse_from`, so a row can never carry a command the CLI
/// rejects.
pub fn cmd_agent_unfence(alias: &str) -> String {
    format!("cadence agent unfence {alias}")
}

pub fn cmd_agent_show(alias: &str) -> String {
    format!("cadence agent show {alias}")
}

pub fn cmd_inbox(alias: &str) -> String {
    format!("cadence inbox {alias}")
}

/// The respond command for a pending request, by request method:
/// provider input requests want an answers file, the approval shapes
/// (provider `*requestApproval`, `session/request_permission`, and
/// brokered `cadence/*`) take a decision, and anything else gets the
/// inspect command — the daemon rejects a respond it cannot map.
pub fn cmd_agent_respond(alias: &str, handle: &str, method: &str) -> String {
    if method == "item/tool/requestUserInput" {
        format!(
            "cadence agent respond {alias} --request {handle} --answers-file answers-{handle}.json"
        )
    } else if method.starts_with("cadence/")
        || method.ends_with("requestApproval")
        || method == "session/request_permission"
    {
        format!("cadence agent respond {alias} --request {handle} --decision accept")
    } else {
        format!("cadence agent requests {alias}")
    }
}

pub fn cmd_issue_show(id: &str) -> String {
    format!("cadence issue show {id}")
}

pub fn cmd_issue_set_ready(id: &str) -> String {
    format!("cadence issue set {id} status=ready")
}

pub const CMD_ISSUE_SYNC: &str = "cadence issue sync";
pub const CMD_RESTART_WHEN_IDLE: &str = "cadence daemon restart --when-idle --ui";

/// A needs-me row before the urgency sort.
struct Item {
    rank: u8,
    age: i64,
    json: Value,
}

fn item(
    rank: u8,
    kind: &str,
    title: &str,
    age: i64,
    project: &str,
    link: Option<&str>,
    command: &str,
) -> Item {
    let age = age.max(0);
    Item {
        rank,
        age,
        json: json!({
            "kind": kind, "title": title, "age": age,
            "project": project, "link": link, "command": command,
        }),
    }
}

/// Urgency order: kind rank ascending, then oldest first inside a kind.
fn sort_needs(needs: &mut [Item]) {
    needs.sort_by(|a, b| a.rank.cmp(&b.rank).then(b.age.cmp(&a.age)));
}

fn git_text(repo: &Path, args: &[String]) -> Result<String, String> {
    let out = run_bounded(
        Command::new("git").arg("-C").arg(repo).args(args),
        GIT_TIMEOUT,
    )
    .map_err(|e| format!("git {}: {e}", args.join(" ")))?;
    if !out.status.success() {
        return Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

fn gh_text(args: &[String]) -> Result<String, String> {
    let out =
        run_bounded(Command::new("gh").args(args), GH_TIMEOUT).map_err(|e| format!("gh: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(format!("gh {}: {}", args.join(" "), err));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// The default-branch ref to count drift against: `origin/HEAD`'s
/// target when the symref exists, else origin/main, origin/master,
/// then the local names.
fn default_ref(repo: &Path) -> Result<String, String> {
    if let Ok(sym) = git_text(
        repo,
        &[
            "rev-parse".into(),
            "--abbrev-ref".into(),
            "origin/HEAD".into(),
        ],
    ) {
        let sym = sym.trim();
        if sym.starts_with("origin/") {
            return Ok(sym.to_string());
        }
    }
    for cand in ["origin/main", "origin/master", "main", "master"] {
        if git_text(repo, &["rev-parse".into(), "--verify".into(), cand.into()])
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false)
        {
            return Ok(cand.to_string());
        }
    }
    Err("no default branch ref".to_string())
}

/// Commits on the repo's default branch after `build_commit`: the
/// count plus subjects bounded to [`DRIFT_SUBJECTS`], each with the
/// squash-merged `(#n)` parsed out. `build_commit` "unknown" — or one
/// git cannot place — is `known:false`, "cannot tell", never zero.
fn compute_drift(repo: &Path, build_commit: &str) -> Value {
    let mut base = json!({"known": false, "repo": repo});
    if build_commit == "unknown" || build_commit.is_empty() {
        base["reason"] = json!("build commit unknown — cannot tell");
        return base;
    }
    let dref = match default_ref(repo) {
        Ok(r) => r,
        Err(e) => {
            base["reason"] = json!(format!("cannot tell — {e}"));
            return base;
        }
    };
    let range = format!("{build_commit}..{dref}");
    let count = match git_text(repo, &["rev-list".into(), "--count".into(), range.clone()]) {
        Ok(c) => match c.trim().parse::<i64>() {
            Ok(n) => n,
            Err(_) => {
                base["reason"] = json!("cannot tell — unreadable count");
                return base;
            }
        },
        Err(e) => {
            base["reason"] = json!(format!("cannot tell — {e}"));
            return base;
        }
    };
    let subjects = git_text(
        repo,
        &[
            "log".into(),
            format!("-{DRIFT_SUBJECTS}"),
            "--format=%s".into(),
            range,
        ],
    )
    .unwrap_or_default();
    let commits: Vec<Value> = subjects
        .lines()
        .map(|s| json!({"subject": s, "pr": pr_number(s)}))
        .collect();
    json!({
        "known": true, "repo": repo, "ref": dref,
        "build_commit": build_commit,
        "count": count, "commits": commits,
    })
}

/// `gh pr list` + the default-branch commit status for one repo slug.
/// Both calls inside one cache entry so a partial failure refreshes
/// together.
fn gh_repo(slug: &str) -> Result<Value, String> {
    let prs = gh_text(&[
        "pr".into(),
        "list".into(),
        "--repo".into(),
        slug.into(),
        "--state".into(),
        "open".into(),
        "--limit".into(),
        "50".into(),
        "--json".into(),
        "number,title,url,headRefOid,headRefName,updatedAt,statusCheckRollup".into(),
    ])?;
    let ci = gh_text(&["api".into(), format!("repos/{slug}/commits/HEAD/status")])?;
    Ok(json!({
        "prs": serde_json::from_str::<Value>(&prs)
            .map_err(|e| format!("gh pr list: unreadable ({e})"))?,
        "ci": serde_json::from_str::<Value>(&ci)
            .map_err(|e| format!("gh status: unreadable ({e})"))?,
    }))
}

fn cache_file(state_dir: &Path) -> PathBuf {
    state_dir.join("overview-gh.json")
}

/// The cache body on disk: the slug set the rows were fetched for
/// plus the repo payloads. A body only serves a request for the same
/// slug set — a tracker with no GitHub remotes must not blank the
/// board's rows, and a different tracker must not inherit them.
struct GhCache {
    at: i64,
    slugs: Vec<String>,
    repos: HashMap<String, Value>,
}

fn read_cache(file: &Path) -> Option<GhCache> {
    let text = std::fs::read_to_string(file).ok()?;
    let cached: Value = serde_json::from_str(&text).ok()?;
    let at = cached["at"].as_i64()?;
    let slugs = cached["slugs"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|s| s.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let repos = cached["repos"]
        .as_object()
        .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default();
    Some(GhCache { at, slugs, repos })
}

/// Temp-write then rename — a crashed reader never sees half a body.
fn write_cache(file: &Path, slugs: &[String], repos: &HashMap<String, Value>, at: i64) {
    let tmp = file.with_extension("tmp");
    let body = serde_json::to_string(&json!({
        "at": at, "slugs": slugs, "repos": repos,
    }))
    .unwrap_or_default();
    if std::fs::write(&tmp, body).is_ok() {
        let _ = std::fs::rename(&tmp, file);
    }
}

/// The GitHub block, 60 s-cached under the state dir. Returns the
/// repos map plus `{state: ok|cached|stale|unavailable, error?}` —
/// `stale` serves the last good body through a `gh` outage, and the
/// screen narrows instead of failing either way.
fn github(state_dir: &Path, slugs: &[String]) -> (HashMap<String, Value>, Value) {
    let file = cache_file(state_dir);
    let now = now_epoch();
    let cached = read_cache(&file);
    if let Some(c) = &cached {
        if now - c.at < GH_CACHE_SECS && c.slugs == slugs {
            return (c.repos.clone(), json!({"state": "cached", "at": c.at}));
        }
    }
    if slugs.is_empty() {
        // Nothing to fetch — and nothing to write: an empty slug set
        // must never stamp over a good cache.
        return (HashMap::new(), json!({"state": "ok"}));
    }
    let mut repos = HashMap::new();
    let mut first_err = None;
    for slug in slugs {
        match gh_repo(slug) {
            Ok(v) => {
                repos.insert(slug.clone(), v);
            }
            Err(e) => {
                if first_err.is_none() {
                    first_err = Some(e);
                }
            }
        }
    }
    if repos.is_empty() {
        // Every call failed: keep the last good rows for this slug
        // set, untimed — better stale rows than blank ones.
        let stale: HashMap<String, Value> = cached
            .map(|c| {
                c.repos
                    .into_iter()
                    .filter(|(k, _)| slugs.contains(k))
                    .collect()
            })
            .unwrap_or_default();
        let state = if stale.is_empty() {
            "unavailable"
        } else {
            "stale"
        };
        return (stale, json!({"state": state, "error": first_err}));
    }
    // Partial failure: stale rows fill the missing slugs when the
    // cache covered them, so one flaky repo can't blank its PRs.
    if let Some(c) = cached {
        for slug in slugs {
            if !repos.contains_key(slug) {
                if let Some(v) = c.repos.get(slug) {
                    repos.insert(slug.clone(), v.clone());
                }
            }
        }
    }
    let _ = std::fs::create_dir_all(state_dir);
    write_cache(&file, slugs, &repos, now);
    (repos, json!({"state": "ok", "error": first_err}))
}

/// The tracker project matching the repo this binary was built from:
/// remote first (normalised both sides), then declared path against
/// the build checkout. Returns the project key and the local clone to
/// walk — the declared `path`, else the build root itself.
fn build_repo_match(projects: &[project::Project]) -> Option<(String, PathBuf)> {
    let build_remote = if BUILD_REMOTE == "unknown" {
        None
    } else {
        Some(project::normalize_remote(BUILD_REMOTE))
    };
    let build_root = if BUILD_ROOT == "unknown" {
        None
    } else {
        Some(
            PathBuf::from(BUILD_ROOT)
                .canonicalize()
                .unwrap_or_else(|_| PathBuf::from(BUILD_ROOT)),
        )
    };
    for p in projects {
        for r in &p.repos {
            let remote_hit = match (&build_remote, &r.remote) {
                (Some(want), Some(have)) => project::normalize_remote(have) == *want,
                _ => false,
            };
            let declared = r.path.as_deref().map(project::expand_home);
            let path_hit = match (&build_root, &declared) {
                (Some(want), Some(have)) => {
                    have.canonicalize().unwrap_or_else(|_| have.clone()) == *want
                }
                _ => false,
            };
            if remote_hit || path_hit {
                let repo = declared
                    .clone()
                    .or_else(|| build_root.clone())
                    .unwrap_or_else(|| PathBuf::from("."));
                return Some((p.key.clone(), repo));
            }
        }
    }
    None
}

/// The whole screen. `pm_dir` names the tracker dir (it may not exist
/// — that just empties the tracker sections); `state_dir` names the
/// runtime dir (daemon socket + the gh cache).
pub fn overview(state_dir: &Path, pm_dir: &Path) -> Value {
    let now = now_epoch();
    let mut needs: Vec<Item> = Vec::new();

    // ---- daemon: agents, approvals, drift identity ----
    // Reachability comes from `health` — a pre-daemon_info daemon
    // answers it, so an old build never reads as "unreachable" while
    // `agent list` works. `daemon_info` only carries the build id.
    let daemon_reachable = client::rpc(state_dir, "health", json!({})).is_ok();
    let info = client::rpc(state_dir, "daemon_info", json!({})).ok();
    let agents = client::rpc(state_dir, "agent_list", json!({}))
        .ok()
        .and_then(|v| v["agents"].as_array().cloned())
        .unwrap_or_default();
    let mut panes_idle = daemon_reachable;
    // An old daemon without `agent_probe` cannot confirm a pane is
    // idle — the drift row must say so instead of vanishing quietly.
    let mut probes_unknown = false;
    for a in &agents {
        let alias = a["alias"].as_str().unwrap_or_default();
        let show = client::rpc(state_dir, "agent_show", json!({"alias": alias}))
            .unwrap_or_else(|_| json!({"messages": [], "queued": 0}));
        let queued = show["queued"].as_i64().unwrap_or(0);
        let age = now - a["updated"].as_f64().unwrap_or(now as f64) as i64;
        if a["state"].as_str() == Some("attention") {
            needs.push(item(
                30,
                "fenced",
                &format!("agent {alias} fenced — reconcile then resume"),
                age,
                "",
                None,
                &cmd_agent_unfence(alias),
            ));
        }
        if a["stalled"].as_bool().unwrap_or(false) {
            needs.push(item(
                40,
                "stalled",
                &format!("agent {alias} turn silent"),
                a["silent_secs"].as_f64().unwrap_or(age as f64) as i64,
                "",
                None,
                &cmd_agent_show(alias),
            ));
        }
        if a["provider"].as_str() == Some(registry::INBOX) && queued > 0 {
            needs.push(item(
                100,
                "inbox_unread",
                &format!("{queued} unread for {alias}"),
                age,
                "",
                None,
                &cmd_inbox(alias),
            ));
        }
        if let Ok(reqs) = client::rpc(state_dir, "agent_requests", json!({"alias": alias})) {
            for req in reqs["requests"].as_array().cloned().unwrap_or_default() {
                let handle = req["request"].as_str().unwrap_or_default();
                let method = req["method"].as_str().unwrap_or("request");
                let what = if method == "item/tool/requestUserInput" {
                    "input request"
                } else {
                    "approval"
                };
                needs.push(item(
                    20,
                    "approval",
                    &format!("{method} {what} for {alias}"),
                    age,
                    "",
                    None,
                    &cmd_agent_respond(alias, handle, method),
                ));
            }
        }
        // Drift is only actionable when everything is idle: any running
        // or submitted message, or any busy pty pane, holds it back.
        let busy_turn = show["messages"]
            .as_array()
            .map(|ms| {
                ms.iter().any(|m| {
                    matches!(
                        m["state"].as_str().unwrap_or_default(),
                        "running" | "submitted"
                    )
                })
            })
            .unwrap_or(false);
        if busy_turn {
            panes_idle = false;
        } else if a["endpoint_kind"].as_str() == Some("pty") && a["endpoint"].is_string() {
            match client::rpc(state_dir, "agent_probe", json!({"alias": alias})) {
                Ok(p) if p["idle"].as_bool().unwrap_or(false) => {}
                Ok(_) => panes_idle = false,
                // "Unknown method" — a daemon that predates the RPC;
                // any other failure reads as busy, conservatively.
                Err(e) if e.to_string().contains("Unknown method") => probes_unknown = true,
                Err(_) => panes_idle = false,
            }
        }
    }

    // ---- tracker + GitHub ----
    // The GitHub read happens first: the tracker's review rows need
    // the open-PR list for the branch-name match (`refs` alone misses
    // PRs nobody linked).
    let pm = issue::Pm::at(pm_dir).ok();
    let mut slugs = Vec::new();
    let mut slug_project: HashMap<String, String> = HashMap::new();
    if let Some(pm) = &pm {
        for p in project::list(&pm.dir).unwrap_or_default() {
            for r in &p.repos {
                if let Some(remote) = &r.remote {
                    let norm = project::normalize_remote(remote);
                    if let Some(slug) = norm.strip_prefix("github.com/") {
                        let slug = slug.to_string();
                        slug_project.insert(slug.clone(), p.key.clone());
                        slugs.push(slug);
                    }
                }
            }
        }
    }
    slugs.sort();
    slugs.dedup();
    let (gh_repos, gh_state) = github(state_dir, &slugs);
    // Lowercased headRefName of every open PR — an issue in review
    // counts as "has a PR" when a `cadence/<id-lowercase>-…` branch is
    // open, even without an explicit `pr` ref.
    let mut open_pr_branches: Vec<String> = Vec::new();
    for data in gh_repos.values() {
        for pr in data["prs"].as_array().cloned().unwrap_or_default() {
            if let Some(head) = pr["headRefName"].as_str() {
                open_pr_branches.push(head.to_lowercase());
            }
        }
    }
    let mut projects_out = Vec::new();
    if let Some(pm) = &pm {
        let issues = board::load_all(&pm.dir, None).unwrap_or_default();
        let views = board::views(&pm.config.notes_dir(), issues);
        let mut status_of: HashMap<String, String> = HashMap::new();
        for v in &views {
            status_of.insert(v.issue.front.id.clone(), v.status.clone());
        }
        for v in &views {
            let id = v.issue.front.id.as_str();
            let project = v.issue.project.as_str();
            let age = parse_iso(&v.issue.front.created)
                .map(|c| now - c)
                .unwrap_or(0);
            let branch_prefix = format!("cadence/{}-", id.to_lowercase());
            let open_pr = v
                .issue
                .front
                .refs
                .iter()
                .any(|r| r.kind == "pr" && r.closed != Some(true))
                || open_pr_branches
                    .iter()
                    .any(|b| b.starts_with(&branch_prefix));
            if v.status == "review" && !open_pr {
                needs.push(item(
                    70,
                    "review_no_pr",
                    &format!("{id} in review with no open PR"),
                    age,
                    project,
                    None,
                    &cmd_issue_show(id),
                ));
            }
            let unblocked = !v.issue.front.blocked_by.is_empty()
                && v.issue
                    .front
                    .blocked_by
                    .iter()
                    .all(|b| status_of.get(b).map(String::as_str) == Some("done"));
            if !matches!(v.status.as_str(), "done" | "dropped") && unblocked {
                needs.push(item(
                    80,
                    "blocked_ready",
                    &format!("{id} unblocked — blockers all done"),
                    age,
                    project,
                    None,
                    &cmd_issue_set_ready(id),
                ));
            }
        }
        for p in project::list(&pm.dir).unwrap_or_default() {
            let mut open_by_status = serde_json::Map::new();
            let mut oldest_review: Option<i64> = None;
            for v in views.iter().filter(|v| v.issue.project == p.key) {
                if matches!(v.status.as_str(), "done" | "dropped") {
                    continue;
                }
                let n = open_by_status
                    .get(&v.status)
                    .and_then(Value::as_i64)
                    .unwrap_or(0);
                open_by_status.insert(v.status.clone(), json!(n + 1));
                if v.status == "review" {
                    let age = parse_iso(&v.issue.front.created)
                        .map(|c| now - c)
                        .unwrap_or(0);
                    oldest_review = Some(oldest_review.map_or(age, |o| o.max(age)));
                }
            }
            projects_out.push(json!({
                "key": p.key,
                "open_by_status": open_by_status,
                "oldest_review_age": oldest_review,
            }));
        }
        // Tracker behind its upstream — local refs only, never a fetch.
        if let Ok(behind) = git_text(
            &pm.dir,
            &[
                "rev-list".into(),
                "--count".into(),
                "HEAD..@{upstream}".into(),
            ],
        ) {
            if let Ok(n) = behind.trim().parse::<i64>() {
                if n > 0 {
                    needs.push(item(
                        110,
                        "tracker_behind",
                        &format!("tracker {n} commit(s) behind upstream"),
                        0,
                        "",
                        None,
                        CMD_ISSUE_SYNC,
                    ));
                }
            }
        }
    }

    // ---- GitHub rows: merge-ready, verdict-less, red CI ----
    for (slug, data) in &gh_repos {
        let project = slug_project.get(slug).cloned().unwrap_or_default();
        for pr in data["prs"].as_array().cloned().unwrap_or_default() {
            let n = pr["number"].as_i64().unwrap_or(0);
            let title = pr["title"].as_str().unwrap_or("");
            let url = pr["url"].as_str().map(str::to_string);
            let rollup = pr["statusCheckRollup"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            let age = pr["updatedAt"]
                .as_str()
                .and_then(parse_iso)
                .map(|u| now - u)
                .unwrap_or(0);
            match verdict_state(&rollup).as_deref() {
                Some("SUCCESS") if checks_green(&rollup) => {
                    // The verdict binds this head — a push after it
                    // makes the copied command refuse instead of
                    // merging an unreviewed head.
                    let head = pr["headRefOid"].as_str().unwrap_or("");
                    needs.push(item(
                        10,
                        "merge",
                        &format!("PR #{n} {title} — verdict pass, checks green"),
                        age,
                        &project,
                        url.as_deref(),
                        &format!(
                            "gh pr merge {n} --repo {slug} --squash --admin --match-head-commit {head}"
                        ),
                    ));
                }
                Some("SUCCESS") | Some("FAILURE") | Some("ERROR") => {}
                _ => needs.push(item(
                    60,
                    "pr_no_verdict",
                    &format!("PR #{n} {title} — no verdict"),
                    age,
                    &project,
                    url.as_deref(),
                    &format!("gh pr view {n} --repo {slug}"),
                )),
            }
        }
        if matches!(data["ci"]["state"].as_str(), Some("failure" | "error")) {
            needs.push(item(
                90,
                "ci_red",
                &format!("default branch CI failing on {slug}"),
                0,
                &project,
                None,
                &format!("gh run list --repo {slug}"),
            ));
        }
    }

    // ---- deploy drift: is what we merged actually running ----
    let projects = pm
        .as_ref()
        .map(|pm| project::list(&pm.dir).unwrap_or_default())
        .unwrap_or_default();
    let mut drift = if !daemon_reachable {
        json!({"matched": false, "reason": "daemon unreachable — cannot tell"})
    } else if info.is_none() {
        // Reachable but predates `daemon_info` — the build commit is
        // unreadable, so drift is unknowable, never zero.
        json!({
            "matched": false,
            "reason": "daemon build unknown (daemon predates daemon_info) — restart to enable drift",
        })
    } else {
        match build_repo_match(&projects) {
            None => json!({
                "matched": false,
                "build_commit": info.as_ref().map(|i| i["build_commit"].clone()).unwrap_or(json!("unknown")),
                "reason": "no tracker repo matches the build — cannot tell",
            }),
            Some((key, repo)) => {
                let build = info
                    .as_ref()
                    .and_then(|i| i["build_commit"].as_str())
                    .unwrap_or("unknown");
                let mut d = compute_drift(&repo, build);
                d["matched"] = json!(true);
                d["project"] = json!(key);
                d
            }
        }
    };
    if drift["known"].as_bool().unwrap_or(false) && drift["count"].as_i64().unwrap_or(0) > 0 {
        if panes_idle {
            let n = drift["count"].as_i64().unwrap_or(0);
            needs.push(item(
                50,
                "drift",
                &format!("{n} merged commit(s) not running — all panes idle"),
                0,
                drift["project"].as_str().unwrap_or(""),
                None,
                CMD_RESTART_WHEN_IDLE,
            ));
        } else {
            // Held back, but say why — a busy pane is different from
            // a daemon that cannot answer `agent_probe` at all.
            drift["held"] = json!(if probes_unknown {
                "cannot tell whether panes are idle — daemon predates agent_probe"
            } else {
                "a pane is busy — restart only when idle"
            });
        }
    }

    sort_needs(&mut needs);
    let daemon = match info {
        Some(mut i) => {
            i["reachable"] = json!(daemon_reachable);
            i
        }
        None if daemon_reachable => json!({
            "reachable": true,
            "info": "daemon predates daemon_info — build identity unreadable",
        }),
        None => json!({"reachable": false}),
    };
    json!({
        "needs_me": needs.iter().map(|i| i.json.clone()).collect::<Vec<_>>(),
        "drift": drift,
        "projects": projects_out,
        "github": gh_state,
        "daemon": daemon,
        "generated_at": now,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_parses() {
        assert_eq!(parse_iso("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_iso("2000-01-01T00:00:00Z"), Some(946684800));
        assert_eq!(parse_iso("2026-09-19T05:33:43"), None);
        assert_eq!(parse_iso(""), None);
        assert_eq!(parse_iso("not a date at all!!!!"), None);
    }

    #[test]
    fn pr_numbers() {
        assert_eq!(pr_number("board: tailnet sharing (#51)"), Some(51));
        assert_eq!(pr_number("no number"), None);
        assert_eq!(pr_number("mid (#5) subject"), None);
        assert_eq!(pr_number("edge (#abc)"), None);
    }

    #[test]
    fn rollup_verdict_and_green() {
        let rollup = vec![
            json!({"__typename":"CheckRun","name":"test","status":"COMPLETED","conclusion":"SUCCESS"}),
            json!({"__typename":"StatusContext","context":"qa-verdict","state":"SUCCESS"}),
        ];
        assert_eq!(verdict_state(&rollup).as_deref(), Some("SUCCESS"));
        assert!(checks_green(&rollup));
        let red = vec![
            json!({"__typename":"CheckRun","name":"test","status":"COMPLETED","conclusion":"FAILURE"}),
            json!({"__typename":"StatusContext","context":"qa-verdict","state":"SUCCESS"}),
        ];
        assert!(!checks_green(&red));
        let pending =
            vec![json!({"__typename":"StatusContext","context":"qa-verdict","state":"PENDING"})];
        assert!(checks_green(&pending));
        assert_eq!(verdict_state(&pending).as_deref(), Some("PENDING"));
        assert_eq!(verdict_state(&[]), None);
        assert!(checks_green(&[]));
    }

    #[test]
    fn ordering_rank_then_age() {
        let mut v = vec![
            item(60, "pr_no_verdict", "b", 5, "", None, "c"),
            item(10, "merge", "a", 1, "", None, "c"),
            item(60, "pr_no_verdict", "c", 50, "", None, "c"),
        ];
        sort_needs(&mut v);
        let kinds: Vec<&str> = v.iter().map(|i| i.json["kind"].as_str().unwrap()).collect();
        assert_eq!(kinds, ["merge", "pr_no_verdict", "pr_no_verdict"]);
        assert_eq!(v[1].json["title"], "c"); // older first within a kind
    }

    fn git(repo: &Path, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {:?}", out.stderr);
    }

    #[test]
    fn drift_counts_commits_and_prs() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        git(repo, &["init", "-b", "main"]);
        git(repo, &["config", "user.email", "t@t"]);
        git(repo, &["config", "user.name", "t"]);
        let commit = |msg: &str| {
            std::fs::write(repo.join("f"), msg).unwrap();
            git(repo, &["add", "f"]);
            git(repo, &["commit", "-m", msg]);
            git_text(repo, &["rev-parse".into(), "HEAD".into()])
                .unwrap()
                .trim()
                .to_string()
        };
        let base = commit("one (#1)");
        // Zero drift: building at HEAD.
        let d = compute_drift(repo, &base);
        assert_eq!(d["known"], true);
        assert_eq!(d["count"], 0);
        commit("two (#2)");
        commit("three — no pr");
        commit("four (#4)");
        let d = compute_drift(repo, &base);
        assert_eq!(d["known"], true);
        assert_eq!(d["count"], 3);
        let prs: Vec<Option<u64>> = d["commits"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["pr"].as_u64())
            .collect();
        assert_eq!(prs, vec![Some(4), None, Some(2)]);
        // Unknown build — cannot tell, not zero.
        let d = compute_drift(repo, "unknown");
        assert_eq!(d["known"], false);
        assert!(d["reason"].as_str().unwrap().contains("cannot tell"));
        // A commit git cannot place — also cannot tell.
        let d = compute_drift(repo, "deadbeef".repeat(5).as_str());
        assert_eq!(d["known"], false);
    }

    #[test]
    fn drift_matches_repo_by_remote_or_path() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("clone");
        std::fs::create_dir_all(&repo).unwrap();
        let p = project::Project {
            key: "cadence".into(),
            prefix: "CAD".into(),
            repos: vec![project::Repo {
                path: Some(repo.to_string_lossy().into()),
                remote: Some("git@github.com:favcrm/cadence.git".into()),
            }],
            components: vec![],
            tags: vec![],
            default_owner: None,
            build: None,
        };
        // Path match against BUILD_ROOT (this crate's checkout) never
        // hits the temp clone — remote match does when remote differs…
        let m = build_repo_match(std::slice::from_ref(&p));
        // Build remote is github.com/favcrm/cadence in this checkout, so
        // the declared remote matches; on foreign checkouts (unknown
        // remote) nothing matches — either outcome is consistent.
        if BUILD_REMOTE != "unknown" {
            assert_eq!(m.map(|(k, _)| k), Some("cadence".to_string()));
        } else {
            assert!(m.is_none());
        }
        // A project whose remote differs and whose path differs never
        // matches.
        let other = project::Project {
            key: "other".into(),
            prefix: "OTH".into(),
            repos: vec![project::Repo {
                path: Some("/definitely/not/the/build".into()),
                remote: Some("git@example.com:other/repo.git".into()),
            }],
            components: vec![],
            tags: vec![],
            default_owner: None,
            build: None,
        };
        assert!(build_repo_match(&[other]).is_none());
    }
}
