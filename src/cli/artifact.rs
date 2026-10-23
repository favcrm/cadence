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
        /// Shown on the canvas tab and the thread card. Several words may follow with no
        /// quotes (`--title Weekly summary`): the master's command guard
        /// splits on spaces and refuses quotes.
        #[arg(long, num_args = 1.., required = true)]
        title: Vec<String>,
        /// The file to put (at most 10 MiB). The master writes it into its
        /// master/tmp first.
        #[arg(long)]
        file: PathBuf,
        /// An artifact id from an earlier put, to add a version. Give it
        /// first: the thread keeps only 160 characters of the command.
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
            // The master's file confinement applies exactly as for the other
            // `--file` verbs: no stdin, and only a regular non-symlink file
            // inside master/tmp, read from the opened fd.
            use std::io::Read;
            let named = file.as_os_str() != "-" && !file.as_os_str().is_empty();
            if !named && cadence_agent::master::caller_is_master() {
                return Err(Error::rejected(cadence_agent::master::NO_STDIN));
            }
            if !named {
                return Err(Error::usage(
                    "artifact put needs --file <path>; nothing was sent",
                ));
            }
            let opened = cadence_agent::master::open_command_file(&state_dir, &file)
                .map_err(|e| Error::rejected(e.to_string()))?;
            let meta = opened
                .metadata()
                .map_err(|e| Error::usage(format!("cannot read {}: {e}", file.display())))?;
            if !meta.is_file() || meta.len() > limit {
                return Err(Error::usage(format!(
                    "{} must be a regular file of at most {limit} bytes; nothing was sent",
                    file.display()
                )));
            }
            let mut bytes = Vec::new();
            opened
                .take(limit + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| Error::usage(format!("cannot read {}: {e}", file.display())))?;
            if bytes.len() as u64 > limit {
                return Err(Error::usage(format!(
                    "{} is over {limit} bytes; nothing was sent",
                    file.display()
                )));
            }
            let mut params = json!({
                "kind": kind,
                "title": title.join(" "),
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
