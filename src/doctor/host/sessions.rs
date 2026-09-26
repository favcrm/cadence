//! CAD-536: `cadence doctor host` check `sessions` — moved verbatim from src/doctor/host.rs.

use super::*;

use std::collections::BTreeMap;

// ---------- owned session trees (CAD-198; CAD-188 phase 1) ----------
//
// Read-only census answering "which agent session trees are on this
// host, who owns each, and what would the unowned in-scope ones free".
// Ownership is a three-way join — registry row (UNSCOPED), the recorded
// endpoint pid, the live process tree — and every disagreement mode is
// named rather than guessed. Nothing here acts: no kills, stops,
// deletes, checkpoints or writes of any kind.
//
// Rules carried from the ops audit
// (/var/www/agent-notes/20260920-160600-ops-cad188-session-gc-audit.md):
//   * The registry read is unscoped BY CONSTRUCTION — the store opens
//     read-only and every agents row is read. `cadence agent list`
//     silently group-scopes under CADENCE_ALIAS; a scoped absence must
//     never prove "unowned" (12 of 20 agents were invisible that way).
//     `registry_scope` is printed so the consumer can see which view
//     produced the classification.
//   * Tree totals are PSS (smaps_rollup) and VmSwap (status) summed
//     once per member pid — never RSS sums and never sums of
//     per-family totals: MCP wrappers are children of the sessions
//     that spawned them, so their cost is already inside the tree.
//   * argv and env are never opened (CAD-141); identity is comm, the
//     (pid, start_jiffies) pair and the cwd/exe links only — and
//     start_jiffies is enforced, not just printed: a recorded
//     endpoint pid that resolves to a process newer than the row's
//     last write is reuse, and no claim may ride on it.
//   * A tree is a reclaim candidate only when ownership is proven
//     absent (ProcessOnly on a fully-read registry) AND the root runs
//     as the daemon's euid AND its cwd lands inside this pm's
//     registered projects — in the same mount namespace, on a live
//     (not deleted) cwd, under scope roots that are themselves
//     provable directories. Foreign users, foreign projects,
//     disputed ownership and unreadable evidence are all listed but
//     protected.

/// comm families that can root an agent session tree — the providers
/// cadence spawns (managed stdio) or a pane can run (pty). Membership
/// is by kernel comm alone; argv is never opened. A provider added
/// without a row here is silently missed by the census — fail-open on
/// *listing* only (the session is invisible, never misclaimed as a
/// candidate or an owner).
const SESSION_FAMILIES: &[&str] = &["claude", "codex", "cursor", "devin"];

/// `VmSwap`/`Pss` reads are per-pid; an `smaps_rollup` absent or denied
/// for part of a tree marks the totals partial, never wrong.
struct TreeMetrics {
    pss_bytes: u64,
    swap_bytes: u64,
    pss_missing: u32,
    swap_missing: u32,
    /// The member list outgrew `MAX_METRIC_PIDS` — totals are a lower
    /// bound over the first N members only.
    truncated: bool,
}

/// Per-member `smaps_rollup`/`status` reads are capped — a session
/// that forked a build must not make `doctor` map-walk the whole
/// build on every `session start`/`session end`. Over the cap the
/// tree's totals are a flagged lower bound, never an estimate.
pub(super) const MAX_METRIC_PIDS: usize = 512;

/// How a live session tree and the registry agree — the CAD-188
/// closed set minus `RecordOnly` (a registry row with no live tree is
/// not a tree; those rows are listed under `records_only`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Agreement {
    /// Registry row + live tree, joined on the recorded endpoint pid.
    Agreed,
    /// Live tree, no registry row — proven against an unscoped read
    /// of a store that opened (or is provably absent: no rows exist).
    ProcessOnly,
    /// The registry generation contradicts the generation a live
    /// running turn token embeds — fence, never a candidate.
    GenerationMismatch,
    /// The join could not be proven — store unreadable, endpoint pid
    /// claimed twice, /proc raced. Fail-closed: never a candidate.
    Unknown,
}

impl Agreement {
    fn as_str(self) -> &'static str {
        match self {
            Self::Agreed => "agreed",
            Self::ProcessOnly => "process-only",
            Self::GenerationMismatch => "generation-mismatch",
            Self::Unknown => "unknown",
        }
    }
}

/// Where the tree's root cwd sits relative to this pm's projects —
/// scope is proven by path, never by alias-name pattern.
#[derive(Clone, PartialEq, Eq)]
enum Scope {
    /// Inside a registered project repo (its key) or the pm dir.
    Project(String),
    /// Resolved but matching nothing registered — a foreign project
    /// or unrelated session; protected, never a candidate.
    Foreign,
    /// cwd unreadable, deleted, or in a different mount namespace —
    /// scope cannot be proven.
    Unproven,
}

impl Scope {
    fn as_str(&self) -> &str {
        match self {
            Self::Project(k) => k,
            Self::Foreign => "foreign",
            Self::Unproven => "unproven",
        }
    }
}

/// One live session tree — the census row.
struct SessionTree {
    root_pid: u32,
    /// starttime jiffies — pid+start is the identity; pid alone is
    /// not (PID reuse is CAD-188 §10).
    root_start_jiffies: u64,
    /// The root's real uid (`status` Uid:) — a session owned by
    /// another user is never a candidate, whatever its cwd says.
    /// `None` = unreadable → uid unproven → still not a candidate.
    root_uid: Option<u32>,
    /// The root's `ns/mnt` differs from ours — its cwd resolves in
    /// another namespace, so path-based scope cannot be proven.
    root_ns_foreign: bool,
    root_family: String,
    root_cwd: Option<PathBuf>,
    root_cwd_deleted: bool,
    root_cpu_secs: u64,
    age_secs: Option<u64>,
    /// Every member's (pid, start_jiffies) — attribution and a later
    /// phase's recheck material; each pid appears in one tree only.
    members: Vec<(u32, u64)>,
    metrics: TreeMetrics,
    owner: Option<usize>,
    agreement: Agreement,
    agreement_why: String,
    scope: Scope,
}

