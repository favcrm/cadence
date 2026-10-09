//! Explicit operator release of one actual accepted app artifact.
use super::app_bindings_rpc::strict_fields;
use super::*;
use crate::issue::app_catalog::workspace;
use crate::platform::agenticos_external::publish::bare_digest;
use crate::store::{app_bindings::BindingProof, app_effects, app_runs, EffectRow};

// Only positively classified SMTP adds native JSON-content patterns. Opaque
// credentials retain the unchanged global raw whole/Unicode-window screen.
fn publication_refuse_leak(
    legacy_smtp: bool,
    what: &'static str,
    text: &str,
    secret: &[u8],
) -> Result<()> {
    crate::platform::refuse_leak(what, text, secret)?;
    if !legacy_smtp {
        return Ok(());
    }
    let secret = std::str::from_utf8(secret)
        .map_err(|_| Error::rejected("app release credential is invalid — withheld"))?;
    let characters: Vec<_> = secret.chars().collect();
    // Select each original eight-character window BEFORE encoding it.
    for pattern in std::iter::once(secret.to_string()).chain(
        characters
            .windows(8)
            .map(|window| window.iter().collect::<String>()),
    ) {
        let encoded = serde_json::to_string(&pattern)?;
        // Remove exactly the two quotes constructed by string serialization.
        let content = &encoded[1..encoded.len() - 1];
        if content != pattern && text.contains(content) {
            return Err(Error::internal(format!(
                "{what} would carry the enrolled credential — withheld"
            )));
        }
    }
    Ok(())
}

/// The frozen image digest in the canonical bare form. Effects staged before
/// CAD-1304 carry custody's `sha256:<hex>`; both forms read as the same digest.
fn frozen_image_digest(authority: &Value) -> Option<&str> {
    authority["image_digest"].as_str().map(bare_digest)
}

// Callers classify the durable record while holding the custody lock.
fn publication_smtp_envelope(
    legacy_smtp: bool,
    bytes: &[u8],
) -> Result<Option<crate::platform::smtp::SmtpEnvelope>> {
    if !legacy_smtp {
        return Ok(None);
    }
    let (envelope, projection) = crate::platform::smtp::custody_decode(bytes)
        .map_err(|_| Error::rejected("app release credential is invalid — withheld"))?;
    publication_refuse_leak(
        true,
        "smtp publication projection",
        &projection.to_json().to_string(),
        envelope.secret(),
    )?;
    Ok(Some(envelope))
}

pub(crate) fn social_effect_install(id: &str) -> Option<String> {
    let mut p = id.split('_');
    if p.next() != Some("sfx") {
        return None;
    };
    let hex = p.next()?;
    let nonce = p.next()?;
    if p.next().is_some()
        || hex.is_empty()
        || hex.len() % 2 != 0
        || nonce.len() != 32
        || !hex
            .bytes()
            .chain(nonce.bytes())
            .all(|b| b.is_ascii_hexdigit())
    {
        return None;
    };
    let bytes = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok())
        .collect::<Option<Vec<_>>>()?;
    String::from_utf8(bytes).ok()
}
/// CAD-1291: the owner's standing publish grant for the approved
/// destination. Cadence never mints one and never takes a grant id from a
/// request or a binding: it asks the hosted door for the standing grants of
/// this destination and accepts a live one only for exactly this account.
/// The post itself is still gated by the operator's approval, the exact
/// effect digest and the draft, binding, destination and image checks.
fn owner_grant(
    sender: &dyn crate::platform::agenticos_external::publish::PublishSender,
    authority: &Value,
) -> std::result::Result<String, crate::platform::agenticos_external::publish::Refusal> {
    use crate::platform::agenticos_external::publish::{select_grant, GrantWant, Refusal};
    let text = |field: &str| {
        authority[field]
            .as_str()
            .ok_or_else(|| Refusal::new("bad_grant", "approved social effect is incomplete"))
    };
    let destination = text("destination_id")?;
    let found = sender.find_grant(destination)?;
    select_grant(
        &found,
        &GrantWant {
            connection_id: text("aos_connection_id")?,
            destination_id: destination,
            toolkit: text("toolkit")?,
        },
    )
}

fn social_effect_id(install: &str) -> String {
    format!(
        "sfx_{}_{}",
        install
            .as_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>(),
        uuid::Uuid::new_v4().simple()
    )
}

