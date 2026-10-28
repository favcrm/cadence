//! CAD-1853: explicit native connection to a cloud agent.
//!
//! Connecting is `cadence agent connect --cloud`. The verbs here inspect
//! or reconcile a connection that already exists. There is no top-level
//! `cloud-agent` command. The saved login supplies custody; the daemon
//! performs the contract call so no caller ever reads the credential.
use clap::Subcommand;
use serde_json::{json, Value};

use super::{print_json, Error, Result};

#[derive(Clone, Subcommand)]
pub(crate) enum CloudAgentAction {
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
                "org '{org}' has no saved cloud login on this host — run `cadence org list` \
                 and `cadence login {org} --cloud-agent` first; nothing was sent"
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

/// `agent connect --cloud`. The flag is the consent: the daemon still
/// requires `accept_assistant_consent`, and this is the only caller that
/// sets it.
pub(super) fn connect(
    state_dir: std::path::PathBuf,
    workspace: Option<String>,
    alias: Option<String>,
) -> Result<i32> {
    let workspace = match workspace {
        Some(slug) => slug,
        None => default_workspace()?,
    };
    let target = saved_target(&workspace)?;
    let value = super::client::rpc(
        &state_dir,
        "cloud_agent_connect",
        json!({
            "org": target.org,
            "org_id": target.org_id,
            "endpoint": target.endpoint,
            "alias": alias.unwrap_or_else(|| "cloud".to_string()),
            "accept_assistant_consent": true,
        }),
    )?;
    print_json(&value);
    Ok(0)
}

fn default_workspace() -> Result<String> {
    let targets = cadence_agent::remote_cli::account_image_targets()?;
    match targets.len() {
        1 => Ok(targets[0].org.clone()),
        0 => Err(Error::usage(
            "no saved login — run `cadence login <slug> --cloud-agent`, then \
             `cadence agent connect --cloud`",
        )),
        _ => Err(Error::usage(format!(
            "more than one saved login — pass --workspace <slug> ({})",
            targets
                .iter()
                .map(|target| target.org.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

pub(super) fn run(state_dir: std::path::PathBuf, action: CloudAgentAction) -> Result<i32> {
    let state = state_dir;
    let value: Value = match action {
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
