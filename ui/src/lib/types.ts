export type StatusSource = "file" | "notes" | "rollup" | "job";

/** One bound agent on an issue card/drawer — from the job binding,
 *  never a text scan. */
export interface CardAgent {
  alias: string;
  task: string;
  task_state: string;
  state: string;
  /** Running kickoff message id for this task, if live. */
  message?: string | null;
  /** Concrete resume command for a stopped pane. */
  resume?: string | null;
}

export interface Ref {
  kind: string;
  url?: string;
  path?: string;
  label?: string;
  closed?: boolean;
}

export interface IssueCard {
  id: string;
  project: string;
  title: string;
  status: string;
  status_source: StatusSource;
  priority: string;
  owner?: string;
  component?: string;
  /** Sorted, de-duplicated slicing labels. */
  tags?: string[];
  parent?: string;
  blocked_by: string[];
  relates: string[];
  duplicate_of?: string;
  refs: Ref[];
  container: boolean;
  ready: boolean;
  blocked: boolean;
  /** Why the card is flagged blocked beyond deps — e.g. "job blocked". */
  blocked_reason?: string | null;
  /** Agents bound to this issue through a job (tasks × jobs.issue_id). */
  agents?: CardAgent[];
  created: string;
  /** Content hash of issue.md — send as `if_rev` on writes. */
  rev: string;
  counts: { comments: number; artifacts: number; refs: number };
  checks: { done: number; total: number };
  /** CAD-405 work model: type, milestone, size; stage/progress/health on epics. */
  work?: WorkBlock;
}

/** CAD-405 epic health. `stalled` is time in stage, not child activity. */
export type HealthState = "on_track" | "at_risk" | "stalled";

/** One reason an epic or milestone is not on track, with its next action. */
export interface HealthReason {
  cause: string;
  issue: string;
  owner?: string | null;
  detail: string;
  next: string;
}

export interface WorkProgress {
  done_weight: number;
  total_weight: number;
  ratio: number;
  counts: Record<string, number>;
}

/** One move `epic_stage` accepts from the epic's current stage. */
export interface StageMove {
  to: string;
  forward: boolean;
  /** Only the proven operator may make it. */
  needs_operator: boolean;
}

export interface WorkStage {
  id: string;
  source: string;
  since: string | null;
  exit: string | null;
  next: string | null;
  next_needs_operator: boolean;
  terminal: boolean;
  stages: string[];
  /** CAD-432: the legal moves; absent from a server that predates it. */
  moves?: StageMove[];
}

/** The `work` block on `/api/issues` cards and `/api/issues/:id`. */
export interface WorkBlock {
  type: string;
  milestone: string | null;
  size: string | null;
  weight: number;
  stage: WorkStage | null;
  progress: WorkProgress | null;
  health: {
    state: HealthState;
    days_in_stage: number | null;
    limit_days: number;
    reasons: HealthReason[];
  } | null;
  config_error?: string;
  config_unapproved?: string;
}

