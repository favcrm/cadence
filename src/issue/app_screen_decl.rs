//! CAD-500 R4: a pure declaration/integrity validator for the
//! `app-screens/v1` screen-package contract.
//!
//! [`validate_map`] checks one closed JSON declaration against a
//! caller-supplied map of UTF-8 asset bodies: exact
//! contract/app/entry grammar, flat asset names against a fixed
//! extension→media mapping, declared sha256/size against the actual
//! bytes, and finite bounds at every stage — raw manifest bytes and
//! bracket depth before parsing, node count after a bounded
//! `serde_json::Value` pass, then a closed typed decode that keeps
//! duplicate JSON fields refused.
//!
//! ## What this proves — and what it does not
//!
//! A returned [`IntegrityCheckedScreen`] proves only that the
//! declaration and the supplied bodies agree: every declared asset is
//! present, every supplied body is declared, and each body's UTF-8
//! byte length and sha256 match its declaration. The declared
//! provenance digests (source/SDK/toolchain) are checked for *syntax*
//! only — they are inert metadata, never recomputed against anything.
//!
//! Returning the checked value grants **no** approval, source-rebuild
//! attestation, mount, execution or effect authority. Asset bodies
//! stay caller-owned inert strings: this validator does not sanitize
//! JS/CSS/SVG/JSON for execution and does not claim code safety.
//! There is no actor, endpoint, filesystem, network, environment or
//! installed-path surface here — the function touches nothing but its
//! two arguments.

use std::collections::{BTreeMap, HashSet};

use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::issue::model;

/// Largest raw manifest, bytes — checked before any parse.
const MAX_MANIFEST_BYTES: usize = 32_768;
/// Deepest `{}`/`[]` nesting — an iterative quote/escape-aware scan
/// refuses deeper manifests before serde sees them.
const MAX_DEPTH: usize = 32;
/// Most JSON nodes in the manifest — counted by an iterative walk over
/// the bounded `Value` tree.
const MAX_NODES: usize = 4_096;
/// Most assets, declared and supplied alike.
const MAX_ASSETS: usize = 32;
/// Largest single `.js` asset body — the screen entry/leaves get the
/// larger script budget (CAD-1254: the install gate's screen-asset cap).
const MAX_JS_BYTES: u64 = crate::issue::app::MAX_SCREEN_ASSET_BYTES;
/// Largest non-js asset body, UTF-8 bytes.
const MAX_ASSET_BYTES: u64 = 131_072;
/// Largest sum of every supplied asset body, bytes.
const MAX_TOTAL_BYTES: u64 = 524_288;

/// The per-leaf bound: `.js` gets the larger screen-script budget,
/// every other asset the flat cap. Keyed on the leaf name's extension.
fn asset_bound(name: &str) -> u64 {
    if name.rsplit_once('.').map(|(_, e)| e) == Some("js") {
        MAX_JS_BYTES
    } else {
        MAX_ASSET_BYTES
    }
}

/// The read-only contract — a mounted frame may only receive the
/// host-pushed projection (its `may` is empty).
const CONTRACT_V1: &str = "app-screens/v1";
/// CAD-1177: the action-capable contract. Identical integrity/asset
/// grammar to v1, plus an optional `host_contract` pin, a `may`
/// allowlist of bridge methods and a bounded `tools` alias→slot map.
/// A screen that does not declare them stays as read-only as v1.
const CONTRACT_V2: &str = "app-screens/v2";
/// The host contract a v2 screen may pin to call tools.
pub const HOST_CONTRACT_ACTIONS: &str = "screen-actions.v1";
/// CAD-1365: the media contract. Identical action mechanics to
/// `screen-actions.v1`, plus the separate `media.read` catalog
/// permission a media screen declares next to `tools.invoke`.
/// A screen pinning it keeps the same closed alias→slot `tools` map —
/// media rights never ride the provider-alias map.
pub const HOST_CONTRACT_MEDIA: &str = "screen-media.v1";
/// The tool bridge method a v2 `may` may list.
pub const MAY_TOOLS_INVOKE: &str = "tools.invoke";
/// CAD-1365: the workspace media-catalog read permission a
/// `screen-media.v1` screen may declare.
pub const MAY_MEDIA_READ: &str = "media.read";
/// Composer handoff for one retained library image. Media contract only.
pub const MAY_MEDIA_CHAT_ATTACH: &str = "media.chat.attach";
/// CAD-1365: the workspace media-catalog publish permission a
/// `screen-media.v1` screen may declare — the draft→published
/// metadata mutation `app_media_publish` serves. Read rights alone
/// never carry it, and no tools alias can hold it.
pub const MAY_MEDIA_PUBLISH: &str = "media.publish";
pub const HOST_CONTRACT_RECORDS: &str = "screen-records.v1";
pub const MAY_RECORDS_CALL: &str = "records.call";
pub const MAY_RECORDS_SLOT: &str = "records.slot";

