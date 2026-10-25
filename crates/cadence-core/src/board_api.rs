//! The board API's response types (CAD-1792) and request bodies (CAD-1798):
//! one contract for the daemon's board, the UI and the hosted tenant-board
//! Worker. The daemon serializes the responses and parses the requests; their
//! JSON Schema is committed under `contracts/board-api/v1/` and the UI's
//! TypeScript is generated from it.
//!
//! A field the daemon passes through from another producer (a provider
//! report, a stored session) stays a `serde_json::Value`; `schemars(with)`
//! then documents the shape without changing a byte of output. A key the
//! daemon omits is `skip_serializing_if`; a key it sends as `null` is not.
use crate::issue::{CardChecks, ClaimCard, IssueCard, Ref};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// Age, staleness and refresh error of the daemon snapshot behind a read —
/// beside `/api/issues`, `/api/agents` and the stream's `agent_meta`.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct Freshness {
    pub snapshot_age_secs: f64,
    pub stale: bool,
    pub refresh_error: bool,
}

/// One bound agent on an issue card or drawer — from the job binding.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct CardAgent {
    pub alias: String,
    pub task: String,
    pub task_state: String,
    /// The agent's state; `null` when the daemon reports none.
    pub state: Option<String>,
    /// Running kickoff message id for this task, if live.
    pub message: Option<String>,
    /// Concrete resume command for a stopped pane.
    pub resume: Option<String>,
}

/// One move `epic_stage` accepts from the epic's current stage.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct StageMove {
    pub to: String,
    pub forward: bool,
    /// Only the proven operator may make it.
    pub needs_operator: bool,
}

/// An epic's stage in its project's stage list.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct WorkStage {
    pub id: String,
    /// `field` | `plan` | `status` | `default`.
    pub source: String,
    /// When the epic entered the stage (ISO); `null` when unknown.
    pub since: Option<String>,
    /// The exit criterion of this stage; `null` for a stage the list lacks.
    pub exit: Option<String>,
    pub next: Option<String>,
    pub next_needs_operator: bool,
    pub terminal: bool,
    pub stages: Vec<String>,
    /// The legal moves from here; empty while PROJECT.md cannot be loaded.
    pub moves: Vec<StageMove>,
}

/// Work-item counts by status bucket.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct ProgressCounts {
    /// Backlog plus ready.
    pub open: u64,
    pub doing: u64,
    pub review: u64,
    pub blocked: u64,
    pub done: u64,
    pub dropped: u64,
    pub total: u64,
}

/// Size-weighted progress over a set of work items.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct WorkProgress {
    pub done_weight: u64,
    pub total_weight: u64,
    pub ratio: f64,
    pub counts: ProgressCounts,
}

/// One reason an epic is not on track, with its next action.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct HealthReason {
    /// `blocked` | `stage_time` | `stalled`.
    pub cause: String,
    pub issue: String,
    pub owner: Option<String>,
    pub detail: String,
    pub next: String,
}

/// An epic's health: `stalled` is time in stage, not child activity.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct WorkHealth {
    /// `on_track` | `at_risk` | `stalled`.
    pub state: String,
    pub days_in_stage: Option<i64>,
    pub limit_days: u64,
    pub reasons: Vec<HealthReason>,
}

/// The CAD-405 work block on a card: type, milestone, size; stage,
/// progress and health on epics.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct WorkBlock {
    #[serde(rename = "type")]
    pub kind: String,
    /// `field` | `implicit`.
    pub type_source: String,
    pub milestone: Option<String>,
    pub milestone_source: Option<String>,
    pub size: Option<String>,
    pub weight: u64,
    /// Epics only; `null` otherwise.
    pub stage: Option<WorkStage>,
    /// Epics only: weighted child progress.
    pub progress: Option<WorkProgress>,
    /// Epics only.
    pub health: Option<WorkHealth>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_unapproved: Option<String>,
}

/// One `GET /api/issues` row: the core card, its work block and, only when
/// a job binds agents to the issue, the agent strip.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct IssueListItem {
    #[serde(flatten)]
    pub card: IssueCard,
    pub work: WorkBlock,
    /// The bound-agent strip, present only when a job binds agents to the issue.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agents: Option<Vec<CardAgent>>,
}

