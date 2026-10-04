//! CAD-535: `cadence app` — moved verbatim from src/main.rs.

use super::*;

/// `cadence app` verbs (CAD-547). `install`/`update`/`set`/`remove`
/// write the tracker directly — one commit each, `Actor:` recorded —
/// `ls`/`show` read it; `approve` is the operator's daemon gate, like
/// `workflow approve`.
#[derive(Subcommand)]
pub(crate) enum AppAction {
    /// Configure exact publication connections for an installation or context.
    Binding {
        #[command(subcommand)]
        action: BindingAction,
    },
    /// Stage and explicitly approve release of an independently reviewed artifact.
    Effect {
        #[command(subcommand)]
        action: EffectAction,
    },
    /// Optional app-owned content settings; creates no worker or provider authority.
    Context {
        #[command(subcommand)]
        action: ContextAction,
    },
    /// Host-managed per-installation customer records with scoped revisions.
    Record {
        #[command(subcommand)]
        action: RecordAction,
    },
    /// Saved segments, exclusion lists, suppressions and frozen
    /// audiences over customer records (CAD-780). No mail is sent.
    Audience {
        #[command(subcommand)]
        action: AudienceAction,
    },
    /// Campaign email content — the scoped-chat inert assistant draft
    /// (CAD-1014): drafts a pending proposal the operator applies or
    /// discards. Never edits, approves or sends.
    Content {
        #[command(subcommand)]
        action: ContentAction,
    },
    /// Stable workspace installation IDs; execution and approval are separate.
    Catalog {
        #[command(subcommand)]
        action: CatalogAction,
    },
    /// App-owned local runs with separate execution approval.
    Run {
        #[command(subcommand)]
        action: RunAction,
    },
    /// Run a fixture-only HMR preview from an explicitly trusted local
    /// Cadence source checkout. This executes that checkout's known
    /// development harness, not an installed app or its manifest.
    Dev {
        /// Preview directory name under app-previews/.
        name: String,
        /// Trusted source checkout containing scripts/app-dev.mjs.
        #[arg(long)]
        source: PathBuf,
        #[arg(long, default_value_t = 3186, value_parser = clap::value_parser!(u16).range(3110..=3199))]
        port: u16,
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        /// Existing private tailnet hostname; required when sharing.
        #[arg(long)]
        allow_host: Vec<String>,
    },
    /// Install an app folder into the project: `app.md` (frontmatter
    /// `app`, `title`, `version`, `needs.connections`) plus
    /// `workflows/*.md` — every one checked like `workflow check` —
    /// and optional flat `rubrics/`, `templates/` dirs. `<source>` is a
    /// local path or a git URL (cloned, pinned to the commit SHA in the
    /// install record). Nothing in the bundle executes, and nothing in
    /// it runs until `cadence app approve`.
    Install {
        /// App source — a folder path or a git URL.
        source: String,
        /// Project key the app installs into.
        #[arg(long)]
        project: String,
    },
    /// Every installed app: project, name, version, workflows, slots
    /// with bindings, digest and approval state.
    #[command(visible_alias = "list")]
    Ls {
        /// Project key; all projects when absent.
        #[arg(long)]
        project: Option<String>,
    },
    /// An installed app's manifest, guide, workflow summaries, slot
    /// bindings, install record, digest and approval state.
    Show {
        /// App name — the `apps/<name>/` folder.
        name: String,
        /// Project key.
        #[arg(long)]
        project: String,
    },
    /// Bind a declared `needs.connections` slot to a connection name —
    /// `<slot>=<connection>`, repeatable; `<slot>=` unbinds it
    /// explicitly. Slots default to `local`. A binding change is
    /// structural — it re-gates the app until `app approve`.
    Set {
        /// App name.
        name: String,
        /// `<slot>=<connection>` pairs — repeatable.
        bindings: Vec<String>,
        /// Project key.
        #[arg(long)]
        project: String,
    },
    /// Replace an installed app from its recorded source (or the given
    /// one), after the same checks `install` runs — prints the diff.
    /// Any structural change re-gates the app until `app approve`.
    Update {
        /// App name.
        name: String,
        /// A folder path or git URL; absent re-reads the recorded
        /// source.
        source: Option<String>,
        /// Project key.
        #[arg(long)]
        project: String,
    },
    /// Remove an installed app — the folder and its install record, one
    /// commit. Refuses while a plan proposed from the app is open.
    Remove {
        /// App name.
        name: String,
        /// Project key.
        #[arg(long)]
        project: String,
    },
    /// Approve the app's current structure — `plan propose
    /// --workflow <app>/<wf>` refuses it until this matches the
    /// installed folder. The approval also derives the app's grants:
    /// exactly the scopes its workflow steps declare on their bound
    /// slots, to the agents the app's default team assigns those steps
    /// (CAD-577). Operator only, through the daemon.
    Approve {
        /// App name.
        name: String,
        /// Project key.
        #[arg(long)]
        project: String,
    },
    /// Withdraw the app's approval (CAD-577) — `plan propose
    /// --workflow <app>/<wf>` refuses again, and every grant the
    /// approval derived is revoked (a waiting effect that loses a
    /// scope is closed). The counterpart to `approve`; operator only,
    /// through the daemon.
    Revoke {
        /// App name.
        name: String,
        /// Project key.
        #[arg(long)]
        project: String,
    },
    /// Record the app's default team (CAD-577): one agent alias per
    /// workflow input role, `<input>=<agent>` repeatable; `<input>=`
    /// clears a role. The team lives with the install record and is
    /// not part of the gate digest, so setting it never re-requires
    /// approval. Operator only, through the daemon.
    SetTeam {
        /// App name.
        name: String,
        /// `<input>=<agent>` pairs — repeatable.
        #[arg(long = "role", required = true)]
        roles: Vec<String>,
        /// Project key.
        #[arg(long)]
        project: String,
    },
    /// Join a new Devin worker for one of the app's team roles (CAD-577)
    /// — the board's "Add worker": a unique role-prefixed alias, under
    /// the operator (a group root), recorded in the app's default team.
    /// Operator only, through the daemon.
    AddWorker {
        /// App name.
        name: String,
        /// The team role the worker fills.
        #[arg(long)]
        role: String,
        /// Project key.
        #[arg(long)]
        project: String,
    },
}

#[derive(Subcommand)]
pub(crate) enum ContextAction {
    /// Create an optional context using explicitly eligible content defaults.
    Create {
        install_id: String,
        #[arg(long)]
        label: String,
        /// Bounded JSON object of content defaults; empty when omitted.
        #[arg(long)]
        defaults: Option<PathBuf>,
        #[arg(long)]
        request_id: String,
    },
    #[command(visible_alias = "list")]
    Ls { install_id: String },
    Show {
        install_id: String,
        context_id: String,
    },
    /// Replace label and defaults at the exact observed revision.
    Set {
        install_id: String,
        context_id: String,
        #[arg(long)]
        expected_revision: u64,
        #[arg(long)]
        label: String,
        #[arg(long)]
        defaults: PathBuf,
    },
    /// Close unfinished eligibility while retaining historical audit.
    Archive {
        install_id: String,
        context_id: String,
        #[arg(long)]
        expected_revision: u64,
    },
}

