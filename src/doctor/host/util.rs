//! CAD-536: `cadence doctor host` — shared measurement
//! helpers (fs/git, proc census, WAL scan, secret scrub,
//! worktree machinery) moved verbatim from src/doctor/host.rs.

use super::*;

use crate::proc::run_bounded;
use crate::worktree::layout;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::os::unix::fs::MetadataExt;
use std::process::Command;

/// Allocated bytes under `path` (`st_blocks`, so sparse files report
/// what they really occupy and `du -sh` reconciles). Descends into
/// real directories only — `ent.metadata()` never follows symlinks,
/// so a lane's shared-cache links are not walked again here. Counts
/// each inode once per call (cargo's hardlinked uplifts can't double
/// up) and stays on the starting path's device, `du -x`-style, so a
/// row's bytes are what `rm -rf` frees *on that filesystem*. Skips
/// anything that vanishes mid-walk — a watchdog walk races with the
/// processes it watches. A directory that exists but cannot be listed
/// marks the walk truncated: the byte count is a lower bound, not a
/// complete measurement. Returns `(bytes, truncated)`.
pub(super) fn dir_size(path: &Path) -> (u64, bool) {
    let (bytes, truncated, _) = dir_size_limited(path, DIR_WALK_BUDGET);
    (bytes, truncated)
}

/// `dir_size` with a caller-chosen entry budget. The third value is
/// how many entries were stat'd. `ent.metadata()` does not follow
/// symlinks; the root `metadata` call does, so callers must not pass
/// a symlink they have refused to follow.
pub(super) fn dir_size_limited(path: &Path, budget: usize) -> (u64, bool, usize) {
    let mut total = 0u64;
    let mut visited = 0_usize;
    let mut truncated = false;
    let mut inodes = std::collections::HashSet::new();
    let root_dev = std::fs::metadata(path).ok().map(|m| m.dev());
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            // Absence is an empty measurement. Permission and I/O
            // failures make the result a lower bound (`truncated`), but
            // the walk goes on: stopping at the first unreadable dir
            // would drop readable siblings depending on readdir order
            // (CAD-262).
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if dir.as_path() == path {
                    return (0, false, 0);
                }
                continue;
            }
            Err(_) => {
                truncated = true;
                continue;
            }
        };
        for ent in entries {
            let ent = match ent {
                Ok(ent) => ent,
                Err(_) => {
                    truncated = true;
                    break;
                }
            };
            if visited >= budget {
                return (total, true, visited);
            }
            visited += 1;
            let Ok(meta) = ent.metadata() else {
                continue;
            };
            if meta.is_dir() {
                if root_dev.is_none_or(|d| meta.dev() == d) {
                    stack.push(ent.path());
                }
            } else if inodes.insert((meta.dev(), meta.ino())) {
                total += meta.blocks().saturating_mul(512);
            }
        }
    }
    (total, truncated, visited)
}

/// POSIX single-quoting for a path emitted inside a shell command —
/// `'a b'` and `'\''`-escaped, so `rm -rf <it>` can never split a
/// path like `/home/ubuntu/My Project` into extra arguments. Paths
/// made of only safe characters print bare for readability.
pub(super) fn shell_quote(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "@%_+=:,./-".contains(c))
    {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// The mount point a path lives on (longest-prefix match in
/// `/proc/self/mounts`), so a reclaim row says which filesystem its
/// bytes actually free. `None` off-Linux — the field is omitted.
fn fs_label(path: &Path) -> Option<String> {
    let canon = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let mounts = std::fs::read_to_string("/proc/self/mounts").ok()?;
    let mut best: Option<String> = None;
    for line in mounts.lines() {
        let Some(mp) = line.split_whitespace().nth(1) else {
            continue;
        };
        let mp = mp.replace("\\040", " ");
        if canon.starts_with(&mp) && best.as_ref().is_none_or(|b| mp.len() > b.len()) {
            best = Some(mp);
        }
    }
    best
}

/// Is `path` under an exclusive flock right now? A non-blocking
/// LOCK_EX attempt — success means free, and the `File` drop releases
/// the probe lock immediately. Cargo's build-lock files answer "is a
/// lane building" the way cargo itself does.
pub(super) fn file_locked(path: &Path) -> bool {
    crate::worktree::file_locked(path)
}

/// Bounded read-only `git`; non-zero exits are data, not errors — a
/// merge-base verdict IS the exit code.
fn git_out(dir: &Path, args: &[&str]) -> Option<std::process::Output> {
    let mut cmd = Command::new("git");
    // --no-optional-locks: `git status` otherwise refreshes .git/index
    // under index.lock — a write, and contention with a worker's `git
    // add`. The watchdog never takes it.
    cmd.arg("--no-optional-locks").arg("-C").arg(dir).args(args);
    run_bounded(&mut cmd, GIT_TIMEOUT).ok()
}

fn git_stdout(dir: &Path, args: &[&str]) -> Option<String> {
    let out = git_out(dir, args)?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// The main checkout root for `cwd`: `rev-parse --git-common-dir`
/// answers the shared `.git` even from inside a linked worktree.
pub(super) fn repo_root(cwd: &Path) -> Option<PathBuf> {
    let common = git_stdout(cwd, &["rev-parse", "--git-common-dir"])?;
    let common = PathBuf::from(&common);
    let common = if common.is_absolute() {
        common
    } else {
        cwd.join(common)
    };
    common.parent().map(|p| p.to_path_buf())
}

pub(super) fn read_u64_file(path: &Path) -> Option<u64> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

// ---------- disk ----------

pub(crate) struct FsFree {
    pub(super) path: PathBuf,
    pub(super) dev: u64,
    pub(super) free: u64,
    pub(super) total: u64,
}

impl FsFree {
    pub(super) fn pct(&self) -> f64 {
        if self.total == 0 {
            100.0
        } else {
            self.free as f64 * 100.0 / self.total as f64
        }
    }
}

/// The fields `stat` yields for free: comm, parentage, CPU, the
/// start-time half of pid+start identity (field 22) and RSS.
pub(super) struct ProcStat {
    pub(super) comm: String,
    pub(super) ppid: u32,
    pub(super) cpu_jiffies: u64,
    pub(super) start_jiffies: u64,
    pub(super) rss_bytes: u64,
}

pub(super) fn proc_stat(pid_dir: &Path) -> Option<ProcStat> {
    let text = std::fs::read_to_string(pid_dir.join("stat")).ok()?;
    let (head, rest) = text.rsplit_once(')')?;
    let comm = head.split_once('(')?.1.trim().to_string();
    let f: Vec<&str> = rest.split_whitespace().collect();
    // rest[0] is field 3 (state); ppid is field 4 → rest[1].
    let ppid: u32 = f.get(1)?.parse().ok()?;
    let utime: u64 = f.get(11)?.parse().ok()?;
    let stime: u64 = f.get(12)?.parse().ok()?;
    let start_jiffies: u64 = f.get(19)?.parse().ok()?;
    let rss_pages: i64 = f.get(21)?.parse().ok()?;
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) }.max(1) as u64;
    let rss = if rss_pages > 0 {
        (rss_pages as u64).saturating_mul(page)
    } else {
        0
    };
    Some(ProcStat {
        comm,
        ppid,
        cpu_jiffies: utime.saturating_add(stime),
        start_jiffies,
        rss_bytes: rss,
    })
}

