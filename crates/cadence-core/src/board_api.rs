//! The board API's response types (CAD-1792): one contract for the daemon's
//! board, the UI and the hosted tenant-board Worker. The daemon serializes
//! these; their JSON Schema is committed under `contracts/board-api/v1/` and
//! the UI's TypeScript is generated from it.
//!
//! A field the daemon passes through from another producer (a provider
//! report, a stored session) stays a `serde_json::Value`; `schemars(with)`
//! then documents the shape without changing a byte of output. A key the
//! daemon omits is `skip_serializing_if`; a key it sends as `null` is not.
use crate::issue::IssueCard;
use schemars::JsonSchema;
use serde::Serialize;
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
    /// Epics only: `{id, source, since, exit, next, next_needs_operator,
    /// terminal, stages, moves}`.
    #[schemars(with = "Option<Value>")]
    pub stage: Value,
    /// Epics only: weighted child progress.
    #[schemars(with = "Option<Value>")]
    pub progress: Value,
    /// Epics only: `{state, reasons, days_in_stage, ..}`.
    #[schemars(with = "Option<Value>")]
    pub health: Value,
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
    /// Optional account/pool allowance views; null means unavailable.
    #[schemars(with = "Option<Value>")]
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
    /// The live session (`{id, origin, user, created, ..}`), or null.
    #[schemars(with = "Option<Value>")]
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
    #[schemars(with = "Option<Value>")]
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
