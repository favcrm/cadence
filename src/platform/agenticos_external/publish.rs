//! CAD-771 slice 1: generic exact-destination send contract machinery.
//!
//! A read-only mirror of the versioned device publish wire contract as
//! LANDED in `agenticos-stack/agenticos-v2` staging at
//! `3d6ced8581d637c39899e71c1187eae831759901` (PR #214 merge; revalidated
//! 2026-09-29 — the contract paths are byte-identical to the reviewed
//! head `12953144c50d13075af2323a2e09a70de9f72b87`, the only staging delta
//! being the unrelated AOS-96 wake-barrier fix). This module is pure validation plus an in-memory fake ledger for
//! adversarial tests. It performs no network, no custody and no live post.
//!
//! Conformance notes (pinned, do not drift without revalidation):
//! - wire version is exactly `"1"` on every document;
//! - caption bound counts Unicode scalar values (`caption.chars().count()`),
//!   max 8000 — never UTF-16 code units, so astral characters count once;
//! - caption digest input is the UTF-8 bytes of the exact approved caption
//!   (this differs from the legacy container-only `publish_content_digest`
//!   in `super::super::agenticos`, which uses UTF-16 lengths — that helper
//!   is NOT the device path and must never be substituted here);
//! - backend image ceiling is 10 MiB on raw bytes; the 512 KiB reviewed-still
//!   pilot bound lives in Cadence only and is referenced here as a literal,
//!   never enforced backend-side;
//! - media keys are content-addressed (`dp1.<workspace>.<connection>.<digest32>`);
//!   worker paths and arbitrary URLs never parse;
//! - `publish.send` is runtime-audience and never shares a credential with
//!   `provider.read`/`provider.draft` — the pilot credential gains no send
//!   authority, silently or otherwise.

use std::collections::HashMap;
use std::sync::Mutex;

use sha2::{Digest, Sha256};

/// Wire version every v1 document carries.
pub const DEVICE_PUBLISH_VERSION: &str = "1";

/// Backend image ceiling: 10 MiB on raw binary bytes at import.
pub const DEVICE_PUBLISH_MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;

/// Pilot reviewed-still bound (Cadence-side only, prose + test literal).
/// Never enforced here; the backend ceiling above stays the backstop.
pub const PILOT_REVIEWED_STILL_BYTES: usize = 512 * 1024;

/// Caption bound: max Unicode scalar values.
pub const DEVICE_PUBLISH_MAX_CAPTION_SCALARS: usize = 8000;

/// Meta destinations a device send may target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Toolkit {
    Instagram,
    Facebook,
}

impl Toolkit {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Instagram => "instagram",
            Self::Facebook => "facebook",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "instagram" => Some(Self::Instagram),
            "facebook" => Some(Self::Facebook),
            _ => None,
        }
    }
}

/// One authorized destination from owner-authorized discovery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Destination {
    pub connection_id: String,
    pub toolkit: Toolkit,
    pub display_name: String,
    pub destination_id: String,
    pub status_active: bool,
    pub available: bool,
}

impl Destination {
    /// True only when active, linked, and open.
    pub fn publishable(&self) -> bool {
        self.available && self.status_active && !self.destination_id.is_empty()
    }
}

/// Durable send states Cadence reconciles against after restart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishState {
    Posted,
    Processing,
    Refused,
    ReconnectNeeded,
}

impl PublishState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Posted => "posted",
            Self::Processing => "processing",
            Self::Refused => "refused",
            Self::ReconnectNeeded => "reconnect_needed",
        }
    }
}

/// Failing verdict for a gate refusal (never a second provider call).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub code: &'static str,
    pub detail: String,
}

impl Refusal {
    fn new(code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: detail.into(),
        }
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.detail)
    }
}

impl std::error::Error for Refusal {}

// ---------- shape validators (same bounds as the pinned contract) ----------

pub fn valid_connection_id(raw: &str) -> bool {
    (1..=80).contains(&raw.len())
        && raw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}

pub fn valid_digest(raw: &str) -> bool {
    raw.len() == 64 && raw.bytes().all(|b| b.is_ascii_hexdigit())
}

