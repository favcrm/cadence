// CAD-536: host-doctor tests — moved verbatim from src/doctor/host.rs.

use super::*;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use tempfile::TempDir;

/// A fully fabricated host: empty proc/tmp/home/repo/pm under one
/// temp root. Tests poke in exactly the state a check reads.
fn fake_scan(root: &TempDir) -> Scan {
    let scan = Scan {
        proc_root: root.path().join("proc"),
        temp_dir: root.path().join("tmp"),
        cargo_target_dir: None,
        home: root.path().join("home"),
        state_dir: root.path().join("state"),
        cwd: root.path().join("repo"),
        devin_data: root.path().join("devin"),
        claude_projects: root.path().join("claude-projects"),
        codex_sessions: root.path().join("codex-sessions"),
        pm_dir: Some(root.path().join("pm")),
        uid: unsafe { libc::geteuid() },
        now: SystemTime::now(),
        thresholds: Thresholds::default(),
        linux: true,
        slots: None,
        tailscaled_socket: Some(root.path().join("tailscaled.sock")),
        fs_probe: Some(|path| {
            Some(FsFree {
                path: path.to_path_buf(),
                dev: 1,
                free: 500 * GIB,
                total: 1_000 * GIB,
            })
        }),
        census: std::cell::OnceCell::new(),
    };
    for d in [
        &scan.proc_root,
        &scan.temp_dir,
        &scan.home,
        &scan.state_dir,
        &scan.cwd,
        scan.pm_dir.as_ref().unwrap(),
    ] {
        std::fs::create_dir_all(d).unwrap();
    }
    scan
}

/// proc/<pid>/ with the files the checks read: stat (age), cmdline,
/// cwd/exe symlinks, fd dir with the given link targets.
fn add_pid(
    proc: &Path,
    pid: u32,
    cwd: Option<&Path>,
    exe: Option<&Path>,
    cmdline: Option<&str>,
    age_secs: u64,
    fds: &[&str],
) -> PathBuf {
    // Space-joined form for callers whose args are space-free;
    // `add_pid_argv` takes the real argv vector.
    add_pid_argv(
        proc,
        pid,
        cwd,
        exe,
        cmdline.map(|c| c.split(' ').collect::<Vec<_>>()).as_deref(),
        age_secs,
        fds,
    )
}

/// `add_pid` with a real argv vector — an element may itself
/// contain spaces (a `-H "Name: value"` header).
fn add_pid_argv(
    proc: &Path,
    pid: u32,
    cwd: Option<&Path>,
    exe: Option<&Path>,
    argv: Option<&[&str]>,
    age_secs: u64,
    fds: &[&str],
) -> PathBuf {
    let dir = proc.join(pid.to_string());
    std::fs::create_dir_all(dir.join("fd")).unwrap();
    // uptime file says 1_000_000s; starttime = uptime - age in
    // jiffies (field 22 → token 19 after the comm paren).
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as u64;
    let starttime = (1_000_000_u64.saturating_sub(age_secs)) * hz;
    std::fs::write(
        dir.join("stat"),
        format!("{pid} (t) S {}{starttime} 0 0", "1 ".repeat(18)),
    )
    .unwrap();
    std::fs::write(proc.join("uptime"), "1000000.00 0.00\n").unwrap();
    if let Some(args) = argv {
        std::fs::write(dir.join("cmdline"), args.join("\0")).unwrap();
    }
    if let Some(cwd) = cwd {
        std::os::unix::fs::symlink(cwd, dir.join("cwd")).unwrap();
    }
    if let Some(exe) = exe {
        std::os::unix::fs::symlink(exe, dir.join("exe")).unwrap();
    }
    for (i, target) in fds.iter().enumerate() {
        std::os::unix::fs::symlink(target, dir.join("fd").join((i + 3).to_string())).unwrap();
    }
    dir
}

/// proc/<pid>/ shaped for the session census: caller controls
/// comm, ppid, cwd and the metric files (`status` VmSwap,
/// `smaps_rollup` Pss) — the census never reads argv.
fn add_session_proc(
    proc: &Path,
    pid: u32,
    ppid: u32,
    comm: &str,
    cwd: Option<&Path>,
    age_secs: u64,
    metrics: (Option<u64>, Option<u64>), // (smaps_rollup Pss kB, status VmSwap kB)
) -> PathBuf {
    let dir = proc.join(pid.to_string());
    std::fs::create_dir_all(&dir).unwrap();
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as u64;
    let starttime = (1_000_000_u64.saturating_sub(age_secs)) * hz;
    // Fields after `)`: state, ppid, 17 fillers to field 21,
    // then starttime (22) and two trailing fields.
    std::fs::write(
        dir.join("stat"),
        format!(
            "{pid} ({comm}) S {ppid} {} {starttime} 0 0",
            "1 ".repeat(17)
        ),
    )
    .unwrap();
    std::fs::write(proc.join("uptime"), "1000000.00 0.00\n").unwrap();
    if let Some(cwd) = cwd {
        std::os::unix::fs::symlink(cwd, dir.join("cwd")).unwrap();
    }
    if let Some(kb) = metrics.1 {
        // The real uid line — a test overwriting this file is how
        // a foreign-user process is faked.
        let euid = unsafe { libc::geteuid() };
        std::fs::write(
                dir.join("status"),
                format!(
                    "Name:\t{comm}\nPid:\t{pid}\nUid:\t{euid}\t{euid}\t{euid}\t{euid}\nVmSwap:\t{kb} kB\n"
                ),
            )
            .unwrap();
    }
    if let Some(kb) = metrics.0 {
        std::fs::write(
            dir.join("smaps_rollup"),
            format!("{pid}\nPss:               {kb} kB\nPss_Anon:          {kb} kB\n"),
        )
        .unwrap();
    }
    dir
}

/// A minimal `cadence.sqlite3` for the census's read-only open —
/// the two tables it queries, no migrations needed.
fn fake_registry(state_dir: &Path) -> rusqlite::Connection {
    std::fs::create_dir_all(state_dir).unwrap();
    let conn = rusqlite::Connection::open(state_dir.join("cadence.sqlite3")).unwrap();
    conn.execute_batch(
        "CREATE TABLE agents(
                alias TEXT PRIMARY KEY, provider TEXT NOT NULL,
                endpoint_kind TEXT NOT NULL, role TEXT NOT NULL,
                cwd TEXT NOT NULL, sandbox TEXT NOT NULL,
                instructions TEXT, thread_id TEXT, session_id TEXT,
                model TEXT, pid INTEGER, endpoint TEXT, params TEXT,
                generation TEXT, state TEXT NOT NULL,
                enabled INTEGER NOT NULL DEFAULT 1, error TEXT,
                created REAL NOT NULL, updated REAL NOT NULL);
             CREATE TABLE messages(
                seq INTEGER PRIMARY KEY AUTOINCREMENT,
                id TEXT UNIQUE NOT NULL, alias TEXT NOT NULL,
                body TEXT NOT NULL, reply_to TEXT, source TEXT NOT NULL,
                state TEXT NOT NULL DEFAULT 'queued',
                turn_id TEXT, result TEXT, error TEXT,
                created REAL NOT NULL, started REAL, completed REAL);",
    )
    .unwrap();
    conn
}

/// Wall-clock now — `agents.updated` is REAL seconds, and the
/// join fences claims whose live pid postdates the row's last
/// write, so fixtures need realistic values.
fn now_epoch() -> f64 {
    SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
}

#[allow(clippy::too_many_arguments)]
fn add_agent(
    conn: &rusqlite::Connection,
    alias: &str,
    kind: &str,
    pid: Option<u32>,
    generation: Option<&str>,
    state: &str,
    cwd: &Path,
    updated: f64,
) {
    conn.execute(
        "INSERT INTO agents(alias, provider, endpoint_kind, role, cwd, sandbox, \
             pid, generation, state, created, updated) \
             VALUES (?1, 'claude', ?2, 'dev', ?6, 'none', ?3, ?4, ?5, 1.0, ?7)",
        rusqlite::params![
            alias,
            kind,
            pid.map(|p| p as i64),
            generation,
            state,
            cwd.to_string_lossy().to_string(),
            updated
        ],
    )
    .unwrap();
}

fn add_message(
    conn: &rusqlite::Connection,
    alias: &str,
    state: &str,
    turn_id: Option<&str>,
    completed: Option<f64>,
) {
    conn.execute(
        "INSERT INTO messages(id, alias, body, source, state, turn_id, created, completed) \
             VALUES (lower(hex(randomblob(8))), ?1, 'b', 't', ?2, ?3, 1.0, ?4)",
        rusqlite::params![alias, state, turn_id, completed],
    )
    .unwrap();
}

/// `<pm>/<key>/project.yaml` — one registered repo path.
fn add_project(pm: &Path, key: &str, repo_path: &Path) {
    let dir = pm.join(key);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("project.yaml"),
        format!(
            "key: {key}\nprefix: {key}-\nrepos:\n  - path: {}\n",
            repo_path.display()
        ),
    )
    .unwrap();
}

/// The `sessions` check's value object out of a full `run`.
fn sessions_value(scan: &Scan) -> Value {
    let report = run(scan);
    report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "sessions")
        .unwrap()["value"]
        .clone()
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn init_repo(root: &Path) {
    git(root, &["init", "-q", "-b", "main", "."]);
    // A real repo ignores build output — `target/` must not read
    // as dirty.
    std::fs::write(root.join(".gitignore"), "/target\n").unwrap();
    git(root, &["add", "-A"]);
    git(
        root,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-qm",
            "init",
        ],
    );
}

fn write_issue(pm: &Path, project: &str, id: &str, status: &str, refs_yaml: &str) {
    let dir = pm.join(project).join(id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
            dir.join("issue.md"),
            format!(
                "---\nid: {id}\ntitle: t\nstatus: {status}\npriority: P2\n{refs_yaml}created: 2026-09-19T00:00:00Z\n---\n\nbody\n"
            ),
        )
        .unwrap();
}

fn sparse(path: &Path, bytes: u64) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::File::create(path).unwrap().set_len(bytes).unwrap();
}

/// `dir_size` measures allocated blocks (`du`-style) — a sparse
/// file reports ~0 — so tests that need real bytes write them.
fn real_bytes(path: &Path, bytes: usize) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, vec![7u8; bytes]).unwrap();
}

/// Puts `mode` back before the owning `TempDir` is removed. Declare
/// it after the `TempDir` so this drops first.
struct RestoreMode {
    path: PathBuf,
    mode: u32,
}

impl Drop for RestoreMode {
    fn drop(&mut self) {
        let _ = std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(self.mode));
    }
}

fn deny_directory(path: &Path) -> RestoreMode {
    let restore = RestoreMode {
        path: path.to_path_buf(),
        mode: 0o755,
    };
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o000)).unwrap();
    restore
}

fn set_mtime_old(path: &Path, secs_ago: i64) {
    let c = CString::new(path.as_os_str().as_bytes()).unwrap();
    let when = libc::timespec {
        tv_sec: (SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            - secs_ago) as libc::time_t,
        tv_nsec: 0,
    };
    let times = [when, when];
    unsafe { libc::utimensat(libc::AT_FDCWD, c.as_ptr(), times.as_ptr(), 0) };
}

// ---------- disk ----------

#[test]
fn disk_level_threshold_edges() {
    let t = Thresholds::default();
    let gib = |n: u64| n * GIB;
    // Plenty free.
    assert_eq!(fs_level(gib(50), gib(100), &t), Level::Ok);
    // 14% free < 15% warn.
    assert_eq!(fs_level(gib(14), gib(100), &t), Level::Warn);
    // Exactly 15% with enough bytes is ok.
    assert_eq!(fs_level(gib(30), gib(200), &t), Level::Ok);
    // Percent fine but bytes under 20 GiB warn.
    assert_eq!(fs_level(gib(19), gib(200), &t), Level::Warn);
    // Under 5% fails.
    assert_eq!(fs_level(gib(4), gib(100), &t), Level::Fail);
    // Exactly 5% and 5 GiB stays warn, not fail.
    assert_eq!(fs_level(gib(5), gib(100), &t), Level::Warn);
    // Percent fine but bytes under 5 GiB fails.
    assert_eq!(fs_level(gib(4), gib(400), &t), Level::Fail);
}

#[test]
fn disk_check_reports_each_filesystem() {
    let root = TempDir::new().unwrap();
    let scan = fake_scan(&root);
    let c = check_disk(&scan);
    // Same fs under the temp root dedups to one filesystem entry.
    let fses = c.value.as_array().unwrap();
    assert!(!fses.is_empty());
    assert_eq!(c.name, "disk");
    assert!(c.detail.contains("% free"));
}

// ---------- provider state ----------

#[test]
fn provider_state_threshold_edges() {
    let t = Thresholds::default();
    assert_eq!(store_level(0, Some(GIB + 1), &t), Level::Warn);
    assert_eq!(store_level(0, Some(10 * GIB + 1), &t), Level::Fail);
    assert_eq!(store_level(10 * GIB + 1, None, &t), Level::Warn);
    assert_eq!(store_level(9 * GIB, Some(MIB), &t), Level::Ok);
    assert_eq!(store_level(0, Some(GIB), &t), Level::Ok); // exactly at warn stays ok
}

#[test]
fn provider_state_missing_dirs_skip_not_fail() {
    let root = TempDir::new().unwrap();
    let scan = fake_scan(&root);
    let c = check_provider_state(&scan);
    assert_eq!(c.level, Level::Ok);
    assert!(c.detail.contains("no known provider stores"));
}

#[test]
fn provider_state_flags_fat_wal_and_store() {
    let root = TempDir::new().unwrap();
    let mut scan = fake_scan(&root);
    // devin sessions.db + a WAL past the fail line (sparse files).
    sparse(&scan.devin_data.join("cli/sessions.db"), 100);
    sparse(&scan.devin_data.join("cli/sessions.db-wal"), 11 * GIB);
    // a directory store over the warn line.
    scan.claude_projects = root.path().join("claude-fat");
    sparse(&scan.claude_projects.join("blob"), 11 * GIB);
    let c = check_provider_state(&scan);
    assert_eq!(c.level, Level::Fail);
    assert!(c.remedy.contains("wal_checkpoint(TRUNCATE)"));
    let stores = c.value.as_array().unwrap();
    assert_eq!(stores.len(), 2); // absent stores skipped
}

// ---------- memory + census ----------

/// Write a fabricated `/proc/meminfo` (values in kB, like the real
/// file) plus the overcommit sysctl.
fn write_meminfo(scan: &Scan, meminfo: &str, overcommit: Option<u64>) {
    std::fs::write(scan.proc_root.join("meminfo"), meminfo).unwrap();
    if let Some(mode) = overcommit {
        std::fs::create_dir_all(scan.proc_root.join("sys/vm")).unwrap();
        std::fs::write(
            scan.proc_root.join("sys/vm/overcommit_memory"),
            format!("{mode}\n"),
        )
        .unwrap();
    } else {
        let _ = std::fs::remove_file(scan.proc_root.join("sys/vm/overcommit_memory"));
    }
}

/// proc/<pid>/stat with a real comm, cpu jiffies and rss pages —
/// the census's whole input. Age comes from the shared 1e6s uptime.
fn add_proc(
    proc: &Path,
    pid: u32,
    comm: &str,
    age_secs: u64,
    cpu_secs: u64,
    rss_pages: u64,
) -> PathBuf {
    let dir = proc.join(pid.to_string());
    std::fs::create_dir_all(&dir).unwrap();
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as u64;
    let starttime = (1_000_000_u64.saturating_sub(age_secs)) * hz;
    // Post-comm fields, positions 3..: state S, fields 4-13 zero,
    // utime(14)/stime(15) carry the cpu, fields 16-21 filler,
    // starttime(22), vsize(23)=0, rss(24).
    std::fs::write(
            dir.join("stat"),
            format!(
                "{pid} ({comm}) S 0 0 0 0 0 0 0 0 0 0 {utime} {stime} 0 0 0 1 0 0 {starttime} 0 {rss_pages}",
                utime = cpu_secs * hz,
                stime = 0,
            ),
        )
        .unwrap();
    std::fs::write(proc.join("uptime"), "1000000.00 0.00\n").unwrap();
    dir
}

/// The measured CAD-154 incident: 3.6 GiB available of ~32 GiB,
/// 208 MiB of 20 GiB swap free, Committed_AS ~99.9 GiB against a
/// ~35.4 GiB CommitLimit with heuristic overcommit — fork() was
/// already returning EAGAIN while every check was green.
#[test]
fn memory_incident_numbers_fail() {
    let root = TempDir::new().unwrap();
    let scan = fake_scan(&root);
    write_meminfo(
        &scan,
        "MemTotal:       33554432 kB\n\
             MemAvailable:    3774873 kB\n\
             SwapTotal:      20971520 kB\n\
             SwapFree:         212992 kB\n\
             Committed_AS:  104752742 kB\n\
             CommitLimit:    37119590 kB\n",
        Some(0),
    );
    let c = check_memory(&scan);
    assert_eq!(c.level, Level::Fail, "{}", c.detail);
    assert!(
        c.detail.contains("overcommit_memory=heuristic"),
        "{}",
        c.detail
    );
    // The fail comes from exhausted swap *with* low available —
    // the commit overshoot is an advisory item under mode 0.
    assert!(c.value["committed_over_limit"].as_bool().unwrap());
    assert!(c.remedy.contains("cadence never kills"), "{}", c.remedy);
}

#[test]
fn memory_threshold_edges() {
    let root = TempDir::new().unwrap();
    let scan = fake_scan(&root);
    let kb = |gib: u64| gib * 1024 * 1024;
    // Exactly at warn (15%) with swap comfy → ok.
    write_meminfo(
        &scan,
        &format!(
            "MemTotal: {t} kB\nMemAvailable: {} kB\nSwapTotal: {s} kB\nSwapFree: {} kB\n",
            kb(15) + kb(15) / 100 + 1024,
            kb(10),
            t = kb(100),
            s = kb(20)
        ),
        Some(0),
    );
    let c = check_memory(&scan);
    assert_eq!(c.level, Level::Ok, "{}", c.detail);
    // 14% available → warn; no commit fields → commit leg silent.
    write_meminfo(
        &scan,
        &format!(
            "MemTotal: {} kB\nMemAvailable: {} kB\nSwapTotal: {} kB\nSwapFree: {} kB\n",
            kb(100),
            kb(14),
            kb(20),
            kb(10)
        ),
        Some(0),
    );
    assert_eq!(check_memory(&scan).level, Level::Warn);
    // 4% available → fail.
    write_meminfo(
        &scan,
        &format!(
            "MemTotal: {} kB\nMemAvailable: {} kB\nSwapTotal: {} kB\nSwapFree: {} kB\n",
            kb(100),
            kb(4),
            kb(20),
            kb(10)
        ),
        Some(0),
    );
    assert_eq!(check_memory(&scan).level, Level::Fail);
    // Swap free 15% (<20 warn) → warn. Swap at 4% with RAM to
    // spare is still only warn — swap exhaustion alone is the
    // steady state of a long-lived host, not a failure.
    write_meminfo(
        &scan,
        &format!(
            "MemTotal: {} kB\nMemAvailable: {} kB\nSwapTotal: {} kB\nSwapFree: {} kB\n",
            kb(100),
            kb(90),
            kb(20),
            kb(3)
        ),
        Some(0),
    );
    assert_eq!(check_memory(&scan).level, Level::Warn);
    write_meminfo(
        &scan,
        &format!(
            "MemTotal: {} kB\nMemAvailable: {} kB\nSwapTotal: {} kB\nSwapFree: {} kB\n",
            kb(100),
            kb(90),
            kb(20),
            kb(20) / 25
        ),
        Some(0),
    );
    assert_eq!(check_memory(&scan).level, Level::Warn);
    // Swap exhausted AND available low together — the incident
    // shape — is the fail.
    write_meminfo(
        &scan,
        &format!(
            "MemTotal: {} kB\nMemAvailable: {} kB\nSwapTotal: {} kB\nSwapFree: {} kB\n",
            kb(100),
            kb(10),
            kb(20),
            kb(20) / 25
        ),
        Some(0),
    );
    assert_eq!(check_memory(&scan).level, Level::Fail);
}

