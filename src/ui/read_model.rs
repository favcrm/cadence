//! The board's read model (CAD-325) — what `/api/issues`, `/api/agents`,
//! `/api/overview` and `/api/stream` answer from instead of re-reading
//! the tracker and fanning out daemon RPCs on every request.
//!
//! - **Tracker**: every issue folder is parsed once and kept with a
//!   stamp — `(inode, size, mtime)` of `issue.md` and of each comment and
//!   artifact. A read re-stamps the folders (a few thousand `lstat`s, no
//!   reads) and re-parses only the folders whose stamp moved, so the
//!   index is never staler than the request: a `cadence issue …` write
//!   shows on the next read. Views (derived status, links, readiness)
//!   are cached per tracker generation, notes-dir stamp and job state.
//! - **Daemon**: one `agent_list` (with the per-agent board slice) and one
//!   `job_list` (with each job's tasks) make a snapshot. While a board
//!   stream is open, one shared watcher refreshes it every second and
//!   reads are served from it; with no stream open a read fetches it.
//! - **Overview**: built from the indexed views, the last daemon probe
//!   pass and the status/claim line times cached per tracker HEAD
//!   (`issue::line_times`, CAD-403). A tracker change rebuilds it on the
//!   next read; a time-stale one is served while a background pass
//!   refreshes it.
//! - **Stream**: one watcher per board, not one per connection, emits
//!   the legacy `issues|jobs|agents|monitoring` frames unchanged plus
//!   entity diffs — `issue`, `agent` and `plan` upserts and deletes
//!   keyed by id — so a client can patch its cache instead of refetching.
//!
//! Nothing here writes: tracker writes still go through `issue::write`,
//! daemon writes through their RPCs.

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, LazyLock, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

use serde_json::{json, Value};

use super::{
    agents_payload_from, board_job_list, dir_mtime, event_resources, value_fp, with_agents,
};
use crate::client;
use crate::issue::{board, model, plan, project, work, write as issue_write, Pm};
use crate::overview;

/// The shared stream watcher's poll period.
const WATCH_EVERY: Duration = Duration::from_secs(1);
/// While the watcher runs, reads serve its last kept daemon snapshot
/// whatever its age (`snapshot_age_secs` says how old) — the watcher's
/// next fetch replaces it, and a read never waits on a slow fetch. Past
/// this age a read joins or starts a fetch instead, so an unreachable
/// watcher cannot leave reads on data from long ago.
const DAEMON_MAX_STALE: Duration = Duration::from_secs(120);
/// A served snapshot this old reports `stale: true` beside its age.
const SNAPSHOT_STALE: Duration = Duration::from_secs(10);
/// An overview this young is served without a rebuild; an older one is
/// served while a background pass refreshes it.
const OVERVIEW_FRESH: Duration = Duration::from_secs(2);
/// The oldest overview ever served: past it a read builds synchronously.
/// A tracker-only rebuild reuses a daemon pass at most this old too.
const OVERVIEW_MAX_AGE: Duration = Duration::from_secs(10);
/// The watcher refreshes the overview on daemon changes only while
/// someone read it this recently.
const OVERVIEW_WANTED: Duration = Duration::from_secs(60);

/// `(state dir, PM dir)` → that board's model.
type Models = HashMap<(PathBuf, PathBuf), Arc<Model>>;

/// id → (diff fingerprint, the entity as a client renders it).
type Entities = HashMap<String, (u64, Value)>;

static MODELS: LazyLock<Mutex<Models>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// How one daemon snapshot is fetched. The board uses [`fetch_daemon`];
/// tests substitute a counting fake.
type Fetch = Box<dyn Fn(&Path) -> DaemonSnap + Send + Sync>;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The model for one board — one per `(state dir, PM dir)`, so every
/// request thread and stream of a server shares it.
pub(super) fn get(state_dir: &Path, pm_dir: &Path) -> Arc<Model> {
    lock(&MODELS)
        .entry((state_dir.to_path_buf(), pm_dir.to_path_buf()))
        .or_insert_with(|| Arc::new(Model::new(state_dir, pm_dir, Box::new(fetch_daemon))))
        .clone()
}

pub(super) struct Model {
    state_dir: PathBuf,
    pm_dir: PathBuf,
    tracker: Mutex<Tracker>,
    daemon: Mutex<DaemonSlot>,
    fetch: Fetch,
    overview: Mutex<OverviewState>,
    hub: Mutex<Hub>,
    /// When the daemon side last moved — a change the watcher saw, or a
    /// write through this board. Nothing read before it is served again.
    changed_at: Mutex<Option<Instant>>,
    /// One overview build at a time; other readers wait for its result.
    build_lock: Mutex<()>,
    overview_builds: AtomicU64,
    /// The builds a read ran itself — the cache missed. Waiting on an
    /// in-flight background rebuild is not counted: that is a fresh
    /// build, not a cache miss, and p95 covers the latency. Background
    /// refreshes are not counted here either: they scale with wall time.
    request_builds: AtomicU64,
    /// Issue folders [`board::load_all`] parsed on the watcher thread.
    /// The incremental `parses` counter does not see that scan.
    collection_parses: AtomicU64,
    /// Daemon fetches started, and callers that waited on one in flight
    /// instead of starting their own (CAD-1221).
    daemon_builds: AtomicU64,
    daemon_joins: AtomicU64,
}

/// Runs its closure on drop — resets a flag even when a panic unwinds.
struct OnDrop<F: FnMut()>(F);

impl<F: FnMut()> Drop for OnDrop<F> {
    fn drop(&mut self) {
        (self.0)()
    }
}

impl Model {
    fn new(state_dir: &Path, pm_dir: &Path, fetch: Fetch) -> Self {
        Model {
            state_dir: state_dir.to_path_buf(),
            pm_dir: pm_dir.to_path_buf(),
            tracker: Mutex::default(),
            daemon: Mutex::default(),
            fetch,
            overview: Mutex::default(),
            hub: Mutex::default(),
            changed_at: Mutex::default(),
            build_lock: Mutex::default(),
            overview_builds: AtomicU64::new(0),
            request_builds: AtomicU64::new(0),
            collection_parses: AtomicU64::new(0),
            daemon_builds: AtomicU64::new(0),
            daemon_joins: AtomicU64::new(0),
        }
    }
}

// ---------- tracker index ----------