/// `GET /api/issues`.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct IssuesResponse {
    pub issues: Vec<IssueListItem>,
    #[serde(flatten)]
    pub freshness: Freshness,
}

/// A running kickoff reduced to what the board shows.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct RunningMessage {
    #[schemars(with = "Option<String>")]
    pub id: Value,
    #[schemars(with = "Option<String>")]
    pub task: Value,
    /// Epoch seconds (the daemon serializes `Message.created` as f64).
    #[schemars(with = "Option<serde_json::Number>")]
    pub created: Value,
    pub summary: Option<String>,
}

/// A provider allowance record as the store keeps it. The envelope is fixed;
/// `data` is the provider's own report (for Codex, its rate-limit buckets).
#[derive(Clone, Debug, Serialize, JsonSchema)]
#[schemars(transform = require_quota_keys)]
pub struct AgentQuota {
    pub provider: String,
    pub assignee: String,
    pub account_id: Option<String>,
    pub thread_id: String,
    /// `available` | `unknown` | `unavailable` | `error` | `blocked` | `stale`.
    pub state: String,
    pub source: String,
    pub observed_at: String,
    pub updated_at: String,
    /// Why the state is not `available`; absent when it is.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub data: Value,
}

/// One task bound to an agent through a job.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct AgentTask {
    pub task: String,
    pub task_state: String,
    pub title: Option<String>,
    pub issue: String,
    pub job: String,
    pub job_title: Option<String>,
    pub job_state: String,
    #[schemars(with = "Option<RunningMessage>")]
    pub message: Value,
}

/// One `/api/agents` row. Mailboxes (`inbox: true`) carry the unread
/// fields; workers carry the rest of the optional ones.
#[derive(Clone, Debug, Default, Serialize, JsonSchema)]
pub struct AgentRow {
    pub alias: String,
    pub provider: Option<String>,
    pub endpoint_kind: Option<String>,
    #[schemars(with = "Option<String>")]
    pub role: Value,
    /// Model-preference role, independent of the runtime `role`.
    #[schemars(with = "Option<String>")]
    pub team_role: Value,
    #[schemars(with = "Option<Value>")]
    pub model_selection: Value,
    #[schemars(with = "Option<String>")]
    pub model_lookup_role: Value,
    /// Provider evidence: the reported effective value beside launch config.
    #[schemars(with = "Option<String>")]
    pub model: Value,
    #[schemars(with = "Option<String>")]
    pub model_reported: Value,
    #[schemars(with = "Option<String>")]
    pub model_configured: Value,
    #[schemars(with = "Option<String>")]
    pub model_source: Value,
    #[schemars(with = "Option<String>")]
    pub effort: Value,
    #[schemars(with = "Option<String>")]
    pub effort_reported: Value,
    #[schemars(with = "Option<String>")]
    pub effort_source: Value,
    #[schemars(with = "Option<bool>")]
    pub effort_applicable: Value,
    /// The provider allowance record; null means unavailable. The store
    /// writes it only through `canonical_quota`, so the envelope is fixed.
    #[schemars(with = "Option<AgentQuota>")]
    pub quota: Value,
    #[schemars(with = "Option<Value>")]
    pub usage_limit: Value,
    /// A hire's catalogue identity (workers only).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    pub catalogue: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    pub look: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    pub kind: Option<Value>,
    pub state: Option<String>,
    pub group: String,
    pub group_root: bool,
    pub running: i64,
    pub queued: i64,
    /// Mailboxes only: the unread backlog and its oldest age.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unread: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<f64>")]
    pub oldest_unread_age_secs: Option<Value>,
    pub unknown: i64,
    pub parked: i64,
    pub fenced: bool,
    /// Issue ids bound through jobs — exact, not text-derived.
    pub on: Vec<String>,
    pub tasks: Vec<AgentTask>,
    /// The first running message, reduced.
    #[schemars(with = "Option<RunningMessage>")]
    pub message: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<Vec<RunningMessage>>")]
    pub running_messages: Option<Vec<Value>>,
    /// The daemon's fence text, which names the recovery commands.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovery: Option<Option<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resume: Option<Option<String>>,
    /// When `resume` applies.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resume_hint: Option<String>,
    pub dead: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inbox: Option<bool>,
    /// Epoch seconds, or ISO text from older observations.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<Value>")]
    pub last_activity: Option<Value>,
    /// Seconds since the running turn's last observed activity.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<f64>")]
    pub silent_secs: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<bool>")]
    pub stalled: Option<Value>,
    /// `stopped (auto, idle 72m)` when the idle timer stopped it.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<String>")]
    pub state_label: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_cursor: Option<i64>,
}

