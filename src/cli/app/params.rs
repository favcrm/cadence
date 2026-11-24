//! Bounded payload readers and RPC parameter builders for `cadence app`.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::io::Read;
use std::path::Path;

use cadence_agent::error::{Error, Result};
use serde_json::json;

use super::{ContentAction, WorksAction};

mod audience;
mod record;

pub(super) use audience::{audience_params, binding_params, effect_params};
pub(super) use record::record_params;

/// CAD-1662: map each native Works verb to its `app_works_*` RPC. JSON
/// files are bounded by `read_audience_json` (64 KiB) and forwarded as-is;
/// the daemon's typed grammar validates every field. No actor is sent.
pub(super) fn works_params(action: &WorksAction) -> Result<(&'static str, serde_json::Value)> {
    Ok(match action {
        WorksAction::Query {
            install_id,
            context_id,
            text,
            kind,
            review,
            export,
            tag,
            owner,
            sort,
        } => (
            "app_works_query",
            json!({"install_id": install_id, "context_id": context_id, "text": text, "kind": kind, "review": review, "export": export, "tag": tag, "owner": owner, "sort": sort}),
        ),
        WorksAction::Get {
            install_id,
            context_id,
            id,
        } => (
            "app_works_get",
            json!({"install_id": install_id, "context_id": context_id, "id": id}),
        ),
        WorksAction::Create {
            install_id,
            context_id,
            body,
            idempotency_key,
        } => {
            let mut params = read_works_json(body, "works body")?;
            let obj = params
                .as_object_mut()
                .ok_or_else(|| Error::invalid("app_works", "works body must be an object"))?;
            obj.insert("install_id".into(), json!(install_id));
            obj.insert("context_id".into(), json!(context_id));
            obj.insert("idempotency_key".into(), json!(idempotency_key));
            ("app_works_create", params)
        }
        WorksAction::Patch {
            install_id,
            context_id,
            id,
            if_revision,
            body,
            idempotency_key,
        } => {
            let mut params = read_works_json(body, "works patch")?;
            let obj = params
                .as_object_mut()
                .ok_or_else(|| Error::invalid("app_works", "works patch must be an object"))?;
            obj.insert("install_id".into(), json!(install_id));
            obj.insert("context_id".into(), json!(context_id));
            obj.insert("id".into(), json!(id));
            obj.insert("if_revision".into(), json!(if_revision));
            obj.insert("idempotency_key".into(), json!(idempotency_key));
            ("app_works_patch", params)
        }
        WorksAction::Trash {
            install_id,
            context_id,
            id,
            if_revision,
            idempotency_key,
        } => (
            "app_works_trash",
            json!({"install_id": install_id, "context_id": context_id, "id": id, "if_revision": if_revision, "idempotency_key": idempotency_key}),
        ),
        WorksAction::Restore {
            install_id,
            context_id,
            id,
            if_revision,
            idempotency_key,
        } => (
            "app_works_restore",
            json!({"install_id": install_id, "context_id": context_id, "id": id, "if_revision": if_revision, "idempotency_key": idempotency_key}),
        ),
        WorksAction::RequestReview {
            install_id,
            context_id,
            id,
            revision,
        } => (
            "app_works_request_review",
            json!({"install_id": install_id, "context_id": context_id, "id": id, "revision": revision}),
        ),
        WorksAction::Review {
            install_id,
            context_id,
            id,
            revision,
            decision,
            note,
        } => (
            "app_works_review",
            json!({"install_id": install_id, "context_id": context_id, "id": id, "revision": revision, "decision": decision, "note": note}),
        ),
        WorksAction::Export {
            install_id,
            context_id,
            id,
            revision,
            format,
        } => {
            let mut params = json!({"install_id": install_id, "context_id": context_id, "id": id, "revision": revision});
            // Omit `format` when absent so the daemon applies its html default
            // instead of receiving an explicit null (refused as invalid).
            if let Some(format) = format {
                params["format"] = json!(format);
            }
            ("app_works_export", params)
        }
    })
}

/// Works create/patch bodies: a near-64 KiB revision plus kind, title,
/// tags or operations exceeds the 64 KiB audience bound, so Works verbs get
/// their own 80 KiB reader. Other commands keep `read_audience_json`.
fn read_works_json(path: &Path, kind: &str) -> Result<serde_json::Value> {
    read_bounded_json(path, kind, 80 * 1024)
}

fn read_audience_json(path: &Path, kind: &str) -> Result<serde_json::Value> {
    read_bounded_json(path, kind, 64 * 1024)
}

fn read_bounded_json(path: &Path, kind: &str, limit: usize) -> Result<serde_json::Value> {
    let file = std::fs::File::open(path)
        .map_err(|e| Error::invalid("app_audience", format!("cannot open {kind}: {e}")))?;
    let mut bytes = Vec::new();
    file.take((limit + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| Error::invalid("app_audience", format!("cannot read {kind}: {e}")))?;
    if bytes.len() > limit {
        return Err(Error::invalid(
            "app_audience",
            format!("{kind} JSON exceeds {}KiB", limit / 1024),
        ));
    }
    serde_json::from_slice(&bytes)
        .map_err(|_| Error::invalid("app_audience", format!("{kind} must be JSON")))
}

pub(super) fn content_params(action: &ContentAction) -> Result<(&'static str, serde_json::Value)> {
    Ok(match action {
        ContentAction::Draft {
            install_id,
            context_id,
            campaign_id,
            proposal_id,
            draft,
            message,
            token,
        } => {
            // The draft JSON file holds {subject, preheader, blocks};
            // the daemon's content grammar validates every field. The
            // message+token are the live scoped turn's identity.
            let body = read_audience_json(draft, "email draft")?;
            let obj = body
                .as_object()
                .ok_or_else(|| Error::invalid("app_content", "email draft must be an object"))?;
            let mut params = json!({
                "install_id": install_id,
                "context_id": context_id,
                "campaign_id": campaign_id,
                "proposal_id": proposal_id,
                "message": message,
                "token": token,
            });
            for key in ["subject", "preheader", "blocks"] {
                if let Some(v) = obj.get(key) {
                    params[key] = v.clone();
                }
            }
            ("app_content_assistant_draft", params)
        }
        ContentAction::Proposals {
            install_id,
            context_id,
            campaign_id,
            message,
            token,
        } => {
            let mut params = json!({"install_id":install_id,"context_id":context_id,"message":message,"token":token});
            if let Some(campaign) = campaign_id {
                params["campaign_id"] = json!(campaign);
            }
            ("app_content_assistant_proposals", params)
        }
        ContentAction::ProposalShow {
            install_id,
            context_id,
            proposal_id,
            message,
            token,
        } => (
            "app_content_assistant_proposal_show",
            json!({"install_id":install_id,"context_id":context_id,"proposal_id":proposal_id,"message":message,"token":token}),
        ),
        ContentAction::AutonomySet { mode } => ("crm_autonomy_set", json!({"mode":mode})),
        ContentAction::AutonomyShow => ("crm_autonomy_show", json!({})),
    })
}