#[derive(Subcommand)]
pub(crate) enum RecordAction {
    /// Create a customer record in an exact installation context.
    Create {
        install_id: String,
        #[arg(long)]
        context_id: String,
        #[arg(long)]
        record_id: String,
        /// JSON file holding the customer profile object.
        #[arg(long)]
        profile: PathBuf,
    },
    /// List customer records in an exact installation context.
    #[command(visible_alias = "list")]
    Ls {
        install_id: String,
        #[arg(long)]
        context_id: String,
    },
    /// Inspect one exact customer record.
    Show {
        install_id: String,
        #[arg(long)]
        context_id: String,
        #[arg(long)]
        record_id: String,
    },
    /// Replace the profile at the exact observed revision.
    Set {
        install_id: String,
        #[arg(long)]
        context_id: String,
        #[arg(long)]
        record_id: String,
        #[arg(long)]
        expected_revision: u64,
        /// JSON file holding the customer profile object.
        #[arg(long)]
        profile: PathBuf,
        /// How consent was given (in_person, web_form, written,
        /// imported, other); required when the change grants consent.
        #[arg(long)]
        consent_method: Option<String>,
        /// Optional consent note, at most 280 characters.
        #[arg(long, requires = "consent_method")]
        consent_note: Option<String>,
    },
    /// Preview bounded CSV text as per-row create/update/skip/error
    /// decisions without mutating anything; prints the preview token
    /// that binds the exact bytes for `csv-import`.
    CsvPreview {
        install_id: String,
        #[arg(long)]
        context_id: String,
        /// CSV file (bounded to 256KiB, 500 rows).
        #[arg(long)]
        csv: PathBuf,
    },
    /// Explicitly import previewed CSV text at a request id: retries
    /// with the same request id replay the stored receipt.
    CsvImport {
        install_id: String,
        #[arg(long)]
        context_id: String,
        /// CSV file holding the exact previewed bytes.
        #[arg(long)]
        csv: PathBuf,
        /// Preview token from `csv-preview` over the same bytes.
        #[arg(long)]
        preview_token: String,
        /// Idempotency key; reuse with different bytes is refused.
        #[arg(long)]
        request_id: String,
        /// Optional JSON array of `{row, action, expected_revision?}`
        /// decisions overriding the preview plan.
        #[arg(long)]
        decisions: Option<PathBuf>,
    },
    /// CAD-1014(b): a scoped chat turn's delegated CSV import —
    /// HANDLE-ONLY. The durable host plan (bytes + decisions the
    /// operator confirmed) resolves server-side from `request_id` +
    /// `confirm_token`; the agent never carries the CSV bytes, a preview
    /// token or a decision set — those ride the operator's `csv-confirm`,
    /// never the ≤48KB chat message. The caller must be the live
    /// assigned agent on `--message`/`--token`.
    CsvAssistantImport {
        install_id: String,
        #[arg(long)]
        context_id: String,
        /// Idempotency key; names the confirmed plan the agent redeems.
        #[arg(long)]
        request_id: String,
        /// The host-minted confirm receipt nonce from `csv-confirm`
        /// (the operator's explicit confirm of this exact plan).
        #[arg(long)]
        confirm_token: String,
        /// The scoped chat message the operator sent (turn identity).
        #[arg(long, requires = "token")]
        message: String,
        /// The live turn token for that message.
        #[arg(long, requires = "message")]
        token: String,
    },
    /// CAD-1014: the operator's explicit, host-side confirm of an exact
    /// previewed CSV import plan — mints the one-use confirm receipt the
    /// assistant import redeems AND stores the durable plan (csv_text +
    /// decisions) the agent resolves by request id + nonce. Prints the
    /// `confirm_token` nonce.
    CsvConfirm {
        install_id: String,
        #[arg(long)]
        context_id: String,
        /// CSV file holding the exact confirmed bytes (the durable plan).
        #[arg(long)]
        csv: PathBuf,
        /// Preview token from `csv-preview` over the confirmed bytes.
        #[arg(long)]
        preview_token: String,
        /// Idempotency key the later import must reuse.
        #[arg(long)]
        request_id: String,
        /// JSON array of `{row, action, expected_revision?}` decisions
        /// whose digest the confirm binds (omit for the default plan).
        #[arg(long)]
        decisions: Option<PathBuf>,
    },
    /// CAD-1014: read-only CSV preview on the live scoped chat turn —
    /// the agent's read of the stamped context. Writes nothing, claims
    /// nothing; the `--message`/`--token` are the turn identity.
    CsvAssistantPreview {
        install_id: String,
        #[arg(long)]
        context_id: String,
        /// CSV file (bounded to 256KiB, 500 rows).
        #[arg(long)]
        csv: PathBuf,
        #[arg(long, requires = "token")]
        message: String,
        #[arg(long, requires = "message")]
        token: String,
    },
}

/// CAD-1014 scoped-chat email draft — the agent drafts an inert
/// `pending`/`assistant-receipt` proposal on its live scoped turn.
/// No manual mint, no request id: the verified turn IS the request.
/// `blocks`/`subject`/`preheader` come from a JSON draft file.
#[derive(Subcommand)]
pub(crate) enum ContentAction {
    /// Draft a campaign email on the live scoped chat turn (inert
    /// pending proposal; first draft or a replacement revision).
    #[command(name = "assistant-draft")]
    Draft {
        install_id: String,
        #[arg(long)]
        context_id: String,
        #[arg(long)]
        campaign_id: String,
        #[arg(long)]
        proposal_id: String,
        /// JSON file holding `{subject, preheader, blocks}`.
        #[arg(long)]
        draft: PathBuf,
        /// The scoped chat message the operator sent (turn identity).
        #[arg(long, requires = "token")]
        message: String,
        /// The live turn token for that message.
        #[arg(long, requires = "message")]
        token: String,
    },
    /// List the context's email proposals (optionally one campaign) on
    /// the live scoped turn — the agent's inert pending draft must be
    /// discoverable before the operator applies it.
    #[command(name = "assistant-proposals")]
    Proposals {
        install_id: String,
        #[arg(long)]
        context_id: String,
        #[arg(long)]
        campaign_id: Option<String>,
        #[arg(long, requires = "token")]
        message: String,
        #[arg(long, requires = "message")]
        token: String,
    },
    /// Show one proposal on the live scoped turn (read-only).
    #[command(name = "assistant-proposal-show")]
    ProposalShow {
        install_id: String,
        #[arg(long)]
        context_id: String,
        #[arg(long)]
        proposal_id: String,
        #[arg(long, requires = "token")]
        message: String,
        #[arg(long, requires = "message")]
        token: String,
    },
}

