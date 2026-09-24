//! CAD-403: when each issue's `status:` and `owner:` lines last changed,
//! for every issue at once — the status clock (CAD-253) and the claim
//! age of an owner-only issue (CAD-383) that `status`, `overview` and
//! the board's poll read.
//!
//! Each answer is what the per-issue
//! `git log -1 --format=%at -G '^<key>:' -- <project>/<id>/issue.md`
//! answers, but two `git log -G --name-only` walks answer them for the
//! whole tracker, and the result is cached in the tracker's git dir
//! (never tracked, so `git add -A` never commits it) keyed by the HEAD
//! it was walked at. An unchanged HEAD costs one `rev-parse` and a file
//! read; a HEAD that moved forward walks only the new commits; any
//! other move — a reset, a rewrite, a lost cache — walks the whole
//! history again, so a stale time is never served.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::proc::{run_bounded, BoundedError};

/// Bump when the cached body's meaning changes — an older body is
/// walked again instead of read.
const VERSION: u32 = 1;

/// The cache file, under the tracker's git dir.
const FILE: &str = "cadence-line-times.json";

/// Every issue file the walks look at.
const PATHSPEC: &str = "*/issue.md";

/// Last-change times of the `status:` and `owner:` lines, by issue
/// file (`<project>/<id>/issue.md`), as of `head`.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct LineTimes {
    version: u32,
    /// The tracker commit the times were read at; empty for a tracker
    /// with no git history, which has no times.
    head: String,
    status: HashMap<String, i64>,
    owner: HashMap<String, i64>,
}

impl LineTimes {
    /// The tracker's line times as of its current HEAD, within
    /// `timeout`. A directory that is not a repo, or a repo with no
    /// commit yet, has no times — every lookup answers `None`, as the
    /// per-issue `git log` did. `Err` only when git could not answer
    /// in time; the caller then has no times at all.
    pub fn load(pm_dir: &Path, timeout: Duration) -> std::result::Result<Self, String> {
        let deadline = Instant::now() + timeout;
        let Some((git_dir, head)) = locate(pm_dir, deadline)? else {
            return Ok(Self::default());
        };
        let file = git_dir.join(FILE);
        let cached = read(&file);
        if let Some(c) = &cached {
            if c.head == head {
                return Ok(cached.unwrap_or_default());
            }
        }
        // Only a fast-forward keeps the cached times: every commit in
        // `old..head` is newer than any the cache saw.
        let base = match cached {
            Some(c) if is_ancestor(pm_dir, &c.head, &head, deadline)? => Some(c),
            _ => None,
        };
        let range = match &base {
            Some(c) => format!("{}..{head}", c.head),
            None => head.clone(),
        };
        // The two walks are independent — run them side by side.
        let (status, owner) = std::thread::scope(|s| {
            let owner = s.spawn(|| walk(pm_dir, "owner", &range, deadline));
            let status = walk(pm_dir, "status", &range, deadline);
            let owner = owner
                .join()
                .unwrap_or_else(|_| Err("tracker line times: owner walk panicked".to_string()));
            (status, owner)
        });
        let (status, owner) = (status?, owner?);
        let mut times = base.unwrap_or_default();
        times.version = VERSION;
        times.head = head;
        times.status.extend(status);
        times.owner.extend(owner);
        write(&file, &times);
        Ok(times)
    }

    /// When the issue's `status:` line last changed.
    pub fn status_at(&self, project: &str, id: &str) -> Option<i64> {
        self.status.get(&rel(project, id)).copied()
    }

    /// When the issue's `owner:` line last changed.
    pub fn owner_at(&self, project: &str, id: &str) -> Option<i64> {
        self.owner.get(&rel(project, id)).copied()
    }
}

fn rel(project: &str, id: &str) -> String {
    format!("{project}/{id}/issue.md")
}

