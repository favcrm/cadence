//! CAD-1393: company document template RPC handlers.
//!
//! Drafting (create, revise, document create, reads) is open to any
//! proven connection: the operator or a registered agent. Approval is
//! the operator's alone, by `operator_connection`. Company scope comes
//! from the store's connection workspace; a request that names a company
//! or an identity is refused, never read.

use super::caller_rule::Who;
use super::*;
use crate::business_templates::{parse_definition, DocumentData, TemplateDefinition};

/// Request fields that would name a company or an actor. Refused, never
/// read: scope and identity come from the connection alone.
const SCOPE_FIELDS: &[&str] = &["company", "company_id", "workspace", "workspace_id", "org"];

impl Shared {
    /// The one drafting gate: a proven connection (operator or a registered
    /// agent), with no request-named scope or identity.
    fn business_template_caller(&self, verb: &str, params: &Value, peer_pid: u32) -> Result<()> {
        for field in SCOPE_FIELDS {
            if params.get(field).is_some() {
                return Err(Error::rejected(format!(
                    "{verb}: company scope comes from the connection; request field '{field}' is not accepted"
                )));
            }
        }
        reject_identity_fields(params, verb)?;
        match self.connection_caller(peer_pid)? {
            Who::Unproven(why) => Err(Error::rejected(format!(
                "{verb} refused: this connection derives no agent identity and is not provably the operator: {why}"
            ))),
            _ => Ok(()),
        }
    }

    /// Dispatch for every `business_template_*` and `business_document_*`
    /// method. Approval is the only operator-only verb.
    pub(super) fn rpc_business_template(
        &self,
        method: &str,
        params: &Value,
        peer_pid: u32,
    ) -> Result<Value> {
        match method {
            "business_template_create" => {
                self.business_template_caller("template create", params, peer_pid)?;
                let name = required_str(params, "name")?;
                let definition = parse_definition(spec_param(params)?)?;
                let source = definition_source(params, &definition)?;
                let version =
                    self.store
                        .business_template_add_version(name, None, &source, &definition)?;
                Ok(template_json(&version))
            }
            "business_template_revise" => {
                self.business_template_caller("template revise", params, peer_pid)?;
                let name = required_str(params, "name")?;
                let base = version_param(params, "base")?;
                let definition = parse_definition(spec_param(params)?)?;
                let source = definition_source(params, &definition)?;
                let version = self.store.business_template_add_version(
                    name,
                    Some(base),
                    &source,
                    &definition,
                )?;
                Ok(template_json(&version))
            }
            "business_template_intake" => {
                self.business_template_caller("template intake", params, peer_pid)?;
                let file = required_str(params, "file")?;
                let source = source_param(params)?;
                let out = if self.hosted_business_documents.is_some() {
                    crate::business_export::hosted::intake(file, &source)?
                } else {
                    crate::business_export::intake(file, &source)?
                };
                use base64::Engine;
                let engine = base64::engine::general_purpose::STANDARD;
                Ok(json!({
                    "file": file,
                    "provenance": serde_json::from_slice::<Value>(&out.provenance)
                        .map_err(|e| Error::internal(e.to_string()))?,
                    "reconstruction_base64": out.reconstruction.map(|b| engine.encode(b)),
                }))
            }
            "business_template_sample" => {
                self.business_template_caller("template sample", params, peer_pid)?;
                let name = required_str(params, "name")?;
                let version = version_param(params, "version")?;
                // A draft or approved version: the retained snapshot of its
                // DOCX and spec, rendered with placeholder values. Nothing is
                // stored; no document is created, issued or sent.
                let (_, source) = self
                    .store
                    .business_template_snapshot_source(name, version)?
                    .ok_or_else(|| {
                        Error::rejected(format!("template '{name}' has no version {version}"))
                    })?;
                let row = self
                    .store
                    .business_template_version(name, version)?
                    .ok_or_else(|| {
                        Error::rejected(format!("template '{name}' has no version {version}"))
                    })?;
                let spec = row.spec.financial()?;
                let draft = crate::business_templates::build_draft(
                    spec,
                    &crate::business_templates::sample_data(spec),
                )?;
                let out = if self.hosted_business_documents.is_some() {
                    crate::business_export::hosted::export_sample(&source, &draft.context())?
                } else {
                    crate::business_export::export_sample(&source, &draft.context())?
                };
                use base64::Engine;
                let engine = base64::engine::general_purpose::STANDARD;
                Ok(json!({
                    "sample": true,
                    "status": row.status,
                    "template": name,
                    "version": version,
                    "context": draft.context(),
                    "docx_base64": engine.encode(out.docx),
                    "pdf_base64": engine.encode(out.pdf),
                }))
            }
            "business_template_show" => {
                self.business_template_caller("template show", params, peer_pid)?;
                let name = required_str(params, "name")?;
                match params.get("version") {
                    None | Some(Value::Null) => Ok(json!({
                        "name": name,
                        "versions": self.store.business_template_versions(name)?,
                    })),
                    Some(_) => {
                        let version = version_param(params, "version")?;
                        let row = self
                            .store
                            .business_template_version(name, version)?
                            .ok_or_else(|| {
                                Error::rejected(format!(
                                    "template '{name}' has no version {version}"
                                ))
                            })?;
                        Ok(template_json(&row))
                    }
                }
            }
            "business_template_approve" => {
                self.operator_connection("template approve", params, peer_pid)?;
                for field in SCOPE_FIELDS {
                    if params.get(field).is_some() {
                        return Err(Error::rejected(format!(
                            "template approve: company scope comes from the connection; request field '{field}' is not accepted"
                        )));
                    }
                }
                let name = required_str(params, "name")?;
                let version = version_param(params, "version")?;
                let digest = required_str(params, "digest")?;
                let source = required_str(params, "source")?;
                let row = self
                    .store
                    .business_template_approve(name, version, digest, source)?;
                Ok(template_json(&row))
            }
            "business_document_create" => {
                self.business_template_caller("document create", params, peer_pid)?;
                let name = required_str(params, "template")?;
                let version = version_param(params, "version")?;
                let data: DocumentData = serde_json::from_value(
                    params.get("data").cloned().unwrap_or_else(|| json!({})),
                )
                .map_err(|e| Error::rejected(format!("document data is malformed: {e}")))?;
                let doc = self.store.business_document_create(name, version, &data)?;
                Ok(doc.to_json())
            }
            "business_document_export" => {
                self.business_template_caller("document export", params, peer_pid)?;
                let id = required_str(params, "id")?;
                let doc = self
                    .store
                    .business_document(id)?
                    .ok_or_else(|| Error::rejected(format!("no document '{id}'")))?;
                let (_, source) = self
                    .store
                    .business_template_source(&doc.template_name, doc.template_version)?
                    .ok_or_else(|| Error::rejected("the template version is not approved"))?;
                let out = if self.hosted_business_documents.is_some() {
                    crate::business_export::hosted::export(&source, &doc.context)?
                } else {
                    crate::business_export::export(&source, &doc.context)?
                };
                use base64::Engine;
                Ok(json!({
                    "id": doc.id,
                    "docx_base64": base64::engine::general_purpose::STANDARD.encode(out.docx),
                    "pdf_base64": base64::engine::general_purpose::STANDARD.encode(out.pdf),
                }))
            }
            "business_document_show" => {
                self.business_template_caller("document show", params, peer_pid)?;
                let id = required_str(params, "id")?;
                let doc = self
                    .store
                    .business_document(id)?
                    .ok_or_else(|| Error::rejected(format!("no document '{id}'")))?;
                Ok(doc.to_json())
            }
            other => Err(Error::internal(format!("not a template method: {other}"))),
        }
    }
}