/// CAD-780 audience verbs. JSON files hold predicates (`[{field,
/// op, value}]`), member ID arrays, suppression targets and base
/// objects (`{mode, ...}`); the daemon's allowlisted grammar
/// validates every value.
#[derive(Subcommand)]
pub(crate) enum AudienceAction {
    /// Save a segment (create, or update at the observed revision).
    SegmentSave {
        install_id: String,
        #[arg(long)]
        context_id: String,
        #[arg(long)]
        segment_id: String,
        #[arg(long)]
        name: String,
        /// JSON file holding the predicates array.
        #[arg(long)]
        predicates: PathBuf,
        /// Observed revision; absent creates.
        #[arg(long)]
        expected_revision: Option<u64>,
    },
    /// CAD-1014(b): a scoped chat turn's delegated segment save. The
    /// operator's own stamped scoped chat message is the intent.
    SegmentAssistantSave {
        install_id: String,
        #[arg(long)]
        context_id: String,
        #[arg(long)]
        segment_id: String,
        #[arg(long)]
        name: String,
        /// JSON file holding the predicates array.
        #[arg(long)]
        predicates: PathBuf,
        /// Observed revision; absent creates.
        #[arg(long)]
        expected_revision: Option<u64>,
        /// The scoped chat message the operator sent (turn identity).
        #[arg(long, requires = "token")]
        message: String,
        /// The live turn token for that message.
        #[arg(long, requires = "message")]
        token: String,
    },
    /// CAD-1014: list the stamped context's segments on the live scoped
    /// chat turn (read-only — revision/membership for the agent).
    SegmentAssistantLs {
        install_id: String,
        #[arg(long)]
        context_id: String,
        #[arg(long, requires = "token")]
        message: String,
        #[arg(long, requires = "message")]
        token: String,
    },
    /// CAD-1014: show one segment (revision/membership) on the live
    /// scoped chat turn (read-only).
    SegmentAssistantShow {
        install_id: String,
        #[arg(long)]
        context_id: String,
        #[arg(long)]
        segment_id: String,
        #[arg(long, requires = "token")]
        message: String,
        #[arg(long, requires = "message")]
        token: String,
    },
    /// CAD-1014: a bounded membership preview over a saved segment
    /// (base/exclusion/final counts + a bounded sample — never the full
    /// member list, never a freeze or send) on the live scoped turn.
    SegmentAssistantPreview {
        install_id: String,
        #[arg(long)]
        context_id: String,
        #[arg(long)]
        segment_id: String,
        #[arg(long, requires = "token")]
        message: String,
        #[arg(long, requires = "message")]
        token: String,
    },
    /// Inspect one exact saved segment.
    SegmentShow {
        install_id: String,
        #[arg(long)]
        context_id: String,
        #[arg(long)]
        segment_id: String,
    },
    /// Every saved segment in an exact installation context.
    SegmentLs {
        install_id: String,
        #[arg(long)]
        context_id: String,
    },
    /// Save an exclusion list (create, or update at the observed
    /// revision).
    ExclusionSave {
        install_id: String,
        #[arg(long)]
        context_id: String,
        #[arg(long)]
        list_id: String,
        #[arg(long)]
        name: String,
        /// JSON file holding the member ID array.
        #[arg(long)]
        members: PathBuf,
        /// Observed revision; absent creates.
        #[arg(long)]
        expected_revision: Option<u64>,
    },
    /// Inspect one exact exclusion list.
    ExclusionShow {
        install_id: String,
        #[arg(long)]
        context_id: String,
        #[arg(long)]
        list_id: String,
    },
    /// Every exclusion list in an exact installation context.
    ExclusionLs {
        install_id: String,
        #[arg(long)]
        context_id: String,
    },
    /// Suppress one address or customer ID with a reason.
    SuppressionAdd {
        install_id: String,
        #[arg(long)]
        context_id: String,
        #[arg(long)]
        email: Option<String>,
        #[arg(long)]
        customer_id: Option<String>,
        #[arg(long)]
        reason: String,
    },
    /// Lift one suppression.
    SuppressionRemove {
        install_id: String,
        #[arg(long)]
        context_id: String,
        #[arg(long)]
        email: Option<String>,
        #[arg(long)]
        customer_id: Option<String>,
    },
    /// Every suppression in an exact installation context.
    SuppressionLs {
        install_id: String,
        #[arg(long)]
        context_id: String,
    },
    /// Exact inclusion/exclusion/final counts plus a bounded sample
    /// for one base mode; writes nothing.
    Preview {
        install_id: String,
        #[arg(long)]
        context_id: String,
        /// JSON file holding the base object.
        #[arg(long)]
        base: PathBuf,
        /// Saved exclusion list applied to any base.
        #[arg(long)]
        exclusion_list_id: Option<String>,
    },
    /// Freeze final membership, digest, scope and revision pins
    /// under a recipient ceiling.
    Prepare {
        install_id: String,
        #[arg(long)]
        context_id: String,
        #[arg(long)]
        freeze_id: String,
        /// JSON file holding the base object.
        #[arg(long)]
        base: PathBuf,
        #[arg(long)]
        exclusion_list_id: Option<String>,
        #[arg(long)]
        max_recipients: u64,
    },
    /// A frozen audience with its live validity.
    FreezeShow {
        install_id: String,
        #[arg(long)]
        context_id: String,
        #[arg(long)]
        freeze_id: String,
    },
}

