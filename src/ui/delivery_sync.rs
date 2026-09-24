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
//! - **Only the project's own repos.** Each PR must be in its ticket's
//!   project remotes ([`crate::delivery::project_pr_refusal`]) before
//!   `gh` reads it.
//! - **Bounded.** One pass in flight ([`DeliverySync::tick`]); at most
//!   [`MAX_PER_PASS`] PRs per pass, least recently observed first; the
//!   interval is clamped to [`MIN_EVERY`]..=[`MAX_EVERY`]; a page view
//!   runs a pass at most every [`PAGE_GAP`] and never during a back-off;
//!   each failed pass doubles the wait up to [`MAX_BACKOFF`].
//! - **Failures are visible.** A failed pass is one Needs-you `info` row
//!   (`kind: delivery_sync`) until a pass succeeds.
//! - **`gh` output is data.** It is parsed into the observation's typed
//!   fields; nothing from it becomes a command. Error text shown in the
//!   row is one line, bounded and secret-redacted. The board never
//!   handles a GitHub token: `gh` reads its own.
//!
//! A read-only board runs no sync: an observation writes the loop's
//! record and may turn auto-merge off on GitHub.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
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
/// PRs one pass reads; the rest wait for the next pass.
pub const MAX_PER_PASS: usize = 20;
/// Bytes of error text the Needs-you row shows.
const ERROR_MAX: usize = 300;

/// What started a pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trigger {
    Timer,
    PageView,
}

/// What one pass found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pass {
    /// No loop awaits GitHub: nothing was read.
    Idle,
    /// Every awaited PR was read and reported.
    Synced(usize),
    /// The pass could not do its job — the Needs-you row's text.
    Failed(String),
}

#[derive(Default)]
struct Sched {
    /// When the timer is next due; `None` is now.
    next_due: Option<Instant>,
    last_start: Option<Instant>,
    failures: u32,
    /// The failing pass's text and when the failures began.
    problem: Option<(String, i64)>,
    /// A page view asked for a pass.
    nudged: bool,
}

/// One board's sync schedule and its last problem.
pub struct DeliverySync {
    every: Duration,
    gh: PathBuf,
    running: AtomicBool,
    sched: Mutex<Sched>,
    wake: Condvar,
}

