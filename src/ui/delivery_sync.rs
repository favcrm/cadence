//! The board's delivery sync (CAD-446): merge decisions appear without
//! a terminal. The daemon never runs `gh`; the board process runs as
//! the operator, so it reads the worker loop's PRs from GitHub with the
//! operator's own `gh` ([`crate::delivery::sync_pr`], the same read as
//! `cadence delivery sync`) on a timer and when a page loads the
//! overview, and hands each observation to the daemon's operator-only
//! `delivery_observe`. A PASS on a green head so becomes a merge
//! decision in Needs-you within one interval of the board being open.
//!
//! The rules:
//! - **Only when GitHub can change something.** A pass reads the loop
//!   from the daemon and keeps the records that
//!   [`crate::delivery::awaiting_github`] names (reviewing, passed,
//!   enqueued, or auto-merge to turn off). None: no `gh` call, no
//!   operator check.
//! - **Only as the operator.** Before any `gh` call the board proves its
//!   own process is the operator's ([`super::home::board_is_operator`],
//!   the proof the daemon runs on the board's connection). A board an
//!   agent started runs no `gh` at all and says so in Needs-you.
//! - **One `gh`, fixed at start.** [`resolve_gh`] turns the board's `gh`
//!   into an absolute path once, from absolute `PATH` entries only; the
//!   board never looks it up again, so a `gh` planted on `PATH` later is
//!   never run. `/api/meta` shows which one it is.
//! - **Only the project's own repos.** Each PR must be in its ticket's
//!   project remotes, read from the daemon's tracker
//!   ([`crate::delivery::project_pr_refusal`]), before `gh` reads it.
//! - **Bounded.** One pass in flight ([`DeliverySync::tick`]); at most
//!   [`MAX_PER_PASS`] PRs per pass, least recently attempted first; the
//!   interval is clamped to [`MIN_EVERY`]..=[`MAX_EVERY`]; a page view
//!   runs a pass at most every [`PAGE_GAP`], at most [`PAGE_BUDGET`]
//!   times per [`PAGE_WINDOW`], and never during a back-off.
//! - **Failures are isolated and visible.** A PR that cannot be read
//!   backs off on its own (doubling, up to [`MAX_BACKOFF`]) while the
//!   others keep their interval; one Needs-you `info` row names each
//!   failing ticket and why. Only a failure of the pass itself (the
//!   daemon unreachable, no usable `gh`, not the operator, a crash)
//!   backs off the whole sync, with its own row.
//! - **`gh` output is data.** It is parsed into the observation's typed
//!   fields; nothing from it becomes a command. Error text shown in a
//!   row is one line of printable text, bounded, with URL credentials
//!   and secret-shaped spans redacted. The board never handles a GitHub
//!   token: `gh` reads its own.
//!
//! A read-only board runs no sync: an observation writes the loop's
//! record and may turn auto-merge off on GitHub.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, LazyLock, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::client;
use crate::delivery::{self, Record};

/// The timer's default period.
pub const EVERY: Duration = Duration::from_secs(60);
/// The shortest period a board may be given.
pub const MIN_EVERY: Duration = Duration::from_secs(1);
/// The longest period a board may be given.
pub const MAX_EVERY: Duration = Duration::from_secs(10 * 60);
/// The longest wait after repeated failures.
pub const MAX_BACKOFF: Duration = Duration::from_secs(15 * 60);
/// A page view runs a pass at most this often.
pub const PAGE_GAP: Duration = Duration::from_secs(15);
/// Page-view passes allowed per [`PAGE_WINDOW`]: `/api/overview` needs
/// no login, so a local client polling it cannot drive `gh` harder than
/// this.
pub const PAGE_BUDGET: usize = 4;
pub const PAGE_WINDOW: Duration = Duration::from_secs(10 * 60);
/// PRs one pass reads; the rest wait for the next pass.
pub const MAX_PER_PASS: usize = 20;
/// Bytes of one error text a row shows.
const ERROR_MAX: usize = 300;
/// Failing tickets a row names before "and N more".
const ROW_TICKETS: usize = 3;

/// What started a pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trigger {
    Timer,
    PageView,
}

/// What one pass found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pass {
    /// No loop is due: nothing was read.
    Idle,
    /// This many PRs were attempted; each one's outcome is its own.
    Synced(usize),
    /// The pass itself could not run — the Needs-you row's text.
    Failed(String),
}

