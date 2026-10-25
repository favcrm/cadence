//! CAD-1006 R3 (revised): the two operator-side screen verbs.
//!
//! `app_screen_mint` — `operator_connection`-gated. Relays the requesting
//! session's REAL bearer credential (`token` + `key` + `origin`) and runs
//! the daemon's native check itself — `Auth::check` for a keyed
//! loopback/tailnet session or `Auth::check_public` for the `__Host-`
//! public cookie — resolving the live `SessionView.id`. CAD-1537: a public
//! session without `BoardUser::is_operator()` (a `member`), or a mint the
//! `--read-only` board relays with `read_only: true`, gets a read-only cap:
//! its projection and service calls are narrowed to
//! `ScreenOperation::is_screen_read`, and it carries no tools or media.
//! A caller-supplied session *id* is never accepted as authority. On
//! success it recomputes the live `bundle_digest`, requires
//! `app_capability_status(install_id, live_digest).state == "approved"`,
//! re-runs [`crate::issue::app_screen_pkg::extract`], and mints one
//! 256-bit frame capability plus a DISTINCT non-authorizing
//! `bridge_nonce` (bound into the cap) into [`Shared::screen_caps`].
//! Returns `{mount, bridge_nonce, generation, tag}` to the trusted host.
//!
//! `app_screen_consume {nonce}` — the SOLE authority is the one-use
//! FrameCap bearer plus the native operator peer guard
//! (`operator_connection`) that denies a registered-agent or detached
//! caller even when it somehow holds a valid nonce. The board's
//! headerless same-origin `GET /api/app-screen/<nonce>` navigation
//! relays ONLY the nonce — no cookie, key, or session field crosses.
//! Consume atomically BURNS the cap first, then re-proves: the stored
//! minting `session` id is still live in `Auth`, the installation's live
//! digest still equals the minted digest, the approval pin still holds,
//! and the package still validates — a revoke/upgrade between mint and
//! consume fails closed. The `bridge_nonce`/`generation`/`tag` rendered
//! into the bootstrap are the cap's stored values, never child-chosen.
//!
//! CAD-1777: an optional host `parent_action_token` on mint pairs a
//! DRAWER child with a live, consumed, non-read-only PRIMARY action
//! context of the same records screen, install, digest, approval epoch
//! and context — proven natively and re-proved at consume under the
//! action-context lock. A child consume retires only a prior child of the
//! same parent (one childslot) and never the parent; consuming or
//! revoking the parent retires its live and pending children. The token
//! never enters the frame, its bootstrap, a PUSH or a log.
//!
//! Token/key material is used only inside `Auth`; it is never stored in
//! a cap, logged, or pushed to the frame.

use std::time::Instant;

use serde_json::{json, Value};

use super::{required_str, Shared};
use crate::error::{Error, Result};
use crate::issue::app_screen_pkg;
use crate::operator_auth::Origin;

/// One outstanding frame capability. `session` is the VERIFIED minting
/// session's display id — resolved through `Auth` from the request's
/// real credential at mint time, never a caller field.
pub(crate) struct ScreenCap {
    pub install_id: String,
    pub digest: String,
    pub tag: String,
    /// Active app context this mount was proven for. Held so a drawer
    /// child's parent re-proof compares the exact same context, not a
    /// caller-supplied one.
    pub context_id: Option<String>,
    /// Verified minting session's FULL credential hash (server-resolved
    /// at mint) — the cap's internal binding. Never the 8-hex display
    /// prefix, so a surviving prefix-colliding row cannot keep a revoked
    /// mint's cap live. Internal only — never rendered or transmitted.
    pub session: String,
    /// Drawer child only: the bounded internal binding to the PRIMARY
    /// action context proven at mint. `None` for a primary mount. Re-proved
    /// at consume, so a parent replaced after mint refuses the child.
    pub parent: Option<super::app_tools_rpc::ParentBinding>,
    /// The exact server-minted pending action context this cap was issued
    /// with. A drawer child's activation requires THIS context to still be
    /// pending and matching, so a cap superseded by another child refuses
    /// instead of disturbing the live childslot. Internal only — the same
    /// value the host already receives as `action_token` in the receipt;
    /// never rendered or transmitted by the frame path.
    pub action_token: Option<String>,
    /// Host mount counter for this install — a superseded mount's mint
    /// is refused at consume.
    pub generation: u64,
    /// Native consent epoch of the approval proven at mint. Consume and
    /// every tool call re-compare it, so a remove+restore or revoke+reconsent
    /// of the same digest cannot revive this cap. Internal only.
    pub approval_epoch: i64,
    /// The DISTINCT non-authorizing bridge nonce minted here and bound
    /// into this cap; the host renders it into the bootstrap and the
    /// child echoes it in init. NOT the FrameCap bearer in the URL —
    /// the host recognizes its own mount's init by this stored value.
    pub bridge_nonce: String,
    /// CAD-1537: minted for a member or a `--read-only` board — consume
    /// narrows the projection to read operations and no tools.
    pub read_only: bool,
    pub issued: Instant,
}

/// CAD-1537: the operations a mount may advertise — every declared one
/// for an operator mount, only the read allowlist for a read-only one.
fn mount_operations(
    pkg: &app_screen_pkg::ScreenPackage,
    read_only: bool,
) -> Vec<crate::issue::app_screen_decl::ScreenOperation> {
    pkg.operations
        .iter()
        .copied()
        .filter(|op| !read_only || op.is_screen_read())
        .collect()
}

