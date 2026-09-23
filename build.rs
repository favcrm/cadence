//! Compile the build's git identity into the binary: the deploy-drift
//! check compares the *running daemon's* commit against the tracker's
//! default branch, and `daemon_info`/`/api/meta`/`--version` report it.
//! Every value falls back to `unknown` when git or a repo is absent —
//! consumers treat `unknown` as "cannot tell", never as zero.

// Runs in cargo, never in the daemon: the CAD-308 spawn registry
// (`cadence_agent::reaper`) does not apply to build scripts.
#![allow(clippy::disallowed_methods)]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// A hung `git` (credential prompt, `safe.directory` refusal waiting
/// on input) must never stall a build — five seconds per call, then
/// kill and fall back to `unknown`.
const GIT_TIMEOUT: Duration = Duration::from_secs(5);

fn git(args: &[&str]) -> Option<String> {
    let mut child = Command::new("git")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + GIT_TIMEOUT;
    let out = loop {
        match child.try_wait() {
            Ok(Some(_)) => break child.wait_with_output().ok()?,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                return None;
            }
        }
    };
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// Re-run this script when the checkout's commit moves. `.git` is a
/// directory in a plain clone but a `gitdir:` file in a worktree —
/// resolve it, watch its HEAD, and watch the ref HEAD names (or
/// `packed-refs` when the ref is packed).
fn watch_checkout() {
    let dotgit = Path::new(".git");
    let gitdir = if dotgit.is_file() {
        let Ok(text) = std::fs::read_to_string(dotgit) else {
            return;
        };
        let Some(rest) = text.trim().strip_prefix("gitdir:") else {
            return;
        };
        PathBuf::from(rest.trim())
    } else {
        dotgit.to_path_buf()
    };
    let head = gitdir.join("HEAD");
    println!("cargo:rerun-if-changed={}", head.display());
    let Ok(head_text) = std::fs::read_to_string(&head) else {
        return;
    };
    let Some(reference) = head_text.trim().strip_prefix("ref:") else {
        return;
    };
    // Worktree gitdirs point at the shared one via `commondir`.
    let common = std::fs::read_to_string(gitdir.join("commondir"))
        .ok()
        .map(|c| gitdir.join(c.trim()))
        .unwrap_or_else(|| gitdir.clone());
    let ref_file = common.join(reference.trim());
    println!("cargo:rerun-if-changed={}", ref_file.display());
    let packed = common.join("packed-refs");
    println!("cargo:rerun-if-changed={}", packed.display());
}

fn main() {
    // The watched files keep the baked commit fresh on commit or
    // checkout; source edits still re-run the script on their own.
    watch_checkout();
    let unknown = || "unknown".to_string();
    for (var, args) in [
        ("CADENCE_BUILD_COMMIT", vec!["rev-parse", "HEAD"]),
        (
            "CADENCE_BUILD_TIME",
            vec!["show", "-s", "--format=%cI", "HEAD"],
        ),
        (
            "CADENCE_BUILD_REMOTE",
            vec!["config", "--get", "remote.origin.url"],
        ),
        ("CADENCE_BUILD_ROOT", vec!["rev-parse", "--show-toplevel"]),
    ] {
        println!(
            "cargo:rustc-env={var}={}",
            git(&args).unwrap_or_else(unknown)
        );
    }
}
