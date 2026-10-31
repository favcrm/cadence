//! CAD-1365: `app_media_read` — the first media surface a mounted
//! `screen-media.v1` frame may reach, relayed by the trusted host for a
//! live mount. Catalog-only: the workspace's bounded metadata
//! projection, never an original locator, byte, secret or prompt.
//!
//! The authority chain is the mount/tool chain's, never a relaxation:
//!
//! - `operator_connection` — a registered agent or detached caller is
//!   refused before anything else (the frame can't reach this verb);
//! - CLOSED top-level params `{action_token, token, key, origin,
//!   input}` — a body-supplied workspace/company/owner/install/context/
//!   creator/caller selector is refused, never read: the mount's own
//!   server-bound context decides every boundary;
//! - `token`/`key`/`origin` relay the request's REAL session credential
//!   and the daemon proves it natively (`tool_session`), with the same
//!   public-surface `BoardUser::is_operator` floor `app_screen_mint` and
//!   `app_tool_invoke` keep;
//! - `tool_context_proven` binds the call to the SAME verified session's
//!   live CONSUMED mount — a forged, foreign-session, pending or
//!   superseded action token refuses before any storage work;
//! - `workspace::with_runtime_snapshot(ctx.install_id)` then re-proves
//!   the LIVE state the context was minted against — `tool_context_proven`
//!   alone does not: the fresh bundle digest must equal `ctx.digest`,
//!   `app_capability_status` must still be `approved`, and the mounted
//!   tag's re-extracted package (`app_screen_pkg::extract`) must pin
//!   `host_contract: "screen-media.v1"` AND declare `may: media.read`
//!   (the nonempty provider-alias `tools` map can never carry media
//!   rights), and the mount's context must still be active under this
//!   install. A revoke or upgrade between mount and read fails closed.
//!
//! `input` is the closed union `{kind:"catalog", limit?:1..20,
//! cursor?:opaque}` — `{kind:"asset", asset_id}` is recognized by the
//! contract but NOT admitted until its custody check lands, and refuses
//! explicitly. Catalog output is `{assets, next_cursor, current_actor}`.
//! `current_actor` and each asset's `creator` carry the tagged app-port
//! `Creator` union — `{kind:"user", id:<verified native
//! BoardUser.sub>, label?}`, `{kind:"operator"}` or `{kind:"unknown"}`
//! — never a bare id, token, hash, UUID, email or handle-derived
//! identity. An empty workspace catalog is a valid positive result.
//!
//! Nothing provider-side is required to read retained metadata: no
//! binding, quote, claim or online provider door runs here.

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::app_bindings_rpc::strict_fields;
use super::app_tools_rpc::{ToolContext, TOOL_CTX_TTL};
use super::{required_str, Shared};
use crate::error::{Error, Result};
use crate::issue::app_catalog::workspace;
use crate::issue::{app_screen_decl, app_screen_pkg};
use crate::operator_auth::Origin;
use std::collections::BTreeMap;
use std::time::Instant;

/// Largest catalog page the contract admits.
const CATALOG_LIMIT_MAX: i64 = 20;
/// The catalog page default when `limit` is absent — at the bound.
const CATALOG_LIMIT_DEFAULT: i64 = CATALOG_LIMIT_MAX;
/// Bound on the opaque cursor string a request may carry.
const CURSOR_INPUT_MAX: usize = 160;
/// Media input is a small closed object — a generous cap for three
/// fields; a larger body is refused, never truncated.
const INPUT_BYTES: usize = 8 * 1024;

impl Shared {
    /// `app_media_read {action_token, token, key, origin, input}` — the
    /// CLOSED catalog read for one live media mount. See the module
    /// header for the full authority chain; every proof runs fresh on
    /// each call and no grant is ever cached into the tool context.
    pub(super) fn rpc_app_media_read(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        if params["input"]["kind"] == "preview" {
            self.media_request_proven(
                "media preview",
                params,
                peer_pid,
                (
                    &["action_token", "token", "key", "origin", "input"],
                    app_screen_decl::MAY_MEDIA_READ,
                ),
                |_| Ok(()),
                |_, (), _| Ok(Value::Null),
            )?;
            return self.media_read_preview(params);
        }
        self.media_request_proven(
            "media read",
            params,
            peer_pid,
            (
                &["action_token", "token", "key", "origin", "input"],
                app_screen_decl::MAY_MEDIA_READ,
            ),
            |params| {
                let input = params.get("input").cloned().unwrap_or(Value::Null);
                if !input.is_object() || serde_json::to_vec(&input)?.len() > INPUT_BYTES {
                    return Err(Error::rejected(
                        "media read input must be a JSON object within its bound",
                    ));
                }
                Ok(input)
            },
            |view, input, ctx| {
                // The workspace is this daemon's own store — the mount
                // proved it; no request field selects one. CAD-1962: the
                // library a mount lists is the curated published subset
                // plus its OWN drafts, so another installation's work in
                // progress never appears here.
                self.media_read_input(view, &ctx.install_id, &input)
            },
        )
    }

    fn media_read_preview(&self, params: &Value) -> Result<Value> {
        use base64::Engine;
        let input = &params["input"];
        strict_fields(input, &["kind", "asset_id", "expected_revision"])?;
        let asset_id = required_str(input, "asset_id")?;
        let revision = input["expected_revision"]
            .as_i64()
            .filter(|revision| *revision > 0)
            .ok_or_else(|| Error::rejected("media preview needs a positive revision"))?;
        let image = self.store.app_media_context_image(asset_id)?;
        // CAD-1962: the same visibility rule the catalog page applies —
        // another installation's draft is not a library entry, so its
        // display material is not served to this mount either.
        let caller = self.media_caller_install(params)?;
        if image.asset.lifecycle != "published" && image.asset.install_id != caller {
            return Err(Error::rejected(
                "media asset is not in the workspace catalog",
            ));
        }
        if image.asset.revision != revision || image.asset.kind != "image" {
            return Err(Error::rejected("media preview reference is stale"));
        }
        let preview = crate::platform::agenticos_external::image::preview_jpeg(
            &image.bytes,
            image.media_type,
        )
        .map_err(|(_, reason)| Error::rejected(reason))?;
        let encoded = base64::engine::general_purpose::STANDARD.encode(preview);
        self.with_media_mount_current(params, &image.original, |_| {
            if self.store.app_media_revision(asset_id)? != revision {
                return Err(Error::rejected(
                    "media preview reference changed during preparation",
                ));
            }
            Ok(json!({
                "asset_id": asset_id, "revision": revision,
                "sha256": image.original.sha256,
                "media_type": "image/jpeg", "base64": encoded,
            }))
        })
    }

