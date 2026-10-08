//! CAD-1110: `app_chat_descriptor {install_id}` — the one generic,
//! read-only verb that serves an installed app's `app-chat.json` to the
//! trusted board (contract: docs/design/CAD-1109-app-chat-v1.md).
//!
//! `operator_connection`-gated like the sibling app reads: an agent pane,
//! a detached child and a forged identity field are refused before any
//! file is touched. The install id is the only input. The descriptor is
//! read from the installed bundle at the digest the install consented to through the same
//! descriptor-confined resolver the screen mount uses
//! (`workspace::with_runtime_read`: no symlinks, no request path), and
//! only when `app_capability_status` says that digest is consented (CAD-1119:
//! an operator install or update records it) and not withdrawn. Everything else — no such install, no descriptor,
//! withdrawn consent, a stale digest, a bundle that moved during the read —
//! answers `{"found": false}`, which the board turns into a 404 that names
//! no other install and no path. The route never parses chat semantics
//! beyond size and JSON validity; the grammar is the install validator's
//! and the client's.

use serde_json::{json, Value};

use super::{required_str, Shared};
use crate::error::{Error, Result};
use crate::issue::app_chat;

impl Shared {
    pub(super) fn rpc_app_chat_descriptor(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        self.operator_connection("app chat descriptor", params, peer_pid)?;
        let fields = params
            .as_object()
            .ok_or_else(|| Error::rejected("chat descriptor params must be an object"))?;
        if fields.keys().any(|k| k != "install_id") {
            return Err(Error::rejected("chat descriptor admits only an install_id"));
        }
        let install_id = required_str(params, "install_id")?;
        let pm_dir = self.pm_dir()?;
        let pm = self.pm_at(&pm_dir)?;
        let store = &self.store;
        use crate::issue::app_catalog::workspace;
        let read = workspace::with_runtime_read(&pm, install_id, |row, files| {
            let digest = row["digest"]
                .as_str()
                .ok_or_else(|| Error::rejected("installation digest unavailable"))?
                .to_string();
            let status = store.app_capability_status(install_id, &digest)?;
            if status["state"].as_str() != Some("approved") {
                return Ok(None);
            }
            let Some(text) = files.get(app_chat::FILE) else {
                return Ok(None);
            };
            let app = crate::issue::app::parse_manifest(
                files
                    .get("app.md")
                    .ok_or_else(|| Error::rejected("installation manifest unavailable"))?,
            )?
            .app;
            let descriptor = app_chat::size_and_json(text)?;
            Ok(Some(
                json!({"descriptor": descriptor, "digest": digest, "app": app}),
            ))
        });
        match read {
            Ok(Some(found)) => Ok(found),
            Ok(None) => Ok(json!({"found": false})),
            // An unknown or removed install, a changed bundle and a bad
            // descriptor file are all "no descriptor" to the board.
            Err(e) if e.kind() == "rejected" => Ok(json!({"found": false})),
            Err(e) => Err(e),
        }
    }
}