/// One `agents` row plus the message facts the census can prove.
struct RegAgent {
    alias: String,
    provider: String,
    endpoint_kind: String,
    generation: Option<String>,
    /// agents.pid — the pane pid for pty endpoints, the provider
    /// process for managed ones; a stale value is a fact, not an
    /// error.
    endpoint_pid: Option<u32>,
    /// agents.cwd — where the row's endpoint lives. Used to fence
    /// "unowned" when the recorded pid has gone stale: a tree rooted
    /// under a stale row's cwd is plausibly that row's session.
    cwd: Option<String>,
    state: String,
    reason: Option<String>,
    queued: u64,
    running: u64,
    /// Generation embedded in a live running turn token
    /// (`<kind>-<generation>-<uuid>`) — the only live-endpoint
    /// generation readable without the daemon.
    running_generation: Option<String>,
    /// Newest completed/started message time, else the row's updated.
    last_progress: Option<f64>,
    /// agents.updated — the row's last write. An endpoint pid is
    /// recorded together with a write, so the process the pid names
    /// can never be NEWER than this: a later start means reuse.
    updated: Option<f64>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RegStore {
    /// No `cadence.sqlite3` — provably zero rows, so "no registry row"
    /// is a proven fact, not a gap.
    Absent,
    Open,
    /// Present but not openable read-only — ownership is UNPROVEN for
    /// every tree; nothing may classify ProcessOnly.
    Unreadable,
}

impl RegStore {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Absent => "absent",
            Self::Open => "open",
            Self::Unreadable => "unreadable",
        }
    }
}

struct RegEvidence {
    store: RegStore,
    agents: Vec<RegAgent>,
}

/// agents + pending/progress facts from `cadence.sqlite3`, opened
/// SQLITE_OPEN_READ_ONLY — a census must not migrate or create the
/// file on a host that never ran the daemon. Caveat: the store is
/// WAL, so a read-only connection still touches `-shm`; a store that
/// exists but can't be prepared comes back `Unreadable` and every
/// tree classifies `unknown` — fail-closed, and silent in the sense
/// that no tree row will say why beyond the store marker.
fn registry_evidence(scan: &Scan) -> RegEvidence {
    let path = scan.state_dir.join("cadence.sqlite3");
    if !path.exists() {
        return RegEvidence {
            store: RegStore::Absent,
            agents: Vec::new(),
        };
    }
    let Ok(conn) = crate::store::open_read_only(&path) else {
        return RegEvidence {
            store: RegStore::Unreadable,
            agents: Vec::new(),
        };
    };
    let mut ev = RegEvidence {
        store: RegStore::Open,
        agents: Vec::new(),
    };
    let Ok(mut st) = conn.prepare(
        "SELECT alias, provider, endpoint_kind, generation, pid, state, \
         error, cwd, updated FROM agents ORDER BY alias",
    ) else {
        // A store whose agents table is unreadable/unmigrated is as
        // good as closed for ownership purposes — fail closed.
        ev.store = RegStore::Unreadable;
        return ev;
    };
    let mut index = std::collections::HashMap::new();
    let Ok(rows) = st.query_map([], |r| {
        Ok(RegAgent {
            alias: r.get(0)?,
            provider: r.get(1)?,
            endpoint_kind: r.get(2)?,
            generation: r.get(3)?,
            endpoint_pid: r
                .get::<_, Option<i64>>(4)?
                .and_then(|p| u32::try_from(p).ok()),
            state: r.get(5)?,
            reason: r.get(6)?,
            cwd: r.get::<_, Option<String>>(7)?,
            queued: 0,
            running: 0,
            running_generation: None,
            last_progress: r.get::<_, Option<f64>>(8)?,
            updated: r.get::<_, Option<f64>>(8)?,
        })
    }) else {
        ev.store = RegStore::Unreadable;
        return ev;
    };
    for (i, row) in rows.flatten().enumerate() {
        index.insert(row.alias.clone(), i);
        ev.agents.push(row);
    }
    if let Ok(mut st) = conn.prepare(
        "SELECT alias, state, COUNT(*) FROM messages \
         WHERE state IN ('queued','submitting','running') \
         GROUP BY alias, state",
    ) {
        if let Ok(rows) = st.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, u64>(2)?,
            ))
        }) {
            for row in rows.flatten() {
                if let Some(a) = index.get(&row.0).map(|i| &mut ev.agents[*i]) {
                    if row.1 == "running" {
                        a.running = row.2;
                    } else {
                        a.queued += row.2;
                    }
                }
            }
        }
    }
    if let Ok(mut st) = conn.prepare(
        "SELECT alias, MAX(COALESCE(completed, started, created)) \
         FROM messages GROUP BY alias",
    ) {
        if let Ok(rows) = st.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<f64>>(1)?))
        }) {
            for (alias, at) in rows.flatten() {
                if let (Some(a), Some(at)) = (index.get(&alias).map(|i| &mut ev.agents[*i]), at) {
                    a.last_progress = Some(at.max(a.last_progress.unwrap_or(0.0)));
                }
            }
        }
    }
    // A running turn's token embeds the endpoint generation that
    // accepted it (`<kind>-<generation>-<uuid>`) — the one piece of
    // live-endpoint state readable without the daemon.
    if let Ok(mut st) = conn.prepare(
        "SELECT alias, turn_id FROM messages \
         WHERE state='running' AND turn_id IS NOT NULL",
    ) {
        if let Ok(rows) = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        {
            for (alias, token) in rows.flatten() {
                if let Some(a) = index.get(&alias).map(|i| &mut ev.agents[*i]) {
                    // `<kind>-<generation>-<uuid>`; the generation is a
                    // simple uuid — 32 hex, no dashes. Any other token
                    // shape is not generation evidence.
                    let gen = token
                        .strip_prefix(&format!("{}-", a.endpoint_kind))
                        .and_then(|rest| rest.split('-').next())
                        .filter(|g| g.len() == 32 && g.chars().all(|c| c.is_ascii_hexdigit()));
                    if let Some(gen) = gen {
                        a.running_generation = Some(gen.to_string());
                    }
                }
            }
        }
    }
    ev
}

