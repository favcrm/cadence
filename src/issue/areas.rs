//! CAD-378: code-area owners and advisory path leases.
//!
//! Two parallel PMs can each be right about their own ticket and still
//! collide on the code: one reworks rules another epic owns, or two
//! lanes invent the same primitive. The merge queue catches text
//! conflicts; this module catches ownership overlaps early — and only
//! ever warns.
//!
//! - **Areas** (`areas:` in the project's `PROJECT.md` frontmatter): a
//!   name, file-level path globs, an owner (an epic id and/or a PM
//!   alias) and optionally `max_open_prs`. Symbol-level areas
//!   (`src/daemon.rs#slot_identity`) are refused as a config error:
//!   globs are file-level only.
//! - **Planned paths** (`paths:` on an issue, `issue set <ID>
//!   paths=a,b`): what a ticket expects to touch.
//! - **Open lanes**: issues with an open worktree ref, their planned
//!   paths and the files their worktree actually changed against its
//!   base (`git diff --name-only <merge-base>` — local git only, never
//!   `gh`).
//!
//! `issue start` / `dispatch` warn when the ticket's planned paths
//! overlap an open lane, touch an area owned by someone else, or touch
//! an area already at `max_open_prs`; they never refuse. The overview
//! adds a Needs-you `area_ack` row while an open lane with a PR changes
//! files in an area owned by someone else, until the owner's PM (or the
//! operator) acks it through the daemon (`area_ack`), which binds the
//! acker from the connection. The ack lives in the daemon's state dir
//! (`area_acks.json`), never in a tracker comment: tracker files take
//! any author an agent writes, so a comment could forge the owner.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::error::{Error, Result};
use crate::issue::board::Issue;
use crate::issue::model::{self, Front};
use crate::issue::parse;

/// Longest glob accepted, in bytes.
pub const PATH_MAX: usize = 256;
/// Most planned paths one issue may declare.
pub const PATHS_MAX: usize = 64;
/// Bound on each git call reading a lane's changed files.
const GIT_TIMEOUT: Duration = Duration::from_secs(5);
/// Files named per lane in a warning before `…`.
const NAMED_MAX: usize = 5;

/// One `areas:` entry.
#[derive(Clone, Debug, PartialEq)]
pub struct Area {
    pub name: String,
    pub paths: Vec<String>,
    /// The owning epic (`CAD-411`), when the owner names one.
    pub epic: Option<String>,
    /// The owning PM's alias, when the owner names one.
    pub pm: Option<String>,
    pub max_open_prs: Option<u32>,
}

