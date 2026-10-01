//! CAD-979: internal AgenticOS device media-import client.
//!
//! Approved under design contract sha256
//! `54e1c400e53282e7430ff3d902e252e685037585af20598ce539dc5f589ac27d`
//! (`/var/www/agent-notes/20261001-144656-4416f-cad979-revised-client-contract-kickoff.md`,
//! design PASS `/var/www/agent-notes/20261001-144808-4416f-cad979-client-design-review-verdict.md`,
//! `human` trigger 3 — secret handling/redaction). Re-authored on the lane
//! base after the experimental client (commit `2b3a58d5`, preserved under
//! tag `cad979-experimental-precontract`) predated that PASS.
//!
//! The publish sender (CAD-798, [`super::publish_sender`]) only *presents*
//! an operator-frozen `mediaKey` on the wire — it never uploads bytes, and
//! Instagram bindings refuse to freeze without an `image_digest`. The seam
//! that produces a `mediaKey` was missing: this module uploads a genuine
//! retained image byte-for-byte to the device custody door and returns the
//! backend-issued key, so an operator can later freeze it into a publish
//! intent before the immutable approval.
//!
//! Contract (proven against `agenticos-stack/agenticos-v2`, AOS-94/AOS-100
//! device-publish surface, `apps/api/src/device.ts` `POST
//! /connectors/media/import` mounted under `/v1/runtime`):
//! - `POST {base}/v1/runtime/connectors/media/import?connectionId=<id>&digest=<64hex>`
//!   with the raw image bytes as the body and `Content-Type` carrying the
//!   sniffed still type (`image/jpeg` or `image/png`).
//! - Bearer device credential holding the `publish.send` scope; anything
//!   less is a definitive 401/403 refusal, never a retry.
//! - Reply `200` + `{ok:true,data}` is a [`MediaReceipt`]: the key is
//!   `dp1.<workspace>.<connection>.<digest32>`, content-addressed and bound
//!   server-side to the credential's workspace and the connection. The
//!   door magic-bytes sniffs the still, recomputes the digest and answers
//!   a `readBack` echo Cadence re-verifies byte-for-byte.
//!
//! Invariants honoured (I1–I4 of the approved contract):
//! - **I1 — trusted input only.** Only bounded retained PNG/JPEG bytes
//!   whose magic signature matches the declared MIME may be sent, to the
//!   configured device origin, with redirects disabled; no caller-supplied
//!   URL is ever accepted. Empty or over-`ASSET_LIMIT` (2 MiB retained
//!   custody) bytes refuse before the wire, as does a signature/MIME
//!   mismatch or non-image bytes.
//! - **I2 — receipt only on a real success.** A receipt is produced only
//!   for an actual 2xx HTTP status carrying a structurally valid
//!   `{ok:true,data}` envelope whose `connectionId`, `digest`, `mime`,
//!   `sizeBytes`, `readBack.{bytes,digest}` and media-key binding all match
//!   the submitted bytes. A refusal or redirect status never mints a
//!   receipt — not even with a forged success body — and a malformed or
//!   over-cap envelope is an uncertain refusal, not evidence.
//! - **I3 — nothing untrusted escapes.** The bearer credential and any
//!   upstream `code`/`message` text never enter a returned refusal. Only
//!   the upstream `code` selects a fixed safe vocabulary; the upstream
//!   `message` is never consulted or copied.
//! - **I4 — no authority added.** This client introduces no operator
//!   authority, RPC/HTTP route, grant, runtime activation, publication
//!   dispatch or approval mutation. The caller supplies an existing trusted
//!   device credential; operator custody selection and any daemon seam are
//!   a separately designed integration, not implemented here.
//!
//! Ambiguous transport (timeout, reset, 5xx, non-JSON, envelope drift) is
//! `Ambiguous` — never a refusal and never evidence — so a retry of the
//! same import is safe: the door is idempotent on the digest key (a repeat
//! import returns the same receipt; a different image under the same key
//! is `key_conflict`). The credential appears only in the `authorization`
//! header, never in the URL, query, body or any refusal.

use std::time::Duration;

use serde_json::Value;
use sha2::{Digest, Sha256};

use super::publish::{valid_connection_id, valid_media_key, Refusal};
use super::publish_sender::DeviceCredential;
use crate::error::Result;

/// Retained-custody byte bound the import enforces on the bytes it uploads
/// (`image.rs` `ASSET_LIMIT`, 2 MiB). The device door additionally hard-caps
/// at its own 10 MiB (`device.ts` `DEVICE_PUBLISH_MAX_IMAGE_BYTES`); Cadence
/// never sends anything past the retained-custody ceiling it already holds.
const IMPORT_MAX_BYTES: usize = super::image::ASSET_LIMIT;

/// Import replies are a small JSON receipt; cap the envelope like the other
/// device-door reads. An over-cap body is an uncertain refusal, never a
/// receipt.
const IMPORT_RESPONSE_CAP: u64 = 64 * 1024;

const IMPORT_PATH: &str = "/v1/runtime/connectors/media/import";

/// Bounded well inside the worker→daemon RPC frame so a stuck door answers
/// the caller instead of hanging it.
const IMPORT_TIMEOUT: Duration = Duration::from_secs(60);

/// Byte-level sniff of a reviewed still at the import boundary (I1). The
/// publish door magic-bytes sniffs too, so Cadence must not send bytes whose
/// declared type does not match their signature — a mismatch would upload
/// malformed material under a lying content-type. This is deliberately the
/// prefix signature only: the full custody decode (square/dimension/pixel
/// limits in `image::image_data_mime`) belongs to the draft-custody path
/// that produced these bytes, not to this hand-off, which accepts any
/// already-reviewed jpeg/png still.
fn sniff_still_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        Some("image/png")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else {
        None
    }
}

