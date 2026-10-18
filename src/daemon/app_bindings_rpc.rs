//! Provider-neutral publication configuration. Configuration is not authority.
use super::social_publish_start::carry_publish;
use super::*;
use crate::issue::{app, app_catalog::workspace};
use crate::store::app_bindings::{binding_drift, BindingDrift, BindingProof};
use std::collections::BTreeMap;

impl Shared {
    pub(super) fn rpc_app_binding(
        self: &Arc<Self>,
        method: &str,
        params: &Value,
        pid: u32,
    ) -> Result<Value> {
        self.operator_connection("app binding management", params, pid)?;
        let allowed: &[&str] = match method {
            "app_binding_create" => &[
                "install_id",
                "context_id",
                "slot",
                "connection_id",
                "request_id",
            ],
            "app_binding_update" => &[
                "install_id",
                "binding_id",
                "expected_revision",
                "connection_id",
            ],
            "app_binding_revoke" => &["install_id", "binding_id", "expected_revision"],
            // CAD-1123 HP4: the operator records the publish destination once.
            "app_binding_publish_set" => &[
                "install_id",
                "binding_id",
                "expected_revision",
                "destination_id",
                "destination_label",
                "toolkit",
                "timezone",
                "grant_id",
            ],
            "app_binding_show" => &["install_id", "binding_id"],
            "app_binding_list" => &["install_id", "context_id"],
            _ => return Err(Error::rejected("unknown app binding method")),
        };
        strict_fields(params, allowed)?;
        let install = required_str(params, "install_id")?;
        crate::proto::identifier(install, "installation id")?;
        let context = match params.get("context_id") {
            None => None,
            Some(Value::String(id)) => Some(id.as_str()),
            Some(_) => {
                return Err(Error::rejected(
                    "context_id must be a non-null string when present",
                ))
            }
        };
        match method {
            "app_binding_publish_set" => {
                let pm = self.pm_at(&self.pm_dir()?)?;
                workspace::with_runtime_snapshot(&pm, install, |_, _| {
                    let _custody = self
                        .platform_custody_lock
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    let _release = self
                        .app_release_lock
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    self.set_binding_publish(install, params)
                })
            }
            "app_binding_show" => self
                .store
                .app_binding_show(install, required_str(params, "binding_id")?),
            "app_binding_list" => {
                let pm = self.pm_at(&self.pm_dir()?)?;
                workspace::with_runtime_read(&pm, install, |bundle, files| {
                    let digest = required_str(bundle, "digest")?;
                    let mut listed =
                        self.store
                            .app_binding_list_preferred(install, context, Some(digest))?;
                    let _custody = self
                        .platform_custody_lock
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    let _release = self
                        .app_release_lock
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    for row in listed["bindings"].as_array_mut().into_iter().flatten() {
                        if row["state"] == "configured" && row["config"]["bundle_digest"] == digest
                        {
                            row["drift"] =
                                self.app_binding_drift_status(install, row, bundle, files);
                        }
                    }
                    Ok(listed)
                })
            }
            "app_binding_revoke" => {
                let pm = self.pm_at(&self.pm_dir()?)?;
                workspace::with_runtime_snapshot(&pm, install, |_, _| {
                    let _custody = self
                        .platform_custody_lock
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    let _release = self
                        .app_release_lock
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    self.store.app_binding_revoke(
                        install,
                        required_str(params, "binding_id")?,
                        binding_revision(params)?,
                    )
                })
            }
            "app_binding_create" | "app_binding_update" => {
                let pm = self.pm_at(&self.pm_dir()?)?;
                workspace::with_runtime_snapshot(&pm, install, |bundle, files| {
                    let _custody = self
                        .platform_custody_lock
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    let _release = self
                        .app_release_lock
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    let existing = if method == "app_binding_update" {
                        Some(
                            self.store
                                .app_binding_show(install, required_str(params, "binding_id")?)?
                                ["binding"]
                                .clone(),
                        )
                    } else {
                        None
                    };
                    let context = existing
                        .as_ref()
                        .and_then(|b| b["context_id"].as_str())
                        .or(context);
                    let slot = existing
                        .as_ref()
                        .and_then(|b| b["slot"].as_str())
                        .unwrap_or(required_str(
                            params,
                            if method == "app_binding_create" {
                                "slot"
                            } else {
                                "binding_id"
                            },
                        )?);
                    let mut config = self.app_binding_config(
                        install,
                        context,
                        slot,
                        required_str(params, "connection_id")?,
                        bundle,
                        files,
                    )?;
                    if let Some(old) = &existing {
                        config = carry_publish(config, &old["config"]);
                    }
                    if method == "app_binding_create" {
                        self.store.app_binding_create(
                            install,
                            context,
                            slot,
                            &config,
                            required_str(params, "request_id")?,
                        )
                    } else {
                        self.store.app_binding_update(
                            install,
                            required_str(params, "binding_id")?,
                            binding_revision(params)?,
                            &config,
                        )
                    }
                })
            }
            _ => Err(Error::rejected("unknown app binding method")),
        }
    }