#[test]
fn memory_commit_leg_modes() {
    let root = TempDir::new().unwrap();
    let scan = fake_scan(&root);
    // Comfy memory, commitment over the limit — the mode decides.
    let base = "MemTotal: 104857600 kB\nMemAvailable: 83886080 kB\n\
                    SwapTotal: 20971520 kB\nSwapFree: 20971520 kB\n\
                    Committed_AS: 60000000 kB\nCommitLimit: 40000000 kB\n";
    // overcommit_memory=1 (always): over-limit is benign — no fail.
    write_meminfo(&scan, base, Some(1));
    let c = check_memory(&scan);
    assert_eq!(c.level, Level::Ok, "{}", c.detail);
    assert!(c.detail.contains("overcommit_memory=always"));
    // overcommit_memory=2 (strict): refusing allocations now.
    write_meminfo(&scan, base, Some(2));
    let c = check_memory(&scan);
    assert_eq!(c.level, Level::Fail, "{}", c.detail);
    assert!(c.detail.contains("strict overcommit"), "{}", c.detail);
    // overcommit_memory=0 (heuristic): CommitLimit is advisory —
    // over-limit is a normal steady state, reported not alarmed.
    write_meminfo(&scan, base, Some(0));
    let c = check_memory(&scan);
    assert_eq!(c.level, Level::Ok, "{}", c.detail);
    assert!(c.detail.contains("overcommit_memory=heuristic"));
    assert!(c.value["committed_over_limit"].as_bool().unwrap());
    // Sysctl unreadable: cannot prove enforcement — warn, not fail.
    write_meminfo(&scan, base, None);
    let c = check_memory(&scan);
    assert_eq!(c.level, Level::Warn, "{}", c.detail);
    assert!(c.detail.contains("overcommit_memory=unknown"));
    // Under the limit: quiet regardless of mode.
    write_meminfo(
        &scan,
        "MemTotal: 104857600 kB\nMemAvailable: 83886080 kB\n\
             SwapTotal: 20971520 kB\nSwapFree: 20971520 kB\n\
             Committed_AS: 10000000 kB\nCommitLimit: 40000000 kB\n",
        Some(2),
    );
    assert_eq!(check_memory(&scan).level, Level::Ok);
}

#[test]
fn memory_no_swap_and_missing_meminfo() {
    let root = TempDir::new().unwrap();
    let scan = fake_scan(&root);
    // No meminfo at all → skipped, not a false fail.
    let c = check_memory(&scan);
    assert_eq!(c.level, Level::Ok);
    assert!(c.value["skipped"].as_bool().unwrap());
    // A host with no swap: the leg reports "no swap", no fail.
    write_meminfo(
        &scan,
        "MemTotal: 104857600 kB\nMemAvailable: 83886080 kB\nSwapTotal: 0 kB\nSwapFree: 0 kB\n",
        Some(0),
    );
    let c = check_memory(&scan);
    assert_eq!(c.level, Level::Ok, "{}", c.detail);
    assert!(c.detail.contains("no swap"), "{}", c.detail);
    // Off-linux the whole check skips.
    let mut scan = fake_scan(&root);
    scan.linux = false;
    assert!(check_memory(&scan).value["skipped"].as_bool().unwrap());
}

#[test]
fn memory_remedy_names_biggest_groups() {
    let root = TempDir::new().unwrap();
    let scan = fake_scan(&root);
    write_meminfo(
        &scan,
        "MemTotal: 33554432 kB\nMemAvailable: 1572864 kB\n",
        Some(0),
    );
    // Leaked browser tree: 3 chrome pids, 50h old, ~0 cpu.
    for pid in [11, 12, 13] {
        add_proc(&scan.proc_root, pid, "chrome", 180_000, 0, 200_000);
    }
    add_proc(&scan.proc_root, 20, "node", 60, 30, 10_000);
    let c = check_memory(&scan);
    assert_eq!(c.level, Level::Fail);
    assert!(c.remedy.contains("chrome ×3"), "{}", c.remedy);
    assert!(c.remedy.contains("oldest 50h"), "{}", c.remedy);
    assert!(c.remedy.contains("idle"), "{}", c.remedy);
    // Remedies name offenders; cadence never kills.
    assert!(c.remedy.contains("cadence never kills"), "{}", c.remedy);
}

#[test]
fn census_groups_and_oldest_idle() {
    let root = TempDir::new().unwrap();
    let scan = fake_scan(&root);
    // Leaked tree: root-owned old chrome at ~zero cpu, plus a busy
    // node and a young claude.
    let euid = unsafe { libc::geteuid() };
    for (pid, comm) in [(11, "chrome"), (12, "chrome_crashpad"), (13, "chrome")] {
        add_proc(&scan.proc_root, pid, comm, 180_000, 0, 100_000);
    }
    add_proc(&scan.proc_root, 20, "node", 120, 90, 50_000);
    add_proc(&scan.proc_root, 30, "claude", 30, 5, 20_000);
    let census = proc_census(&scan);
    assert_eq!(census.procs, 5);
    let chrome = &census.groups["chrome"];
    assert_eq!(chrome.count, 3);
    assert!(chrome.uids.contains(&euid));
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as u64;
    assert_eq!(chrome.rss_bytes, 3 * 100_000 * page);
    let oldest = chrome.oldest_idle.as_ref().unwrap();
    assert_eq!(oldest.age_secs, 180_000);
    assert!(oldest.idle);
    // node burned 90 cpu-seconds in 120 — busy, not idle.
    assert!(census.groups["node"].oldest_idle.is_none());
    assert_eq!(census.groups["node"].oldest.as_ref().unwrap().age_secs, 120);
    let c = check_processes(&scan);
    assert_eq!(c.level, Level::Ok);
    assert!(c.detail.contains("chrome ×3"), "{}", c.detail);
    assert!(c.detail.contains("50h"), "{}", c.detail);
}

#[test]
fn census_family_normalization() {
    assert_eq!(comm_family("chrome_crashpad"), "chrome");
    assert_eq!(comm_family("Chrome"), "chrome");
    assert_eq!(comm_family("node"), "node");
    assert_eq!(comm_family("rust-analyzer"), "rust-analyzer");
    assert_eq!(comm_family("weird-daemon"), "weird-daemon");
}

#[test]
fn census_skipped_off_linux() {
    let root = TempDir::new().unwrap();
    let mut scan = fake_scan(&root);
    scan.linux = false;
    let c = check_processes(&scan);
    assert!(c.value["skipped"].as_bool().unwrap());
}

// ---------- WAL roots ----------

#[test]
fn find_wals_walks_provider_roots() {
    let root = TempDir::new().unwrap();
    let codex = root.path().join("codex");
    // devin's store, a codex *.sqlite, a nested claude db.
    let devin_cli = root.path().join("devin/cli");
    std::fs::create_dir_all(&devin_cli).unwrap();
    std::fs::write(devin_cli.join("sessions.db-wal"), "x").unwrap();
    real_bytes(&codex.join("state_1.sqlite-wal"), 8);
    real_bytes(&codex.join("queue_1.db-wal"), 8);
    real_bytes(&root.path().join("claude/proj/sub/sess.sqlite3-wal"), 8);
    // Not a sqlite store — ignored.
    real_bytes(&codex.join("notes.txt-wal"), 8);
    let found = find_wals(root.path());
    assert!(!found.truncated);
    let names: Vec<String> = found
        .dbs
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
        .collect();
    assert!(names.contains(&"sessions.db".to_string()), "{names:?}");
    assert!(names.contains(&"state_1.sqlite".to_string()), "{names:?}");
    assert!(names.contains(&"queue_1.db".to_string()), "{names:?}");
    assert!(names.contains(&"sess.sqlite3".to_string()), "{names:?}");
    assert_eq!(found.dbs.len(), 4, "{names:?}");
}

#[test]
fn wal_roots_cover_the_three_providers() {
    let roots = wal_roots(Path::new("/home/u"), Path::new("/data"));
    let providers: Vec<&str> = roots.iter().map(|r| r.provider).collect();
    assert_eq!(providers, ["devin", "codex", "claude"]);
    assert!(roots[0].root.ends_with("devin/cli"));
    assert!(roots[1].root.ends_with(".codex"));
    assert!(roots[2].root.ends_with(".claude/projects"));
}

/// A symlinked dir inside a provider root is not descended — the
/// walk must stay inside the root it was given.
#[test]
fn find_wals_skips_symlinked_dirs() {
    let root = TempDir::new().unwrap();
    let real = root.path().join("real");
    std::fs::create_dir_all(&real).unwrap();
    real_bytes(&real.join("a.db-wal"), 8);
    std::os::unix::fs::symlink(&real, root.path().join("link")).unwrap();
    let found = find_wals(root.path());
    assert_eq!(
        found.dbs.len(),
        1,
        "the same wal must not be found twice through the link"
    );
}

/// Hitting the matched-db cap reports truncation rather than
/// silently returning a partial watch list.
#[test]
fn find_wals_reports_truncation() {
    let root = TempDir::new().unwrap();
    for i in 0..1030 {
        real_bytes(&root.path().join(format!("s{i}.db-wal")), 4);
    }
    let found = find_wals(root.path());
    assert!(found.truncated, "1024-cap must surface");
    assert_eq!(found.dbs.len(), 1024);
}

// ---------- pipes ----------

#[test]
fn pipes_counts_fifos_and_flags_soft_limit() {
    let root = TempDir::new().unwrap();
    let mut scan = fake_scan(&root);
    let proc = scan.proc_root.clone();
    std::fs::create_dir_all(proc.join("sys/fs")).unwrap();
    std::fs::write(proc.join("sys/fs/pipe-user-pages-soft"), "32\n").unwrap();
    std::fs::write(proc.join("sys/fs/pipe-max-size"), "1048576\n").unwrap();
    std::fs::write(proc.join("uptime"), "1000.00 0.00\n").unwrap();
    // pid 10: 3 fds on 2 unique pipes + a socket that is not a pipe.
    add_pid(
        &proc,
        10,
        None,
        None,
        Some("holder"),
        60,
        &["pipe:[11]", "pipe:[11]", "pipe:[22]", "socket:[9]"],
    );
    // pid 11: one more pipe.
    add_pid(&proc, 11, None, None, Some("other"), 60, &["pipe:[33]"]);
    // pid 12: unreadable fd dir — counted, not fatal.
    let denied = add_pid(&proc, 12, None, None, Some("denied"), 60, &[]);
    std::fs::set_permissions(denied.join("fd"), std::fs::Permissions::from_mode(0o0)).unwrap();
    // pid 13: fd dir missing entirely — read_dir fails, tolerated.
    std::fs::create_dir_all(proc.join("13")).unwrap();
    // non-pid entries are ignored.
    std::fs::create_dir_all(proc.join("sys")).unwrap();
    let stats = scan_pipes(&scan);
    assert_eq!(stats.fds, 4);
    assert_eq!(stats.pipes, 3);
    assert_eq!(stats.denied, 1);
    assert_eq!(stats.top[0], (10, 3));
    // est_pages = 3 × 16 = 48 > soft 32 → warn + clamped.
    let c = check_pipes(&scan);
    assert_eq!(c.level, Level::Warn);
    assert!(c.value["clamped"].as_bool().unwrap());
    // Now raise the soft limit — the same pipes are fine.
    std::fs::write(proc.join("sys/fs/pipe-user-pages-soft"), "16384\n").unwrap();
    scan.linux = true;
    let c = check_pipes(&scan);
    assert_eq!(c.level, Level::Ok);
    assert!(!c.value["clamped"].as_bool().unwrap());
}

#[test]
fn pipes_skipped_off_linux() {
    let root = TempDir::new().unwrap();
    let mut scan = fake_scan(&root);
    scan.linux = false;
    let c = check_pipes(&scan);
    assert_eq!(c.level, Level::Ok);
    assert!(c.value["skipped"].as_bool().unwrap());
}

// ---------- orphans ----------