pub fn valid_media_key(raw: &str) -> bool {
    (1..=200).contains(&raw.len())
        && raw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'~' | b'-'))
}

pub fn valid_idempotency_key(raw: &str) -> bool {
    (8..=128).contains(&raw.len())
        && raw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}

pub fn valid_grant_id(raw: &str) -> bool {
    let body = raw.strip_prefix("dpq_").unwrap_or("");
    (8..=64).contains(&body.len())
        && body
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}

/// Unicode scalar count (astral characters count once).
pub fn caption_scalar_len(caption: &str) -> usize {
    caption.chars().count()
}

pub fn valid_caption(caption: &str) -> bool {
    !caption.is_empty() && caption_scalar_len(caption) <= DEVICE_PUBLISH_MAX_CAPTION_SCALARS
}

/// Caption digest input: UTF-8 bytes of the exact approved caption.
pub fn caption_digest_of(caption: &str) -> String {
    sha256_hex(caption.as_bytes())
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Content-addressed media key bound to company, destination and digest.
pub fn device_media_key(workspace: &str, connection: &str, digest: &str) -> Option<String> {
    if !valid_workspace(workspace) || !valid_connection_id(connection) || !valid_digest(digest) {
        return None;
    }
    Some(format!("dp1.{workspace}.{connection}.{}", &digest[..32]))
}

fn valid_workspace(raw: &str) -> bool {
    (1..=64).contains(&raw.len())
        && raw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}

/// A media key authorizes only its own company, destination and digest prefix.
pub fn media_key_authorizes(
    media_key: &str,
    workspace: &str,
    connection: &str,
    digest: &str,
) -> bool {
    let parts: Vec<&str> = media_key.split('.').collect();
    if parts.len() != 4 || parts[0] != "dp1" {
        return false;
    }
    if parts[1] != workspace || parts[2] != connection {
        return false;
    }
    let prefix = parts[3];
    prefix.len() == 32
        && prefix.bytes().all(|b| b.is_ascii_hexdigit())
        && digest.len() == 64
        && digest.starts_with(prefix)
}

// ---------- frozen send binding ----------

/// The exact frozen binding one stable key names: one destination, one
/// caption digest, one image digest, one approved Cadence run/effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendBinding {
    pub key: String,
    pub connection_id: String,
    pub destination_id: String,
    pub toolkit: Toolkit,
    pub caption_digest: String,
    pub image_digest: Option<String>,
    pub cadence_run_id: String,
    pub cadence_effect_id: String,
    pub grant_id: String,
}

impl SendBinding {
    pub fn validate(&self) -> Result<(), Refusal> {
        if !valid_idempotency_key(&self.key) {
            return Err(Refusal::new("bad_key", "idempotency key shape is invalid"));
        }
        if !valid_connection_id(&self.connection_id) {
            return Err(Refusal::new(
                "bad_connection",
                "connection id shape is invalid",
            ));
        }
        if self.destination_id.is_empty() || self.destination_id.len() > 120 {
            return Err(Refusal::new(
                "bad_destination",
                "destination id is missing or oversized",
            ));
        }
        if !valid_digest(&self.caption_digest) {
            return Err(Refusal::new(
                "bad_caption_digest",
                "caption digest must be 64 lowercase hex",
            ));
        }
        if let Some(image) = &self.image_digest {
            if !valid_digest(image) {
                return Err(Refusal::new(
                    "bad_image_digest",
                    "image digest must be 64 lowercase hex",
                ));
            }
        }
        if self.cadence_run_id.is_empty() || self.cadence_run_id.len() > 120 {
            return Err(Refusal::new("bad_run", "Cadence run identity is invalid"));
        }
        if self.cadence_effect_id.is_empty() || self.cadence_effect_id.len() > 120 {
            return Err(Refusal::new(
                "bad_effect",
                "Cadence effect identity is invalid",
            ));
        }
        if !valid_grant_id(&self.grant_id) {
            return Err(Refusal::new("bad_grant", "grant id shape is invalid"));
        }
        // Instagram needs an image; Facebook Pages may post text.
        if self.toolkit == Toolkit::Instagram && self.image_digest.is_none() {
            return Err(Refusal::new(
                "image_required",
                "Instagram send needs a reviewed image digest",
            ));
        }
        Ok(())
    }
}

