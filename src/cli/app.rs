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
    /// Offline install validation of a local package: the same bundle
    /// checks `app catalog install-check` runs — entry grammar, per-file
    /// and aggregate caps, manifest, descriptors, workflow checks and
    /// `requires` against this build's contracts — with no daemon, no
    /// store and no PM read or write. `<source>` is a local directory; a
    /// git/URL source is refused (a transport, not bytes) and the
    /// `builtin:<catalog id>` form is retired (CAD-1422). Prints the JSON
    /// report; exit 0 on pass, 3 on refusal.
    /// Catalog admission (duplicate installs, pending journals) is not
    /// reproduced — the report says so.
    Check {
        /// Package directory.
        source: String,
        /// The output is always the JSON report; `--json` makes the
        /// machine-readable contract explicit for scripts.
        #[arg(long)]
        json: bool,
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
    /// Works content (CAD-1662): native operator docs, decks and sheets over
    /// the durable store. Explicit install and context; no actor flags. Review
    /// needs two verified humans, and the daemon derives the actor.
    Works {
        #[command(subcommand)]
        action: WorksAction,
    },
    /// CRM app assistant discovery, consented action invocation, and durable receipt reads.
    Assistant {
        #[command(subcommand)]
        action: AssistantAction,
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
    /// Export the exact host screen frame and kit CSS for offline previews.
    FrameKit {
        /// New output directory for the frame, kit CSS and metadata.
        #[arg(long)]
        out: PathBuf,
        /// Optional local app bundle to mount as the validated screen entry.
        #[arg(long)]
        bundle: Option<PathBuf>,
        /// Exact parent origin allowed by the frame bridge.
        #[arg(long, default_value = "http://127.0.0.1")]
        parent_origin: String,
    },
    /// Emit canonical screen PUSH fixtures from a local validated app bundle.
    Fixtures {
        /// Local app package directory.
        #[arg(long)]
        bundle: PathBuf,
        /// Require the machine-readable fixture envelope.
        #[arg(long)]
        json: bool,
    },
    /// Run the app's declared local preview. In a detached app repository,
    /// `--package .` reads `package.json.cadence` and runs the app-owned
    /// `dev` command with a scrubbed environment. The legacy `--source`
    /// mode runs a trusted Cadence checkout's `scripts/app-dev.mjs`.
    Dev {
        /// Legacy preview directory name under app-previews/.
        name: Option<String>,
        /// Legacy trusted source checkout containing scripts/app-dev.mjs.
        #[arg(long, conflicts_with = "package")]
        source: Option<PathBuf>,
        /// Detached app package root; reads its `package.json.cadence`
        /// contract and runs the app-owned `dev` command.
        #[arg(long, conflicts_with_all = ["name", "source"])]
        package: Option<PathBuf>,
        #[arg(long, default_value_t = 3186, value_parser = clap::value_parser!(u16).range(3110..=3199))]
        port: u16,
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        /// Legacy private tailnet hostname; refused with --package.
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
    /// Set CRM action autonomy: `assisted` keeps the operator decision
    /// on record writes and campaign sends; `full` lets a scoped agent
    /// turn run them directly. Operator only.
    #[command(name = "autonomy-set")]
    AutonomySet { mode: String },
    /// Show the current CRM action autonomy.
    #[command(name = "autonomy-show")]
    AutonomyShow,
}

/// CAD-1662 native Works verbs. Each takes an explicit install and
/// context; the daemon derives the actor from the operator connection,
/// so no actor flag exists. Content and audit-bearing writes carry a
/// caller idempotency key and an `if_revision`/`revision` pin.
#[derive(Subcommand)]
pub(crate) enum WorksAction {
    /// List the context's Works items (optional filters).
    Query {
        install_id: String,
        #[arg(long)]
        context_id: String,
        #[arg(long)]
        text: Option<String>,
        #[arg(long)]
        kind: Option<String>,
        #[arg(long)]
        review: Option<String>,
        #[arg(long)]
        export: Option<String>,
        #[arg(long)]
        tag: Option<String>,
        #[arg(long)]
        owner: Option<String>,
        #[arg(long)]
        sort: Option<String>,
    },
    /// Show one Works item.
    Get {
        install_id: String,
        #[arg(long)]
        context_id: String,
        #[arg(long)]
        id: String,
    },
    /// Create a doc, deck or sheet from a JSON file.
    Create {
        install_id: String,
        #[arg(long)]
        context_id: String,
        /// JSON file holding `{kind, title, content, tags?}`.
        #[arg(long)]
        body: PathBuf,
        #[arg(long)]
        idempotency_key: String,
    },
    /// Apply a JSON patch document pinned to `if_revision`.
    Patch {
        install_id: String,
        #[arg(long)]
        context_id: String,
        #[arg(long)]
        id: String,
        #[arg(long)]
        if_revision: i64,
        /// JSON file holding `{operations: [...]}`.
        #[arg(long)]
        body: PathBuf,
        #[arg(long)]
        idempotency_key: String,
    },
    /// Move one item to trash at `if_revision`.
    Trash {
        install_id: String,
        #[arg(long)]
        context_id: String,
        #[arg(long)]
        id: String,
        #[arg(long)]
        if_revision: i64,
        #[arg(long)]
        idempotency_key: String,
    },
    /// Restore one trashed item at `if_revision`.
    Restore {
        install_id: String,
        #[arg(long)]
        context_id: String,
        #[arg(long)]
        id: String,
        #[arg(long)]
        if_revision: i64,
        #[arg(long)]
        idempotency_key: String,
    },
    /// Request independent review of one revision.
    #[command(name = "request-review")]
    RequestReview {
        install_id: String,
        #[arg(long)]
        context_id: String,
        #[arg(long)]
        id: String,
        #[arg(long)]
        revision: i64,
    },
    /// Record a reviewer decision on a pending revision.
    Review {
        install_id: String,
        #[arg(long)]
        context_id: String,
        #[arg(long)]
        id: String,
        #[arg(long)]
        revision: i64,
        /// `approve` or `changes`.
        #[arg(long)]
        decision: String,
        #[arg(long)]
        note: Option<String>,
    },
    /// Export an approved, current revision (receipt only; content stays in the store).
    Export {
        install_id: String,
        #[arg(long)]
        context_id: String,
        #[arg(long)]
        id: String,
        #[arg(long)]
        revision: i64,
        #[arg(long)]
        format: Option<String>,
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
/// The source the daemon installs from: URLs pass through, a local path
/// is made absolute because the daemon refuses relative ones. A
/// `builtin:` source is refused here — the built-in catalog is retired
/// (CAD-1422); the daemon refuses it too, but the CLI names the retired
/// form instead of passing it through. Shared by `install`,
/// `install-check`, `upgrade-check`.
fn install_source(source: &str) -> Result<String> {
    if source.starts_with("builtin:") {
        return Err(Error::rejected(
            "builtin: sources are retired (CAD-1422) — install from the app's detached repository (a path or Git URL)",
        ));
    }
    if source.contains("://") || source.starts_with("git@") {
        return Ok(source.to_string());
    }
    Ok(std::fs::canonicalize(source)
        .map_err(|e| Error::rejected(format!("workspace app source: {e}")))?
        .to_string_lossy()
        .into_owned())
}

fn catalogue_params(
    params: &mut Value,
    id: &Option<String>,
    version: &Option<String>,
    digest: &Option<String>,
    resolution: &Option<String>,
) -> Result<()> {
    let locator = match (id, version, digest) {
        (None, None, None) => None,
        (Some(id), Some(version), Some(digest)) => {
            Some(json!({"id": id, "version": version, "digest": digest}))
        }
        _ => {
            return Err(Error::rejected(
                "catalogue id, version and digest must be set together",
            ))
        }
    };
    if locator.is_some() && resolution.is_some() {
        return Err(Error::rejected(
            "catalogue locator and manual resolution are mutually exclusive",
        ));
    }
    if let Some(resolution) = resolution {
        if resolution != "manual" {
            return Err(Error::rejected("catalogue resolution must be manual"));
        }
        params["catalogue_resolution"] = json!("manual");
    }
    if let Some(locator) = locator {
        params["catalogue"] = locator;
    }
    Ok(())
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

/// CAD-1662: map each native Works verb to its `app_works_*` RPC. JSON
/// files are bounded by `read_audience_json` (64 KiB) and forwarded as-is;
/// the daemon's typed grammar validates every field. No actor is sent.
fn works_params(action: &WorksAction) -> Result<(&'static str, serde_json::Value)> {
    Ok(match action {
        WorksAction::Query {
            install_id,
            context_id,
            text,
            kind,
            review,
            export,
            tag,
            owner,
            sort,
        } => (
            "app_works_query",
            json!({"install_id": install_id, "context_id": context_id, "text": text, "kind": kind, "review": review, "export": export, "tag": tag, "owner": owner, "sort": sort}),
        ),
        WorksAction::Get {
            install_id,
            context_id,
            id,
        } => (
            "app_works_get",
            json!({"install_id": install_id, "context_id": context_id, "id": id}),
        ),
        WorksAction::Create {
            install_id,
            context_id,
            body,
            idempotency_key,
        } => {
            let mut params = read_works_json(body, "works body")?;
            let obj = params
                .as_object_mut()
                .ok_or_else(|| Error::invalid("app_works", "works body must be an object"))?;
            obj.insert("install_id".into(), json!(install_id));
            obj.insert("context_id".into(), json!(context_id));
            obj.insert("idempotency_key".into(), json!(idempotency_key));
            ("app_works_create", params)
        }
        WorksAction::Patch {
            install_id,
            context_id,
            id,
            if_revision,
            body,
            idempotency_key,
        } => {
            let mut params = read_works_json(body, "works patch")?;
            let obj = params
                .as_object_mut()
                .ok_or_else(|| Error::invalid("app_works", "works patch must be an object"))?;
            obj.insert("install_id".into(), json!(install_id));
            obj.insert("context_id".into(), json!(context_id));
            obj.insert("id".into(), json!(id));
            obj.insert("if_revision".into(), json!(if_revision));
            obj.insert("idempotency_key".into(), json!(idempotency_key));
            ("app_works_patch", params)
        }
        WorksAction::Trash {
            install_id,
            context_id,
            id,
            if_revision,
            idempotency_key,
        } => (
            "app_works_trash",
            json!({"install_id": install_id, "context_id": context_id, "id": id, "if_revision": if_revision, "idempotency_key": idempotency_key}),
        ),
        WorksAction::Restore {
            install_id,
            context_id,
            id,
            if_revision,
            idempotency_key,
        } => (
            "app_works_restore",
            json!({"install_id": install_id, "context_id": context_id, "id": id, "if_revision": if_revision, "idempotency_key": idempotency_key}),
        ),
        WorksAction::RequestReview {
            install_id,
            context_id,
            id,
            revision,
        } => (
            "app_works_request_review",
            json!({"install_id": install_id, "context_id": context_id, "id": id, "revision": revision}),
        ),
        WorksAction::Review {
            install_id,
            context_id,
            id,
            revision,
            decision,
            note,
        } => (
            "app_works_review",
            json!({"install_id": install_id, "context_id": context_id, "id": id, "revision": revision, "decision": decision, "note": note}),
        ),
        WorksAction::Export {
            install_id,
            context_id,
            id,
            revision,
            format,
        } => {
            let mut params = json!({"install_id": install_id, "context_id": context_id, "id": id, "revision": revision});
            // Omit `format` when absent so the daemon applies its html default
            // instead of receiving an explicit null (refused as invalid).
            if let Some(format) = format {
                params["format"] = json!(format);
            }
            ("app_works_export", params)
        }
    })
}

/// Works create/patch bodies: a near-64 KiB revision plus kind, title,
/// tags or operations exceeds the 64 KiB audience bound, so Works verbs get
/// their own 80 KiB reader. Other commands keep `read_audience_json`.
fn read_works_json(path: &Path, kind: &str) -> Result<serde_json::Value> {
    read_bounded_json(path, kind, 80 * 1024)
}

fn read_audience_json(path: &Path, kind: &str) -> Result<serde_json::Value> {
    read_bounded_json(path, kind, 64 * 1024)
}

fn read_bounded_json(path: &Path, kind: &str, limit: usize) -> Result<serde_json::Value> {
    use std::io::Read;
    let file = std::fs::File::open(path)
        .map_err(|e| Error::invalid("app_audience", format!("cannot open {kind}: {e}")))?;
    let mut bytes = Vec::new();
    file.take((limit + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| Error::invalid("app_audience", format!("cannot read {kind}: {e}")))?;
    if bytes.len() > limit {
        return Err(Error::invalid(
            "app_audience",
            format!("{kind} JSON exceeds {}KiB", limit / 1024),
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
        ContentAction::AutonomySet { mode } => ("crm_autonomy_set", json!({"mode":mode})),
        ContentAction::AutonomyShow => ("crm_autonomy_show", json!({})),
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
pub(crate) enum AssistantAction {
    /// List the actions this installation declares for the current context.
    Actions {
        /// Installation id; omitted means the live turn's verified App scope.
        install_id: Option<String>,
        #[arg(long)]
        context_id: Option<String>,
        #[arg(long)]
        message: Option<String>,
        #[arg(long)]
        token: Option<String>,
    },
    Invoke {
        /// Installation id; omitted means the live turn's verified App scope.
        install_id: Option<String>,
        #[arg(long)]
        context_id: Option<String>,
        #[arg(long)]
        message: Option<String>,
        #[arg(long)]
        token: Option<String>,
        #[arg(long)]
        action_id: String,
        /// Idempotency key; omitted derives one from the turn, action and input.
        #[arg(long)]
        operation_id: Option<String>,
        /// Inline JSON, `@file`, a file path, or `-` for stdin.
        #[arg(long)]
        input: String,
        /// Poll until the operation leaves `running`.
        #[arg(long)]
        wait: bool,
    },
    OperationShow {
        /// Installation id; omitted means the live turn's verified App scope.
        install_id: Option<String>,
        #[arg(long)]
        context_id: Option<String>,
        #[arg(long)]
        message: Option<String>,
        #[arg(long)]
        token: Option<String>,
        #[arg(long)]
        operation_id: String,
    },
    /// Operator decision on one pending permission request.
    Decide {
        install_id: String,
        #[arg(long)]
        context_id: String,
        #[arg(long)]
        operation_id: String,
        /// The operation revision the decision was made against.
        #[arg(long)]
        expected_revision: u64,
        #[arg(long, value_parser = ["allow_once", "allow_always", "deny"])]
        decision: String,
    },
}

#[derive(Subcommand)]
pub(crate) enum CatalogAction {
    /// Selected hosted workspace only: inspect an immutable published release; never uploads local files.
    ReleaseCheck {
        listing: String,
        #[arg(long)]
        version: String,
        #[arg(long)]
        digest: String,
        #[arg(long, requires_all = ["expected_digest", "expected_generation"])]
        install_id: Option<String>,
        #[arg(long, requires = "install_id")]
        expected_digest: Option<String>,
        #[arg(long, requires = "install_id")]
        expected_generation: Option<String>,
    },
    /// Selected hosted workspace only: consent to the exact release-check JSON review.
    ReleaseInstall {
        listing: String,
        #[arg(long)]
        version: String,
        #[arg(long)]
        digest: String,
        #[arg(long)]
        operation_id: String,
        #[arg(long)]
        review: PathBuf,
    },
    /// Selected hosted workspace only: update exact old bytes/generation to a reviewed published release.
    ReleaseUpdate {
        install_id: String,
        listing: String,
        #[arg(long)]
        version: String,
        #[arg(long)]
        digest: String,
        #[arg(long)]
        expected_digest: String,
        #[arg(long)]
        expected_generation: String,
        #[arg(long)]
        operation_id: String,
        #[arg(long)]
        review: PathBuf,
    },
    /// Selected hosted workspace only: revoke package approval, not business data or run recovery.
    ReleaseRevoke {
        install_id: String,
        #[arg(long)]
        digest: String,
        #[arg(long)]
        expected_generation: String,
        #[arg(long)]
        operation_id: String,
    },
    /// Selected hosted workspace only: read a durable receipt; dispatched is not executed.
    Operation { operation_id: String },
    /// Selected hosted workspace only: discover the pending operation as a current owner.
    Pending { app: String },
    /// Selected hosted workspace only: observe/cancel preparation; never redispatch a mutation.
    Reconcile { operation_id: String },
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
    Install {
        source: String,
        /// Refuse unless the resolved bundle digest equals this value, as
        /// returned by `install-check`. Omitted: install whatever resolves.
        #[arg(long)]
        expected_digest: Option<String>,
        /// Catalogue id, with `--catalogue-version` and `--catalogue-digest`.
        #[arg(long)]
        catalogue_id: Option<String>,
        #[arg(long)]
        catalogue_version: Option<String>,
        #[arg(long)]
        catalogue_digest: Option<String>,
        /// `manual` clears catalogue provenance on a non-catalogue install.
        #[arg(long)]
        catalogue_resolution: Option<String>,
    },
    /// Read-only: resolve and validate a bundle exactly as install would and
    /// return its digest, name, version and file list. Writes nothing.
    InstallCheck {
        source: String,
        #[arg(long)]
        catalogue_id: Option<String>,
        #[arg(long)]
        catalogue_version: Option<String>,
        #[arg(long)]
        catalogue_digest: Option<String>,
        #[arg(long)]
        catalogue_resolution: Option<String>,
    },
    /// Replace one exact workspace bundle while keeping its installation ID.
    /// The current digest and catalog generation must match the inspected row.
    UpgradeCheck {
        install_id: String,
        source: String,
        #[arg(long)]
        expected_digest: String,
        #[arg(long)]
        expected_generation: String,
        #[arg(long)]
        catalogue_id: Option<String>,
        #[arg(long)]
        catalogue_version: Option<String>,
        #[arg(long)]
        catalogue_digest: Option<String>,
        /// `manual` when this source is not a catalogue release.
        #[arg(long)]
        catalogue_resolution: Option<String>,
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
        #[arg(long)]
        catalogue_id: Option<String>,
        #[arg(long)]
        catalogue_version: Option<String>,
        #[arg(long)]
        catalogue_digest: Option<String>,
        #[arg(long)]
        catalogue_resolution: Option<String>,
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
        /// Registered PM whose workers execute this run. Omitted for a
        /// host-execution workflow (CAD-1171), which runs no worker.
        #[arg(long)]
        owner_pm: Option<String>,
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

/// Bounded JSON object for `app assistant invoke`: inline, `@file`, a path,
/// or `-` for stdin. The source never changes the 32 KiB bound.
fn parse_assistant_input(state_dir: &Path, raw: &str) -> Result<serde_json::Value> {
    use std::io::Read;
    const MAX_INPUT_BYTES: usize = 32 * 1024;
    let bytes = if raw == "-" {
        let mut bytes = Vec::new();
        std::io::stdin()
            .lock()
            .take((MAX_INPUT_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|e| {
                Error::invalid(
                    "app_assistant_input",
                    format!("cannot read assistant input from stdin: {e}"),
                )
            })?;
        bytes
    } else if raw.trim_start().starts_with('{') {
        raw.as_bytes().to_vec()
    } else {
        let path = Path::new(raw.strip_prefix('@').unwrap_or(raw));
        let file = cadence_agent::master::open_command_file(state_dir, path)?;
        let mut bytes = Vec::new();
        file.take((MAX_INPUT_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|e| {
                Error::invalid(
                    "app_assistant_input",
                    format!("cannot read assistant input: {e}"),
                )
            })?;
        bytes
    };
    if bytes.len() > MAX_INPUT_BYTES {
        return Err(Error::invalid(
            "app_assistant_input",
            "assistant input JSON exceeds 32KiB",
        ));
    }
    let input: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|_| Error::invalid("app_assistant_input", "input must be a JSON object"))?;
    if !input.is_object() {
        return Err(Error::invalid(
            "app_assistant_input",
            "input must be a JSON object",
        ));
    }
    Ok(input)
}

#[derive(Clone, Debug)]
struct AssistantScope {
    install_id: String,
    context_id: String,
    message: String,
    token: String,
}

impl AssistantScope {
    /// The daemon's identity fields plus one call's own fields.
    fn params(&self, extra: serde_json::Value) -> serde_json::Value {
        let mut params = json!({
            "install_id": self.install_id,
            "context_id": self.context_id,
            "message": self.message,
            "token": self.token,
        });
        if let (Some(params), Some(extra)) = (params.as_object_mut(), extra.as_object()) {
            for (key, value) in extra {
                params.insert(key.clone(), value.clone());
            }
        }
        params
    }
}

fn assistant_scope(
    state_dir: &Path,
    install_id: Option<&str>,
    context_id: Option<&str>,
    message: Option<&str>,
    token: Option<&str>,
) -> Result<AssistantScope> {
    // `agent_identity` derives the caller from the connection, exactly as
    // `cadence self` does. An operator outside a pane has no live turn and
    // must pass every identity flag.
    let live = client::rpc(state_dir, "agent_identity", json!({})).ok();
    resolve_assistant_scope(live.as_ref(), install_id, context_id, message, token)
}

/// Explicit flags win; anything omitted comes from the caller's one live
/// turn and its verified App stamp. Ambiguity refuses instead of guessing.
fn resolve_assistant_scope(
    live: Option<&serde_json::Value>,
    install_id: Option<&str>,
    context_id: Option<&str>,
    message: Option<&str>,
    token: Option<&str>,
) -> Result<AssistantScope> {
    let turns = live
        .and_then(|value| value.get("running"))
        .and_then(serde_json::Value::as_array);
    let turn = match (message, token) {
        (Some(id), Some(_)) => {
            turns.and_then(|rows| rows.iter().find(|row| row["id"].as_str() == Some(id)))
        }
        (None, None) => match turns.map(Vec::as_slice) {
            Some([only]) => Some(only),
            Some(rows) if !rows.is_empty() => {
                return Err(Error::rejected(format!(
                    "this agent holds {} running turns — pass --message and --token to choose one",
                    rows.len()
                )))
            }
            _ => None,
        },
        _ => {
            return Err(Error::rejected(
                "--message and --token are given together or not at all",
            ))
        }
    };
    let message =
        match (message, turn.and_then(|row| row["id"].as_str())) {
            (Some(id), _) => id.to_string(),
            (None, Some(id)) => id.to_string(),
            (None, None) if live.is_none() => return Err(Error::rejected(
                "this process sees no live agent turn — pass --message, --token and the install id",
            )),
            (None, None) => {
                return Err(Error::rejected(
                    "no running turn to derive the app scope from — pass --message and --token",
                ))
            }
        };
    let token = match (token, turn.and_then(|row| row["turn_id"].as_str())) {
        (Some(token), _) => token.to_string(),
        (None, Some(token)) => token.to_string(),
        (None, None) => {
            return Err(Error::rejected(
                "no live turn token — pass --token together with --message",
            ))
        }
    };
    let install_id = install_id
        .map(str::to_string)
        .or_else(|| {
            turn.and_then(|row| row["app_install_id"].as_str())
                .map(str::to_string)
        })
        .ok_or_else(|| {
            Error::rejected("this turn carries no App installation — pass the install id")
        })?;
    let context_id = context_id
        .map(str::to_string)
        .or_else(|| {
            turn.and_then(|row| row["app_context_id"].as_str())
                .map(str::to_string)
        })
        .ok_or_else(|| Error::rejected("this turn carries no App context — pass --context-id"))?;
    Ok(AssistantScope {
        install_id,
        context_id,
        message,
        token,
    })
}

/// One idempotency key for one (turn, action, input), so re-running the
/// same command replays the operation instead of creating a second effect.
fn derive_operation_id(message: &str, action_id: &str, input: &serde_json::Value) -> String {
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    digest.update(message.as_bytes());
    digest.update(b"\0");
    digest.update(action_id.as_bytes());
    digest.update(b"\0");
    digest.update(serde_json::to_vec(input).expect("assistant input serializes"));
    let hex = format!("{:x}", digest.finalize());
    format!("op-{}", &hex[..24])
}

fn wait_assistant_operation(
    state_dir: &Path,
    scope: &AssistantScope,
    operation_id: &str,
    first: serde_json::Value,
) -> Result<serde_json::Value> {
    use std::time::{Duration, Instant};
    let seconds = std::env::var("CADENCE_ASSISTANT_WAIT_SECS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(120u64);
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut current = first;
    loop {
        let status = current
            .get("operation")
            .and_then(|operation| operation.get("status"))
            .or_else(|| current.get("status"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string();
        if status != "running" {
            let mut value = current;
            if status == "pending_permission" {
                let revision = value
                    .get("operation")
                    .and_then(|operation| operation.get("revision"))
                    .and_then(serde_json::Value::as_i64)
                    .unwrap_or_default();
                if let Some(object) = value.as_object_mut() {
                    object.insert(
                        "wait_note".into(),
                        json!(format!(
                            "operation {operation_id} waits for an operator decision: \
                             `cadence app assistant decide {} --context-id {} \
                             --operation-id {operation_id} --expected-revision {revision} \
                             --decision allow_once` (or `allow_always` / `deny`)",
                            scope.install_id, scope.context_id
                        )),
                    );
                }
            }
            return Ok(value);
        }
        if Instant::now() >= deadline {
            return Err(Error::rejected(format!(
                "operation {operation_id} is still running; read it again with \
                 `cadence app assistant operation-show {} --operation-id {operation_id}`",
                scope.install_id
            )));
        }
        std::thread::sleep(Duration::from_millis(500));
        current = client::rpc(
            state_dir,
            "app_assistant_operation_show",
            scope.params(json!({"operation_id": operation_id})),
        )?;
    }
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
            });
            if let Some(owner) = owner_pm {
                params["owner_pm"] = json!(owner);
            }
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
                CatalogAction::ReleaseCheck { .. }
                | CatalogAction::ReleaseInstall { .. }
                | CatalogAction::ReleaseUpdate { .. }
                | CatalogAction::ReleaseRevoke { .. }
                | CatalogAction::Operation { .. }
                | CatalogAction::Pending { .. }
                | CatalogAction::Reconcile { .. } => {
                    return Err(Error::usage("published-release lifecycle commands require a selected remote workspace (--org); local app/catalog commands are unchanged"));
                }
                CatalogAction::Approve { install_id, digest } => (
                    "app_local_install_approve",
                    json!({"install_id":install_id,"digest":digest}),
                ),
                CatalogAction::Revoke { install_id, digest } => (
                    "app_local_install_revoke",
                    json!({"install_id":install_id,"digest":digest}),
                ),
                CatalogAction::Install {
                    source,
                    expected_digest,
                    catalogue_id,
                    catalogue_version,
                    catalogue_digest,
                    catalogue_resolution,
                } => {
                    let source = install_source(source)?;
                    let mut params = json!({"source":source});
                    if let Some(digest) = expected_digest {
                        params["expected_digest"] = json!(digest);
                    }
                    catalogue_params(
                        &mut params,
                        catalogue_id,
                        catalogue_version,
                        catalogue_digest,
                        catalogue_resolution,
                    )?;
                    ("app_workspace_install", params)
                }
                CatalogAction::InstallCheck {
                    source,
                    catalogue_id,
                    catalogue_version,
                    catalogue_digest,
                    catalogue_resolution,
                } => {
                    let source = install_source(source)?;
                    let mut params = json!({"source":source});
                    catalogue_params(
                        &mut params,
                        catalogue_id,
                        catalogue_version,
                        catalogue_digest,
                        catalogue_resolution,
                    )?;
                    ("app_workspace_install_check", params)
                }
                CatalogAction::Upgrade {
                    install_id,
                    source,
                    expected_digest,
                    expected_generation,
                    expected_new_digest,
                    request_id,
                    catalogue_id,
                    catalogue_version,
                    catalogue_digest,
                    catalogue_resolution,
                } => {
                    let source = if source.contains("://")
                        || source.starts_with("git@")
                        || source.starts_with("builtin:")
                    {
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
                    let mut params = json!({"install_id":install_id,"source":source,
                        "expected_digest":expected_digest,"expected_generation":expected_generation,
                        "expected_new_digest":expected_new_digest,"request_id":request_id});
                    catalogue_params(
                        &mut params,
                        catalogue_id,
                        catalogue_version,
                        catalogue_digest,
                        catalogue_resolution,
                    )?;
                    ("app_workspace_upgrade", params)
                }
                CatalogAction::UpgradeCheck {
                    install_id,
                    source,
                    expected_digest,
                    expected_generation,
                    catalogue_id,
                    catalogue_version,
                    catalogue_digest,
                    catalogue_resolution,
                } => {
                    let source = install_source(source)?;
                    let mut params = json!({"install_id":install_id,"source":source,
                        "expected_digest":expected_digest,"expected_generation":expected_generation});
                    catalogue_params(
                        &mut params,
                        catalogue_id,
                        catalogue_version,
                        catalogue_digest,
                        catalogue_resolution,
                    )?;
                    ("app_workspace_upgrade_check", params)
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
        AppAction::Works { action } => {
            let (method, params) = works_params(action)?;
            client::rpc(state_dir, method, params)?
        }
        AppAction::Assistant { action } => {
            let (method, params, wait): (
                &'static str,
                serde_json::Value,
                Option<(AssistantScope, String)>,
            ) = match action {
                AssistantAction::Actions {
                    install_id,
                    context_id,
                    message,
                    token,
                } => {
                    let scope = assistant_scope(
                        state_dir,
                        install_id.as_deref(),
                        context_id.as_deref(),
                        message.as_deref(),
                        token.as_deref(),
                    )?;
                    ("app_assistant_actions", scope.params(json!({})), None)
                }
                AssistantAction::Invoke {
                    install_id,
                    context_id,
                    message,
                    token,
                    action_id,
                    operation_id,
                    input,
                    wait,
                } => {
                    // The ticket's refusal order: malformed or oversized
                    // input never reaches the daemon, not even the read that
                    // resolves the live turn.
                    let input = parse_assistant_input(state_dir, input)?;
                    let scope = assistant_scope(
                        state_dir,
                        install_id.as_deref(),
                        context_id.as_deref(),
                        message.as_deref(),
                        token.as_deref(),
                    )?;
                    let operation_id = match operation_id {
                        Some(id) => id.clone(),
                        None => derive_operation_id(&scope.message, action_id, &input),
                    };
                    let params = scope.params(json!({
                        "action_id": action_id,
                        "operation_id": operation_id,
                        "input": input,
                    }));
                    let waiting = wait.then(|| (scope.clone(), operation_id));
                    ("app_assistant_invoke", params, waiting)
                }
                AssistantAction::OperationShow {
                    install_id,
                    context_id,
                    message,
                    token,
                    operation_id,
                } => {
                    let scope = assistant_scope(
                        state_dir,
                        install_id.as_deref(),
                        context_id.as_deref(),
                        message.as_deref(),
                        token.as_deref(),
                    )?;
                    (
                        "app_assistant_operation_show",
                        scope.params(json!({"operation_id":operation_id})),
                        None,
                    )
                }
                AssistantAction::Decide {
                    install_id,
                    context_id,
                    operation_id,
                    expected_revision,
                    decision,
                } => (
                    "app_assistant_decision",
                    json!({
                        "install_id": install_id,
                        "context_id": context_id,
                        "operation_id": operation_id,
                        "expected_revision": expected_revision,
                        "decision": decision,
                    }),
                    None,
                ),
            };
            let result = client::rpc(state_dir, method, params)?;
            match wait {
                Some((scope, operation_id)) => {
                    wait_assistant_operation(state_dir, &scope, &operation_id, result)?
                }
                None => result,
            }
        }
        AppAction::FrameKit {
            out,
            bundle,
            parent_origin,
        } => {
            return cadence_agent::app_preview_export::write_frame_kit(
                out,
                bundle.as_deref(),
                parent_origin,
            );
        }
        AppAction::Fixtures { bundle, json } => {
            return cadence_agent::app_preview_export::write_fixtures(bundle, *json);
        }
        AppAction::Dev {
            name,
            source,
            package,
            port,
            host,
            allow_host,
        } => {
            return run_dev(
                name.as_deref(),
                source.as_deref(),
                package.as_deref(),
                *port,
                host,
                allow_host,
            );
        }
        // CAD-1270: `check` is dispatched before org/state-dir
        // resolution — it never reaches the daemon path.
        AppAction::Check { .. } => unreachable!("app check dispatches before state-dir resolution"),
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

/// CAD-1270 `cadence app check <dir>`: the offline bundle validator.
/// Runs before org/state-dir resolution — it opens no state dir, daemon
/// socket or PM; the only filesystem it reads is the named directory. A
/// `builtin:` source gets the retired refusal (CAD-1422). The JSON
/// report prints to stdout; the exit code is the verdict (0 pass, 3
/// refusal — `rejected` in the kind table).
pub(super) fn run_check(source: &str, _json: bool) -> Result<i32> {
    let report = cadence_agent::issue::app_catalog::workspace::check_offline(source)?;
    let ok = report["ok"].as_bool() == Some(true);
    print_json(&report);
    Ok(if ok { 0 } else { 3 })
}

/// No production state, PM, socket or app approval is accessed here.
pub(super) fn run_dev(
    name: Option<&str>,
    source: Option<&Path>,
    package: Option<&Path>,
    port: u16,
    host: &str,
    allow_hosts: &[String],
) -> Result<i32> {
    if let Some(package) = package {
        if !allow_hosts.is_empty() {
            return Err(Error::invalid(
                "app_dev_allow_host",
                "--allow-host is only available in legacy --source mode",
            ));
        }
        if !matches!(host, "127.0.0.1" | "localhost" | "::1") {
            return Err(Error::invalid(
                "app_dev_host",
                "package previews must bind loopback",
            ));
        }
        return run_package_dev(package, port, host, true);
    }
    let name = name.ok_or_else(|| {
        Error::invalid(
            "app_dev_mode",
            "pass a preview name with --source, or --package <dir>",
        )
    })?;
    let source = source.ok_or_else(|| {
        Error::invalid(
            "app_dev_mode",
            "pass a preview name with --source, or --package <dir>",
        )
    })?;
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
    copy_preview_env(&mut command);
    let status = cadence_agent::reaper::status(&mut command)
        .map_err(|e| Error::internal(format!("app development harness: {e}")))?;
    Ok(status.code().unwrap_or(1))
}

struct AppPackageContract {
    app: String,
    dev: String,
    check: Option<String>,
}

/// An app-owned preview: the package's `package.json` declares the app
/// name and the commands; Cadence only validates the package and runs
/// them. No daemon, state dir, PM, socket or approval is read.
fn run_package_dev(root: &Path, port: u16, host: &str, exec_child: bool) -> Result<i32> {
    let root = root
        .canonicalize()
        .map_err(|e| Error::invalid("app_dev_package", format!("package root: {e}")))?;
    let contract = package_contract(&root)?;
    if let Some(check) = contract.check.as_deref() {
        let status = shell_status(check, &root, &contract.app, port, host)?;
        if !status.success() {
            return Ok(status.code().unwrap_or(1));
        }
    }
    let mut child = dev_command(&contract.dev, &root, &contract.app, port, host)?;
    if exec_child {
        use std::os::unix::process::CommandExt;
        let error = child.exec();
        eprintln!("app dev: {error}");
        return Ok(launch_error_code(&error));
    }
    match cadence_agent::reaper::status(&mut child) {
        Ok(status) => Ok(status.code().unwrap_or(1)),
        Err(error) => {
            eprintln!("app dev: {error}");
            Ok(launch_error_code(&error))
        }
    }
}

fn launch_error_code(error: &std::io::Error) -> i32 {
    if error.kind() == std::io::ErrorKind::NotFound {
        127
    } else {
        126
    }
}

fn package_contract(root: &Path) -> Result<AppPackageContract> {
    let manifest = root.join("package.json");
    let bytes = std::fs::read(&manifest).map_err(|e| {
        Error::invalid(
            "app_dev_package",
            format!("read {}: {e}", manifest.display()),
        )
    })?;
    let json: Value = serde_json::from_slice(&bytes)
        .map_err(|e| Error::invalid("app_dev_package", format!("package.json: {e}")))?;
    let cadence = json
        .get("cadence")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            Error::invalid(
                "app_dev_package",
                "package.json needs a cadence object with app and dev",
            )
        })?;
    let app = cadence
        .get("app")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::invalid("app_dev_package", "cadence.app must be a string"))?;
    cadence_agent::proto::identifier(app, "app name")?;
    let dev = package_command(cadence.get("dev"), "cadence.dev", true)?;
    let check = match cadence.get("check") {
        Some(value) => Some(package_command(Some(value), "cadence.check", false)?),
        None => None,
    };
    if !root.join("app/app.md").is_file() {
        return Err(Error::invalid(
            "app_dev_package",
            "package needs app/app.md",
        ));
    }
    Ok(AppPackageContract {
        app: app.to_owned(),
        dev,
        check,
    })
}

fn package_command(value: Option<&Value>, field: &str, one_command: bool) -> Result<String> {
    let text = value
        .and_then(Value::as_str)
        .ok_or_else(|| Error::invalid("app_dev_package", format!("{field} must be a string")))?;
    if text.trim().is_empty() || text.len() > 4096 {
        return Err(Error::invalid(
            "app_dev_package",
            format!("{field} must be 1..=4096 bytes"),
        ));
    }
    if one_command && parse_dev_command(text).is_err() {
        return Err(Error::invalid(
            "app_dev_package",
            "cadence.dev must be one command; put multiple steps in a script",
        ));
    }
    Ok(text.to_owned())
}

/// A command Cadence can prefix with `exec` without truncating a shell list.
/// Quoted metacharacters are ordinary arguments; unquoted separators,
/// redirects, subshells, command substitution and newlines are refused.
struct DevCommand {
    envs: Vec<(String, String)>,
    argv: Vec<String>,
}

fn parse_dev_command(text: &str) -> Result<DevCommand> {
    let words = split_shell_words(text)?;
    let mut envs = Vec::new();
    let mut index = 0;
    while let Some((key, value)) = words.get(index).and_then(|word| assignment(word)) {
        envs.push((key.to_owned(), value.to_owned()));
        index += 1;
    }
    if index == words.len() {
        return Err(Error::invalid(
            "app_dev_package",
            "cadence.dev needs a command after its assignments",
        ));
    }
    Ok(DevCommand {
        envs,
        argv: words[index..].to_vec(),
    })
}

fn assignment(word: &str) -> Option<(&str, &str)> {
    let (key, value) = word.split_once('=')?;
    let mut chars = key.chars();
    let first = chars.next()?;
    if !(first.is_ascii_alphabetic() || first == '_')
        || !chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    {
        return None;
    }
    Some((key, value))
}

/// Split one command into words without invoking a shell. Quoted separators
/// stay in the argument; unquoted separators, redirects, subshells, command
/// substitution and newlines refuse. Expansions are not performed; an app
/// that needs a shell belongs in a script.
fn split_shell_words(text: &str) -> Result<Vec<String>> {
    let chars: Vec<char> = text.chars().collect();
    let mut words = Vec::new();
    let mut current = String::new();
    let mut started = false;
    let mut quote: Option<char> = None;
    let mut index = 0;
    while index < chars.len() {
        let ch = chars[index];
        if ch == '\n' || ch == '\r' {
            return Err(Error::invalid(
                "app_dev_package",
                "cadence.dev must be one command; newlines are refused",
            ));
        }
        if let Some(active) = quote {
            if ch == active {
                quote = None;
            } else if active == '"' && ch == '\\' {
                let next = chars.get(index + 1).ok_or_else(|| {
                    Error::invalid("app_dev_package", "cadence.dev ends with a backslash")
                })?;
                if *next == '\n' || *next == '\r' {
                    return Err(Error::invalid(
                        "app_dev_package",
                        "cadence.dev must be one command; newlines are refused",
                    ));
                }
                if matches!(*next, '$' | '`' | '"' | '\\') {
                    current.push(*next);
                    index += 1;
                } else {
                    current.push('\\');
                }
            } else if ch == '`' || (ch == '$' && chars.get(index + 1) == Some(&'(')) {
                return Err(Error::invalid(
                    "app_dev_package",
                    "cadence.dev does not allow command substitution",
                ));
            } else {
                current.push(ch);
            }
            started = true;
            index += 1;
            continue;
        }
        match ch {
            '\'' | '"' => {
                quote = Some(ch);
                started = true;
            }
            '\\' => {
                let next = chars.get(index + 1).ok_or_else(|| {
                    Error::invalid("app_dev_package", "cadence.dev ends with a backslash")
                })?;
                current.push(*next);
                started = true;
                index += 1;
            }
            ' ' | '\t' => {
                if started {
                    words.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            ';' | '&' | '|' | '<' | '>' | '`' | '(' | ')' => {
                return Err(Error::invalid(
                    "app_dev_package",
                    "cadence.dev must be one command; put multiple steps in a script",
                ));
            }
            '$' if chars.get(index + 1) == Some(&'(') => {
                return Err(Error::invalid(
                    "app_dev_package",
                    "cadence.dev does not allow command substitution",
                ));
            }
            _ => {
                current.push(ch);
                started = true;
            }
        }
        index += 1;
    }
    if quote.is_some() {
        return Err(Error::invalid(
            "app_dev_package",
            "cadence.dev has an unterminated quote",
        ));
    }
    if started {
        words.push(current);
    }
    Ok(words)
}

fn configure_app_env(
    child: &mut std::process::Command,
    root: &Path,
    app: &str,
    port: u16,
    host: &str,
) {
    child
        .current_dir(root)
        .env_clear()
        .env("CADENCE_APP_ROOT", root)
        .env("CADENCE_APP_NAME", app)
        .env("CADENCE_APP_PORT", port.to_string())
        .env("CADENCE_APP_HOST", host)
        .env("CADENCE_APP_CONTRACT", "1");
    copy_preview_env(child);
}

fn shell_base(root: &Path, app: &str, port: u16, host: &str) -> std::process::Command {
    let mut child = std::process::Command::new("sh");
    configure_app_env(&mut child, root, app, port, host);
    child
}

/// Run the one parsed `dev` command directly, with leading `NAME=value`
/// assignments applied to its environment. No shell means `exec` replaces
/// Cadence cleanly and signals reach the app's own trap.
fn dev_command(
    command: &str,
    root: &Path,
    app: &str,
    port: u16,
    host: &str,
) -> Result<std::process::Command> {
    let DevCommand { envs, argv } = parse_dev_command(command)?;
    let mut child = std::process::Command::new(&argv[0]);
    child.args(&argv[1..]);
    configure_app_env(&mut child, root, app, port, host);
    for (key, value) in envs {
        child.env(key, value);
    }
    Ok(child)
}

fn shell_command(
    command: &str,
    root: &Path,
    app: &str,
    port: u16,
    host: &str,
) -> std::process::Command {
    let mut child = shell_base(root, app, port, host);
    child.arg("-c").arg(command);
    child
}

fn shell_status(
    command: &str,
    root: &Path,
    app: &str,
    port: u16,
    host: &str,
) -> Result<std::process::ExitStatus> {
    let mut child = shell_command(command, root, app, port, host);
    cadence_agent::reaper::status(&mut child)
        .map_err(|e| Error::internal(format!("app dev command: {e}")))
}

fn copy_preview_env(command: &mut std::process::Command) {
    for name in [
        "PATH",
        "HOME",
        "LANG",
        "TERM",
        "PNPM_HOME",
        "XDG_CACHE_HOME",
        "TMPDIR",
    ] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package_dir(dev: &str, check: Option<&str>) -> tempfile::TempDir {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("app")).unwrap();
        std::fs::create_dir_all(dir.path().join("scripts")).unwrap();
        std::fs::write(
            dir.path().join("app/app.md"),
            "---\napp: fixture\ntitle: Fixture\nversion: 0.0.1\n---\n",
        )
        .unwrap();
        let script = dir.path().join("scripts/dev.sh");
        std::fs::write(&script, format!("#!/bin/sh\nset -eu\n{dev}\n")).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut cadence = serde_json::json!({"app": "fixture", "dev": "scripts/dev.sh"});
        if let Some(check) = check {
            cadence["check"] = serde_json::json!(check);
        }
        std::fs::write(
            dir.path().join("package.json"),
            serde_json::json!({"cadence": cadence}).to_string(),
        )
        .unwrap();
        dir
    }

    #[test]
    fn package_dev_rejects_a_multi_step_dev_command() {
        let dir = package_dir("printf dev > dev.txt", None);
        std::fs::write(
            dir.path().join("package.json"),
            serde_json::json!({"cadence": {"app": "fixture", "dev": "printf a; printf b"}})
                .to_string(),
        )
        .unwrap();
        let error = run_dev(None, None, Some(dir.path()), 3191, "127.0.0.1", &[]).unwrap_err();
        assert!(error.to_string().contains("one command"), "{error}");
    }

    #[test]
    fn package_dev_preserves_quoted_arguments_and_leading_environment() {
        let dir = package_dir(":", None);
        std::fs::write(
            dir.path().join("package.json"),
            serde_json::json!({
                "cadence": {
                    "app": "fixture",
                    "dev": "GREETING=hello sh -c 'printf \"%s\" \"$GREETING & value\" > quoted.txt'"
                }
            })
            .to_string(),
        )
        .unwrap();
        let code = run_package_dev(dir.path(), 3191, "127.0.0.1", false).unwrap();
        assert_eq!(code, 0);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("quoted.txt")).unwrap(),
            "hello & value"
        );
    }

    #[test]
    fn package_dev_rejects_a_newline_separated_dev_command() {
        let dir = package_dir(":", None);
        std::fs::write(
            dir.path().join("package.json"),
            serde_json::json!({
                "cadence": {"app": "fixture", "dev": "scripts/dev.sh\nscripts/dev.sh"}
            })
            .to_string(),
        )
        .unwrap();
        let error = run_dev(None, None, Some(dir.path()), 3191, "127.0.0.1", &[]).unwrap_err();
        assert!(error.to_string().contains("one command"), "{error}");
    }

    #[test]
    fn parse_dev_command_preserves_double_quoted_backslashes() {
        let DevCommand { envs, argv } = parse_dev_command(r#"node "a\qb" "a\"b""#).unwrap();
        assert!(envs.is_empty());
        assert_eq!(argv[0], "node");
        assert_eq!(argv[1], r"a\qb");
        assert_eq!(argv[2], "a\"b");
    }

    #[test]
    fn package_dev_missing_executable_returns_127() {
        let dir = package_dir(":", None);
        std::fs::write(
            dir.path().join("package.json"),
            serde_json::json!({"cadence": {"app": "fixture", "dev": "does-not-exist-xyz"}})
                .to_string(),
        )
        .unwrap();
        let code = run_package_dev(dir.path(), 3191, "127.0.0.1", false).unwrap();
        assert_eq!(code, 127);
    }

    #[test]
    fn package_dev_runs_check_then_dev_with_the_contract_environment() {
        let dir = package_dir(
            "printf '%s' \"$CADENCE_APP_PORT\" > port.txt; \
             printf '%s' \"$CADENCE_APP_ROOT\" > root.txt; \
             printf '%s' \"$CADENCE_APP_NAME\" > name.txt; \
             printf '%s' \"$CADENCE_APP_CONTRACT\" > contract.txt; \
             printf dev > dev.txt",
            Some("test ! -e dev.txt && printf checked > check.txt"),
        );
        let code = run_package_dev(dir.path(), 3191, "127.0.0.1", false).unwrap();
        assert_eq!(code, 0);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("check.txt")).unwrap(),
            "checked"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("dev.txt")).unwrap(),
            "dev"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("port.txt")).unwrap(),
            "3191"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("root.txt")).unwrap(),
            dir.path().canonicalize().unwrap().display().to_string()
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("name.txt")).unwrap(),
            "fixture"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("contract.txt")).unwrap(),
            "1"
        );
    }

    #[test]
    fn package_dev_check_failure_does_not_spawn_dev() {
        let dir = package_dir(
            "printf dev > dev.txt",
            Some("printf check > check.txt; exit 7"),
        );
        let code = run_package_dev(dir.path(), 3191, "127.0.0.1", false).unwrap();
        assert_eq!(code, 7);
        assert!(dir.path().join("check.txt").is_file());
        assert!(!dir.path().join("dev.txt").exists());
    }

    #[test]
    fn package_dev_refuses_non_loopback_hosts_and_allow_host() {
        let dir = package_dir("printf dev > dev.txt", None);
        let error = run_dev(None, None, Some(dir.path()), 3191, "0.0.0.0", &[]).unwrap_err();
        assert!(error.to_string().contains("loopback"), "{error}");
        let error = run_dev(
            None,
            None,
            Some(dir.path()),
            3191,
            "127.0.0.1",
            &["example.invalid".into()],
        )
        .unwrap_err();
        assert!(error.to_string().contains("--allow-host"), "{error}");
        assert!(!dir.path().join("dev.txt").exists());
    }

    #[test]
    fn package_dev_requires_the_app_manifest_and_dev_command() {
        let dir = package_dir("printf dev > dev.txt", None);
        std::fs::remove_file(dir.path().join("app/app.md")).unwrap();
        let error = run_dev(None, None, Some(dir.path()), 3191, "127.0.0.1", &[]).unwrap_err();
        assert!(error.to_string().contains("app/app.md"), "{error}");

        let dir = package_dir("printf dev > dev.txt", None);
        std::fs::write(
            dir.path().join("package.json"),
            serde_json::json!({"cadence": {"app": "fixture"}}).to_string(),
        )
        .unwrap();
        let error = run_dev(None, None, Some(dir.path()), 3191, "127.0.0.1", &[]).unwrap_err();
        assert!(error.to_string().contains("cadence.dev"), "{error}");
    }

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

    /// CAD-1662: native Works verbs reach the native `app_works_*` methods
    /// with explicit install/context and no actor, reviewer or on-behalf-of
    /// field. Export omits `format` when absent so the daemon default applies.
    #[test]
    fn works_verbs_map_to_native_methods_without_actor_fields() {
        let (method, params) = works_params(&WorksAction::Export {
            install_id: "inst-1".into(),
            context_id: "ctx-1".into(),
            id: "doc-1".into(),
            revision: 3,
            format: None,
        })
        .unwrap();
        assert_eq!(method, "app_works_export");
        assert_eq!(
            params,
            json!({"install_id": "inst-1", "context_id": "ctx-1", "id": "doc-1", "revision": 3})
        );
        assert!(params.get("format").is_none());

        let dir = tempfile::tempdir().unwrap();
        let body = dir.path().join("body.json");
        std::fs::write(
            &body,
            r#"{"kind":"doc","title":"t","content":{"blocks":[]}}"#,
        )
        .unwrap();
        let (method, params) = works_params(&WorksAction::Create {
            install_id: "inst-1".into(),
            context_id: "ctx-1".into(),
            body,
            idempotency_key: "k-1".into(),
        })
        .unwrap();
        assert_eq!(method, "app_works_create");
        for forbidden in ["actor", "reviewer", "on_behalf_of", "session", "role"] {
            assert!(
                params.get(forbidden).is_none(),
                "{forbidden} must not be a CLI field"
            );
        }
        assert_eq!(params["install_id"], "inst-1");
        assert_eq!(params["idempotency_key"], "k-1");
    }

    /// CAD-1662: Works create/patch bodies are bounded by the 80 KiB Works
    /// reader, and an oversized body is refused before any daemon call.
    #[test]
    fn works_bodies_refuse_over_80kib_before_sending() {
        let dir = tempfile::tempdir().unwrap();
        let body = dir.path().join("body.json");
        let text = json!({"kind":"doc","title":"t","content":{"blocks":[{"type":"p","text":"界".repeat(30_000)}]}}).to_string();
        std::fs::write(&body, text).unwrap();
        let error = works_params(&WorksAction::Create {
            install_id: "inst-1".into(),
            context_id: "ctx-1".into(),
            body,
            idempotency_key: "k-1".into(),
        })
        .unwrap_err()
        .to_string();
        assert!(error.contains("exceeds 80KiB"), "{error}");
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

    fn live_turns(rows: serde_json::Value) -> serde_json::Value {
        json!({"alias": "agent", "running": rows})
    }

    #[test]
    fn assistant_scope_derives_from_one_live_turn() {
        let live = live_turns(json!([{
            "id": "msg-1", "turn_id": "tok-1", "task": "t1",
            "app_install_id": "inst-1", "app_context_id": "ctx-1"
        }]));
        let scope = resolve_assistant_scope(Some(&live), None, None, None, None).unwrap();
        assert_eq!(scope.install_id, "inst-1");
        assert_eq!(scope.context_id, "ctx-1");
        assert_eq!(scope.message, "msg-1");
        assert_eq!(scope.token, "tok-1");
        // The daemon still receives every identity field it validates.
        assert_eq!(
            scope.params(json!({"action_id": "customers.search"})),
            json!({"install_id":"inst-1","context_id":"ctx-1","message":"msg-1",
                   "token":"tok-1","action_id":"customers.search"})
        );
    }

    #[test]
    fn assistant_scope_flags_win_and_multiple_turns_refuse() {
        let live = live_turns(json!([
            {"id":"msg-1","turn_id":"tok-1","app_install_id":"inst-1","app_context_id":"ctx-1"},
            {"id":"msg-2","turn_id":"tok-2","app_install_id":"inst-2","app_context_id":"ctx-2"}
        ]));
        let error = resolve_assistant_scope(Some(&live), None, None, None, None)
            .expect_err("two live turns must refuse rather than guess");
        assert!(error.to_string().contains("--message"), "{error}");
        let scope = resolve_assistant_scope(
            Some(&live),
            Some("inst-x"),
            Some("ctx-x"),
            Some("msg-2"),
            Some("tok-2"),
        )
        .unwrap();
        assert_eq!(scope.install_id, "inst-x");
        assert_eq!(scope.context_id, "ctx-x");
        assert_eq!(scope.message, "msg-2");
    }

    #[test]
    fn assistant_scope_without_a_turn_needs_every_flag() {
        let error = resolve_assistant_scope(None, None, None, None, None)
            .expect_err("no live turn and no flags must refuse");
        assert!(error.to_string().contains("--message"), "{error}");
        let scope = resolve_assistant_scope(
            None,
            Some("inst-1"),
            Some("ctx-1"),
            Some("msg-1"),
            Some("tok-1"),
        )
        .unwrap();
        assert_eq!(scope.token, "tok-1");
        // A live turn with no App stamp cannot invent an installation.
        let live = live_turns(json!([{"id":"msg-1","turn_id":"tok-1"}]));
        let error = resolve_assistant_scope(Some(&live), None, None, None, None)
            .expect_err("no stamp must refuse");
        assert!(error.to_string().contains("install"), "{error}");
    }

    #[test]
    fn operation_ids_are_stable_per_turn_action_and_input() {
        let input = json!({"query": "acme"});
        let first = derive_operation_id("msg-1", "customers.search", &input);
        assert_eq!(
            first,
            derive_operation_id("msg-1", "customers.search", &input)
        );
        assert_ne!(
            first,
            derive_operation_id("msg-2", "customers.search", &input)
        );
        assert_ne!(
            first,
            derive_operation_id("msg-1", "customers.show", &input)
        );
        assert_ne!(
            first,
            derive_operation_id("msg-1", "customers.search", &json!({"query": "other"}))
        );
        assert!(first.starts_with("op-"), "{first}");
    }

    #[test]
    fn assistant_input_accepts_inline_and_at_file() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path();
        assert_eq!(
            parse_assistant_input(state, r#"{"query":"acme"}"#).unwrap(),
            json!({"query":"acme"})
        );
        let file = dir.path().join("input.json");
        std::fs::write(&file, r#"{"limit":3}"#).unwrap();
        let at = format!("@{}", file.display());
        assert_eq!(
            parse_assistant_input(state, &at).unwrap(),
            json!({"limit":3})
        );
        assert_eq!(
            parse_assistant_input(state, file.to_str().unwrap()).unwrap(),
            json!({"limit":3})
        );
        assert!(parse_assistant_input(state, r#"[1,2]"#).is_err());
        assert!(parse_assistant_input(state, "{not json").is_err());
    }
}