    /// The installation this media request's mount proved. Used only for
    /// the visibility rule; every authority proof is the caller's own.
    fn media_caller_install(&self, params: &Value) -> Result<String> {
        let token = required_str(params, "token")?;
        let action_token = required_str(params, "action_token")?;
        let key = match params.get("key") {
            None => "",
            Some(Value::String(key)) => key.as_str(),
            Some(_) => {
                return Err(Error::rejected(
                    "media image key must be a session key string",
                ))
            }
        };
        let origin = match params["origin"].as_str() {
            Some("loopback") => Origin::Loopback,
            Some("tailnet") => Origin::Tailnet,
            Some("public") => Origin::Public,
            _ => {
                return Err(Error::rejected(
                    "media image needs a classified session origin",
                ))
            }
        };
        let (session, _) = self
            .tool_session(token, key, origin)
            .ok_or_else(|| Error::rejected("media image needs a live operator session"))?;
        let ctx = self.tool_context_proven(action_token, &session)?;
        ctx.context_id
            .ok_or_else(|| Error::rejected("media image needs a mounted active context"))?;
        Ok(ctx.install_id)
    }

    fn with_media_mount_current<T>(
        &self,
        params: &Value,
        original: &crate::store::app_media::MediaOriginalProof,
        read: impl FnOnce(&ToolContext) -> Result<T>,
    ) -> Result<T> {
        let token = required_str(params, "token")?;
        let action_token = required_str(params, "action_token")?;
        let key = match params.get("key") {
            None => "",
            Some(Value::String(key)) => key.as_str(),
            _ => {
                return Err(Error::rejected(
                    "media image key must be a session key string",
                ))
            }
        };
        let origin = match params["origin"].as_str() {
            Some("loopback") => Origin::Loopback,
            Some("tailnet") => Origin::Tailnet,
            Some("public") => Origin::Public,
            _ => {
                return Err(Error::rejected(
                    "media image needs a classified session origin",
                ))
            }
        };
        let (session, _) = self
            .tool_session(token, key, origin)
            .ok_or_else(|| Error::rejected("media image needs a live operator session"))?;
        let expected = self.tool_context_proven(action_token, &session)?;
        let pm = self.pm_at(&self.pm_dir()?)?;
        let delivery = OriginalDelivery {
            shared: self,
            pm: &pm,
            action_token,
            token,
            key,
            origin,
            proven: original,
            account_source: None,
            expected,
        };
        delivery.with_current(|| read(&delivery.expected))?
    }

    /// Re-prove the producing binding, approval and custody under the
    /// original transfer's pm -> custody -> release ordering.
    pub(super) fn with_media_source_current<T>(
        &self,
        original: &crate::store::app_media::MediaOriginalProof,
        read: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        let pm = self.pm_at(&self.pm_dir()?)?;
        workspace::with_runtime_snapshot(&pm, &original.install_id, |bundle, files| {
            let _custody = self
                .platform_custody_lock
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let _release = self
                .app_release_lock
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            self.app_binding_receipt_current(
                &original.install_id,
                original.context_id.as_deref(),
                &original.slot,
                &original.proof,
                bundle,
                files,
            )?;
            if self
                .store
                .app_capability_status(&original.install_id, required_str(bundle, "digest")?)?
                ["state"]
                != "approved"
            {
                return Err(Error::rejected(
                    "producing installation is not approved at its current digest",
                ));
            }
            read()
        })
    }

    /// Persisted app stamp for a turn image, never caller-supplied scope.
    /// `None` is general native chat with no app stamp. A stamp that no
    /// longer re-proves an active context, or whose conversation is not
    /// owned by that installation, refuses.
    fn image_receiver_scope(
        &self,
        caller: &str,
        message_id: &str,
    ) -> Result<Option<(String, String)>> {
        let raw = self.store.message_app_stamp(message_id)?;
        let Some(obj) = raw.as_ref().and_then(Value::as_object) else {
            return Ok(None);
        };
        if obj.get("verified") != Some(&Value::Bool(true)) {
            return Ok(None);
        }
        let install = obj.get("install_id").and_then(Value::as_str).unwrap_or("");
        let context = obj.get("context_id").and_then(Value::as_str).unwrap_or("");
        if install.is_empty() && context.is_empty() {
            return Ok(None);
        }
        if install.is_empty() || context.is_empty() {
            return Err(Error::rejected(
                "app image receiver stamp does not name an active context",
            ));
        }
        let hint = self
            .store
            .message_app(message_id)?
            .ok_or_else(|| Error::rejected("app image receiver context is not active"))?;
        if hint.get("install_id").and_then(Value::as_str) != Some(install)
            || hint.get("context_id").and_then(Value::as_str) != Some(context)
        {
            return Err(Error::rejected(
                "app image receiver stamp does not match the active context",
            ));
        }
        let conversation = self
            .store
            .message_conversation(caller, message_id)?
            .filter(|row| row.install_id.as_deref() == Some(install))
            .ok_or_else(|| Error::rejected("app image receiver does not own this conversation"))?;
        if conversation.subject.is_some() && conversation.context_id.as_deref() != Some(context) {
            return Err(Error::rejected(
                "app image receiver does not own this conversation",
            ));
        }
        Ok(Some((install.to_string(), context.to_string())))
    }

    fn receiver_install_admitted(
        &self,
        caller: &str,
        message_id: &str,
        expected: &(String, String),
        bundle: &Value,
    ) -> Result<()> {
        let again = self
            .image_receiver_scope(caller, message_id)?
            .ok_or_else(|| Error::rejected("app image receiver is no longer admitted"))?;
        if again != *expected {
            return Err(Error::rejected("app image receiver is no longer admitted"));
        }
        let digest = required_str(bundle, "digest")?;
        if self.store.app_capability_status(&again.0, digest)?["state"] != "approved" {
            return Err(Error::rejected(
                "receiving installation is not approved at its current digest",
            ));
        }
        let (_, proof) = self.store.app_context_proof(&again.0, &again.1)?;
        if proof.install_id != again.0 || proof.id != again.1 {
            return Err(Error::rejected("app image receiver context is not active"));
        }
        Ok(())
    }

    /// Refuse a revoked or stale app receiver before retained bytes are read.
    /// Unstamped general chat stays on the persisted-pin path.
    pub(super) fn gate_image_receiver_before_bytes(
        &self,
        caller: &str,
        message_id: &str,
    ) -> Result<()> {
        let Some(receiver) = self.image_receiver_scope(caller, message_id)? else {
            return Ok(());
        };
        let pm = self.pm_at(&self.pm_dir()?)?;
        workspace::with_runtime_snapshot(&pm, &receiver.0, |bundle, _files| {
            let _custody = self
                .platform_custody_lock
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let _release = self
                .app_release_lock
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            self.receiver_install_admitted(caller, message_id, &receiver, bundle)
        })
    }

    /// Producing custody together with the persisted receiver, under one
    /// pm → custody → release section. No app stamp keeps the producer-only
    /// proof used by general pinned chat.
    pub(super) fn with_turn_image_current<T>(
        &self,
        caller: &str,
        message_id: &str,
        original: &crate::store::app_media::MediaOriginalProof,
        read: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        let Some(receiver) = self.image_receiver_scope(caller, message_id)? else {
            return self.with_media_source_current(original, read);
        };
        let pm = self.pm_at(&self.pm_dir()?)?;
        workspace::with_runtime_snapshot_pair(
            &pm,
            &receiver.0,
            &original.install_id,
            |caller_snap, producer_snap| {
                let _custody = self
                    .platform_custody_lock
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                let _release = self
                    .app_release_lock
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                self.receiver_install_admitted(caller, message_id, &receiver, caller_snap.0)?;
                self.app_binding_receipt_current(
                    &original.install_id,
                    original.context_id.as_deref(),
                    &original.slot,
                    &original.proof,
                    producer_snap.0,
                    producer_snap.1,
                )?;
                if self.store.app_capability_status(
                    &original.install_id,
                    required_str(producer_snap.0, "digest")?,
                )?["state"]
                    != "approved"
                {
                    return Err(Error::rejected(
                        "producing installation is not approved at its current digest",
                    ));
                }
                read()
            },
        )
    }

