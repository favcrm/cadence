//! CAD-1065: `connection_test` — the operator-only, stored-connection
//! SMTP login-without-send check. The full contract lives in the
//! CAD-1065 design supplement (`.tmp/cad1065-supplement-v2/`); this
//! module carries exactly it:
//!
//! * Authority first — the caller must already have passed
//!   `operator_connection` (this method runs behind `rpc_connection`'s
//!   prologue); nothing here reaches a Connection lookup, credential,
//!   DNS, socket or state write before that.
//! * Both `expected_revision` and `expected_registration_digest` are
//!   required keys; `null` is an exact compare, never a wildcard.
//! * One fixed 20-second monotonic budget, started after caller
//!   admission and before any custody or lookup work.
//! * The admission (`platform_custody_lock`) and store guards are
//!   try-locked: contention, poison or a fenced cleanup path is `busy`,
//!   never a wait, a retry or a forensic write.
//! * DNS runs in a single in-flight resolver slot — a worker that sees
//!   only `(host, port)`, never credential material; a timed-out
//!   resolution holds its slot until the syscall actually finishes.
//! * The receipt carries only non-secret metadata and is emitted by one
//!   dedicated serializer; the provider byte stream is never captured.

use std::net::{SocketAddr, ToSocketAddrs};
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

/// The published operation name every receipt and support row carries.
const OPERATION: &str = "smtp-login-no-send-v1";
/// The whole call's monotonic budget, from just after caller admission.
const BUDGET: Duration = Duration::from_secs(20);
/// Custody bytes are the canonical nine-field writer's document; at the
/// field caps (host 253, username 320, secret 1024, sender 254,
/// sender_name 80, port, tls_mode, provider, schema) worst-case JSON
/// escaping stays well under this — a bigger blob is torn, never read.
const MATERIAL_CAP: usize = 16 * 1024;
/// Resolver wait granularity while the in-flight slot's worker runs.
const RESOLVE_POLL: Duration = Duration::from_millis(25);

/// One `connection_test` result. Serialized once, by [`Self::value`];
/// only non-secret metadata may be a field — credential bytes, server
/// text and host material never are.
#[derive(Serialize)]
struct VerificationReceipt {
    schema: u32,
    operation: &'static str,
    connection_id: String,
    revision: Option<u64>,
    registration_digest: Option<String>,
    started_at: String,
    completed_at: String,
    status: &'static str,
    network_attempted: bool,
    authentication_verified: bool,
    email_sent: bool,
    delivery_verified: bool,
    sender_entitlement_verified: bool,
    execution_authority: bool,
    failure: Option<Failure>,
}

#[derive(Serialize)]
struct Failure {
    code: &'static str,
    step: &'static str,
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
            status: "failed",
            network_attempted: false,
            authentication_verified: false,
            email_sent: false,
            delivery_verified: false,
            sender_entitlement_verified: false,
            execution_authority: false,
            failure: None,
        }
    }

    /// Stamp the completion wall-clock and return `{"verification": ..}`.
    fn value(mut self) -> Value {
        self.completed_at = crate::issue::time::iso(crate::issue::time::now_epoch());
        json!({ "verification": self })
    }

    /// The classified failure: `status` + `{code, step}`.
    fn failed(mut self, status: &'static str, code: &'static str, step: &'static str) -> Self {
        self.status = status;
        self.failure = Some(Failure { code, step });
        self
    }
}

/// `expected_revision` — the key must be present; its value is `null`
/// or a positive integer. Anything else is malformed and refuses with
/// the fixed neutral text (the supplied value is never echoed).
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

/// `expected_registration_digest` — present; `null` or the canonical
/// `sha256:` + 64 lowercase-hex digest. Anything else refuses.
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

/// Whether a provider/record pair may ever answer `connection_test`:
/// only the self-hosted direct-SMTP shape — canonical `smtp` provider,
/// `smtp` exchange, `file`/`libsecret` custody — on a daemon that is
/// not hosted. The reason is the same closed vocabulary the receipt's
/// `failure.code` uses; `None` when supported.
fn verification_support(
    provider: &str,
    record: Option<&CredentialRecord>,
    hosted: bool,
    custody_tag: &'static str,
) -> Option<&'static str> {
    if hosted {
        return Some("unsupported_deployment");
    }
    if provider != platform::smtp::PLATFORM {
        return Some("unsupported_provider");
    }
    if let Some(record) = record {
        if record.exchange != platform::smtp::ENROLLMENT_SHAPE {
            return Some("unsupported_provider");
        }
        if !matches!(
            record.custody.as_str(),
            platform::custody::FILE_TAG | platform::custody::LIBSECRET_TAG
        ) || record.custody != custody_tag
        {
            return Some("unsupported_custody");
        }
    }
    None
}

