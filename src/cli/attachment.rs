//! CAD-1168: `cadence attachment` — reads on the retained chat files.
//! Upload is the board's `/api/chat/upload` (operator-only); this CLI
//! is read-only — an operator shell reads any id, and inside the
//! master's running turn the message id and turn token are derived
//! from `cadence self` (never taken as flags a caller could forge a
//! scope with — the daemon re-checks the token against the live turn).

use super::*;

#[derive(Subcommand)]
pub(crate) enum AttachmentAction {
    /// Read one retained attachment: bounded text for txt/md/csv,
    /// metadata only (extractable:false) for pdf/image. Called from an
    /// agent pane it must be the master's own running turn — the
    /// message id and token come from `agent_show`, like `cadence self`.
    Read {
        /// The daemon-minted `chf-…` id the attachments envelope names.
        id: String,
    },
}

pub(super) fn run(state_dir: PathBuf, action: AttachmentAction) -> Result<i32> {
    match action {
        AttachmentAction::Read { id } => {
            let mut params = json!({"id": id});
            // An agent caller (the master's pane) redeems its live
            // turn: derive the running message id and its token the
            // same way `cadence self` prints them — the daemon
            // re-proves both. Outside an agent pane nothing is sent
            // and the daemon's operator proof decides.
            if let Ok(alias) = std::env::var("CADENCE_ALIAS") {
                let show = client::rpc(&state_dir, "agent_show", self_show_params(&alias))?;
                let running = show["messages"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|m| m["state"].as_str() == Some("running"))
                    .cloned()
                    .ok_or_else(|| {
                        Error::rejected(format!(
                            "attachment read needs '{alias}'s running turn — none is running"
                        ))
                    })?;
                let (message, token) = (
                    running["id"].as_str().unwrap_or_default().to_string(),
                    running["turn_id"].as_str().unwrap_or_default().to_string(),
                );
                if message.is_empty() || token.is_empty() {
                    return Err(Error::rejected(format!(
                        "turn tokens for '{alias}' are shown only to that agent's own \
                         pane or managed endpoint — run `cadence attachment read` \
                         inside it"
                    )));
                }
                params["message"] = json!(message);
                params["token"] = json!(token);
            }
            print_json(&client::rpc(&state_dir, "chat_file_read", params)?);
            Ok(0)
        }
    }
}