#[derive(Clone, Debug, Default, Serialize, JsonSchema)]
pub struct AgentTotals {
    pub running: i64,
    pub queued: i64,
    pub fenced: i64,
    pub parked: i64,
    pub inboxes: i64,
}

#[derive(Clone, Copy, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum DaemonReach {
    Reachable,
    Unreachable,
}

/// The agent roster the board derives from the daemon's `agent_list`.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct AgentsPayload {
    pub daemon: DaemonReach,
    pub agents: Vec<AgentRow>,
    pub totals: Option<AgentTotals>,
    /// Issue id to its bound agent strip.
    pub by_issue: BTreeMap<String, Vec<CardAgent>>,
}

/// `GET /api/agents`: the roster beside its snapshot's freshness.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct AgentsResponse {
    #[serde(flatten)]
    pub payload: AgentsPayload,
    #[serde(flatten)]
    pub freshness: Freshness,
}

/// The `gh` this board's delivery sync runs, fixed at start.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct DeliverySyncMeta {
    pub gh: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gh_error: Option<String>,
    pub every_secs: u64,
}

/// The daemon's build, when it answered inside the metadata budget.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct MetaDaemon {
    #[schemars(with = "Option<String>")]
    pub build_commit: Value,
    #[schemars(with = "Option<String>")]
    pub build_time: Value,
    #[schemars(with = "Option<serde_json::Number>")]
    pub started_at: Value,
}

/// Where a board session was opened.
#[derive(Clone, Copy, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum SessionOrigin {
    Loopback,
    Tailnet,
    Public,
}

/// The platform-asserted user of a `public` session.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct SessionUser {
    pub sub: String,
    pub email: String,
    pub name: String,
    /// The mapped Cadence role: `operator` or `member`.
    pub role: String,
    pub handle: String,
}

/// The verified tenancy of a scoped public session.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct SessionScope {
    pub issuer: String,
    pub organization_id: String,
}

/// `user` and `account_id` are always sent, `null` when absent, so the schema
/// requires the keys and keeps them nullable (`schemars(required)` would drop
/// the null). `scope` is skipped when `None`, so it stays optional.
fn require_nullable_keys(schema: &mut schemars::Schema, keys: &[&str]) {
    for key in keys {
        assert!(
            schema
                .get("properties")
                .is_some_and(|p| p.get(*key).is_some()),
            "schema has no property `{key}` to require"
        );
    }
    let required = schema
        .ensure_object()
        .entry("required")
        .or_insert_with(|| Value::Array(Vec::new()))
        .as_array_mut()
        .expect("`required` is an array");
    required.extend(keys.iter().map(|key| Value::from(*key)));
}

fn require_session_keys(schema: &mut schemars::Schema) {
    require_nullable_keys(schema, &["user", "account_id"]);
}

fn require_quota_keys(schema: &mut schemars::Schema) {
    require_nullable_keys(schema, &["account_id"]);
}

/// A live board session as the daemon reports it; never the token.
#[derive(Clone, Debug, Serialize, JsonSchema)]
#[schemars(transform = require_session_keys)]
pub struct SessionInfo {
    pub id: String,
    pub origin: SessionOrigin,
    pub created: i64,
    pub last_used: i64,
    pub idle_expires_at: i64,
    pub expires_at: i64,
    pub user_agent: String,
    pub user: Option<SessionUser>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<SessionScope>,
    /// Non-secret durable account handle; null on legacy sessions.
    pub account_id: Option<String>,
}