/// Owner-minted explicit send grant (workspace-bound, revocable, bounded).
#[derive(Debug, Clone)]
pub struct SendGrant {
    pub id: String,
    pub workspace_id: String,
    pub connection_id: String,
    pub destination_id: String,
    pub toolkit: Toolkit,
    pub caption_digest: String,
    pub image_digest: Option<String>,
    pub cadence_approval_id: String,
    pub max_uses: u32,
    pub remaining_uses: u32,
    pub revoked: bool,
    pub not_before_epoch: i64,
    pub expires_at_epoch: i64,
}

impl SendGrant {
    /// Server-side gate for one presented binding at `now_epoch`.
    /// `credential_workspace` is the workspace the device credential binds
    /// (never a wire field): a grant minted for another workspace never
    /// authorizes, however exact the rest of the binding.
    pub fn authorize(
        &self,
        binding: &SendBinding,
        credential_workspace: &str,
        now_epoch: i64,
    ) -> Result<(), Refusal> {
        if self.revoked {
            return Err(Refusal::new("grant_revoked", "send grant was revoked"));
        }
        if self.workspace_id != credential_workspace {
            return Err(Refusal::new(
                "cross_workspace",
                "send grant belongs to another workspace",
            ));
        }
        if self.id != binding.grant_id {
            return Err(Refusal::new("grant_mismatch", "grant id does not match"));
        }
        // The grant row itself is the source of truth; a binding that
        // names another connection/destination/digest never authorizes.
        if self.connection_id != binding.connection_id
            || self.destination_id != binding.destination_id
            || self.toolkit != binding.toolkit
            || self.caption_digest != binding.caption_digest
            || self.image_digest != binding.image_digest
        {
            return Err(Refusal::new(
                "grant_binding_mismatch",
                "grant does not cover this exact destination/content",
            ));
        }
        if self.max_uses == 0 || self.max_uses > 10 {
            return Err(Refusal::new(
                "grant_bounds",
                "send grant bound is outside 1..=10 uses",
            ));
        }
        if self.remaining_uses == 0 {
            return Err(Refusal::new(
                "grant_exhausted",
                "send grant has no uses left",
            ));
        }
        if now_epoch < self.not_before_epoch || now_epoch > self.expires_at_epoch {
            return Err(Refusal::new(
                "grant_window",
                "send grant is outside its validity window",
            ));
        }
        if self.cadence_approval_id.is_empty() {
            return Err(Refusal::new(
                "grant_approval",
                "send grant names no operator approval",
            ));
        }
        Ok(())
    }
}

// ---------- fake provider ledger (tests only) ----------

/// Recorded outcome for one stable key.
#[derive(Debug, Clone)]
pub struct LedgerOutcome {
    pub state: PublishState,
    pub permalink: Option<String>,
    pub destination_id: String,
    pub caption_digest: String,
    pub image_digest: Option<String>,
    /// Exact provider payloads, oldest first, replayed byte-exact.
    pub provider_payload: Option<String>,
    pub provider_ids: Vec<String>,
    pub repeated: bool,
}

/// In-memory fake of the upstream send ledger + provider. Deterministic,
/// no network. Counts provider calls so tests prove "no second call".
/// Dispatch stays closed until explicitly enabled — mirroring the landed
/// `DEVICE_PUBLISH_SEND_ENABLED` default-off gate: a disabled execution
/// validates everything, mutates nothing, calls no provider, and refuses
/// with `send_disabled`. Preflight staging is unaffected.
pub struct FakePublishLedger {
    inner: Mutex<FakeInner>,
}

#[derive(Default)]
struct FakeInner {
    rows: HashMap<String, (SendBinding, LedgerOutcome)>,
    provider_calls: u64,
    /// Keys whose provider accepted but whose response was "lost".
    lost_response: HashMap<String, LedgerOutcome>,
    /// Dispatch gate: closed until explicitly enabled (landed default-off).
    send_enabled: bool,
}