/// Bounded capability map: ≤128 global, ≤4 per install, ≤8 per session.
pub(crate) const CAP_GLOBAL: usize = 128;
pub(crate) const CAP_PER_INSTALL: usize = 4;
pub(crate) const CAP_PER_SESSION: usize = 8;
/// One-use frame capability lifetime.
pub(crate) const CAP_TTL: std::time::Duration = std::time::Duration::from_secs(60);
/// Mint RATE bound — mints per verified session per rolling 60 s window.
/// This bounds the expensive digest/approval/package re-proof each mint
/// runs, distinct from the outstanding-cap count.
pub(crate) const MINT_RATE_PER_SESSION: usize = 64;
pub(crate) const MINT_RATE_WINDOW: std::time::Duration = std::time::Duration::from_secs(60);
/// Bound the rate map itself (a session-id → timestamps entry).
const MINT_RATE_SESSIONS: usize = 128;

/// A fresh 256-bit nonce as 64 lowercase hex chars (CSPRNG).
fn mint_nonce() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("getrandom");
    let mut out = String::with_capacity(64);
    for b in bytes {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// The nonce grammar — exactly 64 lowercase hex, nothing else. Refuses
/// before any map lookup.
fn nonce_ok(nonce: &str) -> bool {
    nonce.len() == 64
        && nonce
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// A stored minting-session hash is exactly a `sha256(token)` — 64
/// lowercase hex. Refuse anything else before trusting it as a key.
fn session_hash_ok(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// What a pending tool context is bound to when its cap is minted.
#[derive(Clone, Copy)]
struct ToolContextBinding<'a> {
    install_id: &'a str,
    context_id: Option<&'a str>,
    digest: &'a str,
    tag: &'a str,
    generation: u64,
    approval_epoch: i64,
    read_only: bool,
    /// Drawer child only: the proved PRIMARY binding (host token derived
    /// into an internal identity). `None` for a primary mount.
    parent: Option<&'a super::app_tools_rpc::ParentBinding>,
}

impl Shared {
    /// Resolve a session's real bearer credential to its live
    /// `SessionView` through the daemon's own `Auth` — `token` is the
    /// request's session cookie value, `key` the `X-Cadence-Session`
    /// page key (absent on the public cookie-only surface), `origin` the
    /// request's classified origin. Returns `None` when the credential
    /// is absent, dead, revoked or malformed — never an error, so a
    /// lookup failure is indistinguishable from "no such session" and
    /// fails closed. The token is used only inside `Auth`, never stored.
    /// Resolve the request's real session bearer credential to a
    /// `(session, view)` pair: `session` is the verified row's FULL
    /// credential hash — the internal value the cap binds to (never the
    /// 8-hex display id, so a prefix collision with a surviving row
    /// cannot keep a revoked mint's cap live) — and `view` the public
    /// `SessionView` for the public-role check. Returns `None` when the
    /// credential is absent, dead, revoked or malformed. The token/key
    /// are consumed by `Auth` only — never stored here.
    fn session_for_credential(
        &self,
        token: &str,
        key: &str,
        origin: Origin,
    ) -> Option<(String, crate::operator_auth::SessionView)> {
        if origin == Origin::Public {
            return match self.current_public_session(token).ok()? {
                crate::daemon::public_membership::Authority::Granted { hash, view, .. } => {
                    Some((hash, *view))
                }
                crate::daemon::public_membership::Authority::Denied => None,
            };
        }
        let now = self.operator_now();
        let mut auth = self.operator_auth();
        let hash = auth.session_hash(token, key, origin, now).ok().flatten()?;
        let view = auth.check(token, key, origin, now).ok().flatten()?;
        Some((hash, view))
    }

    /// Whether the minting session is still live — re-verified in `Auth`
    /// at consume so a revoke between mint and consume kills the cap.
    /// `session_hash` is the cap's stored FULL credential hash; a
    /// different row sharing the 8-hex display prefix does not satisfy
    /// it. Fail-closed on any lookup error.
    /// CAD-1537: `read_only` — a read-only cap's public member session
    /// counts as live; every other cap still needs a current owner.
    fn session_id_live(&self, session_hash: &str, read_only: bool) -> bool {
        if !session_hash_ok(session_hash) {
            return false;
        }
        match self.public_row_hash(session_hash, read_only) {
            Some(public) => public,
            None => {
                let now = self.operator_now();
                let mut auth = self.operator_auth();
                auth.live_hash(session_hash, now).unwrap_or(false)
            }
        }
    }

    /// Rolling-window mint-rate check for one verified session. Sweeps
    /// timestamps older than the window; refuses when the session already
    /// minted [`MINT_RATE_PER_SESSION`] in it; otherwise records this mint.
    /// Runs BEFORE the expensive live digest/approval/package re-proof —
    /// the caller is already a proven operator + verified session, so the
    /// rate bound is the cheap gate. The map is bounded and swept.
    fn check_mint_rate(&self, session: &str) -> Result<()> {
        let now = Instant::now();
        let mut map = self
            .screen_mint_rate
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // Sweep expired windows / empty entries so the map stays bounded.
        map.retain(|_, ts| {
            ts.retain(|t| now.duration_since(*t) < MINT_RATE_WINDOW);
            !ts.is_empty()
        });
        // A session not yet tracked may not be admitted if the map is
        // already at its session bound.
        if !map.contains_key(session) && map.len() >= MINT_RATE_SESSIONS {
            return Err(Error::rejected("screen mint rate map is full"));
        }
        let entry = map.entry(session.to_string()).or_default();
        if entry.len() >= MINT_RATE_PER_SESSION {
            return Err(Error::rejected(
                "screen mint rate exceeded — too many mounts in this window",
            ));
        }
        entry.push(now);
        Ok(())
    }

    /// Drop expired capabilities — the sweep mint runs before admit.
    fn sweep_screen_caps(&self) {
        let now = Instant::now();
        self.screen_caps
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|_, cap| now.duration_since(cap.issued) < CAP_TTL);
    }

    /// `app_screen_mint {install_id, tag, token, key, origin,
    /// generation, read_only?, parent_action_token?}` — a CLOSED param set:
    /// any other field refuses, so a forged authority field cannot ride a
    /// valid credential.
    /// `read_only` (CAD-1537) can only narrow: the `--read-only` board
    /// relays `true`; any other value refuses. A public `member` session
    /// is read-only whatever the host sends.
    /// `parent_action_token` (CAD-1777) is the trusted host's optional
    /// drawer pairing: the action token of the PRIMARY mount this child
    /// is opened for. It is proven server-side here — never trusted as
    /// authority — and never returned to, or accepted from, the child.
    /// `token`/`key`/`origin` are the consuming board request's own
    /// session credentials, relayed so the *daemon* proves them — never
    /// a caller-claimed session id. `generation` is the host's mount
    /// counter. Returns `{mount, bridge_nonce, generation, tag}`.
    pub(super) fn rpc_app_screen_mint(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        self.operator_connection("app screen mint", params, peer_pid)?;
        const ALLOWED: &[&str] = &[
            "install_id",
            "context_id",
            "tag",
            "token",
            "key",
            "origin",
            "generation",
            "read_only",
            "parent_action_token",
        ];
        let fields = params
            .as_object()
            .ok_or_else(|| Error::rejected("screen mint params must be an object"))?;
        let host_read_only = match fields.get("read_only") {
            None => false,
            Some(Value::Bool(true)) => true,
            Some(_) => return Err(Error::rejected("screen mint read_only can only be true")),
        };
        if fields.keys().any(|k| !ALLOWED.contains(&k.as_str())) {
            return Err(Error::rejected(
                "screen mint params have unsupported fields",
            ));
        }
        let install_id = required_str(params, "install_id")?;
        let parent_action_token = match fields.get("parent_action_token") {
            None => None,
            Some(Value::String(token)) if !token.is_empty() => Some(token.as_str()),
            Some(_) => {
                return Err(Error::rejected(
                    "screen mint parent_action_token must be a non-empty string",
                ))
            }
        };
        let context_id = params
            .get("context_id")
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
            .map(str::to_owned);
        if let Some(context) = context_id.as_deref() {
            // The host may select a context, but cannot assert its authority:
            // prove it is live and owned by this exact installation here.
            let (_, proof) = self.store.app_context_proof(install_id, context)?;
            if proof.install_id != install_id {
                return Err(Error::rejected(
                    "screen context belongs to another installation",
                ));
            }
        }
        let tag = required_str(params, "tag")?;
        let token = required_str(params, "token")?;
        let key = params.get("key").and_then(Value::as_str).unwrap_or("");
        // The origin MUST be a real classification — never default to
        // loopback on an unknown/missing value.
        let origin = match params.get("origin").and_then(Value::as_str) {
            Some("loopback") => Origin::Loopback,
            Some("tailnet") => Origin::Tailnet,
            Some("public") => Origin::Public,
            _ => {
                return Err(Error::rejected(
                    "screen mint needs a classified session origin",
                ))
            }
        };
        let generation = params
            .get("generation")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        // Verify the real credential natively. CAD-1537: a public session
        // without the operator role (a `member`) mounts read-only.
        let (session, view) = self
            .session_for_credential(token, key, origin)
            .ok_or_else(|| {
                Error::rejected("screen mint needs a live operator session — sign in again")
            })?;
        let member = origin == Origin::Public
            && !view
                .user
                .as_ref()
                .is_some_and(crate::operator_auth::BoardUser::is_operator);
        let read_only = host_read_only || member;
        // `session` is the verified row's full credential hash — the
        // cap's internal binding. Bounded mint-rate gate BEFORE the expensive live digest/approval/
        // package re-proof — the caller is a proven operator + verified
        // session, so this cheap bound is what an abusive caller hits first.
        self.check_mint_rate(&session)?;
        let pm_dir = self.pm_dir()?;
        let pm = self.pm_at(&pm_dir)?;
        let (digest, approval_epoch, pkg) = self.screen_package_checked(&pm, install_id, tag)?;
        // CAD-1777: a drawer child is admitted ONLY as a paired
        // `screen-records.v1` screen over the same live, non-read-only
        // primary action context. The host's token is a lookup key; every
        // proof field below is native. Every other contract and every
        // token-less mint keeps its existing primary behavior.
        let parent = match parent_action_token {
            None => None,
            Some(parent_token) => {
                if read_only {
                    return Err(Error::rejected(
                        "a read-only mount cannot host a drawer child",
                    ));
                }
                if pkg.host_contract.as_deref()
                    != Some(crate::issue::app_screen_decl::HOST_CONTRACT_RECORDS)
                {
                    return Err(Error::rejected(
                        "only a records screen admits a drawer child",
                    ));
                }
                let Some(context) = context_id.as_deref() else {
                    return Err(Error::rejected(
                        "a drawer child needs its primary active context",
                    ));
                };
                Some(self.prove_drawer_parent(
                    parent_token,
                    &super::app_tools_rpc::DrawerParentScope {
                        session: &session,
                        install_id,
                        context_id: Some(context),
                        tag,
                        digest: &digest,
                        approval_epoch,
                    },
                )?)
            }
        };

        self.sweep_screen_caps();
        let mut caps = self.screen_caps.lock().unwrap_or_else(|e| e.into_inner());
        if caps.len() >= CAP_GLOBAL {
            return Err(Error::rejected("screen capability map is full"));
        }
        // CAD-1537: read-only caps never take the operator's capacity —
        // they hold at most half the global map, and the per-install bound
        // counts operator and read-only caps apart.
        if read_only && caps.values().filter(|c| c.read_only).count() >= CAP_GLOBAL / 2 {
            return Err(Error::rejected("screen capability map is full"));
        }
        if caps
            .values()
            .filter(|c| c.install_id == install_id && c.read_only == read_only)
            .count()
            >= CAP_PER_INSTALL
        {
            return Err(Error::rejected(
                "too many live screen mounts for this installation",
            ));
        }
        if caps.values().filter(|c| c.session == session).count() >= CAP_PER_SESSION {
            return Err(Error::rejected(
                "too many live screen mounts for this session",
            ));
        }
        let nonce = mint_nonce();
        // The bridge nonce is minted WITH the cap (distinct bytes from
        // the FrameCap) and returned to the trusted host so it can render
        // it into the bootstrap and later recognize the echoing init.
        let bridge_nonce = mint_nonce();
        // CAD-1177: a PENDING action context is minted WITH the cap — an
        // opaque token bound server-side to this mount's verified session
        // + install + tag + generation + digest + declared tool map, with
        // `mount: None`. The host holds it and relays it on
        // `app_tool_invoke`; it is never the FrameCap or bridge nonce and
        // never enters the frame bootstrap or a child message. It only
        // becomes invocable when `app_screen_consume` activates it — a
        // mint that is never consumed, or is superseded, stays refused.
        // No tools, records permissions or media contract → no token (read-only screen).
        // A media-only screen (screen-media.v1 with media.read/publish)
        // still mints a context with empty tools, so media actions work.
        let action_token = self.mint_tool_context(
            &session,
            &ToolContextBinding {
                install_id,
                context_id: context_id.as_deref(),
                digest: &digest,
                tag,
                generation,
                approval_epoch,
                read_only,
                parent: parent.as_ref(),
            },
            &pkg,
        );
        // CAD-1777: a drawer child is only usable through its own exact
        // pending action context. Refuse a child mint that produced none
        // rather than issuing a cap that can never activate.
        if parent.is_some() && action_token.is_null() {
            return Err(Error::rejected("a drawer child needs an action context"));
        }
        let cap_action_token = action_token.as_str().map(str::to_owned);
        // Stable display partition, never service authorization. The full
        // session hash stays private; cache keys survive same-scope remounts.
        let display_scope_key = context_id.as_ref().map(|context| {
            use sha2::{Digest, Sha256};
            format!(
                "{:x}",
                Sha256::digest(
                    serde_json::to_vec(&json!([
                        "screen-display/v1",
                        session,
                        install_id,
                        context,
                        digest
                    ]))
                    .expect("display scope serialization")
                )
            )
        });
        caps.insert(
            nonce.clone(),
            ScreenCap {
                install_id: install_id.to_string(),
                digest,
                tag: tag.to_string(),
                context_id: context_id.clone(),
                session,
                parent,
                action_token: cap_action_token,
                generation,
                approval_epoch,
                bridge_nonce: bridge_nonce.clone(),
                read_only,
                issued: Instant::now(),
            },
        );
        // Aliases the screen declared, advertised to the host only when a
        // tool context was actually minted. Naming an alias grants nothing:
        // every invoke still re-proves binding, approval and operator.
        let tools: Vec<&String> = if action_token.is_null() || read_only {
            Vec::new()
        } else {
            pkg.tools.keys().collect()
        };
        // CAD-1365: host-held media metadata for a package that pins
        // `screen-media.v1` and minted a live action context. It is the
        // package's own DECLARED media permission subset (never wider),
        // emitted only when nonempty — a non-media package carries no
        // `media` field at all. This is declaration metadata, not a new
        // authority secret, and never enters the frame bootstrap.
        let media_may: Vec<&String> = pkg
            .may
            .iter()
            .filter(|m| {
                m.as_str() == crate::issue::app_screen_decl::MAY_MEDIA_READ
                    || m.as_str() == crate::issue::app_screen_decl::MAY_MEDIA_PUBLISH
                    || m.as_str() == crate::issue::app_screen_decl::MAY_MEDIA_CHAT_ATTACH
            })
            .collect();
        let media = if pkg.host_contract.as_deref()
            == Some(crate::issue::app_screen_decl::HOST_CONTRACT_MEDIA)
            && !action_token.is_null()
            && !read_only
            && !media_may.is_empty()
        {
            json!({"contract": crate::issue::app_screen_decl::HOST_CONTRACT_MEDIA, "may": media_may})
        } else {
            Value::Null
        };
        let mut receipt = json!({
            "mount": format!("/api/app-screen/{nonce}"),
            "bridge_nonce": bridge_nonce,
            "generation": generation,
            "tag": tag,
            "action_token": action_token,
            "tools": tools,
        });
        if !media.is_null() {
            receipt["media"] = media;
        }
        if pkg.host_contract.as_deref()
            == Some(crate::issue::app_screen_decl::HOST_CONTRACT_RECORDS)
        {
            receipt["host_contract"] = json!(pkg.host_contract);
            receipt["operations"] = json!(mount_operations(&pkg, read_only));
            receipt["navigation"] = json!(pkg.navigation);
            receipt["display_scope_key"] = json!(display_scope_key);
            // Non-authorizing trusted-host metadata; never bootstrap/PUSH.
            use sha2::{Digest, Sha256};
            receipt["display_session_key"] = json!(format!(
                "{:x}",
                Sha256::digest(
                    serde_json::to_vec(&json!([
                        "screen-display-session/v1",
                        caps.get(&nonce).expect("just inserted cap").session
                    ]))
                    .expect("display session serialization")
                )
            ));
            receipt["install_id"] = json!(install_id);
            receipt["context_id"] = json!(context_id);
            receipt["digest"] = json!(caps.get(&nonce).expect("just inserted cap").digest);
            receipt["remote_images"] = json!(pkg.remote_images);
        }
        Ok(receipt)
    }

    /// `app_screen_consume {nonce}` — a CLOSED param set: exactly
    /// `{nonce}`, nothing else. `operator_connection` peer guard denies a
    /// registered agent or detached caller even holding a valid nonce;
    /// then the cap is BURNED atomically before any further check
    /// (single-use), the stored minting session must still be live, and
    /// the live digest/approval/package re-proof runs. No cookie/key/
    /// session field is accepted or read.
    pub(super) fn rpc_app_screen_consume(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        // The native peer guard: an agent-derived or detached connection
        // is denied outright — a stolen nonce still cannot be spent by
        // one. This is what lets the headerless local GET stay safe.
        self.operator_connection("app screen consume", params, peer_pid)?;
        let fields = params
            .as_object()
            .ok_or_else(|| Error::rejected("screen consume params must be an object"))?;
        if fields.keys().any(|k| k != "nonce") {
            return Err(Error::rejected("screen consume admits only a nonce"));
        }
        let nonce = required_str(params, "nonce")?;
        if !nonce_ok(nonce) {
            return Err(Error::rejected("invalid screen capability"));
        }
        // Atomic burn — remove the cap before further verification, so a
        // second consume of the same nonce finds nothing (single-use).
        let cap = {
            let mut caps = self.screen_caps.lock().unwrap_or_else(|e| e.into_inner());
            match caps.remove(nonce) {
                Some(cap) => cap,
                None => return Err(Error::rejected("screen capability is spent or unknown")),
            }
        };
        if Instant::now().duration_since(cap.issued) >= CAP_TTL {
            return Err(Error::rejected("screen capability expired"));
        }
        // The minting session must still be live — a revoke between mint
        // and consume kills the cap. The id was server-resolved at mint.
        if !self.session_id_live(&cap.session, cap.read_only) {
            return Err(Error::rejected("the minting session is no longer live"));
        }
        let pm_dir = self.pm_dir()?;
        let pm = self.pm_at(&pm_dir)?;
        // Re-verify the LIVE bundle digest + approval + package — a
        // revoke or upgrade between mint and consume refuses here.
        let (live_digest, live_epoch, pkg) =
            self.screen_package_checked(&pm, &cap.install_id, &cap.tag)?;
        if live_digest != cap.digest {
            return Err(Error::rejected(
                "installation digest changed since mint — mount refused",
            ));
        }
        if live_epoch != cap.approval_epoch {
            return Err(Error::rejected(
                "installation was re-approved or revoked since mint — mount refused",
            ));
        }
        // CAD-1177: activate the pending action context for THIS consumed
        // mount — bind it to `cap.bridge_nonce` and retire every other
        // context the session holds for this install (a remount
        // supersedes the stale action handle). CAD-1777: a drawer child's
        // activation re-proves its PRIMARY under the same lock and fails
        // the mount closed when the parent is gone; it never retires the
        // parent, only a prior child of the same parent.
        self.activate_tool_context(&cap)?;
        let mut result = json!({
            "tag": pkg.tag,
            "app": pkg.app,
            "digest": live_digest,
            "install_id": cap.install_id,
            "generation": cap.generation,
            "bridge_nonce": cap.bridge_nonce,
            "contract": pkg.contract,
            "may": pkg.may,
            "tools": if cap.read_only { json!({}) } else { json!(pkg.tools) },
            "manifest": pkg.manifest,
            "assets": pkg.assets,
            "remote_images": pkg.remote_images,
        });
        if pkg.host_contract.as_deref()
            == Some(crate::issue::app_screen_decl::HOST_CONTRACT_RECORDS)
        {
            result["host_contract"] = json!(pkg.host_contract);
            result["operations"] = json!(mount_operations(&pkg, cap.read_only));
            result["navigation"] = json!(pkg.navigation);
        }
        Ok(result)
    }

    /// Mint one PENDING in-memory action context for a freshly minted
    /// cap — bound to the cap's proven (session, install, tag,
    /// generation), `mount: None`. It becomes invocable only when
    /// `app_screen_consume` activates it against the real consumed
    /// mount. Returns the opaque token the trusted host holds (never the
    /// frame); `Value::Null` when the screen declared no tools and no
    /// media contract (no tools and no media yields null). A media-only
    /// screen mints with empty tools.
    fn mint_tool_context(
        &self,
        session: &str,
        binding: &ToolContextBinding<'_>,
        pkg: &app_screen_pkg::ScreenPackage,
    ) -> Value {
        let ToolContextBinding {
            install_id,
            context_id,
            digest,
            tag,
            generation,
            approval_epoch,
            read_only,
            parent,
        } = *binding;
        let media_only = pkg.host_contract.as_deref()
            == Some(crate::issue::app_screen_decl::HOST_CONTRACT_MEDIA)
            && pkg.may.iter().any(|m| {
                m.as_str() == crate::issue::app_screen_decl::MAY_MEDIA_READ
                    || m.as_str() == crate::issue::app_screen_decl::MAY_MEDIA_PUBLISH
                    || m.as_str() == crate::issue::app_screen_decl::MAY_MEDIA_CHAT_ATTACH
            });
        if pkg.tools.is_empty() && pkg.operations.is_empty() && !media_only {
            return Value::Null;
        }
        let token = mint_nonce();
        let mut map = self.tool_contexts.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        map.retain(|_, ctx| now.duration_since(ctx.issued) < super::app_tools_rpc::TOOL_CTX_TTL);
        if map.len() >= super::app_tools_rpc::TOOL_CTX_GLOBAL {
            return Value::Null;
        }
        map.insert(
            token.clone(),
            super::app_tools_rpc::ToolContext {
                install_id: install_id.to_string(),
                context_id: context_id.map(str::to_owned),
                digest: digest.to_string(),
                tag: tag.to_string(),
                generation,
                approval_epoch,
                mount: None,
                session: session.to_string(),
                tools: if read_only {
                    Default::default()
                } else {
                    pkg.tools.clone()
                },
                read_only,
                parent: parent.cloned(),
                exact_primary_active: false,
                issued: now,
            },
        );
        json!(token)
    }

    /// The shared live re-proof used by mint and consume: recompute the
    /// installed workspace bundle digest, require the approval pin,
    /// require the declared screen `app` to equal the installed app
    /// manifest's name, and re-run the package validator over the live
    /// members. Returns the live digest and the integrity-checked
    /// package.
    fn screen_package_checked(
        &self,
        pm: &crate::issue::Pm,
        install_id: &str,
        tag: &str,
    ) -> Result<(String, i64, app_screen_pkg::ScreenPackage)> {
        use crate::issue::app_catalog::workspace;
        let store = &self.store;
        workspace::with_runtime_read(pm, install_id, |row, files| {
            let live_digest = row["digest"]
                .as_str()
                .ok_or_else(|| Error::rejected("installation digest unavailable"))?
                .to_string();
            let status = store.app_capability_status(install_id, &live_digest)?;
            if status["state"].as_str() != Some("approved") {
                return Err(Error::rejected(
                    "installation is not approved at its current digest",
                ));
            }
            // The native consent epoch of THIS approval. Fail closed when
            // absent: an approved status without a valid epoch is refused.
            let approval_epoch = status["epoch"]
                .as_i64()
                .ok_or_else(|| Error::rejected("installation approval epoch unavailable"))?;
            // The screen package's declared app must equal the installed
            // app manifest's name — a package cannot claim a different app.
            let installed_app = crate::issue::app::parse_manifest(
                files
                    .get("app.md")
                    .ok_or_else(|| Error::rejected("installation manifest unavailable"))?,
            )?
            .app;
            let pkg = app_screen_pkg::extract(files, tag)?;
            if pkg.app != installed_app {
                return Err(Error::rejected(
                    "screen package declares an app that is not the installation",
                ));
            }
            Ok((live_digest, approval_epoch, pkg))
        })
    }
}

/// CAD-1123 HP1: the host UI kit every frame document carries — the
/// board's generated tokens (`design/tokens.css`, kept in step with
/// `ui/src/styles.css` by `scripts/design-kit --check`), the design kit's
/// component classes (`design/kit.css`: `.btn`, `.chip`, `.field`,
/// `.card`, `.tabs`, `.scrim`, `.toast`, …) and the frame additions
/// (`.drawer`, `.seg`, `.sheet`). Packages use it instead of vendoring
/// host CSS; its version is stamped on `<html data-cadence-kit>` and in
/// the `#cadence-screen-kit` JSON block.
pub const SCREEN_KIT_CSS: &str = concat!(
    include_str!("../../design/tokens.css"),
    "\n",
    include_str!("../../design/kit.css"),
    "\n",
    include_str!("screen_kit.css"),
);
/// The kit contract version a package can feature-detect.
pub const SCREEN_KIT_VERSION: &str = "cadence-kit/1";

/// Rendered-frame HTML: the static wrapper document carrying the CSP
/// nonce, the bootstrap JSON block, the approved CSS and the approved
/// IIFE — every byte host-verified so the package's own text can never
/// break out of its script/style context. Capped post-escape (768 KiB).
///
/// `csp_nonce` admits the two stamped blocks only; `bridge_nonce` is the
/// non-authorizing init token the child echoes (distinct from the
/// FrameCap bearer in the URL); `parent_origin` is the server-derived
/// exact board origin the child must `postMessage` to. The approved
/// IIFE may intentionally add code or read what it was granted — this
/// wrapper only prevents *accidental* display-injection and
/// unapproved-code execution; it does not constrain what approved code
/// deliberately does. The mount point is a generic `<div id="root">` —
/// the frozen app's own IIFE auto-mounts `#root`/`[data-sc-root]`; the
/// host emits no app-specific symbol.
pub fn render_frame_html(
    csp_nonce: &str,
    tag: &str,
    bridge_nonce: &str,
    generation: u64,
    css: &str,
    js: &str,
    parent_origin: &str,
) -> Result<String> {
    render_frame_document(
        csp_nonce,
        tag,
        bridge_nonce,
        generation,
        (&[css], &[js]),
        parent_origin,
    )
}

/// Revalidate public, caller-constructible package bytes before selecting
/// scripts. The declaration alone supplies explicit order; absent startup
/// retains entry-only execution, regardless of the map's file order.
pub fn render_package_frame_html(
    csp_nonce: &str,
    tag: &str,
    bridge_nonce: &str,
    generation: u64,
    package: &crate::issue::app_screen_pkg::ScreenPackage,
    parent_origin: &str,
) -> Result<String> {
    let checked = crate::issue::app_screen_decl::validate_map(&package.manifest, &package.assets)?;
    let mut styles = Vec::new();
    for (name, body) in &package.assets {
        if name.ends_with(".js") {
            check_script_text(body)?;
        } else if name.ends_with(".css") {
            let css = check_style_text(body)?;
            // Preserve the legacy first-stylesheet selection while checking
            // every supplied stylesheet, including inert leaves.
            if styles.is_empty() {
                styles.push(css);
            }
        }
    }
    let scripts: Vec<&str> = match checked.startup() {
        Some(order) => order
            .iter()
            .map(|name| package.assets[name].as_str())
            .collect(),
        None => vec![package.assets[checked.entry()].as_str()],
    };
    render_frame_document(
        csp_nonce,
        tag,
        bridge_nonce,
        generation,
        (&styles, &scripts),
        parent_origin,
    )
}

fn render_frame_document(
    csp_nonce: &str,
    tag: &str,
    bridge_nonce: &str,
    generation: u64,
    assets: (&[&str], &[&str]),
    parent_origin: &str,
) -> Result<String> {
    let (styles, scripts) = assets;
    let boot = json!({
        "tag": tag,
        "bridge_nonce": bridge_nonce,
        "generation": generation,
        "parent_origin": parent_origin,
    })
    .to_string();
    let boot = escape_json_script(&boot);
    // The kit version lives in its own block: the CAD-1006 boot block is
    // a closed shape that shipped packages validate key by key.
    let kit_boot = escape_json_script(&json!({ "kit": SCREEN_KIT_VERSION }).to_string());
    let csp_nonce = csp_nonce
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    let mut style_blocks = String::new();
    for css in styles {
        style_blocks.push_str(&format!("<style>{}</style>", check_style_text(css)?));
    }
    let mut script_blocks = String::new();
    for js in scripts {
        script_blocks.push_str(&format!(
            "<script nonce=\"{csp_nonce}\">{}</script>",
            check_script_text(js)?
        ));
    }
    let html = format!(
        "<!doctype html><html data-cadence-kit=\"{kit}\"><head><meta charset=\"utf-8\">\
<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
<script nonce=\"{csp}\" type=\"application/json\" id=\"cadence-screen-boot\">{boot}</script>\
<script nonce=\"{csp}\" type=\"application/json\" id=\"cadence-screen-kit\">{kit_boot}</script>\
<style id=\"cadence-kit\">{kit_css}</style>\
{style_blocks}\
</head><body><div id=\"root\"></div>\
{script_blocks}\
</body></html>",
        csp = csp_nonce,
        boot = boot,
        kit = SCREEN_KIT_VERSION,
        kit_boot = kit_boot,
        kit_css = SCREEN_KIT_CSS,
        style_blocks = style_blocks,
        script_blocks = script_blocks,
    );
    if html.len() > 768 * 1024 {
        return Err(Error::rejected(
            "rendered screen document exceeds its bound",
        ));
    }
    Ok(html)
}

/// `<script type="application/json">`-safe escaping for the bootstrap:
/// JSON string escaping with `\\uXXXX` — `<`,`>`,`&` and the JS line
/// separators become `\\u003c`/`\\u003e`/`\\u0026`/`\\u2028`/`\\u2029`
/// literals the JSON parser restores, while the raw-text tokenizer
/// never sees a `</script`/`<!--` token. NOT HTML entities — inside a
/// raw-text `<script>` an `&lt;` reaches `JSON.parse` as a literal and
/// corrupts the field; `\\u003c` is the only correct form here.
fn escape_json_script(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '<' => out.push_str("\\u003c"),
            '>' => out.push_str("\\u003e"),
            '&' => out.push_str("\\u0026"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            _ => out.push(c),
        }
    }
    out
}

