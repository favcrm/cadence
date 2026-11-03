//! CAD-1608: `cadence agent peer` — list, one send, and one-time replies.
//!
//! The daemon derives the sender from the connection. This command has
//! no flag for a sender, a task, a nudge, or an approval.

use super::*;

#[derive(Clone, Subcommand)]
pub(crate) enum PeerAction {
    /// List peers in your group, with the connection's sender record,
    /// each peer's transport, and whether that endpoint is reachable.
    List,
    /// Send one message to a named peer. The result is this attempt's
    /// disposition (`queued`, `delivered`, `refused`, `held`, or
    /// `unknown`), not a claim that a task finished or a model acted.
    Send {
        /// Peer alias from `agent peer list`.
        alias: String,
        /// Exact message body. Stored byte for byte.
        #[arg(long, allow_hyphen_values = true)]
        text: String,
        /// Stable message id. Reusing it is refused and changes nothing.
        #[arg(long)]
        message: Option<String>,
        /// Recipient uid from `agent peer list`. A mismatch refuses
        /// before anything is queued.
        #[arg(long)]
        recipient_uid: Option<String>,
    },
    /// Read replies routed back to this connection. Each reply is
    /// returned once.
    Replies,
}

pub(super) fn run(state_dir: PathBuf, action: PeerAction) -> Result<i32> {
    let result = match action {
        PeerAction::List => client::rpc(&state_dir, "peer_list", json!({}))?,
        PeerAction::Send {
            alias,
            text,
            message,
            recipient_uid,
        } => {
            let mut params = json!({"alias": alias, "text": text});
            if let Some(message) = message {
                params["message"] = json!(message);
            }
            if let Some(uid) = recipient_uid {
                params["recipient_uid"] = json!(uid);
            }
            client::rpc(&state_dir, "peer_send", params)?
        }
        PeerAction::Replies => client::rpc(&state_dir, "peer_replies", json!({}))?,
    };
    print_json(&result);
    Ok(0)
}