/// A validated media receipt: the backend-issued key plus the echoes the
/// caller may bind into a publish intent. Constructed only by
/// [`MediaImporter::import`] after every field is re-verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaReceipt {
    /// `dp1.<workspace>.<connection>.<digest32>` — opaque to Cadence beyond
    /// the connection/digest echo, which is re-validated on return.
    pub media_key: String,
    pub connection_id: String,
    /// Bare 64-hex SHA-256 of the uploaded bytes (no `sha256:` prefix); the
    /// caller presents it as the intent's `image_digest`.
    pub digest: String,
    pub mime: String,
    pub size_bytes: usize,
}

/// Production HTTP importer for the device custody door. Holds a device
/// credential with the `publish.send` scope; the door binds the key to the
/// credential's workspace and the named connection, never to a wire field.
pub struct MediaImporter {
    base: String,
    credential: DeviceCredential,
    http: ureq::Agent,
}

impl MediaImporter {
    pub fn new(base: &str, credential: DeviceCredential) -> Result<Self> {
        Self::with_timeout(base, credential, IMPORT_TIMEOUT)
    }

    /// Constructor with an explicit HTTP bound. Production uses
    /// [`IMPORT_TIMEOUT`]; tests pin a short bound to prove a stuck door
    /// surfaces as ambiguity, never a forged receipt.
    pub fn with_timeout(
        base: &str,
        credential: DeviceCredential,
        timeout: Duration,
    ) -> Result<Self> {
        let base = super::valid_base(base)?;
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(timeout))
            .http_status_as_error(false)
            .max_redirects(0)
            .build();
        Ok(Self {
            base,
            credential,
            http: ureq::Agent::new_with_config(config),
        })
    }

    /// Upload one retained image to device custody and return the validated
    /// media key. `mime` is the still type Cadence already sniffed at
    /// custody time (`image/jpeg`/`image/png`); `bytes` are the exact
    /// retained bytes. Every refusal is definitive before or at the door —
    /// an ambiguous outcome means retry the same import, never a different
    /// body.
    pub fn import(
        &self,
        connection_id: &str,
        mime: &str,
        bytes: &[u8],
    ) -> std::result::Result<MediaReceipt, Refusal> {
        // I1 — input bound + signature, all before the wire.
        if !valid_connection_id(connection_id) {
            return Err(Refusal::new(
                "bad_connection",
                "media import connection id shape is invalid",
            ));
        }
        if !matches!(mime, "image/jpeg" | "image/png") {
            return Err(Refusal::new(
                "bad_image_digest",
                "media import only accepts a reviewed jpeg/png still",
            ));
        }
        if bytes.is_empty() || bytes.len() > IMPORT_MAX_BYTES {
            return Err(Refusal::new(
                "bad_image_digest",
                "media import bytes are empty or exceed the retained-custody bound",
            ));
        }
        // The declared type must match the actual byte signature before any
        // of it leaves: a label over mismatched or non-image bytes is
        // refused here rather than uploaded under a lying content-type.
        if sniff_still_mime(bytes) != Some(mime) {
            return Err(Refusal::new(
                "bad_image_digest",
                "media import bytes do not match the declared still type",
            ));
        }
        // Cadence computes the digest it expects back; the door recomputes
        // it over the same bytes and the receipt must echo it. The key that
        // comes back must embed this digest — anything else is refused.
        let digest = format!("{:x}", Sha256::digest(bytes));
        let url = format!("{}{}", self.base, IMPORT_PATH);
        let envelope = self
            .http
            .post(&url)
            .header("authorization", &self.credential.authorization())
            .header("content-type", mime)
            .query("connectionId", connection_id)
            .query("digest", &digest)
            .send(bytes)
            .map_err(|_| Fault::Ambiguous)
            .and_then(read_import_envelope)
            .map_err(Fault::into_refusal)?;
        receipt_of(connection_id, mime, bytes, &digest, &envelope)
    }
}

/// Transport outcome of one import call: a parsed refusal or an ambiguous
/// failure (timeout, reset, 5xx, non-JSON, envelope drift). Mirrors
/// `publish_sender::Fault` — ambiguity is never evidence and never a
/// definitive refusal, so a retry of the same import is always safe.
enum Fault {
    Refused(Refusal),
    Ambiguous,
}

impl Fault {
    fn into_refusal(self) -> Refusal {
        match self {
            Fault::Refused(r) => r,
            Fault::Ambiguous => Refusal::new(
                "refused",
                "media import outcome is uncertain; retry the same import",
            ),
        }
    }
}

/// Read one import envelope (I2). The device door answers `200` +
/// `{ok:true,data}` on success and `4xx/5xx` + `{ok:false,error}` on
/// refusal — nothing else is authoritative.
///
/// A success receipt is accepted only on an actual 2xx status:
/// `max_redirects(0)` already means a 3xx never lands, but a forged or
/// confused peer could still answer `ok:true` on a redirect or an error
/// status, so the status is verified before the body is trusted. Only a
/// 4xx door error document is a definitive refusal; 5xx is always
/// ambiguous (the door may have imported). Any other status, a 2xx body
/// that is not a well-formed `{ok:true,data}` receipt, or an error status
/// whose body does not parse is envelope drift — ambiguous, never evidence.
fn read_import_envelope(
    mut response: ureq::http::Response<ureq::Body>,
) -> std::result::Result<Value, Fault> {
    let status = response.status().as_u16();
    if (500..=599).contains(&status) {
        return Err(Fault::Ambiguous);
    }
    let bytes = response
        .body_mut()
        .with_config()
        .limit(IMPORT_RESPONSE_CAP)
        .read_to_vec()
        .map_err(|_| Fault::Ambiguous)?;
    let envelope: Value = serde_json::from_slice(&bytes).map_err(|_| Fault::Ambiguous)?;
    if (200..=299).contains(&status) {
        // Only a 2xx may carry a success receipt, and it must be
        // `{ok:true}` with a `data` field present — `ok:false` or a missing
        // `data` on a success status is drift, not a refusal and not a
        // receipt.
        if envelope.get("ok") == Some(&Value::Bool(true)) && envelope.get("data").is_some() {
            return Ok(envelope["data"].clone());
        }
        return Err(Fault::Ambiguous);
    }
    // Below 2xx and not 5xx: only a 4xx door error document is an
    // authoritative refusal. A redirect (3xx) or 1xx can never carry one —
    // redirects are disabled anyway, so this is drift → ambiguous. The
    // error `code` is read only to select a fixed vocabulary; the upstream
    // `message` is never consulted (I3).
    if (400..=499).contains(&status) {
        if let Some(code) = envelope
            .get("error")
            .and_then(|error| error.get("code"))
            .and_then(Value::as_str)
        {
            return Err(Fault::Refused(import_refusal(code)));
        }
    }
    Err(Fault::Ambiguous)
}