    /// Attach is a media-mount operation. The trusted host supplies the
    /// captured conversation, never the opaque frame; a changed target refuses.
    pub(super) fn rpc_app_media_chat_attach(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        use base64::Engine;
        self.media_request_proven(
            "media chat attach",
            params,
            peer_pid,
            (
                &[
                    "action_token",
                    "token",
                    "key",
                    "origin",
                    "asset_id",
                    "expected_revision",
                    "conversation_id",
                ],
                app_screen_decl::MAY_MEDIA_CHAT_ATTACH,
            ),
            |_| Ok(()),
            |_, (), _| Ok(Value::Null),
        )?;
        let asset_id = required_str(params, "asset_id")?;
        if !crate::store::app_media::valid_id(asset_id) {
            return Err(Error::rejected(
                "media chat attach needs a catalog asset id",
            ));
        }
        let revision = params["expected_revision"]
            .as_i64()
            .filter(|revision| *revision > 0)
            .ok_or_else(|| Error::rejected("media chat attach needs a positive revision"))?;
        let conversation_id = match params.get("conversation_id") {
            Some(Value::Null) => None,
            Some(Value::String(id)) if !id.is_empty() && id.len() <= 128 => Some(id.as_str()),
            _ => {
                return Err(Error::rejected(
                    "media chat attach needs a captured host conversation",
                ))
            }
        };
        let image = self.store.app_media_context_image(asset_id)?;
        if image.asset.revision != revision
            || !matches!(
                image.original.media_type.as_str(),
                "image/png" | "image/jpeg"
            )
        {
            return Err(Error::rejected(
                "media chat attach needs a current PNG or JPEG original",
            ));
        }
        let preview = crate::platform::agenticos_external::image::preview_jpeg(
            &image.bytes,
            image.media_type,
        )
        .map_err(|(_, reason)| Error::rejected(reason))?;
        const PREFIX: &str = "data:image/jpeg;base64,";
        let mut data_url = String::with_capacity(PREFIX.len() + preview.len().div_ceil(3) * 4);
        data_url.push_str(PREFIX);
        base64::engine::general_purpose::STANDARD.encode_string(preview, &mut data_url);
        self.with_media_mount_current(params, &image.original, |ctx| {
            if self.store.app_media_revision(asset_id)? != revision {
                return Err(Error::rejected(
                    "media chat attach reference changed during preparation",
                ));
            }
            if let Some(id) = conversation_id {
                let alias = self.resolve_alias("master")?;
                if !self
                    .store
                    .conversation_list(&alias, &ctx.install_id)?
                    .iter()
                    .any(|row| {
                        row.id == id && !row.archived
                        // As on thread_send, only subject conversations bind
                        // to their creation context; ordinary threads do not.
                        && (row.subject.is_none()
                            || (ctx.context_id.is_some()
                                && row.context_id.as_deref() == ctx.context_id.as_deref()))
                    })
                {
                    return Err(Error::rejected(
                        "media chat attach conversation is outside this app context",
                    ));
                }
            }
            Ok(json!({
                "asset_id": asset_id, "revision": revision,
                "host_command": {
                    "operation": "media.chat.attach", "conversation_id": conversation_id,
                    "args": {
                        "install_id": ctx.install_id, "context_id": ctx.context_id,
                        "asset_id": asset_id, "revision": revision,
                        "sha256": image.original.sha256, "media_type": image.media_type,
                        "title": image.asset.title, "preview": data_url,
                    }
                }
            }))
        })
    }

    /// `app_media_publish {action_token, token, key, origin, asset_id,
    /// expected_revision, request_id}` — the CLOSED workspace-catalog
    /// publish for one live `screen-media.v1` mount declaring
    /// `media.publish`. Identical fresh authority chain to
    /// `app_media_read` — operator connection, closed fields, real
    /// session + public-operator floor, the SAME session's consumed
    /// mount, live install digest, approved status, re-extracted
    /// package, active owned context — then the declared permission is
    /// `media.publish`, never implied by `media.read` or a tools
    /// alias. The mutation itself is the store's atomic
    /// accepted-intent step: replay/refusal/CAS/receipt in one tx.
    pub(super) fn rpc_app_media_publish(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        self.media_request_proven(
            "media publish",
            params,
            peer_pid,
            (
                &[
                    "action_token",
                    "token",
                    "key",
                    "origin",
                    "asset_id",
                    "expected_revision",
                    "request_id",
                ],
                app_screen_decl::MAY_MEDIA_PUBLISH,
            ),
            |params| {
                let asset_id = required_str(params, "asset_id")?;
                if !crate::store::app_media::valid_id(asset_id) {
                    return Err(Error::rejected(
                        "media publish asset_id is not a catalog asset id",
                    ));
                }
                let expected_revision = params
                    .get("expected_revision")
                    .and_then(Value::as_i64)
                    .filter(|n| *n > 0)
                    .ok_or_else(|| {
                        Error::rejected(
                            "media publish expected_revision must be a positive integer",
                        )
                    })?;
                let request_id = required_str(params, "request_id")?;
                if request_id.is_empty()
                    || request_id.len() > 128
                    || !request_id
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
                {
                    return Err(Error::rejected(
                        "media publish request_id must be nonempty ASCII \
                         letters/digits/hyphen/underscore within 128 bytes",
                    ));
                }
                Ok((
                    asset_id.to_string(),
                    expected_revision,
                    request_id.to_string(),
                ))
            },
            |_view, (asset_id, expected_revision, request_id), _ctx| {
                // The workspace is this daemon's own store — the mount
                // proved it; the receipt namespace is the workspace
                // catalog itself. Runs inside the runtime snapshot
                // under the release lock: the replay check, revision
                // CAS and receipt insert commit atomically while the
                // freshly proven approval/digest still holds.
                self.store.app_media_publish(
                    &asset_id,
                    expected_revision,
                    &request_id,
                    crate::store::now(),
                )
            },
        )
    }