/// One issue folder: its stamp and what it parsed to (`None` for a
/// folder `load_issue` refuses — skipped, like `load_all` does).
struct Entry {
    stamp: u64,
    loaded: Option<(board::Issue, String)>,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct ViewsKey {
    project: Option<String>,
    gen: u64,
    notes: u64,
    /// `None` — derived without job state (the overview's view).
    jobs: Option<u64>,
}

#[derive(Default)]
struct Tracker {
    /// `project/id` → entry.
    entries: HashMap<String, Entry>,
    /// Bumped whenever any folder was added, removed or re-parsed.
    gen: u64,
    views: HashMap<ViewsKey, Arc<Vec<board::View>>>,
    revs: Option<(u64, Arc<HashMap<String, String>>)>,
    /// Folders parsed since the model was made — the index's cost meter.
    parses: u64,
}

fn hash_meta(path: &Path, h: &mut DefaultHasher) {
    match path.symlink_metadata() {
        Ok(m) => (
            m.ino(),
            m.len(),
            m.mtime(),
            m.mtime_nsec(),
            m.file_type().is_symlink(),
        )
            .hash(h),
        Err(_) => 0u8.hash(h),
    }
}

/// Everything under `dir` that changes how its entries read — names,
/// inodes, sizes and mtimes, in name order.
fn hash_dir(dir: &Path, h: &mut DefaultHasher) {
    hash_meta(dir, h);
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut names: Vec<_> = entries.flatten().map(|e| e.file_name()).collect();
    names.sort();
    for name in names {
        name.hash(h);
        hash_meta(&dir.join(&name), h);
    }
}

/// An issue folder's stamp: `issue.md`, `comments/` and `artifacts/` —
/// the three things [`board::load_issue`] reads.
fn folder_stamp(dir: &Path) -> u64 {
    let mut h = DefaultHasher::new();
    hash_meta(&dir.join("issue.md"), &mut h);
    hash_dir(&dir.join("comments"), &mut h);
    hash_dir(&dir.join("artifacts"), &mut h);
    h.finish()
}

/// The tracker repo's `HEAD` stamp — `.git/HEAD`, the ref it names and
/// `packed-refs`, stat'd (and `HEAD` read): a commit, reset or pull moves
/// it without touching the issue folders, and the overview's git clocks
/// and `tracker_behind` row depend on it.
fn head_stamp(pm_dir: &Path) -> u64 {
    let git = pm_dir.join(".git");
    let mut h = DefaultHasher::new();
    let head = std::fs::read_to_string(git.join("HEAD")).unwrap_or_default();
    head.hash(&mut h);
    if let Some(name) = head.trim().strip_prefix("ref: ") {
        hash_meta(&git.join(name), &mut h);
    }
    hash_meta(&git.join("packed-refs"), &mut h);
    h.finish()
}

/// Issue counts per project key from entries the tracker already parsed.
fn project_counts(tracker: &Tracker) -> HashMap<String, usize> {
    let mut counts = HashMap::new();
    for entry in tracker.entries.values() {
        if let Some((issue, _)) = &entry.loaded {
            *counts.entry(issue.project.clone()).or_default() += 1;
        }
    }
    counts
}

/// A proposed plan and an intake row both render the issue title.
/// Every other title is entity-patch data, not an aggregate input.
fn overview_renders_title(front: &model::Front) -> bool {
    let proposed = front.plan.as_ref().is_some_and(|p| p.state == "proposed");
    let intake = front.status == "backlog" && front.tags.iter().any(|t| t == "intake");
    proposed || intake
}

fn overview_issue_fp(issue: &board::Issue) -> u64 {
    let mut front = issue.front.clone();
    if !overview_renders_title(&front) {
        front.title.clear();
    }
    let comments: Vec<Value> = issue
        .comments
        .iter()
        .map(|c| {
            json!({
                "name": c.name,
                "author": c.front.author,
                "at": c.front.at,
                "kind": c.front.kind,
                "body": c.body,
            })
        })
        .collect();
    value_fp(&json!({
        "project": issue.project,
        "front": front,
        "body": issue.body,
        "comments": comments,
        "artifacts": issue.artifacts,
    }))
}

/// Files beside issue folders — `PROJECT.md`, project config — whose
/// mtime can change the overview without a folder re-parse.
fn project_side_stamp(pm_dir: &Path) -> u64 {
    let mut h = DefaultHasher::new();
    hash_meta(&pm_dir.join("pm.yaml"), &mut h);
    for project in project::list(pm_dir).unwrap_or_default() {
        project.key.hash(&mut h);
        let dir = pm_dir.join(&project.key);
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut names: Vec<_> = entries.flatten().map(|e| e.file_name()).collect();
        names.sort();
        for name in names {
            let path = dir.join(&name);
            if model::valid_id(&name.to_string_lossy()) && path.is_dir() {
                continue;
            }
            name.hash(&mut h);
            hash_meta(&path, &mut h);
            if path.is_dir() {
                hash_dir(&path, &mut h);
            }
        }
    }
    h.finish()
}

/// Upstream refs only. A local `issue set` commit moves `HEAD` and the
/// branch ref; it does not move `refs/remotes`, which is what the
/// overview's behind-count row follows.
fn upstream_stamp(pm_dir: &Path) -> u64 {
    let git = pm_dir.join(".git");
    let mut h = DefaultHasher::new();
    hash_dir(&git.join("refs").join("remotes"), &mut h);
    hash_meta(&git.join("packed-refs"), &mut h);
    h.finish()
}

/// Everything already loaded that can change the rendered overview.
/// `delivery` is the CI observation stamp: it moves the overview
/// without an issue edit, so a title-only skip must not hide it.
fn overview_inputs_fp(pm: &Pm, tracker: &Tracker, delivery: u64) -> u64 {
    let mut parts: Vec<(String, u64)> = tracker
        .entries
        .iter()
        .map(|(key, entry)| {
            let fp = entry
                .loaded
                .as_ref()
                .map(|(issue, _)| overview_issue_fp(issue))
                .unwrap_or(0);
            (key.clone(), fp)
        })
        .collect();
    parts.sort();
    let mut h = DefaultHasher::new();
    notes_stamp(&pm.config.notes_dir()).hash(&mut h);
    project_side_stamp(&pm.dir).hash(&mut h);
    upstream_stamp(&pm.dir).hash(&mut h);
    delivery.hash(&mut h);
    parts.hash(&mut h);
    h.finish()
}

/// The notes dir's stamp — derived statuses read the tagged notes.
fn notes_stamp(dir: &Path) -> u64 {
    let mut h = DefaultHasher::new();
    dir.hash(&mut h);
    hash_dir(dir, &mut h);
    h.finish()
}

impl Tracker {
    /// Re-stamp every issue folder and re-parse the ones that moved.
    fn refresh(&mut self, pm_dir: &Path) {
        let mut seen = HashSet::new();
        let mut changed = false;
        for p in project::list(pm_dir).unwrap_or_default() {
            let dir = pm_dir.join(&p.key);
            if !board::is_real_dir(&dir) {
                continue;
            }
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let id = entry.file_name().to_string_lossy().to_string();
                // file_type() is lstat-style: a symlinked folder is skipped.
                let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
                if !model::valid_id(&id) || !is_dir {
                    continue;
                }
                let key = format!("{}/{id}", p.key);
                // Stamp first, then parse: a write racing the parse moves
                // the stamp again, so the next read re-parses it.
                let stamp = folder_stamp(&entry.path());
                seen.insert(key.clone());
                if self.entries.get(&key).is_some_and(|e| e.stamp == stamp) {
                    continue;
                }
                self.parses += 1;
                let loaded = board::load_issue(pm_dir, &p.key, &id).ok().map(|issue| {
                    let rev = issue_write::issue_rev(&issue.dir).unwrap_or_default();
                    (issue, rev)
                });
                self.entries.insert(key, Entry { stamp, loaded });
                changed = true;
            }
        }
        let before = self.entries.len();
        self.entries.retain(|k, _| seen.contains(k));
        if changed || self.entries.len() != before {
            self.gen += 1;
            self.views.clear();
            self.revs = None;
        }
    }

    /// The loaded issues, `load_all` order, optionally one project's.
    fn issues(&self, project: Option<&str>) -> Vec<board::Issue> {
        let mut issues: Vec<board::Issue> = self
            .entries
            .values()
            .filter_map(|e| e.loaded.as_ref())
            .filter(|(i, _)| project.is_none_or(|p| i.project == p))
            .map(|(i, _)| i.clone())
            .collect();
        issues.sort_by_key(|i| board::natural_key(&i.front.id));
        issues
    }

    fn views(
        &mut self,
        pm: &Pm,
        project: Option<&str>,
        jobs: Option<(&board::JobOutcomes, u64)>,
    ) -> (u64, Arc<Vec<board::View>>) {
        let notes_dir = pm.config.notes_dir();
        let key = ViewsKey {
            project: project.map(str::to_string),
            gen: self.gen,
            notes: notes_stamp(&notes_dir),
            jobs: jobs.map(|(_, fp)| fp),
        };
        let mut h = DefaultHasher::new();
        key.hash(&mut h);
        let fp = h.finish();
        if let Some(views) = self.views.get(&key) {
            return (fp, views.clone());
        }
        let issues = self.issues(project);
        let views = Arc::new(match jobs {
            Some((outcomes, _)) => board::views_with_jobs(&notes_dir, issues, outcomes),
            None => board::views(&notes_dir, issues),
        });
        // Keys of an older generation or notes stamp never hit again.
        self.views
            .retain(|k, _| k.gen == key.gen && k.notes == key.notes);
        self.views.insert(key, views.clone());
        (fp, views)
    }

    /// id → `issue.md` rev, for the cards' `rev` token.
    fn revs(&mut self) -> Arc<HashMap<String, String>> {
        if let Some((gen, revs)) = &self.revs {
            if *gen == self.gen {
                return revs.clone();
            }
        }
        let revs: Arc<HashMap<String, String>> = Arc::new(
            self.entries
                .values()
                .filter_map(|e| e.loaded.as_ref())
                .map(|(i, rev)| (i.front.id.clone(), rev.clone()))
                .collect(),
        );
        self.revs = Some((self.gen, revs.clone()));
        revs
    }
}

/// The derived tracker plus the daemon's per-issue agent strip — what the
/// issue routes render cards, drawers and epics from.
pub(super) struct BoardRead {
    pub views: Arc<Vec<board::View>>,
    revs: Arc<HashMap<String, String>>,
    pub by_issue: Value,
    /// CAD-405 gate approvals, read with the daemon snapshot.
    pub approvals: Arc<work::Approvals>,
    /// Age, staleness and refresh error of the daemon snapshot behind
    /// this read — the same keys `/api/agents` and the stream carry.
    pub freshness: Value,
    pm_dir: PathBuf,
}

impl BoardRead {
    pub fn by_id(&self) -> HashMap<String, &board::View> {
        self.views
            .iter()
            .map(|v| (v.issue.front.id.clone(), v))
            .collect()
    }

    /// The CAD-405 work context the cards and drawers render from.
    pub fn ctx<'a>(&self, by_id: &'a HashMap<String, &'a board::View>) -> work::Ctx<'a> {
        work::Ctx::new(
            &self.pm_dir,
            by_id,
            crate::issue::time::now_epoch(),
            &self.approvals,
        )
    }

    /// The `/api/issues` card: `work::card_json` with the indexed rev and
    /// the issue's agent strip.
    pub fn card(&self, ctx: &work::Ctx, v: &board::View) -> Value {
        let id = &v.issue.front.id;
        let mut card = match self.revs.get(id) {
            Some(rev) => board::card_json_rev(v, json!(rev)),
            None => board::card_json(v),
        };
        card["work"] = work::item_json(ctx, v);
        with_agents(card, &self.by_issue, id)
    }
}

// ---------- daemon snapshot ----------

/// The daemon side of a model: the kept snapshot and the fetch running
/// now, if any. Held only for a field swap, never across an RPC.
#[derive(Default)]
struct DaemonSlot {
    /// The last snapshot kept. Always fetched after the last change mark:
    /// an invalidation clears it, and a fetch that started before the mark
    /// is never kept.
    kept: Option<Arc<DaemonSnap>>,
    /// The fetch in flight. Callers that need a snapshot join it while it
    /// started after the last change mark.
    building: Option<Arc<Build>>,
    /// The last fetch failed (and left `kept` alone, or had nothing kept).
    refresh_failed: bool,
}

/// One daemon fetch shared by its callers. Ends `Done` with the snapshot,
/// or `Abandoned` when its builder unwound, so waiters retry.
struct Build {
    started: Instant,
    outcome: Mutex<Outcome>,
    finished: Condvar,
}

enum Outcome {
    Running,
    Done(Arc<DaemonSnap>),
    Abandoned,
}

impl Build {
    fn new() -> Self {
        Build {
            started: Instant::now(),
            outcome: Mutex::new(Outcome::Running),
            finished: Condvar::new(),
        }
    }

