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
//!   paths and the files their branch committed against its base
//!   (`git diff --name-only <merge-base> HEAD` — two objects, so the
//!   worktree, its attribute filters, its fsmonitor and its hooks are
//!   never consulted; uncommitted and untracked work is invisible to
//!   the board. Local git only, never `gh`).
//!
//! `issue start` / `dispatch` warn when the ticket's planned paths
//! overlap an open lane, touch an area owned by someone else, or touch
//! an area already at `max_open_prs`; they never refuse. The overview
//! adds a Needs-you `area_ack` row while an open lane with a PR changes
//! files in an area owned by someone else, until the owner's PM (or the
//! operator) acks it through the daemon (`area_ack`), which binds the
//! acker from the connection and pins the lane's head — the row
//! re-raises when the lane commits again. The ack lives in the
//! daemon's state dir (`area_acks.json`), never in a tracker comment:
//! tracker files take any author an agent writes, so a comment could
//! forge the owner.
//!
//! A lane's `side.pm` is the actor its start/dispatch record binds —
//! the `Actor:` trailer (or ` (actor)` suffix) of the newest
//! lane-binding tracker commit — never `claim.by`, which is live
//! frontmatter the lane can rewrite to impersonate its owner and
//! suppress its own ack row. The epic side (`parent`, `plan_epic`) is
//! still frontmatter and remains advisory: a lane that claims
//! membership of the owning epic is the same class of self-assertion
//! this feature tolerates. Everything here warns; nothing refuses.
//!
//! Every string that reaches a terminal, a tracker comment or the
//! overview JSON is scrubbed of control and bidi characters
//! ([`scrub`]): refs, aliases and planted frontmatter are all
//! agent-writable.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
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
/// Most wildcard characters (`*`/`?`) one glob may carry — a cap on
/// top of [`PATH_MAX`], since wildcards are what makes matching cost
/// anything at all.
pub const WILD_MAX: usize = 16;
/// Changed files per lane kept for matching, sorted. A lane past the
/// cap warns on the prefix only — advisory, not exhaustive.
const CHANGED_MAX: usize = 8192;
/// Git readers [`open_lanes`] runs at once, whatever the lane count.
const LANE_GIT_WORKERS: usize = 4;
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
    if p.chars().filter(|c| matches!(c, '*' | '?')).count() > WILD_MAX {
        return bad(&format!("has more than {WILD_MAX} wildcards"));
    }
    Ok(())
}

/// Agent-controlled strings (refs, aliases, planted frontmatter,
/// filenames, a `changed_error`'s git stderr) reach terminals, tracker
/// comments and the overview JSON through warnings. Strip anything
/// that moves a cursor or mirrors text: control characters and bidi or
/// other invisible format marks. Matching fields stay raw — this is
/// for display only.
pub(crate) fn scrub(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control() && !is_format(*c))
        .collect()
}