    /// `app_media_original {action_token, token, key, origin, asset_id}`
    /// — the CLOSED original transfer for one live `screen-media.v1`
    /// mount declaring `media.read`. Identical fresh authority chain to
    /// `app_media_read` — operator connection, closed fields, real
    /// session + public-operator floor, the SAME session's consumed
    /// mount, live install digest, approved status, re-extracted
    /// package, active owned context — then the ASSET's private
    /// provenance re-proof: private origin → verified capability
    /// receipt → approved run snapshot → frozen slot binding → CURRENT
    /// binding/connection/revision/credential at the PRODUCING install
    /// (store-wide catalog — never the caller's mount install, never
    /// creator), and only then the adapter's bounded read-only artifact
    /// GET. `deliver` runs only AFTER the runtime snapshot, custody and
    /// release guards are dropped. Each socket send re-proves the whole
    /// original authority under those guards before its nonblocking
    /// write (`OriginalDelivery::with_current`), so a revoke, rotation or
    /// approval change landing mid-stream refuses the next chunk. No
    /// guard is held across a socket wait.
    /// `platform_custody_lock` is taken inside the runtime snapshot
    /// before `app_release_lock` — the funded capability path's own
    /// pm → custody → release ordering — and is never nested.
    pub(super) fn rpc_app_media_original_stream(
        &self,
        params: &Value,
        peer_pid: u32,
        deliver: impl FnOnce(&Value, &[u8], &OriginalDelivery<'_>) -> Result<()>,
    ) -> Result<()> {
        let verb = "media original";
        self.operator_connection(verb, params, peer_pid)?;
        strict_fields(
            params,
            &["action_token", "token", "key", "origin", "asset_id"],
        )?;
        let action_token = required_str(params, "action_token")?;
        let token = required_str(params, "token")?;
        // `key` may be absent (public surface) or a string — never a
        // null or non-string selector.
        let key = match params.get("key") {
            None => "",
            Some(Value::String(s)) => s.as_str(),
            Some(_) => {
                return Err(Error::rejected(format!(
                    "{verb} key must be a session key string",
                )))
            }
        };
        let origin = match params.get("origin").and_then(Value::as_str) {
            Some("loopback") => Origin::Loopback,
            Some("tailnet") => Origin::Tailnet,
            Some("public") => Origin::Public,
            _ => {
                return Err(Error::rejected(format!(
                    "{verb} needs a classified session origin",
                )))
            }
        };
        let asset_id = required_str(params, "asset_id")?;
        if !crate::store::app_media::valid_id(asset_id) {
            return Err(Error::rejected(format!(
                "{verb} asset_id is not a catalog asset id",
            )));
        }
        let (session, view) = self.tool_session(token, key, origin).ok_or_else(|| {
            Error::rejected(format!(
                "{verb} needs a live operator session — sign in again"
            ))
        })?;
        if origin == Origin::Public {
            let operator = view
                .user
                .as_ref()
                .is_some_and(crate::operator_auth::BoardUser::is_operator);
            if !operator {
                return Err(Error::rejected(format!(
                    "{verb} needs an operator-role public session",
                )));
            }
        }
        let ctx = self.tool_context_proven(action_token, &session)?;
        if ctx.context_id.is_none() {
            return Err(Error::rejected(format!(
                "{verb} needs a mounted active context"
            )));
        }
        // CAD-1962: the asset must be in THIS mount's library —
        // published, or this installation's own draft. Another
        // installation's draft refuses here, before any provider work,
        // exactly as an unknown id does.
        if self
            .store
            .app_media_asset_for(&ctx.install_id, asset_id)?
            .is_none()
        {
            return Err(Error::rejected(format!(
                "{verb} asset is not in the workspace catalog",
            )));
        }
        // The asset's private provenance — a pure store read, resolved
        // before the runtime snapshot so the producing install is known.
        // Store-wide catalog: the producing install is provenance, never
        // a partition by the caller's mount install or creator.
        let proven = self.store.app_media_original(asset_id)?;
        let pm_dir = self.pm_dir()?;
        let pm = self.pm_at(&pm_dir)?;
        let mut account_source = None;
        let mut account_fetch = None;
        let mut public_body = None;
        let prepared = workspace::with_runtime_snapshot_pair(
            &pm,
            &ctx.install_id,
            &proven.install_id,
            |(bundle, files), producer| {
                let _custody = self
                    .platform_custody_lock
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                let _release = self
                    .app_release_lock
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                self.original_runtime_current(&ctx, &proven, (bundle, files), producer)?;
                let config = &proven.proof.config;
                let provider = required_str(config, "provider")?;
                let adapter = self
                    .platforms
                    .get(provider)
                    .ok_or_else(|| Error::rejected("media origin adapter unavailable"))?;
                let credential = self.app_capability_credential(config)?;
                let descriptor = json!({
                    "ref": proven.reference,
                    "sha256": proven.sha256,
                    "bytes": proven.bytes,
                    "media_type": proven.media_type,
                });
                if provider == crate::platform::account_media::PLATFORM {
                    // Freeze the FULL source, not just the durable account handle.
                    // The broker checks this exact lineage before renewal/GET.
                    let identity = crate::platform::account_media::current_identity(config)
                        .map_err(Error::rejected)?;
                    let frozen = crate::platform::agenticos_external::AgenticosExternalAdapter::account_image(identity.clone())?;
                    account_source = Some(identity);
                    account_fetch = Some((frozen, credential, descriptor));
                    return Ok(None);
                }
                let (media_type, bytes) = adapter
                    .read_app_media_original(&credential, config, &descriptor)
                    .map_err(Error::rejected)?;
                // Native public membership cannot run under these guards.
                // Refresh it after they drop, then pin the exact result.
                if self.native_public_original(origin) {
                    public_body = Some((media_type, bytes, credential, descriptor));
                    return Ok(None);
                }
                // The session+mount proven above is re-proven AFTER the
                // provider fetch and BEFORE any success header: an expiry
                // or revoke that landed during the bounded GET refuses the
                // delivery instead of riding the earlier proof.
                self.original_mount_current(token, key, origin, action_token, &ctx)?;
                crate::platform::refuse_leak(
                    "media original",
                    &descriptor.to_string(),
                    &credential,
                )?;
                let header = json!({
                    "transfer": "app-media-original/1",
                    "asset_id": asset_id,
                    "media_type": media_type,
                    "bytes": bytes.len() as u64,
                    "sha256": format!("sha256:{:x}", Sha256::digest(&bytes)),
                });
                Ok(Some((header, bytes)))
            },
        )?;
        let (header, bytes) = if let Some((adapter, credential, descriptor)) = account_fetch {
            // No PM, custody or release guard survives the preparation callback.
            let (media_type, bytes) = crate::platform::PlatformAdapter::read_app_media_original(
                &adapter,
                &credential,
                &proven.proof.config,
                &descriptor,
            )
            .map_err(Error::rejected)?;
            // Central public membership, when this daemon is native, is
            // refreshed after the GET and before the guards return.
            let proof = self.original_public_proof(token, origin, action_token, &ctx)?;
            self.original_bytes_current(
                &pm,
                &ctx,
                &proven,
                OriginalSession {
                    token,
                    key,
                    origin,
                    action_token,
                    proof: proof.as_ref(),
                },
                OriginalFetched {
                    asset_id,
                    media_type,
                    bytes,
                    credential: &credential,
                    descriptor: &descriptor,
                    account_source: account_source.as_ref(),
                },
            )?
        } else if let Some((media_type, bytes, credential, descriptor)) = public_body {
            let proof = self.original_public_proof(token, origin, action_token, &ctx)?;
            self.original_bytes_current(
                &pm,
                &ctx,
                &proven,
                OriginalSession {
                    token,
                    key,
                    origin,
                    action_token,
                    proof: proof.as_ref(),
                },
                OriginalFetched {
                    asset_id,
                    media_type,
                    bytes,
                    credential: &credential,
                    descriptor: &descriptor,
                    account_source: None,
                },
            )?
        } else {
            prepared.ok_or_else(|| Error::internal("media original preparation is absent"))?
        };
        let delivery = OriginalDelivery {
            shared: self,
            pm: &pm,
            action_token,
            token,
            key,
            origin,
            proven: &proven,
            account_source,
            expected: ctx,
        };
        deliver(&header, &bytes, &delivery)
    }

    /// The runtime half of the original authority, run under the held
    /// runtime snapshot, `platform_custody_lock` and `app_release_lock`:
    /// the producing install's CURRENT binding/connection/revision against
    /// the run-frozen proof, the mount's live digest, approval at the
    /// mounting and producing installs, the re-extracted media package and
    /// the active context. Runs before the provider GET and again before
    /// every socket send.
    fn original_runtime_current(
        &self,
        ctx: &ToolContext,
        proven: &crate::store::app_media::MediaOriginalProof,
        (bundle, files): (&Value, &BTreeMap<String, String>),
        (pbundle, pfiles): (&Value, &BTreeMap<String, String>),
    ) -> Result<()> {
        let verb = "media original";
        let context_id = ctx
            .context_id
            .as_deref()
            .ok_or_else(|| Error::rejected(format!("{verb} needs a mounted active context")))?;
        // The CURRENT binding/connection/revision at the PRODUCING install
        // must still match the run-frozen proof — drift, replacement,
        // rotation or revocation refuses before any provider access.
        self.app_binding_receipt_current(
            &proven.install_id,
            proven.context_id.as_deref(),
            &proven.slot,
            &proven.proof,
            pbundle,
            pfiles,
        )?;
        let live_digest = required_str(bundle, "digest")?;
        if live_digest != ctx.digest {
            return Err(Error::rejected(format!(
                "installation digest changed since mount — {verb} refused",
            )));
        }
        let status = self
            .store
            .app_capability_status(&ctx.install_id, live_digest)?;
        if status["state"].as_str() != Some("approved") {
            return Err(Error::rejected(
                "installation is not approved at its current digest",
            ));
        }
        let pkg = app_screen_pkg::extract(files, &ctx.tag)?;
        if !pkg.may.iter().any(|m| m == app_screen_decl::MAY_MEDIA_READ) {
            return Err(Error::rejected(
                "screen does not declare media.read — the media surface \
                 is refused to this mount",
            ));
        }
        if pkg.host_contract.as_deref() != Some(app_screen_decl::HOST_CONTRACT_MEDIA) {
            return Err(Error::rejected(format!(
                "{verb} requires the screen-media.v1 host contract",
            )));
        }
        // The producing run's approval must still be current: a revoked or
        // expired install approval at the producing install refuses the
        // original read, exactly as a mount's own approval proof does.
        if self
            .store
            .app_capability_status(&proven.install_id, required_str(pbundle, "digest")?)?["state"]
            .as_str()
            != Some("approved")
        {
            return Err(Error::rejected(
                "producing installation is not approved at its current digest",
            ));
        }
        let (_, proof) = self.store.app_context_proof(&ctx.install_id, context_id)?;
        if proof.install_id != ctx.install_id {
            return Err(Error::rejected(format!(
                "{verb} context belongs to another installation",
            )));
        }
        Ok(())
    }

    /// The caller side of the original authority for sessions whose proof is
    /// local. Native public membership HTTP is refused here so it cannot run
    /// under the caller's PM, custody or release guards.
    fn original_mount_current(
        &self,
        token: &str,
        key: &str,
        origin: Origin,
        action_token: &str,
        expected: &ToolContext,
    ) -> Result<()> {
        if self.native_public_original(origin) {
            return Err(Error::internal(
                "media original public membership must be refreshed outside native guards",
            ));
        }
        let verb = "media original";
        let (session, view) = self.tool_session(token, key, origin).ok_or_else(|| {
            Error::rejected(format!(
                "{verb} session ended during retrieval \u{2014} sign in again"
            ))
        })?;
        if origin == Origin::Public {
            let operator = view
                .user
                .as_ref()
                .is_some_and(crate::operator_auth::BoardUser::is_operator);
            if !operator {
                return Err(Error::rejected(format!(
                    "{verb} needs an operator-role public session",
                )));
            }
        }
        let ctx = self.tool_context_proven(action_token, &session)?;
        self.original_mount_matches(&ctx, expected)
    }

    fn native_public_original(&self, origin: Origin) -> bool {
        origin == Origin::Public
            && matches!(
                self.membership_mode,
                crate::daemon::public_membership::MembershipMode::Native(_)
            )
    }

    /// `None` unless this is a native public session. The lookup, including
    /// both central membership paths, finishes before the caller takes guards.
    fn original_public_proof(
        &self,
        token: &str,
        origin: Origin,
        action_token: &str,
        expected: &ToolContext,
    ) -> Result<Option<OriginalPublicProof>> {
        if !self.native_public_original(origin) {
            return Ok(None);
        }
        self.original_public_fresh(token, action_token, expected)
            .map(Some)
    }

    fn original_public_fresh(
        &self,
        token: &str,
        action_token: &str,
        expected: &ToolContext,
    ) -> Result<OriginalPublicProof> {
        let verb = "media original";
        let crate::daemon::public_membership::Authority::Granted {
            sub,
            role,
            hash,
            realm,
            view,
            ..
        } = self.current_public_session(token)?
        else {
            return Err(Error::rejected(format!(
                "{verb} session ended during retrieval \u{2014} sign in again"
            )));
        };
        if !role.is_operator()
            || !view
                .user
                .as_ref()
                .is_some_and(crate::operator_auth::BoardUser::is_operator)
        {
            return Err(Error::rejected(format!(
                "{verb} needs an operator-role public session",
            )));
        }
        let ctx = self.tool_context_proven(action_token, &hash)?;
        self.original_mount_matches(&ctx, expected)?;
        Ok(OriginalPublicProof {
            hash,
            sub,
            realm,
            operator: true,
            ctx,
        })
    }

    /// Re-check the exact fresh native decision under guards already held.
    /// Local row, realm, mount and approval only. No second membership lookup.
    fn original_public_pinned(
        &self,
        proof: &OriginalPublicProof,
        token: &str,
        action_token: &str,
        expected: &ToolContext,
    ) -> Result<()> {
        let verb = "media original";
        if !self.public_session_still(
            token,
            &proof.hash,
            &proof.sub,
            &proof.realm,
            proof.operator,
        )? {
            return Err(Error::rejected(format!(
                "{verb} session ended during retrieval \u{2014} sign in again"
            )));
        }
        let ctx = self.original_context_local(action_token, &proof.hash)?;
        if ctx.session != proof.ctx.session
            || ctx.install_id != proof.ctx.install_id
            || ctx.context_id != proof.ctx.context_id
            || ctx.digest != proof.ctx.digest
            || ctx.tag != proof.ctx.tag
            || ctx.generation != proof.ctx.generation
            || ctx.mount != proof.ctx.mount
            || ctx.approval_epoch != proof.ctx.approval_epoch
        {
            return Err(Error::rejected(format!(
                "{verb} mount moved during retrieval \u{2014} refused",
            )));
        }
        self.original_mount_matches(&ctx, expected)
    }

    fn original_authority_under_guards(
        &self,
        token: &str,
        key: &str,
        origin: Origin,
        action_token: &str,
        expected: &ToolContext,
        proof: Option<&OriginalPublicProof>,
    ) -> Result<()> {
        if let Some(proof) = proof {
            self.original_public_pinned(proof, token, action_token, expected)
        } else {
            self.original_mount_current(token, key, origin, action_token, expected)
        }
    }

    fn original_mount_matches(&self, ctx: &ToolContext, expected: &ToolContext) -> Result<()> {
        let verb = "media original";
        let context_id = ctx
            .context_id
            .as_deref()
            .ok_or_else(|| Error::rejected(format!("{verb} needs a mounted active context")))?;
        if ctx.install_id != expected.install_id
            || ctx.digest != expected.digest
            || ctx.tag != expected.tag
            || ctx.context_id != expected.context_id
        {
            return Err(Error::rejected(format!(
                "{verb} mount moved during retrieval \u{2014} refused",
            )));
        }
        let (_, proof) = self.store.app_context_proof(&ctx.install_id, context_id)?;
        if proof.install_id != ctx.install_id {
            return Err(Error::rejected(format!(
                "{verb} context belongs to another installation",
            )));
        }
        Ok(())
    }

    /// The local half of `tool_context_proven`. Central `public_row_hash`
    /// membership stays in the unlocked fresh proof.
    fn original_context_local(&self, token: &str, session_hash: &str) -> Result<ToolContext> {
        if token.len() != 64
            || !token
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(Error::rejected("invalid tool action context"));
        }
        let ctx = {
            let map = self.tool_contexts.lock().unwrap_or_else(|e| e.into_inner());
            map.get(token)
                .cloned()
                .ok_or_else(|| Error::rejected("tool action context is stale or unknown"))?
        };
        if Instant::now().duration_since(ctx.issued) >= TOOL_CTX_TTL {
            let mut map = self.tool_contexts.lock().unwrap_or_else(|e| e.into_inner());
            map.remove(token);
            return Err(Error::rejected("tool action context expired"));
        }
        if ctx.session != session_hash {
            return Err(Error::rejected(
                "tool action context does not belong to this session",
            ));
        }
        if ctx.mount.is_none() {
            return Err(Error::rejected(
                "tool action context has no live mount \u{2014} the screen is not consumed",
            ));
        }
        let status = self
            .store
            .app_capability_status(&ctx.install_id, &ctx.digest)?;
        if status["state"].as_str() != Some("approved") {
            return Err(Error::rejected(
                "installation is not approved at its current digest",
            ));
        }
        if status["epoch"].as_i64() != Some(ctx.approval_epoch) {
            return Err(Error::rejected(
                "installation was re-approved or revoked since mount \u{2014} tool call refused",
            ));
        }
        Ok(ctx)
    }

