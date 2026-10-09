//! CAD-798: production AgenticOS publish transport client.
//!
//! The daemon-observed [`PublishSender`](super::publish::PublishSender)
//! speaking the versioned device publish door as LANDED in
//! `agenticos-stack/agenticos-v2` (AOS-94 device-publish v1, revalidated
//! 2026-09-29 against the `agenticos-v2` staging worktree carrying PR
//! #214): bearer-authenticated `POST /v1/runtime/connectors/publish/preflight`,
//! `POST /v1/runtime/connectors/publish` and
//! `GET /v1/runtime/connectors/publish/{key}/status`.
//!
//! Conformance rules (pinned, do not drift without revalidation):
//! - every response document must carry wire version exactly `"1"`;
//!   drift refuses loudly instead of degrading to `processing`;
//! - workspace binding comes from the device credential, never from a
//!   wire field: request bodies carry no workspace id, and a response
//!   echoing a different key, destination or digest fails closed;
//! - the credential needs the `publish.send` scope; it is loaded from a
//!   `0600` file at registration, never logged, never tested with real
//!   material;
//! - registration is explicit config and default-off: without both
//!   `CADENCE_PUBLISH_SEND_URL` and
//!   `CADENCE_PUBLISH_SEND_CREDENTIAL_FILE` no sender is registered and
//!   dispatch stays `processing` exactly as before. One-sided or
//!   malformed config refuses daemon startup loudly rather than
//!   silently running without a sender;
//! - exact-binding preflight precedes the first POST: a definitive
//!   staging refusal (or an unknown decision — AOS-94 permits only
//!   `approved`) means zero sends. Ambiguous preflight is retried
//!   pre-send (staging never mutates, never sends); persistent ambiguity
//!   fails loudly with a nothing-sent refusal so the operator retries
//!   staging under a fresh key. It never returns `processing`, which
//!   could only status-reconcile to 404 forever for a key the door
//!   never saw;
//! - no-duplicate-call recovery: an ambiguous transport outcome (timeout,
//!   reset, 5xx with or without a JSON error document, non-JSON) after
//!   `execute` returns a `processing` outcome with no evidence — never a
//!   retry of the POST — so restart reconcile through `status` recovers
//!   without a second provider call. A drifted or malformed execution
//!   verdict is likewise ambiguity until status proof; only 4xx door
//!   error documents are definitive refusals.
//!   `status` itself is a read and stays retryable: its transport
//!   failures are refusals, which keep the intent `processing`;
//! - a `posted` execution is enriched with a best-effort `status` fetch
//!   so daemon-persisted evidence carries the byte-exact provider ids
//!   and payload the door recorded. Enrichment never downgrades a
//!   posted verdict and never fills evidence from the request — only
//!   from the door's own status document (CAD-771 strict-upstream-echo).
//!
//! Door error-code mapping (fail-closed; the door code is always kept in
//! the refusal detail): `digest_mismatch` → `key_conflict`,
//! `destination_mismatch`/`content_mismatch` → `grant_binding_mismatch`,
//! `workspace_mismatch` → `cross_workspace`, `grant_required` →
//! `grant_mismatch`, `not_ready`/`disabled` → `not_publishable`,
//! `send_disabled` → `send_disabled`, status `not_found` → `unknown_key`.
//! Grant lifecycle codes (`grant_revoked`, `grant_exhausted`, …) pass
//! through the shared vocabulary; anything unrecognized keeps the door
//! code verbatim rather than inventing vocabulary.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use super::publish::{LedgerOutcome, PublishState, Refusal, SendBinding};
use crate::error::{Error, Result};

/// Wire version every v1 door document carries.
pub const DEVICE_PUBLISH_VERSION: &str = "1";

// Route tails, joined to the credential's door prefix (see `route`).
const PREFLIGHT_PATH: &str = "/publish/preflight";
const EXEC_PATH: &str = "/publish";
const STATUS_PATH: &str = "/publish";
const GRANTS_PATH: &str = "/publish/grants";
const DEVICE_PREFIX: &str = "/v1/runtime/connectors";
/// CAD-1267 (AOS-181): the lease door serves the same bodies and envelope
/// under this prefix, with no Authorization header.
const HOSTED_PREFIX: &str = "/v1/runtime/connectors/hosted-publish";
/// The one origin the hosted lease credential may reach: the same fixed
/// `api.internal` the hosted media admission already pins.
const HOSTED_ORIGIN: &str = "http://api.internal";
const RESPONSE_CAP: u64 = 1024 * 1024;

/// Explicit-config surface. Both must be set; neither alone registers.
pub const PUBLISH_SEND_URL_ENV: &str = "CADENCE_PUBLISH_SEND_URL";
pub const PUBLISH_SEND_CREDENTIAL_FILE_ENV: &str = "CADENCE_PUBLISH_SEND_CREDENTIAL_FILE";
/// CAD-979 v9: the `provider.read`/`provider.draft` read credential for the
/// destinations GET (local→AOS `connectionId` map). A separate credential —
/// upstream `oneScopeAudience` forbids `provider.*` + `publish.send` on one
/// token. The send credential's workspace remains the authority; this read
/// only maps ids.
pub const PUBLISH_READ_URL_ENV: &str = "CADENCE_PUBLISH_READ_URL";
pub const PUBLISH_READ_CREDENTIAL_FILE_ENV: &str = "CADENCE_PUBLISH_READ_CREDENTIAL_FILE";

/// Global HTTP bound for door calls. An execution that exceeds it is
/// ambiguous (the provider may still have accepted), so the caller
/// reconciles through `status` instead of retrying the POST.
const DOOR_TIMEOUT: Duration = Duration::from_secs(30);

/// Preflight staging attempts before an uncertain verdict fails the
/// dispatch. Staging never mutates the ledger and never calls the
/// provider, so retrying preflight is always safe — unlike retrying
/// the execution POST.
const PREFLIGHT_ATTEMPTS: u32 = 2;

/// The exact approved material one dispatch presents: the reviewed
/// caption text plus the frozen media key (absent for text-only posts).
#[derive(Debug, Clone)]
pub struct SendMaterial {
    pub caption: String,
    pub media_key: Option<String>,
}

/// Resolves the exact approved material for one frozen binding. The
/// production resolver re-proves it from the daemon store (artifact body
/// plus frozen media key); tests inject synthetic material directly, so
/// no credential and no live door ever appear in tests.
pub type MaterialResolver =
    Arc<dyn Fn(&SendBinding) -> std::result::Result<SendMaterial, Refusal> + Send + Sync>;

/// Device credential with redacted debug: the bearer secret never
/// appears in logs, panic messages or test output.
#[derive(Clone)]
pub struct DeviceCredential(Option<String>);

impl DeviceCredential {
    pub fn new(secret: String) -> Self {
        Self(Some(secret))
    }

    /// CAD-1267: the hosted lease door authenticates by network position
    /// (`api.internal`), so there is no bearer to send. Constructible only
    /// from a [`HostedMediaAdmission`](crate::platform::deployments::HostedMediaAdmission).
    pub(crate) fn hosted_lease(
        _admission: &crate::platform::deployments::HostedMediaAdmission,
    ) -> Self {
        Self(None)
    }

    /// The origin check shared by the door clients. A bearer credential
    /// keeps the existing rule (https, or loopback http). The bearerless
    /// lease credential is admitted only for the fixed image-owned
    /// `api.internal` origin and nothing else.
    pub(crate) fn door_base(&self, base: &str) -> Result<String> {
        if self.0.is_none() {
            if base.trim_end_matches('/') == HOSTED_ORIGIN {
                return Ok(HOSTED_ORIGIN.to_owned());
            }
            return Err(Error::rejected(
                "hosted publish lease is bound to the fixed api.internal origin",
            ));
        }
        super::valid_base(base)
    }

