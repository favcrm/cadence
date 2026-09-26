//! CAD-536: `cadence doctor host` check `task_targets` — moved verbatim from src/doctor/host.rs.

use super::*;

use std::collections::BTreeMap;
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::OpenOptionsExt;

/// How many temp-dir entries the legacy task-target scan will look at
/// before it stops and reports the inventory as truncated.
const TASK_TARGET_TEMP_BUDGET: usize = 8_192;

/// Rows kept after that scan. The host's leaked `cad*-target*` set is
/// small; the cap is what keeps a polluted temp dir from blowing the
/// report up.
pub(super) const TASK_TARGET_ROW_CAP: usize = 64;

/// Shared `stat` budget for every task-target walk in one report, and
/// the most entries one directory may contribute. A real cargo tree
/// trips the per-dir cap (the byte count is then a lower bound and the
/// row warns); a fixture with a handful of files does not.
const TASK_TARGET_STAT_BUDGET: usize = 8_192;

const TASK_TARGET_DIR_STAT_CAP: usize = 1_024;

/// `/proc` pids inspected while attributing cwd/exe to those rows.
const TASK_TARGET_PROC_BUDGET: usize = 8_192;

/// Issue files read while looking for recorded `cargo_target` paths.
const TASK_TARGET_ISSUE_BUDGET: usize = 4_096;

/// Pids quoted on one row. The count is the full number observed.
const TASK_TARGET_PIDS: usize = 8;

// ---------- legacy task cargo targets ----------
//
// `temp-dirs` only matches `cadence-` / `.tmp` / `tmp.`. Per-task
// cargo output such as `/tmp/cad156-fix-target` is invisible there,
// and a name that looks similar is not proof the directory is ours
// or that it is idle. This check inventories that namespace and
// stops. It does not join `--reclaim-plan` and it never emits a
// deletion command: live, locked, foreign, symlink, name-only and
// merely unproven rows are all excluded, and "no cwd/exe pointed
// here" is reported as unproven rather than safe.

/// `cad156-fix-target`, `cad173-nextest-target-one`,
/// `cad176-pr100-target`. The `cad` + digits + `-` head is the
/// legacy task prefix; `target` anywhere in the tail is what keeps
/// `cad156-fix` and `cadence-*` out. Byte-wise so a non-UTF8 temp
/// name cannot be lossily rewritten into a match.
pub(super) fn legacy_task_target_name(name: &std::ffi::OsStr) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let bytes = name.as_bytes();
    let Some(rest) = bytes.strip_prefix(b"cad") else {
        return false;
    };
    let Some(dash) = rest.iter().position(|b| *b == b'-') else {
        return false;
    };
    let num = &rest[..dash];
    let tail = &rest[dash + 1..];
    !num.is_empty()
        && num.iter().all(|b| b.is_ascii_digit())
        && tail.windows(6).any(|w| w == b"target")
}

fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn normalize_abs(path: &Path) -> Option<PathBuf> {
    if !path.is_absolute() {
        return None;
    }
    let text = path.to_string_lossy();
    let trimmed = text.trim_end_matches('/');
    let path = if trimmed.is_empty() {
        PathBuf::from("/")
    } else {
        PathBuf::from(trimmed)
    };
    Some(lexical_normalize(&path))
}

fn under_cadence_tree(path: &Path) -> bool {
    path.components().any(|c| c.as_os_str() == ".cadence")
}

fn configured_cargo_target(scan: &Scan) -> Option<PathBuf> {
    let raw = scan.cargo_target_dir.as_ref()?;
    let joined = if raw.is_absolute() {
        raw.clone()
    } else {
        scan.cwd.join(raw)
    };
    normalize_abs(&joined)
}

/// Keep the stronger gap. `unreadable` must not collapse back to
/// `incomplete` when a later entry fails a milder check.
fn note_record(status: &mut &'static str, next: &'static str) {
    fn rank(s: &str) -> u8 {
        match s {
            "unreadable" => 3,
            "incomplete" => 2,
            _ => 0,
        }
    }
    if rank(next) > rank(status) {
        *status = next;
    }
}

