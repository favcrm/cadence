import { api } from "./api";
import { QueryCache, type Resource } from "./cache";
import { loadThread, type PageReader, type ThreadState } from "../features/home/thread";

/** Page reads of one agent's thread. */
export function threadReader(alias: string): PageReader {
  return {
    after: (after, limit) => api.thread(alias, { after, limit }),
    tail: (limit) => api.thread(alias, { tail: true, limit }),
    before: (before, limit) => api.thread(alias, { before, limit }),
  };
}
import type {
  AgentsPayload,
  AppDetail,
  AppOutputsPayload,
  AppRow,
  AppRun,
  IssueCard,
  IssueDetail,
  MasterState,
  MilestoneRow,
  OutboxItem,
  Overview,
  Project,
  WorkflowRow,
} from "./types";

/** The app's one query cache — every screen reads its stores from here. */
export const cache = new QueryCache();

/**
 * `GET /api/threads/master` — the master's thread (CAD-328). A fetch
 * loads only what the store does not hold yet and keeps its pending
 * messages; the Home screen streams the rest in (`streamInto`).
 */
const masterThread: Resource<ThreadState> = cache.resource<ThreadState>("thread:master", () =>
  loadThread(threadReader("master"), masterThread.get().data),
);

/**
 * The board's resources — one store each, shared by every screen. List
 * names match the `resources` the server lists on `/api/stream` frames
 * (`event_resources` in src/ui.rs); `issue` is a family keyed by id, and
 * App reloads only the open drawer's entry on an `issue` frame.
 */
export const resources = {
  /** `GET /api/issues` — cards, derived status, job-bound agents. */
  issues: cache.resource<IssueCard[]>("issues", () => api.issues().then((r) => r.issues), {
    isEmpty: (rows) => rows.length === 0,
  }),
  /** `GET /api/projects` — project config (key, prefix, repos). */
  projects: cache.resource<Project[]>("projects", () => api.projects().then((r) => r.projects), {
    isEmpty: (rows) => rows.length === 0,
  }),
  /** `GET /api/agents` — agent rows, task bindings and `by_issue`. */
  agents: cache.resource<AgentsPayload>("agents", () => api.agents(), {
    isEmpty: (p) => p.agents.length === 0,
  }),
  /**
   * `GET /api/overview` — slow (daemon probe + gh cache, ~30 s cold); fetched
   * while Home is on screen. Fresh for 30 s, so going back to Home repaints
   * the last payload instead of starting another slow request.
   */
  overview: cache.resource<Overview>("overview", () => api.overview(), { freshMs: 30_000 }),
  masterThread,
  /**
   * `GET /api/master/state` — the master's provider session chips and
   * in-flight turn (CAD-551). A daemon that predates the route fails the
   * fetch and the header falls back to the agents row. Home revalidates
   * it when `agents` moves and after a `masterCommand`.
   */
  masterState: cache.resource<MasterState>("masterState", () => api.masterState()),
  /** `GET /api/issues/<id>` — one drawer's detail (and a plan card's epic). */
  issue: cache.family<string, IssueDetail>("issue", (id) => api.issue(id)),
  /**
   * `GET /api/issues/<id>/lane` — live lane state. Not in `issue.md`, so
   * the focus poll and an agents stream frame invalidate it with the issue.
   */
  lane: cache.family("lane", (id: string) => api.lane(id)),
  /** `GET /api/milestones?project=` — one project's milestone roll-ups (CAD-432). */
  milestones: cache.family<string, MilestoneRow[]>(
    "milestones",
    (project) => api.milestones(project).then((r) => r.milestones),
    { isEmpty: (rows) => rows.length === 0 },
  ),
  /** `GET /api/projects/<key>/workflows` — the project's stored workflows (CAD-496). */
  workflows: cache.family<string, WorkflowRow[]>(
    "workflows",
    (project) => api.workflows(project).then((r) => r.workflows),
    { isEmpty: (rows) => rows.length === 0 },
  ),
  /**
   * `GET /api/apps` — every installed app across the projects (CAD-557).
   * The list is one store; a scoped Apps screen filters client-side so
   * changing the project scope does not refetch.
   */
  apps: cache.resource<AppRow[]>("apps", () => api.apps().then((r) => r.apps), {
    isEmpty: (rows) => rows.length === 0,
  }),
  /**
   * `GET /api/apps/<project>/<name>` — one app's detail, keyed
   * `<project>/<name>` (app names are tag-shaped, so `/` never splits
   * inside one).
   */
  app: cache.family<string, AppDetail>("app", (key) => {
    const [project, name] = key.split("/");
    return api.app(project, name);
  }),
  /**
   * `GET /api/apps/<project>/<name>/runs` — the plans/epics proposed
   * from this app's workflows (CAD-563), keyed `<project>/<name>`.
   */
  appRuns: cache.family<string, AppRun[]>(
    "app_runs",
    (key) => {
      const [project, name] = key.split("/");
      return api.appRuns(project, name).then((r) => r.runs);
    },
    { isEmpty: (rows) => rows.length === 0 },
  ),
  /**
   * `GET /api/apps/<project>/<name>/outputs` — the outbox items this
   * app's runs produced and the sends awaiting release (CAD-563),
   * keyed `<project>/<name>`. Operator-only: the screen fetches it
   * only when the board proves the operator, so a viewer without the
   * read never sees the route's 403.
   */
  appOutputs: cache.family<string, AppOutputsPayload>(
    "app_outputs",
    (key) => {
      const [project, name] = key.split("/");
      return api.appOutputs(project, name);
    },
    { isEmpty: (p) => p.items.length === 0 && (p.pending ?? []).length === 0 },
  ),
  /**
   * `GET /api/outbox` — the `local` platform's published items (CAD-546).
   * Operator-only: on an unsigned board the fetch fails and the screen
   * shows its sign-in hint.
   */
  outbox: cache.resource<OutboxItem[]>("outbox", () => api.outbox().then((r) => r.items), {
    isEmpty: (rows) => rows.length === 0,
  }),
};