    /// Full door path for a route tail: the device door, or the hosted
    /// lease door prefix for the bearerless credential.
    pub(crate) fn route(&self, tail: &str) -> String {
        let prefix = if self.0.is_none() {
            HOSTED_PREFIX
        } else {
            DEVICE_PREFIX
        };
        format!("{prefix}{tail}")
    }

    /// Bearer header value; `pub(crate)` so the sibling media-import
    /// client authenticates the same way. Never logged or returned to a
    /// caller — the secret only ever reaches this one header.
    pub(crate) fn authorization(&self) -> Option<String> {
        self.0.as_ref().map(|secret| format!("Bearer {secret}"))
    }
}

impl std::fmt::Debug for DeviceCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DeviceCredential(redacted)")
    }
}

/// Production HTTP sender implementing the CAD-771 dispatch trait
/// against the landed device door.
pub struct HttpPublishSender {
    base: String,
    credential: DeviceCredential,
    http: ureq::Agent,
    material: MaterialResolver,
}

impl HttpPublishSender {
    pub fn new(
        base: &str,
        credential: DeviceCredential,
        material: MaterialResolver,
    ) -> Result<Self> {
        Self::with_timeout(base, credential, material, DOOR_TIMEOUT)
    }

    /// Constructor with an explicit HTTP bound. Production uses
    /// [`DOOR_TIMEOUT`]; tests pin a short bound to prove
    /// timeout-ambiguity reconciles without a second provider call.
    pub fn with_timeout(
        base: &str,
        credential: DeviceCredential,
        material: MaterialResolver,
        timeout: Duration,
    ) -> Result<Self> {
        let base = credential.door_base(base)?;
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(timeout))
            .http_status_as_error(false)
            .max_redirects(0)
            .build();
        Ok(Self {
            base,
            credential,
            http: ureq::Agent::new_with_config(config),
            material,
        })
    }

    /// Stage one exact binding without claiming execution or calling the
    /// provider. Pure staging: transport ambiguity refuses (the caller
    /// retries preflight, which can never duplicate a send).
    pub fn preflight(
        &self,
        binding: &SendBinding,
        material: &SendMaterial,
    ) -> std::result::Result<bool, Refusal> {
        match self.preflight_inner(binding, material) {
            Ok(repeated) => Ok(repeated),
            Err(Fault::Refused(refusal)) => Err(refusal),
            Err(Fault::Ambiguous) => Err(Refusal::new(
                "refused",
                "publish preflight is uncertain; retry preflight, never execution",
            )),
        }
    }

    fn preflight_inner(
        &self,
        binding: &SendBinding,
        material: &SendMaterial,
    ) -> std::result::Result<bool, Fault> {
        binding.validate().map_err(Fault::Refused)?;
        check_material(binding, material).map_err(Fault::Refused)?;
        let body = send_body(binding, material);
        self.post(&self.credential.route(PREFLIGHT_PATH), &body)
            .and_then(|data| preflight_of(binding, &data).map_err(Fault::Refused))
    }

    fn execute_request(
        &self,
        binding: &SendBinding,
        material: &SendMaterial,
    ) -> std::result::Result<LedgerOutcome, Refusal> {
        binding.validate()?;
        check_material(binding, material)?;
        // Exact-binding preflight precedes the first POST: a definitive
        // staging refusal (or an unknown decision) means zero sends — no
        // POST leaves this client. Ambiguous preflight is retried
        // pre-send (staging never mutates, never sends); persistent
        // ambiguity fails loudly with a nothing-sent refusal so the
        // operator retries staging under a fresh key. It never returns
        // processing, which could only status-reconcile to 404 forever
        // for a key the door never saw.
        let mut attempts: u32 = 0;
        loop {
            match self.preflight_inner(binding, material) {
                Ok(_) => break,
                Err(Fault::Refused(refusal)) => return Err(refusal),
                Err(Fault::Ambiguous) => {
                    attempts += 1;
                    if attempts >= PREFLIGHT_ATTEMPTS {
                        // `nothing_sent`: the POST provably never left —
                        // the caller holds the row (re-stagable), never
                        // burns it refused on a door blip.
                        return Err(Refusal::new(
                            "nothing_sent",
                            "publish preflight is uncertain after retry; nothing was sent — \
                             retry staging under a fresh key, never execution",
                        ));
                    }
                }
            }
        }
        let body = send_body(binding, material);
        match self.post(&self.credential.route(EXEC_PATH), &body) {
            Ok(data) => match execution_of(binding, &data) {
                Ok(mut outcome) => {
                    if outcome.state == PublishState::Posted {
                        // Best-effort enrichment: attach the door's own
                        // byte-exact provider evidence. A failed or
                        // lagging status fetch never downgrades the
                        // posted verdict; reconcile fills the evidence
                        // later.
                        if let Ok(status) = self.status_request(&binding.key) {
                            if status.state == PublishState::Posted {
                                outcome.permalink =
                                    status.permalink.or_else(|| outcome.permalink.clone());
                                if !status.provider_ids.is_empty() {
                                    outcome.provider_ids = status.provider_ids;
                                }
                                if status.provider_payload.is_some() {
                                    outcome.provider_payload = status.provider_payload;
                                }
                            }
                        }
                    }
                    Ok(outcome)
                }
                Err(Fault::Refused(refusal)) => Err(refusal),
                Err(Fault::Ambiguous) => Ok(processing_outcome(binding)),
            },
            Err(Fault::Refused(refusal)) => Err(refusal),
            // Timeout, reset, 5xx or malformed envelope after the POST:
            // the provider may have accepted, so this is `processing`
            // with no evidence — never a retried POST. Restart
            // reconcile through `status` recovers the row.
            Err(Fault::Ambiguous) => Ok(processing_outcome(binding)),
        }
    }

    fn status_request(&self, key: &str) -> std::result::Result<LedgerOutcome, Refusal> {
        if !super::publish::valid_idempotency_key(key) {
            return Err(Refusal::new("bad_key", "status key shape is invalid"));
        }
        let url = format!(
            "{}{}/{key}/status",
            self.base,
            self.credential.route(STATUS_PATH)
        );
        let mut request = self.http.get(&url);
        if let Some(authorization) = self.credential.authorization() {
            request = request.header("authorization", &authorization);
        }
        let envelope = request.call().map_err(|_| {
            Refusal::new(
                "refused",
                "publish status is uncertain; the intent stays processing",
            )
        })?;
        match read_envelope(envelope, true) {
            Ok(data) => status_of(key, &data),
            Err(Fault::Refused(refusal)) => Err(refusal),
            Err(Fault::Ambiguous) => Err(Refusal::new(
                "refused",
                "publish status is uncertain; the intent stays processing",
            )),
        }
    }

    fn post(&self, path: &str, body: &Value) -> std::result::Result<Value, Fault> {
        assert_no_workspace(body);
        let mut request = self.http.post(format!("{}{path}", self.base));
        if let Some(authorization) = self.credential.authorization() {
            request = request.header("authorization", &authorization);
        }
        let envelope = request
            .send_json(body.clone())
            .map_err(|_| Fault::Ambiguous)?;
        read_envelope(envelope, false)
    }
}

impl super::publish::PublishSender for HttpPublishSender {
    fn execute(&self, binding: &SendBinding) -> std::result::Result<LedgerOutcome, Refusal> {
        let material = (self.material)(binding)?;
        self.execute_request(binding, &material)
    }

    fn status(&self, key: &str) -> std::result::Result<LedgerOutcome, Refusal> {
        self.status_request(key)
    }

