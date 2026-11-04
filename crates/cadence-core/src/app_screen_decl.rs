//! CAD-500 R4 / CAD-2008: the one declaration/integrity validator for the
//! `app-screens/v1|v2` screen-package contract.
//!
//! This module is the single implementation of the screen declaration rules.
//! The native host calls [`validate_map`] with the asset bodies it holds; the
//! bounded `app.screen.integrity` operation calls [`validate_facts`] with the
//! size/sha256 facts its host computed from the exact installed bytes; the
//! additive `app.screen.asset-integrity` operation calls
//! [`validate_facts_asset_integrity`] when its host owns the operation
//! vocabulary. All three enter the same private pipeline — one grammar, one
//! bound set, one declared↔supplied comparison — so hosts cannot drift into
//! different answers for the same declaration. The app-level screen *count*
//! bound ([`MAX_SCREENS`]) is stated here too, so the native bundle scan and
//! the hosted count check agree on how many packages one app may declare.
//!
//! [`validate_map`] checks one closed JSON declaration against a
//! caller-supplied map of UTF-8 asset bodies: exact
//! contract/app/entry grammar, flat asset names against a fixed
//! extension→media mapping, declared sha256/size against the actual
//! bytes, and finite bounds at every stage — raw manifest bytes and
//! bracket depth before parsing, node count after a bounded
//! `serde_json::Value` pass, then a closed typed decode that keeps
//! duplicate JSON keys refused at every level, map-valued fields included.
//!
//! ## Shared structure, host-owned operation vocabulary
//!
//! A declaration's `operations` list carries host operation names. Two
//! admission modes share this one parser, one bound set and one
//! declared↔supplied comparison, and differ *only* in who owns that
//! vocabulary:
//!
//! * [`validate_map`] / [`validate_facts`] resolve each operation against
//!   the closed native [`ScreenOperation`] set and require the matching
//!   `records.call`/`records.slot` `may` grant. This is the native
//!   admission and the existing strict `app.screen.integrity` answer;
//!   it is unchanged.
//! * [`validate_facts_asset_integrity`] checks the same declaration,
//!   assets and `screen_count`, but treats each operation as a bounded,
//!   typed, duplicate-free *host token*: no vocabulary is consulted. Which
//!   operations a host serves, and whether each is a CALL or a host-confirmed
//!   SLOT, is that host's own closed vocabulary and its own permission
//!   classification — this crate neither knows nor infers it. Its result is
//!   the narrower [`AssetIntegrityChecked`] (the app tag only), so it can
//!   never be mistaken for an [`IntegrityCheckedScreen`] carrying a resolved
//!   operation list.
//!
//! The admission is chosen by the decoded list's type, not by a runtime flag:
//! the native paths decode [`ScreenOperation`] inside the one typed decode, so
//! the closed vocabulary is resolved by serde exactly as before and no
//! operation name is ever allocated as a string.
//!
//! Both modes prove shared structure and exact declared↔supplied bytes.
//! Neither mode grants operation permission: an admitted host token is a
//! well-formed declaration, never an authorized call or slot, and the
//! host must still apply its closed operation set and its grant checks
//! before publishing, installing or re-validating a package.
//!
//! ## What this proves — and what it does not
//!
//! A returned [`IntegrityCheckedScreen`] proves only that the
//! declaration and the supplied assets agree: every declared asset is
//! present, every supplied asset is declared, and each supplied UTF-8
//! byte length and sha256 match its declaration. With [`validate_facts`]
//! the digests are the *caller's* reported facts: the agreement is with
//! those facts, and it is the caller's job to hash the bytes it actually
//! installed. The declared provenance digests (source/SDK/toolchain) are
//! checked for *syntax* only — they are inert metadata, never recomputed
//! against anything.
//!
//! Returning the checked value grants **no** approval, source-rebuild
//! attestation, mount, execution or effect authority. Asset bodies
//! stay caller-owned inert strings: this validator does not sanitize
//! JS/CSS/SVG/JSON for execution and does not claim code safety.
//! There is no actor, endpoint, filesystem, network, environment or
//! installed-path surface here — the functions touch nothing but their
//! two arguments.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashSet};

