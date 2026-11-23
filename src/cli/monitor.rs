//! CAD-535: `cadence monitor` — moved verbatim from src/main.rs.

use super::*;

#[derive(Subcommand)]
pub(crate) enum MonitorAction {
    /// Register an explicit project/task coverage set. Delivery remains
    /// local and unconfigured; --dispatch only enables the guarded manual
    /// handoff into existing job dispatch. Automatic reconciliation requires
    /// the separate --auto-dispatch opt-in as well.
    Register {
        monitor: String,
        #[arg(long)]
        project: String,
        #[arg(long = "task", required = true)]
        tasks: Vec<String>,
        #[arg(long, default_value_t = 60, value_parser = clap::value_parser!(u64).range(1..=86400))]
        interval_secs: u64,
        #[arg(long)]
        owner: Option<String>,
        #[arg(long)]
        dispatch: bool,
        /// Opt into the background coordinator after every existing manual
        /// dispatch guard passes. This never bypasses approval, readiness,
        /// identity, queue, or quota checks.
        #[arg(long)]
        auto_dispatch: bool,
    },
    /// List persistent monitor registrations and their separate delivery state.
    #[command(after_long_help = cadence_agent::filter::GRAMMAR)]
    #[command(visible_alias = "ls")]
    List {
        /// Sort by id project owner monitoring heartbeat_at created
        /// updated open_alerts; `-KEY` descending.
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
    /// Show one monitor's heartbeat, cursor, coverage, and alert counts.
    Show { monitor: String },
    /// Record an explicit caller heartbeat; this is not a worker-health claim.
    Heartbeat { monitor: String },
    /// List durable local alerts for one monitor.
    #[command(after_long_help = cadence_agent::filter::GRAMMAR)]
    Alerts {
        monitor: String,
        /// Opaque continuation token from the previous page. A scoped
        /// reader never continues with a numeric position; the operator's
        /// legacy numeric cursor is still accepted.
        #[arg(long, default_value_t = String::new())]
        after: String,
        #[arg(long)]
        open: bool,
        #[arg(long, default_value_t = 100)]
        limit: i64,
        /// Sort by seq id monitor task kind state created updated;
        /// `-KEY` descending.
        #[arg(long, allow_hyphen_values = true)]
        sort: Option<String>,
        /// Keep only these keys in each row (comma-joined).
        #[arg(long, value_delimiter = ',')]
        fields: Vec<String>,
        /// Output is JSON already — accepted for grammar parity.
        #[arg(long)]
        json: bool,
    },
    /// Acknowledge one local alert.
    Ack {
        monitor: String,
        /// The opaque ack token from a scoped `monitor alerts` row.
        #[arg(long)]
        ack: Option<String>,
        /// The operator's numeric alert position (legacy).
        #[arg(long)]
        alert: Option<i64>,
    },
    /// Turn monitoring off for this registration. History is retained.
    Stop { monitor: String },
    /// Explicitly pass one covered, eligible task to existing job dispatch.
    Dispatch { monitor: String, task: String },
}

/// The `cadence monitor` tree — thin RPC wrappers. Monitor state and
/// safety checks live in the daemon so every caller sees one contract.
pub(super) fn run_monitor(state_dir: &Path, action: &MonitorAction) -> Result<i32> {
    let rpc = |method: &str, params: Value| client::rpc(state_dir, method, params);
    let pane = std::env::var("CADENCE_ALIAS").ok();
    match action {
        MonitorAction::Register {
            monitor,
            project,
            tasks,
            interval_secs,
            owner,
            dispatch,
            auto_dispatch,
        } => {
            print_json(&rpc(
                "monitor_register",
                json!({"monitor": monitor, "project": project,
                       "tasks": tasks, "interval_secs": interval_secs,
                       "owner": owner.as_deref().or(pane.as_deref()),
                       "dispatch_enabled": dispatch,
                       "auto_dispatch_enabled": auto_dispatch}),
            )?);
        }
        MonitorAction::List {
            sort,
            limit,
            fields,
            json: _,
        } => {
            let mut out = rpc("monitor_list", json!({}))?;
            shape_rows(
                &mut out,
                "monitors",
                sort.as_deref(),
                &[
                    ("id", "id"),
                    ("project", "project"),
                    ("owner", "owner"),
                    ("monitoring", "monitoring"),
                    ("heartbeat_at", "heartbeat_at"),
                    ("open_alerts", "open_alerts"),
                    ("created", "created"),
                    ("updated", "updated"),
                ],
                "id",
                *limit,
                fields,
            )?;
            print_json(&out);
        }
        MonitorAction::Show { monitor } => {
            print_json(&rpc("monitor_show", json!({"monitor": monitor}))?);
        }
        MonitorAction::Heartbeat { monitor } => {
            print_json(&rpc("monitor_heartbeat", json!({"monitor": monitor}))?);
        }
        MonitorAction::Alerts {
            monitor,
            after,
            open,
            limit,
            sort,
            fields,
            json: _,
        } => {
            let mut params = json!({
                "monitor": monitor,
                "open": open,
                "limit": limit,
                // A scoped reader must name the opaque page format; the
                // operator's numeric reply ignores it.
                "cursor_format": cadence_core::board_api::MONITOR_CURSOR_FORMAT,
            });
            if !after.is_empty() {
                // A numeric value stays the operator's legacy cursor; it is
                // forwarded as a number so a scoped reader is refused rather
                // than silently reset. Everything else is an opaque token.
                params["after"] = match after.parse::<i64>() {
                    Ok(position) => json!(position),
                    Err(_) => json!(after),
                };
            }
            let mut out = rpc("monitor_alerts", params)?;
            if out[cadence_core::board_api::READER_SCOPE_FIELD].as_str()
                != Some(cadence_core::board_api::MONITOR_OPERATOR_SCOPE_ACK)
            {
                cadence_agent::overview::monitoring::validate_scoped_monitor_alert_response(
                    monitor,
                    &out,
                )
                .map_err(|error| Error::rejected(format!(
                    "monitor alerts: the daemon did not confirm a valid scoped or operator page ({error}); no page was printed"
                )))?;
            }
            shape_rows(
                &mut out,
                "alerts",
                sort.as_deref(),
                &[
                    ("seq", "seq"),
                    ("id", "id"),
                    ("monitor", "monitor"),
                    ("task", "task"),
                    ("kind", "kind"),
                    ("state", "state"),
                    ("created", "created"),
                    ("updated", "updated"),
                ],
                "seq",
                None,
                fields,
            )?;
            print_json(&out);
        }
        MonitorAction::Ack {
            monitor,
            ack,
            alert,
        } => {
            let mut params = json!({"monitor": monitor, "by": pane.as_deref()});
            match (ack.as_deref(), *alert) {
                (Some(token), None) => params["ack"] = json!(token),
                (None, Some(position)) => params["alert"] = json!(position),
                _ => {
                    return Err(Error::rejected(
                        "monitor ack requires exactly one of --ack (scoped token) or --alert (operator position)",
                    ))
                }
            }
            print_json(&rpc("monitor_alert_ack", params)?);
        }
        MonitorAction::Stop { monitor } => {
            print_json(&rpc("monitor_stop", json!({"monitor": monitor}))?);
        }
        MonitorAction::Dispatch { monitor, task } => {
            print_json(&rpc(
                "monitor_dispatch",
                json!({"monitor": monitor, "task": task}),
            )?);
        }
    }
    Ok(0)
}

pub(super) fn run(state_dir: PathBuf, action: MonitorAction) -> Result<i32> {
    run_monitor(&state_dir, &action)
}