#[derive(Subcommand)]
pub(crate) enum BindingAction {
    /// Bind one declared capability slot to an exact connection ID.
    Create {
        install_id: String,
        #[arg(long)]
        context_id: Option<String>,
        #[arg(long)]
        slot: String,
        #[arg(long)]
        connection_id: String,
        #[arg(long)]
        request_id: String,
    },
    /// List bindings, optionally for an exact context.
    #[command(visible_alias = "list")]
    Ls {
        install_id: String,
        #[arg(long)]
        context_id: Option<String>,
    },
    /// Inspect one installation's exact binding.
    Show {
        install_id: String,
        binding_id: String,
    },
    /// Change the connection at the expected binding revision.
    Set {
        install_id: String,
        binding_id: String,
        #[arg(long)]
        expected_revision: u64,
        #[arg(long)]
        connection_id: String,
    },
    /// CAD-1123: record where this binding publishes (destination, label,
    /// toolkit, timezone and send grant) once, at the expected revision.
    Publish {
        install_id: String,
        binding_id: String,
        #[arg(long)]
        expected_revision: u64,
        #[arg(long)]
        destination_id: String,
        #[arg(long)]
        destination_label: String,
        #[arg(long)]
        toolkit: String,
        #[arg(long)]
        timezone: String,
        #[arg(long)]
        grant_id: String,
    },
    /// Revoke a binding at its expected revision.
    Revoke {
        install_id: String,
        binding_id: String,
        #[arg(long)]
        expected_revision: u64,
    },
}
#[derive(Clone, Copy, clap::ValueEnum)]
pub(crate) enum ResolutionChoice {
    Close,
    Acknowledge,
}
impl ResolutionChoice {
    fn as_str(self) -> &'static str {
        match self {
            Self::Close => "close",
            Self::Acknowledge => "acknowledge",
        }
    }
}
#[derive(Subcommand)]
pub(crate) enum EffectAction {
    /// Close an uncertain receipt or acknowledge a flagged terminal outcome.
    /// Resolution retains history and never executes a release.
    Resolve {
        effect_id: String,
        #[arg(long)]
        digest: String,
        #[arg(long, value_enum)]
        resolution: ResolutionChoice,
    },
    /// Stage stored accepted artifact bytes; no caller content or path is accepted.
    Stage {
        run_id: String,
        #[arg(long)]
        artifact_id: String,
        #[arg(long)]
        slot: String,
        #[arg(long)]
        request_id: String,
        #[arg(long)]
        title: String,
    },
    /// Inspect the complete staged effect and approval digest.
    Show { effect_id: String },
    /// List release receipts; context filtering requires an installation.
    #[command(visible_alias = "list")]
    Ls {
        #[arg(long)]
        install_id: Option<String>,
        #[arg(long, requires = "install_id")]
        context_id: Option<String>,
    },
    /// Approve the exact staged digest for one outward release.
    Accept {
        effect_id: String,
        #[arg(long)]
        digest: String,
    },
    /// Decline the exact staged digest without publishing.
    Decline {
        effect_id: String,
        #[arg(long)]
        digest: String,
    },
}
fn read_record_csv(path: &Path) -> Result<String> {
    use std::io::Read;
    const MAX_CSV_BYTES: usize = 256 * 1024;
    let file = std::fs::File::open(path)
        .map_err(|e| Error::invalid("app_record_csv", format!("cannot open CSV: {e}")))?;
    let mut bytes = Vec::new();
    file.take((MAX_CSV_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| Error::invalid("app_record_csv", format!("cannot read CSV: {e}")))?;
    if bytes.len() > MAX_CSV_BYTES {
        return Err(Error::invalid("app_record_csv", "CSV exceeds 256KiB"));
    }
    String::from_utf8(bytes).map_err(|_| Error::invalid("app_record_csv", "CSV must be UTF-8 text"))
}

fn read_csv_decisions(path: &Path) -> Result<serde_json::Value> {
    use std::io::Read;
    const MAX_DECISIONS_BYTES: usize = 64 * 1024;
    let file = std::fs::File::open(path)
        .map_err(|e| Error::invalid("app_record_csv", format!("cannot open decisions: {e}")))?;
    let mut bytes = Vec::new();
    file.take((MAX_DECISIONS_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| Error::invalid("app_record_csv", format!("cannot read decisions: {e}")))?;
    if bytes.len() > MAX_DECISIONS_BYTES {
        return Err(Error::invalid(
            "app_record_csv",
            "decisions JSON exceeds 64KiB",
        ));
    }
    let decisions: Value = serde_json::from_slice(&bytes)
        .map_err(|_| Error::invalid("app_record_csv", "decisions must be a JSON array"))?;
    if !decisions.is_array() {
        return Err(Error::invalid(
            "app_record_csv",
            "decisions must be a JSON array",
        ));
    }
    Ok(decisions)
}

fn read_record_profile(path: &Path) -> Result<serde_json::Value> {
    use std::io::Read;
    const MAX_PROFILE_BYTES: usize = 16 * 1024;
    let file = std::fs::File::open(path)
        .map_err(|e| Error::invalid("app_record_profile", format!("cannot open profile: {e}")))?;
    let mut bytes = Vec::new();
    file.take((MAX_PROFILE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| Error::invalid("app_record_profile", format!("cannot read profile: {e}")))?;
    if bytes.len() > MAX_PROFILE_BYTES {
        return Err(Error::invalid(
            "app_record_profile",
            "profile JSON exceeds 16KiB",
        ));
    }
    let profile: Value = serde_json::from_slice(&bytes)
        .map_err(|_| Error::invalid("app_record_profile", "profile must be a JSON object"))?;
    if !profile.is_object() {
        return Err(Error::invalid(
            "app_record_profile",
            "profile must be a JSON object",
        ));
    }
    Ok(profile)
}

fn record_params(action: &RecordAction) -> Result<(&'static str, serde_json::Value)> {
    Ok(match action {
        RecordAction::Create {
            install_id,
            context_id,
            record_id,
            profile,
        } => (
            "app_record_create",
            json!({"install_id": install_id, "context_id": context_id, "record_id": record_id, "profile": read_record_profile(profile)?}),
        ),
        RecordAction::Ls {
            install_id,
            context_id,
        } => (
            "app_record_list",
            json!({"install_id": install_id, "context_id": context_id}),
        ),
        RecordAction::Show {
            install_id,
            context_id,
            record_id,
        } => (
            "app_record_show",
            json!({"install_id": install_id, "context_id": context_id, "record_id": record_id}),
        ),
        RecordAction::Set {
            install_id,
            context_id,
            record_id,
            expected_revision,
            profile,
            consent_method,
            consent_note,
        } => {
            let mut params = json!({"install_id": install_id, "context_id": context_id, "record_id": record_id, "expected_revision": expected_revision, "profile": read_record_profile(profile)?});
            if let Some(method) = consent_method {
                params["consent_provenance"] = json!({"method": method, "note": consent_note});
            }
            ("app_record_update", params)
        }
        RecordAction::CsvPreview {
            install_id,
            context_id,
            csv,
        } => (
            "app_record_csv_preview",
            json!({"install_id": install_id, "context_id": context_id, "csv_text": read_record_csv(csv)?}),
        ),
        RecordAction::CsvImport {
            install_id,
            context_id,
            csv,
            preview_token,
            request_id,
            decisions,
        } => {
            let mut params = json!({"install_id": install_id, "context_id": context_id, "csv_text": read_record_csv(csv)?, "preview_token": preview_token, "request_id": request_id});
            if let Some(path) = decisions {
                params["decisions"] = read_csv_decisions(path)?;
            }
            ("app_record_csv_import", params)
        }
        RecordAction::CsvAssistantImport {
            install_id,
            context_id,
            request_id,
            confirm_token,
            message,
            token,
        } => {
            // Handle-only: the daemon resolves the durable plan
            // (csv_text + decisions + their digests) from request_id +
            // confirm_token. No csv_text/preview_token/decisions params.
            (
                "app_record_csv_assistant_import",
                json!({"install_id": install_id, "context_id": context_id, "request_id": request_id, "confirm_token": confirm_token, "message": message, "token": token}),
            )
        }
        RecordAction::CsvConfirm {
            install_id,
            context_id,
            csv,
            preview_token,
            request_id,
            decisions,
        } => {
            // The confirm stores the durable plan: the exact csv_text +
            // normalized decisions the daemon re-verifies against the
            // preview_token + decisions_digest before minting. The agent
            // later resolves the plan by request id + nonce.
            let csv_text = read_record_csv(csv)?;
            let decisions_value = match decisions {
                Some(path) => read_csv_decisions(path)?,
                None => json!([]),
            };
            let decisions_digest =
                cadence_agent::store::app_records::csv_decisions_digest(&decisions_value)?;
            (
                "app_record_csv_confirm",
                json!({"install_id": install_id, "context_id": context_id, "preview_token": preview_token, "request_id": request_id, "csv_text": csv_text, "decisions": decisions_value, "decisions_digest": decisions_digest}),
            )
        }
        RecordAction::CsvAssistantPreview {
            install_id,
            context_id,
            csv,
            message,
            token,
        } => (
            "app_record_csv_assistant_preview",
            json!({"install_id": install_id, "context_id": context_id, "csv_text": read_record_csv(csv)?, "message": message, "token": token}),
        ),
    })
}

fn read_audience_json(path: &Path, kind: &str) -> Result<serde_json::Value> {
    use std::io::Read;
    const MAX_AUDIENCE_BYTES: usize = 64 * 1024;
    let file = std::fs::File::open(path)
        .map_err(|e| Error::invalid("app_audience", format!("cannot open {kind}: {e}")))?;
    let mut bytes = Vec::new();
    file.take((MAX_AUDIENCE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| Error::invalid("app_audience", format!("cannot read {kind}: {e}")))?;
    if bytes.len() > MAX_AUDIENCE_BYTES {
        return Err(Error::invalid(
            "app_audience",
            format!("{kind} JSON exceeds 64KiB"),
        ));
    }
    serde_json::from_slice(&bytes)
        .map_err(|_| Error::invalid("app_audience", format!("{kind} must be JSON")))
}

fn content_params(action: &ContentAction) -> Result<(&'static str, serde_json::Value)> {
    Ok(match action {
        ContentAction::Draft {
            install_id,
            context_id,
            campaign_id,
            proposal_id,
            draft,
            message,
            token,
        } => {
            // The draft JSON file holds {subject, preheader, blocks};
            // the daemon's content grammar validates every field. The
            // message+token are the live scoped turn's identity.
            let body = read_audience_json(draft, "email draft")?;
            let obj = body
                .as_object()
                .ok_or_else(|| Error::invalid("app_content", "email draft must be an object"))?;
            let mut params = json!({
                "install_id": install_id,
                "context_id": context_id,
                "campaign_id": campaign_id,
                "proposal_id": proposal_id,
                "message": message,
                "token": token,
            });
            for key in ["subject", "preheader", "blocks"] {
                if let Some(v) = obj.get(key) {
                    params[key] = v.clone();
                }
            }
            ("app_content_assistant_draft", params)
        }
        ContentAction::Proposals {
            install_id,
            context_id,
            campaign_id,
            message,
            token,
        } => {
            let mut params = json!({"install_id":install_id,"context_id":context_id,"message":message,"token":token});
            if let Some(campaign) = campaign_id {
                params["campaign_id"] = json!(campaign);
            }
            ("app_content_assistant_proposals", params)
        }
        ContentAction::ProposalShow {
            install_id,
            context_id,
            proposal_id,
            message,
            token,
        } => (
            "app_content_assistant_proposal_show",
            json!({"install_id":install_id,"context_id":context_id,"proposal_id":proposal_id,"message":message,"token":token}),
        ),
    })
}

fn audience_params(action: &AudienceAction) -> Result<(&'static str, serde_json::Value)> {
    Ok(match action {
        AudienceAction::SegmentSave {
            install_id,
            context_id,
            segment_id,
            name,
            predicates,
            expected_revision,
        } => {
            let mut params = json!({"install_id": install_id, "context_id": context_id, "segment_id": segment_id, "name": name, "predicates": read_audience_json(predicates, "predicates")?});
            if let Some(revision) = expected_revision {
                params["expected_revision"] = json!(revision);
            }
            ("app_segment_save", params)
        }
        AudienceAction::SegmentAssistantSave {
            install_id,
            context_id,
            segment_id,
            name,
            predicates,
            expected_revision,
            message,
            token,
        } => {
            let mut params = json!({"install_id": install_id, "context_id": context_id, "segment_id": segment_id, "name": name, "predicates": read_audience_json(predicates, "predicates")?, "message": message, "token": token});
            if let Some(revision) = expected_revision {
                params["expected_revision"] = json!(revision);
            }
            ("app_segment_assistant_save", params)
        }
        AudienceAction::SegmentAssistantLs {
            install_id,
            context_id,
            message,
            token,
        } => (
            "app_segment_assistant_list",
            json!({"install_id": install_id, "context_id": context_id, "message": message, "token": token}),
        ),
        AudienceAction::SegmentAssistantPreview {
            install_id,
            context_id,
            segment_id,
            message,
            token,
        } => (
            "app_segment_assistant_preview",
            json!({"install_id":install_id,"context_id":context_id,"segment_id":segment_id,"message":message,"token":token}),
        ),
        AudienceAction::SegmentAssistantShow {
            install_id,
            context_id,
            segment_id,
            message,
            token,
        } => (
            "app_segment_assistant_show",
            json!({"install_id": install_id, "context_id": context_id, "segment_id": segment_id, "message": message, "token": token}),
        ),
        AudienceAction::SegmentShow {
            install_id,
            context_id,
            segment_id,
        } => (
            "app_segment_show",
            json!({"install_id": install_id, "context_id": context_id, "segment_id": segment_id}),
        ),
        AudienceAction::SegmentLs {
            install_id,
            context_id,
        } => (
            "app_segment_list",
            json!({"install_id": install_id, "context_id": context_id}),
        ),
        AudienceAction::ExclusionSave {
            install_id,
            context_id,
            list_id,
            name,
            members,
            expected_revision,
        } => {
            let mut params = json!({"install_id": install_id, "context_id": context_id, "list_id": list_id, "name": name, "member_ids": read_audience_json(members, "members")?});
            if let Some(revision) = expected_revision {
                params["expected_revision"] = json!(revision);
            }
            ("app_exclusion_save", params)
        }
        AudienceAction::ExclusionShow {
            install_id,
            context_id,
            list_id,
        } => (
            "app_exclusion_show",
            json!({"install_id": install_id, "context_id": context_id, "list_id": list_id}),
        ),
        AudienceAction::ExclusionLs {
            install_id,
            context_id,
        } => (
            "app_exclusion_list",
            json!({"install_id": install_id, "context_id": context_id}),
        ),
        AudienceAction::SuppressionAdd {
            install_id,
            context_id,
            email,
            customer_id,
            reason,
        } => {
            let mut params =
                json!({"install_id": install_id, "context_id": context_id, "reason": reason});
            if let Some(address) = email {
                params["email"] = json!(address);
            }
            if let Some(id) = customer_id {
                params["customer_id"] = json!(id);
            }
            ("app_suppression_add", params)
        }
        AudienceAction::SuppressionRemove {
            install_id,
            context_id,
            email,
            customer_id,
        } => {
            let mut params = json!({"install_id": install_id, "context_id": context_id});
            if let Some(address) = email {
                params["email"] = json!(address);
            }
            if let Some(id) = customer_id {
                params["customer_id"] = json!(id);
            }
            ("app_suppression_remove", params)
        }
        AudienceAction::SuppressionLs {
            install_id,
            context_id,
        } => (
            "app_suppression_list",
            json!({"install_id": install_id, "context_id": context_id}),
        ),
        AudienceAction::Preview {
            install_id,
            context_id,
            base,
            exclusion_list_id,
        } => {
            let mut params = json!({"install_id": install_id, "context_id": context_id, "base": read_audience_json(base, "base")?});
            if let Some(list) = exclusion_list_id {
                params["exclusion_list_id"] = json!(list);
            }
            ("app_audience_preview", params)
        }
        AudienceAction::Prepare {
            install_id,
            context_id,
            freeze_id,
            base,
            exclusion_list_id,
            max_recipients,
        } => {
            let mut params = json!({"install_id": install_id, "context_id": context_id, "freeze_id": freeze_id, "base": read_audience_json(base, "base")?, "max_recipients": max_recipients});
            if let Some(list) = exclusion_list_id {
                params["exclusion_list_id"] = json!(list);
            }
            ("app_audience_prepare", params)
        }
        AudienceAction::FreezeShow {
            install_id,
            context_id,
            freeze_id,
        } => (
            "app_audience_show",
            json!({"install_id": install_id, "context_id": context_id, "freeze_id": freeze_id}),
        ),
    })
}

fn release_scope(install: Option<&str>, context: Option<&str>) -> serde_json::Value {
    let mut params = json!({});
    if let Some(id) = install {
        params["install_id"] = json!(id);
    }
    if let Some(id) = context {
        params["context_id"] = json!(id);
    }
    params
}
fn binding_params(action: &BindingAction) -> (&'static str, serde_json::Value) {
    match action {
        BindingAction::Create {
            install_id,
            context_id,
            slot,
            connection_id,
            request_id,
        } => {
            let mut params = release_scope(Some(install_id), context_id.as_deref());
            params["slot"] = json!(slot);
            params["connection_id"] = json!(connection_id);
            params["request_id"] = json!(request_id);
            ("app_binding_create", params)
        }
        BindingAction::Ls {
            install_id,
            context_id,
        } => (
            "app_binding_list",
            release_scope(Some(install_id), context_id.as_deref()),
        ),
        BindingAction::Show {
            install_id,
            binding_id,
        } => (
            "app_binding_show",
            json!({"install_id":install_id,"binding_id":binding_id}),
        ),
        BindingAction::Set {
            install_id,
            binding_id,
            expected_revision,
            connection_id,
        } => (
            "app_binding_update",
            json!({"install_id":install_id,"binding_id":binding_id,"expected_revision":expected_revision,"connection_id":connection_id}),
        ),
        BindingAction::Publish {
            install_id,
            binding_id,
            expected_revision,
            destination_id,
            destination_label,
            toolkit,
            timezone,
            grant_id,
        } => (
            "app_binding_publish_set",
            json!({"install_id":install_id,"binding_id":binding_id,"expected_revision":expected_revision,
            "destination_id":destination_id,"destination_label":destination_label,"toolkit":toolkit,
            "timezone":timezone,"grant_id":grant_id}),
        ),
        BindingAction::Revoke {
            install_id,
            binding_id,
            expected_revision,
        } => (
            "app_binding_revoke",
            json!({"install_id":install_id,"binding_id":binding_id,"expected_revision":expected_revision}),
        ),
    }
}
fn effect_params(action: &EffectAction) -> (&'static str, serde_json::Value) {
    match action {
        EffectAction::Resolve {
            effect_id,
            digest,
            resolution,
        } => (
            "app_effect_resolve",
            json!({"effect_id":effect_id,"digest":digest,"resolution":resolution.as_str()}),
        ),
        EffectAction::Stage {
            run_id,
            artifact_id,
            slot,
            request_id,
            title,
        } => (
            "app_effect_stage",
            json!({"run_id":run_id,"artifact_id":artifact_id,"slot":slot,"request_id":request_id,"title":title}),
        ),
        EffectAction::Show { effect_id } => ("app_effect_show", json!({"effect_id":effect_id})),
        EffectAction::Ls {
            install_id,
            context_id,
        } => (
            "app_effect_list",
            release_scope(install_id.as_deref(), context_id.as_deref()),
        ),
        EffectAction::Accept { effect_id, digest } => (
            "app_effect_decide",
            json!({"effect_id":effect_id,"digest":digest,"decision":"accept"}),
        ),
        EffectAction::Decline { effect_id, digest } => (
            "app_effect_decide",
            json!({"effect_id":effect_id,"digest":digest,"decision":"decline"}),
        ),
    }
}

#[derive(Subcommand)]
pub(crate) enum CatalogAction {
    /// Approve the exact installed digest for supported local artifact steps.
    Approve {
        install_id: String,
        #[arg(long)]
        digest: String,
    },
    /// Revoke local capabilities without changing legacy project approvals.
    Revoke {
        install_id: String,
        #[arg(long)]
        digest: String,
    },
    /// Explicitly resume or roll back a retained catalog migration journal.
    MigrationRecover {
        journal_id: String,
        #[arg(long)]
        rollback: bool,
    },
    /// Install a validated bundle without creating a project or grants.
    Install { source: String },
    /// Replace one exact workspace bundle while keeping its installation ID.
    /// The current digest and catalog generation must match the inspected row.
    UpgradeCheck {
        install_id: String,
        source: String,
        #[arg(long)]
        expected_digest: String,
        #[arg(long)]
        expected_generation: String,
    },
    /// Apply an inspected upgrade, pinned to both old and proposed bytes.
    Upgrade {
        install_id: String,
        source: String,
        #[arg(long)]
        expected_digest: String,
        #[arg(long)]
        expected_generation: String,
        #[arg(long)]
        expected_new_digest: String,
        #[arg(long)]
        request_id: String,
    },
    /// Resume a retained workspace upgrade after interrupted Git delivery.
    UpgradeRecover {
        install_id: String,
        #[arg(long)]
        request_id: String,
    },
    /// List catalogued installations. Never migrates on read.
    #[command(visible_alias = "list")]
    Ls,
    /// Inspect an exact stable installation ID.
    Show { install_id: String },
    /// Explicitly catalogue/backfill existing legacy installations.
    Migrate,
    /// Resume a retained installation journal after a failed delivery.
    Recover { install_id: String },
}

#[derive(Subcommand)]
pub(crate) enum RunAction {
    /// Freeze a checked local workflow and explicit inputs; does not dispatch.
    Create {
        install_id: String,
        #[arg(long)]
        workflow: String,
        /// JSON object of string inputs, including declared worker roles.
        #[arg(long)]
        inputs: PathBuf,
        /// Idempotency key; reuse with different inputs is refused.
        #[arg(long)]
        request_id: String,
        /// Registered PM whose workers execute this run.
        #[arg(long)]
        owner_pm: String,
        /// Optional discovery link; confers no authority.
        #[arg(long)]
        project_link: Option<String>,
        /// Exact optional brand context; omission means a context-free run.
        #[arg(long)]
        context_id: Option<String>,
        /// Exact previously fetched provider result, if this run uses a source post.
        #[arg(long, requires = "selected_post_id")]
        source_receipt_id: Option<String>,
        #[arg(long, requires = "source_receipt_id")]
        selected_post_id: Option<String>,
    },
    /// Approve the exact frozen run snapshot; does not authorize outward release.
    Approve {
        run_id: String,
        #[arg(long)]
        digest: String,
    },
    /// Cancel a run and prevent further dispatch.
    Cancel { run_id: String },
    /// Dispatch eligible steps of an explicitly approved run.
    Dispatch { run_id: String },
    /// Inspect an exact app-owned run and its output receipts.
    Show { run_id: String },
    /// List app-owned runs, optionally for an exact installation.
    #[command(visible_alias = "list")]
    Ls {
        #[arg(long)]
        install_id: Option<String>,
        #[arg(long, requires = "install_id")]
        context_id: Option<String>,
    },
    /// Read a bounded text artifact as the operator or its assigned dependent turn.
    Artifact {
        artifact_id: String,
        /// Active dependent kickoff; requires its current turn token.
        #[arg(long, requires = "token")]
        message: Option<String>,
        #[arg(long, requires = "message")]
        token: Option<String>,
    },
    /// Invoke one frozen read/draft capability during the assigned app turn.
    CapabilityCall {
        #[arg(long)]
        message: String,
        #[arg(long)]
        token: String,
        #[arg(long)]
        slot: String,
        #[arg(long)]
        request_id: String,
        #[arg(long)]
        input_json: String,
    },
    /// Operator view of durable capability receipts for one run.
    CapabilityResults { run_id: String },
    /// Operator or assigned turn view of one exact receipt.
    CapabilityResult {
        receipt_id: String,
        #[arg(long, requires = "token")]
        message: Option<String>,
        #[arg(long, requires = "message")]
        token: Option<String>,
    },
    /// Operator or assigned turn download of one exact bounded asset.
    CapabilityAsset {
        receipt_id: String,
        #[arg(long, requires = "token")]
        message: Option<String>,
        #[arg(long, requires = "message")]
        token: Option<String>,
    },
}

fn read_run_inputs(path: &Path) -> Result<serde_json::Value> {
    use std::io::Read;
    const MAX_INPUT_BYTES: usize = 32 * 1024;
    let file = std::fs::File::open(path)
        .map_err(|e| Error::invalid("app_run_inputs", format!("cannot open inputs: {e}")))?;
    let mut bytes = Vec::new();
    file.take((MAX_INPUT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| Error::invalid("app_run_inputs", format!("cannot read inputs: {e}")))?;
    if bytes.len() > MAX_INPUT_BYTES {
        return Err(Error::invalid(
            "app_run_inputs",
            "inputs JSON exceeds 32KiB",
        ));
    }
    let inputs: std::collections::BTreeMap<String, String> = serde_json::from_slice(&bytes)
        .map_err(|_| Error::invalid("app_run_inputs", "inputs must be a JSON object of strings"))?;
    Ok(json!(inputs))
}

fn run_params(action: &RunAction) -> Result<(&'static str, serde_json::Value)> {
    Ok(match action {
        RunAction::Create {
            install_id,
            workflow,
            inputs,
            request_id,
            owner_pm,
            project_link,
            context_id,
            source_receipt_id,
            selected_post_id,
        } => {
            let mut params = json!({
                "install_id": install_id,
                "workflow": workflow,
                "inputs": read_run_inputs(inputs)?,
                "request_id": request_id,
                "owner_pm": owner_pm,
            });
            if let Some(link) = project_link {
                params["project_link"] = json!(link);
            }
            if let Some(context) = context_id {
                params["context_id"] = json!(context);
            }
            if let (Some(receipt), Some(post)) = (source_receipt_id, selected_post_id) {
                params["source_receipt_id"] = json!(receipt);
                params["selected_post_id"] = json!(post);
            }
            ("app_run_create", params)
        }
        RunAction::Approve { run_id, digest } => {
            ("app_run_approve", json!({"run_id":run_id,"digest":digest}))
        }
        RunAction::Cancel { run_id } => ("app_run_cancel", json!({"run_id":run_id})),
        RunAction::Dispatch { run_id } => ("app_run_dispatch", json!({"run_id":run_id})),
        RunAction::Show { run_id } => ("app_run_show", json!({"run_id":run_id})),
        RunAction::Ls {
            install_id,
            context_id,
        } => {
            let mut params = match install_id {
                Some(id) => json!({"install_id":id}),
                None => json!({}),
            };
            if let Some(context) = context_id {
                params["context_id"] = json!(context);
            }
            ("app_run_list", params)
        }
        RunAction::Artifact {
            artifact_id,
            message,
            token,
        } => {
            let params = match (message, token) {
                (None, None) => json!({"artifact_id":artifact_id}),
                (Some(message), Some(token)) => {
                    json!({"artifact_id":artifact_id,"message":message,"token":token})
                }
                _ => {
                    return Err(Error::invalid(
                        "app_run_artifact",
                        "message and token must be supplied together",
                    ));
                }
            };
            ("app_run_artifact", params)
        }
        RunAction::CapabilityCall {
            message,
            token,
            slot,
            request_id,
            input_json,
        } => {
            let input: Value = serde_json::from_str(input_json).map_err(|_| {
                Error::invalid(
                    "app_run_capability_call",
                    "input-json must be a JSON object",
                )
            })?;
            if !input.is_object() || serde_json::to_vec(&input)?.len() > 64 * 1024 {
                return Err(Error::invalid(
                    "app_run_capability_call",
                    "input-json must be an object within 64 KiB",
                ));
            }
            (
                "app_run_capability_call",
                json!({"message":message,"token":token,"slot":slot,
                "request_id":request_id,"input":input}),
            )
        }
        RunAction::CapabilityResults { run_id } => {
            ("app_run_capability_results", json!({"run_id":run_id}))
        }
        RunAction::CapabilityResult {
            receipt_id,
            message,
            token,
        }
        | RunAction::CapabilityAsset {
            receipt_id,
            message,
            token,
        } => {
            let mut params = json!({"receipt_id":receipt_id});
            if let (Some(message), Some(token)) = (message, token) {
                params["message"] = json!(message);
                params["token"] = json!(token);
            }
            (
                if matches!(action, RunAction::CapabilityResult { .. }) {
                    "app_run_capability_result"
                } else {
                    "app_run_capability_asset"
                },
                params,
            )
        }
    })
}

/// `cadence app …` (CAD-547). `install`/`update`/`set`/`remove` write
/// the tracker directly — one commit each, `Actor:` recorded, all
/// unapproving-by-construction (a tracker write can only change the
/// digest, never the approval record) — `ls`/`show` read; `approve`
/// is the operator's daemon call, like `workflow approve`.
pub(super) fn run_app(state_dir: &Path, action: AppAction) -> Result<i32> {
    use cadence_agent::issue::app;
    let result = match &action {
        AppAction::Binding { action } => {
            let (method, params) = binding_params(action);
            client::rpc(state_dir, method, params)?
        }
        AppAction::Effect { action } => {
            let (method, params) = effect_params(action);
            client::rpc(state_dir, method, params)?
        }
        AppAction::Context { action } => {
            let (method, params) = match action {
                ContextAction::Create {
                    install_id,
                    label,
                    defaults,
                    request_id,
                } => (
                    "app_context_create",
                    json!({"install_id":install_id,"label":label,"input_defaults":match defaults {Some(path)=>read_run_inputs(path)?,None=>json!({})},"request_id":request_id}),
                ),
                ContextAction::Ls { install_id } => {
                    ("app_context_list", json!({"install_id":install_id}))
                }
                ContextAction::Show {
                    install_id,
                    context_id,
                } => (
                    "app_context_show",
                    json!({"install_id":install_id,"context_id":context_id}),
                ),
                ContextAction::Set {
                    install_id,
                    context_id,
                    expected_revision,
                    label,
                    defaults,
                } => (
                    "app_context_update",
                    json!({"install_id":install_id,"context_id":context_id,"expected_revision":expected_revision,"label":label,"input_defaults":read_run_inputs(defaults)?}),
                ),
                ContextAction::Archive {
                    install_id,
                    context_id,
                    expected_revision,
                } => (
                    "app_context_archive",
                    json!({"install_id":install_id,"context_id":context_id,"expected_revision":expected_revision}),
                ),
            };
            client::rpc(state_dir, method, params)?
        }
        AppAction::Catalog { action } => {
            let (method, params) = match action {
                CatalogAction::Approve { install_id, digest } => (
                    "app_local_install_approve",
                    json!({"install_id":install_id,"digest":digest}),
                ),
                CatalogAction::Revoke { install_id, digest } => (
                    "app_local_install_revoke",
                    json!({"install_id":install_id,"digest":digest}),
                ),
                CatalogAction::Install { source } => {
                    let source = if source.contains("://") || source.starts_with("git@") {
                        source.clone()
                    } else {
                        std::fs::canonicalize(source)
                            .map_err(|e| Error::rejected(format!("workspace app source: {e}")))?
                            .to_string_lossy()
                            .into_owned()
                    };
                    ("app_workspace_install", json!({"source":source}))
                }
                CatalogAction::Upgrade {
                    install_id,
                    source,
                    expected_digest,
                    expected_generation,
                    expected_new_digest,
                    request_id,
                } => {
                    let source = if source.contains("://") || source.starts_with("git@") {
                        source.clone()
                    } else {
                        // A committed request is replayable after its local
                        // checkout is removed. The daemon verifies the exact
                        // stored source and digest before returning that row.
                        match std::fs::canonicalize(source) {
                            Ok(path) => path,
                            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                                std::path::absolute(source).map_err(|e| {
                                    Error::rejected(format!("workspace app source: {e}"))
                                })?
                            }
                            Err(error) => {
                                return Err(Error::rejected(format!(
                                    "workspace app source: {error}"
                                )))
                            }
                        }
                        .to_string_lossy()
                        .into_owned()
                    };
                    (
                        "app_workspace_upgrade",
                        json!({"install_id":install_id,"source":source,
                        "expected_digest":expected_digest,"expected_generation":expected_generation,
                        "expected_new_digest":expected_new_digest,"request_id":request_id}),
                    )
                }
                CatalogAction::UpgradeCheck {
                    install_id,
                    source,
                    expected_digest,
                    expected_generation,
                } => {
                    let source = if source.contains("://") || source.starts_with("git@") {
                        source.clone()
                    } else {
                        std::fs::canonicalize(source)
                            .map_err(|e| Error::rejected(format!("workspace app source: {e}")))?
                            .to_string_lossy()
                            .into_owned()
                    };
                    (
                        "app_workspace_upgrade_check",
                        json!({"install_id":install_id,"source":source,
                        "expected_digest":expected_digest,"expected_generation":expected_generation}),
                    )
                }
                CatalogAction::UpgradeRecover {
                    install_id,
                    request_id,
                } => (
                    "app_workspace_upgrade_recover",
                    json!({"install_id":install_id,"request_id":request_id}),
                ),
                CatalogAction::MigrationRecover {
                    journal_id,
                    rollback,
                } => (
                    "app_workspace_migration_recover",
                    json!({"journal_id":journal_id,"rollback":rollback}),
                ),
                CatalogAction::Ls => ("app_workspace_list", json!({})),
                CatalogAction::Show { install_id } => {
                    ("app_workspace_show", json!({"install_id":install_id}))
                }
                CatalogAction::Migrate => ("app_workspace_migrate", json!({})),
                CatalogAction::Recover { install_id } => {
                    ("app_workspace_recover", json!({"install_id":install_id}))
                }
            };
            client::rpc(state_dir, method, params)?
        }
        AppAction::Run { action } => {
            let (method, params) = run_params(action)?;
            client::rpc(state_dir, method, params)?
        }
        AppAction::Record { action } => {
            let (method, params) = record_params(action)?;
            client::rpc(state_dir, method, params)?
        }
        AppAction::Audience { action } => {
            let (method, params) = audience_params(action)?;
            client::rpc(state_dir, method, params)?
        }
        AppAction::Content { action } => {
            let (method, params) = content_params(action)?;
            client::rpc(state_dir, method, params)?
        }
        AppAction::Dev {
            name,
            source,
            port,
            host,
            allow_host,
        } => {
            return run_dev(name, source, *port, host, allow_host);
        }
        AppAction::Install { source, project } => {
            let pm = cadence_agent::issue::Pm::open_default()?;
            app::install(&pm, project, source, state_dir, "")?
        }
        AppAction::Ls { project } => {
            let pm = cadence_agent::issue::Pm::open_default()?;
            app::ls(&pm, project.as_deref(), state_dir)?
        }
        AppAction::Show { name, project } => {
            let pm = cadence_agent::issue::Pm::open_default()?;
            app::show(&pm, project, name, state_dir)?
        }
        AppAction::Set {
            name,
            bindings,
            project,
        } => {
            let pm = cadence_agent::issue::Pm::open_default()?;
            app::set(&pm, project, name, bindings, state_dir, "")?
        }
        AppAction::Update {
            name,
            source,
            project,
        } => {
            let pm = cadence_agent::issue::Pm::open_default()?;
            app::update(&pm, project, name, source.as_deref(), state_dir, "")?
        }
        AppAction::Remove { name, project } => {
            let pm = cadence_agent::issue::Pm::open_default()?;
            app::remove(&pm, project, name, state_dir, "")?
        }
        AppAction::Approve { name, project } => client::rpc(
            state_dir,
            "app_approve",
            json!({"project": project, "name": name}),
        )?,
        AppAction::Revoke { name, project } => client::rpc(
            state_dir,
            "app_revoke",
            json!({"project": project, "name": name}),
        )?,
        AppAction::SetTeam {
            name,
            roles,
            project,
        } => client::rpc(
            state_dir,
            "app_set_team",
            json!({"project": project, "name": name, "team": roles}),
        )?,
        AppAction::AddWorker {
            name,
            role,
            project,
        } => client::rpc(
            state_dir,
            "app_add_worker",
            json!({"project": project, "name": name, "role": role}),
        )?,
    };
    print_json(&result);
    Ok(0)
}

pub(super) fn run(state_dir: PathBuf, action: AppAction) -> Result<i32> {
    run_app(&state_dir, action)
}

/// No production state, PM, socket or app approval is accessed here.
pub(super) fn run_dev(
    name: &str,
    source: &Path,
    port: u16,
    host: &str,
    allow_hosts: &[String],
) -> Result<i32> {
    let source = source
        .canonicalize()
        .map_err(|e| Error::invalid("app_dev_source", format!("trusted source checkout: {e}")))?;
    let harness = source.join("scripts/app-dev.mjs");
    if !harness.is_file() || !source.join("ui/package.json").is_file() {
        return Err(Error::invalid("app_dev_source", "source must be a trusted Cadence checkout with scripts/app-dev.mjs and UI dependencies"));
    }
    let mut command = std::process::Command::new("node");
    command
        .arg(&harness)
        .arg(name)
        .arg("--port")
        .arg(port.to_string())
        .arg("--host")
        .arg(host)
        .current_dir(&source);
    for host in allow_hosts {
        command.arg("--allow-host").arg(host);
    }
    // The preview needs tools, not a native identity, state dir, provider
    // keys, browser cookies or the operator's daemon/session environment.
    command.env_clear();
    for name in [
        "PATH",
        "HOME",
        "LANG",
        "TERM",
        "PNPM_HOME",
        "XDG_CACHE_HOME",
    ] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    let status = cadence_agent::reaper::status(&mut command)
        .map_err(|e| Error::internal(format!("app development harness: {e}")))?;
    Ok(status.code().unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_run_inputs_reject_non_string_maps_without_echoing_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("inputs.json");
        for text in [
            r#"["private input"]"#,
            r#"{"source":123}"#,
            r#"{"source":{"private input":"value"}}"#,
            "private input",
        ] {
            std::fs::write(&path, text).unwrap();
            let error = read_run_inputs(&path).unwrap_err().to_string();
            assert!(error.contains("JSON object of strings"));
            assert!(!error.contains("private input"));
        }
        std::fs::write(&path, r#"{"source":"source text","writer":"op-writer"}"#).unwrap();
        assert_eq!(
            read_run_inputs(&path).unwrap(),
            json!({"source":"source text","writer":"op-writer"})
        );
    }

    #[test]
    fn app_run_inputs_bound_utf8_bytes_before_sending() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("inputs.json");
        let text = json!({"source":"界".repeat(11_000)}).to_string();
        std::fs::write(&path, text).unwrap();
        assert!(read_run_inputs(&path)
            .unwrap_err()
            .to_string()
            .contains("exceeds 32KiB"));
    }

    /// CAD-1014: the assistant CSV import is HANDLE-ONLY — the daemon's
    /// durable plan resolves the bytes server-side, so the CLI emits only
    /// request_id + confirm_token + the turn identity + scope. The wire
    /// carries NO csv_text, preview_token or decisions — those are the
    /// operator's confirm payload, never the agent's redeem.
    #[test]
    fn csv_assistant_import_cli_is_handle_only() {
        let action = RecordAction::CsvAssistantImport {
            install_id: "inst-1".into(),
            context_id: "ctx-1".into(),
            request_id: "req-1".into(),
            confirm_token: "confirm-abc".into(),
            message: "chat-1".into(),
            token: "tok-1".into(),
        };
        let (method, params) = record_params(&action).unwrap();
        assert_eq!(method, "app_record_csv_assistant_import");
        assert_eq!(
            params,
            json!({"install_id":"inst-1","context_id":"ctx-1","request_id":"req-1",
                   "confirm_token":"confirm-abc","message":"chat-1","token":"tok-1"})
        );
        // Bytes/decisions/preview_token never reach the redeem params.
        for field in ["csv_text", "preview_token", "decisions"] {
            assert!(
                params.get(field).is_none(),
                "assistant import carried {field}: {params}"
            );
        }
    }

    /// The operator's `csv-confirm` DOES carry the durable plan:
    /// csv_text + the normalized decisions + their digest, bound to the
    /// preview token + request id. This is the host-held plan the
    /// assistant resolves by handle.
    #[test]
    fn csv_confirm_cli_carries_the_durable_plan() {
        let dir = tempfile::tempdir().unwrap();
        let csv = dir.path().join("in.csv");
        std::fs::write(&csv, "record_id,display_name,email\nc1,A,a@b.co\n").unwrap();
        let decisions = dir.path().join("decisions.json");
        std::fs::write(&decisions, r#"[{"row":1,"action":"create"}]"#).unwrap();
        let action = RecordAction::CsvConfirm {
            install_id: "inst-1".into(),
            context_id: "ctx-1".into(),
            csv,
            preview_token: "sha256:pt".into(),
            request_id: "req-1".into(),
            decisions: Some(decisions),
        };
        let (method, params) = record_params(&action).unwrap();
        assert_eq!(method, "app_record_csv_confirm");
        // The plan: bytes + decisions + their computed digest + the
        // preview binding + request id — the durable host plan.
        assert_eq!(
            params["csv_text"],
            "record_id,display_name,email\nc1,A,a@b.co\n"
        );
        assert_eq!(params["decisions"], json!([{"row":1,"action":"create"}]));
        assert_eq!(params["preview_token"], "sha256:pt");
        assert_eq!(params["request_id"], "req-1");
        assert!(params["decisions_digest"]
            .as_str()
            .is_some_and(|d| d.starts_with("sha256:")));
        // Never smuggles identity/routing.
        for field in ["by", "actor", "workspace", "install_id", "context_id"] {
            // install_id/context_id are legitimate URL scope fields.
            if matches!(field, "install_id" | "context_id") {
                continue;
            }
            assert!(params.get(field).is_none());
        }
    }
}