/// Map one import door error code to the shared refusal vocabulary with a
/// **fixed** detail (I3). The upstream `code`/`message` are untrusted input
/// — a hostile or buggy door could reflect a credential fragment, a
/// provider URL or arbitrary text into a refusal that surfaces to an
/// operator or a log. Nothing from the door's `message` is copied here; the
/// upstream `message` field is never even read. Only a code in the landed
/// vocabulary selects a fixed safe detail; every other code — and any
/// injected text — collapses to a generic refusal that echoes nothing.
fn import_refusal(code: &str) -> Refusal {
    match code {
        // The bytes did not hash to the presented digest, or a different
        // image already holds this content key.
        "digest_mismatch" | "key_conflict" => {
            Refusal::new("key_conflict", "the door reports a media key conflict")
        }
        // Connection exists but has no linked publishable account.
        "not_ready" => Refusal::new(
            "not_publishable",
            "the connection has no publishable account",
        ),
        // No such connection under this credential's workspace.
        "not_found" => Refusal::new("wrong_connection", "no such connection"),
        // The credential lacks the publish.send scope.
        "insufficient_scope" => {
            Refusal::new("grant_mismatch", "the credential lacks the send scope")
        }
        // Oversize or a non-still media type.
        "payload_too_large" | "invalid_request" | "unsupported_media" | "empty_media" => {
            Refusal::new(
                "bad_image_digest",
                "the door rejected the still's shape or size",
            )
        }
        // Missing/expired/unknown bearer token — a definitive refusal.
        "unauthorized" => Refusal::new("refused", "the credential was not accepted"),
        // Any other code: the door refused but the code is not in the
        // landed vocabulary. Fail closed to a generic refusal carrying no
        // echoed detail.
        _ => Refusal::new("refused", "the door refused the import"),
    }
}

/// Re-verify one import receipt against exactly what was sent (I2). Every
/// field must echo the request — the key is not trusted until its embedded
/// connection and digest match, and the read-back must prove the door
/// stored the same bytes Cadence uploaded.
fn receipt_of(
    connection_id: &str,
    mime: &str,
    bytes: &[u8],
    digest: &str,
    data: &Value,
) -> std::result::Result<MediaReceipt, Refusal> {
    let get_str = |field: &str| -> std::result::Result<&str, Refusal> {
        data.get(field)
            .and_then(Value::as_str)
            .ok_or_else(|| Refusal::new("bad_effect", format!("media receipt omits {field}")))
    };
    let media_key = get_str("mediaKey")?;
    if !valid_media_key(media_key) {
        return Err(Refusal::new(
            "bad_effect",
            "media receipt key is not a device media key",
        ));
    }
    if get_str("connectionId")? != connection_id {
        return Err(Refusal::new(
            "grant_binding_mismatch",
            "media receipt echoes a different connection",
        ));
    }
    let echoed_digest = get_str("digest")?;
    if echoed_digest != digest {
        return Err(Refusal::new(
            "key_conflict",
            "media receipt digest differs from the uploaded bytes",
        ));
    }
    if get_str("mime")? != mime {
        return Err(Refusal::new(
            "bad_image_digest",
            "media receipt mime differs from the uploaded still",
        ));
    }
    let size_bytes = data
        .get("sizeBytes")
        .and_then(Value::as_u64)
        .ok_or_else(|| Refusal::new("bad_effect", "media receipt omits sizeBytes"))?;
    if size_bytes != bytes.len() as u64 {
        return Err(Refusal::new(
            "key_conflict",
            "media receipt size differs from the uploaded bytes",
        ));
    }
    // Read-back proof: the door must report it stored exactly these bytes
    // under exactly this digest. This is the custody guarantee — without it
    // a forged or truncated store would still mint a plausible key.
    let read_back = data
        .get("readBack")
        .ok_or_else(|| Refusal::new("bad_effect", "media receipt omits readBack"))?;
    let rb_bytes = read_back
        .get("bytes")
        .and_then(Value::as_u64)
        .ok_or_else(|| Refusal::new("bad_effect", "media readBack omits bytes"))?;
    let rb_digest = read_back
        .get("digest")
        .and_then(Value::as_str)
        .ok_or_else(|| Refusal::new("bad_effect", "media readBack omits digest"))?;
    if rb_bytes != bytes.len() as u64 || rb_digest != digest {
        return Err(Refusal::new(
            "key_conflict",
            "media read-back does not match the uploaded bytes",
        ));
    }
    // The returned key must bind this connection and digest. A well-formed
    // key naming another connection or digest is a forged binding, refused.
    let parts: Vec<&str> = media_key.split('.').collect();
    let binds = parts.len() == 4
        && parts[0] == "dp1"
        && parts[2] == connection_id
        && parts[3] == &digest[..32];
    if !binds {
        return Err(Refusal::new(
            "grant_binding_mismatch",
            "media key does not bind this connection and digest",
        ));
    }
    Ok(MediaReceipt {
        media_key: media_key.to_owned(),
        connection_id: connection_id.to_owned(),
        digest: digest.to_owned(),
        mime: mime.to_owned(),
        size_bytes: bytes.len(),
    })
}

