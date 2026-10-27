import type { LanePayload } from "../features/issues/lane";
import { ConditionalGet } from "./conditionalGet";
import { ApiError, errorMessage } from "./apiError";
import { sessionKey, sessionHeaders } from "./sessionKey";
import {
  bindPrivateAbort,
  notifyPrivateSessionRequired,
  privateFetch,
  privateReadsAllowed,
  privateSessionGeneration,
  usingPrivateOperation,
  type PrivateOperation,
} from "./privateSession";
import type {
  AgentDetail,
  AgentRegisterBody,
  AgentRolesPayload,
  InstructionsResult,
  AgentsPayload,
  AppDetail,
  AppOutputsPayload,
  AppRunsPayload,
  AppsPayload,
  ConnectionPayload,
  ConnectionProvidersPayload,
  ConnectionsPayload,
  ConnectionVerificationPayload,
  Health,
  IssueCard,
  IssueDetail,
  IssueHistoryEntry,
  MasterCommandResult,
  MasterIdentity,
  MasterModels,
  MasterState,
  MemoryCard,
  MemoryDetail,
  Meta,
  MilestoneRow,
  AgentCatalog,
  AgentPreviewResult,
  AgentSettingsConfig,
  AgentSettingsSnapshot,
  AgentSyncResult,
  OutboxDetail,
  UpdateBanner,
  UpdateCheck,
  UpdateStatus,
  OutboxList,
  Overview,
  Project,
  ProjectContext,
  ContextRole,
  ThreadRef,
  WorkflowPreview,
  WorkflowsPayload,
  AvatarLook,
} from "./types";

/** The daemon-minted retained-file row `POST /api/chat/upload` answers
 *  (CAD-1168): the id is the only handle; metadata is server-derived. */
export interface ChatFileRow {
  id: string;
  sha256: string;
  size: number;
  mime: string;
  name: string;
  scope: string;
  context_id: string;
  uploader: string;
  user_id: string;
  thread_id: string;
  location_version: 1;
  created: number;
}

/** Validate the daemon's ChatFile::to_json receipt before a row can become
 *  ready. This checks the wire shape only; native scope proofs remain the
 *  authority for upload and later reads. */
function decodeChatFileRow(value: unknown): ChatFileRow {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    throw new Error("upload returned an invalid attachment receipt — retry or remove the file");
  }
  const row = value as Record<string, unknown>;
  if (
    typeof row.id !== "string" || !/^chf-[0-9a-f]{32}$/.test(row.id) ||
    typeof row.sha256 !== "string" || !/^[0-9a-f]{64}$/.test(row.sha256) ||
    typeof row.size !== "number" || !Number.isSafeInteger(row.size) || row.size <= 0 ||
    typeof row.mime !== "string" || row.mime.length === 0 ||
    typeof row.name !== "string" || row.name.trim().length === 0 ||
    typeof row.scope !== "string" || row.scope.length === 0 ||
    typeof row.context_id !== "string" ||
    typeof row.uploader !== "string" || row.uploader.length === 0 ||
    typeof row.user_id !== "string" || !/^acu-[0-9a-f]{32}$/.test(row.user_id) ||
    typeof row.thread_id !== "string" || row.thread_id.length === 0 ||
    row.location_version !== 1 ||
    typeof row.created !== "number" || !Number.isFinite(row.created)
  ) {
    throw new Error("upload returned an invalid attachment receipt — retry or remove the file");
  }
  return row as unknown as ChatFileRow;
}

/** Immutable native selector for a retained file. Home may carry its
 *  real conversation id without an app binding; app destinations also
 *  carry the install and optional context. */
export type ChatFileDestination =
  | { install_id: string; conversation: string; context_id?: string }
  | { conversation: string };

/** Native-served capability state; a hint only, never upload authority. */
export interface AppFileUploadProjection {
  declared: boolean;
  available: boolean;
}

/** `GET /api/threads/<alias>` — `thread` is null until the first message. */
export interface ThreadPage {
  alias?: string;
  thread: { id: string; alias: string; created: string; updated: string } | null;
  entries: unknown[];
  cursor?: number;
  /** Backward reads (`tail`/`before`): older entries remain. */
  more_before?: boolean;
  /** CAD-1700: the complete thread's newest visible assistant reply seq
   *  (0 when none), independent of the page cursor. */
  reply_seq?: number;
}

/** CAD-1485: `POST /api/member-assistant/state` — the member's assistant
 *  binding (daemon-owned `to_json`) plus the bound conversation id. */
export interface MemberAssistState {
  assistant: {
    org: string;
    sub: string;
    assistant_id: string;
    alias: string;
    conversation_id: string | null;
    state: string;
    bootstrap_state?: string | null;
    authority_rev?: number;
    created?: number;
    updated?: number;
    /** CAD-1817: the binding's committed identity. `name`/`tone` null
     *  mean unset; `look` is always a canonical catalog slug; `revision`
     *  is the binding's integer bootstrap revision. An unreadable
     *  identity fails the read rather than silently reporting unset. */
    identity: MemberAssistantIdentity;
  };
  conversation_id: string | null;
}

/** The private assistant's committed identity projection. User address
 *  and preferences are deliberately absent — they stay private and are
 *  not part of the displayed identity. */