    fn original_bytes_current(
        &self,
        pm: &crate::issue::Pm,
        ctx: &ToolContext,
        proven: &crate::store::app_media::MediaOriginalProof,
        session: OriginalSession<'_>,
        fetched: OriginalFetched<'_>,
    ) -> Result<(Value, Vec<u8>)> {
        workspace::with_runtime_snapshot_pair(
            pm,
            &ctx.install_id,
            &proven.install_id,
            |caller, producer| {
                let _custody = self
                    .platform_custody_lock
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                let _release = self
                    .app_release_lock
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                self.original_runtime_current(ctx, proven, caller, producer)?;
                self.original_authority_under_guards(
                    session.token,
                    session.key,
                    session.origin,
                    session.action_token,
                    ctx,
                    session.proof,
                )?;
                if let Some(expected) = fetched.account_source {
                    let current =
                        crate::platform::account_media::current_identity(&proven.proof.config)
                            .map_err(Error::rejected)?;
                    if expected != &current {
                        return Err(Error::rejected(
                            "media original account source changed during retrieval",
                        ));
                    }
                }
                crate::platform::refuse_leak(
                    "media original",
                    &fetched.descriptor.to_string(),
                    fetched.credential,
                )?;
                Ok((
                    json!({
                        "transfer": "app-media-original/1", "asset_id": fetched.asset_id,
                        "media_type": fetched.media_type, "bytes": fetched.bytes.len() as u64,
                        "sha256": format!("sha256:{:x}", Sha256::digest(&fetched.bytes)),
                    }),
                    fetched.bytes,
                ))
            },
        )
    }