#[test]
fn orphans_flag_deleted_worktrees_and_old_test_binaries() {
    let root = TempDir::new().unwrap();
    let scan = fake_scan(&root);
    let proc = scan.proc_root.clone();
    let repo = scan.cwd.clone();
    // live worktree dir — must NOT flag.
    let alive = repo.join(".cadence/wt/alive");
    std::fs::create_dir_all(&alive).unwrap();
    // pid 20: cwd in a deleted worktree.
    add_pid(
        &proc,
        20,
        Some(&repo.join(".cadence/wt/gone")),
        None,
        Some("sleep 99"),
        60,
        &[],
    );
    // pid 21: cwd in a live worktree — not an orphan.
    add_pid(
        &proc,
        21,
        Some(&alive),
        None,
        Some("cargo test"),
        80_000,
        &[],
    );
    // pid 22: exe is a test binary in a deleted worktree, 3h old.
    add_pid(
        &proc,
        22,
        None,
        Some(&repo.join(".cadence/wt/gone2/target/debug/deps/integration-abc123")),
        Some("integration-abc123"),
        10_800,
        &[],
    );
    // pid 23: test binary outside any worktree, 2h old — flagged
    // on age alone.
    add_pid(
        &proc,
        23,
        None,
        Some(&repo.join("target/debug/deps/board-deadbeef")),
        Some("board-deadbeef"),
        7_200,
        &[],
    );
    // pid 24: same test-binary shape but only 10 min — too young.
    add_pid(
        &proc,
        24,
        None,
        Some(&repo.join("target/debug/deps/board-young")),
        Some("board-young"),
        600,
        &[],
    );
    // pid 25: ordinary long-lived process nowhere near a worktree.
    add_pid(
        &proc,
        25,
        None,
        Some(Path::new("/usr/bin/sleep")),
        Some("sleep 99"),
        90_000,
        &[],
    );
    let c = check_orphans(&scan);
    assert_eq!(c.level, Level::Warn);
    let pids: Vec<u64> = c.value["pids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["pid"].as_u64().unwrap())
        .collect();
    // Sorted by pid regardless of read_dir order.
    assert_eq!(pids, vec![20, 22, 23]);
    // One named kill line per pid (CAD-257): comm from stat,
    // cwd from the link, age from starttime.
    let kills: Vec<&str> = c
        .remedy
        .lines()
        .filter(|l| l.starts_with("kill "))
        .collect();
    assert_eq!(
        kills,
        vec![
            format!(
                "kill 20  # t  cwd={}  age=1m",
                repo.join(".cadence/wt/gone").display()
            )
            .as_str(),
            // No cwd link → `?`, never a guess.
            "kill 22  # t  cwd=?  age=3h",
            "kill 23  # t  cwd=?  age=2h",
        ],
        "{}",
        c.remedy
    );
    assert!(c.detail.contains("pid 20"));
}

#[test]
fn orphans_tolerate_denied_and_vanished_pids() {
    let root = TempDir::new().unwrap();
    let scan = fake_scan(&root);
    let proc = scan.proc_root.clone();
    // Denied: cwd+exe both unreadable inside an existing pid dir.
    let denied = add_pid(&proc, 30, None, None, Some("x"), 1, &[]);
    std::fs::set_permissions(&denied, std::fs::Permissions::from_mode(0o0)).unwrap();
    // Vanished: a pid dir that is gone when probed.
    let gone = proc.join("31");
    let probe = probe_pid(&gone, 31, Some(1_000.0), &scan);
    assert!(matches!(probe, Probe::Missing));
    let c = check_orphans(&scan);
    assert_eq!(c.value["unreadable"].as_u64().unwrap(), 1);
    std::fs::set_permissions(&denied, std::fs::Permissions::from_mode(0o755)).unwrap();
    // Nothing else flags → ok.
    assert_eq!(c.level, Level::Ok);
}

// ---------- argv redaction (CAD-108) ----------

#[test]
fn redact_argv_flag_and_env_forms() {
    let cases: Vec<(Vec<&str>, &str)> = vec![
            // `--flag=value`
            (
                vec![
                    "npm",
                    "exec",
                    "figma-developer-mcp",
                    "--figma-api-key=figd_TESTKEY0001",
                    "--stdio",
                ],
                "npm exec figma-developer-mcp --figma-api-key=[REDACTED] --stdio",
            ),
            // `--flag value`
            (
                vec!["tool", "--token", "s3cr3t", "--verbose"],
                "tool --token [REDACTED] --verbose",
            ),
            (
                vec!["aws", "sso", "--aws-secret-access-key", "wJalrXUtnFEMI"],
                "aws sso --aws-secret-access-key [REDACTED]",
            ),
            // a flag-shaped next arg is still the value — the flag
            // names a secret, so `--verbose` is eaten
            (vec!["t", "--token", "--verbose"], "t --token [REDACTED]"),
            // …but a secret flag is never eaten as a value
            (
                vec!["t", "--token", "--password", "hunter2"],
                "t --token --password [REDACTED]",
            ),
            // trailing flag with no value — nothing to redact
            (vec!["t", "--verbose", "--api-key"], "t --verbose --api-key"),
            // `NAME=value` env-style
            (
                vec!["env", "GITHUB_TOKEN=ghp_TEST", "cmd"],
                "env GITHUB_TOKEN=[REDACTED] cmd",
            ),
            (
                vec!["env", "PGPASSWORD=hunter2", "psql"],
                "env PGPASSWORD=[REDACTED] psql",
            ),
            // value shaped like a credential under a plain flag/env name
            (vec!["t", "--header=xoxb-TEST"], "t --header=[REDACTED]"),
            (vec!["t", "URL=sk-TEST"], "t URL=[REDACTED]"),
            // env names carrying secrets by convention
            (
                vec![
                    "env",
                    "DATABASE_URL=postgres://u:p@h/db",
                    "SENTRY_DSN=https://x@y",
                    "SLACK_WEBHOOK=https://z",
                    "DB_CONN=mysql://h",
                ],
                "env DATABASE_URL=[REDACTED] SENTRY_DSN=[REDACTED] SLACK_WEBHOOK=[REDACTED] DB_CONN=[REDACTED]",
            ),
            // ordinary arguments pass through untouched
            (
                vec!["cargo", "test", "--", "--port", "3010", "/tmp/x"],
                "cargo test -- --port 3010 /tmp/x",
            ),
            (
                vec!["t", "--config=/etc/app.conf", "verbose"],
                "t --config=/etc/app.conf verbose",
            ),
            (
                vec!["env", "EDITOR=vim", "URL=https://x/?q=1"],
                "env EDITOR=vim URL=[REDACTED]",
            ),
        ];
    for (argv, want) in cases {
        assert_eq!(redact_argv(&argv), want, "{argv:?}");
    }
}

/// The three leak shapes from the CAD-108 round-2 review, plus
/// the `--flag value` and `--flag=` regressions.
#[test]
fn redact_argv_header_uri_and_short_flags() {
    let cases: Vec<(Vec<&str>, &str)> = vec![
        // header-style single arguments — one argv element holds
        // `Name: value` with the space inside
        (
            vec![
                "curl",
                "-H",
                "Authorization: Basic YWxpY2U6c3VwZXJzZWNyZXQ=",
                "https://api.x",
            ],
            "curl -H Authorization: [REDACTED] https://api.x",
        ),
        (
            vec!["tool", "-H", "X-Api-Key: figd_LIVE"],
            "tool -H X-Api-Key: [REDACTED]",
        ),
        // an innocent header name still loses a credential value
        (
            vec!["tool", "-H", "X-Custom: sk-LIVE"],
            "tool -H X-Custom: [REDACTED]",
        ),
        // URIs with embedded credentials
        (
            vec!["psql", "postgres://admin:hunter2@db.example.com:5432/app"],
            "psql postgres://admin:[REDACTED]@db.example.com:5432/app",
        ),
        (
            vec![
                "git",
                "clone",
                "https://user:ghp_ABCDEFGHIJKLMNOPQRST@github.com/o/r.git",
            ],
            "git clone https://user:[REDACTED]@github.com/o/r.git",
        ),
        // a credential-shaped userinfo without a password
        (
            vec!["git", "clone", "https://ghp_LIVETOKEN@github.com/o/r.git"],
            "git clone https://[REDACTED]@github.com/o/r.git",
        ),
        // a URI secret inside a `NAME=value` / `--flag=` value
        (
            vec!["t", "CONFIG=postgres://u:hunter2@db/x"],
            "t CONFIG=postgres://u:[REDACTED]@db/x",
        ),
        // short attached flags
        (
            vec!["mysql", "-uroot", "-phunter2", "db"],
            "mysql -uroot -p[REDACTED] db",
        ),
        (
            vec!["redis-cli", "-a", "hunter2"],
            "redis-cli -a [REDACTED]",
        ),
        (
            vec!["curl", "-u", "admin:hunter2", "https://x"],
            "curl -u [REDACTED] https://x",
        ),
        (vec!["curl", "-uadmin:hunter2"], "curl -u[REDACTED]"),
        // a `-` value is still the value of a secret flag
        (vec!["t", "--password", "-p123"], "t --password [REDACTED]"),
        // regression: the incident shape stays redacted
        (
            vec!["t", "--figma-api-key=figd_LIVE"],
            "t --figma-api-key=[REDACTED]",
        ),
        (
            vec!["t", "--figma-api-key", "figd_LIVE"],
            "t --figma-api-key [REDACTED]",
        ),
    ];
    for (argv, want) in cases {
        assert_eq!(redact_argv(&argv), want, "{argv:?}");
    }
}

/// The over-redaction side of the review — ordinary argv that
/// must survive untouched.
#[test]
fn redact_argv_ordinary_args_survive() {
    let cases: Vec<(Vec<&str>, &str)> = vec![
        (
            vec!["npm", "publish", "--access", "public"],
            "npm publish --access public",
        ),
        (
            vec![
                "git",
                "checkout",
                "4f2a9c1d8e3b5a7c9f1e2d3b4a5c6d7e8f9a0b1c",
            ],
            "git checkout 4f2a9c1d8e3b5a7c9f1e2d3b4a5c6d7e8f9a0b1c",
        ),
        (
            vec!["git", "log", "--oneline", "-n", "deadbeef", "abc1234"],
            "git log --oneline -n deadbeef abc1234",
        ),
        // canonical UUIDs, bare and as a flag value
        (
            vec!["t", "550e8400-e29b-41d4-a716-446655440000"],
            "t 550e8400-e29b-41d4-a716-446655440000",
        ),
        (
            vec!["t", "--uuid=550e8400-e29b-41d4-a716-446655440000"],
            "t --uuid=550e8400-e29b-41d4-a716-446655440000",
        ),
        // keyword inside a word is not a keyword
        (
            vec!["t", "monkey=banana", "bypass=1"],
            "t monkey=banana bypass=1",
        ),
        // `-p` carrying ports for ssh/docker is not a password
        (vec!["ssh", "-p", "2222", "host"], "ssh -p 2222 host"),
        (
            vec!["docker", "run", "-p", "8080:80", "img"],
            "docker run -p 8080:80 img",
        ),
        // bundled short flags are not `-a <secret>`
        (vec!["ps", "-aux"], "ps -aux"),
        (vec!["ps", "-a", "-f"], "ps -a -f"),
        // `-u` with a plain username
        (vec!["mysql", "-u", "root", "db"], "mysql -u root db"),
        // long paths as flag values or standalone args
        (
            vec![
                "t",
                "--path=/usr/lib/x86_64-linux-gnu/libsomewhatlongername.so",
            ],
            "t --path=/usr/lib/x86_64-linux-gnu/libsomewhatlongername.so",
        ),
        (
            vec!["t", "/usr/lib/x86_64-linux-gnu/libsomethingverylongname.so"],
            "t /usr/lib/x86_64-linux-gnu/libsomethingverylongname.so",
        ),
    ];
    for (argv, want) in cases {
        assert_eq!(redact_argv(&argv), want, "{argv:?}");
    }
}

#[test]
fn redact_argv_credential_shapes() {
    for token in [
        "figd_TESTTOKEN",
        "ghp_TESTTOKEN",
        "gho_TESTTOKEN",
        "github_pat_TESTTOKEN",
        "sk-TESTTOKEN",
        "xoxb-TESTTOKEN",
        "xoxp-TESTTOKEN",
        // AKIA + 16 uppercase/digits is the whole shape.
        "AKIAXXXXXXXXXXXXXXXX",
        concat!(
            "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9",
            ".eyJzdWIiOiIxMjM0NTY3ODkwIn0",
            ".dozjgNryP4J3jVmNHl0w5t_Q"
        ),
        // 32+ char high-entropy token
        "9f8e7d6c5b4a3f2e1d0c9b8a7f6e5d4c",
    ] {
        assert_eq!(
            redact_argv(&["tool", token]),
            format!("tool {REDACTED}"),
            "{token}"
        );
    }
    // …but ordinary long args survive: a path (`/` excluded), an
    // all-alpha run (no digit), a short token-looking arg.
    for arg in [
        "/usr/lib/x86_64-linux-gnu/libsomethingverylongname.so",
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "short-token",
    ] {
        assert_eq!(redact_argv(&["tool", arg]), format!("tool {arg}"), "{arg}");
    }
    // Under a flag or env name the base64 set applies — an AWS
    // secret access key carries `/` and `+` and may be digit-free.
    let aws_secret = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";
    assert_eq!(
        redact_argv(&["t", &format!("--data={aws_secret}")]),
        "t --data=[REDACTED]"
    );
    assert_eq!(
        redact_argv(&["env", &format!("DATA={aws_secret}")]),
        "env DATA=[REDACTED]"
    );
}

/// CAD-141. Before the blob split, each of these scripts is one
/// argv element and `redact_argv` returned it unchanged — the
/// secret was still in the display string. Assertions check that
/// the secret is gone and that benign context remains; they do
/// not pin an incidental spelling of the mask.
#[test]
fn redact_argv_shell_blob_hides_nested_secrets() {
    let hidden_kept: &[(&str, &[&str], &[&str])] = &[
        (
            "run --token s3cr3tvalue && echo ok",
            &["s3cr3tvalue"],
            &["run", "--token", "echo ok"],
        ),
        (
            "run --token=s3cr3tvalue --verbose",
            &["s3cr3tvalue"],
            &["run", "--token", "--verbose"],
        ),
        (
            "run --token=\"s3cr3tvalue\" --verbose",
            &["s3cr3tvalue"],
            &["run", "--verbose"],
        ),
        (
            "run --token='s3cr3tvalue' --verbose",
            &["s3cr3tvalue"],
            &["run", "--verbose"],
        ),
        (
            "echo \"run --token s3cr3tvalue\"",
            &["s3cr3tvalue"],
            &["echo"],
        ),
        (
            "export TOKEN=\"s3cr3tvalue\" && echo ok",
            &["s3cr3tvalue"],
            &["export", "echo ok"],
        ),
        (
            "env GITHUB_TOKEN=ghp_TESTTOKEN cmd",
            &["ghp_TESTTOKEN"],
            &["env", "cmd"],
        ),
        (
            concat!(
                "curl -H 'Authorization: Basic ",
                "YWxpY2U6c3VwZXJzZWNyZXQ=",
                "' https://api.x"
            ),
            &["YWxpY2U6c3VwZXJzZWNyZXQ"],
            &["curl", "https://api.x"],
        ),
        (
            "curl -H \"X-Custom: sk-LIVE\" https://api.x",
            &["sk-LIVE"],
            &["curl", "https://api.x"],
        ),
        (
            "psql postgres://admin:hunter2@db.example.com:5432/app",
            &["hunter2"],
            &["psql", "postgres://admin:", "db.example.com"],
        ),
        ("echo figd_TESTTOKEN", &["figd_TESTTOKEN"], &["echo"]),
        ("mysql -phunter2 db", &["hunter2"], &["mysql", "db"]),
        (
            "echo \"say 'run --token s3cr3tvalue'\"",
            &["s3cr3tvalue"],
            &["echo"],
        ),
        (
            "export MSG=\"run --token s3cr3tvalue\"",
            &["s3cr3tvalue"],
            &["export"],
        ),
        (
            "EDITOR=vim cmd --token s3cr3tvalue",
            &["s3cr3tvalue"],
            &["EDITOR=vim", "cmd"],
        ),
        (
            "curl -H Authorization: Basic s3cr3tvalue https://api.x",
            &["s3cr3tvalue"],
            &["curl"],
        ),
        (
            "tool --password=\"hello hunter2\" --verbose",
            &["hunter2"],
            &["tool", "--verbose"],
        ),
    ];
    for (script, hidden, kept) in hidden_kept {
        let out = redact_argv(&["sh", "-c", script]);
        for secret in *hidden {
            assert!(
                !out.contains(secret),
                "leaked {secret} from {script:?} -> {out}"
            );
        }
        for bit in *kept {
            assert!(out.contains(bit), "lost {bit} from {script:?} -> {out}");
        }
        assert!(
            out.contains("[REDACTED]"),
            "no mask from {script:?} -> {out}"
        );
    }
    // A secret-named assignment that fills the element still
    // hides the value; the trailing word is withheld with it.
    let sealed = redact_argv(&["sh", "-c", "PGPASSWORD=hunter2 psql"]);
    assert!(!sealed.contains("hunter2"), "{sealed}");
    assert!(sealed.contains("[REDACTED]"), "{sealed}");
    // Same secret behind `export` keeps the following command.
    let exported = redact_argv(&["sh", "-c", "export PGPASSWORD=hunter2 psql"]);
    assert!(!exported.contains("hunter2"), "{exported}");
    assert!(exported.contains("psql"), "{exported}");
    // Direct `--password=two words` must not split the tail back out.
    let direct = redact_argv(&["tool", "--password=hello hunter2"]);
    assert_eq!(direct, "tool --password=[REDACTED]");
    assert!(!direct.contains("hunter2"));
}

/// Benign command text, including quotes, `$`, ports, SHAs and
/// ordinary flags, stays byte-identical. Malformed text is
/// withheld only when a secret is still visible.
#[test]
fn redact_argv_shell_blob_keeps_benign_text() {
    for script in [
        "echo hello && ls /tmp",
        "echo 'hello world'",
        "echo \"hello world\"",
        "export MSG=\"hello world\" && echo ok",
        "echo don't stop",
        "git checkout 4f2a9c1d8e3b5a7c9f1e2d3b4a5c6d7e8f9a0b1c",
        "npm publish --access public",
        "ssh -p 2222 host",
        "t monkey=banana",
        "echo $HOME && ls /tmp",
        "echo \"hello",
    ] {
        assert_eq!(
            redact_argv(&["sh", "-c", script]),
            format!("sh -c {script}"),
            "{script}"
        );
    }
    // Unbalanced quote, or `$`, next to a real secret: withhold
    // rather than print the value. No literal secret survives.
    for script in [
        "run --token \"s3cr3tvalue",
        "run --token s3cr3tvalue && echo $HOME",
    ] {
        let out = redact_argv(&["sh", "-c", script]);
        assert!(!out.contains("s3cr3tvalue"), "{script} -> {out}");
        assert!(out.contains("[REDACTED]"), "{script} -> {out}");
    }
}

/// Unicode whitespace is not a shell separator. The QA fixture
/// `run --token<NBSP>s3cr3tvalue` is still ambiguous
/// credential-bearing diagnostic text: the value must not remain
/// visible, while benign Unicode text is unchanged.
#[test]
fn redact_argv_unicode_whitespace_hides_flag_value() {
    let secret = "s3cr3tvalue";
    let ascii = redact_argv(&["sh", "-c", "run --token s3cr3tvalue && echo ok"]);
    assert!(!ascii.contains(secret), "{ascii}");
    assert!(ascii.contains("echo ok"), "{ascii}");
    assert!(ascii.contains("--token"), "{ascii}");
    // NBSP, then em space — one other Unicode space.
    for sep in ['\u{00a0}', '\u{2003}'] {
        let script = format!("run --token{sep}{secret}");
        let out = redact_argv(&["sh", "-c", &script]);
        assert!(
            !out.contains(secret),
            "U+{:04X} leaked in {out}",
            sep as u32
        );
        assert!(out.contains("[REDACTED]"), "U+{:04X} {out}", sep as u32);
        assert!(out.contains("run"), "{out}");
        assert!(out.contains("--token"), "{out}");
        let glued = format!("--token{sep}{secret}");
        let direct = redact_argv(&["tool", &glued]);
        assert!(!direct.contains(secret), "U+{:04X} {direct}", sep as u32);
        assert!(direct.contains("--token"), "{direct}");
    }
    for script in [
        "echo café",
        "echo hello\u{00a0}world",
        "echo hello\u{2003}world",
    ] {
        assert_eq!(
            redact_argv(&["sh", "-c", script]),
            format!("sh -c {script}"),
            "{script:?}"
        );
    }
}

#[test]
fn orphans_redact_shell_blob_argv() {
    let root = TempDir::new().unwrap();
    let scan = fake_scan(&root);
    let repo = scan.cwd.clone();
    add_pid_argv(
        &scan.proc_root,
        43,
        Some(&repo.join(".cadence/wt/gone")),
        None,
        Some(&["sh", "-c", "run --token s3cr3tvalue && echo ok"]),
        7_200,
        &[],
    );
    let c = check_orphans(&scan);
    let blob = serde_json::to_string(&c.to_json()).unwrap();
    for text in [&blob, &c.detail, &c.remedy] {
        assert!(!text.contains("s3cr3tvalue"), "{text}");
    }
    assert!(blob.contains("[REDACTED]"), "{blob}");
    assert!(blob.contains("echo ok"), "{blob}");
}

#[test]
fn orphans_redact_secret_argv() {
    let root = TempDir::new().unwrap();
    let scan = fake_scan(&root);
    let repo = scan.cwd.clone();
    add_pid(
        &scan.proc_root,
        40,
        Some(&repo.join(".cadence/wt/gone")),
        None,
        Some("npm exec figma-developer-mcp --figma-api-key=figd_TESTKEY0002 --stdio"),
        7_200,
        &[],
    );
    let c = check_orphans(&scan);
    // detail, remedy and the serialised JSON all carry `head` —
    // none may contain the credential.
    let blob = serde_json::to_string(&c.to_json()).unwrap();
    for text in [&blob, &c.detail, &c.remedy] {
        assert!(!text.contains("figd_TESTKEY0002"), "{text}");
    }
    assert!(blob.contains("--figma-api-key=[REDACTED]"));
    assert!(blob.contains("figma-developer-mcp"));
}

#[test]
fn orphans_redact_header_and_uri_argv() {
    let root = TempDir::new().unwrap();
    let scan = fake_scan(&root);
    let repo = scan.cwd.clone();
    // argv element with a space inside — only the vector form of
    // add_pid can write it.
    add_pid_argv(
        &scan.proc_root,
        41,
        Some(&repo.join(".cadence/wt/gone")),
        None,
        Some(&[
            "curl",
            "-H",
            "Authorization: Basic YWxpY2U6c3VwZXJzZWNyZXQ=",
            "https://api.x",
        ]),
        7_200,
        &[],
    );
    add_pid_argv(
        &scan.proc_root,
        42,
        Some(&repo.join(".cadence/wt/gone")),
        None,
        Some(&["psql", "postgres://admin:hunter2@db:5432/app"]),
        7_200,
        &[],
    );
    let c = check_orphans(&scan);
    let blob = serde_json::to_string(&c.to_json()).unwrap();
    for text in [&blob, &c.detail, &c.remedy] {
        assert!(!text.contains("YWxpY2U6c3VwZXJzZWNyZXQ"), "{text}");
        assert!(!text.contains("hunter2"), "{text}");
    }
    assert!(blob.contains("Authorization: [REDACTED]"), "{blob}");
    assert!(blob.contains("postgres://admin:[REDACTED]@db"), "{blob}");
    // …while the ordinary parts stay readable.
    assert!(blob.contains("curl"), "{blob}");
    assert!(blob.contains("https://api.x"), "{blob}");
}

#[test]
fn deleted_worktree_path_matching() {
    let root = TempDir::new().unwrap();
    let gone = root.path().join("repo/.cadence/wt/x");
    assert!(deleted_worktree(&gone).is_some());
    let live = root.path().join("repo/.cadence/wt/y");
    std::fs::create_dir_all(&live).unwrap();
    assert!(deleted_worktree(&live).is_none());
    assert!(deleted_worktree(Path::new("/usr/bin/bash")).is_none());
    assert!(deleted_worktree(Path::new("/repo/.cadence/wtbak/x")).is_none());
    // The proc "(deleted)" suffix is stripped before the exists check.
    let deleted_marked = PathBuf::from(format!("{} (deleted)", gone.display()));
    assert!(deleted_worktree(&deleted_marked).is_some());
}

// ---------- temp dirs ----------

#[test]
fn temp_dirs_count_by_prefix_and_age() {
    let root = TempDir::new().unwrap();
    let mut scan = fake_scan(&root);
    let tmp = scan.temp_dir.clone();
    for name in [
        "cadence-issue-at-1-2",
        "cadence-smoke",
        ".tmpAbc",
        "tmp.XYZ",
    ] {
        std::fs::create_dir_all(tmp.join(name)).unwrap();
    }
    std::fs::create_dir_all(tmp.join("unrelated")).unwrap();
    std::fs::write(tmp.join("cadence-file"), b"not a dir").unwrap();
    // Fresh dirs are below the age threshold — nothing counts.
    let c = check_temp_dirs(&scan);
    assert_eq!(c.level, Level::Ok);
    assert_eq!(c.value["count"].as_u64().unwrap(), 0);
    // Age them past a day; lower the count bar so four dirs warn.
    for name in [
        "cadence-issue-at-1-2",
        "cadence-smoke",
        ".tmpAbc",
        "tmp.XYZ",
    ] {
        set_mtime_old(&tmp.join(name), 90_000);
    }
    scan.thresholds.temp_warn_count = 3;
    let c = check_temp_dirs(&scan);
    assert_eq!(c.level, Level::Warn);
    assert_eq!(c.value["count"].as_u64().unwrap(), 4);
    assert!(c.remedy.contains("rm -rf"));
    assert!(c.remedy.contains("cadence-issue-at-1-2"));
}

/// CAD-273: an old pinned-runner install is never a leak, so the
/// remedy can never `rm -rf` the reviewed binary. A look-alike dir
/// without the binary, or with a non-version suffix, still counts.
#[test]
fn temp_dirs_never_list_the_pinned_runner() {
    let root = TempDir::new().unwrap();
    let scan = fake_scan(&root);
    let tmp = scan.temp_dir.clone();
    let runner = tmp.join("cadence-nextest-0.9.145");
    real_bytes(&runner.join("cargo-nextest"), 64);
    std::fs::create_dir_all(tmp.join("cadence-nextest-0.9.146")).unwrap();
    std::fs::create_dir_all(tmp.join("cadence-nextest-contract.AbC")).unwrap();
    real_bytes(&tmp.join("cadence-nextest-contract.AbC/cargo-nextest"), 8);
    for name in [
        "cadence-nextest-0.9.145",
        "cadence-nextest-0.9.146",
        "cadence-nextest-contract.AbC",
    ] {
        set_mtime_old(&tmp.join(name), 90_000);
    }
    let c = check_temp_dirs(&scan);
    assert_eq!(c.value["count"].as_u64().unwrap(), 2, "{}", c.value);
    assert!(
        !c.remedy.contains("cadence-nextest-0.9.145"),
        "{}",
        c.remedy
    );
    assert!(c.remedy.contains("cadence-nextest-0.9.146"), "{}", c.remedy);
    assert!(
        c.remedy.contains("cadence-nextest-contract.AbC"),
        "{}",
        c.remedy
    );
}

// ---------- legacy task cargo targets ----------

fn task_row<'a>(value: &'a Value, name: &str) -> &'a Value {
    value["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == name)
        .unwrap_or_else(|| panic!("no task-target row {name} in {value}"))
}