impl Shared {
    fn stage_social_draft_effect(self: &Arc<Self>, params: &Value, pid: u32) -> Result<Value> {
        let expected = [
            "action_token",
            "tool_alias",
            "key",
            "origin",
            "proof",
            "request_id",
            "token",
        ];
        if !params.as_object().is_some_and(|o| {
            o.len() == expected.len() && o.keys().all(|k| expected.contains(&k.as_str()))
        }) {
            return Err(Error::rejected(
                "social draft effect request fields are invalid",
            ));
        }
        let proof = params
            .get("proof")
            .and_then(Value::as_object)
            .ok_or_else(|| Error::rejected("social draft effect needs typed draft proof"))?;
        if proof.len() != 3 || proof.get("kind").and_then(Value::as_str) != Some("social_draft") {
            return Err(Error::rejected("social draft proof fields are invalid"));
        }
        let draft_id = proof
            .get("draft_id")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::rejected("social draft proof needs a draft ID"))?;
        let revision = proof
            .get("revision")
            .and_then(Value::as_i64)
            .filter(|r| *r > 0)
            .ok_or_else(|| Error::rejected("social draft proof needs a positive revision"))?;
        let request = required_str(params, "request_id")?;
        crate::proto::identifier(request, "social draft effect request ID")?;
        let (ctx, context) = self.social_draft_action(params, pid, true)?;
        let pm = self.pm_at(&self.pm_dir()?)?;
        workspace::with_completed_bundle_snapshot(
            &pm,
            &ctx.install_id,
            &ctx.digest,
            |bundle, files| {
                let _release = self
                    .app_release_lock
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                let records =
                    crate::store::app_records::RecordStore::open(&self.state_dir, &ctx.install_id)?;
                let latest = records.app_social_draft_show(&context, draft_id)?;
                if latest["revision"].as_i64() != Some(revision) {
                    return Err(Error::rejected(
                        "social draft changed; review the latest revision",
                    ));
                }
                let frozen_draft =
                    records.app_social_draft_revision(&context, draft_id, revision)?;
                let manifest = crate::issue::app::parse_manifest(
                    files
                        .get("app.md")
                        .ok_or_else(|| Error::rejected("installation manifest unavailable"))?,
                )?;
                let mut send_slots = manifest
                    .capabilities
                    .iter()
                    .filter(|(_, need)| need.effect == "send");
                let (slot, need) = send_slots
                    .next()
                    .ok_or_else(|| Error::rejected("installation declares no send capability"))?;
                if send_slots.next().is_some() {
                    return Err(Error::rejected(
                        "social publish requires exactly one declared send slot",
                    ));
                }
                need.validate()?;
                let binding = self
                    .app_binding_live(&ctx.install_id, Some(&context), slot, bundle, files)?
                    .ok_or_else(|| Error::rejected("social publish binding is absent"))?;
                if binding.config["mapping"]["effect"] != "send" {
                    return Err(Error::rejected(
                        "social publish binding is not send authority",
                    ));
                }
                let target = super::social_publish_start::PublishTarget::from_binding_config(
                    &binding.config,
                )?;
                let resolver = self.social_media_resolver.clone().ok_or_else(|| {
                    Error::rejected("capability_unavailable: no media resolver configured")
                })?;
                let resolved = resolver
                    .resolve(&target.toolkit, &target.destination_id)
                    .into_result()
                    .map_err(|e| Error::rejected(e.to_string()))?;
                let asset_id = frozen_draft["asset_id"].as_str();
                if target.toolkit == "instagram" && asset_id.is_none() {
                    return Err(Error::rejected(
                        "Instagram publish needs a retained JPEG or PNG image",
                    ));
                }
                let (asset_digest, mime, size) = if let Some(asset) = asset_id {
                    let (header, digest, bytes) =
                        self.store.app_tool_asset_bytes(&ctx.install_id, asset)?;
                    if bytes.len() > 2 * 1024 * 1024 {
                        return Err(Error::rejected(
                            "publish image exceeds the 2 MiB retained-media limit",
                        ));
                    }
                    let mime =
                        crate::platform::agenticos_external::image::image_mime(&bytes, &header)
                            .map_err(Error::rejected)?;
                    if !matches!(mime, "image/jpeg" | "image/png") {
                        return Err(Error::rejected("publish image must be JPEG or PNG"));
                    }
                    image::load_from_memory(&bytes)
                        .map_err(|_| Error::rejected("retained publish image cannot be decoded"))?;
                    if crate::store::app_runs::artifact_digest(&bytes) != digest {
                        return Err(Error::rejected("retained publish image digest changed"));
                    }
                    (
                        Some(
                            crate::platform::agenticos_external::publish::bare_digest(&digest)
                                .to_owned(),
                        ),
                        Some(mime.to_string()),
                        Some(bytes.len()),
                    )
                } else {
                    (None, None, None)
                };
                let id = social_effect_id(&ctx.install_id);
                let approval = format!("social-approval-{}", uuid::Uuid::new_v4().simple());
                let key = format!("social_{}", uuid::Uuid::new_v4().simple());
                let frozen = json!({"source":{"kind":"social_draft","draft_id":draft_id,"revision":revision},"install_id":ctx.install_id,"context_id":context,"bundle_digest":ctx.digest,"draft_id":draft_id,"revision":revision,"caption":frozen_draft["caption"],"caption_digest":crate::platform::agenticos_external::publish::caption_digest_of(frozen_draft["caption"].as_str().unwrap_or("")),"asset_id":asset_id,"image_digest":asset_digest,"mime":mime,"size_bytes":size,"toolkit":target.toolkit,"destination_id":target.destination_id,"destination_label":target.destination_label,"timezone":target.timezone,"aos_connection_id":resolved.aos_connection_id,"binding":{"slot":slot,"revision":binding.revision,"digest":binding.digest,"config":binding.config},"effect_id":id,"idempotency_key":key,"approval_id":approval});
                records.app_social_effect_stage(
                    &context,
                    &crate::store::app_social_drafts::EffectStage {
                        draft: draft_id,
                        revision,
                        request,
                        effect_id: &id,
                        frozen: &frozen,
                        approval: &approval,
                    },
                )
            },
        )
    }

    fn publish_social_draft_now(&self, id: &str, digest: &str) -> Result<Value> {
        use crate::platform::agenticos_external::publish::{Preflight, PublishState};
        let install = social_effect_install(id)
            .ok_or_else(|| Error::rejected("not a social draft effect"))?;
        let records = crate::store::app_records::RecordStore::open(&self.state_dir, &install)?;
        let shown = records.app_social_effect_show(id)?;
        if shown["effect"]["digest"] != digest || shown["effect"]["state"] != "approved" {
            return Err(Error::rejected(
                "Publish now requires the exact approved social draft effect",
            ));
        }
        let authority = shown["effect"]["authority"].clone();
        let sender = self.social_publish_sender.clone().ok_or_else(|| {
            Error::rejected("capability_unavailable: publish sender is not registered")
        })?;
        let pm = self.pm_at(&self.pm_dir()?)?;
        let id_owned = id.to_owned();
        let digest_owned = digest.to_owned();
        workspace::with_completed_bundle_snapshot(
            &pm,
            &install,
            required_str(&authority, "bundle_digest")?,
            |bundle, files| {
                let _custody = self
                    .platform_custody_lock
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                let _release = self
                    .app_release_lock
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                let context = required_str(&authority, "context_id")?;
                let slot = required_str(&authority["binding"], "slot")?;
                let current = self
                    .app_binding_live(&install, Some(context), slot, bundle, files)?
                    .ok_or_else(|| Error::rejected("social publish binding is unavailable"))?;
                if current.config["mapping"]["effect"] != "send"
                    || current.digest != authority["binding"]["digest"]
                    || current.revision != authority["binding"]["revision"]
                {
                    return Err(Error::rejected(
                        "social publish binding changed since approval",
                    ));
                }
                let current_draft = records
                    .app_social_draft_show(context, required_str(&authority, "draft_id")?)?;
                if current_draft["revision"].as_i64() != authority["revision"].as_i64() {
                    return Err(Error::rejected("social draft changed since approval"));
                }
                let resolver = self.social_media_resolver.clone().ok_or_else(|| {
                    Error::rejected("capability_unavailable: no media resolver configured")
                })?;
                let resolved = resolver
                    .resolve(
                        required_str(&authority, "toolkit")?,
                        required_str(&authority, "destination_id")?,
                    )
                    .into_result()
                    .map_err(|e| Error::rejected(e.to_string()))?;
                if resolved.aos_connection_id != authority["aos_connection_id"] {
                    return Err(Error::rejected("social destination changed since approval"));
                }
                // The owner's standing grant comes first: no grant, no media
                // import and no claim.
                let grant_id = owner_grant(sender.as_ref(), &authority)
                    .map_err(|refusal| Error::rejected(refusal.to_string()))?;
                let media_key = if let Some(asset) = authority["asset_id"].as_str() {
                    let (header, asset_digest, bytes) =
                        self.store.app_tool_asset_bytes(&install, asset)?;
                    if bytes.len() > 2 * 1024 * 1024
                        || Some(bare_digest(&asset_digest)) != frozen_image_digest(&authority)
                        || Some(bytes.len() as u64) != authority["size_bytes"].as_u64()
                    {
                        return Err(Error::rejected("social draft image changed since approval"));
                    }
                    let mime =
                        crate::platform::agenticos_external::image::image_mime(&bytes, &header)
                            .map_err(Error::rejected)?;
                    if Some(mime) != authority["mime"].as_str() {
                        return Err(Error::rejected(
                            "social draft image type changed since approval",
                        ));
                    }
                    image::load_from_memory(&bytes)
                        .map_err(|_| Error::rejected("social draft image cannot be decoded"))?;
                    let importer = self.social_media_importer.clone().ok_or_else(|| {
                        Error::rejected("capability_unavailable: no media importer configured")
                    })?;
                    let receipt = importer
                        .import(&resolved.aos_connection_id, mime, &bytes)
                        .map_err(|e| Error::rejected(e.to_string()))?;
                    if receipt.digest != bare_digest(&asset_digest)
                        || receipt.mime != mime
                        || receipt.size_bytes != bytes.len()
                        || receipt.connection_id != resolved.aos_connection_id
                    {
                        return Err(Error::rejected(
                            "media import receipt does not match approved image",
                        ));
                    }
                    Some(receipt.media_key)
                } else {
                    None
                };
                records.app_social_effect_media_key(&id_owned, media_key.as_deref())?;
                let key = required_str(&authority, "idempotency_key")?;
                let mut bound = authority.clone();
                bound["grant_id"] = json!(grant_id);
                bound["image_digest"] = json!(frozen_image_digest(&authority));
                let binding = super::social_publish_rpc::sender_binding(&bound, &json!(key))
                    .ok_or_else(|| Error::rejected("approved social publish binding is invalid"))?;
                match sender.preflight(&binding) {
                    Preflight::Approved => {}
                    Preflight::Refused(r) => return Err(Error::rejected(r.to_string())),
                    Preflight::Uncertain(r) => {
                        return Err(Error::rejected(format!("publish preflight uncertain: {r}")))
                    }
                }
                let Some(_claimed) =
                    records.app_social_effect_claim_send(&id_owned, &digest_owned)?
                else {
                    return Err(Error::rejected(
                        "social effect is no longer approved for Publish now",
                    ));
                };
                match sender.execute(&binding) {
                    Ok(outcome) if outcome.state == PublishState::Posted => records
                        .app_social_effect_finish(&id_owned, "posted", &outcome.evidence_json()),
                    Ok(outcome) if outcome.state == PublishState::Refused => records
                        .app_social_effect_finish(&id_owned, "refused", &outcome.evidence_json()),
                    Ok(_) => records.app_social_effect_show(&id_owned),
                    Err(refusal) => records
                        .app_social_effect_show(&id_owned)
                        .map(|mut receipt| {
                            receipt["send_error"] = json!(refusal.to_string());
                            receipt
                        }),
                }
            },
        )
    }

    pub(super) fn rpc_app_effect(
        self: &Arc<Self>,
        method: &str,
        params: &Value,
        pid: u32,
    ) -> Result<Value> {
        self.operator_connection("app artifact release", params, pid)?;
        strict_fields(
            params,
            match method {
                "app_effect_stage" => &[
                    "run_id",
                    "artifact_id",
                    "slot",
                    "request_id",
                    "title",
                    "proof",
                    "action_token",
                    "token",
                    "key",
                    "origin",
                    "tool_alias",
                ],
                "app_effect_show" => &["effect_id"],
                "app_effect_list" => &["install_id", "context_id"],
                "app_effect_decide" => &["effect_id", "digest", "decision"],
                "app_effect_publish_now" => &["effect_id", "digest"],
                "app_effect_resolve" => &["effect_id", "digest", "resolution"],
                _ => return Err(Error::rejected("unknown app effect method")),
            },
        )?;
        for key in ["install_id", "context_id"] {
            if params.get(key).is_some_and(|v| !v.is_string()) {
                return Err(Error::rejected(
                    "app effect filters must be strings when present",
                ));
            }
        }
        match method {
            "app_effect_show" => {
                let id = required_str(params, "effect_id")?;
                if let Some(install) = social_effect_install(id) {
                    crate::store::app_records::RecordStore::open(&self.state_dir, &install)?
                        .app_social_effect_show(id)
                } else {
                    self.store.app_effect_show(id)
                }
            }
            "app_effect_list" => self.store.app_effect_list(
                optional_str(params, "install_id"),
                optional_str(params, "context_id"),
            ),
            "app_effect_stage" => {
                if params.get("proof").is_some() {
                    self.stage_social_draft_effect(params, pid)
                } else {
                    self.stage_app_artifact(params)
                }
            }
            "app_effect_publish_now" => self.publish_social_draft_now(
                required_str(params, "effect_id")?,
                required_str(params, "digest")?,
            ),
            "app_effect_resolve" => {
                // Historical reconciliation deliberately needs neither a live
                // installation nor PM/custody locks. It never executes a send.
                let _release = self
                    .app_release_lock
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                let resolved = self.store.app_effect_resolve(
                    required_str(params, "effect_id")?,
                    required_str(params, "digest")?,
                    required_str(params, "resolution")?,
                )?;
                self.wake();
                Ok(resolved)
            }
            "app_effect_decide" => {
                let id = required_str(params, "effect_id")?;
                let digest = required_str(params, "digest")?;
                let accept = match required_str(params, "decision")? {
                    "accept" => true,
                    "decline" => false,
                    _ => {
                        return Err(Error::rejected(
                            "app effect decision must be accept or decline",
                        ))
                    }
                };
                if let Some(install) = social_effect_install(id) {
                    let records =
                        crate::store::app_records::RecordStore::open(&self.state_dir, &install)?;
                    let frozen = records.app_social_effect_show(id)?;
                    if frozen["effect"]["digest"] != digest
                        || frozen["effect"]["state"] != "waiting"
                    {
                        return Err(Error::rejected(
                            "social effect digest is stale or effect is no longer waiting",
                        ));
                    }
                    let authority = &frozen["effect"]["authority"];
                    let pm = self.pm_at(&self.pm_dir()?)?;
                    return workspace::with_completed_bundle_snapshot(
                        &pm,
                        &install,
                        required_str(authority, "bundle_digest")?,
                        |bundle, files| {
                            if accept {
                                let _release = self
                                    .app_release_lock
                                    .lock()
                                    .unwrap_or_else(|e| e.into_inner());
                                let context = required_str(authority, "context_id")?;
                                let slot = required_str(&authority["binding"], "slot")?;
                                let current_binding = self
                                    .app_binding_live(&install, Some(context), slot, bundle, files)?
                                    .ok_or_else(|| {
                                        Error::rejected("social publish binding is unavailable")
                                    })?;
                                if current_binding.config["mapping"]["effect"] != "send"
                                    || current_binding.digest != authority["binding"]["digest"]
                                    || current_binding.revision != authority["binding"]["revision"]
                                {
                                    return Err(Error::rejected(
                                        "social publish binding changed since staging",
                                    ));
                                }
                                let draft = required_str(authority, "draft_id")?;
                                let revision = authority["revision"].as_i64().unwrap_or(0);
                                let current = records.app_social_draft_show(context, draft)?;
                                if current["revision"].as_i64() != Some(revision) {
                                    return Err(Error::rejected(
                                        "social draft changed since effect staging",
                                    ));
                                }
                                let asset = authority["asset_id"].as_str();
                                if let Some(asset) = asset {
                                    let (_, digest, bytes) =
                                        self.store.app_tool_asset_bytes(&install, asset)?;
                                    if Some(bare_digest(&digest)) != frozen_image_digest(authority)
                                        || bytes.len()
                                            != authority["size_bytes"].as_u64().unwrap_or(0)
                                                as usize
                                    {
                                        return Err(Error::rejected(
                                            "social draft image changed since staging",
                                        ));
                                    }
                                }
                            }
                            records
                                .app_social_effect_decide(id, digest, accept)?
                                .ok_or_else(|| {
                                    Error::rejected("social effect has already been decided")
                                })
                        },
                    );
                }
                let frozen = self.store.app_effect_show(id)?;
                if frozen["effect"]["digest"] != digest || frozen["effect"]["state"] != "waiting" {
                    return Err(Error::rejected(
                        "app release digest is stale or effect is no longer waiting",
                    ));
                }
                let authority = &frozen["effect"]["authority"];
                let pm = self.pm_at(&self.pm_dir()?)?;
                let decided = workspace::with_completed_bundle_snapshot(
                    &pm,
                    required_str(authority, "install_id")?,
                    required_str(authority, "bundle_digest")?,
                    |bundle, files| {
                        let _custody = self
                            .platform_custody_lock
                            .lock()
                            .unwrap_or_else(|e| e.into_inner());
                        let _release = self
                            .app_release_lock
                            .lock()
                            .unwrap_or_else(|e| e.into_inner());
                        if accept {
                            self.app_release_current(authority, bundle, files)?;
                        }
                        self.store.effect_decide(required_str(&frozen["effect"],"request")?,accept,
                        &json!({"by":{"member":"operator","role":"operator","rule":"exact-app-artifact-release"},"at":crate::issue::time::iso(crate::issue::time::now_epoch())}))?
                        .ok_or_else(||Error::rejected("app effect has already been decided"))
                    },
                )?;
                // The durable-decision fixture runs before the release locks.
                // A callback may legitimately change/revoke current authority.
                if !accept
                    || self
                        .effect_execute_gate
                        .as_ref()
                        .is_some_and(|gate| !gate(&decided))
                {
                    return self.store.app_effect_show(id);
                }
                self.execute_app_artifact(id, digest)
            }
            _ => Err(Error::rejected("unknown app effect method")),
        }
    }

    pub(super) fn stage_app_artifact(&self, params: &Value) -> Result<Value> {
        let run_id = required_str(params, "run_id")?;
        let run = self.store.app_run_show(run_id)?;
        let install = required_str(&run, "install_id")?;
        let request_id = required_str(params, "request_id")?;
        crate::proto::identifier(request_id, "app release request id")?;
        let request = format!(
            "app-release-{}",
            uuid::Uuid::new_v5(
                &uuid::Uuid::NAMESPACE_OID,
                format!("{install}:{request_id}").as_bytes()
            )
            .simple()
        );
        let pm = self.pm_at(&self.pm_dir()?)?;
        workspace::with_completed_bundle_snapshot(
            &pm,
            install,
            required_str(&run["snapshot"], "bundle_digest")?,
            |bundle, files| {
                let _custody = self
                    .platform_custody_lock
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                let _release = self
                    .app_release_lock
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                let slot = required_str(params, "slot")?;
                let material = self.store.app_publication_material(
                    run_id,
                    required_str(params, "artifact_id")?,
                    required_str(bundle, "digest")?,
                    slot,
                )?;
                let proof: BindingProof = serde_json::from_value(material["binding"].clone())
                    .map_err(|_| Error::rejected("frozen publication binding is invalid"))?;
                self.app_binding_receipt_current(
                    install,
                    run["context_id"].as_str(),
                    slot,
                    &proof,
                    bundle,
                    files,
                )?;
                let effect_id = match self.store.effect_by_request(&request)? {
                    Some(row) => row.effect_id,
                    None => format!("effect-{}", uuid::Uuid::new_v4().simple()),
                };
                let artifact = &material["artifact"];
                let mut authority = json!({"schema":1,"install_id":install,"context":run["snapshot"]["context"],
                "run_id":run_id,"run_snapshot_digest":run["snapshot_digest"],"epoch":run["epoch"],
                "bundle_digest":bundle["digest"],"artifact_id":artifact["id"],"artifact_digest":artifact["digest"],
                "slot":slot,"binding":proof,"material_digest":app_runs::material_digest(&material),
                "producer_receipt_digest":app_runs::material_digest(&material["producer_receipt"]),
                "review_receipt_digest":app_runs::material_digest(&material["review_receipt"])});
                if let Some(asset) = material.get("asset") {
                    authority["asset"] = asset.clone();
                }
                let core_digest = app_effects::authority_digest(&authority);
                let mut provenance = json!({"schema":1,"authorization_kind":"app_artifact","effect_id":effect_id,"authority_digest":core_digest,
                "install_id":install,"context_id":run["context_id"],"context_revision":run["snapshot"]["context"]["revision"],
                "context_digest":run["snapshot"]["context"]["digest"],"run_id":run_id,"run_snapshot_digest":run["snapshot_digest"],
                "artifact_id":artifact["id"],"artifact_digest":artifact["digest"],"binding_id":proof.id,
                "binding_revision":proof.revision,"binding_digest":proof.digest,
                "connection_id":proof.config["connection_id"],"connection_kind":proof.config["connection_kind"],
                "connection_revision":proof.config["connection_revision"],"registration_digest":proof.config["registration_digest"],
                "sink_registration":proof.config["sink_registration"],"mapping":proof.config["mapping"],
                "review_receipt_digest":authority["review_receipt_digest"]});
                if let Some(asset) = material.get("asset") {
                    provenance["asset"] = asset.clone();
                }
                authority["provenance"] = provenance;
                let provider = required_str(&proof.config, "provider")?;
                let account = required_str(&proof.config, "account")?;
                let tool = required_str(&proof.config["mapping"], "tool")?;
                let adapter = self
                    .platforms
                    .get(provider)
                    .ok_or_else(|| Error::rejected("publication adapter unavailable"))?;
                let legacy_smtp = self.legacy_smtp_publication(provider, account)?;
                let input = adapter
                    .prepare_app_artifact(
                        required_str(params, "title")?,
                        required_str(artifact, "text")?,
                        &authority["provenance"],
                        material.get("asset"),
                    )
                    .map_err(|error| {
                        if legacy_smtp {
                            Error::rejected("app release preparation failed — withheld")
                        } else {
                            Error::rejected(error)
                        }
                    })?;
                let preview = adapter.preview(account, tool, &input);
                if serde_json::to_vec(&input)?.len() > 64 * 1024 || preview.len() > 16 * 1024 {
                    return Err(Error::rejected(
                        "publication input or complete preview exceeds its byte bound",
                    ));
                }
                let bytes = crate::platform::load_credential(
                    &self.store,
                    &self.platform_custody,
                    provider,
                    account,
                )?;
                let envelope = publication_smtp_envelope(legacy_smtp, &bytes)?;
                let secret = envelope.as_ref().map_or(
                    bytes.as_slice(),
                    crate::platform::smtp::SmtpEnvelope::secret,
                );
                publication_refuse_leak(
                    legacy_smtp,
                    "app release input",
                    &input.to_string(),
                    secret,
                )?;
                publication_refuse_leak(legacy_smtp, "app release preview", &preview, secret)?;
                let input_summary = required_str(params, "title")?;
                if legacy_smtp {
                    publication_refuse_leak(
                        legacy_smtp,
                        "app release input",
                        input_summary,
                        secret,
                    )?;
                    publication_refuse_leak(
                        legacy_smtp,
                        "app release input",
                        &serde_json::to_string(input_summary)?,
                        secret,
                    )?;
                }
                let row = EffectRow {
                    effect_id,
                    request,
                    agent: required_str(&run["snapshot"], "owner_pm")?.into(),
                    platform: provider.into(),
                    account: account.into(),
                    tool: tool.into(),
                    label: None,
                    input,
                    input_summary: input_summary.into(),
                    preview,
                    source_name: None,
                    source_hash: Some(required_str(artifact, "digest")?.into()),
                    scopes: serde_json::from_value(proof.config["mapping"]["scopes"].clone())?,
                    task: None,
                    state: "waiting".into(),
                    close_reason: None,
                    decision: None,
                    outcome: None,
                    needs_you: false,
                    staged_at: 0.0,
                    updated_at: 0.0,
                };
                self.store.app_effect_stage(&row, &authority)
            },
        )
    }

    fn app_release_current(
        &self,
        authority: &Value,
        bundle: &Value,
        files: &std::collections::BTreeMap<String, String>,
    ) -> Result<()> {
        let install = required_str(authority, "install_id")?;
        let proof: BindingProof = serde_json::from_value(authority["binding"].clone())
            .map_err(|_| Error::rejected("app effect binding receipt is invalid"))?;
        let slot = required_str(authority, "slot")?;
        self.app_binding_receipt_current(
            install,
            authority["context"]["id"].as_str(),
            slot,
            &proof,
            bundle,
            files,
        )?;
        let material = self.store.app_publication_material(
            required_str(authority, "run_id")?,
            required_str(authority, "artifact_id")?,
            required_str(bundle, "digest")?,
            slot,
        )?;
        if app_runs::material_digest(&material) != authority["material_digest"] {
            return Err(Error::rejected(
                "accepted publication material receipt changed",
            ));
        }
        Ok(())
    }

    fn legacy_smtp_publication(&self, provider: &str, account: &str) -> Result<bool> {
        if provider != crate::platform::smtp::PLATFORM
            || crate::platform::is_builtin(provider, account)
        {
            return Ok(false);
        }
        Ok(self
            .store
            .platform_credential(provider, account)?
            .is_some_and(|record| {
                record.platform == crate::platform::smtp::PLATFORM
                    && record.exchange == crate::platform::smtp::ENROLLMENT_SHAPE
                    && matches!(
                        record.custody.as_str(),
                        crate::platform::custody::FILE_TAG
                            | crate::platform::custody::LIBSECRET_TAG
                    )
            }))
    }

    fn execute_app_artifact(&self, id: &str, digest: &str) -> Result<Value> {
        let frozen = self.store.app_effect_show(id)?;
        let authority = &frozen["effect"]["authority"];
        let pm = self.pm_at(&self.pm_dir()?)?;
        workspace::with_completed_bundle_snapshot(
            &pm,
            required_str(authority, "install_id")?,
            required_str(authority, "bundle_digest")?,
            |bundle, files| {
                let _custody = self
                    .platform_custody_lock
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                let _release = self
                    .app_release_lock
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                self.app_release_current(authority, bundle, files)?;
                let config = &authority["binding"]["config"];
                let provider = required_str(config, "provider")?;
                let account = required_str(config, "account")?;
                let legacy_smtp = self.legacy_smtp_publication(provider, account)?;
                let bytes = crate::platform::load_credential(
                    &self.store,
                    &self.platform_custody,
                    provider,
                    account,
                )?;
                let envelope = publication_smtp_envelope(legacy_smtp, &bytes)?;
                let secret = envelope.as_ref().map_or(
                    bytes.as_slice(),
                    crate::platform::smtp::SmtpEnvelope::secret,
                );
                let row = self
                    .store
                    .app_effect_claim(id, digest, |conn, current| {
                        let material = crate::store::Store::app_publication_material_in(
                            conn,
                            required_str(current, "run_id")?,
                            required_str(current, "artifact_id")?,
                            required_str(bundle, "digest")?,
                            required_str(current, "slot")?,
                        )?;
                        Ok(app_runs::material_digest(&material) == current["material_digest"])
                    })?
                    .ok_or_else(|| {
                        Error::rejected("app effect execution claim was not acquired")
                    })?;
                // No SQLite guard survives the checked claim. The trusted barrier
                // exposes precisely the Local commit/revoke ordering in tests.
                if self
                    .app_release_claim_gate
                    .as_ref()
                    .is_some_and(|gate| !gate(&row))
                {
                    return self.store.app_effect_show(id);
                }
                let adapter = self
                    .platforms
                    .get(provider)
                    .ok_or_else(|| Error::rejected("publication adapter unavailable"))?;
                let result = adapter.execute_app_artifact(
                    &bytes,
                    &row.tool,
                    &row.input,
                    id,
                    row.source_hash.as_deref(),
                );
                let verified = adapter.read_back(&row.tool, &row.input);
                let (ok, uncertain, outcome) = match result {
                    Ok(value) => {
                        let outcome = json!({"kind":"released","result":value,"verified":verified});
                        if publication_refuse_leak(
                            legacy_smtp,
                            "app release outcome",
                            &outcome["result"].to_string(),
                            secret,
                        )
                        .is_err()
                        {
                            let verified = if legacy_smtp {
                                crate::contract_fixture::Verified::Unknown
                            } else {
                                verified
                            };
                            (
                                false,
                                true,
                                json!({"kind":"uncertain","error":"provider outcome withheld","verified":verified}),
                            )
                        } else {
                            (true, false, outcome)
                        }
                    }
                    Err(error) if legacy_smtp => {
                        let (uncertain, error) = match error {
                            crate::platform::AppArtifactError::Refused(error) => (false, error),
                            crate::platform::AppArtifactError::Uncertain(error) => (true, error),
                        };
                        let outcome = json!({"kind":if uncertain { "uncertain" } else { "refused" },"error":error,"verified":verified});
                        if publication_refuse_leak(
                            legacy_smtp,
                            "app release error",
                            outcome["error"].as_str().unwrap(),
                            secret,
                        )
                        .is_err()
                            || publication_refuse_leak(
                                legacy_smtp,
                                "app release error",
                                &outcome["error"].to_string(),
                                secret,
                            )
                            .is_err()
                        {
                            (
                                false,
                                true,
                                json!({"kind":"uncertain","error":"provider error withheld","verified":crate::contract_fixture::Verified::Unknown}),
                            )
                        } else {
                            (false, uncertain, outcome)
                        }
                    }
                    Err(error) => {
                        let (uncertain, error) = match error {
                            crate::platform::AppArtifactError::Refused(error) => (false, error),
                            crate::platform::AppArtifactError::Uncertain(error) => (true, error),
                        };
                        let error =
                            if crate::platform::refuse_leak("app release error", &error, &bytes)
                                .is_err()
                            {
                                "provider error withheld".to_string()
                            } else {
                                error
                            };
                        (
                            false,
                            uncertain,
                            json!({"kind":if uncertain { "uncertain" } else { "refused" },"error":error,"verified":verified}),
                        )
                    }
                };
                if uncertain {
                    let effect = self.store.app_effect_uncertain(id, &outcome)?;
                    self.wake();
                    return Ok(effect);
                }
                self.store.effect_outcome(
                    id,
                    ok,
                    &outcome,
                    if ok {
                        "app artifact released"
                    } else {
                        "app artifact release failed"
                    },
                )?;
                self.wake();
                self.store.app_effect_show(id)
            },
        )
    }
}

