import type * as Api from "./generated/board-api";

export type StatusSource = "file" | "notes" | "rollup" | "job";

/** One bound agent on an issue card/drawer — from the job binding,
 *  never a text scan. Generated from the daemon's Rust type. */
export type CardAgent = Api.CardAgent;

export type Ref = Api.Ref;

/** CAD-1817: the canonical avatar catalog slug — the daemon's closed union,
 *  matching `AVATAR_LOOKS` in `features/agents/AgentAvatar.tsx`. */
export type AvatarLook = Api.AvatarLook;

/** CAD-1817: the workspace coordinator's committed identity projection. */
export type MasterIdentity = Api.MasterIdentity;

/** A board card (`GET /api/issues` row): the core card, its work block and
 *  the bound-agent strip. Generated from the daemon's Rust type. */
export type IssueCard = Api.IssueListItem;

/** CAD-405 epic health. `stalled` is time in stage, not child activity. */
export type HealthState = "on_track" | "at_risk" | "stalled";

/** One reason an epic or milestone is not on track, with its next action. */
export type HealthReason = Api.HealthReason;
export type WorkProgress = Api.WorkProgress;
/** One move `epic_stage` accepts from the epic's current stage. */
export type StageMove = Api.StageMove;
export type WorkStage = Api.WorkStage;
/** The `work` block on `/api/issues` cards and `/api/issues/:id`. */
export type WorkBlock = Api.WorkBlock;

/** `GET /api/milestones` — one (project, milestone) roll-up. */
export interface MilestoneRow {
  config_error?: string | null;
  project: string;
  id: string;
  title: string | null;
  exit: string | null;
  configured: boolean;
  description?: string | null;
  owner?: string | null;
  start_date?: string | null;
  target_date?: string | null;
  status?: "planned" | "active" | "achieved" | "cancelled" | null;
  completed_date?: string | null;
  evidence?: string[];
  depends_on?: string[];
  schedule?: { state: "unscheduled" | "upcoming" | "due_today" | "overdue" | "achieved" | "cancelled"; days_remaining: number | null };
  progress: WorkProgress;
  health: { state: HealthState; reasons: HealthReason[] };
  epics: {
    id: string;
    title: string;
    owner?: string | null;
    stage: string | null;
    progress: number | null;
    health: HealthState;
    task_count?: number;
  }[];
  issues: {
    id: string;
    title: string;
    type: string;
    status: string;
    size?: string | null;
    owner?: string | null;
    blocked: boolean;
  }[];
  /** All scoped tasks; `issues` is the legacy list of tasks without an epic. */
  tasks?: {
    id: string;
    title: string;
    type: string;
    status: string;
    size?: string | null;
    owner?: string | null;
    parent?: string | null;
    blocked: boolean;
  }[];
}

export interface LinkRef {
  id: string;
  title?: string;
  status?: string;
  status_source?: StatusSource;
  missing: boolean;
}

export interface ActivityItem {
  at: string;
  kind: "note" | "comment" | "commit";
  note_kind?: string;
  title?: string;
  name?: string;
  author?: string;
  body?: string;
  commit?: string;
  subject?: string;
}

/** A code commit that names this issue — an `Issue: <ID>` trailer or
 *  a whole-word `(ID)` in the subject, found in a project repo. */
export interface IssueCommit {
  repo: string;
  sha: string;
  at: string;
  author: string;
  subject: string;
  /** Reachable from the repo's default branch; `null` when unknown. */
  on_default?: boolean | null;
}

/** `GET /api/issues/<id>/history` — one parsed git-log entry. */
export interface IssueHistoryEntry {
  sha: string;
  at: string;
  by: string;
  kind:
    | "created"
    | "set"
    | "link"
    | "unlink"
    | "ref"
    | "comment"
    | "attach"
    | "tag"
    | "stage"
    | "plan"
    | "other";
  summary: string;
  fields?: Record<string, string | null>;
  /** `stage` entries (CAD-432): the move and its note. */
  from?: string;
  to?: string;
  note?: string | null;
}

/** `GET /api/issues/<id>`: the issue-detail contract. Unlike a board card it
 *  has no `counts`; its comments are the recorded `comments` rows. */
export interface IssueDetail extends Omit<IssueCard, "counts"> {
  frontmatter: Record<string, unknown>;
  body: string;
  path: string;
  links: {
    parent?: LinkRef;
    children: LinkRef[];
    blocked_by: LinkRef[];
    blocks: LinkRef[];
    relates: LinkRef[];
    duplicate_of?: LinkRef;
    duplicates: LinkRef[];
  };
  comments: { name: string; author: string; at: string; kind?: string; body: string }[];
  notes_chain: { name: string; kind: string; at: string; title: string }[];
  artifacts: { name: string; size: number }[];
  activity: ActivityItem[];
  commits?: IssueCommit[];
  commits_skipped?: { repo: string; reason: string }[];
  /** CAD-359/360: plan state, tickets and progress — null unless a plan epic. */
  plan?: PlanBlock | null;
  /** CAD-341 task reports on the ticket (questions carry `open`). */
  reports?: TaskReport[];
}

/** One ticket of a plan, with its derived status. */
export interface PlanTicket {
  id: string;
  title: string;
  status: string;
  size?: string | null;
  weight?: number;
  owner?: string | null;
  blocked_by?: string[];
  /** Number of acceptance criteria. */
  acceptance?: number;
}

/** The `plan` block of a plan epic's detail. */
export interface PlanBlock {
  state: string;
  proposed_by?: string;
  proposed_at?: string;
  decided_by?: string | null;
  decided_at?: string | null;
  reason?: string | null;
  tickets: PlanTicket[];
  progress?: {
    done_weight: number;
    total_weight: number;
    ratio: number;
    counts?: Record<string, number>;
  };
}