    /// Waits for the fetch to end. `None` when its builder abandoned it.
    fn wait(&self) -> Option<Arc<DaemonSnap>> {
        let mut outcome = lock(&self.outcome);
        loop {
            match &*outcome {
                Outcome::Running => {
                    outcome = self
                        .finished
                        .wait(outcome)
                        .unwrap_or_else(|e| e.into_inner());
                }
                Outcome::Done(snap) => return Some(snap.clone()),
                Outcome::Abandoned => return None,
            }
        }
    }

    fn end(&self, outcome: Outcome) {
        *lock(&self.outcome) = outcome;
        self.finished.notify_all();
    }
}

/// Ends a fetch that unwinds before [`Model::fetch_shared`] completes it:
/// waiters wake to retry, and the slot forgets the build.
struct BuildGuard<'a> {
    model: &'a Model,
    build: Arc<Build>,
    done: bool,
}

impl Drop for BuildGuard<'_> {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        {
            let mut slot = lock(&self.model.daemon);
            if slot
                .building
                .as_ref()
                .is_some_and(|b| Arc::ptr_eq(b, &self.build))
            {
                slot.building = None;
            }
        }
        self.build.end(Outcome::Abandoned);
    }
}

pub(super) struct DaemonSnap {
    /// When the fetch started. The data is at least this old, and a
    /// slower, older fetch never replaces a newer one.
    at: Instant,
    outcomes: board::JobOutcomes,
    /// `None` when `job_list` failed.
    jobs_fp: Option<u64>,
    agents: Value,
    /// `None` when `agent_list` failed.
    agents_fp: Option<u64>,
    approvals: Arc<work::Approvals>,
    /// `agent_list` or `job_list` failed: the agents are an "unreachable"
    /// stand-in, never served in place of a good snapshot.
    failed: bool,
}

/// Fields of an `agent_list` row (and of the board's agent row) that the
/// daemon computes from "now" and so move every second on their own — a
/// running turn's `silent_secs`, a silent end's `ended_secs`, the
/// awaiting-report clock, a mailbox's backlog age (`inbox`, beside the
/// absolute `oldest_created_at` that still moves on a new or drained
/// backlog), its health block's idle and unread ages and the warning
/// text that quotes them. They stay in what is served (the
/// overview's age cap bounds them); they are left out of the change
/// fingerprints, which would otherwise mark a change every tick while an
/// agent runs and keep the overview cache from ever serving. Job and
/// monitor rows carry no such field: their times are absolute stamps.
const TICKING: &[(&str, &[&str])] = &[
    // `oldest_unread_age_secs` on a board inbox row (CAD-480) is the
    // same clock-derived age `inbox.oldest_age_secs` carries.
    ("", &["silent_secs", "ended_secs", "oldest_unread_age_secs"]),
    ("awaiting_report", &["since_secs", "remaining_secs"]),
    ("inbox", &["oldest_age_secs"]),
    (
        "inbox_health",
        &["idle_secs", "oldest_unread_age_secs", "warning"],
    ),
];

/// An agent row without its [`TICKING`] fields — what a change is.
fn stable_row(row: &Value) -> Value {
    let mut row = row.clone();
    for (block, keys) in TICKING {
        let target = if block.is_empty() {
            Some(&mut row)
        } else {
            row.get_mut(*block)
        };
        if let Some(obj) = target.and_then(Value::as_object_mut) {
            for key in *keys {
                obj.remove(*key);
            }
        }
    }
    row
}

/// The fingerprint of a list of agent rows, ticking fields left out.
fn rows_fp(rows: &Value) -> Value {
    Value::Array(
        rows.as_array()
            .map(|rows| rows.iter().map(stable_row).collect())
            .unwrap_or_default(),
    )
}

/// Two RPCs against a CAD-325 daemon: `job_list` with task rows (job
/// outcomes and task bindings) and `agent_list` with each actor's board
/// slice. An older daemon still answers — through the per-job and
/// per-agent fallbacks in [`agents_payload_from`].
///
/// The three calls run concurrently: each daemon connection can wait up
/// to one 50 ms accept poll before it is served, so in sequence they
/// cost that wait three times over.
fn fetch_daemon(state_dir: &Path) -> DaemonSnap {
    let at = Instant::now();
    let (jobs, list, approvals) = std::thread::scope(|s| {
        let jobs = s.spawn(|| board_job_list(state_dir));
        let list = s.spawn(|| client::rpc(state_dir, "agent_list", json!({"board": true})).ok());
        let approvals = s.spawn(|| work::fetch_approvals(state_dir));
        (
            jobs.join().unwrap_or_default(),
            list.join().unwrap_or_default(),
            approvals.join().unwrap_or_default(),
        )
    });
    let failed = list.is_none() || jobs.is_none();
    let agents = agents_payload_from(state_dir, list.clone(), jobs.as_ref());
    let agents_fp =
        list.map(|l| value_fp(&json!([rows_fp(&l["agents"]), rows_fp(&agents["agents"])])));
    let outcomes = jobs
        .as_ref()
        .map(|l| board::outcomes_from_jobs(l["jobs"].as_array().map(Vec::as_slice).unwrap_or(&[])))
        .unwrap_or_default();
    DaemonSnap {
        at,
        outcomes,
        jobs_fp: jobs.as_ref().map(value_fp),
        agents,
        agents_fp,
        approvals: Arc::new(approvals),
        failed,
    }
}

// ---------- overview ----------

#[derive(Default)]
struct OverviewState {
    /// The last build: value, when its inputs were read, its tracker key.
    value: Option<(Value, Instant, u64)>,
    /// The last daemon pass and when it started.
    sources: Option<(Arc<overview::DaemonSources>, Instant)>,
    building: bool,
    wanted: Option<Instant>,
    /// Fingerprint of tracker fields that can change the rendered
    /// overview. An ordinary title edit leaves it unchanged, so the
    /// watcher can reuse the cached aggregate without rebuilding.
    inputs: Option<u64>,
}

// ---------- stream hub ----------

#[derive(Default)]
struct Hub {
    subs: Vec<Sender<Arc<str>>>,
    running: bool,
}

/// What the watcher last saw — legacy fingerprints plus the entity maps
/// the diffs are taken against.
struct Watch {
    tracker: Option<SystemTime>,
    delivery: u64,
    jobs: Option<u64>,
    agents: Option<u64>,
    monitoring: Option<u64>,
    /// id → fingerprint of the card as the client would render it.
    cards: HashMap<String, u64>,
    plans: HashMap<String, u64>,
    rows: HashMap<String, u64>,
    projects: Option<u64>,
    overview: Option<u64>,
    /// Fetch start of the newest snapshot published: an older one is never
    /// published after it.
    at: Option<Instant>,
    /// The refresh-error state last published.
    refresh_error: bool,
}

/// A heartbeat the stream loop drops: a failed send is how the watcher
/// learns a client hung up.
pub(super) const HEARTBEAT: &str = "";

fn frame(event: &str, data: &Value) -> Arc<str> {
    format!("event: {event}\ndata: {data}\n\n").into()
}

/// The frame shape every client before CAD-325 keys on.
fn legacy_frame(name: &str) -> Arc<str> {
    frame(name, &json!({"resources": event_resources(name)}))
}

/// Upsert/delete frames for `kind` between `old` and `new` entity maps.
fn diff_frames(kind: &str, old: &HashMap<String, u64>, new: &Entities, frames: &mut Vec<Arc<str>>) {
    let mut ids: Vec<&String> = new
        .iter()
        .filter(|(id, (fp, _))| old.get(*id) != Some(fp))
        .map(|(id, _)| id)
        .collect();
    ids.sort_by_key(|id| board::natural_key(id));
    for id in ids {
        let data = json!({"op": "upsert", "id": id, kind: new[id].1});
        frames.push(frame(kind, &data));
    }
    let mut gone: Vec<&String> = old.keys().filter(|id| !new.contains_key(*id)).collect();
    gone.sort_by_key(|id| board::natural_key(id));
    for id in gone {
        frames.push(frame(kind, &json!({"op": "delete", "id": id})));
    }
}

/// Opt-in entity clients retain legacy compatibility without refetching
/// collections already supplied as authoritative entity patches.
pub(super) fn entity_frame(text: &str) -> String {
    let Some((head, tail)) = text.split_once("\ndata: ") else {
        return text.into();
    };
    let Some(kind) = head.strip_prefix("event: ") else {
        return text.into();
    };
    let Ok(mut data) = serde_json::from_str::<Value>(tail.trim()) else {
        return text.into();
    };
    if matches!(kind, "issue" | "agent" | "plan") {
        data["resource"] = json!(kind);
        data["rev"] = json!(format!("{:016x}", value_fp(&data)));
    } else if let Some(resources) = data["resources"].as_array_mut() {
        resources.retain(|r| {
            !matches!(r.as_str(), Some("issues" | "agents" | "issue"))
                && !(kind == "issues" && matches!(r.as_str(), Some("projects" | "overview")))
        });
    }
    format!("event: {kind}\ndata: {data}\n\n")
}

/// Compare rendered aggregates, not inferred issue fields. Ignore only
/// display clocks; audience/rank/warnings and every title/body/count remain.
fn aggregate_fp(value: &Value) -> u64 {
    let mut stable = value.clone();
    let now = stable["generated_at"].as_i64();
    if let Some(root) = stable.as_object_mut() {
        root.remove("generated_at");
    }
    // homeNeeds sorts plans/questions first, then descending age. Keep
    // that permutation as well as the fourteen-day Old bucket, rather
    // than treating an action input as an arbitrary decorative clock.
    if let Some(rows) = stable["needs_me"].as_array_mut() {
        let mut order: Vec<_> = rows
            .iter()
            .enumerate()
            .map(|(i, row)| {
                let rank = match row["kind"].as_str() {
                    Some("plan") => 0,
                    Some("question") => 1,
                    _ => 2,
                };
                (rank, std::cmp::Reverse(row["age"].as_i64().unwrap_or(0)), i)
            })
            .collect();
        order.sort();
        let order: Vec<_> = order.into_iter().map(|(_, _, i)| i).collect();
        for row in rows {
            let age = row["age"].as_i64().unwrap_or(0);
            if let Some(fields) = row.as_object_mut() {
                fields.insert("age".into(), json!(age >= 14 * 86400));
            }
        }
        stable["needs_age_order"] = json!(order);
    }
    if let Some(projects) = stable["projects"].as_array_mut() {
        for project in projects {
            if let (Some(now), Some(age)) = (now, project["oldest_review_age"].as_i64()) {
                project["oldest_review_age"] = json!(now - age);
            }
            // Claim age is a display clock backed by the retained since.
            if let Some(claims) = project["claims"].as_array_mut() {
                for claim in claims {
                    if let Some(fields) = claim.as_object_mut() {
                        fields.remove("age_secs");
                    }
                }
            }
        }
    }
    // Unknown/nested age fields remain: normalization follows audited
    // consumer paths, never recursive key spelling.
    value_fp(&stable)
}

