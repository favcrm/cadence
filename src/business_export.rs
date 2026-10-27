//! CAD-1393: DOCX/PDF export of a retained draft through the pinned
//! business-documents converter.
//!
//! The template DOCX and the draft context are written to a private
//! temporary directory, then `skills/business-documents/scripts/run_isolated.py
//! render-template` runs them in the pinned offline container. The runner
//! refuses any image whose ID or CAD-1345 labels do not match its pin, so an
//! absent or unpinned image fails closed. Nothing here issues, numbers or
//! sends a document.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate::error::{Error, Result};
use crate::proc::{self, BoundedError};

/// CAD-1416: hosted intake and export, selected by the daemon's real lease.
pub(crate) mod hosted;

/// Wall-clock bound for one export, above the runner's own 105 s limit.
const EXPORT_TIMEOUT: Duration = Duration::from_secs(120);
/// Output cap for runner stdout/stderr that is kept for the error message.
const OUTPUT_CAP: usize = 16 * 1024;

/// The rendered DOCX and PDF bytes.
pub struct Exported {
    pub docx: Vec<u8>,
    pub pdf: Vec<u8>,
}

/// Structured extraction of one reference file (CAD-1393 intake).
pub struct Intake {
    /// The `provenance.json` record: source hash, extractor identity and
    /// evidence (text blocks or pages, placeholders, loops, tables, styles).
    pub provenance: Vec<u8>,
    /// The editable DOCX reconstruction, PDF references only.
    pub reconstruction: Option<Vec<u8>>,
}

/// Run the pinned extractor on one DOCX or PDF reference. Read-only: the
/// runner writes to a private staging directory that is then discarded.
/// Scanned, encrypted, macro or otherwise unsupported input is refused by
/// the extractor with its own message.
pub fn intake(file_name: &str, bytes: &[u8]) -> Result<Intake> {
    check_reference_name(file_name)?;
    let runner = runner_dir()?;
    let root = tempfile::Builder::new()
        .prefix("cad1393-intake-")
        .tempdir_in("/tmp")
        .map_err(|e| Error::internal(format!("cannot create intake staging: {e}")))?;
    let source = root.path().join(file_name);
    if source.parent() != Some(root.path()) {
        return Err(Error::rejected(
            "reference filename must be a safe bare DOCX or PDF name",
        ));
    }
    let out = root.path().join("out");
    std::fs::create_dir(&out).map_err(|e| Error::internal(e.to_string()))?;
    std::fs::write(&source, bytes).map_err(|e| Error::internal(e.to_string()))?;

    let mut cmd = Command::new("python3");
    cmd.arg(runner.join("run_isolated.py"))
        .arg("intake")
        .arg(&source)
        .arg("--output-dir")
        .arg(&out);
    let output = match proc::run_bounded_limited(&mut cmd, EXPORT_TIMEOUT, OUTPUT_CAP) {
        Ok((output, _)) => output,
        Err(BoundedError::TimedOut { .. }) => {
            return Err(Error::rejected(
                "reference extraction timed out and was stopped",
            ))
        }
        Err(e) => {
            return Err(Error::rejected(format!(
                "reference extraction is unavailable: {e}"
            )))
        }
    };
    if !output.status.success() {
        let why = String::from_utf8_lossy(&output.stderr);
        return Err(Error::rejected(format!(
            "reference refused: {}",
            why.trim().chars().take(400).collect::<String>()
        )));
    }
    let stem = std::path::Path::new(file_name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("source");
    let provenance = std::fs::read(out.join(format!("{stem}.provenance.json")))
        .map_err(|_| Error::rejected("reference extraction produced no provenance"))?;
    let reconstruction = std::fs::read(out.join(format!("{stem}.reconstruction.docx"))).ok();
    Ok(Intake {
        provenance,
        reconstruction,
    })
}

/// The reference name must be one bare file name: ASCII letters, digits,
/// dot, underscore or hyphen, no leading dot, ending in .docx or .pdf (any
/// case). Separators, absolute or dot-relative forms and controls fail here,
/// before any staging directory or write exists.
fn check_reference_name(file_name: &str) -> Result<()> {
    let bare = !file_name.starts_with('.')
        && file_name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    let lower = file_name.to_ascii_lowercase();
    if bare && (lower.ends_with(".docx") || lower.ends_with(".pdf")) {
        return Ok(());
    }
    Err(Error::rejected(
        "reference filename must be a safe bare DOCX or PDF name",
    ))
}

/// Locate the lane's runner. `CADENCE_BUSINESS_SCRIPTS` lets a deployment
/// point at a reviewed copy; without it the runner is taken from the
/// source tree this binary was built from.
fn runner_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("CADENCE_BUSINESS_SCRIPTS") {
        return Ok(PathBuf::from(dir));
    }
    let built = Path::new(env!("CARGO_MANIFEST_DIR")).join("skills/business-documents/scripts");
    if built.join("run_isolated.py").is_file() {
        return Ok(built);
    }
    Err(Error::rejected(
        "the business-documents converter is not available in this build",
    ))
}

/// Render `template_docx` with the draft `context` and return DOCX and PDF.
pub fn export(template_docx: &[u8], context: &serde_json::Value) -> Result<Exported> {
    render(template_docx, context, false)
}

/// Render a labelled sample preview of an approved version. The output
/// carries a banner that it is placeholder data, not a document to send.
pub fn export_sample(template_docx: &[u8], context: &serde_json::Value) -> Result<Exported> {
    render(template_docx, context, true)
}

fn render(template_docx: &[u8], context: &serde_json::Value, sample: bool) -> Result<Exported> {
    let runner = runner_dir()?;
    let root = tempfile::Builder::new()
        .prefix("cad1393-export-")
        .tempdir_in("/tmp")
        .map_err(|e| Error::internal(format!("cannot create export staging: {e}")))?;
    let template = root.path().join("template.docx");
    let context_file = root.path().join("context.json");
    let out = root.path().join("out");
    std::fs::create_dir(&out).map_err(|e| Error::internal(e.to_string()))?;
    std::fs::write(&template, template_docx).map_err(|e| Error::internal(e.to_string()))?;
    let context_bytes = serde_json::to_vec(context).map_err(|e| Error::internal(e.to_string()))?;
    std::fs::write(&context_file, context_bytes).map_err(|e| Error::internal(e.to_string()))?;

    let mut cmd = Command::new("python3");
    cmd.arg(runner.join("run_isolated.py"))
        .args(["render-template", "--template"])
        .arg(&template)
        .arg("--context")
        .arg(&context_file)
        .arg("--output-dir")
        .arg(&out);
    if sample {
        cmd.arg("--sample");
    }
    let output = match proc::run_bounded_limited(&mut cmd, EXPORT_TIMEOUT, OUTPUT_CAP) {
        Ok((output, _)) => output,
        Err(BoundedError::TimedOut { .. }) => {
            return Err(Error::rejected("document export timed out and was stopped"))
        }
        Err(e) => {
            return Err(Error::rejected(format!(
                "document export is unavailable: {e}"
            )))
        }
    };
    if !output.status.success() {
        let why = String::from_utf8_lossy(&output.stderr);
        return Err(Error::rejected(format!(
            "document export refused: {}",
            why.trim().chars().take(400).collect::<String>()
        )));
    }
    let docx = std::fs::read(out.join("document.docx"))
        .map_err(|_| Error::rejected("document export produced no DOCX"))?;
    let pdf = std::fs::read(out.join("document.pdf"))
        .map_err(|_| Error::rejected("document export produced no PDF"))?;
    Ok(Exported { docx, pdf })
}