/// Closed vocabulary shared by admission and the governed service executor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, Deserialize)]
pub enum ScreenOperation {
    #[serde(rename = "records.list")]
    RecordsList,
    #[serde(rename = "records.show")]
    RecordsShow,
    #[serde(rename = "records.create")]
    RecordsCreate,
    #[serde(rename = "records.update")]
    RecordsUpdate,
    #[serde(rename = "records.csv.preview")]
    RecordsCsvPreview,
    #[serde(rename = "records.csv.import")]
    RecordsCsvImport,
    #[serde(rename = "records.csv.confirm")]
    RecordsCsvConfirm,
    #[serde(rename = "segments.list")]
    SegmentsList,
    #[serde(rename = "segments.show")]
    SegmentsShow,
    #[serde(rename = "segments.save")]
    SegmentsSave,
    #[serde(rename = "exclusions.list")]
    ExclusionsList,
    #[serde(rename = "exclusions.show")]
    ExclusionsShow,
    #[serde(rename = "exclusions.save")]
    ExclusionsSave,
    #[serde(rename = "suppressions.list")]
    SuppressionsList,
    #[serde(rename = "suppressions.add")]
    SuppressionsAdd,
    #[serde(rename = "audience.preview")]
    AudiencePreview,
    #[serde(rename = "audience.prepare")]
    AudiencePrepare,
    #[serde(rename = "audience.show")]
    AudienceShow,
    #[serde(rename = "content.list")]
    ContentList,
    #[serde(rename = "content.show")]
    ContentShow,
    #[serde(rename = "content.save")]
    ContentSave,
    #[serde(rename = "content.clone")]
    ContentClone,
    #[serde(rename = "content.render")]
    ContentRender,
    #[serde(rename = "content.approve")]
    ContentApprove,
    #[serde(rename = "proposals.list")]
    ProposalsList,
    #[serde(rename = "proposals.show")]
    ProposalsShow,
    #[serde(rename = "proposals.render")]
    ProposalsRender,
    #[serde(rename = "proposals.apply")]
    ProposalsApply,
    #[serde(rename = "proposals.discard")]
    ProposalsDiscard,
    #[serde(rename = "sender-bindings.list")]
    SenderBindingsList,
    #[serde(rename = "sender-bindings.show")]
    SenderBindingsShow,
    #[serde(rename = "sender-bindings.save")]
    SenderBindingsSave,
    #[serde(rename = "content.test-prepare")]
    ContentTestPrepare,
    #[serde(rename = "content.send-prepare")]
    ContentSendPrepare,
    #[serde(rename = "sends.prepare")]
    SendsPrepare,
    #[serde(rename = "sends.show")]
    SendsShow,
    #[serde(rename = "sends.list")]
    SendsList,
    #[serde(rename = "sends.approve")]
    SendsApprove,
    #[serde(rename = "sends.resolve")]
    SendsResolve,
    #[serde(rename = "smtp.show")]
    SmtpShow,
    #[serde(rename = "smtp.manage")]
    SmtpManage,
    #[serde(rename = "smtp.bind")]
    SmtpBind,
    #[serde(rename = "smtp.rebind")]
    SmtpRebind,
    #[serde(rename = "smtp.revoke")]
    SmtpRevoke,
    #[serde(rename = "smtp.test-send")]
    SmtpTestSend,
    #[serde(rename = "origin.show")]
    OriginShow,
    #[serde(rename = "origin.manage")]
    OriginManage,
    #[serde(rename = "chat.campaign.open")]
    ChatCampaignOpen,
    #[serde(rename = "chat.campaign.brief")]
    ChatCampaignBrief,
    #[serde(rename = "screen.preview.show")]
    ScreenPreviewShow,
    #[serde(rename = "screen.preview.clear")]
    ScreenPreviewClear,
    #[serde(rename = "screen.route.patch")]
    ScreenRoutePatch,
    // CAD-1562: the hosted WhatsApp door (`agenticos.whatsapp-door.v1`).
    #[serde(rename = "whatsapp.status")]
    WhatsappStatus,
    #[serde(rename = "whatsapp.connect_session")]
    WhatsappConnectSession,
    #[serde(rename = "whatsapp.conversations.list")]
    WhatsappConversationsList,
    #[serde(rename = "whatsapp.conversations.show")]
    WhatsappConversationsShow,
    #[serde(rename = "whatsapp.messages")]
    WhatsappMessages,
    #[serde(rename = "whatsapp.reply.prepare")]
    WhatsappReplyPrepare,
    #[serde(rename = "whatsapp.reply.send")]
    WhatsappReplySend,
    #[serde(rename = "whatsapp.reply.status")]
    WhatsappReplyStatus,
}