fn aggregate_frame(
    old_projects: Option<u64>,
    projects: Option<u64>,
    old_overview: Option<u64>,
    overview: Option<u64>,
) -> Arc<str> {
    let mut resources = Vec::new();
    if projects != old_projects {
        resources.push("projects");
    }
    // A hidden/unbuilt overview cannot prove sameness: mark it invalid
    // conservatively. Hidden clients defer reads until they show it.
    if overview.is_none() || overview != old_overview {
        resources.push("overview");
    }
    // An empty list proves this tracker batch checked the aggregate
    // outputs without requiring any client read (also a stream checkpoint).
    frame("aggregates", &json!({"resources": resources}))
}

fn fps(map: &Entities) -> HashMap<String, u64> {
    map.iter().map(|(k, (fp, _))| (k.clone(), *fp)).collect()
}

impl Model {
    /// The daemon snapshot a read serves. While the watcher runs, its kept
    /// snapshot, however old up to [`DAEMON_MAX_STALE`]: the watcher's next
    /// fetch replaces it, and a read never waits on a slow one. Otherwise a
    /// fetch shared with every caller that needs one now.
    fn daemon_snap(&self) -> Arc<DaemonSnap> {
        let watched = lock(&self.hub).running;
        let kept = lock(&self.daemon).kept.clone();
        if let Some(snap) = kept.filter(|snap| {
            watched
                && !snap.failed
                && snap.at.elapsed() < DAEMON_MAX_STALE
                && self.after_change(snap.at)
        }) {
            return snap;
        }
        self.fetch_shared()
    }

    /// The snapshot from the fetch in flight when it started after the
    /// last change mark, else from a fetch started now. Concurrent callers
    /// share one fetch. A waiter whose fetch was abandoned retries. The
    /// fetch runs outside the slot lock, which is held only to swap fields.
    fn fetch_shared(&self) -> Arc<DaemonSnap> {
        loop {
            let (build, leader) = {
                let mut slot = lock(&self.daemon);
                match slot.building.clone() {
                    Some(build) if self.after_change(build.started) => (build, false),
                    _ => {
                        let build = Arc::new(Build::new());
                        slot.building = Some(build.clone());
                        (build, true)
                    }
                }
            };
            if !leader {
                self.daemon_joins.fetch_add(1, Ordering::Relaxed);
                if let Some(snap) = build.wait() {
                    return snap;
                }
                continue;
            }
            self.daemon_builds.fetch_add(1, Ordering::Relaxed);
            let mut guard = BuildGuard {
                model: self,
                build: build.clone(),
                done: false,
            };
            // The snapshot is as old as the fetch's start, whatever the
            // fetcher stamped: the change marks compare against that start.
            let mut fetched = (self.fetch)(&self.state_dir);
            fetched.at = build.started;
            let snap = {
                let mut slot = lock(&self.daemon);
                if slot
                    .building
                    .as_ref()
                    .is_some_and(|b| Arc::ptr_eq(b, &build))
                {
                    slot.building = None;
                }
                self.keep_in(&mut slot, Arc::new(fetched))
            };
            build.end(Outcome::Done(snap.clone()));
            guard.done = true;
            return snap;
        }
    }

    /// Keep `snap` unless a newer one is kept or it started before the
    /// last change mark (a fetch racing a write may predate the write),
    /// and return what the fetch's callers are served. The mark is read
    /// under the slot lock: [`Self::invalidate`] marks first and clears
    /// the slot after, so either this keep sees the mark or the clear
    /// lands after it. A failed fetch is never kept and never replaces the
    /// last good snapshot: that is served instead, with the failure
    /// reported by [`Self::freshness`]; with none kept, the failed
    /// snapshot answers its callers alone.
    fn keep_in(&self, slot: &mut DaemonSlot, snap: Arc<DaemonSnap>) -> Arc<DaemonSnap> {
        if !self.after_change(snap.at) {
            return snap;
        }
        slot.refresh_failed = snap.failed;
        if snap.failed {
            return slot.kept.clone().unwrap_or(snap);
        }
        if slot.kept.as_ref().is_none_or(|old| old.at <= snap.at) {
            slot.kept = Some(snap.clone());
        }
        snap
    }

    /// The snapshot the stream watcher publishes from: one fetched after
    /// the last change mark (a build a write overtook is fetched again,
    /// never published) and not older than `after`, the newest snapshot
    /// already published. `None` when a write keeps overtaking the fetch.
    /// A failed snapshot (no good one to fall back on) comes back as is:
    /// the watcher publishes tracker changes from it, never daemon ones.
    fn fetch_current(&self, after: Option<Instant>) -> Option<Arc<DaemonSnap>> {
        for _ in 0..3 {
            let snap = self.fetch_shared();
            if !self.after_change(snap.at) {
                continue;
            }
            if after.is_some_and(|at| snap.at < at) {
                return None;
            }
            return Some(snap);
        }
        None
    }

    /// Age, staleness and refresh error of `snap` as it is served, beside
    /// an HTTP payload or a stream frame.
    fn freshness(&self, snap: &DaemonSnap) -> Value {
        let error = snap.failed || lock(&self.daemon).refresh_failed;
        let age = snap.at.elapsed();
        json!({
            "snapshot_age_secs": age.as_secs_f64(),
            "stale": error || age >= SNAPSHOT_STALE,
            "refresh_error": error,
        })
    }

    /// Was something read at `at` read after the last change mark?
    fn after_change(&self, at: Instant) -> bool {
        lock(&self.changed_at).is_none_or(|c| at >= c)
    }

    fn mark_changed(&self, at: Instant) {
        let mut c = lock(&self.changed_at);
        if c.is_none_or(|old| old < at) {
            *c = Some(at);
        }
    }

    /// Cost meters for tests: folders parsed, overview builds, and the
    /// builds a read ran itself (the cache missed; waiting on an in-flight
    /// background rebuild is not counted) — so far.
    pub(super) fn stats(&self) -> Value {
        json!({
            "parses": lock(&self.tracker).parses,
            "overview_builds": self.overview_builds.load(Ordering::Relaxed),
            "request_builds": self.request_builds.load(Ordering::Relaxed),
            "collection_parses": self.collection_parses.load(Ordering::Relaxed),
            "daemon_builds": self.daemon_builds.load(Ordering::Relaxed),
            "daemon_joins": self.daemon_joins.load(Ordering::Relaxed),
        })
    }

    fn board_with(&self, pm: &Pm, project: Option<&str>, snap: &DaemonSnap) -> BoardRead {
        let mut t = lock(&self.tracker);
        t.refresh(&pm.dir);
        let jobs = (&snap.outcomes, snap.jobs_fp.unwrap_or(0));
        let (_, views) = t.views(pm, project, Some(jobs));
        BoardRead {
            views,
            revs: t.revs(),
            by_issue: snap.agents["by_issue"].clone(),
            approvals: snap.approvals.clone(),
            freshness: self.freshness(snap),
            pm_dir: pm.dir.clone(),
        }
    }

    /// The derived tracker (optionally one project's, derived over that
    /// project alone — as `load_all(project)` did) with job state and
    /// the agent strip.
    pub(super) fn board(&self, pm: &Pm, project: Option<&str>) -> BoardRead {
        let snap = self.daemon_snap();
        self.board_with(pm, project, &snap)
    }

    /// `/api/agents`, with the snapshot's [`Self::freshness`] keys:
    /// `snapshot_age_secs` is its age since its fetch started (the data is
    /// at least that old).
    pub(super) fn agents(&self) -> Value {
        let snap = self.daemon_snap();
        let mut agents = snap.agents.clone();
        merge_freshness(&mut agents, self.freshness(&snap));
        agents
    }

    /// A board write is about to answer — drop what the daemon side
    /// cached, and refuse anything read before now, so the writer's next
    /// read sees its own write. Tracker writes need nothing more: the
    /// next read re-stamps the folders.
    pub(super) fn invalidate(&self) {
        self.mark_changed(Instant::now());
        // A fetch in flight may predate the write: its build is dropped
        // here, so no reader joins it, and its keep is refused by the mark.
        *lock(&self.daemon) = DaemonSlot::default();
        let mut st = lock(&self.overview);
        st.value = None;
        st.sources = None;
        st.inputs = None;
    }

    /// The overview's tracker input and its key — the views' key plus
    /// the repo `HEAD`, so a commit alone rebuilds the overview too.
    fn tracker_views(&self, pm: &Pm) -> (u64, Arc<Vec<board::View>>) {
        let (key, views) = {
            let mut t = lock(&self.tracker);
            t.refresh(&pm.dir);
            t.views(pm, None, None)
        };
        let mut h = DefaultHasher::new();
        (key, head_stamp(&pm.dir), self.delivery_stamp()).hash(&mut h);
        (h.finish(), views)
    }

