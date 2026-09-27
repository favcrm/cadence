//! Operator discovery and custody management. No capability grants are created.
use super::*;
use crate::platform;
use crate::proto::identifier;
use crate::store::CredentialRecord;

impl Shared {
    fn connection_descriptor(
        &self,
        provider: &str,
    ) -> Result<crate::platform::connections::ProviderDescriptor> {
        let adapter = self
            .platforms
            .get(provider)
            .ok_or_else(|| Error::rejected("connection provider adapter is unavailable"))?;
        let descriptor = adapter
            .connection_descriptor()
            .ok_or_else(|| Error::rejected("connection provider management is unavailable"))?;
        if descriptor.provider != provider {
            return Err(Error::rejected(
                "provider descriptor identity is inconsistent",
            ));
        }
        descriptor.validate(adapter.table())?;
        Ok(descriptor)
    }
    fn connection_record(
        &self,
        provider: &str,
        account: &str,
        record: Option<&CredentialRecord>,
    ) -> Result<Value> {
        let adapter = self.platforms.get(provider);
        let descriptor = self.connection_descriptor(provider).ok();
        let reviewed = adapter.and_then(|a| a.table().manifest_version.clone());
        let reported = adapter.and_then(|a| a.reported_manifest_version());
        let pin = match (&reviewed, &reported) {
            (Some(a), Some(b)) if a == b => "matched",
            (_, None) => "missing",
            _ => "mismatched",
        };
        let registration = adapter.and_then(|a| a.connection_registration()).map(|r| {
            crate::platform::connections::registration_digest(&format!(
                "{r}:{}",
                serde_json::to_string(&descriptor).unwrap_or_default()
            ))
        });
        let builtin = record.is_none();
        let id = match record {
            Some(r) => r.connection_id.clone(),
            None => format!(
                "builtin-{}",
                uuid::Uuid::new_v5(
                    &uuid::Uuid::NAMESPACE_OID,
                    format!(
                        "{}:{provider}:{account}",
                        self.store.connection_workspace_id()?
                    )
                    .as_bytes()
                )
                .simple()
            ),
        };
        let custody_available = record.is_none()
            || platform::load_credential(&self.store, &self.platform_custody, provider, account)
                .is_ok();
        Ok(
            json!({"id":id,"provider":provider,"account":account,"kind":if builtin{"builtin"}else{"enrolled"},"revision":record.map(|r|r.credential_revision),"registration_digest":registration,"descriptor":descriptor,"scopes":record.map(|r|r.scopes.clone()).unwrap_or_default(),"status":{"adapter_registered":adapter.is_some(),"descriptor_available":descriptor.is_some(),"custody_available":custody_available,"manifest_status":pin,"reviewed_pin":reviewed,"reported_pin":reported,"execution_authority":false,"network_checked":false}}),
        )
    }
    fn connection_metadata_projection(&self, record: &CredentialRecord) -> Result<Value> {
        let mut projection =
            self.connection_record(&record.platform, &record.account, Some(record))?;
        projection["status"]["custody_available"] = json!([false, true]);
        Ok(projection)
    }
    fn connection_list_locked(&self) -> Result<Vec<Value>> {
        let mut rows = vec![self.connection_record("local", "local", None)?];
        for (provider, adapter) in &self.platforms {
            if let Some(descriptor) = adapter.connection_descriptor() {
                if descriptor.validate(adapter.table()).is_ok() {
                    for account in descriptor.builtin_accounts {
                        if provider != "local" || account != "local" {
                            rows.push(self.connection_record(provider, &account, None)?);
                        }
                    }
                }
            }
        }
        for record in self.store.platform_credentials()? {
            rows.push(self.connection_record(&record.platform, &record.account, Some(&record))?);
        }
        rows.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
        Ok(rows)
    }
    pub(super) fn rpc_connection(
        self: &Arc<Self>,
        method: &str,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        self.operator_connection("connection management", params, peer_pid)?;
        let allowed: &[&str] = match method {
            "connection_providers" | "connection_list" => &[],
            "connection_show" | "connection_check" | "connection_revoke" => &["connection_id"],
            "connection_create" => &[
                "provider",
                "account",
                "shape",
                "token",
                "scopes",
                "accept_same_uid_risk",
            ],
            "connection_rotate" => &["connection_id", "token", "scopes", "accept_same_uid_risk"],
            _ => return Err(Error::rejected("unknown connection operation")),
        };
        let object = params
            .as_object()
            .ok_or_else(|| Error::rejected("connection parameters must be an object"))?;
        if object.keys().any(|k| !allowed.contains(&k.as_str())) {
            return Err(Error::rejected("unsupported connection parameter"));
        }
        if params
            .get("accept_same_uid_risk")
            .is_some_and(|v| !v.is_boolean())
        {
            return Err(Error::rejected("custody risk acceptance must be boolean"));
        }
        match method {
            "connection_providers" => {
                let mut rows = Vec::new();
                for (provider, adapter) in &self.platforms {
                    let descriptor = self.connection_descriptor(provider).ok();
                    rows.push(json!({"provider":provider,"descriptor":descriptor,"descriptor_available":descriptor.is_some(),"manifest_status":match (adapter.table().manifest_version.as_deref(),adapter.reported_manifest_version()) {(Some(a),Some(b)) if a==b=>"matched",(_,None)=>"missing",_=>"mismatched"},"reviewed_pin":adapter.table().manifest_version,"reported_pin":adapter.reported_manifest_version(),"network_checked":false,"registration_digest":adapter.connection_registration().map(|r|crate::platform::connections::registration_digest(&format!("{r}:{}",serde_json::to_string(&descriptor).unwrap_or_default())))}));
                }
                if !self.platforms.contains_key("local") {
                    rows.push(json!({"provider":"local","descriptor":Value::Null,"descriptor_available":false,"manifest_status":"missing","reviewed_pin":Value::Null,"reported_pin":Value::Null,"network_checked":false,"registration_digest":Value::Null}));
                }
                rows.sort_by(|a, b| a["provider"].as_str().cmp(&b["provider"].as_str()));
                Ok(json!({"providers":rows}))
            }
            "connection_list" | "connection_show" | "connection_check" => {
                let _guard = self
                    .platform_custody_lock
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                let rows = self.connection_list_locked()?;
                if method == "connection_list" {
                    return Ok(json!({"connections":rows}));
                }
                let id = connection_id(params)?;
                let row = rows
                    .into_iter()
                    .find(|r| r["id"] == id)
                    .ok_or_else(|| Error::rejected("connection is unavailable or stale"))?;
                Ok(json!({"connection":row}))
            }
            "connection_create" => {
                let provider = identifier(required_str(params, "provider")?, "Provider")?;
                let account = identifier(required_str(params, "account")?, "Account")?;
                let descriptor = self.connection_descriptor(&provider)?;
                let shape = required_str(params, "shape")?;
                if platform::is_builtin(&provider, &account)
                    || descriptor.builtin_accounts.contains(&account)
                    || !descriptor.enrollment_shapes.iter().any(|s| s == shape)
                {
                    return Err(Error::rejected(
                        "connection enrollment shape is unsupported",
                    ));
                }
                let token = required_str(params, "token")?;
                let enrolled=self.enroll_inner(&json!({"platform":provider,"account":account,"shape":shape,"token":token,"scopes":params.get("scopes").ok_or_else(||Error::rejected("scopes are required"))?,"accept_same_uid_risk":params.get("accept_same_uid_risk").cloned().unwrap_or(json!(false))}),false,None,Some(&descriptor.capabilities.iter().flat_map(|c|c.scopes.clone()).collect::<Vec<_>>()),Some(&|record|self.connection_metadata_projection(record))).map_err(|error|connection_error(error,token))?;
                let _guard = self
                    .platform_custody_lock
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                let record = self
                    .store
                    .connection_credential(
                        enrolled["account"]["connection_id"]
                            .as_str()
                            .ok_or_else(|| Error::internal("missing connection identity"))?,
                    )?
                    .ok_or_else(|| Error::rejected("connection was concurrently removed"))?;
                Ok(json!({"connection":self.connection_record(&provider,&account,Some(&record))?}))
            }
            "connection_rotate" | "connection_revoke" => {
                let id = connection_id(params)?;
                let record = self
                    .store
                    .connection_credential(id)?
                    .ok_or_else(|| Error::rejected("connection is unavailable or stale"))?;
                if method == "connection_revoke" {
                    self.revoke_inner(
                        &json!({"platform":record.platform,"account":record.account}),
                        Some(id),
                    )?;
                    return Ok(json!({"connection_id":id,"revoked":true}));
                }
                let descriptor = self.connection_descriptor(&record.platform)?;
                if !descriptor
                    .enrollment_shapes
                    .iter()
                    .any(|s| s == &record.exchange)
                {
                    return Err(Error::rejected(
                        "connection management shape is unsupported",
                    ));
                }
                let mut mapped = params.clone();
                mapped.as_object_mut().unwrap().remove("connection_id");
                mapped["platform"] = json!(record.platform);
                mapped["account"] = json!(record.account);
                let token = required_str(params, "token")?;
                mapped["shape"] = json!(record.exchange);
                self.enroll_inner(
                    &mapped,
                    true,
                    Some(id),
                    Some(
                        &descriptor
                            .capabilities
                            .iter()
                            .flat_map(|c| c.scopes.clone())
                            .collect::<Vec<_>>(),
                    ),
                    Some(&|record| self.connection_metadata_projection(record)),
                )
                .map_err(|error| connection_error(error, token))?;
                let _guard = self
                    .platform_custody_lock
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                let current = self
                    .store
                    .connection_credential(id)?
                    .ok_or_else(|| Error::rejected("connection was concurrently removed"))?;
                Ok(
                    json!({"connection":self.connection_record(&current.platform,&current.account,Some(&current))?}),
                )
            }
            _ => unreachable!(),
        }
    }
}

fn connection_error(error: Error, token: &str) -> Error {
    // Legacy custody errors can name an account. A supplied credential must
    // not be reflected through that public metadata, even on a refused write.
    if platform::refuse_leak("connection error", &error.to_string(), token.as_bytes()).is_err() {
        Error::rejected("connection credential operation refused")
    } else {
        error
    }
}

fn connection_id(params: &Value) -> Result<&str> {
    let id = required_str(params, "connection_id")?;
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    {
        return Err(Error::rejected("connection ID is invalid"));
    }
    Ok(id)
}