use schemars::JsonSchema;
use serde::de::{MapAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::issue::valid_tag;

/// Largest raw manifest, bytes — checked before any parse.
pub const MAX_MANIFEST_BYTES: usize = 32_768;
/// Deepest `{}`/`[]` nesting — an iterative quote/escape-aware scan
/// refuses deeper manifests before serde sees them.
const MAX_DEPTH: usize = 32;
/// Most JSON nodes in the manifest — counted by an iterative walk over
/// the bounded `Value` tree.
const MAX_NODES: usize = 4_096;
/// Most assets, declared and supplied alike.
pub const MAX_ASSETS: usize = 32;
/// Most operations one declaration may list — the same bound under both
/// admission modes.
const MAX_OPERATIONS: usize = 64;
/// Longest operation token, bytes. The longest canonical host operation is
/// well under this; the cap keeps a declaration's operation list small
/// whatever vocabulary a host serves.
const MAX_OPERATION_BYTES: usize = 64;
/// Most screen packages one app may declare. The native bundle scan
/// (`app_screen_pkg::tags_in`) and the hosted `app.screen.integrity` count
/// check both read this one number, so a package set is bounded identically on
/// both hosts.
pub const MAX_SCREENS: usize = 8;
/// The whole-app aggregate content budget, bytes. A screen's supplied assets
/// share it, and the native installer keeps the same number: the limit is
/// stated here once and re-exported as the native app budget, so the two
/// hosts cannot disagree about the boundary.
pub const MAX_TOTAL_BYTES: u64 = 2 * 1024 * 1024;

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
/// CAD-1962: the permission a `screen-media.v1` screen declares to copy
/// one of its own retained images into the shared workspace library.
/// Write rights: read and publish never imply it, and no tools alias can
/// hold it.
pub const MAY_MEDIA_IMPORT: &str = "media.import";
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
    // CAD-1590: door v2 templates and broadcasts.
    #[serde(rename = "whatsapp.templates.list")]
    WhatsappTemplatesList,
    #[serde(rename = "whatsapp.template.show")]
    WhatsappTemplateShow,
    #[serde(rename = "whatsapp.template.prepare")]
    WhatsappTemplatePrepare,
    #[serde(rename = "whatsapp.template.send")]
    WhatsappTemplateSend,
    #[serde(rename = "whatsapp.template.status")]
    WhatsappTemplateStatus,
    #[serde(rename = "whatsapp.broadcasts.list")]
    WhatsappBroadcastsList,
    #[serde(rename = "whatsapp.broadcast.show")]
    WhatsappBroadcastShow,
    #[serde(rename = "whatsapp.broadcast.estimate")]
    WhatsappBroadcastEstimate,
    #[serde(rename = "whatsapp.broadcast.results")]
    WhatsappBroadcastResults,
    #[serde(rename = "whatsapp.broadcast.draft")]
    WhatsappBroadcastDraft,
    #[serde(rename = "whatsapp.broadcast.test.prepare")]
    WhatsappBroadcastTestPrepare,
    #[serde(rename = "whatsapp.broadcast.test.send")]
    WhatsappBroadcastTestSend,
    #[serde(rename = "whatsapp.broadcast.prepare")]
    WhatsappBroadcastPrepare,
    #[serde(rename = "whatsapp.broadcast.send")]
    WhatsappBroadcastSend,
    #[serde(rename = "whatsapp.broadcast.stop")]
    WhatsappBroadcastStop,
    // CAD-1651: takeover. Set only reduces automation (no SLOT); release
    // lets FavCRM's AI reply again, so it is a confirm SLOT.
    #[serde(rename = "whatsapp.handoff.set")]
    WhatsappHandoffSet,
    #[serde(rename = "whatsapp.handoff.release")]
    WhatsappHandoffRelease,
    // CAD-1636: the reply copilot. Not door verbs (`is_whatsapp`): the
    // host serves them itself, on a hosted daemon only.
    #[serde(rename = "chat.whatsapp.open")]
    ChatWhatsappOpen,
    #[serde(rename = "chat.whatsapp.brief")]
    ChatWhatsappBrief,
    #[serde(rename = "whatsapp.draft.show")]
    WhatsappDraftShow,
    #[serde(rename = "whatsapp.draft.discard")]
    WhatsappDraftDiscard,
    // CAD-1662: Works content. A closed CALL set plus two host-confirmed
    // SLOT operations (review decision and export receipt).
    #[serde(rename = "works.query")]
    WorksQuery,
    #[serde(rename = "works.get")]
    WorksGet,
    #[serde(rename = "works.create")]
    WorksCreate,
    #[serde(rename = "works.patch")]
    WorksPatch,
    #[serde(rename = "works.trash")]
    WorksTrash,
    #[serde(rename = "works.restore")]
    WorksRestore,
    #[serde(rename = "works.request_review")]
    WorksRequestReview,
    #[serde(rename = "works.review")]
    WorksReview,
    #[serde(rename = "works.export")]
    WorksExport,
    // CAD-1827: CRM campaign work metadata (`crm-campaign-work/v1`). The
    // show operation is a read; save is a *direct* write with revision
    // CAS and caller-derived attribution, not a host confirmation slot.
    // Neither grants approval, send or publish authority.
    #[serde(rename = "campaign-work.show")]
    CampaignWorkShow,
    #[serde(rename = "campaign-work.save")]
    CampaignWorkSave,
}

impl ScreenOperation {
    /// CAD-1827: an operation of the paired `crm-campaign-work/v1`
    /// contract. A package that declares one must also declare the
    /// contract in its manifest, and vice versa.
    pub fn is_campaign_work(self) -> bool {
        matches!(self, Self::CampaignWorkShow | Self::CampaignWorkSave)
    }

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
                | Self::WhatsappTemplateSend
                | Self::WhatsappBroadcastTestSend
                | Self::WhatsappBroadcastSend
                | Self::WhatsappBroadcastStop
                | Self::WhatsappHandoffRelease
                | Self::ChatWhatsappBrief
                | Self::WorksReview
                | Self::WorksExport
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
                | Self::WhatsappTemplatesList
                | Self::WhatsappTemplateShow
                | Self::WhatsappTemplatePrepare
                | Self::WhatsappTemplateSend
                | Self::WhatsappTemplateStatus
                | Self::WhatsappBroadcastsList
                | Self::WhatsappBroadcastShow
                | Self::WhatsappBroadcastEstimate
                | Self::WhatsappBroadcastResults
                | Self::WhatsappBroadcastDraft
                | Self::WhatsappBroadcastTestPrepare
                | Self::WhatsappBroadcastTestSend
                | Self::WhatsappBroadcastPrepare
                | Self::WhatsappBroadcastSend
                | Self::WhatsappBroadcastStop
                | Self::WhatsappHandoffSet
                | Self::WhatsappHandoffRelease
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
                // CAD-1590: template and broadcast reads only; drafts,
                // prepares, sends and stop stay operator-only.
                | Self::WhatsappTemplatesList
                | Self::WhatsappTemplateShow
                | Self::WhatsappTemplateStatus
                | Self::WhatsappBroadcastsList
                | Self::WhatsappBroadcastShow
                | Self::WhatsappBroadcastEstimate
                | Self::WhatsappBroadcastResults
                // CAD-1636: members read the parked draft only.
                | Self::WhatsappDraftShow
                // CAD-1827: recorded campaign work facts are a read; the
                // work write stays operator-only.
                | Self::CampaignWorkShow
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
            | Self::WhatsappReplySend
            | Self::WhatsappTemplatePrepare
            | Self::WhatsappTemplateSend => 49_152,
            // CAD-1662: a Works revision holds up to 64 KiB of content plus
            // title, tags, operations and key overhead; not widened for others.
            Self::WorksCreate | Self::WorksPatch => 81_920,
            // CAD-1590: an individual audience holds up to 1000 ids.
            Self::WhatsappBroadcastDraft => 196_608,
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
            if !valid_tag(&section.id)
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
    /// v2 `screen-records.v1` only: the declared operations resolved
    /// against the closed native vocabulary. Always fully resolved: this
    /// value is only ever produced by the native-vocabulary admissions.
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
    /// The declared `screen-records.v1` operations, resolved against the
    /// closed native vocabulary.
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
    /// The declared app tag (`valid_tag` grammar).
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
    /// Consume the value for its app tag — the only field an ABI success
    /// value carries, so the encoded result never clones it.
    pub fn into_app(self) -> String {
        self.app
    }
}