impl ScreenOperation {
    pub fn is_slot(self) -> bool {
        matches!(
            self,
            Self::RecordsCsvImport
                | Self::RecordsCsvConfirm
                | Self::ContentApprove
                | Self::SendsApprove
                | Self::SendsResolve
                | Self::SmtpManage
                | Self::SmtpBind
                | Self::SmtpRebind
                | Self::SmtpRevoke
                | Self::SmtpTestSend
                | Self::OriginManage
                | Self::ChatCampaignOpen
                | Self::ChatCampaignBrief
                | Self::WhatsappReplySend
        )
    }
    /// CAD-1562: an operation of the hosted WhatsApp door.
    pub fn is_whatsapp(self) -> bool {
        matches!(
            self,
            Self::WhatsappStatus
                | Self::WhatsappConnectSession
                | Self::WhatsappConversationsList
                | Self::WhatsappConversationsShow
                | Self::WhatsappMessages
                | Self::WhatsappReplyPrepare
                | Self::WhatsappReplySend
                | Self::WhatsappReplyStatus
        )
    }
    /// CAD-1537: the closed allowlist a read-only screen (a member or a
    /// `--read-only` board viewer) may call. Every other operation —
    /// each write, SLOT and effectful host command — is refused for it.
    pub fn is_screen_read(self) -> bool {
        matches!(
            self,
            Self::RecordsList
                | Self::RecordsShow
                | Self::SegmentsList
                | Self::SegmentsShow
                | Self::ExclusionsList
                | Self::ExclusionsShow
                | Self::SuppressionsList
                | Self::AudiencePreview
                | Self::ContentList
                | Self::ContentShow
                | Self::ContentRender
                | Self::ProposalsList
                | Self::ProposalsShow
                | Self::ProposalsRender
                | Self::SendsList
                | Self::SendsShow
                | Self::SmtpShow
                | Self::OriginShow
                // Presentation-only host commands: the daemon validates and
                // echoes them; the host changes its URL or inert preview.
                | Self::ScreenRoutePatch
                | Self::ScreenPreviewShow
                | Self::ScreenPreviewClear
                // CAD-1562: WhatsApp reads. The connect link and every
                // reply step stay operator-only.
                | Self::WhatsappStatus
                | Self::WhatsappConversationsList
                | Self::WhatsappConversationsShow
                | Self::WhatsappMessages
                | Self::WhatsappReplyStatus
        )
    }
    pub fn args_limit(self) -> usize {
        match self {
            Self::RecordsCsvPreview | Self::RecordsCsvImport | Self::RecordsCsvConfirm => 327_680,
            Self::RecordsCreate
            | Self::RecordsUpdate
            | Self::ContentSave
            | Self::ScreenPreviewShow
            | Self::WhatsappReplyPrepare
            | Self::WhatsappReplySend => 49_152,
            _ => 16_384,
        }
    }
}

#[derive(Clone, Debug, serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScreenNavigation {
    pub section_query: String,
    pub default_section: String,
    pub sections: Vec<ScreenSection>,
}

#[derive(Clone, Debug, serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScreenSection {
    pub id: String,
    pub label: String,
}

impl ScreenNavigation {
    fn validate(&self) -> Result<()> {
        // Only app-owned keys can be declared; host authority/routing keys
        // never become a package-selected destination.
        const RESERVED: &[&str] = &[
            "app",
            "install",
            "install_id",
            "context",
            "context_id",
            "actor",
            "identity",
            "auth",
            "token",
            "key",
            "session",
            "session_key",
            "origin",
            "project",
            "workspace",
            "conversation",
            "thread",
            "message",
            "view",
            "appview",
            "record",
            "record_id",
            "segment",
            "segment_id",
            "screen",
            "contract-preview",
            "nonce",
            "proof",
        ];
        let key = self.section_query.as_bytes();
        if key.is_empty()
            || key.len() > 32
            || !key[0].is_ascii_lowercase()
            || !key
                .iter()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-'))
            || RESERVED.contains(&self.section_query.as_str())
        {
            return Err(Error::rejected(
                "navigation section_query must be an app-owned query key",
            ));
        }
        if self.sections.is_empty() || self.sections.len() > 8 {
            return Err(Error::rejected("navigation requires 1..8 sections"));
        }
        let mut seen = HashSet::new();
        for section in &self.sections {
            if !model::valid_tag(&section.id)
                || !seen.insert(&section.id)
                || section.label.is_empty()
                || section.label.encode_utf16().count() > 40
                || section.label.chars().any(char::is_control)
            {
                return Err(Error::rejected("invalid or duplicate navigation section"));
            }
        }
        if !seen.contains(&self.default_section) {
            return Err(Error::rejected(
                "navigation default_section is not declared",
            ));
        }
        Ok(())
    }
}
/// The one entry point — pinned, and it must be a declared, supplied
/// asset.
const ENTRY: &str = "client.js";