    /// CAD-1291: read-only lookup of the owner's standing grants for one
    /// destination. The door scopes it to this company; Cadence selects the
    /// live one for the approved account and AgenticOS re-checks company,
    /// destination, revocation and the daily cap at preflight and send.
    fn find_grant(
        &self,
        destination_id: &str,
    ) -> std::result::Result<Vec<super::publish::FoundGrant>, Refusal> {
        let uncertain = || {
            Refusal::new(
                "refused",
                "grant lookup is uncertain; nothing was sent, try again",
            )
        };
        if destination_id.is_empty()
            || destination_id.len() > 120
            || !destination_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
        {
            return Err(Refusal::new(
                "bad_destination",
                "destination id shape is invalid",
            ));
        }
        let url = format!(
            "{}{}?destinationId={destination_id}",
            self.base,
            self.credential.route(GRANTS_PATH)
        );
        let mut request = self.http.get(&url);
        if let Some(authorization) = self.credential.authorization() {
            request = request.header("authorization", &authorization);
        }
        let response = request.call().map_err(|_| uncertain())?;
        let data = match read_envelope(response, false) {
            Ok(data) => data,
            Err(Fault::Refused(refusal)) => return Err(refusal),
            Err(Fault::Ambiguous) => return Err(uncertain()),
        };
        data.get("grants")
            .and_then(Value::as_array)
            .and_then(|grants| {
                grants
                    .iter()
                    .map(super::publish::FoundGrant::from_wire)
                    .collect::<Option<Vec<_>>>()
            })
            .ok_or_else(uncertain)
    }

    /// CAD-1041: pre-claim staging for an explicit send-now. Resolves
    /// the exact approved material the same way `execute` does (store
    /// re-proof), then stages once. Ambiguity maps to
    /// `Preflight::Uncertain` — the caller leaves the row queued and
    /// tells the operator to retry; a definitive door refusal maps to
    /// `Preflight::Refused`, which the caller claims and reports.
    /// Staging never sends, so this probe is always safe to repeat.
    fn preflight(&self, binding: &SendBinding) -> super::publish::Preflight {
        use super::publish::Preflight;
        let material = match (self.material)(binding) {
            Ok(material) => material,
            // A transient material-read failure (store unavailable /
            // busy) is Uncertain — the caller tells the operator to
            // retry, never burns the row on a blip.
            Err(refusal) if refusal.code == "store_unavailable" => {
                return Preflight::Uncertain(refusal);
            }
            Err(refusal) => return Preflight::Refused(refusal),
        };
        match self.preflight_inner(binding, &material) {
            Ok(_) => Preflight::Approved,
            Err(Fault::Refused(refusal)) => Preflight::Refused(refusal),
            Err(Fault::Ambiguous) => Preflight::Uncertain(Refusal::new(
                "refused",
                "publish preflight is uncertain; row stays queued",
            )),
        }
    }
}

/// Transport outcome of one door call: either a parsed refusal or an
/// ambiguous failure (timeout, reset, 5xx, non-JSON, envelope drift).
enum Fault {
    Refused(Refusal),
    Ambiguous,
}

/// Read one door envelope: `{ok:true,data}` or `{ok:false,error}`.
/// Anything else (non-JSON, over-cap, wrong shape, error HTTP without a
/// door envelope) is ambiguous — never a refusal, never evidence.
///
/// HTTP 5xx is always ambiguous even with a door error document: after
/// an accepted POST the provider may still have posted, so a 503 JSON
/// error is `processing` (reconcile through status), never a definitive
/// refusal. Definitive refusals come from 4xx door documents only.
fn read_envelope(
    mut response: ureq::http::Response<ureq::Body>,
    is_status: bool,
) -> std::result::Result<Value, Fault> {
    let status = response.status().as_u16();
    if (500..=599).contains(&status) {
        return Err(Fault::Ambiguous);
    }
    let bytes = response
        .body_mut()
        .with_config()
        .limit(RESPONSE_CAP)
        .read_to_vec()
        .map_err(|_| Fault::Ambiguous)?;
    let envelope: Value = serde_json::from_slice(&bytes).map_err(|_| Fault::Ambiguous)?;
    if envelope.get("ok") == Some(&Value::Bool(true)) {
        return Ok(envelope.get("data").cloned().unwrap_or(Value::Null));
    }
    if let Some(error) = envelope.get("error") {
        let code = error.get("code").and_then(Value::as_str).unwrap_or("");
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("the door refused without detail");
        return Err(Fault::Refused(door_refusal(
            status, code, message, is_status,
        )));
    }
    Err(Fault::Ambiguous)
}

/// Map one door error document to the shared refusal vocabulary. The
/// door code is always preserved in the detail; codes outside the
/// shared vocabulary keep the door code verbatim as the refusal code
/// rather than inventing vocabulary.
fn door_refusal(status: u16, code: &str, message: &str, is_status: bool) -> Refusal {
    let _ = status;
    let detail = format!("door {code}: {message}");
    let mapped: Option<&str> = match code {
        "digest_mismatch" => Some("key_conflict"),
        "destination_mismatch" | "content_mismatch" => Some("grant_binding_mismatch"),
        "workspace_mismatch" => Some("cross_workspace"),
        "grant_required" => Some("grant_mismatch"),
        "not_ready" | "disabled" => Some("not_publishable"),
        "send_disabled" => Some("send_disabled"),
        "not_found" if is_status => Some("unknown_key"),
        "not_found" => Some("wrong_connection"),
        _ => None,
    };
    match mapped {
        Some(vocab) => Refusal::new(vocab, detail),
        // Shared-vocabulary codes pass through; anything else keeps
        // the door code verbatim so operators see the exact signal.
        None if Refusal::code_for(code) != "refused" => {
            Refusal::new(Refusal::code_for(code), detail)
        }
        None => Refusal::new(code, detail),
    }
}

/// The exact wire body for preflight and execution: caption text plus
/// grant presentation. No workspace field exists — binding comes from
/// the bearer credential. Debug builds assert that invariant.
fn send_body(binding: &SendBinding, material: &SendMaterial) -> Value {
    let mut body = json!({
        "key": binding.key,
        "connectionId": binding.connection_id,
        "caption": material.caption,
        "cadenceEffectId": binding.cadence_effect_id,
        "grant": {
            "id": binding.grant_id,
            "connectionId": binding.connection_id,
            "destinationId": binding.destination_id,
            "captionDigest": binding.caption_digest,
            "imageDigest": binding.image_digest,
        },
    });
    match &binding.source {
        super::publish::PublicationSource::Run { run_id } => body["cadenceRunId"] = json!(run_id),
        super::publish::PublicationSource::SocialDraft { draft_id, revision } => {
            body["cadenceDraftId"] = json!(draft_id);
            body["draftRevision"] = json!(revision);
        }
    }
    if let Some(media_key) = &material.media_key {
        body["mediaKey"] = Value::String(media_key.clone());
    }
    body
}

fn assert_no_workspace(body: &Value) {
    debug_assert!(
        body.get("workspaceId").is_none(),
        "wire body names a workspace"
    );
    debug_assert!(
        body.get("workspace_id").is_none(),
        "wire body names a workspace"
    );
    if let Some(grant) = body.get("grant") {
        debug_assert!(
            grant.get("workspaceId").is_none(),
            "grant presentation names a workspace"
        );
    }
}

/// The resolved material must reproduce the frozen binding exactly:
/// caption digest over UTF-8 bytes plus the frozen media key binding.
/// Anything else is a changed approval, failed closed before any door
/// call — no provider call ever leaves this client for a mismatched
/// binding.
fn check_material(
    binding: &SendBinding,
    material: &SendMaterial,
) -> std::result::Result<(), Refusal> {
    if !super::publish::valid_caption(&material.caption) {
        return Err(Refusal::new(
            "bad_caption_digest",
            "resolved caption violates the caption bound",
        ));
    }
    if super::publish::caption_digest_of(&material.caption) != binding.caption_digest {
        return Err(Refusal::new(
            "grant_binding_mismatch",
            "resolved caption differs from the frozen approval",
        ));
    }
    match (&material.media_key, &binding.image_digest) {
        (None, None) => Ok(()),
        (Some(key), Some(digest)) => {
            let parts: Vec<&str> = key.split('.').collect();
            let binds = parts.len() == 4
                && parts[0] == "dp1"
                && parts[2] == binding.connection_id
                && parts[3].len() == 32
                && digest.len() == 64
                && digest.starts_with(parts[3]);
            if binds {
                Ok(())
            } else {
                Err(Refusal::new(
                    "grant_binding_mismatch",
                    "resolved media key differs from the frozen approval",
                ))
            }
        }
        _ => Err(Refusal::new(
            "grant_binding_mismatch",
            "resolved media key differs from the frozen approval",
        )),
    }
}