/// The `verification_support` metadata a connection or provider row
/// carries: non-authorizing advertisement only — `supported` false or
/// the metadata absent never proves admission, and `supported` true
/// never grants it.
pub(super) fn verification_support_json(reason: Option<&'static str>) -> Value {
    json!({
        "operation": match reason {
            None => json!(OPERATION),
            Some(_) => Value::Null,
        },
        "supported": reason.is_none(),
        "reason_code": reason,
    })
}

impl Shared {
    /// The `verification_support` field for a connection row —
    /// per-record admission shape recomputed from live daemon state,
    /// informational and never authority.
    pub(super) fn connection_verification_support(
        &self,
        provider: &str,
        record: Option<&CredentialRecord>,
    ) -> Value {
        let hosted = self.smtp_internal.is_some() || self.hosted_email.is_some();
        verification_support_json(verification_support(
            provider,
            record,
            hosted,
            self.platform_custody.tag(),
        ))
    }

    /// The `connection_test` handler. Caller admission
    /// (`operator_connection`) and the parameter allowlist already ran
    /// in `rpc_connection`; this starts the fixed budget, takes the
    /// fail-fast admission and store guards, then either refuses with
    /// a fixed neutral envelope (before a trusted row exists) or
    /// returns a typed receipt built only from server-side state.
    pub(super) fn connection_test(&self, params: &Value) -> Result<Value> {
        let id = connection_id(params)?;
        let expected_revision = expected_revision(params)?;
        let expected_digest = expected_registration_digest(params)?;

        let deadline = crate::platform::OpDeadline::in_seconds(BUDGET.as_secs());
        let started_at = crate::issue::time::now_epoch();

        // The custody-critical-section guard serializes this op with
        // custody mutation and any sibling check; contention or poison
        // is fail-fast busy, never a wait or a poisoned-guard read.
        let _custody = self
            .platform_custody_lock
            .try_lock()
            .map_err(|_| Error::busy("connection test is busy"))?;
        // A previous check's libsecret child could not be provably
        // killed and reaped — the path stays closed while custody
        // bytes may still sit with an unobserved process.
        if self.connection_test_fenced.load(Ordering::SeqCst) {
            return Err(Error::busy(
                "connection test is fenced pending credential cleanup",
            ));
        }

        // The current Connection projection — the fail-fast read only.
        // A read that fails (contention, poison) keeps the fixed neutral
        // envelope; a clean "no such row" is a typed receipt — the
        // absent/incarnation case the caller must distinguish, not an
        // operator-facing error.
        let record = self
            .store
            .connection_credential_strict(id)
            .map_err(|_| Error::rejected("connection is unavailable or stale"))?;

        let hosted = self.smtp_internal.is_some() || self.hosted_email.is_some();
        let Some(record) = record else {
            // Missing/foreign/built-in: there is no enrolled row, so the
            // server-side values are both null. A caller expecting a live
            // revision or digest is stale; one matching the null pair is
            // unsupported (a hosted daemon is unsupported_deployment).
            let mut receipt = VerificationReceipt::new(id, started_at);
            if expected_revision.is_some() || expected_digest.is_some() {
                return Ok(receipt
                    .failed("stale", "stale_connection", "admission")
                    .value());
            }
            receipt.status = "unsupported";
            receipt.failure = Some(Failure {
                code: if hosted {
                    "unsupported_deployment"
                } else {
                    "unsupported_provider"
                },
                step: "admission",
            });
            return Ok(receipt.value());
        };

        let registration = self
            .platforms
            .get(&record.platform)
            .and_then(|adapter| adapter.connection_registration())
            .map(|registration| {
                crate::platform::connections::registration_digest(&format!(
                    "{registration}:{}",
                    serde_json::to_string(&self.connection_descriptor(&record.platform).ok())
                        .unwrap_or_default()
                ))
            });

        let mut receipt = VerificationReceipt::new(id, started_at);
        receipt.revision = Some(record.credential_revision);
        receipt.registration_digest = registration.clone();

        // The stale-vs-expected compare runs on the server snapshot
        // alone, before any credential, DNS or socket work.
        if expected_revision != Some(record.credential_revision) || expected_digest != registration
        {
            return Ok(receipt
                .failed("stale", "stale_connection", "admission")
                .value());
        }
        if let Some(reason) = verification_support(
            &record.platform,
            Some(&record),
            hosted,
            self.platform_custody.tag(),
        ) {
            return Ok(receipt.failed("unsupported", reason, "admission").value());
        }

        // Secret access: reserve the operation's single resolver slot
        // before key access, then read custody through the bounded
        // deadline-aware load — never the unbounded `load` path.
        if self
            .connection_test_resolver
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Ok(receipt.failed("failed", "busy", "admission").value());
        }
        // From here the slot is owned by this call. If the DNS worker
        // spawns it takes the slot over — `release_slot` tracks which
        // of the two still holds it.
        let mut slot_owned = true;