impl FakePublishLedger {
    /// Closed dispatch, mirroring the landed default-off gate.
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(FakeInner::default()),
        }
    }

    /// Test-only enablement: models operator-confirmed activation. The
    /// production path has no send registration at all until AOS-94 is
    /// revalidated and explicitly enabled.
    pub fn enabled() -> Self {
        let ledger = Self::new();
        ledger.set_send_enabled(true);
        ledger
    }

    pub fn set_send_enabled(&self, enabled: bool) {
        self.inner.lock().unwrap().send_enabled = enabled;
    }

    pub fn provider_calls(&self) -> u64 {
        self.inner.lock().unwrap().provider_calls
    }

    /// Preflight stages a send without claiming execution or calling the
    /// provider. Same key + different binding fails; same key + same
    /// binding replays the staged verdict.
    pub fn preflight(
        &self,
        binding: &SendBinding,
        destination: &Destination,
        grant: &SendGrant,
        credential_workspace: &str,
        now_epoch: i64,
    ) -> Result<bool, Refusal> {
        binding.validate()?;
        check_destination(binding, destination)?;
        grant.authorize(binding, credential_workspace, now_epoch)?;
        let inner = self.inner.lock().unwrap();
        if let Some((recorded, _)) = inner.rows.get(&binding.key) {
            if recorded != binding {
                return Err(Refusal::new(
                    "key_conflict",
                    "same key with different content or destination",
                ));
            }
            return Ok(true);
        }
        Ok(false)
    }

    /// Execution claims one grant use and calls the fake provider exactly
    /// once per key. Replays return the recorded outcome without another
    /// call. `refuse` simulates a provider refusal; `lose_response`
    /// simulates timeout-after-accept (state stays `processing` until
    /// `status` reconciles it).
    pub fn execute(
        &self,
        binding: &SendBinding,
        destination: &Destination,
        grant: &mut SendGrant,
        credential_workspace: &str,
        now_epoch: i64,
        behavior: FakeProviderBehavior,
    ) -> Result<LedgerOutcome, Refusal> {
        binding.validate()?;
        check_destination(binding, destination)?;
        grant.authorize(binding, credential_workspace, now_epoch)?;
        if !self.inner.lock().unwrap().send_enabled {
            // Default-off gate: validated everything, mutate nothing.
            return Err(Refusal::new(
                "send_disabled",
                "send dispatch is not enabled",
            ));
        }
        let mut inner = self.inner.lock().unwrap();
        if let Some((recorded, outcome)) = inner.rows.get(&binding.key) {
            if recorded != binding {
                return Err(Refusal::new(
                    "key_conflict",
                    "same key with different content or destination",
                ));
            }
            let mut replay = outcome.clone();
            replay.repeated = true;
            return Ok(replay);
        }
        match behavior {
            FakeProviderBehavior::Refuse => {
                let outcome = LedgerOutcome {
                    state: PublishState::Refused,
                    permalink: None,
                    destination_id: binding.destination_id.clone(),
                    caption_digest: binding.caption_digest.clone(),
                    image_digest: binding.image_digest.clone(),
                    provider_payload: Some(r#"{"refused":"policy"}"#.to_owned()),
                    provider_ids: vec![],
                    repeated: false,
                };
                inner
                    .rows
                    .insert(binding.key.clone(), (binding.clone(), outcome.clone()));
                grant.remaining_uses = grant.remaining_uses.saturating_sub(1);
                Ok(outcome)
            }
            FakeProviderBehavior::LoseResponseAfterAccept => {
                inner.provider_calls += 1;
                let outcome = LedgerOutcome {
                    state: PublishState::Processing,
                    permalink: None,
                    destination_id: binding.destination_id.clone(),
                    caption_digest: binding.caption_digest.clone(),
                    image_digest: binding.image_digest.clone(),
                    provider_payload: None,
                    provider_ids: vec!["provider-post-pending-1".to_owned()],
                    repeated: false,
                };
                inner
                    .lost_response
                    .insert(binding.key.clone(), outcome.clone());
                inner
                    .rows
                    .insert(binding.key.clone(), (binding.clone(), outcome.clone()));
                grant.remaining_uses = grant.remaining_uses.saturating_sub(1);
                Ok(outcome)
            }
            FakeProviderBehavior::Post => {
                inner.provider_calls += 1;
                let permalink = format!(
                    "https://www.instagram.com/p/{}/",
                    &binding.caption_digest[..11]
                );
                let payload = format!(
                    r#"{{"id":"provider-post-1","permalink":"{permalink}","caption_digest":"{}"}}"#,
                    binding.caption_digest
                );
                let outcome = LedgerOutcome {
                    state: PublishState::Posted,
                    permalink: Some(permalink),
                    destination_id: binding.destination_id.clone(),
                    caption_digest: binding.caption_digest.clone(),
                    image_digest: binding.image_digest.clone(),
                    provider_payload: Some(payload),
                    provider_ids: vec!["provider-post-1".to_owned()],
                    repeated: false,
                };
                inner
                    .rows
                    .insert(binding.key.clone(), (binding.clone(), outcome.clone()));
                grant.remaining_uses = grant.remaining_uses.saturating_sub(1);
                Ok(outcome)
            }
        }
    }

    /// Durable status query. Reconciles a lost-response row into `posted`
    /// with byte-exact provider evidence — never a second provider call
    /// and never a bare success string.
    pub fn status(&self, key: &str) -> Result<LedgerOutcome, Refusal> {
        if !valid_idempotency_key(key) {
            return Err(Refusal::new("bad_key", "status key shape is invalid"));
        }
        let mut inner = self.inner.lock().unwrap();
        if let Some(pending) = inner.lost_response.remove(key) {
            let (binding, _) = inner
                .rows
                .get(key)
                .cloned()
                .ok_or_else(|| Refusal::new("unknown_key", "no send under this key"))?;
            let permalink = format!(
                "https://www.instagram.com/p/{}/",
                &binding.caption_digest[..11]
            );
            let payload = format!(
                r#"{{"id":"provider-post-1","permalink":"{permalink}","caption_digest":"{}"}}"#,
                binding.caption_digest
            );
            let reconciled = LedgerOutcome {
                state: PublishState::Posted,
                permalink: Some(permalink),
                destination_id: pending.destination_id.clone(),
                caption_digest: pending.caption_digest.clone(),
                image_digest: pending.image_digest.clone(),
                provider_payload: Some(payload),
                provider_ids: vec!["provider-post-1".to_owned()],
                repeated: true,
            };
            inner
                .rows
                .insert(key.to_owned(), (binding, reconciled.clone()));
            return Ok(reconciled);
        }
        inner
            .rows
            .get(key)
            .map(|(_, outcome)| {
                let mut replay = outcome.clone();
                replay.repeated = true;
                replay
            })
            .ok_or_else(|| Refusal::new("unknown_key", "no send under this key"))
    }
}

