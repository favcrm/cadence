//! CAD-1601: `cadence issue recover review|decide|execute|status` — the
//! operator recovery of an interrupted tracker write. Every verb goes
//! through the daemon (`client::rpc`): the daemon proves the operator on
//! decide/execute, and this CLI never opens a lease-free tracker handle.

use std::path::{Path, PathBuf};

use clap::Subcommand;
use serde_json::{json, Value};

use crate::client;
use crate::error::{Error, Result};

use super::print_json;

#[derive(Subcommand)]
pub enum RecoverAction {
    /// Read-only, complete review of an interrupted tracker write:
    /// affected paths with full content, protected foreign paths, the
    /// blocked operation, and global blockers. Prints the review JSON;
    /// pass it to `decide` and `execute` unchanged. Changes nothing.
    Review {
        /// The blocked operation being recovered, named for the review.
        #[arg(long)]
        operation: Option<String>,
    },
    /// Preview a Keep, Discard or Leave of the listed literal paths
    /// against a review. Operator only. Prints the decision JSON; no
    /// tracker state changes.
    Decide {
        /// The review JSON printed by `recover review`.
        #[arg(long)]
        review: PathBuf,
        /// What to do with the selected paths.
        #[arg(long, value_parser = ["keep", "discard", "leave"])]
        choice: String,
        /// A literal repo-relative path to act on; repeat for each path.
        #[arg(long = "path", required = true)]
        path: Vec<String>,
    },
    /// Apply a previewed decision. Operator only. The daemon re-checks
    /// the reviewed state under the tracker lock before any change. Prints
    /// the attempt JSON; pass it to `status`.
    Execute {
        /// The review JSON printed by `recover review`.
        #[arg(long)]
        review: PathBuf,
        /// The decision JSON printed by `recover decide`.
        #[arg(long)]
        decision: PathBuf,
        /// The same choice as the decision.
        #[arg(long, value_parser = ["keep", "discard", "leave"])]
        choice: String,
        /// The same literal paths as the decision; repeat for each path.
        #[arg(long = "path", required = true)]
        path: Vec<String>,
    },
    /// The truthful outcome of an attempt and the blockers that remain.
    /// Read-only; safe to run after a failed or unknown `execute`.
    Status {
        /// The attempt JSON printed by `recover execute`.
        #[arg(long)]
        attempt: PathBuf,
    },
}

/// `cadence issue recover …` — every verb is a daemon RPC.
pub fn run_recover(action: &RecoverAction, state_dir: &Path) -> Result<i32> {
    let out = match action {
        RecoverAction::Review { operation } => client::rpc(
            state_dir,
            "tracker_recovery_review",
            json!({"blocked_operation": operation}),
        )?,
        RecoverAction::Decide {
            review,
            choice,
            path,
        } => client::rpc(
            state_dir,
            "tracker_recovery_decide",
            json!({
                "review": read_json_file(review, "--review")?,
                "choice": choice,
                "paths": path,
            }),
        )?,
        RecoverAction::Execute {
            review,
            decision,
            choice,
            path,
        } => client::rpc(
            state_dir,
            "tracker_recovery_execute",
            json!({
                "review": read_json_file(review, "--review")?,
                "decision": read_json_file(decision, "--decision")?,
                "choice": choice,
                "paths": path,
            }),
        )?,
        RecoverAction::Status { attempt } => client::rpc(
            state_dir,
            "tracker_recovery_status",
            json!({"attempt": read_json_file(attempt, "--attempt")?}),
        )?,
    };
    print_json(&out);
    Ok(0)
}

/// Read a JSON file that an earlier recovery step printed. Every failure
/// names the flag, the file, the cause and the next action; none panics.
fn read_json_file(path: &Path, flag: &str) -> Result<Value> {
    let shown = path.display();
    let text = std::fs::read_to_string(path).map_err(|e| {
        Error::rejected(format!(
            "tracker recovery: cannot read {flag} file '{shown}' ({e}); nothing \
             was sent to the daemon. Next: pass the file the previous recovery \
             step printed to stdout, then retry; a retry helps once the path is \
             readable."
        ))
    })?;
    serde_json::from_str(&text).map_err(|e| {
        Error::rejected(format!(
            "tracker recovery: {flag} file '{shown}' is not valid JSON ({e}); \
             nothing was sent to the daemon. Next: regenerate it by rerunning the \
             previous recovery step and saving its stdout unchanged, then retry."
        ))
    })
}