// ACCEPTANCE-CHECK SLOT (CAD-1291). Reserved for the independent acceptance
// check, written by the reviewer or the ticket author, not the implementer,
// in a new `tests/cad1291_acceptance.rs` (feature `test-seam`). It must prove
// against the real guards that these are refused: an agent or container
// caller on `app_effect_publish_now`, over RPC and HTTP; an agent or
// container minting a grant (AgenticOS side); a non-owner member and a standing grant sent to another destination
// or company, or past its cap or revoked (AgenticOS side); and an unapproved
// or changed draft, with no provider call.

#[cfg(test)]
mod grant_tests {
    use super::*;
    use crate::platform::agenticos_external::publish::{
        FoundGrant, LedgerOutcome, PublishSender, Refusal, SendBinding,
    };

    /// A door that lists fixed grants and never sends.
    struct Door(Vec<FoundGrant>);
    impl PublishSender for Door {
        fn execute(&self, _: &SendBinding) -> std::result::Result<LedgerOutcome, Refusal> {
            unreachable!("a grant lookup never sends")
        }
        fn status(&self, _: &str) -> std::result::Result<LedgerOutcome, Refusal> {
            unreachable!("a grant lookup never reconciles")
        }
        fn find_grant(&self, destination: &str) -> std::result::Result<Vec<FoundGrant>, Refusal> {
            Ok(self
                .0
                .iter()
                .filter(|g| g.destination_id == destination)
                .cloned()
                .collect())
        }
    }