/// Issue ids whose worktree ref records this cargo target. Symlinked
/// issue files and project dirs are skipped rather than followed; a
/// skip or a read error makes the search status incomplete so a
/// missing record is not treated as proof of non-ownership.
fn recorded_cargo_targets(pm: Option<&Path>) -> (BTreeMap<PathBuf, Vec<String>>, &'static str) {
    let mut map: BTreeMap<PathBuf, Vec<String>> = BTreeMap::new();
    let Some(pm) = pm else {
        return (map, "no-tracker");
    };
    let Ok(meta) = std::fs::symlink_metadata(pm) else {
        return (map, "unreadable");
    };
    if !meta.is_dir() {
        return (map, "unreadable");
    }
    let Ok(projects) = std::fs::read_dir(pm) else {
        return (map, "unreadable");
    };
    let mut status = "complete";
    let mut seen = 0_usize;
    for project in projects {
        let project = match project {
            Ok(project) => project,
            Err(_) => {
                note_record(&mut status, "incomplete");
                continue;
            }
        };
        let Ok(kind) = project.file_type() else {
            note_record(&mut status, "incomplete");
            continue;
        };
        if !kind.is_dir() {
            continue;
        }
        let Ok(issues) = std::fs::read_dir(project.path()) else {
            note_record(&mut status, "unreadable");
            continue;
        };
        for issue in issues {
            if seen >= TASK_TARGET_ISSUE_BUDGET {
                return (map, "truncated");
            }
            let issue = match issue {
                Ok(issue) => issue,
                Err(_) => {
                    note_record(&mut status, "incomplete");
                    continue;
                }
            };
            let Ok(kind) = issue.file_type() else {
                note_record(&mut status, "incomplete");
                continue;
            };
            if !kind.is_dir() {
                continue;
            }
            let file = issue.path().join("issue.md");
            let meta = match std::fs::symlink_metadata(&file) {
                Ok(meta) => meta,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => {
                    note_record(&mut status, "incomplete");
                    continue;
                }
            };
            if meta.file_type().is_symlink() {
                note_record(&mut status, "incomplete");
                continue;
            }
            if !meta.is_file() {
                continue;
            }
            seen += 1;
            if meta.len() > 1_048_576 {
                note_record(&mut status, "incomplete");
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&file) else {
                note_record(&mut status, "incomplete");
                continue;
            };
            let Ok((front, _)) = crate::issue::parse::parse_issue(&text) else {
                note_record(&mut status, "incomplete");
                continue;
            };
            for r in &front.refs {
                if r.kind != "worktree" {
                    continue;
                }
                let Some(raw) = r.cargo_target.as_deref() else {
                    continue;
                };
                let Some(path) = normalize_abs(Path::new(raw)) else {
                    note_record(&mut status, "incomplete");
                    continue;
                };
                let ids = map.entry(path).or_default();
                if !ids.iter().any(|id| id == &front.id) {
                    ids.push(front.id.clone());
                    ids.sort();
                }
            }
        }
    }
    (map, status)
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum LockBit {
    Absent,
    Free,
    Held,
    Unknown,
}

/// What `lstat` can say about `path` without crossing a symlink.
/// `symlink_metadata` on the full path still walks ancestor links, so
/// `/tmp/link/target` looks like a real directory when `link` is a
/// symlink. Each component is `lstat`'d on its own and the walk stops
/// at the first link.
pub(super) enum LexicalKind {
    Absent,
    /// `at` is the symlink component. `final_component` is false when
    /// an ancestor, not the path itself, is the link.
    Symlink {
        at: PathBuf,
        final_component: bool,
    },
    Ready(std::fs::Metadata),
    Error,
}

pub(super) fn lexical_kind(path: &Path) -> LexicalKind {
    if !path.is_absolute() {
        return LexicalKind::Error;
    }
    let mut cur = PathBuf::new();
    let comps: Vec<_> = path.components().collect();
    let last = comps.len().saturating_sub(1);
    for (i, c) in comps.into_iter().enumerate() {
        match c {
            std::path::Component::RootDir | std::path::Component::Prefix(_) => {
                cur.push(c);
            }
            std::path::Component::CurDir | std::path::Component::ParentDir => {
                return LexicalKind::Error;
            }
            std::path::Component::Normal(name) => {
                cur.push(name);
                match std::fs::symlink_metadata(&cur) {
                    Ok(meta) if meta.file_type().is_symlink() => {
                        return LexicalKind::Symlink {
                            at: cur,
                            final_component: i == last,
                        };
                    }
                    Ok(meta) if i == last => return LexicalKind::Ready(meta),
                    Ok(meta) if meta.is_dir() => {}
                    Ok(_) => return LexicalKind::Error,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        return LexicalKind::Absent;
                    }
                    Err(_) => return LexicalKind::Error,
                }
            }
        }
    }
    LexicalKind::Error
}