/// The proven facts of [`validate_facts_asset_integrity`] — the declared app
/// tag, and nothing else.
///
/// Deliberately *not* an [`IntegrityCheckedScreen`]. The host-owned admission
/// never resolves the declaration's `operations` against any vocabulary, so
/// there is no operation list this admission could honestly publish: handing
/// back a screen value with an empty list would read as a verified empty set.
/// A caller therefore cannot confuse the host-owned result with a native one,
/// and this value grants no operation, call, slot, install, mount, execution
/// or effect authority.
#[derive(Debug)]
pub struct AssetIntegrityChecked {
    app: String,
}

impl AssetIntegrityChecked {
    /// The declared app tag (`valid_tag` grammar) — the same value
    /// `app.screen.integrity` returns for a declaration it admits.
    pub fn app(&self) -> &str {
        &self.app
    }
    /// Consume the value for its app tag, moving the one String it holds.
    pub fn into_app(self) -> String {
        self.app
    }
}

/// The closed top-level `app-screens/v1` shape. `deny_unknown_fields`
/// refuses stray keys; serde's own duplicate-field error refuses
/// repeated ones. `may` is the optional method list — the contract
/// carries none, so only absence or `[]` passes the explicit check.
///
/// `O` is the declaration's operation-list type, and it *is* the admission:
/// the native paths instantiate [`ScreenOperation`] so serde resolves the
/// closed vocabulary during the one typed decode (no name strings, no second
/// parse, unchanged parse-time refusal of an unknown operation), while
/// [`validate_facts_asset_integrity`] instantiates `String` so a host token is
/// bounded and typed but never resolved against this build's vocabulary.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDecl<O> {
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
    /// A repeated alias refuses: a map decode would otherwise keep only
    /// the last slot and hide the shadowed one.
    #[serde(default, deserialize_with = "unique_map")]
    tools: BTreeMap<String, String>,
    /// v2 `screen-records.v1` only; see [`DeclaredOperations`].
    #[serde(default)]
    operations: O,
    #[serde(default)]
    navigation: Option<ScreenNavigation>,
    #[serde(default)]
    startup: Option<Vec<String>>,
}

/// The `may` grants a declaration actually carries, reduced to the two facts
/// the native operation rule needs. Computed once while `may` is validated, so
/// the admission never re-borrows (or re-allocates) the declaration's `may`
/// list after the declaration is consumed.
#[derive(Default)]
struct MayGrants {
    records_call: bool,
    records_slot: bool,
}

/// One declaration's `operations` list under the admission its host runs.
///
/// The type of the decoded list chooses the implementation, so a host-owned
/// list can never be mistaken for a native one. The two methods match the two
/// points the declaration is needed: [`validate`](Self::validate) applies the
/// admission's operation rules where the original validator applied them —
/// before asset grammar and before any body is hashed — and
/// [`into_checked`](Self::into_checked) consumes the declaration at the end to
/// assemble only the fields that admission publishes.
trait DeclaredOperations: Sized {
    /// The published result of this admission.
    type Checked;
    /// How many operations the declaration lists.
    fn len(&self) -> usize;
    /// Whether the declaration lists none.
    fn is_empty(&self) -> bool;
    /// Apply this admission's operation rules. Borrowed: the list stays in the
    /// declaration until the result is assembled.
    fn validate(&self, grants: MayGrants) -> Result<()>;
    /// Assemble the published result, consuming the declaration so each
    /// admission moves only the fields it actually carries.
    fn into_checked(decl: RawDecl<Self>) -> Self::Checked;
}

impl DeclaredOperations for Vec<ScreenOperation> {
    type Checked = IntegrityCheckedScreen;