/// Every pid's `stat` row — the census walk. cwd/exe links are read
/// only for the handful of session roots afterwards, not per pid.
fn collect_procs(proc_root: &Path) -> (BTreeMap<u32, ProcStat>, u64, u64) {
    let mut procs = BTreeMap::new();
    let mut unreadable = 0_u64;
    let mut vanished = 0_u64;
    let Ok(pids) = std::fs::read_dir(proc_root) else {
        return (procs, unreadable, vanished);
    };
    for ent in pids.flatten() {
        let Some(pid) = ent.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        match proc_stat(&ent.path()) {
            Some(st) => {
                procs.insert(pid, st);
            }
            None if ent.path().exists() => unreadable += 1,
            None => vanished += 1,
        }
    }
    (procs, unreadable, vanished)
}

/// Does walking ppid from `pid` reach a session-family ancestor —
/// cycle-safe, bounded by the process count. A session comm with such
/// an ancestor is a member of that session's tree, not a root.
fn has_session_ancestor(pid: u32, procs: &BTreeMap<u32, ProcStat>) -> bool {
    let mut seen = std::collections::HashSet::new();
    let mut cur = procs.get(&pid).map(|p| p.ppid).unwrap_or(0);
    while cur != 0 && seen.insert(cur) {
        match procs.get(&cur) {
            None => return false, // parent gone/unreadable — chain ends
            Some(p) if SESSION_FAMILIES.contains(&comm_family(&p.comm).as_str()) => {
                return true;
            }
            Some(p) => cur = p.ppid,
        }
    }
    false
}

/// `/proc/<pid>/cwd` — read_link target, with the kernel's
/// " (deleted)" suffix split out so a dead worktree is visible.
fn proc_cwd(pid_dir: &Path) -> (Option<PathBuf>, bool) {
    match std::fs::read_link(pid_dir.join("cwd")) {
        Ok(p) => {
            let s = p.to_string_lossy();
            if let Some(live) = s.strip_suffix(" (deleted)") {
                (Some(PathBuf::from(live)), true)
            } else {
                (Some(p), false)
            }
        }
        Err(_) => (None, false),
    }
}

/// `smaps_rollup` `Pss:` in bytes — the proportional figure the audit
/// validated. `None` = absent (kernel <4.15, fixture) or denied.
fn proc_pss(pid_dir: &Path) -> Option<u64> {
    let text = std::fs::read_to_string(pid_dir.join("smaps_rollup")).ok()?;
    let v = text.lines().find_map(|l| l.strip_prefix("Pss:"))?;
    let kb: u64 = v.trim().trim_end_matches("kB").trim().parse().ok()?;
    Some(kb * 1024)
}

/// `status` `VmSwap:` in bytes — the cost RSS hides (idle wrappers
/// are swapped out). A readable status without the line means the
/// kernel reports no swap for the pid — 0, not missing.
fn proc_swap(pid_dir: &Path) -> Option<u64> {
    let text = std::fs::read_to_string(pid_dir.join("status")).ok()?;
    let v = text
        .lines()
        .find_map(|l| l.strip_prefix("VmSwap:"))
        .unwrap_or("0");
    let kb: u64 = v.trim().trim_end_matches("kB").trim().parse().ok()?;
    Some(kb * 1024)
}