impl Area {
    /// `CAD-411/pm-cc`, `CAD-411` or `pm-cc`.
    pub fn owner(&self) -> String {
        [self.epic.as_deref(), self.pm.as_deref()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join("/")
    }

    /// Does `glob` (a planned path) reach into this area?
    pub fn touches(&self, glob: &str) -> bool {
        self.paths.iter().any(|a| overlaps(a, glob))
    }

    /// Does this area cover the concrete file `path`?
    pub fn covers(&self, path: &str) -> bool {
        self.paths.iter().any(|a| matches(a, path))
    }

    /// Is `who` on the owner's side: the owning epic itself or one of
    /// its tickets (child or plan ticket), or dispatched by the owning
    /// PM. Such work never needs a warning or an ack.
    pub fn owned_by(&self, who: &Side) -> bool {
        let epic = self.epic.as_deref().is_some_and(|e| {
            who.issue == e
                || who.parent.as_deref() == Some(e)
                || who.plan_epic.as_deref() == Some(e)
        });
        let pm = self
            .pm
            .as_deref()
            .is_some_and(|p| who.pm.as_deref() == Some(p));
        epic || pm
    }

    fn to_json(&self) -> Value {
        json!({"name": self.name, "paths": self.paths, "owner": self.owner(),
               "epic": self.epic, "pm": self.pm, "max_open_prs": self.max_open_prs})
    }
}

/// Who a piece of work belongs to, for [`Area::owned_by`].
#[derive(Clone, Debug, Default)]
pub struct Side {
    pub issue: String,
    pub parent: Option<String>,
    pub plan_epic: Option<String>,
    /// The PM that dispatched it — the requester at dispatch, the
    /// issue's claim holder for an open lane.
    pub pm: Option<String>,
}

impl Side {
    pub fn of(front: &Front, pm: Option<&str>) -> Side {
        Side {
            issue: front.id.clone(),
            parent: front.parent.clone(),
            plan_epic: front.plan_epic.clone(),
            pm: pm.filter(|p| !p.is_empty()).map(str::to_string),
        }
    }
}

#[derive(Deserialize)]
struct Raw {
    #[serde(default)]
    areas: Option<BTreeMap<String, RawArea>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawArea {
    paths: Vec<String>,
    owner: String,
    #[serde(default)]
    max_open_prs: Option<u32>,
}

/// `PROJECT.md`'s `areas:` block. No frontmatter or no `areas:` is no
/// areas; anything malformed is an error naming the area and the value.
/// The rest of the frontmatter belongs to other readers and is ignored.
pub fn parse_config(text: &str) -> Result<Vec<Area>> {
    let trimmed = text.strip_prefix('\u{feff}').unwrap_or(text);
    if !trimmed.starts_with("---\n") && !trimmed.starts_with("---\r\n") {
        return Ok(vec![]);
    }
    let (yaml, _) = parse::split_front(trimmed)?;
    let bad = |m: String| Error::rejected(format!("PROJECT.md areas: {m}"));
    let raw: Raw = serde_yaml::from_str(yaml).map_err(|e| bad(e.to_string()))?;
    let mut out = Vec::new();
    for (name, a) in raw.areas.unwrap_or_default() {
        if !model::valid_tag(&name) {
            return Err(bad(format!(
                "area '{name}' — 1-32 lowercase letters, digits or hyphens"
            )));
        }
        if a.paths.is_empty() {
            return Err(bad(format!("area '{name}' lists no paths")));
        }
        for p in &a.paths {
            check_path(p).map_err(|e| bad(format!("area '{name}': {e}")))?;
        }
        let (epic, pm) = parse_owner(&a.owner).map_err(|e| bad(format!("area '{name}': {e}")))?;
        if a.max_open_prs == Some(0) {
            return Err(bad(format!(
                "area '{name}': max_open_prs must be at least 1 (omit it for no limit)"
            )));
        }
        out.push(Area {
            name,
            paths: a.paths,
            epic,
            pm,
            max_open_prs: a.max_open_prs,
        });
    }
    Ok(out)
}

/// `EPIC`, `pm-alias` or `EPIC/pm-alias` — an issue id is the epic, any
/// other alias-shaped word the PM.
fn parse_owner(owner: &str) -> std::result::Result<(Option<String>, Option<String>), String> {
    let (mut epic, mut pm) = (None, None);
    let parts: Vec<&str> = owner.split('/').map(str::trim).collect();
    if owner.trim().is_empty() || parts.len() > 2 {
        return Err(format!(
            "owner '{owner}' — an epic id, a PM alias, or 'EPIC/pm-alias'"
        ));
    }
    for part in parts {
        if model::valid_id(part) {
            if epic.replace(part.to_string()).is_some() {
                return Err(format!("owner '{owner}' names two epics"));
            }
        } else if crate::issue::claim::check_alias(part, "owner").is_ok() {
            if pm.replace(part.to_string()).is_some() {
                return Err(format!("owner '{owner}' names two PMs"));
            }
        } else {
            return Err(format!(
                "owner '{owner}': '{part}' is neither an issue id nor an alias"
            ));
        }
    }
    Ok((epic, pm))
}

/// One planned path or area glob: repo-relative, file-level. `*` and
/// `?` match inside one path segment, a `**` segment any number of
/// segments, and a trailing `/` (or a plain directory path) everything
/// under it.
pub fn check_path(p: &str) -> Result<()> {
    let bad = |why: &str| Err(Error::rejected(format!("path '{p}' {why}")));
    if p.is_empty() {
        return bad("is empty");
    }
    if p.len() > PATH_MAX {
        return bad(&format!("is longer than {PATH_MAX} bytes"));
    }
    if p.contains('#') {
        return bad(
            "names a symbol — areas and planned paths are file-level globs only \
             (symbol-level areas are not supported)",
        );
    }
    if p.chars().any(|c| c.is_control() || c == '\\' || c == ',') {
        return bad("contains a control character, a backslash or a comma");
    }
    if p.starts_with('/') || p.starts_with('~') {
        return bad("must be relative to the repo root");
    }
    let body = p.strip_suffix('/').unwrap_or(p);
    if body
        .split('/')
        .any(|s| s.is_empty() || s == "." || s == "..")
    {
        return bad("has an empty, '.' or '..' segment");
    }
    Ok(())
}

/// `issue set <ID> paths=a,b` — validated, sorted, de-duplicated; an
/// empty value clears.
pub fn parse_paths(value: &str) -> Result<Vec<String>> {
    let mut out: Vec<String> = value
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    for p in &out {
        check_path(p)?;
    }
    out.sort();
    out.dedup();
    if out.len() > PATHS_MAX {
        return Err(Error::rejected(format!(
            "{} planned paths — at most {PATHS_MAX}; use a directory or a glob",
            out.len()
        )));
    }
    Ok(out)
}

fn is_wild(seg: &str) -> bool {
    seg.contains('*') || seg.contains('?')
}

/// The glob's segments, a trailing `/` read as `/**`.
fn segments(glob: &str) -> Vec<&str> {
    match glob.strip_suffix('/') {
        Some(dir) => dir.split('/').chain(std::iter::once("**")).collect(),
        None => glob.split('/').collect(),
    }
}

fn seg_match(pat: &[u8], s: &[u8]) -> bool {
    match (pat.first(), s.first()) {
        (None, None) => true,
        (Some(b'*'), _) => seg_match(&pat[1..], s) || (!s.is_empty() && seg_match(pat, &s[1..])),
        (Some(b'?'), Some(_)) => seg_match(&pat[1..], &s[1..]),
        (Some(p), Some(c)) if p == c => seg_match(&pat[1..], &s[1..]),
        _ => false,
    }
}

fn segs_match(pat: &[&str], path: &[&str]) -> bool {
    match pat.first() {
        None => path.is_empty(),
        Some(&"**") => (0..=path.len()).any(|i| segs_match(&pat[1..], &path[i..])),
        Some(p) => {
            !path.is_empty()
                && seg_match(p.as_bytes(), path[0].as_bytes())
                && segs_match(&pat[1..], &path[1..])
        }
    }
}

/// Does `glob` cover the concrete file `path`? A glob without wildcards
/// is a file or a directory: it covers itself and everything under it.
pub fn matches(glob: &str, path: &str) -> bool {
    let pat = segments(glob);
    let file: Vec<&str> = path.split('/').collect();
    if segs_match(&pat, &file) {
        return true;
    }
    !pat.iter().any(|s| is_wild(s)) && file.len() > pat.len() && file[..pat.len()] == pat[..]
}

/// Can two globs cover a common file? Exact when either side is a plain
/// path; otherwise conservative — their literal leading segments are
/// compared, so an advisory warning may over-report, never under-report.
pub fn overlaps(a: &str, b: &str) -> bool {
    let plain = |g: &str| !segments(g).iter().any(|s| is_wild(s));
    let body = |g: &str| g.strip_suffix('/').unwrap_or(g).to_string();
    if plain(a) && plain(b) {
        return matches(a, &body(b)) || matches(b, &body(a));
    }
    if matches(a, &body(b)) || matches(b, &body(a)) {
        return true;
    }
    let lit = |g: &str| -> Vec<String> {
        segments(g)
            .into_iter()
            .take_while(|s| !is_wild(s))
            .map(str::to_string)
            .collect()
    };
    let (la, lb) = (lit(a), lit(b));
    let n = la.len().min(lb.len());
    la[..n] == lb[..n]
}

/// `<pm>/<key>/PROJECT.md`'s areas, strictly: a symlinked, unreadable
/// or malformed file is an error.
pub fn load(pm_dir: &Path, key: &str) -> Result<Vec<Area>> {
    match crate::issue::work::read_project_md(pm_dir, key)? {
        None => Ok(vec![]),
        Some(text) => parse_config(&text).map_err(|e| Error::rejected(format!("{key}: {e}"))),
    }
}

/// [`load`] for readers and advisory checks: a bad config is no areas
/// plus the error, surfaced as `areas_error` (the `config_error`
/// pattern) — an advisory feature never blocks work.
pub fn load_or_error(pm_dir: &Path, key: &str) -> (Vec<Area>, Option<String>) {
    match load(pm_dir, key) {
        Ok(areas) => (areas, None),
        Err(e) => (vec![], Some(e.to_string())),
    }
}

/// One open lane: an issue whose worktree ref is still open.
#[derive(Clone, Debug)]
pub struct Lane {
    pub issue: String,
    pub project: String,
    pub side: Side,
    /// The lane's worker — the issue's `owner`.
    pub worker: Option<String>,
    /// The open `pr` ref, else the review loop's recorded PR.
    pub pr: Option<String>,
    pub worktree: PathBuf,
    pub planned: Vec<String>,
    pub changed: Vec<String>,
    /// Why `changed` could not be read (a missing checkout, a git error).
    pub changed_error: Option<String>,
}

impl Lane {
    /// `CAD-1 (worker w1, PR <url>)`.
    pub fn label(&self) -> String {
        format!(
            "{} (worker {}, PR {})",
            self.issue,
            self.worker.as_deref().unwrap_or("-"),
            self.pr.as_deref().unwrap_or("none yet")
        )
    }

