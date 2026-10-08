//! CAD-1230 independent acceptance check (written by the reviewer, not the
//! implementer; AGENTS.md "Gates and security work").
//!
//! The fix lets a REPLAYED start return a run that went terminal between
//! the replay's read and its dispatch. The approval gate must not move: a
//! replay of a run that is not terminal and has no approval is still
//! refused and is never dispatched.

use super::cad1120_tests::{refusal, Fx};
use super::*;

#[test]
fn cad1230_a_replay_never_dispatches_an_unapproved_run() {
    let fx = Fx::new();
    fx.start_team();
    // Created, not approved: awaiting_approval, not terminal.
    let run = fx.create("cad1230-unapproved").unwrap();
    assert_eq!(run["state"], "awaiting_approval", "{run}");
    let id = run["id"].as_str().unwrap().to_string();

    let replay = fx.shared.dispatch_started_run(&id, true);
    assert!(
        refusal(replay).contains("approval is absent or stale"),
        "a replay must not bypass the approval gate"
    );
    let shown = fx.shared.store.app_run_show(&id).unwrap();
    assert_eq!(
        shown["state"], "awaiting_approval",
        "the refused replay must leave the run untouched: {shown}"
    );
}
