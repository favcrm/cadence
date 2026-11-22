//! Clap CLI argument and subcommand definitions for `cadence settings`.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::wildcard_imports, clippy::unwrap_used))]

use std::path::PathBuf;

use clap::{Args, Subcommand, ValueEnum};

#[derive(Subcommand)]
pub(crate) enum SettingsAction {
    /// defaults, profiles, pins, history and settings-scoped grants.
    /// Every mutation takes `--expected-revision` from `show`.
    Agents {
        /// Settings-scoped grant id authorizing exactly this operation
        /// and payload (`settings agents grant list`) — accepted on reads
        /// and mutations alike; the daemon strips it before binding the
        /// payload bytes.
        #[arg(long, global = true, value_name = "ID")]
        grant: Option<String>,
        #[command(subcommand)]
        action: AgentsAction,
    },
}

#[derive(Subcommand)]
pub(crate) enum AgentsAction {
    /// The full authoritative document: revision, config, observations,
    /// catalogs and history. Machine-readable JSON.
    Show,
    /// Every registered gated agent evaluated against this authority — who
    /// would be refused at launch and why, with the effective policy. Read
    /// only: nothing is launched, mutated or written. Use it before saving a
    /// policy edit, so a change cannot silently strand the fleet.
    Fleet,
    /// Replace the whole document atomically from a JSON file (`--file
    /// -` or a pipe reads stdin). The file is the same `config` object
    /// `show` prints — harnesses/providers/models/profiles/scope/
    /// roleDefaults/pins exactly. Binds the file's bytes: `--digest`
    /// must equal sha256 of the canonical payload, and a settings grant
    /// binds those bytes daemon-side, so a replaced file is refused.
    Apply {
        /// JSON document file; `-` (or a piped stdin) reads stdin.
        #[arg(long, value_name = "FILE")]
        file: Option<PathBuf>,
        #[command(flatten)]
        mutation: Mutation,
        /// sha256:<hex> of the canonical payload — refuse before send
        /// when the file's content does not match.
        #[arg(long, value_name = "sha256:HEX")]
        digest: Option<String>,
    },
    /// Manage launch harness rows (`id`, `label`, adapter `cli`,
    /// `note`). Installed/version/live are host observations, never
    /// writable flags.
    Harness {
        #[command(subcommand)]
        action: HarnessAction,
    },
    /// Manage inference provider rows (`id`, `label`, `kind`, `pin`,
    /// `gate`, `paid`, `note`) — settings entries, not Connections
    /// accounts.
    Provider {
        #[command(subcommand)]
        action: ProviderAction,
    },
    /// Manage model rows and their launch scope. `thinking`/`images`
    /// catalog facts stay unknown (null) unless explicitly set.
    Model {
        #[command(subcommand)]
        action: ModelAction,
    },
    /// Provider model catalogs: refresh from the host catalog and
    /// import entries as scope `off`. A failed refresh stays visible;
    /// importing never authorizes a model.
    Catalog {
        #[command(subcommand)]
        action: CatalogAction,
    },
    /// Team-role default models (pm research architect dev qa devops
    /// worker) — selection defaults, not runtime identity.
    Role {
        #[command(subcommand)]
        action: RoleAction,
    },
    /// Saved launch profiles (`id`, `role`, `model`/`inherit`, `job`)
    /// plus export sync and launch preview.
    Profile {
        #[command(subcommand)]
        action: ProfileAction,
    },
    /// Pi extension pins (`pkg`, `pinned`). Editing a pin never
    /// installs a package.
    Pin {
        #[command(subcommand)]
        action: PinAction,
    },
    /// Available settings/profile history — recorded saves, not
    /// fabricated per-field diffs.
    History,
    /// Settings-scoped authority for a designated management agent:
    /// request an exact operation, the operator allows or rejects it,
    /// and the bound mutation carries `--grant`. Works while the master
    /// is down; ordinary workers and app-scoped callers are refused by
    /// the daemon.
    Grant {
        #[command(subcommand)]
        action: GrantAction,
    },
}

#[derive(Subcommand)]
pub(crate) enum HarnessAction {
    /// Every harness row with its host observations (installed, version,
    /// live agents).
    #[command(visible_alias = "ls")]
    List,
    /// One harness row with its host observations.
    Show { id: String },
    /// Create or update a harness. On update, unpassed fields keep their
    /// stored values; `cli` must stay a registered adapter program.
    Put {
        id: String,
        #[arg(long)]
        label: Option<String>,
        /// Registered adapter program name, never a path.
        #[arg(long)]
        cli: Option<String>,
        #[arg(long)]
        note: Option<String>,
        #[command(flatten)]
        mutation: Mutation,
    },
    /// Remove a harness. Refused while an active agent uses it.
    Remove {
        id: String,
        #[command(flatten)]
        mutation: Mutation,
    },
}

