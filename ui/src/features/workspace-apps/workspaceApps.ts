import { ApiError } from "../../lib/api";
import { sessionHeaders } from "../../lib/sessionKey";
import type { Agent } from "../../lib/types";

export type { Agent };
export interface Installation {
  install_id: string; name: string; title: string; version: string;
  summary: string; digest: string; storage_kind: "workspace" | "legacy";
  project_link: string | null; approved: boolean | null; executable: boolean;
  approval: { state: string }; guide: string; files: string[];
}
export interface AppContext {
  id: string; install_id: string; revision: number; digest: string; state: string;
  config: { schema: number; label: string; input_defaults: Record<string, string> };
}
export interface ActionMapping {
  capability: string; version: number; action: string; resource_kind: string;
  effect: string; semantics: string; input_contract: string; output_contract: string;
}
export interface Connection {
  id: string; provider: string; account: string; kind: string;
  descriptor: { action_mappings: ActionMapping[] } | null;
  status: { manifest_status: string; custody_available: boolean; adapter_registered: boolean };
}
export interface AppBinding {
  id: string; install_id: string; context_id: string | null; slot: string;
  revision: number; state: string; digest: string;
  config: { connection_id: string; provider: string; account: string; mapping: ActionMapping };
}
export interface ArtifactReceipt { id: string; step_id: string; digest: string; media_type: string; size: number }
export interface TextArtifact { id: string; digest: string; media_type: string; size: number; text: string }
export interface WorkspaceRun {
  id: string; install_id: string; context_id: string | null; state: string;
  snapshot_digest: string; approved_digest: string | null;
  snapshot: {
    workflow: { title: string; steps: { id: string; kind: string; assignee: string; dependencies: string[]; instruction: string }[]; publication_slot?: string | null }; inputs: Record<string, string>; owner_pm: string;
    context?: { id: string; revision: number; digest: string } | null;
    publication?: { slot: string; binding: { id: string; revision: number; digest: string } | null };
    capabilities?: Record<string, { id: string; revision: number; digest: string }>;
    quotes?: Record<string, CapabilityQuote["quote"]>;
    assignments: Record<string, { alias: string; role: string; provider: string }>;
  };
  steps: { step_id: string; task_id: string; state: string; message_id: string | null }[];
  artifacts: ArtifactReceipt[];
  reviews: { step_id: string; artifact_digest: string; reviewer: string; decision: string; rationale: string }[];
}
export interface SourcePost {
  id: string; caption: string; permalink: string; published_at: string | null;
  published_at_unix: number | null; media_kind: string; preview_url: string | null;
}
export interface SourceReceipt {
  id: string; run_id: string; slot: string; digest: string; binding_digest: string;
  result: {
    schema: 1; kind: "social.source.posts"; provider: "agenticos_external";
    source_tool: "scrapecreators.instagram.user.posts"; handle: string;
    profile_verified: boolean; empty_reason: string | null;
    posts: SourcePost[]; more_available: boolean;
    charge?: { currency: string; scale: number; amount: string } | null;
  };
}
export interface CapabilityQuote {
  slot: string;
  binding_digest: string;
  quote_digest: string;
  quote: { schema: 1; currency: "USD"; unit_price_micros: number; units: number; total_price_micros: number; price_revision: string };
}
export interface AppEffect {
  effect_id: string; request: string; state: string; needs_you: boolean; digest: string;
  authorization_kind: "app_artifact";
  authority: {
    install_id: string; run_id: string; context: { id: string } | null;
    artifact_id: string; artifact_digest: string;
    binding: { id: string; revision: number; digest: string };
    [key: string]: unknown;
  };
  record: {
    title?: string; platform?: string; account?: string;
    input?: { text?: string; title?: string };
    preview?: string;
    outcome?: { kind?: string; verified?: boolean; result?: unknown; error?: string };
    [key: string]: unknown;
  };
}
export interface CreateRun {
  install_id: string; workflow: string; inputs: Record<string, string>;
  request_id: string; owner_pm: string; context_id?: string;
  source_receipt_id?: string; selected_post_id?: string;
}
export interface WorkspaceOutbox {
  item: {
    effect_id: string; project: null; title: string; published_at: string;
    scope: string; authorization_kind: "app_artifact";
    post?: string | null; preview?: string;
    authority_digest: string; input_digest: string;
    provenance: Record<string, unknown>;
    [key: string]: unknown;
  };
}
type BindingCreate = { slot: string; connection_id: string; request_id: string; context_id?: string };
const part = encodeURIComponent;
const installation = (id: string) => `/api/app-installations/${part(id)}`;
const run = (id: string) => `/api/app-runs/${part(id)}`;
const effect = (id: string) => `/api/app-effects/${part(id)}`;