    /// The shared fresh authority proof both media verbs run on EVERY
    /// call — accepted-intent replay included, nothing cached:
    /// operator connection → closed fields → real session (with the
    /// public-surface operator-role floor) → the SAME session's
    /// consumed action token → live install digest == the minted one →
    /// approved status → the mounted tag's re-extracted package must
    /// pin `screen-media.v1` AND declare `required_may` → active
    /// context owned by this install. `parse` extracts the op-specific
    /// terms from the already-field-checked params; `execute` runs the
    /// op itself INSIDE the runtime snapshot while `app_release_lock`
    /// is still held, after the fresh context proof — the operation and
    /// the authority it was proven under are one critical section, so
    /// an install mutation or approval revocation cannot slip between
    /// proof and store work. Returns the op's result directly.
    fn media_request_proven<T>(
        &self,
        verb: &str,
        params: &Value,
        peer_pid: u32,
        (allowed, required_may): (&[&str], &str),
        parse: impl FnOnce(&Value) -> Result<T>,
        execute: impl FnOnce(&crate::operator_auth::SessionView, T, &ToolContext) -> Result<Value>,
    ) -> Result<Value> {
        self.operator_connection(verb, params, peer_pid)?;
        strict_fields(params, allowed)?;
        let action_token = required_str(params, "action_token")?;
        let token = required_str(params, "token")?;
        let key = params.get("key").and_then(Value::as_str).unwrap_or("");
        let origin = match params.get("origin").and_then(Value::as_str) {
            Some("loopback") => Origin::Loopback,
            Some("tailnet") => Origin::Tailnet,
            Some("public") => Origin::Public,
            _ => {
                return Err(Error::rejected(format!(
                    "{verb} needs a classified session origin",
                )))
            }
        };
        let terms = parse(params)?;
        // Prove the real session natively; on the public surface it must
        // ALSO carry the operator role — the same floor mounting itself
        // keeps. The action context then binds this call to that SAME
        // session's live consumed mount.
        let (session, view) = self.tool_session(token, key, origin).ok_or_else(|| {
            Error::rejected(format!(
                "{verb} needs a live operator session — sign in again"
            ))
        })?;
        if origin == Origin::Public {
            let operator = view
                .user
                .as_ref()
                .is_some_and(crate::operator_auth::BoardUser::is_operator);
            if !operator {
                return Err(Error::rejected(format!(
                    "{verb} needs an operator-role public session",
                )));
            }
        }
        let ctx = self.tool_context_proven(action_token, &session)?;
        // The mount must carry an active context of its own install —
        // the media surface is scoped to the mount's workspace, never to
        // a caller-named one, and requires a live mounted context like
        // the tool surface does.
        let context_id = ctx
            .context_id
            .as_deref()
            .ok_or_else(|| Error::rejected(format!("{verb} needs a mounted active context")))?;

        let pm_dir = self.pm_dir()?;
        let pm = self.pm_at(&pm_dir)?;
        workspace::with_runtime_snapshot(&pm, &ctx.install_id, |bundle, files| {
            // Serialize against install release/admission mutation with
            // the same existing lock the tool executor takes — no new
            // lock architecture.
            let _release = self
                .app_release_lock
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            // The live install must still sit at the mount's minted
            // digest — `tool_context_proven` proved the mount token,
            // never the bundle.
            let live_digest = required_str(bundle, "digest")?;
            if live_digest != ctx.digest {
                return Err(Error::rejected(format!(
                    "installation digest changed since mount — {verb} refused",
                )));
            }
            let status = self
                .store
                .app_capability_status(&ctx.install_id, live_digest)?;
            if status["state"].as_str() != Some("approved") {
                return Err(Error::rejected(
                    "installation is not approved at its current digest",
                ));
            }
            // Re-extract the mounted tag's package against the LIVE
            // bytes and require the media declaration on it — the
            // context's captured tool-alias map is provider vocabulary
            // only and can never carry media rights.
            let pkg = app_screen_pkg::extract(files, &ctx.tag)?;
            if !pkg.may.iter().any(|m| m == required_may) {
                return Err(Error::rejected(format!(
                    "screen does not declare {required_may} — the media surface \
                     is refused to this mount",
                )));
            }
            if pkg.host_contract.as_deref() != Some(app_screen_decl::HOST_CONTRACT_MEDIA) {
                return Err(Error::rejected(format!(
                    "{verb} requires the screen-media.v1 host contract",
                )));
            }
            // The mount's context must still be live and owned by this
            // exact installation — re-proven fresh, not cached.
            let (_, proof) = self.store.app_context_proof(&ctx.install_id, context_id)?;
            if proof.install_id != ctx.install_id {
                return Err(Error::rejected(format!(
                    "{verb} context belongs to another installation",
                )));
            }
            execute(&view, terms, &ctx)
        })
    }

