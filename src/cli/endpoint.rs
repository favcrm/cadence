//! CAD-1857: `cadence endpoint` — the agent ingress endpoint registry.
//!
//! An endpoint is one agent's receiving identity (email, webhook or
//! schedule) bound to exactly one stored workflow. It is created disabled
//! and unverified; `verify` and `enable` are separate operator steps.
//! This verb only writes the local registry; ingress and wake are separate.
//!
//! Every command goes through the daemon: the daemon holds the store and the
//! rollout lease, so a CLI open would risk migrating under a live daemon.

use super::*;

#[derive(Subcommand)]
pub(crate) enum EndpointAction {
    /// Create a disabled, unverified endpoint. On a local daemon binds one
    /// stored workflow (`--project`/`--workflow`); on a hosted org targets
    /// either an isolated inbox or an approved Event Routine (`--target-kind`).
    Add {
        /// Agent alias (or agent uid on a hosted org).
        agent: String,
        /// email, webhook or schedule.
        #[arg(long)]
        kind: String,
        /// Stable per-endpoint local part/URL name (1-20 lowercase
        /// letters, digits or hyphens).
        #[arg(long)]
        slug: String,
        /// Project key that owns the workflow (required on local daemon).
        #[arg(long)]
        project: Option<String>,
        /// Stored workflow name under `<project>/workflows/` (required on local daemon).
        #[arg(long)]
        workflow: Option<String>,
        /// Hosted target kind: `inbox` (default) or `routine`.
        #[arg(long = "target-kind")]
        target_kind: Option<String>,
        /// Hosted Event Routine id when `--target-kind routine`.
        #[arg(long)]
        routine: Option<String>,
        /// Hosted webhook auth mode: `bearer` or `hmac_sha256`.
        #[arg(long = "auth-mode")]
        auth_mode: Option<String>,
        /// Hosted email sender allowlist entries (repeatable).
        #[arg(long = "allow-sender")]
        allow_sender: Vec<String>,
        /// Hosted webhook rate limit (1-600 requests per minute).
        #[arg(long = "rate-limit-rpm")]
        rate_limit_rpm: Option<u32>,
        /// Hosted paused delivery hold capacity (0-500).
        #[arg(long = "hold-capacity")]
        hold_capacity: Option<u32>,
    },
    /// Every registered endpoint, optionally filtered by agent or state.
    #[command(visible_alias = "ls")]
    List {
        #[arg(long)]
        agent: Option<String>,
        #[arg(long)]
        state: Option<String>,
    },
    /// One endpoint by id (hosted orgs also return recent deliveries).
    Show { id: String },
    /// Mark the endpoint verified. It stays disabled until `enable`.
    Verify { id: String },
    /// Enable an endpoint. Refuses until `verify` has run.
    Enable { id: String },
    /// Pause an active hosted endpoint (holds deliveries within hold capacity).
    Pause { id: String },
    /// Disable an endpoint. Always allowed.
    Disable { id: String },
    /// Rotate a hosted webhook secret (returns the new secret once with 15m overlap).
    #[command(name = "rotate-secret")]
    RotateSecret { id: String },
    /// Read bounded recent deliveries for a hosted endpoint.
    Deliveries {
        id: String,
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
    /// Remove the endpoint row.
    #[command(visible_alias = "rm")]
    Remove { id: String },
}

pub(crate) fn run(state_dir: PathBuf, action: EndpointAction) -> Result<i32> {
    run_endpoint(&state_dir, &action)
}

fn run_endpoint(state_dir: &Path, action: &EndpointAction) -> Result<i32> {
    let rpc = |method: &str, params: Value| client::rpc(state_dir, method, params);
    match action {
        EndpointAction::Add {
            agent,
            kind,
            slug,
            project,
            workflow,
            target_kind,
            routine,
            auth_mode,
            allow_sender,
            rate_limit_rpm,
            hold_capacity,
        } => {
            if target_kind.is_some()
                || routine.is_some()
                || auth_mode.is_some()
                || !allow_sender.is_empty()
                || rate_limit_rpm.is_some()
                || hold_capacity.is_some()
            {
                return Err(Error::usage(
                    "hosted options (--target-kind, --routine, --auth-mode, --allow-sender, --rate-limit-rpm, --hold-capacity) require `--org <org>`",
                ));
            }
            let Some(project) = project.as_deref() else {
                return Err(Error::usage(
                    "local `cadence endpoint add` requires `--project <key>`",
                ));
            };
            let Some(workflow) = workflow.as_deref() else {
                return Err(Error::usage(
                    "local `cadence endpoint add` requires `--workflow <name>`",
                ));
            };
            print_json(&rpc(
                "endpoint_add",
                serde_json::json!({
                    "agent": agent,
                    "kind": kind,
                    "slug": slug,
                    "project": project,
                    "workflow": workflow,
                }),
            )?);
        }
        EndpointAction::List { agent, state } => print_json(&rpc(
            "endpoint_list",
            serde_json::json!({"agent": agent, "state": state}),
        )?),
        EndpointAction::Show { id } => {
            print_json(&rpc("endpoint_show", serde_json::json!({"id": id}))?)
        }
        EndpointAction::Verify { id } => {
            print_json(&rpc("endpoint_verify", serde_json::json!({"id": id}))?)
        }
        EndpointAction::Enable { id } => {
            print_json(&rpc("endpoint_enable", serde_json::json!({"id": id}))?)
        }
        EndpointAction::Disable { id } => {
            print_json(&rpc("endpoint_disable", serde_json::json!({"id": id}))?)
        }
        EndpointAction::Remove { id } => {
            print_json(&rpc("endpoint_remove", serde_json::json!({"id": id}))?)
        }
        EndpointAction::Pause { .. }
        | EndpointAction::RotateSecret { .. }
        | EndpointAction::Deliveries { .. } => {
            return Err(Error::rejected(
                "`endpoint pause`, `rotate-secret`, and `deliveries` are supported on hosted orgs (`--org <org>`)",
            ));
        }
    }
    Ok(0)
}