/// Financial versions require the historical base64 DOCX. Authoring packs
/// omit `source` (or send an empty string) and never accept a document.
fn definition_source(params: &Value, definition: &TemplateDefinition) -> Result<Vec<u8>> {
    match definition {
        TemplateDefinition::Financial(_) => source_param(params),
        TemplateDefinition::Authoring(_) => match params.get("source") {
            None | Some(Value::Null) => Ok(Vec::new()),
            Some(Value::String(text)) if text.is_empty() => Ok(Vec::new()),
            Some(_) => Err(Error::rejected(
                "authoring reference packs do not accept a DOCX source",
            )),
        },
    }
}

/// The DOCX source, base64 in the `source` field. Bounded before decoding.
fn source_param(params: &Value) -> Result<Vec<u8>> {
    use base64::Engine;
    let text = params
        .get("source")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::rejected("Missing 'source' (base64 DOCX bytes)"))?;
    if text.len() > crate::business_templates::DOCX_MAX_BYTES * 4 / 3 + 4 {
        return Err(Error::rejected("DOCX source exceeds the upload limit"));
    }
    base64::engine::general_purpose::STANDARD
        .decode(text)
        .map_err(|_| Error::rejected("'source' is not valid base64"))
}

fn spec_param(params: &Value) -> Result<Value> {
    params
        .get("spec")
        .cloned()
        .ok_or_else(|| Error::rejected("Missing 'spec' (the template spec object)"))
}

fn version_param(params: &Value, field: &str) -> Result<i64> {
    params
        .get(field)
        .and_then(Value::as_i64)
        .filter(|v| *v > 0)
        .ok_or_else(|| Error::rejected(format!("Missing or non-positive integer '{field}'")))
}

fn template_json(row: &crate::store::TemplateVersion) -> Value {
    json!({
        "template": row.summary(),
        "spec": row.spec,
    })
}
