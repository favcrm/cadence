//! CAD-535: `cadence delivery` — moved verbatim from src/main.rs.

use super::*;

#[derive(Subcommand)]
pub(crate) enum DeliveryAction {
    /// Every ticket in the loop and where it stands. Value flags
    /// repeat and comma-join and match ANY of their values; different
    /// flags AND.
    #[command(after_long_help = cadence_agent::filter::GRAMMAR)]
    Ls {
        /// Only this ticket — `delivery ls X-1` is `--issue X-1`.
        issue: Option<String>,
        /// Ticket ids; repeatable — any of them (joins the
        /// positional).
        #[arg(long = "issue", value_delimiter = ',')]
        issues: Vec<String>,
        /// Loop state (working reviewing unstaffed passed enqueued
        /// escalated merged declined closed); repeatable.
        #[arg(long, value_delimiter = ',')]
        state: Vec<String>,
        /// Tracker project key; repeatable.
        #[arg(long, value_delimiter = ',')]
        project: Vec<String>,
        /// Only rows still in the loop (not merged/declined/closed).
        #[arg(long)]
        open: bool,
        /// Sort by issue project state worker reviewer since
        /// dispatched_at; `-KEY` descending [default: dispatched_at].
        #[arg(long, allow_hyphen_values = true)]
        sort: Option<String>,
        /// Keep only the first N rows.
        #[arg(long)]
        limit: Option<usize>,
        /// Keep only these keys in each row (comma-joined).
        #[arg(long, value_delimiter = ',')]
        fields: Vec<String>,
        /// Output is JSON already — accepted for grammar parity.
        #[arg(long)]
        json: bool,
    },
    /// Read each open PR in the loop from GitHub (head, CI, diff stats)
    /// and hand it to the daemon; turn auto-merge off where the head
    /// moved past what was reviewed. Operator only. `--watch <secs>`
    /// repeats until interrupted.
    Sync {
        /// Only this ticket.
        issue: Option<String>,
        /// Repeat every N seconds.
        #[arg(long)]
        watch: Option<u64>,
    },
    /// Merge a PASSed ticket: enqueue its PR in the merge queue pinned
    /// to the reviewed head (`gh pr merge --auto --squash
    /// --match-head-commit`). Operator only.
    Merge {
        /// The ticket id.
        issue: String,
    },
    /// Decline the merge decision (or an escalated review) with a
    /// reason. Operator only.
    Decline {
        /// The ticket id.
        issue: String,
        /// Why.
        #[arg(long)]
        reason: String,
    },
}

pub(super) fn run_delivery(state_dir: &Path, action: DeliveryAction) -> Result<i32> {
    use cadence_agent::delivery;
    let result = match action {
        DeliveryAction::Ls {
            issue,
            issues,
            state,
            project,
            open,
            sort,
            limit,
            fields,
            json: _,
        } => {
            let mut all = issues.clone();
            if let Some(one) = &issue {
                all.push(one.clone());
            }
            // The grammar: an unknown --project is an error naming the
            // valid set, never an empty page. The daemon re-checks.
            if !project.is_empty() {
                let pm = cadence_agent::issue::Pm::open_default()?;
                let keys: Vec<String> = cadence_agent::issue::project::list(&pm.dir)?
                    .into_iter()
                    .map(|p| p.key)
                    .collect();
                for want in &project {
                    if !keys.iter().any(|k| k == want) {
                        return Err(Error::rejected(format!(
                            "Unknown --project '{want}' — known: {}",
                            keys.join(" ")
                        )));
                    }
                }
            }
            let mut out = client::rpc(
                state_dir,
                "delivery_list",
                json!({"issues": all, "states": state, "projects": project,
                       "open": open}),
            )?;
            shape_rows(
                &mut out,
                "records",
                sort.as_deref(),
                &[
                    ("issue", "issue"),
                    ("project", "project"),
                    ("state", "state"),
                    ("worker", "worker"),
                    ("reviewer", "reviewer"),
                    ("since", "since"),
                    ("dispatched_at", "dispatched_at"),
                ],
                "issue",
                limit,
                &fields,
            )?;
            out
        }
        DeliveryAction::Sync { issue, watch } => match watch {
            None => delivery::sync(state_dir, issue.as_deref(), delivery::GH)?,
            Some(secs) => loop {
                match delivery::sync(state_dir, issue.as_deref(), delivery::GH) {
                    Ok(v) => println!("{v}"),
                    Err(e) => eprintln!("delivery sync: {e}"),
                }
                std::thread::sleep(std::time::Duration::from_secs(secs.max(5)));
            },
        },
        DeliveryAction::Merge { issue } => delivery::merge(state_dir, &issue, delivery::GH)?,
        DeliveryAction::Decline { issue, reason } => client::rpc(
            state_dir,
            "delivery_decline",
            json!({"issue": issue, "reason": reason}),
        )?,
    };
    print_json(&result);
    Ok(0)
}

pub(super) fn run(state_dir: PathBuf, action: DeliveryAction) -> Result<i32> {
    run_delivery(&state_dir, action)
}
