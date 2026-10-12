//! CAD-1393: `cadence template` and `cadence document` — company document
//! templates and drafts over the daemon's business-template RPCs. Input is
//! a JSON file; the daemon validates, calculates and enforces scope.

use super::*;

#[derive(Subcommand)]
pub(crate) enum TemplateAction {
    /// Create version 1 of a template from its DOCX source and a spec JSON
    /// file. The daemon assigns the version; the result is a draft and is
    /// not approved.
    Create {
        /// The template name (lowercase letters, digits, '-').
        name: String,
        /// The company's DOCX template, retained as the approved source.
        #[arg(long)]
        docx: PathBuf,
        /// The spec JSON: kind, fields, fixed_text and calculation rules.
        #[arg(long)]
        file: PathBuf,
    },
    /// Create the next version of a template from `--base`. The base is
    /// left unchanged; the result is a new draft.
    Revise {
        name: String,
        /// The version this revision starts from.
        #[arg(long)]
        base: i64,
        #[arg(long)]
        docx: PathBuf,
        #[arg(long)]
        file: PathBuf,
    },
    /// List the versions of a template, or show one with `--version`.
    Show {
        name: String,
        #[arg(long)]
        version: Option<i64>,
    },
    /// Read a company DOCX or readable PDF as a reference: its text,
    /// placeholders, loop tags, tables and styles, with provenance. Proposes
    /// nothing and stores nothing; a PDF also yields an editable DOCX
    /// reconstruction in `--out` that needs its own approval.
    Intake {
        /// The reference file (DOCX or readable PDF).
        file: PathBuf,
        /// Existing directory for the reconstruction DOCX (PDF only).
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Render a labelled SAMPLE preview of a draft or approved version with
    /// placeholder values, to DOCX and PDF in `--out`. Stores nothing.
    Sample {
        name: String,
        #[arg(long)]
        version: i64,
        #[arg(long)]
        out: PathBuf,
    },
    /// Approve exactly one version (operator connection only). `--sha256`
    /// must be the content digest `template show` printed for it.
    Approve {
        name: String,
        #[arg(long)]
        version: i64,
        #[arg(long)]
        sha256: String,
        /// Who decided and where (e.g. "chris in chat 22:33Z").
        #[arg(long)]
        source: String,
    },
}

#[derive(Subcommand)]
pub(crate) enum DocumentAction {
    /// Create a draft from an approved template version. Validates the data
    /// and computes totals in code. Issues no invoice number and sends
    /// nothing.
    Create {
        /// The approved template name.
        template: String,
        #[arg(long)]
        version: i64,
        /// The data JSON: fields and line items (quantities and prices as text).
        #[arg(long)]
        file: PathBuf,
    },
    /// Show a retained draft with its input and computed context.
    Show { id: String },
    /// Export a retained draft to DOCX and PDF files in `--out`. Creates no
    /// invoice, number or send: the files are a preview of the draft.
    Export {
        id: String,
        /// Existing directory the DOCX and PDF are written into.
        #[arg(long)]
        out: PathBuf,
    },
}

/// The DOCX bytes, base64 for the JSON request.
fn read_docx(path: &PathBuf) -> Result<String> {
    use base64::Engine;
    let bytes = std::fs::read(path)
        .map_err(|e| Error::rejected(format!("Cannot read {}: {e}", path.display())))?;
    Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
}

fn read_json(path: &PathBuf) -> Result<Value> {
    let bytes = std::fs::read(path)
        .map_err(|e| Error::rejected(format!("Cannot read {}: {e}", path.display())))?;
    serde_json::from_slice(&bytes)
        .map_err(|e| Error::rejected(format!("{} is not valid JSON: {e}", path.display())))
}

pub(super) fn run_template(state_dir: PathBuf, action: TemplateAction) -> Result<i32> {
    let result = match action {
        TemplateAction::Create { name, docx, file } => client::rpc(
            &state_dir,
            "business_template_create",
            json!({"name": name, "source": read_docx(&docx)?, "spec": read_json(&file)?}),
        )?,
        TemplateAction::Revise {
            name,
            base,
            docx,
            file,
        } => client::rpc(
            &state_dir,
            "business_template_revise",
            json!({"name": name, "base": base, "source": read_docx(&docx)?, "spec": read_json(&file)?}),
        )?,
        TemplateAction::Show { name, version } => {
            let mut params = json!({"name": name});
            if let Some(version) = version {
                params["version"] = json!(version);
            }
            client::rpc(&state_dir, "business_template_show", params)?
        }
        TemplateAction::Intake { file, out } => {
            use base64::Engine;
            let bytes = std::fs::read(&file)
                .map_err(|e| Error::rejected(format!("Cannot read {}: {e}", file.display())))?;
            let name = file
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| Error::rejected("the reference file needs a file name"))?
                .to_string();
            let source = base64::engine::general_purpose::STANDARD.encode(bytes);
            let mut result = client::rpc(
                &state_dir,
                "business_template_intake",
                json!({"file": name, "source": source}),
            )?;
            if let Some(encoded) = result["reconstruction_base64"].as_str().map(str::to_string) {
                let out = out.ok_or_else(|| {
                    Error::rejected("a PDF reconstruction needs --out (an existing directory)")
                })?;
                let docx = base64::engine::general_purpose::STANDARD
                    .decode(encoded)
                    .map_err(|_| Error::rejected("intake returned invalid DOCX bytes"))?;
                let stem = file
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("reference");
                let path = out.join(format!("{stem}.reconstruction.docx"));
                std::fs::write(&path, docx)
                    .map_err(|e| Error::rejected(format!("cannot write reconstruction: {e}")))?;
                result["reconstruction_file"] = json!(path);
            }
            if let Some(object) = result.as_object_mut() {
                object.remove("reconstruction_base64");
            }
            result
        }
        TemplateAction::Sample { name, version, out } => {
            use base64::Engine;
            let result = client::rpc(
                &state_dir,
                "business_template_sample",
                json!({"name": name, "version": version}),
            )?;
            let engine = base64::engine::general_purpose::STANDARD;
            let docx = engine
                .decode(result["docx_base64"].as_str().unwrap_or_default())
                .map_err(|_| Error::rejected("sample returned invalid DOCX bytes"))?;
            let pdf = engine
                .decode(result["pdf_base64"].as_str().unwrap_or_default())
                .map_err(|_| Error::rejected("sample returned invalid PDF bytes"))?;
            let docx_path = out.join(format!("{name}-v{version}-sample.docx"));
            let pdf_path = out.join(format!("{name}-v{version}-sample.pdf"));
            std::fs::write(&docx_path, docx)
                .and_then(|_| std::fs::write(&pdf_path, pdf))
                .map_err(|e| Error::rejected(format!("cannot write sample: {e}")))?;
            json!({"sample": true, "docx": docx_path, "pdf": pdf_path, "context": result["context"]})
        }
        TemplateAction::Approve {
            name,
            version,
            sha256,
            source,
        } => client::rpc(
            &state_dir,
            "business_template_approve",
            json!({"name": name, "version": version, "digest": sha256, "source": source}),
        )?,
    };
    print_json(&result);
    Ok(0)
}