    fn len(&self) -> usize {
        self.as_slice().len()
    }
    fn is_empty(&self) -> bool {
        self.as_slice().is_empty()
    }
    /// The native rule, unchanged: a repeated operation or an operation whose
    /// CALL/SLOT class is not granted in `may` refuses.
    fn validate(&self, grants: MayGrants) -> Result<()> {
        let mut seen = HashSet::with_capacity(self.len());
        for operation in self {
            let granted = if operation.is_slot() {
                grants.records_slot
            } else {
                grants.records_call
            };
            if !seen.insert(*operation) || !granted {
                return Err(Error::rejected(
                    "duplicate operation or missing records permission",
                ));
            }
        }
        Ok(())
    }
    /// Moves the already-validated list and the rest of the metadata straight
    /// into the result — no clone, no per-operation allocation.
    fn into_checked(decl: RawDecl<Self>) -> IntegrityCheckedScreen {
        IntegrityCheckedScreen {
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
                .into_iter()
                .filter_map(|value| match value {
                    Value::String(value) => Some(value),
                    _ => None,
                })
                .collect(),
            host_contract: decl.host_contract,
            tools: decl.tools,
            operations: decl.operations,
            navigation: decl.navigation,
            startup: decl.startup,
        }
    }
}

impl DeclaredOperations for Vec<String> {
    type Checked = AssetIntegrityChecked;

    fn len(&self) -> usize {
        self.as_slice().len()
    }
    fn is_empty(&self) -> bool {
        self.as_slice().is_empty()
    }
    /// The host-owned rule: bounded, typed and duplicate-free tokens only. No
    /// grant or membership check exists here to run — the caller applies its
    /// own closed operation set and its own CALL/SLOT classification.
    fn validate(&self, _grants: MayGrants) -> Result<()> {
        let mut seen: HashSet<&str> = HashSet::with_capacity(self.len());
        for name in self {
            if !seen.insert(name.as_str()) {
                return Err(Error::rejected(format!(
                    "operation {name:?} is declared twice"
                )));
            }
            check_host_operation(name)?;
        }
        Ok(())
    }
    /// Only the app tag is moved out; nothing else is assembled.
    fn into_checked(decl: RawDecl<Self>) -> AssetIntegrityChecked {
        AssetIntegrityChecked { app: decl.app }
    }
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

/// One supplied asset as a host *reports* it: the exact byte length of the
/// installed UTF-8 body and the `sha256:<64 lowercase hex>` digest the host
/// computed over those same bytes.
///
/// The validator compares a declaration against these facts and never sees
/// the bytes, so the agreement it proves is with the caller's report. A
/// caller that has not hashed the exact bytes it installed has proved only
/// self-consistency, not package integrity.
#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScreenAssetFact {
    /// Exact UTF-8 byte length of the supplied body.
    pub size: u64,
    /// The body's `sha256:<64 lowercase hex>` digest.
    pub sha256: String,
}

/// The supplied side of one declaration↔supplied comparison, borrowed.
///
/// Only lengths are read while the bound stage runs; a digest is produced
/// only after the declaration's own grammar, size and digest syntax already
/// agreed, so a declaration that is invalid for any other reason never
/// causes a body to be hashed.
trait Supplied {
    /// How many assets were supplied.
    fn count(&self) -> usize;
    /// Every supplied name with its byte length, borrowed from the caller.
    fn lengths(&self) -> impl Iterator<Item = (&str, u64)>;
    /// The byte length supplied for `name`, or `None` when it is absent.
    fn length(&self, name: &str) -> Option<u64>;
    /// The `sha256:<64 lowercase hex>` digest of the supplied bytes for
    /// `name`: computed on demand from a body, quoted from a fact.
    fn sha256(&self, name: &str) -> Option<Cow<'_, str>>;
}

/// Asset bodies the caller holds: the digest is computed from the bytes.
struct SuppliedBodies<'a>(&'a BTreeMap<String, String>);

impl Supplied for SuppliedBodies<'_> {
    fn count(&self) -> usize {
        self.0.len()
    }
    fn lengths(&self) -> impl Iterator<Item = (&str, u64)> {
        self.0
            .iter()
            .map(|(name, body)| (name.as_str(), body.len() as u64))
    }
    fn length(&self, name: &str) -> Option<u64> {
        self.0.get(name).map(|body| body.len() as u64)
    }
    fn sha256(&self, name: &str) -> Option<Cow<'_, str>> {
        self.0
            .get(name)
            .map(|body| Cow::Owned(format!("sha256:{:x}", Sha256::digest(body.as_bytes()))))
    }
}

/// Host-reported facts: no bytes, so nothing is hashed here at all.
struct SuppliedFacts<'a>(&'a BTreeMap<String, ScreenAssetFact>);

impl Supplied for SuppliedFacts<'_> {
    fn count(&self) -> usize {
        self.0.len()
    }
    fn lengths(&self) -> impl Iterator<Item = (&str, u64)> {
        self.0.iter().map(|(name, fact)| (name.as_str(), fact.size))
    }
    fn length(&self, name: &str) -> Option<u64> {
        self.0.get(name).map(|fact| fact.size)
    }
    fn sha256(&self, name: &str) -> Option<Cow<'_, str>> {
        self.0
            .get(name)
            .map(|fact| Cow::Borrowed(fact.sha256.as_str()))
    }
}

/// Validate one declaration against caller-supplied UTF-8 asset bodies.
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
    validate_with::<_, Vec<ScreenOperation>>(manifest, &SuppliedBodies(assets))
}

/// Validate one declaration against host-reported facts for the exact bytes
/// that host installed — the same pipeline [`validate_map`] runs, with the
/// digests quoted from the caller instead of computed from bodies.
///
/// Nothing is hashed here: the facts are the whole supplied side. See
/// [`ScreenAssetFact`] for what the agreement does and does not prove.
pub fn validate_facts(
    manifest: &str,
    facts: &BTreeMap<String, ScreenAssetFact>,
) -> Result<IntegrityCheckedScreen> {
    validate_with::<_, Vec<ScreenOperation>>(manifest, &SuppliedFacts(facts))
}

