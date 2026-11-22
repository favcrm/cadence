//! Sidecar parsing and encoding for wiki collection metadata.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::fs;

use serde_json::{json, Value};

use super::super::revision::{self, Pinned};
use super::super::{allowed, normalize, vault_dir, Caller, Op};
use super::{sidecar_path, under_root, Doc, Meta, DOC_CAP, LABEL_CAP, SIDECAR_CAP};
use crate::error::{Error, Result};
use crate::issue::Pm;

pub(super) fn parse_client(
    pm: &Pm,
    caller: &Caller,
    root: &str,
    metadata: &Value,
    provenance: &str,
) -> Result<Meta> {
    let obj = metadata
        .as_object()
        .ok_or_else(|| Error::rejected("wiki collection refused: metadata must be an object"))?;
    for key in obj.keys() {
        if !matches!(
            key.as_str(),
            "publisher" | "reviewer" | "review_project" | "documents"
        ) {
            return Err(Error::rejected(format!(
                "wiki collection refused: metadata field '{key}' is not accepted"
            )));
        }
    }
    let review_project = optional_label(obj.get("review_project"), "review_project")?;
    if let Some(project) = &review_project {
        let projects = crate::issue::project::list(&pm.dir)?;
        if !projects.iter().any(|item| item.key == *project) {
            return Err(Error::rejected(format!(
                "wiki collection refused: review project '{project}' does not exist"
            )));
        }
    }
    let rows = obj
        .get("documents")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::rejected("wiki collection refused: documents must be an array"))?;
    if rows.len() > DOC_CAP {
        return Err(Error::rejected(
            "wiki collection refused: a collection lists at most 256 documents",
        ));
    }
    let mut documents = Vec::new();
    for row in rows {
        documents.push(parse_doc(pm, caller, root, row, provenance)?);
    }
    let mut seen = Vec::new();
    for doc in &documents {
        if seen.iter().any(|path| path == &doc.path) {
            return Err(Error::rejected(format!(
                "wiki collection refused: '{}' is listed twice",
                doc.path
            )));
        }
        seen.push(doc.path.clone());
    }
    Ok(Meta {
        publisher: optional_label(obj.get("publisher"), "publisher")?,
        reviewer: optional_label(obj.get("reviewer"), "reviewer")?,
        review_project,
        documents,
    })
}

fn parse_doc(pm: &Pm, caller: &Caller, root: &str, row: &Value, provenance: &str) -> Result<Doc> {
    let obj = row
        .as_object()
        .ok_or_else(|| Error::rejected("wiki collection refused: a document must be an object"))?;
    for key in obj.keys() {
        if !matches!(
            key.as_str(),
            "path"
                | "source_type"
                | "version"
                | "applicability"
                | "supersedes"
                | "approved_revision"
                | "approved_sha256"
        ) {
            return Err(Error::rejected(format!(
                "wiki collection refused: document field '{key}' is not accepted"
            )));
        }
    }
    let path = obj
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::rejected("wiki collection refused: a document needs a path"))?;
    let path = normalize(path)?;
    if !under_root(root, &path) || path == root {
        return Err(Error::rejected(format!(
            "wiki collection refused: '{path}' is outside this collection"
        )));
    }
    let segs: Vec<&str> = path.split('/').collect();
    allowed(pm, caller, Op::Read, &segs)?;
    let supersedes = match optional_label(obj.get("supersedes"), "supersedes")? {
        Some(other) => {
            let other = normalize(&other)?;
            if !under_root(root, &other) {
                return Err(Error::rejected(
                    "wiki collection refused: supersedes must stay inside the collection",
                ));
            }
            Some(other)
        }
        None => None,
    };
    let approved = optional_label(obj.get("approved_revision"), "approved_revision")?;
    let claimed = optional_label(obj.get("approved_sha256"), "approved_sha256")?;
    let (approved_git, approved_sha256, approved_by) = if let Some(selector) = approved {
        let pinned: Pinned = revision::pin(pm, caller, &path, &selector)?;
        if pinned.sha256.is_empty() {
            return Err(Error::rejected(
                "wiki collection refused: a directory cannot be an approved source",
            ));
        }
        if claimed
            .as_ref()
            .is_some_and(|claimed| claimed != &pinned.sha256)
        {
            return Err(Error::rejected(
                "wiki collection refused: approved_sha256 does not match that revision's source bytes",
            ));
        }
        (
            Some(pinned.git),
            Some(pinned.sha256),
            Some(provenance.to_string()),
        )
    } else {
        if claimed.is_some() {
            return Err(Error::rejected(
                "wiki collection refused: a digest is not an approval without a resolved revision",
            ));
        }
        (None, None, None)
    };
    Ok(Doc {
        path,
        source_type: optional_label(obj.get("source_type"), "source_type")?,
        version: optional_label(obj.get("version"), "version")?,
        applicability: optional_label(obj.get("applicability"), "applicability")?,
        supersedes,
        approved_git,
        approved_sha256,
        approved_by,
    })
}