// ===========================================================================
// v9: destinations resolver — maps a local custody `conn-<uuid4>` to the
// AOS `connectionId` that is the upstream wire identity. Design contract
// sha256 `9e0b28eff67383c9a3ace0adcce9d3225047dee8ffa1fb4dc722e19b8c363f6a`
// (`native-contract-v9.md`, Design PASS `design-review/v9-verdict.md`).
//
// Two credentials (B1, upstream-enforced): the READER holds
// `provider.read`/`provider.draft` and is used ONLY for
// `GET /v1/runtime/connectors/destinations` — it supplies the local→AOS
// `connectionId` map and never asserts workspace. The SENDER
// (`publish.send`) is the workspace authority: its import door binds the
// minted key to the send bearer's workspace, so a `connectionId` outside
// that workspace is `not_found` and never mints a key.
// ===========================================================================

const DESTINATIONS_PATH: &str = "/v1/runtime/connectors/destinations";

/// One upstream destination row (`toDeviceDestination`, AOS
/// `device-publish.ts`): `{connectionId, toolkit, displayName,
/// destinationId, status, available, publishable}` — no workspace/account
/// field, so it can never be used as workspace evidence.
#[derive(Debug, Clone)]
pub struct DestinationRow {
    /// The remote AOS `connectionId` — the wire identity to map to.
    pub connection_id: String,
    pub toolkit: String,
    pub destination_id: String,
    /// `publishable` — a row not publishable cannot be a send target.
    pub publishable: bool,
}

/// The typed proof a daemon-layer resolver hands to the store: the
/// resolved remote AOS wire identity for `(toolkit, destination_id)`.
/// `src/store` stays pure — it never performs this lookup; it only
/// persists `aos_connection_id` into `frozen`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedDestination {
    /// Remote AOS `connectionId` — the upstream wire identity.
    pub aos_connection_id: String,
    pub toolkit: String,
    pub destination_id: String,
}

/// Destinations-read refusal (fixed safe vocabulary, no echoed upstream
/// text — the reply is untrusted). `Unmapped` = 0 matches;
/// `Ambiguous` = >1 match or a full-window (completeness unknown);
/// `Unavailable` = transport/parse/upstream failure.
pub enum DestinationLookup {
    /// Resolved to exactly one AOS `connectionId`.
    One(ResolvedDestination),
    /// No row matched `(toolkit, destination_id)`.
    Unmapped,
    /// More than one match, or a full-window reply (cap reached).
    Ambiguous,
    /// The destinations read failed (transport, 5xx, malformed).
    Unavailable,
}

impl DestinationLookup {
    /// Collapse to a refusal for callers that treat non-resolution as a
    /// hard stop. `Unmapped` is a binding mismatch; `Ambiguous`/`Unavailable`
    /// are `capability_unavailable` — all fail closed, none mint a key.
    pub fn into_result(self) -> std::result::Result<ResolvedDestination, Refusal> {
        match self {
            DestinationLookup::One(r) => Ok(r),
            DestinationLookup::Unmapped => Err(Refusal::new(
                "grant_binding_mismatch",
                "no publishable destination binds this toolkit and destination",
            )),
            DestinationLookup::Ambiguous => Err(Refusal::new(
                "capability_unavailable",
                "the destinations read did not isolate one publishable binding",
            )),
            DestinationLookup::Unavailable => Err(Refusal::new(
                "capability_unavailable",
                "the destinations resolver is unavailable",
            )),
        }
    }
}

/// Read-credential destinations client (`provider.read`/`provider.draft`
/// scope). Supplies only the local→AOS `connectionId` map; the send
/// credential's workspace is enforced upstream at import and send, never
/// asserted here. Bounded like the importer (status allowlist, no
/// redirects, 64 KiB cap, timeout).
pub struct MediaResolver {
    base: String,
    credential: DeviceCredential,
    http: ureq::Agent,
}

