//! CAD-1853: explicit native connection to a cloud agent.
//!
//! This is a distinct external connection, not `agent connect` (hosted
//! local-agent pickup) and not a hire/location picker. The saved hosted
//! login for the exact org supplies the credential custody; the daemon
//! performs the contract call so no caller ever reads the credential.
use clap::Subcommand;
use serde_json::{json, Value};

use super::{print_json, Error, Result};

#[derive(Subcommand)]
pub(crate) enum CloudAgentAction {
    /// Connect one hosted workspace's cloud agent; explicit consent required.
    Connect {
        /// Saved hosted workspace slug (from `cadence org list`).
        #[arg(long)]
        workspace: String,
        /// Local alias for the canonical Threads work conversation.
        #[arg(long, default_value = "cloud")]
        alias: String,
        /// Explicitly consent to cloud_agent.read/cloud_agent.write model turns.
        #[arg(long)]
        accept_assistant_consent: bool,
    },
    /// List this machine's cloud-agent connections and their truthful state.
    #[command(visible_alias = "list")]
    Ls,
    /// Show identity, runtime, capabilities, sessions and the Threads link.
    Show { cloud_agent_id: String },
    /// Send one request (or explicitly wake a queued one) and read the reply.
    Send {
        cloud_agent_id: String,
        /// Canonical work-conversation thread id; defaults to the agent's home thread.
        #[arg(long)]
        thread_id: Option<String>,
        /// Stable idempotency key. Reusing it with different text is refused.
        #[arg(long)]
        operation_id: Option<String>,
        #[arg(long)]
        text: String,
        /// Queue only; do not invoke the model now.
        #[arg(long)]
        no_wake: bool,
    },
    /// Enroll one local managed peer (split-UID only) as a cloud invoker.
    EnrollPeer {
        cloud_agent_id: String,
        /// Local agent alias whose own process is on the split agent UID boundary.
        #[arg(long)]
        agent: String,
    },
    /// Read a durable operation back; readback never invokes the model.
    Read {
        cloud_agent_id: String,
        operation_id: String,
    },
    /// Explicitly ask to cancel; the contract is expected to refuse unsupported.
    Cancel {
        cloud_agent_id: String,
        operation_id: String,
    },
    /// Stop admitting new turns locally and drop the explicit consent.
    Disconnect { cloud_agent_id: String },
}

fn new_operation_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

fn saved_target(org: &str) -> Result<cadence_agent::remote_cli::RemoteTarget> {
    let targets = cadence_agent::remote_cli::account_image_targets()?;
    targets
        .into_iter()
        .find(|target| target.org == org)
        .ok_or_else(|| {
            Error::rejected(format!(
                "org '{org}' has no saved remote login on this host — run `cadence org list` \
                 and `cadence login --org {org}` first; nothing was sent"
            ))
        })
}

fn home_thread(state_dir: &std::path::Path, id: &str) -> Result<String> {
    let shown = super::client::rpc(state_dir, "cloud_agent_show", json!({"cloud_agent_id": id}))?;
    shown["agent"]["thread"]["id"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| Error::rejected("cloud agent has no canonical work conversation"))
}

pub(super) fn run(state_dir: std::path::PathBuf, action: CloudAgentAction) -> Result<i32> {
    let state = state_dir;
    let value: Value = match action {
        CloudAgentAction::Connect {
            workspace,
            alias,
            accept_assistant_consent,
        } => {
            let target = saved_target(&workspace)?;
            super::client::rpc(
                &state,
                "cloud_agent_connect",
                json!({
                    "org": target.org,
                    "org_id": target.org_id,
                    "endpoint": target.endpoint,
                    "alias": alias,
                    "accept_assistant_consent": accept_assistant_consent,
                }),
            )?
        }
        CloudAgentAction::Ls => super::client::rpc(&state, "cloud_agent_list", json!({}))?,
        CloudAgentAction::Show { cloud_agent_id } => super::client::rpc(
            &state,
            "cloud_agent_show",
            json!({"cloud_agent_id": cloud_agent_id}),
        )?,
        CloudAgentAction::Send {
            cloud_agent_id,
            thread_id,
            operation_id,
            text,
            no_wake,
        } => {
            let thread = match thread_id {
                Some(thread) => thread,
                None => home_thread(&state, &cloud_agent_id)?,
            };
            super::client::rpc(
                &state,
                "cloud_agent_send",
                json!({
                    "cloud_agent_id": cloud_agent_id,
                    "thread_id": thread,
                    "operation_id": operation_id.unwrap_or_else(new_operation_id),
                    "text": text,
                    "wake": !no_wake,
                }),
            )?
        }
        CloudAgentAction::EnrollPeer {
            cloud_agent_id,
            agent,
        } => super::client::rpc(
            &state,
            "cloud_agent_enroll_peer",
            json!({"cloud_agent_id": cloud_agent_id, "agent": agent}),
        )?,
        CloudAgentAction::Read {
            cloud_agent_id,
            operation_id,
        } => super::client::rpc(
            &state,
            "cloud_agent_read",
            json!({"cloud_agent_id": cloud_agent_id, "operation_id": operation_id}),
        )?,
        CloudAgentAction::Cancel {
            cloud_agent_id,
            operation_id,
        } => super::client::rpc(
            &state,
            "cloud_agent_cancel",
            json!({"cloud_agent_id": cloud_agent_id, "operation_id": operation_id}),
        )?,
        CloudAgentAction::Disconnect { cloud_agent_id } => super::client::rpc(
            &state,
            "cloud_agent_disconnect",
            json!({"cloud_agent_id": cloud_agent_id}),
        )?,
    };
    print_json(&value);
    Ok(0)
}