fn optional_label(value: Option<&Value>, field: &str) -> Result<Option<String>> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => {
            let text = text.trim();
            if text.is_empty() {
                return Ok(None);
            }
            if text.len() > LABEL_CAP || text.chars().any(char::is_control) {
                return Err(Error::rejected(format!(
                    "wiki collection refused: {field} must be at most {LABEL_CAP} non-control characters"
                )));
            }
            Ok(Some(text.to_string()))
        }
        Some(_) => Err(Error::rejected(format!(
            "wiki collection refused: {field} must be a string or null"
        ))),
    }
}

pub(super) fn load(pm: &Pm, root: &str) -> Result<(Meta, Option<String>)> {
    let abs = sidecar_path(&vault_dir(pm)?, root)?;
    let bytes = match fs::read(&abs) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok((
                Meta {
                    publisher: None,
                    reviewer: None,
                    review_project: None,
                    documents: Vec::new(),
                },
                None,
            ))
        }
        Err(e) => return Err(e.into()),
    };
    if bytes.len() > SIDECAR_CAP {
        return Err(Error::rejected(
            "wiki collection refused: metadata sidecar is over the 256KiB bound",
        ));
    }
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|_| Error::rejected("wiki collection refused: metadata sidecar is not JSON"))?;
    let rev = super::super::rev_bytes(&bytes);
    Ok((parse_stored(&value)?, Some(rev)))
}

fn parse_stored(value: &Value) -> Result<Meta> {
    let obj = value
        .as_object()
        .ok_or_else(|| Error::rejected("wiki collection refused: stored metadata is malformed"))?;
    let documents = obj
        .get("documents")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            Error::rejected("wiki collection refused: stored documents are malformed")
        })?;
    let mut docs = Vec::new();
    for row in documents {
        let path = row["path"].as_str().unwrap_or("").to_string();
        if path.is_empty() {
            continue;
        }
        docs.push(Doc {
            path,
            source_type: row["source_type"].as_str().map(str::to_string),
            version: row["version"].as_str().map(str::to_string),
            applicability: row["applicability"].as_str().map(str::to_string),
            supersedes: row["supersedes"].as_str().map(str::to_string),
            approved_git: row["approved_revision"].as_str().map(str::to_string),
            approved_sha256: row["approved_sha256"].as_str().map(str::to_string),
            approved_by: row["approved_by"].as_str().map(str::to_string),
        });
    }
    Ok(Meta {
        publisher: obj["publisher"].as_str().map(str::to_string),
        reviewer: obj["reviewer"].as_str().map(str::to_string),
        review_project: obj["review_project"].as_str().map(str::to_string),
        documents: docs,
    })
}

pub(super) fn encode(meta: &Meta) -> Result<Vec<u8>> {
    let documents: Vec<Value> = meta
        .documents
        .iter()
        .map(|doc| {
            json!({
                "path": doc.path,
                "source_type": doc.source_type,
                "version": doc.version,
                "applicability": doc.applicability,
                "supersedes": doc.supersedes,
                "approved_revision": doc.approved_git,
                "approved_sha256": doc.approved_sha256,
                "approved_by": doc.approved_by,
            })
        })
        .collect();
    let value = json!({
        "publisher": meta.publisher,
        "reviewer": meta.reviewer,
        "review_project": meta.review_project,
        "documents": documents,
    });
    let mut bytes = serde_json::to_vec_pretty(&value)
        .map_err(|e| Error::internal(format!("wiki collection metadata: {e}")))?;
    bytes.push(b'\n');
    if bytes.len() > SIDECAR_CAP {
        return Err(Error::rejected(
            "wiki collection refused: metadata is over the 256KiB bound",
        ));
    }
    Ok(bytes)
}
