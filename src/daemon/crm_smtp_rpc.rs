//! One host-custodied SMTP sender per CRM installation/context (CAD-785).
//!
//! Every method first proves the operator connection, then resolves
//! the installation through the workspace catalog snapshot and
//! proves the live context on core — an unknown, archived or foreign
//! context refuses before any file or socket opens, reads included.
//! The payload grammar is exact: identity-shaped (`by`, `actor`),
//! receipt-shaped (`assistant_receipt`, `turn_id`, `nonce`),
//! discovery-link (`project`, `project_link`) and routing
//! (`workspace`) fields are unsupported and refused, as are unknown
//! fields of any kind.
//!
//! Binding is the typed link row in core: exactly one live
//! `(install, context) → connection` binding exists, pinning the
//! credential (authorization) revision observed at bind time.
//! Rotation bumps the credential revision, so the link goes stale
//! by construction and the send path refuses until the operator
//! rebinds under CAS; revocation — of the link or of the credential
//! — refuses the same way. There is no agent-origin path: an agent
//! caller or detached child is refused before anything is read.
//!
//! The test send is a distinct operator effect: one operator-typed
//! address, the exact CAD-782 frozen HTML/text bytes with the
//! host-custodied verified sender, a single multipart submission
//! over mandatory authenticated TLS, and a receipt recording SMTP
//! acceptance/refusal only — never delivery, never reads. No bulk
//! path exists in this ticket.
use super::*;
use crate::issue::app_catalog::workspace;
use crate::store::app_records::{email_shape_valid, RecordStore};

fn link_revision(params: &Value) -> Result<i64> {
    params
        .get("expected_revision")
        .and_then(Value::as_i64)
        .filter(|n| *n > 0)
        .ok_or_else(|| Error::rejected("expected binding revision must be a positive integer"))
}

fn request_id(params: &Value) -> Result<&str> {
    let id = required_str(params, "request_id")?;
    crate::proto::identifier(id, "binding request ID")?;
    Ok(id)
}

/// Operator-typed test recipient: strict address shape, ASCII-only
/// SMTP bytes, no markup, and never a `.invalid` placeholder — a
/// test send must be receivable by the operator, not thrown at a
/// reserved name.
fn test_recipient(address: &str) -> Result<()> {
    if !email_shape_valid(address)
        || !address.is_ascii()
        || address.contains(['<', '>', '(', ')', '[', ']', '\\', '"', '\'', ';', ',', '`'])
        || address.to_lowercase().contains(".invalid")
    {
        return Err(Error::rejected(
            "SMTP test recipient must be one operator address",
        ));
    }
    Ok(())
}

impl Shared {
    /// Resolve the live installation/context, then run `action`.
    /// Unknown or diverted installations and unknown, archived or
    /// foreign contexts refuse before any file or socket opens.
    pub(super) fn crm_smtp_scope<R>(
        self: &Arc<Self>,
        install: &str,
        context: &str,
        action: impl FnOnce() -> Result<R>,
    ) -> Result<R> {
        crate::proto::identifier(install, "installation ID")?;
        let pm = self.pm_at(&self.pm_dir()?)?;
        workspace::with_runtime_snapshot(&pm, install, |_, _| {
            let _release = self
                .app_release_lock
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            self.store.app_context_proof(install, context)?;
            Ok(())
        })?;
        action()
    }

