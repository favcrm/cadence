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
            |view, input| {
                // The workspace is this daemon's own store — the mount
                // proved it; no request field selects one. The catalog
                // is store-wide: same-workspace collaborators read the
                // same rows regardless of which install produced them.
                self.media_read_input(view, &input)
            },
        )
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
            |_view, (asset_id, expected_revision, request_id)| {
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
        // The asset's private provenance — a pure store read, resolved
        // before the runtime snapshot so the producing install is known.
        // Store-wide catalog: the producing install is provenance, never
        // a partition by the caller's mount install or creator.
        let proven = self.store.app_media_original(asset_id)?;
        let pm_dir = self.pm_dir()?;
        let pm = self.pm_at(&pm_dir)?;
        let (header, bytes) = workspace::with_runtime_snapshot_pair(
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
                let (media_type, bytes) = adapter
                    .read_app_media_original(&credential, config, &descriptor)
                    .map_err(Error::rejected)?;
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
                Ok((header, bytes))
            },
        )?;
        let delivery = OriginalDelivery {
            shared: self,
            pm: &pm,
            action_token,
            token,
            key,
            origin,
            proven: &proven,
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

    /// The caller side of the original authority: the verified session is
    /// still live, the public operator-role floor holds, and the consumed
    /// mount is still the one `expected` proved. Pure store/Auth reads; no
    /// custody, release or runtime lock is taken here.
    fn original_mount_current(
        &self,
        token: &str,
        key: &str,
        origin: Origin,
        action_token: &str,
        expected: &ToolContext,
    ) -> Result<()> {
        let verb = "media original";
        let (session, view) = self.tool_session(token, key, origin).ok_or_else(|| {
            Error::rejected(format!(
                "{verb} session ended during retrieval — sign in again"
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
                "{verb} mount moved during retrieval — refused",
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
        execute: impl FnOnce(&crate::operator_auth::SessionView, T) -> Result<Value>,
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
            execute(&view, terms)
        })
    }

    /// The closed media-read input union and the bounded catalog page.
    /// `kind:"asset"` is a recognized contract member but NOT admitted —
    /// detail needs its own custody check and refuses explicitly here.
    fn media_read_input(
        &self,
        view: &crate::operator_auth::SessionView,
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
                let (page, next_cursor) = self.store.app_media_catalog(limit, cursor)?;
                let assets: Vec<Value> = page.iter().map(|a| a.projection()).collect();
                Ok(json!({
                    "assets": assets,
                    "next_cursor": next_cursor,
                    "current_actor": media_actor(view),
                }))
            }
            "asset" => Err(Error::rejected(
                "media asset detail is not admitted — no asset read exists yet",
            )),
            _ => Err(Error::rejected(
                "media read input kind is not a recognized media read",
            )),
        }
    }
}

/// The original transfer's write-time authority. Each nonblocking send
/// runs under `with_current`, which re-proves the WHOLE original authority
/// under the held guards before the write: the runtime snapshot,
/// `platform_custody_lock` and `app_release_lock` (pm → custody → release),
/// the producing binding, approvals and active context, then the caller's
/// live session and consumed mount (Auth → tool_context). The write itself
/// is one nonblocking send inside those guards. No guard is held across a
/// `WouldBlock` wait, so a revoke serializes with a single send.
pub(super) struct OriginalDelivery<'a> {
    shared: &'a Shared,
    pm: &'a crate::issue::Pm,
    action_token: &'a str,
    token: &'a str,
    key: &'a str,
    origin: Origin,
    proven: &'a crate::store::app_media::MediaOriginalProof,
    expected: ToolContext,
}

impl OriginalDelivery<'_> {
    pub(super) fn with_current<T>(&self, send: impl FnOnce() -> T) -> Result<T> {
        let refused = || Error::rejected("media original authority ended — delivery refused");
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
                self.shared.original_mount_current(
                    self.token,
                    self.key,
                    self.origin,
                    self.action_token,
                    &self.expected,
                )?;
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
