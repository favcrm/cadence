//! Explicit operator release of one actual accepted app artifact.
use super::app_bindings_rpc::strict_fields;
use super::*;
use crate::issue::app_catalog::workspace;
use crate::store::{app_bindings::BindingProof, app_effects, app_runs, EffectRow};

impl Shared {
    pub(super) fn rpc_app_effect(
        self: &Arc<Self>,
        method: &str,
        params: &Value,
        pid: u32,
    ) -> Result<Value> {
        if method != "app_effect_resolve" {
            self.operator_connection("app artifact release", params, pid)?;
        }
        strict_fields(
            params,
            match method {
                "app_effect_stage" => &["run_id", "artifact_id", "slot", "request_id", "title"],
                "app_effect_show" => &["effect_id"],
                "app_effect_list" => &["install_id", "context_id"],
                "app_effect_decide" => &["effect_id", "digest", "decision"],
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
            "app_effect_show" => self
                .store
                .app_effect_show(required_str(params, "effect_id")?),
            "app_effect_list" => self.store.app_effect_list(
                optional_str(params, "install_id"),
                optional_str(params, "context_id"),
            ),
            "app_effect_stage" => self.stage_app_artifact(params),
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
                let frozen = self.store.app_effect_show(id)?;
                if frozen["effect"]["digest"] != digest || frozen["effect"]["state"] != "waiting" {
                    return Err(Error::rejected(
                        "app release digest is stale or effect is no longer waiting",
                    ));
                }
                let authority = &frozen["effect"]["authority"];
                let pm = self.pm_at(&self.pm_dir()?)?;
                let decided = workspace::with_runtime_snapshot(
                    &pm,
                    required_str(authority, "install_id")?,
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

    fn stage_app_artifact(&self, params: &Value) -> Result<Value> {
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
        workspace::with_runtime_snapshot(&pm, install, |bundle, files| {
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
            let core_digest = app_effects::authority_digest(&authority);
            let provenance = json!({"schema":1,"authorization_kind":"app_artifact","effect_id":effect_id,"authority_digest":core_digest,
                "install_id":install,"context_id":run["context_id"],"context_revision":run["snapshot"]["context"]["revision"],
                "context_digest":run["snapshot"]["context"]["digest"],"run_id":run_id,"run_snapshot_digest":run["snapshot_digest"],
                "artifact_id":artifact["id"],"artifact_digest":artifact["digest"],"binding_id":proof.id,
                "binding_revision":proof.revision,"binding_digest":proof.digest,
                "connection_id":proof.config["connection_id"],"connection_kind":proof.config["connection_kind"],
                "connection_revision":proof.config["connection_revision"],"registration_digest":proof.config["registration_digest"],
                "sink_registration":proof.config["sink_registration"],"mapping":proof.config["mapping"],
                "review_receipt_digest":authority["review_receipt_digest"]});
            authority["provenance"] = provenance.clone();
            let provider = required_str(&proof.config, "provider")?;
            let account = required_str(&proof.config, "account")?;
            let tool = required_str(&proof.config["mapping"], "tool")?;
            let adapter = self
                .platforms
                .get(provider)
                .ok_or_else(|| Error::rejected("publication adapter unavailable"))?;
            let input = adapter
                .prepare_app_text(
                    required_str(params, "title")?,
                    required_str(artifact, "text")?,
                    &provenance,
                )
                .map_err(Error::rejected)?;
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
            crate::platform::refuse_leak("app release input", &input.to_string(), &bytes)?;
            crate::platform::refuse_leak("app release preview", &preview, &bytes)?;
            let row = EffectRow {
                effect_id,
                request,
                agent: required_str(&run["snapshot"], "owner_pm")?.into(),
                platform: provider.into(),
                account: account.into(),
                tool: tool.into(),
                label: None,
                input,
                input_summary: required_str(params, "title")?.into(),
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
        })
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

    fn execute_app_artifact(&self, id: &str, digest: &str) -> Result<Value> {
        let frozen = self.store.app_effect_show(id)?;
        let authority = &frozen["effect"]["authority"];
        let pm = self.pm_at(&self.pm_dir()?)?;
        workspace::with_runtime_snapshot(
            &pm,
            required_str(authority, "install_id")?,
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
                let bytes = crate::platform::load_credential(
                    &self.store,
                    &self.platform_custody,
                    provider,
                    account,
                )?;
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
                        if crate::platform::refuse_leak(
                            "app release outcome",
                            &value.to_string(),
                            &bytes,
                        )
                        .is_err()
                        {
                            (
                                false,
                                true,
                                json!({"kind":"uncertain","error":"provider outcome withheld","verified":verified}),
                            )
                        } else {
                            (
                                true,
                                false,
                                json!({"kind":"released","result":value,"verified":verified}),
                            )
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