    /// The live verified connection behind one link: enrolled SMTP
    /// custody, reviewed registration matched, authorization revision
    /// exactly the link's. Anything else — missing, revoked,
    /// rotated, mismatched manifest — refuses as stale/unavailable.
    /// Call with the custody lock held; the test send holds it
    /// across the submission so a concurrent revoke or rotate
    /// cannot interleave the send.
    pub(super) fn crm_smtp_authority(
        &self,
        connection_id: &str,
        auth_revision: i64,
    ) -> Result<(
        crate::store::CredentialRecord,
        crate::platform::smtp::SmtpEnvelope,
        crate::platform::smtp::SmtpProjection,
        Value,
    )> {
        let record = self
            .store
            .connection_credential(connection_id)?
            .ok_or_else(|| Error::rejected("SMTP sender connection is unavailable or stale"))?;
        if record.exchange != crate::platform::smtp::ENROLLMENT_SHAPE {
            return Err(Error::rejected(
                "SMTP sender connection is unavailable or stale",
            ));
        }
        let revision = i64::try_from(record.credential_revision)
            .map_err(|_| Error::rejected("SMTP sender connection is unavailable or stale"))?;
        if revision != auth_revision {
            return Err(Error::rejected(
                "SMTP sender authorization revision is stale; rebind the sender",
            ));
        }
        let row = self
            .connection_list_locked()?
            .into_iter()
            .find(|row| row["id"] == connection_id)
            .ok_or_else(|| Error::rejected("SMTP sender connection is unavailable or stale"))?;
        if row["status"]["manifest_status"] != "matched"
            || row["status"]["custody_available"] != true
            || !row["registration_digest"].is_string()
            || row["smtp"].is_null()
        {
            return Err(Error::rejected(
                "SMTP sender reviewed registration or custody is unavailable",
            ));
        }
        let bytes = crate::platform::load_credential(
            &self.store,
            &self.platform_custody,
            &record.platform,
            &record.account,
        )?;
        let (envelope, projection) = crate::platform::smtp::custody_decode(&bytes)?;
        crate::platform::refuse_leak(
            "smtp send authority",
            &projection.to_json().to_string(),
            envelope.secret(),
        )?;
        // The projection the receipt names must be the custody bytes
        // just loaded — a listed row is informational, never
        // authority.
        if projection.to_json() != row["smtp"] {
            return Err(Error::rejected(
                "SMTP sender connection changed under claim",
            ));
        }
        Ok((record, envelope, projection, row))
    }