    /// Native image inspection sends verified retained display material;
    /// the message's original digest remains the reference-edit pin.
    pub(super) fn rpc_app_media_inspect_stream(
        &self,
        params: &Value,
        peer_pid: u32,
        deliver: impl FnOnce(&Value, &[u8], &MediaInspectionDelivery<'_>) -> Result<()>,
    ) -> Result<()> {
        let pin = self.media_inspect_turn(params, peer_pid)?;
        let asset_id = required_str(params, "asset_id")?;
        let message_id = required_str(params, "message")?;
        let caller = match self.connection_caller(peer_pid)? {
            crate::daemon::caller_rule::Who::Agent(alias) => alias,
            _ => return Err(Error::rejected("media inspect is an agent turn's verb")),
        };
        // Receiver admission is persisted scope, proved before any retained
        // byte read. Producer pairing repeats inside each currentness check.
        self.gate_image_receiver_before_bytes(&caller, message_id)?;
        let image = self.store.app_media_context_image(asset_id)?;
        if pin["revision"].as_i64() != Some(image.asset.revision)
            || pin["sha256"].as_str() != Some(image.original.sha256.as_str())
        {
            return Err(Error::rejected("media inspect reference is stale"));
        }
        let delivery = MediaInspectionDelivery {
            shared: self,
            params,
            peer_pid,
            original: &image.original,
            asset_id,
            revision: image.asset.revision,
            pin: &pin,
        };
        delivery.with_current(|| ())?;
        let header = json!({
            "asset_id": asset_id, "revision": image.asset.revision,
            "sha256": image.digest, "media_type": image.media_type,
            "bytes": image.bytes.len(),
        });
        deliver(&header, &image.bytes, &delivery)
    }

    /// CAD-1482: re-prove the exact assigned turn and its one image pin.
    /// The caller is the connection's agent. The token must be that
    /// agent's current assigned turn, and the asset must be the single
    /// library image pinned on that message. Bytes are the retained
    /// receipt blob — this verb never submits or downloads from a provider.
    fn media_inspect_turn(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        strict_fields(params, &["asset_id", "message", "token"])?;
        let asset_id = required_str(params, "asset_id")?;
        let message_id = required_str(params, "message")?;
        let token = required_str(params, "token")?;
        if !crate::store::app_media::valid_id(asset_id) {
            return Err(Error::rejected(
                "media inspect asset_id is not a catalog asset id",
            ));
        }
        let caller = match self.connection_caller(peer_pid)? {
            crate::daemon::caller_rule::Who::Agent(alias) => alias,
            _ => return Err(Error::rejected("media inspect is an agent turn's verb")),
        };
        self.assigned_agent_endpoint_session(peer_pid, &caller, "media inspect")?;
        if message_id.is_empty() || message_id.len() > 128 || token.is_empty() || token.len() > 256
        {
            return Err(Error::rejected("media inspect turn identity is malformed"));
        }
        let stored = self
            .store
            .message(message_id)?
            .ok_or_else(|| Error::rejected("media inspect turn is unknown"))?;
        if stored.alias != caller
            || stored.state != "running"
            || stored.turn_id.as_deref() != Some(token)
        {
            return Err(Error::rejected(
                "media inspect needs the active assigned chat turn",
            ));
        }
        let agent = self.store.agent(&caller)?;
        if !crate::adapter::registry::turn_token_current(
            &agent.provider,
            &agent.endpoint_kind,
            agent.generation.as_deref(),
            token,
        ) {
            return Err(Error::rejected(
                "media inspect turn token is no longer current",
            ));
        }
        let pinned = self.store.message_media_refs(message_id)?;
        let matched = pinned
            .as_ref()
            .and_then(|refs| refs.as_array())
            .is_some_and(|refs| {
                refs.len() == 1
                    && refs[0]["asset_id"].as_str() == Some(asset_id)
                    && refs[0]["revision"].as_i64().is_some()
                    && refs[0]["sha256"].as_str().is_some()
            });
        if !matched {
            return Err(Error::rejected(
                "media inspect asset is not attached to this turn",
            ));
        }
        match pinned {
            Some(Value::Array(mut refs)) => refs
                .pop()
                .ok_or_else(|| Error::rejected("media inspect image pin is absent")),
            _ => Err(Error::rejected("media inspect image pin is absent")),
        }
    }

    /// The closed media-read input union: the bounded catalog page and
    /// one asset's metadata projection. Both list what this installation
    /// may see — published rows from any installation plus its OWN
    /// drafts (CAD-1962). Neither returns bytes or a locator; the
    /// original transfer is its own door.
    fn media_read_input(
        &self,
        view: &crate::operator_auth::SessionView,
        install: &str,
        input: &Value,
    ) -> Result<Value> {
        let kind = input
            .get("kind")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::rejected("media read input needs a kind"))?;
        match kind {
            "catalog" => {
                let allowed = ["kind", "limit", "cursor"];
                if input
                    .as_object()
                    .is_some_and(|o| o.keys().any(|k| !allowed.contains(&k.as_str())))
                {
                    return Err(Error::rejected(
                        "media catalog input has unsupported fields",
                    ));
                }
                let limit = match input.get("limit") {
                    None | Some(Value::Null) => CATALOG_LIMIT_DEFAULT,
                    Some(v) => {
                        let n = v.as_i64().ok_or_else(|| {
                            Error::rejected("media catalog limit must be an integer")
                        })?;
                        if !(1..=CATALOG_LIMIT_MAX).contains(&n) {
                            return Err(Error::rejected("media catalog limit must be 1..=20"));
                        }
                        n
                    }
                };
                let cursor = match input.get("cursor") {
                    None | Some(Value::Null) => None,
                    Some(Value::String(s)) if s.len() <= CURSOR_INPUT_MAX => Some(s.as_str()),
                    Some(_) => {
                        return Err(Error::rejected(
                            "media catalog cursor is not a valid cursor",
                        ))
                    }
                };
                let (page, next_cursor) =
                    self.store.app_media_catalog_for(install, limit, cursor)?;
                let assets: Vec<Value> = page.iter().map(|asset| asset.projection()).collect();
                Ok(json!({
                    "assets": assets,
                    "next_cursor": next_cursor,
                    "current_actor": media_actor(view),
                }))
            }
            "asset" => {
                // CAD-1962: one catalog row by id — the same metadata
                // projection the page emits, never bytes or a locator.
                // Another installation's draft resolves to nothing, so
                // an unpublished asset is indistinguishable from an
                // unknown one.
                if input
                    .as_object()
                    .is_some_and(|o| o.keys().any(|k| !matches!(k.as_str(), "kind" | "asset_id")))
                {
                    return Err(Error::rejected("media asset input has unsupported fields"));
                }
                let asset_id = input
                    .get("asset_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| Error::rejected("media asset input needs an asset_id"))?;
                let asset = self
                    .store
                    .app_media_asset_for(install, asset_id)?
                    .ok_or_else(|| {
                        Error::rejected("media asset is not in the workspace catalog")
                    })?;
                Ok(json!({
                    "asset": asset.projection(),
                    "current_actor": media_actor(view),
                }))
            }
            _ => Err(Error::rejected(
                "media read input kind is not a recognized media read",
            )),
        }
    }
}

/// Like original delivery, a turn image re-proves authority before every
/// nonblocking send and holds no guard while waiting for socket capacity.
pub(super) struct MediaInspectionDelivery<'a> {
    shared: &'a Shared,
    params: &'a Value,
    peer_pid: u32,
    original: &'a crate::store::app_media::MediaOriginalProof,
    asset_id: &'a str,
    revision: i64,
    pin: &'a Value,
}