/// `GET /api/meta`: what this client may do and who writes credit to, plus
/// the build identity of the serving binary and the daemon's.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct MetaResponse {
    pub read_only: bool,
    /// Explicit local unsigned-read policy.
    pub anonymous_reads: bool,
    /// A live session; `null` when the check could not answer in budget.
    pub signed_in: Option<bool>,
    pub hosted: bool,
    /// The live session, or null. The daemon's session check supplies it;
    /// the board relays it untouched.
    #[schemars(with = "Option<SessionInfo>")]
    pub session: Value,
    /// The command that signs this origin in.
    pub login_hint: String,
    pub device_login: bool,
    pub tab_signed_out: bool,
    pub actor: String,
    /// How a tailnet proxy request was proven, or null.
    #[schemars(with = "Option<Value>")]
    pub tailnet_proof: Value,
    /// Only with `?operator=1`; `null` is unknown, never a non-operator.
    pub operator: Option<bool>,
    pub platform_account_configured: bool,
    pub tailnet_url: Option<String>,
    pub version: String,
    pub build_commit: String,
    pub build_time: String,
    pub daemon: Option<MetaDaemon>,
    pub delivery_sync: Option<DeliverySyncMeta>,
}

/// The app conversation a Home turn belongs to, proven from the message's
/// persisted provenance; `context_id` is `""` for an installation-only chat.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct AppOrigin {
    pub install_id: String,
    pub context_id: String,
    pub conversation_id: String,
}

/// The master turn the working row mirrors: `working` while it runs,
/// `queued` for the queue head.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct MasterTurn {
    pub state: String,
    pub message: String,
    pub conversation: Option<String>,
    pub app_origin: Option<AppOrigin>,
    /// The operator's message text. A member never receives it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Epoch seconds: `started` while it runs, `created` while it waits.
    pub since: f64,
    /// Queue position of a queued head (always 1).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub position: Option<u32>,
}

/// One queued turn's routing metadata; never its body.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct MasterQueued {
    pub message: String,
    pub conversation: Option<String>,
}

/// The live session's context usage, when the provider answers `stats`.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct MasterContext {
    #[schemars(with = "Option<serde_json::Number>")]
    pub tokens: Value,
    #[schemars(with = "Option<serde_json::Number>")]
    pub window: Value,
    #[schemars(with = "Option<serde_json::Number>")]
    pub percent: Value,
}

/// The live provider session, when the provider answers `state`.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct MasterSession {
    #[schemars(with = "Option<String>")]
    pub id: Value,
    #[schemars(with = "Option<serde_json::Number>")]
    pub messages: Value,
    #[schemars(with = "Option<bool>")]
    pub streaming: Value,
    #[schemars(with = "Option<bool>")]
    pub compacting: Value,
}

/// `master_state` (daemon RPC; `GET /api/master/state` relays it): what the
/// master header shows. A member's copy drops the turn's `summary`.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct MasterState {
    pub alias: String,
    pub provider: String,
    pub endpoint_kind: String,
    pub live: bool,
    pub model: Option<String>,
    #[schemars(with = "Option<String>")]
    pub model_label: Value,
    pub effort: Option<String>,
    pub context: Option<MasterContext>,
    pub session: Option<MasterSession>,
    /// The session verbs the live provider accepts.
    pub commands: Vec<String>,
    pub turn: Option<MasterTurn>,
    pub queue: Vec<MasterQueued>,
    pub queued: i64,
}

/// A conversation (thread) row.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct ThreadInfo {
    pub id: String,
    pub alias: String,
    pub created: f64,
    pub updated: f64,
    pub install_id: Option<String>,
    pub context_id: Option<String>,
    /// `campaign:<id>` for a campaign conversation.
    pub subject: Option<String>,
    pub general: bool,
    pub is_general: bool,
    pub archived: bool,
    pub title: Option<String>,
}

/// One thread entry. For a message entry of the same thread the payload
/// also carries `delivery: {state, started, completed}`.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct ThreadEntryRow {
    pub seq: i64,
    pub thread: String,
    pub role: String,
    pub kind: String,
    pub text: String,
    pub payload: Option<Value>,
    pub message: Option<String>,
    pub created: f64,
}

/// `thread_read` (daemon RPC; `GET /api/threads/<alias>` relays it).
/// A forward read carries `cursor`; a backward one also `more_before`.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct ThreadPage {
    pub alias: String,
    pub thread: Option<ThreadInfo>,
    pub entries: Vec<ThreadEntryRow>,
    pub cursor: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub more_before: Option<bool>,
    pub reply_seq: i64,
}