/// Non-blocking exclusive probe of one cargo lock file.
///
/// A FIFO named `.cargo-lock` blocks `open` forever. A final-component
/// symlink is not the only trap: `lstat` of the basename still follows
/// ancestor links, and a replacement between the check and `open` can
/// swap in a FIFO or a symlink. Non-regular files are rejected first.
/// The open itself is one `O_NOFOLLOW | O_NONBLOCK` call, and the fd is
/// kept only when `fstat` still says it is a regular file.
pub(super) fn probe_lock_file(path: &Path) -> LockBit {
    match lexical_kind(path) {
        LexicalKind::Absent => return LockBit::Absent,
        LexicalKind::Ready(meta) if meta.is_file() => {}
        LexicalKind::Ready(_) | LexicalKind::Symlink { .. } | LexicalKind::Error => {
            return LockBit::Unknown;
        }
    }
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return LockBit::Absent,
        Err(_) => return LockBit::Unknown,
    };
    match file.metadata() {
        Ok(meta) if meta.is_file() => {}
        _ => return LockBit::Unknown,
    }
    try_lock_probe(&file)
}

/// Take and give back an exclusive flock on an open lock file. A lock
/// the probe won is released with `flock`, not by close: a fork in
/// another thread shares this descriptor until its child execs, and a
/// closed probe would hold cargo's lock for that long (CAD-389).
pub(super) fn try_lock_probe(file: &std::fs::File) -> LockBit {
    use std::os::unix::io::AsRawFd;
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
        return LockBit::Free;
    }
    let err = std::io::Error::last_os_error();
    if err.raw_os_error() == Some(libc::EWOULDBLOCK) {
        LockBit::Held
    } else {
        LockBit::Unknown
    }
}

fn fold_lock(status: &'static str, bit: LockBit) -> &'static str {
    match (status, bit) {
        (_, LockBit::Held) | ("held", _) => "held",
        ("unknown", _) | (_, LockBit::Unknown) => "unknown",
        (_, LockBit::Free) => "free",
        (status, LockBit::Absent) => status,
    }
}

/// Cargo's build locks at the target root and under `debug/` /
/// `release/` only. A symlinked profile directory is not entered,
/// and neither is a directory reached through an ancestor symlink.
fn cargo_lock_status(dir: &Path) -> &'static str {
    match lexical_kind(dir) {
        LexicalKind::Ready(meta) if meta.is_dir() => {}
        _ => return "unknown",
    }
    let mut status = "absent";
    for name in [".cargo-lock", ".cargo-build-lock", ".cargo-artifact-lock"] {
        status = fold_lock(status, probe_lock_file(&dir.join(name)));
        if status == "held" {
            return status;
        }
    }
    for sub in ["debug", "release"] {
        let subdir = dir.join(sub);
        match std::fs::symlink_metadata(&subdir) {
            Ok(m) if m.file_type().is_symlink() => {
                status = fold_lock(status, LockBit::Unknown);
            }
            Ok(m) if m.is_dir() => {
                for name in [".cargo-lock", ".cargo-build-lock", ".cargo-artifact-lock"] {
                    status = fold_lock(status, probe_lock_file(&subdir.join(name)));
                    if status == "held" {
                        return status;
                    }
                }
            }
            _ => {}
        }
    }
    status
}