/// The remedy with the fixture root masked. A random TempDir name
/// such as `/tmp/.tmpXrmQ2a` would otherwise match a search for an
/// `rm` command (CAD-389).
fn remedy_without_root(c: &Check, root: &TempDir) -> String {
    c.remedy
        .replace(&root.path().display().to_string(), "<root>")
}

#[test]
fn legacy_task_target_name_matches_only_the_missed_shape() {
    for name in [
        "cad156-fix-target",
        "cad173-nextest-target-one",
        "cad176-pr100-target",
        "cad9-target",
    ] {
        assert!(
            legacy_task_target_name(std::ffi::OsStr::new(name)),
            "{name}"
        );
    }
    for name in [
        "cadence-issue-at-1-2",
        "cadence-smoke",
        ".tmpAbc",
        "tmp.XYZ",
        "unrelated",
        "my-target",
        "cad-target",
        "cad156-fix",
        "CAD156-fix-target",
        "notcad156-fix-target",
        "target",
    ] {
        assert!(
            !legacy_task_target_name(std::ffi::OsStr::new(name)),
            "{name} must stay outside the inventory"
        );
    }
}

#[test]
fn task_targets_list_missed_names_and_skip_unrelated() {
    let root = TempDir::new().unwrap();
    let scan = fake_scan(&root);
    let tmp = scan.temp_dir.clone();
    for name in [
        "cad156-fix-target",
        "cad173-nextest-target-one",
        "cad176-pr100-target",
    ] {
        let dir = tmp.join(name);
        std::fs::create_dir_all(dir.join("debug")).unwrap();
        real_bytes(&dir.join("debug/lib.rlib"), 4096);
        set_mtime_old(&dir, 90_000);
    }
    // Same age as a leak the temp-dirs check would delete — these
    // names must not join that `rm -rf` list.
    for name in [
        "cad156-fix-target",
        "cad173-nextest-target-one",
        "cad176-pr100-target",
    ] {
        set_mtime_old(&tmp.join(name), 90_000);
    }
    for name in [
        "unrelated",
        "cadence-issue-at-1-2",
        "cadence-smoke",
        "my-target",
        "cad-target",
        "cad156-fix",
        "target",
    ] {
        std::fs::create_dir_all(tmp.join(name)).unwrap();
        set_mtime_old(&tmp.join(name), 90_000);
    }
    std::fs::write(tmp.join("cad156-fix-target-file"), b"not a dir").unwrap();

    let temps = check_temp_dirs(&scan);
    assert_eq!(temps.value["count"].as_u64(), Some(2));
    let listed: Vec<&str> = temps.value["dirs"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|d| d["path"].as_str())
        .collect();
    assert!(listed
        .iter()
        .all(|p| !p.contains("cad156") && !p.contains("cad173") && !p.contains("cad176")));

    let c = check_task_targets(&scan);
    assert_eq!(c.level, Level::Ok, "{}", c.detail);
    assert_eq!(c.value["count"], 3);
    assert_eq!(c.value["safe_to_delete"], false);
    assert_eq!(c.value["record_search"], "complete");
    assert!(
        !remedy_without_root(&c, &root).contains("rm"),
        "{}",
        c.remedy
    );
    assert!(c.remedy.contains("read-only"));
    assert!(c.detail.contains("not proof"));
    let names: Vec<&str> = c.value["rows"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["name"].as_str())
        .collect();
    assert_eq!(
        names,
        vec![
            "cad156-fix-target",
            "cad173-nextest-target-one",
            "cad176-pr100-target",
        ]
    );
    for name in &names {
        let row = task_row(&c.value, name);
        assert_eq!(row["ownership"], "name-only");
        assert_eq!(row["proven"], false);
        assert_eq!(row["activity"], "unproven");
        assert_eq!(row["cwd_exe"], "none-observed");
        assert_eq!(row["cargo_lock"], "absent");
        assert_eq!(row["safe_to_delete"], false);
        assert_eq!(row["reclaim_candidate"], false);
        assert_eq!(row["action"], "none");
        assert_eq!(row["followed"], false);
        assert_eq!(row["exclude"], "name-only");
        assert!(row["age_secs"].as_u64().unwrap() >= 80_000);
        assert!(row["bytes"].as_u64().unwrap() > 0);
        assert_eq!(row["bytes_truncated"], false);
    }
}

#[test]
fn task_targets_do_not_follow_foreign_symlinks() {
    let root = TempDir::new().unwrap();
    let scan = fake_scan(&root);
    let tmp = scan.temp_dir.clone();
    let outside = root.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    real_bytes(&outside.join("payload"), 50_000);
    std::os::unix::fs::symlink(&outside, tmp.join("cad999-foreign-target")).unwrap();
    // A relative link that escapes the temp dir is foreign too.
    std::os::unix::fs::symlink("../outside", tmp.join("cad998-rel-target")).unwrap();
    // A link that stays inside the temp dir is still not walked.
    let inside = tmp.join("cad997-inside-target");
    std::fs::create_dir_all(&inside).unwrap();
    real_bytes(&inside.join("payload"), 50_000);
    std::os::unix::fs::symlink(&inside, tmp.join("cad997-link-target")).unwrap();

    let c = check_task_targets(&scan);
    assert_eq!(c.level, Level::Warn, "{}", c.detail);
    for name in [
        "cad999-foreign-target",
        "cad998-rel-target",
        "cad997-link-target",
    ] {
        let row = task_row(&c.value, name);
        assert_eq!(row["symlink"], true);
        assert_eq!(row["ancestor_symlink"], false);
        assert_eq!(row["followed"], false);
        assert_eq!(row["bytes"], Value::Null);
        assert_eq!(row["bytes_skipped"], "symlink");
        assert_eq!(row["cargo_lock"], "unknown");
        assert_eq!(row["safe_to_delete"], false);
        assert_eq!(row["action"], "none");
        assert_eq!(row["exclude"], "symlink");
        let blob = serde_json::to_string(row).unwrap();
        assert!(!blob.contains("payload"), "{blob}");
    }
    assert_eq!(
        task_row(&c.value, "cad999-foreign-target")["foreign_symlink"],
        true
    );
    assert_eq!(
        task_row(&c.value, "cad998-rel-target")["foreign_symlink"],
        true
    );
    assert_eq!(
        task_row(&c.value, "cad997-link-target")["foreign_symlink"],
        false
    );
    // The real directory the inside link points at is its own row
    // and is measured; the link row must not have added those bytes.
    let inside_row = task_row(&c.value, "cad997-inside-target");
    assert!(inside_row["bytes"].as_u64().unwrap() > 0);
    assert_eq!(
        task_row(&c.value, "cad997-link-target")["bytes"],
        Value::Null
    );
}

fn mkfifo(path: &Path) {
    let c = CString::new(path.as_os_str().as_bytes()).unwrap();
    assert_eq!(
        unsafe { libc::mkfifo(c.as_ptr(), 0o644) },
        0,
        "mkfifo {}",
        path.display()
    );
}

/// The probe must return. A regression that blocks in `open` fails
/// this instead of hanging the suite.
fn probe_lock_bounded(path: &Path) -> LockBit {
    let path = path.to_path_buf();
    let shown = path.display().to_string();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(probe_lock_file(&path));
    });
    rx.recv_timeout(Duration::from_secs(2))
        .unwrap_or_else(|_| panic!("probe_lock_file blocked on {shown}"))
}

fn check_targets_bounded(scan: Scan) -> Check {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(check_task_targets(&scan));
    });
    rx.recv_timeout(Duration::from_secs(2))
        .expect("task-target inventory blocked")
}

#[test]
fn task_targets_lock_probe_rejects_fifo_and_symlink_without_blocking() {
    // `rm` in the root on purpose: the no-`rm` remedy check below
    // must not depend on what the random TempDir name spells.
    let root = tempfile::Builder::new()
        .prefix("cad389-rm-")
        .tempdir()
        .unwrap();
    let scan = fake_scan(&root);
    let fifo_dir = scan.temp_dir.join("cad410-fifo-target");
    std::fs::create_dir_all(fifo_dir.join("debug")).unwrap();
    mkfifo(&fifo_dir.join(".cargo-lock"));
    mkfifo(&fifo_dir.join("debug/.cargo-lock"));

    let link_dir = scan.temp_dir.join("cad411-locklink-target");
    std::fs::create_dir_all(&link_dir).unwrap();
    let fifo = root.path().join("elsewhere-fifo");
    mkfifo(&fifo);
    std::os::unix::fs::symlink(&fifo, link_dir.join(".cargo-lock")).unwrap();

    let free_dir = scan.temp_dir.join("cad412-freelock-target");
    std::fs::create_dir_all(&free_dir).unwrap();
    std::fs::write(free_dir.join(".cargo-lock"), b"lock").unwrap();

    assert_eq!(
        probe_lock_bounded(&fifo_dir.join(".cargo-lock")),
        LockBit::Unknown
    );
    assert_eq!(
        probe_lock_bounded(&fifo_dir.join("debug/.cargo-lock")),
        LockBit::Unknown
    );
    assert_eq!(
        probe_lock_bounded(&link_dir.join(".cargo-lock")),
        LockBit::Unknown
    );
    assert_eq!(
        probe_lock_bounded(&free_dir.join(".cargo-lock")),
        LockBit::Free
    );

    let c = check_targets_bounded(scan);
    for name in ["cad410-fifo-target", "cad411-locklink-target"] {
        let row = task_row(&c.value, name);
        assert_eq!(row["cargo_lock"], "unknown", "{name}");
        assert_eq!(row["followed"], false, "{name}");
        assert_eq!(row["safe_to_delete"], false, "{name}");
        assert_eq!(row["action"], "none", "{name}");
    }
    assert_eq!(
        task_row(&c.value, "cad412-freelock-target")["cargo_lock"],
        "free"
    );
    assert!(
        !remedy_without_root(&c, &root).contains("rm"),
        "{}",
        c.remedy
    );
}

#[test]
fn lock_probe_releases_a_lock_its_descriptor_shares() {
    let root = TempDir::new().unwrap();
    let lock = root.path().join(".cargo-lock");
    std::fs::write(&lock, b"lock").unwrap();
    let file = std::fs::File::open(&lock).unwrap();
    // Stands in for a sibling thread's fork that has not exec'd yet.
    let forked = file.try_clone().unwrap();
    assert_eq!(try_lock_probe(&file), LockBit::Free);
    drop(file);
    assert_eq!(probe_lock_bounded(&lock), LockBit::Free);
    drop(forked);
}

#[test]
fn task_targets_do_not_follow_ancestor_symlinks() {
    let root = TempDir::new().unwrap();
    let mut scan = fake_scan(&root);
    let real = root.path().join("real-cache");
    let hidden = real.join("cad400-ancestor-target");
    std::fs::create_dir_all(&hidden).unwrap();
    real_bytes(&hidden.join("payload.bin"), 50_000);
    mkfifo(&hidden.join(".cargo-lock"));
    let via = scan.temp_dir.join("via-link");
    std::os::unix::fs::symlink(&real, &via).unwrap();
    let through = via.join("cad400-ancestor-target");
    // Final-component lstat would follow `via-link` and then block
    // on the FIFO. The probe must stop at the ancestor link.
    assert_eq!(
        probe_lock_bounded(&through.join(".cargo-lock")),
        LockBit::Unknown
    );
    assert!(matches!(
        lexical_kind(&through),
        LexicalKind::Symlink {
            final_component: false,
            ..
        }
    ));
    scan.cargo_target_dir = Some(through);
    let c = check_targets_bounded(scan);
    let row = task_row(&c.value, "cad400-ancestor-target");
    assert_eq!(row["ancestor_symlink"], true);
    assert_eq!(row["symlink"], true);
    assert_eq!(row["followed"], false);
    assert_eq!(row["bytes"], Value::Null);
    assert_eq!(row["bytes_skipped"], "symlink");
    assert_eq!(row["cargo_lock"], "unknown");
    assert_eq!(row["foreign_symlink"], true);
    assert_eq!(row["safe_to_delete"], false);
    assert_eq!(row["reclaim_candidate"], false);
    assert_eq!(row["action"], "none");
    let blob = serde_json::to_string(&c.to_json()).unwrap();
    assert!(!blob.contains("payload.bin"), "{blob}");
    assert!(!blob.contains("rm "), "{blob}");
}

#[test]
fn task_targets_skip_foreign_uid_walks() {
    let root = TempDir::new().unwrap();
    let mut scan = fake_scan(&root);
    scan.uid = scan.uid.wrapping_add(1);
    let dir = scan.temp_dir.join("cad156-fix-target");
    std::fs::create_dir_all(dir.join("debug")).unwrap();
    real_bytes(&dir.join("debug/lib.rlib"), 20_000);
    // A symlink planted inside must not be followed just because
    // the name matched: the whole tree is someone else's.
    std::os::unix::fs::symlink("/etc", dir.join("debug/escape")).unwrap();

    let c = check_task_targets(&scan);
    let row = task_row(&c.value, "cad156-fix-target");
    assert_eq!(row["uid_matches"], false);
    assert_eq!(row["bytes"], Value::Null);
    assert_eq!(row["bytes_skipped"], "foreign-uid");
    assert_eq!(row["cargo_lock"], "unknown");
    assert_eq!(row["activity"], "unknown");
    assert_eq!(row["exclude"], "foreign-uid");
    assert_eq!(row["safe_to_delete"], false);
    assert_eq!(row["followed"], false);
    assert_eq!(c.level, Level::Warn);
    let blob = serde_json::to_string(&c.to_json()).unwrap();
    assert!(!blob.contains("escape"), "{blob}");
}

