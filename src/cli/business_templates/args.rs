//! Clap CLI argument and subcommand definitions for `cadence template` and `cadence document`.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::path::PathBuf;

use clap::Subcommand;

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
