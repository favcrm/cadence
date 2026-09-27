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

pub(crate) fn git(checkout: &Path, args: &[&str]) -> Option<String> {
    let mut child = Command::new("git")
        .args(args)
        .current_dir(checkout)
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

/// Existing directory watches catch loose-ref creation/deletion, including
/// packing and unpacking. A missing watched file makes Cargo rerun forever.
/// Watch packed-refs only while present: creating it cannot supersede a loose
/// ref until that ref changes/disappears under the watched refs directory.
/// Watch the nearest existing ref parent, not objects, index or reflogs.
pub(crate) fn checkout_watch_paths(checkout: &Path) -> Vec<PathBuf> {
    let dotgit = checkout.join(".git");
    let mut paths = Vec::new();
    let gitdir = if dotgit.is_file() {
        paths.push(dotgit.clone());
        let Ok(text) = std::fs::read_to_string(&dotgit) else {
            return paths;
        };
        let Some(rest) = text.trim().strip_prefix("gitdir:") else {
            return paths;
        };
        checkout.join(rest.trim())
    } else {
        dotgit
    };
    let Ok(gitdir) = gitdir.canonicalize() else {
        return paths;
    };
    let head = gitdir.join("HEAD");
    if !head.is_file() {
        // An incomplete Git directory can acquire HEAD later.
        paths.push(gitdir);
        return paths;
    }
    paths.push(head.clone());
    let Ok(head_text) = std::fs::read_to_string(&head) else {
        return paths;
    };
    let Some(reference) = head_text.trim().strip_prefix("ref:") else {
        return paths;
    };
    // Worktree commondir paths are relative to the worktree's Git directory.
    let commondir = gitdir.join("commondir");
    let common = if commondir.is_file() {
        paths.push(commondir.clone());
        std::fs::read_to_string(&commondir)
            .ok()
            .map(|c| gitdir.join(c.trim()))
            .unwrap_or_else(|| gitdir.clone())
    } else {
        gitdir
    };
    let Ok(common) = common.canonicalize() else {
        return paths;
    };
    let mut ref_parent = common.join(reference.trim());
    ref_parent.pop();
    // Retain an existing ancestor if pruning removed a nested ref directory.
    // Its directory watch observes that directory/ref being created again.
    while !ref_parent.is_dir() {
        if !ref_parent.pop() {
            return paths;
        }
    }
    paths.push(ref_parent);
    let packed = common.join("packed-refs");
    if packed.is_file() {
        paths.push(packed);
    }
    paths.sort();
    paths.dedup();
    paths
}

fn watch_checkout() {
    for path in checkout_watch_paths(Path::new(".")) {
        println!("cargo:rerun-if-changed={}", path.display());
    }
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
            git(Path::new("."), &args).unwrap_or_else(unknown)
        );
    }
}