    fn authority(destination: &str) -> Value {
        json!({"aos_connection_id":"con_ig","destination_id":destination,"toolkit":"instagram"})
    }

    fn standing() -> FoundGrant {
        FoundGrant {
            id: "dpq_standing_001".into(),
            connection_id: "con_ig".into(),
            destination_id: "1784".into(),
            toolkit: "instagram".into(),
            remaining_today: 5,
            revoked: false,
        }
    }

    #[test]
    fn publish_needs_the_owners_standing_grant_for_the_approved_account() {
        let door = Door(vec![standing()]);
        assert_eq!(
            owner_grant(&door, &authority("1784")).unwrap(),
            "dpq_standing_001"
        );
        assert_eq!(
            owner_grant(&Door(vec![]), &authority("1784"))
                .unwrap_err()
                .code,
            "grant_required"
        );
        // The approved destination differs from the granted one.
        assert_eq!(
            owner_grant(&door, &authority("9999")).unwrap_err().code,
            "grant_required"
        );
        let mut capped = standing();
        capped.remaining_today = 0;
        assert_eq!(
            owner_grant(&Door(vec![capped]), &authority("1784"))
                .unwrap_err()
                .code,
            "grant_cap_reached"
        );
    }

    #[test]
    fn a_draft_effect_never_freezes_a_standing_grant() {
        // The authority built at staging carries no grant id, so a grant
        // configured on the binding (or forged into a request) can never be
        // the one presented; only the owner's per-post grant is.
        let source = include_str!("app_effects_rpc.rs");
        let stage = source
            .split("fn stage_social_draft_effect")
            .nth(1)
            .and_then(|rest| rest.split("fn publish_social_draft_now").next())
            .expect("stage function present");
        assert!(!stage.contains("\"grant_id\""), "staging froze a grant id");
    }
}
