//! Audience, binding, and effect RPC parameter builders for `cadence app`.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use cadence_agent::error::Result;
use serde_json::json;

use super::super::{AudienceAction, BindingAction, EffectAction};

pub(in super::super) fn audience_params(
    action: &AudienceAction,
) -> Result<(&'static str, serde_json::Value)> {
    Ok(match action {
        AudienceAction::SegmentSave {
            install_id,
            context_id,
            segment_id,
            name,
            predicates,
            expected_revision,
        } => {
            let mut params = json!({"install_id": install_id, "context_id": context_id, "segment_id": segment_id, "name": name, "predicates": super::read_audience_json(predicates, "predicates")?});
            if let Some(revision) = expected_revision {
                params["expected_revision"] = json!(revision);
            }
            ("app_segment_save", params)
        }
        AudienceAction::SegmentAssistantSave {
            install_id,
            context_id,
            segment_id,
            name,
            predicates,
            expected_revision,
            message,
            token,
        } => {
            let mut params = json!({"install_id": install_id, "context_id": context_id, "segment_id": segment_id, "name": name, "predicates": super::read_audience_json(predicates, "predicates")?, "message": message, "token": token});
            if let Some(revision) = expected_revision {
                params["expected_revision"] = json!(revision);
            }
            ("app_segment_assistant_save", params)
        }
        AudienceAction::SegmentAssistantLs {
            install_id,
            context_id,
            message,
            token,
        } => (
            "app_segment_assistant_list",
            json!({"install_id": install_id, "context_id": context_id, "message": message, "token": token}),
        ),
        AudienceAction::SegmentAssistantPreview {
            install_id,
            context_id,
            segment_id,
            message,
            token,
        } => (
            "app_segment_assistant_preview",
            json!({"install_id":install_id,"context_id":context_id,"segment_id":segment_id,"message":message,"token":token}),
        ),
        AudienceAction::SegmentAssistantShow {
            install_id,
            context_id,
            segment_id,
            message,
            token,
        } => (
            "app_segment_assistant_show",
            json!({"install_id": install_id, "context_id": context_id, "segment_id": segment_id, "message": message, "token": token}),
        ),
        AudienceAction::SegmentShow {
            install_id,
            context_id,
            segment_id,
        } => (
            "app_segment_show",
            json!({"install_id": install_id, "context_id": context_id, "segment_id": segment_id}),
        ),
        AudienceAction::SegmentLs {
            install_id,
            context_id,
        } => (
            "app_segment_list",
            json!({"install_id": install_id, "context_id": context_id}),
        ),
        AudienceAction::ExclusionSave {
            install_id,
            context_id,
            list_id,
            name,
            members,
            expected_revision,
        } => {
            let mut params = json!({"install_id": install_id, "context_id": context_id, "list_id": list_id, "name": name, "member_ids": super::read_audience_json(members, "members")?});
            if let Some(revision) = expected_revision {
                params["expected_revision"] = json!(revision);
            }
            ("app_exclusion_save", params)
        }
        AudienceAction::ExclusionShow {
            install_id,
            context_id,
            list_id,
        } => (
            "app_exclusion_show",
            json!({"install_id": install_id, "context_id": context_id, "list_id": list_id}),
        ),
        AudienceAction::ExclusionLs {
            install_id,
            context_id,
        } => (
            "app_exclusion_list",
            json!({"install_id": install_id, "context_id": context_id}),
        ),
        AudienceAction::SuppressionAdd {
            install_id,
            context_id,
            email,
            customer_id,
            reason,
        } => {
            let mut params =
                json!({"install_id": install_id, "context_id": context_id, "reason": reason});
            if let Some(address) = email {
                params["email"] = json!(address);
            }
            if let Some(id) = customer_id {
                params["customer_id"] = json!(id);
            }
            ("app_suppression_add", params)
        }
        AudienceAction::SuppressionRemove {
            install_id,
            context_id,
            email,
            customer_id,
        } => {
            let mut params = json!({"install_id": install_id, "context_id": context_id});
            if let Some(address) = email {
                params["email"] = json!(address);
            }
            if let Some(id) = customer_id {
                params["customer_id"] = json!(id);
            }
            ("app_suppression_remove", params)
        }
        AudienceAction::SuppressionLs {
            install_id,
            context_id,
        } => (
            "app_suppression_list",
            json!({"install_id": install_id, "context_id": context_id}),
        ),
        AudienceAction::Preview {
            install_id,
            context_id,
            base,
            exclusion_list_id,
        } => {
            let mut params = json!({"install_id": install_id, "context_id": context_id, "base": super::read_audience_json(base, "base")?});
            if let Some(list) = exclusion_list_id {
                params["exclusion_list_id"] = json!(list);
            }
            ("app_audience_preview", params)
        }
        AudienceAction::Prepare {
            install_id,
            context_id,
            freeze_id,
            base,
            exclusion_list_id,
            max_recipients,
        } => {
            let mut params = json!({"install_id": install_id, "context_id": context_id, "freeze_id": freeze_id, "base": super::read_audience_json(base, "base")?, "max_recipients": max_recipients});
            if let Some(list) = exclusion_list_id {
                params["exclusion_list_id"] = json!(list);
            }
            ("app_audience_prepare", params)
        }
        AudienceAction::FreezeShow {
            install_id,
            context_id,
            freeze_id,
        } => (
            "app_audience_show",
            json!({"install_id": install_id, "context_id": context_id, "freeze_id": freeze_id}),
        ),
    })
}