#[test]
fn task_targets_live_lock_and_cwd_are_active_not_safe() {
    let root = TempDir::new().unwrap();
    let scan = fake_scan(&root);
    let live = scan.temp_dir.join("cad176-pr100-target");
    let idle = scan.temp_dir.join("cad156-fix-target");
    let decoy = scan.temp_dir.join("cad156-fix-target-extra");
    for dir in [&live, &idle, &decoy] {
        std::fs::create_dir_all(dir.join("debug")).unwrap();
    }
    let lock_path = live.join("debug/.cargo-lock");
    std::fs::write(&lock_path, b"lock").unwrap();
    let _held = crate::worktree::TestFileLock::acquire(&lock_path);
    add_pid(
        &scan.proc_root,
        176,
        Some(&live.join("debug")),
        None,
        Some("cargo test --lib host_inventory_marker"),
        30,
        &[],
    );
    // Component-wise: this cwd is the sibling, not the live dir.
    add_pid(
        &scan.proc_root,
        177,
        Some(&decoy),
        Some(&live.join("debug/cadence")),
        None,
        30,
        &[],
    );

    let c = check_task_targets(&scan);
    assert_eq!(c.level, Level::Warn, "{}", c.detail);
    let live_row = task_row(&c.value, "cad176-pr100-target");
    assert_eq!(live_row["cargo_lock"], "held");
    assert_eq!(live_row["activity"], "active");
    assert_eq!(live_row["cwd_exe"], "observed");
    assert_eq!(live_row["exclude"], "active");
    assert_eq!(live_row["safe_to_delete"], false);
    assert_eq!(live_row["reclaim_candidate"], false);
    assert_eq!(live_row["action"], "none");
    let pids: Vec<u64> = live_row["pids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p.as_u64().unwrap())
        .collect();
    assert_eq!(pids, vec![176, 177]);
    let idle_row = task_row(&c.value, "cad156-fix-target");
    assert_eq!(idle_row["activity"], "unproven");
    assert_eq!(idle_row["cwd_exe"], "none-observed");
    assert_eq!(idle_row["cargo_lock"], "absent");
    assert_eq!(idle_row["safe_to_delete"], false);
    assert!(idle_row["pids"].as_array().unwrap().is_empty());
    let decoy_row = task_row(&c.value, "cad156-fix-target-extra");
    let decoy_pids: Vec<u64> = decoy_row["pids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p.as_u64().unwrap())
        .collect();
    assert_eq!(decoy_pids, vec![177]);
    assert!(!decoy_row["pids"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p.as_u64() == Some(176)));
    let blob = serde_json::to_string(&c.to_json()).unwrap();
    // The synthetic cmdline is a neutral marker. This inventory
    // must not echo process arguments.
    assert!(!blob.contains("host_inventory_marker"), "{blob}");
    assert!(!blob.contains("rm -rf"), "{blob}");
    assert!(c.detail.contains("not proof"));
}

#[test]
fn task_targets_recorded_and_configured_are_proven() {
    let root = TempDir::new().unwrap();
    let mut scan = fake_scan(&root);
    let recorded = scan.temp_dir.join("recorded-custom-out");
    let named = scan.temp_dir.join("cad200-only-target");
    let both = scan.temp_dir.join("cad201-recorded-target");
    let configured = scan.cwd.join(".cadence/target/shared");
    for dir in [&recorded, &named, &both, &configured] {
        std::fs::create_dir_all(dir).unwrap();
    }
    real_bytes(&named.join("lib.rlib"), 128);
    scan.cargo_target_dir = Some(PathBuf::from(".cadence/target/shared"));
    let pm = scan.pm_dir.clone().unwrap();
    write_issue(
            &pm,
            "cadence",
            "CAD-201",
            "doing",
            &format!(
                "refs:\n  - kind: worktree\n    path: {}\n    cargo_target: {}\n  - kind: worktree\n    path: {}\n    cargo_target: {}\n",
                scan.cwd.join(".cadence/wt/cad-201").display(),
                both.display(),
                scan.cwd.join(".cadence/wt/custom").display(),
                recorded.display(),
            ),
        );

    let c = check_task_targets(&scan);
    assert_eq!(c.value["record_search"], "complete");
    assert_eq!(c.value["record_conclusive"], true);
    let named_row = task_row(&c.value, "cad200-only-target");
    assert_eq!(named_row["ownership"], "name-only");
    assert_eq!(named_row["proven"], false);
    assert_eq!(named_row["safe_to_delete"], false);
    let both_row = task_row(&c.value, "cad201-recorded-target");
    assert_eq!(both_row["ownership"], "recorded");
    assert_eq!(both_row["proven"], true);
    assert_eq!(both_row["name_match"], true);
    assert_eq!(both_row["issues"][0], "CAD-201");
    assert_eq!(both_row["safe_to_delete"], false);
    assert_eq!(both_row["reclaim_candidate"], false);
    let recorded_row = task_row(&c.value, "recorded-custom-out");
    assert_eq!(recorded_row["ownership"], "recorded");
    assert_eq!(recorded_row["proven"], true);
    assert_eq!(recorded_row["name_match"], false);
    assert_eq!(recorded_row["pressure"], true);
    let configured_row = task_row(&c.value, "shared");
    assert_eq!(configured_row["ownership"], "configured");
    assert_eq!(configured_row["proven"], true);
    assert_eq!(configured_row["configured"], true);
    assert_eq!(configured_row["pressure"], false);
    assert_eq!(configured_row["bytes_skipped"], "tracked-elsewhere");
    assert_eq!(configured_row["safe_to_delete"], false);
    // Unrelated names stay out even when a tracker is present.
    assert!(c.value["rows"]
        .as_array()
        .unwrap()
        .iter()
        .all(|r| r["name"] != "unrelated"));
    let blob = serde_json::to_string(&c.to_json()).unwrap();
    assert!(!blob.contains("rm "), "{blob}");
}

#[test]
fn task_targets_unreadable_tracker_does_not_prove_name_only() {
    let root = TempDir::new().unwrap();
    let scan = fake_scan(&root);
    let pm = scan.pm_dir.clone().unwrap();
    std::fs::remove_dir_all(&pm).unwrap();
    std::fs::write(&pm, b"not a directory").unwrap();
    std::fs::create_dir_all(scan.temp_dir.join("cad156-fix-target")).unwrap();
    let c = check_task_targets(&scan);
    assert_eq!(c.value["record_search"], "unreadable");
    assert_eq!(c.value["record_conclusive"], false);
    let row = task_row(&c.value, "cad156-fix-target");
    assert_eq!(row["ownership"], "name-only");
    assert_eq!(row["proven"], false);
    assert_eq!(row["safe_to_delete"], false);
}

#[test]
fn dir_size_limited_unreadable_directory_is_truncated() {
    let root = TempDir::new().unwrap();
    let missing = root.path().join("missing-cad233");
    let (bytes, truncated, visited) = dir_size_limited(&missing, 32);
    assert_eq!(bytes, 0);
    assert!(!truncated);
    assert_eq!(visited, 0);

    let dir = root.path().join("sized");
    std::fs::create_dir_all(dir.join("open")).unwrap();
    real_bytes(&dir.join("open/seen.bin"), 4096);
    let (open_bytes, open_truncated, _) = dir_size_limited(&dir, 32);
    assert!(!open_truncated);
    assert!(open_bytes >= 4096);

    let secret = dir.join("secret");
    std::fs::create_dir_all(&secret).unwrap();
    real_bytes(&secret.join("hidden.bin"), 80_000);
    let _restore = deny_directory(&secret);
    let (bytes, truncated, _) = dir_size_limited(&dir, 32);
    assert!(truncated);
    assert!(bytes < 80_000, "hidden bytes were counted: {bytes}");
    assert!(bytes >= open_bytes);
}

/// CAD-262: an unreadable subdir must not hide readable siblings,
/// whatever order readdir yields them in. With eight readable
/// siblings around one unreadable dir, a walk that stops at the
/// first error only counts them all when the unreadable one happens
/// to be listed first (about 1 in 9), so an order-dependent walk
/// fails here almost every run.
#[test]
fn dir_size_limited_counts_every_readable_sibling_of_an_unreadable_dir() {
    let root = TempDir::new().unwrap();
    let dir = root.path().join("sized");
    let names = ["a0", "b1", "c2", "d3", "w4", "x5", "y6", "z7"];
    for name in names {
        real_bytes(&dir.join(name).join("seen.bin"), 4096);
    }
    let (all_open, open_truncated, _) = dir_size_limited(&dir, 256);
    assert!(!open_truncated);
    assert!(all_open >= 8 * 4096, "{all_open}");
    let secret = dir.join("m-secret");
    real_bytes(&secret.join("hidden.bin"), 80_000);
    let _restore = deny_directory(&secret);
    let (bytes, truncated, _) = dir_size_limited(&dir, 256);
    assert!(truncated, "an unreadable subdir is a lower bound");
    assert_eq!(
        bytes, all_open,
        "every readable sibling counted, hidden bytes not"
    );
    // An unreadable root is still a truncated empty measurement.
    let (bytes, truncated, visited) = dir_size_limited(&secret, 256);
    assert_eq!((bytes, truncated, visited), (0, true, 0));
}

#[test]
fn task_targets_unreadable_child_is_not_a_finished_measurement() {
    let root = TempDir::new().unwrap();
    let scan = fake_scan(&root);
    let dir = scan.temp_dir.join("cad420-partial-target");
    std::fs::create_dir_all(dir.join("secret")).unwrap();
    real_bytes(&dir.join("seen.bin"), 2048);
    real_bytes(&dir.join("secret/hidden.bin"), 80_000);
    let _restore = deny_directory(&dir.join("secret"));
    let c = check_task_targets(&scan);
    let row = task_row(&c.value, "cad420-partial-target");
    assert_eq!(row["bytes_truncated"], true);
    assert_eq!(row["safe_to_delete"], false);
    assert_eq!(row["reclaim_candidate"], false);
    assert_eq!(row["action"], "none");
    assert_eq!(c.value["scan_truncated"], true);
    assert_eq!(c.value["safe_to_delete"], false);
    assert_eq!(c.level, Level::Warn);
    assert!(row["bytes"].as_u64().unwrap() < 80_000);
    let blob = serde_json::to_string(&c.to_json()).unwrap();
    assert!(!blob.contains("rm "), "{blob}");
}

#[test]
fn task_targets_denied_issue_record_is_not_conclusive() {
    let root = TempDir::new().unwrap();
    let scan = fake_scan(&root);
    let pm = scan.pm_dir.clone().unwrap();
    let hidden = pm.join("cadence").join("CAD-420");
    std::fs::create_dir_all(&hidden).unwrap();
    std::fs::write(
            hidden.join("issue.md"),
            format!(
                "---\nid: CAD-420\ntitle: t\nstatus: doing\npriority: P2\nrefs:\n  - kind: worktree\n    path: {}\n    cargo_target: {}\ncreated: 2026-09-19T00:00:00Z\n---\n\nbody\n",
                scan.cwd.display(),
                scan.temp_dir.join("cad420-recorded-target").display(),
            ),
        )
        .unwrap();
    write_issue(&pm, "cadence", "CAD-421", "doing", "");
    let _restore = deny_directory(&hidden);
    std::fs::create_dir_all(scan.temp_dir.join("cad420-recorded-target")).unwrap();
    let c = check_task_targets(&scan);
    assert_eq!(c.value["record_search"], "incomplete");
    assert_eq!(c.value["record_conclusive"], false);
    let row = task_row(&c.value, "cad420-recorded-target");
    assert_eq!(row["ownership"], "name-only");
    assert_eq!(row["proven"], false);
    assert_eq!(row["safe_to_delete"], false);
    assert_eq!(row["reclaim_candidate"], false);
    assert_eq!(c.level, Level::Warn);
    assert_ne!(c.detail, "none");
}

#[test]
fn task_targets_unreadable_tracker_with_no_rows_is_not_none() {
    let root = TempDir::new().unwrap();
    let scan = fake_scan(&root);
    let pm = scan.pm_dir.clone().unwrap();
    std::fs::remove_dir_all(&pm).unwrap();
    std::fs::write(&pm, b"not a directory").unwrap();
    let c = check_task_targets(&scan);
    assert_eq!(c.value["record_search"], "unreadable");
    assert_eq!(c.value["record_conclusive"], false);
    assert_eq!(c.value["count"], 0);
    assert_eq!(c.value["safe_to_delete"], false);
    assert_ne!(c.detail, "none");
    assert!(c.detail.contains("unreadable"), "{}", c.detail);
    assert_eq!(c.level, Level::Warn);
    assert!(c.remedy.is_empty());
}

#[test]
fn task_targets_denied_proc_dir_is_not_a_complete_idle_scan() {
    let root = TempDir::new().unwrap();
    let scan = fake_scan(&root);
    let pid = scan.proc_root.join("4242");
    std::fs::create_dir_all(&pid).unwrap();
    let _restore = deny_directory(&pid);
    std::fs::create_dir_all(scan.temp_dir.join("cad421-proc-target")).unwrap();
    let c = check_task_targets(&scan);
    assert_eq!(c.value["proc_scan"], "partial");
    let row = task_row(&c.value, "cad421-proc-target");
    assert_eq!(row["cwd_exe"], "unreadable");
    assert_eq!(row["activity"], "unknown");
    assert_eq!(row["exclude"], "unknown");
    assert_eq!(row["safe_to_delete"], false);
    assert_eq!(row["reclaim_candidate"], false);
    assert_eq!(row["action"], "none");
    assert_eq!(c.level, Level::Warn);
    assert!(c.detail.contains("not proof"), "{}", c.detail);
    assert!(c.detail.contains("partial"), "{}", c.detail);
    let body = c.to_json();
    assert_eq!(body["level"], "warn");
    assert_eq!(exit_code(&json!({"level": body["level"]})), 1);
}

#[test]
fn task_targets_unreadable_proc_with_no_rows_is_not_a_healthy_gate() {
    let root = TempDir::new().unwrap();
    let scan = fake_scan(&root);
    let _restore = deny_directory(&scan.proc_root);
    let c = check_task_targets(&scan);
    assert_eq!(c.value["proc_scan"], "unreadable");
    assert_eq!(c.value["count"], 0);
    assert_eq!(c.value["safe_to_delete"], false);
    assert_eq!(c.level, Level::Warn);
    assert_ne!(c.detail, "none");
    assert!(c.detail.contains("unreadable"), "{}", c.detail);
    let body = c.to_json();
    assert_eq!(exit_code(&json!({"level": body["level"]})), 1);
    assert!(c.remedy.is_empty());
}

#[test]
fn task_targets_unreadable_temp_dir_is_not_an_empty_inventory() {
    let root = TempDir::new().unwrap();
    let scan = fake_scan(&root);
    std::fs::create_dir_all(scan.temp_dir.join("cad422-hidden-target")).unwrap();
    let _restore = deny_directory(&scan.temp_dir);
    let c = check_task_targets(&scan);
    assert_eq!(c.value["temp_scan"], "unreadable");
    assert_eq!(c.value["count"], 0);
    assert_eq!(c.value["safe_to_delete"], false);
    assert_eq!(c.level, Level::Warn);
    assert!(c.detail.contains("unreadable"), "{}", c.detail);
    assert_ne!(c.detail, "none");
    assert!(c.remedy.is_empty());
}

#[test]
fn task_targets_truncate_a_long_name_scan() {
    let root = TempDir::new().unwrap();
    let scan = fake_scan(&root);
    for i in 0..=TASK_TARGET_ROW_CAP {
        std::fs::create_dir_all(scan.temp_dir.join(format!("cad{i:04}-row-target"))).unwrap();
    }
    let c = check_task_targets(&scan);
    assert_eq!(c.value["count"], TASK_TARGET_ROW_CAP);
    assert_eq!(c.value["scan_truncated"], true);
    assert_eq!(c.level, Level::Warn);
    assert!(task_row(&c.value, "cad0000-row-target")["safe_to_delete"] == false);
    assert!(c.value["rows"]
        .as_array()
        .unwrap()
        .iter()
        .all(|r| r["name"] != format!("cad{TASK_TARGET_ROW_CAP:04}-row-target")));
    assert!(
        !remedy_without_root(&c, &root).contains("rm"),
        "{}",
        c.remedy
    );
}

// ---------- stale worktrees ----------

#[test]
fn worktrees_flag_merged_or_closed_only() {
    let root = TempDir::new().unwrap();
    let scan = fake_scan(&root);
    let repo = scan.cwd.clone();
    init_repo(&repo);
    // wt1: branch merged into main, clean tree → stale.
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            ".cadence/wt/feat-a",
            "-b",
            "feat-a",
        ],
    );
    std::fs::write(repo.join(".cadence/wt/feat-a/f"), b"x").unwrap();
    git(&repo.join(".cadence/wt/feat-a"), &["add", "-A"]);
    git(
        &repo.join(".cadence/wt/feat-a"),
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-qm",
            "work",
        ],
    );
    git(&repo, &["merge", "-q", "feat-a"]);
    // wt2: unmerged branch, no tracker → not stale.
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            ".cadence/wt/feat-b",
            "-b",
            "feat-b",
        ],
    );
    std::fs::write(repo.join(".cadence/wt/feat-b/f2"), b"x").unwrap();
    git(&repo.join(".cadence/wt/feat-b"), &["add", "-A"]);
    git(
        &repo.join(".cadence/wt/feat-b"),
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-qm",
            "wip",
        ],
    );
    // wt3: unmerged but tracker says done → stale.
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            ".cadence/wt/cad-1-x",
            "-b",
            "cadence/cad-1-x",
        ],
    );
    std::fs::write(repo.join(".cadence/wt/cad-1-x/f3"), b"x").unwrap();
    git(&repo.join(".cadence/wt/cad-1-x"), &["add", "-A"]);
    git(
        &repo.join(".cadence/wt/cad-1-x"),
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-qm",
            "wip",
        ],
    );
    write_issue(
        scan.pm_dir.as_ref().unwrap(),
        "cadence",
        "CAD-1",
        "done",
        "",
    );
    // wt4: merged branch but dirty tree and open tracker → not stale.
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            ".cadence/wt/cad-2-y",
            "-b",
            "cadence/cad-2-y",
        ],
    );
    std::fs::write(repo.join(".cadence/wt/cad-2-y/f4"), b"x").unwrap();
    git(&repo.join(".cadence/wt/cad-2-y"), &["add", "-A"]);
    git(
        &repo.join(".cadence/wt/cad-2-y"),
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-qm",
            "work",
        ],
    );
    git(&repo, &["merge", "-q", "cadence/cad-2-y"]);
    write_issue(
        scan.pm_dir.as_ref().unwrap(),
        "cadence",
        "CAD-2",
        "doing",
        "",
    );
    std::fs::write(repo.join(".cadence/wt/cad-2-y/uncommitted"), b"wip").unwrap();

    let c = check_worktrees(&scan);
    assert_eq!(c.level, Level::Warn);
    let mut names: Vec<String> = c.value["stale"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            s["path"]
                .as_str()
                .unwrap()
                .rsplit('/')
                .next()
                .unwrap()
                .to_string()
        })
        .collect();
    names.sort();
    assert_eq!(names, vec!["cad-1-x", "feat-a"]);
    assert!(c.remedy.contains("cadence issue finish CAD-1"));

    // Now close CAD-2's worktree ref in the tracker — dirty tree or
    // not, a closed ref means finished.
    write_issue(
        scan.pm_dir.as_ref().unwrap(),
        "cadence",
        "CAD-2",
        "doing",
        &format!(
            "refs:\n- kind: worktree\n  path: {}\n  closed: true\n",
            repo.join(".cadence/wt/cad-2-y").display()
        ),
    );
    let c = check_worktrees(&scan);
    let mut names: Vec<String> = c.value["stale"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            s["path"]
                .as_str()
                .unwrap()
                .rsplit('/')
                .next()
                .unwrap()
                .to_string()
        })
        .collect();
    names.sort();
    assert_eq!(names, vec!["cad-1-x", "cad-2-y", "feat-a"]);
}

#[test]
fn worktrees_count_shared_target_once() {
    let root = TempDir::new().unwrap();
    let scan = fake_scan(&root);
    let repo = scan.cwd.clone();
    init_repo(&repo);
    // Two live lanes, one shared cache — the cache's bytes land
    // once in `shared_cargo_target`, never inside a lane's row.
    for name in ["cad-1-a", "cad-2-b"] {
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                &format!(".cadence/wt/{name}"),
                "-b",
                &format!("cadence/{name}"),
            ],
        );
    }
    let shared = repo.join(".cadence/target/shared");
    real_bytes(&shared.join("dep.rlib"), 4 * 1024 * 1024);
    let c = check_worktrees(&scan);
    let st = &c.value["shared_cargo_target"];
    assert_eq!(
        st["path"].as_str().unwrap(),
        shared.to_string_lossy(),
        "{st}"
    );
    assert_eq!(st["bytes"].as_u64().unwrap(), 4 * 1024 * 1024);
    // Exactly one shared entry — a `stale` row per lane never
    // carries the shared bytes with it.
    assert!(c.value["stale"]
        .as_array()
        .unwrap()
        .iter()
        .all(|s| s["path"].as_str().unwrap() != shared.to_string_lossy()));
    assert!(c.detail.contains("shared cargo cache"), "{}", c.detail);
}

