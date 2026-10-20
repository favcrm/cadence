//! Closed screen-records adapter. No caller-selected native method, scope,
//! credential connection or approval material reaches an executor.
use super::{app_tools_rpc::ToolContext, required_str, Shared};
use crate::error::{Error, Result};
use crate::issue::{
    app_catalog::workspace,
    app_screen_decl::{ScreenOperation, HOST_CONTRACT_RECORDS, MAY_RECORDS_CALL, MAY_RECORDS_SLOT},
    app_screen_pkg,
};
use crate::operator_auth::Origin;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::{Duration, Instant};

const RESULT_CAP: usize = 1_048_576;
const CONFIRM_TTL: Duration = Duration::from_secs(60);

fn screen_access_denied(reason: &'static str) -> Error {
    Error::invalid("screen_access_denied", reason)
}

// Only authority derivations use this adapter. Storage/transport failures
// keep their class, and executor/CAS errors never pass through it.
fn screen_authority_error(error: Error, reason: &'static str) -> Error {
    if error.kind() == "rejected" {
        screen_access_denied(reason)
    } else {
        error
    }
}

/// Private, non-deserializable authority carried into the actual executor.
/// Every use re-proves the stored context inside its runtime/release fence.
#[derive(Clone)]
pub(super) struct ScreenServiceProof {
    action_token: String,
    // Server-private bearer; only native attribution consumers receive it.
    public_session: Option<String>,
    context: ToolContext,
    operation: ScreenOperation,
    confirmed: Option<(Value, Value)>,
    csv_confirmation: Option<(String, String)>,
    command_args: Option<Value>,
}

impl ScreenServiceProof {
    pub(super) fn install(&self) -> &str {
        &self.context.install_id
    }
    pub(super) fn context(&self) -> Result<&str> {
        self.context
            .context_id
            .as_deref()
            .ok_or_else(|| screen_access_denied("missing screen context"))
    }
    pub(super) fn csv_confirmation(&self) -> Option<(&str, &str)> {
        self.csv_confirmation
            .as_ref()
            .map(|(token, digest)| (token.as_str(), digest.as_str()))
    }
}

pub(super) struct ScreenServiceConfirmation {
    proof: ScreenServiceProof,
    args: Value,
    facts: Value,
    issued: Instant,
}

impl ScreenServiceConfirmation {
    pub(super) fn current_in(
        &self,
        mounts: &std::collections::HashMap<String, ToolContext>,
    ) -> bool {
        self.issued.elapsed() < CONFIRM_TTL
            && mounts.get(&self.proof.action_token).is_some_and(|ctx| {
                ctx.mount == self.proof.context.mount && ctx.session == self.proof.context.session
            })
    }
}

