import { sessionHeaders } from "./sessionKey";
import type {
  AgentDetail,
  AgentsPayload,
  AppDetail,
  AppOutputsPayload,
  AppRunsPayload,
  AppsPayload,
  Health,
  IssueCard,
  IssueDetail,
  IssueHistoryEntry,
  MasterCommandResult,
  MasterState,
  MemoryCard,
  MemoryDetail,
  Meta,
  MilestoneRow,
  ModelDefaultsConfig,
  ModelDefaultsSnapshot,
  OutboxDetail,
  UpdateBanner,
  UpdateCheck,
  UpdateStatus,
  OutboxList,
  Overview,
  Project,
  ProjectContext,
  ContextRole,
  WorkflowPreview,
  WorkflowsPayload,
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
  const resp = await fetch(path, { headers: sessionHeaders() });
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
      ...sessionHeaders(),
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
      ...sessionHeaders(),
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
  /** `withOperator` asks the server to run the operator proof (CAD-432) — once per page load. */
  meta: (withOperator = false) => get<Meta>(withOperator ? "/api/meta?operator=1" : "/api/meta"),
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
      ...sessionHeaders(),
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
      ...sessionHeaders(),
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
  /** `GET /api/projects/<key>/workflows` — the project's stored workflows (CAD-496). */
  workflows: (project: string) =>
    get<WorkflowsPayload>(`/api/projects/${encodeURIComponent(project)}/workflows`),
  /**
   * `GET /api/apps` — every installed app, one row per `<project>/<app>`
   * (CAD-557). `?project=` scopes the list to one project.
   */
  apps: (project?: string) =>
    get<AppsPayload>(project ? `/api/apps?project=${encodeURIComponent(project)}` : "/api/apps"),
  /**
   * `GET /api/apps/<project>/<name>` — one app: guide, checked workflow
   * summaries, rubrics, bindings, install record, doctor findings.
   */
  app: (project: string, name: string) =>
    get<AppDetail>(`/api/apps/${encodeURIComponent(project)}/${encodeURIComponent(name)}`),
  /**
   * `GET /api/apps/<project>/<name>/runs` — the plans/epics proposed
   * from this app's workflows, by the recorded `plan.workflow`
   * provenance (CAD-563).
   */
  appRuns: (project: string, name: string) =>
    get<AppRunsPayload>(
      `/api/apps/${encodeURIComponent(project)}/${encodeURIComponent(name)}/runs`,
    ),
  /**
   * `GET /api/apps/<project>/<name>/outputs` — the outbox items this
   * app's runs produced (CAD-563); operator-only, like `/api/outbox`.
   */
  appOutputs: (project: string, name: string) =>
    get<AppOutputsPayload>(
      `/api/apps/${encodeURIComponent(project)}/${encodeURIComponent(name)}/outputs`,
    ),
  /**
   * `POST /api/apps/<project>/<name>/approve` — relays the daemon's
   * `app_approve`; operator-only on the board, so the body is `{}` —
   * attribution is the board's proven connection, not a field.
   */
  appApprove: (project: string, name: string) =>
    post<Record<string, unknown>>(
      `/api/apps/${encodeURIComponent(project)}/${encodeURIComponent(name)}/approve`,
      {},
    ),
  /** `GET /api/outbox` — the `local` platform's published items (operator-only). */
  outbox: () => get<OutboxList>("/api/outbox"),
  /** `GET /api/outbox?effect_id=` — one item, rendered post included. */
  outboxItem: (effectId: string) =>
    get<OutboxDetail>(`/api/outbox?effect_id=${encodeURIComponent(effectId)}`),
  /**
   * `GET /api/projects/<key>/workflows/<name>/preview?inputs=<json>` —
   * what `plan propose --workflow` renders for these inputs. The query
   * is built by hand (encodeURIComponent, not URLSearchParams) because
   * the server decodes %XX but does not treat '+' as a space.
   */
  workflowPreview: (project: string, name: string, inputs: Record<string, string>) =>
    get<WorkflowPreview>(
      `/api/projects/${encodeURIComponent(project)}/workflows/${encodeURIComponent(name)}/preview` +
        `?inputs=${encodeURIComponent(JSON.stringify(inputs))}`,
    ),
  /**
   * `POST /api/projects/<key>/workflows/<name>/propose` — relays the
   * daemon's `plan_propose` (the same call `cadence plan propose
   * --workflow` makes); operator-only on the board.
   */
  workflowPropose: (project: string, name: string, inputs: Record<string, string>) =>
    post<Record<string, unknown>>(
      `/api/projects/${encodeURIComponent(project)}/workflows/${encodeURIComponent(name)}/propose`,
      { inputs },
    ),
  /** `GET /api/milestones?project=` — progress and worst health per milestone. */
  milestones: (project: string) =>
    get<{ milestones: MilestoneRow[] }>(`/api/milestones?project=${encodeURIComponent(project)}`),
  /** `POST /api/epics/<id>/stage` — relays `epic_stage`; operator-only on the board. */
  moveStage: (epic: string, stage: string, note?: string) =>
    post<{ epic: string; from: string; to: string; by: string; at: string }>(
      `/api/epics/${encodeURIComponent(epic)}/stage`,
      note && note.trim() ? { stage, note: note.trim() } : { stage },
    ),
  /** `POST /api/delivery/<id>/merge` — the operator's merge decision
   *  (CAD-431): the board's own `gh` enqueues the PR pinned to the
   *  reviewed head. */
  mergeDelivery: (issue: string) =>
    post<Record<string, unknown>>(`/api/delivery/${encodeURIComponent(issue)}/merge`, {}),
  /** `GET /api/master/summary?since=` — 501 on a daemon without it. */
  masterSummary: (since: number) =>
    get<Record<string, unknown>>(`/api/master/summary?since=${Math.floor(since)}`),
  /** `GET /api/master/state` — session chips + in-flight turn (CAD-551);
   *  501 on a daemon that predates it. */
  masterState: () => get<MasterState>("/api/master/state"),
  /** `POST /api/master/command` — an allowlisted session verb (CAD-551);
   *  operator-only, 400 `unknown_command` outside the board's set. */
  masterCommand: (command: string, arg?: string, wait?: number) =>
    post<MasterCommandResult>(
      "/api/master/command",
      wait !== undefined ? { command, arg, wait } : arg ? { command, arg } : { command },
    ),

  modelDefaults: () => get<ModelDefaultsSnapshot>("/api/settings/model-defaults"),
  /** CAD-561: the Settings Update card — current version, the last
   *  check, the running update's progress and the drain state. */
  updateStatus: () => get<UpdateStatus>("/api/update"),
  /** The draining banner, cheap enough for every page to poll. */
  updateBanner: () => get<UpdateBanner | null>("/api/update/banner"),
  /** Run the real check (gh) now — the operator's Check for updates. */
  updateCheck: () => post<UpdateCheck>("/api/update/check", {}),
  /** Start the update: the same pipeline as `cadence update`. */
  startUpdate: () => post<{ started: boolean }>("/api/update", {}),
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
      ...sessionHeaders(),
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