    pub(super) fn rpc_crm_smtp(
        self: &Arc<Self>,
        method: &str,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        self.operator_connection("CRM SMTP sender management", params, peer_pid)?;
        // Receipt-shaped fields are refused everywhere: no assistant
        // attribution exists on this path, so any present receipt is
        // forged. Identity, routing and discovery-link fields cannot
        // claim scope either — the snapshot and the link own that.
        if let Some(fields) = params.as_object() {
            if fields.keys().any(|key| {
                matches!(
                    key.as_str(),
                    "assistant_receipt"
                        | "turn_id"
                        | "nonce"
                        | "by"
                        | "actor"
                        | "workspace"
                        | "project"
                        | "project_link"
                )
            }) {
                return Err(Error::rejected("CRM SMTP payload has unsupported fields"));
            }
        }
        let allowed: &[&str] = match method {
            "crm_smtp_bind" => &["install_id", "context_id", "connection_id", "request_id"],
            "crm_smtp_rebind" => &[
                "install_id",
                "context_id",
                "connection_id",
                "expected_revision",
            ],
            "crm_smtp_revoke" => &["install_id", "context_id", "expected_revision"],
            "crm_smtp_show" => &["install_id", "context_id"],
            "crm_smtp_test_send" => &["install_id", "context_id", "campaign_id", "to_email"],
            _ => return Err(Error::rejected("unknown CRM SMTP method")),
        };
        let fields = params
            .as_object()
            .ok_or_else(|| Error::rejected("CRM SMTP payload must be an object"))?;
        if fields.keys().any(|key| !allowed.contains(&key.as_str())) {
            return Err(Error::rejected("CRM SMTP payload has unsupported fields"));
        }
        let install = required_str(params, "install_id")?;
        let context = required_str(params, "context_id")?;
        match method {
            "crm_smtp_bind" => {
                let connection = required_str(params, "connection_id")?;
                let request = request_id(params)?;
                self.crm_smtp_scope(install, context, || {
                    let _custody = self
                        .platform_custody_lock
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    let record = self
                        .store
                        .connection_credential(connection)?
                        .ok_or_else(|| {
                            Error::rejected("SMTP sender connection is unavailable or stale")
                        })?;
                    if record.exchange != crate::platform::smtp::ENROLLMENT_SHAPE {
                        return Err(Error::rejected(
                            "SMTP sender connection is unavailable or stale",
                        ));
                    }
                    // Bind through the same reviewed-registration and
                    // custody gate the send path enforces, so a bind
                    // can never name an unverified connection.
                    let (_, _, projection, _) = self.crm_smtp_authority(
                        connection,
                        i64::try_from(record.credential_revision).map_err(|_| {
                            Error::rejected("SMTP sender connection is unavailable or stale")
                        })?,
                    )?;
                    self.store.crm_smtp_bind(
                        install,
                        context,
                        connection,
                        i64::try_from(record.credential_revision).map_err(|_| {
                            Error::internal("SMTP authorization revision overflows")
                        })?,
                        &projection,
                        request,
                    )
                })
            }
            "crm_smtp_rebind" => {
                let connection = required_str(params, "connection_id")?;
                let expected = link_revision(params)?;
                self.crm_smtp_scope(install, context, || {
                    let _custody = self
                        .platform_custody_lock
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    let record = self
                        .store
                        .connection_credential(connection)?
                        .ok_or_else(|| {
                            Error::rejected("SMTP sender connection is unavailable or stale")
                        })?;
                    if record.exchange != crate::platform::smtp::ENROLLMENT_SHAPE {
                        return Err(Error::rejected(
                            "SMTP sender connection is unavailable or stale",
                        ));
                    }
                    let (_, _, projection, _) = self.crm_smtp_authority(
                        connection,
                        i64::try_from(record.credential_revision).map_err(|_| {
                            Error::rejected("SMTP sender connection is unavailable or stale")
                        })?,
                    )?;
                    self.store.crm_smtp_rebind(
                        install,
                        context,
                        connection,
                        i64::try_from(record.credential_revision).map_err(|_| {
                            Error::internal("SMTP authorization revision overflows")
                        })?,
                        &projection,
                        expected,
                    )
                })
            }
            "crm_smtp_revoke" => {
                let expected = link_revision(params)?;
                self.crm_smtp_scope(install, context, || {
                    let _custody = self
                        .platform_custody_lock
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    self.store.crm_smtp_revoke(install, context, expected)
                })
            }
            "crm_smtp_show" => self.crm_smtp_scope(install, context, || {
                let _custody = self
                    .platform_custody_lock
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                let link = self
                    .store
                    .crm_smtp_link(install, context)?
                    .ok_or_else(|| {
                        Error::rejected(
                            "no SMTP sender is bound to this installation and context",
                        )
                    })?;
                if link.state != "live" {
                    return Err(Error::rejected(
                        "SMTP sender binding is revoked; bind it again instead",
                    ));
                }
                let (_, _, projection, row) =
                    self.crm_smtp_authority(&link.connection_id, link.auth_revision)?;
                Ok(json!({
                    "binding": {
                        "install_id": install,
                        "context_id": context,
                        "connection_id": link.connection_id,
                        "auth_revision": link.auth_revision,
                        "link_revision": link.link_revision,
                        "state": link.state,
                        "digest": link.digest,
                        "sender": {"name": projection.sender_name, "address": projection.sender},
                        "transport": {"host": projection.host, "port": projection.port, "tls_mode": projection.tls_mode, "username": projection.username},
                        "connection": row,
                    },
                }))
            }),
            "crm_smtp_test_send" => {
                let campaign = required_str(params, "campaign_id")?;
                crate::proto::identifier(campaign, "campaign ID")?;
                let to = required_str(params, "to_email")?;
                test_recipient(to)?;
                self.crm_smtp_scope(install, context, || {
                    // Frozen bytes first: the exact current revision
                    // through the CAD-782 renderer. Reads observe the
                    // live context proof above; an unknown campaign
                    // refuses before any socket opens.
                    let records = RecordStore::open(&self.state_dir, install)?;
                    let link = self
                        .store
                        .crm_smtp_link(install, context)?
                        .ok_or_else(|| {
                            Error::rejected(
                                "no SMTP sender is bound to this installation and context",
                            )
                        })?;
                    if link.state != "live" {
                        return Err(Error::rejected(
                            "SMTP sender binding is revoked; bind it again instead",
                        ));
                    }
                    // Authority, custody bytes and the submission hold
                    // one lock: a concurrent rotate, revoke or rebind
                    // cannot interleave this send.
                    let _custody = self
                        .platform_custody_lock
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    let live = self
                        .store
                        .crm_smtp_link(install, context)?
                        .ok_or_else(|| {
                            Error::rejected(
                                "no SMTP sender is bound to this installation and context",
                            )
                        })?;
                    if live.state != "live"
                        || live.link_revision != link.link_revision
                        || live.digest != link.digest
                    {
                        return Err(Error::rejected(
                            "SMTP sender binding changed under claim",
                        ));
                    }
                    let (_, envelope, projection, _) =
                        self.crm_smtp_authority(&link.connection_id, link.auth_revision)?;
                    let rendered = records.app_content_verified_test_bytes(
                        context,
                        campaign,
                        &projection.sender_name,
                        &projection.sender,
                    )?;
                    let message = crate::platform::smtp::SmtpMessage {
                        to: to.to_string(),
                        subject: rendered["subject"]
                            .as_str()
                            .ok_or_else(|| Error::internal("SMTP render lost its subject"))?
                            .to_string(),
                        html: rendered["html"]
                            .as_str()
                            .ok_or_else(|| Error::internal("SMTP render lost its HTML"))?
                            .to_string(),
                        text: rendered["text"]
                            .as_str()
                            .ok_or_else(|| Error::internal("SMTP render lost its text"))?
                            .to_string(),
                        unsubscribe_url: rendered["unsubscribe_url"]
                            .as_str()
                            .unwrap_or("")
                            .to_string(),
                        idempotency_key: None,
                    };
                    let content_digest = rendered["content_digest"]
                        .as_str()
                        .ok_or_else(|| Error::internal("SMTP render lost its digest"))?;
                    let outcome = crate::platform::smtp::send(
                        &envelope,
                        &message,
                        content_digest,
                        self.smtp_test_ca.as_deref(),
                    )?;
                    let receipt = json!({
                        "test_send": true,
                        "kind": "test",
                        "install_id": install,
                        "context_id": context,
                        "campaign_id": campaign,
                        "connection_id": link.connection_id,
                        "auth_revision": link.auth_revision,
                        "link_revision": link.link_revision,
                        "link_digest": link.digest,
                        "content_revision": rendered["content_revision"],
                        "content_digest": rendered["content_digest"],
                        "binding_digest": rendered["binding_digest"],
                        "payload_digest": crate::store::app_runs::material_digest(&json!({
                            "domain": "cadence-crm-smtp-test-send-v1",
                            "payload_digest": rendered["payload_digest"],
                            "to_email": to,
                            "connection_id": link.connection_id,
                            "auth_revision": link.auth_revision,
                            "link_digest": link.digest,
                        })),
                        "transport": {"host": projection.host, "port": projection.port, "tls_mode": projection.tls_mode},
                        "sender": {"name": projection.sender_name, "address": projection.sender},
                        "multipart": {"html_bytes": message.html.len(), "text_bytes": message.text.len()},
                        "accepted": outcome.accepted,
                        "smtp_code": outcome.code,
                        "smtp_message": outcome.message,
                        "delivery_claim": "smtp-acceptance-only",
                        "unsubscribe_authority": "test send (no unsubscribe token)",
                    });
                    // The receipt must not carry the secret even when
                    // the server echoed credential-shaped bytes — the
                    // send path screens first; this is the second bar.
                    crate::platform::refuse_leak(
                        "smtp test receipt",
                        &receipt.to_string(),
                        envelope.secret(),
                    )?;
                    // CAD-786: an accepted test send is the only
                    // "sent after preview" evidence a campaign
                    // prepare accepts, and it binds this exact
                    // content + link digest — an edit or rebind
                    // makes the evidence stale by construction.
                    if outcome.accepted {
                        records.app_campaign_test_send_record(
                            context,
                            campaign,
                            content_digest,
                            &link.digest,
                        )?;
                    }
                    self.store.note_crm_smtp_test(install, context, &receipt);
                    Ok(json!({"receipt": receipt}))
                })
            }
            _ => Err(Error::rejected("unknown CRM SMTP method")),
        }
    }
}