/// Coalesce comm variants into the family an operator thinks in —
/// `chrome`, `chrome_crashpad` and `chrome-sandbox` are one group.
pub(super) fn comm_family(comm: &str) -> String {
    let c = comm.trim_end_matches("(deleted)").trim().to_lowercase();
    for family in [
        "chrome",
        "chromium",
        "firefox",
        "node",
        "deno",
        "cargo",
        "rustc",
        "rust-analyzer",
        "claude",
        "devin",
        "codex",
        "cursor",
        "cadence",
        "python",
        "tmux",
        "postgres",
        "redis",
    ] {
        // Exact match or a `-`/`_` boundary — `nodemon` is not `node`,
        // `chrome_crashpad` and `chrome-sandbox` are chrome.
        if c == *family
            || c.strip_prefix(family)
                .is_some_and(|rest| rest.starts_with('-') || rest.starts_with('_'))
        {
            return family.to_string();
        }
    }
    c
}

/// The oldest process seen in a group, for the census line.
#[derive(Clone)]
pub(super) struct OldestProc {
    pub(super) pid: u32,
    pub(super) age_secs: u64,
    pub(super) cpu_secs: u64,
    pub(super) idle: bool,
}

#[derive(Default)]
pub(super) struct GroupAgg {
    pub(super) count: u64,
    pub(super) rss_bytes: u64,
    pub(super) uids: BTreeSet<u32>,
    /// Oldest process overall.
    pub(super) oldest: Option<OldestProc>,
    /// Oldest *idle* process — long-lived at near-zero CPU is the
    /// leaked-session shape CAD-154 watches for.
    pub(super) oldest_idle: Option<OldestProc>,
}

/// One pass over `proc_root` grouping every readable pid by comm
/// family — all users, not just ours: the leaked sessions that
/// starved this host were root's.
#[derive(Default)]
pub(crate) struct Census {
    pub(super) groups: BTreeMap<String, GroupAgg>,
    pub(super) procs: u64,
    pub(super) unreadable: u64,
    pub(super) vanished: u64,
}

/// The shared census — one `/proc` walk per `run`, reused by the
/// `processes` check and any `memory` remedy in the same report.
pub(super) fn census_of(scan: &Scan) -> &Census {
    scan.census.get_or_init(|| proc_census(scan))
}

pub(super) fn proc_census(scan: &Scan) -> Census {
    let mut census = Census::default();
    let uptime = proc_uptime(&scan.proc_root);
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) }.max(1) as u64;
    let Ok(pids) = std::fs::read_dir(&scan.proc_root) else {
        return census;
    };
    for ent in pids.flatten() {
        let Some(pid) = ent.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        let Ok(meta) = ent.metadata() else {
            census.vanished += 1;
            continue;
        };
        let Some(stat) = proc_stat(&ent.path()) else {
            // A pid that vanished mid-scan is normal; an unreadable
            // stat is hidepid or a race — counted either way.
            if ent.path().exists() {
                census.unreadable += 1;
            } else {
                census.vanished += 1;
            }
            continue;
        };
        let age = uptime.map(|u| (u as u64).saturating_sub(stat.start_jiffies / hz));
        let cpu_secs = stat.cpu_jiffies / hz;
        // "Idle": alive over an hour at under ~1% duty — the leaked
        // browser sessions of CAD-154 burned nothing for days.
        let idle =
            age.is_some_and(|a| a >= 3_600) && cpu_secs.saturating_mul(100) <= age.unwrap_or(0);
        let group = census.groups.entry(comm_family(&stat.comm)).or_default();
        group.count += 1;
        group.rss_bytes += stat.rss_bytes;
        group.uids.insert(meta.uid());
        census.procs += 1;
        if let Some(age_secs) = age {
            let proc = OldestProc {
                pid,
                age_secs,
                cpu_secs,
                idle,
            };
            if group.oldest.as_ref().is_none_or(|o| age_secs > o.age_secs) {
                group.oldest = Some(proc.clone());
            }
            if idle
                && group
                    .oldest_idle
                    .as_ref()
                    .is_none_or(|o| age_secs > o.age_secs)
            {
                group.oldest_idle = Some(proc);
            }
        }
    }
    census
}

/// Top `n` groups by resident bytes — the remedy names these.
pub(super) fn top_groups(census: &Census, n: usize) -> Vec<(&String, &GroupAgg)> {
    let mut groups: Vec<(&String, &GroupAgg)> = census.groups.iter().collect();
    groups.sort_by_key(|(_, g)| std::cmp::Reverse(g.rss_bytes));
    groups.truncate(n);
    groups
}

/// `"chrome ×28 12.4 GiB (oldest 50h idle)"` — one group's census line.
pub(super) fn group_line(name: &str, g: &GroupAgg) -> String {
    let mut out = format!("{name} ×{} {}", g.count, human(g.rss_bytes));
    // The idle oldest tells the leak story when there is one.
    let oldest = g.oldest_idle.as_ref().or(g.oldest.as_ref());
    if let Some(o) = oldest {
        out.push_str(&format!(
            " (oldest {}h, pid {}{}{})",
            o.age_secs / 3600,
            o.pid,
            if o.idle { ", idle" } else { "" },
            if g.uids.len() == 1 && g.uids.contains(&0) {
                ", as root"
            } else {
                ""
            }
        ));
    }
    out
}

// ---------- provider WAL roots (shared with the daemon watcher) ----------

/// One provider's sqlite watch root: `*-wal` files anywhere under it
/// are checkpoint candidates. The daemon watches these; the provider
/// name is what the "no live turn" gate checks.
pub(crate) struct WalRoot {
    pub provider: &'static str,
    pub label: &'static str,
    pub root: PathBuf,
}

/// The roots the daemon's WAL watcher scans — the same provider
/// stores `provider-state` reports on (devin `sessions.db`, the codex
/// dir's `*.sqlite`, claude projects' nested dbs).
pub(crate) fn wal_roots(home: &Path, data_home: &Path) -> Vec<WalRoot> {
    vec![
        WalRoot {
            provider: "devin",
            label: "devin sessions.db",
            root: data_home.join("devin/cli"),
        },
        WalRoot {
            provider: "codex",
            label: "codex sessions",
            root: home.join(".codex"),
        },
        WalRoot {
            provider: "claude",
            label: "claude projects",
            root: home.join(".claude/projects"),
        },
    ]
}