impl MediaInspectionDelivery<'_> {
    pub(super) fn with_current<T>(&self, send: impl FnOnce() -> T) -> Result<T> {
        let caller = match self.shared.connection_caller(self.peer_pid)? {
            crate::daemon::caller_rule::Who::Agent(alias) => alias,
            _ => return Err(Error::rejected("media inspect is an agent turn's verb")),
        };
        let message_id = required_str(self.params, "message")?;
        self.shared
            .with_turn_image_current(&caller, message_id, self.original, || {
                let current = self.shared.media_inspect_turn(self.params, self.peer_pid)?;
                if &current != self.pin
                    || self.shared.store.app_media_revision(self.asset_id)? != self.revision
                {
                    return Err(Error::rejected(
                        "media inspect authority ended during delivery",
                    ));
                }
                Ok(send())
            })
    }
}

struct OriginalSession<'a> {
    token: &'a str,
    key: &'a str,
    origin: Origin,
    action_token: &'a str,
    proof: Option<&'a OriginalPublicProof>,
}

struct OriginalFetched<'a> {
    asset_id: &'a str,
    media_type: String,
    bytes: Vec<u8>,
    credential: &'a [u8],
    descriptor: &'a Value,
    account_source: Option<&'a crate::remote_cli::AccountImageIdentity>,
}

struct OriginalPublicProof {
    hash: String,
    sub: String,
    realm: crate::operator_auth::PublicRealm,
    operator: bool,
    ctx: ToolContext,
}

/// The original transfer's write-time authority. Each nonblocking send
/// runs under `with_current`, which re-proves the original authority
/// under the held guards before the write: runtime snapshot,
/// `platform_custody_lock` and `app_release_lock` (pm → custody → release),
/// the producing binding, approvals and active context, then the caller's
/// live session and consumed mount. Native public membership HTTP is
/// refreshed before those guards and pinned locally inside them. The write
/// itself is one nonblocking send inside the guards. No guard is held
/// across a `WouldBlock` wait, so a revoke serializes with a single send.
pub(super) struct OriginalDelivery<'a> {
    shared: &'a Shared,
    pm: &'a crate::issue::Pm,
    action_token: &'a str,
    token: &'a str,
    key: &'a str,
    origin: Origin,
    proven: &'a crate::store::app_media::MediaOriginalProof,
    account_source: Option<crate::remote_cli::AccountImageIdentity>,
    expected: ToolContext,
}

impl OriginalDelivery<'_> {
    pub(super) fn with_current<T>(&self, send: impl FnOnce() -> T) -> Result<T> {
        let refused = || Error::rejected("media original authority ended — delivery refused");
        let proof = self.shared.original_public_proof(
            self.token,
            self.origin,
            self.action_token,
            &self.expected,
        )?;
        workspace::with_runtime_snapshot_pair(
            self.pm,
            &self.expected.install_id,
            &self.proven.install_id,
            |caller, producer| {
                let _custody = self
                    .shared
                    .platform_custody_lock
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                let _release = self
                    .shared
                    .app_release_lock
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                self.shared.original_runtime_current(
                    &self.expected,
                    self.proven,
                    caller,
                    producer,
                )?;
                self.shared.original_authority_under_guards(
                    self.token,
                    self.key,
                    self.origin,
                    self.action_token,
                    &self.expected,
                    proof.as_ref(),
                )?;
                if let Some(expected) = &self.account_source {
                    let current =
                        crate::platform::account_media::current_identity(&self.proven.proof.config)
                            .map_err(Error::rejected)?;
                    if &current != expected {
                        return Err(refused());
                    }
                }
                let mut auth = self.shared.operator_auth();
                if !auth.live_hash(&self.expected.session, self.shared.operator_now())? {
                    return Err(refused());
                }
                let map = self
                    .shared
                    .tool_contexts
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                let ctx = map.get(self.action_token).ok_or_else(refused)?;
                let e = &self.expected;
                let same_mount = ctx.session == e.session
                    && ctx.install_id == e.install_id
                    && ctx.context_id == e.context_id
                    && ctx.digest == e.digest
                    && ctx.tag == e.tag
                    && ctx.generation == e.generation
                    && ctx.mount.is_some()
                    && ctx.mount == e.mount;
                if !same_mount || Instant::now().duration_since(ctx.issued) >= TOOL_CTX_TTL {
                    return Err(refused());
                }
                Ok(send())
            },
        )
    }
}

/// The actor the reply honestly names: the verified named subject a
/// public session carries, or the unnamed local operator. Never a
/// token, hash, UUID, email or handle-derived person.
fn media_actor(view: &crate::operator_auth::SessionView) -> Value {
    match &view.user {
        Some(user) => json!({"kind": "user", "id": user.sub}),
        None => json!({"kind": "operator"}),
    }
}