/** One task report row (`reports` on an issue detail). */
export interface TaskReport {
  name: string;
  kind?: string;
  agent?: string;
  at?: string | null;
  options?: string[];
  impact?: string | null;
  answers?: string | null;
  body?: string;
  open?: boolean;
  error?: string;
  /** Reviewed head and verdict, emitted for daemon-filed reviews. */
  sha?: string | null;
  verdict?: string | null;
  /** Pull request recorded by a completion report. */
  pr?: string | null;
}

export interface Project {
  key: string;
  prefix: string;
  components: string[];
  /** Declared tag vocabulary — empty accepts any well-formed tag. */
  tags?: string[];
  default_owner?: string;
  repos: { path?: string; remote?: string }[];
  issues: number;
}

export type ContextRole = "pm" | "dev" | "qa" | "devops";

export interface ProjectContextDocument {
  id: string;
  kind: string;
  path: string;
  title: string;
  required: boolean;
  selected: boolean;
  selection_reason: string;
  state: string;
  reason?: string;
  bytes?: number;
  excerpt?: string;
  truncated?: boolean;
}

export interface ProjectContext {
  project: string;
  state: string;
  manifest: {
    path: string;
    state: string;
    revision?: string;
    entry_count?: number;
    entries_omitted?: number;
    errors: string[];
  };
  snapshot: {
    head_revision?: string;
    expected_revision?: string;
    revision_state: string;
    dirty: boolean | null;
    dirty_truncated: boolean;
    repo_identity?: string;
    error?: string;
  };
  documents: ProjectContextDocument[];
  memories: {
    included: { id: string; kind: string; confidence: string; verified_at?: string }[];
    lessons: string;
    withheld: { id: string; status: string; reason: string }[];
    withheld_total: number;
    withheld_omitted: number;
    load_errors: string[];
    load_errors_total: number;
    load_errors_omitted: number;
    matched_total: number;
  };
  limits: {
    max_manifest_bytes: number;
    max_manifest_entries: number;
    max_path_bytes: number;
    max_title_bytes: number;
    max_document_bytes: number;
    max_excerpt_bytes: number;
    max_git_output_bytes: number;
    max_memory_entries: number;
    max_memory_lesson_bytes: number;
    max_response_bytes: number;
    response_truncated: boolean;
    response_excerpt_reductions: number;
  };
}

/** One `needs_me` row — what is waiting on a human, with the command. */
export interface NeedsMe {
  kind: string;
  title: string;
  /** CAD-1219: optional plain-words card title (about 50 characters) and
   *  one-sentence reason. Display text only; older rows omit them. */
  short_title?: string;
  why?: string;
  /** Seconds in this state. */
  age: number;
  project: string;
  link?: string | null;
  command: string;
  /** What the row is about — rows sharing a subject are merged. */
  subject?: { kind: string; id: string };
  /** Every cause for the subject, most severe first (`kind` is the first). */
  causes?: {
    cause: string;
    title: string;
    age: number;
    command: string;
    audience?: NeedAudience;
    since?: number | null;
  }[];
  /** Who must act on a stale inbox: its group root, or `operator`. */
  owner?: string;
  /** Who the row is for, resolved server-side (CAD-253): a team row
   *  escalates to `operator` when no live owner can act or it has waited
   *  past 60 minutes. */
  audience?: NeedAudience;
  /** Why — e.g. `owner pm is dead`, `no owner`, `unhandled 74m`,
   *  `owner pm can act`; null for dependency/info rows. */
  audience_reason?: string | null;
  /** Epoch secs when the row's condition began — the unhandled clock;
   *  null when the kind has no reliable start (owner-only escalation). */
  since?: number | null;
}

export type NeedAudience = "operator" | "team" | "dependency" | "info";

/** Deploy drift — merged commits on the default branch past the
 *  running daemon's build commit. */
export interface Drift {
  matched: boolean;
  project?: string;
  repo?: string;
  ref?: string;
  build_commit?: string;
  known?: boolean;
  count?: number;
  commits?: { subject: string; pr?: number | null }[];
  reason?: string;
  /** Present when drift exists but the restart row is held back. */
  held?: string;
}

/** One default-branch SHA's own `ci.yml` push-run verdict (CAD-267).
 *  Only the SHA's own successful run is `passed`; a covered SHA keeps
 *  its `cancelled`/`missing` label. */
export interface ShaCi {
  sha: string;
  state: "passed" | "failed" | "pending" | "cancelled" | "missing";
  /** Cancelled/missing only: nearest later SHA whose own run passed. */
  covered_by?: string | null;
  run_id?: number | null;
  run_url?: string | null;
  /** Raw run conclusion (`cancelled`, `skipped`, `timed_out`, …). */
  conclusion?: string | null;
  created_at?: string | null;
}

/** Default-branch CI for one repo, newest SHA first. */
export interface MainCi {
  slug: string;
  project: string;
  branch?: string;
  workflow?: string;
  /** `first_parent` — ordered by the local clone; `runs` — no clone, run order. */
  order?: "first_parent" | "runs";
  log_error?: string | null;
  error?: string | null;
  shas?: ShaCi[];
}

/** CAD-383: one in-flight (doing/review) issue and who holds it. */
export interface OverviewClaim {
  issue: string;
  project: string;
  status: string;
  by: string | null;
  owner: string | null;
  note?: string | null;
  since: string | null;
  age_secs: number | null;
}

/** CAD-378: one open lane, the code areas it plans or changes, and
 *  the other lanes whose planned or changed paths it overlaps. */
export interface OverviewLane {
  issue: string;
  worker: string | null;
  pm: string | null;
  pr: string | null;
  planned: string[];
  changed_count: number;
  changed_error?: string | null;
  areas: string[];
  overlaps: string[];
}

export interface OverviewProject {
  key: string;
  open_by_status: Record<string, number>;
  oldest_review_age?: number | null;
  claims?: OverviewClaim[];
  lanes?: OverviewLane[];
  areas?: string[];
  areas_error?: string | null;
}