/// What `find_wals` walked to. `truncated` is the honest signal that
/// the caps stopped the walk before every `*-wal` was reached — a
/// `~/.claude/projects` on a busy host is exactly the store that can
/// exceed them.
#[derive(Default)]
pub(crate) struct WalScan {
    pub dbs: Vec<PathBuf>,
    pub truncated: bool,
}

/// `*.db`/`*.sqlite`/`*.sqlite3` WAL siblings under `root`, returning
/// the DB paths (a `*-wal` file's presence means the store is in WAL
/// mode already). Depth-capped; the *matched-db* count is capped so a
/// dirent-heavy root can't starve the match set, and truncation is
/// reported rather than silent.
pub(crate) fn find_wals(root: &Path) -> WalScan {
    const MAX_DEPTH: u8 = 4;
    const MAX_DBS: usize = 1_024;
    // Dirent safety bound, far above any real store: a runaway walk
    // must not stall the daemon's tick, but hitting this reports
    // truncation instead of quietly missing the fat WAL.
    const MAX_VISITED: usize = 65_536;
    let mut scan = WalScan::default();
    let mut stack = vec![(root.to_path_buf(), 0_u8)];
    let mut visited = 0_usize;
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for ent in entries.flatten() {
            if scan.dbs.len() >= MAX_DBS || visited >= MAX_VISITED {
                scan.truncated = true;
                return scan;
            }
            visited += 1;
            // DirEntry::metadata does not follow links — a symlinked
            // dir inside a root is skipped rather than descended.
            let Ok(meta) = ent.metadata() else {
                continue;
            };
            if meta.is_dir() {
                if depth < MAX_DEPTH {
                    stack.push((ent.path(), depth + 1));
                }
                continue;
            }
            let name = ent.file_name().to_string_lossy().to_string();
            let Some(stem) = name.strip_suffix("-wal") else {
                continue;
            };
            if stem.ends_with(".db") || stem.ends_with(".sqlite") || stem.ends_with(".sqlite3") {
                scan.dbs.push(dir.join(stem));
            }
        }
    }
    scan
}

/// `/proc/uptime`'s first field, seconds since boot.
pub(super) fn proc_uptime(proc_root: &Path) -> Option<f64> {
    std::fs::read_to_string(proc_root.join("uptime"))
        .ok()?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

/// Age from `/proc/<pid>/stat` field 22 (starttime, jiffies since boot).
pub(super) fn pid_age_secs(pid_dir: &Path, uptime: Option<f64>) -> Option<u64> {
    let text = std::fs::read_to_string(pid_dir.join("stat")).ok()?;
    // comm may hold spaces/parens — fields after the last ')' are safe.
    let after_comm = text.rsplit_once(')')?.1;
    let start_jiffies: u64 = after_comm.split_whitespace().nth(19)?.parse().ok()?;
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) }.max(1) as u64;
    let started = start_jiffies / hz;
    Some((uptime? as u64).saturating_sub(started))
}

/// `17h`, `3d`, `42m`, `9s` — the largest whole unit.
fn age_label(secs: u64) -> String {
    match secs {
        s if s >= 86_400 => format!("{}d", s / 86_400),
        s if s >= 3_600 => format!("{}h", s / 3_600),
        s if s >= 60 => format!("{}m", s / 60),
        s => format!("{s}s"),
    }
}

/// One `kill <pid>  # <comm>  cwd=<cwd>  age=<age>` line per pid, read
/// from `<proc_root>/<pid>` at report time — an operator sees what each
/// pid is before signalling it (CAD-257). `comm` is the kernel's short
/// executable name, never argv. A pid that vanished is omitted; an
/// unreadable cwd or age prints `?`.
pub(super) fn kill_lines(proc_root: &Path, pids: &[u32]) -> Vec<String> {
    let uptime = proc_uptime(proc_root);
    pids.iter()
        .filter_map(|&pid| {
            let dir = proc_root.join(pid.to_string());
            if !dir.is_dir() {
                return None;
            }
            let comm = std::fs::read_to_string(dir.join("comm"))
                .ok()
                .map(|c| c.trim().to_string())
                .filter(|c| !c.is_empty())
                .or_else(|| {
                    let stat = std::fs::read_to_string(dir.join("stat")).ok()?;
                    let (_, rest) = stat.split_once('(')?;
                    Some(rest.rsplit_once(')')?.0.to_string())
                })
                .unwrap_or_else(|| "?".to_string());
            let cwd = std::fs::read_link(dir.join("cwd"))
                .map(|p| p.display().to_string())
                .unwrap_or_else(|_| "?".to_string());
            let age = pid_age_secs(&dir, uptime)
                .map(age_label)
                .unwrap_or_else(|| "?".to_string());
            Some(format!("kill {pid}  # {comm}  cwd={cwd}  age={age}"))
        })
        .collect()
}

/// A kill remedy: the reason line, then one named `kill` line per pid
/// still present. Empty when every pid has exited.
pub(super) fn kill_remedy(proc_root: &Path, pids: &[u32], why: &str) -> String {
    let lines = kill_lines(proc_root, pids);
    if lines.is_empty() {
        return String::new();
    }
    format!("{why}:\n{}", lines.join("\n"))
}

/// The only text a secret value is ever replaced by.
pub(super) const REDACTED: &str = "[REDACTED]";

/// Words that mark an argument name as credential-bearing —
/// `--figma-api-key`, `GITHUB_TOKEN`, `PGPASSWORD`, `DATABASE_URL`,
/// `SENTRY_DSN`, `SLACK_WEBHOOK`. A keyword only counts at a word
/// boundary (`-`, `_` or the start of the name): `monkey`, `keyboard`
/// and npm's `--access` stay ordinary while `api-key`,
/// `aws_secret_access_key` and `access_token` still trip it.
const SECRET_WORDS: &[&str] = &[
    "key", "token", "secret", "pass", "auth", "bearer", "cred", "url", "dsn", "webhook", "conn",
];

fn secret_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    // Inside a SHOUTED name words concatenate without separators
    // (`PGPASSWORD`, `AWSACCESSKEYID`) — anywhere counts.
    let shouted = !n.is_empty() && name.chars().all(|c| !c.is_ascii_lowercase());
    SECRET_WORDS.iter().any(|kw| {
        n.match_indices(kw)
            .any(|(i, _)| shouted || i == 0 || matches!(n.as_bytes()[i - 1], b'-' | b'_'))
    })
}

