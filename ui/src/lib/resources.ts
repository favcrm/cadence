import { api, ApiError, type MyWorkMembership, type MyWorkPayload } from "./api";
import { normalizeAgentsPayload } from "./agentOn";
import { myWorkApiQuery, myWorkViewKey, type MyWorkView } from "./myWorkQuery";
import { QueryCache, type Resource } from "./cache";
import { loadThread, type PageReader, type ThreadState } from "../features/home/thread";
import { onPrivateReadsPaused, onPrivateSessionReset } from "./privateSession";

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
  Connection,
  ConnectionProvider,
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

const unavailablePrivateRecord = (error: unknown) => error instanceof ApiError &&
  (error.status === 401 || error.status === 403 || error.status === 404);

let myWorkController: AbortController | null = null;
const unavailableIssueListeners = new Set<(id: string) => void>();

/** Rows per page, and the most one request may ask for. */
export const MY_WORK_PAGE = 50;
const MY_WORK_PAGE_MAX = 100;
/** A refresh re-reads at most this many rows; past it the list restarts at one page. */
const MY_WORK_REFRESH_MAX = 500;

/** The list as loaded: the server's pages, joined, and the view they answer. */
export interface MyWorkList extends MyWorkPayload {
  viewKey: string;
}

let myWorkView: MyWorkView = {};

function abortMyWorkRead(): void {
  myWorkController?.abort();
}

onPrivateReadsPaused(abortMyWorkRead);
onPrivateSessionReset(abortMyWorkRead);

/**
 * Reads the current view. A refresh of the view already shown re-reads as
 * many rows as are loaded, up to MY_WORK_REFRESH_MAX, so a live update or
 * Refresh keeps the rows "Load more" added; past that bound it restarts at
 * one page.
 */
async function readMyWork(): Promise<MyWorkList> {
  abortMyWorkRead();
  const controller = new AbortController();
  myWorkController = controller;
  const view = myWorkView;
  const viewKey = myWorkViewKey(view);
  const shown = resources.myWork.get().data;
  const loaded = shown?.viewKey === viewKey ? shown.issues.length : 0;
  const want = Math.max(MY_WORK_PAGE, loaded > MY_WORK_REFRESH_MAX ? 0 : loaded);
  try {
    let body = await api.myWork(myWorkApiQuery(view, { limit: Math.min(want, MY_WORK_PAGE_MAX) }), controller.signal);
    while (body.next_cursor && body.issues.length < want) {
      const limit = Math.min(want - body.issues.length, MY_WORK_PAGE_MAX);
      const next = await api.myWork(myWorkApiQuery(view, { cursor: body.next_cursor, limit }), controller.signal);
      body = { ...next, issues: [...body.issues, ...next.issues] };
    }
    return { ...body, viewKey };
  } finally {
    if (myWorkController === controller) myWorkController = null;
  }
}

/** Point the list at `view`. A different view replaces the pending read. */
export function showMyWorkView(view: MyWorkView): void {
  if (myWorkViewKey(view) === myWorkViewKey(myWorkView)) {
    void resources.myWork.revalidate();
    return;
  }
  myWorkView = view;
  void refreshMyWork();
}

/** Explicit refresh replaces the pending read without dropping same-session rows. */
export function refreshMyWork(): Promise<void> {
  abortMyWorkRead();
  resources.myWork.pause();
  return resources.myWork.refresh();
}

let loadingMore: Promise<void> | null = null;

/**
 * Append the next page of the view on screen. A refresh or a view change
 * in the meantime wins: the page is dropped unless it continues the list
 * it was asked for.
 */