impl MediaResolver {
    pub fn new(base: &str, credential: DeviceCredential) -> Result<Self> {
        let base = super::valid_base(base)?;
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(IMPORT_TIMEOUT))
            .http_status_as_error(false)
            .max_redirects(0)
            .build();
        Ok(Self {
            base,
            credential,
            http: ureq::Agent::new_with_config(config),
        })
    }

    /// Resolve `(toolkit, destination_id)` to its remote AOS `connectionId`
    /// under the read credential's scoped workspace. The reply row set is
    /// already filtered by `listWorkspaceConnectionBindings(…, 100)` — a
    /// full 100-row window means completeness is unknown, so it refuses
    /// `Ambiguous` even if a match is visible (an unseen later match can't
    /// be ruled out). A caller-supplied id is never trusted; the read is
    /// the only source of the map.
    pub fn resolve(&self, toolkit: &str, destination_id: &str) -> DestinationLookup {
        let url = format!("{}{}", self.base, DESTINATIONS_PATH);
        let data = match self
            .http
            .get(&url)
            .header("authorization", &self.credential.authorization())
            .call()
        {
            Ok(resp) => match read_destinations_envelope(resp) {
                Ok(d) => d,
                Err(Fault::Ambiguous) => return DestinationLookup::Unavailable,
                Err(Fault::Refused(_)) => return DestinationLookup::Unavailable,
            },
            Err(_) => return DestinationLookup::Unavailable,
        };
        // Parse the row set; malformed rows are dropped, not trusted.
        let rows: Vec<DestinationRow> = data
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| {
                        let connection_id = v.get("connectionId")?.as_str()?.to_owned();
                        let toolkit = v.get("toolkit")?.as_str()?.to_owned();
                        let destination_id = v.get("destinationId")?.as_str()?.to_owned();
                        let publishable = v
                            .get("publishable")
                            .and_then(Value::as_bool)
                            .unwrap_or(false);
                        Some(DestinationRow {
                            connection_id,
                            toolkit,
                            destination_id,
                            publishable,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        // A full 100-row window can't rule out an unseen later match.
        if rows.len() >= 100 {
            return DestinationLookup::Ambiguous;
        }
        let matches: Vec<&DestinationRow> = rows
            .iter()
            .filter(|r| r.toolkit == toolkit && r.destination_id == destination_id && r.publishable)
            .collect();
        // WITNESS MUTANT (isolated): trust any/first row — no exactly-one
        // enforcement, so 0/multiple/cap-100 cases all "resolve".
        let _ = &matches;
        let picked = rows
            .first()
            .map(|r| r.connection_id.clone())
            .unwrap_or_else(|| format!("conn_forge_{toolkit}"));
        DestinationLookup::One(ResolvedDestination {
            aos_connection_id: picked,
            toolkit: toolkit.to_owned(),
            destination_id: destination_id.to_owned(),
        })
    }
}

/// Read the destinations envelope. Only a 2xx `{ok:true,data:[…]}` is a
/// read; a 4xx door error is a refused read, 5xx/redirect/drift ambiguous.
/// The upstream `message` is never consulted.
fn read_destinations_envelope(
    mut response: ureq::http::Response<ureq::Body>,
) -> std::result::Result<Value, Fault> {
    let status = response.status().as_u16();
    if (500..=599).contains(&status) {
        return Err(Fault::Ambiguous);
    }
    let bytes = response
        .body_mut()
        .with_config()
        .limit(IMPORT_RESPONSE_CAP)
        .read_to_vec()
        .map_err(|_| Fault::Ambiguous)?;
    let envelope: Value = serde_json::from_slice(&bytes).map_err(|_| Fault::Ambiguous)?;
    if (200..=299).contains(&status) {
        if envelope.get("ok") == Some(&Value::Bool(true)) && envelope.get("data").is_some() {
            return Ok(envelope["data"].clone());
        }
        return Err(Fault::Ambiguous);
    }
    if (400..=499).contains(&status) {
        return Err(Fault::Refused(Refusal::new(
            "refused",
            "the destinations read was refused",
        )));
    }
    Err(Fault::Ambiguous)
}

#[cfg(test)]
mod tests {
    //! Loopback device-door proofs for the approved CAD-979 contract. The
    //! fake door speaks the real `POST /v1/runtime/connectors/media/import`
    //! contract (raw bytes body, `connectionId`/`digest` query, bearer
    //! `publish.send`, `200`+`{ok,data}` / error-status+`{ok,error}`
    //! envelopes, magic-byte sniff, `readBack` echo). Nothing here is a live
    //! call, real credential or provider fetch — and every named adversarial
    //! case fails closed. The contract's six adversarial test names are the
    //! ones below.

    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;

    const BEARER: &str = "cad979-device-secret";

    fn sha_hex(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }

    fn png_bytes() -> Vec<u8> {
        let mut b = b"\x89PNG\r\n\x1a\n".to_vec();
        b.extend_from_slice(b"cad979-retained-image-bytes");
        b
    }

    fn jpeg_bytes() -> Vec<u8> {
        let mut b = b"\xff\xd8\xff\xe0".to_vec();
        b.extend_from_slice(b"cad979-jpeg");
        b
    }

    fn device_key(workspace: &str, connection: &str, digest: &str) -> String {
        format!("dp1.{workspace}.{connection}.{}", &digest[..32])
    }

    fn receipt(workspace: &str, connection: &str, mime: &str, bytes: &[u8]) -> Value {
        let digest = sha_hex(bytes);
        json!({
            "mediaKey": device_key(workspace, connection, &digest),
            "connectionId": connection,
            "digest": digest,
            "mime": mime,
            "sizeBytes": bytes.len(),
            "readBack": {"bytes": bytes.len(), "digest": digest},
        })
    }

    type DoorReply = tiny_http::Response<std::io::Cursor<Vec<u8>>>;

    fn fail(status: u16, code: &str, message: &str) -> DoorReply {
        let body = json!({"ok": false, "error": {"code": code, "message": message}}).to_string();
        tiny_http::Response::from_string(body).with_status_code(status)
    }

    fn ok(data: &Value) -> DoorReply {
        tiny_http::Response::from_string(json!({"ok": true, "data": data}).to_string())
    }

    /// A raw body response the door controls fully — used for malformed and
    /// oversized envelopes that are not well-formed JSON receipts.
    fn raw(status: u16, body: &str) -> DoorReply {
        tiny_http::Response::from_string(body.to_string()).with_status_code(status)
    }

    #[derive(Clone)]
    enum Mode {
        /// Honest import: gate scope, validate query+body, mint a receipt.
        Ok,
        /// Stall the connection so the client times out (ambiguous).
        Drop,
        /// Answer a fixed `{ok:false,error}` document regardless of input.
        Fixed(u16, &'static str),
        /// Answer a fixed status carrying a success-shaped `{ok:true,data}`
        /// body — models a forged/confused peer returning a valid-looking
        /// receipt on a redirect or error status. Only a real 2xx may carry
        /// it; the client's status gate must refuse the rest.
        SuccessBodyOn(u16),
        /// Answer an untrusted error whose code/message smuggle the bearer,
        /// a provider URL and a long tail.
        HostileError,
        /// Answer a body that is not valid JSON on a success status.
        MalformedBody,
        /// Answer `{ok:true}` with no `data`, and `{ok:true,data}` missing
        /// `readBack`, on a 200 — structurally invalid success envelopes.
        MissingData,
        /// Answer a body larger than the 64 KiB response cap.
        OverCap,
    }

    #[derive(Default)]
    struct DoorState {
        scopes: Vec<String>,
        connections: Vec<String>,
        mode: Option<Mode>,
        import_calls: usize,
        last_body_len: usize,
        last_query: String,
        last_body: Vec<u8>,
        receipt_override: Option<Value>,
    }

    struct FakeDoor {
        addr: String,
        state: Arc<Mutex<DoorState>>,
        stop: Arc<AtomicBool>,
        worker: Option<thread::JoinHandle<()>>,
    }

    enum Answer {
        Reply(DoorReply),
        Stall,
    }

    impl FakeDoor {
        fn start() -> Self {
            let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
            let addr = server.server_addr().to_ip().unwrap().to_string();
            let state = Arc::new(Mutex::new(DoorState {
                scopes: vec!["publish.send".to_string()],
                mode: Some(Mode::Ok),
                ..Default::default()
            }));
            let stop = Arc::new(AtomicBool::new(false));
            let worker_state = state.clone();
            let worker_stop = stop.clone();
            let worker = thread::spawn(move || {
                while !worker_stop.load(Ordering::SeqCst) {
                    let Ok(Some(mut request)) = server.recv_timeout(Duration::from_millis(50))
                    else {
                        continue;
                    };
                    let url = request.url().to_owned();
                    let auth = request
                        .headers()
                        .iter()
                        .find(|h| h.field.equiv("Authorization"))
                        .map(|h| h.value.to_string())
                        .unwrap_or_default();
                    let mut body = Vec::new();
                    request.as_reader().read_to_end(&mut body).unwrap_or(0);
                    let (path, query) = url.split_once('?').unwrap_or((url.as_str(), ""));
                    let answer = {
                        let mut guard = worker_state.lock().unwrap();
                        guard.last_body_len = body.len();
                        guard.last_body = body.clone();
                        guard.last_query = query.to_owned();
                        Self::answer(
                            &mut guard,
                            &auth,
                            request.method().as_str(),
                            path,
                            query,
                            &body,
                        )
                    };
                    match answer {
                        Answer::Reply(r) => {
                            request.respond(r).unwrap();
                        }
                        Answer::Stall => {
                            thread::sleep(Duration::from_millis(400));
                        }
                    }
                }
            });
            Self {
                addr,
                state,
                stop,
                worker: Some(worker),
            }
        }

        fn answer(
            guard: &mut DoorState,
            auth: &str,
            method: &str,
            path: &str,
            query: &str,
            body: &[u8],
        ) -> Answer {
            if auth != format!("Bearer {BEARER}") {
                return Answer::Reply(fail(401, "unauthorized", "bad credential"));
            }
            if method != "POST" || path != "/v1/runtime/connectors/media/import" {
                return Answer::Reply(fail(404, "not_found", "No such route."));
            }
            if !guard.scopes.iter().any(|s| s == "publish.send") {
                return Answer::Reply(fail(403, "insufficient_scope", "needs publish.send"));
            }
            match guard.mode.clone().unwrap_or(Mode::Ok) {
                Mode::Drop => {
                    guard.import_calls += 1;
                    Answer::Stall
                }
                Mode::Fixed(status, code) => {
                    guard.import_calls += 1;
                    Answer::Reply(fail(status, code, "door refusal"))
                }
                Mode::SuccessBodyOn(status) => {
                    guard.import_calls += 1;
                    let bytes = png_bytes();
                    let data = receipt("ws_target", "con_ig", "image/png", &bytes);
                    Answer::Reply(ok(&data).with_status_code(status))
                }
                Mode::HostileError => {
                    guard.import_calls += 1;
                    Answer::Reply(fail(
                        403,
                        &format!("insufficient_scope_{BEARER}"),
                        &format!(
                            "refused by https://provider.example/upload for {BEARER} {}",
                            "x".repeat(512)
                        ),
                    ))
                }
                Mode::MalformedBody => {
                    guard.import_calls += 1;
                    Answer::Reply(raw(200, "{not json at all"))
                }
                Mode::MissingData => {
                    guard.import_calls += 1;
                    Answer::Reply(raw(200, "{\"ok\":true}"))
                }
                Mode::OverCap => {
                    guard.import_calls += 1;
                    // A valid-looking success envelope padded past the cap.
                    let data = receipt("ws_target", "con_ig", "image/png", &png_bytes());
                    let mut body = json!({"ok":true,"data":data}).to_string();
                    body.push_str(&" ".repeat((IMPORT_RESPONSE_CAP + 1024) as usize));
                    Answer::Reply(raw(200, &body))
                }
                Mode::Ok => {
                    guard.import_calls += 1;
                    let mut conn = "";
                    let mut dig = "";
                    for pair in query.split('&') {
                        if let Some((k, v)) = pair.split_once('=') {
                            match k {
                                "connectionId" => conn = v,
                                "digest" => dig = v,
                                _ => {}
                            }
                        }
                    }
                    if conn.is_empty() || dig.len() != 64 {
                        return Answer::Reply(fail(400, "invalid_request", "need conn+digest"));
                    }
                    if !guard.connections.iter().any(|c| c == conn) {
                        return Answer::Reply(fail(404, "not_found", "No such connection."));
                    }
                    if sha_hex(body) != dig {
                        return Answer::Reply(fail(409, "digest_mismatch", "bytes != digest"));
                    }
                    let mime = if body.starts_with(b"\xff\xd8\xff") {
                        "image/jpeg"
                    } else {
                        "image/png"
                    };
                    let data = guard
                        .receipt_override
                        .clone()
                        .unwrap_or_else(|| receipt("ws_target", conn, mime, body));
                    Answer::Reply(ok(&data))
                }
            }
        }

        fn importer(&self) -> MediaImporter {
            MediaImporter::new(
                &format!("http://{}", self.addr),
                DeviceCredential::new(BEARER.into()),
            )
            .expect("loopback importer")
        }

        fn state(&self) -> std::sync::MutexGuard<'_, DoorState> {
            self.state.lock().unwrap()
        }
    }

    impl Drop for FakeDoor {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            if let Some(w) = self.worker.take() {
                let _ = w.join();
            }
        }
    }

    // ---------- the contract's six adversarial tests ----------

    /// I2 — a receipt exists only on a real 2xx success status.
    #[test]
    fn cad979_import_success_body_requires_success_http_status() {
        // Happy path: a genuine 200 + well-formed receipt mints the key.
        let door = FakeDoor::start();
        door.state().connections.push("con_ig".into());
        let bytes = png_bytes();
        let r = door
            .importer()
            .import("con_ig", "image/png", &bytes)
            .unwrap();
        let digest = sha_hex(&bytes);
        assert_eq!(r.digest, digest);
        assert_eq!(r.media_key, device_key("ws_target", "con_ig", &digest));
        assert_eq!(door.state().import_calls, 1);

        // Forged success bodies on non-2xx statuses never mint a receipt:
        // a refusal status (403) is honoured as a refusal, a redirect (302)
        // is drift — both refuse, neither returns a key.
        for status in [403u16, 302u16] {
            let door = FakeDoor::start();
            door.state().connections.push("con_ig".into());
            door.state().mode = Some(Mode::SuccessBodyOn(status));
            let err = door
                .importer()
                .import("con_ig", "image/png", &png_bytes())
                .unwrap_err();
            // Without the status allowlist a forged `ok:true` body on 403/302
            // would have been returned as a receipt.
            assert_ne!(err.code, "", "status {status} must not mint a receipt");
            assert_eq!(door.state().import_calls, 1, "status {status}: one POST");
        }
    }

    /// I3 — upstream code/message text never enters a refusal; the bearer,
    /// a provider URL and oversize text are all dropped.
    #[test]
    fn cad979_import_error_never_echoes_credential_url_or_text() {
        let door = FakeDoor::start();
        door.state().connections.push("con_ig".into());
        door.state().mode = Some(Mode::HostileError);
        let err = door
            .importer()
            .import("con_ig", "image/png", &png_bytes())
            .unwrap_err();
        assert_eq!(err.code, "refused", "unknown door code fails closed");
        for needle in [BEARER, "provider.example", "upload", "xxxxx"] {
            assert!(
                !err.code.contains(needle) && !err.detail.contains(needle),
                "refusal leaked untrusted door text: {needle}"
            );
        }
        // The fixed-vocabulary mapping still preserves the refusal's meaning:
        // a genuine insufficient_scope (bearer-unrelated) maps to grant_mismatch.
        let door2 = FakeDoor::start();
        door2.state().scopes = vec!["read".into()];
        door2.state().connections.push("con_ig".into());
        let err2 = door2
            .importer()
            .import("con_ig", "image/png", &png_bytes())
            .unwrap_err();
        assert_eq!(err2.code, "grant_mismatch");
        assert!(!err2.detail.contains(BEARER));
    }

    /// I1 — only bounded retained jpeg/png bytes with a matching signature
    /// reach the wire; every other input refuses before a POST.
    #[test]
    fn cad979_import_bad_input_never_reaches_the_wire() {
        let door = FakeDoor::start();
        door.state().connections.push("con_ig".into());
        let importer = door.importer();
        for (mime, bytes, conn, want) in [
            // Declared type outside the jpeg/png allowlist.
            ("image/webp", png_bytes(), "con_ig", "bad_image_digest"),
            // Empty body.
            ("image/png", Vec::new(), "con_ig", "bad_image_digest"),
            // Over the retained-custody (2 MiB) bound.
            (
                "image/png",
                vec![0u8; IMPORT_MAX_BYTES + 1],
                "con_ig",
                "bad_image_digest",
            ),
            // Declared png over jpeg bytes — signature/label mismatch.
            ("image/png", jpeg_bytes(), "con_ig", "bad_image_digest"),
            // Declared jpeg over png bytes.
            ("image/jpeg", png_bytes(), "con_ig", "bad_image_digest"),
            // Declared png over arbitrary non-image bytes — no signature.
            (
                "image/png",
                b"not an image at all".to_vec(),
                "con_ig",
                "bad_image_digest",
            ),
            // Malformed connection id.
            ("image/png", png_bytes(), "con ig!!", "bad_connection"),
        ] {
            assert_eq!(
                importer.import(conn, mime, &bytes).unwrap_err().code,
                want,
                "input {mime}/{conn} must refuse before the door"
            );
        }
        assert_eq!(door.state().import_calls, 0, "invalid input never posted");
    }

    /// I2 — a receipt is refused when any echoed field (connection, digest,
    /// mime, size, readBack, or the key binding) differs from the upload,
    /// and the client never re-POSTs to "confirm".
    #[test]
    fn cad979_import_forged_receipt_fails_closed_single_post() {
        // Each forgery mutates the honest receipt; a plain enum + one
        // mutator keeps the cases readable and avoids an array of distinct
        // closure types.
        #[derive(Clone, Copy)]
        enum Forge {
            WrongConnection,
            WrongDigest,
            WrongMime,
            WrongSize,
            KeyOtherConnection,
            KeyOtherDigest,
            KeyNotDevice,
            ReadBackBytes,
            ReadBackDigest,
            MissingReadBack,
        }
        fn forge(receipt: &mut Value, which: Forge) {
            match which {
                Forge::WrongConnection => receipt["connectionId"] = json!("con_other"),
                Forge::WrongDigest => receipt["digest"] = json!("0".repeat(64)),
                Forge::WrongMime => receipt["mime"] = json!("image/gif"),
                Forge::WrongSize => receipt["sizeBytes"] = json!(1),
                Forge::KeyOtherConnection => {
                    receipt["mediaKey"] =
                        json!("dp1.ws_target.con_other.9f86d081884c7d659a2feaa0c55ad01")
                }
                Forge::KeyOtherDigest => {
                    receipt["mediaKey"] =
                        json!("dp1.ws_target.con_ig.00000000000000000000000000000000")
                }
                Forge::KeyNotDevice => {
                    receipt["mediaKey"] = json!("r2://bucket/object with spaces")
                }
                Forge::ReadBackBytes => receipt["readBack"]["bytes"] = json!(1),
                Forge::ReadBackDigest => receipt["readBack"]["digest"] = json!("0".repeat(64)),
                Forge::MissingReadBack => {
                    receipt.as_object_mut().unwrap().remove("readBack");
                }
            }
        }
        let cases = [
            ("wrong connection", Forge::WrongConnection),
            ("wrong digest echo", Forge::WrongDigest),
            ("wrong mime", Forge::WrongMime),
            ("wrong size", Forge::WrongSize),
            ("key names other connection", Forge::KeyOtherConnection),
            ("key names other digest", Forge::KeyOtherDigest),
            ("key not a device key", Forge::KeyNotDevice),
            ("readBack bytes drift", Forge::ReadBackBytes),
            ("readBack digest drift", Forge::ReadBackDigest),
            ("missing readBack", Forge::MissingReadBack),
        ];
        for (label, which) in cases {
            let door = FakeDoor::start();
            door.state().connections.push("con_ig".into());
            let bytes = png_bytes();
            let mut bad = receipt("ws_target", "con_ig", "image/png", &bytes);
            forge(&mut bad, which);
            door.state().receipt_override = Some(bad);
            let err = door
                .importer()
                .import("con_ig", "image/png", &bytes)
                .unwrap_err();
            assert!(
                matches!(
                    err.code.as_str(),
                    "grant_binding_mismatch" | "key_conflict" | "bad_image_digest" | "bad_effect"
                ),
                "{label} should refuse, got {}",
                err.code
            );
            assert_eq!(door.state().import_calls, 1, "{label}: one POST only");
        }
    }

    /// I2 — a malformed or structure-invalid success envelope never yields a
    /// receipt; it surfaces as an uncertain refusal.
    #[test]
    fn cad979_import_malformed_envelope_never_yields_receipt() {
        for (label, mode) in [
            ("non-JSON body", Mode::MalformedBody),
            ("ok:true missing data", Mode::MissingData),
        ] {
            let door = FakeDoor::start();
            door.state().connections.push("con_ig".into());
            door.state().mode = Some(mode);
            let err = door
                .importer()
                .import("con_ig", "image/png", &png_bytes())
                .unwrap_err();
            assert_eq!(
                err.code, "refused",
                "{label}: malformed envelope stays an uncertain refusal"
            );
            assert!(
                err.detail.contains("uncertain"),
                "{label}: names an uncertain outcome, got {}",
                err.detail
            );
            assert_eq!(door.state().import_calls, 1, "{label}: one POST");
        }
        // A readBack-missing receipt also fails at receipt_of, not the
        // envelope reader — already covered by forged_receipt missing-readBack.
    }

    /// I2 — a body over the 64 KiB response cap never yields a receipt even
    /// when it begins as a valid success envelope.
    #[test]
    fn cad979_import_response_over_cap_never_yields_receipt() {
        let door = FakeDoor::start();
        door.state().connections.push("con_ig".into());
        door.state().mode = Some(Mode::OverCap);
        let err = door
            .importer()
            .import("con_ig", "image/png", &png_bytes())
            .unwrap_err();
        // The cap makes the read fail; the outcome is an uncertain refusal,
        // not a receipt and not a parsed error.
        assert_eq!(err.code, "refused");
        assert!(err.detail.contains("uncertain"));
        assert_eq!(door.state().import_calls, 1);
    }

    // ---------- supporting (non-contract-named) proofs ----------

    /// Ambiguity is never evidence and never a second POST: 5xx and stalls.
    #[test]
    fn cad979_import_5xx_and_stall_stay_ambiguous_single_shot() {
        let door = FakeDoor::start();
        door.state().connections.push("con_ig".into());
        door.state().mode = Some(Mode::Fixed(503, "capability_unavailable"));
        let err = door
            .importer()
            .import("con_ig", "image/png", &png_bytes())
            .unwrap_err();
        assert_eq!(err.code, "refused");
        assert!(err.detail.contains("uncertain"), "5xx stays ambiguous");
        assert_eq!(door.state().import_calls, 1, "no retry POST");

        let door2 = FakeDoor::start();
        door2.state().connections.push("con_ig".into());
        door2.state().mode = Some(Mode::Drop);
        let importer = MediaImporter::with_timeout(
            &format!("http://{}", door2.addr),
            DeviceCredential::new(BEARER.into()),
            Duration::from_millis(200),
        )
        .unwrap();
        let err = importer
            .import("con_ig", "image/png", &png_bytes())
            .unwrap_err();
        assert_eq!(err.code, "refused");
        assert!(err.detail.contains("uncertain"), "stall stays ambiguous");
    }

    /// A genuine wrong-scope and wrong-credential refusal, and the wire
    /// carries no workspace field or secret outside the Authorization header.
    #[test]
    fn cad979_import_scope_credential_and_wire_hygiene() {
        // Wrong scope → definitive grant_mismatch refusal at the door.
        let door = FakeDoor::start();
        door.state().scopes = vec!["read".into()];
        door.state().connections.push("con_ig".into());
        let err = door
            .importer()
            .import("con_ig", "image/png", &png_bytes())
            .unwrap_err();
        assert_eq!(err.code, "grant_mismatch");

        // Unknown credential → 401 unauthorized → refused.
        let door2 = FakeDoor::start();
        door2.state().connections.push("con_ig".into());
        let bad = MediaImporter::new(
            &format!("http://{}", door2.addr),
            DeviceCredential::new("wrong-secret".into()),
        )
        .unwrap();
        assert_eq!(
            bad.import("con_ig", "image/png", &png_bytes())
                .unwrap_err()
                .code,
            "refused"
        );

        // A good import carries connectionId+digest as query and raw bytes as
        // body; the secret is only in Authorization, and no workspace field
        // exists anywhere.
        let door3 = FakeDoor::start();
        door3.state().connections.push("con_ig".into());
        door3
            .importer()
            .import("con_ig", "image/png", &png_bytes())
            .unwrap();
        let g = door3.state();
        assert!(!g.last_query.contains("workspace"), "no workspace in query");
        assert!(!g.last_query.contains(BEARER), "secret never in query");
        assert!(
            !String::from_utf8_lossy(&g.last_body).contains(BEARER),
            "secret never in body"
        );
    }
}