/// `NAME=value` env-name shape — `FOO_1` yes, `x?y` no.
fn env_name(name: &str) -> bool {
    let mut c = name.chars();
    matches!(c.next(), Some(f) if f.is_ascii_alphabetic() || f == '_')
        && c.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Argv text that is not a credential on its face — git SHAs (7, 8,
/// 40 or 64 hex chars) and canonical UUIDs are the ordinary long
/// tokens in process lists and must survive.
fn exempt_plain(s: &str) -> bool {
    let hex = |p: &str| !p.is_empty() && p.chars().all(|c| c.is_ascii_hexdigit());
    if matches!(s.len(), 7 | 8 | 40 | 64) && hex(s) {
        return true;
    }
    let segs: Vec<&str> = s.split('-').collect();
    segs.len() == 5
        && segs
            .iter()
            .zip([8_usize, 4, 4, 4, 12])
            .all(|(p, n)| p.len() == n && hex(p))
}

/// Token prefixes that name their issuer — evidence on their own, no
/// entropy or keyword context needed: Figma `figd_`; GitHub `ghp_`,
/// `gho_`, `ghu_`, `ghs_`, `ghr_`, `github_pat_`; GitLab `glpat-`;
/// Anthropic and OpenAI `sk-…`; Slack `xox…` and `xapp-`; AWS key-id
/// families `AKIA`, `ASIA`, `ABIA`, `ACCA`, `A3T`; Alibaba `LTAI`; npm
/// `npm_`; Devin `dvn_`; Google `AIza`. One list for every credential
/// gate — this
/// scrubber's [`credential_shape`], `session`'s `looks_secret` and the
/// `crate::secret` scan's entropy bypass — so the copies cannot drift
/// apart (CAD-440).
pub(crate) const SECRET_PREFIXES: &[&str] = &[
    "figd_",
    "ghp_",
    "gho_",
    "ghu_",
    "ghs_",
    "ghr_",
    "github_pat_",
    "glpat-",
    "sk-",
    "xoxa-",
    "xoxb-",
    "xoxe-",
    "xoxe.",
    "xoxo-",
    "xoxp-",
    "xoxr-",
    "xoxs-",
    "xapp-",
    "A3T",
    "ABIA",
    "ACCA",
    "AKIA",
    "ASIA",
    "LTAI",
    "npm_",
    "dvn_",
    "AIza",
];

/// `token` opens with a known provider prefix — [`SECRET_PREFIXES`].
pub(crate) fn has_secret_prefix(token: &str) -> bool {
    SECRET_PREFIXES.iter().any(|p| token.starts_with(p))
}

/// Credential shapes — a known provider prefix ([`has_secret_prefix`]),
/// JWTs, and 32+ char high-entropy tokens. `slash_ok` widens
/// the entropy charset to base64 (`/`, `+`) for a value under a flag
/// or env name — AWS secret access keys carry both and can be
/// digit-free — while a standalone arg with `/` stays classed as a
/// path and keeps the strict charset.
fn credential_shape(s: &str, slash_ok: bool) -> bool {
    if exempt_plain(s) {
        return false;
    }
    if has_secret_prefix(s) {
        return true;
    }
    // JWT: `eyJ…`.`…`.`…` — three non-empty base64url segments.
    if s.starts_with("eyJ") {
        let segs: Vec<&str> = s.split('.').collect();
        if segs.len() == 3
            && segs.iter().all(|seg| {
                !seg.is_empty()
                    && seg
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
            })
        {
            return true;
        }
    }
    // 32+ char high-entropy token. Strict charset for standalone args
    // (`/` means path); a flag/env value may use the full base64 set,
    // where a `/` or `+` alone satisfies the mix requirement — but an
    // absolute path is never a token.
    s.len() >= 32
        && !s.starts_with('-')
        && !(slash_ok && s.starts_with('/'))
        && s.chars().all(|c| {
            c.is_ascii_alphanumeric()
                || matches!(c, '_' | '-' | '.' | '~' | '+' | '=')
                || (slash_ok && c == '/')
        })
        && s.chars().any(|c| c.is_ascii_alphabetic())
        && (s.chars().any(|c| c.is_ascii_digit())
            || (slash_ok && (s.contains('/') || s.contains('+'))))
}

fn looks_like_credential(arg: &str) -> bool {
    credential_shape(arg, false)
}

/// The value half of `name=value` or `--flag=value`: the standalone
/// shapes plus the base64 set.
fn secret_value(s: &str) -> bool {
    credential_shape(s, true)
}

/// `user:pass` userinfo shape — `-u admin:hunter2` yes, `-u root`,
/// `8080:80` port maps and `127.0.0.1:8080` no.
fn user_pass_shape(s: &str) -> bool {
    let Some((u, p)) = s.split_once(':') else {
        return false;
    };
    !u.is_empty()
        && !p.is_empty()
        && u.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '%'))
        && !(portish(u) && portish(p))
}

/// Pure digits/dots/colons — ports and port maps (`2222`, `8080:80`,
/// `127.0.0.1:8080:80`) that `-p` carries for ssh and docker, not a
/// password.
fn portish(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '.' | ':'))
}

/// `scheme://user:<secret>@host` inside any text — the password half
/// of URI userinfo becomes `[REDACTED]`; a credential-shaped
/// username-only userinfo is redacted whole.
fn scrub_uri(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find("://") {
        out.push_str(&rest[..i + 3]);
        let after = &rest[i + 3..];
        let end = after.find(['/', '?', '#']).unwrap_or(after.len());
        let netloc = &after[..end];
        match netloc.find('@') {
            Some(at) => {
                let ui = &netloc[..at];
                match ui.find(':') {
                    Some(c) => {
                        out.push_str(&ui[..c + 1]);
                        out.push_str(REDACTED);
                    }
                    None if looks_like_credential(ui) => out.push_str(REDACTED),
                    _ => out.push_str(ui),
                }
                out.push('@');
                out.push_str(&netloc[at + 1..]);
            }
            None => out.push_str(netloc),
        }
        rest = &after[end..];
    }
    out.push_str(rest);
    out
}

/// `Key: value` scrubbing for an argument or `name=` value that
/// survived the name tests: a `:` whose left side names a secret
/// (`Authorization: Basic …`) or whose right side holds a credential
/// shape (`X-Custom: figd_…`) redacts the remainder of the argument,
/// spaces included; then URI userinfo loses its password.
fn scrub_tail(s: &str) -> String {
    let colon_scrubbed = match s.split_once(':') {
        Some((left, right))
            if secret_name(left)
                || right.split_whitespace().any(|seg| {
                    secret_value(seg.trim_matches(|c: char| matches!(c, '"' | '\'')))
                }) =>
        {
            format!("{left}: [REDACTED]")
        }
        _ => s.to_string(),
    };
    scrub_uri(&colon_scrubbed)
}

/// True when `arg` is itself a secret-bearing flag — the next-arg
/// consumer must not eat another flag's name as its value
/// (`--token --password x` still redacts `x`).
fn is_secret_flag(arg: &str) -> bool {
    if !arg.starts_with('-') || arg.len() < 2 {
        return false;
    }
    let body = arg.trim_start_matches('-');
    let name = body.split('=').next().unwrap_or(body);
    secret_name(name) || (!arg.starts_with("--") && matches!(name, "p" | "a" | "u"))
}

/// Display-text boundary. ASCII whitespace and Unicode spaces (NBSP,
/// em space, and the rest of `char::is_whitespace`) both count. A
/// shell does not split on NBSP; a flag and value divided by one are
/// still ambiguous credential-bearing diagnostic text, so the value
/// must not stay visible.
fn diagnostic_ws(c: char) -> bool {
    c.is_whitespace()
}

