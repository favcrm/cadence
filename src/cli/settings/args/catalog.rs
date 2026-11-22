//! Catalog, role, profile, pin, and grant subcommands for `cadence settings`.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::path::PathBuf;

use clap::{Subcommand, ValueEnum};

use super::Mutation;

#[derive(Subcommand)]
pub(crate) enum CatalogAction {
    /// Refresh one provider's model catalog from the host (`omp
    /// models`). The result reports source, fetch time and any error —
    /// a failed fetch is never an empty successful catalog.
    Refresh { provider: String },
    /// Import catalog entries for a provider as scope `off` — discovery
    /// does not authorize a model. Nothing imports without an explicit
    /// selection: repeat `--model <id>` or pass `--all-new` for every
    /// catalog entry the document does not already have. The daemon
    /// recomputes the catalog inside the same call, so a stale client
    /// copy cannot import.
    Import {
        provider: String,
        /// Catalog model id to import; repeatable.
        #[arg(
            long = "model",
            conflicts_with = "all_new",
            required_unless_present = "all_new"
        )]
        models: Vec<String>,
        /// Import every catalog entry not already in the document.
        #[arg(long)]
        all_new: bool,
        #[command(flatten)]
        mutation: Mutation,
    },
}

#[derive(Clone, Copy, ValueEnum)]
pub(crate) enum RoleArg {
    Pm,
    Research,
    Architect,
    Dev,
    Qa,
    Devops,
    Worker,
}

impl RoleArg {
    pub(in super::super) fn as_str(self) -> &'static str {
        match self {
            Self::Pm => "pm",
            Self::Research => "research",
            Self::Architect => "architect",
            Self::Dev => "dev",
            Self::Qa => "qa",
            Self::Devops => "devops",
            Self::Worker => "worker",
        }
    }
}

#[derive(Subcommand)]
pub(crate) enum RoleAction {
    /// Configured and effective model selection per team role.
    #[command(visible_alias = "ls")]
    List,
    /// Set one team role's default model.
    Set {
        #[arg(value_enum)]
        role: RoleArg,
        model: String,
        #[command(flatten)]
        mutation: Mutation,
    },
    /// Clear one team role's default (falls back to the provider
    /// default).
    Clear {
        #[arg(value_enum)]
        role: RoleArg,
        #[command(flatten)]
        mutation: Mutation,
    },
}

#[derive(Subcommand)]
pub(crate) enum ProfileAction {
    /// Every saved profile with version, usedBy, history and generated
    /// export-file state.
    #[command(visible_alias = "ls")]
    List,
    /// One profile: fields plus version, usedBy, history, resolved model
    /// and per-harness export-file state.
    Show { id: String },
    /// Create or update a profile. `--model inherit` selects at launch;
    /// the job document comes from `--job` or `--job-file` (no shell
    /// quoting of a multiline file). `--digest` binds the payload bytes.
    Put {
        id: String,
        #[arg(long, value_enum)]
        role: Option<RoleArg>,
        /// Model id, or `inherit` (the default for a new profile).
        #[arg(long)]
        model: Option<String>,
        /// Inline job text.
        #[arg(long, conflicts_with = "job_file")]
        job: Option<String>,
        /// Job document file; `-` (or a piped stdin) reads stdin.
        #[arg(long, value_name = "FILE")]
        job_file: Option<PathBuf>,
        /// sha256:<hex> of the canonical payload — refuse before send
        /// when the assembled payload does not match.
        #[arg(long, value_name = "sha256:HEX")]
        digest: Option<String>,
        #[command(flatten)]
        mutation: Mutation,
    },
    /// Rename a profile atomically — references move with it.
    Rename {
        id: String,
        to: String,
        #[command(flatten)]
        mutation: Mutation,
    },
    /// Remove a profile. Refused while an active agent references it.
    Remove {
        id: String,
        #[command(flatten)]
        mutation: Mutation,
    },
    /// Regenerate Pi/Claude export files from the saved revision. One
    /// profile id or `--all` (the default); unmanaged files are
    /// reported, never overwritten. Saving does not sync.
    Sync {
        /// Sync only this profile.
        profile: Option<String>,
        /// Sync every profile (the default when no id is given).
        #[arg(long, conflicts_with = "profile")]
        all: bool,
        #[command(flatten)]
        mutation: Mutation,
    },
    /// Preview a saved profile on a harness: the command, resolved model
    /// and its source, `allowed` and every refusal reason — without
    /// starting anything. Preview is not authentication or launch proof.
    Preview {
        profile: String,
        /// Harness id the profile would launch under.
        #[arg(long)]
        harness: String,
        #[command(flatten)]
        mutation: Mutation,
    },
}

#[derive(Subcommand)]
pub(crate) enum PinAction {
    /// Every extension pin, including provider pin metadata.
    #[command(visible_alias = "ls")]
    List,
    /// Pin a package to a version (`name@version` validation is
    /// daemon-side). Never installs anything.
    Put {
        pkg: String,
        /// Pinned version spec.
        #[arg(long)]
        pinned: String,
        #[command(flatten)]
        mutation: Mutation,
    },
    /// Remove a pin.
    Remove {
        pkg: String,
        #[command(flatten)]
        mutation: Mutation,
    },
}

#[derive(Subcommand)]
pub(crate) enum GrantAction {
    /// Request operator approval for one exact settings operation.
    /// `--payload`/`--payload-file` is the ENTIRE intended operation
    /// payload verbatim (for example `{"id":"<model>","scope":"allow"}`)
    /// — the operator's allow binds those exact bytes, so a different
    /// field, a replaced file's content or a moved revision refuses on
    /// use. Prints the request id to hand the operator.
    Request {
        /// Operation to run once granted (for example `model.scope`).
        operation: String,
        /// Intended payload as a JSON object.
        #[arg(long, conflicts_with = "payload_file")]
        payload: Option<String>,
        /// Intended payload JSON from a file; `-` reads stdin.
        #[arg(long, value_name = "FILE")]
        payload_file: Option<PathBuf>,
        /// Revision the eventual mutation will require (defaults to the
        /// revision current when the operator decides).
        #[arg(long, value_name = "REV")]
        expected_revision: Option<String>,
        /// Why the change is needed — shown to the operator.
        #[arg(long)]
        reason: String,
    },
    /// Pending requests and live settings-scoped grants (an agent sees
    /// its own rows; the operator sees all).
    #[command(visible_alias = "ls")]
    List,
    /// Approve one pending request (operator only) — binds the exact
    /// stored payload, its revision, the requester identity and org.
    Allow {
        request_id: String,
        /// Current revision token from `show`.
        #[arg(long, value_name = "REV")]
        expected_revision: String,
    },
    /// Reject one pending request (operator only).
    Reject {
        request_id: String,
        /// Current revision token from `show`.
        #[arg(long, value_name = "REV")]
        expected_revision: String,
    },
    /// Revoke one live grant by id (operator only); it stops matching
    /// immediately.
    Revoke { grant_id: String },
}