/// Validate one declaration against host-reported facts under the
/// **host-owned operation admission** — the shared body-integrity half of
/// `app.screen.asset-integrity`.
///
/// It runs the identical pipeline as [`validate_facts`]: same byte, depth,
/// node, asset, grammar, `may`/`tools`/navigation/startup and
/// declared↔supplied checks, so a malformed declaration, a duplicated JSON
/// field or operation, a wrong size or digest refuses exactly as before. The
/// single difference is the `operations` list: each name must be a bounded,
/// typed, duplicate-free host token, but the core does **not** resolve it
/// against [`ScreenOperation`] and does **not** require a `records.call` or
/// `records.slot` grant, because which operations a host serves and how it
/// classifies them is that host's own closed vocabulary.
///
/// The result is the narrower [`AssetIntegrityChecked`] — the proven app tag
/// only, moved out of the decoded declaration. No screen metadata is
/// assembled: the host path never builds a `may` list or any other field it
/// would immediately drop. The value cannot be passed off as an
/// [`IntegrityCheckedScreen`], so an operation set this crate never resolved
/// can never read as a verified one. It authorizes nothing: a host that
/// admits a package must still check each token against its own operation set
/// and its own permission classification at every publication, install and
/// revalidation entry point.
pub fn validate_facts_asset_integrity(
    manifest: &str,
    facts: &BTreeMap<String, ScreenAssetFact>,
) -> Result<AssetIntegrityChecked> {
    validate_with::<_, Vec<String>>(manifest, &SuppliedFacts(facts))
}

/// The one pipeline every entry point runs: one bound set, one grammar, one
/// declared↔supplied comparison.
///
/// `O` selects the operation admission at compile time: [`ScreenOperation`]
/// resolves the closed native vocabulary inside the typed decode, `String`
/// leaves a host token unresolved. `O::Checked` is that admission's own
/// result, assembled by moving only the fields it publishes — the native path
/// keeps every screen field, the host path keeps the app tag.
fn validate_with<S, O>(manifest: &str, supplied: &S) -> Result<O::Checked>
where
    S: Supplied,
    O: DeclaredOperations,
    for<'de> O: Deserialize<'de> + Default,
{
    // Supplied-side bounds first — before anything copies, parses or hashes a
    // byte. Only lengths are read.
    check_supplied_bounds(supplied)?;

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
    // The one map-valued field (`tools`) carries its own duplicate-key guard.
    // For the native admission the operation vocabulary is resolved here, by
    // serde, exactly as before.
    let decl: RawDecl<O> = serde_json::from_str(manifest)
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
    check_declaration(decl, supplied)
}