export interface MonitorAlert {
  seq: number;
  monitor: string;
  project: string;
  monitor_owner: string;
  task?: string | null;
  event_seq: number;
  fingerprint: string;
  kind: string;
  payload: unknown;
  state: "open" | "acknowledged" | "resolved" | string;
  attempts: number;
  last_error?: string | null;
  created: number;
  updated: number;
  age_secs: number;
  next_action: string;
  next_owner: string;
  authority: string;
  evidence: {
    monitor: string;
    alert_seq: number;
    event_seq: number;
    fingerprint: string;
    payload: unknown;
  };
}

export interface MonitorRow {
  id: string;
  project: string;
  owner: string;
  interval_secs: number;
  monitoring: "active" | "degraded" | "stale" | "off" | string;
  heartbeat_at?: number | null;
  last_check_at?: number | null;
  last_success_at?: number | null;
  next_check_at?: number | null;
  event_cursor: number;
  coverage: string[];
  delivery: { configured: boolean; state: string; push?: boolean; detail?: string };
  dispatch_enabled: boolean;
  auto_dispatch_enabled: boolean;
  open_alerts: number;
  total_alerts: number;
  error?: string | null;
  alerts?: MonitorAlert[];
  alerts_error?: string;
}

export interface Monitoring {
  available: boolean;
  state: "active" | "degraded" | "stale" | "stopped" | "unavailable" | string;
  last_success_at?: number | null;
  last_check_at?: number | null;
  next_check_at?: number | null;
  open_alerts: number;
  errors: { monitor?: string; error: string }[];
  delivery: { configured: boolean; state: string; push: boolean; detail: string };
  monitors: MonitorRow[];
  alerts: MonitorAlert[];
}

/** `GET /api/overview` — everything derived, nothing stored. */
export interface Overview {
  needs_me: NeedsMe[];
  drift: Drift;
  projects: OverviewProject[];
  github: {
    state: "ok" | "cached" | "stale" | "unavailable";
    error?: string | null;
    at?: number;
    /** When the served PR/CI rows were fetched. */
    as_of?: number | null;
  };
  /** Default-branch CI per repo from GitHub Actions runs (CAD-267). */
  main_ci?: MainCi[];
  daemon: { reachable: boolean; build_commit?: string; build_time?: string; started_at?: number; info?: string };
  monitoring?: Monitoring;
  /** Sources that missed their time bound — the view narrowed. */
  degraded?: { source: string; subject?: string; detail: string }[];
  generated_at: number;
}

/** A task assignment an agent is bound to through its job. */
export type AgentTask = Api.AgentTask;

export type UsageState =
  | "available"
  | "unknown"
  | "stale"
  | "error"
  | "unavailable"
  | "blocked";

/** Account/pool-scoped provider allowance telemetry. Optional because the
 * current daemon has no quota collector for most providers. */
export interface UsageLimit {
  state?: UsageState | string | null;
  provider?: string | null;
  assignee?: string | null;
  account_id?: string | null;
  thread_id?: string | null;
  used?: number | null;
  used_percent?: number | null;
  remaining?: number | null;
  limit?: number | null;
  unit?: string | null;
  window?: string | null;
  window_seconds?: number | null;
  reset_at?: string | null;
  source?: string | null;
  observed_at?: string | null;
  updated_at?: string | null;
  message?: string | null;
  reason?: string | null;
  /** Provider-owned buckets are preserved without collapsing them into a
   * synthetic scalar. */
  data?: Record<string, unknown> | null;
  pool?: {
    id?: string | null;
    label?: string | null;
    agents?: string[];
    count?: number | null;
  } | null;
}

/**
 * `POST /api/agents` body. `location` is the only setup choice: `cloud`
 * sends no provider or endpoint kind (the daemon sets `sandbox`/`sandbox`);
 * `local` sends the existing provider and endpoint kind plus cwd. No
 * per-issue location exists.
 */
/** CAD-1752: a hire from the built-in role catalogue sends `catalogue` and
 *  no location, provider or endpoint kind — the install picks the runtime. */
export interface AgentRegisterBody {
  alias: string;
  location?: "local" | "cloud";
  catalogue?: string;
  look?: string;
  provider?: string;
  endpoint_kind?: string;
  role?: string;
  team_role?: string;
  model_policy?: string;
  cwd?: string;
  sandbox?: string;
  instructions?: string;
  params?: string;
}

/** One role of `GET /api/agent-roles`. */
export interface AgentRole {
  id: string;
  category: string;
  title: string;
  look: string;
  team_role: string;
  kind: "tickets" | "apps";
  job: string;
  does: string[];
  skills: string[];
  tools: string[];
  first_tasks: string[];
  /** The text a hire stores as the agent's instructions. */
  instructions: string;
  /** The model a hire would launch on, or why none resolves. */
  model: { id: string; source: string } | { error: string };
}

export interface AgentRolesPayload {
  categories: { id: string; label: string }[];
  roles: AgentRole[];
  install: { hosted: boolean; provider: string; endpoint_kind: string };
}

/** `POST /api/agents/<alias>/instructions` reply. `applies` says when the
 *  agent sees the new text. */
export interface InstructionsResult {
  alias: string;
  state: string;
  applies: "next_turn" | "next_launch" | "not_delivered";
  briefing?: string | { error: string };
  old_sha256?: string | null;
  new_sha256?: string;
}

/** One `/api/agents` row. Generated from the daemon's Rust type, except the
 *  fields re-declared here: a registered agent always reports `provider`,
 *  `endpoint_kind` and `state` (only the Rust row type is lenient), the board
 *  reads the allowance record as the wider `UsageLimit`, and the detail read
 *  merges the launch `params` in. */
export type Agent = Omit<
  Api.AgentRow,
  "provider" | "endpoint_kind" | "state" | "quota" | "usage_limit" | "model_selection" | "last_activity"