export interface MemberAssistantIdentity {
  name: string | null;
  tone: string | null;
  look: AvatarLook;
  revision: number;
}

/** `POST /api/master/identity` (CAD-1817): the proven operator's
 *  workspace-identity edit. Only the supplied fields change; an empty
 *  string clears `name`/`tone`. */
export interface MasterIdentityUpdate {
  expected_revision: string;
  name?: string;
  tone?: string;
  look?: AvatarLook;
}

/** The committed result and when it takes effect for the coordinator. */
export interface MasterIdentityUpdateResult {
  identity: MasterIdentity;
  applies: "next_turn";
}

/** `POST /api/member-assistant/history` — the member's bound conversation
 *  page. `entries` carry the stored `role`/`kind`/`seq`; `cursor` is the
 *  forward resume watermark. A member names no thread — the binding's
 *  own conversation is always the read. */
export interface MemberAssistHistory {
  conversation: { id: string; alias: string; created?: string; updated?: string } | null;
  entries: unknown[];
  cursor?: number;
  more_before?: boolean;
}

/** `POST /api/member-assistant/send` receipt: the accepted message id and
 *  queue state. `queued`/`attention` tell the truth when the assistant's
 *  endpoint could not start — the row stays queued, never a fabricated
 *  reply. */
export interface MemberAssistSend {
  message: string;
  state: string;
  duplicate?: boolean;
  queued?: boolean;
  attention?: string;
}

export { ApiError } from "./apiError";

const conditional = new ConditionalGet(
  (input, init) => fetch(input, init),
  () => `${sessionKey() ?? ""}:${privateSessionGeneration()}`,
);

async function readConditional<T>(path: string, signal: AbortSignal | undefined, operation: PrivateOperation | null): Promise<T> {
  const protectedRead = !path.startsWith("/api/meta");
  const generation = operation?.generation ?? privateSessionGeneration();
  if (protectedRead) operation?.assertCurrent();
  const controller = new AbortController();
  const stopCaller = signal ? bindPrivateAbort(controller, signal) : () => {};
  const stopOperation = operation ? bindPrivateAbort(controller, operation.signal) : () => {};
  const stop = () => {
    stopCaller();
    stopOperation();
  };
  try {
    if (controller.signal.aborted) throw new DOMException("The private session changed", "AbortError");
    const { response: resp, value } = await conditional.get<T>(path, sessionHeaders(), controller.signal);
    if (operation) operation.assertCurrent();
    else if (generation !== privateSessionGeneration() || (protectedRead && !privateReadsAllowed())) {
      throw new Error("Session changed during request");
    }
    if (!resp.ok && !(resp.status === 304 && value !== undefined)) {
      const body = await resp.json().catch(() => null);
      if (operation) operation.assertCurrent();
      else if (generation !== privateSessionGeneration()) throw new Error("Session changed during response");
      const code = body?.code ?? body?.error?.code;
      if (protectedRead && resp.status === 401 && (code === "board_session_required" || code === "operator_session_required") && typeof window !== "undefined") {
        window.dispatchEvent(new Event("cadence:session-required"));
      }
      throw new ApiError(
        errorMessage(body, `${resp.status} ${resp.statusText}`),
        resp.status,
        body ? { ...body, code } : undefined,
      );
    }
    if (operation) operation.assertCurrent();
    return value as T;
  } finally {
    stop();
  }
}

async function get<T>(path: string, signal?: AbortSignal, operation?: PrivateOperation): Promise<T> {
  const protectedRead = !path.startsWith("/api/meta");
  if (!protectedRead) return readConditional<T>(path, signal, null);
  if (!operation && !privateReadsAllowed()) throw new Error("Session verification required");
  return usingPrivateOperation(operation, (op) => readConditional<T>(path, signal, op), signal);
}

export interface WriteResp {
  issue: IssueDetail;
  card: IssueCard;
  warnings?: string[];
}

/** `GET /api/issues/<id>/kickoff` — enough for the drawer confirm. */
export interface KickoffOptions {
  defaults: {
    group?: string | null;
    provider?: string | null;
    model?: string | null;
    effort?: string | null;
  };
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
  operation?: PrivateOperation,
): Promise<WriteResp> {
  return usingPrivateOperation(operation, async (op) => {
    const resp = await privateFetch(path, {
      method,
      headers: {
        "Content-Type": "application/json",
        "X-Cadence-Board": "1",
        ...sessionHeaders(),
      },
      body: body === undefined ? undefined : JSON.stringify(body),
    }, op);
    const parsed = await resp.json().catch(() => null);
    op.assertCurrent();
    if (!resp.ok) {
      throw new ApiError(
        errorMessage(parsed, `${resp.status} ${resp.statusText}`),
        resp.status,
        parsed ?? undefined,
      );
    }
    return parsed as WriteResp;
  });
}

