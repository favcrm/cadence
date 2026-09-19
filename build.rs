//! Compile the build's git identity into the binary: the deploy-drift
//! check compares the *running daemon's* commit against the tracker's
//! default branch, and `daemon_info`/`/api/meta`/`--version` report it.
//! Every value falls back to `unknown` when git or a repo is absent —
//! consumers treat `unknown` as "cannot tell", never as zero.

use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}

fn main() {
    // No rerun-if directives: cargo's conservative default re-runs the
    // script on any package change, keeping the commit fresh.
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
