//! CAD-536: `cadence doctor host` check `orphans` — moved verbatim from src/doctor/host.rs.

use super::*;

use crate::worktree::layout;

// ---------- orphaned work ----------

pub(super) struct Orphan {
    pub(super) pid: u32,
    pub(super) age_secs: Option<u64>,
    pub(super) head: String,
    pub(super) reasons: Vec<&'static str>,
}

/// `readlink` targets append " (deleted)" when the inode is gone.
/// Match a path inside a `.cadence/wt/` tree that no longer exists.
pub(super) fn deleted_worktree(target: &Path) -> Option<PathBuf> {
    let text = target.to_string_lossy();
    let stripped = text.strip_suffix(" (deleted)").unwrap_or(&text);
    let in_wt = layout::in_worktrees_dir(stripped);
    let path = PathBuf::from(stripped);
    (in_wt && !path.exists()).then_some(path)
}

/// Cargo test binaries live at `target/{debug,release}/deps/<name>-<hash>`.
fn is_test_binary(path: &Path) -> bool {
    let text = path.to_string_lossy();
    let stripped = text.strip_suffix(" (deleted)").unwrap_or(&text);
    stripped.contains("/target/debug/deps/") || stripped.contains("/target/release/deps/")
}

/// Redact credential material from an argv, returning it joined with
/// spaces — the one place argv becomes display text. The executable
/// and ordinary arguments pass through; secret *values* become
/// `[REDACTED]`: the value of any `--flag=value` or `--flag value`
/// whose flag name looks credential-bearing, `NAME=value` env-style
/// arguments with such a name, `Key: value` header arguments, the
/// password half of `scheme://user:pass@host`, `-p`/`-a`/`-u`
/// short-flag values, and any standalone argument matching a known
/// credential shape or the 32+ char high-entropy token shape.
///
/// An element that itself holds a command (`sh -c 'run --token …'`)
/// is split once on unquoted whitespace, including Unicode spaces,
/// and run through those same rules, so a secret inside the script
/// is not printed. A Unicode space is not shell syntax; it is only
/// a character boundary so a flag value cannot hide against the
/// flag. Quotes are grouping only — this is not a shell parser and
/// it never executes the text. `$`, backticks and backslashes fail
/// closed: the element
/// is withheld when a secret flag, assignment, header or credential
/// shape is still visible, and left unchanged when it is not. A
/// secret-named `NAME=value` that occupies the whole element still
/// redacts the entire value, trailing words included.
/// Public because `src/session.rs` shares it — argv-as-text must
/// share one scrubber rather than re-implement.
pub fn redact_argv<S: AsRef<str>>(argv: &[S]) -> String {
    redact_argv_parts(argv, true, false).join(" ")
}

/// argv[0] plus a redacted, ~120-char head of the full command line —
/// test-binary detection needs the untruncated argv0, and the head is
/// display text so `redact_argv` scrubs it before anything stores it.
pub(super) fn cmdline(pid_dir: &Path) -> (Option<PathBuf>, Option<String>) {
    use std::io::Read;
    let Ok(file) = std::fs::File::open(pid_dir.join("cmdline")) else {
        return (None, None);
    };
    // argv can be huge — 8 KiB is far past the 120 characters kept.
    let mut raw = Vec::new();
    if file.take(8 * 1024).read_to_end(&mut raw).is_err() {
        return (None, None);
    }
    let mut parts: Vec<String> = raw
        .split(|b| *b == 0)
        .map(|p| String::from_utf8_lossy(p).into_owned())
        .collect();
    // The file ends with a NUL — drop only that trailing empty so
    // interior empty arguments stay visible.
    if parts.last().is_some_and(|p| p.is_empty()) {
        parts.pop();
    }
    let argv0 = parts.first().map(PathBuf::from);
    let head = (!parts.is_empty()).then(|| redact_argv(&parts).chars().take(120).collect());
    (argv0, head)
}

pub(super) enum Probe {
    Missing,
    Denied,
    Ok(Option<Orphan>),
}

