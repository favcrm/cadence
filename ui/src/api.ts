import type {
  AgentsPayload,
  Health,
  IssueCard,
  IssueDetail,
  Project,
} from "./types";

async function get<T>(path: string): Promise<T> {
  const resp = await fetch(path);
  if (!resp.ok) {
    const body = await resp.json().catch(() => null);
    throw new Error(body?.error ?? `${resp.status} ${resp.statusText}`);
  }
  return resp.json() as Promise<T>;
}

export const api = {
  health: () => get<Health>("/api/health"),
  projects: () => get<{ projects: Project[] }>("/api/projects"),
  issues: (project?: string) =>
    get<{ issues: IssueCard[] }>(
      project ? `/api/issues?project=${encodeURIComponent(project)}` : "/api/issues",
    ),
  issue: (id: string) => get<IssueDetail>(`/api/issues/${id}`),
  file: async (id: string) => {
    const resp = await fetch(`/api/issues/${id}/file`);
    if (!resp.ok) throw new Error(`${resp.status} ${resp.statusText}`);
    return resp.text();
  },
  agents: () => get<AgentsPayload>("/api/agents"),
};
