import { api } from "./api";
import { QueryCache } from "./cache";
import type { AgentsPayload, IssueCard, IssueDetail, Overview, Project } from "./types";

/** The app's one query cache — every screen reads its stores from here. */
export const cache = new QueryCache();

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
  /** `GET /api/issues/<id>` — one drawer's detail. */
  issue: cache.family<string, IssueDetail>("issue", (id) => api.issue(id)),
};