/// `status` `Uid:` real uid — the ownership axis a foreign user's
/// session fails. `None` = status unreadable or field absent: uid
/// unproven, and unproven is never "ours".
fn proc_uid(pid_dir: &Path) -> Option<u32> {
    let text = std::fs::read_to_string(pid_dir.join("status")).ok()?;
    text.lines()
        .find_map(|l| l.strip_prefix("Uid:"))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

/// True when the pid lives in a different mount namespace than the
/// doctor — its `/proc/<pid>/cwd` then resolves in the target's root,
/// so a cwd string matching a registered repo is not scope proof.
/// Missing links (fixtures, restricted procfs) read as same-ns.
fn proc_ns_foreign(proc_root: &Path, pid: u32) -> bool {
    let ours = std::fs::read_link(proc_root.join("self/ns/mnt"));
    let theirs = std::fs::read_link(proc_root.join(pid.to_string()).join("ns/mnt"));
    matches!((ours, theirs), (Ok(o), Ok(t)) if o != t)
}

/// The per-pid PSS+swap pass, run over a tree's members once each —
/// partial reads are counted, totals stay measured-not-estimated.
/// Past `MAX_METRIC_PIDS` the pass stops and marks itself truncated.
fn tree_metrics(proc_root: &Path, members: &[u32]) -> TreeMetrics {
    let mut m = TreeMetrics {
        pss_bytes: 0,
        swap_bytes: 0,
        pss_missing: 0,
        swap_missing: 0,
        truncated: members.len() > MAX_METRIC_PIDS,
    };
    for pid in members.iter().take(MAX_METRIC_PIDS) {
        let dir = proc_root.join(pid.to_string());
        match proc_pss(&dir) {
            Some(b) => m.pss_bytes += b,
            None => m.pss_missing += 1,
        }
        match proc_swap(&dir) {
            Some(b) => m.swap_bytes += b,
            None => m.swap_missing += 1,
        }
    }
    m
}

/// Session roots = topmost session-family pids; each tree is the
/// transitive descendants of its root, each pid counted once — an
/// MCP wrapper lands inside its session's tree, never beside it.
fn session_trees(
    procs: &BTreeMap<u32, ProcStat>,
    proc_root: &Path,
    uptime: Option<f64>,
) -> Vec<SessionTree> {
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) }.max(1) as u64;
    let mut children: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    for (pid, st) in procs {
        children.entry(st.ppid).or_default().push(*pid);
    }
    let mut roots: Vec<u32> = procs
        .iter()
        .filter(|(pid, st)| {
            SESSION_FAMILIES.contains(&comm_family(&st.comm).as_str())
                && !has_session_ancestor(**pid, procs)
        })
        .map(|(pid, _)| *pid)
        .collect();
    roots.sort_unstable();
    let mut trees = Vec::new();
    for root in roots {
        // Visited set: a ppid cycle from mid-walk pid reuse must not
        // loop the DFS — `members` itself is built from `seen`.
        let mut members = Vec::new();
        let mut seen = std::collections::HashSet::new();
        seen.insert(root);
        let mut stack = vec![root];
        while let Some(pid) = stack.pop() {
            members.push(pid);
            if let Some(kids) = children.get(&pid) {
                for &kid in kids {
                    if seen.insert(kid) {
                        stack.push(kid);
                    }
                }
            }
        }
        members.sort_unstable();
        let st = &procs[&root];
        let root_dir = proc_root.join(root.to_string());
        let (cwd, cwd_deleted) = proc_cwd(&root_dir);
        let started = st.start_jiffies / hz;
        let member_rows: Vec<(u32, u64)> = members
            .iter()
            .map(|p| (*p, procs.get(p).map(|s| s.start_jiffies).unwrap_or(0)))
            .collect();
        trees.push(SessionTree {
            root_pid: root,
            root_start_jiffies: st.start_jiffies,
            root_uid: proc_uid(&root_dir),
            root_ns_foreign: proc_ns_foreign(proc_root, root),
            root_family: comm_family(&st.comm),
            root_cwd: cwd,
            root_cwd_deleted: cwd_deleted,
            root_cpu_secs: st.cpu_jiffies / hz,
            age_secs: uptime.map(|u| (u as u64).saturating_sub(started)),
            members: member_rows,
            metrics: tree_metrics(proc_root, &members),
            owner: None,
            agreement: Agreement::Unknown,
            agreement_why: String::new(),
            scope: Scope::Unproven,
        });
    }
    trees
}

/// Canonicalised `(label, path)` pairs that prove a tree's cwd is
/// inside this pm — one per registered project repo, plus the pm dir
/// itself. No pm.yaml → no scope proof → every unowned tree is
/// `unproven`, never foreign and never a candidate.
///
/// A registered path must name a directory INSIDE the host: empty or
/// relative strings are skipped (`Path::starts_with("")` is true for
/// everything), canonicalisation must succeed, and a path that
/// resolves to `/` or to the scan's home dir would scope the whole
/// host — skipped too. One bad pm.yaml line must never widen scope.
fn scope_roots(scan: &Scan) -> Vec<(String, PathBuf)> {
    let mut roots = Vec::new();
    let Some(pm) = &scan.pm_dir else {
        return roots;
    };
    let home = std::fs::canonicalize(&scan.home).unwrap_or_else(|_| scan.home.clone());
    let mut push = |label: String, raw: &Path| {
        let Some(canon) = (raw.is_absolute() && !raw.as_os_str().is_empty())
            .then(|| std::fs::canonicalize(raw).ok())
            .flatten()
        else {
            return;
        };
        if canon == Path::new("/") || canon == home {
            return;
        }
        roots.push((label, canon));
    };
    for project in crate::issue::project::list(pm).unwrap_or_default() {
        for repo in &project.repos {
            if let Some(path) = &repo.path {
                let expanded = crate::issue::project::expand_home(path);
                push(format!("project:{}", project.key), &expanded);
            }
        }
    }
    push("pm".to_string(), pm);
    roots
}

/// What /proc proves about one row's recorded endpoint pid. `Live`
/// is the only state that may carry ownership; the rest are stale
/// or unverifiable claims that must fence, never bind.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ClaimState {
    /// No endpoint pid recorded (inbox agents) — not a claim.
    None,
    /// pid alive, same uid, start bound by the row's last write.
    Live,
    /// Recorded pid is not in /proc — the endpoint is gone.
    Dead,
    /// Live pid but started AFTER the row's last write — reuse.
    Reused,
    /// Live pid owned by another uid — not this daemon's endpoint.
    ForeignUid,
    /// uid or start-time unreadable — the claim can't be proven.
    Unverifiable,
}

impl ClaimState {
    fn as_str(self) -> &'static str {
        match self {
            Self::None => "none-recorded",
            Self::Live => "live",
            Self::Dead => "dead",
            Self::Reused => "reused",
            Self::ForeignUid => "foreign-uid",
            Self::Unverifiable => "unverifiable",
        }
    }
}

