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
    pub code: String,
    pub detail: String,
}

impl Refusal {
    pub fn new(code: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            detail: detail.into(),
        }
    }

    /// The code vocabulary the landed contract refuses with. Unknown wire
    /// codes fail closed to a generic refusal rather than inventing a code.
    pub fn code_for(name: &str) -> &'static str {
        match name {
            "bad_key" => "bad_key",
            "bad_connection" => "bad_connection",
            "bad_destination" => "bad_destination",
            "bad_caption_digest" => "bad_caption_digest",
            "bad_image_digest" => "bad_image_digest",
            "bad_run" => "bad_run",
            "bad_effect" => "bad_effect",
            "bad_grant" => "bad_grant",
            "bad_intent" => "bad_intent",
            "bad_revision" => "bad_revision",
            "bad_timezone" => "bad_timezone",
            "cancel_closed" => "cancel_closed",
            "cross_workspace" => "cross_workspace",
            "grant_approval" => "grant_approval",
            "grant_binding_mismatch" => "grant_binding_mismatch",
            "grant_bounds" => "grant_bounds",
            "grant_exhausted" => "grant_exhausted",
            "grant_mismatch" => "grant_mismatch",
            "grant_revoked" => "grant_revoked",
            "grant_window" => "grant_window",
            "image_required" => "image_required",
            "key_conflict" => "key_conflict",
            "not_publishable" => "not_publishable",
            "send_disabled" => "send_disabled",
            "unknown_key" => "unknown_key",
            "wrong_connection" => "wrong_connection",
            "wrong_destination" => "wrong_destination",
            "wrong_toolkit" => "wrong_toolkit",
            _ => "refused",
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

/// Lowercase 64-hex, exactly like the landed `sha256Schema`: uppercase
/// never verifies backend-side, so the gate refuses it here too.
pub fn valid_digest(raw: &str) -> bool {
    raw.len() == 64
        && raw
            .bytes()
            .all(|b: u8| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The one digest form the publish path freezes and compares: bare
/// 64-hex (callers validate with `valid_digest`), exactly what the door, the import receipt and
/// `SendBinding::validate` speak. Custody (`app_tool_results.asset_digest`)
/// keeps `sha256:<hex>`; an effect staged before CAD-1304 froze that form
/// and cannot be rewritten (its digest covers the frozen authority), so
/// every read side strips the prefix before comparing.
pub fn bare_digest(raw: &str) -> &str {
    raw.strip_prefix("sha256:").unwrap_or(raw)
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
        && prefix
            .bytes()
            .all(|b: u8| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        && digest.len() == 64
        && digest.starts_with(prefix)
}

/// CAD-979 v9: bind a key to its resolved remote AOS `connectionId` +
/// image digest prefix only — NOT the workspace. The key's `parts[1]` is
/// the send credential's upstream workspace, which Cadence never knows
/// locally (no local workspace authority); that binding is enforced
/// upstream at mint/grant (`SendGrant.authorize` `cross_workspace`), not
/// by a local compare. Freeze-time authorization binds connection+digest.
pub fn media_key_authorizes_connection(media_key: &str, connection: &str, digest: &str) -> bool {
    let parts: Vec<&str> = media_key.split('.').collect();
    if parts.len() != 4 || parts[0] != "dp1" {
        return false;
    }
    if parts[2] != connection {
        return false;
    }
    let prefix = parts[3];
    prefix.len() == 32
        && prefix
            .bytes()
            .all(|b: u8| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        && digest.len() == 64
        && digest.starts_with(prefix)
}

// ---------- frozen send binding ----------

/// Source authority for an exact frozen send. A standalone draft is never
/// disguised as a fabricated app run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PublicationSource {
    Run { run_id: String },
    SocialDraft { draft_id: String, revision: i64 },
}

/// The exact frozen binding one stable key names: one destination, one
/// caption digest, one image digest, and a real typed authority source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendBinding {
    pub key: String,
    pub connection_id: String,
    pub destination_id: String,
    pub toolkit: Toolkit,
    pub caption_digest: String,
    pub image_digest: Option<String>,
    pub source: PublicationSource,
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
        match &self.source {
            PublicationSource::Run { run_id } if run_id.is_empty() || run_id.len() > 120 => {
                return Err(Refusal::new("bad_run", "Cadence run identity is invalid"));
            }
            PublicationSource::SocialDraft { draft_id, revision }
                if draft_id.is_empty() || draft_id.len() > 120 || *revision < 1 =>
            {
                return Err(Refusal::new("bad_draft", "social draft source is invalid"));
            }
            _ => {}
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
        if self.cadence_approval_id.is_empty() || self.cadence_approval_id.len() > 120 {
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

    /// CAD-1041: the request keys the ledger has staged or sent — the
    /// send-now identity pin test reads them to prove only the named
    /// intent's key ever reached the provider.
    pub fn keys(&self) -> Vec<String> {
        self.inner.lock().unwrap().rows.keys().cloned().collect()
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

/// CAD-1291: the owner's standing publish grant for one destination, as the
/// hosted door lists it. The owner mints it once on AgenticOS (company,
/// connection, destination and toolkit scoped, no content digests,
/// revocable); Cadence never mints one. Each post is still approved
/// by the Cadence operator and re-checked here; AgenticOS re-checks the
/// destination, company and revocation at preflight and send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoundGrant {
    pub id: String,
    pub connection_id: String,
    pub destination_id: String,
    pub toolkit: String,
    pub revoked: bool,
}

impl FoundGrant {
    /// Strict wire parse: anything but a standing grant record is refused.
    pub fn from_wire(wire: &serde_json::Value) -> Option<Self> {
        let text = |field: &str| wire.get(field)?.as_str().map(str::to_owned);
        if text("kind")? != "standing" {
            return None;
        }
        Some(Self {
            id: text("id").filter(|id| valid_grant_id(id))?,
            connection_id: text("connectionId")?,
            destination_id: text("destinationId")?,
            toolkit: text("toolkit")?,
            revoked: !wire.get("revokedAt")?.is_null(),
        })
    }
}

/// The one account the approved post goes to.
#[derive(Debug, Clone)]
pub struct GrantWant<'a> {
    pub connection_id: &'a str,
    pub destination_id: &'a str,
    pub toolkit: &'a str,
}

/// Pick the live standing grant for exactly this destination. No grant, a
/// grant for another account or a revoked one never authorizes: the approved post stays unsent.
pub fn select_grant(found: &[FoundGrant], want: &GrantWant<'_>) -> Result<String, Refusal> {
    let own: Vec<&FoundGrant> = found
        .iter()
        .filter(|grant| {
            grant.connection_id == want.connection_id
                && grant.destination_id == want.destination_id
                && grant.toolkit == want.toolkit
        })
        .collect();
    if own.is_empty() {
        return Err(Refusal::new(
            "grant_required",
            "the owner has not allowed Cadence to publish to this account in AgenticOS",
        ));
    }
    let live: Vec<&&FoundGrant> = own.iter().filter(|grant| !grant.revoked).collect();
    if live.is_empty() {
        return Err(Refusal::new(
            "grant_revoked",
            "the owner revoked publishing to this account in AgenticOS",
        ));
    }
    Ok(live[0].id.clone())
}

/// Daemon-side dispatch observation: the party that speaks to the provider
/// door, so posted reports verify against evidence the daemon itself
/// observed — never operator-supplied JSON alone. Production leaves the
/// daemon without one until the send adapter lands; tests register a fake.
/// A forged receipt with matching binding fields but fabricated evidence
/// fails closed against the recorded bytes.
pub trait PublishSender: Send + Sync {
    /// Dispatch one exact binding against the provider door. The returned
    /// outcome is daemon-observed evidence, persisted before any report.
    fn execute(&self, binding: &SendBinding) -> Result<LedgerOutcome, Refusal>;
    /// Reconcile one stable key against the provider door. Never a second
    /// provider call for an already-accepted send.
    fn status(&self, key: &str) -> Result<LedgerOutcome, Refusal>;
    /// CAD-1041: pre-claim staging probe. The default approves — senders
    /// without a separate staging door (the in-memory test fakes, which
    /// validate inside `execute`) need no preflight; the production HTTP
    /// sender overrides it so the operator's send-now can stay queued on
    /// ambiguity instead of claiming a row it cannot send.
    fn preflight(&self, _binding: &SendBinding) -> Preflight {
        Preflight::Approved
    }
    /// CAD-1291: list the owner's standing grants for one destination. The
    /// default refuses, so a sender with no hosted door can never invent one.
    fn find_grant(&self, _destination_id: &str) -> Result<Vec<FoundGrant>, Refusal> {
        Err(Refusal::new(
            "grant_required",
            "this sender cannot look up an owner grant",
        ))
    }
}

/// CAD-1041: the pre-claim staging verdict an explicit send-now needs
/// before committing a row to `processing`. `Approved` means the door
/// staged the exact binding — claiming and executing is safe. `Refused`
/// is a definitive door refusal — the claim may proceed and report it.
/// `Uncertain` means a transport-ambiguous staging answer (timeout, 5xx,
/// drift): nothing was sent and nothing can be proven, so the row must
/// stay `queued` for a later send-now — claiming it would burn the
/// intent to `refused` on a transient door blip.
#[derive(Debug)]
pub enum Preflight {
    Approved,
    Refused(Refusal),
    Uncertain(Refusal),
}

impl LedgerOutcome {
    /// Evidence document as persisted on the intent row: the exact binding
    /// the provider answered plus its byte-exact evidence.
    pub fn evidence_json(&self) -> serde_json::Value {
        serde_json::json!({"state": self.state.as_str(),
            "permalink": self.permalink,
            "provider_ids": self.provider_ids,
            "provider_payload": self.provider_payload,
            "destination_id": self.destination_id,
            "caption_digest": self.caption_digest,
            "image_digest": self.image_digest})
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
    fn bare_digest_accepts_the_custody_form_and_the_wire_form() {
        let hex = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";
        assert!(!valid_digest(&format!("sha256:{hex}")));
        assert_eq!(bare_digest(&format!("sha256:{hex}")), hex);
        assert_eq!(bare_digest(hex), hex);
        assert!(valid_digest(bare_digest(&format!("sha256:{hex}"))));
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
    // ---- CAD-1291: select the owner's standing grant for this destination ----

    fn grant_wire() -> serde_json::Value {
        serde_json::json!({"id":"dpq_standing_001","kind":"standing","workspaceId":"ws_a",
            "connectionId":"con_ig","destinationId":"1784","toolkit":"instagram",
            "dailyCap":20,"remainingToday":20,"revokedAt": serde_json::Value::Null})
    }

    fn want() -> GrantWant<'static> {
        GrantWant {
            connection_id: "con_ig",
            destination_id: "1784",
            toolkit: "instagram",
        }
    }

    #[test]
    fn standing_grant_selected_only_for_its_own_destination() {
        let grant = FoundGrant::from_wire(&grant_wire()).unwrap();
        // Positive control.
        assert_eq!(
            select_grant(std::slice::from_ref(&grant), &want()).unwrap(),
            "dpq_standing_001"
        );
        assert_eq!(
            select_grant(&[], &want()).unwrap_err().code,
            "grant_required"
        );
        for other in [
            GrantWant {
                destination_id: "9999",
                ..want()
            },
            GrantWant {
                connection_id: "con_other",
                ..want()
            },
            GrantWant {
                toolkit: "facebook",
                ..want()
            },
        ] {
            assert_eq!(
                select_grant(std::slice::from_ref(&grant), &other)
                    .unwrap_err()
                    .code,
                "grant_required"
            );
        }
    }

    #[test]
    fn revoked_standing_grant_is_never_selected() {
        let mut revoked = grant_wire();
        revoked["revokedAt"] = serde_json::json!("2026-10-09T00:10:00.000Z");
        let revoked = FoundGrant::from_wire(&revoked).unwrap();
        assert_eq!(
            select_grant(std::slice::from_ref(&revoked), &want())
                .unwrap_err()
                .code,
            "grant_revoked"
        );
        // A live grant beside a revoked one still selects.
        let live = FoundGrant::from_wire(&grant_wire()).unwrap();
        assert!(select_grant(&[revoked, live], &want()).is_ok());
    }

    #[test]
    fn only_well_formed_standing_records_parse() {
        let mut per_post = grant_wire();
        per_post["kind"] = serde_json::json!("post");
        let mut bad_id = grant_wire();
        bad_id["id"] = serde_json::json!("grant");
        let mut no_revoked = grant_wire();
        no_revoked.as_object_mut().unwrap().remove("revokedAt");
        for wire in [per_post, bad_id, no_revoked] {
            assert!(FoundGrant::from_wire(&wire).is_none(), "{wire}");
        }
    }

    #[test]
    fn a_sender_without_a_hosted_door_cannot_find_a_grant() {
        struct Plain;
        impl PublishSender for Plain {
            fn execute(&self, _: &SendBinding) -> Result<LedgerOutcome, Refusal> {
                unreachable!()
            }
            fn status(&self, _: &str) -> Result<LedgerOutcome, Refusal> {
                unreachable!()
            }
        }
        assert_eq!(Plain.find_grant("1784").unwrap_err().code, "grant_required");
    }
}