fn has_diagnostic_ws(s: &str) -> bool {
    s.chars().any(diagnostic_ws)
}

/// One argv element is a shell command when it carries whitespace and
/// is not already a single header or a secret-named assignment. Those
/// two stay whole: splitting `Authorization: Basic …` or
/// `--password=two words` would put the tail back on the page.
fn shell_blob_arg(arg: &str) -> bool {
    has_diagnostic_ws(arg) && !header_unit(arg) && !sealed_assignment(arg)
}

/// `Name: value` as one element — the colon rule already redacts the
/// rest, so a nested split must not reopen it. A colon whose left side
/// itself contains whitespace is a command (`psql postgres://…`), not
/// a header.
fn header_unit(arg: &str) -> bool {
    let colon_first = match (arg.find(':'), arg.find('=')) {
        (Some(c), Some(e)) => c < e,
        (Some(_), None) => true,
        _ => false,
    };
    if !colon_first {
        return false;
    }
    let (left, right) = arg.split_once(':').unwrap();
    if has_diagnostic_ws(left) {
        return false;
    }
    secret_name(left)
        || right
            .split_whitespace()
            .any(|seg| secret_value(seg.trim_matches(|c: char| matches!(c, '"' | '\''))))
}

/// `NAME=value` / `--flag=value` whose name owns the whole element.
/// A secret name redacts every character after `=`, spaces included.
/// A plain name with no space in the value is one token; a space means
/// later shell words (`EDITOR=vim cmd --token …`) and must be split.
fn sealed_assignment(arg: &str) -> bool {
    let Some((name, value)) = arg.split_once('=') else {
        return false;
    };
    if name.is_empty() || has_diagnostic_ws(name) {
        return false;
    }
    let flagged = name.starts_with('-');
    let bare = name.trim_start_matches('-');
    if bare.is_empty() || !(flagged || env_name(name)) {
        return false;
    }
    if secret_name(bare) {
        return true;
    }
    !has_diagnostic_ws(value)
}

/// `sh -c` is the outer element (depth 0). Each quoted command inside
/// it opens one more layer, up to this depth. Past it, secret-bearing
/// text is withheld instead of parsed further.
const SHELL_BLOB_DEPTH: u8 = 2;

struct ShellWord<'a> {
    start: usize,
    end: usize,
    raw: &'a str,
    logical: String,
}

/// Bounded word split for one command element. Single and double
/// quotes group a word (so a value can contain spaces); a quote
/// mid-word (`don't`) stays literal. Unicode spaces are character
/// boundaries in the diagnostic text, not shell metacharacters.
/// `$`, backticks and backslashes are expansions or escapes — the
/// split fails closed instead of guessing. Nothing here is executed.
fn split_shell_words(s: &str) -> std::result::Result<Vec<ShellWord<'_>>, ()> {
    let b = s.as_bytes();
    let mut words = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let Some(ch) = s[i..].chars().next() else {
            break;
        };
        if diagnostic_ws(ch) {
            i += ch.len_utf8();
            continue;
        }
        let start = i;
        while i < b.len() {
            let Some(ch) = s[i..].chars().next() else {
                break;
            };
            if diagnostic_ws(ch) {
                break;
            }
            match b[i] {
                b'\\' | b'$' | b'`' => return Err(()),
                b'\'' | b'"' if i == start || b[i - 1] == b'=' => {
                    let q = b[i];
                    i += 1;
                    let mut closed = false;
                    while i < b.len() {
                        if q == b'"' && matches!(b[i], b'\\' | b'$' | b'`') {
                            return Err(());
                        }
                        if b[i] == q {
                            i += 1;
                            closed = true;
                            break;
                        }
                        i += s[i..].chars().next().map_or(1, char::len_utf8);
                    }
                    if !closed {
                        return Err(());
                    }
                }
                _ => i += s[i..].chars().next().map_or(1, char::len_utf8),
            }
        }
        let raw = &s[start..i];
        words.push(ShellWord {
            start,
            end: i,
            logical: logical_shell_word(raw),
            raw,
        });
    }
    Ok(words)
}

fn logical_shell_word(raw: &str) -> String {
    strip_shell_value_quotes(unwrap_shell_quotes(raw))
}

fn unwrap_shell_quotes(raw: &str) -> &str {
    let b = raw.as_bytes();
    if b.len() >= 2 && matches!(b[0], b'\'' | b'"') && b[b.len() - 1] == b[0] {
        return &raw[1..b.len() - 1];
    }
    raw
}

/// `--token="value"` and `NAME='value'` classify as the unquoted
/// forms the flag and env rules already know. The quotes are dropped
/// even when more characters follow them (`TOKEN="x"extra`) so a
/// secret name still owns the value.
fn strip_shell_value_quotes(s: &str) -> String {
    let b = s.as_bytes();
    let Some(eq) = b.iter().position(|c| *c == b'=') else {
        return s.to_string();
    };
    if eq + 1 >= b.len() {
        return s.to_string();
    }
    let q = b[eq + 1];
    if q != b'\'' && q != b'"' {
        return s.to_string();
    }
    let rest = &s[eq + 2..];
    let Some(rel) = rest.find(q as char) else {
        return s.to_string();
    };
    let mut out = String::with_capacity(s.len() - 2);
    out.push_str(&s[..=eq]);
    out.push_str(&rest[..rel]);
    out.push_str(&rest[rel + 1..]);
    out
}

/// Tokenizer gave up. Withhold the element when a flag, assignment,
/// header or credential shape is still visible; benign text (an
/// unmatched quote, `echo $HOME`) stays so a failed split is not a
/// blanket mask.
fn blob_may_hold_secret(s: &str) -> bool {
    if s.contains("://") && s.contains('@') {
        return true;
    }
    for raw in s.split_whitespace() {
        let tok = raw.trim_matches(|c: char| matches!(c, '"' | '\'' | ',' | ';' | ')' | '('));
        if tok.is_empty() {
            continue;
        }
        if looks_like_credential(tok) {
            return true;
        }
        if let Some(body) = tok.strip_prefix('-') {
            let name = body.split(['=', ':']).next().unwrap_or(body);
            if secret_name(name) {
                return true;
            }
            if !tok.starts_with("--") && matches!(name.chars().next(), Some('p' | 'a' | 'u')) {
                return true;
            }
        }
        if let Some((name, value)) = tok.split_once('=') {
            let name = name.trim_matches(|c: char| matches!(c, '"' | '\''));
            let value = value.trim_matches(|c: char| matches!(c, '"' | '\''));
            if secret_name(name.trim_start_matches('-'))
                || secret_value(value)
                || looks_like_credential(value)
            {
                return true;
            }
        }
        if let Some((name, value)) = tok.split_once(':') {
            let name = name.trim_matches(|c: char| matches!(c, '"' | '\''));
            if secret_name(name) {
                return true;
            }
            let value = value.trim_matches(|c: char| matches!(c, '"' | '\''));
            if secret_value(value) || looks_like_credential(value) {
                return true;
            }
        }
    }
    false
}