/// Slack between a process's wall-clock start and the row's `updated`
/// write: open records the pid and bumps `updated` in one step, and
/// later state writes only push `updated` further out — so a start
/// within the tolerance is the recorded process, one past it is not.
const CLAIM_START_TOLERANCE_SECS: f64 = 120.0;

/// Classify one row's endpoint-pid claim against live /proc. The
/// start-time agreement proof: `updated` is written when the pid is
/// recorded and on every later state write, so the recorded process
/// can never have started after it — `proc_start > updated + slack`
/// means the pid was recycled onto a different process.
fn classify_claim(
    a: &RegAgent,
    procs: &BTreeMap<u32, ProcStat>,
    proc_root: &Path,
    uptime: Option<f64>,
    now: SystemTime,
    euid: u32,
) -> ClaimState {
    let Some(pid) = a.endpoint_pid else {
        return ClaimState::None;
    };
    let Some(st) = procs.get(&pid) else {
        return ClaimState::Dead;
    };
    match proc_uid(&proc_root.join(pid.to_string())) {
        Some(u) if u != euid => return ClaimState::ForeignUid,
        None => return ClaimState::Unverifiable,
        _ => {}
    }
    let (Some(up), Some(updated)) = (uptime, a.updated) else {
        return ClaimState::Unverifiable;
    };
    let Ok(now_s) = now.duration_since(std::time::UNIX_EPOCH) else {
        return ClaimState::Unverifiable;
    };
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) }.max(1) as f64;
    let start_wall = now_s.as_secs_f64() - up + st.start_jiffies as f64 / hz;
    if start_wall > updated + CLAIM_START_TOLERANCE_SECS {
        ClaimState::Reused
    } else {
        ClaimState::Live
    }
}

/// Does the tree's root cwd sit at or under a row's `agents.cwd` —
/// i.e. could this be that row's session? Both sides canonicalised;
/// an unresolvable row cwd proves no overlap.
fn cwd_overlaps(agent_cwd: Option<&String>, tree_cwd: Option<&PathBuf>) -> bool {
    let (Some(a), Some(t)) = (agent_cwd, tree_cwd) else {
        return false;
    };
    let ac = std::fs::canonicalize(a).unwrap_or_else(|_| PathBuf::from(a));
    let tc = std::fs::canonicalize(t).unwrap_or_else(|_| t.clone());
    tc.starts_with(&ac)
}

/// The three-way join — live tree ↔ agents row via the recorded
/// endpoint pid, over an UNSCOPED agents list. Only `Live` claims
/// (pid+start bound to the row's last write, same uid) may bind; a
/// pid claimed by two rows is ambiguous for both; and a row whose
/// recorded pid went stale while its cwd still covers a tree makes
/// that tree `Unknown`, never `ProcessOnly`. Ambiguity is `Unknown`,
/// not a guess.
fn join_ownership(
    trees: &mut [SessionTree],
    ev: &RegEvidence,
    procs: &BTreeMap<u32, ProcStat>,
    claims: &[ClaimState],
) {
    let mut claim: BTreeMap<u32, Vec<usize>> = BTreeMap::new();
    for (i, a) in ev.agents.iter().enumerate() {
        if let Some(p) = a.endpoint_pid {
            claim.entry(p).or_default().push(i);
        }
    }
    let mut members: std::collections::HashSet<u32> = std::collections::HashSet::new();
    let mut seen_ancestor = std::collections::HashSet::new();
    for tree in trees.iter_mut() {
        // The lineage an endpoint pid may legitimately sit on: the
        // root itself (managed), an ancestor (pty pane pid is the
        // shell above the provider), or a member of the tree.
        members.clear();
        members.extend(tree.members.iter().map(|(pid, _)| *pid));
        let mut cur = procs.get(&tree.root_pid).map(|p| p.ppid).unwrap_or(0);
        seen_ancestor.clear();
        while cur != 0 && seen_ancestor.insert(cur) {
            match procs.get(&cur) {
                Some(p) => {
                    members.insert(cur);
                    cur = p.ppid;
                }
                None => break,
            }
        }
        // Claims on the lineage, split by what /proc proves about them.
        let mut live_hits: Vec<usize> = Vec::new();
        let mut stale_hits: Vec<usize> = Vec::new();
        for (pid, owners) in &claim {
            if members.contains(pid) {
                for &i in owners {
                    match claims[i] {
                        ClaimState::Live => live_hits.push(i),
                        ClaimState::None => {}
                        _ => stale_hits.push(i),
                    }
                }
            }
        }
        live_hits.sort_unstable();
        live_hits.dedup();
        stale_hits.sort_unstable();
        stale_hits.dedup();
        match (live_hits.as_slice(), stale_hits.as_slice()) {
            ([], []) => {
                // No row's recorded pid is anywhere on the lineage.
                // Before calling that ProcessOnly, fence on rows whose
                // claim went stale while their cwd still covers this
                // tree — a daemon restart/pane respawn leaves exactly
                // that shape, and the session may be theirs.
                let stale_owner = ev.agents.iter().enumerate().find(|(i, a)| {
                    !matches!(claims[*i], ClaimState::Live | ClaimState::None)
                        && cwd_overlaps(a.cwd.as_ref(), tree.root_cwd.as_ref())
                });
                match (stale_owner, &ev.store) {
                    (Some((_, a)), _) => {
                        tree.agreement = Agreement::Unknown;
                        tree.agreement_why = format!(
                            "row {} holds a stale endpoint pid under this cwd — \
                             ownership unproven",
                            a.alias
                        );
                    }
                    (None, RegStore::Open) => {
                        tree.agreement = Agreement::ProcessOnly;
                        tree.agreement_why =
                            "no agents row claims this tree (unscoped registry read)".to_string();
                    }
                    (None, RegStore::Absent) => {
                        tree.agreement = Agreement::ProcessOnly;
                        tree.agreement_why =
                            "no cadence.sqlite3 — no registry rows exist".to_string();
                    }
                    (None, RegStore::Unreadable) => {
                        tree.agreement = Agreement::Unknown;
                        tree.agreement_why =
                            "cadence.sqlite3 unreadable — ownership unproven".to_string();
                    }
                }
            }
            ([], stale) => {
                tree.agreement = Agreement::Unknown;
                let why: Vec<String> = stale
                    .iter()
                    .map(|&i| format!("{} ({})", ev.agents[i].alias, claims[i].as_str()))
                    .collect();
                tree.agreement_why = format!(
                    "recorded endpoint pid is not the live process — {}",
                    why.join(", ")
                );
            }
            ([one], []) => {
                let a = &ev.agents[*one];
                tree.owner = Some(*one);
                match (&a.generation, &a.running_generation) {
                    (Some(recorded), Some(live)) if recorded != live => {
                        tree.agreement = Agreement::GenerationMismatch;
                        tree.agreement_why = format!(
                            "registry generation {}… ≠ running turn's {}",
                            recorded.chars().take(8).collect::<String>(),
                            live.chars().take(8).collect::<String>()
                        );
                    }
                    _ => {
                        tree.agreement = Agreement::Agreed;
                        tree.agreement_why =
                            format!("endpoint pid+start claims the tree ({})", a.alias);
                    }
                }
            }
            (live, stale) => {
                tree.agreement = Agreement::Unknown;
                tree.agreement_why = format!(
                    "endpoint pid claimed by {} live + {} stale registry rows — ambiguous",
                    live.len(),
                    stale.len()
                );
            }
        }
    }
}