pub(super) fn run_document(state_dir: PathBuf, action: DocumentAction) -> Result<i32> {
    let result = match action {
        DocumentAction::Create {
            template,
            version,
            file,
        } => client::rpc(
            &state_dir,
            "business_document_create",
            json!({"template": template, "version": version, "data": read_json(&file)?}),
        )?,
        DocumentAction::Show { id } => {
            client::rpc(&state_dir, "business_document_show", json!({"id": id}))?
        }
        DocumentAction::Export { id, out } => {
            use base64::Engine;
            let result = client::rpc(&state_dir, "business_document_export", json!({"id": id}))?;
            let engine = base64::engine::general_purpose::STANDARD;
            let docx = engine
                .decode(result["docx_base64"].as_str().unwrap_or_default())
                .map_err(|_| Error::rejected("export returned invalid DOCX bytes"))?;
            let pdf = engine
                .decode(result["pdf_base64"].as_str().unwrap_or_default())
                .map_err(|_| Error::rejected("export returned invalid PDF bytes"))?;
            let docx_path = out.join(format!("{id}.docx"));
            let pdf_path = out.join(format!("{id}.pdf"));
            std::fs::write(&docx_path, docx)
                .and_then(|_| std::fs::write(&pdf_path, pdf))
                .map_err(|e| Error::rejected(format!("cannot write export: {e}")))?;
            json!({"id": id, "docx": docx_path, "pdf": pdf_path})
        }
    };
    print_json(&result);
    Ok(0)
}