/// A link target in the drawer: the issue's title and status when it is on
/// the board, else just its id with `missing: true`.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct LinkRef {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_source: Option<String>,
    pub missing: bool,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct IssueLinks {
    pub parent: Option<LinkRef>,
    pub children: Vec<LinkRef>,
    pub blocked_by: Vec<LinkRef>,
    pub blocks: Vec<LinkRef>,
    pub relates: Vec<LinkRef>,
    pub duplicate_of: Option<LinkRef>,
    pub duplicates: Vec<LinkRef>,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct ArtifactRow {
    pub name: String,
    pub size: u64,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct CommentRow {
    pub name: String,
    pub author: String,
    pub at: String,
    pub kind: Option<String>,
    pub body: String,
}

/// A notes-chain entry tagged to the issue.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct NoteRow {
    pub name: String,
    pub kind: String,
    pub at: String,
    pub title: String,
}

/// One checklist item of the `Acceptance` section; `done` mirrors `checked`.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct AcceptanceRow {
    pub text: String,
    pub checked: bool,
    pub done: bool,
}

/// One `git log` line of the issue's `issue.md`.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct HistoryEntry {
    pub commit: String,
    pub at: String,
    pub subject: String,
}

/// One merged activity entry: `kind` is `note` (with `note_kind`, `title`,
/// `name`), `comment` (`author`, `body`, `name`) or `commit` (`commit`,
/// `subject`).
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct ActivityEntry {
    pub at: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note_kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
}

/// `GET /api/issues/:id` and `issue show --json`: the drawer payload.
/// The board adds `agents` when a job binds agents to the issue.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct IssueDetail {
    pub id: String,
    pub project: String,
    pub title: String,
    pub status: String,
    pub status_source: String,
    pub priority: String,
    pub owner: Option<String>,
    pub claim: Option<ClaimCard>,
    pub component: Option<String>,
    pub tags: Vec<String>,
    pub paths: Vec<String>,
    pub parent: Option<String>,
    pub container: bool,
    pub ready: bool,
    pub blocked: bool,
    pub blocked_reason: Option<String>,
    pub created: String,
    pub rev: String,
    /// The issue's front matter as stored.
    #[schemars(with = "serde_json::Map<String, Value>")]
    pub frontmatter: Value,
    pub body: String,
    pub path: String,
    pub links: IssueLinks,
    pub refs: Vec<Ref>,
    pub artifacts: Vec<ArtifactRow>,
    pub comments: Vec<CommentRow>,
    /// Task reports filed on the issue (`issue/task_report.rs` rows).
    #[schemars(with = "Vec<Value>")]
    pub reports: Vec<Value>,
    pub notes_chain: Vec<NoteRow>,
    pub acceptance: Vec<AcceptanceRow>,
    pub size: Option<String>,
    /// Plan state, tickets and weighted progress; null unless a plan epic.
    #[schemars(with = "Option<Value>")]
    pub plan: Value,
    pub history: Vec<HistoryEntry>,
    pub activity: Vec<ActivityEntry>,
    #[schemars(with = "Vec<Value>")]
    pub commits: Vec<Value>,
    #[schemars(with = "Vec<Value>")]
    pub commits_skipped: Vec<Value>,
    pub checks: CardChecks,
    /// The CAD-405 work block (`work::detail`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub work: Option<WorkBlock>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agents: Option<Vec<CardAgent>>,
}

// ---- Request bodies (CAD-1798) ----
//
// What the board's write routes parse. Each is `deny_unknown_fields`: an
// identity-shaped key (`by`, `actor`, `operator`, a browser-stamped
// `verified`) is refused, never ignored. The schema of each is committed
// beside the response schemas.

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NewIssueReq {
    pub project: String,
    pub title: String,
    pub priority: Option<String>,
    pub owner: Option<String>,
    pub component: Option<String>,
    pub tags: Option<Vec<String>>,
    pub parent: Option<String>,
    pub blocked_by: Option<Vec<String>>,
    /// CAD-140: the board's report/idea composer files the description
    /// with the issue — one call instead of create-then-comment.
    /// `None`/blank keeps `new_issue`'s default body.
    pub body: Option<String>,
}