fn withhold_or_keep(s: &str) -> String {
    if blob_may_hold_secret(s) {
        REDACTED.to_string()
    } else {
        s.to_string()
    }
}

/// Apply the flat argv rules inside one command element and splice
/// the results back, keeping the original whitespace and quoting
/// wherever a word did not change.
fn redact_shell_blob(s: &str, depth: u8) -> String {
    if depth > SHELL_BLOB_DEPTH {
        return withhold_or_keep(s);
    }
    let words = match split_shell_words(s) {
        Ok(words) => words,
        Err(()) => return withhold_or_keep(s),
    };
    if words.is_empty() {
        return s.to_string();
    }
    let logicals: Vec<String> = words
        .iter()
        .map(|w| {
            if has_diagnostic_ws(&w.logical)
                && !header_unit(&w.logical)
                && !sealed_assignment(&w.logical)
            {
                redact_shell_blob(&w.logical, depth + 1)
            } else {
                w.logical.clone()
            }
        })
        .collect();
    let redacted = redact_argv_parts(&logicals, false, true);
    let keep = redacted.len().min(words.len());
    let mut out = String::with_capacity(s.len());
    let mut pos = 0;
    for i in 0..keep {
        out.push_str(&s[pos..words[i].start]);
        if redacted[i] == words[i].logical {
            out.push_str(words[i].raw);
        } else {
            out.push_str(&redacted[i]);
        }
        pos = words[i].end;
    }
    if keep == words.len() {
        out.push_str(&s[pos..]);
    }
    out
}

/// `blobs`: a top-level element that is a command string is split.
/// `header_tail`: inside that split, a bare `Name:` consumes the
/// following words so the header value cannot survive beside it.
pub(super) fn redact_argv_parts<S: AsRef<str>>(
    argv: &[S],
    blobs: bool,
    header_tail: bool,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(argv.len());
    let mut i = 0;
    while i < argv.len() {
        let arg = argv[i].as_ref();
        if blobs && shell_blob_arg(arg) {
            out.push(redact_shell_blob(arg, 0));
            i += 1;
            continue;
        }
        if arg.starts_with('-') && arg.len() > 1 {
            // A flag: `--name=value`, `--name value`, `-x<value>` or a
            // bare switch.
            let body = arg.trim_start_matches('-');
            match body.split_once('=') {
                Some((name, value)) => {
                    if secret_name(name) || secret_value(value) {
                        out.push(format!(
                            "{}={REDACTED}",
                            &arg[..arg.len() - value.len() - 1]
                        ));
                    } else {
                        out.push(format!(
                            "{}={}",
                            &arg[..arg.len() - value.len() - 1],
                            scrub_tail(value)
                        ));
                    }
                }
                None => {
                    if secret_name(body) {
                        out.push(arg.to_string());
                        // A secret flag eats the next argument as its
                        // value even when it starts with `-`
                        // (`--password -p123`) — unless it is itself
                        // a secret flag (`--token --password x`).
                        if argv.get(i + 1).is_some_and(|v| !is_secret_flag(v.as_ref())) {
                            out.push(REDACTED.to_string());
                            i += 1;
                        }
                    } else if !arg.starts_with("--") {
                        let c = body.chars().next().unwrap();
                        if body.len() > c.len_utf8() {
                            // `-p<pass>` (mysql), `-u<user:pass>`
                            // (curl), `-x<token>` when the tail is a
                            // credential shape.
                            let tail = &body[c.len_utf8()..];
                            let secret_tail = match c {
                                'p' => !portish(tail),
                                'u' => user_pass_shape(tail),
                                _ => looks_like_credential(tail),
                            };
                            if secret_tail {
                                out.push(format!("-{c}{REDACTED}"));
                            } else {
                                out.push(arg.to_string());
                            }
                        } else {
                            // Bare single-letter flag: `-p`/`-a`
                            // take a value, `-u` takes `user:pass`.
                            out.push(arg.to_string());
                            let take_next = match body {
                                "p" | "a" => argv.get(i + 1).is_some_and(|v| {
                                    !v.as_ref().starts_with('-') && !portish(v.as_ref())
                                }),
                                "u" => argv.get(i + 1).is_some_and(|v| user_pass_shape(v.as_ref())),
                                _ => false,
                            };
                            if take_next {
                                out.push(REDACTED.to_string());
                                i += 1;
                            }
                        }
                    } else {
                        out.push(arg.to_string());
                    }
                }
            }
        } else {
            // A `:` whose left side names a secret redacts the rest of
            // the argument, spaces included — `Authorization: Basic …`
            // arrives as one argv element. Only when the `:` precedes
            // any `=` — a `NAME=value` arg keeps its env form.
            let colon_first = match (arg.find(':'), arg.find('=')) {
                (Some(c), Some(e)) => c < e,
                (Some(_), None) => true,
                _ => false,
            };
            if colon_first {
                let (left, right) = arg.split_once(':').unwrap();
                if secret_name(left)
                    || right.split_whitespace().any(|seg| {
                        secret_value(seg.trim_matches(|c: char| matches!(c, '"' | '\'')))
                    })
                {
                    out.push(format!("{left}: [REDACTED]"));
                    // A bare `Name:` inside a command string is only
                    // the header; the value is the words after it.
                    // Eating them here is what keeps
                    // `Authorization: Basic <secret>` from printing
                    // once the colon no longer shares their element.
                    if header_tail && right.trim().is_empty() {
                        return out;
                    }
                    i += 1;
                    continue;
                }
            }
            if let Some((name, value)) = arg.split_once('=') {
                // `NAME=value` env-style, or any `x=y` whose value is
                // a credential shape; the header/URI rules apply
                // inside the value too.
                if (env_name(name) && secret_name(name)) || secret_value(value) {
                    out.push(format!("{name}={REDACTED}"));
                } else {
                    out.push(format!("{name}={}", scrub_tail(value)));
                }
            } else if looks_like_credential(arg) {
                out.push(REDACTED.to_string());
            } else {
                out.push(scrub_tail(arg));
            }
        }
        i += 1;
    }
    out
}

// ---------- stale worktrees ----------

/// `git worktree list --porcelain` → worktree path to branch name.
fn worktree_branches(root: &Path) -> BTreeMap<PathBuf, String> {
    let mut map = BTreeMap::new();
    let Some(text) = git_stdout(root, &["worktree", "list", "--porcelain"]) else {
        return map;
    };
    let mut current: Option<PathBuf> = None;
    for line in text.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            current = Some(PathBuf::from(path));
        } else if let Some(branch) = line.strip_prefix("branch refs/heads/") {
            if let Some(path) = current.take() {
                map.insert(path, branch.to_string());
            }
        }
    }
    map
}

