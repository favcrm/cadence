import { api } from "./api";
import { Resource } from "./cache";
import type { AgentsPayload, IssueCard, Overview, Project } from "./types";

/**
 * The board's list resources — one store each, shared by every screen.
 * Names match the `resources` the server lists on `/api/stream` frames
 * (`event_resources` in src/ui.rs); the open issue detail (`issue`) is
 * reloaded by App because its key changes with the drawer.
 */
export const resources = {
  /** `GET /api/issues` — cards, derived status, job-bound agents. */
  issues: new Resource<IssueCard[]>(() => api.issues().then((r) => r.issues), {
    isEmpty: (rows) => rows.length === 0,
  }),
  /** `GET /api/projects` — project config (key, prefix, repos). */
  projects: new Resource<Project[]>(() => api.projects().then((r) => r.projects), {
    isEmpty: (rows) => rows.length === 0,
  }),
  /** `GET /api/agents` — agent rows, task bindings and `by_issue`. */
  agents: new Resource<AgentsPayload>(() => api.agents(), {
    isEmpty: (p) => p.agents.length === 0,
  }),
  /** `GET /api/overview` — slow (daemon probe + gh cache); fetched while visible. */
  overview: new Resource<Overview>(() => api.overview()),
};
