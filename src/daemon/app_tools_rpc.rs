//! CAD-1177: the standalone tool invocation entry — a mounted app screen
//! calls one declared logical alias through the host broker WITHOUT a
//! workflow file, workflow run, assigned agent, owner PM or team.
//!
//! This is a SEPARATE operator-session-bound entry next to the strict
//! run-bound `app_run_capability_call` (which keeps its assigned-turn
//! proof unchanged). The authority chain is the same guarded primitives,
//! never a relaxation:
//!
//! - `operator_connection` — a registered agent or detached caller is
//!   refused before anything else (the frame can't reach this verb;
//!   only the trusted host relays it for a live mount);
//! - the request's REAL session credential is re-proven natively
//!   (`session_for_credential`), exactly as `app_screen_mint` does —
//!   never a caller-claimed session id;
//! - the live mount action context (`tool_contexts`, minted at consume
//!   and bound server-side to the verified session + install + digest +
//!   tag + generation + declared tool map) is matched — a stale,
//!   foreign or missing token refuses;
//! - the install's LIVE digest and approval pin are re-proven and must
//!   still equal the context's minted digest — a revoke or a package
//!   swap between mount and call fails closed;
//! - the alias must be one the package itself declared in its
//!   `app-screens/v2` `tools` map, resolving to a `needs.capabilities`
//!   slot — an undeclared alias or a slot the install does not declare
//!   refuses before any binding/custody/provider work;
//! - the resolved slot's `effect` must be `read` or `draft` — a `send`
//!   (publish) never reaches this read/draft executor; it is refused
//!   before mutation (send goes through the separate host-owned
//!   approval/effect machinery);
//! - `app_binding_live` re-proves the current reviewed binding;
//!   `app_capability_quote` re-quotes the live price at invoke (the
//!   operator's own session is the approval — there is no separate
//!   frozen run approval to compare, and the adapter re-enforces the
//!   charge ceiling at execution);
//! - `app_tool_claim` reserves the one paid op BEFORE provider I/O and
//!   `app_tool_record` retains exactly one immutable receipt keyed by
//!   the caller's stable `request_id` (SEC-003 idempotency).
//!
//! The frame supplies only `{alias, input, request_id}`; install,
//! digest, slot, binding, provider, account, credential and effect
//! class are all derived host-side — never child- or caller-chosen.

use std::sync::Arc;
use std::time::Instant;

use serde_json::{json, Value};

use super::app_bindings_rpc::strict_fields;
use super::{required_str, Shared};
use crate::contract_fixture::{classify_call, Effect};
use crate::error::{Error, Result};
use crate::issue::app_catalog::workspace;
use crate::operator_auth::Origin;
use crate::platform::{AppCapabilityError, ImageReason};
use crate::store::app_runs;

/// One mounted screen's action context. Minted at `app_screen_mint`
/// against the cap's proven (session, install, tag, generation) — but
/// `live` only once that exact mount is consumed, and bound at consume
/// to the consumed mount's server-recognized `mount` identity. A pending
/// (never-consumed), superseded or torn-down context refuses on
/// `tool_context_proven`. Held only in memory — never the frame, never
/// a caller field.
#[derive(Clone)]
pub(crate) struct ToolContext {
    pub install_id: String,
    /// Active context proven by the trusted mount request. Install-only
    /// historical tool receipts remain contextless; only new actions on
    /// this mount gain this context scope.
    pub context_id: Option<String>,
    pub digest: String,
    /// The screen tag this mount was minted for — a same-install remount
    /// on a different tag is a different mount and supersedes it.
    pub tag: String,
    /// The host's monotonic mount counter for this install — carried so
    /// a superseded generation cannot invoke.
    pub generation: u64,
    /// Server-recognized identity of the CONSUMED mount this context was
    /// activated for (`bridge_nonce`). `None` while still pending — a
    /// mint that never reached consume stays un-invocable.
    pub mount: Option<String>,
    /// Verified minting session's FULL credential hash — re-proven live
    /// on every call so a revoked session kills the context.
    pub session: String,
    /// Declared alias → capability slot, copied from the integrity-
    /// checked package at mint. The closed invocable set.
    pub tools: std::collections::BTreeMap<String, String>,
    pub issued: Instant,
}

/// Bounded action-context map: ≤256 live mounts' contexts process-wide.
pub(crate) const TOOL_CTX_GLOBAL: usize = 256;
/// Action-context lifetime — a mount's context outlives its frame but
/// is re-proven against the live session + install + current mount on
/// every call, so a stale mount cannot invoke even while the token is
/// within TTL.
pub(crate) const TOOL_CTX_TTL: std::time::Duration = std::time::Duration::from_secs(3600);
/// The alias grammar a tool call may name (matches the declaration).
const ALIAS_MAX: usize = 64;
const REQUEST_ID_MAX: usize = 128;
/// Tool input is a bounded JSON object.
const INPUT_BYTES: usize = 64 * 1024;