> & {
  provider: string;
  endpoint_kind: string;
  state: string;
  /** Launch params; the detail read carries them. */
  params?: Record<string, unknown>;
  model_selection?: {
    source?: string | null;
    lookup_role?: string | null;
    revision?: number | null;
    model?: string | null;
  } | null;
  quota?: UsageLimit | null;
  usage_limit?: UsageLimit | null;
  /** Daemon messages use epoch seconds; older observations may use ISO text. */
  last_activity?: string | number | null;
};

/** `GET /api/agents` (the roster beside its snapshot's freshness). */
export type AgentsPayload = Omit<Api.AgentsResponse, "agents"> & { agents: Agent[] };

/** Best-effort destination for a verified app-scoped master turn. This is
 * navigation metadata only; the app route proves its binding independently. */
export interface MasterAppOrigin {
  install_id: string;
  context_id: string;
  conversation_id: string;
}

/** `GET /api/master/state` (CAD-551) — the master's provider session for
 * the header chips and the working row. `null`/`undefined` fields are
 * "the provider could not say", never a fabricated value; a daemon that
 * predates the route answers 501 and the UI falls back to `/api/agents`. */
export interface MasterState {
  alias: string;
  provider: string;
  endpoint_kind: string;
  /** A live provider endpoint answered — chips marked live only then. */
  live: boolean;
  /** Effective model id (live session wins over launch config). */
  model?: string | null;
  /** Display name when the provider has one (else `model`). */
  model_label?: string | null;
  effort?: string | null;
  context?: {
    tokens?: number | null;
    window?: number | null;
    percent?: number | null;
  } | null;
  session?: {
    id?: string | null;
    messages?: number | null;
    streaming?: boolean | null;
    compacting?: boolean | null;
  } | null;
  /** Provider session verbs the daemon will relay. */
  commands?: string[];
  /** The turn in flight, or the queue head waiting on one. */
  turn?: {
    state: "working" | "queued";
    message: string;
    /** The persisted message's conversation, when its enqueue entry exists. */
    conversation?: string | null;
    /** Current verified app scope; null when no origin can be re-proved. */
    app_origin?: MasterAppOrigin | null;
    summary?: string | null;
    /** Epoch seconds the message started (working) or was created (queued). */
    since?: number | null;
    /** 1-based queue position of a queued message; null while working. */
    position?: number | null;
  } | null;
  /** Queued master messages in delivery order (max 20). */
  queue?: { message: string; conversation?: string | null }[];
  queued?: number;
  /** CAD-1817: the workspace coordinator's committed identity — its own
   *  protected IDENTITY/SOUL projection, never a member's private profile.
   *  Generated as `MasterIdentity` from the daemon's Rust type. */
  identity: MasterIdentity;
  /** Hosted-only actor projection, bound to its selected conversation. */
  actor_identity?: Api.MasterState["actor_identity"];
}

/** `POST /api/master/command` — one allowlisted session verb's answer. */
export interface MasterCommandResult {
  command: string;
  ok: boolean;
  result?: unknown;
}

/**
 * `GET /api/master/models` — the model picker's read (CAD-575's route;
 * CAD-574's dropdowns consume it). `cost_tier` is the catalog's
 * Free / Low cost / Paid / unknown; `allowed_for` names the roles the
 * model may serve. Every field is optional — a daemon or provider that
 * cannot say simply omits it.
 */
export interface MasterModels {
  current?: { model?: string | null; effort?: string | null } | null;
  models?: {
    id?: string;
    label?: string | null;
    cost_tier?: string | null;
    allowed_for?: string[];
  }[];
  efforts?: string[];
}

/** A needs-me subject a thread message cites (CAD-574 `thread_send` refs). */
export interface ThreadRef {
  kind: string;
  id: string;
}

/** GET /api/agents/<alias> — the drawer detail. */
export interface AgentDetail {
  agent: {
    alias: string;
    provider: string;
    endpoint_kind: string;
    role?: string;
    cwd?: string;
    sandbox?: string;
    thread_id?: string | null;
    session_id?: string | null;
    model?: string | null;
    model_reported?: string | null;
    model_configured?: string | null;
    model_source?: string | null;
    effort?: string | null;
    effort_reported?: string | null;
    effort_source?: string | null;
    effort_applicable?: boolean | null;
    quota?: UsageLimit | null;
    usage_limit?: UsageLimit | null;
    pid?: number | null;
    state: string;
    enabled: boolean;
    error?: string | null;
    endpoint?: string | null;
    params?: Record<string, unknown>;
    generation?: number;
    dead?: boolean;
    silent_secs?: number;
    stalled?: boolean;
    capabilities?: Record<string, unknown> | null;
    tasks?: string[];
    /** CAD-1752: the project of the task this agent is running now. */
    turn_project?: string | null;
    /** The stored job, when the board serves it. */
    instructions?: string | null;
    team_role?: string | null;
  };
  queued: number;
  unknown: number;
  /** The agent page's figures; a null one is genuinely absent. */
  stats?: {
    hired_at: number | null;
    tasks_done: number;
    reports: number;
    avg_turn_secs: number | null;
    prs_merged: number | null;
    drafts_sent: number | null;
  } | null;
  running: {
    id: string;
    task?: string;
    created?: string;
    summary?: string | null;
  }[];
  /** Why `events` is empty when the daemon's event read failed. */
  events_error?: string | null;
  events: {
    seq: number;
    alias: string;
    kind: string;
    payload?: Record<string, unknown>;
    job_id?: string | null;
    task_id?: string | null;
    at: string;
  }[];
  fenced: boolean;
  recovery?: string | null;
  resume?: string | null;
  tasks?: string[];
  /** Issue ids bound through tasks × jobs.issue_id. */
  on?: string[];
}

/** GET /api/meta — what this client may do and who writes credit to,
 *  plus the build identity of the serving binary and the daemon's when
 *  reachable. Tailnet-shared boards resolve the actor from Tailscale
 *  identity headers; `read_only` boards refuse every write. */