/// Ambiguous execution collapses to `processing` with no evidence: the
/// provider may have accepted, so reconcile — never a retried POST —
/// recovers the row.
fn processing_outcome(binding: &SendBinding) -> LedgerOutcome {
    LedgerOutcome {
        state: PublishState::Processing,
        permalink: None,
        destination_id: binding.destination_id.clone(),
        caption_digest: binding.caption_digest.clone(),
        image_digest: binding.image_digest.clone(),
        provider_payload: None,
        provider_ids: vec![],
        repeated: false,
    }
}

/// Validate one preflight document against the binding that staged it.
/// AOS-94 permits exactly one staging decision here: `approved` (with
/// `executable`). Anything else — declined, pending, granted, unknown —
/// refuses staging with zero sends.
fn preflight_of(binding: &SendBinding, data: &Value) -> std::result::Result<bool, Refusal> {
    check_echo(binding, data, data.get("key").and_then(Value::as_str))
        .map_err(Fault::into_refusal)?;
    if data.get("decision").and_then(Value::as_str) != Some("approved") {
        return Err(Refusal::new(
            "refused",
            "the door did not approve preflight",
        ));
    }
    if data.get("executable").and_then(Value::as_bool) != Some(true) {
        let reason = data
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or("the door reports this binding is not executable");
        return Err(Refusal::new("refused", reason));
    }
    Ok(data
        .get("repeated")
        .and_then(Value::as_bool)
        .unwrap_or(false))
}