    /// Called under PM -> custody -> release. Resolve only reviewed provider
    /// metadata and the exact ConnectionId; never use defaults or aliases.
    pub(super) fn app_binding_config(
        &self,
        install: &str,
        context: Option<&str>,
        slot: &str,
        connection: &str,
        bundle: &Value,
        files: &BTreeMap<String, String>,
    ) -> Result<Value> {
        let manifest = app::parse_manifest(
            files
                .get("app.md")
                .ok_or_else(|| Error::rejected("installation manifest unavailable"))?,
        )?;
        let declaration = manifest
            .capabilities
            .get(slot)
            .ok_or_else(|| Error::rejected("publication slot is not declared by this app"))?;
        declaration.validate()?;
        let context = match context {
            Some(id) => {
                let row = self.store.app_context_show(install, id)?;
                let row = &row["context"];
                if row["state"] != "active" {
                    return Err(Error::rejected("binding context is archived"));
                }
                json!({"id":id,"install_id":install,"revision":row["revision"],"digest":row["digest"]})
            }
            None => Value::Null,
        };
        let connection = self
            .connection_list_locked()?
            .into_iter()
            .find(|row| row["id"] == connection)
            .ok_or_else(|| Error::rejected("connection is unavailable or stale"))?;
        if connection["status"]["manifest_status"] != "matched"
            || connection["status"]["custody_available"] != true
            || !connection["registration_digest"].is_string()
        {
            return Err(Error::rejected(
                "connection reviewed registration or custody is unavailable",
            ));
        }
        let provider = connection["provider"]
            .as_str()
            .ok_or_else(|| Error::rejected("connection provider unavailable"))?;
        let descriptor = self.connection_descriptor(provider)?;
        let mapping = descriptor.resolve_action(
            &declaration.capability,
            declaration.version,
            &declaration.action,
            &declaration.resource_kind,
        )?;
        if mapping.effect != declaration.effect {
            return Err(Error::rejected(
                "provider action effect differs from app contract",
            ));
        }
        if connection["kind"] == "enrolled" {
            let scopes = connection["scopes"]
                .as_array()
                .ok_or_else(|| Error::rejected("connection scopes unavailable"))?;
            if mapping
                .scopes
                .iter()
                .any(|scope| !scopes.iter().any(|v| v.as_str() == Some(scope)))
            {
                return Err(Error::rejected(
                    "connection does not cover reviewed action scopes",
                ));
            }
        }
        let adapter = self
            .platforms
            .get(provider)
            .ok_or_else(|| Error::rejected("connection adapter unavailable"))?;
        let sink = adapter
            .connection_registration()
            .ok_or_else(|| Error::rejected("connection sink registration unavailable"))?;
        let mut config = json!({"schema":1,"install_id":install,"context":context,"bundle_digest":bundle["digest"],
            "workspace_id":self.store.connection_workspace_id()?,"connection_id":connection["id"],
            "provider":provider,"account":connection["account"],"connection_kind":connection["kind"],
            "connection_revision":connection["revision"],"registration_digest":connection["registration_digest"],
            "sink_registration":sink,"descriptor_revision":descriptor.revision,
            "reviewed_pin":connection["status"]["reviewed_pin"],"reported_pin":connection["status"]["reported_pin"],
            "mapping":mapping,"declaration":declaration});
        if provider == crate::platform::account_media::PLATFORM {
            let source = crate::platform::account_media::identities()?
                .into_iter()
                .find(|identity| {
                    connection["account"]
                        == crate::platform::account_media::account_handle(identity)
                })
                .ok_or_else(|| Error::rejected("saved account source is missing or replaced"))?;
            config["account_source"] =
                serde_json::to_value(crate::platform::account_media::AccountSource::from(&source))?;
        }
        Ok(config)
    }