export type Meta = Api.MetaResponse & {
  /** A hosted board sets this when its Worker serves /api/wiki. */
  wiki?: boolean;
};

export interface Health {
  ok: boolean;
  pm_dir?: string;
  pm_present: boolean;
  projects: number;
  issues: number;
  daemon: string;
  embedded: boolean;
}

/** GET /api/memories — one project-memory card. */
export interface MemoryQuorumCheck {
  eligible: boolean;
  reason?: string | null;
}

export interface MemoryQuorum {
  /** Whether this current revision is available to agents. */
  eligible: boolean;
  reason?: string | null;
  /** Whether the candidate's policy permits native finalization. */
  accept?: MemoryQuorumCheck | null;
  /** Whether revalidation requirements permit native finalization. */
  verify?: MemoryQuorumCheck | null;
}

export interface MemoryFinalization {
  operation: string;
  cycle: number;
  digest: string;
  finalized_at: string;
  finalizer: string;
}

/** A lesson's freshness as retrieval reads it (CAD-395). */
export interface MemoryEvidence {
  /** verified | unverified | withheld (stale or unused-expired). */
  state: "verified" | "unverified" | "withheld" | string;
  /** "verified <date>", "unverified (last verified <date>)", "unverified" or "withheld". */
  label: string;
  /** Why a withheld lesson is not injected. */
  reason?: string | null;
  /** The verify finalization time — not the raw verified_at field. */
  last_verified?: string | null;
  window_days?: number | null;
}

/** Bounded policy projection; canonical audit history stays in project memory. */
export interface MemoryPolicyV2 {
  revision: number;
  nature: string;
  provenance: string;
  audience: string;
  roles: string[];
  author_vendor?: string | null;
  contributors_count: number;
  evidence_refs_count: number;
  requirements: string[];
  verification_condition?: {
    check_id: string;
    source_revision?: string | null;
    target_revision?: string | null;
  } | null;
  feedback: {
    used_reports: number;
    wrong_reports: number;
    reread_reports: number;
    last_used_at?: string | null;
  };
}

export interface MemoryCard {
  project: string;
  slug: string;
  type: string;
  status: string;
  confidence: string;
  scope: {
    project: boolean;
    components: string[];
    paths: string[];
    providers: string[];
    tags: string[];
  };
  source?: string | null;
  author?: string | null;
  created: string;
  verified_at?: string | null;
  /** Absent on older daemons; absence shows no evidence label. */
  evidence?: MemoryEvidence | null;
  supersedes?: string | null;
  /** Semantic revision digest used by native review/finalization. */
  revision_digest?: string | null;
  review_cycle?: number | null;
  active_operation?: string | null;
  /** Immutable receipts retained across review cycles. */
  review_count?: number | null;
  finalization_count?: number | null;
  /** V2 returns the latest eight decisions; full counts/history are retained. */
  finalized_operations?: MemoryFinalization[] | null;
  /** Absent on older daemons or for an existing legacy record. */
  policy?: MemoryPolicyV2 | null;
  /** Absent on older daemons; absence means verification is unavailable. */
  quorum?: MemoryQuorum | null;
  fact: string;
  path: string;
}

/** GET /api/memories/<project>/<slug> — card + raw body. */
export interface MemoryDetail extends MemoryCard {
  body: string;
}

/** CAD-1548 Settings > Agents. Field names match the approved draft document. */
export type AgentScope = "off" | "allow" | "master" | "worker";
export type AgentProviderKind = "Pi extension" | "native CLI";

export interface AgentHarness {
  id: string;
  label: string;
  cli: string;
  note: string;
}

export interface AgentProvider {
  id: string;
  label: string;
  kind: AgentProviderKind;
  pin: string;
  gate: boolean;
  paid: boolean;
  note: string;
}

export interface AgentModel {
  id: string;
  provider: string;
  ctx: string;
  out: string;
  thinking: boolean | null;
  images: boolean | null;
}

export interface AgentProfile {
  id: string;
  role: string;
  model: string;
  job: string;
}

export interface AgentPin {
  pkg: string;
  pinned: string;
}

export interface AgentSettingsConfig {
  harnesses: AgentHarness[];
  providers: AgentProvider[];
  models: AgentModel[];
  profiles: AgentProfile[];
  scope: Record<string, AgentScope>;
  roleDefaults: Record<string, string>;
  pins: AgentPin[];
}

export interface AgentHarnessObservation {
  installed: boolean | null;
  version: string | null;
  live: number;
}

export interface AgentProfileFile {
  path: string;
  status: "not written" | "in sync" | "needs sync" | "unmanaged";
}

export interface AgentProfileObservation {
  version: string;
  usedBy: string[];
  history: Array<{ at: string; what: string }>;
  files: AgentProfileFile[];
}

export interface AgentSettingsObservations {
  harnesses: Record<string, AgentHarnessObservation>;
  models: Record<string, { live: number }>;
  profiles: Record<string, AgentProfileObservation>;
}

export interface AgentCatalogModel {
  id: string;
  ctx: string;
  out: string;
  thinking: boolean | null;
  images: boolean | null;
}

export interface AgentCatalog {
  source: string;
  fetchedAt: string;
  models: AgentCatalogModel[];
  error?: string;
}

export interface AgentSettingsSnapshot {
  revision: string;
  read_only: boolean;
  config: AgentSettingsConfig;
  observations: AgentSettingsObservations;
  catalogs: Record<string, AgentCatalog>;
}

export interface AgentSyncResult {
  snapshot: AgentSettingsSnapshot;
  files: Array<{ profile: string; path: string; status: string }>;
}

export interface AgentPreviewResult {
  command: string | null;
  model: string | null;
  source: string;
  allowed: boolean;
  reasons: string[];
  profile_revision: string;
}