    // CI observations move delivery state without moving an agent or
    // tracker issue. Atomic replacement must invalidate cached decisions.
    fn delivery_stamp(&self) -> u64 {
        let mut h = DefaultHasher::new();
        hash_meta(&self.state_dir.join("delivery.json"), &mut h);
        h.finish()
    }

    fn compose(
        &self,
        views: &[board::View],
        sources: &overview::DaemonSources,
        gh_wait: Duration,
    ) -> Value {
        self.overview_builds.fetch_add(1, Ordering::Relaxed);
        overview::overview_board_from(
            &self.state_dir,
            &self.pm_dir,
            overview::Reuse { views, sources },
            gh_wait,
        )
    }

    /// The cached overview when it is still servable: same tracker key,
    /// daemon inputs read after the last change mark, younger than
    /// [`OVERVIEW_MAX_AGE`]. With how old it is.
    fn servable(&self, st: &OverviewState, key: u64) -> Option<(Value, Duration)> {
        let (value, at, k) = st.value.as_ref()?;
        let age = at.elapsed();
        (*k == key && age < OVERVIEW_MAX_AGE && self.after_change(*at))
            .then(|| (value.clone(), age))
    }

    fn keep_overview(&self, value: &Value, at: Instant, key: u64) {
        if !self.after_change(at) {
            return;
        }
        let inputs = self.overview_inputs();
        let mut st = lock(&self.overview);
        if st.value.as_ref().is_none_or(|(_, b, _)| *b <= at) {
            st.value = Some((value.clone(), at, key));
            st.inputs = inputs;
        }
    }

    /// Tracker fields that can change the rendered overview, from the
    /// issues already loaded. Ordinary titles are left out; a proposed
    /// plan or an intake row renders its title, so those stay in.
    fn overview_inputs(&self) -> Option<u64> {
        let pm = Pm::at(&self.pm_dir).ok()?;
        let mut t = lock(&self.tracker);
        t.refresh(&pm.dir);
        Some(overview_inputs_fp(&pm, &t, self.delivery_stamp()))
    }

    /// `/api/overview`. Served from the cache while it is servable (a
    /// background pass refreshes one older than [`OVERVIEW_FRESH`]);
    /// otherwise built now — one build at a time, later readers take its
    /// result. A tracker-only change reuses a recent daemon pass.
    pub(super) fn overview(self: &Arc<Self>) -> Value {
        self.overview_value(true)
    }

    fn overview_value(self: &Arc<Self>, mark_wanted: bool) -> Value {
        let Ok(pm) = Pm::at(&self.pm_dir) else {
            // No tracker to index — the plain build is all daemon.
            return overview::overview_board(&self.state_dir, &self.pm_dir);
        };
        let (key, _) = self.tracker_views(&pm);
        {
            let mut st = lock(&self.overview);
            if mark_wanted {
                st.wanted = Some(Instant::now());
            }
            if let Some((value, age)) = self.servable(&st, key) {
                if age >= OVERVIEW_FRESH && !st.building {
                    st.building = true;
                    self.rebuild_overview_later();
                }
                return value;
            }
        }
        let _one = lock(&self.build_lock);
        // Whoever held the lock may have built what this read needs; the
        // tracker may have moved while it waited.
        let (key, views) = self.tracker_views(&pm);
        let (recent, cold) = {
            let st = lock(&self.overview);
            if let Some((value, _)) = self.servable(&st, key) {
                return value;
            }
            let recent = st
                .sources
                .as_ref()
                .filter(|(_, at)| at.elapsed() < OVERVIEW_MAX_AGE && self.after_change(*at))
                .map(|(s, at)| (s.clone(), *at));
            (recent, st.value.is_none())
        };
        let (sources, at) = match recent {
            Some(recent) => recent,
            None => self.fresh_sources(),
        };
        // A cold build may wait on gh like the plain board build; a
        // rebuild serves the gh cache and lets its refresh land later.
        let gh_wait = if cold {
            overview::Options::board().gh_wait
        } else {
            Duration::ZERO
        };
        self.request_builds.fetch_add(1, Ordering::Relaxed);
        let value = self.compose(&views, &sources, gh_wait);
        self.keep_overview(&value, at, key);
        value
    }

    /// A daemon probe pass, kept for tracker-only rebuilds.
    fn fresh_sources(&self) -> (Arc<overview::DaemonSources>, Instant) {
        let at = Instant::now();
        let fresh = Arc::new(overview::daemon_sources(
            &self.state_dir,
            &overview::Options::board(),
        ));
        if self.after_change(at) {
            lock(&self.overview).sources = Some((fresh.clone(), at));
        }
        (fresh, at)
    }

    /// A full pass — fresh daemon probes, then the build — off the
    /// request path, under the build lock so a reader that needs a build
    /// waits for this one. The caller set `building`.
    fn rebuild_overview_later(self: &Arc<Self>) {
        let me = self.clone();
        std::thread::spawn(move || {
            let _reset = OnDrop(|| lock(&me.overview).building = false);
            let _one = lock(&me.build_lock);
            let (sources, at) = me.fresh_sources();
            if let Ok(pm) = Pm::at(&me.pm_dir) {
                let (key, views) = me.tracker_views(&pm);
                let value = me.compose(&views, &sources, overview::Options::board().gh_wait);
                me.keep_overview(&value, at, key);
            }
        });
    }

    /// The daemon moved under a watched board: refresh the overview in
    /// the background when someone has been reading it, so the refetch
    /// the change frame triggers finds (or waits for) a fresh build.
    fn nudge_overview(self: &Arc<Self>) {
        let mut st = lock(&self.overview);
        let wanted = st.wanted.is_some_and(|w| w.elapsed() < OVERVIEW_WANTED);
        if wanted && !st.building {
            st.building = true;
            self.rebuild_overview_later();
        }
    }

    /// Project counts from the loaded tracker, and the overview
    /// fingerprint when its inputs moved. A title-only edit re-parses
    /// that one folder for the entity patch and then stops: the
    /// collection is not scanned again and the overview is not rebuilt.
    fn aggregate_fps(self: &Arc<Self>) -> (Option<u64>, Option<u64>) {
        let Ok(pm) = Pm::at(&self.pm_dir) else {
            return (None, None);
        };
        let (projects, inputs) = {
            let mut t = lock(&self.tracker);
            t.refresh(&pm.dir);
            let projects = value_fp(&super::projects_payload(&pm, &project_counts(&t)));
            let inputs = overview_inputs_fp(&pm, &t, self.delivery_stamp());
            (projects, inputs)
        };
        let wanted = lock(&self.overview)
            .wanted
            .is_some_and(|at| at.elapsed() < OVERVIEW_WANTED);
        if !wanted {
            return (Some(projects), None);
        }
        if let Some(fp) = self.cached_overview_aggregate(inputs) {
            return (Some(projects), Some(fp));
        }
        (
            Some(projects),
            Some(aggregate_fp(&self.overview_value(false))),
        )
    }

    /// The cached overview's aggregate fingerprint when `inputs` still
    /// describes it. `None` when the cache is cold or the tracker change
    /// can affect the rendered overview.
    fn cached_overview_aggregate(&self, inputs: u64) -> Option<u64> {
        let value = {
            let st = lock(&self.overview);
            if st.inputs != Some(inputs) {
                return None;
            }
            st.value.as_ref().map(|(value, _, _)| value.clone())
        }?;
        Some(aggregate_fp(&value))
    }

    /// `/api/projects` from the indexed issues. Same bytes as a full
    /// collection load; the count does not read issue bodies again.
    pub(super) fn projects(&self, pm: &Pm) -> Value {
        let mut t = lock(&self.tracker);
        t.refresh(&pm.dir);
        super::projects_payload(pm, &project_counts(&t))
    }

    // ---------- stream ----------

    /// Join the board's stream. The first subscriber starts the watcher
    /// with a baseline taken here — before the caller writes the stream
    /// head, so a change the client makes once it sees the stream live
    /// is never absorbed into the baseline.
    pub(super) fn subscribe(self: &Arc<Self>) -> Receiver<Arc<str>> {
        let (tx, rx) = mpsc::channel();
        let start = {
            let mut hub = lock(&self.hub);
            hub.subs.push(tx);
            !std::mem::replace(&mut hub.running, true)
        };
        if start {
            // A panic before the watcher spawns must not leave `running` set.
            let _reset = self.reset_hub_on_panic();
            // The baseline joins the shared fetch like every other reader.
            let snap = self
                .fetch_current(None)
                .unwrap_or_else(|| self.fetch_shared());
            let (cards, plans) = self.entities(&snap);
            let (projects, overview) = self.aggregate_fps();
            let base = Watch {
                tracker: dir_mtime(&self.pm_dir),
                delivery: self.delivery_stamp(),
                jobs: snap.jobs_fp,
                agents: snap.agents_fp,
                monitoring: self.monitoring_fp(),
                cards: fps(&cards),
                plans: fps(&plans),
                rows: fps(&agent_rows(&snap)),
                projects,
                overview,
                at: Some(snap.at),
                refresh_error: snap.failed,
            };
            let me = self.clone();
            std::thread::spawn(move || me.watch(base));
        }
        rx
    }

    fn monitoring_fp(&self) -> Option<u64> {
        client::rpc(&self.state_dir, "monitor_list", json!({}))
            .ok()
            .map(|l| value_fp(&l))
    }