struct CsvInFlight<'a> {
    key: String,
    live: &'a std::sync::Mutex<std::collections::HashSet<String>>,
}
impl Drop for CsvInFlight<'_> {
    fn drop(&mut self) {
        self.live
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.key);
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CallRequest {
    action_token: String,
    operation: ScreenOperation,
    args: Value,
    token: String,
    #[serde(default)]
    key: String,
    origin: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfirmRequest {
    action_token: String,
    confirmation_token: String,
    confirmation: Confirmation,
    token: String,
    #[serde(default)]
    key: String,
    origin: String,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Confirmation {
    Tap(Tap),
    Recipient(Recipient),
    Connection(Connection),
    Resolution(Resolution),
    GlobalOrigin(GlobalOrigin),
    Csv(CsvConfirmation),
    Brief(BriefConfirmation),
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BriefConfirmation {
    confirmed: bool,
    brief: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Tap {
    confirmed: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Recipient {
    confirmed: bool,
    to_email: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Connection {
    confirmed: bool,
    connection_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Resolution {
    confirmed: bool,
    resolution: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GlobalOrigin {
    confirmed: bool,
    unsubscribe_origin: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CsvConfirmation {
    confirmed: bool,
    csv_confirm_token: String,
}

// Each operation decodes a closed top-level body; native nested validators
// remain authoritative. Option fields are omitted, never converted to null.
macro_rules! args {
    ($name:ident { $($field:ident : $ty:ty),* $(,)? }) => {
        #[derive(Deserialize, Serialize)]
        #[serde(deny_unknown_fields)]
        struct $name { $($field: $ty),* }
    };
}
args!(Empty {});
args!(RecordList { query: Option<String>, limit: Option<u64>, cursor: Option<String> });
args!(RecordId { record_id: String });
args!(RecordCreate { record_id: String, profile: Value, consent_provenance: Option<Value> });
args!(RecordUpdate { record_id: String, expected_revision: i64, profile: Value, consent_provenance: Option<Value> });
args!(CsvPreview { csv_text: String });
args!(CsvImport { csv_text: String, preview_token: String, request_id: String, decisions: Option<Vec<Value>> });
args!(SegmentId { segment_id: String });
args!(SegmentSave { segment_id: String, name: String, predicates: Vec<Value>, expected_revision: Option<i64> });
args!(ExclusionId { list_id: String });
args!(ExclusionSave { list_id: String, name: String, member_ids: Vec<String>, expected_revision: Option<i64> });
args!(SuppressionAdd { email: Option<String>, customer_id: Option<String>, reason: String });
args!(AudiencePreviewArgs { base: Value, exclusion_list_id: Option<String> });
args!(AudiencePrepareArgs { freeze_id: String, base: Value, exclusion_list_id: Option<String>, max_recipients: i64 });
args!(FreezeId { freeze_id: String });
args!(CampaignId {
    campaign_id: String
});
args!(ContentSaveArgs { campaign_id: String, subject: String, preheader: Option<String>, blocks: Option<Vec<Value>>, html: Option<String>, text: Option<String>, name: Option<String>, expected_revision: Option<i64> });
args!(ContentCloneArgs { campaign_id: String, expected_revision: i64, name: String, copy_audience: bool, copy_sender: bool, source_freeze_id: Option<String>, source_binding_id: Option<String> });
args!(ContentRenderArgs { campaign_id: String, revision: Option<i64>, sample_first_name: Option<String>, binding_id: Option<String> });
args!(ContentApproveArgs {
    campaign_id: String,
    expected_revision: i64
});
args!(ProposalList { campaign_id: Option<String> });
args!(ProposalId {
    proposal_id: String
});
args!(ProposalApply { proposal_id: String, expected_revision: Option<i64> });
args!(BindingId { binding_id: String });
// Deliberately NO connection_id: only the live admitted scoped SMTP link
// can supply custody selection, inside the executor fence.
args!(BindingSave { binding_id: String, sender_name: String, sender_address: String, unsubscribe_base: String, expected_revision: Option<i64> });
args!(TestPrepare { campaign_id: String, to_email: String, binding_id: Option<String> });
args!(SendPrepare { campaign_id: String, binding_id: String, audience_freeze_id: Option<String> });
args!(SendPrepareArgs {
    campaign_id: String,
    audience_freeze_id: String,
    request_id: String
});
args!(SendId { send_id: String });
args!(ResolveArgs {
    send_id: String,
    customer_id: String
});
args!(CampaignBrief {
    campaign_id: String,
    brief: String
});
args!(PreviewShow {
    preview_id: String,
    document: String,
    anchor: PreviewAnchor,
    mode: PreviewMode
});
args!(PreviewClear { preview_id: String });
args!(Anchor {
    x: f64,
    y: f64,
    w: f64,
    h: f64
});
#[derive(Deserialize, Serialize)]
#[serde(untagged)]
enum PreviewAnchor {
    Box(Anchor),
    Footer(Footer),
}
#[derive(Deserialize, Serialize)]
enum Footer {
    #[serde(rename = "footer")]
    Footer,
}
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum PreviewMode {
    Draft,
    Saved,
}
// CAD-1562: WhatsApp door arguments. No key, digest, company or
// connection field exists: the host derives each of them.
args!(WaPage { cursor: Option<String>, limit: Option<u64> });
args!(WaConversation {
    conversation_id: String
});
args!(WaMessages { conversation_id: String, cursor: Option<String>, limit: Option<u64> });
args!(WaReply { conversation_id: String, text: String, reply_to_message_id: Option<String> });
args!(WaReplySend { conversation_id: String, text: String, effect_id: String, reply_to_message_id: Option<String> });
args!(WaEffect { effect_id: String });
// CAD-1590: door v2. Nested audience/exclusions/variables stay JSON and
// are validated natively; no key, digest or connection field exists.
args!(WaTemplates { status: Option<String> });
args!(WaTemplate {
    template_id: String
});
args!(WaTemplatePrepare {
    conversation_id: String,
    template_id: String,
    variables: Value
});
args!(WaTemplateSend {
    effect_id: String,
    conversation_id: String,
    template_id: String,
    variables: Value
});
args!(WaBroadcast {
    broadcast_id: String
});
args!(WaResults { broadcast_id: String, cursor: Option<String>, limit: Option<u64>, status: Option<String> });
args!(WaDraft { broadcast_id: Option<String>, name: String, template_id: String, body_parameters: Value, header_media_url: Option<String>, audience: Value, exclusions: Option<Value> });
args!(WaTestPrepare {
    broadcast_id: String,
    to: String
});
args!(WaBroadcastPrepare { broadcast_id: String, send_at: Option<String> });
args!(RoutePatch { section: Option<String>, view: Option<String>, record_id: Option<String>, segment_id: Option<String>, replace: Option<bool> });

fn typed<T: for<'de> Deserialize<'de> + Serialize>(value: &Value) -> Result<Value> {
    let body: T = serde_json::from_value(value.clone())
        .map_err(|_| Error::rejected("screen service arguments exceed their closed shape"))?;
    let mut value = serde_json::to_value(body)?;
    // Option fields are absent on native APIs, not null. Unknown/forged keys
    // already refused above; this never strips caller-selected scope fields.
    if let Some(fields) = value.as_object_mut() {
        fields.retain(|_, value| !value.is_null());
    }
    Ok(value)
}

fn call_args(operation: ScreenOperation, value: &Value) -> Result<Value> {
    use ScreenOperation::*;
    match operation {
        RecordsList => typed::<RecordList>(value),
        RecordsShow => typed::<RecordId>(value),
        RecordsCreate => typed::<RecordCreate>(value),
        RecordsUpdate => typed::<RecordUpdate>(value),
        RecordsCsvPreview => typed::<CsvPreview>(value),
        SegmentsList | ExclusionsList | SuppressionsList | ContentList | SenderBindingsList => {
            typed::<Empty>(value)
        }
        SegmentsShow => typed::<SegmentId>(value),
        SegmentsSave => typed::<SegmentSave>(value),
        ExclusionsShow => typed::<ExclusionId>(value),
        ExclusionsSave => typed::<ExclusionSave>(value),
        SuppressionsAdd => typed::<SuppressionAdd>(value),
        AudiencePreview => typed::<AudiencePreviewArgs>(value),
        AudiencePrepare => typed::<AudiencePrepareArgs>(value),
        AudienceShow => typed::<FreezeId>(value),
        ContentShow => typed::<CampaignId>(value),
        ContentSave => typed::<ContentSaveArgs>(value),
        ContentClone => typed::<ContentCloneArgs>(value),
        ContentRender => typed::<ContentRenderArgs>(value),
        ProposalsList => typed::<ProposalList>(value),
        ProposalsShow | ProposalsRender | ProposalsDiscard => typed::<ProposalId>(value),
        ProposalsApply => typed::<ProposalApply>(value),
        SenderBindingsShow => typed::<BindingId>(value),
        SenderBindingsSave => typed::<BindingSave>(value),
        ContentTestPrepare => typed::<TestPrepare>(value),
        ContentSendPrepare => typed::<SendPrepare>(value),
        SendsPrepare => typed::<SendPrepareArgs>(value),
        SendsShow => typed::<SendId>(value),
        SendsList => typed::<ProposalList>(value),
        SmtpShow | OriginShow => typed::<Empty>(value),
        WhatsappStatus | WhatsappConnectSession => typed::<Empty>(value),
        WhatsappConversationsList => typed::<WaPage>(value),
        WhatsappConversationsShow => typed::<WaConversation>(value),
        WhatsappMessages => typed::<WaMessages>(value),
        WhatsappReplyPrepare => typed::<WaReply>(value),
        WhatsappReplyStatus => typed::<WaEffect>(value),
        WhatsappTemplatesList => typed::<WaTemplates>(value),
        WhatsappTemplateShow => typed::<WaTemplate>(value),
        WhatsappTemplatePrepare => typed::<WaTemplatePrepare>(value),
        WhatsappTemplateStatus => typed::<WaEffect>(value),
        WhatsappBroadcastsList => typed::<WaPage>(value),
        WhatsappBroadcastShow | WhatsappBroadcastEstimate => typed::<WaBroadcast>(value),
        WhatsappBroadcastResults => typed::<WaResults>(value),
        WhatsappBroadcastDraft => typed::<WaDraft>(value),
        WhatsappBroadcastTestPrepare => typed::<WaTestPrepare>(value),
        WhatsappBroadcastPrepare => typed::<WaBroadcastPrepare>(value),
        ScreenPreviewShow => typed::<PreviewShow>(value),
        ScreenPreviewClear => typed::<PreviewClear>(value),
        ScreenRoutePatch => {
            // Preserve explicit null clears and field absence separately.
            let _: RoutePatch = serde_json::from_value(value.clone())
                .map_err(|_| Error::rejected("unsupported route patch"))?;
            if value.get("replace").is_some_and(|flag| !flag.is_boolean()) {
                return Err(Error::rejected("route replace must be a boolean"));
            }
            Ok(value.clone())
        }
        _ => Err(Error::rejected(
            "operation is not an implemented non-protected screen CALL",
        )),
    }
}

fn slot_args(operation: ScreenOperation, value: &Value) -> Result<Value> {
    match operation {
        ScreenOperation::RecordsCsvImport | ScreenOperation::RecordsCsvConfirm => {
            typed::<CsvImport>(value)
        }
        ScreenOperation::ContentApprove => typed::<ContentApproveArgs>(value),
        ScreenOperation::SendsApprove => typed::<SendId>(value),
        ScreenOperation::SendsResolve => typed::<ResolveArgs>(value),
        ScreenOperation::SmtpBind
        | ScreenOperation::SmtpRebind
        | ScreenOperation::SmtpRevoke
        | ScreenOperation::SmtpManage
        | ScreenOperation::OriginManage => typed::<Empty>(value),
        ScreenOperation::SmtpTestSend | ScreenOperation::ChatCampaignOpen => {
            typed::<CampaignId>(value)
        }
        ScreenOperation::ChatCampaignBrief => typed::<CampaignBrief>(value),
        ScreenOperation::WhatsappReplySend => typed::<WaReplySend>(value),
        ScreenOperation::WhatsappTemplateSend => typed::<WaTemplateSend>(value),
        ScreenOperation::WhatsappBroadcastTestSend | ScreenOperation::WhatsappBroadcastSend => {
            typed::<WaEffect>(value)
        }
        ScreenOperation::WhatsappBroadcastStop => typed::<WaBroadcast>(value),
        _ => Err(Error::rejected("screen SLOT operation is not implemented")),
    }
}

fn native_method(operation: ScreenOperation) -> Result<&'static str> {
    use ScreenOperation::*;
    Ok(match operation {
        RecordsList => "app_record_list",
        RecordsShow => "app_record_show",
        RecordsCreate => "app_record_create",
        RecordsUpdate => "app_record_update",
        RecordsCsvPreview => "app_record_csv_preview",
        RecordsCsvImport => "app_record_csv_import",
        RecordsCsvConfirm => "app_record_csv_confirm",
        SegmentsList => "app_segment_list",
        SegmentsShow => "app_segment_show",
        SegmentsSave => "app_segment_save",
        ExclusionsList => "app_exclusion_list",
        ExclusionsShow => "app_exclusion_show",
        ExclusionsSave => "app_exclusion_save",
        SuppressionsList => "app_suppression_list",
        SuppressionsAdd => "app_suppression_add",
        AudiencePreview => "app_audience_preview",
        AudiencePrepare => "app_audience_prepare",
        AudienceShow => "app_audience_show",
        ContentList => "app_content_list",
        ContentShow => "app_content_show",
        ContentSave => "app_content_save",
        ContentClone => "app_content_clone",
        ContentRender => "app_content_render",
        ContentApprove => "app_content_approve",
        ProposalsList => "app_content_proposal_list",
        ProposalsShow => "app_content_proposal_show",
        ProposalsRender => "app_content_proposal_render",
        ProposalsApply => "app_content_proposal_apply",
        ProposalsDiscard => "app_content_proposal_discard",
        SenderBindingsList => "app_sender_binding_list",
        SenderBindingsShow => "app_sender_binding_show",
        SenderBindingsSave => "app_sender_binding_save",
        ContentTestPrepare => "app_content_test_prepare",
        ContentSendPrepare => "app_content_send_prepare",
        SendsPrepare => "crm_send_prepare",
        SendsShow => "crm_send_show",
        SendsList => "crm_send_list",
        SendsApprove => "crm_send_approve",
        SendsResolve => "crm_send_resolve",
        SmtpShow => "crm_smtp_show",
        SmtpBind => "crm_smtp_bind",
        SmtpRebind => "crm_smtp_rebind",
        SmtpRevoke => "crm_smtp_revoke",
        SmtpTestSend => "crm_smtp_test_send",
        OriginShow => "crm_send_origin_show",
        OriginManage => "crm_send_origin_set",
        WhatsappStatus => "whatsapp_status",
        WhatsappConnectSession => "whatsapp_connect_session",
        WhatsappConversationsList => "whatsapp_conversations_list",
        WhatsappConversationsShow => "whatsapp_conversations_show",
        WhatsappMessages => "whatsapp_messages",
        WhatsappReplyPrepare => "whatsapp_reply_prepare",
        WhatsappReplySend => "whatsapp_reply_send",
        WhatsappReplyStatus => "whatsapp_reply_status",
        WhatsappTemplatesList => "whatsapp_templates_list",
        WhatsappTemplateShow => "whatsapp_template_show",
        WhatsappTemplatePrepare => "whatsapp_template_prepare",
        WhatsappTemplateSend => "whatsapp_template_send",
        WhatsappTemplateStatus => "whatsapp_template_status",
        WhatsappBroadcastsList => "whatsapp_broadcasts_list",
        WhatsappBroadcastShow => "whatsapp_broadcast_show",
        WhatsappBroadcastEstimate => "whatsapp_broadcast_estimate",
        WhatsappBroadcastResults => "whatsapp_broadcast_results",
        WhatsappBroadcastDraft => "whatsapp_broadcast_draft",
        WhatsappBroadcastTestPrepare => "whatsapp_broadcast_test_prepare",
        WhatsappBroadcastTestSend => "whatsapp_broadcast_test_send",
        WhatsappBroadcastPrepare => "whatsapp_broadcast_prepare",
        WhatsappBroadcastSend => "whatsapp_broadcast_send",
        WhatsappBroadcastStop => "whatsapp_broadcast_stop",
        _ => {
            return Err(Error::rejected(
                "screen operation has no implemented executor",
            ))
        }
    })
}

fn sanitized_smtp(mut value: Value) -> Value {
    if let Some(binding) = value.get_mut("binding").and_then(Value::as_object_mut) {
        // Native projection remains unchanged for native clients. A frame
        // receives only sender/status/revisions, not custody discovery rows.
        binding.remove("connection");
        binding.remove("transport");
        binding.remove("install_id");
        binding.remove("context_id");
    }
    value
}

fn bounded(operation: ScreenOperation, args: &Value) -> Result<()> {
    if !args.is_object() || serde_json::to_vec(args)?.len() > operation.args_limit() {
        return Err(Error::rejected(
            "screen operation arguments exceed their byte bound",
        ));
    }
    Ok(())
}

fn response(value: Value) -> Result<Value> {
    if serde_json::to_vec(&value)?.len() > RESULT_CAP {
        return Err(Error::rejected(
            "screen service response exceeds its byte bound",
        ));
    }
    Ok(value)
}

impl Shared {
    fn screen_csv_claim(&self, proof: &ScreenServiceProof) -> Result<Option<CsvInFlight<'_>>> {
        if !matches!(
            proof.operation,
            ScreenOperation::RecordsCsvPreview
                | ScreenOperation::RecordsCsvImport
                | ScreenOperation::RecordsCsvConfirm
        ) {
            return Ok(None);
        }
        let mut live = self
            .screen_service_csv
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if live.len() >= super::app_tools_rpc::TOOL_CTX_GLOBAL
            || !live.insert(proof.action_token.clone())
        {
            return Err(Error::rejected(
                "a CSV operation is already in flight for this mount",
            ));
        }
        Ok(Some(CsvInFlight {
            key: proof.action_token.clone(),
            live: &self.screen_service_csv,
        }))
    }

    fn screen_service_proof(
        &self,
        action_token: &str,
        token: &str,
        key: &str,
        origin: &str,
        operation: ScreenOperation,
    ) -> Result<ScreenServiceProof> {
        let origin = match origin {
            "loopback" => Origin::Loopback,
            "tailnet" => Origin::Tailnet,
            "public" => Origin::Public,
            _ => {
                return Err(screen_access_denied(
                    "screen service requires classified session origin",
                ))
            }
        };
        let (session, view) = self
            .tool_session(token, key, origin)
            .ok_or_else(|| screen_access_denied("screen service requires live operator session"))?;
        let member = origin == Origin::Public
            && !view
                .user
                .as_ref()
                .is_some_and(crate::operator_auth::BoardUser::is_operator);
        let context = self
            .tool_context_proven(action_token, &session)
            .map_err(|error| {
                screen_authority_error(error, "screen mount or session unavailable")
            })?;
        // CAD-1537: a member uses only a read-only mount, and a read-only
        // mount admits only the read allowlist — every write, SLOT
        // (prepare and confirm) and host command is refused here.
        if member && !context.read_only {
            return Err(screen_access_denied(
                "screen service requires operator-role public session",
            ));
        }
        if context.read_only && !operation.is_screen_read() {
            return Err(screen_access_denied(
                "a read-only screen admits only read operations",
            ));
        }
        if context.context_id.is_none() {
            return Err(screen_access_denied(
                "screen service requires active context",
            ));
        }
        Ok(ScreenServiceProof {
            action_token: action_token.to_owned(),
            public_session: (origin == Origin::Public).then(|| token.to_owned()),
            context,
            operation,
            confirmed: None,
            csv_confirmation: None,
            command_args: None,
        })
    }

    /// Native None retains its auth/context/CAS contract, but holds the
    /// release fence through execution too: native edits must not race a
    /// screen's frozen facts and its actual executor. Screen Some adds its
    /// current action-context map entry held through the actual DB operation.
    /// Lock order: PM -> optional custody -> release -> action contexts.
    pub(super) fn screen_service_scope<R>(
        &self,
        install: &str,
        context: &str,
        prove_context: bool,
        proof: Option<&ScreenServiceProof>,
        action: impl FnOnce() -> Result<R>,
    ) -> Result<R> {
        let pm = self.pm_at(&self.pm_dir()?)?;
        if let Some(proof) = proof {
            workspace::with_runtime_snapshot(&pm, install, |bundle, files| {
                let _custody = matches!(
                    proof.operation,
                    ScreenOperation::SenderBindingsSave
                        | ScreenOperation::SendsPrepare
                        | ScreenOperation::SendsShow
                        | ScreenOperation::SendsList
                        | ScreenOperation::SendsApprove
                        | ScreenOperation::SendsResolve
                        | ScreenOperation::SmtpShow
                        | ScreenOperation::SmtpManage
                        | ScreenOperation::SmtpBind
                        | ScreenOperation::SmtpRebind
                        | ScreenOperation::SmtpRevoke
                        | ScreenOperation::SmtpTestSend
                        | ScreenOperation::OriginShow
                        | ScreenOperation::OriginManage
                )
                .then(|| {
                    self.platform_custody_lock
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                });
                let _release = self
                    .app_release_lock
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                let current = self
                    .tool_context_proven(&proof.action_token, &proof.context.session)
                    .map_err(|error| {
                        screen_authority_error(error, "screen mount or session unavailable")
                    })?;
                let mounts = self.tool_contexts.lock().unwrap_or_else(|e| e.into_inner());
                let mounted = mounts
                    .get(&proof.action_token)
                    .ok_or_else(|| screen_access_denied("screen mount retired"))?;
                if current.install_id != install
                    || current.context_id.as_deref() != Some(context)
                    || current.digest != proof.context.digest
                    || current.mount != proof.context.mount
                    || current.generation != proof.context.generation
                    || current.tag != proof.context.tag
                    || mounted.install_id != current.install_id
                    || mounted.mount != current.mount
                    || mounted.generation != current.generation
                    || mounted.session != current.session
                    || mounted.context_id != current.context_id
                    || mounted.digest != current.digest
                    || mounted.tag != current.tag
                {
                    return Err(screen_access_denied(
                        "screen service scope or current mount changed",
                    ));
                }
                let digest = required_str(bundle, "digest").map_err(|error| {
                    screen_authority_error(error, "screen installation digest unavailable")
                })?;
                if digest != current.digest
                    || self.store.app_capability_status(install, digest)?["state"] != "approved"
                {
                    return Err(screen_access_denied(
                        "screen bundle changed or approval revoked",
                    ));
                }
                let pkg = app_screen_pkg::extract(files, &current.tag).map_err(|error| {
                    screen_authority_error(error, "current screen package unavailable")
                })?;
                let manifest =
                    crate::issue::app::parse_manifest(files.get("app.md").ok_or_else(|| {
                        screen_access_denied("screen installation manifest missing")
                    })?)
                    .map_err(|error| {
                        screen_authority_error(error, "screen installation manifest unavailable")
                    })?;
                let permission = if proof.operation.is_slot() {
                    MAY_RECORDS_SLOT
                } else {
                    MAY_RECORDS_CALL
                };
                if pkg.app != manifest.app
                    || pkg.host_contract.as_deref() != Some(HOST_CONTRACT_RECORDS)
                    || !pkg.operations.contains(&proof.operation)
                    || !pkg.may.iter().any(|p| p == permission)
                {
                    return Err(screen_access_denied(
                        "screen operation is not declared by current package",
                    ));
                }
                self.store
                    .app_context_proof(install, context)
                    .map_err(|error| screen_authority_error(error, "screen context unavailable"))?;
                if proof.operation == ScreenOperation::ScreenRoutePatch {
                    let navigation = pkg
                        .navigation
                        .as_ref()
                        .ok_or_else(|| Error::rejected("screen has no navigation declaration"))?;
                    let args = proof
                        .command_args
                        .as_ref()
                        .ok_or_else(|| Error::rejected("missing route command"))?;
                    if let Some(section) = args.get("section").filter(|s| !s.is_null()) {
                        if !navigation
                            .sections
                            .iter()
                            .any(|declared| section.as_str() == Some(declared.id.as_str()))
                        {
                            return Err(Error::rejected(
                                "section is not declared by current screen",
                            ));
                        }
                    }
                }
                if let Some((args, facts)) = &proof.confirmed {
                    if &self.screen_slot_facts_locked(proof, args)? != facts {
                        return Err(Error::rejected("confirmation facts changed; prepare again"));
                    }
                }
                action()
            })
        } else {
            workspace::with_runtime_snapshot(&pm, install, |_, _| {
                let _release = self
                    .app_release_lock
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                if prove_context {
                    self.store.app_context_proof(install, context)?;
                }
                action()
            })
        }
    }

    pub(super) fn screen_service_method(
        proof: Option<&ScreenServiceProof>,
        method: &str,
    ) -> Result<()> {
        if let Some(proof) = proof {
            if native_method(proof.operation)? != method {
                return Err(Error::rejected("screen executor does not match operation"));
            }
        }
        Ok(())
    }

    fn screen_service_execute(
        self: &Arc<Self>,
        proof: &ScreenServiceProof,
        args: Value,
        peer_pid: u32,
    ) -> Result<Value> {
        let mut params = args;
        params["install_id"] = json!(proof.context.install_id);
        params["context_id"] = json!(proof.context.context_id);
        use ScreenOperation::*;
        // Only native attribution consumers receive the verified public bearer;
        // each re-derives the human from its live session, never child fields.
        if matches!(
            proof.operation,
            RecordsCreate | RecordsUpdate | RecordsCsvImport | ContentSave
        ) {
            if let Some(public_session) = &proof.public_session {
                params["public_session"] = json!(public_session);
            }
        }
        if matches!(
            proof.operation,
            ScreenRoutePatch | ScreenPreviewShow | ScreenPreviewClear | SmtpManage
        ) {
            let mut command_proof = proof.clone();
            command_proof.command_args = Some(params.clone());
            return self.screen_service_scope(
                proof.install(),
                proof.context()?,
                true,
                Some(&command_proof),
                || self.screen_host_command_locked(proof, &params),
            );
        }
        if matches!(proof.operation, ChatCampaignOpen | ChatCampaignBrief) {
            return self.screen_service_scope(proof.install(), proof.context()?, true, Some(proof), || {
                self.operator_chat("screen campaign chat", peer_pid)?;
                let campaign = required_str(&params, "campaign_id")?;
                let context = proof.context()?;
                let subject = format!("campaign:{campaign}");
                self.verify_campaign_subject(proof.install(), context, &subject)?;
                let alias = self.resolve_alias("master")?;
                if proof.operation == ChatCampaignOpen {
                    // Page entry selects an existing conversation or a draft
                    // subject. It must not create one until the first send.
                    let conversation = self.store.conversation_list(&alias, proof.install())?
                        .into_iter().find(|row| !row.archived && row.context_id.as_deref() == Some(context) && row.subject.as_deref() == Some(subject.as_str()));
                    return Ok(json!({"host_command": {"operation": proof.operation, "args": {"campaign_id": campaign},
                        "conversation_id": conversation.map(|row| row.id)}}));
                }
                let shown = crate::store::app_records::RecordStore::open(&self.state_dir, proof.install())?.app_content_show(context, campaign)?;
                let name = shown["content"]["name"].as_str().unwrap_or(campaign);
                let text = format!("Draft the email for campaign {campaign} (“{name}”): {}", required_str(&params, "brief")?.trim());
                let (conversation, _) = self.store.conversation_create(&alias, proof.install(), Some(context), crate::store::ConversationKind::Subject(&subject))?;
                // Native thread validation and enqueue execute while the
                // current mount/runtime/release guard remains held. No frame
                // alias, scope, message ID, receipt or turn token is accepted.
                let message = format!("screen-{}", uuid::Uuid::new_v4().simple());
                let mut thread_params = json!({"alias": alias, "text": text,
                    "message": message, "app": {"install_id": proof.install(), "context_id": proof.context()?},
                    "conversation": conversation.id});
                if let Some(public_session) = &proof.public_session {
                    thread_params["public_session"] = json!(public_session);
                }
                let receipt = self.rpc_thread_send(&thread_params, peer_pid)?;
                Ok(json!({"host_command": {"operation": proof.operation, "args": {"campaign_id": campaign}, "conversation_id": conversation.id},
                    "receipt": {"message": receipt["message"], "queued": true}}))
            });
        }
        if matches!(proof.operation, OriginShow | OriginManage) {
            params
                .as_object_mut()
                .expect("typed object")
                .remove("install_id");
            params
                .as_object_mut()
                .expect("typed object")
                .remove("context_id");
        }
        let method = native_method(proof.operation)?;
        match proof.operation {
            RecordsList | RecordsShow | RecordsCreate | RecordsUpdate | RecordsCsvPreview
            | RecordsCsvImport | RecordsCsvConfirm => {
                self.rpc_app_record_with_proof(method, &params, peer_pid, Some(proof))
            }
            SegmentsList | SegmentsShow | SegmentsSave | ExclusionsList | ExclusionsShow
            | ExclusionsSave | SuppressionsList | SuppressionsAdd | AudiencePreview
            | AudiencePrepare | AudienceShow => {
                self.rpc_app_audience_with_proof(method, &params, peer_pid, Some(proof))
            }
            SendsPrepare | SendsShow | SendsList | SendsApprove | SendsResolve | OriginShow
            | OriginManage => self.rpc_crm_send_with_proof(method, &params, peer_pid, Some(proof)),
            SmtpShow | SmtpBind | SmtpRebind | SmtpRevoke | SmtpTestSend => {
                let value = self.rpc_crm_smtp_with_proof(method, &params, peer_pid, Some(proof))?;
                Ok(sanitized_smtp(value))
            }
            op if op.is_whatsapp() => {
                self.rpc_whatsapp_with_proof(method, &params, peer_pid, Some(proof))
            }
            _ => self.rpc_app_content_with_proof(method, &params, peer_pid, Some(proof)),
        }
    }

    pub(super) fn rpc_app_screen_service_call(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        self.operator_connection("app screen service call", params, peer_pid)
            .map_err(|error| {
                screen_authority_error(error, "screen service requires operator connection")
            })?;
        let request: CallRequest = serde_json::from_value(params.clone())
            .map_err(|_| Error::rejected("unsupported screen service request"))?;
        bounded(request.operation, &request.args)?;
        let args = call_args(request.operation, &request.args)?;
        let proof = self.screen_service_proof(
            &request.action_token,
            &request.token,
            &request.key,
            &request.origin,
            request.operation,
        )?;
        let _csv = self.screen_csv_claim(&proof)?;
        response(self.screen_service_execute(&proof, args, peer_pid)?)
    }

    fn screen_slot_facts(&self, proof: &ScreenServiceProof, args: &Value) -> Result<Value> {
        let install = &proof.context.install_id;
        let context = proof
            .context
            .context_id
            .as_deref()
            .ok_or_else(|| Error::rejected("missing screen context"))?;
        self.screen_service_scope(install, context, true, Some(proof), || {
            self.screen_slot_facts_locked(proof, args)
        })
    }

    fn screen_slot_facts_locked(&self, proof: &ScreenServiceProof, args: &Value) -> Result<Value> {
        let install = &proof.context.install_id;
        let context = proof
            .context
            .context_id
            .as_deref()
            .ok_or_else(|| Error::rejected("missing screen context"))?;
        let records = crate::store::app_records::RecordStore::open(&self.state_dir, install)?;
        match proof.operation {
            ScreenOperation::ContentApprove => {
                let shown =
                    records.app_content_show(context, required_str(args, "campaign_id")?)?;
                if shown["content"]["revision"] != args["expected_revision"] {
                    return Err(Error::rejected(
                        "content revision changed before confirmation",
                    ));
                }
                Ok(shown)
            }
            ScreenOperation::RecordsCsvImport | ScreenOperation::RecordsCsvConfirm => {
                let preview = records
                    .app_record_csv_preview(context, &super::app_records_rpc::csv_text(args)?)?;
                if preview["preview_token"] != args["preview_token"] {
                    return Err(Error::rejected("CSV preview changed before confirmation"));
                }
                super::app_records_rpc::csv_decisions(args)?;
                Ok(preview)
            }
            ScreenOperation::SendsApprove | ScreenOperation::SendsResolve => {
                let id = required_str(args, "send_id")?;
                let send = records
                    .app_campaign_send(context, id)?
                    .ok_or_else(|| Error::rejected("send is unavailable in this scope"))?;
                if proof.operation == ScreenOperation::SendsResolve {
                    let customer = required_str(args, "customer_id")?;
                    let row = records
                        .app_campaign_delivery(context, id, customer)?
                        .ok_or_else(|| Error::rejected("delivery is unavailable in this scope"))?;
                    if row.state != "uncertain" {
                        return Err(Error::rejected("only uncertain deliveries can be resolved"));
                    }
                    return Ok(
                        json!({"send_id": id, "customer_id": customer, "state": row.state,
                        "email": crate::store::app_sends::masked_email(&row.email), "send_digest": send.send_digest}),
                    );
                }
                if send.state != "prepared" {
                    return Err(Error::rejected("send is not awaiting approval"));
                }
                let content = records.app_content_approved(context, &send.campaign_id)?;
                Ok(
                    json!({"send_id": id, "state": send.state, "send_digest": send.send_digest,
                    "campaign_id": send.campaign_id, "content_revision": content.0, "content_digest": content.1,
                    "audience": records.app_audience_show(context, &send.audience_freeze_id)?,
                    "binding": self.screen_smtp_facts_locked(proof)?,
                    "unsubscribe_origin": self.crm_send_origin()?}),
                )
            }
            ScreenOperation::SmtpBind
            | ScreenOperation::SmtpRebind
            | ScreenOperation::SmtpRevoke
            | ScreenOperation::SmtpManage => {
                let mut facts = self.screen_smtp_facts_locked(proof)?;
                if matches!(
                    proof.operation,
                    ScreenOperation::SmtpBind | ScreenOperation::SmtpRebind
                ) {
                    let mut choices = Vec::new();
                    for row in self.connection_list_locked()? {
                        if let Some(id) = row["id"].as_str() {
                            if let Ok(rev) = self.crm_sender_revision(id) {
                                if let Ok((_, projection)) = self.crm_smtp_authority(id, rev) {
                                    choices.push(json!({"connection_id": id, "auth_revision": rev, "sender": projection.to_json()}));
                                }
                            }
                        }
                    }
                    facts["connections"] = json!(choices);
                }
                Ok(facts)
            }
            ScreenOperation::SmtpTestSend => Ok(json!({
                "content": records.app_content_show(context, required_str(args, "campaign_id")?)?,
                "binding": self.screen_smtp_facts_locked(proof)?,
            })),
            ScreenOperation::ChatCampaignOpen | ScreenOperation::ChatCampaignBrief => {
                let shown =
                    records.app_content_show(context, required_str(args, "campaign_id")?)?;
                if proof.operation == ScreenOperation::ChatCampaignBrief {
                    let brief = required_str(args, "brief")?;
                    if brief.trim().is_empty() || brief.len() > 12_000 {
                        return Err(Error::rejected("campaign brief exceeds its bound"));
                    }
                    Ok(json!({"content": shown, "brief": brief, "effect": "assistant-message"}))
                } else {
                    Ok(json!({"content": shown}))
                }
            }
            ScreenOperation::WhatsappReplySend => self.whatsapp_reply_facts(install, context, args),
            ScreenOperation::WhatsappTemplateSend => {
                self.whatsapp_template_facts(install, context, args)
            }
            ScreenOperation::WhatsappBroadcastTestSend => {
                self.whatsapp_test_facts(install, context, args)
            }
            ScreenOperation::WhatsappBroadcastSend => {
                self.whatsapp_broadcast_facts(install, context, args)
            }
            ScreenOperation::WhatsappBroadcastStop => self.whatsapp_stop_facts(args),
            ScreenOperation::OriginManage => Ok(json!({"scope": "daemon-global",
                "unsubscribe_origin": self.store.crm_setting("unsubscribe_origin")?.or_else(|| self.unsubscribe_origin.clone())})),
            _ => Err(Error::rejected(
                "screen SLOT has no implemented authoritative card",
            )),
        }
    }

    fn screen_smtp_facts_locked(&self, proof: &ScreenServiceProof) -> Result<Value> {
        let Some(link) = self
            .store
            .crm_smtp_link(proof.install(), proof.context()?)?
        else {
            return Ok(json!({"binding": null}));
        };
        Ok(
            json!({"binding": {"connection_id": link.connection_id, "auth_revision": link.auth_revision,
            "link_revision": link.link_revision, "state": link.state, "digest": link.digest}}),
        )
    }

    fn screen_confirmation_args(
        &self,
        proof: &mut ScreenServiceProof,
        args: &mut Value,
        confirmation: Confirmation,
    ) -> Result<()> {
        use ScreenOperation::*;
        match (proof.operation, confirmation) {
            (
                RecordsCsvConfirm | ContentApprove | SendsApprove | SmtpRevoke | SmtpManage
                | ChatCampaignOpen,
                Confirmation::Tap(tap),
            ) if tap.confirmed => {}
            // CAD-1562: the tap approves the host-computed digest of the
            // prepared reply the card drew; the frame never supplies it.
            (WhatsappReplySend, Confirmation::Tap(tap)) if tap.confirmed => {
                let facts = &proof
                    .confirmed
                    .as_ref()
                    .ok_or_else(|| Error::rejected("confirmation missing"))?
                    .1;
                *args = json!({"effect_id": facts["effect_id"], "digest": facts["digest"]});
            }
            // CAD-1590: each send tap approves the host-drawn card's own
            // effect and digest; the stop tap the card's broadcast.
            (
                WhatsappTemplateSend | WhatsappBroadcastTestSend | WhatsappBroadcastSend,
                Confirmation::Tap(tap),
            ) if tap.confirmed => {
                let facts = &proof
                    .confirmed
                    .as_ref()
                    .ok_or_else(|| Error::rejected("confirmation missing"))?
                    .1;
                *args = json!({"effect_id": facts["effect_id"], "digest": facts["digest"]});
            }
            (WhatsappBroadcastStop, Confirmation::Tap(tap)) if tap.confirmed => {
                let facts = &proof
                    .confirmed
                    .as_ref()
                    .ok_or_else(|| Error::rejected("confirmation missing"))?
                    .1;
                *args = json!({"broadcast_id": facts["broadcast_id"]});
            }
            (ChatCampaignBrief, Confirmation::Brief(input))
                if input.confirmed && args["brief"].as_str() == Some(input.brief.as_str()) => {}
            (RecordsCsvImport, Confirmation::Csv(input)) if input.confirmed => {
                let held = self
                    .screen_service_confirmations
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&input.csv_confirm_token)
                    .ok_or_else(|| Error::rejected("CSV confirmation is spent or unknown"))?;
                let mut normalized = args.clone();
                normalized["decisions"] =
                    args.get("decisions").cloned().unwrap_or_else(|| json!([]));
                if held.issued.elapsed() >= CONFIRM_TTL
                    || held.proof.operation != RecordsCsvConfirm
                    || held.proof.action_token != proof.action_token
                    || held.proof.context.mount != proof.context.mount
                    || held.proof.context.session != proof.context.session
                    || held.args != normalized
                {
                    return Err(Error::rejected(
                        "CSV confirmation does not belong to this mounted plan",
                    ));
                }
                let digest =
                    crate::store::app_records::csv_decisions_digest(&normalized["decisions"])?;
                proof.csv_confirmation = Some((input.csv_confirm_token, digest));
            }
            (SmtpTestSend, Confirmation::Recipient(input)) if input.confirmed => {
                args["to_email"] = json!(input.to_email);
            }
            (SmtpBind | SmtpRebind, Confirmation::Connection(input)) if input.confirmed => {
                let facts = &proof
                    .confirmed
                    .as_ref()
                    .ok_or_else(|| Error::rejected("confirmation missing"))?
                    .1;
                if !facts["connections"].as_array().is_some_and(|choices| {
                    choices
                        .iter()
                        .any(|row| row["connection_id"] == input.connection_id)
                }) {
                    return Err(Error::rejected(
                        "SMTP connection was not an admitted host choice",
                    ));
                }
                args["connection_id"] = json!(input.connection_id);
                if proof.operation == SmtpBind {
                    args["request_id"] = json!(format!("screen-{}", crate::store::app_runs::material_digest(&json!({"mount": proof.context.mount, "connection": args["connection_id"]})).trim_start_matches("sha256:")));
                }
            }
            (SendsResolve, Confirmation::Resolution(input))
                if input.confirmed
                    && matches!(input.resolution.as_str(), "accepted" | "failed") =>
            {
                args["resolution"] = json!(input.resolution);
            }
            (OriginManage, Confirmation::GlobalOrigin(input)) if input.confirmed => {
                args["unsubscribe_origin"] = json!(input.unsubscribe_origin);
            }
            _ => {
                return Err(Error::rejected(
                    "confirmation does not match protected operation or was cancelled",
                ))
            }
        }
        let facts = &proof
            .confirmed
            .as_ref()
            .ok_or_else(|| Error::rejected("confirmation missing"))?
            .1;
        if proof.operation == SendsApprove {
            args["send_digest"] = facts["send_digest"].clone();
        }
        if matches!(proof.operation, SmtpRebind | SmtpRevoke) {
            let revision = facts["binding"]["link_revision"]
                .as_i64()
                .ok_or_else(|| Error::rejected("no current SMTP binding"))?;
            args["expected_revision"] = json!(revision);
        }
        Ok(())
    }

    fn screen_host_command_locked(
        &self,
        proof: &ScreenServiceProof,
        params: &Value,
    ) -> Result<Value> {
        use ScreenOperation::*;
        let mut args = params.clone();
        let fields = args
            .as_object_mut()
            .ok_or_else(|| Error::rejected("host command must be an object"))?;
        fields.remove("install_id");
        fields.remove("context_id");
        match proof.operation {
            ScreenRoutePatch => {
                if let Some(view) = args.get("view").filter(|v| !v.is_null()) {
                    if !matches!(view.as_str(), Some("list" | "new")) {
                        return Err(Error::rejected("unsupported screen view"));
                    }
                }
                for key in ["record_id", "segment_id"] {
                    if let Some(id) = args.get(key).filter(|v| !v.is_null()) {
                        crate::proto::identifier(
                            id.as_str()
                                .ok_or_else(|| Error::rejected("invalid screen selector"))?,
                            "screen selector",
                        )?;
                    }
                }
            }
            ScreenPreviewShow => {
                crate::proto::identifier(required_str(&args, "preview_id")?, "preview ID")?;
                let anchor: PreviewAnchor = serde_json::from_value(args["anchor"].clone())?;
                if let PreviewAnchor::Box(a) = anchor {
                    if [a.x, a.y, a.w, a.h].iter().any(|v| !v.is_finite()) || a.w < 0.0 || a.h < 0.0
                    {
                        return Err(Error::rejected("invalid preview anchor"));
                    }
                }
            }
            ScreenPreviewClear => {
                crate::proto::identifier(required_str(&args, "preview_id")?, "preview ID")?;
            }
            SmtpManage => {}
            _ => return Err(Error::rejected("unsupported host command")),
        }
        Ok(json!({"host_command": {"operation": proof.operation, "args": args}}))
    }

    pub(super) fn rpc_app_screen_service_slot_prepare(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        self.operator_connection("app screen service slot prepare", params, peer_pid)
            .map_err(|error| {
                screen_authority_error(error, "screen service requires operator connection")
            })?;
        let request: CallRequest = serde_json::from_value(params.clone())
            .map_err(|_| Error::rejected("unsupported screen slot request"))?;
        bounded(request.operation, &request.args)?;
        let args = slot_args(request.operation, &request.args)?;
        let proof = self.screen_service_proof(
            &request.action_token,
            &request.token,
            &request.key,
            &request.origin,
            request.operation,
        )?;
        let _csv = self.screen_csv_claim(&proof)?;
        let facts = self.screen_slot_facts(&proof, &args)?;
        let mut confirmations = self
            .screen_service_confirmations
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        confirmations.retain(|_, item| item.issued.elapsed() < CONFIRM_TTL);
        if confirmations.len() >= 256
            || confirmations
                .values()
                .filter(|item| item.proof.action_token == request.action_token)
                .count()
                >= 4
        {
            return Err(Error::rejected("screen confirmation capacity reached"));
        }
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes)
            .map_err(|_| Error::rejected("confirmation entropy unavailable"))?;
        let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        // Only the trusted host receives card facts and token. No frame proof.
        let mut card = json!({"operation": request.operation, "facts": facts});
        // CAD-1590: the stop card also shows live counts, outside the
        // facts the confirmation binds.
        if request.operation == ScreenOperation::WhatsappBroadcastStop {
            card["live"] = self.whatsapp_stop_live(&args)?;
        }
        confirmations.insert(
            token.clone(),
            ScreenServiceConfirmation {
                proof,
                args,
                facts,
                issued: Instant::now(),
            },
        );
        response(json!({"confirmation_token": token, "card": card}))
    }

    pub(super) fn rpc_app_screen_service_slot_confirm(
        self: &Arc<Self>,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        self.operator_connection("app screen service slot confirm", params, peer_pid)
            .map_err(|error| {
                screen_authority_error(error, "screen service requires operator connection")
            })?;
        let request: ConfirmRequest = serde_json::from_value(params.clone())
            .map_err(|_| Error::rejected("unsupported screen confirmation request"))?;
        // Burn BEFORE reproof or mutation. Failure is never automatic retry.
        let confirmed = self
            .screen_service_confirmations
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&request.confirmation_token)
            .ok_or_else(|| Error::rejected("screen confirmation is spent or unknown"))?;
        if confirmed.issued.elapsed() >= CONFIRM_TTL
            || confirmed.proof.action_token != request.action_token
        {
            return Err(Error::rejected(
                "screen confirmation expired, cancelled or foreign",
            ));
        }
        let mut proof = self.screen_service_proof(
            &request.action_token,
            &request.token,
            &request.key,
            &request.origin,
            confirmed.proof.operation,
        )?;
        if proof.context.session != confirmed.proof.context.session
            || proof.context.mount != confirmed.proof.context.mount
        {
            return Err(screen_access_denied(
                "confirmation does not belong to current mount/session",
            ));
        }
        let _csv = self.screen_csv_claim(&proof)?;
        proof.confirmed = Some((confirmed.args.clone(), confirmed.facts));
        let mut args = confirmed.args;
        self.screen_confirmation_args(&mut proof, &mut args, request.confirmation)?;
        if proof.operation == ScreenOperation::RecordsCsvConfirm {
            // Native CSV semantics: absent decisions normalize to [].
            let decisions = args.get("decisions").cloned().unwrap_or_else(|| json!([]));
            args["decisions_digest"] =
                json!(crate::store::app_records::csv_decisions_digest(&decisions)?);
            args["decisions"] = decisions;
        }
        let result = self.screen_service_execute(&proof, args.clone(), peer_pid)?;
        if proof.operation == ScreenOperation::RecordsCsvConfirm {
            let token = required_str(&result, "confirm_token")?;
            // Private bridge: bind the native nonce to this live mount and
            // exact plan. Import consumes this entry and the native reserve
            // transaction spends the nonce. Never forward it to the frame.
            let mut held = self
                .screen_service_confirmations
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            held.retain(|_, item| item.issued.elapsed() < CONFIRM_TTL);
            if held.len() >= 256
                || held
                    .values()
                    .filter(|item| item.proof.action_token == proof.action_token)
                    .count()
                    >= 4
            {
                return Err(Error::rejected("CSV confirmation capacity reached"));
            }
            args.as_object_mut()
                .expect("typed object")
                .remove("decisions_digest");
            let facts = proof.confirmed.as_ref().expect("confirmed facts").1.clone();
            held.insert(
                token.to_owned(),
                ScreenServiceConfirmation {
                    proof,
                    args,
                    facts,
                    issued: Instant::now(),
                },
            );
        }
        response(result)
    }
}