/// CAD-1123 (operator decision Q1): the closed set of named remote image
/// origins a screen may opt into with `remote_images`. A declaration
/// names a set, never a host, so a package cannot widen the frame's
/// egress beyond what the host lists here. The browser loads these
/// directly into the frame; the daemon never fetches them.
const REMOTE_IMAGE_SETS: &[(&str, &[&str])] = &[(
    "instagram-cdn",
    &["https://*.cdninstagram.com", "https://*.fbcdn.net"],
)];

/// The CSP `img-src` sources one declared remote image set admits, or
/// `None` for a name the host does not list.
pub fn remote_image_sources(name: &str) -> Option<&'static [&'static str]> {
    REMOTE_IMAGE_SETS
        .iter()
        .find(|(set, _)| *set == name)
        .map(|(_, hosts)| *hosts)
}

/// A declaration that passed every check [`validate_map`] makes.
///
/// Carries small validated metadata only — never asset bodies and
/// never host paths. Fields are private, there is no `Deserialize`
/// and no unchecked constructor, so a value can only come out of the
/// validator. See the module docs: this value is integrity evidence,
/// not authority of any kind.
#[derive(Debug)]
pub struct IntegrityCheckedScreen {
    contract: String,
    app: String,
    entry: String,
    asset_count: usize,
    source_digest: String,
    sdk_digest: String,
    toolchain_digest: String,
    remote_images: Vec<String>,
    /// v2 only: the declared `may` methods (the closed `tools.invoke`/
    /// `media.*` set). Empty on v1 and on a v2 that declares none.
    may: Vec<String>,
    /// v2 only: the declared `host_contract` pin.
    host_contract: Option<String>,
    /// v2 only: declared logical tool alias → declared capability slot.
    /// The alias is app-owned vocabulary the frame calls; the slot is
    /// the `needs.capabilities` key it resolves through. Empty on v1.
    tools: BTreeMap<String, String>,
    operations: Vec<ScreenOperation>,
    navigation: Option<ScreenNavigation>,
    startup: Option<Vec<String>>,
}

impl IntegrityCheckedScreen {
    /// The declared remote image sets — each a name in the host's closed
    /// list, distinct; empty unless the screen opted in.
    pub fn remote_images(&self) -> &[String] {
        &self.remote_images
    }
    /// The contract this declaration passed under (`app-screens/v1` or
    /// `app-screens/v2`).
    pub fn contract(&self) -> &str {
        &self.contract
    }
    /// The declared `may` bridge methods (v2 only; empty on v1).
    pub fn may(&self) -> &[String] {
        &self.may
    }
    /// The declared `host_contract` pin (v2 only; `None` unless the
    /// declaration named one of the host-served contracts).
    pub fn host_contract(&self) -> Option<&str> {
        self.host_contract.as_deref()
    }
    /// The declared tool alias→capability-slot map (v2 only; empty on v1).
    pub fn tools(&self) -> &BTreeMap<String, String> {
        &self.tools
    }
    pub fn operations(&self) -> &[ScreenOperation] {
        &self.operations
    }
    pub fn navigation(&self) -> Option<&ScreenNavigation> {
        self.navigation.as_ref()
    }
    /// Explicit execution order, records-v2 only; absence keeps all leaves
    /// except the entry inert. Integrity evidence, never execution authority.
    pub fn startup(&self) -> Option<&[String]> {
        self.startup.as_deref()
    }
    /// The declared app tag (`model::valid_tag` grammar).
    pub fn app(&self) -> &str {
        &self.app
    }
    /// The entry asset name — always `client.js` in v1.
    pub fn entry(&self) -> &str {
        &self.entry
    }
    /// How many assets the declaration lists (1..=32).
    pub fn asset_count(&self) -> usize {
        self.asset_count
    }
    /// Declared provenance digest — syntax checked, inert; no source
    /// rebuild is recomputed or implied.
    pub fn source_digest(&self) -> &str {
        &self.source_digest
    }
    /// Declared SDK digest — syntax checked, inert.
    pub fn sdk_digest(&self) -> &str {
        &self.sdk_digest
    }
    /// Declared toolchain digest — syntax checked, inert.
    pub fn toolchain_digest(&self) -> &str {
        &self.toolchain_digest
    }
}

/// The closed top-level `app-screens/v1` shape. `deny_unknown_fields`
/// refuses stray keys; serde's own duplicate-field error refuses
/// repeated ones. `may` is the optional method list — the contract
/// carries none, so only absence or `[]` passes the explicit check.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDecl {
    contract: String,
    app: String,
    entry: String,
    assets: Vec<RawAsset>,
    provenance: RawProvenance,
    /// Optional; `null`, a non-array or any member refuses. v1 requires
    /// empty; v2 admits only the closed `MAY_TOOLS_INVOKE` verb.
    #[serde(default)]
    may: Vec<Value>,
    /// Optional named remote image sets (CAD-1123); each must be a host
    /// listed set, without repeats.
    #[serde(default)]
    remote_images: Vec<String>,
    /// v2 only: the host contract this screen was built against. Must be
    /// a host-served contract when present (v2 with `may`/`tools`) — an
    /// unknown or missing contract keeps the screen read-only.
    #[serde(default)]
    host_contract: Option<String>,
    /// v2 only: logical alias → capability-slot declaration. Each alias
    /// is app-owned; each slot must name a `needs.capabilities` key the
    /// installed app declares (checked at invoke, never trusted here).
    #[serde(default)]
    tools: BTreeMap<String, String>,
    #[serde(default)]
    operations: Vec<ScreenOperation>,
    #[serde(default)]
    navigation: Option<ScreenNavigation>,
    #[serde(default)]
    startup: Option<Vec<String>>,
}

