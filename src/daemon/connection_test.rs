use std::net::SocketAddr;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use serde_json::{json, Value};

use super::connections_rpc::connection_id;
use super::*;
use crate::platform;
use crate::store::CredentialRecord;

const OPERATION: &str = "smtp-login-no-send-v1";
const BUDGET: Duration = Duration::from_secs(20);
const MATERIAL_CAP: usize = 16 * 1024;
const RESOLVE_POLL: Duration = Duration::from_millis(25);
const MAX_RESOLVED: usize = 64;

use crate::platform::smtp::{FailureCode, Step};

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum Status {
    Success,
    Failed,
    Stale,
    Unsupported,
}

#[derive(Serialize)]
struct Failure {
    code: FailureCode,
    step: Step,
}

#[derive(Serialize)]
struct VerificationReceipt {
    schema: u32,
    operation: &'static str,
    connection_id: String,
    revision: Option<u64>,
    registration_digest: Option<String>,
    started_at: String,
    completed_at: String,
    status: Status,
    network_attempted: bool,
    authentication_verified: bool,
    email_sent: bool,
    delivery_verified: bool,
    sender_entitlement_verified: bool,
    execution_authority: bool,
    failure: Option<Failure>,
}

impl VerificationReceipt {
    fn new(connection_id: &str, started_at: i64) -> Self {
        Self {
            schema: 1,
            operation: OPERATION,
            connection_id: connection_id.to_string(),
            revision: None,
            registration_digest: None,
            started_at: crate::issue::time::iso(started_at),
            completed_at: String::new(),
            status: Status::Failed,
            network_attempted: false,
            authentication_verified: false,
            email_sent: false,
            delivery_verified: false,
            sender_entitlement_verified: false,
            execution_authority: false,
            failure: None,
        }
    }

    fn value(mut self) -> Value {
        self.completed_at = crate::issue::time::iso(crate::issue::time::now_epoch());
        json!({ "verification": self })
    }

    fn failed(mut self, status: Status, code: FailureCode, step: Step) -> Self {
        self.status = status;
        self.failure = Some(Failure { code, step });
        self
    }
}

fn expected_revision(params: &Value) -> Result<Option<u64>> {
    match params.get("expected_revision") {
        None => Err(Error::rejected("connection test parameters are malformed")),
        Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_u64()
            .filter(|n| *n > 0)
            .map(Some)
            .ok_or_else(|| Error::rejected("connection test parameters are malformed")),
    }
}

fn expected_registration_digest(params: &Value) -> Result<Option<String>> {
    match params.get("expected_registration_digest") {
        None => Err(Error::rejected("connection test parameters are malformed")),
        Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => {
            let rest = s
                .strip_prefix("sha256:")
                .ok_or_else(|| Error::rejected("connection test parameters are malformed"))?;
            if rest.len() != 64
                || !rest
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            {
                return Err(Error::rejected("connection test parameters are malformed"));
            }
            Ok(Some(s.clone()))
        }
        Some(_) => Err(Error::rejected("connection test parameters are malformed")),
    }
}

fn registration_digest_for(shared: &Shared, provider: &str) -> Option<String> {
    let adapter = shared.platforms.get(provider)?;
    let descriptor = shared.connection_descriptor(provider).ok();
    adapter.connection_registration().map(|r| {
        crate::platform::connections::registration_digest(&format!(
            "{r}:{}",
            serde_json::to_string(&descriptor).unwrap_or_default()
        ))
    })
}

fn supported_reason(
    provider: &str,
    record: Option<&CredentialRecord>,
    hosted: bool,
    custody_tag: &'static str,
) -> Option<FailureCode> {
    if hosted {
        return Some(FailureCode::UnsupportedDeployment);
    }
    if provider != platform::smtp::PLATFORM {
        return Some(FailureCode::UnsupportedProvider);
    }
    if let Some(record) = record {
        if record.exchange != platform::smtp::ENROLLMENT_SHAPE {
            return Some(FailureCode::UnsupportedProvider);
        }
        if !matches!(
            record.custody.as_str(),
            platform::custody::FILE_TAG | platform::custody::LIBSECRET_TAG
        ) || record.custody != custody_tag
        {
            return Some(FailureCode::UnsupportedCustody);
        }
    }
    None
}

