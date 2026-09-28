//! Provider-neutral publication configuration. Configuration is not authority.
use super::*;
use crate::issue::{app, app_catalog::workspace};
use crate::store::app_bindings::BindingProof;
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
            "app_binding_show" => self
                .store
                .app_binding_show(install, required_str(params, "binding_id")?),
            "app_binding_list" => {
                let pm = self.pm_at(&self.pm_dir()?)?;
                workspace::with_runtime_snapshot(&pm, install, |bundle, _| {
                    self.store.app_binding_list_preferred(
                        install,
                        context,
                        Some(required_str(bundle, "digest")?),
                    )
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
                    let config = self.app_binding_config(
                        install,
                        context,
                        slot,
                        required_str(params, "connection_id")?,
                        bundle,
                        files,
                    )?;
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
        let configured = self.app_binding_config(
            install,
            context,
            slot,
            proof.config["connection_id"]
                .as_str()
                .ok_or_else(|| Error::rejected("binding connection receipt is missing"))?,
            bundle,
            files,
        )?;
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
