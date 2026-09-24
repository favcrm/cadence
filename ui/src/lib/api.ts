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
  ModelDefaultsConfig,
  ModelDefaultsSnapshot,
  Overview,
  Project,
  ProjectContext,
  ContextRole,
} from "./types";

/** `GET /api/threads/<alias>` — `thread` is null until the first message. */
export interface ThreadPage {
  alias?: string;
  thread: { id: string; alias: string; created: string; updated: string } | null;
  entries: unknown[];
  cursor?: number;
  /** Backward reads (`tail`/`before`): older entries remain. */
  more_before?: boolean;
}

export class ApiError extends Error {
  status: number;
  conflict?: string;
  check?: string;
  code?: string;
  revision?: number | null;
  card?: IssueCard;
  constructor(
    message: string,
    status: number,
    opts?: {
      conflict?: string;
      check?: string;
      code?: string;
      revision?: number | null;
      card?: IssueCard;
    },
  ) {
    super(message);
    this.status = status;
    this.conflict = opts?.conflict;
    this.check = opts?.check;
    this.code = opts?.code;
    this.revision = opts?.revision;
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

/** A board write that answers its own JSON (not an issue `WriteResp`). */
async function post<T>(path: string, body: object): Promise<T> {
  const resp = await fetch(path, {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      "X-Cadence-Board": "1",
    },
    body: JSON.stringify(body),
  });
  const parsed = await resp.json().catch(() => null);
  if (!resp.ok) {
    throw new ApiError(
      parsed?.error ?? `${resp.status} ${resp.statusText}`,
      resp.status,
      parsed ?? undefined,
    );
  }
  return parsed as T;
}

/** A plan decision's request: approve takes nothing, reject a reason. */
export function planDecision(
  epic: string,
  verb: "approve" | "reject",
  reason?: string,
): { path: string; body: { reason?: string } } {
  const path = `/api/plans/${encodeURIComponent(epic)}/${verb}`;
  if (verb === "approve") return { path, body: {} };
  const why = (reason ?? "").trim();
  if (!why) throw new ApiError("a rejection needs a reason", 400, { code: "reason_required" });
  return { path, body: { reason: why } };
}

export const api = {
  health: () => get<Health>("/api/health"),
  meta: () => get<Meta>("/api/meta"),
  overview: () => get<Overview>("/api/overview"),
  projects: () => get<{ projects: Project[] }>("/api/projects"),
  projectContext: (project: string, role: ContextRole = "pm", expectedRevision?: string) => {
    const query = new URLSearchParams({ role });
    if (expectedRevision) query.set("expected_revision", expectedRevision);
    return get<ProjectContext>(
      `/api/projects/${encodeURIComponent(project)}/context?${query.toString()}`,
    );
  },
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
  monitorAck: (monitor: string, alert: number) =>
    fetch(
      `/api/monitors/${encodeURIComponent(monitor)}/alerts/${alert}/ack`,
      {
        method: "POST",
        headers: {
          "Content-Type": "application/json",
          "X-Cadence-Board": "1",
        },
        body: "{}",
      },
    ).then(async (resp) => {
      const parsed = await resp.json().catch(() => null);
      if (!resp.ok) {
        throw new ApiError(
          parsed?.error ?? `${resp.status} ${resp.statusText}`,
          resp.status,
          parsed ?? undefined,
        );
      }
      return parsed as { ok: boolean; alert: unknown };
    }),
  memories: (opts: { project?: string; status?: string; type?: string } = {}) => {
    const q = new URLSearchParams();
    if (opts.project) q.set("project", opts.project);
    if (opts.status) q.set("status", opts.status);
    if (opts.type) q.set("type", opts.type);
    const s = q.toString();
    return get<{ memories: MemoryCard[]; memory_errors?: string[] }>(
      `/api/memories${s ? `?${s}` : ""}`,
    );
  },
  memory: (project: string, slug: string) =>
    get<MemoryDetail>(
      `/api/memories/${encodeURIComponent(project)}/${encodeURIComponent(slug)}`,
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

  /** `GET /api/threads/<alias>` — one page after `after`. */
  thread: (alias: string, at: { after?: number; before?: number; tail?: boolean; limit?: number } = {}) => {
    const q = new URLSearchParams({ limit: String(at.limit ?? 200) });
    if (at.tail) q.set("tail", "1");
    else if (at.before !== undefined) q.set("before", String(at.before));
    else q.set("after", String(at.after ?? 0));
    return get<ThreadPage>(`/api/threads/${encodeURIComponent(alias)}?${q.toString()}`);
  },
  /** `POST /api/threads/<alias>/messages` — `message` makes a retry idempotent. */
  threadSend: (alias: string, text: string, message: string) =>
    post<Record<string, unknown>>(`/api/threads/${encodeURIComponent(alias)}/messages`, {
      text,
      message,
    }),
  /** `POST /api/plans/<epic>/approve|reject` — operator-only (CAD-328). */
  decidePlan: (epic: string, verb: "approve" | "reject", reason?: string) => {
    let req: ReturnType<typeof planDecision>;
    try {
      req = planDecision(epic, verb, reason);
    } catch (e) {
      return Promise.reject(e);
    }
    return post<Record<string, unknown>>(req.path, req.body);
  },
  /** `POST /api/issues/<id>/answers` — the operator's answer report. */
  answer: (issue: string, question: string, text: string) =>
    write("POST", `/api/issues/${encodeURIComponent(issue)}/answers`, { question, text }),
  /** `GET /api/master/summary?since=` — 501 on a daemon without it. */
  masterSummary: (since: number) =>
    get<Record<string, unknown>>(`/api/master/summary?since=${Math.floor(since)}`),

  modelDefaults: () => get<ModelDefaultsSnapshot>("/api/settings/model-defaults"),
  saveModelDefaults: (body: { expected_revision: number; config: ModelDefaultsConfig }) =>
    writeSettings(body),
};

async function writeSettings(body: {
  expected_revision: number;
  config: ModelDefaultsConfig;
}): Promise<ModelDefaultsSnapshot> {
  const resp = await fetch("/api/settings/model-defaults", {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      "X-Cadence-Board": "1",
    },
    body: JSON.stringify(body),
  });
  const parsed = await resp.json().catch(() => null);
  if (!resp.ok) {
    throw new ApiError(
      parsed?.error ?? `${resp.status} ${resp.statusText}`,
      resp.status,
      parsed ?? undefined,
    );
  }
  return parsed as ModelDefaultsSnapshot;
}