struct ProcSeen {
    pids: Vec<u32>,
    extra: usize,
}

/// One pass over `proc_root`. Cwd and exe are `read_link` results —
/// the link text, not a followed target. `partial` means a pid denied
/// both links or a directory entry could not be read, so a row with
/// no hit is not proof that nothing references it.
fn task_target_proc_hits(proc_root: &Path, roots: &[PathBuf]) -> (Vec<ProcSeen>, &'static str) {
    let mut hits = roots
        .iter()
        .map(|_| ProcSeen {
            pids: Vec::new(),
            extra: 0,
        })
        .collect::<Vec<_>>();
    let Ok(entries) = std::fs::read_dir(proc_root) else {
        return (hits, "unreadable");
    };
    let mut seen = 0_usize;
    let mut partial = false;
    for ent in entries {
        let ent = match ent {
            Ok(ent) => ent,
            Err(_) => {
                partial = true;
                continue;
            }
        };
        if seen >= TASK_TARGET_PROC_BUDGET {
            return (hits, "truncated");
        }
        let Some(pid) = ent.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        seen += 1;
        let cwd = std::fs::read_link(ent.path().join("cwd"));
        let exe = std::fs::read_link(ent.path().join("exe"));
        if cwd
            .as_ref()
            .is_err_and(|e| e.kind() == std::io::ErrorKind::PermissionDenied)
            && exe
                .as_ref()
                .is_err_and(|e| e.kind() == std::io::ErrorKind::PermissionDenied)
        {
            partial = true;
            continue;
        }
        for (i, root) in roots.iter().enumerate() {
            let matched = [cwd.as_ref(), exe.as_ref()]
                .into_iter()
                .flatten()
                .any(|link| {
                    let text = link.to_string_lossy();
                    let stripped = text.strip_suffix(" (deleted)").unwrap_or(text.as_ref());
                    let path = lexical_normalize(Path::new(stripped));
                    path == *root || path.starts_with(root)
                });
            if !matched {
                continue;
            }
            let hit = &mut hits[i];
            if hit.pids.len() < TASK_TARGET_PIDS {
                if !hit.pids.contains(&pid) {
                    hit.pids.push(pid);
                }
            } else {
                hit.extra += 1;
            }
        }
    }
    for hit in &mut hits {
        hit.pids.sort_unstable();
    }
    let status = if partial { "partial" } else { "complete" };
    (hits, status)
}

struct TaskCandidate {
    path: PathBuf,
    name_match: bool,
}