/// CAD-140: `POST /api/reports` — file a question, feedback, idea or
/// bug through the canonical intake path. `project` is the board the
/// operator files from: ideas land there, while questions, feedback
/// and bugs route to the `cadence` project like `cadence report` (the
/// server decides — the UI never routes). Unknown fields are refused
/// so a forged `by`/`actor` never reaches the write.
#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReportReq {
    pub kind: String,
    pub project: String,
    pub title: String,
    pub priority: Option<String>,
    pub body: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PatchReq {
    pub status: Option<String>,
    pub priority: Option<String>,
    /// `""` clears owner.
    pub owner: Option<String>,
    /// `""` clears component.
    pub component: Option<String>,
    pub title: Option<String>,
    pub body: Option<String>,
    /// Replaces the tag list; `[]` clears it.
    pub tags: Option<Vec<String>>,
    pub if_rev: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LinkReq {
    #[serde(rename = "type")]
    pub kind: String,
    pub target: String,
    pub if_rev: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RefReq {
    pub kind: String,
    pub url: Option<String>,
    pub path: Option<String>,
    pub label: Option<String>,
    pub if_rev: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CommentReq {
    pub body: String,
    pub if_rev: Option<String>,
}

/// Body for `POST /api/agents`. Every field is one `rpc_register` reads;
/// `deny_unknown_fields` refuses identity-shaped keys (`by`, `actor`,
/// `operator`, …) and anything else the daemon has no say in. Location is
/// not a daemon field: `cloud` pins provider and endpoint kind to `sandbox`.
#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentRegisterReq {
    pub alias: String,
    /// Start blank only; a `catalogue` hire takes the runtime from the install.
    pub location: Option<String>,
    pub provider: Option<String>,
    pub endpoint_kind: Option<String>,
    pub role: Option<String>,
    pub team_role: Option<String>,
    pub model_policy: Option<String>,
    pub cwd: Option<String>,
    pub sandbox: Option<String>,
    pub instructions: Option<String>,
    pub params: Option<String>,
    /// CAD-1752: hire from this role of `GET /api/agent-roles`.
    pub catalogue: Option<String>,
    pub look: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ThreadMessageReq {
    pub text: String,
    /// Client-chosen id: a retried POST is the same message, not two.
    pub message: Option<String>,
    /// CAD-802: the shell chat's current installation/context. Never
    /// authority — the daemon resolves both against its store and
    /// refuses unknown installs, unknown/archived contexts and any
    /// extra key (including a browser-stamped `verified`).
    pub app: Option<ThreadApp>,
    /// CAD-574: needs-me subjects the message cites (the rail's "Ask
    /// master"). The daemon's `thread_refs` is the strict side — this
    /// field only carries `{kind,id}` pairs through the relay.
    pub refs: Option<Vec<ThreadRef>>,
    /// CAD-1098: a selector among the conversations of `app`'s
    /// installation — never authority; the daemon checks it.
    pub conversation: Option<String>,
    /// CAD-1168: retained chat-file ids from `/api/chat/upload`. The
    /// daemon's `thread_attachments` resolves them — this relay only
    /// carries `{id}` handles through, never names or paths.
    pub attachments: Option<Vec<ThreadAttachment>>,
    /// One retained library image. The daemon re-proves id, revision and digest.
    pub media_refs: Option<Vec<ThreadMediaRef>>,
    /// CAD-1591: the contextual-reply recipient constraint — the uid the
    /// operator's composer recorded. Relayed verbatim; the daemon
    /// validates the shape and fences it inside the enqueue
    /// transaction, never here.
    pub expected_recipient_uid: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ThreadMediaRef {
    pub asset_id: String,
    pub revision: i64,
    pub sha256: String,
}

/// A retained chat-file handle — the daemon re-validates the grammar
/// and existence; the board never dereferences it to a path.
#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ThreadAttachment {
    pub id: String,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConversationReq {
    pub install_id: Option<String>,
    pub context_id: Option<String>,
    pub subject: Option<String>,
    pub general: Option<bool>,
    /// CAD-1772: `"recruit"` — the master's Recruit conversation.
    pub kind: Option<String>,
}

#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ThreadRef {
    pub kind: String,
    pub id: String,
}

/// CAD-802: the shell chat's current App. The shape alone travels
/// here — the daemon proves the installation and context against its
/// own store and stamps the verified binding on the entry.
#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ThreadApp {
    pub install_id: String,
    /// Absent for an installation-only binding (no context selected).
    pub context_id: Option<String>,
}
