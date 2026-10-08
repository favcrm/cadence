//! CAD-1168: `cadence attachment` — reads on the retained chat files.
//! Upload is the board's `/api/chat/upload` (operator-only); this CLI
//! is read-only — an operator shell reads any id, and the master reads
//! only an id on its own running turn's envelope (the daemon resolves
//! that turn from the proven caller; the CLI sends the id alone).

use super::*;

#[derive(Subcommand)]
pub(crate) enum AttachmentAction {
    /// Read one retained attachment: bounded text for txt/md/csv.
    /// From an agent pane it must be the master's own running turn.
    Read {
        /// The daemon-minted `chf-…` id the attachments envelope names.
        id: String,
    },
}

pub(super) fn run(state_dir: PathBuf, action: AttachmentAction) -> Result<i32> {
    match action {
        AttachmentAction::Read { id } => {
            // Only the id is sent: the daemon proves the caller and, for
            // the master, resolves its own live turn (and re-proves the
            // file is on that turn's envelope). Nothing here could be
            // forged into a scope.
            print_json(&client::rpc(
                &state_dir,
                "chat_file_read",
                json!({"id": id}),
            )?);
            Ok(0)
        }
    }
}