/** A board write that answers its own JSON (not an issue `WriteResp`). */
async function post<T>(path: string, body: object, operation?: PrivateOperation): Promise<T> {
  return usingPrivateOperation(operation, async (op) => {
    const resp = await privateFetch(path, {
      method: "POST",
      headers: {
        "Content-Type": "application/json",
        "X-Cadence-Board": "1",
        ...sessionHeaders(),
      },
      body: JSON.stringify(body),
    }, op);
    const parsed = await resp.json().catch(() => null);
    op.assertCurrent();
    if (!resp.ok) {
      throw new ApiError(
        errorMessage(parsed, `${resp.status} ${resp.statusText}`),
        resp.status,
        parsed ?? undefined,
      );
    }
    return parsed as T;
  });
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

export interface MasterPermissionRequest {
  id: string;
  command: string;
  cwd: string;
  reason: string;
  risk: string;
  status: string;
  decision_label?: string;
  expires_at: number;
  prefix: string[] | null;
}

export interface MasterPermissionRule {
  id: string;
  effect: "allow" | "deny";
  scope: "exact" | "prefix";
  argv: string[];
  tail: string[];
  cwd: string;
  by: string;
  at: number;
}

function textField(record: object, key: string): string | undefined {
  if (!Object.prototype.hasOwnProperty.call(record, key)) return undefined;
  const value: unknown = Reflect.get(record, key);
  return typeof value === "string" && value.length > 0 ? value : undefined;
}

/** Local `{error, code}` and hosted `{error:{code,message}}` upload refusals. */
function refusedUpload(body: unknown, status: number, statusText: string): { message: string; code?: string } {
  const fallback = `${status} ${statusText}`.trim();
  if (body === null || typeof body !== "object" || Array.isArray(body)) return { message: fallback };
  const topCode = textField(body, "code");
  const error: unknown = Reflect.get(body, "error");
  if (typeof error === "string" && error.length > 0) return { message: error, code: topCode };
  if (error !== null && typeof error === "object" && !Array.isArray(error)) {
    return {
      message: textField(error, "message") ?? fallback,
      code: topCode ?? textField(error, "code"),
    };
  }
  return { message: fallback, code: topCode };
}

/** Recorded owner handoff for the current personal assignment, never a Git-author guess. */
export interface AssignmentContext {
  kind: "assigned" | "initial_owner";
  actor: string;
  at: string;
  self_assigned: boolean;
}

/** One My work queue row (CAD-1703): what a card renders, not the full board card. */
export interface MyWorkIssue {
  id: string;
  project: string;
  title: string;
  status: string;
  priority: string;
  blocked: boolean;
  blocked_reason: string | null;
  container: boolean;
  checks: { done: number; total: number };
  counts: { comments: number };
  created: string;
  /** Last recorded activity (status/owner change, comment or creation). */
  updated: string | null;
}

/** One page of `GET /api/my-work`. `counts` cover every filter except the tab. */
export interface MyWorkPayload {
  owner: string;
  issues: MyWorkIssue[];
  total: number;
  next_cursor: string | null;
  counts: { active: number; done: number; dropped: number; all: number };
  status_counts: Record<string, number>;
  /** Projects the caller has any issue in — the project filter's options. */
  projects: string[];
}

/** `GET /api/my-work/issues/<ID>` — whether the issue is still the caller's. */
export interface MyWorkMembership {
  owner: string;
  id: string;
  assigned: boolean;
  assignment: AssignmentContext | null;
}


export const api = {
  health: () => get<Health>("/api/health"),
  /** `withOperator` asks the server to run the operator proof (CAD-432) — once per page load.
   *  `signal` bounds the wait (CAD-1193): an aborted read is a failed
   *  probe — unknown — never a sign-out or a resolved negative. */
  meta: (withOperator = false, signal?: AbortSignal) =>
    get<Meta>(withOperator ? "/api/meta?operator=1" : "/api/meta", signal),
  overview: () => get<Overview>("/api/overview"),
  myWork: (query: string, signal?: AbortSignal) => get<MyWorkPayload>(`/api/my-work?${query}`, signal),
  myWorkIssue: (id: string) => get<MyWorkMembership>(`/api/my-work/issues/${encodeURIComponent(id)}`),
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
  file: async (id: string, operation?: PrivateOperation) => {
    return usingPrivateOperation(operation, async (op) => {
      const resp = await privateFetch(`/api/issues/${id}/file`, { headers: sessionHeaders() }, op);
      if (!resp.ok) throw new ApiError(`${resp.status} ${resp.statusText}`, resp.status);
      const text = await resp.text();
      op.assertCurrent();
      return text;
    });
  },
  agents: () => get<AgentsPayload>("/api/agents"),
  agent: (alias: string) =>
    get<AgentDetail>(`/api/agents/${encodeURIComponent(alias)}`),
  monitorAck: (monitor: string, alert: number) =>
    post<{ ok: boolean; alert: unknown }>(`/api/monitors/${encodeURIComponent(monitor)}/alerts/${alert}/ack`, {}),
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
    /** CAD-140: the report/idea description, filed with the issue. */
    body?: string;
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

  /** `GET /api/issues/<id>/kickoff` — providers, models, groups, defaults. */
  kickoffOptions: (id: string) =>
    get<KickoffOptions>(`/api/issues/${encodeURIComponent(id)}/kickoff`),

  /** `POST /api/issues/<id>/kickoff` — operator join + dispatch. */
  kickoff: (
    id: string,
    body: {
      group: string;
      provider: string;
      model?: string;
      effort?: string;
      alias?: string;
      note?: string;
    },
  ) => write("POST", `/api/issues/${encodeURIComponent(id)}/kickoff`, body),

  attach: async (id: string, name: string, data: Blob, operation?: PrivateOperation): Promise<WriteResp> => {
    return usingPrivateOperation(operation, async (op) => {
      const resp = await privateFetch(
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
        op,
      );
      const parsed = await resp.json().catch(() => null);
      op.assertCurrent();
      if (!resp.ok) {
        throw new ApiError(
          errorMessage(parsed, `${resp.status} ${resp.statusText}`),
          resp.status,
          parsed ?? undefined,
        );
      }
      return parsed as WriteResp;
    });
  },

  artifactUrl: (id: string, name: string) =>
    `/api/issues/${id}/artifacts/${encodeURIComponent(name)}`,

  /** `GET /api/threads/<alias>` — one page after `after`. */
  thread: (
    alias: string,
    at: { after?: number; before?: number; tail?: boolean; limit?: number; conversation?: string } = {},
  ) => {
    const q = new URLSearchParams({ limit: String(at.limit ?? 200) });
    // CAD-1098: a selector only — the daemon validates it (404 creates nothing).
    if (at.conversation) q.set("conversation", at.conversation);
    if (at.tail) q.set("tail", "1");
    else if (at.before !== undefined) q.set("before", String(at.before));
    else q.set("after", String(at.after ?? 0));
    return get<ThreadPage>(`/api/threads/${encodeURIComponent(alias)}?${q.toString()}`);
  },
  /** `POST /api/threads/master/conversations {kind:"recruit"}` (CAD-1772):
   *  find-or-create the master's single Recruit conversation. */
  recruitConversation: () =>
    post<{ conversation: { id: string } }>("/api/threads/master/conversations", { kind: "recruit" }),
  /** `POST /api/threads/<alias>/messages` — `message` makes a retry idempotent.
   *  `refs` are the needs-me subjects the message cites (CAD-574 "Ask
   *  master"): they land on the entry's payload, never in the text.
   *  `app` is the shell chat's current installation/context (CAD-802):
   *  never authority — the daemon proves both against its store and
   *  stamps the verified binding on the entry, refusing anything else.
   *  `expectedRecipientUid` (CAD-1591) is an identity constraint, not
   *  authority: the contextual Threads reply pins the recorded sender it
   *  is answering, and the daemon refuses before any side effect when
   *  the alias now names a different uid. Ordinary sends omit it. */
  threadSend: (
    alias: string,
    text: string,
    message: string,
    refs?: ThreadRef[],
    app?: { install_id: string; context_id?: string },
    conversation?: string,
    attachments?: { id: string }[],
    operation?: PrivateOperation,
    mediaRefs?: { asset_id: string; revision: number; sha256: string }[],
    expectedRecipientUid?: string,
  ) =>
    post<Record<string, unknown>>(
      `/api/threads/${encodeURIComponent(alias)}/messages`,
      {
        text,
        message,
        ...(refs && refs.length > 0 ? { refs } : {}),
        ...(app ? { app } : {}),
        // CAD-1098: only a selector; the daemon resolves scope, subject and install.
        ...(conversation ? { conversation } : {}),
        // CAD-1168: `chf-…` ids of retained uploads — the daemon checks
        // grammar and existence, never the client's metadata.
        ...(attachments && attachments.length > 0 ? { attachments } : {}),
        ...(mediaRefs && mediaRefs.length > 0 ? { media_refs: mediaRefs } : {}),
        ...(expectedRecipientUid ? { expected_recipient_uid: expectedRecipientUid } : {}),
      },
      operation,
    ),
  /** `POST /api/chat/upload` — one retained chat attachment (CAD-1168).
   *  Operator-only multipart; returns the daemon-minted row. `signal`
   *  aborts only this upload's transport (a canceled row's late answer
   *  is ignored by its caller); it never deletes a retained blob. */
  chatFileUpload: (
    file: File,
    signal?: AbortSignal,
    destination?: ChatFileDestination,
  ): Promise<ChatFileRow> => {
    if (signal?.aborted) return Promise.reject(new DOMException("upload canceled", "AbortError"));
    return usingPrivateOperation(undefined, (operation) => new Promise((resolve, reject) => {
      if (operation.signal.aborted) {
        reject(new DOMException("upload canceled", "AbortError"));
        return;
      }
      const form = new FormData();
      form.append("name", file.name);
      if (destination !== undefined) {
        form.append("conversation", destination.conversation);
        if ("install_id" in destination) {
          form.append("install_id", destination.install_id);
          if (destination.context_id !== undefined) form.append("context_id", destination.context_id);
        }
      }
      form.append("file", file, file.name);
      const xhr = new XMLHttpRequest();
      let settled = false;
      const onAbort = () => xhr.abort();
      operation.signal.addEventListener("abort", onAbort, { once: true });
      const detach = () => {
        operation.signal.removeEventListener("abort", onAbort);
        xhr.onload = null;
        xhr.onerror = null;
        xhr.onabort = null;
      };
      const settle = (done: () => void) => {
        if (settled) return;
        settled = true;
        detach();
        done();
      };
      xhr.open("POST", "/api/chat/upload");
      xhr.setRequestHeader("X-Cadence-Board", "1");
      for (const [key, value] of Object.entries(sessionHeaders())) xhr.setRequestHeader(key, value);
      xhr.onload = () => {
        const status = xhr.status;
        const statusText = xhr.statusText;
        const responseText = xhr.responseText;
        const stillCurrent = !operation.signal.aborted;
        let body: unknown = null;
        try {
          body = JSON.parse(responseText);
        } catch {
          body = null;
        }
        settle(() => {
          if (!stillCurrent) {
            reject(new DOMException("upload canceled", "AbortError"));
            return;
          }
          if (status >= 200 && status < 300) {
            try {
              resolve(decodeChatFileRow(body));
            } catch (error) {
              reject(error);
            }
            return;
          }
          if (status === 401) notifyPrivateSessionRequired(status, body, operation);
          const refusal = refusedUpload(body, status, statusText);
          reject(new ApiError(refusal.message, status, refusal.code === undefined ? undefined : { code: refusal.code }));
        });
      };
      xhr.onerror = () => {
        settle(() => reject(new ApiError("upload failed — connection lost", 0)));
      };
      xhr.onabort = () => {
        settle(() => reject(new DOMException("upload canceled", "AbortError")));
      };
      xhr.send(form);
    }), signal);
  },
  /** CAD-1485: `POST /api/member-assistant/state` — the signed-in member's
   *  own assistant surface state (binding + bound conversation id). The
   *  public session cookie is the only credential; the board mints the
   *  assistant envelope server-side. Body is `{}`. */
  memberAssistState: () =>
    post<MemberAssistState>(`/api/member-assistant/state`, {}),
  /** `POST /api/member-assistant/history` — a page of the member's bound
   *  conversation. `after`/`before`/`tail`/`limit` match the daemon's own
   *  fields; a member never names a thread (the binding owns it). */
  memberAssistHistory: (page: {
    after?: number;
    before?: number;
    tail?: boolean;
    limit?: number;
  }) => post<MemberAssistHistory>(`/api/member-assistant/history`, { ...page }),
  /** `POST /api/member-assistant/send` — send `text` to the member's own
   *  assistant. `message` is the client-chosen idempotent id a retry
   *  reuses (one message, not two). */
  memberAssistSend: (text: string, message: string) =>
    post<MemberAssistSend>(`/api/member-assistant/send`, { text, message }),
  /** `GET /api/app-installations/<id>/chat-descriptor` (CAD-1110): the installed
   *  package's `app-chat.json`, pinned to the digest its install consented to. A
   *  404 means no descriptor (plain shared chat). */
  appChatDescriptor: (installId: string) =>
    get<{ descriptor: unknown; digest: unknown; app: unknown; file_upload?: AppFileUploadProjection }>(
      `/api/app-installations/${encodeURIComponent(installId)}/chat-descriptor`,
    ),
  /** Generic app-assistant/v1 discovery, durable operations and scoped permissions. */
  assistantActions: (installId: string, contextId: string) =>
    get<{ contract: "app-assistant/v1"; actions: unknown[]; descriptor_digest: string }>(
      `/api/app-installations/${encodeURIComponent(installId)}/assistant/actions?context_id=${encodeURIComponent(contextId)}`,
    ),
  assistantOperations: (installId: string, contextId: string) =>
    get<{ operations: unknown[] }>(
      `/api/app-installations/${encodeURIComponent(installId)}/assistant/operations?context_id=${encodeURIComponent(contextId)}`,
    ),
  assistantOperation: (installId: string, operationId: string) =>
    get<{ operation: unknown }>(
      `/api/app-installations/${encodeURIComponent(installId)}/assistant/operations/${encodeURIComponent(operationId)}`,
    ),
  assistantDecision: (installId: string, operationId: string, body: { decision: "allow_once" | "allow_always" | "deny"; expected_revision: number }) =>
    post<{ operation: unknown }>(
      `/api/app-installations/${encodeURIComponent(installId)}/assistant/operations/${encodeURIComponent(operationId)}/decision`,
      body,
    ),
  assistantPermissions: (installId: string, contextId: string) =>
    get<{ permissions: unknown[] }>(
      `/api/app-installations/${encodeURIComponent(installId)}/assistant/permissions?context_id=${encodeURIComponent(contextId)}`,
    ),
  assistantRevoke: (installId: string, permissionId: string, body: { expected_revision: number }) =>
    post<{ permission: unknown }>(
      `/api/app-installations/${encodeURIComponent(installId)}/assistant/permissions/${encodeURIComponent(permissionId)}/revoke`,
      body,
    ),
  assistantBlock: (installId: string, contextId: string, body: { action_id: string; resource_id: string }) =>
    post<{ permission: unknown }>(
      `/api/app-installations/${encodeURIComponent(installId)}/assistant/permissions/block?context_id=${encodeURIComponent(contextId)}`,
      body,
    ),
  /** `GET /api/app-installations/<id>/conversations` (CAD-1098; wire shape owned by conversationClient). */
  conversationList: (installId: string) =>
    get<Record<string, unknown>>(`/api/app-installations/${encodeURIComponent(installId)}/conversations`),
  /**
   * `POST` the same path: `context_id` is the proven context the
   * conversation is made under (the daemon proves install, context and
   * campaign); `subject` (`campaign:<id>`) is idempotent, none always makes a new one.
   */
  conversationCreate: (installId: string, contextId: string, subject?: string, operation?: PrivateOperation) =>
    post<Record<string, unknown>>(
      `/api/app-installations/${encodeURIComponent(installId)}/conversations`,
      {
        // The context is optional: General and New need none.
        ...(contextId !== "" ? { context_id: contextId } : {}),
        ...(subject ? { subject } : {}),
      },
      operation,
    ),
  /**
   * `POST /api/needs/<verb>` — the rail's snooze/dismiss (CAD-574);
   * operator-only, relayed to the daemon's `needs_dismiss`. `secs` is
   * the snooze window (86400 or 604800); dismiss takes none.
   */
  /** Operator decisions on a master permission request (CAD-615). */
  permissionAllowOnce: (id: string) =>
    post<Record<string, unknown>>(`/api/master/permissions/${encodeURIComponent(id)}/allow-once`, {}),
  permissionAlways: (id: string, scope: "exact" | "prefix", tail: string[]) =>
    post<Record<string, unknown>>(`/api/master/permissions/${encodeURIComponent(id)}/always`, { scope, tail }),
  permissionReject: (id: string, dontAskAgain: boolean) =>
    post<Record<string, unknown>>(`/api/master/permissions/${encodeURIComponent(id)}/reject`, {
      dont_ask_again: dontAskAgain,
    }),
  permissionRules: () =>
    get<{ requests: MasterPermissionRequest[]; rules: MasterPermissionRule[] }>("/api/master/permissions"),
  permissionRevoke: (id: string) =>
    post<Record<string, unknown>>(`/api/master/permissions/rules/${encodeURIComponent(id)}/revoke`, {}),
  needDecide: (verb: "snooze" | "dismiss", kind: string, id: string, secs?: number) =>
    post<Record<string, unknown>>(
      `/api/needs/${verb}`,
      secs !== undefined ? { kind, id, secs } : { kind, id },
    ),
  /**
   * `POST /api/agents/<alias>/resume|unfence` — the rail's agent actions
   * (CAD-574); operator-only on the board, the daemon's own rules still
   * apply (`agent_unfence` is operator-only by connection). `unfence`
   * sends the operator's reconcile `status` — the route refuses a
   * missing or unknown one.
   */
  agentAct: (alias: string, verb: "resume" | "unfence", status?: string) =>
    post<Record<string, unknown>>(
      `/api/agents/${encodeURIComponent(alias)}/${verb}`,
      verb === "unfence" ? { status } : {},
    ),
  /**
   * `POST /api/agents` - create one agent row through the daemon's
   * `agent_register` (the CLI's registration path). Operator-only. Cloud
   * agents send `location: "cloud"` and the daemon pins provider and
   * endpoint kind to `sandbox`.
   */
  registerAgent: (body: AgentRegisterBody) => post<Record<string, unknown>>("/api/agents", body),
  /** `GET /api/agent-roles` (CAD-1752) — the role catalogue, the install's
   *  runtime and the model each role would launch on. Any signed-in viewer. */
  agentRoles: () => get<AgentRolesPayload>("/api/agent-roles"),
  /** `POST /api/agents/<alias>/instructions` (CAD-1752) — replace the
   *  agent's standing instructions. Operator only. */
  setAgentInstructions: (alias: string, instructions: string) =>
    post<InstructionsResult>(`/api/agents/${encodeURIComponent(alias)}/instructions`, { instructions }),
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
  /**
   * `POST /api/reports` (CAD-140) — file a question, feedback, idea
   * or bug through the canonical intake path. `project` is the board
   * filed from: ideas land there, every other kind routes to the
   * `cadence` project server-side. Answers a `WriteResp`.
   */
  report: (req: {
    kind: "question" | "feedback" | "idea" | "bug";
    project: string;
    title: string;
    priority?: string;
    body?: string;
  }) => write("POST", "/api/reports", req),
  /**
   * `POST /api/ideas/<id>/decide` (CAD-140) — the operator's idea
   * decision: `approve` takes nothing, `reject` needs a reason, `park`
   * needs a `park_until` date. `expect_rev` binds the call to the
   * issue shown — a moved issue refuses with 409 `stale_view`.
   * Attribution is the board's proven connection, never a body field.
   */
  ideaDecide: (
    issue: string,
    action: "approve" | "reject" | "park",
    opts: { reason?: string; park_until?: string; expect_rev: string },
  ) =>
    post<Record<string, unknown>>(
      `/api/ideas/${encodeURIComponent(issue)}/decide`,
      {
        action,
        ...(opts?.reason ? { reason: opts.reason } : {}),
        ...(opts?.park_until ? { park_until: opts.park_until } : {}),
        ...(opts?.expect_rev ? { expect_rev: opts.expect_rev } : {}),
      },
    ),
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
  /**
   * `POST /api/apps/<project>/<name>/team` — relays the daemon's
   * `app_set_team`; operator-only. The body carries the app's default
   * team as `<input>=<agent>` pairs (CAD-577).
   */
  appSetTeam: (project: string, name: string, team: string[]) =>
    post<Record<string, unknown>>(
      `/api/apps/${encodeURIComponent(project)}/${encodeURIComponent(name)}/team`,
      { team },
    ),
  /**
   * `POST /api/apps/<project>/<name>/worker` — relays the daemon's
   * `app_add_worker`: join a new Devin worker for one team role, with a
   * unique role-prefixed alias, under the operator. Operator-only
   * (CAD-577).
   */
  appAddWorker: (project: string, name: string, role: string) =>
    post<{ alias?: string; role?: string }>(
      `/api/apps/${encodeURIComponent(project)}/${encodeURIComponent(name)}/worker`,
      { role },
    ),
  /** `GET /api/outbox` — the `local` platform's published items (operator-only). */
  outbox: () => get<OutboxList>("/api/outbox"),
  /** `GET /api/outbox?effect_id=` — one item, rendered post included. */
  outboxItem: (effectId: string) =>
    get<OutboxDetail>(`/api/outbox?effect_id=${encodeURIComponent(effectId)}`),
  /**
   * `GET /api/connection-providers` — every registered provider with
   * its reviewed descriptor (CAD-688). Operator-only: the Settings
   * Connections page fetches it only when the board proves the
   * operator, so a viewer without the read never sees a 403.
   */
  connectionProviders: () => get<ConnectionProvidersPayload>("/api/connection-providers"),
  /**
   * `POST /api/connection-providers/<provider>/connect` (CAD-1839) —
   * start a hosted provider's sign-in. The tenant worker owns the
   * credential and the OAuth state; this board sends an empty body and
   * receives only `{ok, hosted, url}`. Operator-only, same board
   * headers/credentials as every other connection write.
   */
  connectionProviderConnect: (provider: string) =>
    post<{ ok: boolean; hosted: boolean; url?: string }>(
      `/api/connection-providers/${encodeURIComponent(provider)}/connect`,
      {},
    ),
  /** `GET /api/connections` — every exact provider account (operator-only). */
  connections: () => get<ConnectionsPayload>("/api/connections"),
  /** `GET /api/connections/<id>` — one connection's metadata (operator-only). */
  connection: (id: string) =>
    get<ConnectionPayload>(`/api/connections/${encodeURIComponent(id)}`),
  /**
   * `POST /api/connections` — enroll a scoped token for a supported
   * provider (CAD-688). Operator-only. The token crosses this one
   * request into daemon custody; the caller clears it immediately and
   * never stores it.
   */
  connectionCreate: (body: {
    provider: string;
    account: string;
    shape: "token" | "smtp";
    token?: string;
    host?: string;
    port?: number;
    tls_mode?: string;
    username?: string;
    secret?: string;
    sender?: string;
    sender_name?: string;
    scopes: string[];
    accept_same_uid_risk?: boolean;
  }) => post<ConnectionPayload>("/api/connections", body),
  /**
   * `POST /api/connections/<id>/rotate` — replace the credential,
   * preserving the connection's identity (operator-only). Omitting
   * `scopes` keeps the current scopes.
   */
  connectionRotate: (
    id: string,
    body: {
      token?: string;
      secret?: string;
      host?: string;
      port?: number;
      tls_mode?: string;
      username?: string;
      sender?: string;
      sender_name?: string;
      scopes?: string[];
      accept_same_uid_risk?: boolean;
    },
  ) => post<ConnectionPayload>(`/api/connections/${encodeURIComponent(id)}/rotate`, body),
  /** `POST /api/connections/<id>/revoke` — revoke it (operator-only). */
  connectionRevoke: (id: string) =>
    post<{ connection_id: string; revoked: boolean }>(
      `/api/connections/${encodeURIComponent(id)}/revoke`,
      {},
    ),
  /**
   * `POST /api/connections/<id>/status` — the local configuration
   * check (operator-only). It inspects local configuration only: it
   * sends no provider effect and proves no upstream connectivity.
   */
  connectionCheck: (id: string) =>
    post<ConnectionPayload>(`/api/connections/${encodeURIComponent(id)}/status`, {}),
  /**
   * `POST /api/connections/<id>/test` — the approved SMTP no-send login
   * verification (CAD-1065/CAD-1085). Operator-only. Sends exactly the
   * two required expected keys, explicitly including null — no token,
   * secret, host, provider, actor, timeout or recipient. The returned
   * receipt is typed evidence only: a success proves an authenticated
   * login and clean close, never a delivery, sender entitlement or
   * execution authority, and a `stale` receipt is not
   * replacement-current evidence.
   */
  connectionTest: (
    id: string,
    expected: {
      expected_revision: number | null;
      expected_registration_digest: string | null;
    },
  ) =>
    post<ConnectionVerificationPayload>(
      `/api/connections/${encodeURIComponent(id)}/test`,
      expected,
    ),
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
  /** `POST /api/delivery/<id>/merge` — the operator's merge approval
   *  (CAD-431, CAD-140): the daemon records the approval object and
   *  lands pinned to the reviewed head. `sha` is REQUIRED — the head
   *  the card showed; a moved head refuses with 409 `head_moved` and
   *  nothing is recorded. */
  mergeDelivery: (issue: string, sha: string) =>
    post<Record<string, unknown>>(
      `/api/delivery/${encodeURIComponent(issue)}/merge`,
      sha ? { sha } : {},
    ),
  /** `POST /api/delivery/<id>/decline` — the operator declines with a
   *  reason (CAD-140); the worker loop stands down. */
  declineDelivery: (issue: string, reason: string) =>
    post<Record<string, unknown>>(`/api/delivery/${encodeURIComponent(issue)}/decline`, { reason }),
  /** `POST /api/approvals/approve` (CAD-1218) — records the operator's
   *  merge approval for the exact head the drawer showed. A moved head
   *  refuses with 409 `head_moved`; a head with an earlier record refuses. */
  approveHead: (repo: string, pr: number, head: string) =>
    post<{ approval_id: string }>("/api/approvals/approve", { repo, pr, head }),
  /** `GET /api/approvals/state` (CAD-1218) — whether this head has an
   *  approval in force, revoked, or none. */
  approvalState: (repo: string, pr: number, head: string) =>
    get<{ state: "in-force" | "revoked" | "missing"; approval_id?: string; board_revocable?: boolean }>(
      `/api/approvals/state?repo=${encodeURIComponent(repo)}&pr=${pr}&head=${encodeURIComponent(head)}`,
    ),
  /** `POST /api/approvals/<id>/revoke` (CAD-1218) — withdraw that approval. */
  revokeApproval: (id: string, reason: string) =>
    post<Record<string, unknown>>(`/api/approvals/${encodeURIComponent(id)}/revoke`, { reason }),
  /** `GET /api/master/summary?since=` — 501 on a daemon without it. */
  masterSummary: (since: number) =>
    get<Record<string, unknown>>(`/api/master/summary?since=${Math.floor(since)}`),
  /** `GET /api/master/state` — session chips + in-flight turn (CAD-551);
   *  501 on a daemon that predates it. */
  masterState: (conversation?: string) =>
    get<MasterState>(conversation === undefined ? "/api/master/state" : `/api/master/state?conversation=${encodeURIComponent(conversation)}`),
  /** `POST /api/master/identity` (CAD-1817) — the proven operator edits
   *  the workspace coordinator's name/personality/avatar. The response is
   *  the committed identity plus when it applies; the UI never assumes a
   *  save from the request alone. */
  masterIdentityUpdate: (body: MasterIdentityUpdate) =>
    post<MasterIdentityUpdateResult>("/api/master/identity", body),
  /** `POST /api/master/command` — an allowlisted session verb (CAD-551);
   *  operator-only, 400 `unknown_command` outside the board's set. */
  masterCommand: (command: string, arg?: string, wait?: number, message?: string) =>
    post<MasterCommandResult>("/api/master/command", {
      command,
      ...(arg ? { arg } : {}),
      ...(wait !== undefined ? { wait } : {}),
      ...(message !== undefined ? { message } : {}),
    }),
  /** `GET /api/master/models` — the dropdowns' list (CAD-575's route,
   *  operator-gated; the chip shows the refusal until it lands). */
  masterModels: () => get<MasterModels>("/api/master/models"),

  /** CAD-1548 — harnesses, profiles, providers and models. */
  agentSettings: () => get<AgentSettingsSnapshot>("/api/settings/agents"),
  saveAgentSettings: (body: { expected_revision: string; config: AgentSettingsConfig }) =>
    post<AgentSettingsSnapshot>("/api/settings/agents", body),
  refreshAgentCatalog: (provider: string) =>
    post<AgentCatalog>("/api/settings/agents/catalog", { provider }),
  syncAgentProfiles: (body: { expected_revision: string; profile: string | null }) =>
    post<AgentSyncResult>("/api/settings/agents/sync", body),
  previewAgentLaunch: (body: { expected_revision: string; profile: string; harness: string }) =>
    post<AgentPreviewResult>("/api/settings/agents/preview", body),
  /** CAD-561: the Settings Update card — current version, the last
   *  check, the running update's progress and the drain state. */
  updateStatus: () => get<UpdateStatus>("/api/update"),
  /** The draining banner, cheap enough for every page to poll. */
  updateBanner: () => get<UpdateBanner | null>("/api/update/banner"),
  /** Run the real check (gh) now — the operator's Check for updates. */
  updateCheck: () => post<UpdateCheck>("/api/update/check", {}),
  /** Start the update: the same pipeline as `cadence update`. */
  startUpdate: () => post<{ started: boolean }>("/api/update", {}),
  /** `GET /api/issues/<id>/lane` — the card's live agent, or `lane: null`. */
  lane: (id: string) => get<LanePayload>(`/api/issues/${encodeURIComponent(id)}/lane`),
  /** Ask for status. Optional extra is appended to the `status?` template server-side. */
  laneAsk: (id: string, text?: string) =>
    post<Record<string, unknown>>(
      `/api/issues/${encodeURIComponent(id)}/lane/ask`,
      text ? { text } : {},
    ),
  /** A real operator instruction on the lane thread. */
  laneInstruct: (id: string, text: string) =>
    post<Record<string, unknown>>(`/api/issues/${encodeURIComponent(id)}/lane/instruct`, { text }),
  laneInterrupt: (id: string) =>
    post<Record<string, unknown>>(`/api/issues/${encodeURIComponent(id)}/lane/interrupt`, {}),
  laneStop: (id: string) =>
    post<Record<string, unknown>>(`/api/issues/${encodeURIComponent(id)}/lane/stop`, {}),
  laneUnfence: (id: string, body: { status: string; note?: string; resume?: boolean }) =>
    post<Record<string, unknown>>(`/api/issues/${encodeURIComponent(id)}/lane/unfence`, body),
  laneReassign: (id: string, body: { provider: string; model?: string; effort?: string; note?: string }) =>
    post<Record<string, unknown>>(`/api/issues/${encodeURIComponent(id)}/lane/reassign`, body),
};