/// Supplied-side bounds: count, then each length, then the aggregate — all
/// before a single byte is parsed or hashed.
fn check_supplied_bounds<S: Supplied>(supplied: &S) -> Result<()> {
    let count = supplied.count();
    if count > MAX_ASSETS {
        return Err(Error::rejected(format!(
            "{count} supplied assets — at most {MAX_ASSETS}"
        )));
    }
    let mut total_bytes = 0u64;
    for (name, len) in supplied.lengths() {
        if len > MAX_TOTAL_BYTES {
            return Err(Error::rejected(format!(
                "asset {name:?} body is {len} bytes — at most {MAX_TOTAL_BYTES}"
            )));
        }
        // At most `MAX_ASSETS` lengths, each already bounded: no overflow.
        total_bytes += len;
    }
    if total_bytes > MAX_TOTAL_BYTES {
        return Err(Error::rejected(format!(
            "asset bodies total {total_bytes} bytes — at most {MAX_TOTAL_BYTES}"
        )));
    }
    Ok(())
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

/// A host-owned operation token: 1..=64 bytes of printable, non-blank ASCII.
///
/// This is the *bound and type* half only — the three properties the shared
/// contract keeps for a host token: bounded, typed (a JSON string) and
/// duplicate-free (checked by the caller loop). It deliberately does **not**
/// impose a vocabulary grammar: which operations a host serves, and whether
/// each is a CALL or a host-confirmed SLOT, is the caller's closed set and
/// classification, and a core-side grammar would only re-block a legitimate
/// host name. No asset byte is compared for a token this bound refuses.
fn check_host_operation(name: &str) -> Result<()> {
    let shaped = (1..=MAX_OPERATION_BYTES).contains(&name.len())
        && name.bytes().all(|b| b.is_ascii_graphic());
    if !shaped {
        return Err(Error::rejected(format!(
            "operation {name:?} — 1 to {MAX_OPERATION_BYTES} printable ASCII bytes"
        )));
    }
    Ok(())
}

/// The grammar and declared↔supplied match for one decoded declaration,
/// consumed so the admission can assemble its own result from moved fields.
fn check_declaration<S, O>(decl: RawDecl<O>, supplied: &S) -> Result<O::Checked>
where
    S: Supplied,
    O: DeclaredOperations,
{
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
    if !valid_tag(&decl.app) {
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
    // Scoped so the duplicate-detection set's borrow of `decl.may` ends before
    // the declaration is consumed by its admission.
    let grants = {
        let mut may_seen = HashSet::new();
        let mut grants = MayGrants::default();
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
                                MAY_MEDIA_READ
                                    | MAY_MEDIA_PUBLISH
                                    | MAY_MEDIA_IMPORT
                                    | MAY_MEDIA_CHAT_ATTACH
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
            grants.records_call |= name == MAY_RECORDS_CALL;
            grants.records_slot |= name == MAY_RECORDS_SLOT;
        }
        grants
    };
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
    // Contract-level operation rules and the admission's own operation rules
    // run here, before `tools`/`remote_images`/asset grammar and before
    // `check_assets` — so a declaration already known invalid is refused
    // before any supplied body is compared (or hashed). Result assembly
    // happens at the end, from the moved declaration.
    if decl.host_contract.as_deref() == Some(HOST_CONTRACT_RECORDS) {
        if !decl.tools.is_empty() || decl.operations.len() > MAX_OPERATIONS {
            return Err(Error::rejected(format!(
                "records screens require empty tools and at most {MAX_OPERATIONS} operations"
            )));
        }
        decl.operations.validate(grants)?;
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
        if !valid_tag(slot) {
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
        let bound = MAX_TOTAL_BYTES;
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
    check_assets(&decl.assets, supplied)?;
    Ok(O::into_checked(decl))
}

/// A flat leaf `<stem>.<ext>`: the stem is `valid_tag` (which
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
    if !valid_tag(stem) {
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

/// The declared↔supplied match: every declared asset is supplied with exactly
/// the declared UTF-8 byte length and sha256, and every supplied name is
/// declared — no undeclared asset passes. The digest is compared last, so a
/// declaration that is invalid for any other reason never hashes a body.
fn check_assets<S: Supplied>(assets: &[RawAsset], supplied: &S) -> Result<()> {
    for asset in assets {
        let Some(len) = supplied.length(&asset.name) else {
            return Err(Error::rejected(format!(
                "declared asset {:?} is not supplied",
                asset.name
            )));
        };
        if asset.size != len {
            return Err(Error::rejected(format!(
                "asset {:?} declares {} bytes — the body is {len}",
                asset.name, asset.size
            )));
        }
        let Some(actual) = supplied.sha256(&asset.name) else {
            return Err(Error::rejected(format!(
                "declared asset {:?} is not supplied",
                asset.name
            )));
        };
        if asset.sha256.as_str() != &*actual {
            return Err(Error::rejected(format!(
                "asset {:?} sha256 does not match its body",
                asset.name
            )));
        }
    }
    let declared: HashSet<&str> = assets.iter().map(|a| a.name.as_str()).collect();
    for (name, _) in supplied.lengths() {
        if !declared.contains(name) {
            return Err(Error::rejected(format!(
                "supplied asset {name:?} is not declared"
            )));
        }
    }
    Ok(())
}

/// A string→string map that refuses a repeated key. serde's map decoding keeps
/// the last value for a repeated key, so a declaration could otherwise hide a
/// shadowed tool slot behind one. Every other object in a declaration is a
/// closed struct, whose derived decode already refuses a repeated field.
fn unique_map<'de, D>(deserializer: D) -> std::result::Result<BTreeMap<String, String>, D::Error>
where
    D: Deserializer<'de>,
{
    struct UniqueMap;
    impl<'de> Visitor<'de> for UniqueMap {
        type Value = BTreeMap<String, String>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a map of string values without a repeated key")
        }
        fn visit_map<A: MapAccess<'de>>(
            self,
            mut map: A,
        ) -> std::result::Result<Self::Value, A::Error> {
            let mut out = BTreeMap::new();
            while let Some(key) = map.next_key::<String>()? {
                if out.contains_key(&key) {
                    return Err(serde::de::Error::custom(format!(
                        "tool alias {key:?} is declared twice"
                    )));
                }
                out.insert(key, map.next_value::<String>()?);
            }
            Ok(out)
        }
    }
    deserializer.deserialize_map(UniqueMap)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BODY: &str = "console.log(1);\n";

    fn digest(bytes: &str) -> String {
        format!("sha256:{:x}", Sha256::digest(bytes.as_bytes()))
    }

    /// A genuine `app-screens/v1` declaration for exactly `body` as
    /// `client.js`, with every digest computed from the same bytes.
    fn declaration(app: &str, body: &str) -> String {
        serde_json::json!({
            "contract": "app-screens/v1",
            "app": app,
            "entry": "client.js",
            "assets": [{
                "name": "client.js",
                "media_type": "text/javascript",
                "sha256": digest(body),
                "size": body.len(),
            }],
            "provenance": {
                "source_digest": digest("source"),
                "sdk_digest": digest("sdk"),
                "toolchain_digest": digest("toolchain"),
            },
        })
        .to_string()
    }

    fn facts(body: &str) -> BTreeMap<String, ScreenAssetFact> {
        BTreeMap::from([(
            "client.js".to_string(),
            ScreenAssetFact {
                size: body.len() as u64,
                sha256: digest(body),
            },
        )])
    }

    /// One implementation, two callers: the bodies a native host holds and the
    /// facts a hosted host reports reach the same verdict.
    #[test]
    fn facts_and_bodies_reach_the_same_verdict() {
        let manifest = declaration("demo", BODY);
        let bodies = BTreeMap::from([("client.js".to_string(), BODY.to_string())]);
        let from_bodies = validate_map(&manifest, &bodies).expect("bodies agree");
        let from_facts = validate_facts(&manifest, &facts(BODY)).expect("facts agree");
        assert_eq!(from_facts.app(), from_bodies.app());
        assert_eq!(from_facts.app(), "demo");
        assert_eq!(from_facts.asset_count(), 1);
        assert_eq!(from_facts.entry(), from_bodies.entry());
    }

    /// Every way the supplied facts can disagree with the declaration refuses:
    /// a same-length body swap (digest only), a wrong size, a missing fact and
    /// an undeclared fact.
    #[test]
    fn wrong_facts_are_refused() {
        let manifest = declaration("demo", BODY);
        let swapped = BODY.replace('1', "2");
        assert_eq!(swapped.len(), BODY.len());

        let mut tampered = facts(BODY);
        tampered.get_mut("client.js").unwrap().sha256 = digest(&swapped);
        assert!(validate_facts(&manifest, &tampered).is_err());

        let mut sized = facts(BODY);
        sized.get_mut("client.js").unwrap().size += 1;
        assert!(validate_facts(&manifest, &sized).is_err());

        assert!(validate_facts(&manifest, &BTreeMap::new()).is_err());

        let mut extra = facts(BODY);
        extra.insert(
            "styles.css".to_string(),
            ScreenAssetFact {
                size: 0,
                sha256: digest(""),
            },
        );
        assert!(validate_facts(&manifest, &extra).is_err());
    }

    /// Duplicate object keys refuse wherever they appear: a repeated top-level
    /// field (serde's derived decode) and a repeated key inside the `tools`
    /// map, which a plain map decode would otherwise collapse to the last
    /// value. Each refusal is proved by behaviour, not by refusal wording: the
    /// same declaration carrying the key once is admitted.
    #[test]
    fn a_repeated_key_anywhere_is_refused() {
        let manifest = declaration("demo", BODY);
        let repeated_field = manifest.replacen('{', "{\"app\":\"other\",", 1);
        assert!(validate_facts(&repeated_field, &facts(BODY)).is_err());

        let v2_with_tools = |tools: &str| {
            manifest
                .replacen("\"app-screens/v1\"", "\"app-screens/v2\"", 1)
                .replacen(
                    '{',
                    &format!("{{\"host_contract\":\"screen-media.v1\",\"tools\":{tools},"),
                    1,
                )
        };
        let unique_alias = v2_with_tools("{\"media.read\":\"catalog\"}");
        assert!(
            validate_facts(&unique_alias, &facts(BODY)).is_ok(),
            "one alias is a valid declaration: {unique_alias}"
        );

        let repeated_alias =
            v2_with_tools("{\"media.read\":\"catalog\",\"media.read\":\"catalog\"}");
        assert!(
            validate_facts(&repeated_alias, &facts(BODY)).is_err(),
            "a shadowed tool alias must refuse: {repeated_alias}"
        );
    }

    /// The declaration grammar and bounds still decide before any digest is
    /// compared: a declaration that breaks the closed grammar refuses from
    /// bodies and from facts alike, while the valid one it was derived from is
    /// admitted by both.
    #[test]
    fn declaration_rules_are_unchanged() {
        let manifest = declaration("demo", BODY);
        let bodies = BTreeMap::from([("client.js".to_string(), BODY.to_string())]);
        assert!(validate_map(&manifest, &bodies).is_ok());
        assert!(validate_facts(&manifest, &facts(BODY)).is_ok());

        let broken = manifest.replacen("\"app\":\"demo\"", "\"app\":\"Demo\"", 1);
        assert!(validate_map(&broken, &bodies).is_err());
        assert!(validate_facts(&broken, &facts(BODY)).is_err());
    }

    /// A `screen-records.v1` declaration — the one contract that carries an
    /// operations list — for exactly `body` as `client.js`.
    fn records_declaration(body: &str, may: &[&str], operations: &[&str]) -> String {
        serde_json::json!({
            "contract": "app-screens/v2",
            "app": "demo",
            "entry": "client.js",
            "assets": [{
                "name": "client.js",
                "media_type": "text/javascript",
                "sha256": digest(body),
                "size": body.len(),
            }],
            "provenance": {
                "source_digest": digest("source"),
                "sdk_digest": digest("sdk"),
                "toolchain_digest": digest("toolchain"),
            },
            "host_contract": "screen-records.v1",
            "may": may,
            "operations": operations,
        })
        .to_string()
    }

    /// The seam this operation exists for: a host-only operation passes the
    /// host-owned asset-integrity admission while every native/strict
    /// admission still refuses it. The admitted value is the narrow
    /// [`AssetIntegrityChecked`] — the app tag and nothing else, so no
    /// operation list can be read as verified — and it grants nothing.
    #[test]
    fn a_host_only_operation_passes_only_the_asset_integrity_admission() {
        let manifest = records_declaration(BODY, &[MAY_RECORDS_CALL], &["sends.cancel"]);
        let checked = validate_facts_asset_integrity(&manifest, &facts(BODY))
            .expect("a well-formed host operation token is admitted");
        assert_eq!(checked.app(), "demo");

        assert!(
            validate_facts(&manifest, &facts(BODY)).is_err(),
            "the strict fact admission still refuses an operation this core does not serve"
        );
        let bodies = BTreeMap::from([("client.js".to_string(), BODY.to_string())]);
        assert!(
            validate_map(&manifest, &bodies).is_err(),
            "the native body admission still refuses it"
        );

        // The same declaration with a native operation passes both: the
        // host-owned admission decides the token bound only, never membership.
        let native = records_declaration(BODY, &[MAY_RECORDS_CALL], &["records.list"]);
        assert!(validate_facts_asset_integrity(&native, &facts(BODY)).is_ok());
        assert!(validate_facts(&native, &facts(BODY)).is_ok());

        // Deferring the CALL/SLOT grant to the caller does not widen the
        // `may` vocabulary or any other declaration rule.
        let ungranted_may = records_declaration(BODY, &["records.nope"], &["sends.cancel"]);
        assert!(validate_facts_asset_integrity(&ungranted_may, &facts(BODY)).is_err());
        let slot_on_actions = records_declaration(BODY, &[MAY_RECORDS_SLOT], &["sends.cancel"])
            .replacen("\"screen-records.v1\"", "\"screen-actions.v1\"", 1);
        assert!(validate_facts_asset_integrity(&slot_on_actions, &facts(BODY)).is_err());
    }

    /// The host-owned admission compares bytes exactly as the strict one
    /// does: a same-length body swap, a wrong size, a missing fact and an
    /// undeclared fact all refuse.
    #[test]
    fn asset_integrity_still_compares_the_supplied_bytes_exactly() {
        let manifest = records_declaration(BODY, &[MAY_RECORDS_CALL], &["sends.cancel"]);
        assert!(validate_facts_asset_integrity(&manifest, &facts(BODY)).is_ok());

        let swapped = BODY.replace('1', "2");
        assert_eq!(swapped.len(), BODY.len());

        let mut tampered = facts(BODY);
        tampered.get_mut("client.js").unwrap().sha256 = digest(&swapped);
        assert!(validate_facts_asset_integrity(&manifest, &tampered).is_err());

        let mut sized = facts(BODY);
        sized.get_mut("client.js").unwrap().size += 1;
        assert!(validate_facts_asset_integrity(&manifest, &sized).is_err());

        assert!(validate_facts_asset_integrity(&manifest, &BTreeMap::new()).is_err());

        let mut extra = facts(BODY);
        extra.insert(
            "styles.css".to_string(),
            ScreenAssetFact {
                size: 0,
                sha256: digest(""),
            },
        );
        assert!(validate_facts_asset_integrity(&manifest, &extra).is_err());
    }

    /// Deferring the vocabulary does not weaken the shared parser:
    /// malformed and duplicated declarations still refuse, operation tokens
    /// stay typed, bounded and duplicate-free, and an unknown contract or
    /// field still refuses.
    #[test]
    fn asset_integrity_still_refuses_malformed_and_duplicate_declarations() {
        let manifest = records_declaration(BODY, &[MAY_RECORDS_CALL], &["sends.cancel"]);
        assert!(validate_facts_asset_integrity(&manifest, &facts(BODY)).is_ok());

        // A repeated operation entry, in the list and as a JSON field.
        let duplicate =
            records_declaration(BODY, &[MAY_RECORDS_CALL], &["sends.cancel", "sends.cancel"]);
        assert!(validate_facts_asset_integrity(&duplicate, &facts(BODY)).is_err());
        let repeated_field = manifest.replacen('{', "{\"app\":\"other\",", 1);
        assert!(validate_facts_asset_integrity(&repeated_field, &facts(BODY)).is_err());

        // The token bound stays enforced: non-empty, printable ASCII, at most
        // 64 bytes. The vocabulary grammar is the caller's, so a name like
        // `Sends.cancel` is a well-formed token here and the host's own
        // closed set is what refuses it.
        for bad in [
            "",
            "sends cancel",
            "sends\tcancel",
            "sends\ncancel",
            "sends.cancél",
        ] {
            let malformed = records_declaration(BODY, &[MAY_RECORDS_CALL], &[bad]);
            assert!(
                validate_facts_asset_integrity(&malformed, &facts(BODY)).is_err(),
                "operation {bad:?} must refuse"
            );
        }
        let too_long = "a".repeat(MAX_OPERATION_BYTES + 1);
        let malformed = records_declaration(BODY, &[MAY_RECORDS_CALL], &[too_long.as_str()]);
        assert!(validate_facts_asset_integrity(&malformed, &facts(BODY)).is_err());

        // A non-string member refuses through the typed decode.
        let non_string = manifest.replacen("\"sends.cancel\"", "1", 1);
        assert!(validate_facts_asset_integrity(&non_string, &facts(BODY)).is_err());

        // The operation count keeps its bound, at 64 accepted and 65 refused.
        {
            let names: Vec<String> = (0..MAX_OPERATIONS).map(|i| format!("sends.o{i}")).collect();
            let refs: Vec<&str> = names.iter().map(String::as_str).collect();
            let full = records_declaration(BODY, &[MAY_RECORDS_CALL], &refs);
            assert!(validate_facts_asset_integrity(&full, &facts(BODY)).is_ok());
        }
        {
            let mut names: Vec<String> =
                (0..MAX_OPERATIONS).map(|i| format!("sends.o{i}")).collect();
            names.push("sends.overflow".to_string());
            let refs: Vec<&str> = names.iter().map(String::as_str).collect();
            let oversized = records_declaration(BODY, &[MAY_RECORDS_CALL], &refs);
            assert!(validate_facts_asset_integrity(&oversized, &facts(BODY)).is_err());
        }

        // Operations still require the records host contract, the exact
        // provenance/digest grammar, and valid JSON.
        let actions = manifest.replacen("\"screen-records.v1\"", "\"screen-actions.v1\"", 1);
        assert!(validate_facts_asset_integrity(&actions, &facts(BODY)).is_err());
        let v1 = manifest.replacen("\"app-screens/v2\"", "\"app-screens/v1\"", 1);
        assert!(validate_facts_asset_integrity(&v1, &facts(BODY)).is_err());
        let bad_digest = manifest.replacen(&digest(BODY), "sha256:short", 1);
        assert!(validate_facts_asset_integrity(&bad_digest, &facts(BODY)).is_err());
        assert!(validate_facts_asset_integrity("{", &facts(BODY)).is_err());
    }
}