    /// Cards and plans keyed by id, each with the fingerprint a diff
    /// compares — the card without its ticking claim age.
    fn entities(&self, snap: &DaemonSnap) -> (Entities, Entities) {
        let Ok(pm) = Pm::at(&self.pm_dir) else {
            return (HashMap::new(), HashMap::new());
        };
        let read = self.board_with(&pm, None, snap);
        let by_id = read.by_id();
        let ctx = read.ctx(&by_id);
        let mut cards = HashMap::new();
        let mut plans = HashMap::new();
        for v in read.views.iter() {
            let id = v.issue.front.id.clone();
            let card = read.card(&ctx, v);
            let mut stable = card.clone();
            if stable["claim"].is_object() {
                stable["claim"]["age_secs"] = Value::Null;
            }
            cards.insert(
                id.clone(),
                (value_fp(&json!([stable, folder_stamp(&v.issue.dir)])), card),
            );
            let plan = plan::plan_json(v, &by_id);
            if !plan.is_null() {
                plans.insert(id, (value_fp(&plan), plan));
            }
        }
        (cards, plans)
    }

    /// A panicking watcher (or a start that panics before the watcher
    /// runs) must not leave `running` set — no stream would ever start
    /// another. Dropping the senders ends the streams; clients reconnect
    /// and start a fresh watcher.
    fn reset_hub_on_panic(self: &Arc<Self>) -> OnDrop<impl FnMut()> {
        let me = self.clone();
        OnDrop(move || {
            if std::thread::panicking() {
                let mut hub = lock(&me.hub);
                hub.running = false;
                hub.subs.clear();
            }
        })
    }

    fn watch(self: Arc<Self>, mut w: Watch) {
        let _reset = self.reset_hub_on_panic();
        loop {
            std::thread::sleep(WATCH_EVERY);
            {
                // Nobody left — stop polling. `subscribe` checks
                // `running` under the same lock, so a racing subscriber
                // either lands before this or starts a new watcher.
                let mut hub = lock(&self.hub);
                if hub.subs.is_empty() {
                    hub.running = false;
                    return;
                }
            }
            let mut frames = vec![Arc::<str>::from(HEARTBEAT)];
            self.tick(&mut w, &mut frames);
            let mut hub = lock(&self.hub);
            hub.subs
                .retain(|tx| frames.iter().all(|f| tx.send(f.clone()).is_ok()));
            if hub.subs.is_empty() {
                hub.running = false;
                return;
            }
        }
    }

    /// One poll: the legacy change frames, in the pre-CAD-325 order, then
    /// the entity diffs.
    fn tick(self: &Arc<Self>, w: &mut Watch, frames: &mut Vec<Arc<str>>) {
        // The change mark: whatever was read before this tick began may
        // predate the change it finds.
        let started = Instant::now();
        // Fetched before the watcher's state moves: with nothing current to
        // publish the tick leaves it as it was and the next one retries.
        let Some(snap) = self.fetch_current(w.at) else {
            return;
        };
        if !snap.failed {
            w.at = Some(snap.at);
        }
        let scanned = board::collection_parses();
        let tracker = dir_mtime(&self.pm_dir);
        let issues = tracker != w.tracker;
        if issues {
            w.tracker = tracker;
            frames.push(legacy_frame("issues"));
        }
        let delivery_stamp = self.delivery_stamp();
        let delivery = delivery_stamp != w.delivery;
        if delivery {
            w.delivery = delivery_stamp;
            if !issues {
                frames.push(legacy_frame("issues"));
            }
        }
        let jobs = !snap.failed && snap.jobs_fp != w.jobs;
        if jobs {
            w.jobs = snap.jobs_fp;
            frames.push(legacy_frame("jobs"));
        }
        let agents = !snap.failed && snap.agents_fp != w.agents;
        if agents {
            w.agents = snap.agents_fp;
            frames.push(legacy_frame("agents"));
        }
        let monitoring_fp = self.monitoring_fp();
        let monitoring = monitoring_fp.is_some() && monitoring_fp != w.monitoring;
        if monitoring {
            w.monitoring = monitoring_fp;
            frames.push(legacy_frame("monitoring"));
        }
        if issues || jobs || agents {
            let (cards, plans) = self.entities(&snap);
            diff_frames("issue", &w.cards, &cards, frames);
            diff_frames("plan", &w.plans, &plans, frames);
            w.cards = fps(&cards);
            w.plans = fps(&plans);
        }
        let refresh_error = self.freshness(&snap)["refresh_error"] == json!(true);
        if !snap.failed && (jobs || agents || refresh_error != w.refresh_error) {
            w.refresh_error = refresh_error;
            // Bindings and totals can move without an agent row moving.
            let mut meta = json!({
                "daemon": snap.agents["daemon"], "totals": snap.agents["totals"],
                "by_issue": snap.agents["by_issue"]
            });
            merge_freshness(&mut meta, self.freshness(&snap));
            frames.push(frame("agent_meta", &meta));
            let rows = agent_rows(&snap);
            diff_frames("agent", &w.rows, &rows, frames);
            w.rows = fps(&rows);
        }
        if issues || delivery {
            let (projects, overview) = self.aggregate_fps();
            frames.push(aggregate_frame(w.projects, projects, w.overview, overview));
            w.projects = projects;
            w.overview = overview;
        }
        if jobs || agents || monitoring || delivery {
            // Before the frames go out: the refetch they trigger must not
            // be served anything read before this tick.
            self.mark_changed(started);
            self.nudge_overview();
        }
        let scanned = board::collection_parses().saturating_sub(scanned);
        if scanned > 0 {
            self.collection_parses.fetch_add(scanned, Ordering::Relaxed);
        }
    }
}

/// Adds the keys of the freshness object `fresh` to the JSON object `into`.
pub(super) fn merge_freshness(into: &mut Value, fresh: Value) {
    if let (Some(into), Value::Object(fresh)) = (into.as_object_mut(), fresh) {
        into.extend(fresh);
    }
}