/// Invisible or direction-overriding marks — the bidi controls,
/// joiners, soft hyphen, BOM, word joiners, interlinear anchors and
/// hangul fillers — none of which a path, ref or alias legitimately
/// carries.
fn is_format(c: char) -> bool {
    matches!(c,
        '\u{00AD}'              // soft hyphen
        | '\u{061C}'            // arabic letter mark
        | '\u{115F}' | '\u{1160}' | '\u{FFA0}'  // hangul fillers
        | '\u{200B}'..='\u{200F}' // ZWSP ZWNJ ZWJ LRM RLM
        | '\u{202A}'..='\u{202E}' // LRE RLE PDF RLO LRO
        | '\u{2060}'..='\u{2064}' // word joiner, invisible operators
        | '\u{2066}'..='\u{2069}' // LRI RLI FSI PDI
        | '\u{FEFF}'            // BOM / ZWNBSP
        | '\u{FFF9}'..='\u{FFFB}' // interlinear annotation anchors
    )
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

/// One pattern segment against one path segment. `*` is the only
/// pattern that consumes a variable run, so a single backtracking
/// point — the last `*` seen — suffices: retry means that `*` takes
/// one more byte. O(|pat| × |s|) worst case, no exponential retry, so
/// `src/******************z` against sixty `a`s returns in
/// microseconds instead of hanging a render.
fn seg_match(pat: &[u8], s: &[u8]) -> bool {
    let (mut p, mut c) = (0usize, 0usize);
    // (pattern index just past the last `*`, string index it resumes at).
    let mut star: Option<(usize, usize)> = None;
    while c < s.len() {
        match pat.get(p) {
            Some(b'*') => {
                star = Some((p + 1, c));
                p += 1;
            }
            Some(&pc) if pc == b'?' || pc == s[c] => {
                p += 1;
                c += 1;
            }
            // Mismatch or pattern exhausted mid-string: the last `*`
            // absorbs one more byte, or there is no match.
            _ => match star {
                Some((sp, sc)) => {
                    p = sp;
                    c = sc + 1;
                    star = Some((sp, c));
                }
                None => return false,
            },
        }
    }
    while pat.get(p) == Some(&b'*') {
        p += 1;
    }
    p == pat.len()
}

/// Pattern segments against path segments — `**` matches any run of
/// segments, everything else is one [`seg_match`]. A dynamic program
/// over (pattern × path): `dp[i][j]` is "`pat[i..]` matches
/// `path[j..]`", filled back to front in O(|pat| × |path|) cells, so
/// stacked `**`s cannot multiply work.
fn segs_match(pat: &[&str], path: &[&str]) -> bool {
    let (n, m) = (pat.len(), path.len());
    let mut dp = vec![vec![false; m + 1]; n + 1];
    for i in (0..=n).rev() {
        for j in (0..=m).rev() {
            dp[i][j] = match pat.get(i) {
                None => j == m,
                Some(&"**") => dp[i + 1][j] || (j < m && dp[i][j + 1]),
                Some(p) => j < m && seg_match(p.as_bytes(), path[j].as_bytes()) && dp[i + 1][j + 1],
            };
        }
    }
    dp[0][0]
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
    /// The worktree's committed tip — what an ack pins to.
    pub head: Option<String>,
    pub worktree: PathBuf,
    pub planned: Vec<String>,
    pub changed: Vec<String>,
    /// Why `changed` could not be read (a missing checkout, a git error).
    pub changed_error: Option<String>,
}

impl Lane {
    /// `CAD-1 (worker w1, PR <url>)` — scrubbed: the ref values and
    /// alias are agent-writable and land in terminals and comments.
    pub fn label(&self) -> String {
        scrub(&format!(
            "{} (worker {}, PR {})",
            self.issue,
            self.worker.as_deref().unwrap_or("-"),
            self.pr.as_deref().unwrap_or("none yet")
        ))
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

    /// Display form — every field that can carry agent-written bytes
    /// is scrubbed.
    pub fn to_json(&self) -> Value {
        let scrubbed = |xs: &[String]| xs.iter().map(|x| scrub(x)).collect::<Vec<_>>();
        json!({"issue": self.issue, "project": self.project,
               "worker": self.worker.as_deref().map(scrub),
               "pm": self.side.pm.as_deref().map(scrub),
               "pr": self.pr.as_deref().map(scrub),
               "worktree": self.worktree,
               "planned": scrubbed(&self.planned), "changed": scrubbed(&self.changed),
               "changed_error": self.changed_error.as_deref().map(scrub)})
    }
}

/// One lane's parallel probe: committed changed files, the tip an ack
/// pins to, and the PM the dispatch record binds.
type Probe = (
    std::result::Result<Vec<String>, String>,
    Option<String>,
    Option<String>,
);

/// The open lanes among `issues` (not done/dropped, with an open
/// worktree ref), each probed for its committed changes, head and
/// recorded PM. Probes run on a bounded pool — at most
/// [`LANE_GIT_WORKERS`] git readers at once whatever the lane count —
/// so a board render cannot fan out a process per lane. `prs` maps an
/// issue id to the review loop's recorded PR URL.
///
/// `pm_dir` is the tracker repo: a lane's `side.pm` is the actor its
/// dispatch record binds ([`recorded_pm`]), never `claim.by` — live
/// frontmatter the lane itself can rewrite to impersonate its owner.
/// `parent`/`plan_epic` stay frontmatter — advisory by design.
pub fn open_lanes(pm_dir: &Path, issues: &[&Issue], prs: &BTreeMap<String, String>) -> Vec<Lane> {
    let (lane_issues, mut lanes): (Vec<&Issue>, Vec<Lane>) = issues
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
            // Planted frontmatter bypasses `issue set`'s check_path —
            // validate on load and drop what a write would refuse.
            let planned: Vec<String> = f
                .paths
                .iter()
                .filter(|p| check_path(p).is_ok())
                .cloned()
                .collect();
            Some((
                *i,
                Lane {
                    issue: f.id.clone(),
                    project: i.project.clone(),
                    side: Side::of(f, None),
                    worker: f.owner.clone(),
                    pr,
                    head: None,
                    worktree: PathBuf::from(wt),
                    planned,
                    changed: vec![],
                    changed_error: None,
                },
            ))
        })
        .unzip();
    let next = AtomicUsize::new(0);
    let probed: Mutex<Vec<(usize, Probe)>> = Mutex::new(Vec::new());
    std::thread::scope(|s| {
        for _ in 0..LANE_GIT_WORKERS.min(lanes.len()) {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                let (Some(issue), Some(lane)) = (lane_issues.get(i), lanes.get(i)) else {
                    break;
                };
                let probe: Probe = (
                    changed_files(&lane.worktree),
                    lane_head(&lane.worktree).ok(),
                    recorded_pm(pm_dir, issue),
                );
                probed
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push((i, probe));
            });
        }
    });
    for (i, (changed, head, pm)) in probed.into_inner().unwrap_or_default() {
        let lane = &mut lanes[i];
        match changed {
            Ok(files) => lane.changed = files,
            Err(e) => lane.changed_error = Some(e),
        }
        lane.head = head;
        lane.side.pm = pm;
    }
    lanes
}