/// Validate one execution document against the binding it executed. A
/// malformed provider verdict — version drift, echo forgery, unknown
/// status string — is ambiguity (the send may have posted), never a
/// refusal; status proof decides. Only 4xx door error documents refuse.
fn execution_of(binding: &SendBinding, data: &Value) -> std::result::Result<LedgerOutcome, Fault> {
    if data.get("version").and_then(Value::as_str) != Some(DEVICE_PUBLISH_VERSION) {
        // Contract drift after an accepted POST: the provider may have
        // posted under our key, so this stays ambiguity (processing +
        // reconcile) rather than a terminal refusal. The status path
        // still refuses drift loudly on reads.
        return Err(Fault::Ambiguous);
    }
    let result = &data["result"];
    check_echo(binding, data, result.get("key").and_then(Value::as_str))?;
    let state = match result.get("status").and_then(Value::as_str) {
        Some("posted") => PublishState::Posted,
        Some("processing") => PublishState::Processing,
        Some("failed") => PublishState::Refused,
        _ => return Err(Fault::Ambiguous),
    };
    Ok(LedgerOutcome {
        state,
        permalink: result
            .get("permalink")
            .and_then(Value::as_str)
            .map(str::to_owned),
        destination_id: binding.destination_id.clone(),
        caption_digest: binding.caption_digest.clone(),
        image_digest: binding.image_digest.clone(),
        provider_payload: None,
        provider_ids: vec![],
        repeated: result
            .get("repeated")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

/// Validate one status document. The door's row is authoritative for the
/// digests here; key echo, version pin and evidence shapes fail closed.
/// Binding of the row to the frozen intent is owned by the intent
/// lifecycle (771), not this transport.
fn status_of(key: &str, data: &Value) -> std::result::Result<LedgerOutcome, Refusal> {
    if data.get("version").and_then(Value::as_str) != Some(DEVICE_PUBLISH_VERSION) {
        return Err(Refusal::new(
            "refused",
            "door status version drift; refusing rather than recording drifted evidence",
        ));
    }
    if data.get("key").and_then(Value::as_str) != Some(key) {
        return Err(Refusal::new(
            "key_conflict",
            "the door answered status for a different idempotency key",
        ));
    }
    let state = match data.get("state").and_then(Value::as_str) {
        Some("posted") => PublishState::Posted,
        Some("processing") => PublishState::Processing,
        Some("failed") | Some("refused") => PublishState::Refused,
        Some("reconnect_needed") => PublishState::ReconnectNeeded,
        _ => {
            return Err(Refusal::new(
                "refused",
                "door status verdict is malformed; the intent stays processing",
            ));
        }
    };
    let digest = |name: &str| data.get(name).and_then(Value::as_str);
    if digest("destinationId").is_none_or(|value| value.is_empty() || value.len() > 120)
        || digest("captionDigest").is_none_or(|value| !super::publish::valid_digest(value))
        || digest("imageDigest").is_some_and(|value| !super::publish::valid_digest(value))
    {
        return Err(Refusal::new(
            "refused",
            "door status evidence binding is malformed; the intent stays processing",
        ));
    }
    let provider_ids = data
        .get("providerIds")
        .and_then(Value::as_array)
        .map(|ids| {
            ids.iter()
                .filter_map(Value::as_str)
                .filter(|id| !id.is_empty() && id.len() <= 120)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if data.get("providerIds").is_some()
        && provider_ids.len()
            != data
                .get("providerIds")
                .and_then(Value::as_array)
                .map(Vec::len)
                .unwrap_or(0)
    {
        return Err(Refusal::new(
            "refused",
            "door status provider ids are malformed; the intent stays processing",
        ));
    }
    // Byte-exact evidence: the door replays execution-time persistence
    // without re-serialization; the client keeps the document value and
    // persists its compact form. Null before any provider attempt.
    let provider_payload = match data.get("providerPayload") {
        None | Some(Value::Null) => None,
        Some(payload) => Some(
            serde_json::to_string(payload)
                .map_err(|_| Refusal::new("refused", "door status evidence is not serializable"))?,
        ),
    };
    Ok(LedgerOutcome {
        state,
        permalink: data
            .get("permalink")
            .and_then(Value::as_str)
            .map(str::to_owned),
        destination_id: digest("destinationId").unwrap_or_default().to_owned(),
        caption_digest: digest("captionDigest").unwrap_or_default().to_owned(),
        image_digest: digest("imageDigest").map(str::to_owned),
        provider_payload,
        provider_ids,
        // A status read replays recorded evidence by construction —
        // never a second provider call.
        repeated: true,
    })
}

impl Fault {
    fn into_refusal(self) -> Refusal {
        match self {
            Fault::Refused(refusal) => refusal,
            Fault::Ambiguous => Refusal::new("refused", "the door answer is uncertain"),
        }
    }
}

/// Echo check shared by preflight and execution: the door must answer
/// for this exact key, destination and digests. On the keyed POST paths
/// a mismatch means the outcome for OUR key is uncertain, so it stays
/// ambiguity (processing + reconcile) rather than a refusal — except
/// preflight, which mutates nothing and may refuse outright.
fn check_echo(
    binding: &SendBinding,
    data: &Value,
    key: Option<&str>,
) -> std::result::Result<(), Fault> {
    if key != Some(binding.key.as_str()) {
        return Err(Fault::Ambiguous);
    }
    let echo = |name: &str| data.get(name).and_then(Value::as_str);
    if echo("destinationId") != Some(binding.destination_id.as_str())
        || echo("captionDigest") != Some(binding.caption_digest.as_str())
        || echo("imageDigest") != binding.image_digest.as_deref()
    {
        return Err(Fault::Ambiguous);
    }
    Ok(())
}

// ---------- explicit-config registration (default-off) ----------

/// Register the production sender from explicit config, or leave the
/// daemon without one (default-off). Both env knobs must be set;
/// anything malformed refuses startup loudly.
pub fn attach_publish_sender(
    state_dir: &Path,
    opts: &mut crate::daemon::ServeOptions,
) -> Result<()> {
    if opts.social_publish_sender.is_some() {
        return Ok(());
    }
    let metadata = match &opts.provider_deployments {
        Some(value) => Some(value.clone()),
        None => crate::platform::deployments::load()?,
    };
    attach_publish_sender_with(state_dir, opts, metadata.as_ref())
}

/// CAD-1267: image metadata carrying the hosted-media lease assertion (AOS-181
/// serves publish, import and destinations on the same `api.internal` door,
/// under `hosted-publish`) is the only thing that registers the bearerless sender, importer and
/// resolver over `api.internal`. No declaration leaves the device-credential
/// env path exactly as it was; a hosted declaration plus device env config
/// is a conflict and refuses startup (the lease is never silently replaced).
fn attach_publish_sender_with(
    state_dir: &Path,
    opts: &mut crate::daemon::ServeOptions,
    metadata: Option<&crate::platform::deployments::DeploymentMetadata>,
) -> Result<()> {
    if let Some(admission) = metadata.and_then(|value| value.hosted_media()) {
        let device_env = [
            PUBLISH_SEND_URL_ENV,
            PUBLISH_SEND_CREDENTIAL_FILE_ENV,
            PUBLISH_READ_URL_ENV,
            PUBLISH_READ_CREDENTIAL_FILE_ENV,
        ]
        .iter()
        .any(|name| std::env::var_os(name).is_some());
        if device_env {
            return Err(Error::rejected(
                "hosted publish lease conflicts with device publish credential config",
            ));
        }
        let base = HOSTED_ORIGIN;
        let credential = DeviceCredential::hosted_lease(&admission);
        opts.social_publish_sender = Some(Arc::new(HttpPublishSender::new(
            base,
            credential.clone(),
            production_resolver(state_dir),
        )?));
        if opts.social_media_importer.is_none() {
            opts.social_media_importer = Some(Arc::new(
                crate::platform::agenticos_external::media_import::MediaImporter::new(
                    base,
                    credential.clone(),
                )?,
            ));
        }
        if opts.social_media_resolver.is_none() {
            opts.social_media_resolver = Some(Arc::new(
                crate::platform::agenticos_external::media_import::MediaResolver::new(
                    base, credential,
                )?,
            ));
        }
        return Ok(());
    }
    let url = std::env::var(PUBLISH_SEND_URL_ENV).unwrap_or_default();
    let credential_file = std::env::var(PUBLISH_SEND_CREDENTIAL_FILE_ENV).unwrap_or_default();
    if url.is_empty() && credential_file.is_empty() {
        return Ok(());
    }
    if url.is_empty() || credential_file.is_empty() {
        return Err(Error::rejected(format!(
            "{PUBLISH_SEND_URL_ENV} and {PUBLISH_SEND_CREDENTIAL_FILE_ENV} must be set together"
        )));
    }
    let credential = read_credential_file(Path::new(&credential_file))?;
    let resolver = production_resolver(state_dir);
    opts.social_publish_sender = Some(Arc::new(HttpPublishSender::new(
        url.trim_end_matches('/'),
        credential.clone(),
        resolver,
    )?));
    // CAD-979: the retained-media import seam reuses the same configured
    // `publish.send` credential and base URL — one importer held once on
    // `opts`, resolved here at attach (never per-call env, never serialized).
    if opts.social_media_importer.is_none() {
        opts.social_media_importer = Some(Arc::new(
            crate::platform::agenticos_external::media_import::MediaImporter::new(
                url.trim_end_matches('/'),
                credential,
            )?,
        ));
    }
    // CAD-979 v9: the destinations resolver is a separate `provider.read`
    // credential — it maps local→AOS `connectionId` only and never asserts
    // workspace. Absent/unset → `capability_unavailable` at import (the
    // resolver is required to place any media on the AOS wire).
    if opts.social_media_resolver.is_none() {
        let read_url = std::env::var(PUBLISH_READ_URL_ENV).unwrap_or_default();
        let read_file = std::env::var(PUBLISH_READ_CREDENTIAL_FILE_ENV).unwrap_or_default();
        if !read_url.is_empty() && !read_file.is_empty() {
            let read_credential = read_credential_file(Path::new(&read_file))?;
            opts.social_media_resolver = Some(Arc::new(
                crate::platform::agenticos_external::media_import::MediaResolver::new(
                    read_url.trim_end_matches('/'),
                    read_credential,
                )?,
            ));
        }
    }
    Ok(())
}

/// The production material resolver for one state dir: re-proves the
/// exact approved caption from the daemon store at dispatch time.
/// Shared by daemon registration and dispatch-time reconciliation so
/// both speak the same re-proof.
pub fn production_resolver(state_dir: &Path) -> MaterialResolver {
    let state_dir = state_dir.to_path_buf();
    Arc::new(move |binding| store_material(&state_dir, binding))
}

/// Re-prove the exact approved caption from the daemon store — artifact
/// Load the device bearer credential: non-empty, bounded, `0600`.
fn read_credential_file(path: &Path) -> Result<DeviceCredential> {
    let meta = std::fs::metadata(path).map_err(|_| {
        Error::rejected(format!(
            "{PUBLISH_SEND_CREDENTIAL_FILE_ENV} names an unreadable file"
        ))
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.mode() & 0o077 != 0 {
            return Err(Error::rejected(format!(
                "publish send credential file must be mode 0600 (run: chmod 0600 {})",
                path.display()
            )));
        }
    }
    let raw = std::fs::read_to_string(path).map_err(|_| {
        Error::rejected(format!(
            "{PUBLISH_SEND_CREDENTIAL_FILE_ENV} names an unreadable file"
        ))
    })?;
    let secret = raw.trim().to_owned();
    if secret.is_empty() || secret.len() > 8192 {
        return Err(Error::rejected(
            "publish send credential file holds no usable credential",
        ));
    }
    Ok(DeviceCredential::new(secret))
}

/// body for the frozen triple plus the
/// frozen media key — and refuse when anything drifted. Reads through a
/// short-lived read-only connection (never creates, migrates or locks
/// the store); dispatches are human-approved and rare.
fn store_material(
    state_dir: &Path,
    binding: &SendBinding,
) -> std::result::Result<SendMaterial, Refusal> {
    // A social draft effect lives in its install's record store, not the
    // main store; the install is encoded in the `sfx_` effect id.
    let db_path = match &binding.source {
        super::publish::PublicationSource::SocialDraft { .. } => {
            crate::daemon::app_effects_rpc::social_effect_install(&binding.cadence_effect_id)
                .and_then(|install| {
                    crate::store::app_records::record_db_path(state_dir, &install).ok()
                })
                .ok_or_else(|| {
                    Refusal::new(
                        "unknown_key",
                        "no approved social draft effect holds this key",
                    )
                })?
        }
        _ => state_dir.join("cadence.sqlite3"),
    };
    // Transient store failures (open / busy) are `store_unavailable` —
    // the send-now preflight maps them to Uncertain (row stays queued,
    // the operator retries), never Refused: a SQLITE_BUSY blip must not
    // burn a row.
    let conn =
        rusqlite::Connection::open_with_flags(&db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|_| {
                Refusal::new("store_unavailable", "publish material store is unavailable")
            })?;
    conn.busy_timeout(crate::store::BUSY_TIMEOUT)
        .map_err(|_| Refusal::new("store_unavailable", "publish material store is unavailable"))?;
    if let super::publish::PublicationSource::SocialDraft { draft_id, revision } = &binding.source {
        let row: (String,String,String,Option<String>) = conn.query_row(
            "SELECT frozen_json,state,digest,media_key FROM app_social_effects WHERE effect_id=?",
            rusqlite::params![binding.cadence_effect_id],
            |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)),
        ).map_err(|_|Refusal::new("unknown_key","no approved social draft effect holds this key"))?;
        let frozen: Value = serde_json::from_str(&row.0)
            .map_err(|_| Refusal::new("bad_effect", "frozen social draft effect is corrupt"))?;
        if !matches!(row.1.as_str(), "approved" | "sending")
            || crate::store::app_runs::material_digest(&frozen) != row.2
            || frozen["idempotency_key"].as_str() != Some(binding.key.as_str())
            || frozen["effect_id"].as_str() != Some(binding.cadence_effect_id.as_str())
            || frozen["source"]["kind"].as_str() != Some("social_draft")
            || frozen["source"]["draft_id"].as_str() != Some(draft_id.as_str())
            || frozen["source"]["revision"].as_i64() != Some(*revision)
            || frozen["aos_connection_id"].as_str() != Some(binding.connection_id.as_str())
            || frozen["destination_id"].as_str() != Some(binding.destination_id.as_str())
            || frozen["caption_digest"].as_str() != Some(binding.caption_digest.as_str())
            || frozen["image_digest"]
                .as_str()
                .map(super::publish::bare_digest)
                != binding.image_digest.as_deref()
        {
            return Err(Refusal::new(
                "grant_binding_mismatch",
                "social draft effect differs from its approved binding",
            ));
        }
        let caption = frozen["caption"]
            .as_str()
            .ok_or_else(|| Refusal::new("bad_effect", "approved caption is missing"))?
            .to_owned();
        if super::publish::caption_digest_of(&caption) != binding.caption_digest {
            return Err(Refusal::new(
                "grant_binding_mismatch",
                "approved caption digest changed",
            ));
        }
        return Ok(SendMaterial {
            caption,
            media_key: row.3,
        });
    }
    let frozen_text: String = conn
        .query_row(
            "SELECT frozen FROM social_publish_intents WHERE request=?1",
            rusqlite::params![binding.key],
            |row| row.get(0),
        )
        .map_err(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => Refusal::new(
                "unknown_key",
                "no frozen publish intent holds this idempotency key",
            ),
            // SQLITE_BUSY after the busy timeout, or any other read
            // failure, is transient: never a definitive refusal.
            _ => Refusal::new("store_unavailable", "publish material store is unavailable"),
        })?;
    let frozen: Value = serde_json::from_str(&frozen_text)
        .map_err(|_| Refusal::new("bad_effect", "frozen publish intent is corrupt"))?;
    // v9: `binding.connection_id` is the remote AOS `connectionId` (the
    // wire identity), persisted as `frozen["aos_connection_id"]` — NOT the
    // local custody `conn-<uuid4>` in `frozen["connection_id"]`. Compare the
    // wire field; a historical row without `aos_connection_id` fails closed
    // (`Some(expected)` never matches an absent field → refuse).
    for (field, expected) in [
        ("aos_connection_id", binding.connection_id.as_str()),
        ("destination_id", binding.destination_id.as_str()),
        ("caption_digest", binding.caption_digest.as_str()),
        ("grant_id", binding.grant_id.as_str()),
    ] {
        if frozen.get(field).and_then(Value::as_str) != Some(expected) {
            return Err(Refusal::new(
                "grant_binding_mismatch",
                "frozen publish intent differs from the dispatch binding",
            ));
        }
    }
    if frozen.get("image_digest").and_then(Value::as_str) != binding.image_digest.as_deref() {
        return Err(Refusal::new(
            "grant_binding_mismatch",
            "frozen publish intent differs from the dispatch binding",
        ));
    }
    let field = |name: &str| {
        frozen
            .get(name)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| {
                Refusal::new("bad_effect", "frozen publish intent names no run material")
            })
    };
    let material = crate::store::Store::app_publication_material_in(
        &conn,
        &field("run_id")?,
        &field("artifact_id")?,
        &field("bundle_digest")?,
        &field("slot")?,
    )
    .map_err(|_| {
        Refusal::new(
            "bad_effect",
            "approved publish material is no longer current",
        )
    })?;
    let caption = material
        .get("artifact")
        .and_then(|artifact| artifact.get("text"))
        .and_then(Value::as_str)
        .ok_or_else(|| Refusal::new("bad_effect", "approved publish material has no caption"))?
        .to_owned();
    if super::publish::caption_digest_of(&caption) != binding.caption_digest {
        return Err(Refusal::new(
            "grant_binding_mismatch",
            "approved caption differs from the frozen approval",
        ));
    }
    Ok(SendMaterial {
        caption,
        media_key: frozen
            .get("media_key")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

/// Resolver for sender tests: fixed synthetic material, no store.
#[cfg(test)]
pub fn test_resolver(caption: &str, media_key: Option<&str>) -> MaterialResolver {
    let caption = caption.to_owned();
    let media_key = media_key.map(str::to_owned);
    Arc::new(move |_| {
        Ok(SendMaterial {
            caption: caption.clone(),
            media_key: media_key.clone(),
        })
    })
}

#[cfg(test)]
mod tests {
    use super::super::publish::{caption_digest_of, Toolkit};
    use super::*;

    /// CAD-1041: only a missing row is `unknown_key` (definitive); any
    /// other read failure — a store without the table, or SQLITE_BUSY once
    /// the busy timeout runs out — is `store_unavailable`, which send-now's
    /// preflight treats as Uncertain (the row stays queued).
    #[test]
    fn store_material_maps_read_failures_to_store_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        let db = rusqlite::Connection::open(dir.path().join("cadence.sqlite3")).unwrap();
        db.execute_batch("CREATE TABLE unrelated (x INTEGER)")
            .unwrap();
        let refusal = super::store_material(dir.path(), &binding()).unwrap_err();
        assert_eq!(refusal.code, "store_unavailable", "{refusal}");
        db.execute_batch("CREATE TABLE social_publish_intents (request TEXT, frozen TEXT)")
            .unwrap();
        let refusal = super::store_material(dir.path(), &binding()).unwrap_err();
        assert_eq!(refusal.code, "unknown_key", "{refusal}");
        // A writer holding an exclusive lock (rollback journal): the read
        // waits out the busy timeout, then SQLITE_BUSY.
        db.execute_batch("BEGIN EXCLUSIVE").unwrap();
        let refusal = super::store_material(dir.path(), &binding()).unwrap_err();
        assert_eq!(refusal.code, "store_unavailable", "{refusal}");
        db.execute_batch("ROLLBACK").unwrap();
    }

    fn binding() -> SendBinding {
        // Fixture idempotency key built from parts: no secret-shaped
        // literal is ever adjacent to the `key:` field (secrets scan).
        let key = ["cad798", "test", "id", "01"].join("-");
        SendBinding {
            key,
            connection_id: "con_harbour_fb".into(),
            destination_id: "275491372109884".into(),
            toolkit: Toolkit::Facebook,
            caption_digest: caption_digest_of("Harbour at dusk."),
            image_digest: None,
            source: super::super::publish::PublicationSource::Run {
                run_id: "cad_run_test_01".into(),
            },
            cadence_effect_id: "cad_fx_test_01".into(),
            grant_id: "dpq_test_grant_01".into(),
        }
    }

    fn material() -> SendMaterial {
        SendMaterial {
            caption: "Harbour at dusk.".into(),
            media_key: None,
        }
    }

    #[test]
    fn wire_body_carries_no_workspace() {
        let body = send_body(&binding(), &material());
        assert_eq!(body["key"], json!("cad798-test-id-01"));
        assert_eq!(body["caption"], json!("Harbour at dusk."));
        assert_eq!(body["grant"]["id"], json!("dpq_test_grant_01"));
        assert!(body.get("mediaKey").is_none());
        assert!(body.get("workspaceId").is_none());
        assert!(body.get("workspace_id").is_none());
        assert!(body["grant"].get("workspaceId").is_none());
        let with_media = SendMaterial {
            caption: "Harbour at dusk.".into(),
            media_key: Some("dp1.ws.con.9f86d081884c7d659a2feaa0c55ad01".into()),
        };
        assert_eq!(
            send_body(&binding(), &with_media)["mediaKey"],
            json!("dp1.ws.con.9f86d081884c7d659a2feaa0c55ad01")
        );
    }

    // ---- CAD-1267: hosted publish lease registration ----

    /// The baked hosted-media entry is the one declaration that admits the
    /// lease door; there is no separate publish transport.
    const HOSTED_PUBLISH: &str = r#"{"schema":1,"providers":[{"provider":"agenticos_external","origin":"http://api.internal","manifest_pin":"agenticos-external-provider-tools@3","transport":"hosted-media-lease@1"}]}"#;
    /// Metadata that declares no hosted lease transport at all.
    const NO_TRANSPORT: &str = r#"{"schema":1,"providers":[{"provider":"agenticos_external","origin":"https://api-v2.agenticos.hk","manifest_pin":"agenticos-external-provider-tools@3"}]}"#;

    /// Env is process-global: every test that reads or writes the device
    /// publish knobs holds this lock and restores a clean environment.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    const DEVICE_ENVS: [&str; 4] = [
        PUBLISH_SEND_URL_ENV,
        PUBLISH_SEND_CREDENTIAL_FILE_ENV,
        PUBLISH_READ_URL_ENV,
        PUBLISH_READ_CREDENTIAL_FILE_ENV,
    ];

    fn clean_env() {
        for name in DEVICE_ENVS {
            std::env::remove_var(name);
        }
    }

    fn metadata(raw: &str) -> crate::platform::deployments::DeploymentMetadata {
        crate::platform::deployments::DeploymentMetadata::parse(raw.as_bytes()).unwrap()
    }

    fn registered(opts: &crate::daemon::ServeOptions) -> (bool, bool, bool) {
        (
            opts.social_publish_sender.is_some(),
            opts.social_media_importer.is_some(),
            opts.social_media_resolver.is_some(),
        )
    }

    #[test]
    fn hosted_publish_metadata_registers_sender_importer_and_resolver() {
        let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clean_env();
        let mut opts = crate::daemon::ServeOptions::default();
        let dir = tempfile::tempdir().unwrap();
        attach_publish_sender_with(dir.path(), &mut opts, Some(&metadata(HOSTED_PUBLISH))).unwrap();
        assert_eq!(registered(&opts), (true, true, true));
    }

    #[test]
    fn no_hosted_publish_declaration_registers_no_sender() {
        let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clean_env();
        let dir = tempfile::tempdir().unwrap();
        for declared in [None, Some(metadata(NO_TRANSPORT))] {
            let mut opts = crate::daemon::ServeOptions::default();
            attach_publish_sender_with(dir.path(), &mut opts, declared.as_ref()).unwrap();
            assert_eq!(registered(&opts), (false, false, false));
        }
    }

    #[test]
    fn device_credential_path_is_unchanged_without_a_declaration() {
        use std::os::unix::fs::PermissionsExt;
        let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clean_env();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("send.cred");
        std::fs::write(&file, ["device", "material"].join("-")).unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::env::set_var(PUBLISH_SEND_URL_ENV, "https://publish.example");
        std::env::set_var(PUBLISH_SEND_CREDENTIAL_FILE_ENV, &file);
        let mut opts = crate::daemon::ServeOptions::default();
        // Metadata without the lease assertion leaves the device path alone.
        attach_publish_sender_with(dir.path(), &mut opts, Some(&metadata(NO_TRANSPORT))).unwrap();
        // Sender and importer register; the resolver needs its own read env.
        assert_eq!(registered(&opts), (true, true, false));
        // One-sided device config still refuses startup.
        std::env::remove_var(PUBLISH_SEND_CREDENTIAL_FILE_ENV);
        let mut opts = crate::daemon::ServeOptions::default();
        assert!(attach_publish_sender_with(dir.path(), &mut opts, None).is_err());
        clean_env();
    }

    #[test]
    fn unknown_publish_transport_is_refused_not_registered() {
        for transport in ["hosted-publish-lease@1", "hosted-media-lease@2", "publish"] {
            let raw = HOSTED_PUBLISH.replace("hosted-media-lease@1", transport);
            assert!(
                crate::platform::deployments::DeploymentMetadata::parse(raw.as_bytes()).is_err(),
                "{transport}"
            );
        }
    }

    #[test]
    fn hosted_declaration_with_device_env_is_a_conflict() {
        let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clean_env();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var(PUBLISH_SEND_URL_ENV, "https://publish.example");
        let mut opts = crate::daemon::ServeOptions::default();
        assert!(
            attach_publish_sender_with(dir.path(), &mut opts, Some(&metadata(HOSTED_PUBLISH)))
                .is_err()
        );
        assert_eq!(registered(&opts), (false, false, false));
        clean_env();
    }

    #[test]
    fn lease_credential_sends_no_bearer_and_only_reaches_api_internal() {
        let admission = metadata(HOSTED_PUBLISH).hosted_media().unwrap();
        let lease = DeviceCredential::hosted_lease(&admission);
        assert!(lease.authorization().is_none());
        assert_eq!(
            lease.route("/publish/preflight"),
            "/v1/runtime/connectors/hosted-publish/publish/preflight"
        );
        assert_eq!(
            DeviceCredential::new("x".into()).route("/media/import"),
            "/v1/runtime/connectors/media/import"
        );
        assert!(lease.door_base("http://api.internal").is_ok());
        for other in [
            "https://publish.example",
            "http://127.0.0.1:3110",
            "http://api.internal:8080",
            "http://evil.internal",
        ] {
            assert!(lease.door_base(other).is_err(), "{other}");
        }
        // A bearer credential keeps the old origin rule: plain http to
        // api.internal is refused for it.
        let bearer = DeviceCredential::new("x".into());
        assert!(bearer.door_base("http://api.internal").is_err());
        assert!(bearer.door_base("https://publish.example").is_ok());
    }

    /// CAD-1267: a social draft effect is read from its install's record
    /// store (install decoded from the `sfx_` id), never the main store.
    #[test]
    fn social_draft_material_reads_the_install_record_store() {
        let dir = tempfile::tempdir().unwrap();
        let install = "install-one";
        let hex: String = install.bytes().map(|b| format!("{b:02x}")).collect();
        let effect_id = format!("sfx_{hex}_{:032x}", 7);
        let caption = "Hosted caption";
        let key = format!("social_{:032x}", 7);
        let bound = SendBinding {
            key: key.clone(),
            connection_id: "con_hosted".into(),
            destination_id: "275491372109884".into(),
            toolkit: Toolkit::Facebook,
            caption_digest: caption_digest_of(caption),
            image_digest: None,
            source: super::super::publish::PublicationSource::SocialDraft {
                draft_id: "draft-1".into(),
                revision: 1,
            },
            cadence_effect_id: effect_id.clone(),
            grant_id: "dpq_test_grant_01".into(),
        };
        let frozen = json!({
            "idempotency_key": key, "effect_id": effect_id,
            "source": {"kind": "social_draft", "draft_id": "draft-1", "revision": 1},
            "aos_connection_id": "con_hosted", "destination_id": "275491372109884",
            "caption_digest": caption_digest_of(caption), "image_digest": null,
            "grant_id": "dpq_test_grant_01", "caption": caption,
        });
        let path = crate::store::app_records::record_db_path(dir.path(), install).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let db = rusqlite::Connection::open(path).unwrap();
        db.execute_batch(
            "CREATE TABLE app_social_effects(effect_id TEXT, frozen_json TEXT, state TEXT, digest TEXT, media_key TEXT)",
        )
        .unwrap();
        db.execute(
            "INSERT INTO app_social_effects VALUES(?,?,?,?,NULL)",
            rusqlite::params![
                effect_id,
                frozen.to_string(),
                "approved",
                crate::store::app_runs::material_digest(&frozen)
            ],
        )
        .unwrap();
        let material = super::store_material(dir.path(), &bound).unwrap();
        assert_eq!(material.caption, caption);
        // An effect id that names no install is refused, never read from
        // the main store.
        let mut forged = bound.clone();
        forged.cadence_effect_id = "sfx_zz_bad".into();
        assert_eq!(
            super::store_material(dir.path(), &forged).unwrap_err().code,
            "unknown_key"
        );
    }

    #[test]
    fn credential_debug_redacts() {
        let credential = DeviceCredential::new("secret-material".into());
        assert_eq!(format!("{credential:?}"), "DeviceCredential(redacted)");
        assert!(credential
            .authorization()
            .unwrap()
            .contains("secret-material"));
    }

    #[test]
    fn material_mismatch_fails_closed_before_any_door_call() {
        let frozen = binding();
        let wrong_caption = SendMaterial {
            caption: "Tampered caption.".into(),
            media_key: None,
        };
        assert!(check_material(&frozen, &wrong_caption).is_err());
        let wrong_media = SendMaterial {
            caption: "Harbour at dusk.".into(),
            media_key: Some("dp1.ws.con.00000000000000000000000000000000".into()),
        };
        assert!(check_material(&frozen, &wrong_media).is_err());
        assert!(check_material(&binding(), &material()).is_ok());
    }

    #[test]
    fn door_codes_map_to_shared_vocabulary() {
        assert_eq!(
            door_refusal(409, "digest_mismatch", "m", false).code,
            "key_conflict"
        );
        assert_eq!(
            door_refusal(409, "content_mismatch", "m", false).code,
            "grant_binding_mismatch"
        );
        assert_eq!(
            door_refusal(403, "workspace_mismatch", "m", false).code,
            "cross_workspace"
        );
        assert_eq!(
            door_refusal(409, "send_disabled", "m", false).code,
            "send_disabled"
        );
        assert_eq!(
            door_refusal(404, "not_found", "m", true).code,
            "unknown_key"
        );
        assert_eq!(
            door_refusal(404, "not_found", "m", false).code,
            "wrong_connection"
        );
        assert_eq!(
            door_refusal(409, "grant_exhausted", "m", false).code,
            "grant_exhausted"
        );
        assert_eq!(
            door_refusal(409, "grant_revoked", "m", false).code,
            "grant_revoked"
        );
        // Unknown codes keep the door code verbatim with the door detail.
        let refusal = door_refusal(503, "weird_future_code", "boom", false);
        assert_eq!(refusal.code, "weird_future_code");
        assert!(refusal.detail.contains("weird_future_code"));
        assert!(refusal.detail.contains("boom"));
    }

    #[test]
    fn execution_drift_and_forgery_are_ambiguity_not_refusal() {
        // The provider may still have posted under our key: reconcile,
        // never a terminal refusal, recovers the row — including when
        // the verdict document itself drifted versions.
        let binding = binding();
        let forged = json!({
            "version": "1",
            "result": {"key": binding.key, "decision": "approved",
                "executed": true, "status": "posted",
                "permalink": "https://example.test/p/1", "repeated": false},
            "destinationId": "999999999999999",
            "captionDigest": binding.caption_digest,
            "imageDigest": Value::Null,
        });
        assert!(matches!(
            execution_of(&binding, &forged),
            Err(Fault::Ambiguous)
        ));
        let drifted = json!({
            "version": "2",
            "result": {"key": binding.key, "decision": "approved",
                "executed": true, "status": "posted",
                "permalink": None::<String>, "repeated": false},
            "destinationId": binding.destination_id,
            "captionDigest": binding.caption_digest,
            "imageDigest": Value::Null,
        });
        assert!(matches!(
            execution_of(&binding, &drifted),
            Err(Fault::Ambiguous)
        ));
    }

    #[test]
    fn preflight_decisions_gate_staging() {
        // Exactly `approved` stages; declined, pending and even granted
        // refuse with zero sends (AOS-94 permits only approved here).
        let binding = binding();
        let verdict = |decision: &str, executable: bool| {
            json!({
                "key": binding.key, "decision": decision, "executable": executable,
                "repeated": false, "destinationId": binding.destination_id,
                "captionDigest": binding.caption_digest, "imageDigest": Value::Null,
                "reason": Value::Null,
            })
        };
        assert_eq!(
            preflight_of(&binding, &verdict("approved", true)),
            Ok(false)
        );
        assert!(preflight_of(&binding, &verdict("granted", true)).is_err());
        assert!(preflight_of(&binding, &verdict("declined", false)).is_err());
        assert!(preflight_of(&binding, &verdict("pending", false)).is_err());
    }
    /// CAD-1291: the lookup is a read-only GET on the door's grants route,
    /// scoped by destination; a malformed record fails closed.
    #[test]
    fn find_grant_reads_the_door_and_refuses_malformed_records() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let bodies = [
            r#"{"ok":true,"data":{"grants":[{"id":"dpq_ownergrant_001","kind":"standing","connectionId":"c","destinationId":"d","toolkit":"instagram","remainingToday":3,"revokedAt":null}]}}"#,
            r#"{"ok":true,"data":{"grants":[{"id":"nope"}]}}"#,
        ];
        let server = std::thread::spawn(move || {
            let mut lines = Vec::new();
            for body in bodies {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buf = [0u8; 4096];
                let n = stream.read(&mut buf).unwrap();
                lines.push(
                    String::from_utf8_lossy(&buf[..n])
                        .lines()
                        .next()
                        .unwrap()
                        .to_owned(),
                );
                let reply = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(reply.as_bytes()).unwrap();
            }
            lines
        });
        let sender = HttpPublishSender::new(
            &format!("http://127.0.0.1:{port}"),
            DeviceCredential::new("x".repeat(8)),
            test_resolver("c", None),
        )
        .unwrap();
        use super::super::publish::PublishSender;
        let found = sender.find_grant("1784").unwrap();
        assert_eq!(found[0].id, "dpq_ownergrant_001");
        assert_eq!(sender.find_grant("1784").unwrap_err().code, "refused");
        assert_eq!(
            sender.find_grant("a b/../x").unwrap_err().code,
            "bad_destination"
        );
        let lines = server.join().unwrap();
        assert!(
            lines[0].starts_with("GET /v1/runtime/connectors/publish/grants?destinationId=1784 "),
            "{}",
            lines[0]
        );
    }
    /// CAD-1291: a social draft effect freezes no grant id (the owner's
    /// standing grant is found at send time), so the stored-material check
    /// must not demand one; every other frozen field still has to match.
    #[test]
    fn social_draft_material_needs_no_frozen_grant_id() {
        let dir = tempfile::tempdir().unwrap();
        let install = "install-1";
        let effect_id = format!(
            "sfx_{}_{}",
            install
                .bytes()
                .map(|b| format!("{b:02x}"))
                .collect::<String>(),
            "0".repeat(32)
        );
        let mut b = binding();
        b.source = super::super::publish::PublicationSource::SocialDraft {
            draft_id: "d1".into(),
            revision: 2,
        };
        b.cadence_effect_id = effect_id.clone();
        let frozen = json!({"idempotency_key": b.key, "effect_id": effect_id,
            "source":{"kind":"social_draft","draft_id":"d1","revision":2},
            "aos_connection_id": b.connection_id, "destination_id": b.destination_id,
            "caption_digest": b.caption_digest, "caption":"Harbour at dusk."});
        let path = crate::store::app_records::record_db_path(dir.path(), install).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch(
            "CREATE TABLE app_social_effects (effect_id TEXT, frozen_json TEXT, state TEXT, digest TEXT, media_key TEXT)",
        )
        .unwrap();
        db.execute(
            "INSERT INTO app_social_effects VALUES (?,?,?,?,NULL)",
            rusqlite::params![
                effect_id,
                frozen.to_string(),
                "approved",
                crate::store::app_runs::material_digest(&frozen)
            ],
        )
        .unwrap();
        let material = super::store_material(dir.path(), &b).unwrap();
        assert_eq!(material.caption, "Harbour at dusk.");
        // A drifted caption digest is still refused.
        let mut drifted = b.clone();
        drifted.caption_digest = "0".repeat(64);
        assert_eq!(
            super::store_material(dir.path(), &drifted)
                .unwrap_err()
                .code,
            "grant_binding_mismatch"
        );
    }
}
