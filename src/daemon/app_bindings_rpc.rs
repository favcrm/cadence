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
                "aos_connection_id",
            ],
            // CAD-1143: the operator's read of the destinations the bound
            // connection's workspace authorizes — discovery only, never
            // send/workspace authority. Params are the scope ids alone.
            "app_publish_destinations_list" => &["install_id", "context_id"],
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
            "app_publish_destinations_list" => {
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
                    self.publish_destinations_list(install, context, bundle, files)
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
        Ok(
            json!({"schema":1,"install_id":install,"context":context,"bundle_digest":bundle["digest"],
            "workspace_id":self.store.connection_workspace_id()?,"connection_id":connection["id"],
            "provider":provider,"account":connection["account"],"connection_kind":connection["kind"],
            "connection_revision":connection["revision"],"registration_digest":connection["registration_digest"],
            "sink_registration":sink,"descriptor_revision":descriptor.revision,
            "reviewed_pin":connection["status"]["reviewed_pin"],"reported_pin":connection["status"]["reported_pin"],
            "mapping":mapping,"declaration":declaration}),
        )
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

    /// CAD-1143: `app_publish_destinations_list` — the operator's read-only
    /// discovery under the configured runtime READ credential, scoped to one
    /// configured publication binding. Params are the scope ids alone
    /// (`install_id`, optional `context_id`); the operator proof ran before
    /// this call. Validates the install through the runtime snapshot, the
    /// (optional) context as belonging-to and active on this installation, the
    /// manifest's single valid `text.publish` publication slot, and an existing
    /// configured binding whose receipt is still current (revoked/drifted/stale
    /// refuses — never migrated or written here). Only then does the attached
    /// read credential issue one bounded destinations GET; a full window or any
    /// malformed row fails closed, and a missing resolver or upstream fault
    /// maps to `capability_unavailable`. The rows are what the READ credential's
    /// workspace happens to authorize — that scoping is NOT evidence the read,
    /// send or binding share an AOS workspace, so the rows are never send or
    /// workspace authority; they grant no per-binding account and no
    /// configuration or send right.
    fn publish_destinations_list(
        &self,
        install: &str,
        context: Option<&str>,
        bundle: &Value,
        files: &BTreeMap<String, String>,
    ) -> Result<Value> {
        // An explicit context must belong to this installation and be active.
        if let Some(ctx) = context {
            self.store.app_context_proof(install, ctx)?;
        }
        // The manifest's publication slot: the exact unique valid
        // `text.publish` declaration — schema 1, version 1, action `publish`,
        // `connection_account`, effect `send`. Zero or several matches (or an
        // unsupported contract) is refused before any read; we never pick an
        // arbitrary first declaration.
        let manifest = app::parse_manifest(
            files
                .get("app.md")
                .ok_or_else(|| Error::rejected("installation manifest unavailable"))?,
        )?;
        let mut slots = manifest
            .capabilities
            .iter()
            .filter(|(_, declaration)| declaration.capability == "text.publish")
            .map(|(slot, declaration)| (slot.clone(), declaration));
        let (slot, declaration) = match (slots.next(), slots.next()) {
            (Some(one), None) => one,
            _ => {
                return Err(Error::rejected(
                    "this app must declare exactly one publication slot",
                ))
            }
        };
        if declaration.action != "publish"
            || declaration.effect != "send"
            || declaration.version != 1
            || declaration.resource_kind != "connection_account"
        {
            return Err(Error::rejected(
                "publication slot is not the send/publish connection-account contract",
            ));
        }
        declaration.validate()?;
        // Require the existing configured publication binding for this exact
        // scope on the current bundle digest, and re-prove its receipt is still
        // current. Read-only: a drifted/revoked/stale receipt refuses here and
        // is never migrated or written by this call.
        let proof = self
            .store
            .app_binding_for_slot(install, context, &slot, required_str(bundle, "digest")?)?
            .ok_or_else(|| {
                Error::rejected("publication binding is absent or revoked for this scope")
            })?;
        self.app_binding_receipt_current(install, context, &slot, &proof, bundle, files)?;
        // The attached read credential (provider.read/draft) — never the send
        // credential. Absent or a failed/ambiguous/complete-window read maps
        // to `capability_unavailable`; a clean list returns every validated row
        // (including non-publishable), never survivor-filtered.
        let resolver = self.social_media_resolver.clone().ok_or_else(|| {
            Error::rejected("capability_unavailable: no media resolver configured")
        })?;
        let destinations = match resolver.list() {
            crate::platform::agenticos_external::media_import::DestinationList::Complete(rows) => {
                rows.iter()
                    .map(|row| {
                        json!({
                            "connection_id": row.connection_id,
                            "toolkit": row.toolkit,
                            "destination_id": row.destination_id,
                            "display_name": row.display_name,
                            "status": row.status,
                            "available": row.available,
                            "publishable": row.publishable,
                        })
                    })
                    .collect::<Vec<Value>>()
            }
            crate::platform::agenticos_external::media_import::DestinationList::Ambiguous
            | crate::platform::agenticos_external::media_import::DestinationList::Unavailable => {
                return Err(Error::rejected(
                    "capability_unavailable: the destinations read did not produce a complete list",
                ))
            }
        };
        Ok(json!({"install_id": install, "destinations": destinations}))
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
        let current = self
            .store
            .app_binding_for_slot(install, context, slot, required_str(bundle, "digest")?)?
            .ok_or_else(|| Error::rejected("publication binding is absent or revoked"))?;
        if current.id != proof.id
            || current.revision != proof.revision
            || current.digest != proof.digest
            || current.config != proof.config
        {
            return Err(Error::rejected(
                "publication binding revision or incarnation changed",
            ));
        }
        let configured = carry_publish(
            self.app_binding_config(
                install,
                context,
                slot,
                proof.config["connection_id"]
                    .as_str()
                    .ok_or_else(|| Error::rejected("binding connection receipt is missing"))?,
                bundle,
                files,
            )?,
            &proof.config,
        );
        if configured != proof.config {
            return Err(Error::rejected(
                "publication binding installation/context/connection receipt is stale",
            ));
        }
        Ok(())
    }
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
