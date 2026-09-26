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
        #[arg(long, default_value_t = 0)]
        after: i64,
        #[arg(long)]
        open: bool,
        #[arg(long, default_value_t = 100)]
        limit: i64,
        /// Sort by seq monitor task kind state created updated;
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
        #[arg(long)]
        alert: i64,
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
            let mut out = rpc(
                "monitor_alerts",
                json!({"monitor": monitor, "after": after,
                       "open": open, "limit": limit}),
            )?;
            shape_rows(
                &mut out,
                "alerts",
                sort.as_deref(),
                &[
                    ("seq", "seq"),
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
        MonitorAction::Ack { monitor, alert } => {
            print_json(&rpc(
                "monitor_alert_ack",
                json!({"monitor": monitor, "alert": alert,
                       "by": pane.as_deref()}),
            )?);
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
