//! Source ingestion beside the wiki's canonical blob pointer.
//!
//! A PDF remains the raw, content-addressed source. Its extracted UTF-8 text
//! becomes a versioned `.extracted.md` page in the same wiki folder. The page
//! carries the source hash so an update never silently preserves old text.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use serde_json::{json, Value};

use super::{blobs_dir, ls, read, rm, vault_dir, write, Caller, ABSENT};
use crate::error::Result;
use crate::issue::Pm;

const EXTRACT_BYTES: usize = 16 * 1024 * 1024;

fn pdf_text(source: &Path) -> std::result::Result<String, String> {
    let (output, bounds) = crate::proc::run_bounded_limited(
        Command::new("pdftotext")
            .args(["-enc", "UTF-8", "-nopgbrk"])
            .arg(source)
            .arg("-"),
        Duration::from_secs(20),
        EXTRACT_BYTES,
    )
    .map_err(|e| format!("pdftotext unavailable or timed out: {e}"))?;
    if bounds.stdout_exceeded {
        return Err(format!("extracted text exceeds {EXTRACT_BYTES} bytes"));
    }
    if !output.status.success() {
        return Err("pdftotext could not extract this PDF".into());
    }
    let text = String::from_utf8(output.stdout).map_err(|_| "extracted text is not UTF-8")?;
    Ok(text.trim().to_string())
}

/// Add or replace the generated source text page after the raw PDF blob has
/// committed. Extraction failure is recorded as a source page with no old text.
/// A manually authored page at the reserved name is never overwritten.
pub fn pdf(pm: &Pm, caller: &Caller, path: &str, sha256: &str) -> Result<Value> {
    let sidecar = format!("{path}.extracted.md");
    let quoted_path = serde_json::to_string(path)?;
    let current = read(pm, caller, &sidecar).ok();
    let marker = format!("source_path: {quoted_path}\n");
    if let Some(page) = &current {
        let old = page["text"].as_str().unwrap_or("");
        if !old.starts_with("---\nkind: extracted-source\n") || !old.contains(&marker) {
            return Ok(json!({"status":"conflict","path":sidecar,
                             "reason":"a page at the extraction path was not generated from this source"}));
        }
    }
    let blob = blobs_dir(&vault_dir(pm)?).join(sha256);
    let (status, body, reason) = match pdf_text(&blob) {
        Ok(text) if text.is_empty() => (
            "empty",
            String::new(),
            Some("PDF has no extractable text; OCR may be needed".to_string()),
        ),
        Ok(text) => ("indexed", text, None),
        Err(why) => ("failed", String::new(), Some(why)),
    };
    let name = path.rsplit('/').next().unwrap_or(path);
    let page = format!(
        "---\nkind: extracted-source\nsource_path: {quoted_path}\nsource_sha256: {sha256}\nextraction_status: {status}\n---\n\n# Source text: {name}\n\n{body}\n"
    );
    let if_rev = current
        .as_ref()
        .and_then(|v| v["rev"].as_str())
        .unwrap_or(ABSENT);
    let saved = write(pm, caller, &sidecar, &page, Some(if_rev))?;
    if saved.get("conflict").is_some() {
        return Ok(json!({"status":"conflict","path":sidecar,
                         "reason":"the extraction page changed during the upload"}));
    }
    let mut out = json!({"status":status,"path":sidecar,"source_sha256":sha256});
    if let Some(reason) = reason {
        out["reason"] = json!(reason);
    }
    Ok(out)
}

/// The blob is already durable when extraction runs. Preserve that outcome
/// even if a converter or generated-page write fails, and make the failure
/// visible to the caller rather than reporting a successful index.
pub fn pdf_result(pm: &Pm, caller: &Caller, path: &str, sha256: &str) -> Value {
    pdf(pm, caller, path, sha256).unwrap_or_else(
        |e| json!({"status":"failed","path":format!("{path}.extracted.md"),"reason":e.to_string()}),
    )
}

/// Clean up a generated page after its raw PDF is moved or removed. A page
/// authored at the same path by a person is left alone.
pub fn remove_generated(pm: &Pm, caller: &Caller, source: &str) -> Result<()> {
    let sidecar = format!("{source}.extracted.md");
    if let Ok(page) = read(pm, caller, &sidecar) {
        let marker = format!("source_path: {}\n", serde_json::to_string(source)?);
        if page["text"].as_str().is_some_and(|text| {
            text.starts_with("---\nkind: extracted-source\n") && text.contains(&marker)
        }) {
            rm(pm, caller, &sidecar)?;
        }
    }
    Ok(())
}

/// A directory move carries generated pages and blob pointers together. Fix
/// each moved page's source path while preserving its extracted body and hash.
/// A conflicting edit is reported; the index excludes its stale path until it
/// is resolved, so agents never follow a false provenance pointer.
pub fn relocate_tree(pm: &Pm, caller: &Caller, from: &str, to: &str) -> Result<Value> {
    let mut stack = vec![to.to_string()];
    let mut updated = 0usize;
    let mut conflicts = Vec::new();
    while let Some(dir) = stack.pop() {
        let listing = ls(pm, caller, &dir)?;
        let Some(entries) = listing["entries"].as_array() else {
            continue;
        };
        for entry in entries {
            let Some(path) = entry["path"].as_str() else {
                continue;
            };
            if entry["kind"] == "dir" {
                stack.push(path.to_string());
                continue;
            }
            let Some(source) = path.strip_suffix(".extracted.md") else {
                continue;
            };
            let Some(suffix) = source.strip_prefix(to).filter(|s| s.starts_with('/')) else {
                continue;
            };
            let old_source = format!("{from}{suffix}");
            let Some(page) = read(pm, caller, path).ok() else {
                continue;
            };
            let Some(text) = page["text"].as_str() else {
                continue;
            };
            let old_prefix = format!(
                "---\nkind: extracted-source\nsource_path: {}\n",
                serde_json::to_string(&old_source)?
            );
            if !text.starts_with(&old_prefix) {
                continue;
            }
            let new_prefix = format!(
                "---\nkind: extracted-source\nsource_path: {}\n",
                serde_json::to_string(source)?
            );
            let revised = text.replacen(&old_prefix, &new_prefix, 1);
            let saved = write(pm, caller, path, &revised, page["rev"].as_str())?;
            if saved.get("conflict").is_some() {
                conflicts.push(path.to_string());
            } else {
                updated += 1;
            }
        }
    }
    Ok(
        json!({"status":if conflicts.is_empty() {"indexed"} else {"conflict"},
              "updated":updated,"conflicts":conflicts}),
    )
}
