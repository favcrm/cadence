//! CAD-139: `cadence idea decide` — the operator's gate.

use super::*;

/// The operator's decision on a researched idea. Exactly one of
/// `--approve`, `--reject`, or `--park`.
#[derive(Subcommand)]
pub(crate) enum IdeaAction {
    /// Approve, reject, or park a plan that is waiting.
    Decide {
        /// The idea's issue id.
        issue: String,
        /// Create the proposed backlog children and close the idea as planned.
        #[arg(long, conflicts_with_all = ["reject", "park"])]
        approve: bool,
        /// Close the idea. Requires `--reason`.
        #[arg(long, conflicts_with_all = ["approve", "park"])]
        reject: bool,
        /// Why the idea was rejected. Recorded on the decision object.
        #[arg(long, requires = "reject")]
        reason: Option<String>,
        /// Leave the plan until `--until`, then surface it again.
        #[arg(long, conflicts_with_all = ["approve", "reject"])]
        park: bool,
        /// UTC date `YYYY-MM-DD` the park lasts until.
        #[arg(long, requires = "park")]
        until: Option<String>,
    },
}

pub(super) fn run(state_dir: &Path, action: IdeaAction) -> Result<i32> {
    let IdeaAction::Decide {
        issue,
        approve,
        reject,
        reason,
        park,
        until,
    } = action;
    let action = if approve {
        "approve"
    } else if reject {
        "reject"
    } else if park {
        "park"
    } else {
        return Err(Error::rejected(
            "idea decide needs exactly one of --approve, --reject, or --park",
        ));
    };
    let mut params = json!({ "issue": issue, "action": action });
    if let Some(reason) = reason {
        params["reason"] = json!(reason);
    }
    if let Some(until) = until {
        params["park_until"] = json!(until);
    }
    let out = client::rpc(state_dir, "idea_decide", params)?;
    print_json(&out);
    Ok(0)
}
