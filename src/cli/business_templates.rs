//! CAD-1393: `cadence template` and `cadence document` — company document
//! templates and drafts over the daemon's business-template RPCs. Input is
//! a JSON file; the daemon validates, calculates and enforces scope.
//!
//! Quotation and invoice versions keep a DOCX source. An authoring pack
//! (`kind` is `authoring`) is guidelines and reference bytes only: create
//! and revise omit `--docx`, and `show --out` writes that exact version
//! into a new directory. It does not render, price, or approve anything.

#![forbid(unsafe_code)]

use super::*;
use std::ffi::{CString, OsStr};
use std::os::unix::ffi::OsStrExt;

use cadence_agent::fd_fs::DirFd;

#[derive(Subcommand)]
pub(crate) enum TemplateAction {
    /// List all templates with their latest and latest-approved versions.
    #[command(visible_alias = "ls")]
    List,
    /// Create version 1 from a spec JSON file. A quotation or invoice also
    /// needs `--html` or `--docx`. An authoring pack must omit both. The daemon
    /// assigns the version; the result is a draft and is not approved.
    Create {
        /// The template name (lowercase letters, digits, '-').
        name: String,
        /// HTML5 source for a quotation or invoice. Refused for an authoring pack.
        #[arg(long, conflicts_with = "docx")]
        html: Option<PathBuf>,
        /// DOCX source for a quotation or invoice. Refused for an authoring pack.
        #[arg(long, conflicts_with = "html")]
        docx: Option<PathBuf>,
        /// Spec JSON. Financial: kind, fields, fixed_text, calculation.
        /// Authoring: kind "authoring", purpose, format, guidelines, references.
        #[arg(long)]
        file: PathBuf,
    },
    /// Create the next version from `--base`. The base is left unchanged;
    /// the result is a new draft and does not inherit approval.
    Revise {
        name: String,
        /// The version this revision starts from.
        #[arg(long)]
        base: i64,
        /// HTML5 source for a quotation or invoice. Refused for an authoring pack.
        #[arg(long, conflicts_with = "docx")]
        html: Option<PathBuf>,
        /// DOCX source for a quotation or invoice. Refused for an authoring pack.
        #[arg(long, conflicts_with = "html")]
        docx: Option<PathBuf>,
        #[arg(long)]
        file: PathBuf,
    },
    /// List the versions of a template, or show one with `--version`.
    /// `--out` materialises an authoring pack or writes an HTML template.
    Show {
        name: String,
        #[arg(long)]
        version: Option<i64>,
        /// New directory for an authoring pack (`guidelines.md`, reference
        /// files, `template.json`), or new `.html` file for HTML templates. Requires `--version`. Must not exist.
        #[arg(long)]
        out: Option<PathBuf>,
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
    /// placeholder values. Stores nothing.
    Sample {
        name: String,
        #[arg(long)]
        version: Option<i64>,
        #[arg(long)]
        out: PathBuf,
        /// html, pdf, docx, or both.
        #[arg(long)]
        format: Option<String>,
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

#[derive(clap::Args, Clone, Debug)]
pub(crate) struct ArtifactSaveArgs {
    /// Save the rendered output as a Canvas artifact.
    #[arg(long)]
    pub(crate) save_artifact: bool,
    #[arg(long)]
    pub(crate) title: Option<String>,
    #[arg(long)]
    pub(crate) label: Option<String>,
    #[arg(long)]
    pub(crate) customer: Option<String>,
    #[arg(long)]
    pub(crate) issue: Option<String>,
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
        version: Option<i64>,
        /// The data JSON: fields and line items (quantities and prices as text).
        #[arg(long)]
        file: PathBuf,
        /// Allow empty or "UNRESOLVED" unit prices and mark `has_unresolved_pricing`.
        #[arg(long)]
        allow_partial: bool,
        #[command(flatten)]
        artifact: ArtifactSaveArgs,
    },
    /// Show a retained draft with its input and computed context.
    Show { id: String },
    /// Export a retained draft to HTML/PDF/DOCX (`--out`) and/or Canvas (`--save-artifact`).
    Export {
        id: String,
        #[arg(long)]
        out: Option<PathBuf>,
        #[arg(long)]
        format: Option<String>,
        #[command(flatten)]
        artifact: ArtifactSaveArgs,
    },
}

fn erej(m: impl Into<String>) -> Error {
    Error::rejected(m)
}
fn b64_dec(s: &str, msg: &str) -> Result<Vec<u8>> {
    use base64::{engine::general_purpose::STANDARD as B64, Engine};
    B64.decode(s).map_err(|_| erej(msg))
}
fn b64_enc(b: &[u8]) -> String {
    use base64::{engine::general_purpose::STANDARD as B64, Engine};
    B64.encode(b)
}

pub(super) fn read_bounded_template_file(path: &Path, cap: u64, label: &str) -> Result<Vec<u8>> {
    use std::io::Read;
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt;

    let d = path.display();
    let over = |n| {
        erej(format!(
            "{label} refused: {n} bytes > {cap}-byte cap; nothing sent"
        ))
    };
    let mut opt = std::fs::OpenOptions::new();
    opt.read(true);
    #[cfg(unix)]
    opt.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    let file = opt.open(path).map_err(|e| {
        #[cfg(unix)]
        if e.raw_os_error() == Some(libc::ELOOP) {
            return erej(format!("{label} refused: {d} is a symlink"));
        }
        erej(format!("{label}: cannot open {d}: {e}"))
    })?;
    let meta = file
        .metadata()
        .map_err(|e| erej(format!("{label}: cannot read {d}: {e}")))?;
    if !meta.is_file() {
        return Err(erej(format!("{label} refused: {d} is not a file")));
    }
    if meta.len() > cap {
        return Err(over(meta.len()));
    }
    let mut bytes = Vec::new();
    (&file).take(cap + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > cap {
        return Err(over(bytes.len() as u64));
    }
    Ok(bytes)
}

/// The DOCX bytes, base64 for the JSON request.
fn read_docx(path: &Path) -> Result<String> {
    Ok(b64_enc(&read_bounded_template_file(
        path,
        10 * 1024 * 1024,
        "DOCX template",
    )?))
}

pub(super) fn read_html_source(path: &Path) -> Result<String> {
    let cap = cadence_agent::business_templates::html::HTML_MAX_BYTES;
    let bytes = read_bounded_template_file(path, cap as u64, "HTML template")?;
    let d = path.display();
    if bytes.is_empty() {
        return Err(erej(format!("HTML template {d} must be 1..={cap} bytes")));
    }
    String::from_utf8(bytes).map_err(|_| erej(format!("{d} is not valid UTF-8 HTML")))
}

pub(super) fn read_json(path: &PathBuf) -> Result<Value> {
    let d = path.display();
    let bytes = std::fs::read(path).map_err(|e| erej(format!("Cannot read {d}: {e}")))?;
    serde_json::from_slice(&bytes).map_err(|e| erej(format!("{d} is not valid JSON: {e}")))
}

pub(super) fn validate_artifact_meta_flags(a: &ArtifactSaveArgs) -> Result<()> {
    let any = a.title.is_some() || a.label.is_some() || a.customer.is_some() || a.issue.is_some();
    if !a.save_artifact && any {
        return Err(Error::usage(
            "--title, --label, --customer and --issue require --save-artifact",
        ));
    }
    Ok(())
}

fn validate_native_save_artifact(a: &ArtifactSaveArgs) -> Result<()> {
    validate_artifact_meta_flags(a)?;
    if a.save_artifact && (a.label.is_some() || a.customer.is_some() || a.issue.is_some()) {
        return Err(erej(
            "--label, --customer and --issue with --save-artifact require a hosted --org workspace",
        ));
    }
    Ok(())
}

pub(super) fn validate_export_target(out: &Option<PathBuf>, save_artifact: bool) -> Result<()> {
    if out.is_none() && !save_artifact {
        return Err(Error::usage(
            "document export requires --out <path> or --save-artifact",
        ));
    }
    Ok(())
}

pub(super) fn normalize_sample_or_export_format(format: Option<&str>) -> Result<Option<String>> {
    match format {
        None => Ok(None),
        Some(raw) => match raw.trim().to_ascii_lowercase().as_str() {
            f @ ("html" | "pdf" | "docx" | "both") => Ok(Some(f.to_string())),
            _ => Err(Error::usage(format!(
                "unsupported format '{raw}'; expected html, pdf, docx, or both"
            ))),
        },
    }
}

pub(super) fn write_new_file(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt;
    let mut opt = std::fs::OpenOptions::new();
    opt.write(true).create_new(true);
    #[cfg(unix)]
    opt.mode(0o600);
    let d = path.display();
    let mut file = opt
        .open(path)
        .map_err(|e| erej(format!("cannot create {d}: {e}")))?;
    if let Err(err) = file.write_all(bytes) {
        let _ = std::fs::remove_file(path);
        return Err(erej(format!("cannot write {d}: {err}")));
    }
    Ok(())
}

pub(super) fn resolve_single_or_dir_out(out: &Path, name: &str) -> PathBuf {
    if out.is_dir() {
        out.join(name)
    } else {
        out.to_path_buf()
    }
}

fn native_template_params(
    name: &str,
    base: Option<i64>,
    html: Option<PathBuf>,
    docx: Option<PathBuf>,
    file: &PathBuf,
) -> Result<(&'static str, Value)> {
    let spec = read_json(file)?;
    let auth = spec.get("kind").and_then(Value::as_str) == Some("authoring");
    let (h, d) = match (auth, html, docx) {
        (true, Some(_), _) | (true, _, Some(_)) => {
            return Err(erej("an authoring pack does not accept --html or --docx"))
        }
        (true, None, None) => (None, None),
        (false, Some(p), None) => (Some(read_html_source(&p)?), None),
        (false, None, Some(p)) => (None, Some(read_docx(&p)?)),
        (false, Some(_), Some(_)) => {
            return Err(Error::usage("pass either --html or --docx, not both"))
        }
        (false, None, None) => {
            return Err(erej(
                "a quotation or invoice template requires --html or --docx",
            ))
        }
    };
    let mut params = json!({"name": name, "spec": spec});
    if let Some(v) = h {
        params["html"] = json!(v);
    }
    if let Some(v) = d {
        params["source"] = json!(v);
    }
    if let Some(b) = base {
        params["base"] = json!(b);
        return Ok(("business_template_revise", params));
    }
    Ok(("business_template_create", params))
}

fn decode_b64_field(v: &Value, key: &str, missing: &str, bad: &str) -> Result<Vec<u8>> {
    b64_dec(
        v.get(key)
            .and_then(Value::as_str)
            .ok_or_else(|| erej(missing))?,
        bad,
    )
}

fn extract_html_bytes_from_value(v: &Value) -> Result<Vec<u8>> {
    if let Some(h) = v.get("html").and_then(Value::as_str) {
        return Ok(h.as_bytes().to_vec());
    }
    decode_b64_field(
        v,
        "html_base64",
        "server response contained no HTML content",
        "invalid HTML base64",
    )
}

pub(super) fn write_rendered_outputs(
    out: &Path,
    stem: &str,
    format: Option<&str>,
    summary: &mut serde_json::Map<String, Value>,
    get_html: impl FnOnce() -> Result<Vec<u8>>,
    get_pdf: impl FnOnce() -> Result<Vec<u8>>,
) -> Result<()> {
    let fmt = format.unwrap_or("html");
    if fmt == "pdf" {
        let p = resolve_single_or_dir_out(out, &format!("{stem}.pdf"));
        write_new_file(&p, &get_pdf()?)?;
        summary.insert("pdf".into(), json!(p));
        return Ok(());
    }
    let html = get_html()?;
    let hp = resolve_single_or_dir_out(out, &format!("{stem}.html"));
    if fmt == "both" {
        let pdf = get_pdf()?;
        let pp = if out.is_dir() {
            out.join(format!("{stem}.pdf"))
        } else {
            out.with_extension("pdf")
        };
        if pp == hp {
            return Err(erej(
                "pass a directory or .html path for --out with --format both",
            ));
        }
        write_new_file(&hp, &html)?;
        if let Err(err) = write_new_file(&pp, &pdf) {
            let _ = std::fs::remove_file(&hp);
            return Err(err);
        }
        summary.insert("html".into(), json!(hp));
        summary.insert("pdf".into(), json!(pp));
        return Ok(());
    }
    write_new_file(&hp, &html)?;
    summary.insert("html".into(), json!(hp));
    Ok(())
}

fn write_sample_or_export(
    out: &Path,
    stem: &str,
    fmt: Option<&str>,
    res: &Value,
    mode: &str,
) -> Result<Value> {
    if res.get("docx_base64").is_none() {
        let mut s = if mode == "sample" {
            json!({"sample": true, "context": res["context"]})
        } else {
            json!({"id": res["id"]})
        };
        let map = s.as_object_mut().expect("json object");
        let gh = || extract_html_bytes_from_value(res);
        let gp = || decode_b64_field(res, "pdf_base64", "PDF output missing", "invalid PDF bytes");
        write_rendered_outputs(out, stem, fmt, map, gh, gp)?;
        return Ok(s);
    }
    let docx = decode_b64_field(res, "docx_base64", "missing DOCX", "invalid DOCX bytes")?;
    let pdf = decode_b64_field(res, "pdf_base64", "missing PDF", "invalid PDF bytes")?;
    let (dp, pp) = (
        out.join(format!("{stem}.docx")),
        out.join(format!("{stem}.pdf")),
    );
    std::fs::write(&dp, docx)
        .and_then(|_| std::fs::write(&pp, pdf))
        .map_err(|e| erej(format!("cannot write {mode}: {e}")))?;
    Ok(if mode == "sample" {
        json!({"sample": true, "docx": dp, "pdf": pp, "context": res["context"]})
    } else {
        json!({"id": stem, "docx": dp, "pdf": pp})
    })
}

pub(super) fn run_template(state_dir: PathBuf, action: TemplateAction) -> Result<i32> {
    let result = match action {
        TemplateAction::List => client::rpc(&state_dir, "business_template_list", json!({}))?,
        TemplateAction::Create {
            name,
            html,
            docx,
            file,
        } => {
            let (m, p) = native_template_params(&name, None, html, docx, &file)?;
            client::rpc(&state_dir, m, p)?
        }
        TemplateAction::Revise {
            name,
            base,
            html,
            docx,
            file,
        } => {
            let (m, p) = native_template_params(&name, Some(base), html, docx, &file)?;
            client::rpc(&state_dir, m, p)?
        }
        TemplateAction::Show { name, version, out } => {
            if out.is_some() && version.is_none() {
                return Err(erej("template show --out requires --version"));
            }
            let mut params = json!({"name": name});
            if let Some(v) = version {
                params["version"] = json!(v);
            }
            let r = client::rpc(&state_dir, "business_template_show", params)?;
            match out {
                Some(p) if r["spec"]["kind"].as_str() == Some("authoring") => {
                    materialise_authoring_pack(&p, r)?
                }
                Some(p)
                    if r["template"]["source_format"].as_str() == Some("html")
                        || r.get("html").is_some() =>
                {
                    write_new_file(&p, &extract_html_bytes_from_value(&r)?)?;
                    json!({"template": r["template"], "spec": r["spec"], "written": p})
                }
                Some(_) => {
                    return Err(erej(
                        "template show --out supports authoring packs and HTML templates",
                    ))
                }
                None => r,
            }
        }
        TemplateAction::Intake { file, out } => {
            use base64::Engine;
            let bytes = read_bounded_template_file(&file, 10 * 1024 * 1024, "template intake")?;
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
        TemplateAction::Sample {
            name,
            version,
            out,
            format,
        } => {
            let fmt = normalize_sample_or_export_format(format.as_deref())?;
            let mut params = json!({"name": name});
            if let Some(v) = version {
                params["version"] = json!(v);
            }
            if let Some(f) = &fmt {
                params["format"] = json!(f);
            }
            let r = client::rpc(&state_dir, "business_template_sample", params)?;
            let stem = format!(
                "{name}-v{}-sample",
                r["version"].as_i64().or(version).unwrap_or(1)
            );
            write_sample_or_export(&out, &stem, fmt.as_deref(), &r, "sample")?
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

fn save_native_artifact(
    state_dir: &Path,
    a: &ArtifactSaveArgs,
    default_title: &str,
    kind: &str,
    bytes: &[u8],
) -> Result<Value> {
    use base64::{engine::general_purpose::STANDARD as B64, Engine};
    client::rpc(
        state_dir,
        "artifact_put",
        json!({"kind": kind, "title": a.title.as_deref().unwrap_or(default_title), "content": B64.encode(bytes)}),
    )
}

pub(super) fn run_document(state_dir: PathBuf, action: DocumentAction) -> Result<i32> {
    let result = match action {
        DocumentAction::Create {
            template,
            version,
            file,
            allow_partial,
            artifact,
        } => {
            validate_native_save_artifact(&artifact)?;
            let mut params = json!({"template": template, "data": read_json(&file)?, "allow_partial": allow_partial});
            if let Some(v) = version {
                params["version"] = json!(v);
            }
            let mut created = client::rpc(&state_dir, "business_document_create", params)?;
            if artifact.save_artifact {
                let bytes = extract_html_bytes_from_value(&created).map_err(|_| {
                    Error::rejected(
                        "--save-artifact on document create requires an HTML template draft",
                    )
                })?;
                let title = format!(
                    "{template} v{} ({})",
                    created["version"].as_i64().unwrap_or(1),
                    created["id"].as_str().unwrap_or("draft")
                );
                created["artifact"] =
                    save_native_artifact(&state_dir, &artifact, &title, "html", &bytes)?;
            }
            created
        }
        DocumentAction::Show { id } => {
            client::rpc(&state_dir, "business_document_show", json!({"id": id}))?
        }
        DocumentAction::Export {
            id,
            out,
            format,
            artifact,
        } => {
            validate_native_save_artifact(&artifact)?;
            validate_export_target(&out, artifact.save_artifact)?;
            let format = normalize_sample_or_export_format(format.as_deref())?;
            let mut params = json!({"id": id});
            if let Some(fmt) = &format {
                params["format"] = json!(fmt);
            }
            let result = client::rpc(&state_dir, "business_document_export", params)?;
            let mut out_val = match out {
                Some(p) => write_sample_or_export(&p, &id, format.as_deref(), &result, "export")?,
                None => json!({"id": id}),
            };
            if artifact.save_artifact {
                let (kind, bytes) = if format.as_deref() == Some("pdf") {
                    (
                        "pdf",
                        decode_b64_field(
                            &result,
                            "pdf_base64",
                            "--save-artifact could not obtain PDF export bytes",
                            "export returned invalid PDF bytes",
                        )?,
                    )
                } else {
                    ("html", extract_html_bytes_from_value(&result)?)
                };
                out_val["artifact"] = save_native_artifact(
                    &state_dir,
                    &artifact,
                    &format!("Document {id}"),
                    kind,
                    &bytes,
                )?;
            }
            out_val
        }
    };
    print_json(&result);
    Ok(0)
}

#[derive(serde::Serialize)]
struct PackManifest<'a> {
    template: &'a Value,
    spec: &'a cadence_agent::business_templates::TemplateDefinition,
}

/// Validate once through the model, then materialise data into a fresh
/// private directory. Reference contents are never interpreted or executed.
pub(super) fn materialise_authoring_pack(out: &Path, mut show: Value) -> Result<Value> {
    use base64::Engine;
    use std::io::Write;

    let definition = cadence_agent::business_templates::parse_definition(show["spec"].take())?;
    let Some(pack) = definition.authoring() else {
        return Err(Error::rejected(format!(
            "template show --out materialises authoring packs, not '{}'",
            definition.kind()
        )));
    };
    let mut dir = FreshDir::create(out)?;
    let result = (|| -> Result<()> {
        dir.create_file("guidelines.md")?
            .write_all(pack.guidelines.as_bytes())
            .map_err(|e| Error::rejected(format!("cannot write guidelines: {e}")))?;
        for reference in &pack.references {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(&reference.content_base64)
                .map_err(|e| Error::rejected(format!("cannot decode reference: {e}")))?;
            dir.create_file(&reference.name)?
                .write_all(&bytes)
                .map_err(|e| Error::rejected(format!("cannot write reference: {e}")))?;
        }
        let manifest = PackManifest {
            template: &show["template"],
            spec: &definition,
        };
        serde_json::to_writer_pretty(dir.create_file("template.json")?, &manifest)
            .map_err(|e| Error::rejected(format!("cannot write template manifest: {e}")))?;
        Ok(())
    })();
    if let Err(error) = result {
        dir.abort();
        return Err(error);
    }
    let references: Vec<_> = pack.references.iter().map(|r| out.join(&r.name)).collect();
    Ok(json!({
        "template": show["template"],
        "out": out,
        "guidelines": out.join("guidelines.md"),
        "references": references,
        "template_json": out.join("template.json"),
    }))
}

fn leaf_cstring(name: &str) -> Result<CString> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.bytes().any(|b| b == 0 || b == b'/' || b == b'\\')
        || Path::new(name).file_name().and_then(|s| s.to_str()) != Some(name)
    {
        return Err(Error::rejected(format!(
            "refusing to write '{name}' outside the new directory"
        )));
    }
    CString::new(name).map_err(|_| Error::rejected("path component contains NUL"))
}

fn io_pack_err(context: &str, err: std::io::Error) -> Error {
    if err.raw_os_error() == Some(libc::ELOOP) {
        Error::rejected(format!("{context}: refusing to follow a symlink"))
    } else {
        Error::rejected(format!("{context}: {err}"))
    }
}

fn open_dir_path(path: &Path) -> Result<DirFd> {
    if CString::new(path.as_os_str().as_bytes()).is_err() {
        return Err(Error::rejected("path contains NUL"));
    }
    DirFd::open_nofollow(path)
        .map_err(|err| io_pack_err(&format!("cannot open directory {}", path.display()), err))
}

fn openat_dir(dir: &DirFd, name: &OsStr) -> Result<DirFd> {
    let c = CString::new(name.as_bytes())
        .map_err(|_| Error::rejected("path component contains NUL"))?;
    dir.open_child_dir(&c)
        .map_err(|err| io_pack_err(&format!("cannot open {}", name.to_string_lossy()), err))
}

/// Walk `path` with `O_NOFOLLOW` on every component, including `.` and `/`.
fn open_nofollow_dir(path: &Path) -> Result<DirFd> {
    use std::path::Component;
    let mut components = path.components().peekable();
    if components.peek().is_none() {
        return open_dir_path(Path::new("."));
    }
    let first = components.next().expect("peeked");
    let mut fd = match first {
        Component::RootDir => open_dir_path(Path::new("/"))?,
        Component::CurDir => open_dir_path(Path::new("."))?,
        Component::ParentDir => openat_dir(&open_dir_path(Path::new("."))?, OsStr::new(".."))?,
        Component::Normal(name) => openat_dir(&open_dir_path(Path::new("."))?, name)?,
        Component::Prefix(_) => {
            return Err(Error::rejected("template show --out path is not usable"));
        }
    };
    for component in components {
        fd = match component {
            Component::CurDir => fd,
            Component::Normal(name) => openat_dir(&fd, name)?,
            Component::ParentDir => openat_dir(&fd, OsStr::new(".."))?,
            Component::RootDir | Component::Prefix(_) => {
                return Err(Error::rejected("template show --out path is not usable"));
            }
        };
    }
    Ok(fd)
}

/// A directory this process just created. Dropping it leaves the files in
/// place; `abort` removes only the names this process created.
struct FreshDir {
    parent: DirFd,
    dir: DirFd,
    name: CString,
    files: Vec<CString>,
}

impl FreshDir {
    fn create(out: &Path) -> Result<Self> {
        let leaf = out.file_name().ok_or_else(|| {
            Error::rejected(format!(
                "template show --out needs a new directory name, not {}",
                out.display()
            ))
        })?;
        if leaf == "." || leaf == ".." {
            return Err(Error::rejected(
                "template show --out needs a new directory name",
            ));
        }
        let parent_path = match out.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent,
            _ => Path::new("."),
        };
        let parent = open_nofollow_dir(parent_path).map_err(|err| {
            if err.to_string().contains("No such file") {
                Error::rejected(format!(
                    "the parent of {} must already exist",
                    out.display()
                ))
            } else {
                err
            }
        })?;
        let name = CString::new(leaf.as_bytes())
            .map_err(|_| Error::rejected("output directory name contains NUL"))?;
        // mkdirat creates exclusively; an existing file or link gives EEXIST.
        // 0700, not 0755: the pack can hold confidential company references.
        if let Err(err) = parent.mkdir(&name, 0o700) {
            if err.raw_os_error() == Some(libc::EEXIST) {
                return Err(Error::rejected(format!(
                    "refusing to overwrite {}; template show --out requires a directory that does not exist",
                    out.display()
                )));
            }
            return Err(io_pack_err(
                &format!("cannot create {}", out.display()),
                err,
            ));
        }
        let dir = openat_dir(&parent, leaf)?;
        // mkdirat masks with the process umask. Force owner-only access.
        if let Err(err) = dir.fchmod(0o700) {
            let _ = parent.rmdir_child(&name);
            return Err(io_pack_err(
                &format!("cannot restrict permissions on {}", out.display()),
                err,
            ));
        }
        Ok(Self {
            parent,
            dir,
            name,
            files: Vec::new(),
        })
    }

    fn create_file(&mut self, name: &str) -> Result<std::fs::File> {
        let c = leaf_cstring(name)?;
        let file = match self.dir.open_file_nofollow(
            &c,
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
            0o644,
        ) {
            Ok(file) => file,
            Err(err) => {
                if err.raw_os_error() == Some(libc::EEXIST) {
                    return Err(Error::rejected(format!(
                        "refusing to overwrite {name} in the output directory"
                    )));
                }
                return Err(io_pack_err(&format!("cannot create {name}"), err));
            }
        };
        self.files.push(c);
        Ok(file)
    }

    fn abort(self) {
        for name in &self.files {
            let _ = self.dir.unlink_child(name);
        }
        let _ = self.parent.rmdir_child(&self.name);
    }
}
