//! Operator discovery and custody management. No capability grants are created.
use super::*;
use crate::platform;
use crate::store::CredentialRecord;

impl Shared {
    pub(super) fn connection_descriptor(
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
            None => self.builtin_connection_id(provider, account)?,
        };
        let custody_available = record.is_none()
            || platform::load_credential(&self.store, &self.platform_custody, provider, account)
                .is_ok();
        // CAD-785/CAD-1064: enrolled SMTP senders project their
        // non-secret transport/sender material from custody. A failure
        // is never swallowed: the row stays an SMTP sender
        // (`smtp_sender`, decided from the exchange shape) with
        // `smtp: null` and a typed, secret-free `smtp_error`.
        // CAD-1063: the hosted built-in `agenticos` account is the CRM
        // sender on a hosted daemon. The platform sends, so it has no
        // credential: never rotatable, never a typed custody fault.
        let hosted_sender = record.is_none()
            && provider == platform::agenticos::PLATFORM
            && account == platform::agenticos::HOSTED_ACCOUNT
            && self.hosted_email.is_some();
        let smtp_sender = hosted_sender
            || matches!(record, Some(r) if r.exchange == platform::smtp::ENROLLMENT_SHAPE);
        let (smtp, smtp_error) = match record {
            Some(record) if smtp_sender => match self.smtp_projection_typed(record) {
                Ok(projection) => (Some(projection.to_json()), None),
                Err(fault) => (None, Some(fault.code())),
            },
            None if hosted_sender => (
                self.hosted_email.as_ref().map(|h| h.projection().to_json()),
                None,
            ),
            _ => (None, None),
        };
        let verification_support = self.connection_verification_support(provider, record);
        Ok(
            json!({"id":id,"provider":provider,"account":account,"kind":if builtin{"builtin"}else{"enrolled"},"revision":record.map(|r|r.credential_revision),"registration_digest":registration,"descriptor":descriptor,"scopes":record.map(|r|r.scopes.clone()).unwrap_or_default(),"smtp":smtp,"smtp_sender":smtp_sender,"smtp_error":smtp_error,"verification_support":verification_support,"status":{"adapter_registered":adapter.is_some(),"descriptor_available":descriptor.is_some(),"custody_available":custody_available,"manifest_status":pin,"reviewed_pin":reviewed,"reported_pin":reported,"execution_authority":false,"network_checked":false}}),
        )
    }
    /// The id of a built-in (credential-less) connection row.
    pub(super) fn builtin_connection_id(&self, provider: &str, account: &str) -> Result<String> {
        Ok(format!(
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
        ))
    }

    /// CAD-1063: `Some` when `connection_id` names the hosted platform
    /// sender on a daemon that has the hosted email door.
    pub(super) fn hosted_sender(
        &self,
        connection_id: &str,
    ) -> Result<Option<&crate::platform::hosted_email::HostedEmail>> {
        let Some(hosted) = self.hosted_email.as_ref() else {
            return Ok(None);
        };
        let id = self.builtin_connection_id(
            platform::agenticos::PLATFORM,
            platform::agenticos::HOSTED_ACCOUNT,
        )?;
        Ok((id == connection_id).then_some(hosted))
    }

    /// CAD-1126: on a hosted daemon an SMTP enrolment is proven live
    /// before anything is stored: connect, TLS and login through
    /// `smtp.internal` (`/v1/verify`, no send). A wrong password or a
    /// blocked host refuses here, in plain words, and custody stays
    /// untouched. Self-hosted enrolment is unchanged (no live check).
    fn verify_hosted_smtp(&self, params: &Value) -> Result<()> {
        let Some(relay) = self.smtp_internal.as_ref() else {
            return Ok(());
        };
        let enrollment = platform::smtp::parse_enrollment(params)?;
        relay.verify(&platform::smtp_internal::Server {
            host: &enrollment.host,
            port: enrollment.port,
            tls_mode: &enrollment.tls_mode,
            username: &enrollment.username,
            secret: &enrollment.secret,
        })
    }

    /// Live non-secret SMTP material for one enrolled record.
    /// Custody-only: the secret never enters the projection by
    /// construction, and the projection is screened before return.
    pub(super) fn smtp_projection_typed(
        &self,
        record: &CredentialRecord,
    ) -> std::result::Result<platform::smtp::SmtpProjection, platform::smtp::ProjectionFault> {
        use platform::smtp::ProjectionFault;
        if record.exchange != platform::smtp::ENROLLMENT_SHAPE {
            return Err(ProjectionFault::Unavailable);
        }
        let bytes = platform::load_credential(
            &self.store,
            &self.platform_custody,
            &record.platform,
            &record.account,
        )
        .map_err(|_| ProjectionFault::Unavailable)?;
        platform::smtp::project_custody(&bytes)
    }
    fn connection_metadata_projection(&self, record: &CredentialRecord) -> Result<Value> {
        let mut projection =
            self.connection_record(&record.platform, &record.account, Some(record))?;
        projection["status"]["custody_available"] = json!([false, true]);
        Ok(projection)
    }
    pub(super) fn connection_list_locked(&self) -> Result<Vec<Value>> {
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
            "connection_test" => &[
                "connection_id",
                "expected_revision",
                "expected_registration_digest",
            ],
            // CAD-785: the `smtp` shape carries typed host, port,
            // TLS mode, username, secret and sender fields instead of
            // an opaque token. Both shapes share one allowlist; each
            // shape refuses the other's credential field below.
            "connection_create" => &[
                "provider",
                "account",
                "shape",
                "token",
                "host",
                "port",
                "tls_mode",
                "username",
                "secret",
                "sender",
                "sender_name",
                "scopes",
                "accept_same_uid_risk",
            ],
            "connection_rotate" => &[
                "connection_id",
                "token",
                "host",
                "port",
                "tls_mode",
                "username",
                "secret",
                "sender",
                "sender_name",
                "scopes",
                "accept_same_uid_risk",
            ],
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
                    let verification_support = self.connection_verification_support(provider, None);
                    rows.push(json!({"provider":provider,"descriptor":descriptor,"descriptor_available":descriptor.is_some(),"manifest_status":match (adapter.table().manifest_version.as_deref(),adapter.reported_manifest_version()) {(Some(a),Some(b)) if a==b=>"matched",(_,None)=>"missing",_=>"mismatched"},"reviewed_pin":adapter.table().manifest_version,"reported_pin":adapter.reported_manifest_version(),"network_checked":false,"registration_digest":adapter.connection_registration().map(|r|crate::platform::connections::registration_digest(&format!("{r}:{}",serde_json::to_string(&descriptor).unwrap_or_default()))),"verification_support":verification_support}));
                }
                if !self.platforms.contains_key("local") {
                    rows.push(json!({"provider":"local","descriptor":Value::Null,"descriptor_available":false,"manifest_status":"missing","reviewed_pin":Value::Null,"reported_pin":Value::Null,"network_checked":false,"registration_digest":Value::Null,"verification_support":self.connection_verification_support("local",None)}));
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
                    // `hosted_smtp`: enrolled senders send through the
                    // hosted `smtp.internal` pass-through (CAD-1126).
                    return Ok(
                        json!({"connections":rows,"hosted_smtp":self.smtp_internal.is_some()}),
                    );
                }
                let id = connection_id(params)?;
                let row = rows
                    .into_iter()
                    .find(|r| r["id"] == id)
                    .ok_or_else(|| Error::rejected("connection is unavailable or stale"))?;
                Ok(json!({"connection":row}))
            }
            "connection_test" => self.connection_test(params),
            "connection_create" => {
                let provider = platform::connections::provider_identifier(
                    required_str(params, "provider")?,
                    "Provider",
                )?;
                let account = platform::connections::provider_account_identifier(
                    &provider,
                    required_str(params, "account")?,
                )?;
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
                // Each shape refuses the other's credential field:
                // a token paste is never reinterpreted as SMTP
                // material, and SMTP fields never ride a token shape.
                if shape == platform::smtp::ENROLLMENT_SHAPE {
                    if params.get("token").is_some() {
                        return Err(Error::rejected(
                            "SMTP enrollment carries typed fields — no token",
                        ));
                    }
                    let shapeliness = smtp_create_params(&provider, &account, params)?;
                    self.verify_hosted_smtp(&shapeliness)?;
                    let enrolled = self
                        .enroll_inner(
                            &shapeliness,
                            false,
                            None,
                            Some(
                                &descriptor
                                    .capabilities
                                    .iter()
                                    .flat_map(|c| c.scopes.clone())
                                    .collect::<Vec<_>>(),
                            ),
                            Some(&|record| self.connection_metadata_projection(record)),
                        )
                        .map_err(|error| {
                            connection_error(
                                error,
                                params.get("secret").and_then(Value::as_str).unwrap_or(""),
                            )
                        })?;
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
                    return Ok(
                        json!({"connection":self.connection_record(&provider,&account,Some(&record))?}),
                    );
                }
                if params.get("secret").is_some()
                    || params.get("host").is_some()
                    || params.get("port").is_some()
                    || params.get("tls_mode").is_some()
                    || params.get("username").is_some()
                    || params.get("sender").is_some()
                    || params.get("sender_name").is_some()
                {
                    return Err(Error::rejected(
                        "token enrollment carries a token — no SMTP fields",
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
                // SMTP rotation always carries the fresh secret;
                // transport/sender fields re-validate when present
                // and inherit live custody otherwise.
                if record.exchange == platform::smtp::ENROLLMENT_SHAPE {
                    if params.get("token").is_some() {
                        return Err(Error::rejected(
                            "SMTP rotation carries the fresh secret — no token",
                        ));
                    }
                    // With a readable projection absent fields inherit.
                    // With none (corrupt, withheld or unavailable) the
                    // operator re-enters every transport field.
                    let mut mapped = match self.smtp_projection_typed(&record) {
                        Ok(current) => smtp_rotate_params(&record, params, &current)?,
                        Err(_) => smtp_rotate_full_params(params)?,
                    };
                    mapped["platform"] = json!(record.platform);
                    mapped["account"] = json!(record.account);
                    mapped["shape"] = json!(record.exchange);
                    self.verify_hosted_smtp(&mapped)?;
                    let secret = params.get("secret").and_then(Value::as_str).unwrap_or("");
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
                    .map_err(|error| connection_error(error, secret))?;
                    let _guard = self
                        .platform_custody_lock
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    let current = self
                        .store
                        .connection_credential(id)?
                        .ok_or_else(|| Error::rejected("connection was concurrently removed"))?;
                    return Ok(
                        json!({"connection":self.connection_record(&current.platform,&current.account,Some(&current))?}),
                    );
                }
                if params.get("secret").is_some() {
                    return Err(Error::rejected(
                        "token rotation carries a token — no SMTP secret",
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

/// Forward exactly the typed shape-`smtp` grammar into the custody
/// path. Every required field rides along; `sender_name` is
/// optional. The grammar is validated eagerly so refusals name the
/// field before custody is touched.
fn smtp_create_params(provider: &str, account: &str, params: &Value) -> Result<Value> {
    let mut out = json!({
        "platform": provider,
        "account": account,
        "shape": platform::smtp::ENROLLMENT_SHAPE,
        "scopes": params.get("scopes").ok_or_else(|| Error::rejected("scopes are required"))?,
        "accept_same_uid_risk": params.get("accept_same_uid_risk").cloned().unwrap_or(json!(false)),
    });
    for field in ["host", "port", "tls_mode", "username", "secret", "sender"] {
        out[field] = params
            .get(field)
            .cloned()
            .ok_or_else(|| Error::rejected(format!("SMTP enrollment is missing '{field}'")))?;
    }
    if let Some(name) = params.get("sender_name") {
        if !name.is_string() {
            return Err(Error::rejected("SMTP field 'sender_name' must be a string"));
        }
        out["sender_name"] = name.clone();
    }
    platform::smtp::parse_enrollment(&out)?;
    Ok(out)
}

/// Merge a rotate's partial re-spec onto live custody: absent
/// transport/sender fields inherit, the fresh `secret` is required,
/// and the merged grammar validates before any custody write. The
/// caller's `connection_id` is stripped — the record writes it.
fn smtp_rotate_params(
    record: &CredentialRecord,
    params: &Value,
    current: &platform::smtp::SmtpProjection,
) -> Result<Value> {
    let _ = record;
    let mut out = params.clone();
    let object = out
        .as_object_mut()
        .ok_or_else(|| Error::rejected("connection parameters must be an object"))?;
    object.remove("connection_id");
    for (field, inherited) in [
        ("host", current.host.clone()),
        ("tls_mode", current.tls_mode.clone()),
        ("username", current.username.clone()),
        ("sender", current.sender.clone()),
        ("sender_name", current.sender_name.clone()),
    ] {
        if object.get(field).is_none() {
            object.insert(field.to_string(), json!(inherited));
        }
    }
    if object.get("port").is_none() {
        object.insert("port".to_string(), json!(current.port));
    }
    for field in [
        "host",
        "port",
        "tls_mode",
        "username",
        "secret",
        "sender",
        "sender_name",
    ] {
        if let Some(value) = object.get(field) {
            if field != "port" && !value.is_string() {
                return Err(Error::rejected(format!(
                    "SMTP field '{field}' must be a string"
                )));
            }
        }
    }
    // Full-grammar validation of the merged document: `overlay`
    // requires the fresh secret and re-checks every inherited byte.
    let merged = Value::Object(object.clone());
    platform::smtp::overlay_rotate(current, &merged)?;
    Ok(Value::Object(object.clone()))
}

/// Rotation of a sender whose settings cannot be read: nothing is
/// inherited, so the full typed grammar must arrive.
fn smtp_rotate_full_params(params: &Value) -> Result<Value> {
    let mut out = params.clone();
    let object = out
        .as_object_mut()
        .ok_or_else(|| Error::rejected("connection parameters must be an object"))?;
    object.remove("connection_id");
    platform::smtp::parse_enrollment(&out).map_err(|error| {
        Error::rejected(format!(
            "this sender's saved settings cannot be read, so every field must be re-entered: {error}"
        ))
    })?;
    Ok(out)
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

pub(super) fn connection_id(params: &Value) -> Result<&str> {
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
