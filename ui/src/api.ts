import type {
  AgentDetail,
  AgentsPayload,
  Health,
  IssueCard,
  IssueDetail,
  IssueHistoryEntry,
  MemoryCard,
  MemoryDetail,
  Meta,
  Project,
} from "./types";

export class ApiError extends Error {
  status: number;
  conflict?: string;
  check?: string;
  card?: IssueCard;
  constructor(
    message: string,
    status: number,
    opts?: { conflict?: string; check?: string; card?: IssueCard },
  ) {
    super(message);
    this.status = status;
    this.conflict = opts?.conflict;
    this.check = opts?.check;
    this.card = opts?.card;
  }
}

async function get<T>(path: string): Promise<T> {
  const resp = await fetch(path);
  if (!resp.ok) {
    const body = await resp.json().catch(() => null);
    throw new ApiError(
      body?.error ?? `${resp.status} ${resp.statusText}`,
      resp.status,
      body ?? undefined,
    );
  }
  return resp.json() as Promise<T>;
}

export interface WriteResp {
  issue: IssueDetail;
  card: IssueCard;
  warnings?: string[];
}

/**
 * The board's write shape: non-simple content type + the custom header.
 * A cross-site page cannot send either without a preflight, and the
 * server answers no preflight — so a rejected write here means a real
 * server-side refusal, not a CORS artefact.
 */
async function write<T extends object | undefined>(
  method: "POST" | "PATCH" | "DELETE",
  path: string,
  body?: T,
): Promise<WriteResp> {
  const resp = await fetch(path, {
    method,
    headers: {
      "Content-Type": "application/json",
      "X-Cadence-Board": "1",
    },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const parsed = await resp.json().catch(() => null);
  if (!resp.ok) {
    throw new ApiError(
      parsed?.error ?? `${resp.status} ${resp.statusText}`,
      resp.status,
      parsed ?? undefined,
    );
  }
  return parsed as WriteResp;
}

/** Memory writes return `{ok, write, memory}` — not the issue shape. */
async function memoryWrite(
  path: string,
  body?: object,
): Promise<{ ok: boolean; memory: MemoryDetail }> {
  const resp = await fetch(path, {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      "X-Cadence-Board": "1",
    },
    body: body === undefined ? "{}" : JSON.stringify(body),
  });
  const parsed = await resp.json().catch(() => null);
  if (!resp.ok) {
    throw new ApiError(
      parsed?.error ?? `${resp.status} ${resp.statusText}`,
      resp.status,
      parsed ?? undefined,
    );
  }
  return parsed;
}

export const api = {
  health: () => get<Health>("/api/health"),
  meta: () => get<Meta>("/api/meta"),
  projects: () => get<{ projects: Project[] }>("/api/projects"),
  issues: (project?: string) =>
    get<{ issues: IssueCard[] }>(
      project ? `/api/issues?project=${encodeURIComponent(project)}` : "/api/issues",
    ),
  issue: (id: string) => get<IssueDetail>(`/api/issues/${id}`),
  history: (id: string, limit = 10) =>
    get<{ id: string; history: IssueHistoryEntry[] }>(
      `/api/issues/${id}/history?limit=${limit}`,
    ),
  file: async (id: string) => {
    const resp = await fetch(`/api/issues/${id}/file`);
    if (!resp.ok) throw new ApiError(`${resp.status} ${resp.statusText}`, resp.status);
    return resp.text();
  },
  agents: () => get<AgentsPayload>("/api/agents"),
  agent: (alias: string) =>
    get<AgentDetail>(`/api/agents/${encodeURIComponent(alias)}`),
  memories: (opts: { project?: string; status?: string; type?: string } = {}) => {
    const q = new URLSearchParams();
    if (opts.project) q.set("project", opts.project);
    if (opts.status) q.set("status", opts.status);
    if (opts.type) q.set("type", opts.type);
    const s = q.toString();
    return get<{ memories: MemoryCard[] }>(
      `/api/memories${s ? `?${s}` : ""}`,
    );
  },
  memory: (project: string, slug: string) =>
    get<MemoryDetail>(
      `/api/memories/${encodeURIComponent(project)}/${encodeURIComponent(slug)}`,
    ),
  memoryAccept: (project: string, slug: string) =>
    memoryWrite(
      `/api/memories/${encodeURIComponent(project)}/${encodeURIComponent(slug)}/accept`,
    ),
  memoryReject: (project: string, slug: string) =>
    memoryWrite(
      `/api/memories/${encodeURIComponent(project)}/${encodeURIComponent(slug)}/reject`,
    ),

  create: (req: {
    project: string;
    title: string;
    priority?: string;
    owner?: string;
    component?: string;
    tags?: string[];
    parent?: string;
    blocked_by?: string[];
  }) => write("POST", "/api/issues", req),

  patch: (
    id: string,
    patch: {
      status?: string;
      priority?: string;
      owner?: string;
      component?: string;
      title?: string;
      body?: string;
      /** Replaces the tag list; `[]` clears it. */
      tags?: string[];
    },
    ifRev?: string,
  ) => write("PATCH", `/api/issues/${id}`, { ...patch, if_rev: ifRev }),

  link: (id: string, type: string, target: string, ifRev?: string) =>
    write("POST", `/api/issues/${id}/links`, { type, target, if_rev: ifRev }),
  unlink: (id: string, type: string, target: string, ifRev?: string) =>
    write("DELETE", `/api/issues/${id}/links`, { type, target, if_rev: ifRev }),

  addRef: (
    id: string,
    kind: string,
    target: { url?: string; path?: string },
    label?: string,
    ifRev?: string,
  ) => write("POST", `/api/issues/${id}/refs`, { kind, ...target, label, if_rev: ifRev }),

  comment: (id: string, body: string, ifRev?: string) =>
    write("POST", `/api/issues/${id}/comments`, { body, if_rev: ifRev }),

  attach: async (id: string, name: string, data: Blob): Promise<WriteResp> => {
    const resp = await fetch(
      `/api/issues/${id}/artifacts?name=${encodeURIComponent(name)}`,
      {
        method: "POST",
        headers: {
          "Content-Type": "application/octet-stream",
          "X-Cadence-Board": "1",
        },
        body: data,
      },
    );
    const parsed = await resp.json().catch(() => null);
    if (!resp.ok) {
      throw new ApiError(
        parsed?.error ?? `${resp.status} ${resp.statusText}`,
        resp.status,
        parsed ?? undefined,
      );
    }
    return parsed as WriteResp;
  },

  artifactUrl: (id: string, name: string) =>
    `/api/issues/${id}/artifacts/${encodeURIComponent(name)}`,
};
