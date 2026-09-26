//! CAD-535: `cadence thread` — moved verbatim from src/main.rs.

use super::*;

#[derive(Subcommand)]
pub(crate) enum ThreadAction {
    /// Print the thread's entries (oldest first) and the cursor to
    /// continue from — for debugging the chat.
    Show {
        /// Agent alias or provider-native id.
        alias: String,
        /// Entries after this sequence number.
        #[arg(long, default_value_t = 0)]
        after: i64,
        /// Page size (1-500).
        #[arg(long, default_value_t = 100)]
        limit: i64,
    },
}

pub(super) fn run(state_dir: PathBuf, action: ThreadAction) -> Result<i32> {
    match action {
        ThreadAction::Show {
            alias,
            after,
            limit,
        } => {
            let page = client::rpc(
                &state_dir,
                "thread_read",
                json!({"alias": alias, "after": after, "limit": limit}),
            )?;
            print_json(&page);
            Ok(0)
        }
    }
}