impl Default for FakePublishLedger {
    fn default() -> Self {
        Self::new()
    }
}

/// Deterministic fake provider behaviors for tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FakeProviderBehavior {
    Post,
    Refuse,
    LoseResponseAfterAccept,
}

fn check_destination(binding: &SendBinding, destination: &Destination) -> Result<(), Refusal> {
    if destination.connection_id != binding.connection_id {
        return Err(Refusal::new(
            "wrong_connection",
            "binding names a different connection than discovery",
        ));
    }
    if destination.destination_id != binding.destination_id {
        return Err(Refusal::new(
            "wrong_destination",
            "binding destination differs from discovered account",
        ));
    }
    if destination.toolkit != binding.toolkit {
        return Err(Refusal::new(
            "wrong_toolkit",
            "binding toolkit differs from discovered destination",
        ));
    }
    if !destination.publishable() {
        return Err(Refusal::new(
            "not_publishable",
            "destination is not active, linked and open",
        ));
    }
    Ok(())
}

// ---------- frozen scheduled intent (slice 2 shape, defined now) ----------

/// Durable states for a scheduled external post.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScheduledState {
    Queued,
    Cancelled,
    Processing,
    Posted,
}

/// Frozen operator-approved intent for one scheduled send. Durability,
/// cancellation and dispatch rechecks land in slice 2; the shape is
/// frozen here so adapter, UI and tests share one identity.
#[derive(Debug, Clone)]
pub struct ScheduledIntent {
    pub intent_id: String,
    pub binding: SendBinding,
    pub due_epoch: i64,
    pub timezone: String,
    pub state: ScheduledState,
    pub binding_revision: String,
}

