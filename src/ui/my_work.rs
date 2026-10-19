//! The current caller's PM issues, using the existing board identity and read model.
//! This is a personal filter, not a project-visibility or execution grant.

use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::time::Duration;

use serde_json::{json, Value};
use tiny_http::Request;

use crate::issue::{history, Pm};

use super::{operator, project_policy, read_model, HttpResp, ServeOpts};

/// How long the one tracker history walk may take, and how many
/// git reads the before/after owner proofs may make across every
/// card. Past the bound the affected cards carry `null` — assignment
/// provenance is never a refusal.
const PROVENANCE_WALK_TIMEOUT: Duration = Duration::from_secs(5);
const PROVENANCE_SHOW_BUDGET: usize = 128;

/// Keep the allowed read projection across the board's final live-session check.
pub(super) struct Snapshot {
    pm: Pm,
    readable: BTreeSet<String>,
    board: read_model::BoardRead,
    /// `<project>/<id>` → that issue's assignment context: the proven
    /// commit that gave it its current owner, or `null` when history
    /// cannot prove one (see [`history::owner_assignments`]).
    assignments: HashMap<String, Value>,
}

impl Snapshot {
    /// `owner` is the caller-derived handle this read answers for and
    /// `subject` the caller's verified platform `sub` when the session
    /// carries one — session identities, never request fields — so the
    /// one history walk runs scoped to the caller's own issues inside
    /// the project-scoped board, before the caller's live-session
    /// recheck.
    pub(super) fn load(
        state_dir: &Path,
        pm_dir: &Path,
        owner: &str,
        subject: Option<&str>,
        policy_actor: &str,
    ) -> crate::Result<Self> {
        let pm = Pm::at(pm_dir)?;
        let readable = project_policy::readable(&pm, policy_actor);
        let board = read_model::get(state_dir, pm_dir).board_readable(&pm, &readable);
        // The caller's own cards, in board order — provenance proofs
        // spend the shared budget in this same order every time.
        let owned: Vec<(String, &str)> = board
            .views
            .iter()
            .filter(|view| view.issue.front.owner.as_deref() == Some(owner))
            .map(|view| {
                (
                    format!("{}/{}", view.issue.project, view.issue.front.id),
                    owner,
                )
            })
            .collect();
        let assignments = history::owner_assignments(
            pm_dir,
            &owned,
            subject,
            PROVENANCE_WALK_TIMEOUT,
            PROVENANCE_SHOW_BUDGET,
        );
        Ok(Self {
            pm,
            readable,
            board,
            assignments,
        })
    }

    pub(super) fn payload(mut self, owner: &str, policy_actor: &str) -> Option<Value> {
        // A blocking read or session check may outlive a project grant.
        if project_policy::readable(&self.pm, policy_actor) != self.readable {
            return None;
        }
        let by_id = self.board.by_id();
        let ctx = self.board.ctx(&by_id);
        let mut assignments = std::mem::take(&mut self.assignments);
        let mut body = json!({
            "owner": owner,
            "issues": self.board.views.iter()
                .filter(|view| view.issue.front.owner.as_deref() == Some(owner))
                .map(|view| {
                    let mut card = self.board.card(&ctx, view);
                    let rel =
                        format!("{}/{}", view.issue.project, view.issue.front.id);
                    card["assignment"] =
                        assignments.remove(&rel).unwrap_or(Value::Null);
                    card
                })
                .collect::<Vec<_>>(),
        });
        read_model::merge_freshness(&mut body, self.board.freshness);
        Some(body)
    }
}

fn is_owner(caller: &operator::Caller) -> bool {
    matches!(caller, operator::Caller::Operator(_))
        || matches!(caller, operator::Caller::Named(named) if named.operator)
}

pub(super) fn get(
    request: &Request,
    state_dir: &Path,
    pm_dir: &Path,
    opts: &ServeOpts,
    raw_query: &str,
) -> HttpResp {
    // No owner, subject, role or project selector: the session alone names "my".
    if !raw_query.is_empty() {
        return super::err_response(400, "My work does not accept query parameters");
    }
    let caller = match operator::board_caller(request, state_dir, opts, false, None) {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    if matches!(caller, operator::Caller::Agent(_)) {
        return super::err_response(403, "My work requires a human board session");
    }
    // The verified platform subject a `Named` session carries — what
    // a write's `Activity-Receipt` recorded verbatim. The shared
    // operator session has none, so its `self_assigned` is never true.
    let subject = match &caller {
        operator::Caller::Named(named) => Some(named.sub.as_str()),
        _ => None,
    };
    let policy_actor = if is_owner(&caller) {
        Cow::Borrowed("operator")
    } else {
        Cow::Owned(operator::caller_spelling(&caller))
    };
    let snapshot = match Snapshot::load(state_dir, pm_dir, caller.author(), subject, &policy_actor)
    {
        Ok(snapshot) => snapshot,
        Err(error) => return super::err_response(503, &error.to_string()),
    };
    // Tracker/daemon reads may await I/O. Never release the result after a
    // membership withdrawal or a caller change during that read.
    match operator::board_caller(request, state_dir, opts, false, None) {
        Ok(current)
            if !matches!(current, operator::Caller::Agent(_))
                && current.author() == caller.author()
                && is_owner(&current) == is_owner(&caller)
                && (match &current {
                    operator::Caller::Named(named) => Some(named.sub.as_str()),
                    _ => None,
                }) == subject =>
        {
            match snapshot.payload(current.author(), &policy_actor) {
                Some(body) => super::json_response(body),
                None => super::err_response(403, "My work project access changed; refresh"),
            }
        }
        Ok(_) => super::err_response(403, "My work session changed; sign in again"),
        Err(response) => response,
    }
}
