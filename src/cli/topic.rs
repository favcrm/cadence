//! CAD-1412: `cadence topic` — an agent reads and replies in a human room it
//! is a member of. Two verbs only, each forwarding the existing daemon RPC
//! unchanged (`topic_read` / `topic_publish`). The daemon derives the caller
//! from the connection (the agent's own pane) and refuses everything else;
//! this module adds no authority, no create, subscribe or request verb.

use super::*;

#[derive(Subcommand)]
pub(crate) enum TopicAction {
    /// Read one page of a topic you are a member of (oldest first).
    /// `--before <seq>` pages back from an earlier post; `--limit` is 1-100.
    Read {
        /// Opaque topic id, as named in the Ask instruction.
        topic: String,
        /// Posts before this sequence number.
        #[arg(long)]
        before: Option<i64>,
        /// Page size (1-100, default 50).
        #[arg(long)]
        limit: Option<i64>,
    },
    /// Post a reply in a topic you are a member of.
    Publish {
        /// Opaque topic id, as named in the Ask instruction.
        topic: String,
        /// Reply body.
        text: String,
        /// Idempotency key; a retry with the same id and text is a duplicate.
        #[arg(long = "id")]
        client_id: Option<String>,
    },
}

pub(super) fn run(state_dir: PathBuf, action: TopicAction) -> Result<i32> {
    match action {
        TopicAction::Read {
            topic,
            before,
            limit,
        } => {
            let mut params = json!({"topic": topic});
            if let Some(before) = before {
                params["before"] = json!(before);
            }
            if let Some(limit) = limit {
                params["limit"] = json!(limit);
            }
            print_json(&client::rpc(&state_dir, "topic_read", params)?);
            Ok(0)
        }
        TopicAction::Publish {
            topic,
            text,
            client_id,
        } => {
            let id = client_id.unwrap_or_else(|| {
                let nanos = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or_default();
                format!("reply-{nanos}")
            });
            let params = json!({"topic": topic, "text": text, "id": id});
            print_json(&client::rpc(&state_dir, "topic_publish", params)?);
            Ok(0)
        }
    }
}