/// `git -C <dir>` as a pure object read. The `-c` flags disarm what an
/// agent-controlled worktree config could otherwise make the board
/// run: fsmonitor and hooks are off, and no external diff driver is
/// consulted. Inherited `GIT_*` env is dropped so a caller's
/// environment cannot retarget the repo, its index or its objects.
fn git_line(dir: &Path, args: &[&str]) -> std::result::Result<String, String> {
    let out = crate::proc::run_bounded(
        Command::new("git")
            .args([
                "-c",
                "core.fsmonitor=",
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "diff.external=",
                "-c",
                "diff.noprefix=false",
            ])
            .arg("-C")
            .arg(dir)
            .args(args)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORKTREE")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_OBJECT_DIRECTORY")
            .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES"),
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

/// The files a lane's branch changed against its base — the merge base
/// of `HEAD` with the repo's default branch (`origin/HEAD`, else
/// `origin/main`, else `main`, else `master`). Both diff sides are
/// commits, so this never hashes, stats or filters a worktree file:
/// uncommitted and untracked work is invisible, by design — the board
/// warns on what a lane has committed. Sorted, de-duplicated, capped
/// at [`CHANGED_MAX`].
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
    let text = git_line(wt, &["diff", "--name-only", "--no-renames", &mb, "HEAD"])?;
    let mut files: Vec<String> = text
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    files.sort();
    files.dedup();
    files.truncate(CHANGED_MAX);
    Ok(files)
}

/// The lane's committed tip — what an ack pins to so the row re-raises
/// when the lane commits again.
pub fn lane_head(wt: &Path) -> std::result::Result<String, String> {
    if !wt.is_dir() {
        return Err(format!("worktree {} is missing", wt.display()));
    }
    git_line(wt, &["rev-parse", "--verify", "HEAD"])
}