pub(super) fn check_task_targets(scan: &Scan) -> Check {
    let name = "task-targets";
    let t = &scan.thresholds;
    let threshold = json!(format!(
        "warn: ≥{} pressure dirs, ≥{}, a truncated or incomplete temp/tracker/proc scan, or any active/locked/unknown row — inventory only, never a deletion list",
        t.temp_warn_count,
        human(t.temp_warn_bytes)
    ));
    let (recorded, record_search) = recorded_cargo_targets(scan.pm_dir.as_deref());
    let configured = configured_cargo_target(scan);

    let mut candidates: BTreeMap<PathBuf, bool> = BTreeMap::new();
    let mut temp_scan = "complete";
    match std::fs::read_dir(&scan.temp_dir) {
        Ok(entries) => {
            let mut n = 0_usize;
            for ent in entries {
                if n >= TASK_TARGET_TEMP_BUDGET {
                    temp_scan = "truncated";
                    break;
                }
                let ent = match ent {
                    Ok(ent) => ent,
                    Err(_) => {
                        if temp_scan == "complete" {
                            temp_scan = "incomplete";
                        }
                        continue;
                    }
                };
                n += 1;
                if !legacy_task_target_name(&ent.file_name()) {
                    continue;
                }
                // `file_type` does not follow links. A symlink is
                // inventoried and not walked; a regular file is not a
                // cargo target directory.
                let Ok(kind) = ent.file_type() else {
                    temp_scan = "incomplete";
                    continue;
                };
                if !kind.is_dir() && !kind.is_symlink() {
                    continue;
                }
                let path = lexical_normalize(&ent.path());
                candidates.insert(path, true);
            }
        }
        Err(_) => temp_scan = "unreadable",
    }
    for path in recorded.keys() {
        candidates.entry(path.clone()).or_insert(false);
    }
    if let Some(path) = &configured {
        candidates.entry(path.clone()).or_insert(false);
    }

    let mut scan_truncated = temp_scan == "truncated" || record_search == "truncated";
    let mut selected: Vec<TaskCandidate> = candidates
        .into_iter()
        .map(|(path, name_match)| TaskCandidate { path, name_match })
        .collect();
    if selected.len() > TASK_TARGET_ROW_CAP {
        selected.truncate(TASK_TARGET_ROW_CAP);
        scan_truncated = true;
    }

    let roots: Vec<PathBuf> = selected.iter().map(|c| c.path.clone()).collect();
    let (hits, proc_scan) = task_target_proc_hits(&scan.proc_root, &roots);

    let pressure_slots = selected
        .iter()
        .filter(|c| !under_cadence_tree(&c.path))
        .count()
        .max(1);
    // Every legacy directory gets the same slice of the stat budget.
    // Walking recorded `.cadence` targets first would consume it and
    // leave the `/tmp/cad*-target*` rows unsized.
    let per_dir_budget =
        (TASK_TARGET_STAT_BUDGET / pressure_slots).clamp(1, TASK_TARGET_DIR_STAT_CAP);
    let mut rows: Vec<Value> = Vec::new();
    let mut pressure_count = 0_u64;
    let mut pressure_bytes = 0_u64;
    let record_gap = !matches!(record_search, "complete" | "no-tracker");
    // An unknown proc/tracker/temp scan must not leave the check `ok`.
    // `run` takes the worst level and `exit_code` treats `ok` as a
    // healthy host.
    let proc_gap = proc_scan != "complete";
    let mut attention = temp_scan != "complete" || scan_truncated || record_gap || proc_gap;
    for (cand, hit) in selected.iter().zip(hits.iter()) {
        let path = &cand.path;
        let issues = recorded.get(path).cloned().unwrap_or_default();
        let is_configured = configured.as_ref().is_some_and(|p| p == path);
        let name_match = cand.name_match || path.file_name().is_some_and(legacy_task_target_name);
        let ownership = if !issues.is_empty() {
            "recorded"
        } else if is_configured {
            "configured"
        } else {
            "name-only"
        };
        let proven = ownership != "name-only";
        let pressure = !under_cadence_tree(path);
        let quoted = shell_quote(&path.display().to_string());

        // Component-wise lstat. `symlink_metadata(path)` would follow
        // an ancestor and report the final directory as real.
        let looked = lexical_kind(path);
        let ancestor_symlink = matches!(
            looked,
            LexicalKind::Symlink {
                final_component: false,
                ..
            }
        );
        let link_at = match &looked {
            LexicalKind::Symlink { at, .. } => Some(at.clone()),
            _ => None,
        };
        let symlink = link_at.is_some();
        let meta = match &looked {
            LexicalKind::Ready(m) => Some(m.clone()),
            LexicalKind::Symlink { at, .. } => std::fs::symlink_metadata(at).ok(),
            _ => None,
        };
        let exists = matches!(looked, LexicalKind::Ready(_) | LexicalKind::Symlink { .. });
        let uid = meta.as_ref().map(|m| m.uid());
        let uid_matches = uid.is_some_and(|u| u == scan.uid);
        let age_secs = meta.as_ref().and_then(|m| {
            m.modified().ok().map(|modified| {
                scan.now
                    .duration_since(modified)
                    .map(|d| d.as_secs())
                    .unwrap_or(0)
            })
        });
        let link_target = link_at
            .as_deref()
            .and_then(|at| std::fs::read_link(at).ok())
            .map(|t| t.display().to_string());
        let foreign_symlink = if let Some(at) = &link_at {
            match &link_target {
                Some(target) => {
                    let target = PathBuf::from(target);
                    let base = at.parent().unwrap_or(Path::new("/"));
                    let resolved = if target.is_absolute() {
                        lexical_normalize(&target)
                    } else {
                        lexical_normalize(&base.join(target))
                    };
                    let tmp = lexical_normalize(&scan.temp_dir);
                    !resolved.starts_with(&tmp)
                }
                None => true,
            }
        } else {
            false
        };

        let (bytes, bytes_truncated, bytes_skipped, cargo_lock) = if symlink {
            (None, false, Some("symlink"), "unknown")
        } else if matches!(looked, LexicalKind::Error) {
            (None, false, Some("unreadable"), "unknown")
        } else if !exists {
            (None, false, Some("absent"), "absent")
        } else if !uid_matches {
            (None, false, Some("foreign-uid"), "unknown")
        } else if meta.as_ref().is_some_and(|m| !m.is_dir()) {
            (None, false, Some("not-a-directory"), "unknown")
        } else if !pressure {
            // The worktree check already measures these trees.
            (
                None,
                false,
                Some("tracked-elsewhere"),
                cargo_lock_status(path),
            )
        } else {
            let (b, truncated, _) = dir_size_limited(path, per_dir_budget);
            if truncated {
                scan_truncated = true;
            }
            (Some(b), truncated, None, cargo_lock_status(path))
        };

        let observed = !hit.pids.is_empty();
        // Hits we did see stay observed. A row with no hit is
        // `none-observed` only when the proc scan finished. Partial,
        // truncated, and unreadable scans are unknown negatives.
        let cwd_exe = if observed {
            "observed"
        } else if proc_scan != "complete" {
            "unreadable"
        } else {
            "none-observed"
        };
        let activity = if observed || cargo_lock == "held" {
            "active"
        } else if symlink
            || !exists
            || !uid_matches
            || cwd_exe == "unreadable"
            || cargo_lock == "unknown"
        {
            "unknown"
        } else {
            "unproven"
        };
        let exclude = if symlink {
            "symlink"
        } else if exists && !uid_matches {
            "foreign-uid"
        } else if activity == "active" {
            "active"
        } else if activity == "unknown" {
            "unknown"
        } else if ownership == "name-only" {
            "name-only"
        } else {
            "unproven"
        };

        if pressure && exists {
            pressure_count += 1;
            pressure_bytes = pressure_bytes.saturating_add(bytes.unwrap_or(0));
            if bytes_truncated
                || activity == "active"
                || activity == "unknown"
                || cargo_lock == "held"
                || cargo_lock == "unknown"
            {
                attention = true;
            }
        }

        rows.push(json!({
            "path": path,
            "quoted": quoted,
            "name": path.file_name().map(|n| n.to_string_lossy().into_owned()),
            "name_match": name_match,
            "ownership": ownership,
            "proven": proven,
            "configured": is_configured,
            "issues": issues,
            "owner_uid": uid,
            "uid_matches": exists && uid_matches,
            "age_secs": age_secs,
            "bytes": bytes,
            "bytes_truncated": bytes_truncated,
            "bytes_skipped": bytes_skipped,
            "exists": exists,
            "symlink": symlink,
            "ancestor_symlink": ancestor_symlink,
            // True only if a symlink component was crossed. Nothing in
            // this check crosses one, including an ancestor of the
            // final path, so this stays false.
            "followed": false,
            "foreign_symlink": foreign_symlink,
            "cargo_lock": cargo_lock,
            "activity": activity,
            "cwd_exe": cwd_exe,
            "pids": hit.pids,
            "pids_omitted": hit.extra,
            "pressure": pressure,
            "reclaim_candidate": false,
            "safe_to_delete": false,
            "action": "none",
            "exclude": exclude,
        }));
    }

    if scan_truncated {
        attention = true;
    }
    let level = if attention
        || pressure_count >= t.temp_warn_count
        || pressure_bytes >= t.temp_warn_bytes
    {
        Level::Warn
    } else {
        Level::Ok
    };

    let shown_rows: Vec<&Value> = rows
        .iter()
        .filter(|r| r["pressure"] == json!(true))
        .chain(rows.iter().filter(|r| r["pressure"] != json!(true)))
        .collect();
    let detail = if rows.is_empty() {
        if temp_scan == "unreadable" {
            format!(
                "temp dir {} unreadable — task targets not inventoried",
                scan.temp_dir.display()
            )
        } else if record_gap || temp_scan != "complete" || proc_scan != "complete" {
            format!(
                "none listed — temp {temp_scan}, tracker {record_search}, proc {proc_scan}; not a conclusive absence"
            )
        } else {
            "none".to_string()
        }
    } else {
        let name_only = rows
            .iter()
            .filter(|r| r["ownership"] == "name-only")
            .count();
        let proven_n = rows.iter().filter(|r| r["proven"] == json!(true)).count();
        let active = rows.iter().filter(|r| r["activity"] == "active").count();
        let unproven = rows.iter().filter(|r| r["activity"] == "unproven").count();
        let locked = rows.iter().filter(|r| r["cargo_lock"] == "held").count();
        let size = if rows.iter().any(|r| r["bytes_truncated"] == json!(true)) || scan_truncated {
            format!("at least {}", human(pressure_bytes))
        } else {
            human(pressure_bytes)
        };
        let shown = shown_rows
            .iter()
            .take(5)
            .map(|r| r["quoted"].as_str().unwrap_or("?"))
            .collect::<Vec<_>>()
            .join(" ");
        let more = if rows.len() > 5 {
            format!(" (+{} more)", rows.len() - 5)
        } else {
            String::new()
        };
        let idle_note = if unproven > 0 || proc_scan != "complete" {
            " — none-observed is not proof the directory is idle"
        } else {
            ""
        };
        format!(
            "{n} task targets, {size} pressure ({name_only} name-only, {proven_n} proven; {active} active, {locked} cargo-locked, {unproven} cwd/exe none-observed{idle_note}; proc {proc_scan}){more}: {shown}",
            n = rows.len(),
        )
    };
    let remedy = if rows.is_empty() {
        String::new()
    } else {
        format!(
            "read-only inventory — nothing is deleted. A matching name is not Cadence ownership. No cwd/exe reference is not proof the directory is idle. Recheck ownership, process cwd/exe, and cargo locks immediately before any authorised reclaim. {}",
            shown_rows
                .iter()
                .take(5)
                .map(|r| format!(
                    "{} ({}, {}, lock {})",
                    r["quoted"].as_str().unwrap_or("?"),
                    r["ownership"].as_str().unwrap_or("?"),
                    r["activity"].as_str().unwrap_or("?"),
                    r["cargo_lock"].as_str().unwrap_or("?")
                ))
                .collect::<Vec<_>>()
                .join(" ")
        )
    };
    let value = json!({
        "count": rows.len(),
        "bytes": pressure_bytes,
        "pressure_count": pressure_count,
        "scan_truncated": scan_truncated,
        "temp_scan": temp_scan,
        "record_search": record_search,
        "proc_scan": proc_scan,
        "safe_to_delete": false,
        "record_conclusive": record_search == "complete",
        "note": "A matching name is not Cadence ownership. No observed cwd/exe reference is not proof the directory is idle. Recheck ownership, process cwd/exe, and cargo locks immediately before any authorised reclaim.",
        "rows": rows,
    });
    check(name, level, value, threshold, detail, remedy)
}