/// `git -C <pm_dir> <args>` within what is left of `deadline`. `Ok`
/// carries the exit status' success and stdout; `Err` is a git that
/// could not be run or did not finish in time.
fn git(
    pm_dir: &Path,
    args: &[&str],
    deadline: Instant,
) -> std::result::Result<(bool, String), String> {
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return Err("tracker line times: out of time".to_string());
    }
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(pm_dir).args(args);
    match run_bounded(&mut cmd, left) {
        Ok(out) => Ok((
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
        )),
        Err(BoundedError::Spawn(e)) => Err(format!("tracker line times: spawn git: {e}")),
        Err(e) => Err(format!("tracker line times: {e}")),
    }
}

/// The tracker's git dir and HEAD commit; `None` when it has neither.
fn locate(
    pm_dir: &Path,
    deadline: Instant,
) -> std::result::Result<Option<(PathBuf, String)>, String> {
    let (ok, out) = git(
        pm_dir,
        &[
            "rev-parse",
            "--absolute-git-dir",
            "--verify",
            "HEAD^{commit}",
        ],
        deadline,
    )?;
    let mut lines = out.lines().map(str::trim);
    match (ok, lines.next(), lines.next()) {
        (true, Some(dir), Some(head)) if !dir.is_empty() && !head.is_empty() => {
            Ok(Some((PathBuf::from(dir), head.to_string())))
        }
        _ => Ok(None),
    }
}

/// Whether `old` is an ancestor of (or equal to) `new`. An `old` the
/// repo no longer has is not.
fn is_ancestor(
    pm_dir: &Path,
    old: &str,
    new: &str,
    deadline: Instant,
) -> std::result::Result<bool, String> {
    if old.is_empty() {
        return Ok(false);
    }
    let (ok, _) = git(pm_dir, &["merge-base", "--is-ancestor", old, new], deadline)?;
    Ok(ok)
}

/// For each issue file, the author time of the newest commit in
/// `range` whose diff adds or removes a `<key>:` line in it — the first
/// one `git log` lists, as `git log -1 -G` would pick.
fn walk(
    pm_dir: &Path,
    key: &str,
    range: &str,
    deadline: Instant,
) -> std::result::Result<HashMap<String, i64>, String> {
    let pattern = format!("^{key}:");
    let (ok, out) = git(
        pm_dir,
        &[
            "-c",
            "core.quotePath=false",
            "log",
            "--no-renames",
            "--format=%x00%at",
            "--name-only",
            "-G",
            &pattern,
            range,
            "--",
            PATHSPEC,
        ],
        deadline,
    )?;
    if !ok {
        return Err(format!("tracker line times: git log {range} failed"));
    }
    Ok(parse_walk(&out))
}

/// `\0<epoch>` starts a commit; the non-empty lines after it are the
/// files it touched. The first time a file appears wins.
fn parse_walk(out: &str) -> HashMap<String, i64> {
    let mut times = HashMap::new();
    let mut at: Option<i64> = None;
    for line in out.lines() {
        if let Some(epoch) = line.strip_prefix('\0') {
            at = epoch.trim().parse().ok();
        } else if let (Some(at), false) = (at, line.is_empty()) {
            times.entry(line.to_string()).or_insert(at);
        }
    }
    times
}

fn read(file: &Path) -> Option<LineTimes> {
    let text = std::fs::read_to_string(file).ok()?;
    let times: LineTimes = serde_json::from_str(&text).ok()?;
    (times.version == VERSION && !times.head.is_empty()).then_some(times)
}