/// The PM a lane's start/dispatch record binds — the actor of the
/// newest tracker commit that bound the lane: `<id>: start …` (start
/// and dispatch both write it), `<id>: claim …` (a fresh claim or a
/// take-over) or `<id>: ref worktree` recorded by hand. A `release` or
/// a `ref worktree closed` lifts the binding — older commits are stale
/// and the scan stops there. The actor is the `Actor:` trailer
/// (CAD-42), else the ` (actor)` subject suffix, else the git author.
/// With no binding commit, the newest `dispatch`/`claim` comment's
/// author stands in.
///
/// `front.claim.by` is never consulted: it is live frontmatter a lane
/// rewrites without leaving a record, so trusting it would let a lane
/// name its owner and suppress its own ack row. The record is still
/// only *evidence* — a hand-forged commit or comment can fake it —
/// which is why every use of it warns and never refuses.
fn recorded_pm(pm_dir: &Path, issue: &Issue) -> Option<String> {
    let id = issue.front.id.as_str();
    let rel = format!("{}/{id}", issue.project);
    let log = git_line(
        pm_dir,
        &[
            "log",
            "--format=%s%x1f%an%x1f%(trailers:key=Actor,valueonly,separator=%x2C)",
            "--",
            &rel,
        ],
    )
    .unwrap_or_default();
    for line in log.lines() {
        let mut fields = line.split('\x1f');
        let (Some(subject), Some(author), Some(trailer)) =
            (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let Some(rest) = subject.strip_prefix(&format!("{id}: ")) else {
            continue;
        };
        if rest.starts_with("release") || rest.starts_with("ref worktree closed") {
            break;
        }
        if !(rest.starts_with("start ")
            || rest.starts_with("claim ")
            || rest.starts_with("ref worktree"))
        {
            continue;
        }
        let actor = trailer
            .split(',')
            .find(|t| !t.trim().is_empty())
            .map(str::trim)
            .map(str::to_string)
            .or_else(|| {
                crate::issue::history::split_paren(rest)
                    .1
                    .filter(|a| !a.is_empty())
            })
            .unwrap_or_else(|| author.to_string());
        return crate::issue::claim::check_alias(&actor, "pm")
            .ok()
            .map(|_| actor);
    }
    issue
        .comments
        .iter()
        .rev()
        .find(|c| matches!(c.front.kind.as_deref(), Some("dispatch") | Some("claim")))
        .and_then(|c| {
            crate::issue::claim::check_alias(&c.front.author, "pm")
                .ok()
                .map(|_| c.front.author.clone())
        })
}

/// Files named in warning text — scrubbed; `files` can carry planted
/// or quoted-from-git bytes.
fn named(files: &[String]) -> String {
    let mut out: Vec<String> = files.iter().take(NAMED_MAX).map(|f| scrub(f)).collect();
    if files.len() > NAMED_MAX {
        out.push("…".to_string());
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
                "text": scrub(&format!(
                    "{} plans {} in area '{}' owned by {} — agree it with the owner; \
                     open lanes there: {open_names}",
                    me.issue, named(&mine), area.name, area.owner()
                )),
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
                    "text": scrub(&format!(
                        "area '{}' (owner {}) is at capacity: {} open lane(s)/PR(s), \
                         max_open_prs {max} — {open_names}",
                        area.name, area.owner(), open.len()
                    )),
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
            "worker": lane.worker.as_deref().map(scrub),
            "pr": lane.pr.as_deref().map(scrub),
            "paths": hits.iter().map(|h| scrub(h)).collect::<Vec<_>>(),
            "text": scrub(&format!(
                "{}'s planned paths overlap open lane {}: {}",
                me.issue, lane.label(), named(&hits)
            )),
        }));
    }
    out
}

/// The `leases` block `issue start` and `dispatch` return: the areas
/// the ticket's planned paths touch and the warnings. Every failure is
/// reported in the block, never raised — the start goes ahead.
pub fn check_start(pm_dir: &Path, project: &str, front: &Front, requester: &str) -> Value {
    let (areas, config_error) = load_or_error(pm_dir, project);
    // Planted frontmatter bypasses `issue set`'s check_path — validate
    // on load; a path a write would refuse is dropped, not matched.
    let planned: Vec<String> = front
        .paths
        .iter()
        .filter(|p| check_path(p).is_ok())
        .cloned()
        .collect();
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
    let lanes = open_lanes(pm_dir, &refs, &BTreeMap::new());
    let me = Side::of(front, Some(requester));
    block["warnings"] = json!(warnings(&areas, &me, &planned, &lanes));
    block
}