export function loadMoreMyWork(): Promise<void> {
  const shown = resources.myWork.get();
  const list = shown.data;
  if (loadingMore || shown.inFlight || !list?.next_cursor || list.viewKey !== myWorkViewKey(myWorkView)) {
    return loadingMore ?? Promise.resolve();
  }
  const cursor = list.next_cursor;
  const view = myWorkView;
  loadingMore = api.myWork(myWorkApiQuery(view, { cursor, limit: MY_WORK_PAGE }))
    .then((next) => {
      resources.myWork.mutate((current) => current.viewKey !== list.viewKey || current.next_cursor !== cursor
        ? current
        : { ...next, viewKey: current.viewKey, issues: [...current.issues, ...next.issues] });
    })
    .finally(() => { loadingMore = null; });
  return loadingMore;
}

export function onIssueUnavailable(listener: (id: string) => void): () => void {
  unavailableIssueListeners.add(listener);
  return () => { unavailableIssueListeners.delete(listener); };
}

function retirePersonalIssue(id: string): void {
  abortMyWorkRead();
  resources.myWork.pause();
  resources.myWork.mutate((body) => ({ ...body, issues: body.issues.filter((row) => row.id !== id) }));
  for (const listener of unavailableIssueListeners) listener(id);
  if (resources.myWork.observed()) void resources.myWork.refresh();
}

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
  /** Caller-scoped personal projection, one view at a time; private-cache reset retires it with the session. */
  myWork: cache.resource<MyWorkList>("myWork", readMyWork, {
    isEmpty: (body) => body.issues.length === 0,
    discardOnError: unavailablePrivateRecord,
  }),
  /**
   * `GET /api/my-work/issues/<id>` — whether the peeked issue is still the
   * caller's, and its recorded handoff. The list holds one page, so the
   * peek's write guard asks the server instead of searching the page.
   */
  myWorkIssue: cache.family<string, MyWorkMembership>("myWorkIssue", (id) => api.myWorkIssue(id), {
    discardOnError: unavailablePrivateRecord,
  }),
  /** `GET /api/projects` — project config (key, prefix, repos). */
  projects: cache.resource<Project[]>("projects", () => api.projects().then((r) => r.projects), {
    isEmpty: (rows) => rows.length === 0,
  }),
  /** `GET /api/agents` — agent rows, task bindings and `by_issue`. */
  agents: cache.resource<AgentsPayload>("agents", () => api.agents().then(normalizeAgentsPayload), {
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
  masterState: cache.resource<MasterState>("master", () => api.masterState()),
  /** Hosted identity/status for one server-selected conversation, never Home's cached actor. */
  masterConversation: cache.family<string, MasterState>("masterConversation", (id) => api.masterState(id), {
    discardOnError: unavailablePrivateRecord,
  }),
  /** `GET /api/issues/<id>` — one drawer's detail (and a plan card's epic). */
  issue: cache.family<string, IssueDetail>("issue", (id) => api.issue(id), (id) => ({
    discardOnError: unavailablePrivateRecord,
    onDiscard: () => retirePersonalIssue(id),
  })),
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
   * One store for the whole list: the Apps home lists every legacy
   * project app (CAD-1607) and names each card's project.
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
  /**
   * `GET /api/connection-providers` — every registered provider with
   * its reviewed descriptor (CAD-585). Operator-only: the Settings
   * Connections page fetches it only when the board proves the
   * operator.
   */
  connectionProviders: cache.resource<ConnectionProvider[]>(
    "connectionProviders",
    () =>
      api
        .connectionProviders()
        .then((r) => r.providers)
        .catch((error: unknown) => {
          // A tenant board does not carry the provider catalogue. That is a
          // known empty directory, not a transient read failure; other
          // failures stay failed so the catalogue can be retried.
          if (error instanceof ApiError && error.status === 501) return [];
          throw error;
        }),
    { isEmpty: (rows) => rows.length === 0 },
  ),
  /**
   * `GET /api/connections` — every exact provider account, metadata
   * only (CAD-585). Operator-only, like the providers read.
   */
  connections: cache.resource<Connection[]>("connections", () =>
    api.connections().then((r) => r.connections),
  ),
};