/// `/api/agents` rows keyed by alias.
fn agent_rows(snap: &DaemonSnap) -> Entities {
    snap.agents["agents"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|r| {
                    let alias = r["alias"].as_str()?.to_string();
                    Some((alias, (value_fp(&stable_row(r)), r.clone())))
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(dir: &Path) {
        std::fs::create_dir_all(dir.join("comments")).unwrap();
        std::fs::create_dir_all(dir.join("artifacts")).unwrap();
        std::fs::write(dir.join("issue.md"), "a").unwrap();
    }

    #[test]
    fn delivery_observation_invalidates_the_overview_without_agent_or_tracker_changes() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state = tmp.path().join("state");
        let tracker = tmp.path().join("pm");
        std::fs::create_dir_all(&state).unwrap();
        std::fs::create_dir_all(&tracker).unwrap();
        std::fs::write(tracker.join("pm.yaml"), "schema: 1\n").unwrap();
        let pm = Pm::at(&tracker).unwrap();
        let model = get(&state, &tracker);
        let key_before = model.tracker_views(&pm).0;
        let mut watch = Watch {
            tracker: dir_mtime(&tracker),
            delivery: model.delivery_stamp(),
            jobs: None,
            agents: None,
            monitoring: None,
            cards: HashMap::new(),
            plans: HashMap::new(),
            rows: HashMap::new(),
            projects: None,
            overview: None,
            at: None,
            refresh_error: false,
        };
        let mut frames = Vec::new();
        model.tick(&mut watch, &mut frames);
        frames.clear();
        *lock(&model.changed_at) = None;
        // Delivery sync writes this outside the board's HTTP write path.
        std::fs::write(state.join("delivery.json"), "{}\n").unwrap();
        assert_ne!(key_before, model.tracker_views(&pm).0);
        model.tick(&mut watch, &mut frames);
        assert!(
            frames
                .iter()
                .any(|f| f.starts_with("event: issues\n") && f.contains("\"overview\"")),
            "{frames:?}"
        );
        assert!(lock(&model.changed_at).is_some());
        frames.clear();
        model.tick(&mut watch, &mut frames);
        assert!(
            frames.is_empty(),
            "stable delivery state must not cause a refetch loop"
        );
    }

    /// A model whose daemon fetch counts its calls, answers with the call
    /// number as `agents.v`, sleeps `delay(call)` and panics on `panic_on`.
    fn fake_daemon(
        delay: impl Fn(u64) -> Duration + Send + Sync + 'static,
        panic_on: Option<u64>,
    ) -> (Arc<Model>, Arc<AtomicU64>) {
        fake_daemon_failing(delay, panic_on, |_| false)
    }

    /// [`fake_daemon`] whose fetch number `n` reports a failed refresh when
    /// `fails(n)`. Every snapshot carries `n` as its job and agent fingerprint.
    fn fake_daemon_failing(
        delay: impl Fn(u64) -> Duration + Send + Sync + 'static,
        panic_on: Option<u64>,
        fails: impl Fn(u64) -> bool + Send + Sync + 'static,
    ) -> (Arc<Model>, Arc<AtomicU64>) {
        let calls = Arc::new(AtomicU64::new(0));
        let seen = calls.clone();
        let fetch: Fetch = Box::new(move |_| {
            let n = seen.fetch_add(1, Ordering::SeqCst) + 1;
            std::thread::sleep(delay(n));
            if panic_on == Some(n) {
                panic!("fetch {n} unwound");
            }
            DaemonSnap {
                at: Instant::now(),
                outcomes: Default::default(),
                jobs_fp: Some(n),
                agents: json!({"daemon": "reachable", "agents": [], "v": n}),
                agents_fp: Some(n),
                approvals: Arc::new(Default::default()),
                failed: fails(n),
            }
        });
        (
            Arc::new(Model::new(
                Path::new("/nonexistent/state"),
                Path::new("/nonexistent/pm"),
                fetch,
            )),
            calls,
        )
    }

    #[test]
    fn concurrent_cold_reads_share_one_daemon_fetch() {
        let (model, calls) = fake_daemon(|_| Duration::from_millis(400), None);
        let barrier = Arc::new(std::sync::Barrier::new(8));
        let readers: Vec<_> = (0..8)
            .map(|_| {
                let (m, b) = (model.clone(), barrier.clone());
                std::thread::spawn(move || {
                    b.wait();
                    m.agents()["v"].clone()
                })
            })
            .collect();
        for reader in readers {
            assert_eq!(reader.join().unwrap(), 1);
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(model.stats()["daemon_builds"], 1);
        assert_eq!(model.stats()["daemon_joins"], 7);
    }

    #[test]
    fn a_watched_read_serves_the_kept_snapshot_while_a_refresh_is_slow() {
        let (model, calls) = fake_daemon(
            |n| {
                if n == 1 {
                    Duration::ZERO
                } else {
                    Duration::from_millis(1500)
                }
            },
            None,
        );
        lock(&model.hub).running = true;
        assert_eq!(model.agents()["v"], 1);
        let refresh = {
            let m = model.clone();
            std::thread::spawn(move || m.fetch_shared())
        };
        std::thread::sleep(Duration::from_millis(150));
        let asked = Instant::now();
        let served = model.agents();
        assert!(
            asked.elapsed() < Duration::from_millis(500),
            "a read waited on the slow refresh"
        );
        assert_eq!(served["v"], 1);
        assert!(served["snapshot_age_secs"].as_f64().unwrap() > 0.0);
        assert_eq!(refresh.join().unwrap().agents["v"], 2);
        assert_eq!(model.agents()["v"], 2);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_fetch_started_before_a_write_never_overwrites_the_post_write_view() {
        let (model, calls) = fake_daemon(
            |n| {
                if n == 1 {
                    Duration::from_millis(400)
                } else {
                    Duration::ZERO
                }
            },
            None,
        );
        lock(&model.hub).running = true;
        let before_write = {
            let m = model.clone();
            std::thread::spawn(move || m.agents()["v"].clone())
        };
        std::thread::sleep(Duration::from_millis(100));
        model.invalidate();
        assert_eq!(
            model.agents()["v"],
            2,
            "the write must not join the older fetch"
        );
        assert_eq!(before_write.join().unwrap(), 1);
        assert_eq!(
            model.agents()["v"],
            2,
            "the late older fetch must not be kept"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    fn blank_watch() -> Watch {
        Watch {
            tracker: None,
            delivery: 0,
            jobs: None,
            agents: None,
            monitoring: None,
            cards: HashMap::new(),
            plans: HashMap::new(),
            rows: HashMap::new(),
            projects: None,
            overview: None,
            at: None,
            refresh_error: false,
        }
    }

    #[test]
    fn the_first_stream_subscriber_joins_the_shared_fetch() {
        let (model, calls) = fake_daemon(|_| Duration::from_millis(400), None);
        let reader = {
            let m = model.clone();
            std::thread::spawn(move || m.agents()["v"].clone())
        };
        std::thread::sleep(Duration::from_millis(100));
        let _rx = model.subscribe();
        assert_eq!(reader.join().unwrap(), 1);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "the stream baseline must not build a second snapshot"
        );
        assert_eq!(model.stats()["daemon_joins"], 1);
    }

    #[test]
    fn a_failed_refresh_keeps_the_last_good_snapshot_and_says_so() {
        let (model, calls) = fake_daemon_failing(|_| Duration::ZERO, None, |n| n == 2);
        lock(&model.hub).running = true;
        let good = model.agents();
        assert_eq!(good["v"], 1);
        assert_eq!(good["refresh_error"], false);
        let failed = model.fetch_shared();
        assert_eq!(failed.agents["v"], 1, "the failed fetch replaced last-good");
        let served = model.agents();
        assert_eq!(served["v"], 1);
        assert_eq!(served["refresh_error"], true);
        assert_eq!(served["stale"], true);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        // The next good fetch clears the error.
        assert_eq!(model.fetch_shared().agents["v"], 3);
        assert_eq!(model.agents()["refresh_error"], false);
    }

    #[test]
    fn the_watcher_publishes_nothing_for_a_failed_refresh() {
        let (model, _) = fake_daemon_failing(|_| Duration::ZERO, None, |n| n == 2);
        let mut watch = blank_watch();
        let mut frames = Vec::new();
        model.tick(&mut watch, &mut frames);
        assert_eq!(watch.agents, Some(1));
        // A write drops last-good; the next fetch fails with nothing to
        // fall back on, so the stream keeps what it published.
        model.invalidate();
        frames.clear();
        model.tick(&mut watch, &mut frames);
        assert_eq!(watch.agents, Some(1), "published an unreachable stand-in");
        assert!(
            frames.iter().all(|f| !f.starts_with("event: agent")),
            "{frames:?}"
        );
        model.tick(&mut watch, &mut frames);
        assert_eq!(watch.agents, Some(3));
    }

    #[test]
    fn the_watcher_never_publishes_a_build_a_write_overtook() {
        // Fetch 1 (the watcher's) is slow; a write lands while it runs and
        // a post-write read builds fetch 2 and keeps it. Fetch 1 ends
        // afterwards: the cache refuses it, and the watcher must not
        // publish it after the newer view.
        let (model, calls) = fake_daemon(
            |n| {
                if n == 1 {
                    Duration::from_millis(500)
                } else {
                    Duration::ZERO
                }
            },
            None,
        );
        let ticking = {
            let m = model.clone();
            std::thread::spawn(move || {
                let mut watch = blank_watch();
                let mut frames = Vec::new();
                m.tick(&mut watch, &mut frames);
                (watch.agents, watch.at)
            })
        };
        std::thread::sleep(Duration::from_millis(100));
        model.invalidate();
        let post_write = model.agents();
        assert_eq!(post_write["v"], 2);
        let (published, _) = ticking.join().unwrap();
        assert_ne!(published, Some(1), "published the pre-write build");
        assert_eq!(published, Some(3), "the watcher fetches a current view");
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn a_tick_skips_a_snapshot_older_than_the_one_it_published() {
        let (model, _) = fake_daemon(|_| Duration::ZERO, None);
        let mut watch = blank_watch();
        let mut frames = Vec::new();
        model.tick(&mut watch, &mut frames);
        let published = watch.agents;
        watch.at = Some(Instant::now() + Duration::from_secs(60));
        model.tick(&mut watch, &mut frames);
        assert_eq!(watch.agents, published, "an older snapshot was published");
    }

    #[test]
    fn a_waiter_retries_when_the_fetch_it_joined_unwinds() {
        let (model, calls) = fake_daemon(|_| Duration::from_millis(300), Some(1));
        let crashing = {
            let m = model.clone();
            std::thread::spawn(move || m.agents())
        };
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(model.agents()["v"], 2);
        assert!(crashing.join().is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn folder_stamp_moves_on_every_input_the_loader_reads() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join("CAD-1");
        fixture(&dir);
        let s0 = folder_stamp(&dir);
        assert_eq!(s0, folder_stamp(&dir), "stable while nothing moves");

        std::fs::write(dir.join("issue.md"), "ab").unwrap();
        let s1 = folder_stamp(&dir);
        assert_ne!(s0, s1, "issue.md edit");

        std::fs::write(dir.join("comments").join("c.md"), "x").unwrap();
        let s2 = folder_stamp(&dir);
        assert_ne!(s1, s2, "new comment");

        std::fs::write(dir.join("comments").join("c.md"), "xy").unwrap();
        let s3 = folder_stamp(&dir);
        assert_ne!(s2, s3, "comment edited in place");

        std::fs::write(dir.join("artifacts").join("a.txt"), "1").unwrap();
        let s4 = folder_stamp(&dir);
        assert_ne!(s3, s4, "new artifact");

        // Same size, same content length — a rename-over still moves the
        // inode, which is how `issue::write` replaces issue.md.
        std::fs::write(dir.join("next.md"), "ab").unwrap();
        std::fs::rename(dir.join("next.md"), dir.join("issue.md")).unwrap();
        assert_ne!(s4, folder_stamp(&dir), "atomic replace");
    }

    #[test]
    fn stable_row_drops_only_the_ticking_fields() {
        let row = json!({
            "alias": "w", "state": "busy", "stalled": false,
            "silent_secs": 12, "ended_secs": 3,
            "awaiting_report": {"message": "m", "since_secs": 40, "remaining_secs": 20},
            "inbox_health": {"unread": 2, "idle_secs": 9, "oldest_unread_age_secs": 9,
                             "warning": "oldest 9s", "stale": false},
        });
        let later = json!({
            "alias": "w", "state": "busy", "stalled": false,
            "silent_secs": 13, "ended_secs": 4,
            "awaiting_report": {"message": "m", "since_secs": 41, "remaining_secs": 19},
            "inbox_health": {"unread": 2, "idle_secs": 10, "oldest_unread_age_secs": 10,
                             "warning": "oldest 10s", "stale": false},
        });
        assert_eq!(value_fp(&stable_row(&row)), value_fp(&stable_row(&later)));
        let mut moved = later.clone();
        moved["inbox_health"]["unread"] = json!(3);
        assert_ne!(value_fp(&stable_row(&row)), value_fp(&stable_row(&moved)));
        moved = later;
        moved["stalled"] = json!(true);
        assert_ne!(value_fp(&stable_row(&row)), value_fp(&stable_row(&moved)));
    }

    #[test]
    fn mailbox_backlog_age_ticks_but_the_backlog_still_moves() {
        let mailbox = |queued: i64, oldest: Option<f64>, age: Option<f64>| {
            json!({"alias": "box", "inbox": {
                "state": if queued > 0 { "backlog" } else { "idle" },
                "queued": queued,
                "oldest_created_at": oldest,
                "oldest_age_secs": age,
            }})
        };
        let one = mailbox(1, Some(100.0), Some(5.0));
        // The clock alone: no change.
        assert_eq!(
            value_fp(&stable_row(&one)),
            value_fp(&stable_row(&mailbox(1, Some(100.0), Some(6.0))))
        );
        // A new message queued: a change.
        assert_ne!(
            value_fp(&stable_row(&one)),
            value_fp(&stable_row(&mailbox(2, Some(100.0), Some(6.0))))
        );
        // The backlog drained: a change.
        assert_ne!(
            value_fp(&stable_row(&one)),
            value_fp(&stable_row(&mailbox(0, None, None)))
        );
    }

    #[test]
    fn aggregate_changes_follow_visible_semantics_not_display_clocks() {
        let projects = json!({"projects": [{"key": "cadence", "issues": 12}]});
        let overview = json!({"generated_at": 10, "projects": [{"key": "cadence", "open_by_status": {"doing": 2}, "oldest_review_age": 10}],
            "needs_me": [{"title": "proposed plan title", "age": 10, "audience": "operator", "summary": "needs decision"}],
            "recent_updates": [{"title": "recent title", "since": 10}], "signed_in": true});
        let mut ticking = overview.clone();
        ticking["generated_at"] = json!(20);
        ticking["projects"][0]["oldest_review_age"] = json!(20);
        ticking["needs_me"][0]["age"] = json!(20);
        assert_eq!(aggregate_fp(&overview), aggregate_fp(&ticking));
        let project_fp = Some(value_fp(&projects));
        let fp = Some(aggregate_fp(&overview));
        assert_eq!(
            aggregate_frame(project_fp, project_fp, fp, Some(aggregate_fp(&ticking))),
            frame("aggregates", &json!({"resources": []})),
            "duplicate aggregate snapshots do not cause collection reads"
        );
        for path in ["title", "audience", "summary"] {
            let mut changed = overview.clone();
            changed["needs_me"][0][path] = json!("changed");
            assert_ne!(
                aggregate_fp(&overview),
                aggregate_fp(&changed),
                "actionable {path} stays in fingerprint"
            );
        }
        let mut below = overview.clone();
        below["needs_me"][0]["age"] = json!(14 * 86400 - 1);
        let mut above = below.clone();
        above["needs_me"][0]["age"] = json!(14 * 86400 + 1);
        assert_eq!(
            aggregate_frame(
                project_fp,
                project_fp,
                Some(aggregate_fp(&below)),
                Some(aggregate_fp(&above))
            ),
            frame("aggregates", &json!({"resources": ["overview"]})),
            "otherwise identical real-shaped Needs-you row crosses Old boundary"
        );
        let mut reordered = overview.clone();
        reordered["needs_me"]
            .as_array_mut()
            .unwrap()
            .push(json!({"kind": "other", "age": 11, "subject": {"kind": "issue", "id": "CAD-7"}}));
        let mut changed_order = reordered.clone();
        changed_order["needs_me"][0]["age"] = json!(12);
        assert_ne!(
            aggregate_fp(&reordered),
            aggregate_fp(&changed_order),
            "age-based rail ordering remains actionable"
        );
        let mut unknown_age = overview.clone();
        unknown_age["future_extension"] = json!({"age": 10});
        let mut changed_unknown = unknown_age.clone();
        changed_unknown["future_extension"]["age"] = json!(20);
        assert_ne!(
            aggregate_fp(&unknown_age),
            aggregate_fp(&changed_unknown),
            "unknown age fields are preserved"
        );
        let mut oldest_changed = overview.clone();
        oldest_changed["projects"][0]["oldest_review_age"] = json!(5);
        assert_ne!(
            aggregate_fp(&overview),
            aggregate_fp(&oldest_changed),
            "a different oldest review is not a mere display-clock tick"
        );
        let mut changed = overview.clone();
        changed["recent_updates"][0]["title"] = json!("changed recent title");
        assert_ne!(aggregate_fp(&overview), aggregate_fp(&changed));
        changed = overview.clone();
        changed["signed_in"] = json!(false);
        assert_ne!(
            aggregate_fp(&overview),
            aggregate_fp(&changed),
            "session data is retained"
        );
        changed = overview.clone();
        changed["projects"][0]["open_by_status"]["doing"] = json!(3);
        assert_eq!(
            aggregate_frame(project_fp, project_fp, fp, Some(aggregate_fp(&changed))),
            frame("aggregates", &json!({"resources": ["overview"]}))
        );
        let mut count = projects.clone();
        count["projects"][0]["issues"] = json!(13);
        assert_eq!(
            aggregate_frame(project_fp, Some(value_fp(&count)), fp, fp),
            frame("aggregates", &json!({"resources": ["projects"]}))
        );
        assert_eq!(
            aggregate_frame(project_fp, project_fp, None, None),
            frame("aggregates", &json!({"resources": ["overview"]})),
            "hidden overview is conservatively marked invalid"
        );
    }

    #[test]
    fn entity_protocol_preserves_target_and_filters_covered_resources() {
        // Every legacy event kind, not only jobs. Entity clients drop
        // collections the stream already patches, and an issues event
        // also drops the aggregates the aggregates frame owns. Anything
        // else — outbox, app_runs, app_outputs, workflows — must remain.
        // A hardcoded list, so deleting one of those from the filter or
        // from the event's resource set fails here.
        let kept = [
            (
                "issues",
                &[
                    "workflows",
                    "apps",
                    "app",
                    "app_runs",
                    "outbox",
                    "app_outputs",
                ][..],
            ),
            (
                "jobs",
                &["overview", "app_runs", "outbox", "app_outputs"][..],
            ),
            ("agents", &["overview", "outbox", "apps", "app"][..]),
            ("monitoring", &["overview"][..]),
        ];
        for (kind, expect) in kept {
            let text = entity_frame(&legacy_frame(kind));
            let data = text
                .split_once("\ndata: ")
                .map(|(_, tail)| tail.trim())
                .unwrap();
            let value: Value = serde_json::from_str(data).unwrap();
            let got: Vec<&str> = value["resources"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r.as_str().unwrap())
                .collect();
            assert_eq!(
                got, expect,
                "{kind} entity frame dropped a covered resource"
            );
            let source = event_resources(kind);
            for dropped in ["issues", "agents", "issue"] {
                if source.contains(&dropped) {
                    assert!(
                        !got.contains(&dropped),
                        "{kind} still refetches patched collection {dropped}"
                    );
                }
            }
            if kind == "issues" {
                assert!(!got.contains(&"projects") && !got.contains(&"overview"));
            }
        }
        let source = frame(
            "issue",
            &json!({"id": "CAD-1", "op": "upsert", "issue": {"id": "CAD-1"}}),
        );
        let optimized = entity_frame(&source);
        assert!(optimized.contains("\"resource\":\"issue\""));
        assert!(optimized.contains("\"rev\":"));
        assert!(optimized.contains("\"id\":\"CAD-1\""));
        assert_eq!(
            optimized,
            entity_frame(&source),
            "stable revision for stable patch"
        );
    }

    #[test]
    fn ordinary_title_is_not_an_overview_input_but_status_and_rendered_titles_are() {
        let issue = |title: &str, status: &str| {
            let mut issue = board::Issue {
                project: "cadence".into(),
                dir: PathBuf::from("cadence/CAD-2"),
                front: model::Front::new("CAD-2", title, "2026-09-01T00:00:00Z"),
                body: "body".into(),
                comments: vec![],
                artifacts: vec![],
            };
            issue.front.status = status.into();
            issue
        };
        let titled = issue("one", "doing");
        let retitled = issue("two", "doing");
        assert_eq!(
            overview_issue_fp(&titled),
            overview_issue_fp(&retitled),
            "ordinary title edits do not move the overview input"
        );
        let mut done = retitled.clone();
        done.front.status = "done".into();
        assert_ne!(
            overview_issue_fp(&titled),
            overview_issue_fp(&done),
            "status changes the overview input"
        );
        let mut plan = titled.clone();
        plan.front.plan = Some(model::Plan {
            state: "proposed".into(),
            proposed_by: "pm".into(),
            proposed_at: "2026-09-01T00:00:00Z".into(),
            tickets: vec![],
            decided_by: None,
            decided_at: None,
            reason: None,
            workflow: None,
        });
        let mut plan_retitled = plan.clone();
        plan_retitled.front.title = "renamed plan".into();
        assert_ne!(
            overview_issue_fp(&plan),
            overview_issue_fp(&plan_retitled),
            "a proposed plan renders its title"
        );
        let mut intake = issue("report", "backlog");
        intake.front.tags = vec!["intake".into()];
        let mut intake_retitled = intake.clone();
        intake_retitled.front.title = "renamed report".into();
        assert_ne!(
            overview_issue_fp(&intake),
            overview_issue_fp(&intake_retitled),
            "an intake row renders its title"
        );
    }

    #[test]
    fn diff_frames_upsert_changed_and_delete_gone() {
        let old: HashMap<String, u64> = [("CAD-1", 1), ("CAD-2", 2), ("CAD-9", 9)]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();
        let new: Entities = [
            ("CAD-1", 1, json!({"id": "CAD-1"})),
            ("CAD-2", 3, json!({"id": "CAD-2", "status": "doing"})),
            ("CAD-10", 10, json!({"id": "CAD-10"})),
        ]
        .into_iter()
        .map(|(k, fp, v)| (k.to_string(), (fp, v)))
        .collect();
        let mut frames = Vec::new();
        diff_frames("issue", &old, &new, &mut frames);
        let text: Vec<&str> = frames.iter().map(|f| &**f).collect();
        assert_eq!(
            text,
            vec![
                "event: issue\ndata: {\"id\":\"CAD-2\",\"issue\":{\"id\":\"CAD-2\",\"status\":\"doing\"},\"op\":\"upsert\"}\n\n",
                "event: issue\ndata: {\"id\":\"CAD-10\",\"issue\":{\"id\":\"CAD-10\"},\"op\":\"upsert\"}\n\n",
                "event: issue\ndata: {\"id\":\"CAD-9\",\"op\":\"delete\"}\n\n",
            ]
        );
    }
}