/// Everything decidable about one pid; races collapse to Missing.
pub(super) fn probe_pid(pid_dir: &Path, pid: u32, uptime: Option<f64>, scan: &Scan) -> Probe {
    let cwd = std::fs::read_link(pid_dir.join("cwd"));
    let exe = std::fs::read_link(pid_dir.join("exe"));
    if !pid_dir.exists() {
        return Probe::Missing;
    }
    if cwd
        .as_ref()
        .is_err_and(|e| e.kind() == std::io::ErrorKind::PermissionDenied)
        && exe
            .as_ref()
            .is_err_and(|e| e.kind() == std::io::ErrorKind::PermissionDenied)
    {
        return Probe::Denied;
    }
    let mut reasons = Vec::new();
    for target in [cwd.as_deref().ok(), exe.as_deref().ok()]
        .into_iter()
        .flatten()
    {
        if deleted_worktree(target).is_some() {
            reasons.push(concat!(
                "cwd/exe under a deleted ",
                layout::worktrees_rel!(),
                " worktree"
            ));
            break;
        }
    }
    let age = pid_age_secs(pid_dir, uptime);
    let (argv0, head) = cmdline(pid_dir);
    let test_path = exe.as_deref().ok().or(argv0.as_deref());
    if test_path.is_some_and(is_test_binary)
        && age.is_some_and(|a| a >= scan.thresholds.orphan_min_age_secs)
    {
        reasons.push("cargo test binary older than an hour");
    }
    if reasons.is_empty() {
        return Probe::Ok(None);
    }
    Probe::Ok(Some(Orphan {
        pid,
        age_secs: age,
        head: head
            .or_else(|| exe.ok().map(|e| redact_argv(&[e.display().to_string()])))
            .unwrap_or_else(|| "(unknown)".to_string()),
        reasons,
    }))
}

pub(super) fn check_orphans(scan: &Scan) -> Check {
    let name = "orphans";
    let threshold = json!(format!(
        "warn: any process under a deleted {}, or a test binary older than {}s",
        layout::WORKTREES_REL,
        scan.thresholds.orphan_min_age_secs
    ));
    let mut orphans = Vec::new();
    let mut denied = 0_u64;
    let mut vanished = 0_u64;
    let uptime = proc_uptime(&scan.proc_root);
    if let Ok(pids) = std::fs::read_dir(&scan.proc_root) {
        for ent in pids.flatten() {
            let Some(pid) = ent.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else {
                continue;
            };
            match probe_pid(&ent.path(), pid, uptime, scan) {
                Probe::Ok(Some(o)) => orphans.push(o),
                Probe::Denied => denied += 1,
                Probe::Missing => vanished += 1,
                Probe::Ok(None) => {}
            }
        }
    }
    // read_dir order is arbitrary — sort so the pids[] array and the
    // `kill …` remedy are deterministic for users too.
    orphans.sort_by_key(|o| o.pid);
    let level = if orphans.is_empty() {
        Level::Ok
    } else {
        Level::Warn
    };
    let mut detail = if orphans.is_empty() {
        "none".to_string()
    } else {
        format!(
            "{}: {}",
            orphans.len(),
            orphans
                .iter()
                .take(5)
                .map(|o| {
                    let age = o
                        .age_secs
                        .map(|a| format!("{}h", a / 3600))
                        .unwrap_or_else(|| "?h".to_string());
                    format!("pid {} ({} {})", o.pid, age, o.head)
                })
                .collect::<Vec<_>>()
                .join("; ")
        )
    };
    if denied + vanished > 0 {
        detail.push_str(&format!(
            "; {} unreadable, {} vanished mid-scan",
            denied, vanished
        ));
    }
    let remedy = kill_remedy(
        &scan.proc_root,
        &orphans.iter().take(10).map(|o| o.pid).collect::<Vec<_>>(),
        "orphaned; their worktrees are gone — the watchdog never signals them itself",
    );
    let value = json!({
        "count": orphans.len(),
        "pids": orphans.iter().map(|o| json!({
            "pid": o.pid,
            "age_secs": o.age_secs,
            "head": o.head,
            "reasons": o.reasons,
        })).collect::<Vec<_>>(),
        "unreadable": denied,
        "vanished": vanished,
    });
    check(name, level, value, threshold, detail, remedy)
}