fn session_hash_ok(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// How a failed generation call settles its intent, from the adapter's
/// TYPED outcome — never from message text. Only a `Refused` (the provider
/// confirmed it did not execute) is terminal; every `Uncertain` keeps the
/// intent so a retry reuses the same idempotency key and no second hold
/// is ever placed under a new request id.
#[derive(Debug, PartialEq, Eq)]
enum GenerationSettlement {
    Refused,
    Pending,
    Uncertain,
}

fn generation_settlement(error: &AppCapabilityError) -> GenerationSettlement {
    match error {
        AppCapabilityError::Refused(_) => GenerationSettlement::Refused,
        // `idempotency_in_progress` is the provider's own typed code, carried
        // in the uncertain reason; it only picks the pending sub-state.
        AppCapabilityError::Uncertain(reason) if reason.contains("idempotency_in_progress") => {
            GenerationSettlement::Pending
        }
        AppCapabilityError::Uncertain(_) => GenerationSettlement::Uncertain,
    }
}

impl Shared {
    /// Re-prove the caller's real session the way `app_screen_mint` does:
    /// the request's own token/key/origin are relayed and the daemon's
    /// `Auth` resolves the live credential hash + `SessionView`.
    /// Returns `(full credential hash, view)`; `None` on any failure —
    /// indistinguishable from "no session", failing closed. The view is
    /// needed for the public-surface operator-role requirement below.
    pub(super) fn tool_session(
        &self,
        token: &str,
        key: &str,
        origin: Origin,
    ) -> Option<(String, crate::operator_auth::SessionView)> {
        let now = self.operator_now();
        let mut auth = self.operator_auth();
        let hash = match origin {
            Origin::Public => auth.public_session_hash(token, now),
            _ => auth.session_hash(token, key, origin, now),
        }
        .ok()
        .flatten()?;
        let view = match origin {
            Origin::Public => auth.check_public(token, now),
            _ => auth.check(token, key, origin, now),
        }
        .ok()
        .flatten()?;
        Some((hash, view))
    }

    /// Is the stored context's minting session still live in `Auth`?
    /// Fail-closed on any lookup error — a revoked session ends the mount.
    fn tool_session_live(&self, session_hash: &str) -> bool {
        if !session_hash_ok(session_hash) {
            return false;
        }
        let now = self.operator_now();
        let mut auth = self.operator_auth();
        auth.live_hash(session_hash, now).unwrap_or(false)
    }

    /// Resolve and re-prove the action context a tool call names. Returns
    /// the validated context, refusing on: unknown/expired token, dead
    /// minting session, a session the request does not actually carry,
    /// or an install whose live digest moved off the minted one.
    pub(super) fn tool_context_proven(
        &self,
        token: &str,
        session_hash: &str,
    ) -> Result<ToolContext> {
        if !session_hash_ok(token) {
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
        // The request must carry the SAME session the mount was minted
        // for — a different operator session cannot borrow the mount.
        if ctx.session != session_hash {
            return Err(Error::rejected(
                "tool action context does not belong to this session",
            ));
        }
        // The minting session must still be live (revocation kills it).
        if !self.tool_session_live(&ctx.session) {
            let mut map = self.tool_contexts.lock().unwrap_or_else(|e| e.into_inner());
            map.remove(token);
            return Err(Error::rejected("the mounting session is no longer live"));
        }
        // The mint must have reached a real consumed mount. A pending
        // context (`mount: None` — the document was never consumed, or
        // the consume was superseded before activation) is refused; a
        // superseded/torn-down mount has already been removed from the
        // map above, so reaching here with `Some` proves the context is
        // still bound to a live consumed mount.
        if ctx.mount.is_none() {
            return Err(Error::rejected(
                "tool action context has no live mount — the screen is not consumed",
            ));
        }
        Ok(ctx)
    }

    /// Activate the pending action context for a freshly consumed mount,
    /// binding it to the consumed mount's `mount` identity
    /// (`bridge_nonce`), and retire every OTHER context for the same
    /// (session, install) — a superseded or remounted screen's old
    /// action handle must refuse. Called by `app_screen_consume` once
    /// the mount's own proofs pass; returns the action token the host
    /// continues to hold.
    pub(crate) fn activate_tool_context(&self, cap: &crate::daemon::app_screens_rpc::ScreenCap) {
        let mut map = self.tool_contexts.lock().unwrap_or_else(|e| e.into_inner());
        // Keep ONLY this mount's own context — a remount/generation bump
        // or a same-install remount supersedes every other context this
        // session holds for the install. The kept one (same session,
        // install, tag and generation) is then bound to the consumed
        // mount identity.
        map.retain(|_, ctx| {
            !(ctx.session == cap.session && ctx.install_id == cap.install_id)
                || (ctx.tag == cap.tag && ctx.generation == cap.generation)
        });
        for ctx in map.values_mut() {
            if ctx.session == cap.session
                && ctx.install_id == cap.install_id
                && ctx.tag == cap.tag
                && ctx.generation == cap.generation
            {
                ctx.mount = Some(cap.bridge_nonce.clone());
            }
        }
    }

    /// Revoke the action context bound to a mount the host is tearing
    /// down — the mount's own action handle stops working the moment the
    /// screen unmounts. No-op when the token is already gone (a
    /// supersede/revoke won the race). Server-derived authority only;
    /// the frame never names a token.
    pub(crate) fn revoke_tool_context(&self, token: &str) {
        let mut map = self.tool_contexts.lock().unwrap_or_else(|e| e.into_inner());
        map.remove(token);
    }

    /// `app_tool_invoke {action_token, tool_alias, input, request_id, token,
    /// key, origin}` — the standalone guarded tool entry. CLOSED params.
    /// `token`/`key`/`origin` relay the consuming board request's own
    /// session credentials so the daemon proves them natively; the
    /// action context binds the call to one verified mount.
    pub(super) fn rpc_app_tool_invoke(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        self.operator_connection("app tool invoke", params, peer_pid)?;
        strict_fields(
            params,
            &[
                "action_token",
                "tool_alias",
                "input",
                "request_id",
                "generation_scope",
                "token",
                "key",
                "origin",
            ],
        )?;
        let action_token = required_str(params, "action_token")?;
        let alias = required_str(params, "tool_alias")?;
        let request_id = required_str(params, "request_id")?;
        let token = required_str(params, "token")?;
        let key = params.get("key").and_then(Value::as_str).unwrap_or("");
        let origin = match params.get("origin").and_then(Value::as_str) {
            Some("loopback") => Origin::Loopback,
            Some("tailnet") => Origin::Tailnet,
            Some("public") => Origin::Public,
            _ => {
                return Err(Error::rejected(
                    "tool invoke needs a classified session origin",
                ))
            }
        };
        let input = params.get("input").cloned().unwrap_or_else(|| json!({}));
        if !input.is_object() || serde_json::to_vec(&input)?.len() > INPUT_BYTES {
            return Err(Error::rejected(
                "tool input must be a JSON object within 64 KiB",
            ));
        }
        crate::proto::identifier(request_id, "tool request id")?;
        if request_id.len() > REQUEST_ID_MAX || alias.len() > ALIAS_MAX {
            return Err(Error::rejected(
                "tool alias or request id exceeds its bound",
            ));
        }
        // Prove the real session natively, then the live mount context it
        // must match. Both fail closed before any binding/custody work.
        // On the public surface the session must ALSO carry the operator
        // role (a public `member` session never invokes a tool) — the same
        // gate `app_screen_mint` applies to mounting a screen at all.
        let (session, view) = self.tool_session(token, key, origin).ok_or_else(|| {
            Error::rejected("tool invoke needs a live operator session — sign in again")
        })?;
        if origin == Origin::Public {
            let operator = view
                .user
                .as_ref()
                .is_some_and(crate::operator_auth::BoardUser::is_operator);
            if !operator {
                return Err(Error::rejected(
                    "tool invoke needs an operator-role public session",
                ));
            }
        }
        let ctx = self.tool_context_proven(action_token, &session)?;

        // The alias must be one the mount's own approved package declared,
        // resolving to its declared capability slot — before any broker work.
        let slot = ctx
            .tools
            .get(alias)
            .cloned()
            .ok_or_else(|| Error::rejected("tool is not declared by this screen"))?;

        // Re-prove the live installation digest + approval pin and the
        // declared-slot binding, all under the PM/custody/release locks.
        let pm_dir = self.pm_dir()?;
        let pm = self.pm_at(&pm_dir)?;
        workspace::with_runtime_snapshot(&pm, &ctx.install_id, |bundle, files| {
            let _custody = self
                .platform_custody_lock
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let _release = self
                .app_release_lock
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let live_digest = required_str(bundle, "digest")?;
            if live_digest != ctx.digest {
                return Err(Error::rejected(
                    "installation digest changed since mount — tool call refused",
                ));
            }
            let status = self
                .store
                .app_capability_status(&ctx.install_id, live_digest)?;
            if status["state"].as_str() != Some("approved") {
                return Err(Error::rejected(
                    "installation is not approved at its current digest",
                ));
            }
            // The declared slot must be a real `needs.capabilities` key of
            // THIS install (the manifest is the source of truth, not the
            // screen's own claim).
            let manifest = crate::issue::app::parse_manifest(
                files
                    .get("app.md")
                    .ok_or_else(|| Error::rejected("installation manifest unavailable"))?,
            )?;
            let declaration = manifest.capabilities.get(slot.as_str()).ok_or_else(|| {
                Error::rejected("tool resolves to a slot the app does not declare")
            })?;
            declaration.validate()?;
            // A standalone invoke is a read/draft only — never a send.
            // The slot's declared effect must itself be read/draft, so a
            // publish alias is refused at the door before any execution.
            if !matches!(declaration.effect.as_str(), "read" | "draft") {
                return Err(Error::rejected(
                    "standalone tools never run a send/publish effect",
                ));
            }
            // Idempotency FIRST: a repeated request id returns its
            // retained receipt without a new provider dispatch/charge.
            if let Some(existing) = self.store.app_tool_result_for_request(request_id)? {
                let same = existing["install_id"] == ctx.install_id
                    && existing["alias"] == json!(alias)
                    && existing["input_digest"] == json!(app_runs::material_digest(&input));
                if !same {
                    return Err(Error::rejected(
                        "tool request id was already used for a different intent",
                    ));
                }
                if let (Some(context), Some(completed), Some(receipt_id)) = (
                    ctx.context_id.as_deref(),
                    existing["created_at"].as_f64(),
                    existing["id"].as_str(),
                ) {
                    let scoped = crate::store::app_records::RecordStore::open(
                        &self.state_dir,
                        &ctx.install_id,
                    )?;
                    scoped.app_social_tool_receipt_attach(context, receipt_id, completed)?;
                    scoped.app_social_generation_complete_request(request_id, receipt_id)?;
                    if existing["result"]["kind"] == "social.source.posts" {
                        let handle = existing["result"]["handle"]
                            .as_str()
                            .unwrap_or("")
                            .trim()
                            .trim_start_matches('@')
                            .to_ascii_lowercase();
                        let configured = scoped.app_social_sources_show(context)?;
                        if configured["handles"].as_array().is_some_and(|hs| {
                            hs.iter().any(|v| v.as_str() == Some(handle.as_str()))
                        }) {
                            scoped.app_social_freshness_save(
                                context, &handle, receipt_id, completed,
                            )?;
                        }
                    }
                }
                return Ok(json!({"receipt": existing, "replayed": true}));
            }
            // Resolve the current reviewed binding for the declared slot.
            let proof = self
                .app_binding_live(
                    &ctx.install_id,
                    ctx.context_id.as_deref(),
                    &slot,
                    bundle,
                    files,
                )?
                .ok_or_else(|| Error::rejected("tool capability binding is absent"))?;
            let mut saved_handle = None;
            if declaration.capability == "social.read" || declaration.action == "list_posts" {
                let context = ctx.context_id.as_deref().ok_or_else(|| {
                    Error::rejected("standalone social reads need a mounted context")
                })?;
                let requested = input["handle"]
                    .as_str()
                    .ok_or_else(|| Error::rejected("social read needs a configured source handle"))?
                    .trim()
                    .trim_start_matches('@')
                    .to_ascii_lowercase();
                let saved =
                    crate::store::app_records::RecordStore::open(&self.state_dir, &ctx.install_id)?
                        .app_social_sources_show(context)?;
                if !saved["handles"].as_array().is_some_and(|items| {
                    items.iter().any(|h| h.as_str() == Some(requested.as_str()))
                }) {
                    return Err(Error::rejected(
                        "social read handle is not saved in this context",
                    ));
                }
                saved_handle = Some(requested);
            }
            // Re-quote at invoke — the operator's session is the approval;
            // the adapter re-enforces the charge ceiling at execution.
            let quote = self.app_capability_quote(&proof)?;
            let config = &proof.config;
            let provider = required_str(config, "provider")?;
            let tool = required_str(&config["mapping"], "tool")?;
            let effect = required_str(&config["mapping"], "effect")?;
            let adapter = self
                .platforms
                .get(provider)
                .ok_or_else(|| Error::rejected("bound capability adapter unavailable"))?;
            if !matches!(effect, "read" | "draft")
                || classify_call(
                    adapter.table(),
                    adapter.reported_manifest_version().as_deref(),
                    tool,
                ) != if effect == "read" {
                    Effect::Read
                } else {
                    Effect::Draft
                }
            {
                return Err(Error::rejected(
                    "bound tool is not a current read/draft action",
                ));
            }
            let input_digest = app_runs::material_digest(&input);
            let generation_capability = matches!(
                declaration.capability.as_str(),
                "text.generate" | "media.generate"
            );
            let mut generation: Option<(
                crate::store::app_records::RecordStore,
                String,
                String,
                Value,
                Value,
            )> = None;
            let mut adapter_input = input.clone();
            let mut authority_inputs = input.clone();
            // The source authority freezes the validated saved handle under
            // the workflow input name `profile_handle`, as a run does.
            if let Some(handle) = saved_handle {
                adapter_input = json!({"handle": handle});
                authority_inputs = json!({"profile_handle": handle});
            }
            let mut authority_source = Value::Null;
            let mut draft_snapshot: Option<Value> = None;
            let generation_caller_digest = input_digest.clone();
            if generation_capability {
                let spec = params
                    .get("generation_scope")
                    .and_then(Value::as_object)
                    .ok_or_else(|| {
                        Error::rejected("generation tool needs a stable draft-operation scope")
                    })?;
                if !(spec.len() == 3
                    && spec.contains_key("operation")
                    && spec.contains_key("draft_id")
                    && spec.contains_key("revision"))
                {
                    return Err(Error::rejected("generation scope fields are invalid"));
                }
                let operation = spec
                    .get("operation")
                    .and_then(Value::as_str)
                    .ok_or_else(|| Error::rejected("generation scope operation is missing"))?;
                let expected = if declaration.capability == "text.generate" {
                    "caption"
                } else {
                    "image"
                };
                if operation != expected {
                    return Err(Error::rejected(
                        "generation scope operation differs from the bound tool",
                    ));
                }
                let context = ctx
                    .context_id
                    .as_deref()
                    .ok_or_else(|| Error::rejected("generation needs a mounted active context"))?;
                let draft = spec
                    .get("draft_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| Error::rejected("generation needs a draft id"))?;
                let revision = spec
                    .get("revision")
                    .and_then(Value::as_i64)
                    .filter(|v| *v > 0)
                    .ok_or_else(|| Error::rejected("generation needs a positive draft revision"))?;
                crate::proto::identifier(draft, "generation draft ID")?;
                let records =
                    crate::store::app_records::RecordStore::open(&self.state_dir, &ctx.install_id)?;
                if records.app_social_draft_show(context, draft)?["revision"].as_i64()
                    != Some(revision)
                {
                    return Err(Error::rejected("generation draft revision is stale"));
                }
                let scope = json!({"operation":operation,"draft_id":draft,"revision":revision});
                let intent_digest = app_runs::material_digest(
                    &json!({"context":context,"alias":alias,"tool":tool,"scope":scope}),
                );
                let mut plan = input.clone();
                if operation == "image" {
                    let image_idea = image_idea(&input)?;
                    // Standalone image generation is grounded in the draft's
                    // retained source, never a caller-supplied receipt or facts.
                    let current = records.app_social_draft_show(context, draft)?;
                    draft_snapshot = Some(current.clone());
                    let source_ref = &current["source"];
                    if source_ref["kind"] != "tool_receipt" {
                        return Err(Error::rejected(
                            "standalone image generation needs a retained tool-receipt source",
                        ));
                    }
                    let receipt_id = source_ref["receipt_id"]
                        .as_str()
                        .ok_or_else(|| Error::rejected("draft source receipt is missing"))?;
                    let post_id = source_ref["post_id"]
                        .as_str()
                        .ok_or_else(|| Error::rejected("draft source post is missing"))?;
                    let receipt = self.store.app_tool_result(receipt_id)?;
                    if receipt["install_id"] != ctx.install_id
                        || receipt["result"]["kind"] != "social.source.posts"
                        || !self.store.app_tool_source_post_exists(
                            &ctx.install_id,
                            receipt_id,
                            post_id,
                        )?
                    {
                        return Err(Error::rejected(
                            "draft source post is not present in an owned retained receipt",
                        ));
                    }
                    let posts = receipt["result"]["posts"]
                        .as_array()
                        .ok_or_else(|| Error::rejected("source receipt has no normalized posts"))?;
                    let mut selected = posts.iter().filter(|post| post["id"] == post_id);
                    let post = selected.next().ok_or_else(|| {
                        Error::rejected("draft source post is absent from its receipt")
                    })?;
                    if selected.next().is_some() {
                        return Err(Error::rejected("draft source post is ambiguous"));
                    }
                    let caption = post["caption"]
                        .as_str()
                        .ok_or_else(|| Error::rejected("draft source post has no caption"))?;
                    let frozen_source = crate::issue::workflow::source_input_line(caption)?;
                    let subject = current["caption"]
                        .as_str()
                        .unwrap_or("")
                        .chars()
                        .take(120)
                        .collect::<String>();
                    if subject.trim().is_empty() {
                        return Err(Error::rejected(
                            "draft caption cannot seed an image subject",
                        ));
                    }
                    plan = json!({
                        "subject": subject,
                        "source": frozen_source,
                        "brand_voice": "",
                        "image_prompt": image_idea,
                    });
                    adapter_input = json!({});
                    authority_inputs = plan.clone();
                    authority_source = json!({
                        "receipt_id": receipt_id,
                        "post": post,
                        "post_digest": app_runs::material_digest(post),
                    });
                }
                let gate = records.app_social_generation_begin_with(
                    context,
                    &intent_digest,
                    &crate::store::app_social_drafts::GenerationStart {
                        alias,
                        tool,
                        caller_input_digest: &generation_caller_digest,
                        input: &plan,
                        scope: &scope,
                        request: request_id,
                    },
                )?;
                match gate["mode"].as_str() {
                    Some("existing") => {
                        let mut intent = gate["intent"].clone();
                        // The app validates this reply with `outcome: null`; the
                        // reason code rides the draft listing instead.
                        if operation == "image" {
                            intent["outcome"] = Value::Null;
                        }
                        return Ok(json!({"generation_intent":intent,"replayed":true}));
                    }
                    Some("terminal") => {
                        return Err(Error::rejected(
                            "generation intent already reached a terminal outcome",
                        ))
                    }
                    Some("start" | "retry") => {
                        // On an explicit retry, the retained plan wins over
                        // changed defaults or source projections.
                        if gate["mode"] == "retry" {
                            plan = gate["intent"]["input"].clone();
                            if operation == "image" {
                                adapter_input = json!({});
                                authority_inputs = plan.clone();
                                let current = draft_snapshot.as_ref().ok_or_else(|| {
                                    Error::rejected("draft image source snapshot is unavailable")
                                })?;
                                let receipt_id =
                                    current["source"]["receipt_id"].as_str().ok_or_else(|| {
                                        Error::rejected("draft source receipt is missing")
                                    })?;
                                let receipt = self.store.app_tool_result(receipt_id)?;
                                let post_id =
                                    current["source"]["post_id"].as_str().ok_or_else(|| {
                                        Error::rejected("draft source post is missing")
                                    })?;
                                let post = receipt["result"]["posts"]
                                    .as_array()
                                    .and_then(|posts| {
                                        posts.iter().find(|post| post["id"] == post_id)
                                    })
                                    .ok_or_else(|| {
                                        Error::rejected("retained image source post is unavailable")
                                    })?;
                                authority_source = json!({"receipt_id":receipt_id,"post":post,"post_digest":app_runs::material_digest(post)});
                            }
                        }
                        // CAD-1315: an image intent that already has its job
                        // never submits again from here. An unresolved one is
                        // re-checked under the SAME key; an active one just
                        // gets a worker if none is alive.
                        if operation == "image" {
                            if let Some(job) =
                                records.app_social_image_job_for_request(request_id)?
                            {
                                if job.state == "uncertain" {
                                    records.app_social_image_job_resume(
                                        &job.call_id,
                                        self.image_job_deadline(),
                                    )?;
                                }
                                self.spawn_image_job_worker(&ctx.install_id, &job.call_id);
                                return Ok(pending_image_reply(request_id, &scope, &plan));
                            }
                        }
                        generation = Some((records, context.to_owned(), intent_digest, scope, plan))
                    }
                    _ => return Err(Error::rejected("generation intent state is invalid")),
                }
            } else if params.get("generation_scope").is_some() {
                return Err(Error::rejected(
                    "generation scope is accepted only by reviewed text/image generation tools",
                ));
            }
            let call_id = format!(
                "app-tool-{}",
                uuid::Uuid::new_v5(
                    &uuid::Uuid::NAMESPACE_OID,
                    format!("{}:{}", ctx.install_id, request_id).as_bytes()
                )
                .simple()
            );
            // Reserve the one paid op BEFORE provider I/O; a repeat of the
            // same id admits only the byte-identical intent.
            self.store
                .app_tool_claim(crate::store::app_tools::AppToolClaim {
                    request: request_id,
                    install: &ctx.install_id,
                    alias,
                    slot: &slot,
                    binding_digest: &proof.digest,
                    input_digest: &input_digest,
                    call_id: &call_id,
                })?;
            // Standalone invocation authority is distinct from workflow-run
            // authority; never manufacture run-shaped identifiers here.
            let mut authority = json!({
                "schema": 1,
                "install_id": ctx.install_id,
                "context_id": ctx.context_id.clone().map(Value::String).unwrap_or(Value::Null),
                "slot": slot,
                "binding": proof,
                "inputs": authority_inputs,
                "source": authority_source,
                "quote": quote,
                "call_id": call_id,
                "invocation_id": call_id,
            });
            // CAD-1315: image generation is a queued job. Freeze the exact
            // prompt (a restart replays a byte-identical body under the same
            // key), write the job row and return; a host worker does the
            // provider I/O outside every lock held here.
            if let Some((records, context, intent, scope, plan)) = &generation {
                if scope["operation"] == "image" {
                    match crate::platform::agenticos_external::frozen_image_prompt(
                        &authority,
                        &adapter_input,
                    ) {
                        Ok(prompt) => authority["frozen_prompt"] = json!(prompt),
                        Err(_) => {
                            records.app_social_generation_finish(
                                context,
                                intent,
                                request_id,
                                "refused",
                                None,
                                Some(ImageReason::PlanInvalid.code()),
                            )?;
                            return Err(Error::rejected("image plan is invalid"));
                        }
                    }
                    let spec = json!({
                        "alias": alias, "slot": slot, "binding_digest": proof.digest,
                        "input_digest": input_digest, "input": input, "authority": authority,
                    });
                    records.app_social_image_job_start(
                        context,
                        intent,
                        request_id,
                        &call_id,
                        &spec,
                        self.image_job_deadline(),
                    )?;
                    self.spawn_image_job_worker(&ctx.install_id, &call_id);
                    return Ok(pending_image_reply(request_id, scope, plan));
                }
            }
            let credential = self.app_capability_credential(config)?;
            let output = match adapter.execute_app_capability_outcome(
                &credential,
                &authority,
                &adapter_input,
                &call_id,
            ) {
                Ok(output) => output,
                Err(error) => {
                    if let Some((records, context, intent, scope, plan)) = &generation {
                        let settlement = generation_settlement(&error);
                        if settlement == GenerationSettlement::Refused {
                            records.app_social_generation_finish(
                                context,
                                intent,
                                request_id,
                                "refused",
                                None,
                                Some("verified_provider_refusal"),
                            )?;
                            return Err(Error::rejected(error.to_string()));
                        }
                        let pending = settlement == GenerationSettlement::Pending;
                        let state = if pending { "pending" } else { "uncertain" };
                        let reason = if pending {
                            "upstream_idempotency_in_progress"
                        } else {
                            "upstream_outcome_uncertain"
                        };
                        records.app_social_generation_finish(
                            context,
                            intent,
                            request_id,
                            state,
                            None,
                            Some(reason),
                        )?;
                        return Ok(
                            json!({"generation_intent":{"request_id":request_id,"operation":scope["operation"],"draft_id":scope["draft_id"],"revision":scope["revision"],"input_digest":app_runs::material_digest(plan),"input":plan,"state":state,"updated_at":crate::issue::time::now_epoch()},"replayed":true}),
                        );
                    }
                    return Err(Error::rejected(error.to_string()));
                }
            };
            crate::platform::refuse_leak(
                "app tool result",
                &output.result.to_string(),
                &credential,
            )?;
            if let Some(asset) = &output.asset {
                crate::platform::refuse_leak(
                    "app tool asset",
                    &String::from_utf8_lossy(&asset.bytes),
                    &credential,
                )?;
            }
            let receipt = self
                .store
                .app_tool_record(crate::store::app_tools::AppToolRecord {
                    id: &call_id,
                    request: request_id,
                    install: &ctx.install_id,
                    alias,
                    slot: &slot,
                    binding_digest: &proof.digest,
                    input_digest: &input_digest,
                    input: &input,
                    result: &output.result,
                    asset: output
                        .asset
                        .as_ref()
                        .map(|asset| (asset.media_type.as_str(), asset.bytes.as_slice())),
                })?;
            if let Some((records, context, intent, _scope, _plan)) = &generation {
                records.app_social_generation_finish(
                    context,
                    intent,
                    request_id,
                    "completed",
                    receipt["id"].as_str(),
                    None,
                )?;
            }
            if let Some(context) = ctx.context_id.as_deref() {
                if let Some(created) = receipt["created_at"].as_f64() {
                    crate::store::app_records::RecordStore::open(&self.state_dir, &ctx.install_id)?
                        .app_social_tool_receipt_attach(
                            context,
                            required_str(&receipt, "id")?,
                            created,
                        )?;
                }
            }
            if output.result["kind"].as_str() == Some("social.source.posts") {
                if let (Some(context), Some(handle), Some(created)) = (
                    ctx.context_id.as_deref(),
                    output.result["handle"].as_str(),
                    receipt["created_at"].as_f64(),
                ) {
                    let requested = input["handle"]
                        .as_str()
                        .unwrap_or("")
                        .trim()
                        .trim_start_matches('@')
                        .to_ascii_lowercase();
                    let fetched = handle.trim().trim_start_matches('@').to_ascii_lowercase();
                    if requested == fetched {
                        crate::store::app_records::RecordStore::open(
                            &self.state_dir,
                            &ctx.install_id,
                        )?
                        .app_social_freshness_save(
                            context,
                            &fetched,
                            required_str(&receipt, "id")?,
                            created,
                        )?;
                    }
                }
            }
            Ok(json!({"receipt": receipt, "replayed": false}))
        })
    }

    /// `app_tool_revoke {action_token}` — the trusted host tearing a
    /// mount down revokes its action handle. `operator_connection`-gated;
    /// the token is the host-held mount proof, never the frame. A
    /// session-scoped revoke only fires when the request's proven session
    /// owns the context, so one operator session cannot tear down
    /// another's mount. Idempotent — an already-gone token is a no-op.
    pub(super) fn rpc_app_tool_revoke(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        self.operator_connection("app tool revoke", params, peer_pid)?;
        strict_fields(params, &["action_token", "token", "key", "origin"])?;
        let action_token = required_str(params, "action_token")?;
        let token = required_str(params, "token")?;
        let key = params.get("key").and_then(Value::as_str).unwrap_or("");
        let origin = match params.get("origin").and_then(Value::as_str) {
            Some("loopback") => Origin::Loopback,
            Some("tailnet") => Origin::Tailnet,
            Some("public") => Origin::Public,
            _ => {
                return Err(Error::rejected(
                    "tool revoke needs a classified session origin",
                ))
            }
        };
        let Some((session, _view)) = self.tool_session(token, key, origin) else {
            return Err(Error::rejected("tool revoke needs a live operator session"));
        };
        // Only revoke a context the proven session actually owns — a
        // foreign session's token is left untouched (and not leaked).
        let owned = {
            let map = self.tool_contexts.lock().unwrap_or_else(|e| e.into_inner());
            map.get(action_token)
                .is_some_and(|ctx| ctx.session == session)
        };
        if owned {
            self.revoke_tool_context(action_token);
        }
        Ok(json!({"revoked": owned}))
    }

    /// `app_tool_result {receipt_id}` / `app_tool_results {install_id}` —
    /// operator reads of retained standalone tool receipts.
    pub(super) fn rpc_app_tool_result(&self, params: &Value, peer_pid: u32) -> Result<Value> {
        self.operator_connection("app tool result", params, peer_pid)?;
        let object = params
            .as_object()
            .ok_or_else(|| Error::rejected("app tool result params must be an object"))?;
        match (
            object.contains_key("receipt_id"),
            object.contains_key("install_id"),
        ) {
            (true, false) if object.len() == 1 => self
                .store
                .app_tool_result(required_str(params, "receipt_id")?),
            (false, true) if object.len() == 1 => self
                .store
                .app_tool_results(required_str(params, "install_id")?),
            (false, true) if object.len() == 2 && object.contains_key("context_id") => {
                let install = required_str(params, "install_id")?;
                let context = required_str(params, "context_id")?;
                self.store.app_context_proof(install, context)?;
                let records =
                    crate::store::app_records::RecordStore::open(&self.state_dir, install)?;
                let visible = records.app_social_freshness_receipts(context)?;
                let mut results = self.store.app_tool_results(install)?;
                if let Some(rows) = results["results"].as_array_mut() {
                    rows.retain(|row| {
                        row["id"]
                            .as_str()
                            .is_some_and(|id| visible.iter().any(|allowed| allowed == id))
                    });
                }
                Ok(results)
            }
            _ => Err(Error::rejected(
                "app tool result takes exactly receipt_id or install_id",
            )),
        }
    }
}

#[cfg(test)]
mod generation_settlement_tests {
    use super::*;

    #[test]
    fn only_a_confirmed_refusal_settles_a_generation_intent_terminally() {
        assert_eq!(
            generation_settlement(&AppCapabilityError::Refused("refused".into())),
            GenerationSettlement::Refused
        );
        for reason in [
            "AgenticOS text generation outcome is uncertain: uncertain",
            "AgenticOS text generation outcome is uncertain: generation_result_unavailable",
            "AgenticOS text generation outcome is uncertain: rate_limited",
        ] {
            assert_eq!(
                generation_settlement(&AppCapabilityError::Uncertain(reason.into())),
                GenerationSettlement::Uncertain,
                "{reason}"
            );
        }
        assert_eq!(
            generation_settlement(&AppCapabilityError::Uncertain(
                "AgenticOS text generation outcome is uncertain: idempotency_in_progress".into()
            )),
            GenerationSettlement::Pending
        );
    }
}

/// The reply for an image intent whose job is queued or running. It is the
/// shape a pending intent has always had, so an older app bundle validates it.
fn pending_image_reply(request_id: &str, scope: &Value, plan: &Value) -> Value {
    json!({"generation_intent":{"request_id":request_id,"operation":scope["operation"],"draft_id":scope["draft_id"],"revision":scope["revision"],"input_digest":app_runs::material_digest(plan),"input":plan,"state":"pending","updated_at":crate::issue::time::now_epoch()},"replayed":true})
}

/// CAD-1303: the owner's per-post image idea. The image tool input is `{}` or
/// exactly `{image_prompt}`: plain text, at most 512 characters, no control
/// characters. Anything else is refused, never truncated or dropped.
fn image_idea(input: &Value) -> Result<String> {
    let fields = input
        .as_object()
        .ok_or_else(|| Error::rejected("image input must be an object"))?;
    if fields.is_empty() {
        return Ok(String::new());
    }
    let idea = match (fields.len(), fields.get("image_prompt")) {
        (1, Some(Value::String(text))) => text.trim(),
        _ => {
            return Err(Error::rejected(
                "image input accepts only an image_prompt text",
            ))
        }
    };
    if idea.is_empty() || idea.chars().count() > 512 || idea.chars().any(char::is_control) {
        return Err(Error::rejected(
            "image idea must be plain text of at most 512 characters",
        ));
    }
    Ok(idea.to_owned())
}

#[cfg(test)]
mod image_idea_tests {
    use super::image_idea;
    use serde_json::json;

    #[test]
    fn image_idea_is_trimmed_bounded_plain_text_and_never_truncated() {
        assert_eq!(image_idea(&json!({})).unwrap(), "");
        assert_eq!(
            image_idea(&json!({"image_prompt": "  a calm bowl of noodles "})).unwrap(),
            "a calm bowl of noodles"
        );
        assert_eq!(
            image_idea(&json!({"image_prompt": "x".repeat(512)}))
                .unwrap()
                .len(),
            512
        );
        for bad in [
            json!({"image_prompt": "x".repeat(513)}),
            json!({"image_prompt": ""}),
            json!({"image_prompt": "   "}),
            json!({"image_prompt": "line\nbreak"}),
            json!({"image_prompt": "nul\u{0}"}),
            json!({"image_prompt": 7}),
            json!({"image_prompt": "ok", "subject": "forged"}),
            json!({"source": "forged"}),
            json!([]),
        ] {
            assert!(image_idea(&bad).is_err(), "{bad}");
        }
    }
}
