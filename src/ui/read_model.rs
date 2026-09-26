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
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};
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
/// While the watcher runs, a daemon snapshot this young is served as is.
const DAEMON_FRESH: Duration = Duration::from_secs(3);
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

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The model for one board — one per `(state dir, PM dir)`, so every
/// request thread and stream of a server shares it.
pub(super) fn get(state_dir: &Path, pm_dir: &Path) -> Arc<Model> {
    lock(&MODELS)
        .entry((state_dir.to_path_buf(), pm_dir.to_path_buf()))
        .or_insert_with(|| {
            Arc::new(Model {
                state_dir: state_dir.to_path_buf(),
                pm_dir: pm_dir.to_path_buf(),
                tracker: Mutex::default(),
                daemon: Mutex::default(),
                overview: Mutex::default(),
                hub: Mutex::default(),
                changed_at: Mutex::default(),
                build_lock: Mutex::default(),
                overview_builds: AtomicU64::new(0),
                request_builds: AtomicU64::new(0),
            })
        })
        .clone()
}

pub(super) struct Model {
    state_dir: PathBuf,
    pm_dir: PathBuf,
    tracker: Mutex<Tracker>,
    daemon: Mutex<Option<Arc<DaemonSnap>>>,
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
}

/// Runs its closure on drop — resets a flag even when a panic unwinds.
struct OnDrop<F: FnMut()>(F);

impl<F: FnMut()> Drop for OnDrop<F> {
    fn drop(&mut self) {
        (self.0)()
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

pub(super) struct DaemonSnap {
    /// When the fetch started — a slower, older fetch never replaces it.
    at: Instant,
    outcomes: board::JobOutcomes,
    /// `None` when `job_list` failed.
    jobs_fp: Option<u64>,
    agents: Value,
    /// `None` when `agent_list` failed.
    agents_fp: Option<u64>,
    approvals: Arc<work::Approvals>,
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
    jobs: Option<u64>,
    agents: Option<u64>,
    monitoring: Option<u64>,
    /// id → fingerprint of the card as the client would render it.
    cards: HashMap<String, u64>,
    plans: HashMap<String, u64>,
    rows: HashMap<String, u64>,
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
        resources.retain(|r| !matches!(r.as_str(), Some("issues" | "agents" | "issue")));
    }
    format!("event: {kind}\ndata: {data}\n\n")
}

fn fps(map: &Entities) -> HashMap<String, u64> {
    map.iter().map(|(k, (fp, _))| (k.clone(), *fp)).collect()
}

impl Model {
    /// The daemon snapshot a read serves: the watcher's, while it runs
    /// and is fresh; otherwise a fetch now.
    fn daemon_snap(&self) -> Arc<DaemonSnap> {
        let watched = lock(&self.hub).running;
        if watched {
            if let Some(snap) = lock(&self.daemon).as_ref() {
                if snap.at.elapsed() < DAEMON_FRESH {
                    return snap.clone();
                }
            }
        }
        let snap = Arc::new(fetch_daemon(&self.state_dir));
        self.keep_snap(snap.clone());
        snap
    }

    /// Keep `snap` unless a newer one is kept or it started before the
    /// last change mark (a fetch racing a write may predate the write).
    fn keep_snap(&self, snap: Arc<DaemonSnap>) {
        if !self.after_change(snap.at) {
            return;
        }
        let mut slot = lock(&self.daemon);
        if slot.as_ref().is_none_or(|old| old.at <= snap.at) {
            *slot = Some(snap);
        }
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

    /// `/api/agents`.
    pub(super) fn agents(&self) -> Value {
        self.daemon_snap().agents.clone()
    }

    /// A board write is about to answer — drop what the daemon side
    /// cached, and refuse anything read before now, so the writer's next
    /// read sees its own write. Tracker writes need nothing more: the
    /// next read re-stamps the folders.
    pub(super) fn invalidate(&self) {
        self.mark_changed(Instant::now());
        *lock(&self.daemon) = None;
        let mut st = lock(&self.overview);
        st.value = None;
        st.sources = None;
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
        (key, head_stamp(&pm.dir)).hash(&mut h);
        (h.finish(), views)
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
        let mut st = lock(&self.overview);
        if st.value.as_ref().is_none_or(|(_, b, _)| *b <= at) {
            st.value = Some((value.clone(), at, key));
        }
    }

    /// `/api/overview`. Served from the cache while it is servable (a
    /// background pass refreshes one older than [`OVERVIEW_FRESH`]);
    /// otherwise built now — one build at a time, later readers take its
    /// result. A tracker-only change reuses a recent daemon pass.
    pub(super) fn overview(self: &Arc<Self>) -> Value {
        let Ok(pm) = Pm::at(&self.pm_dir) else {
            // No tracker to index — the plain build is all daemon.
            return overview::overview_board(&self.state_dir, &self.pm_dir);
        };
        let (key, _) = self.tracker_views(&pm);
        {
            let mut st = lock(&self.overview);
            st.wanted = Some(Instant::now());
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
            let snap = Arc::new(fetch_daemon(&self.state_dir));
            self.keep_snap(snap.clone());
            let (cards, plans) = self.entities(&snap);
            let base = Watch {
                tracker: dir_mtime(&self.pm_dir),
                jobs: snap.jobs_fp,
                agents: snap.agents_fp,
                monitoring: self.monitoring_fp(),
                cards: fps(&cards),
                plans: fps(&plans),
                rows: fps(&agent_rows(&snap)),
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

    fn watch(self: Arc<Self>, mut w: Watch) {
        // A panicking watcher must not leave `running` set — no stream
        // would ever start another. Dropping the senders ends the
        // streams; clients reconnect and start a fresh watcher.
        let me = self.clone();
        let _reset = OnDrop(move || {
            if std::thread::panicking() {
                let mut hub = lock(&me.hub);
                hub.running = false;
                hub.subs.clear();
            }
        });
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
        let tracker = dir_mtime(&self.pm_dir);
        let issues = tracker != w.tracker;
        if issues {
            w.tracker = tracker;
            frames.push(legacy_frame("issues"));
        }
        let snap = Arc::new(fetch_daemon(&self.state_dir));
        self.keep_snap(snap.clone());
        let jobs = snap.jobs_fp != w.jobs;
        if jobs {
            w.jobs = snap.jobs_fp;
            frames.push(legacy_frame("jobs"));
        }
        let agents = snap.agents_fp != w.agents;
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
        if jobs || agents {
            // Bindings and totals can move without an agent row moving.
            frames.push(frame(
                "agent_meta",
                &json!({
                    "daemon": snap.agents["daemon"], "totals": snap.agents["totals"],
                    "by_issue": snap.agents["by_issue"]
                }),
            ));
            let rows = agent_rows(&snap);
            diff_frames("agent", &w.rows, &rows, frames);
            w.rows = fps(&rows);
        }
        if jobs || agents || monitoring {
            // Before the frames go out: the refetch they trigger must not
            // be served anything read before this tick.
            self.mark_changed(started);
            self.nudge_overview();
        }
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
    fn entity_protocol_preserves_target_and_filters_covered_resources() {
        let legacy = legacy_frame("jobs");
        let text = entity_frame(&legacy);
        assert!(!text.contains("\"issues\""));
        assert!(!text.contains("\"agents\""));
        assert!(text.contains("\"overview\""));
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