#[test]
fn reclaim_plan_lists_without_deleting() {
    let root = TempDir::new().unwrap();
    let scan = fake_scan(&root);
    let repo = scan.cwd.clone();
    init_repo(&repo);
    // A live lane with a per-lane target/, the shared cache, and
    // a stale lane with its own target/ — all listed, none
    // deleted, and the stale lane's bytes never count its
    // target/ twice.
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            ".cadence/wt/cad-1-live",
            "-b",
            "cadence/cad-1-live",
        ],
    );
    let live = repo.join(".cadence/wt/cad-1-live");
    // A commit past base keeps the lane genuinely live — a branch
    // at base with a clean tree reads as stale.
    std::fs::write(live.join("wip.txt"), "x").unwrap();
    git(&live, &["add", "-A"]);
    git(
        &live,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-qm",
            "wip",
        ],
    );
    let lane_target = live.join("target");
    real_bytes(&lane_target.join("dep.rlib"), 1024 * 1024);
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            ".cadence/wt/feat-gone",
            "-b",
            "feat-gone",
        ],
    );
    let gone = repo.join(".cadence/wt/feat-gone");
    // Committed content keeps the tree clean past the merge; the
    // ignored target/ adds reclaimable bytes without dirtying it.
    real_bytes(&gone.join("notes.txt"), 64 * 1024);
    git(&gone, &["add", "-A"]);
    git(
        &gone,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-qm",
            "notes",
        ],
    );
    git(&repo, &["merge", "-q", "feat-gone"]);
    real_bytes(&gone.join("target/dep.rlib"), 512 * 1024);
    let shared = repo.join(".cadence/target/shared");
    // Reclaimable bytes live in the shared subdirs — and a
    // retired r2-era `examples/` gets its own row.
    real_bytes(&shared.join("debug/deps/dep.rlib"), 2 * 1024 * 1024);
    real_bytes(&shared.join("debug/examples/ex.bin"), 128 * 1024);

    let plan = reclaim_plan(&scan);
    let rows = plan["rows"].as_array().unwrap();
    let kinds: Vec<&str> = rows.iter().map(|r| r["kind"].as_str().unwrap()).collect();
    assert!(
        kinds.contains(&"worktree-target")
            && kinds.contains(&"shared-cargo-cache")
            && kinds.contains(&"stale-worktree")
            && kinds.contains(&"retired-shared-dir"),
        "{kinds:?}"
    );
    // Live-lane target rows are informational — excluded from the
    // reclaimable total and surfaced on their own line instead.
    assert_eq!(
        plan["reclaimable_bytes"].as_u64().unwrap(),
        rows.iter()
            .filter(|r| r["kind"] != "worktree-target")
            .map(|r| r["bytes"].as_u64().unwrap())
            .sum::<u64>()
    );
    assert_eq!(
        plan["freed_with_lanes_bytes"].as_u64().unwrap(),
        rows.iter()
            .filter(|r| r["kind"] == "worktree-target")
            .map(|r| r["bytes"].as_u64().unwrap())
            .sum::<u64>()
    );
    // A stale lane's whole dir — target/ included — is freed by
    // its own row's command, so its bytes are the full dir.
    for stale in rows.iter().filter(|r| r["kind"] == "stale-worktree") {
        let (whole, _) = dir_size(Path::new(stale["path"].as_str().unwrap()));
        assert_eq!(stale["bytes"].as_u64().unwrap(), whole, "{stale}");
    }
    let gone_row = rows
        .iter()
        .find(|r| {
            r["kind"] == "stale-worktree" && r["path"].as_str().unwrap().ends_with("feat-gone")
        })
        .unwrap();
    assert!(gone_row["bytes"].as_u64().unwrap() >= 512 * 1024);
    // A stale lane never also emits an informational target row.
    assert!(!rows.iter().any(|r| r["kind"] == "worktree-target"
        && r["path"]
            .as_str()
            .unwrap()
            .starts_with(gone.to_str().unwrap())));
    // A live lane's target row describes how it frees — it never
    // reads as "finish your in-progress work".
    let live = rows
        .iter()
        .find(|r| r["kind"] == "worktree-target")
        .unwrap();
    assert!(live["action"]
        .as_str()
        .unwrap()
        .contains("freed with the lane"));
    // Every row names its action and filesystem; nothing deleted.
    assert!(rows
        .iter()
        .all(|r| !r["action"].as_str().unwrap().is_empty()));
    assert!(lane_target.is_dir() && shared.is_dir());
    assert!(gone.is_dir());
    let text = render_reclaim(&plan);
    assert!(
        text.contains("total reclaimable") && text.contains("shared-cargo-cache"),
        "{text}"
    );
}

#[test]
fn reclaim_plan_quotes_paths_and_reports_lock() {
    // A repo whose path contains a space — unquoted `rm -rf`
    // would split it into extra arguments.
    let root = tempfile::Builder::new()
        .prefix("my proj ")
        .tempdir()
        .unwrap();
    let mut scan = fake_scan(&root);
    let repo = root.path().join("repo dir");
    std::fs::create_dir_all(&repo).unwrap();
    scan.cwd = repo.clone();
    init_repo(&repo);
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            ".cadence/wt/feat gone",
            "-b",
            "feat-gone",
        ],
    );
    git(&repo, &["merge", "-q", "feat-gone"]);
    let shared = repo.join(".cadence/target/shared");
    real_bytes(&shared.join("dep.rlib"), 4096);

    let plan = reclaim_plan(&scan);
    let rows = plan["rows"].as_array().unwrap();
    // Every emitted command's path args round-trip through a real
    // shell word-split: `set -- <quoted>` must hand back exactly
    // the original paths.
    let actions: Vec<String> = rows
        .iter()
        .map(|r| r["action"].as_str().unwrap().to_string())
        .collect();
    let stale = actions
        .iter()
        .find(|a| a.contains("worktree remove"))
        .expect("stale row")
        .clone();
    // The git -C line: `git -C <root> worktree remove <path>` —
    // extract the two path args and ask `sh` to split them.
    let (root_q, path_q) = stale
        .strip_prefix("git -C ")
        .unwrap()
        .split_once(" worktree remove ")
        .unwrap();
    for (q, want) in [
        (root_q, repo.display().to_string()),
        (
            path_q,
            repo.join(".cadence/wt/feat gone").display().to_string(),
        ),
    ] {
        let out = Command::new("sh")
            .arg("-c")
            .arg(format!("set -- {q}; printf %s \"$1\""))
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), want, "{stale}");
    }
    // The shared row's rm -rf clears *contents*, one quoted glob
    // per shared subdir — run it through a real `sh` and prove
    // the dirs themselves (every lane's symlink target) survive.
    let rm = actions
        .iter()
        .find(|a| a.contains("rm -rf"))
        .expect("shared row")
        .clone();
    let d = shared.join("debug");
    for name in ["deps", ".fingerprint", "build", "incremental"] {
        std::fs::create_dir_all(d.join(name)).unwrap();
        std::fs::write(d.join(name).join("cached.o"), b"x").unwrap();
    }
    let rm_cmd = rm.split("  #").next().unwrap();
    let out = Command::new("sh").arg("-c").arg(rm_cmd).output().unwrap();
    assert!(out.status.success(), "{rm}");
    for name in ["deps", ".fingerprint", "build", "incremental"] {
        let dir = d.join(name);
        assert!(dir.is_dir(), "{name} must survive for lane symlinks");
        assert!(
            std::fs::read_dir(&dir).unwrap().next().is_none(),
            "{name} emptied"
        );
    }
    // And the quoted glob args word-split correctly: the first
    // arg expands inside the space-containing path.
    std::fs::write(d.join("deps/marker"), b"x").unwrap();
    let out = Command::new("sh")
        .arg("-c")
        .arg(format!(
            "set -- {}; printf %s \"$1\"",
            rm_cmd.strip_prefix("rm -rf ").unwrap()
        ))
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        d.join("deps/marker").display().to_string()
    );

    // The lock assertion is independent of stale-worktree quoting.
    // Retire that linked worktree before the held-lock scan so this
    // phase exercises only shared-cache lock handling and does not
    // launch unrelated git probes.
    git(
        &repo,
        &["worktree", "remove", "-f", ".cadence/wt/feat gone"],
    );

    // A held .cargo-lock swaps the rm -rf for an idle note — and
    // with no freeing command emitted, the row's bytes leave the
    // reclaimable total too.
    let lock = shared.join("debug/.cargo-lock");
    std::fs::create_dir_all(lock.parent().unwrap()).unwrap();
    std::fs::write(&lock, "").unwrap();
    std::fs::write(d.join("deps/cached2.o"), vec![7u8; 8192]).unwrap();
    let f = crate::worktree::TestFileLock::acquire(&lock);
    let plan = reclaim_plan(&scan);
    let shared_row = plan["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["kind"] == "shared-cargo-cache")
        .unwrap()
        .clone();
    assert!(shared_row["cargo_locked"].as_bool().unwrap());
    assert!(shared_row["action"]
        .as_str()
        .unwrap()
        .contains("cargo build"));
    assert!(shared_row["bytes"].as_u64().unwrap() >= 8192);
    let stale_bytes: u64 = plan["rows"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["kind"] == "stale-worktree")
        .map(|r| r["bytes"].as_u64().unwrap())
        .sum();
    assert_eq!(
        plan["reclaimable_bytes"].as_u64().unwrap(),
        stale_bytes,
        "locked shared row must not count toward the total"
    );
    f.release(); // probe must see the lock released
    assert!(!file_locked(&lock));
}

/// CAD-385: `pane-identity` warns on every recorded agent pid with
/// no recorded process start time — all of them on a store older
/// than v14 — and names the remedy; a row whose recorded start no
/// longer matches `/proc` is reported stale but maps nothing, so it
/// does not warn; no store is ok.
#[test]
fn pane_identity_names_rows_without_a_start_time_with_the_remedy() {
    let root = TempDir::new().unwrap();
    let scan = fake_scan(&root);
    assert_eq!(check_pane_identity(&scan).level, Level::Ok);

    let proc = scan.proc_root.clone();
    add_pid(&proc, 4101, None, None, None, 60, &[]);
    add_pid(&proc, 4102, None, None, None, 60, &[]);
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as i64;
    let start = (1_000_000 - 60) * hz;
    let conn = fake_registry(&scan.state_dir);
    let cwd = root.path().join("repo");
    add_agent(
        &conn,
        "pm",
        "pty",
        Some(4101),
        Some("g"),
        "idle",
        &cwd,
        now_epoch(),
    );
    add_agent(
        &conn,
        "w1",
        "managed",
        Some(4102),
        None,
        "idle",
        &cwd,
        now_epoch(),
    );
    add_agent(
        &conn,
        "gone",
        "pty",
        None,
        None,
        "offline",
        &cwd,
        now_epoch(),
    );

    // v13 shape: no `pid_start` column — every recorded pid warns.
    let c = check_pane_identity(&scan);
    assert_eq!(c.level, Level::Warn, "{}", c.detail);
    assert_eq!(
        c.value["no_start_time"],
        json!(["pm (pid 4101)", "w1 (pid 4102)"])
    );
    assert!(c.remedy.contains("cadence daemon restart"), "{}", c.remedy);
    assert!(c.remedy.contains("pane-identity"), "{}", c.remedy);

    conn.execute_batch("ALTER TABLE agents ADD COLUMN pid_start INTEGER")
        .unwrap();
    conn.execute("UPDATE agents SET pid_start=?1 WHERE alias='pm'", [start])
        .unwrap();
    conn.execute(
        "UPDATE agents SET pid_start=?1 WHERE alias='w1'",
        [start - 1],
    )
    .unwrap();
    let c = check_pane_identity(&scan);
    assert_eq!(c.level, Level::Ok, "{}", c.detail);
    assert_eq!(c.value["stale"], json!(["w1 (pid 4102)"]));
    assert!(c.detail.contains("stale"), "{}", c.detail);

    conn.execute("UPDATE agents SET pid_start=NULL WHERE alias='pm'", [])
        .unwrap();
    let c = check_pane_identity(&scan);
    assert_eq!(c.level, Level::Warn);
    assert_eq!(c.value["no_start_time"], json!(["pm (pid 4101)"]));
    assert!(render(&json!({"level": "warn", "checks": [c.to_json()]}))
        .contains("remedy: restart the daemon"));
}

#[test]
fn worktrees_skip_when_no_repo() {
    let root = TempDir::new().unwrap();
    let mut scan = fake_scan(&root);
    scan.cwd = root.path().join("nowhere");
    std::fs::create_dir_all(&scan.cwd).unwrap();
    let c = check_worktrees(&scan);
    assert_eq!(c.level, Level::Ok);
    assert!(c.value["skipped"].as_bool().unwrap());
}

// ---------- plumbing ----------

#[test]
fn exit_code_maps_worst_level() {
    assert_eq!(exit_code(&json!({"level": "ok"})), 0);
    assert_eq!(exit_code(&json!({"level": "warn"})), 1);
    assert_eq!(exit_code(&json!({"level": "fail"})), 2);
    assert_eq!(exit_code(&json!({})), 0);
}

#[test]
fn run_emits_all_checks() {
    let root = TempDir::new().unwrap();
    let mut scan = fake_scan(&root);
    scan.pm_dir = None; // nothing anywhere — cleanest possible host
    let report = run(&scan);
    let names: Vec<&str> = report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        vec![
            "disk",
            "provider-state",
            "pipes",
            "memory",
            "processes",
            "sessions",
            "pane-identity",
            "orphans",
            "temp-dirs",
            "task-targets",
            "worktrees",
            "load",
            "config",
            "tailnet",
            "agent-uid"
        ]
    );
    for c in report["checks"].as_array().unwrap() {
        for k in ["level", "value", "threshold", "detail", "remedy"] {
            assert!(c.get(k).is_some(), "check missing {k}");
        }
    }
    // `agent-uid` reads the live host (LiveHost by design — a
    // fixture cannot lie to it), so a runner whose system gitconfig
    // already arms §4's negative legitimately warns. The
    // clean-host exit applies to the checks driven by `scan`.
    let worst = report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["name"] != "agent-uid")
        .map(|c| c["level"].as_str().unwrap_or("ok"))
        .max_by_key(|l| match *l {
            "fail" => 2,
            "warn" => 1,
            _ => 0,
        })
        .unwrap_or("ok");
    assert_eq!(worst, "ok", "{}", render(&report));
    let task = report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "task-targets")
        .unwrap();
    assert_eq!(task["level"], "ok");
    assert_eq!(task["detail"], "none");
    assert_eq!(task["value"]["count"], 0);
    assert_eq!(task["value"]["safe_to_delete"], false);
    assert_eq!(task["remedy"], "");
}

#[test]
fn pm_yaml_host_table_overrides() {
    let root = TempDir::new().unwrap();
    let pm = root.path().join("pm");
    std::fs::create_dir_all(&pm).unwrap();
    assert!(host_overrides(&pm).is_none());
    std::fs::write(
            pm.join("pm.yaml"),
            "schema: 1\nhost:\n  wal_fail_bytes: 5\n  temp_warn_count: 2\n  mem_warn_pct: 25\n  wal_max_bytes: 4096\n  confine_pi_workers: true\n",
        )
        .unwrap();
    let o = host_overrides(&pm).unwrap();
    assert_eq!(o.wal_fail_bytes, Some(5));
    assert_eq!(o.temp_warn_count, Some(2));
    assert_eq!(o.mem_warn_pct, Some(25.0));
    assert_eq!(o.wal_max_bytes, Some(4096));
    assert_eq!(o.confine_pi_workers, Some(true));
    let t = Thresholds::resolve(Some(o));
    assert_eq!(t.wal_fail_bytes, 5);
    assert_eq!(t.temp_warn_count, 2);
    assert_eq!(t.mem_warn_pct, 25.0);
    assert_eq!(t.wal_max_bytes, 4096);
    assert_eq!(t.disk_warn_pct, 15.0); // untouched keys keep defaults
    assert_eq!(t.mem_fail_pct, 5.0);
    // A pm.yaml without the table is fine too.
    std::fs::write(pm.join("pm.yaml"), "schema: 1\n").unwrap();
    assert!(host_overrides(&pm).is_none());
    assert!(host_thresholds(Some(&pm)).config_error.is_none());
}

#[test]
fn pm_yaml_host_errors_fail_the_wal_watch_closed() {
    let root = TempDir::new().unwrap();
    let pm = root.path().join("pm");
    std::fs::create_dir_all(&pm).unwrap();
    // The explicit opt-out is honoured.
    std::fs::write(
        pm.join("pm.yaml"),
        "schema: 1\nhost:\n  wal_checkpoint: false\n",
    )
    .unwrap();
    let t = host_thresholds(Some(&pm));
    assert!(!t.wal_checkpoint && t.config_error.is_none());
    // A quoted bool or a misspelled key is refused, named, and turns
    // the writer off instead of silently reverting to defaults.
    for (body, needle) in [
        (
            "  wal_checkpoint: \"false\"\n  wal_max_bytes: 4096\n",
            "wal_checkpoint",
        ),
        ("  wal_max_byte: 4096\n", "wal_max_byte"),
    ] {
        std::fs::write(pm.join("pm.yaml"), format!("schema: 1\nhost:\n{body}")).unwrap();
        let t = host_thresholds(Some(&pm));
        let err = t.config_error.clone().unwrap_or_default();
        assert!(err.contains(needle), "{needle}: {err}");
        assert!(!t.wal_checkpoint, "{needle}: WAL watch must be off");
        assert_eq!(t.wal_max_bytes, GIB, "{needle}: defaults in force");
        let mut scan = fake_scan(&root);
        scan.thresholds = t;
        let c = check_config(&scan);
        assert_eq!(c.level, Level::Warn);
        assert!(c.detail.contains(needle), "{}", c.detail);
    }
}

// ---------- session census (CAD-198) ----------

