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
    | "other";
  summary: string;
  fields?: Record<string, string | null>;
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

/** One `needs_me` row — what is waiting on a human, with the command. */
export interface NeedsMe {
  kind: string;
  title: string;
  /** Seconds in this state. */
  age: number;
  project: string;
  link?: string | null;
  command: string;
}

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

export interface OverviewProject {
  key: string;
  open_by_status: Record<string, number>;
  oldest_review_age?: number | null;
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
  github: { state: "ok" | "cached" | "stale" | "unavailable"; error?: string | null; at?: number };
  daemon: { reachable: boolean; build_commit?: string; build_time?: string; started_at?: number; info?: string };
  monitoring?: Monitoring;
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
    turn_id?: string;
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
    turn_id?: string;
    created?: string;
    summary?: string | null;
  } | null;
  running_messages?: {
    id: string;
    task?: string;
    turn_id?: string;
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
    turn_id?: string;
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
  actor: string;
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
  supersedes?: string | null;
  fact: string;
  path: string;
}

/** GET /api/memories/<project>/<slug> — card + raw body. */
export interface MemoryDetail extends MemoryCard {
  body: string;
}