/// `origin/HEAD` when set, else a local main/master that verifies.
fn default_base(root: &Path) -> Option<String> {
    if let Some(head) = git_stdout(
        root,
        &["symbolic-ref", "refs/remotes/origin/HEAD", "--short"],
    ) {
        return Some(head);
    }
    for cand in ["main", "master"] {
        if git_out(root, &["rev-parse", "--verify", "--quiet", cand])
            .is_some_and(|o| o.status.success())
        {
            return Some(cand.to_string());
        }
    }
    None
}

/// `cad-72-host-watchdog` → `CAD-72`; names without a `<prefix>-<num>`
/// head are not issues.
fn issue_id_from_name(name: &str) -> Option<String> {
    let mut parts = name.split('-');
    let prefix = parts.next()?;
    let num = parts.next()?;
    let ok = !prefix.is_empty()
        && prefix.chars().all(|c| c.is_ascii_lowercase())
        && !num.is_empty()
        && num.chars().all(|c| c.is_ascii_digit());
    ok.then(|| format!("{}-{}", prefix.to_uppercase(), num))
}

/// `<pm>/<project>/<ID>/issue.md` — projects are the top-level dirs.
fn find_issue(pm_dir: &Path, id: &str) -> Option<PathBuf> {
    for ent in std::fs::read_dir(pm_dir).ok()?.flatten() {
        let file = ent.path().join(id).join("issue.md");
        if file.is_file() {
            return Some(file);
        }
    }
    None
}

/// Is this worktree's issue closed? `Some(true)` provably closed
/// (status done/dropped, or the worktree ref marked `closed: true`),
/// `Some(false)` still open, `None` no tracker truth available.
fn tracker_closed(scan: &Scan, wt_path: &Path, id: Option<&str>) -> Option<bool> {
    let pm = scan.pm_dir.as_deref()?;
    let file = find_issue(pm, id?)?;
    let text = std::fs::read_to_string(&file).ok()?;
    let (front, _body) = crate::issue::parse::parse_issue(&text).ok()?;
    if matches!(front.status.as_str(), "done" | "dropped") {
        return Some(true);
    }
    let wt = wt_path.to_string_lossy();
    Some(front.refs.iter().any(|r| {
        r.kind == "worktree" && r.closed == Some(true) && r.path.as_deref() == Some(wt.as_ref())
    }))
}

/// The worktree staleness scan shared by the `worktrees` check and
/// `--reclaim-plan`: `(<stale rows>, <remedy per row>, <dirs scanned>)`.
pub(super) fn stale_worktrees(
    scan: &Scan,
    root: &Path,
    wt_root: &Path,
) -> (Vec<Value>, Vec<String>, usize) {
    let branches = worktree_branches(root);
    let base = default_base(root);
    let mut stale: Vec<Value> = Vec::new();
    let mut remedies: Vec<String> = Vec::new();
    let mut scanned = 0_usize;
    if let Ok(entries) = std::fs::read_dir(wt_root) {
        for ent in entries.flatten() {
            let Ok(meta) = ent.metadata() else {
                continue;
            };
            if !meta.is_dir() {
                continue;
            }
            scanned += 1;
            let path = ent.path();
            let wt_name = ent.file_name().to_string_lossy().to_string();
            let branch = branches.get(&path).cloned();
            // A merged branch is only stale when the tree is clean —
            // a branch at its base commit is a trivial ancestor, so
            // "merged" alone would flag worktrees whose uncommitted
            // work is still in flight (including the one this runs in).
            let merged = branch
                .as_deref()
                .zip(base.as_deref())
                .is_some_and(|(b, base)| {
                    git_out(root, &["merge-base", "--is-ancestor", b, base])
                        .is_some_and(|o| o.status.success())
                });
            let id = issue_id_from_name(&wt_name);
            let closed = tracker_closed(scan, &path, id.as_deref());
            if !merged && closed != Some(true) {
                continue;
            }
            let clean = git_out(&path, &["status", "--porcelain"])
                .is_some_and(|o| o.status.success() && o.stdout.is_empty());
            let is_stale = closed == Some(true) || (merged && clean);
            if !is_stale {
                continue;
            }
            let (bytes, truncated) = dir_size(&path);
            let mut why = Vec::new();
            if merged {
                why.push(format!("merged into {}", base.as_deref().unwrap_or("?")));
            }
            if closed == Some(true) {
                why.push("tracker ref closed".to_string());
            }
            if !clean {
                why.push("dirty tree".to_string());
            }
            if branch.is_none() {
                why.push("no branch recorded".to_string());
            }
            stale.push(json!({
                "path": path,
                "branch": branch,
                "issue": id,
                "merged": merged,
                "tracker_closed": closed,
                "clean": clean,
                "bytes": bytes,
                "bytes_truncated": truncated,
                "why": why.join(", "),
            }));
            remedies.push(match &id {
                Some(id) => format!("cadence issue finish {id}"),
                None => format!(
                    "git -C {} worktree remove {}",
                    shell_quote(&root.display().to_string()),
                    shell_quote(&path.display().to_string())
                ),
            });
        }
    }
    (stale, remedies, scanned)
}

// ---------- reclaim plan ----------