    /// A genuinely never-bound image slot may use the one account owned by
    /// the proven native actor. Any explicit row or tombstone is authoritative.
    pub(super) fn app_binding_auto_image(
        &self,
        install: &str,
        context: Option<&str>,
        slot: &str,
        actor: &Value,
        bundle: &Value,
        files: &BTreeMap<String, String>,
    ) -> Result<Option<BindingProof>> {
        if let Some(proof) = self.app_binding_live(install, context, slot, bundle, files)? {
            self.app_account_binding_owned(actor, &proof)?;
            return Ok(Some(proof));
        }
        if self
            .store
            .app_binding_slot_recorded(install, context, slot)?
        {
            return Ok(None);
        }
        let manifest = app::parse_manifest(
            files
                .get("app.md")
                .ok_or_else(|| Error::rejected("installation manifest unavailable"))?,
        )?;
        let declaration = manifest
            .capabilities
            .get(slot)
            .ok_or_else(|| Error::rejected("capability slot is not declared by this app"))?;
        declaration.validate()?;
        if declaration.capability != "media.generate"
            || declaration.version != 1
            || declaration.action != "generate_image"
            || declaration.effect != "draft"
            || declaration.resource_kind != "connection_account"
        {
            return Ok(None);
        }
        let mut eligible = crate::platform::account_media::identities()?
            .into_iter()
            .filter(|source| {
                Self::account_image_owned(
                    actor,
                    &crate::platform::account_media::AccountSource::from(source),
                )
            });
        let source = match (eligible.next(), eligible.next()) {
            (Some(source), None) => source,
            (None, _) => {
                return Err(Error::rejected(
                    "no signed-in image account belongs to the proven native actor",
                ))
            }
            _ => {
                return Err(Error::rejected(
                    "choose the image account from the signed-in accounts",
                ))
            }
        };
        self.account_image_actor_live(actor)?;
        let account = crate::platform::account_media::account_handle(&source);
        let connection =
            self.builtin_connection_id(crate::platform::account_media::PLATFORM, &account)?;
        let config = self.app_binding_config(install, context, slot, &connection, bundle, files)?;
        if config["account_source"]
            != serde_json::to_value(crate::platform::account_media::AccountSource::from(&source))?
        {
            return Err(Error::rejected(
                "saved image account changed during binding",
            ));
        }
        let request = format!("account-image-{}", uuid::Uuid::new_v4().simple());
        self.store
            .app_binding_create(install, context, slot, &config, &request)?;
        self.store
            .app_binding_for_slot(install, context, slot, required_str(bundle, "digest")?)
    }

    /// CAD-1119: the binding a new run or quote uses for one slot. A
    /// receipt that moved only in provider bookkeeping (descriptor or pin
    /// revision, registration receipt, the provider's tool name for the
    /// same reviewed action) migrates here without the operator, keeping
    /// the installation's approval. Any change to the slot contract or the
    /// connection refuses until the operator binds the slot again. Called
    /// under PM -> custody -> release, from operator-gated calls only.
    pub(super) fn app_binding_live(
        &self,
        install: &str,
        context: Option<&str>,
        slot: &str,
        bundle: &Value,
        files: &BTreeMap<String, String>,
    ) -> Result<Option<BindingProof>> {
        let Some(proof) = self.store.app_binding_for_slot(
            install,
            context,
            slot,
            required_str(bundle, "digest")?,
        )?
        else {
            return Ok(None);
        };
        let connection = proof.config["connection_id"]
            .as_str()
            .ok_or_else(|| Error::rejected("binding connection receipt is missing"))?;
        let fresh = carry_publish(
            self.app_binding_config(install, context, slot, connection, bundle, files)?,
            &proof.config,
        );
        match binding_drift(&proof.config, &fresh) {
            BindingDrift::Same => Ok(Some(proof)),
            BindingDrift::Compatible(_) => self
                .store
                .app_binding_migrate(install, &proof, &fresh)
                .map(Some),
            BindingDrift::NeedsConfirm(changes) => {
                let fields: Vec<&str> = changes
                    .iter()
                    .filter_map(|change| change["field"].as_str())
                    .collect();
                Err(Error::rejected(format!(
                    "the {slot} connection changed beyond the bound contract ({}); \
                     the operator must confirm it by binding the slot again",
                    fields.join(", ")
                )))
            }
        }
    }

    /// The operator's view of one configured binding's receipt against the
    /// connection now: `current`, `migrates` (silently, on next use),
    /// `needs_confirm` with the field diff, or `unavailable` with why.
    fn app_binding_drift_status(
        &self,
        install: &str,
        row: &Value,
        bundle: &Value,
        files: &BTreeMap<String, String>,
    ) -> Value {
        let fresh = row["config"]["connection_id"]
            .as_str()
            .ok_or_else(|| Error::rejected("binding connection receipt is missing"))
            .and_then(|connection| {
                self.app_binding_config(
                    install,
                    row["context_id"].as_str(),
                    row["slot"].as_str().unwrap_or_default(),
                    connection,
                    bundle,
                    files,
                )
                .map(|fresh| carry_publish(fresh, &row["config"]))
            });
        match fresh.map(|fresh| binding_drift(&row["config"], &fresh)) {
            Ok(BindingDrift::Same) => json!({"state": "current", "changes": []}),
            Ok(BindingDrift::Compatible(changes)) => {
                json!({"state": "migrates", "changes": changes})
            }
            Ok(BindingDrift::NeedsConfirm(changes)) => {
                json!({"state": "needs_confirm", "changes": changes})
            }
            Err(error) => json!({"state": "unavailable", "reason": error.to_string()}),
        }
    }