    /// Does this lane plan or change anything in `area`?
    pub fn in_area(&self, area: &Area) -> bool {
        self.planned.iter().any(|p| area.touches(p)) || self.changed.iter().any(|c| area.covers(c))
    }

    /// This lane's planned or changed paths that `glob` reaches.
    pub fn hits(&self, glob: &str) -> Vec<String> {
        self.planned
            .iter()
            .filter(|p| overlaps(glob, p))
            .chain(self.changed.iter().filter(|c| matches(glob, c)))
            .cloned()
            .collect()
    }

    pub fn to_json(&self) -> Value {
        json!({"issue": self.issue, "project": self.project, "worker": self.worker,
               "pm": self.side.pm, "pr": self.pr, "worktree": self.worktree,
               "planned": self.planned, "changed": self.changed,
               "changed_error": self.changed_error})
    }
}

/// The open lanes among `issues` (not done/dropped, with an open
/// worktree ref), their changed files read in parallel. `prs` maps an
/// issue id to the review loop's recorded PR URL.
pub fn open_lanes(issues: &[&Issue], prs: &BTreeMap<String, String>) -> Vec<Lane> {
    let mut lanes: Vec<Lane> = issues
        .iter()
        .filter(|i| !matches!(i.front.status.as_str(), "done" | "dropped"))
        .filter_map(|i| {
            let f = &i.front;
            let wt = f
                .refs
                .iter()
                .find(|r| r.kind == "worktree" && r.closed != Some(true))
                .and_then(|r| r.path.clone())?;
            let pr = f
                .refs
                .iter()
                .find(|r| r.kind == "pr" && r.closed != Some(true))
                .and_then(|r| r.url.clone().or_else(|| r.path.clone()))
                .or_else(|| prs.get(&f.id).cloned());
            Some(Lane {
                issue: f.id.clone(),
                project: i.project.clone(),
                side: Side::of(f, f.claim.as_ref().map(|c| c.by.as_str())),
                worker: f.owner.clone(),
                pr,
                worktree: PathBuf::from(wt),
                planned: f.paths.clone(),
                changed: vec![],
                changed_error: None,
            })
        })
        .collect();
    let dirs: Vec<PathBuf> = lanes.iter().map(|l| l.worktree.clone()).collect();
    let results: Vec<std::result::Result<Vec<String>, String>> = std::thread::scope(|s| {
        let handles: Vec<_> = dirs
            .iter()
            .map(|dir| s.spawn(move || changed_files(dir)))
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap_or_else(|_| Err("reader panicked".into())))
            .collect()
    });
    for (lane, result) in lanes.iter_mut().zip(results) {
        match result {
            Ok(files) => lane.changed = files,
            Err(e) => lane.changed_error = Some(e),
        }
    }
    lanes
}

fn git_line(dir: &Path, args: &[&str]) -> std::result::Result<String, String> {
    let out = crate::proc::run_bounded(
        Command::new("git").arg("-C").arg(dir).args(args),
        GIT_TIMEOUT,
    )
    .map_err(|e| format!("git {}: {e:?}", args.join(" ")))?;
    if !out.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The files a lane's worktree changed against its base — the merge
/// base of `HEAD` with the repo's default branch (`origin/HEAD`, else
/// `origin/main`, else `main`, else `master`), committed or not.
pub fn changed_files(wt: &Path) -> std::result::Result<Vec<String>, String> {
    if !wt.is_dir() {
        return Err(format!("worktree {} is missing", wt.display()));
    }
    let base = git_line(wt, &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"])
        .ok()
        .into_iter()
        .chain(["origin/main", "main", "master"].map(str::to_string))
        .find(|b| git_line(wt, &["rev-parse", "--verify", "--quiet", b]).is_ok())
        .ok_or_else(|| "no default branch to diff against".to_string())?;
    let mb = git_line(wt, &["merge-base", "HEAD", &base])?;
    let text = git_line(wt, &["diff", "--name-only", "--no-renames", &mb])?;
    let mut files: Vec<String> = text
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    files.sort();
    files.dedup();
    Ok(files)
}

fn named(files: &[String]) -> String {
    let mut out: Vec<&str> = files.iter().take(NAMED_MAX).map(String::as_str).collect();
    if files.len() > NAMED_MAX {
        out.push("…");
    }
    out.join(", ")
}

/// The advisory warnings for starting `me` with `planned` paths against
/// the other open `lanes`: planned paths overlapping a lane's planned
/// or changed paths; an area owned by someone other than `me`'s side;
/// an area already holding `max_open_prs` open lanes. Pure — the caller
/// prints and records them; nothing is refused.
pub fn warnings(areas: &[Area], me: &Side, planned: &[String], lanes: &[Lane]) -> Vec<Value> {
    let others: Vec<&Lane> = lanes.iter().filter(|l| l.issue != me.issue).collect();
    let mut out = Vec::new();
    for area in areas {
        let mine: Vec<String> = planned
            .iter()
            .filter(|p| area.touches(p))
            .cloned()
            .collect();
        if mine.is_empty() {
            continue;
        }
        let open: Vec<&Lane> = others.iter().copied().filter(|l| l.in_area(area)).collect();
        let open_names = if open.is_empty() {
            "none".to_string()
        } else {
            open.iter()
                .map(|l| l.label())
                .collect::<Vec<_>>()
                .join("; ")
        };
        if !area.owned_by(me) {
            out.push(json!({
                "kind": "owned",
                "area": area.name,
                "owner": area.owner(),
                "lanes": open.iter().map(|l| l.issue.clone()).collect::<Vec<_>>(),
                "text": format!(
                    "{} plans {} in area '{}' owned by {} — agree it with the owner; \
                     open lanes there: {open_names}",
                    me.issue, named(&mine), area.name, area.owner()
                ),
            }));
        }
        if let Some(max) = area.max_open_prs {
            if open.len() >= max as usize {
                out.push(json!({
                    "kind": "capacity",
                    "area": area.name,
                    "owner": area.owner(),
                    "max_open_prs": max,
                    "lanes": open.iter().map(|l| l.issue.clone()).collect::<Vec<_>>(),
                    "text": format!(
                        "area '{}' (owner {}) is at capacity: {} open lane(s)/PR(s), \
                         max_open_prs {max} — {open_names}",
                        area.name, area.owner(), open.len()
                    ),
                }));
            }
        }
    }
    for lane in others {
        let mut hits: Vec<String> = planned.iter().flat_map(|p| lane.hits(p)).collect();
        hits.sort();
        hits.dedup();
        if hits.is_empty() {
            continue;
        }
        out.push(json!({
            "kind": "overlap",
            "lane": lane.issue,
            "worker": lane.worker,
            "pr": lane.pr,
            "paths": hits,
            "text": format!(
                "{}'s planned paths overlap open lane {}: {}",
                me.issue, lane.label(), named(&hits)
            ),
        }));
    }
    out
}

/// The `leases` block `issue start` and `dispatch` return: the areas
/// the ticket's planned paths touch and the warnings. Every failure is
/// reported in the block, never raised — the start goes ahead.
pub fn check_start(pm_dir: &Path, project: &str, front: &Front, requester: &str) -> Value {
    let (areas, config_error) = load_or_error(pm_dir, project);
    let planned = &front.paths;
    let touched: Vec<Value> = areas
        .iter()
        .filter(|a| planned.iter().any(|p| a.touches(p)))
        .map(Area::to_json)
        .collect();
    let mut block = json!({
        "planned": planned,
        "areas": touched,
        "warnings": [],
        "config_error": config_error,
    });
    if planned.is_empty() {
        return block;
    }
    let issues = match crate::issue::board::load_all(pm_dir, Some(project)) {
        Ok(issues) => issues,
        Err(e) => {
            block["error"] = json!(format!("open lanes unreadable: {e}"));
            return block;
        }
    };
    let refs: Vec<&Issue> = issues.iter().filter(|i| i.front.id != front.id).collect();
    let lanes = open_lanes(&refs, &BTreeMap::new());
    let me = Side::of(front, Some(requester));
    block["warnings"] = json!(warnings(&areas, &me, planned, &lanes));
    block
}

/// The warning lines of a `leases` block — what the CLI prints and the
/// tracker comment records.
pub fn warning_lines(block: &Value) -> Vec<String> {
    let mut out: Vec<String> = block["warnings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|w| w["text"].as_str().map(str::to_string))
        .collect();
    if let Some(e) = block["config_error"].as_str() {
        out.push(format!("{e} — area checks skipped"));
    }
    out
}

// ---- acks: daemon-owned, never a tracker file ----

/// `<state>/area_acks.json` — written only by the daemon's `area_ack`.
pub fn acks_path(state_dir: &Path) -> PathBuf {
    state_dir.join("area_acks.json")
}

/// The ack key for `issue` × `area`.
pub fn ack_key(issue: &str, area: &str) -> String {
    format!("{issue}/{area}")
}

/// Every recorded ack; an unreadable file is none — the rows stay up,
/// the fail-safe direction.
pub fn acks(state_dir: &Path) -> Map<String, Value> {
    std::fs::read_to_string(acks_path(state_dir))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

static ACK_WRITER: Mutex<()> = Mutex::new(());

/// Record one ack (tmp + rename, writers serialized in-process).
pub fn record_ack(state_dir: &Path, key: &str, record: Value) -> Result<()> {
    let _guard = ACK_WRITER.lock().unwrap_or_else(|e| e.into_inner());
    let mut all = acks(state_dir);
    all.insert(key.to_string(), record);
    let path = acks_path(state_dir);
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(&Value::Object(all))?)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// `cadence issue ack <ID> --area <name>` — the command a Needs-you row
/// names.
pub fn cmd_ack(issue: &str, area: &str) -> String {
    format!("cadence issue ack {issue} --area {area}")
}

/// One lane × owned-area pair that needs the owner's ack: the lane has
/// a PR and changed files in an area owned by someone else, and nobody
/// with the right has acked it.
#[derive(Clone, Debug)]
pub struct AckNeed {
    pub issue: String,
    pub project: String,
    pub area: Area,
    pub files: Vec<String>,
    pub pr: Option<String>,
    pub worker: Option<String>,
}

/// The ack rows for `lanes` of one project. `has_pr` answers whether a
/// lane has a PR beyond its own record (an open PR branch on GitHub).
pub fn ack_needs(
    areas: &[Area],
    lanes: &[Lane],
    acks: &Map<String, Value>,
    has_pr: impl Fn(&Lane) -> bool,
) -> Vec<AckNeed> {
    let mut out = Vec::new();
    for lane in lanes {
        if lane.pr.is_none() && !has_pr(lane) {
            continue;
        }
        for area in areas {
            if area.owned_by(&lane.side) {
                continue;
            }
            let files: Vec<String> = lane
                .changed
                .iter()
                .filter(|c| area.covers(c))
                .cloned()
                .collect();
            if files.is_empty() || acks.contains_key(&ack_key(&lane.issue, &area.name)) {
                continue;
            }
            out.push(AckNeed {
                issue: lane.issue.clone(),
                project: lane.project.clone(),
                area: area.clone(),
                files,
                pr: lane.pr.clone(),
                worker: lane.worker.clone(),
            });
        }
    }
    out
}

impl AckNeed {
    pub fn title(&self) -> String {
        format!(
            "{} changes area '{}' owned by {} — needs the owner's ack ({})",
            self.issue,
            self.area.name,
            self.area.owner(),
            named(&self.files)
        )
    }
}

/// The board overlay for one project: each open lane with the areas it
/// plans or changes and the other lanes it overlaps.
pub fn overlay(areas: &[Area], lanes: &[Lane]) -> Vec<Value> {
    lanes
        .iter()
        .map(|l| {
            let in_areas: Vec<&str> = areas
                .iter()
                .filter(|a| l.in_area(a))
                .map(|a| a.name.as_str())
                .collect();
            let mine: Vec<&String> = l.planned.iter().chain(l.changed.iter()).collect();
            let overlaps_with: Vec<&str> = lanes
                .iter()
                .filter(|o| o.issue != l.issue)
                .filter(|o| mine.iter().any(|p| !o.hits(p).is_empty()))
                .map(|o| o.issue.as_str())
                .collect();
            json!({
                "issue": l.issue,
                "worker": l.worker,
                "pm": l.side.pm,
                "pr": l.pr,
                "planned": l.planned,
                "changed_count": l.changed.len(),
                "changed_error": l.changed_error,
                "areas": in_areas,
                "overlaps": overlaps_with,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(areas: &str) -> String {
        format!("---\nproject: demo\nareas:\n{areas}---\n# Demo\n")
    }

    #[test]
    fn globs_match_files_and_directories() {
        assert!(matches("src/peer.rs", "src/peer.rs"));
        assert!(!matches("src/peer.rs", "src/peer.rs.bak"));
        assert!(matches("src/daemon", "src/daemon/caller_rule.rs"));
        assert!(matches("src/daemon/", "src/daemon/caller_rule.rs"));
        assert!(!matches("src/daemon/", "src/daemon.rs"));
        assert!(matches("src/*.rs", "src/peer.rs"));
        assert!(!matches("src/*.rs", "src/daemon/caller_rule.rs"));
        assert!(matches("src/**/*.rs", "src/peer.rs"));
        assert!(matches("src/**/*.rs", "src/daemon/caller_rule.rs"));
        assert!(matches("**/caller_rule.rs", "src/daemon/caller_rule.rs"));
        assert!(matches("src/pe?r.rs", "src/peer.rs"));
        assert!(!matches("ui/**", "src/peer.rs"));
    }

    #[test]
    fn globs_overlap_conservatively() {
        assert!(overlaps("src/peer.rs", "src/peer.rs"));
        assert!(overlaps("src/daemon/", "src/daemon/caller_rule.rs"));
        assert!(overlaps("src/daemon/caller_rule.rs", "src/daemon"));
        assert!(overlaps("src/**", "src/peer.rs"));
        assert!(overlaps("src/*.rs", "src/peer.rs"));
        assert!(overlaps("src/daemon/**", "src/**/*.rs"));
        assert!(!overlaps("src/peer.rs", "src/daemon.rs"));
        assert!(!overlaps("ui/**", "src/**"));
        assert!(!overlaps("src/daemon/", "src/daemon.rs"));
    }

    #[test]
    fn areas_parse_with_owner_forms() {
        let areas = parse_config(&cfg(
            "  caller-identity:\n    paths: [src/peer.rs, src/daemon/caller_rule.rs]\n    \
             owner: CAD-411/pm-cc\n    max_open_prs: 2\n  board:\n    paths: [ui/]\n    \
             owner: pm-ui\n  store:\n    paths: [src/store.rs]\n    owner: CAD-9\n",
        ))
        .unwrap();
        assert_eq!(areas.len(), 3);
        let a = areas.iter().find(|a| a.name == "caller-identity").unwrap();
        assert_eq!(
            (a.epic.as_deref(), a.pm.as_deref(), a.max_open_prs),
            (Some("CAD-411"), Some("pm-cc"), Some(2))
        );
        assert_eq!(a.owner(), "CAD-411/pm-cc");
        let b = areas.iter().find(|a| a.name == "board").unwrap();
        assert_eq!((b.epic.as_deref(), b.pm.as_deref()), (None, Some("pm-ui")));
        let s = areas.iter().find(|a| a.name == "store").unwrap();
        assert_eq!((s.epic.as_deref(), s.pm.as_deref()), (Some("CAD-9"), None));
        // No frontmatter or no `areas:` is no areas; other keys are
        // other readers' business.
        assert!(parse_config("# Demo\n").unwrap().is_empty());
        assert!(parse_config("---\nstages: [a, b]\n---\n")
            .unwrap()
            .is_empty());
    }

    /// A bad `areas:` block is an error that names the area and why.
    #[test]
    fn bad_areas_config_is_a_clear_error() {
        for (areas, want) in [
            (
                "  x:\n    paths: [src/daemon.rs#slot_identity]\n    owner: pm\n",
                "file-level globs only",
            ),
            ("  x:\n    paths: []\n    owner: pm\n", "lists no paths"),
            (
                "  x:\n    paths: [/etc/passwd]\n    owner: pm\n",
                "relative",
            ),
            (
                "  x:\n    paths: [src/../x]\n    owner: pm\n",
                "'..' segment",
            ),
            ("  x:\n    paths: [a]\n    owner: ''\n", "owner ''"),
            (
                "  x:\n    paths: [a]\n    owner: CAD-1/CAD-2\n",
                "two epics",
            ),
            ("  x:\n    paths: [a]\n    owner: a/b/c\n", "owner 'a/b/c'"),
            (
                "  x:\n    paths: [a]\n    owner: 'p m'\n",
                "neither an issue id",
            ),
            (
                "  x:\n    paths: [a]\n    owner: pm\n    max_open_prs: 0\n",
                "max_open_prs must be at least 1",
            ),
            (
                "  x:\n    paths: [a]\n    owner: pm\n    maxprs: 2\n",
                "maxprs",
            ),
            ("  x:\n    paths: [a]\n", "owner"),
            ("  Bad_Name:\n    paths: [a]\n    owner: pm\n", "Bad_Name"),
        ] {
            let err = parse_config(&cfg(areas)).unwrap_err().to_string();
            assert!(err.contains("PROJECT.md areas"), "{areas}: {err}");
            assert!(err.contains(want), "{areas}: wanted '{want}': {err}");
        }
        // Not a map at all.
        let err = parse_config("---\nareas: [a, b]\n---\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("PROJECT.md areas"), "{err}");
    }

    #[test]
    fn planned_paths_parse_and_refuse() {
        assert_eq!(
            parse_paths(" src/b.rs, src/a.rs ,src/b.rs,").unwrap(),
            vec!["src/a.rs", "src/b.rs"]
        );
        assert!(parse_paths("").unwrap().is_empty());
        for bad in ["src/x.rs#f", "/abs", "../up", "a//b", "a\\b"] {
            assert!(parse_paths(bad).is_err(), "{bad}");
        }
    }

    fn area(
        name: &str,
        paths: &[&str],
        epic: Option<&str>,
        pm: Option<&str>,
        max: Option<u32>,
    ) -> Area {
        Area {
            name: name.into(),
            paths: paths.iter().map(|s| s.to_string()).collect(),
            epic: epic.map(str::to_string),
            pm: pm.map(str::to_string),
            max_open_prs: max,
        }
    }

    fn lane(issue: &str, pm: &str, planned: &[&str], changed: &[&str], pr: Option<&str>) -> Lane {
        Lane {
            issue: issue.into(),
            project: "demo".into(),
            side: Side {
                issue: issue.into(),
                pm: Some(pm.into()),
                ..Side::default()
            },
            worker: Some(format!("w-{issue}")),
            pr: pr.map(str::to_string),
            worktree: PathBuf::from("/nowhere"),
            planned: planned.iter().map(|s| s.to_string()).collect(),
            changed: changed.iter().map(|s| s.to_string()).collect(),
            changed_error: None,
        }
    }

    fn me(pm: &str) -> Side {
        Side {
            issue: "D-9".into(),
            pm: Some(pm.into()),
            ..Side::default()
        }
    }

    fn kinds(ws: &[Value]) -> Vec<&str> {
        ws.iter().map(|w| w["kind"].as_str().unwrap()).collect()
    }

    /// Planned paths overlapping another lane's planned or ACTUAL
    /// changed paths warn, naming that lane's ticket, worker and PR.
    #[test]
    fn overlap_warns_on_planned_and_changed_paths() {
        let lanes = vec![
            lane(
                "D-1",
                "pm-a",
                &["src/peer.rs"],
                &[],
                Some("https://x/pull/7"),
            ),
            lane("D-2", "pm-a", &[], &["src/daemon/caller_rule.rs"], None),
            lane("D-3", "pm-a", &["ui/"], &["ui/src/App.tsx"], None),
        ];
        let ws = warnings(
            &[],
            &me("pm-a"),
            &["src/peer.rs".into(), "src/daemon/".into()],
            &lanes,
        );
        assert_eq!(kinds(&ws), vec!["overlap", "overlap"], "{ws:?}");
        let t0 = ws[0]["text"].as_str().unwrap();
        assert!(
            t0.contains("D-1") && t0.contains("w-D-1") && t0.contains("https://x/pull/7"),
            "{t0}"
        );
        assert_eq!(ws[1]["lane"], "D-2");
        assert_eq!(ws[1]["paths"], json!(["src/daemon/caller_rule.rs"]));
        // The ticket's own lane never overlaps itself, and no planned
        // paths is no warning.
        let own = vec![lane(
            "D-9",
            "pm-a",
            &["src/peer.rs"],
            &["src/peer.rs"],
            None,
        )];
        assert!(warnings(&[], &me("pm-a"), &["src/peer.rs".into()], &own).is_empty());
        assert!(warnings(&[], &me("pm-a"), &[], &lanes).is_empty());
    }

    /// An area at `max_open_prs` warns with the owner and the open
    /// lanes; one below it does not.
    #[test]
    fn capacity_warns_at_max_open_prs() {
        let areas = [area(
            "caller",
            &["src/peer.rs", "src/daemon/"],
            Some("D-50"),
            Some("pm-a"),
            Some(2),
        )];
        let one = vec![lane(
            "D-1",
            "pm-a",
            &["src/peer.rs"],
            &[],
            Some("https://x/pull/1"),
        )];
        let ws = warnings(&areas, &me("pm-a"), &["src/daemon/x.rs".into()], &one);
        assert!(kinds(&ws).is_empty(), "{ws:?}");
        let two = vec![
            lane(
                "D-1",
                "pm-a",
                &["src/peer.rs"],
                &[],
                Some("https://x/pull/1"),
            ),
            lane(
                "D-2",
                "pm-a",
                &[],
                &["src/daemon/y.rs"],
                Some("https://x/pull/2"),
            ),
        ];
        let ws = warnings(&areas, &me("pm-a"), &["src/daemon/x.rs".into()], &two);
        // Different files in one area: a capacity warning, no overlap.
        assert_eq!(kinds(&ws), vec!["capacity"], "{ws:?}");
        let t = ws[0]["text"].as_str().unwrap();
        assert!(
            t.contains("'caller'") && t.contains("D-50/pm-a") && t.contains("max_open_prs 2"),
            "{t}"
        );
        assert!(
            t.contains("https://x/pull/1") && t.contains("https://x/pull/2"),
            "{t}"
        );
    }

    /// An area owned by someone else warns; the owner's PM, the owning
    /// epic and its tickets do not.
    #[test]
    fn foreign_owner_warns_but_owner_side_does_not() {
        let areas = [area(
            "caller",
            &["src/peer.rs"],
            Some("D-50"),
            Some("pm-a"),
            None,
        )];
        let ws = warnings(&areas, &me("pm-b"), &["src/peer.rs".into()], &[]);
        assert_eq!(kinds(&ws), vec!["owned"], "{ws:?}");
        assert!(ws[0]["text"]
            .as_str()
            .unwrap()
            .contains("owned by D-50/pm-a"));
        assert!(warnings(&areas, &me("pm-a"), &["src/peer.rs".into()], &[]).is_empty());
        let child = Side {
            issue: "D-9".into(),
            parent: Some("D-50".into()),
            pm: Some("pm-b".into()),
            ..Side::default()
        };
        assert!(warnings(&areas, &child, &["src/peer.rs".into()], &[]).is_empty());
        let plan_ticket = Side {
            issue: "D-9".into(),
            plan_epic: Some("D-50".into()),
            pm: Some("pm-b".into()),
            ..Side::default()
        };
        assert!(warnings(&areas, &plan_ticket, &["src/peer.rs".into()], &[]).is_empty());
        // Paths outside every area never warn about owners.
        assert!(warnings(&areas, &me("pm-b"), &["ui/".into()], &[]).is_empty());
    }

    /// The ack row needs a PR, a changed file in a foreign-owned area,
    /// and no recorded ack; the owner's own lanes never need one.
    #[test]
    fn ack_needs_follow_changes_prs_and_acks() {
        let areas = [area(
            "caller",
            &["src/peer.rs"],
            Some("D-50"),
            Some("pm-a"),
            None,
        )];
        let lanes = vec![
            lane(
                "D-1",
                "pm-b",
                &[],
                &["src/peer.rs"],
                Some("https://x/pull/1"),
            ),
            lane("D-2", "pm-b", &[], &["src/peer.rs"], None),
            lane(
                "D-3",
                "pm-a",
                &[],
                &["src/peer.rs"],
                Some("https://x/pull/3"),
            ),
            lane(
                "D-4",
                "pm-b",
                &["src/peer.rs"],
                &["README.md"],
                Some("https://x/pull/4"),
            ),
        ];
        let none = Map::new();
        let needs = ack_needs(&areas, &lanes, &none, |_| false);
        assert_eq!(
            needs.iter().map(|n| n.issue.as_str()).collect::<Vec<_>>(),
            vec!["D-1"]
        );
        assert!(needs[0].title().contains("owned by D-50/pm-a"));
        // An open PR branch seen on GitHub counts as a PR.
        let needs = ack_needs(&areas, &lanes, &none, |l| l.issue == "D-2");
        assert_eq!(needs.len(), 2);
        let mut acked = Map::new();
        acked.insert(ack_key("D-1", "caller"), json!({"by": "pm-a"}));
        assert!(ack_needs(&areas, &lanes, &acked, |_| false).is_empty());
    }

    #[test]
    fn overlay_lists_areas_and_overlaps() {
        let areas = [area("caller", &["src/peer.rs"], None, Some("pm-a"), None)];
        let lanes = vec![
            lane("D-1", "pm-a", &["src/peer.rs"], &[], None),
            lane("D-2", "pm-b", &[], &["src/peer.rs"], None),
            lane("D-3", "pm-b", &[], &["ui/x.ts"], None),
        ];
        let rows = overlay(&areas, &lanes);
        assert_eq!(rows[0]["areas"], json!(["caller"]));
        assert_eq!(rows[0]["overlaps"], json!(["D-2"]));
        assert_eq!(rows[1]["overlaps"], json!(["D-1"]));
        assert_eq!(rows[2]["areas"], json!([]));
        assert_eq!(rows[2]["overlaps"], json!([]));
    }
}