/// cwd → scope verdict. Both sides canonicalised. A deleted cwd only
/// names where the process stood — the directory is gone, and the
/// ` (deleted)` suffix is a string a directory can literally carry —
/// so deletion is `unproven`, not a match. A foreign mount namespace
/// makes the cwd string incomparable → `unproven`. An unreadable cwd
/// proves nothing → `unproven`, never foreign.
fn classify_scope(tree: &mut SessionTree, roots: &[(String, PathBuf)]) {
    let Some(cwd) = &tree.root_cwd else {
        tree.scope = Scope::Unproven;
        return;
    };
    if tree.root_cwd_deleted || tree.root_ns_foreign {
        tree.scope = Scope::Unproven;
        return;
    }
    let canon = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.clone());
    for (label, root) in roots {
        if canon.starts_with(root) {
            tree.scope = Scope::Project(label.clone());
            return;
        }
    }
    tree.scope = Scope::Foreign;
}

/// Reclaim verdict for one tree. Candidates are exactly the
/// ProcessOnly + same-uid + in-scope trees; everything else is
/// protected with the reason this census can prove. `confidence`
/// weighs age (the audit's tiers: ~3 days = high, hours = medium)
/// and accounting completeness.
fn reclaim_verdict(
    tree: &SessionTree,
    ev: &RegEvidence,
    euid: u32,
) -> (bool, Option<String>, String, String) {
    // (candidate, protected_reason, confidence, basis)
    let protected = |why: String| (false, Some(why), String::new(), String::new());
    match tree.agreement {
        Agreement::Agreed => {
            let alias = tree
                .owner
                .map(|i| ev.agents[i].alias.as_str())
                .unwrap_or("?");
            protected(format!("owned — registry row {alias}"))
        }
        Agreement::GenerationMismatch => {
            protected("generation disagreement — fenced, never a candidate".to_string())
        }
        Agreement::Unknown => protected(format!("unknown — {}", tree.agreement_why)),
        Agreement::ProcessOnly => {
            // uid before scope: a foreign user's tree inside a
            // registered repo is foreign, not a candidate — and a
            // root whose uid can't be read is unproven.
            match tree.root_uid {
                Some(u) if u != euid => {
                    return protected(format!("root owned by uid {u} — foreign user"));
                }
                None => {
                    return protected("root uid unreadable — ownership unproven".to_string());
                }
                _ => {}
            }
            match &tree.scope {
                Scope::Foreign => {
                    protected("outside this pm's project scope — foreign".to_string())
                }
                Scope::Unproven => protected(
                    "root cwd unreadable, deleted, or in another mount \
                     namespace — project scope unproven"
                        .to_string(),
                ),
                Scope::Project(_) => {
                    let mut why = Vec::new();
                    let mut confidence = match tree.age_secs {
                        Some(a) if a >= 72 * 3_600 => "high",
                        Some(a) if a >= 4 * 3_600 => "medium",
                        _ => "low",
                    };
                    match tree.age_secs {
                        Some(a) => why.push(format!("root age {}h", a / 3_600)),
                        None => {
                            confidence = "low";
                            why.push("age unproven (no /proc/uptime)".to_string());
                        }
                    }
                    if tree.metrics.truncated {
                        if confidence == "high" {
                            confidence = "medium";
                        }
                        why.push(format!(
                            "accounting truncated at {} members — totals are a lower bound",
                            MAX_METRIC_PIDS
                        ));
                    }
                    if tree.metrics.pss_missing + tree.metrics.swap_missing > 0 {
                        if confidence == "high" {
                            confidence = "medium";
                        }
                        why.push(format!(
                            "partial accounting ({} pids missing PSS, {} missing swap)",
                            tree.metrics.pss_missing, tree.metrics.swap_missing
                        ));
                    }
                    (
                        true,
                        None,
                        confidence.to_string(),
                        format!("unowned and in-scope; {}", why.join("; ")),
                    )
                }
            }
        }
    }
}

