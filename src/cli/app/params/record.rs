//! Record RPC parameter builders and bounded payload readers for `cadence app`.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::io::Read;
use std::path::Path;

use cadence_agent::error::{Error, Result};
use serde_json::{json, Value};

use super::super::RecordAction;

fn read_record_csv(path: &Path) -> Result<String> {
    const MAX_CSV_BYTES: usize = 256 * 1024;
    let file = std::fs::File::open(path)
        .map_err(|e| Error::invalid("app_record_csv", format!("cannot open CSV: {e}")))?;
    let mut bytes = Vec::new();
    file.take((MAX_CSV_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| Error::invalid("app_record_csv", format!("cannot read CSV: {e}")))?;
    if bytes.len() > MAX_CSV_BYTES {
        return Err(Error::invalid("app_record_csv", "CSV exceeds 256KiB"));
    }
    String::from_utf8(bytes).map_err(|_| Error::invalid("app_record_csv", "CSV must be UTF-8 text"))
}

fn read_csv_decisions(path: &Path) -> Result<serde_json::Value> {
    const MAX_DECISIONS_BYTES: usize = 64 * 1024;
    let file = std::fs::File::open(path)
        .map_err(|e| Error::invalid("app_record_csv", format!("cannot open decisions: {e}")))?;
    let mut bytes = Vec::new();
    file.take((MAX_DECISIONS_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| Error::invalid("app_record_csv", format!("cannot read decisions: {e}")))?;
    if bytes.len() > MAX_DECISIONS_BYTES {
        return Err(Error::invalid(
            "app_record_csv",
            "decisions JSON exceeds 64KiB",
        ));
    }
    let decisions: Value = serde_json::from_slice(&bytes)
        .map_err(|_| Error::invalid("app_record_csv", "decisions must be a JSON array"))?;
    if !decisions.is_array() {
        return Err(Error::invalid(
            "app_record_csv",
            "decisions must be a JSON array",
        ));
    }
    Ok(decisions)
}

fn read_record_profile(path: &Path) -> Result<serde_json::Value> {
    const MAX_PROFILE_BYTES: usize = 16 * 1024;
    let file = std::fs::File::open(path)
        .map_err(|e| Error::invalid("app_record_profile", format!("cannot open profile: {e}")))?;
    let mut bytes = Vec::new();
    file.take((MAX_PROFILE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| Error::invalid("app_record_profile", format!("cannot read profile: {e}")))?;
    if bytes.len() > MAX_PROFILE_BYTES {
        return Err(Error::invalid(
            "app_record_profile",
            "profile JSON exceeds 16KiB",
        ));
    }
    let profile: Value = serde_json::from_slice(&bytes)
        .map_err(|_| Error::invalid("app_record_profile", "profile must be a JSON object"))?;
    if !profile.is_object() {
        return Err(Error::invalid(
            "app_record_profile",
            "profile must be a JSON object",
        ));
    }
    Ok(profile)
}

pub(in super::super) fn record_params(
    action: &RecordAction,
) -> Result<(&'static str, serde_json::Value)> {
    Ok(match action {
        RecordAction::Create {
            install_id,
            context_id,
            record_id,
            profile,
        } => (
            "app_record_create",
            json!({"install_id": install_id, "context_id": context_id, "record_id": record_id, "profile": read_record_profile(profile)?}),
        ),
        RecordAction::Ls {
            install_id,
            context_id,
        } => (
            "app_record_list",
            json!({"install_id": install_id, "context_id": context_id}),
        ),
        RecordAction::Show {
            install_id,
            context_id,
            record_id,
        } => (
            "app_record_show",
            json!({"install_id": install_id, "context_id": context_id, "record_id": record_id}),
        ),
        RecordAction::Set {
            install_id,
            context_id,
            record_id,
            expected_revision,
            profile,
            consent_method,
            consent_note,
        } => {
            let mut params = json!({"install_id": install_id, "context_id": context_id, "record_id": record_id, "expected_revision": expected_revision, "profile": read_record_profile(profile)?});
            if let Some(method) = consent_method {
                params["consent_provenance"] = json!({"method": method, "note": consent_note});
            }
            ("app_record_update", params)
        }
        RecordAction::CsvPreview {
            install_id,
            context_id,
            csv,
        } => (
            "app_record_csv_preview",
            json!({"install_id": install_id, "context_id": context_id, "csv_text": read_record_csv(csv)?}),
        ),
        RecordAction::CsvImport {
            install_id,
            context_id,
            csv,
            preview_token,
            request_id,
            decisions,
        } => {
            let mut params = json!({"install_id": install_id, "context_id": context_id, "csv_text": read_record_csv(csv)?, "preview_token": preview_token, "request_id": request_id});
            if let Some(path) = decisions {
                params["decisions"] = read_csv_decisions(path)?;
            }
            ("app_record_csv_import", params)
        }
        RecordAction::CsvAssistantImport {
            install_id,
            context_id,
            request_id,
            confirm_token,
            message,
            token,
        } => {
            // Handle-only: the daemon resolves the durable plan
            // (csv_text + decisions + their digests) from request_id +
            // confirm_token. No csv_text/preview_token/decisions params.
            (
                "app_record_csv_assistant_import",
                json!({"install_id": install_id, "context_id": context_id, "request_id": request_id, "confirm_token": confirm_token, "message": message, "token": token}),
            )
        }
        RecordAction::CsvConfirm {
            install_id,
            context_id,
            csv,
            preview_token,
            request_id,
            decisions,
        } => {
            // The confirm stores the durable plan: the exact csv_text +
            // normalized decisions the daemon re-verifies against the
            // preview_token + decisions_digest before minting. The agent
            // later resolves the plan by request id + nonce.
            let csv_text = read_record_csv(csv)?;
            let decisions_value = match decisions {
                Some(path) => read_csv_decisions(path)?,
                None => json!([]),
            };
            let decisions_digest =
                cadence_agent::store::app_records::csv_decisions_digest(&decisions_value)?;
            (
                "app_record_csv_confirm",
                json!({"install_id": install_id, "context_id": context_id, "preview_token": preview_token, "request_id": request_id, "csv_text": csv_text, "decisions": decisions_value, "decisions_digest": decisions_digest}),
            )
        }
        RecordAction::CsvAssistantPreview {
            install_id,
            context_id,
            csv,
            message,
            token,
        } => (
            "app_record_csv_assistant_preview",
            json!({"install_id": install_id, "context_id": context_id, "csv_text": read_record_csv(csv)?, "message": message, "token": token}),
        ),
    })
}
