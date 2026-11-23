//! CAD-535: `cadence app` — moved verbatim from src/main.rs.

use super::*;

mod dev;
mod dispatch;
mod params;
mod runs;

pub(super) use dev::run_dev;
#[cfg(test)]
use dev::{parse_dev_command, run_package_dev, DevCommand};
pub(super) use dispatch::{run, run_check, run_inspect};
#[cfg(test)]
use params::{record_params, works_params};
#[cfg(test)]
use runs::{derive_operation_id, parse_assistant_input, read_run_inputs, resolve_assistant_scope};

/// `cadence app` verbs (CAD-547). `install`/`update`/`set`/`remove`
/// write the tracker directly — one commit each, `Actor:` recorded —
/// `ls`/`show` read it; `approve` is the operator's daemon gate, like
/// `workflow approve`.
#[derive(Subcommand)]
pub(crate) enum AppAction {
    /// [native daemon only] Configure exact publication connections for an installation or context.
    Binding {
        #[command(subcommand)]
        action: BindingAction,
    },
    /// [native daemon only] Stage and explicitly approve release of an independently reviewed artifact.
    Effect {
        #[command(subcommand)]
        action: EffectAction,
    },
    /// [native daemon only] Optional app-owned content settings; creates no worker or provider authority.
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
    /// Inspect complete package integrity without claiming target compatibility.
    /// Observes Native requirements; installs, approves and consents to nothing.
    Inspect {
        /// Package directory.
        source: String,
        /// Emit the machine-readable inspection receipt.
        #[arg(long)]
        json: bool,
    },
    /// [native daemon only] Host-managed per-installation customer records with scoped revisions.
    /// For a hosted workspace (`--org`), use `app describe <app>` and `app call <app> records.*`.
    Record {
        #[command(subcommand)]
        action: RecordAction,
    },
    /// [native daemon only] Saved segments, exclusion lists, suppressions and frozen
    /// audiences over customer records (CAD-780). No mail is sent.
    /// For a hosted workspace (`--org`), use `app call <app> segments.*` or `audience.preview`.
    Audience {
        #[command(subcommand)]
        action: AudienceAction,
    },
    /// [native daemon only] Campaign email content — the scoped-chat inert assistant draft
    /// (CAD-1014): drafts a pending proposal the operator applies or
    /// discards. Never edits, approves or sends.
    /// For a hosted workspace (`--org`), use `app call <app> content.*` or `proposals.*`.
    Content {
        #[command(subcommand)]
        action: ContentAction,
    },
    /// [native daemon only] Works content (CAD-1662): native operator docs, decks and sheets over
    /// the durable store. Explicit install and context; no actor flags. Review
    /// needs two verified humans, and the daemon derives the actor.
    Works {
        #[command(subcommand)]
        action: WorksAction,
    },
    /// [native daemon only] CRM app assistant discovery, consented action invocation, and durable receipt reads.
    /// For a hosted workspace (`--org`), use `app describe <app>` and `app call <app> <operation>`.
    Assistant {
        #[command(subcommand)]
        action: AssistantAction,
    },
    /// List installed apps, or describe the actions one hosted app offers.
    Describe {
        /// Installed app name; omit to list apps.
        app: Option<String>,
    },
    /// Call a hosted app action using its declared arguments.
    Call {
        /// Installed app name.
        app: String,
        /// Action name from `app describe`.
        operation: String,
        /// Action arguments as one JSON object: inline, `@file`, a path, or `-` for stdin.
        #[arg(long, default_value = "{}")]
        args: String,
        /// Stable request handle; keep it and the same arguments if the outcome is uncertain.
        #[arg(long)]
        request: Option<String>,
        /// Reason shown with an action held for an owner's decision.
        #[arg(long)]
        reason: Option<String>,
    },
    /// Stable workspace installation IDs; execution and approval are separate.
    Catalog {
        #[command(subcommand)]
        action: CatalogAction,
    },
    /// [native daemon only] App-owned local runs with separate execution approval.
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
        /// Static preview only: inspect integrity without requiring Native support.
        #[arg(long, requires = "bundle")]
        integrity_only: bool,
    },
    /// Emit canonical screen PUSH fixtures from a local validated app bundle.
    Fixtures {
        /// Local app package directory.
        #[arg(long)]
        bundle: PathBuf,
        /// Require the machine-readable fixture envelope.
        #[arg(long)]
        json: bool,
        /// Synthetic fixtures only; does not prove target runtime compatibility.
        #[arg(long)]
        integrity_only: bool,
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
    /// Explicit per-member read access to an installation's exact
    /// conversations (CAD-2215). Operator only, through the daemon. The
    /// local CLI lists and revokes; creating a grant needs the board's
    /// authenticated session flow (a live owner plus the platform roster)
    /// and is deliberately not offered as a CLI credential handoff.
    Access {
        #[command(subcommand)]
        action: AccessAction,
    },
}

#[derive(Subcommand)]
pub(crate) enum AccessAction {
    /// List every grant (active and revoked) and the installation's live
    /// conversations, so an operator can see the exact conversation ids.
    #[command(visible_alias = "list")]
    Ls { install_id: String },
    /// Revoke one exact grant, or every grant this member holds for the
    /// installation when `--conversation` is omitted. Works without the
    /// member being present and needs no member session.
    Revoke {
        install_id: String,
        #[arg(long)]
        member: String,
        #[arg(long)]
        conversation: Option<String>,
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
    /// Retire a terminal legacy run's unrecorded team-role provenance.
    /// The run's state and history are unchanged; the app-team check
    /// stops holding its input values.
    RetireLegacy { run_id: String },
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
    /// Record accept or decline of one reviewed artifact. Does not publish.
    /// One decision per run; the same request replays and a different
    /// decision conflicts.
    Disposition {
        run_id: String,
        #[arg(long)]
        request_id: String,
        #[arg(long)]
        artifact_id: String,
        #[arg(long)]
        digest: String,
        #[arg(long, value_enum)]
        decision: RunDisposition,
    },
    /// Write one host issue for one 1-based proposal. Requires an accepted
    /// disposition of this artifact. The same request returns the same
    /// issue; any other proposal on the run conflicts.
    Ticket {
        run_id: String,
        #[arg(long)]
        request_id: String,
        #[arg(long)]
        artifact_id: String,
        #[arg(long)]
        digest: String,
        /// 1-based index into `## Proposed next steps`.
        #[arg(long)]
        index: u32,
        #[arg(long)]
        project: String,
    },
}

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub(crate) enum RunDisposition {
    Accept,
    Decline,
}

impl RunDisposition {
    fn as_str(self) -> &'static str {
        match self {
            Self::Accept => "accept",
            Self::Decline => "decline",
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