/// `doctor --host`'s owned-session-tree census — the CAD-188 phase-1
/// surface. It acts on nothing, but the level follows the evidence:
/// an unreadable registry is an evidence failure (every tree becomes
/// `unknown`, and a watchdog keying on the exit code must not see 0),
/// and a live candidate list warns so `render` actually prints the
/// authorisation remedy — remedies render only for non-ok levels, so
/// a constant `Ok` would ship a caveat that cannot print. `session
/// start`/`end` map warn to exit 1 ("GO with warnings") — honest for
/// both cases: a human's `claude` in the repo stays visible, and a
/// broken registry is loud.
pub(super) fn check_sessions(scan: &Scan) -> Check {
    let name = "sessions";
    let threshold = json!(
        "warn: registry unreadable, or unowned in-scope session trees present \
         (dry-run census — never an action)"
    );
    if !scan.linux {
        return check(
            name,
            Level::Ok,
            json!({"skipped": true}),
            threshold,
            "session census is linux-only".to_string(),
            String::new(),
        );
    }
    let ev = registry_evidence(scan);
    let (procs, unreadable, vanished) = collect_procs(&scan.proc_root);
    let uptime = proc_uptime(&scan.proc_root);
    let claims: Vec<ClaimState> = ev
        .agents
        .iter()
        .map(|a| classify_claim(a, &procs, &scan.proc_root, uptime, scan.now, scan.uid))
        .collect();
    let mut trees = session_trees(&procs, &scan.proc_root, uptime);
    join_ownership(&mut trees, &ev, &procs, &claims);
    let roots = scope_roots(scan);
    for tree in &mut trees {
        classify_scope(tree, &roots);
    }
    // Registry rows no live tree claimed — the record-only class:
    // rows with a dead endpoint pid, or none at all (inbox). Still
    // listed so the census shows the whole registry it read, with
    // what /proc proved about each recorded endpoint pid.
    let records_only: Vec<Value> = ev
        .agents
        .iter()
        .enumerate()
        .filter(|(i, _)| !trees.iter().any(|t| t.owner == Some(*i)))
        .map(|(i, a)| {
            json!({
                "alias": a.alias,
                "provider": a.provider,
                "endpoint_kind": a.endpoint_kind,
                "state": a.state,
                "reason": a.reason,
                "endpoint_pid": a.endpoint_pid,
                "endpoint_state": claims[i].as_str(),
                "cwd": a.cwd,
                "queued": a.queued,
                "running": a.running,
                "last_progress": a.last_progress,
                "agreement": "record-only",
            })
        })
        .collect();
    let mut candidates: Vec<(u32, String)> = Vec::new();
    let mut rows = Vec::new();
    let mut cand_pss = 0_u64;
    let mut cand_swap = 0_u64;
    let mut cand_truncated = false;
    for tree in &trees {
        let (candidate, protected, confidence, basis) = reclaim_verdict(tree, &ev, scan.uid);
        if candidate {
            cand_truncated |= tree.metrics.truncated;
            // The detail line is what a human pastes into a terminal —
            // carry the context a bare pid lacks.
            candidates.push((
                tree.root_pid,
                format!(
                    "{}({}, {}, uid={}, {}h)",
                    tree.root_pid,
                    tree.root_family,
                    tree.scope.as_str(),
                    tree.root_uid
                        .map(|u| u.to_string())
                        .unwrap_or_else(|| "?".to_string()),
                    tree.age_secs.unwrap_or(0) / 3_600
                ),
            ));
            cand_pss += tree.metrics.pss_bytes;
            cand_swap += tree.metrics.swap_bytes;
        }
        let owner = tree.owner.map(|i| &ev.agents[i]);
        rows.push(json!({
            "root": {
                "pid": tree.root_pid,
                "start_jiffies": tree.root_start_jiffies,
                "uid": tree.root_uid,
                "ns_foreign": tree.root_ns_foreign,
                "family": tree.root_family,
                "cwd": tree.root_cwd,
                "cwd_deleted": tree.root_cwd_deleted,
                "age_secs": tree.age_secs,
                "cpu_secs": tree.root_cpu_secs,
            },
            "alias": owner.map(|a| a.alias.as_str()),
            "endpoint_kind": owner.map(|a| a.endpoint_kind.as_str()),
            "generation": owner.and_then(|a| a.generation.as_deref()),
            "endpoint_pid": owner.and_then(|a| a.endpoint_pid),
            "state": owner.map(|a| a.state.as_str()),
            "reason": owner.and_then(|a| a.reason.as_deref()),
            "pending": owner.map(|a| json!({"queued": a.queued, "running": a.running})),
            "last_progress": owner.and_then(|a| a.last_progress),
            "agreement": tree.agreement.as_str(),
            "agreement_why": tree.agreement_why,
            "scope": tree.scope.as_str(),
            "procs": tree.members.len(),
            "members": tree
                .members
                .iter()
                .map(|(pid, start)| json!({"pid": pid, "start_jiffies": start}))
                .collect::<Vec<_>>(),
            "pss_bytes": tree.metrics.pss_bytes,
            "swap_bytes": tree.metrics.swap_bytes,
            "pss_missing_pids": tree.metrics.pss_missing,
            "swap_missing_pids": tree.metrics.swap_missing,
            "metrics_truncated": tree.metrics.truncated,
            "reclaim": {
                "candidate": candidate,
                "pss_bytes": tree.metrics.pss_bytes,
                "swap_bytes": tree.metrics.swap_bytes,
                "confidence": confidence,
                "basis": basis,
                "protected": protected,
            },
        }));
    }
    let owned = trees
        .iter()
        .filter(|t| t.agreement == Agreement::Agreed)
        .count();
    let process_only = trees
        .iter()
        .filter(|t| t.agreement == Agreement::ProcessOnly)
        .count();
    let unowned_in_scope = trees
        .iter()
        .filter(|t| t.agreement == Agreement::ProcessOnly && matches!(t.scope, Scope::Project(_)))
        .count();
    let unowned_foreign = trees
        .iter()
        .filter(|t| t.agreement == Agreement::ProcessOnly && t.scope == Scope::Foreign)
        .count();
    let unowned_unproven = trees
        .iter()
        .filter(|t| t.agreement == Agreement::ProcessOnly && t.scope == Scope::Unproven)
        .count();
    let mismatched = trees
        .iter()
        .filter(|t| t.agreement == Agreement::GenerationMismatch)
        .count();
    let uncertain = trees
        .iter()
        .filter(|t| t.agreement == Agreement::Unknown)
        .count();
    let foreign_uid = trees
        .iter()
        .filter(|t| {
            t.agreement == Agreement::ProcessOnly && t.root_uid.is_some_and(|u| u != scan.uid)
        })
        .count();
    let registry_scope = match ev.store {
        RegStore::Open => format!("unscoped — all {} agents rows", ev.agents.len()),
        RegStore::Absent => "unscoped — store absent (zero rows)".to_string(),
        RegStore::Unreadable => "unscoped read FAILED — store unreadable".to_string(),
    };
    let mut detail = format!(
        "{} trees: {} owned, {} unowned ({} in-scope / {} foreign / {} unproven / \
         {} foreign-uid), {} generation-mismatch, {} uncertain; registry {} rows ({})",
        trees.len(),
        owned,
        process_only,
        unowned_in_scope,
        unowned_foreign,
        unowned_unproven,
        foreign_uid,
        mismatched,
        uncertain,
        ev.agents.len(),
        ev.store.as_str(),
    );
    if !candidates.is_empty() {
        // PSS shares can overlap across candidate trees, so the sum
        // is normally an upper bound — but a truncated tree's totals
        // are a lower bound over its first members, and one of those
        // in the list makes the aggregate neither: say so.
        if cand_truncated {
            detail.push_str(&format!(
                "; candidates (dry-run) would free ~{} RAM + ~{} swap \
                 (estimate — PSS overlap and a truncated tree)",
                human(cand_pss),
                human(cand_swap)
            ));
        } else {
            detail.push_str(&format!(
                "; candidates (dry-run) would free ≤{} RAM + ≤{} swap (upper bound)",
                human(cand_pss),
                human(cand_swap)
            ));
        }
        detail.push_str(&format!(
            "; candidates: {}",
            candidates
                .iter()
                .take(8)
                .map(|(_, d)| d.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        ));
        // Inlined into detail because render() only prints `remedy`
        // for non-ok checks and candidates stay ok — the caveat must
        // reach the operator next to the pid list it covers.
        detail.push_str(
            "; dry-run — nothing is stopped; any cleanup needs a separately \
             authorised phase with an ownership recheck at action time",
        );
    }
    if unreadable + vanished > 0 {
        detail.push_str(&format!(
            "; {} pids unreadable, {} vanished mid-scan",
            unreadable, vanished
        ));
    }
    let value = json!({
        "registry_scope": registry_scope,
        "registry_agents": ev.agents.len(),
        "store": ev.store.as_str(),
        "trees": rows,
        "records_only": records_only,
        "candidates": candidates
            .iter()
            .map(|(pid, _)| *pid)
            .collect::<Vec<_>>(),
        "totals": {
            "trees": trees.len(),
            "owned": owned,
            "process_only": process_only,
            "unowned_in_scope": unowned_in_scope,
            "unowned_foreign": unowned_foreign,
            "unowned_unproven": unowned_unproven,
            "foreign_uid": foreign_uid,
            "generation_mismatch": mismatched,
            "uncertain": uncertain,
            "candidates": candidates.len(),
            "candidate_pss_bytes": cand_pss,
            "candidate_swap_bytes": cand_swap,
            // False the moment any candidate's metrics were truncated
            // — a lower-bound tree in the sum means the aggregate is
            // an estimate, not an upper bound.
            "candidate_sums_upper_bound": !cand_truncated,
        },
        "procs_scanned": procs.len(),
        "pids_unreadable": unreadable,
        "pids_vanished": vanished,
    });
    let remedy = if candidates.is_empty() {
        String::new()
    } else {
        "dry-run census — nothing is stopped or reaped; per-tree rows are under \
         checks.sessions.trees in --json. Any cleanup needs a separately authorised \
         phase with an ownership recheck at action time."
            .to_string()
    };
    // Warn only on evidence failure: an unreadable store means the
    // census could not classify, and a watchdog must see that. A live
    // candidate list is a routine condition (a human ran an agent in a
    // repo) and stays ok — its caveat is inlined in `detail` above.
    let level = if ev.store == RegStore::Unreadable {
        Level::Warn
    } else {
        Level::Ok
    };
    check(name, level, value, threshold, detail, remedy)
}
