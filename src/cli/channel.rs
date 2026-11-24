//! Hosted workspace channels CLI (`cadence channel ls | read | post`).
//!
//! Hosted channels (`channel_list`, `channel_read`, `channel_post`) are a
//! workspace-hosted collaboration surface rather than a local daemon feature;
//! local daemon rooms live under `cadence topic`.

use super::*;

pub(crate) const LOCAL_ONLY_REFUSAL: &str = "channels require a hosted workspace: pass --org <org> for a logged-in workspace, or use `cadence topic` for local daemon rooms. Nothing was sent and no daemon was started";

#[derive(Subcommand)]
pub(crate) enum ChannelAction {
    /// List channels you belong to in the hosted workspace.
    #[command(alias = "list")]
    Ls,
    /// Read one page of messages in a hosted channel (`--before` cursor, `--limit` 1-100).
    Read {
        /// Channel room id.
        room: String,
        /// Pagination cursor returned by an earlier read.
        #[arg(long)]
        before: Option<String>,
        /// Page size (1-100).
        #[arg(long)]
        limit: Option<u64>,
    },
    /// Post a message to a hosted channel (`--id` deduplicates retries).
    Post {
        /// Channel room id.
        room: String,
        /// Message body (1-16000 bytes).
        text: String,
        /// Stable caller-chosen idempotency key; generated automatically if omitted.
        #[arg(long = "id")]
        client_id: Option<String>,
    },
}

pub(super) fn run_local() -> Result<i32> {
    Err(Error::usage(LOCAL_ONLY_REFUSAL))
}