/// One declared asset member — closed, all four fields required.
/// `size: u64` makes negative/float/string/null refuse through serde
/// typing alone.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAsset {
    name: String,
    media_type: String,
    sha256: String,
    size: u64,
}

/// Declared provenance — required, closed, digest *syntax* only.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawProvenance {
    source_digest: String,
    sdk_digest: String,
    toolchain_digest: String,
}

/// Validate one `app-screens/v1` declaration against caller-supplied
/// UTF-8 asset bodies.
///
/// `manifest` is the raw JSON text (a separate argument — never an
/// asset-map member). `assets` maps flat asset names to bodies; it is
/// borrowed read-only and is identical on success and on refusal.
///
/// Check order is part of the contract: supplied count/size bounds,
/// raw manifest byte cap and bracket depth before any parse; a
/// bounded `Value` pass only to enforce the node budget; then the
/// closed typed decode of the *original* raw text (so duplicate JSON
/// fields still refuse — a `Value` map would hide them); then the
/// grammar and the declared↔supplied integrity match. Resource bounds
/// precede body hashing; integrity refusals can occur while comparing bodies.
pub fn validate_map(
    manifest: &str,
    assets: &BTreeMap<String, String>,
) -> Result<IntegrityCheckedScreen> {
    // Supplied-side bounds first — before anything copies or hashes a
    // byte. The map is already caller-owned; only lengths are read.
    if assets.len() > MAX_ASSETS {
        return Err(Error::rejected(format!(
            "{} supplied assets — at most {MAX_ASSETS}",
            assets.len()
        )));
    }
    let mut total_bytes = 0u64;
    for (name, body) in assets.iter() {
        let len = body.len() as u64;
        let bound = asset_bound(name);
        if len > bound {
            return Err(Error::rejected(format!(
                "asset {name:?} body is {len} bytes — at most {bound}"
            )));
        }
        total_bytes += len;
    }
    if total_bytes > MAX_TOTAL_BYTES {
        return Err(Error::rejected(format!(
            "asset bodies total {total_bytes} bytes — at most {MAX_TOTAL_BYTES}"
        )));
    }

    // Raw byte cap and bracket depth before serde touches the text.
    if manifest.len() > MAX_MANIFEST_BYTES {
        return Err(Error::rejected(format!(
            "manifest is {} bytes — at most {MAX_MANIFEST_BYTES}",
            manifest.len()
        )));
    }
    check_depth(manifest)?;

    // The bounded `Value` parse exists only for the node budget — it
    // decides nothing about validity (malformed input still refuses
    // here) and never feeds semantics.
    let value: Value = serde_json::from_str(manifest)
        .map_err(|e| Error::rejected(format!("manifest is not valid JSON: {e}")))?;
    check_nodes(&value)?;

    // The closed typed decode reads the ORIGINAL raw text so duplicate
    // fields at every struct level refuse; a `Value` map would hide them.
    let decl: RawDecl = serde_json::from_str(manifest)
        .map_err(|e| Error::rejected(format!("manifest declaration: {e}")))?;

    // Preserve the exact old v1/screen-actions schema floors: even empty
    // new fields are records-contract-only, not a silent old-schema widening.
    if decl.host_contract.as_deref() != Some(HOST_CONTRACT_RECORDS)
        && value.as_object().is_some_and(|fields| {
            fields.contains_key("operations") || fields.contains_key("navigation")
        })
    {
        return Err(Error::rejected(
            "operations/navigation require screen-records.v1",
        ));
    }
    if value.get("startup").is_some() && decl.startup.is_none() {
        return Err(Error::rejected("startup must be a non-null array"));
    }
    check_declaration(&decl)?;
    check_assets(&decl, assets)?;

    let tools = decl
        .tools
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    Ok(IntegrityCheckedScreen {
        contract: decl.contract,
        asset_count: decl.assets.len(),
        app: decl.app,
        entry: decl.entry,
        source_digest: decl.provenance.source_digest,
        sdk_digest: decl.provenance.sdk_digest,
        toolchain_digest: decl.provenance.toolchain_digest,
        remote_images: decl.remote_images,
        may: decl
            .may
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        host_contract: decl.host_contract,
        tools,
        operations: decl.operations,
        navigation: decl.navigation,
        startup: decl.startup,
    })
}