/** One declared `inputs:` entry in a workflow's frontmatter (CAD-496). */
export interface WorkflowInput {
  name: string;
  ask?: string | null;
  optional?: boolean;
  /** The declared shape (`kind: slug`), when the workflow pins one. */
  kind?: string | null;
  /** An example value the run form shows in the empty field. */
  example?: string | null;
}

/**
 * `GET /api/projects/<key>/workflows` row — one stored workflow beside
 * PROJECT.md. `approved` is `true`/`false`, or the string
 * "unknown — daemon unreachable" when approvals cannot be read.
 * `error` marks a file that never parsed (a symlink, broken template).
 */
export interface WorkflowRow {
  project: string;
  /** Stored name, or `<app>/<workflow>` for an installed app's (CAD-547). */
  name: string;
  /** The app an `<app>/<wf>` row comes from — absent on stored workflows. */
  app?: string;
  title?: string | null;
  goal?: string | null;
  tickets?: number | null;
  inputs?: WorkflowInput[] | null;
  approved?: boolean | string | null;
  digest?: string | null;
  errors?: string[];
  notes?: string[];
  error?: string;
}

export interface WorkflowsPayload {
  workflows: WorkflowRow[];
}

/**
 * `GET /api/projects/<key>/workflows/<name>/preview` — what
 * `plan propose --workflow` would render for the current inputs, or
 * the refusal in `error` with its named `code` (the daemon's
 * one_line / not_distinct / render_diverged).
 */
export interface WorkflowPreview {
  project: string;
  name: string;
  rendered?: string | null;
  error?: string;
  code?: string | null;
  approved?: boolean | string | null;
  digest?: string | null;
}

/**
 * `GET /api/outbox` — one published `local`-platform item (CAD-546):
 * the effect's project, title and a bounded `preview` of the rendered
 * post, plus the item's outbox path and its board link.
 */
export interface OutboxItem {
  effect_id: string;
  project: string;
  title: string;
  published_at: string;
  post_sha256?: string;
  content_sha256?: string;
  attachments?: { name: string; sha256: string; bytes: number }[];
  preview?: string;
  path?: string;
  board_url?: string;
}

export interface OutboxList {
  items: OutboxItem[];
}

/** `GET /api/outbox?effect_id=` — the item plus its rendered post. */
export interface OutboxDetail {
  item: OutboxItem & { post?: string | null };
}

/**
 * The Apps board's approval word (CAD-557) — why `approved` is false:
 * a matching recorded digest, a record whose digest moved (`changed`),
 * none at all (`unapproved`), or an unreadable approval store
 * (`unknown` — never shown as granted).
 */
export type AppApproval = "approved" | "changed" | "unapproved" | "unknown";

/** Where an app was installed from — a git install pins its commit SHA. */
export interface AppSource {
  kind?: string;
  path?: string;
  url?: string;
  sha?: string;
}

/** One declared connection slot and its effective binding. */
export interface AppSlot {
  slot: string;
  /** The bound connection's name; `null` reads "unbound". */
  bound: string | null;
}

/**
 * `GET /api/apps` row — one installed app per project (`app::ls`).
 * `error` marks a row that never described (a symlink, a stray record,
 * a broken manifest) — the rest of the fields may be absent.
 */
export interface AppRow {
  project: string;
  name: string | null;
  title?: string;
  version?: string;
  /** The app's one-line purpose (`summary:` in app.md), when declared. */
  summary?: string | null;
  /** The bundle's workflow names — bare `do-check`, not `app/do-check`. */
  workflows?: string[];
  /** The card's primary action: the first workflow and its human label. */
  primary?: { workflow: string; label?: string | null } | null;
  connections?: AppSlot[];
  digest?: string;
  approved?: boolean | string | null;
  approval?: AppApproval | string;
  source?: AppSource | null;
  /** The app's default team (CAD-577): input role → agent alias. */
  team?: Record<string, string>;
  installed_at?: string;
  installed_by?: string;
  updated_at?: string;
  error?: string;
}

export interface AppsPayload {
  apps: AppRow[];
  count: number;
}

/** One step of a bundle workflow, from the canonical render (CAD-563). */
export interface AppWorkflowStep {
  /** The ticket's title, input names standing in for values. */
  title: string;
  /** The `agent:` line's value — an input name when it is `{{input}}`. */
  agent?: string | null;
  size?: string | null;
}

/** One bundle workflow's checked summary, as `app show` reports it. */
export interface AppWorkflow {
  /** The app-qualified name — `studio/do-check`. */
  name: string;
  ok?: boolean;
  errors?: string[];
  notes?: string[];
  title?: string | null;
  /** The workflow's human label (`label:`), when declared. */
  label?: string | null;
  tickets?: number | null;
  inputs?: WorkflowInput[] | null;
  /** The steps, in order (CAD-563 r2). */
  steps?: AppWorkflowStep[] | null;
  /** Inputs whose values must differ at render. */
  distinct?: string[] | null;
  uses?: string[];
}

/** A bundle rubric's name and body. */
export interface AppRubric {
  name: string;
  body: string;
}

/**
 * This app's row of `cadence app doctor` — its slot findings. Absent
 * (`null`) when the app is missing from the scan.
 */
export interface AppDoctor {
  project: string;
  app: string;
  slots_ok?: { slot: string; connection: string; verified?: string }[];
  unbound?: string[];
  unknown_connection?: { slot: string; connection: string }[];
  stray_bindings?: string[];
  connection_check?: string;
}

/**
 * `GET /api/apps/<project>/<name>` — the row plus the agent guide, the
 * checked workflow summaries, the rubrics, the install record and the
 * doctor findings.
 */
export interface AppDetail extends Omit<AppRow, "workflows"> {
  project: string;
  name: string;
  guide?: string;
  workflows?: AppWorkflow[];
  rubrics?: AppRubric[];
  record?: Record<string, unknown> | null;
  doctor?: AppDoctor | null;
}