    pub(super) fn app_binding_receipt_current(
        &self,
        install: &str,
        context: Option<&str>,
        slot: &str,
        proof: &BindingProof,
        bundle: &Value,
        files: &BTreeMap<String, String>,
    ) -> Result<()> {
        let current = self.configured_slot_proof(install, context, slot, bundle)?;
        if binding_proof_exact(&current, proof) {
            return self.binding_receipt_live(install, context, slot, &proof.config, bundle, files);
        }
        Err(Error::rejected(
            "publication binding revision or incarnation changed",
        ))
    }

    /// CAD-1508: library refine may use a retained source whose producing
    /// binding was carried onto the installed bundle by approved upgrades.
    /// A proof that already matches the live row takes the same path as
    /// [`Self::app_binding_receipt_current`]. Anything else still refuses.
    pub(super) fn app_binding_receipt_library_source(
        &self,
        install: &str,
        context: Option<&str>,
        slot: &str,
        proof: &BindingProof,
        bundle: &Value,
        files: &BTreeMap<String, String>,
    ) -> Result<()> {
        let current = self.configured_slot_proof(install, context, slot, bundle)?;
        if binding_proof_exact(&current, proof) {
            return self.binding_receipt_live(install, context, slot, &proof.config, bundle, files);
        }
        if self
            .store
            .app_binding_upgrade_carried(install, context, slot, proof, &current)?
        {
            return self.binding_receipt_live(
                install,
                context,
                slot,
                &current.config,
                bundle,
                files,
            );
        }
        Err(Error::rejected(incarnation_refusal(proof, &current)))
    }

    fn configured_slot_proof(
        &self,
        install: &str,
        context: Option<&str>,
        slot: &str,
        bundle: &Value,
    ) -> Result<BindingProof> {
        self.store
            .app_binding_for_slot(install, context, slot, required_str(bundle, "digest")?)?
            .ok_or_else(|| Error::rejected("publication binding is absent or revoked"))
    }

    fn binding_receipt_live(
        &self,
        install: &str,
        context: Option<&str>,
        slot: &str,
        config: &Value,
        bundle: &Value,
        files: &BTreeMap<String, String>,
    ) -> Result<()> {
        let configured = carry_publish(
            self.app_binding_config(
                install,
                context,
                slot,
                config["connection_id"]
                    .as_str()
                    .ok_or_else(|| Error::rejected("binding connection receipt is missing"))?,
                bundle,
                files,
            )?,
            config,
        );
        if configured != *config {
            return Err(Error::rejected(
                "publication binding installation/context/connection receipt is stale",
            ));
        }
        Ok(())
    }
}

fn shown_binding_id(id: &str) -> &str {
    if crate::proto::identifier(id, "binding id").is_ok() {
        id
    } else {
        "unrecognized"
    }
}

fn shown_digest(digest: &str) -> &str {
    let Some(hex) = digest.strip_prefix("sha256:") else {
        return "unrecognized";
    };
    if hex.len() == 64
        && hex
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        digest
    } else {
        "unrecognized"
    }
}

fn binding_proof_exact(current: &BindingProof, proof: &BindingProof) -> bool {
    current.id == proof.id
        && current.revision == proof.revision
        && current.digest == proof.digest
        && current.config == proof.config
}

fn incarnation_refusal(proof: &BindingProof, current: &BindingProof) -> String {
    format!(
        "publication binding incarnation changed: retained proof is binding {} revision {} digest {}; live binding is {} revision {} digest {}. Regenerate the source image on the current build",
        shown_binding_id(&proof.id),
        proof.revision,
        shown_digest(&proof.digest),
        shown_binding_id(&current.id),
        current.revision,
        shown_digest(&current.digest),
    )
}

pub(super) fn strict_fields(params: &Value, allowed: &[&str]) -> Result<()> {
    let object = params
        .as_object()
        .ok_or_else(|| Error::rejected("app release parameters must be an object"))?;
    if object.keys().any(|k| !allowed.contains(&k.as_str())) {
        return Err(Error::rejected(
            "app release payload has unsupported fields",
        ));
    }
    Ok(())
}
fn binding_revision(params: &Value) -> Result<i64> {
    params
        .get("expected_revision")
        .and_then(Value::as_i64)
        .filter(|n| *n > 0)
        .ok_or_else(|| Error::rejected("expected binding revision must be a positive integer"))
}
