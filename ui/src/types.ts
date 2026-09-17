export type StatusSource = "file" | "notes" | "rollup";

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
  created: string;
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

export interface Agent {
  alias: string;
  provider: string;
  endpoint_kind: string;
  state: string;
  group: string;
  running: number;
  queued: number;
  unknown: number;
  parked: number;
  fenced: boolean;
  on: string[];
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