/** One ticket of an app run's plan, as `plan_json` reports it. */
export interface RunTicket {
  id: string;
  title: string;
  status: string;
  size?: string | null;
  /** S=1, M=3, L=8 — the size-weighted progress unit. */
  weight?: number;
  owner?: string | null;
  blocked_by?: string[];
  acceptance?: number;
}

/** The plan block of a run — `plan show`'s shape (CAD-563). */
export interface AppRunPlan {
  /** `proposed` | `approved` | `rejected`. */
  state: string;
  proposed_by?: string | null;
  proposed_at?: string | null;
  decided_by?: string | null;
  decided_at?: string | null;
  reason?: string | null;
  tickets: RunTicket[];
  /** `plan.rs` counts by status (backlog, ready, doing, ...), not by bucket. */
  progress: Omit<WorkProgress, "counts"> & { counts: Record<string, number> };
}

/**
 * `GET /api/apps/<project>/<name>/runs` row — one plan/epic proposed
 * from this app's workflows, by the recorded `plan.workflow`
 * provenance (CAD-563).
 */
export interface AppRun {
  epic: string;
  title: string;
  /** The epic's derived status (rollup over its tickets). */
  status: string;
  /** `<app>/<wf>` — the provenance the epic records. */
  workflow: string;
  plan: AppRunPlan;
}

export interface AppRunsPayload {
  project: string;
  name: string;
  runs: AppRun[];
}

/**
 * `GET /api/apps/<project>/<name>/outputs` — the outbox items this
 * app's runs produced (CAD-563), each naming the runs it is attributed
 * to, plus the sends those runs staged and the operator has not
 * released. Operator-only, like `/api/outbox`: a board that cannot
 * prove the operator gets the route's refusal.
 */
export interface AppOutputsPayload {
  project: string;
  name: string;
  items: AppRunOutput[];
  /** Staged sends awaiting the operator's release (or in flight). */
  pending?: AppPendingSend[];
}

/** One published item, with the runs it is attributed to. */
export interface AppRunOutput extends OutboxItem {
  runs?: string[];
}

/** One staged send of the app's runs the operator has not released. */
export interface AppPendingSend {
  effect_id: string;
  /** `waiting` (awaiting the release) or `decided` (in flight). */
  state: string;
  title?: string | null;
  runs: string[];
}

/** CAD-561: the update another process has in flight (the marker). */
export interface PendingUpdate {
  phase: string;
  target: string;
  from?: string | null;
  by: string;
  since: number;
}

/** One in-flight turn an update waits on. */
export interface WaitingTurn {
  alias: string;
  message: string;
  state: string;
  age_secs: number;
}

/** `GET /api/update` — the Settings Update card's whole view. */
export interface UpdateStatus {
  current: { sha: string | null; version: string | null };
  check: UpdateCheck | null;
  checked_at: number | null;
  checking: boolean;
  update_available: boolean;
  change_count: number;
  changes: string[];
  migration: boolean;
  blockers: string[];
  running: boolean;
  lines: string[];
  result: Record<string, unknown> | null;
  error: string | null;
  pending: PendingUpdate | null;
  waiting: WaitingTurn[];
}

/** `cadence update --check`'s report, as the board caches it. */
export interface UpdateCheck {
  current: string | null;
  current_version: string | null;
  target: string;
  target_version: string | null;
  up_to_date: boolean;
  changes: string[];
  change_count: number;
  schema: { current: number | null; target: number | null; migration: boolean };
  lease: Record<string, unknown> | null;
  waiters: WaitingTurn[];
  blockers: string[];
}

/** `GET /api/update/banner` — non-null while an update is draining. */
export interface UpdateBanner {
  pending: PendingUpdate;
  waiting: WaitingTurn[];
  count: number;
}

/**
 * One reviewed provider capability (CAD-585/CAD-688): the exact tools,
 * scopes and effect the provider's reviewed contract declares for a
 * versioned capability id. Describes compatibility only — never
 * execution authority.
 */
export interface ConnectionCapability {
  id: string;
  version: number;
  tools: string[];
  scopes: string[];
  effect: string;
  semantics: string;
}

/**
 * One provider's reviewed connection descriptor (CAD-688): how it
 * connects (`enrollment_shapes`), its built-in accounts and the
 * capabilities it declares. `descriptor` is null when the provider
 * has no reviewed metadata — then it stays unavailable.
 */
export interface ConnectionProviderDescriptor {
  schema: number;
  provider: string;
  revision: string;
  enrollment_shapes: string[];
  builtin_accounts: string[];
  capabilities: ConnectionCapability[];
}

/**
 * CAD-1065/CAD-1085: the provider's broker-computed, non-authorizing
 * declaration of whether the approved no-send login verification is
 * offered for it. Optional and untrusted until trusted — absent,
 * unknown, `supported:false` or an inconsistent entry means the
 * operation is unavailable; it is never inferred from the provider
 * name, descriptor shape or a successful local configuration read.
 */
export interface ConnectionVerificationSupport {
  operation: "smtp-login-no-send-v1" | null;
  supported: boolean;
  reason_code: string | null;
}

/** `GET /api/connection-providers` row — one registered provider. */
export interface ConnectionProvider {
  provider: string;
  descriptor: ConnectionProviderDescriptor | null;
  descriptor_available: boolean;
  manifest_status: string;
  reviewed_pin: string | null;
  reported_pin: string | null;
  /** Always false: listing providers never checks the network. */
  network_checked: boolean;
  registration_digest: string | null;
  verification_support?: ConnectionVerificationSupport | null;
}

export interface ConnectionProvidersPayload {
  providers: ConnectionProvider[];
}

/**
 * One connection row (CAD-688): an exact provider account in this
 * workspace. `kind` is `builtin` (always present, e.g. Local outbox)
 * or `enrolled` (an operator-enrolled credential). Carries metadata
 * only — never secret bytes.
 */
/** CAD-1085: one metadata-only human credential request. No credential,
 *  fragment, digest or entry-session binding is ever a field here — the
 *  trusted entry page carries the value, never this record. */
