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
    /// Create a disabled, unverified endpoint bound to one workflow. The
    /// workflow source digest is pinned now; a later workflow edit does not
    /// silently change this endpoint.
    Add {
        /// Agent alias; resolved to the durable uid.
        agent: String,
        /// email, webhook or schedule.
        #[arg(long)]
        kind: String,
        /// Stable per-endpoint local part/URL name (1-20 lowercase
        /// letters, digits or hyphens).
        #[arg(long)]
        slug: String,
        /// Project key that owns the workflow.
        #[arg(long)]
        project: String,
        /// Stored workflow name under `<project>/workflows/`.
        #[arg(long)]
        workflow: String,
    },
    /// Every registered endpoint, optionally filtered by agent or state.
    #[command(visible_alias = "ls")]
    List {
        #[arg(long)]
        agent: Option<String>,
        #[arg(long)]
        state: Option<String>,
    },
    /// One endpoint by id.
    Show { id: String },
    /// Mark the endpoint verified. It stays disabled until `enable`.
    Verify { id: String },
    /// Enable an endpoint. Refuses until `verify` has run.
    Enable { id: String },
    /// Disable an endpoint. Always allowed.
    Disable { id: String },
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
        } => print_json(&rpc(
            "endpoint_add",
            serde_json::json!({
                "agent": agent,
                "kind": kind,
                "slug": slug,
                "project": project,
                "workflow": workflow,
            }),
        )?),
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
    }
    Ok(0)
}