#[test]
fn sessions_tree_counts_members_once_and_sums_pss_swap() {
    let root = TempDir::new().unwrap();
    let scan = fake_scan(&root);
    let repo = root.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    add_project(scan.pm_dir.as_deref().unwrap(), "cadence", &repo);
    let proc = scan.proc_root.clone();
    // devin root → npm wrapper → node server (the MCP stack).
    add_session_proc(
        &proc,
        100,
        1,
        "devin",
        Some(&repo),
        300_000,
        (Some(100), Some(50)),
    );
    add_session_proc(
        &proc,
        101,
        100,
        "npm",
        Some(&repo),
        300_000,
        (Some(20), Some(200)),
    );
    add_session_proc(
        &proc,
        102,
        101,
        "node",
        Some(&repo),
        300_000,
        (Some(30), Some(300)),
    );
    // A second, unrelated claude tree with its own child.
    add_session_proc(
        &proc,
        200,
        1,
        "claude",
        Some(&repo),
        100_000,
        (Some(10), Some(5)),
    );
    add_session_proc(
        &proc,
        201,
        200,
        "node",
        Some(&repo),
        90_000,
        (Some(4), Some(1)),
    );
    // A process outside any session.
    add_session_proc(
        &proc,
        300,
        1,
        "postgres",
        Some(&repo),
        10_000,
        (Some(1), Some(0)),
    );
    let v = sessions_value(&scan);
    let trees = v["trees"].as_array().unwrap();
    assert_eq!(trees.len(), 2, "{v}");
    let devin = trees
        .iter()
        .find(|t| t["root"]["family"] == "devin")
        .unwrap();
    assert_eq!(devin["procs"], 3);
    assert_eq!(devin["pss_bytes"], 150 * 1024);
    assert_eq!(devin["swap_bytes"], 550 * 1024);
    assert_eq!(devin["pss_missing_pids"], 0);
    // The wrapper stack is inside the tree total — never a
    // separate family sum and never double-counted.
    let pids: Vec<u64> = trees
        .iter()
        .flat_map(|t| {
            t["members"]
                .as_array()
                .unwrap()
                .iter()
                .map(|m| m["pid"].as_u64().unwrap())
                .collect::<Vec<_>>()
        })
        .collect();
    let mut dedup = pids.clone();
    dedup.sort_unstable();
    dedup.dedup();
    assert_eq!(pids.len(), dedup.len(), "a pid appears in two trees");
    assert!(pids.contains(&101) && pids.contains(&102));
    // pid+start identity is carried, never pid alone.
    assert_eq!(devin["root"]["pid"], 100);
    assert!(devin["root"]["start_jiffies"].as_u64().unwrap() > 0);
}

#[test]
fn sessions_owned_via_managed_root_and_pty_ancestor() {
    let root = TempDir::new().unwrap();
    let scan = fake_scan(&root);
    let repo = root.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    add_project(scan.pm_dir.as_deref().unwrap(), "cadence", &repo);
    let proc = scan.proc_root.clone();
    // Managed: agents.pid IS the provider root.
    add_session_proc(
        &proc,
        200,
        1,
        "claude",
        Some(&repo),
        5_000,
        (Some(10), Some(1)),
    );
    // Pty: agents.pid is the pane shell (bash) above the devin root.
    add_session_proc(
        &proc,
        50,
        1,
        "bash",
        Some(&repo),
        400_000,
        (Some(1), Some(0)),
    );
    add_session_proc(
        &proc,
        100,
        50,
        "devin",
        Some(&repo),
        400_000,
        (Some(9), Some(2)),
    );
    let conn = fake_registry(&scan.state_dir);
    add_agent(
        &conn,
        "qa-1",
        "managed",
        Some(200),
        Some("g1"),
        "idle",
        &repo,
        now_epoch(),
    );
    add_agent(
        &conn,
        "devin-d",
        "pty",
        Some(50),
        Some("g2"),
        "idle",
        &repo,
        now_epoch(),
    );
    drop(conn);
    let v = sessions_value(&scan);
    let trees = v["trees"].as_array().unwrap();
    let by_alias = |a: &str| {
        trees
            .iter()
            .find(|t| t["alias"] == a)
            .unwrap_or_else(|| panic!("no tree owned by {a}: {v}"))
            .clone()
    };
    let managed = by_alias("qa-1");
    assert_eq!(managed["agreement"], "agreed");
    assert_eq!(managed["endpoint_kind"], "managed");
    assert_eq!(managed["generation"], "g1");
    assert_eq!(managed["reclaim"]["candidate"], false);
    assert_eq!(managed["reclaim"]["protected"], "owned — registry row qa-1");
    let pty = by_alias("devin-d");
    assert_eq!(pty["agreement"], "agreed");
    assert_eq!(pty["endpoint_pid"], 50);
    assert_eq!(pty["root"]["pid"], 100);
}

#[test]
fn sessions_unowned_in_scope_is_a_dry_run_candidate() {
    let root = TempDir::new().unwrap();
    let scan = fake_scan(&root);
    let repo = root.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    add_project(scan.pm_dir.as_deref().unwrap(), "cadence", &repo);
    let proc = scan.proc_root.clone();
    // No registry at all — provably zero rows → ProcessOnly stands.
    add_session_proc(
        &proc,
        700,
        1,
        "devin",
        Some(&repo),
        300_000,
        (Some(100), Some(80)),
    );
    add_session_proc(
        &proc,
        701,
        700,
        "node",
        Some(&repo),
        300_000,
        (Some(20), Some(40)),
    );
    let v = sessions_value(&scan);
    assert_eq!(v["store"], "absent");
    let tree = &v["trees"][0];
    assert_eq!(tree["agreement"], "process-only");
    assert_eq!(tree["scope"], "project:cadence");
    let r = &tree["reclaim"];
    assert_eq!(r["candidate"], true);
    // 300_000s ≈ 83h → high confidence.
    assert_eq!(r["confidence"], "high");
    assert_eq!(r["pss_bytes"], 120 * 1024);
    assert_eq!(r["swap_bytes"], 120 * 1024);
    assert_eq!(v["totals"]["candidates"], 1);
    // Dry-run only — the check carries no executable action.
    let report = run(&scan);
    let c = report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "sessions")
        .unwrap();
    // Candidates are routine (a human ran an agent in a repo) and
    // stay ok — only evidence failure (unreadable store) warns.
    // The authorisation caveat travels in `detail`, since render()
    // never prints a remedy for an ok check.
    assert_eq!(c["level"], "ok");
    assert!(c["detail"]
        .as_str()
        .unwrap()
        .contains("separately authorised phase"));
    assert!(!c["remedy"].as_str().unwrap().contains("kill"));
    assert!(c["detail"].as_str().unwrap().contains("700("));
}

#[test]
fn sessions_foreign_and_unproven_scopes_protected() {
    let root = TempDir::new().unwrap();
    let scan = fake_scan(&root);
    let repo = root.path().join("repo");
    let foreign = root.path().join("elsewhere");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&foreign).unwrap();
    add_project(scan.pm_dir.as_deref().unwrap(), "cadence", &repo);
    let proc = scan.proc_root.clone();
    // Foreign: cwd outside every registered project.
    add_session_proc(
        &proc,
        500,
        1,
        "devin",
        Some(&foreign),
        300_000,
        (Some(1), Some(1)),
    );
    // Unproven: no cwd link at all.
    add_session_proc(&proc, 600, 1, "claude", None, 300_000, (Some(1), Some(1)));
    let v = sessions_value(&scan);
    let trees = v["trees"].as_array().unwrap();
    let f = trees.iter().find(|t| t["root"]["pid"] == 500).unwrap();
    assert_eq!(f["scope"], "foreign");
    assert_eq!(f["reclaim"]["candidate"], false);
    assert!(f["reclaim"]["protected"]
        .as_str()
        .unwrap()
        .contains("foreign"));
    let u = trees.iter().find(|t| t["root"]["pid"] == 600).unwrap();
    assert_eq!(u["scope"], "unproven");
    assert_eq!(u["reclaim"]["candidate"], false);
    assert_eq!(v["totals"]["candidates"], 0);
}

#[test]
fn sessions_unreadable_store_never_proves_unowned() {
    let root = TempDir::new().unwrap();
    let scan = fake_scan(&root);
    let repo = root.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    add_project(scan.pm_dir.as_deref().unwrap(), "cadence", &repo);
    std::fs::create_dir_all(&scan.state_dir).unwrap();
    // A store file that is not sqlite — present but unreadable.
    std::fs::write(scan.state_dir.join("cadence.sqlite3"), b"not a database").unwrap();
    let proc = scan.proc_root.clone();
    add_session_proc(
        &proc,
        700,
        1,
        "devin",
        Some(&repo),
        300_000,
        (Some(1), Some(1)),
    );
    let v = sessions_value(&scan);
    assert_eq!(v["store"], "unreadable");
    let tree = &v["trees"][0];
    // Absence is unproven → Unknown → protected, never a candidate.
    assert_eq!(tree["agreement"], "unknown");
    assert_eq!(tree["reclaim"]["candidate"], false);
    assert_eq!(v["totals"]["candidates"], 0);
}

#[test]
fn sessions_generation_mismatch_is_fenced() {
    let root = TempDir::new().unwrap();
    let scan = fake_scan(&root);
    let repo = root.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    add_project(scan.pm_dir.as_deref().unwrap(), "cadence", &repo);
    let proc = scan.proc_root.clone();
    add_session_proc(
        &proc,
        50,
        1,
        "bash",
        Some(&repo),
        400_000,
        (Some(1), Some(0)),
    );
    add_session_proc(
        &proc,
        100,
        50,
        "devin",
        Some(&repo),
        400_000,
        (Some(9), Some(2)),
    );
    let conn = fake_registry(&scan.state_dir);
    let gen_old = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let gen_live = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    add_agent(
        &conn,
        "devin-d",
        "pty",
        Some(50),
        Some(gen_old),
        "busy",
        &repo,
        now_epoch(),
    );
    // A live turn token minted under a DIFFERENT generation —
    // the endpoint moved under the registry row.
    add_message(
        &conn,
        "devin-d",
        "running",
        Some(&format!("pty-{gen_live}-{}", "c".repeat(32))),
        None,
    );
    drop(conn);
    let v = sessions_value(&scan);
    let tree = &v["trees"][0];
    assert_eq!(tree["agreement"], "generation-mismatch");
    assert_eq!(tree["reclaim"]["candidate"], false);
    assert!(tree["reclaim"]["protected"]
        .as_str()
        .unwrap()
        .contains("generation"));
}

#[test]
fn sessions_pending_and_progress_surface() {
    let root = TempDir::new().unwrap();
    let scan = fake_scan(&root);
    let repo = root.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    add_project(scan.pm_dir.as_deref().unwrap(), "cadence", &repo);
    let proc = scan.proc_root.clone();
    add_session_proc(
        &proc,
        200,
        1,
        "claude",
        Some(&repo),
        5_000,
        (Some(10), Some(1)),
    );
    let conn = fake_registry(&scan.state_dir);
    let gen = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let now = now_epoch();
    // updated is recent enough for a live claim (proc started
    // ~83min ago); the message timestamp is newer still, so it
    // must win last_progress.
    add_agent(
        &conn,
        "qa-1",
        "managed",
        Some(200),
        Some(gen),
        "busy",
        &repo,
        now - 300.0,
    );
    add_message(&conn, "qa-1", "queued", None, None);
    add_message(&conn, "qa-1", "queued", None, None);
    add_message(
        &conn,
        "qa-1",
        "running",
        Some(&format!("claude-{gen}-{}", "d".repeat(32))),
        None,
    );
    add_message(&conn, "qa-1", "done", None, Some(now + 60.0));
    drop(conn);
    let v = sessions_value(&scan);
    let tree = &v["trees"][0];
    assert_eq!(tree["alias"], "qa-1");
    assert_eq!(tree["state"], "busy");
    assert_eq!(tree["pending"]["queued"], 2);
    assert_eq!(tree["pending"]["running"], 1);
    assert_eq!(tree["last_progress"], now + 60.0);
    // Running token's generation matches the row — still agreed.
    assert_eq!(tree["agreement"], "agreed");
}

#[test]
fn sessions_never_disclose_argv_or_env() {
    let root = TempDir::new().unwrap();
    let scan = fake_scan(&root);
    let repo = root.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    add_project(scan.pm_dir.as_deref().unwrap(), "cadence", &repo);
    let proc = scan.proc_root.clone();
    let dir = add_session_proc(
        &proc,
        700,
        1,
        "devin",
        Some(&repo),
        300_000,
        (Some(1), Some(1)),
    );
    // A bearer secret in argv and env — the census must not read
    // either file, let alone print them.
    std::fs::write(dir.join("cmdline"), "devin\0--token\0S3CR3T-BEARER").unwrap();
    std::fs::write(dir.join("environ"), "GH_TOKEN=S3CR3T-BEARER\0HOME=/u").unwrap();
    let v = sessions_value(&scan);
    let text = serde_json::to_string(&v).unwrap();
    assert!(!text.contains("S3CR3T"), "{text}");
    assert!(!text.contains("cmdline") && !text.contains("environ"));
}

#[test]
fn sessions_partial_metrics_degrade_confidence() {
    let root = TempDir::new().unwrap();
    let scan = fake_scan(&root);
    let repo = root.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    add_project(scan.pm_dir.as_deref().unwrap(), "cadence", &repo);
    let proc = scan.proc_root.clone();
    add_session_proc(
        &proc,
        700,
        1,
        "devin",
        Some(&repo),
        300_000,
        (Some(100), Some(80)),
    );
    // Child whose metric files are absent → partial accounting.
    add_session_proc(&proc, 701, 700, "node", Some(&repo), 300_000, (None, None));
    let v = sessions_value(&scan);
    let tree = &v["trees"][0];
    assert_eq!(tree["pss_missing_pids"], 1);
    assert_eq!(tree["swap_missing_pids"], 1);
    // 83h would be high, but partial accounting caps at medium.
    assert_eq!(tree["reclaim"]["confidence"], "medium");
    assert_eq!(tree["reclaim"]["candidate"], true);
}

#[test]
fn sessions_foreign_uid_never_candidate() {
    let root = TempDir::new().unwrap();
    let scan = fake_scan(&root);
    let repo = root.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    add_project(scan.pm_dir.as_deref().unwrap(), "cadence", &repo);
    let proc = scan.proc_root.clone();
    // Unowned, in-scope, old — every candidacy trait except uid:
    // the root belongs to another user.
    let dir = add_session_proc(
        &proc,
        700,
        1,
        "devin",
        Some(&repo),
        300_000,
        (Some(100), Some(80)),
    );
    let euid = unsafe { libc::geteuid() };
    let foreign = euid + 1;
    std::fs::write(
        dir.join("status"),
        format!("Name:\tdevin\nUid:\t{foreign}\t{foreign}\t{foreign}\t{foreign}\n"),
    )
    .unwrap();
    // No registry — "unowned" is a proven fact; uid still fences.
    let v = sessions_value(&scan);
    let tree = &v["trees"][0];
    assert_eq!(tree["agreement"], "process-only");
    assert_eq!(tree["scope"], "project:cadence");
    assert_eq!(tree["root"]["uid"], foreign);
    assert_eq!(tree["reclaim"]["candidate"], false);
    assert!(tree["reclaim"]["protected"]
        .as_str()
        .unwrap()
        .contains(&format!("uid {foreign}")));
    assert_eq!(v["totals"]["foreign_uid"], 1);
    assert_eq!(v["totals"]["candidates"], 0);
}

#[test]
fn sessions_uid_unreadable_is_protected() {
    let root = TempDir::new().unwrap();
    let scan = fake_scan(&root);
    let repo = root.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    add_project(scan.pm_dir.as_deref().unwrap(), "cadence", &repo);
    let proc = scan.proc_root.clone();
    // No status file at all → uid unproven → never a candidate.
    add_session_proc(&proc, 700, 1, "devin", Some(&repo), 300_000, (None, None));
    let v = sessions_value(&scan);
    let tree = &v["trees"][0];
    assert_eq!(tree["root"]["uid"], Value::Null);
    assert_eq!(tree["reclaim"]["candidate"], false);
    assert!(tree["reclaim"]["protected"]
        .as_str()
        .unwrap()
        .contains("uid unreadable"));
}

#[test]
fn sessions_scope_rejects_empty_relative_and_root_paths() {
    let root = TempDir::new().unwrap();
    let scan = fake_scan(&root);
    let repo = root.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    // A project whose every repo path is hostile to prefix
    // matching: empty, relative, `/`, and $HOME itself.
    let pm = scan.pm_dir.as_deref().unwrap();
    let dir = pm.join("bad");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
            dir.join("project.yaml"),
            format!(
                "key: bad\nprefix: bad-\nrepos:\n  - path: \"\"\n  - path: relative\n  - path: /\n  - path: {}\n",
                scan.home.display()
            ),
        )
        .unwrap();
    let proc = scan.proc_root.clone();
    add_session_proc(
        &proc,
        700,
        1,
        "devin",
        Some(&repo),
        300_000,
        (Some(1), Some(1)),
    );
    let v = sessions_value(&scan);
    let tree = &v["trees"][0];
    // Only the pm dir itself survives validation → the repo cwd
    // is outside every proven root → foreign, never a candidate.
    assert_eq!(tree["scope"], "foreign", "{v}");
    assert_eq!(tree["reclaim"]["candidate"], false);
    assert_eq!(v["totals"]["candidates"], 0);
}

#[test]
fn sessions_stale_endpoint_pid_row_fences_tree() {
    let root = TempDir::new().unwrap();
    let scan = fake_scan(&root);
    let repo = root.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    add_project(scan.pm_dir.as_deref().unwrap(), "cadence", &repo);
    let proc = scan.proc_root.clone();
    // A live unowned tree under the repo…
    add_session_proc(
        &proc,
        700,
        1,
        "devin",
        Some(&repo),
        300_000,
        (Some(1), Some(1)),
    );
    // …and a registry row whose endpoint pid is DEAD while its
    // cwd still covers the tree — the pane-respawn shape. The
    // tree may be that agent's session → Unknown, never
    // process-only.
    let conn = fake_registry(&scan.state_dir);
    add_agent(
        &conn,
        "ghost-1",
        "pty",
        Some(9999),
        Some("g1"),
        "idle",
        &repo,
        now_epoch() - 500_000.0,
    );
    drop(conn);
    let v = sessions_value(&scan);
    let tree = &v["trees"][0];
    assert_eq!(tree["agreement"], "unknown", "{v}");
    assert!(tree["agreement_why"].as_str().unwrap().contains("ghost-1"));
    assert_eq!(tree["reclaim"]["candidate"], false);
    // The row itself still shows under records_only.
    let rec = &v["records_only"][0];
    assert_eq!(rec["alias"], "ghost-1");
    assert_eq!(rec["endpoint_state"], "dead");
}

#[test]
fn sessions_recycled_endpoint_pid_is_unknown_not_agreed() {
    let root = TempDir::new().unwrap();
    let scan = fake_scan(&root);
    let repo = root.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    add_project(scan.pm_dir.as_deref().unwrap(), "cadence", &repo);
    let proc = scan.proc_root.clone();
    // Live devin root — and a registry row naming its pid, but
    // the row's last write predates this process's start by days:
    // the pid was recycled; it cannot be the recorded endpoint.
    add_session_proc(
        &proc,
        700,
        1,
        "devin",
        Some(&repo),
        5_000,
        (Some(1), Some(1)),
    );
    let conn = fake_registry(&scan.state_dir);
    add_agent(
        &conn,
        "qa-1",
        "managed",
        Some(700),
        Some("g1"),
        "idle",
        &repo,
        now_epoch() - 1_000_000.0,
    );
    drop(conn);
    let v = sessions_value(&scan);
    let tree = &v["trees"][0];
    assert_eq!(tree["agreement"], "unknown", "{v}");
    assert!(tree["agreement_why"].as_str().unwrap().contains("reused"));
    assert_eq!(tree["reclaim"]["candidate"], false);
}

