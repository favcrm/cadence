//! CAD-1726: `cadence artifact put` — the agent shows its work.
//! Only the master's live turn may put; the daemon resolves that turn from
//! the proven caller and takes the conversation and scope from it, so the
//! CLI sends the kind, title, bytes and (for a new version) the id alone.

use super::*;

#[derive(Subcommand)]
pub(crate) enum ArtifactAction {
    /// Put a file on the canvas beside the chat. The same `--id` again
    /// stores the next version; without it a new artifact is created.
    Put {
        /// md, pdf, html, image or csv.
        #[arg(long)]
        kind: String,
        /// Shown on the card and the tab.
        #[arg(long)]
        title: String,
        /// The file to put (at most 10 MiB).
        #[arg(long)]
        file: PathBuf,
        /// An artifact id from an earlier put, to add a version.
        #[arg(long)]
        id: Option<String>,
    },
}

pub(super) fn run(state_dir: PathBuf, action: ArtifactAction) -> Result<i32> {
    match action {
        ArtifactAction::Put {
            kind,
            title,
            file,
            id,
        } => {
            use base64::Engine;
            let limit = cadence_agent::store::ARTIFACT_MAX_BYTES as u64;
            let meta = std::fs::metadata(&file)
                .map_err(|e| Error::usage(format!("cannot read {}: {e}", file.display())))?;
            if !meta.is_file() || meta.len() > limit {
                return Err(Error::usage(format!(
                    "{} must be a regular file of at most {limit} bytes; nothing was sent",
                    file.display()
                )));
            }
            let bytes = std::fs::read(&file)
                .map_err(|e| Error::usage(format!("cannot read {}: {e}", file.display())))?;
            let mut params = json!({
                "kind": kind,
                "title": title,
                "content": base64::engine::general_purpose::STANDARD.encode(bytes),
            });
            if let Some(id) = id {
                params["id"] = json!(id);
            }
            print_json(&client::rpc(&state_dir, "artifact_put", params)?);
            Ok(0)
        }
    }
}