pub(super) fn verification_support_json(reason: Option<FailureCode>) -> Value {
    json!({
        "operation": if reason.is_none() { json!(OPERATION) } else { Value::Null },
        "supported": reason.is_none(),
        "reason_code": reason.map(|r| serde_json::to_value(r).unwrap_or(Value::Null)).unwrap_or(Value::Null),
    })
}

enum ConnectionIdentity {
    Enrolled(CredentialRecord, Option<String>),
    Builtin {
        provider: String,
        account: String,
        digest: Option<String>,
    },
}

impl Shared {
    pub(super) fn connection_verification_support(
        &self,
        provider: &str,
        record: Option<&CredentialRecord>,
    ) -> Value {
        let hosted = self.smtp_internal.is_some() || self.hosted_email.is_some();
        verification_support_json(supported_reason(
            provider,
            record,
            hosted,
            self.platform_custody.tag(),
        ))
    }

    fn resolve_builtin(&self, id: &str) -> Result<Option<(String, String, Option<String>)>> {
        let workspace = self.store.connection_workspace_id_strict()?;
        let mut candidates: Vec<(String, String)> = Vec::new();
        if !self.platforms.contains_key("local") {
            candidates.push(("local".to_string(), "local".to_string()));
        }
        for (provider, adapter) in &self.platforms {
            if let Some(descriptor) = adapter.connection_descriptor() {
                if descriptor.validate(adapter.table()).is_ok() {
                    for account in descriptor.builtin_accounts {
                        if provider != "local" || account != "local" {
                            candidates.push((provider.clone(), account.clone()));
                        }
                    }
                }
            }
        }
        for (provider, account) in candidates {
            let candidate = format!(
                "builtin-{}",
                uuid::Uuid::new_v5(
                    &uuid::Uuid::NAMESPACE_OID,
                    format!("{}:{provider}:{account}", workspace).as_bytes()
                )
                .simple()
            );
            if candidate == id {
                let digest = registration_digest_for(self, &provider);
                return Ok(Some((provider, account, digest)));
            }
        }
        Ok(None)
    }

    fn resolve_identity(&self, id: &str) -> Result<Option<ConnectionIdentity>> {
        if let Some(record) = self.store.connection_credential_strict(id)? {
            let digest = registration_digest_for(self, &record.platform);
            return Ok(Some(ConnectionIdentity::Enrolled(record, digest)));
        }
        if let Some((provider, account, digest)) = self.resolve_builtin(id)? {
            return Ok(Some(ConnectionIdentity::Builtin {
                provider,
                account,
                digest,
            }));
        }
        Ok(None)
    }