/// One ticket's own schedule.
struct RecSync {
    failures: u32,
    next_due: Option<Instant>,
    last_attempt: Option<Instant>,
    /// Why the last attempt failed, and when the failures began.
    error: Option<(String, i64)>,
}

#[derive(Default)]
struct Sched {
    /// When the timer is next due; `None` is now.
    next_due: Option<Instant>,
    last_start: Option<Instant>,
    /// Failed passes in a row.
    failures: u32,
    /// The failing pass's text and when the failures began.
    problem: Option<(String, i64)>,
    /// A page view asked for a pass.
    nudged: bool,
    /// When page-view passes ran, within the last [`PAGE_WINDOW`].
    page_runs: Vec<Instant>,
    records: HashMap<String, RecSync>,
}

/// One board's sync schedule and its problems.
pub struct DeliverySync {
    every: Duration,
    /// The `gh` fixed at start ([`resolve_gh`]), or why there is none.
    gh: std::result::Result<PathBuf, String>,
    running: AtomicBool,
    /// CAD-482: this board declared itself its operator's by arming the
    /// test seam — the sync's `board_is_operator` answer in a pane.
    seam_armed: AtomicBool,
    sched: Mutex<Sched>,
    wake: Condvar,
}

impl DeliverySync {
    /// `every` defaults to [`EVERY`] and is clamped to
    /// [`MIN_EVERY`]..=[`MAX_EVERY`]; `gh` is the resolved operator's
    /// `gh`. The timer's first pass is one interval out; the first page
    /// view runs one at once.
    pub fn new(every: Option<Duration>, gh: std::result::Result<PathBuf, String>) -> Arc<Self> {
        let every = every.unwrap_or(EVERY).clamp(MIN_EVERY, MAX_EVERY);
        Arc::new(DeliverySync {
            every,
            gh,
            running: AtomicBool::new(false),
            seam_armed: AtomicBool::new(false),
            sched: Mutex::new(Sched {
                next_due: Some(Instant::now() + every),
                ..Sched::default()
            }),
            wake: Condvar::new(),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Sched> {
        self.sched.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The wait after `failures` failures in a row: the interval,
    /// doubled per failure, at most [`MAX_BACKOFF`].
    fn backoff(&self, failures: u32) -> Duration {
        let factor = 1u32.checked_shl(failures.min(20)).unwrap_or(u32::MAX);
        self.every
            .checked_mul(factor)
            .unwrap_or(MAX_BACKOFF)
            .min(MAX_BACKOFF.max(self.every))
    }

    /// Run one pass with `run` if `trigger` makes one due and none is in
    /// flight; `None` when it did not run. The timer is due at its next
    /// time. A page view is due as well when no back-off is running, the
    /// last pass began at least [`PAGE_GAP`] ago and the page budget is
    /// not spent. A panic in `run` is a failed pass, not a dead thread.
    pub fn tick(&self, trigger: Trigger, run: impl FnOnce() -> Pass) -> Option<Pass> {
        let now = Instant::now();
        let by_page = {
            let mut st = self.lock();
            st.page_runs
                .retain(|t| now.duration_since(*t) < PAGE_WINDOW);
            let timer_due = st.next_due.is_none_or(|d| now >= d);
            let page_due = trigger == Trigger::PageView
                && st.failures == 0
                && st.page_runs.len() < PAGE_BUDGET
                && st
                    .last_start
                    .is_none_or(|s| now.duration_since(s) >= PAGE_GAP);
            if !timer_due && !page_due {
                return None;
            }
            !timer_due
        };
        // One pass in flight: every trigger that finds one running is
        // dropped, not queued.
        if self.running.swap(true, Ordering::AcqRel) {
            return None;
        }
        struct Release<'a>(&'a AtomicBool);
        impl Drop for Release<'_> {
            fn drop(&mut self) {
                self.0.store(false, Ordering::Release);
            }
        }
        let _release = Release(&self.running);
        {
            let mut st = self.lock();
            st.last_start = Some(now);
            if by_page {
                st.page_runs.push(now);
            }
        }
        let pass =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(run)).unwrap_or_else(|panic| {
                let what = panic
                    .downcast_ref::<String>()
                    .map(String::as_str)
                    .or_else(|| panic.downcast_ref::<&str>().copied())
                    .unwrap_or("a panic");
                Pass::Failed(row_text(&format!(
                    "merge decisions are not refreshing: the board's delivery sync failed \
                     unexpectedly — {what}"
                )))
            });
        let done = Instant::now();
        let mut st = self.lock();
        match &pass {
            Pass::Idle | Pass::Synced(_) => {
                st.failures = 0;
                st.problem = None;
                st.next_due = Some(done + self.every);
            }
            Pass::Failed(why) => {
                st.failures = st.failures.saturating_add(1);
                let since = st.problem.as_ref().map_or_else(epoch_now, |p| p.1);
                st.problem = Some((why.clone(), since));
                st.next_due = Some(done + self.backoff(st.failures));
            }
        }
        Some(pass)
    }

    /// A page view: wake the board's sync thread, which runs a pass if
    /// [`Self::tick`] finds one due. Never blocks the request.
    pub fn nudge(&self) {
        self.lock().nudged = true;
        self.wake.notify_one();
    }

    /// The records this pass reads: those awaiting GitHub whose own
    /// back-off has run out, least recently attempted first (never
    /// attempted before all), at most [`MAX_PER_PASS`]. A ticket that
    /// left the loop's awaited states drops its schedule and its error.
    pub fn select(&self, records: Vec<Record>, now: Instant) -> Vec<Record> {
        let mut st = self.lock();
        let awaited: Vec<Record> = records
            .into_iter()
            .filter(delivery::awaiting_github)
            .collect();
        st.records
            .retain(|issue, _| awaited.iter().any(|r| &r.issue == issue));
        let mut due: Vec<(Option<Instant>, Record)> = awaited
            .into_iter()
            .filter_map(|r| {
                let mine = st.records.get(&r.issue);
                let ready = mine.and_then(|m| m.next_due).is_none_or(|d| now >= d);
                ready.then(|| (mine.and_then(|m| m.last_attempt), r))
            })
            .collect();
        due.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.issue.cmp(&b.1.issue)));
        due.truncate(MAX_PER_PASS);
        due.into_iter().map(|(_, r)| r).collect()
    }

    /// Record one ticket's attempt: a success clears its back-off and
    /// error; a failure backs it off alone, doubling.
    pub fn record(&self, issue: &str, outcome: std::result::Result<(), String>, at: Instant) {
        let mut st = self.lock();
        let entry = st.records.entry(issue.to_string()).or_insert(RecSync {
            failures: 0,
            next_due: None,
            last_attempt: None,
            error: None,
        });
        entry.last_attempt = Some(at);
        match outcome {
            Ok(()) => {
                entry.failures = 0;
                entry.next_due = None;
                entry.error = None;
            }
            Err(why) => {
                entry.failures = entry.failures.saturating_add(1);
                let since = entry.error.as_ref().map_or_else(epoch_now, |e| e.1);
                entry.error = Some((row_text(&why), since));
                let wait = self.backoff(entry.failures);
                entry.next_due = Some(at + wait);
            }
        }
    }

    /// The Needs-you `info` rows: the pass's own problem, and one row
    /// naming every ticket whose PR the board cannot read and why.
    pub fn needs_rows(&self, now: i64) -> Vec<Value> {
        let st = self.lock();
        let mut rows = Vec::new();
        if let Some((why, since)) = &st.problem {
            rows.push(crate::overview::delivery_sync_row(
                why,
                "delivery-sync",
                *since,
                now,
            ));
        }
        let mut failing: Vec<(&String, &(String, i64))> = st
            .records
            .iter()
            .filter_map(|(issue, r)| r.error.as_ref().map(|e| (issue, e)))
            .collect();
        failing.sort_by(|a, b| a.0.cmp(b.0));
        if let Some(since) = failing.iter().map(|(_, e)| e.1).min() {
            let named: Vec<String> = failing
                .iter()
                .take(ROW_TICKETS)
                .map(|(issue, (why, _))| format!("{issue} — {why}"))
                .collect();
            let more = match failing.len().saturating_sub(ROW_TICKETS) {
                0 => String::new(),
                n => format!("; and {n} more"),
            };
            let title = format!(
                "merge decisions are not refreshing for {} ticket(s): the board's GitHub \
                 read failed for {}{more}",
                failing.len(),
                named.join("; ")
            );
            let mut row =
                crate::overview::delivery_sync_row(&title, "delivery-sync-tickets", since, now);
            row["tickets"] = json!(failing
                .iter()
                .map(|(issue, (why, since))| json!({"issue": issue, "error": why, "since": since}))
                .collect::<Vec<_>>());
            rows.push(row);
        }
        rows
    }

    /// What `/api/meta` shows: the `gh` this board runs and its period.
    pub fn meta(&self) -> Value {
        match &self.gh {
            Ok(gh) => json!({"gh": gh, "every_secs": self.every.as_secs()}),
            Err(e) => json!({"gh": null, "gh_error": e, "every_secs": self.every.as_secs()}),
        }
    }

    /// The board's sync thread: wait for the timer or a page view, then
    /// run a pass. Never returns.
    fn run_forever(self: Arc<Self>, state_dir: PathBuf, pm_dir: PathBuf) {
        loop {
            let trigger = {
                let mut st = self.lock();
                if !st.nudged {
                    let wait = st
                        .next_due
                        .map_or(Duration::ZERO, |d| {
                            d.saturating_duration_since(Instant::now())
                        })
                        .min(self.every);
                    st = self
                        .wake
                        .wait_timeout(st, wait)
                        .map(|(g, _)| g)
                        .unwrap_or_else(|e| e.into_inner().0);
                }
                if std::mem::take(&mut st.nudged) {
                    Trigger::PageView
                } else {
                    Trigger::Timer
                }
            };
            self.tick(trigger, || {
                let pass = || {
                    self.pass(&state_dir, &pm_dir, || {
                        super::home::board_is_operator(
                            &state_dir,
                            self.seam_armed.load(Ordering::Relaxed),
                        )
                    })
                };
                // CAD-482: an armed board's daemon calls assert the
                // identity the process declares — its own
                // `CADENCE_TEST_AS`, or the operator's when it asserts
                // nothing (an armed fixture board is its operator's, the
                // same rule `board_is_operator` answers with). Without
                // the seam this compiles out and the calls keep their
                // ambient caller.
                if self.seam_armed.load(Ordering::Relaxed) {
                    crate::test_seam::scoped(
                        crate::test_seam::env_asserted()
                            .unwrap_or(crate::test_seam::Asserted::Operator),
                        pass,
                    )
                } else {
                    pass()
                }
            });
        }
    }

    /// One pass: read the loop from the daemon; when a PR is due, prove
    /// the board is the operator's, then read each due PR in its
    /// project's repos with the fixed `gh` and report it. One ticket's
    /// failure is that ticket's.
    fn pass(&self, state_dir: &Path, board_pm: &Path, is_operator: impl Fn() -> bool) -> Pass {
        let list = match client::rpc(state_dir, "delivery_list", json!({})) {
            Ok(v) => v,
            Err(e) => {
                return Pass::Failed(row_text(&format!(
                    "merge decisions are not refreshing: the board cannot read the review \
                     loop from the daemon — {e}"
                )))
            }
        };
        let records: Vec<Record> = list["records"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|r| serde_json::from_value(r.clone()).ok())
            .collect();
        let due = self.select(records, Instant::now());
        if due.is_empty() {
            return Pass::Idle;
        }
        if !is_operator() {
            return Pass::Failed(
                "merge decisions are not refreshing: this board was not started by the \
                 operator, so it does not read GitHub — restart it from the operator's shell \
                 (`cadence ui start`) or run `cadence delivery sync`"
                    .into(),
            );
        }
        let gh = match &self.gh {
            Ok(gh) => gh.to_string_lossy().to_string(),
            Err(e) => {
                return Pass::Failed(row_text(&format!(
                    "merge decisions are not refreshing: the board found no usable `gh` when \
                     it started — {e}"
                )))
            }
        };
        // The project remotes are the daemon's tracker's — the one its
        // done-report check used — not whatever this board was given.
        let Some(pm_dir) = list["pm_dir"].as_str().map(PathBuf::from) else {
            return Pass::Failed(
                "merge decisions are not refreshing: the daemon does not say which tracker \
                 holds the project remotes — restart it on this build"
                    .into(),
            );
        };
        for rec in &due {
            let Some(url) = rec.pr.as_deref() else {
                continue;
            };
            let outcome = match delivery::project_pr_refusal(&pm_dir, &rec.project, url) {
                Ok(None) => {
                    let row = delivery::sync_pr(state_dir, &rec.issue, url, &gh);
                    match row["error"].as_str() {
                        Some(e) => Err(e.to_string()),
                        None => Ok(()),
                    }
                }
                Ok(Some(why)) => Err(why),
                Err(e) => Err(e.to_string()),
            };
            self.record(&rec.issue, outcome, Instant::now());
        }
        // The next overview read shows what the daemon just recorded.
        super::read_model::get(state_dir, board_pm).invalidate();
        Pass::Synced(due.len())
    }
}

