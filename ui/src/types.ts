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
}

export interface Project {
  key: string;
  prefix: string;
  components: string[];
  default_owner?: string;
  repos: { path?: string; remote?: string }[];
  issues: number;
}

/** A task assignment an agent is bound to through its job. */
export interface AgentTask {
  task: string;
  task_state: string;
  issue: string;
  message?: { id: string; task?: string; turn_id?: string; created?: string } | null;
}

export interface Agent {
  alias: string;
  provider: string;
  endpoint_kind: string;
  state: string;
  group: string;
  group_root?: boolean;
  running: number;
  queued: number;
  unknown: number;
  parked: number;
  fenced: boolean;
  /** Issue ids bound through jobs — exact, not text-derived. */
  on: string[];
  tasks?: AgentTask[];
  /** First running message, reduced. */
  message?: { id: string; task?: string; turn_id?: string; created?: string } | null;
  running_messages?: { id: string; task?: string; turn_id?: string; created?: string }[];
  /** The daemon's fence text — already names the recovery commands. */
  recovery?: string | null;
  /** Concrete resume command for a stopped pane, e.g. `devin -r devin-x`. */
  resume?: string | null;
  /** When `resume` applies — the pane is gone until then. */
  resume_hint?: string;
  dead?: boolean;
  inbox?: boolean;
  last_activity?: string | null;
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
    pid?: number | null;
    state: string;
    enabled: boolean;
    error?: string | null;
    endpoint?: string | null;
    params?: Record<string, unknown>;
    generation?: number;
    dead?: boolean;
    capabilities?: Record<string, unknown> | null;
    tasks?: string[];
  };
  queued: number;
  unknown: number;
  running: { id: string; task?: string; turn_id?: string; created?: string }[];
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

export interface Health {
  ok: boolean;
  pm_dir?: string;
  pm_present: boolean;
  projects: number;
  issues: number;
  daemon: string;
  embedded: boolean;
}