    pub(super) fn connection_test(self: &Arc<Self>, params: &Value) -> Result<Value> {
        let id = connection_id(params)?;
        let expected_revision = expected_revision(params)?;
        let expected_digest = expected_registration_digest(params)?;

        let deadline = crate::platform::OpDeadline::in_seconds(BUDGET.as_secs());
        let started_at = crate::issue::time::now_epoch();

        if self.connection_test_fenced.load(Ordering::SeqCst) {
            return Err(Error::busy(
                "connection test is fenced pending credential cleanup",
            ));
        }

        // The guard is acquired and held by the worker thread for the
        // whole operation: if a cleanup kill-path cannot observe the
        // reap inside the budget, the SAME thread keeps holding the
        // lock (with the owned Child) and polls `try_wait` until exit
        // is observed — the guard never crosses a thread boundary
        // (`MutexGuard` is `!Send`) and is never released early, so
        // no queued custody user can interleave while the child is
        // unobserved. The result (including the busy/fenced answer)
        // is sent back over the channel as soon as the verdict is
        // known; the worker only outlives the request while it still
        // holds the serialization for cleanup.
        let shared = Arc::clone(self);
        let id = id.to_string();
        let (result_tx, result_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let custody = match shared.platform_custody_lock.try_lock() {
                Ok(guard) => guard,
                Err(_) => {
                    let _ = result_tx.send(Err(Error::busy("connection test is busy")));
                    return;
                }
            };
            let mut pending = None;
            let result = shared.connection_test_locked(
                &id,
                expected_revision,
                expected_digest,
                &deadline,
                started_at,
                &mut pending,
            );
            let _ = result_tx.send(result);
            if let Some(mut child) = pending {
                let _custody = custody;
                loop {
                    match child.try_wait() {
                        Ok(Some(_)) => break,
                        Ok(None) => {
                            std::thread::sleep(std::time::Duration::from_millis(25));
                        }
                        Err(_) => break,
                    }
                }
            }
        });
        result_rx
            .recv()
            .unwrap_or_else(|_| Err(Error::internal("connection test worker exited")))
    }

    fn connection_test_locked(
        &self,
        id: &str,
        expected_revision: Option<u64>,
        expected_digest: Option<String>,
        deadline: &crate::platform::OpDeadline,
        started_at: i64,
        pending_child: &mut Option<std::process::Child>,
    ) -> Result<Value> {
        let Some(identity) = self.resolve_identity(id)? else {
            return Err(Error::rejected("connection is unavailable or stale"));
        };

        match identity {
            ConnectionIdentity::Builtin {
                provider,
                account,
                digest,
            } => {
                let mut receipt = VerificationReceipt::new(id, started_at);
                receipt.revision = None;
                receipt.registration_digest = digest.clone();
                if expected_revision.is_some() || expected_digest != digest {
                    return Ok(receipt
                        .failed(Status::Stale, FailureCode::StaleConnection, Step::Admission)
                        .value());
                }
                let hosted = self.smtp_internal.is_some() || self.hosted_email.is_some();
                let reason = supported_reason(&provider, None, hosted, self.platform_custody.tag());
                let code = reason.unwrap_or(FailureCode::UnsupportedProvider);
                let _ = account;
                Ok(receipt
                    .failed(Status::Unsupported, code, Step::Admission)
                    .value())
            }
            ConnectionIdentity::Enrolled(record, registration) => {
                let mut receipt = VerificationReceipt::new(id, started_at);
                receipt.revision = Some(record.credential_revision);
                receipt.registration_digest = registration.clone();

                if expected_revision != Some(record.credential_revision)
                    || expected_digest != registration
                {
                    return Ok(receipt
                        .failed(Status::Stale, FailureCode::StaleConnection, Step::Admission)
                        .value());
                }
                let hosted = self.smtp_internal.is_some() || self.hosted_email.is_some();
                if let Some(reason) = supported_reason(
                    &record.platform,
                    Some(&record),
                    hosted,
                    self.platform_custody.tag(),
                ) {
                    return Ok(receipt
                        .failed(Status::Unsupported, reason, Step::Admission)
                        .value());
                }
                if registration.is_none() {
                    return Ok(receipt
                        .failed(
                            Status::Unsupported,
                            FailureCode::UnsupportedProvider,
                            Step::Admission,
                        )
                        .value());
                }
                let manifest_ok = self.platforms.get(&record.platform).is_some_and(|adapter| {
                    adapter.table().manifest_version.as_deref()
                        == Some(platform::smtp::MANIFEST_PIN)
                        && adapter.reported_manifest_version().as_deref()
                            == Some(platform::smtp::MANIFEST_PIN)
                });
                if !manifest_ok || self.connection_descriptor(&record.platform).is_err() {
                    return Ok(receipt
                        .failed(
                            Status::Unsupported,
                            FailureCode::UnsupportedProvider,
                            Step::Admission,
                        )
                        .value());
                }

                if self
                    .connection_test_resolver
                    .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                    .is_err()
                {
                    return Ok(receipt
                        .failed(Status::Failed, FailureCode::Busy, Step::Admission)
                        .value());
                }
                let mut slot_owned = true;
                let bytes = match self.platform_custody.load_bounded(
                    &record.custody,
                    &crate::platform::Key {
                        platform: &record.platform,
                        account: &record.account,
                    },
                    MATERIAL_CAP,
                    deadline,
                    &self.connection_test_fenced,
                    pending_child,
                ) {
                    Ok(bytes) => bytes,
                    Err(e) => {
                        if self.connection_test_fenced.load(Ordering::SeqCst) {
                            release_slot(&self.connection_test_resolver, &mut slot_owned);
                            return Err(Error::busy(
                                "connection test is fenced pending credential cleanup",
                            ));
                        }
                        release_slot(&self.connection_test_resolver, &mut slot_owned);
                        return Ok(if e.kind() == "busy" {
                            receipt
                                .failed(Status::Failed, FailureCode::Timeout, Step::Configuration)
                                .value()
                        } else {
                            receipt
                                .failed(
                                    Status::Failed,
                                    FailureCode::CustodyUnavailable,
                                    Step::Configuration,
                                )
                                .value()
                        });
                    }
                };
                if crate::secret::fingerprint(&bytes) != record.fingerprint {
                    release_slot(&self.connection_test_resolver, &mut slot_owned);
                    return Ok(receipt
                        .failed(
                            Status::Failed,
                            FailureCode::CustodyUnavailable,
                            Step::Configuration,
                        )
                        .value());
                }
                let envelope = match crate::platform::smtp::custody_decode(&bytes) {
                    Ok((envelope, _)) => envelope,
                    Err(_) => {
                        release_slot(&self.connection_test_resolver, &mut slot_owned);
                        return Ok(receipt
                            .failed(
                                Status::Failed,
                                FailureCode::ConfigurationUnavailable,
                                Step::Configuration,
                            )
                            .value());
                    }
                };
                drop(bytes);
                if deadline.expired() {
                    release_slot(&self.connection_test_resolver, &mut slot_owned);
                    return Ok(receipt
                        .failed(Status::Failed, FailureCode::Timeout, Step::Configuration)
                        .value());
                }

                receipt.network_attempted = true;
                let addresses =
                    match self.resolve_login_host(&envelope.host, envelope.port, deadline) {
                        Ok(addresses) => addresses,
                        Err(failure) => {
                            return Ok(receipt
                                .failed(Status::Failed, failure.code, failure.step)
                                .value());
                        }
                    };
                slot_owned = false;
                if deadline.expired() {
                    return Ok(receipt
                        .failed(Status::Failed, FailureCode::Timeout, Step::Dns)
                        .value());
                }

                let result = crate::platform::smtp::verify_login(
                    &envelope,
                    &addresses,
                    self.smtp_test_ca.as_deref(),
                    deadline,
                );
                release_slot(&self.connection_test_resolver, &mut slot_owned);
                match result {
                    Ok(()) => {
                        if deadline.expired() {
                            return Ok(receipt
                                .failed(Status::Failed, FailureCode::Timeout, Step::Quit)
                                .value());
                        }
                        let registration_now = registration_digest_for(self, &record.platform);
                        receipt.registration_digest = registration_now;
                        receipt.status = Status::Success;
                        receipt.authentication_verified = true;
                        Ok(receipt.value())
                    }
                    Err(failure) => Ok(receipt
                        .failed(Status::Failed, failure.code, failure.step)
                        .value()),
                }
            }
        }
    }

    fn resolve_login_host(
        &self,
        host: &str,
        port: u16,
        deadline: &crate::platform::OpDeadline,
    ) -> std::result::Result<Vec<SocketAddr>, Failure> {
        let (tx, rx) = mpsc::channel();
        let slot = Arc::clone(&self.connection_test_resolver);
        let target = (host.to_string(), port);
        std::thread::spawn(move || {
            let answer = std::net::ToSocketAddrs::to_socket_addrs(&target)
                .map(|resolved| resolved.take(MAX_RESOLVED).collect::<Vec<_>>());
            let _ = tx.send(answer);
            slot.store(false, Ordering::SeqCst);
        });
        loop {
            let Some(left) = deadline.remaining() else {
                return Err(Failure {
                    code: FailureCode::Timeout,
                    step: Step::Dns,
                });
            };
            match rx.recv_timeout(left.min(RESOLVE_POLL)) {
                Ok(Ok(addresses)) if !addresses.is_empty() => return Ok(addresses),
                Ok(_) => {
                    return Err(Failure {
                        code: FailureCode::DnsFailed,
                        step: Step::Dns,
                    })
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if deadline.expired() {
                        return Err(Failure {
                            code: FailureCode::Timeout,
                            step: Step::Dns,
                        });
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(Failure {
                        code: FailureCode::DnsFailed,
                        step: Step::Dns,
                    });
                }
            }
        }
    }
}

fn release_slot(slot: &std::sync::atomic::AtomicBool, slot_owned: &mut bool) {
    if *slot_owned {
        *slot_owned = false;
        slot.store(false, Ordering::SeqCst);
    }
}