/// Start the board's sync thread; the handle serves page-view nudges,
/// `/api/meta` and the Needs-you rows. A thread that cannot start is
/// logged and shown as the sync's problem.
pub fn start(
    state_dir: &Path,
    pm_dir: &Path,
    every: Option<Duration>,
    gh: std::result::Result<PathBuf, String>,
    seam_armed: bool,
) -> Arc<DeliverySync> {
    let sync = DeliverySync::new(every, gh);
    sync.seam_armed.store(seam_armed, Ordering::Relaxed);
    let (worker, state_dir, pm_dir) = (sync.clone(), state_dir.to_path_buf(), pm_dir.to_path_buf());
    if let Err(e) = std::thread::Builder::new()
        .name("delivery-sync".into())
        .spawn(move || worker.run_forever(state_dir, pm_dir))
    {
        let why = format!(
            "merge decisions are not refreshing: the board could not start its delivery sync \
             — {e}"
        );
        eprintln!("cadence ui: {why}");
        sync.lock().problem = Some((why, epoch_now()));
    }
    sync
}

/// The board's `gh` as an absolute path, fixed once at start: `gh` when
/// it names a path (made absolute), else the first executable `gh` in
/// an ABSOLUTE `PATH` entry — an empty or relative entry would resolve
/// against whatever directory the board runs in.
pub fn resolve_gh(gh: &Path, path_env: Option<&OsStr>) -> std::result::Result<PathBuf, String> {
    use std::os::unix::fs::PermissionsExt;
    let runnable = |p: &Path| {
        std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    };
    if gh.components().count() > 1 || gh.is_absolute() {
        let abs = std::path::absolute(gh).map_err(|e| format!("{}: {e}", gh.display()))?;
        return if runnable(&abs) {
            Ok(abs)
        } else {
            Err(format!("{} is not an executable file", abs.display()))
        };
    }
    std::env::split_paths(path_env.unwrap_or_default())
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(gh))
        .find(|p| runnable(p))
        .ok_or_else(|| format!("no executable `{}` on an absolute PATH entry", gh.display()))
}