/** GET /api/protected-entry — where the trusted credential form lives. */
export interface ProtectedEntry {
  configured: boolean;
  protected: boolean;
  origin: string | null;
  board_origin: string;
}

export interface CredentialRequest {
  request_id: string;
  request_revision: number;
  provider: string;
  verb: "enroll" | "rotate" | "verify";
  connection_id: string | null;
  /** Opaque target account for a fresh enrollment; the connection id for
   *  rotate. Human-facing text is `display_alias`. */
  target_id: string;
  display_alias: string;
  shape:
    | "token"
    | "smtp"
    | "native-model-api-key"
    | "native-model-devin-credentials"
    | null;
  scopes: string[];
  expected_credential_revision: number | null;
  expected_authorization_revision: number | null;
  custody_domain: string;
  custody_epoch: number;
  context_digest: string;
  requester_kind: "operator_cli" | "operator_ui" | "agent";
  requester_participant: string | null;
  conversation_id: string | null;
  target_agent_uid: string | null;
  state: "pending" | "submitting" | "done" | "cancelled" | "expired" | "unknown";
  status: string;
  result_code: string | null;
  verification_request_id: string | null;
  /** CAD-1085: server-selected absolute URL of the trusted entry page for
   *  this request. Present only when the deployment has a pinned separate
   *  origin; never built from the current page origin. */
  entry_url?: string | null;
  created_at_ms: number;
  updated_at_ms: number;
  expires_at_ms: number;
  entry_path: string;
  /** Read-only reconciliation; a locked key/advanced epoch is a blocker,
   *  never a success claim. Absent on the create response path only. */
  readiness?: {
    ready: boolean;
    blocker: string | null;
    custody_epoch: number | null;
    context_current: boolean;
  };
}

/** CAD-1085: an agent's durable connection/access projection — metadata
 *  only, plus the daemon's reported custody facts. */
export interface AgentAccess {
  agent: string;
  grants: {
    agent: string;
    platform: string;
    account: string;
    scopes: string[];
    granted_at: number;
    by: string;
  }[];
  connections: Connection[];
  connections_error: string | null;
  custody: {
    configured: boolean;
    unlocked: boolean;
    fenced: boolean;
    isolation: string;
    protected: boolean;
  };
  network_checked: boolean;
  execution_authority: boolean;
}

export interface Connection {
  id: string;
  provider: string;
  account: string;
  kind: "builtin" | "enrolled";
  revision: number | null;
  registration_digest: string | null;
  scopes: string[];
  /**
   * Non-secret SMTP sender projection (CAD-785): transport and
   * verified sender from host custody. Absent for other providers —
   * and never carrying the secret on any provider.
   */
  smtp?: {
    host: string;
    port: number;
    tls_mode: string;
    username: string;
    sender: string;
    sender_name: string;
  } | null;
  /**
   * CAD-1064: true when the record's exchange shape is SMTP, even if the
   * settings cannot be projected. Decide "is this an SMTP sender" from
   * this, never from `smtp` presence.
   */
  smtp_sender?: boolean;
  /** Why `smtp` is null on an SMTP sender — a typed, secret-free code. */
  smtp_error?: "custody_corrupt" | "withheld_leak" | "unavailable" | null;
  status: {
    adapter_registered: boolean;
    descriptor_available: boolean;
    custody_available: boolean;
    manifest_status: string;
    reviewed_pin: string | null;
    reported_pin: string | null;
    execution_authority: boolean;
    network_checked: boolean;
  };
}

export interface ConnectionsPayload {
  connections: Connection[];
  /** CAD-1126: enrolled SMTP senders send through the hosted
   *  `smtp.internal` pass-through (a hosted workspace). */
  hosted_smtp?: boolean;
}

export interface ConnectionPayload {
  connection: Connection;
}

/**
 * CAD-1065/CAD-1085: the approved SMTP no-send login check's closed
 * failure code — the stage the check stopped at, never provider or
 * credential text.
 */
export type ConnectionVerificationFailureCode =
  | "unsupported_provider"
  | "unsupported_deployment"
  | "unsupported_custody"
  | "stale_connection"
  | "configuration_unavailable"
  | "custody_unavailable"
  | "busy"
  | "dns_failed"
  | "destination_refused"
  | "connect_failed"
  | "tls_failed"
  | "auth_failed"
  | "timeout"
  | "quit_failed"
  | "verification_failed";

/** The step the verification was in when it stopped. */
export type ConnectionVerificationFailureStep =
  | "admission"
  | "configuration"
  | "dns"
  | "connect"
  | "tls"
  | "auth"
  | "quit";

/**
 * `POST /api/connections/<id>/test` receipt (CAD-1065/CAD-1085). All
 * fields are required; the email/delivery/sender-entitlement/execution
 * flags are always exactly `false` — a verification proves only an
 * authenticated login, never a delivery, sender entitlement or
 * execution authority. Timestamps are daemon UTC RFC3339 diagnostics,
 * not a lease/TTL. A `stale` result describes the request's identity,
 * not the current connection; `revision`/`registration_digest` carry
 * the SERVER-current tuple so the UI can tell it is stale.
 */
export interface ConnectionVerificationReceipt {
  schema: 1;
  operation: "smtp-login-no-send-v1";
  connection_id: string;
  revision: number | null;
  registration_digest: string | null;
  started_at: string;
  completed_at: string;
  status: "success" | "failed" | "unsupported" | "stale";
  network_attempted: boolean;
  authentication_verified: boolean;
  email_sent: false;
  delivery_verified: false;
  sender_entitlement_verified: false;
  execution_authority: false;
  failure: {
    code: ConnectionVerificationFailureCode;
    step: ConnectionVerificationFailureStep;
  } | null;
}

/** The typed envelope `POST /api/connections/<id>/test` returns. */
export interface ConnectionVerificationPayload {
  verification: ConnectionVerificationReceipt;
}