impl ScheduledIntent {
    pub fn validate(&self) -> Result<(), Refusal> {
        self.binding.validate()?;
        if self.intent_id.is_empty() || self.intent_id.len() > 120 {
            return Err(Refusal::new("bad_intent", "scheduled intent id is invalid"));
        }
        if self.timezone.is_empty() || self.timezone.len() > 64 {
            return Err(Refusal::new("bad_timezone", "timezone is invalid"));
        }
        if self.binding_revision.is_empty() {
            return Err(Refusal::new("bad_revision", "binding revision is required"));
        }
        Ok(())
    }

    /// Operator cancellation before dispatch. Any other state refuses.
    pub fn cancel(&mut self) -> Result<(), Refusal> {
        if self.state != ScheduledState::Queued {
            return Err(Refusal::new(
                "cancel_closed",
                "only a queued intent can be cancelled",
            ));
        }
        self.state = ScheduledState::Cancelled;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(byte: u8) -> String {
        sha256_hex(&[byte])
    }

    #[test]
    fn toolkit_round_trip_and_bounds() {
        assert_eq!(Toolkit::parse("instagram"), Some(Toolkit::Instagram));
        assert_eq!(Toolkit::parse("facebook"), Some(Toolkit::Facebook));
        assert_eq!(Toolkit::parse("tiktok"), None);
        assert_eq!(Toolkit::Instagram.as_str(), "instagram");
        assert_eq!(Toolkit::Facebook.as_str(), "facebook");
        assert_eq!(DEVICE_PUBLISH_MAX_IMAGE_BYTES, 10 * 1024 * 1024);
        assert!(valid_media_key(
            "dp1.ws_harbour.con_harbour_ig.9f86d081884c7d65"
        ));
        assert!(!valid_media_key("r2://bucket/object with spaces"));
        assert_eq!(PublishState::ReconnectNeeded.as_str(), "reconnect_needed");
    }

    #[test]
    fn caption_counts_scalars_not_utf16_units() {
        // "é" is 1 scalar but 2 UTF-8 bytes / 1 UTF-16 unit; "🦊" is 1
        // scalar but 2 UTF-16 units. The bound must count scalars.
        assert_eq!(caption_scalar_len("é🦊"), 2);
        assert_eq!("é🦊".encode_utf16().count(), 3);
        assert!(valid_caption("é🦊"));
        assert!(!valid_caption(""));
        assert!(!valid_caption(
            &"x".repeat(DEVICE_PUBLISH_MAX_CAPTION_SCALARS + 1)
        ));
        // Astral characters count once toward the bound.
        assert!(valid_caption(
            &"🦊".repeat(DEVICE_PUBLISH_MAX_CAPTION_SCALARS)
        ));
    }

    #[test]
    fn caption_digest_is_utf8_not_utf16_length_prefixed() {
        // Pinned rule: digest input is UTF-8 bytes of the exact caption.
        assert_eq!(caption_digest_of("hello"), sha256_hex("hello".as_bytes()));
        assert_ne!(
            caption_digest_of("🦊"),
            sha256_hex(&[0x3D, 0xD8, 0x66, 0xDE])
        );
    }

    #[test]
    fn media_key_binds_company_destination_and_digest() {
        let digest_value = digest(9);
        let key = device_media_key("ws_harbour", "con_harbour_ig", &digest_value).unwrap();
        assert!(media_key_authorizes(
            &key,
            "ws_harbour",
            "con_harbour_ig",
            &digest_value
        ));
        assert!(!media_key_authorizes(
            &key,
            "ws_other",
            "con_harbour_ig",
            &digest_value
        ));
        assert!(!media_key_authorizes(
            &key,
            "ws_harbour",
            "con_other",
            &digest_value
        ));
        assert!(!media_key_authorizes(
            "r2://bucket/object",
            "ws_harbour",
            "con_harbour_ig",
            &digest_value
        ));
        assert!(!media_key_authorizes(
            "https://cdn.example.test/image.png",
            "ws_harbour",
            "con_harbour_ig",
            &digest_value
        ));
    }
}