/// Iterative, quote/escape-aware bracket scan: refuse `{}`/`[]`
/// nesting deeper than [`MAX_DEPTH`] before serde parses. String
/// contents — escaped quotes and literal brackets included — never
/// count as structure. Malformedness stays serde's job; this only
/// bounds the one thing serde's recursive descent must be shielded
/// from.
fn check_depth(raw: &str) -> Result<()> {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    // `"`, `\` and the brackets are all single bytes <0x80, so a byte
    // walk cannot split a UTF-8 sequence.
    for &b in raw.as_bytes() {
        if in_string {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' | b'[' => {
                depth += 1;
                if depth > MAX_DEPTH {
                    return Err(Error::rejected(format!(
                        "manifest nesting exceeds depth {MAX_DEPTH}"
                    )));
                }
            }
            // A closer without an opener is malformed, not deep — serde
            // refuses it; the saturating_sub keeps the scan panic-free.
            b'}' | b']' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    Ok(())
}

/// Post-parse node budget: an iterative walk (heap stack, never
/// recursion) counting every `Value` — each scalar, each array
/// element, each object member's value, and each container once.
fn check_nodes(value: &Value) -> Result<()> {
    let mut nodes = 0usize;
    let mut stack = vec![value];
    while let Some(v) = stack.pop() {
        nodes += 1;
        if nodes > MAX_NODES {
            return Err(Error::rejected(format!(
                "manifest exceeds {MAX_NODES} JSON nodes"
            )));
        }
        match v {
            Value::Array(items) => {
                if nodes + stack.len() + items.len() > MAX_NODES {
                    return Err(Error::rejected("manifest exceeds JSON node budget"));
                }
                stack.extend(items.iter());
            }
            Value::Object(map) => {
                if nodes + stack.len() + map.len() > MAX_NODES {
                    return Err(Error::rejected("manifest exceeds JSON node budget"));
                }
                stack.extend(map.values());
            }
            _ => {}
        }
    }
    Ok(())
}

/// The closed grammar beyond serde's structural decode: exact
/// contract/app/entry, an effectively empty `may`, at most
/// [`MAX_ASSETS`] uniquely-named assets each carrying the pinned
/// `client.js` entry, name/media/digest grammar per member, and
/// provenance digest syntax.
/// A v2 logical alias grammar: `word` or dotted `word.word`, each word
/// lowercase-alnum starting with a letter (e.g. `instagram.read`,
/// `image.generate`). Bounded, never a provider/tool/account/route name.
fn valid_tool_alias(alias: &str) -> bool {
    if alias.is_empty() || alias.len() > 64 {
        return false;
    }
    alias.split('.').all(|word| {
        !word.is_empty()
            && word.len() <= 32
            && word.as_bytes()[0].is_ascii_lowercase()
            && word
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
    })
}

fn check_declaration(decl: &RawDecl) -> Result<()> {
    let is_v2 = match decl.contract.as_str() {
        c if c == CONTRACT_V1 => false,
        c if c == CONTRACT_V2 => true,
        other => {
            return Err(Error::rejected(format!(
                "contract {other:?} — the only accepted contracts are \
                 {CONTRACT_V1:?} and {CONTRACT_V2:?}",
            )))
        }
    };
    if !model::valid_tag(&decl.app) {
        return Err(Error::rejected(format!(
            "app {:?} — 1-32 lowercase letters, digits or hyphens, not starting with a hyphen",
            decl.app
        )));
    }
    if decl.entry != ENTRY {
        return Err(Error::rejected(format!(
            "entry {:?} — the only entry is {ENTRY:?}",
            decl.entry
        )));
    }
    // `may` is a string allowlist of bridge methods. v1 admits none;
    // v2 admits only the closed set, each a string, no repeats. A
    // non-string member or a method outside the set refuses (fail closed).
    // The `media.*` permissions exist only under `screen-media.v1` — an
    // actions-contract screen cannot smuggle catalog rights in `may`.
    // `media.read` and `media.publish` are the granted media members;
    // publish is a separate declaration — read never implies mutation.
    let media_contract = decl.host_contract.as_deref() == Some(HOST_CONTRACT_MEDIA);
    let mut may_seen = HashSet::new();
    for member in &decl.may {
        let Some(name) = member.as_str() else {
            return Err(Error::rejected("`may` members are strings"));
        };
        let records = decl.host_contract.as_deref() == Some(HOST_CONTRACT_RECORDS);
        let granted = is_v2
            && if records {
                matches!(name, MAY_RECORDS_CALL | MAY_RECORDS_SLOT)
            } else {
                name == MAY_TOOLS_INVOKE
                    || (media_contract
                        && matches!(
                            name,
                            MAY_MEDIA_READ | MAY_MEDIA_PUBLISH | MAY_MEDIA_CHAT_ATTACH
                        ))
            };
        if !granted {
            return Err(Error::rejected(format!(
                "`may` method {name:?} is not a method this contract grants"
            )));
        }
        if !may_seen.insert(name) {
            return Err(Error::rejected(format!(
                "`may` method {name:?} is declared twice"
            )));
        }
    }
    // `host_contract`/`tools` are v2-only action declarations. Declaring
    // either under v1 refuses — a v1 screen stays read-only and cannot
    // smuggle a contract it was not reviewed against.
    if !is_v2
        && (decl.host_contract.is_some()
            || !decl.tools.is_empty()
            || !decl.operations.is_empty()
            || decl.navigation.is_some())
    {
        return Err(Error::rejected(
            "host_contract and tools require contract app-screens/v2",
        ));
    }
    if let Some(host) = &decl.host_contract {
        if host != HOST_CONTRACT_ACTIONS
            && host != HOST_CONTRACT_MEDIA
            && host != HOST_CONTRACT_RECORDS
        {
            return Err(Error::rejected(format!(
                "host_contract {host:?} — the only served contracts are \
                 {HOST_CONTRACT_ACTIONS:?}, {HOST_CONTRACT_MEDIA:?} and {HOST_CONTRACT_RECORDS:?}",
            )));
        }
    }
    // A screen that declares tools or a may-method must pin the host
    // contract it was built against; the pin alone grants nothing.
    if (!decl.tools.is_empty() || !decl.may.is_empty()) && decl.host_contract.is_none() {
        return Err(Error::rejected(
            "a screen declaring may/tools must pin host_contract",
        ));
    }
    if decl.host_contract.as_deref() == Some(HOST_CONTRACT_RECORDS) {
        if !decl.tools.is_empty() || decl.operations.len() > 64 {
            return Err(Error::rejected(
                "records screens require empty tools and at most 64 operations",
            ));
        }
        let mut operations = HashSet::new();
        for op in &decl.operations {
            let permission = if op.is_slot() {
                MAY_RECORDS_SLOT
            } else {
                MAY_RECORDS_CALL
            };
            if !operations.insert(op) || !may_seen.contains(permission) {
                return Err(Error::rejected(
                    "duplicate operation or missing records permission",
                ));
            }
        }
        if let Some(nav) = &decl.navigation {
            nav.validate()?;
        }
    } else if !decl.operations.is_empty() || decl.navigation.is_some() {
        return Err(Error::rejected(
            "operations/navigation require screen-records.v1",
        ));
    }
    // `tools` is a bounded alias→slot map (≤8, matching the app
    // capability-slot ceiling). Each alias is closed grammar; each slot
    // is a tag grammar capability key. The actual slot existence is
    // proven at invoke against the installed app — here we only prove
    // the declaration is well-formed and bounded.
    if decl.tools.len() > 8 {
        return Err(Error::rejected(format!(
            "{} declared tools — at most 8",
            decl.tools.len()
        )));
    }
    for (alias, slot) in &decl.tools {
        if !valid_tool_alias(alias) {
            return Err(Error::rejected(format!(
                "tool alias {alias:?} — lowercase words joined by dots"
            )));
        }
        if !model::valid_tag(slot) {
            return Err(Error::rejected(format!(
                "tool {alias:?} slot {slot:?} — must name a capability slot"
            )));
        }
    }
    let mut sets = HashSet::new();
    for name in &decl.remote_images {
        if remote_image_sources(name).is_none() {
            return Err(Error::rejected(format!(
                "remote_images {name:?} — not a host-listed image set"
            )));
        }
        if !sets.insert(name.as_str()) {
            return Err(Error::rejected(format!(
                "remote_images {name:?} is declared twice"
            )));
        }
    }
    if decl.assets.is_empty() {
        return Err(Error::rejected(format!(
            "at least the {ENTRY:?} entry asset must be declared"
        )));
    }
    if decl.assets.len() > MAX_ASSETS {
        return Err(Error::rejected(format!(
            "{} declared assets — at most {MAX_ASSETS}",
            decl.assets.len()
        )));
    }
    let mut names = HashSet::with_capacity(decl.assets.len());
    let mut entry_declared = false;
    for asset in &decl.assets {
        check_asset_name(&asset.name)?;
        check_media(&asset.name, &asset.media_type)?;
        check_digest(&asset.sha256, "asset sha256")?;
        let bound = asset_bound(&asset.name);
        if asset.size > bound {
            return Err(Error::rejected(format!(
                "asset {:?} declares {} bytes — at most {bound}",
                asset.name, asset.size
            )));
        }
        if !names.insert(asset.name.as_str()) {
            return Err(Error::rejected(format!(
                "asset {:?} is declared twice",
                asset.name
            )));
        }
        entry_declared |= asset.name == ENTRY;
    }
    if !entry_declared {
        return Err(Error::rejected(format!(
            "entry {ENTRY:?} must be one of the declared assets"
        )));
    }
    if let Some(startup) = &decl.startup {
        if !is_v2 || decl.host_contract.as_deref() != Some(HOST_CONTRACT_RECORDS) {
            return Err(Error::rejected(
                "startup requires app-screens/v2 and screen-records.v1",
            ));
        }
        if !(2..=MAX_ASSETS).contains(&startup.len())
            || startup.last().map(String::as_str) != Some(ENTRY)
        {
            return Err(Error::rejected(
                "startup requires 2..32 scripts with client.js last",
            ));
        }
        let scripts: HashSet<&str> = decl
            .assets
            .iter()
            .filter(|asset| asset.media_type == "text/javascript")
            .map(|asset| asset.name.as_str())
            .collect();
        let mut ordered = HashSet::new();
        for name in startup {
            if !scripts.contains(name.as_str()) || !ordered.insert(name.as_str()) {
                return Err(Error::rejected(
                    "startup members must be distinct declared JavaScript leaves",
                ));
            }
        }
        if ordered != scripts {
            return Err(Error::rejected(
                "startup must list every declared JavaScript leaf",
            ));
        }
    }
    // Provenance is declared syntax — checked for shape, never
    // recomputed against a rebuild.
    check_digest(&decl.provenance.source_digest, "provenance.source_digest")?;
    check_digest(&decl.provenance.sdk_digest, "provenance.sdk_digest")?;
    check_digest(
        &decl.provenance.toolchain_digest,
        "provenance.toolchain_digest",
    )?;
    Ok(())
}

/// A flat leaf `<stem>.<ext>`: the stem is `model::valid_tag` (which
/// already forbids slash, backslash, percent, control, non-ASCII and
/// dot-containing stems) and the single final extension is one of the
/// mapped four.
fn check_asset_name(name: &str) -> Result<()> {
    let Some((stem, ext)) = name.rsplit_once('.') else {
        return Err(Error::rejected(format!(
            "asset name {name:?} — expected a flat <name>.<ext> leaf"
        )));
    };
    if !matches!(ext, "js" | "css" | "svg" | "json") {
        return Err(Error::rejected(format!(
            "asset name {name:?} — the only extensions are js, css, svg, json"
        )));
    }
    if !model::valid_tag(stem) {
        return Err(Error::rejected(format!(
            "asset name {name:?} — the stem must be a 1-32 char tag (lowercase, digits, hyphens)"
        )));
    }
    Ok(())
}

/// The fixed extension→media mapping; `client.js` therefore must be
/// JavaScript media.
fn check_media(name: &str, media_type: &str) -> Result<()> {
    let ext = name.rsplit_once('.').map(|(_, ext)| ext).unwrap_or("");
    let want = match ext {
        "js" => "text/javascript",
        "css" => "text/css",
        "svg" => "image/svg+xml",
        "json" => "application/json",
        _ => {
            return Err(Error::rejected(format!(
                "asset name {name:?} has no mapped media type"
            )))
        }
    };
    if media_type != want {
        return Err(Error::rejected(format!(
            "asset {name:?} declares media_type {media_type:?} — {want:?} is required"
        )));
    }
    Ok(())
}

/// `sha256:` plus exactly 64 lowercase hex digits — the only digest
/// syntax anywhere in the declaration.
fn check_digest(digest: &str, what: &str) -> Result<()> {
    let Some(hex) = digest.strip_prefix("sha256:") else {
        return Err(Error::rejected(format!(
            "{what} {digest:?} — expected sha256:<64 lowercase hex>"
        )));
    };
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(Error::rejected(format!(
            "{what} {digest:?} — expected sha256:<64 lowercase hex>"
        )));
    }
    Ok(())
}

/// The declared↔supplied match: every declared asset exists in the
/// map with exactly the declared UTF-8 byte length and sha256, and
/// every supplied name is declared — no undeclared body passes.
fn check_assets(decl: &RawDecl, assets: &BTreeMap<String, String>) -> Result<()> {
    for asset in &decl.assets {
        let Some(body) = assets.get(&asset.name) else {
            return Err(Error::rejected(format!(
                "declared asset {:?} is not supplied",
                asset.name
            )));
        };
        if asset.size != body.len() as u64 {
            return Err(Error::rejected(format!(
                "asset {:?} declares {} bytes — the body is {}",
                asset.name,
                asset.size,
                body.len()
            )));
        }
        let actual = format!("sha256:{:x}", Sha256::digest(body.as_bytes()));
        if asset.sha256 != actual {
            return Err(Error::rejected(format!(
                "asset {:?} sha256 does not match its body",
                asset.name
            )));
        }
    }
    let declared: HashSet<&str> = decl.assets.iter().map(|a| a.name.as_str()).collect();
    for name in assets.keys() {
        if !declared.contains(name.as_str()) {
            return Err(Error::rejected(format!(
                "supplied asset {name:?} is not declared"
            )));
        }
    }
    Ok(())
}