#[test]
fn sessions_ambiguous_claim_is_unknown() {
    let root = TempDir::new().unwrap();
    let scan = fake_scan(&root);
    let repo = root.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    add_project(scan.pm_dir.as_deref().unwrap(), "cadence", &repo);
    let proc = scan.proc_root.clone();
    add_session_proc(
        &proc,
        700,
        1,
        "devin",
        Some(&repo),
        5_000,
        (Some(1), Some(1)),
    );
    let conn = fake_registry(&scan.state_dir);
    for alias in ["a-1", "a-2"] {
        add_agent(
            &conn,
            alias,
            "managed",
            Some(700),
            Some("g1"),
            "idle",
            &repo,
            now_epoch(),
        );
    }
    drop(conn);
    let v = sessions_value(&scan);
    let tree = &v["trees"][0];
    assert_eq!(tree["agreement"], "unknown", "{v}");
    assert!(tree["agreement_why"]
        .as_str()
        .unwrap()
        .contains("ambiguous"));
    assert_eq!(tree["reclaim"]["candidate"], false);
}

#[test]
fn sessions_deleted_cwd_is_unproven_not_in_scope() {
    let root = TempDir::new().unwrap();
    let scan = fake_scan(&root);
    let repo = root.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    add_project(scan.pm_dir.as_deref().unwrap(), "cadence", &repo);
    let proc = scan.proc_root.clone();
    let dir = add_session_proc(&proc, 700, 1, "devin", None, 300_000, (Some(1), Some(1)));
    // The kernel's deleted-marker form: read_link yields the old
    // path plus the literal suffix — must not string-match into
    // scope.
    std::os::unix::fs::symlink(format!("{} (deleted)", repo.display()), dir.join("cwd")).unwrap();
    let v = sessions_value(&scan);
    let tree = &v["trees"][0];
    assert_eq!(tree["root"]["cwd_deleted"], true);
    assert_eq!(tree["scope"], "unproven", "{v}");
    assert_eq!(tree["reclaim"]["candidate"], false);
}

#[test]
fn sessions_foreign_mount_namespace_is_unproven() {
    let root = TempDir::new().unwrap();
    let scan = fake_scan(&root);
    let repo = root.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    add_project(scan.pm_dir.as_deref().unwrap(), "cadence", &repo);
    let proc = scan.proc_root.clone();
    // Our own mnt-ns anchor: proc/self → a fake doctor pid dir.
    let selfdir = proc.join("900");
    std::fs::create_dir_all(selfdir.join("ns")).unwrap();
    std::os::unix::fs::symlink("mnt:[1111]", selfdir.join("ns/mnt")).unwrap();
    std::os::unix::fs::symlink(&selfdir, proc.join("self")).unwrap();
    // In-scope cwd but a different mnt ns → not comparable.
    let a = add_session_proc(
        &proc,
        700,
        1,
        "devin",
        Some(&repo),
        300_000,
        (Some(1), Some(1)),
    );
    std::fs::create_dir_all(a.join("ns")).unwrap();
    std::os::unix::fs::symlink("mnt:[2222]", a.join("ns/mnt")).unwrap();
    // Same-ns control → normal classification.
    let b = add_session_proc(
        &proc,
        600,
        1,
        "claude",
        Some(&repo),
        300_000,
        (Some(1), Some(1)),
    );
    std::fs::create_dir_all(b.join("ns")).unwrap();
    std::os::unix::fs::symlink("mnt:[1111]", b.join("ns/mnt")).unwrap();
    let v = sessions_value(&scan);
    let trees = v["trees"].as_array().unwrap();
    let foreign_ns = trees.iter().find(|t| t["root"]["pid"] == 700).unwrap();
    assert_eq!(foreign_ns["root"]["ns_foreign"], true);
    assert_eq!(foreign_ns["scope"], "unproven", "{v}");
    assert_eq!(foreign_ns["reclaim"]["candidate"], false);
    let same_ns = trees.iter().find(|t| t["root"]["pid"] == 600).unwrap();
    assert_eq!(same_ns["root"]["ns_foreign"], false);
    assert_eq!(same_ns["scope"], "project:cadence");
}

#[test]
fn sessions_ppid_cycle_terminates() {
    let root = TempDir::new().unwrap();
    let scan = fake_scan(&root);
    let proc = scan.proc_root.clone();
    // A ppid cycle among session comms — each has a session
    // ancestor, so neither is a root and the walk must end.
    add_session_proc(&proc, 800, 801, "devin", None, 5_000, (Some(1), Some(0)));
    add_session_proc(&proc, 801, 800, "claude", None, 5_000, (Some(1), Some(0)));
    let v = sessions_value(&scan);
    assert_eq!(v["trees"].as_array().unwrap().len(), 0, "{v}");
}

#[test]
fn sessions_metrics_cap_marks_truncated() {
    let root = TempDir::new().unwrap();
    let scan = fake_scan(&root);
    let repo = root.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    add_project(scan.pm_dir.as_deref().unwrap(), "cadence", &repo);
    let proc = scan.proc_root.clone();
    add_session_proc(
        &proc,
        700,
        1,
        "devin",
        Some(&repo),
        300_000,
        (Some(1), Some(1)),
    );
    for pid in 800..(800 + MAX_METRIC_PIDS as u32 + 3) {
        add_session_proc(
            &proc,
            pid,
            700,
            "node",
            Some(&repo),
            300_000,
            (Some(1), Some(0)),
        );
    }
    let v = sessions_value(&scan);
    let tree = &v["trees"][0];
    assert_eq!(tree["metrics_truncated"], true);
    // Members list stays complete — only the metric pass is capped.
    assert_eq!(tree["procs"], MAX_METRIC_PIDS + 4);
    assert_eq!(tree["reclaim"]["confidence"], "medium");
    // A truncated tree in the candidate list means the aggregate
    // is an estimate, not an upper bound — the flag must flip.
    assert_eq!(tree["reclaim"]["candidate"], true);
    assert_eq!(v["totals"]["candidate_sums_upper_bound"], false);
}

#[test]
fn sessions_unreadable_store_warns_and_exits_nonzero() {
    let root = TempDir::new().unwrap();
    let scan = fake_scan(&root);
    std::fs::create_dir_all(&scan.state_dir).unwrap();
    std::fs::write(scan.state_dir.join("cadence.sqlite3"), b"not a database").unwrap();
    let report = run(&scan);
    let c = report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "sessions")
        .unwrap();
    // Evidence failure must never exit 0 — a watchdog loop
    // keying on the exit code learns the store could not be read.
    assert_ne!(c["level"], "ok", "{c}");
    assert!(exit_code(&report) >= 1);
}

#[test]
fn sessions_candidate_caveat_is_visible() {
    let root = TempDir::new().unwrap();
    let scan = fake_scan(&root);
    let repo = root.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    add_project(scan.pm_dir.as_deref().unwrap(), "cadence", &repo);
    let proc = scan.proc_root.clone();
    add_session_proc(
        &proc,
        700,
        1,
        "devin",
        Some(&repo),
        300_000,
        (Some(100), Some(80)),
    );
    // A *readable* registry (zero rows) — the candidate check still
    // reports ok, pinning the warn-only-on-evidence-failure rule so
    // nobody "fixes" it back.
    drop(fake_registry(&scan.state_dir));
    let report = run(&scan);
    let c = report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "sessions")
        .unwrap();
    assert_eq!(c["level"], "ok", "{c}");
    // The caveat must reach the text render — the `remedy` field
    // holding it is not the point; the printed line is.
    let text = render(&report);
    assert!(
        text.contains("separately authorised phase"),
        "caveat missing from render:\n{text}"
    );
}

#[test]
fn pm_yaml_host_slot_keys_parse() {
    let root = TempDir::new().unwrap();
    let pm = root.path().join("pm");
    std::fs::create_dir_all(&pm).unwrap();
    std::fs::write(
        pm.join("pm.yaml"),
        "schema: 1\nhost:\n  build_slots: 5\n  suite_slots: 2\n  \
             jobs_per_lane: 8\n  starve_secs: 300\n  \
             priority_lanes: [qa-1, qa-2]\n  load_warn_ratio: 1.5\n  \
             io_stall_fail_pct: 45\n",
    )
    .unwrap();
    let o = host_overrides(&pm).unwrap();
    assert_eq!(o.build_slots, Some(5));
    assert_eq!(o.suite_slots, Some(2));
    assert_eq!(o.jobs_per_lane, Some(8));
    assert_eq!(o.starve_secs, Some(300));
    assert_eq!(o.priority_lanes.as_deref().unwrap().len(), 2);
    assert_eq!(o.load_warn_ratio, Some(1.5));
    // …and it resolves through to the threshold.
    let t = Thresholds::resolve(Some(o));
    assert_eq!(t.load_warn_ratio, Some(1.5));
    assert_eq!(t.io_stall_fail_pct, 45.0);
}

// ---------- load (CAD-113) ----------

/// Fabricate `loadavg` + `pressure/io` under the scan's proc root.
fn proc_load(scan: &Scan, load1: f64, io_avg10: Option<f64>) {
    std::fs::create_dir_all(scan.proc_root.join("pressure")).unwrap();
    std::fs::write(
        scan.proc_root.join("loadavg"),
        format!("{load1} 1.00 1.00 1/100 999\n"),
    )
    .unwrap();
    let io = match io_avg10 {
        Some(v) => format!("some avg10={v} avg60=0.00 avg300=0.00 total=1\n"),
        None => String::new(),
    };
    std::fs::write(scan.proc_root.join("pressure/io"), io).unwrap();
}

#[test]
fn load_check_levels_and_slot_detail() {
    let root = TempDir::new().unwrap();
    let mut scan = fake_scan(&root);
    let cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1) as f64;
    // Calm host: level ok, detail still names the slot queue.
    scan.slots = Some(json!({
        "pools": {
            "build": {"capacity": 3, "held": [{"a": 1}, {"a": 2}]},
            "suite": {"capacity": 1, "held": []},
        },
        "waiting": [{"wait_secs": 252.0}],
    }));
    proc_load(&scan, 0.5, Some(2.0));
    let c = check_load(&scan);
    assert_eq!(c.level, Level::Ok, "{}", c.detail);
    assert!(
        c.detail.contains("slots 2/3 build 0/1 suite"),
        "{}",
        c.detail
    );
    assert!(c.detail.contains("longest 4m12s"), "{}", c.detail);
    // An explicit warn ratio pins the bands regardless of cpus.
    scan.thresholds.load_warn_ratio = Some(1.0);
    // Warn band: load1 above cpus but under 2x.
    proc_load(&scan, cpus * 1.5, Some(10.0));
    let c = check_load(&scan);
    assert_eq!(c.level, Level::Warn, "{}", c.detail);
    assert!(c.remedy.contains("build-slot status"));
    // Fail band: io stall alone can carry it.
    proc_load(&scan, 0.5, Some(70.0));
    let c = check_load(&scan);
    assert_eq!(c.level, Level::Fail, "{}", c.detail);
    // Load alone over 2x also fails.
    proc_load(&scan, cpus * 2.5, Some(0.0));
    let c = check_load(&scan);
    assert_eq!(c.level, Level::Fail, "{}", c.detail);
    // Unset, the warn line derives from the slot plan — the
    // farm's own (3+1)×4 jobs on this box: warn only above it.
    scan.thresholds.load_warn_ratio = None;
    let derived = ((3.0 + 1.0) * 4.0 * 1.25 / cpus).max(1.0);
    // The fixture's slot config (3/1/4) equals the defaults, so
    // the derived ratio matches either way; below it → ok.
    proc_load(&scan, cpus * derived * 0.9, Some(2.0));
    let c = check_load(&scan);
    assert_eq!(c.level, Level::Ok, "below plan: {}", c.detail);
    proc_load(&scan, cpus * derived * 1.5, Some(2.0));
    let c = check_load(&scan);
    assert_eq!(c.level, Level::Warn, "above plan: {}", c.detail);
    // Unreachable daemon reports, never penalises.
    scan.slots = None;
    proc_load(&scan, 0.5, Some(2.0));
    let c = check_load(&scan);
    assert_eq!(c.level, Level::Ok);
    assert!(c.detail.contains("daemon unreachable"), "{}", c.detail);
    // Neither file exists → skipped ok.
    let root = TempDir::new().unwrap();
    let scan = fake_scan(&root);
    let c = check_load(&scan);
    assert_eq!(c.level, Level::Ok);
    assert!(c.value["skipped"].as_bool().unwrap_or(false));
}

/// CAD-257: a kill remedy names each pid's comm, cwd and age, read
/// from the real `/proc` — a live child `sleep` is the witness; a
/// pid that has exited is omitted rather than printed bare.
#[test]
fn kill_lines_name_comm_cwd_and_age_from_proc() {
    let dir = TempDir::new().unwrap();
    let mut child = Command::new("sleep")
        .arg("30")
        .current_dir(dir.path())
        .spawn()
        .unwrap();
    let pid = child.id();
    // `spawn` can return before exec renames the task: the vfork
    // hand-back (mm_release) precedes `__set_task_comm` in the
    // kernel, so comm may still read as this test binary for a
    // moment. Wait, bounded, for the exec to finish.
    let comm = format!("/proc/{pid}/comm");
    for _ in 0..500 {
        if std::fs::read_to_string(&comm).is_ok_and(|c| c.trim() == "sleep") {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let lines = kill_lines(Path::new("/proc"), &[pid]);
    let remedy = kill_remedy(Path::new("/proc"), &[pid], "why");
    let _ = child.kill();
    let _ = child.wait();
    let cwd = dir.path().canonicalize().unwrap();
    assert_eq!(lines.len(), 1, "{lines:?}");
    let line = &lines[0];
    assert!(
        line.starts_with(&format!("kill {pid}  # sleep  cwd={}  age=", cwd.display())),
        "{line}"
    );
    // Just spawned: seconds old, never an unknown `?`.
    let age = line.rsplit_once("age=").unwrap().1;
    assert!(
        age.ends_with('s') && age[..age.len() - 1].parse::<u64>().is_ok(),
        "{line}"
    );
    assert_eq!(remedy, format!("why:\n{line}"));
    // Reaped → gone from /proc → omitted; nothing left → no remedy.
    assert!(kill_lines(Path::new("/proc"), &[pid]).is_empty());
    assert_eq!(kill_remedy(Path::new("/proc"), &[pid], "why"), "");
}

/// This test's user name — the uid the fixture socket is owned by.
fn own_name() -> String {
    let out = Command::new("id").arg("-un").output().unwrap();
    assert!(out.status.success());
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// CAD-509 — no tailscaled and no sharing is a fact, not a
/// failure: a host that never touches the tailnet stays `ok`.
#[test]
fn tailnet_check_is_quiet_without_tailscale() {
    let root = TempDir::new().unwrap();
    let scan = fake_scan(&root); // tailscaled_socket points nowhere
    let c = check_tailnet(&scan).to_json();
    assert_eq!(c["level"], "ok", "{c}");
    assert_eq!(c["value"]["refusals"], json!(["tailscaled_socket"]));
}

/// CAD-509 — a missing `OperatorUser` (tailscaled's omitempty) is
/// "no operator": never `not_operator_user`. A non-object prefs
/// body stays a LocalAPI refusal — fail closed, never a silent
/// pass.
#[test]
fn tailnet_check_reads_a_missing_operator_user_as_none() {
    let root = TempDir::new().unwrap();
    let mut scan = fake_scan(&root); // sharing off → advisory only
    let tsdir = root.path().join("ts");
    let sock = crate::tailnet_proof::tests::localapi(&tsdir);
    for (f, v) in [
        ("status.json", json!({"TUN": true})),
        ("prefs.json", json!({"WantRunning": true})),
        ("serve.json", json!({})),
    ] {
        crate::tailnet_proof::tests::localapi_says(&tsdir, f, v);
    }
    scan.tailscaled_socket = Some(sock);
    let c = check_tailnet(&scan).to_json();
    // The fixture socket is owned by this uid, so `foreign_uid`
    // stays — `not_operator_user` must not.
    assert_eq!(c["value"]["refusals"], json!(["foreign_uid"]), "{c}");
    assert_eq!(c["level"], "warn", "{c}");

    crate::tailnet_proof::tests::localapi_says(&tsdir, "prefs.json", json!(["x"]));
    let c = check_tailnet(&scan).to_json();
    assert_eq!(c["value"]["refusals"], json!(["localapi"]), "{c}");
}

/// CAD-509 — with sharing on, every host-side refusal prints its
/// remedy in proof order, then the board start and the link: one
/// pass, one spent link.
#[test]
fn tailnet_check_prints_the_whole_remedy_chain() {
    let root = TempDir::new().unwrap();
    let mut scan = fake_scan(&root);
    // Sharing on, board down: the probe port is bound then dropped
    // so nothing answers on it.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    std::fs::write(
        scan.state_dir.join("ui.json"),
        json!({
            "port": port,
            "tailscale": {
                "dns_name": "box.tailnet.example",
                "https_port": 9450,
                "target": format!("http://127.0.0.1:{port}"),
            },
        })
        .to_string(),
    )
    .unwrap();
    // TUN on, this uid as the operator, a forwarder to the board.
    let tsdir = root.path().join("ts");
    let sock = crate::tailnet_proof::tests::localapi(&tsdir);
    for (f, v) in [
        ("status.json", json!({"TUN": true})),
        ("prefs.json", json!({"OperatorUser": own_name()})),
        (
            "serve.json",
            json!({"TCP": {"443": {"TCPForward": format!("127.0.0.1:{port}")}}}),
        ),
    ] {
        crate::tailnet_proof::tests::localapi_says(&tsdir, f, v);
    }
    scan.tailscaled_socket = Some(sock);
    let c = check_tailnet(&scan).to_json();
    assert_eq!(c["level"], "fail", "{c}");
    assert_eq!(
        c["value"]["refusals"],
        json!(["not_operator_user", "no_tcp_forwarder", "foreign_uid"]),
        "{c}"
    );
    let remedy = c["remedy"].as_str().unwrap();
    let at = |s: &str| {
        remedy
            .find(s)
            .unwrap_or_else(|| panic!("{s} not in {remedy}"))
    };
    let (op, fwd, foreign) = (
        at("--operator=root"),
        at("TCP forwarder"),
        at("its own user"),
    );
    let (start, link) = (at("cadence ui start"), at("cadence ui login --tailnet"));
    assert!(
        op < fwd && fwd < foreign && foreign < start && start < link,
        "{remedy}"
    );
}

use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
