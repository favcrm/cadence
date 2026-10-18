//! The current caller's PM issues, using the existing board identity and read model.
//! This is a personal filter, not a project-visibility or execution grant.

use std::borrow::Cow;
use std::collections::BTreeSet;
use std::path::Path;

use serde_json::{json, Value};
use tiny_http::Request;

use crate::issue::Pm;

use super::{operator, project_policy, read_model, HttpResp, ServeOpts};

/// Keep the allowed read projection across the board's final live-session check.
pub(super) struct Snapshot {
    pm: Pm,
    readable: BTreeSet<String>,
    board: read_model::BoardRead,
}

impl Snapshot {
    pub(super) fn load(state_dir: &Path, pm_dir: &Path, policy_actor: &str) -> crate::Result<Self> {
        let pm = Pm::at(pm_dir)?;
        let readable = project_policy::readable(&pm, policy_actor);
        let board = read_model::get(state_dir, pm_dir).board_readable(&pm, &readable);
        Ok(Self {
            pm,
            readable,
            board,
        })
    }

    pub(super) fn payload(self, owner: &str, policy_actor: &str) -> Option<Value> {
        // A blocking read or session check may outlive a project grant.
        if project_policy::readable(&self.pm, policy_actor) != self.readable {
            return None;
        }
        let by_id = self.board.by_id();
        let ctx = self.board.ctx(&by_id);
        let mut body = json!({
            "owner": owner,
            "issues": self.board.views.iter()
                .filter(|view| view.issue.front.owner.as_deref() == Some(owner))
                .map(|view| self.board.card(&ctx, view))
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
    let policy_actor = if is_owner(&caller) {
        Cow::Borrowed("operator")
    } else {
        Cow::Owned(operator::caller_spelling(&caller))
    };
    let snapshot = match Snapshot::load(state_dir, pm_dir, &policy_actor) {
        Ok(snapshot) => snapshot,
        Err(error) => return super::err_response(503, &error.to_string()),
    };
    // Tracker/daemon reads may await I/O. Never release the result after a
    // membership withdrawal or a caller change during that read.
    match operator::board_caller(request, state_dir, opts, false, None) {
        Ok(current)
            if !matches!(current, operator::Caller::Agent(_))
                && current.author() == caller.author()
                && is_owner(&current) == is_owner(&caller) =>
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