/// What `--reclaim-plan` lists: live lanes' `target/` dirs
/// (informational — they free only when the lane does), the shared
/// cache's reclaimable subdirs (cleared contents-only, and only when
/// no build holds one of cargo's lock files), retired shared dirs an
/// older cadence planted, and stale worktrees. The stale scan runs
/// first: a stale lane's *whole* dir — `target/` included — is freed
/// by that row's own `issue finish`/`worktree remove` command, so its
/// bytes count toward `reclaimable_bytes` and it gets no separate
/// informational row. Live-lane `target/` rows report separately as
/// `freed_with_lanes_bytes` — the headline number is what the plan's
/// own commands free today. A locked shared cache emits no freeing
/// command, so its bytes stay out of the total too. Listing only:
/// nothing here deletes or signals anything, and every emitted
/// command is shell-quoted so a path with a space can never split
/// into extra `rm -rf` arguments.
pub fn reclaim_plan(scan: &Scan) -> Value {
    use crate::worktree::{RETIRED_DEBUG_DIRS, SHARED_DEBUG_DIRS, SHARED_DEBUG_FILES};
    let mut rows: Vec<Value> = Vec::new();
    let Some(root) = repo_root(&scan.cwd) else {
        return json!({"rows": rows, "reclaimable_bytes": 0, "freed_with_lanes_bytes": 0,
                      "skipped": format!("{} is not inside a git repo", scan.cwd.display())});
    };
    let wt_root = layout::worktrees_dir(&root);
    // Stale scan first — a stale lane's whole dir is freed by its own
    // row's command, so it must not also emit an informational
    // worktree-target row.
    let mut stale_rows: Vec<Value> = Vec::new();
    let mut stale_paths: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    if wt_root.is_dir() {
        let (stale, _remedies, _scanned) = stale_worktrees(scan, &root, &wt_root);
        for s in stale {
            let path = PathBuf::from(s["path"].as_str().unwrap_or_default());
            stale_paths.insert(path.clone());
            stale_rows.push(json!({
                "kind": "stale-worktree",
                "path": s["path"],
                "bytes": s["bytes"],
                "bytes_truncated": s["bytes_truncated"],
                "filesystem": fs_label(&path),
                "action": match s["issue"].as_str() {
                    Some(id) => format!("cadence issue finish {id}"),
                    None => format!(
                        "git -C {} worktree remove {}",
                        shell_quote(&root.display().to_string()),
                        shell_quote(s["path"].as_str().unwrap_or("?"))
                    ),
                },
                "why": s["why"],
            }));
        }
    }
    // Live lanes' target/ rows — informational, freed with the lane.
    if let Ok(entries) = std::fs::read_dir(&wt_root) {
        for ent in entries.flatten() {
            if !ent.metadata().is_ok_and(|m| m.is_dir()) || stale_paths.contains(&ent.path()) {
                continue;
            }
            let target = ent.path().join("target");
            if target.is_dir() {
                let (bytes, truncated) = dir_size(&target);
                let name = ent.file_name().to_string_lossy().to_string();
                rows.push(json!({
                    "kind": "worktree-target",
                    "path": target,
                    "bytes": bytes,
                    "bytes_truncated": truncated,
                    "filesystem": fs_label(&target),
                    "action": match issue_id_from_name(&name) {
                        // "Freed with the lane" — a live lane's target
                        // is not a recommendation to finish its work.
                        Some(id) => format!("freed with the lane — cadence issue finish {id} removes it"),
                        None => format!(
                            "freed with the lane — git -C {} worktree remove {}",
                            shell_quote(&root.display().to_string()),
                            shell_quote(&ent.path().display().to_string())
                        ),
                    },
                }));
            }
        }
    }
    let shared = crate::worktree::shared_target_dir(&root);
    if shared.is_dir() {
        let d = shared.join("debug");
        // `bytes` counts exactly what the emitted command frees: the
        // contents of the shared subdirs — nothing else in the tree.
        let mut bytes = 0_u64;
        let mut truncated = false;
        for name in SHARED_DEBUG_DIRS {
            let (b, t) = dir_size(&d.join(name));
            bytes += b;
            truncated |= t;
        }
        // Any of cargo's lock files held means a build is live.
        let locked = SHARED_DEBUG_FILES
            .iter()
            .any(|name| file_locked(&d.join(name)));
        rows.push(json!({
            "kind": "shared-cargo-cache",
            "path": shared,
            "bytes": bytes,
            "bytes_truncated": truncated,
            "filesystem": fs_label(&shared),
            "cargo_locked": locked,
            "action": if locked {
                // A snapshot flag — the lock may already be free by
                // the time anyone reads this; the emitted command is
                // withheld rather than risking a live build.
                "a cargo build held the shared lock at scan time — \
                 rerun the plan when lanes are idle".to_string()
            } else {
                // Clear the *contents* of the hashed subdirs, never
                // the dirs themselves: every lane symlinks to those
                // dirs, and deleting them would leave the links
                // dangling — cargo dies with EEXIST on the next
                // build. `*` plus `.[!.]*` covers dotfiles too; on an
                // empty dir both go unmatched and `-f` swallows the
                // literal argument.
                let globs = SHARED_DEBUG_DIRS
                    .iter()
                    .map(|name| {
                        let q = shell_quote(&d.join(name).display().to_string());
                        format!("{q}/* {q}/.[!.]*")
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                format!(
                    "rm -rf {globs}  # clears cached dep artifacts — \
                     the dirs stay, so lanes keep building and rebuild lazily"
                )
            },
        }));
        // Dirs an older cadence shared but none does now — the
        // contents clear the same way; the dir itself stays for any
        // lane whose r2-era link still points at it.
        for name in RETIRED_DEBUG_DIRS {
            let dir = d.join(name);
            if dir.is_dir() && !dir.is_symlink() {
                let (b, t) = dir_size(&dir);
                let q = shell_quote(&dir.display().to_string());
                rows.push(json!({
                    "kind": "retired-shared-dir",
                    "path": dir,
                    "bytes": b,
                    "bytes_truncated": t,
                    "filesystem": fs_label(&dir),
                    "action": format!(
                        "rm -rf {q}/* {q}/.[!.]*  # retired shared dir no current lane links"
                    ),
                }));
            }
        }
    }
    rows.extend(stale_rows);
    // `reclaimable_bytes` = what the emitted commands free today:
    // every row except live-lane targets (freed with their lane) and
    // a lock-blocked shared cache (no command emitted).
    let reclaimable: u64 = rows
        .iter()
        .filter(|r| r["kind"] != "worktree-target")
        .filter(|r| !(r["kind"] == "shared-cargo-cache" && r["cargo_locked"] == json!(true)))
        .map(|r| r["bytes"].as_u64().unwrap_or(0))
        .sum();
    let with_lanes: u64 = rows
        .iter()
        .filter(|r| r["kind"] == "worktree-target")
        .map(|r| r["bytes"].as_u64().unwrap_or(0))
        .sum();
    json!({
        "rows": rows,
        "reclaimable_bytes": reclaimable,
        "freed_with_lanes_bytes": with_lanes,
    })
}

/// Text form of the plan: one line per reclaimable row, then the total.
pub fn render_reclaim(plan: &Value) -> String {
    let mut out = String::from("cadence doctor --host --reclaim-plan — nothing here is deleted\n");
    if let Some(skipped) = plan["skipped"].as_str() {
        out.push_str(&format!("skipped: {skipped}\n"));
        return out;
    }
    if let Some(rows) = plan["rows"].as_array() {
        for r in rows {
            let size = if r["bytes_truncated"].as_bool().unwrap_or(false) {
                format!("≥{}", human(r["bytes"].as_u64().unwrap_or(0)))
            } else {
                human(r["bytes"].as_u64().unwrap_or(0))
            };
            let fs = r["filesystem"]
                .as_str()
                .map(|f| format!(" on {f}"))
                .unwrap_or_default();
            out.push_str(&format!(
                "{:<20} {:>10}  {}{}\n       {}\n",
                r["kind"].as_str().unwrap_or("?"),
                size,
                r["path"].as_str().unwrap_or("?"),
                fs,
                r["action"].as_str().unwrap_or("")
            ));
        }
    }
    out.push_str(&format!(
        "total reclaimable: {}\n",
        human(plan["reclaimable_bytes"].as_u64().unwrap_or(0))
    ));
    let with_lanes = plan["freed_with_lanes_bytes"].as_u64().unwrap_or(0);
    if with_lanes > 0 {
        out.push_str(&format!(
            "target/ freed with their lanes: {} (not counted above)\n",
            human(with_lanes)
        ));
    }
    out
}