/// Emit the approved IIFE source EXACTLY — a raw-text `<script>` block
/// cannot contain `</script`, `<!--` or `-->` (the HTML parser would
/// terminate the block or open a comment), so the mount FAILS CLOSED on
/// a case-insensitive match rather than rewriting bytes (a `\\u003c`
/// backslash-escape would corrupt operators/regex/React `</tag>`).
/// Preserving the exact approved source is what makes the approved
/// bytes' sha256 the executed bytes.
fn check_script_text(text: &str) -> Result<&str> {
    let lower = text.to_ascii_lowercase();
    if lower.contains("</script") || lower.contains("<!--") || lower.contains("-->") {
        return Err(Error::rejected(
            "screen entry contains a forbidden HTML-script end token",
        ));
    }
    Ok(text)
}

/// Emit the approved CSS EXACTLY — a `<style>` raw-text block cannot
/// contain a case-insensitive `</style` end token; fail closed instead
/// of rewriting (a rewritten stylesheet is not the approved bytes).
fn check_style_text(text: &str) -> Result<&str> {
    if text.to_ascii_lowercase().contains("</style") {
        return Err(Error::rejected(
            "screen stylesheet contains a forbidden </style end token",
        ));
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_preserves_exact_js_and_refuses_end_tokens() {
        let clean = "(function(){var r=1<2&&2>1;return r;})();";
        let h = render_frame_html(
            "cspnonce",
            "main",
            "bridgenonce64",
            1,
            "a{color:red}",
            clean,
            "http://h",
        );
        assert!(h.is_ok());
        let h = h.unwrap();
        // Exact bytes preserved — verbatim, not backslash-rewritten.
        assert!(h.contains(clean));
        assert!(
            h.contains("<div id=\"root\"></div>"),
            "no generic #root mount: {h}"
        );
        assert!(h.contains("cadence-screen-boot"));
        // Bootstrap uses \uXXXX not HTML entities.
        let h2 = render_frame_html("csp", "main", "b<nonce>", 1, "a{}", "x()", "http://h").unwrap();
        assert!(!h2.contains("&lt;"), "bootstrap used HTML entities: {h2}");
        assert!(
            h2.contains("\\u003c"),
            "bootstrap not \\uXXXX-escaped: {h2}"
        );
        // End tokens refuse, never rewrite.
        for bad in ["a</script>b", "a<!--b", "a-->b", "a</ScRiPt>b"] {
            assert!(
                render_frame_html("csp", "m", "b", 1, "a{}", bad, "http://h").is_err(),
                "end-token IIFE admitted: {bad}"
            );
        }
        // CSS </style refuses.
        assert!(render_frame_html("csp", "m", "b", 1, "a</style>x", "x()", "http://h").is_err());
    }

    #[test]
    fn nonce_grammar_is_exact_64_lower_hex() {
        assert!(nonce_ok(&"a".repeat(64)));
        assert!(nonce_ok(&"0123456789abcdef".repeat(4)));
        assert!(!nonce_ok(&"a".repeat(63)));
        assert!(!nonce_ok(&"a".repeat(65)));
        assert!(!nonce_ok(&"A".repeat(64)));
        assert!(!nonce_ok(&"g".repeat(64)));
        assert!(!nonce_ok(""));
    }

    #[test]
    fn cad1123_frame_document_carries_the_host_kit_without_package_css() {
        let h =
            render_frame_html("csp", "main", &"b".repeat(64), 1, "", "x()", "http://h").unwrap();
        assert!(
            h.contains("<html data-cadence-kit=\"cadence-kit/1\">"),
            "{h}"
        );
        assert!(h.contains("id=\"cadence-screen-kit\">{\"kit\":\"cadence-kit/1\"}</script>"));
        // The board's generated tokens and the design kit, verbatim.
        let kit_start = h.find("<style id=\"cadence-kit\">").unwrap();
        let kit = &h[kit_start..kit_start + h[kit_start..].find("</style>").unwrap()];
        assert!(kit.contains(include_str!("../../design/tokens.css")));
        assert!(kit.contains(include_str!("../../design/kit.css")));
        for class in [
            ".btn {",
            ".field {",
            ".chip {",
            ".card {",
            ".toast {",
            ".scrim",
            ".tabs",
            ".drawer {",
            ".seg {",
            ".sheet {",
        ] {
            assert!(kit.contains(class), "kit lacks {class}");
        }
        for token in [
            "--color-accent:",
            "prefers-color-scheme: light",
            "[data-theme=\"light\"]",
        ] {
            assert!(kit.contains(token), "kit lacks {token}");
        }
        // The kit precedes the package stylesheet so the package can
        // override it, and the closed CAD-1006 boot block is unchanged.
        assert!(kit_start < h.rfind("<style>").unwrap());
        let boot_start = h.find("cadence-screen-boot\">").unwrap() + "cadence-screen-boot\">".len();
        let boot_end = boot_start + h[boot_start..].find("</script>").unwrap();
        let boot: Value = serde_json::from_str(&h[boot_start..boot_end]).unwrap();
        let mut keys: Vec<_> = boot.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(keys, ["bridge_nonce", "generation", "parent_origin", "tag"]);
    }
}