        let bytes = match self.platform_custody.load_bounded(
            &record.custody,
            &crate::platform::Key {
                platform: &record.platform,
                account: &record.account,
            },
            MATERIAL_CAP,
            &deadline,
            &self.connection_test_fenced,
        ) {
            Ok(bytes) => bytes,
            Err(e) => {
                release_slot(&self.connection_test_resolver, &mut slot_owned);
                return Ok(if e.kind() == "busy" {
                    receipt.failed("failed", "timeout", "configuration").value()
                } else {
                    receipt
                        .failed("failed", "custody_unavailable", "configuration")
                        .value()
                });
            }
        };
        if crate::secret::fingerprint(&bytes) != record.fingerprint {
            release_slot(&self.connection_test_resolver, &mut slot_owned);
            return Ok(receipt
                .failed("failed", "custody_unavailable", "configuration")
                .value());
        }
        let envelope = match crate::platform::smtp::custody_decode(&bytes) {
            Ok((envelope, _)) => envelope,
            Err(_) => {
                release_slot(&self.connection_test_resolver, &mut slot_owned);
                return Ok(receipt
                    .failed("failed", "configuration_unavailable", "configuration")
                    .value());
            }
        };
        drop(bytes);

        // DNS: the in-flight resolver sees only `(host, port)` — never
        // credential material, never a socket — and answers or holds
        // its slot until the syscall actually ends. `network_attempted`
        // marks the moment the worker spawns, not the reservation.
        receipt.network_attempted = true;
        let addresses = match self.resolve_login_host(&envelope.host, envelope.port, &deadline) {
            Ok(addresses) => addresses,
            Err(failure) => {
                return Ok(receipt.failed("failed", failure.code, failure.step).value());
            }
        };
        // The spawned worker owns the slot now and frees the flag when
        // `to_socket_addrs` actually returns — this call must never
        // free it again, even on early failure returns below.
        slot_owned = false;
        let result = crate::platform::smtp::verify_login(
            &envelope,
            &addresses,
            self.smtp_test_ca.as_deref(),
            &deadline,
        );
        release_slot(&self.connection_test_resolver, &mut slot_owned);
        match result {
            Ok(()) => {
                receipt.status = "success";
                receipt.authentication_verified = true;
                Ok(receipt.value())
            }
            Err(failure) => Ok(receipt.failed("failed", failure.code, failure.step).value()),
        }
    }

    /// Resolve `host:port` on the operation's resolver slot (already
    /// reserved by the caller): spawn one short-lived worker carrying
    /// only the destination pair, then wait the remaining budget for
    /// its answer. The worker frees the slot when `to_socket_addrs`
    /// actually returns — including when this caller has long since
    /// classified `timeout` and dropped everything.
    ///
    /// On any return the spawned worker owns the slot; the caller
    /// must not free it. `Err` carries the classified `dns` failure.
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
            let answer = target
                .to_socket_addrs()
                .map(|resolved| resolved.collect::<Vec<_>>());
            let _ = tx.send(answer);
            slot.store(false, Ordering::SeqCst);
        });
        loop {
            let Some(left) = deadline.remaining() else {
                return Err(Failure {
                    code: "timeout",
                    step: "dns",
                });
            };
            match rx.recv_timeout(left.min(RESOLVE_POLL)) {
                Ok(Ok(addresses)) if !addresses.is_empty() => return Ok(addresses),
                Ok(_) => {
                    return Err(Failure {
                        code: "dns_failed",
                        step: "dns",
                    })
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if deadline.expired() {
                        return Err(Failure {
                            code: "timeout",
                            step: "dns",
                        });
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(Failure {
                        code: "dns_failed",
                        step: "dns",
                    });
                }
            }
        }
    }
}

/// Free the resolver slot only when no spawned worker still owns it:
/// after `resolve_login_host` runs, `slot_owned` is always false —
/// the worker's own completion clears the flag.
fn release_slot(slot: &std::sync::atomic::AtomicBool, slot_owned: &mut bool) {
    if *slot_owned {
        *slot_owned = false;
        slot.store(false, Ordering::SeqCst);
    }
}