fn release_scope(install: Option<&str>, context: Option<&str>) -> serde_json::Value {
    let mut params = json!({});
    if let Some(id) = install {
        params["install_id"] = json!(id);
    }
    if let Some(id) = context {
        params["context_id"] = json!(id);
    }
    params
}

pub(in super::super) fn binding_params(
    action: &BindingAction,
) -> (&'static str, serde_json::Value) {
    match action {
        BindingAction::Create {
            install_id,
            context_id,
            slot,
            connection_id,
            request_id,
        } => {
            let mut params = release_scope(Some(install_id), context_id.as_deref());
            params["slot"] = json!(slot);
            params["connection_id"] = json!(connection_id);
            params["request_id"] = json!(request_id);
            ("app_binding_create", params)
        }
        BindingAction::Ls {
            install_id,
            context_id,
        } => (
            "app_binding_list",
            release_scope(Some(install_id), context_id.as_deref()),
        ),
        BindingAction::Show {
            install_id,
            binding_id,
        } => (
            "app_binding_show",
            json!({"install_id":install_id,"binding_id":binding_id}),
        ),
        BindingAction::Set {
            install_id,
            binding_id,
            expected_revision,
            connection_id,
        } => (
            "app_binding_update",
            json!({"install_id":install_id,"binding_id":binding_id,"expected_revision":expected_revision,"connection_id":connection_id}),
        ),
        BindingAction::Publish {
            install_id,
            binding_id,
            expected_revision,
            destination_id,
            destination_label,
            toolkit,
            timezone,
            grant_id,
        } => (
            "app_binding_publish_set",
            json!({"install_id":install_id,"binding_id":binding_id,"expected_revision":expected_revision,
            "destination_id":destination_id,"destination_label":destination_label,"toolkit":toolkit,
            "timezone":timezone,"grant_id":grant_id}),
        ),
        BindingAction::Revoke {
            install_id,
            binding_id,
            expected_revision,
        } => (
            "app_binding_revoke",
            json!({"install_id":install_id,"binding_id":binding_id,"expected_revision":expected_revision}),
        ),
    }
}

pub(in super::super) fn effect_params(action: &EffectAction) -> (&'static str, serde_json::Value) {
    match action {
        EffectAction::Resolve {
            effect_id,
            digest,
            resolution,
        } => (
            "app_effect_resolve",
            json!({"effect_id":effect_id,"digest":digest,"resolution":resolution.as_str()}),
        ),
        EffectAction::Stage {
            run_id,
            artifact_id,
            slot,
            request_id,
            title,
        } => (
            "app_effect_stage",
            json!({"run_id":run_id,"artifact_id":artifact_id,"slot":slot,"request_id":request_id,"title":title}),
        ),
        EffectAction::Show { effect_id } => ("app_effect_show", json!({"effect_id":effect_id})),
        EffectAction::Ls {
            install_id,
            context_id,
        } => (
            "app_effect_list",
            release_scope(install_id.as_deref(), context_id.as_deref()),
        ),
        EffectAction::Accept { effect_id, digest } => (
            "app_effect_decide",
            json!({"effect_id":effect_id,"digest":digest,"decision":"accept"}),
        ),
        EffectAction::Decline { effect_id, digest } => (
            "app_effect_decide",
            json!({"effect_id":effect_id,"digest":digest,"decision":"decline"}),
        ),
    }
}