fn epoch_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// `scheme://user:pass@` — credentials in a URL.
static URL_CREDENTIALS: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"([A-Za-z][A-Za-z0-9+.\-]*://)[^/\s@]+@").expect("static regex")
});

/// A bidirectional-text or invisible formatting control.
fn is_bidi_or_format(c: char) -> bool {
    matches!(c,
        '\u{061C}' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}'
        | '\u{2060}'..='\u{2069}' | '\u{FEFF}')
}

/// Error text fit for a Needs-you row: its first line, control and
/// bidi characters removed, URL credentials and secret-shaped spans
/// redacted, at most [`ERROR_MAX`] bytes. It is shown as text and never
/// run.
pub fn row_text(raw: &str) -> String {
    let line = raw
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let printable: String = line
        .chars()
        .filter(|c| !c.is_control() && !is_bidi_or_format(*c))
        .collect();
    let no_userinfo = URL_CREDENTIALS.replace_all(&printable, "${1}[REDACTED]@");
    let clean = crate::secret::redact_text(&no_userinfo)
        .unwrap_or_else(|_| "(error text withheld: it could not be scanned for secrets)".into());
    if clean.len() <= ERROR_MAX {
        return clean;
    }
    let mut end = ERROR_MAX;
    while !clean.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &clean[..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Barrier;

    fn sync(every: u64) -> Arc<DeliverySync> {
        DeliverySync::new(
            Some(Duration::from_secs(every)),
            Ok(PathBuf::from("/usr/bin/gh")),
        )
    }

    /// Concurrent triggers (the timer and many page views at once) run
    /// exactly one pass; the rest are dropped while it is in flight.
    #[test]
    fn one_pass_in_flight_under_concurrent_triggers() {
        let s = sync(60);
        let (runs, live, peak) = (
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicUsize::new(0)),
        );
        s.lock().next_due = None;
        let gate = Arc::new(Barrier::new(8));
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let (s, runs, live, peak, gate) = (
                    s.clone(),
                    runs.clone(),
                    live.clone(),
                    peak.clone(),
                    gate.clone(),
                );
                std::thread::spawn(move || {
                    gate.wait();
                    let trigger = if i % 2 == 0 {
                        Trigger::Timer
                    } else {
                        Trigger::PageView
                    };
                    s.tick(trigger, || {
                        runs.fetch_add(1, Ordering::SeqCst);
                        let now = live.fetch_add(1, Ordering::SeqCst) + 1;
                        peak.fetch_max(now, Ordering::SeqCst);
                        std::thread::sleep(Duration::from_millis(300));
                        live.fetch_sub(1, Ordering::SeqCst);
                        Pass::Synced(1)
                    })
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        assert_eq!(peak.load(Ordering::SeqCst), 1);
        // The in-flight flag is released: the next due pass runs.
        s.lock().next_due = None;
        assert_eq!(s.tick(Trigger::Timer, || Pass::Idle), Some(Pass::Idle));
    }

    /// The timer waits its interval; a page view is due only past
    /// [`PAGE_GAP`]; a failed pass backs off, doubling, and no page view
    /// cuts a back-off short; a success clears it.
    #[test]
    fn timer_page_view_and_backoff_schedule() {
        let s = sync(60);
        // The timer's first pass is an interval out; a page view runs one.
        assert_eq!(s.tick(Trigger::Timer, || unreachable!()), None);
        assert_eq!(s.tick(Trigger::PageView, || Pass::Idle), Some(Pass::Idle));
        assert_eq!(s.tick(Trigger::Timer, || unreachable!()), None);
        assert_eq!(s.tick(Trigger::PageView, || unreachable!()), None);
        s.lock().last_start = Some(Instant::now() - PAGE_GAP);
        assert_eq!(
            s.tick(Trigger::PageView, || Pass::Synced(1)),
            Some(Pass::Synced(1))
        );

        // A failure: the row appears and the next pass waits 2 intervals.
        s.lock().next_due = None;
        let before = Instant::now();
        s.tick(Trigger::Timer, || Pass::Failed("boom".into()));
        let (due, since) = {
            let st = s.lock();
            (st.next_due.unwrap(), st.problem.clone().unwrap().1)
        };
        assert!(
            due >= before + Duration::from_secs(120),
            "{:?}",
            due - before
        );
        let rows = s.needs_rows(since + 5);
        assert_eq!(rows.len(), 1, "{rows:?}");
        let row = &rows[0];
        assert_eq!(row["kind"], "delivery_sync", "{row}");
        assert_eq!(row["audience"], "info", "{row}");
        assert_eq!(row["title"], "boom", "{row}");
        assert_eq!(row["command"], "cadence delivery sync", "{row}");
        assert_eq!(row["age"], 5, "{row}");
        // A page view does not cut the back-off short.
        s.lock().last_start = Some(Instant::now() - PAGE_GAP * 4);
        assert_eq!(s.tick(Trigger::PageView, || unreachable!()), None);
        // A second failure doubles it and keeps when the problem began.
        s.lock().next_due = None;
        let before = Instant::now();
        s.tick(Trigger::Timer, || Pass::Failed("boom again".into()));
        {
            let st = s.lock();
            assert!(st.next_due.unwrap() >= before + Duration::from_secs(240));
            assert_eq!(st.problem.as_ref().unwrap().1, since);
        }
        // A success clears the row and the back-off.
        s.lock().next_due = None;
        s.tick(Trigger::Timer, || Pass::Idle);
        assert!(s.needs_rows(0).is_empty());
        assert_eq!(s.lock().failures, 0);
    }

    /// Page views (an unauthenticated `/api/overview` poll) run at most
    /// [`PAGE_BUDGET`] passes per [`PAGE_WINDOW`], however they are
    /// spaced.
    #[test]
    fn page_views_spend_a_bounded_budget() {
        let s = sync(600);
        let mut ran = 0;
        for _ in 0..PAGE_BUDGET * 3 {
            s.lock().last_start = Some(Instant::now() - PAGE_GAP);
            if s.tick(Trigger::PageView, || Pass::Idle).is_some() {
                ran += 1;
            }
        }
        assert_eq!(ran, PAGE_BUDGET);
        // The timer is not charged to the budget, nor refused by it.
        s.lock().next_due = None;
        assert!(s.tick(Trigger::Timer, || Pass::Idle).is_some());
        // An old window no longer counts.
        let old = Instant::now() - PAGE_WINDOW;
        s.lock().page_runs = vec![old; PAGE_BUDGET];
        s.lock().last_start = Some(Instant::now() - PAGE_GAP);
        assert!(s.tick(Trigger::PageView, || Pass::Idle).is_some());
    }

    /// A panic inside a pass is a failed pass with a row; the sync goes
    /// on, and the in-flight flag is released.
    #[test]
    fn a_panicking_pass_is_a_failed_pass() {
        let s = sync(60);
        s.lock().next_due = None;
        let out = s.tick(Trigger::Timer, || panic!("kaboom"));
        match out {
            Some(Pass::Failed(why)) => assert!(why.contains("kaboom"), "{why}"),
            other => panic!("{other:?}"),
        }
        assert!(s.needs_rows(0)[0]["title"]
            .as_str()
            .unwrap()
            .contains("failed unexpectedly"));
        s.lock().next_due = None;
        assert_eq!(s.tick(Trigger::Timer, || Pass::Idle), Some(Pass::Idle));
    }

    #[test]
    fn interval_and_backoff_are_bounded() {
        assert_eq!(sync(0).every, MIN_EVERY);
        assert_eq!(sync(86_400).every, MAX_EVERY);
        assert_eq!(DeliverySync::new(None, Err("x".into())).every, EVERY);
        let s = sync(60);
        assert_eq!(s.backoff(1), Duration::from_secs(120));
        assert_eq!(s.backoff(3), Duration::from_secs(480));
        for n in [4, 20, 31, 32, u32::MAX] {
            assert_eq!(s.backoff(n), MAX_BACKOFF, "{n}");
        }
        assert_eq!(sync(1).backoff(u32::MAX), MAX_BACKOFF);
    }

    fn rec(issue: &str, state: delivery::State, pr: bool) -> Record {
        let mut r = Record::new(issue, "demo", "w1", 0);
        r.state = state;
        r.pr = pr.then(|| "https://github.com/acme/app/pull/7".to_string());
        r
    }

    fn ids(v: Vec<Record>) -> Vec<String> {
        v.into_iter().map(|r| r.issue).collect()
    }

    /// Only loops whose PR GitHub can change are read; a worker's ticket,
    /// a finished loop and a loop without a PR cost no `gh` call. At most
    /// [`MAX_PER_PASS`].
    #[test]
    fn a_pass_reads_only_loops_awaiting_github() {
        use delivery::State::*;
        let s = sync(60);
        let mut off = rec("D-9", Merged, true);
        off.disable_auto = true;
        let all = vec![
            rec("D-1", Working, true),
            rec("D-2", Reviewing, true),
            rec("D-3", Passed, true),
            rec("D-4", Enqueued, true),
            rec("D-5", Escalated, true),
            rec("D-6", Unstaffed, true),
            rec("D-7", Merged, true),
            rec("D-8", Passed, false),
            off,
        ];
        assert_eq!(
            ids(s.select(all, Instant::now())),
            ["D-2", "D-3", "D-4", "D-9"]
        );
        assert!(s
            .select(
                vec![rec("D-1", Working, true), rec("D-5", Escalated, true)],
                Instant::now()
            )
            .is_empty());
        let many: Vec<Record> = (0..MAX_PER_PASS + 5)
            .map(|n| rec(&format!("D-{n:02}"), Passed, true))
            .collect();
        assert_eq!(s.select(many, Instant::now()).len(), MAX_PER_PASS);
    }

    /// I1: one ticket whose PR cannot be read backs off alone — the
    /// others stay due every interval — and the row names it and why.
    /// Order is by last ATTEMPT, so a ticket that never succeeds cannot
    /// stay first and starve the rest past [`MAX_PER_PASS`].
    #[test]
    fn a_failing_ticket_backs_off_alone_and_never_starves_the_rest() {
        use delivery::State::*;
        let s = sync(60);
        let t0 = Instant::now();
        let both = || vec![rec("D-2", Passed, true), rec("D-3", Passed, true)];
        assert_eq!(ids(s.select(both(), t0)), ["D-2", "D-3"]);
        s.record("D-2", Ok(()), t0);
        s.record("D-3", Err("gh: HTTP 404\nmore".into()), t0);
        // One interval later: D-2 is due, D-3 waits out its back-off.
        let t1 = t0 + Duration::from_secs(60);
        assert_eq!(ids(s.select(both(), t1)), ["D-2"]);
        s.record("D-2", Ok(()), t1);
        let t2 = t0 + Duration::from_secs(120);
        // D-3's back-off ran out; it was attempted before D-2 was.
        assert_eq!(ids(s.select(both(), t2)), ["D-3", "D-2"]);
        s.record("D-3", Err("gh: HTTP 404".into()), t2);
        assert_eq!(ids(s.select(both(), t2 + Duration::from_secs(60))), ["D-2"]);
        let rows = s.needs_rows(epoch_now());
        assert_eq!(rows.len(), 1, "{rows:?}");
        let title = rows[0]["title"].as_str().unwrap();
        assert!(
            title.contains("1 ticket(s)") && title.contains("D-3 — gh: HTTP 404"),
            "{title}"
        );
        assert!(!title.contains("D-2") && !title.contains("more"), "{title}");
        assert_eq!(rows[0]["tickets"][0]["issue"], "D-3", "{}", rows[0]);
        // Last attempt, not last success: a ticket that always fails
        // comes AFTER one attempted less recently.
        let s = sync(60);
        let many: Vec<Record> = (0..MAX_PER_PASS + 1)
            .map(|n| rec(&format!("D-{n:02}"), Passed, true))
            .collect();
        let first = s.select(many.clone(), t0);
        assert_eq!(first.len(), MAX_PER_PASS);
        for r in &first {
            s.record(&r.issue, Ok(()), t0);
        }
        s.record("D-00", Err("x".into()), t0);
        // Past every back-off, the one never attempted comes first and
        // the failing D-00 is not ahead of it.
        let later = t0 + MAX_BACKOFF;
        let next = ids(s.select(many, later));
        assert_eq!(next[0], format!("D-{MAX_PER_PASS:02}"), "{next:?}");
        // A ticket that leaves the awaited states drops its error.
        s.select(vec![rec("D-01", Merged, true)], later);
        assert!(s.needs_rows(0).is_empty());
    }

    /// Error text in a row is one printable line, bounded, and never
    /// carries a token-shaped span or URL credentials.
    #[test]
    fn row_text_is_one_redacted_printable_bounded_line() {
        let token = format!("ghp_{}", "A1b2C3d4E5".repeat(4).get(..36).unwrap());
        let raw = format!("gh: HTTP 401 bad credentials {token}\nsecond line\n");
        let text = row_text(&raw);
        assert!(!text.contains(&token), "{text}");
        assert!(
            !text.contains('\n') && !text.contains("second line"),
            "{text}"
        );
        assert!(text.starts_with("gh: HTTP 401"), "{text}");
        let long = row_text(&"é".repeat(1000));
        assert!(long.len() <= ERROR_MAX + '…'.len_utf8(), "{}", long.len());
        // Controls and bidi overrides are dropped.
        let (esc, rlo, pdi) = (char::from(0x1b), '\u{202E}', '\u{2069}');
        let text = row_text(&format!("a{esc}[31mb{rlo}c{pdi}d\te"));
        assert_eq!(text, "a[31mbcde", "{text:?}");
        // URL userinfo is redacted.
        let pass = ["hunter", "2"].concat();
        let text = row_text(&format!("fetch https://bob:{pass}@github.com/o/r failed"));
        assert!(!text.contains(&pass) && !text.contains("bob"), "{text}");
        assert!(text.contains("https://[REDACTED]@github.com/o/r"), "{text}");
    }

    /// `gh` is fixed to an absolute path; a relative or empty `PATH`
    /// entry is never searched.
    #[test]
    fn gh_resolves_once_to_an_absolute_path() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::TempDir::new().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let gh = bin.join("gh");
        std::fs::write(&gh, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
        // A planted `gh` reachable through a RELATIVE entry (resolved
        // against this process's cwd), listed first.
        let planted_dir = dir.path().join("planted");
        std::fs::create_dir(&planted_dir).unwrap();
        let planted = planted_dir.join("gh");
        std::fs::write(&planted, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&planted, std::fs::Permissions::from_mode(0o755)).unwrap();
        let cwd = std::env::current_dir().unwrap();
        let up = "../".repeat(cwd.components().count() - 1);
        let relative = PathBuf::from(format!(
            "{up}{}",
            planted_dir.strip_prefix("/").unwrap().display()
        ));
        assert!(relative.join("gh").is_file(), "{}", relative.display());
        let path = std::env::join_paths([relative.as_path(), Path::new(""), &bin]).unwrap();
        assert_eq!(
            resolve_gh(Path::new("gh"), Some(&path)).unwrap(),
            gh,
            "the absolute entry, never the relative one"
        );
        let rel_only = std::env::join_paths([relative.as_path(), Path::new(".")]).unwrap();
        assert!(resolve_gh(Path::new("gh"), Some(&rel_only)).is_err());
        assert_eq!(resolve_gh(&gh, None).unwrap(), gh);
        let data = bin.join("data");
        std::fs::write(&data, "x").unwrap();
        assert!(resolve_gh(&data, None).is_err(), "not executable");
        assert!(resolve_gh(Path::new("gh"), None).is_err());
    }
}