#[derive(Clone, Copy, ValueEnum)]
pub(crate) enum ProviderKindArg {
    /// Provider models ride the Pi allow/gate policy.
    PiExtension,
    /// Provider is a native agent CLI; its models do not use the Pi gate.
    NativeCli,
}

impl ProviderKindArg {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::PiExtension => "Pi extension",
            Self::NativeCli => "native CLI",
        }
    }
}

#[derive(Subcommand)]
pub(crate) enum ProviderAction {
    /// Every provider row.
    #[command(visible_alias = "ls")]
    List,
    /// One provider row.
    Show { id: String },
    /// Create or update a provider. `kind` is `pi-extension` or
    /// `native-cli`; `gate` follows the kind unless explicitly set.
    Put {
        id: String,
        #[arg(long)]
        label: Option<String>,
        #[arg(long, value_enum)]
        kind: Option<ProviderKindArg>,
        /// Pin metadata for the provider (`name@version` or text).
        #[arg(long)]
        pin: Option<String>,
        /// Route this provider's models through the Pi allow gate
        /// (default on for `pi-extension`, off for `native-cli`).
        #[arg(long, conflicts_with = "no_gate")]
        gate: bool,
        #[arg(long)]
        no_gate: bool,
        /// Mark the provider as paid/billable.
        #[arg(long, conflicts_with = "free")]
        paid: bool,
        /// Mark the provider as free.
        #[arg(long)]
        free: bool,
        #[arg(long)]
        note: Option<String>,
        #[command(flatten)]
        mutation: Mutation,
    },
    /// Remove a provider. Refused while its models are referenced or an
    /// active agent uses it.
    Remove {
        id: String,
        #[command(flatten)]
        mutation: Mutation,
    },
}

#[derive(Subcommand)]
pub(crate) enum ModelAction {
    /// Every model row with its launch scope, live count and the role
    /// defaults selecting it.
    #[command(visible_alias = "ls")]
    List,
    /// One model row: id, provider, ctx/out, thinking/images facts
    /// (null = unknown, never false), scope, live count, role defaults.
    Show { id: String },
    /// Create or update a model. Unknown capability facts stay unknown:
    /// pass neither `--thinking`/`--no-thinking` nor
    /// `--images`/`--no-images` to leave a fact unset.
    Put {
        id: String,
        /// Provider id the model belongs to.
        #[arg(long)]
        provider: Option<String>,
        /// Context window label (for example `200k`).
        #[arg(long)]
        ctx: Option<String>,
        /// Output token label.
        #[arg(long)]
        out: Option<String>,
        /// The model is known to support thinking/reasoning.
        #[arg(long, conflicts_with = "no_thinking")]
        thinking: bool,
        /// The model is known NOT to support thinking.
        #[arg(long)]
        no_thinking: bool,
        /// The model is known to accept image input.
        #[arg(long, conflicts_with = "no_images")]
        images: bool,
        /// The model is known NOT to accept image input.
        #[arg(long)]
        no_images: bool,
        /// Also set the launch scope in the same transaction (gated
        /// providers only) — a convenience for one-shot provisioning;
        /// `model scope` does the same alone.
        #[arg(long, value_enum)]
        scope: Option<ScopeArg>,
        #[command(flatten)]
        mutation: Mutation,
    },
    /// Rename a model atomically — scope, profile and role-default
    /// references move with it in the same transaction.
    Rename {
        id: String,
        to: String,
        #[command(flatten)]
        mutation: Mutation,
    },
    /// Remove a model. Refused while a profile or role default
    /// references it or an active agent runs it.
    Remove {
        id: String,
        #[command(flatten)]
        mutation: Mutation,
    },
    /// Set a gated provider model's launch scope: `off`, `allow` (any
    /// use), `master`, or `worker`. Native-CLI provider models have no
    /// gate and refuse.
    Scope {
        id: String,
        #[arg(value_enum)]
        scope: ScopeArg,
        #[command(flatten)]
        mutation: Mutation,
    },
}

#[derive(Clone, Copy, ValueEnum)]
pub(crate) enum ScopeArg {
    Off,
    Allow,
    Master,
    Worker,
}

impl ScopeArg {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Allow => "allow",
            Self::Master => "master",
            Self::Worker => "worker",
        }
    }
}

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
    pub(super) fn as_str(self) -> &'static str {
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

/// Shared flag of every document mutation: the observed revision token.
/// Grant authority is the parent's global `--grant`, not a per-mutation
/// flag — a grant binds an operation, so it belongs on every verb.
#[derive(Args, Clone)]
pub(crate) struct Mutation {
    /// Revision token from `show` (`.revision`, `N:hex`). Refused with a
    /// conflict when the document moved — no silent retry.
    #[arg(long, value_name = "REV")]
    pub(super) expected_revision: String,
}