/// The warning lines of a `leases` block — what the CLI prints and the
/// `lease` comment records. Scrubbed at the boundary: a warning's text
/// already is, but a planted `config_error` or a caller-built block
/// goes through the same filter.
pub fn warning_lines(block: &Value) -> Vec<String> {
    let mut out: Vec<String> = block["warnings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|w| w["text"].as_str().map(scrub))
        .collect();
    if let Some(e) = block["config_error"].as_str() {
        out.push(scrub(&format!("{e} — area checks skipped")));
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
///
/// An ack suppresses the row only while the lane's head is still the
/// commit the ack pinned: a lane that commits again after the ack —
/// touching the area or not — re-raises it, so a fresh change gets a
/// fresh look. Acks recorded without a `head` (pre-pinning records)
/// suppress nothing; the row stays up, the fail-safe direction.
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
                .map(|c| scrub(c))
                .collect();
            let acked = acks
                .get(&ack_key(&lane.issue, &area.name))
                .is_some_and(|a| {
                    lane.head
                        .as_deref()
                        .is_some_and(|h| a["head"].as_str() == Some(h))
                });
            if files.is_empty() || acked {
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
        scrub(&format!(
            "{} changes area '{}' owned by {} — needs the owner's ack ({})",
            self.issue,
            self.area.name,
            self.area.owner(),
            named(&self.files)
        ))
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
                "worker": l.worker.as_deref().map(scrub),
                "pm": l.side.pm.as_deref().map(scrub),
                "pr": l.pr.as_deref().map(scrub),
                "planned": l.planned.iter().map(|p| scrub(p)).collect::<Vec<_>>(),
                "changed_count": l.changed.len(),
                "changed_error": l.changed_error.as_deref().map(scrub),
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
            head: Some(format!("h-{issue}")),
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
        // An ack suppresses the row only while the lane's head is the
        // pinned commit: acks without a head (pre-pinning records) and
        // acks for a stale head keep the row up.
        let mut acked = Map::new();
        acked.insert(ack_key("D-1", "caller"), json!({"by": "pm-a"}));
        assert_eq!(
            ack_needs(&areas, &lanes, &acked, |_| false).len(),
            1,
            "an ack with no pinned head suppresses nothing"
        );
        acked.insert(
            ack_key("D-1", "caller"),
            json!({"by": "pm-a", "head": "stale"}),
        );
        assert_eq!(
            ack_needs(&areas, &lanes, &acked, |_| false).len(),
            1,
            "the lane moved since the ack — the row re-raises"
        );
        acked.insert(
            ack_key("D-1", "caller"),
            json!({"by": "pm-a", "head": "h-D-1"}),
        );
        assert!(
            ack_needs(&areas, &lanes, &acked, |_| false).is_empty(),
            "an ack pinned to the current head clears the row"
        );
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

    /// The reviewer's hang: `src/******************z` over sixty `a`s
    /// and a stack of `**`s must resolve in well under 100ms — the
    /// matchers are linear, no exponential backtracking.
    #[test]
    fn globs_are_linear_on_pathological_patterns() {
        let t0 = std::time::Instant::now();
        let deep_a = format!("src/{}", "a".repeat(60));
        for _ in 0..50 {
            assert!(!matches("src/******************z", &deep_a));
        }
        let stacked = format!("{}z", "**/".repeat(60));
        let deep: String = (0..60)
            .map(|i| format!("d{i}"))
            .collect::<Vec<_>>()
            .join("/");
        assert!(!matches(&stacked, &deep));
        // Glob-on-glob overlap goes through the same matchers.
        assert!(overlaps("x/******************z", "x/******************y"));
        assert!(
            t0.elapsed() < std::time::Duration::from_millis(100),
            "pathological patterns took {:?}",
            t0.elapsed()
        );
    }

    /// Wildcard and length caps refuse the pathological shapes at the
    /// field, so they never reach the matcher at all.
    #[test]
    fn check_path_caps_wildcards() {
        assert!(check_path("src/**/*.rs").is_ok());
        assert!(check_path(&format!("src/{}", "*".repeat(WILD_MAX))).is_ok());
        let err = check_path(&format!("src/{}", "*".repeat(WILD_MAX + 1))).unwrap_err();
        assert!(err.to_string().contains("wildcards"), "{err}");
        assert!(parse_paths("src/******************z").is_err());
    }

    /// Control and bidi bytes in agent-writable fields never reach a
    /// warning, a lease comment or the overview JSON.
    #[test]
    fn warning_text_is_scrubbed() {
        let mut evil = lane("D-1", "pm-b", &["src/peer.rs"], &[], None);
        evil.worker = Some("w\u{1b}[2J\u{202e}detaruS".into());
        evil.pr = Some("https://x/\u{7}pull/9".into());
        evil.changed = vec!["src/pe\u{1b}er.rs".into()];
        assert!(!evil.label().chars().any(|c| c.is_control() || is_format(c)));
        let ws = warnings(&[], &me("pm-a"), &["src/".into()], &[evil]);
        assert_eq!(kinds(&ws), vec!["overlap"]);
        let w = &ws[0];
        for v in [
            w["text"].as_str().unwrap().to_string(),
            w["worker"].as_str().unwrap().to_string(),
            w["pr"].as_str().unwrap().to_string(),
            w["paths"].to_string(),
        ] {
            assert!(
                !v.chars().any(|c| c.is_control() || is_format(c)),
                "unscrubbed bytes in {v:?}"
            );
        }
        // The lease comment path is the same lines.
        for line in warning_lines(&json!({"warnings": ws, "config_error": null})) {
            assert!(
                !line.chars().any(|c| c.is_control() || is_format(c)),
                "{line:?}"
            );
        }
        // The ESC byte goes; the harmless `[0m` it introduced stays as
        // literal text — scrubbing removes controls, not sequences.
        assert_eq!(scrub("a\u{1b}[0mb\u{202e}c\u{200b}"), "a[0mbc");
    }

    /// `changed_files` diffs two commits — a planted clean filter, an
    /// fsmonitor command and uncommitted or untracked work in the lane's
    /// worktree must stay untouched and invisible.
    #[test]
    fn changed_files_read_commits_only() {
        let dir = tmpdir("changed");
        std::fs::write(dir.join("base.txt"), "base\n").unwrap();
        git(&dir, &["add", "-A"]);
        git(&dir, &["commit", "-qm", "base"]);
        git(&dir, &["checkout", "-qb", "lane"]);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/x.rs"), "x\n").unwrap();
        git(&dir, &["add", "-A"]);
        git(&dir, &["commit", "-qm", "lane work"]);
        // Uncommitted and untracked: invisible to the board.
        std::fs::write(dir.join("base.txt"), "dirty\n").unwrap();
        std::fs::write(dir.join("untracked.rs"), "u\n").unwrap();
        // Traps a worktree read would fire: a clean filter (runs when
        // git hashes the dirty file) and an fsmonitor command (runs
        // when git scans the worktree). Neither may execute.
        let fired = dir.join("fired");
        let pwn = dir.join("pwn.sh");
        std::fs::write(&pwn, format!("#!/bin/sh\ntouch '{}'\n", fired.display())).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&pwn, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::write(dir.join(".gitattributes"), "* filter=pwn\n").unwrap();
        git(
            &dir,
            &["config", "filter.pwn.clean", &pwn.display().to_string()],
        );
        git(
            &dir,
            &["config", "core.fsmonitor", &pwn.display().to_string()],
        );
        assert_eq!(
            changed_files(&dir).unwrap(),
            vec!["src/x.rs".to_string()],
            "only committed changes count"
        );
        assert!(!fired.exists(), "a worktree filter/fsmonitor ran");
        assert_eq!(lane_head(&dir).unwrap().len(), 40);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A lane's PM comes from its start/dispatch record — the `Actor:`
    /// trailer of the newest binding commit — never from `claim.by`,
    /// which the lane can rewrite at will.
    #[test]
    fn recorded_pm_binds_the_commit_actor_not_frontmatter() {
        let dir = tmpdir("pm");
        let idir = dir.join("demo/D-1");
        std::fs::create_dir_all(&idir).unwrap();
        std::fs::write(dir.join("demo/project.yaml"), "key: demo\nprefix: D\n").unwrap();
        let text = |by: &str, n: usize| {
            format!(
                "---\nid: D-1\ntitle: t\nstatus: doing\npriority: P2\n\
                 claim: {{by: {by}, at: '2026-01-01T00:00:00Z'}}\n\
                 created: '2026-01-01T00:00:00Z'\n---\nedit {n}\n"
            )
        };
        // The lie is live in the file from the start: claim.by is the
        // area's owner pm-own, while the start record binds pm-a.
        std::fs::write(idir.join("issue.md"), text("pm-own", 0)).unwrap();
        git(&dir, &["add", "-A"]);
        git(
            &dir,
            &[
                "commit",
                "-qm",
                "D-1: start cadence/d-1-x\n\nIssue: D-1\nActor: pm-a",
            ],
        );
        let issue = issue_at(&dir, "demo", "D-1");
        assert_eq!(issue.front.claim.as_ref().unwrap().by, "pm-own");
        assert_eq!(recorded_pm(&dir, &issue).as_deref(), Some("pm-a"));
        // A hand-committed frontmatter edit is not a binding subject.
        std::fs::write(idir.join("issue.md"), text("pm-own", 1)).unwrap();
        git(&dir, &["add", "-A"]);
        git(
            &dir,
            &["commit", "-qm", "wip: D-1 claims pm-own\n\nActor: pm-own"],
        );
        assert_eq!(recorded_pm(&dir, &issue).as_deref(), Some("pm-a"));
        // A take-over re-binds; a release unbinds.
        std::fs::write(idir.join("issue.md"), text("pm-b", 2)).unwrap();
        git(&dir, &["add", "-A"]);
        git(
            &dir,
            &[
                "commit",
                "-qm",
                "D-1: claim take-over by pm-b from pm-a\n\nIssue: D-1\nActor: pm-b",
            ],
        );
        assert_eq!(recorded_pm(&dir, &issue).as_deref(), Some("pm-b"));
        std::fs::write(idir.join("issue.md"), text("pm-b", 3)).unwrap();
        git(&dir, &["add", "-A"]);
        git(
            &dir,
            &[
                "commit",
                "-qm",
                "D-1: release by pm-b\n\nIssue: D-1\nActor: pm-b",
            ],
        );
        assert_eq!(recorded_pm(&dir, &issue), None);
        // With no binding commit, the newest dispatch/claim comment
        // stands in; with neither, the lane is simply unowned.
        let (front, body) = parse::parse_issue(
            "---\nid: D-9\ntitle: t\nstatus: doing\npriority: P2\n\
             created: '2026-01-01T00:00:00Z'\n---\n",
        )
        .unwrap();
        let mut orphan = Issue {
            project: "demo".into(),
            dir: dir.join("demo/D-9"),
            front,
            body,
            comments: vec![],
            artifacts: vec![],
        };
        assert_eq!(recorded_pm(&dir, &orphan), None);
        orphan.comments.push(crate::issue::board::Comment {
            name: "x.md".into(),
            front: model::CommentFront {
                author: "pm-z".into(),
                at: "t".into(),
                kind: Some("dispatch".into()),
            },
            body: String::new(),
        });
        assert_eq!(recorded_pm(&dir, &orphan).as_deref(), Some("pm-z"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `open_lanes` drops planted paths `issue set` would have refused
    /// and binds `side.pm` to the dispatch record.
    #[test]
    fn open_lanes_validates_loaded_paths_and_binds_pm() {
        let dir = tmpdir("lanes");
        let idir = dir.join("demo/D-1");
        std::fs::create_dir_all(&idir).unwrap();
        std::fs::write(dir.join("demo/project.yaml"), "key: demo\nprefix: D\n").unwrap();
        std::fs::write(
            idir.join("issue.md"),
            format!(
                "---\nid: D-1\ntitle: t\nstatus: doing\npriority: P2\n\
                 claim: {{by: pm-own, at: '2026-01-01T00:00:00Z'}}\n\
                 paths: ['src/ok.rs', '../escape', 'src/******************z']\n\
                 refs:\n  - kind: worktree\n    path: '{}'\n\
                 created: '2026-01-01T00:00:00Z'\n---\n",
                dir.display()
            ),
        )
        .unwrap();
        git(&dir, &["add", "-A"]);
        git(
            &dir,
            &[
                "commit",
                "-qm",
                "D-1: start cadence/d-1-x\n\nIssue: D-1\nActor: pm-a",
            ],
        );
        let issues = [issue_at(&dir, "demo", "D-1")];
        let refs: Vec<&Issue> = issues.iter().collect();
        let lanes = open_lanes(&dir, &refs, &BTreeMap::new());
        assert_eq!(lanes.len(), 1);
        assert_eq!(lanes[0].planned, vec!["src/ok.rs".to_string()]);
        assert_eq!(lanes[0].side.pm.as_deref(), Some("pm-a"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A throwaway git repo under TMPDIR — commits carry the fixture
    /// identity, and HOME is pinned to the dir so host config never
    /// leaks in.
    fn tmpdir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cad378-areas-{tag}-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-qb", "main"]);
        dir
    }

    fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("HOME", dir)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Load an issue the way the board does — the live file, comments
    /// included — without standing up the whole fixture.
    fn issue_at(pm_dir: &Path, project: &str, id: &str) -> Issue {
        crate::issue::board::find_issue(pm_dir, id)
            .unwrap_or_else(|_| panic!("{project}/{id} unreadable"))
    }
}