impl DeliverySync {
    /// `every` defaults to [`EVERY`] and is clamped to
    /// [`MIN_EVERY`]..=[`MAX_EVERY`]; `gh` is the operator's `gh`. The
    /// timer's first pass is one interval out; the first page view runs
    /// one at once.
    pub fn new(every: Option<Duration>, gh: PathBuf) -> Arc<Self> {
        let every = every.unwrap_or(EVERY).clamp(MIN_EVERY, MAX_EVERY);
        Arc::new(DeliverySync {
            every,
            gh,
            running: AtomicBool::new(false),
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

    /// The wait after `failures` failed passes in a row: the interval,
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
    /// time. A page view is due as well when no back-off is running and
    /// the last pass began at least [`PAGE_GAP`] ago.
    pub fn tick(&self, trigger: Trigger, run: impl FnOnce() -> Pass) -> Option<Pass> {
        let now = Instant::now();
        {
            let st = self.lock();
            let timer_due = st.next_due.is_none_or(|d| now >= d);
            let page_due = trigger == Trigger::PageView
                && st.failures == 0
                && st
                    .last_start
                    .is_none_or(|s| now.duration_since(s) >= PAGE_GAP);
            if !timer_due && !page_due {
                return None;
            }
        }
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
        self.lock().last_start = Some(now);
        let pass = run();
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

    /// The Needs-you `info` row for the current problem, if any.
    pub fn needs_row(&self, now: i64) -> Option<Value> {
        let st = self.lock();
        let (why, since) = st.problem.as_ref()?;
        Some(crate::overview::delivery_sync_row(why, *since, now))
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
                pass(&state_dir, &pm_dir, &self.gh, || {
                    super::home::board_is_operator(&state_dir)
                })
            });
        }
    }
}

/// Start the board's sync thread; the handle serves page-view nudges
/// and the Needs-you row.
pub fn start(
    state_dir: &Path,
    pm_dir: &Path,
    every: Option<Duration>,
    gh: PathBuf,
) -> Arc<DeliverySync> {
    let sync = DeliverySync::new(every, gh);
    let (worker, state_dir, pm_dir) = (sync.clone(), state_dir.to_path_buf(), pm_dir.to_path_buf());
    std::thread::Builder::new()
        .name("delivery-sync".into())
        .spawn(move || worker.run_forever(state_dir, pm_dir))
        .ok();
    sync
}

fn epoch_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Error text fit for the Needs-you row: its first line, secret-shaped
/// spans redacted, at most [`ERROR_MAX`] bytes. It is shown as text and
/// never run.
pub fn row_text(raw: &str) -> String {
    let line = raw
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let clean = crate::secret::redact_text(line)
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

/// The records one pass reads, least recently observed first, at most
/// [`MAX_PER_PASS`].
pub fn awaited(records: Vec<Record>) -> Vec<Record> {
    let mut due: Vec<Record> = records
        .into_iter()
        .filter(delivery::awaiting_github)
        .collect();
    due.sort_by_key(|r| {
        (
            r.observed.as_ref().map_or(i64::MIN, |o| o.at),
            r.issue.clone(),
        )
    });
    due.truncate(MAX_PER_PASS);
    due
}

/// One pass: read the loop from the daemon; when a PR awaits GitHub,
/// prove the board is the operator's, then read each PR in its
/// project's repos with the operator's `gh` and report it.
fn pass(state_dir: &Path, pm_dir: &Path, gh: &Path, is_operator: impl Fn() -> bool) -> Pass {
    let list = match client::rpc(state_dir, "delivery_list", json!({})) {
        Ok(v) => v,
        Err(e) => {
            return Pass::Failed(row_text(&format!(
                "merge decisions are not refreshing: the board cannot read the review loop \
                 from the daemon — {e}"
            )))
        }
    };
    let records: Vec<Record> = list["records"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| serde_json::from_value(r.clone()).ok())
        .collect();
    let due = awaited(records);
    if due.is_empty() {
        return Pass::Idle;
    }
    if !is_operator() {
        return Pass::Failed(
            "merge decisions are not refreshing: this board was not started by the operator, \
             so it does not read GitHub — restart it from the operator's shell \
             (`cadence ui start`) or run `cadence delivery sync`"
                .into(),
        );
    }
    let gh = gh.to_string_lossy();
    let mut errors = Vec::new();
    for rec in &due {
        let Some(url) = rec.pr.as_deref() else {
            continue;
        };
        match delivery::project_pr_refusal(pm_dir, &rec.project, url) {
            Ok(None) => {}
            Ok(Some(why)) => {
                errors.push(format!("{} {why}", rec.issue));
                continue;
            }
            Err(e) => {
                errors.push(format!("{}: {e}", rec.issue));
                continue;
            }
        }
        let row = delivery::sync_pr(state_dir, &rec.issue, url, &gh);
        if let Some(e) = row["error"].as_str() {
            errors.push(format!("{}: {e}", rec.issue));
        }
    }
    // The next overview read shows what the daemon just recorded.
    super::read_model::get(state_dir, pm_dir).invalidate();
    match errors.first() {
        None => Pass::Synced(due.len()),
        Some(first) => {
            let more = match errors.len() {
                1 => String::new(),
                n => format!(" (and {} more)", n - 1),
            };
            Pass::Failed(row_text(&format!(
                "merge decisions are not refreshing: the board's GitHub read failed for \
                 {first}{more}"
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Barrier;

    fn sync(every: u64) -> Arc<DeliverySync> {
        DeliverySync::new(Some(Duration::from_secs(every)), PathBuf::from("gh"))
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
    /// [`PAGE_GAP`]; a failure backs off, doubling, and no page view
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
        let row = s.needs_row(since + 5).unwrap();
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
        assert!(s.needs_row(0).is_none());
        assert_eq!(s.lock().failures, 0);
    }

    #[test]
    fn interval_and_backoff_are_bounded() {
        assert_eq!(sync(0).every, MIN_EVERY);
        assert_eq!(sync(86_400).every, MAX_EVERY);
        assert_eq!(DeliverySync::new(None, "gh".into()).every, EVERY);
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

    /// Only loops whose PR GitHub can change are read; a worker's ticket,
    /// a finished loop and a loop without a PR cost no `gh` call. At most
    /// [`MAX_PER_PASS`], least recently observed first.
    #[test]
    fn a_pass_reads_only_loops_awaiting_github() {
        use delivery::State::*;
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
        let ids: Vec<String> = awaited(all).into_iter().map(|r| r.issue).collect();
        assert_eq!(ids, ["D-2", "D-3", "D-4", "D-9"]);
        assert!(awaited(vec![rec("D-1", Working, true), rec("D-5", Escalated, true)]).is_empty());

        let many: Vec<Record> = (0..MAX_PER_PASS + 5)
            .map(|n| {
                let mut r = rec(&format!("D-{n}"), Passed, true);
                r.observed = Some(delivery::Observed {
                    at: 1000 - n as i64,
                    ..Default::default()
                });
                r
            })
            .collect();
        let due = awaited(many);
        assert_eq!(due.len(), MAX_PER_PASS);
        assert_eq!(due[0].issue, format!("D-{}", MAX_PER_PASS + 4));
    }

    /// Error text in the row is one line, bounded, and never carries a
    /// token-shaped span.
    #[test]
    fn row_text_is_one_redacted_bounded_line() {
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
    }
}