/// Temp-write then rename — a concurrent reader sees the old body or
/// the new one, never half. The temp name is per process so two
/// writers never share one. A write that fails leaves the next call to
/// walk again.
fn write(file: &Path, times: &LineTimes) {
    let Ok(body) = serde_json::to_string(times) else {
        return;
    };
    let tmp = file.with_extension(format!("{}.tmp", std::process::id()));
    if std::fs::write(&tmp, body).is_ok() && std::fs::rename(&tmp, file).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: Duration = Duration::from_secs(20);

    /// A tracker repo with one issue file per `(project, id)`.
    struct Tracker(tempfile::TempDir);

    impl Tracker {
        fn new() -> Self {
            let t = Tracker(tempfile::tempdir().unwrap());
            t.git(&["init", "-q"]);
            t
        }

        fn dir(&self) -> &Path {
            self.0.path()
        }

        fn git(&self, args: &[&str]) -> String {
            let out = Command::new("git")
                .arg("-C")
                .arg(self.dir())
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}: {out:?}");
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        }

        /// Write the issue's front lines and commit them at `at`.
        fn commit(&self, id: &str, lines: &[&str], at: i64) {
            let dir = self.dir().join("demo").join(id);
            std::fs::create_dir_all(&dir).unwrap();
            let body = format!("---\nid: {id}\n{}\n---\nbody\n", lines.join("\n"));
            std::fs::write(dir.join("issue.md"), body).unwrap();
            self.git(&["add", "-A"]);
            let date = format!("@{at} +0000");
            let out = Command::new("git")
                .arg("-C")
                .arg(self.dir())
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(["commit", "-q", "-m", id])
                .env("GIT_AUTHOR_DATE", &date)
                .env("GIT_COMMITTER_DATE", &date)
                .output()
                .unwrap();
            assert!(out.status.success(), "{out:?}");
        }

        /// The per-issue `git log -1 -G` answer the cache replaces.
        fn oracle(&self, key: &str, id: &str) -> Option<i64> {
            self.git(&[
                "log",
                "-1",
                "--format=%at",
                "-G",
                &format!("^{key}:"),
                "--",
                &format!("demo/{id}/issue.md"),
            ])
            .parse()
            .ok()
        }

        fn load(&self) -> LineTimes {
            LineTimes::load(self.dir(), T).unwrap()
        }

        fn assert_matches_git(&self, ids: &[&str]) {
            let times = self.load();
            for id in ids {
                assert_eq!(
                    times.status_at("demo", id),
                    self.oracle("status", id),
                    "{id} status"
                );
                assert_eq!(
                    times.owner_at("demo", id),
                    self.oracle("owner", id),
                    "{id} owner"
                );
            }
        }
    }

    #[test]
    fn no_repo_or_no_commit_has_no_times() {
        let plain = tempfile::tempdir().unwrap();
        let times = LineTimes::load(plain.path(), T).unwrap();
        assert_eq!(times.owner_at("demo", "D-1"), None);
        let t = Tracker::new();
        assert_eq!(t.load().status_at("demo", "D-1"), None);
    }

    /// Claim, release and unrelated commits: every answer equals the
    /// per-issue `git log -G`, and the cache follows HEAD forward.
    #[test]
    fn times_follow_claims_releases_and_commits() {
        let t = Tracker::new();
        t.commit("D-1", &["status: backlog"], 1_000);
        t.commit("D-2", &["status: ready", "owner: w2"], 2_000);
        t.assert_matches_git(&["D-1", "D-2", "D-3"]);
        let first = t.load();
        assert_eq!(first.status_at("demo", "D-1"), Some(1_000));
        assert_eq!(first.owner_at("demo", "D-1"), None);
        assert_eq!(first.owner_at("demo", "D-2"), Some(2_000));

        // A claim: owner set, status moves.
        t.commit("D-1", &["status: doing", "owner: w1"], 3_000);
        t.assert_matches_git(&["D-1", "D-2"]);
        assert_eq!(t.load().owner_at("demo", "D-1"), Some(3_000));
        // An edit that touches neither line leaves both times alone.
        t.commit("D-1", &["status: doing", "owner: w1", "title: x"], 4_000);
        t.assert_matches_git(&["D-1"]);
        assert_eq!(t.load().owner_at("demo", "D-1"), Some(3_000));
        // A release drops the owner line: that is a change too.
        t.commit("D-1", &["status: doing", "title: x"], 5_000);
        t.assert_matches_git(&["D-1", "D-2"]);
        assert_eq!(t.load().owner_at("demo", "D-1"), Some(5_000));
        // A later claim by someone else.
        t.commit("D-1", &["status: doing", "owner: w9", "title: x"], 6_000);
        t.assert_matches_git(&["D-1", "D-2"]);
        let times = t.load();
        assert_eq!(times.owner_at("demo", "D-1"), Some(6_000));
        assert_eq!(times.head, t.git(&["rev-parse", "HEAD"]));
    }

    /// A HEAD that moves back (or sideways) is walked from scratch —
    /// times from commits the tracker no longer has are never served.
    #[test]
    fn rewound_head_never_serves_stale_times() {
        let t = Tracker::new();
        t.commit("D-1", &["status: ready", "owner: w1"], 1_000);
        let base = t.git(&["rev-parse", "HEAD"]);
        t.commit("D-1", &["status: doing", "owner: w2"], 2_000);
        assert_eq!(t.load().owner_at("demo", "D-1"), Some(2_000));
        t.git(&["reset", "-q", "--hard", &base]);
        t.assert_matches_git(&["D-1"]);
        assert_eq!(t.load().owner_at("demo", "D-1"), Some(1_000));
        // Sideways: a different commit on top of the old base.
        t.commit("D-1", &["status: review", "owner: w3"], 3_000);
        t.assert_matches_git(&["D-1"]);
        assert_eq!(t.load().owner_at("demo", "D-1"), Some(3_000));
    }

    /// The cache is read when HEAD has not moved, and a corrupt or
    /// foreign-version body is walked again rather than trusted.
    #[test]
    fn cache_is_keyed_by_head_and_version() {
        let t = Tracker::new();
        t.commit("D-1", &["status: doing", "owner: w1"], 1_000);
        let file = PathBuf::from(t.git(&["rev-parse", "--absolute-git-dir"])).join(FILE);
        t.load();
        let mut body: LineTimes = read(&file).expect("cache written");
        // A planted time under the same HEAD is served — proof the
        // walk was skipped.
        body.owner.insert("demo/D-1/issue.md".into(), 42);
        write(&file, &body);
        assert_eq!(t.load().owner_at("demo", "D-1"), Some(42));
        // Another version is not trusted.
        body.version = VERSION + 1;
        write(&file, &body);
        assert_eq!(t.load().owner_at("demo", "D-1"), Some(1_000));
        std::fs::write(&file, "not json").unwrap();
        assert_eq!(t.load().owner_at("demo", "D-1"), Some(1_000));
        // The cache never shows up as tracker content.
        assert_eq!(t.git(&["status", "--porcelain"]), "");
    }

    /// The claim clock `status`/`overview` use: `claim.at` when the
    /// issue carries a claim, else the cached `owner:` time, and no age
    /// without line times or an owner.
    #[test]
    fn claim_clock_reads_claim_at_then_cached_owner_time() {
        use crate::issue::claim::Clock;
        use crate::issue::model::{Claim, Front};
        let t = Tracker::new();
        t.commit("D-1", &["status: doing", "owner: w1"], 1_000);
        let times = t.load();
        let mut front = Front::new("D-1", "t", "2026-09-23T00:00:00Z");
        front.owner = Some("w1".into());
        assert_eq!(Clock::new(Some(&times)).since("demo", &front), Some(1_000));
        assert_eq!(Clock::new(None).since("demo", &front), None);
        front.claim = Some(Claim {
            by: "pm".into(),
            at: "1970-01-01T00:33:20Z".into(),
            note: None,
        });
        assert_eq!(Clock::new(Some(&times)).since("demo", &front), Some(2_000));
        front.claim = None;
        front.owner = None;
        assert_eq!(Clock::new(Some(&times)).since("demo", &front), None);
    }

    #[test]
    fn parse_takes_the_newest_per_file() {
        let out = "\u{0}300\n\na/D-1/issue.md\nb/D-2/issue.md\n\u{0}200\n\na/D-1/issue.md\n";
        let times = parse_walk(out);
        assert_eq!(times["a/D-1/issue.md"], 300);
        assert_eq!(times["b/D-2/issue.md"], 300);
        assert_eq!(times.len(), 2);
    }
}
