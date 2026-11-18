//! CAD-1393: company document template RPC handlers.
//!
//! Drafting (create, revise, document create, reads) is open to any
//! proven connection: the operator or a registered agent. Approval is
//! the operator's alone, by `operator_connection`. Company scope comes
//! from the store's connection workspace; a request that names a company
//! or an identity is refused, never read.

use super::caller_rule::Who;
use super::*;
use crate::business_templates::{
    build_suggested_html_from_intake, parse_definition, DocumentData, SourceFormat,
    TemplateDefinition,
};

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
            "business_template_list" => {
                self.business_template_caller("template list", params, peer_pid)?;
                Ok(json!({ "templates": self.store.business_template_list()? }))
            }
            "business_template_create" | "business_template_revise" => {
                let rev = method == "business_template_revise";
                let op = if rev {
                    "template revise"
                } else {
                    "template create"
                };
                self.business_template_caller(op, params, peer_pid)?;
                let name = required_str(params, "name")?;
                let base = rev.then(|| version_param(params, "base")).transpose()?;
                let def = parse_definition(spec_param(params)?)?;
                let (fmt, src) = definition_source(params, &def)?;
                let ver = self
                    .store
                    .business_template_add_version_with_format(name, base, fmt, &src, &def)?;
                let html = (ver.source_format == "html")
                    .then(|| String::from_utf8(src).ok())
                    .flatten();
                Ok(template_json(&ver, html))
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
                let provenance: Value = serde_json::from_slice(&out.provenance)
                    .map_err(|e| Error::internal(e.to_string()))?;
                Ok(json!({
                    "file": file,
                    "suggested_html": build_suggested_html_from_intake(file, &provenance),
                    "provenance": provenance,
                    "reconstruction_base64": out.reconstruction.map(|b| engine.encode(b)),
                }))
            }
            "business_template_sample" => {
                self.business_template_caller("template sample", params, peer_pid)?;
                let name = required_str(params, "name")?;
                let version = opt_ver(params, "version", &self.store, name, false)?;
                let format = optional_str(params, "format");
                // A draft or approved version: the retained snapshot of its
                // DOCX and spec, rendered with placeholder values. Nothing is
                // stored; no document is created, issued or sent.
                let (_, source) = self
                    .store
                    .business_template_snapshot_source(name, version)?
                    .ok_or_else(|| no_ver(name, version))?;
                let row = self
                    .store
                    .business_template_version(name, version)?
                    .ok_or_else(|| no_ver(name, version))?;
                let spec = row.spec.financial()?;
                let draft = crate::business_templates::build_draft(
                    spec,
                    &crate::business_templates::sample_data(spec),
                )?;
                if row.source_format == "html" {
                    ensure_local_html_format(format, "sample")?;
                    let h = crate::business_export::export_html(&source, &draft.context(), true)?;
                    return Ok(json!({
                        "sample": true, "status": row.status, "template": name,
                        "version": version, "source_format": "html",
                        "content_digest": row.content_sha256, "context": draft.context(),
                        "html_base64": b64_std(h.as_bytes()), "html": h,
                    }));
                }
                if format == Some("html") {
                    return Err(Error::rejected(format!("template '{name}' v{version} is a legacy DOCX template and cannot sample as HTML")));
                }
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
                    "source_format": row.source_format,
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
                            .ok_or_else(|| no_ver(name, version))?;
                        let html = (row.source_format == "html")
                            .then(|| self.store.business_template_snapshot_source(name, version))
                            .transpose()?
                            .flatten()
                            .and_then(|(_, b)| String::from_utf8(b).ok());
                        Ok(template_json(&row, html))
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
                Ok(template_json_v(&row))
            }
            "business_document_create" => {
                self.business_template_caller("document create", params, peer_pid)?;
                let name = required_str(params, "template")?;
                let ver = opt_ver(params, "version", &self.store, name, true)?;
                let partial = params
                    .get("allow_partial")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let raw = params.get("data").cloned().unwrap_or_else(|| json!({}));
                let data: DocumentData = serde_json::from_value(raw)
                    .map_err(|e| Error::rejected(format!("document data is malformed: {e}")))?;
                let doc = self
                    .store
                    .business_document_create_with_options(name, ver, &data, partial)?;
                let mut out = doc.to_json();
                if self
                    .store
                    .business_template_version(name, ver)?
                    .is_some_and(|t| t.source_format == "html")
                {
                    if let Some((_, source)) = self.store.business_template_source(name, ver)? {
                        let h = crate::business_export::export_html(&source, &doc.context, false)?;
                        out["source_format"] = json!("html");
                        out["html_base64"] = json!(b64_std(h.as_bytes()));
                        out["html"] = json!(h);
                    }
                }
                Ok(out)
            }
            "business_document_export" => {
                self.business_template_caller("document export", params, peer_pid)?;
                let (id, format) = (required_str(params, "id")?, optional_str(params, "format"));
                let doc = self
                    .store
                    .business_document(id)?
                    .ok_or_else(|| Error::rejected(format!("no document '{id}'")))?;
                let (tn, tv) = (&doc.template_name, doc.template_version);
                let row = self
                    .store
                    .business_template_version(tn, tv)?
                    .ok_or_else(|| Error::rejected("the template version no longer exists"))?;
                let (_, source) = self
                    .store
                    .business_template_source(tn, tv)?
                    .ok_or_else(|| Error::rejected("the template version is not approved"))?;
                if row.source_format == "html" {
                    ensure_local_html_format(format, "export")?;
                    let h = crate::business_export::export_html(&source, &doc.context, false)?;
                    return Ok(json!({
                        "id": doc.id, "template": tn, "version": tv, "source_format": "html",
                        "format": "html", "has_unresolved_pricing": doc.has_unresolved_pricing,
                        "html_base64": b64_std(h.as_bytes()), "html": h,
                    }));
                }
                if format == Some("html") {
                    return Err(Error::rejected(format!(
                        "document '{id}' uses a legacy DOCX template and cannot export as HTML"
                    )));
                }
                let out = if self.hosted_business_documents.is_some() {
                    crate::business_export::hosted::export(&source, &doc.context)?
                } else {
                    crate::business_export::export(&source, &doc.context)?
                };
                Ok(json!({
                    "id": doc.id, "source_format": row.source_format, "has_unresolved_pricing": doc.has_unresolved_pricing,
                    "docx_base64": b64_std(&out.docx), "pdf_base64": b64_std(&out.pdf),
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

fn no_ver(name: &str, v: i64) -> Error {
    Error::rejected(format!("template '{name}' has no version {v}"))
}

fn b64_std(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn opt_ver(params: &Value, field: &str, store: &Store, name: &str, approved: bool) -> Result<i64> {
    if !matches!(params.get(field), None | Some(Value::Null)) {
        return version_param(params, field);
    }
    if approved {
        return store
            .business_template_latest_approved_version(name)?
            .ok_or_else(|| {
                Error::rejected(format!(
                    "template '{name}' has no approved version yet; approve one first"
                ))
            });
    }
    store
        .business_template_versions(name)?
        .into_iter()
        .filter_map(|v| v.get("version").and_then(Value::as_i64))
        .max()
        .ok_or_else(|| Error::rejected(format!("template '{name}' has no versions")))
}

fn ensure_local_html_format(format: Option<&str>, op: &str) -> Result<()> {
    match format.unwrap_or("html") {
        "html" => Ok(()),
        "docx" => Err(Error::rejected(format!("HTML templates cannot {op} as DOCX; use --format html or hosted --format pdf"))),
        "pdf" | "both" => Err(Error::rejected("PDF rendering for HTML templates requires hosted Cloudflare Browser Rendering (--org <org>); use --format html locally")),
        other => Err(Error::rejected(format!("unsupported {op} format '{other}'"))),
    }
}

fn b64_decode(raw: &str, msg: &str) -> Result<Vec<u8>> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(raw)
        .map_err(|_| Error::rejected(msg))
}

fn definition_source(params: &Value, def: &TemplateDefinition) -> Result<(SourceFormat, Vec<u8>)> {
    let html_str = params.get("html").and_then(Value::as_str);
    let html_b64 = params.get("html_base64").and_then(Value::as_str);
    let docx_b64 = params
        .get("source")
        .or_else(|| params.get("docx_base64"))
        .and_then(Value::as_str);
    match def {
        TemplateDefinition::Financial(_) => {
            if (html_str.is_some() && html_b64.is_some())
                || ((html_str.is_some() || html_b64.is_some())
                    && docx_b64.is_some_and(|s| !s.is_empty()))
            {
                return Err(Error::rejected(
                    "supply one template source ('html', 'html_base64', or DOCX)",
                ));
            }
            if let Some(h) = html_str {
                if h.len() > crate::business_templates::HTML_MAX_BYTES {
                    return Err(Error::rejected("HTML template exceeds the upload limit"));
                }
                return Ok((SourceFormat::Html, h.as_bytes().to_vec()));
            }
            if let Some(enc) = html_b64 {
                if enc.len() > crate::business_templates::HTML_MAX_BYTES * 4 / 3 + 4 {
                    return Err(Error::rejected("HTML template exceeds the upload limit"));
                }
                return Ok((
                    SourceFormat::Html,
                    b64_decode(enc, "'html_base64' is not valid base64")?,
                ));
            }
            Ok((SourceFormat::Docx, source_param(params)?))
        }
        TemplateDefinition::Authoring(_) => {
            if html_str.is_some() || html_b64.is_some() || docx_b64.is_some_and(|s| !s.is_empty()) {
                return Err(Error::rejected(
                    "authoring reference packs do not accept an HTML or DOCX source",
                ));
            }
            match params.get("source").or_else(|| params.get("docx_base64")) {
                None | Some(Value::Null) | Some(Value::String(_)) => {
                    Ok((SourceFormat::Authoring, Vec::new()))
                }
                Some(_) => Err(Error::rejected(
                    "authoring reference packs do not accept a DOCX source",
                )),
            }
        }
    }
}

fn source_param(params: &Value) -> Result<Vec<u8>> {
    let text = params
        .get("source")
        .or_else(|| params.get("docx_base64"))
        .and_then(Value::as_str)
        .ok_or_else(|| Error::rejected("Missing 'source' (base64 DOCX or HTML bytes)"))?;
    if text.len() > crate::business_templates::DOCX_MAX_BYTES * 4 / 3 + 4 {
        return Err(Error::rejected("DOCX source exceeds the upload limit"));
    }
    b64_decode(text, "'source' is not valid base64")
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

fn template_json_v(row: &crate::store::TemplateVersion) -> Value {
    json!({
        "template": row.summary(),
        "spec": row.spec,
    })
}

fn template_json(row: &crate::store::TemplateVersion, html: Option<String>) -> Value {
    let mut out = template_json_v(row);
    if let Some(html) = html {
        out["html"] = json!(html);
    }
    out
}