/** Reads are abortable and uncached so receipts never outlive an operator session. */
async function request<T>(path: string, signal?: AbortSignal, body?: object): Promise<T> {
  const response = await fetch(path, {
    method: body === undefined ? "GET" : "POST", signal,
    credentials: "same-origin", cache: "no-store",
    headers: body === undefined ? sessionHeaders() : {
      "Content-Type": "application/json", "X-Cadence-Board": "1", ...sessionHeaders(),
    },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }),
  });
  const value = await response.json().catch(() => null);
  if (!response.ok) throw new ApiError(value?.error ?? `${response.status} ${response.statusText}`, response.status);
  if (value === null) throw new ApiError("The server returned an invalid app receipt", 502);
  return value as T;
}

export const workspaceApps = {
  installations: (signal?: AbortSignal) => request<Installation[]>("/api/app-installations", signal),
  detail: (id: string, signal?: AbortSignal) => request<Installation>(installation(id), signal),
  contexts: async (id: string, signal?: AbortSignal) => (await request<{ contexts: AppContext[] }>(`${installation(id)}/contexts`, signal)).contexts,
  bindings: async (id: string, contextId?: string, signal?: AbortSignal) => (await request<{ bindings: AppBinding[] }>(contextId ? `${installation(id)}/contexts/${part(contextId)}/bindings` : `${installation(id)}/bindings`, signal)).bindings,
  connections: async (signal?: AbortSignal) => (await request<{ connections: Connection[] }>("/api/connections", signal)).connections,
  agents: async (signal?: AbortSignal) => (await request<{ agents: Agent[] }>("/api/agents", signal)).agents,
  runs: async (id: string, contextId?: string, signal?: AbortSignal) => {
    const query = new URLSearchParams({ install_id: id });
    if (contextId) query.set("context_id", contextId);
    return (await request<{ runs: WorkspaceRun[] }>(`/api/app-runs?${query}`, signal)).runs;
  },
  effects: async (id: string, contextId?: string, signal?: AbortSignal) => (await request<{ effects: AppEffect[] }>(contextId ? `${installation(id)}/contexts/${part(contextId)}/effects` : `${installation(id)}/effects`, signal)).effects,
  approveInstall: (id: string, digest: string) => request<unknown>(`${installation(id)}/approve`, undefined, { digest }),
  createContext: async (id: string, body: { label: string; input_defaults: Record<string, string>; request_id: string }) => (await request<{ context: AppContext }>(`${installation(id)}/contexts`, undefined, body)).context,
  createBinding: async (id: string, body: BindingCreate) => (await request<{ binding: AppBinding }>(`${installation(id)}/bindings`, undefined, body)).binding,
  updateBinding: async (id: string, bindingId: string, body: { expected_revision: number; connection_id: string }) => (await request<{ binding: AppBinding }>(`${installation(id)}/bindings/${part(bindingId)}/update`, undefined, body)).binding,
  createRun: (body: CreateRun) => request<WorkspaceRun>("/api/app-runs", undefined, body),
  approveRun: (id: string, digest: string) => request<WorkspaceRun>(`${run(id)}/approve`, undefined, { digest }),
  dispatchRun: (id: string) => request<WorkspaceRun>(`${run(id)}/dispatch`, undefined, {}),
  capabilityResults: async (id: string, signal?: AbortSignal) => (await request<{ results: SourceReceipt[] }>(`${run(id)}/capability-results`, signal)).results,
  capabilityResult: (id: string, signal?: AbortSignal) => request<SourceReceipt>(`/api/app-capability-results/${part(id)}`, signal),
  bindingQuote: (id: string, slot: string, contextId?: string, signal?: AbortSignal) => request<CapabilityQuote>(contextId ? `${installation(id)}/contexts/${part(contextId)}/bindings/${part(slot)}/quote` : `${installation(id)}/bindings/${part(slot)}/quote`, signal),
  artifact: (id: string, signal?: AbortSignal) => request<TextArtifact>(`/api/app-run-artifacts/${part(id)}`, signal),
  stageEffect: async (id: string, body: { artifact_id: string; slot: string; request_id: string; title: string }) => (await request<{ effect: AppEffect }>(`${run(id)}/effects`, undefined, body)).effect,
  decideEffect: async (id: string, body: { digest: string; decision: "accept" | "decline" }) => (await request<{ effect: AppEffect }>(`${effect(id)}/decide`, undefined, body)).effect,
  resolveEffect: async (id: string, body: { digest: string; resolution: "close" | "acknowledge" }) => (await request<{ effect: AppEffect }>(`${effect(id)}/resolve`, undefined, body)).effect,
  outbox: (id: string, signal?: AbortSignal) => request<WorkspaceOutbox>(`/api/outbox?effect_id=${part(id)}`, signal),
};