/** `GET /api/milestones` — one (project, milestone) roll-up. */
export interface MilestoneRow {
  project: string;
  id: string;
  title: string | null;
  exit: string | null;
  configured: boolean;
  progress: WorkProgress;
  health: { state: HealthState; reasons: HealthReason[] };
  epics: {
    id: string;
    title: string;
    owner?: string | null;
    stage: string | null;
    progress: number | null;
    health: HealthState;
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

export interface IssueDetail extends IssueCard {
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

export interface OverviewProject {
  key: string;
  open_by_status: Record<string, number>;
  oldest_review_age?: number | null;
  claims?: OverviewClaim[];
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
export interface AgentTask {
  task: string;
  task_state: string;
  issue: string;
  /** Task title from the job ledger, when one was supplied. */
  title?: string | null;
  /** Job id/title/state are the source of current-work context. */
  job?: string | null;
  job_title?: string | null;
  job_state?: string | null;
  message?: {
    id: string;
    task?: string;
    created?: string;
    summary?: string | null;
  } | null;
}

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

export interface Agent {
  alias: string;
  provider: string;
  endpoint_kind: string;
  state: string;
  role?: string | null;
  /** Model-preference role. Independent of runtime `role`. */
  team_role?: string | null;
  model_lookup_role?: string | null;
  model_selection?: {
    source?: string | null;
    lookup_role?: string | null;
    revision?: number | null;
    model?: string | null;
  } | null;
  group: string;
  group_root?: boolean;
  /** Provider evidence: reported effective value beside launch config. */
  model?: string | null;
  model_reported?: string | null;
  model_configured?: string | null;
  model_source?: string | null;
  effort?: string | null;
  effort_reported?: string | null;
  effort_source?: string | null;
  effort_applicable?: boolean | null;
  /** Optional account/pool allowance view; absent means unavailable. */
  quota?: UsageLimit | null;
  usage_limit?: UsageLimit | null;
  running: number;
  queued: number;
  unknown: number;
  parked: number;
  fenced: boolean;
  /** Issue ids bound through jobs — exact, not text-derived. */
  on: string[];
  tasks?: AgentTask[];
  /** First running message, reduced. */
  message?: {
    id: string;
    task?: string;
    created?: string;
    summary?: string | null;
  } | null;
  running_messages?: {
    id: string;
    task?: string;
    created?: string;
    summary?: string | null;
  }[];
  /** The daemon's fence text — already names the recovery commands. */
  recovery?: string | null;
  /** Concrete resume command for a stopped pane, e.g. `devin -r devin-x`. */
  resume?: string | null;
  /** When `resume` applies — the pane is gone until then. */
  resume_hint?: string;
  dead?: boolean;
  inbox?: boolean;
  last_activity?: string | null;
  /** Seconds since the running turn's last observed activity. */
  silent_secs?: number;
  /** The daemon declared this running turn stalled. */
  stalled?: boolean;
  /** CAD-96: `stopped (auto, idle 72m)` when the idle timer stopped it. */
  state_label?: string | null;
  event_cursor?: number;
}

export interface AgentsPayload {
  daemon: "reachable" | "unreachable";
  agents: Agent[];
  totals: {
    running: number;
    queued: number;
    fenced: number;
    parked: number;
    inboxes: number;
  } | null;
  /** issue id → bound agent strip, for cards and drawers. */
  by_issue?: Record<string, CardAgent[]>;
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
  };
  queued: number;
  unknown: number;
  running: {
    id: string;
    task?: string;
    created?: string;
    summary?: string | null;
  }[];
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
export interface Meta {
  read_only: boolean;
  /** CAD-313: this browser holds a live operator session. Absent on a
   *  server that predates sessions. */
  signed_in?: boolean;
  session?: {
    id: string;
    origin: "loopback" | "tailnet";
    created: number;
    last_used: number;
    idle_expires_at: number;
    expires_at: number;
    user_agent: string;
  } | null;
  /** The command that signs this origin in (`cadence ui login [--tailnet]`). */
  login_hint?: string;
  actor: string;
  /** CAD-432 (only with `?operator=1`): this client may make the
   *  operator's decisions — a live session (CAD-313) plus the board's
   *  operator proof on the peer and on the board process. */
  operator?: boolean;
  tailnet_url: string | null;
  version: string;
  build_commit: string;
  build_time: string;
  daemon?: {
    build_commit: string;
    build_time: string;
    started_at: number;
  } | null;
}

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
  /** Whether proposal receipts are ready for PM finalization. */
  accept?: MemoryQuorumCheck | null;
  /** Whether verification receipts are ready for PM finalization. */
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
  /** verified (inside the window) | unverified | withheld (stale mark). */
  state: "verified" | "unverified" | "withheld" | string;
  /** "verified <date>", "unverified (last verified <date>)", "unverified" or "withheld". */
  label: string;
  /** Why a withheld lesson is not injected. */
  reason?: string | null;
  /** The verify finalization time — not the raw verified_at field. */
  last_verified?: string | null;
  window_days?: number | null;
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
  finalized_operations?: MemoryFinalization[] | null;
  /** Absent on older daemons; absence means verification is unavailable. */
  quorum?: MemoryQuorum | null;
  fact: string;
  path: string;
}

/** GET /api/memories/<project>/<slug> — card + raw body. */
export interface MemoryDetail extends MemoryCard {
  body: string;
}

export interface ModelSelector {
  mode: "model" | "provider_default";
  model?: string;
}

export interface ProviderModelDefaults {
  default: ModelSelector;
  roles: Record<string, ModelSelector>;
}

export interface ModelDefaultsConfig {
  schema: number;
  providers: Record<string, ProviderModelDefaults>;
}

export interface ModelProviderInfo {
  id: string;
  label: string;
  eligible: boolean;
  kinds: string[];
  suggestions: string[];
  suggestions_note: string;
  limitation?: string | null;
}

export interface ModelRoleInfo {
  id: string;
  label: string;
}

/** GET/POST /api/settings/model-defaults */
export interface ModelDefaultsSnapshot {
  revision: number;
  config: ModelDefaultsConfig;
  providers: ModelProviderInfo[];
  roles: ModelRoleInfo[];
  read_only: boolean;
}
