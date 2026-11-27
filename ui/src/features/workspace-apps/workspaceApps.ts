import { ApiError } from "../../lib/api";
import { sessionHeaders, sessionKey } from "../../lib/sessionKey";
import { onPrivateReadsPaused, onPrivateSessionReset, privateFetch, privateReadsAllowed, privateSessionGeneration, withPrivateOperation } from "../../lib/privateSession";
import type { Agent } from "../../lib/types";
import {
  parseGrantReceipt,
  parseMemberAccess,
  parseRevokeReceipt,
  type MemberAccess,
  type MemberAccessGrant,
  type RevokeReceipt,
} from "../explorer/memberAccess";
import type { AppDisplayI18n, ListingDisplayLocale } from "./appLocale";
import type { ScreenNavigation } from "./screen/screenProtocol";
import { parseCatalogGrant, parseCatalogTools, type CatalogEffect } from "./platformCatalog";

export type { Agent };

/** Keep child-facing manifest aliases out of operator identity fields on RPC. */
function toolActionBody<T extends { alias: string }>(body: T): Omit<T, "alias"> & { tool_alias: string } {
  const { alias, ...fields } = body;
  if ("tool_alias" in fields) throw new Error("tool_alias is host-owned request data");
  return { ...fields, tool_alias: alias };
}
/** The catalogue release an install or an upgrade pins: id + exact
 * version + digest, always carried together (CAD-1471/CAD-1565). It
 * names a release; it is never a grant by itself. */
export interface CatalogueLocator { id: string; version: string; digest: string }

export interface Installation {
  install_id: string; name: string; title: string; version: string;
  summary: string; digest: string; catalog_generation: string; storage_kind: "workspace" | "legacy";
  /** The install's recorded upstream — `kind` is path/git/builtin (CAD-1129). */
  source?: { kind?: string; url?: string; sha?: string; dir?: string; path?: string; id?: string };
  /** Catalogue release locator recorded after an exact metadata match. Null on a path or Git install. */
  catalogue?: CatalogueLocator | null;
  /** Explicit private-upload provenance; absent or null means unknown. */
  source_kind?: "private" | null;
  project_link: string | null; approved: boolean | null; executable: boolean;
  /** CAD-1129 soft remove: epoch seconds of the mark, and when Restore stops working. */
  removed?: number | null; restore_after?: number | null;
  approval: { state: string }; guide: string; files: string[];
  /** Declared slot contract from the live bundle manifest (CAD-585): typed capability slots. */
  capabilities: Record<string, SlotDeclaration> | null;
  /** Untyped legacy slots from `needs.connections`, kept working as before. */
  connection_slots: string[];
  /** CAD-1543: the live bundle declares host SMTP custody (a screen lists `smtp.bind`). */
  smtp_custody?: boolean;
  /** Declared screen navigation (`app-screens/v1|v2` single-screen package). */
  navigation?: ScreenNavigation | null;
  /** CAD-2710: the screen's declared app views, as far as enum option labels go. Absent on an older daemon. */
  views?: DeclaredView[] | null;
  /** Display translations merged from `listing.i18n` and top-level `i18n`. */
  i18n?: AppDisplayI18n | null;
  listing?: Listing | null;
  /** Tenant lifecycle; absent on older/local responses, which mean active. */
  state?: "active" | "disabled" | "removed";
  disabled_at?: number | null;
  removed_at?: number | null;
  /** Hosted lifecycle deadline, in Unix milliseconds. */
  restore_until?: number | null;
  /**
   * CAD-1171: `execution: "host"` runs the workflow's capability step
   * in-process for the operator's own click — no worker, no owner PM and
   * no separate approve round. `"agent"` (the default, and what an older
   * daemon that omits the field means) keeps the worker path.
   */
  workflows?: { name: string; source_digest?: string; label?: string | null; capability_slots?: string[]; distinct?: string[]; execution?: "agent" | "host"; inputs: { name: string; ask?: string | null; optional?: boolean; kind?: string | null; example?: string | null; default?: string | null; context_default?: boolean }[] }[];
}
/** One declared app view: an enum param's `labels` are value -> locale -> text (CAD-2710). */
export interface DeclaredView {
  id: string;
  params?: Record<string, { labels?: Record<string, Record<string, string>> }>;
}
/** A write acknowledgement, distinct from a later installation read. */
export type CommittedInstallation = Installation & { committed: true };

/** One declared `needs.capabilities` slot: the result the app needs. Reviewed providers own the matching tools. */
export interface SlotDeclaration {
  schema: number; capability: string; version: number;
  action: string; resource_kind: string; effect: string;
}
export interface AppContext {
  id: string; install_id: string; revision: number; digest: string; state: string;
  config: { schema: number; label: string; input_defaults: Record<string, string> };
}
export interface ActionMapping {
  capability: string; version: number; action: string; resource_kind: string;
  effect: string; semantics: string; scopes: string[];
  input_contract: string; output_contract: string;
}
export interface Connection {
  id: string; provider: string; account: string; kind: string;
  scopes: string[];
  descriptor: { action_mappings: ActionMapping[] } | null;
  status: { manifest_status: string; custody_available: boolean; adapter_registered: boolean };
}
/** One field a bound connection's re-derived receipt changed (CAD-1119). */
export interface BindingChange { field: string; from: unknown; to: unknown }
/**
 * The daemon's view of a configured binding against its connection now:
 * `current`; `migrates` silently on next use (provider bookkeeping only);
 * `needs_confirm` when the slot contract changed; `unavailable` with why.
 */
export interface BindingDrift {
  state: "current" | "migrates" | "needs_confirm" | "unavailable";
  changes?: BindingChange[];
  reason?: string;
}
export interface AppBinding {
  id: string; install_id: string; context_id: string | null; slot: string;
  revision: number; state: string; digest: string;
  config: { bundle_digest: string; connection_id?: string; provider: string; account?: string; mapping?: ActionMapping;
    connection_kind?: string; tool_slug?: string; declaration?: SlotDeclaration;
    publish?: { destination_id: string; destination_label: string; toolkit: string } };
  drift?: BindingDrift;
}

export interface PlatformToolRequest {
  id: string;
  install_id: string;
  slot: string;
  bundle_digest: string;
  tool_slug: string;
  state: "pending" | "approved" | "denied" | "stale";
  management_url: string;
}

function platformToolRequest(value: unknown, installId: string): PlatformToolRequest {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new ApiError("Invalid tool access request", 502);
  const row = value as Record<string, unknown>;
  if (typeof row.id !== "string" || !/^atr_[0-9a-f]{32}$/.test(row.id) ||
    row.install_id !== installId || typeof row.slot !== "string" || !row.slot ||
    typeof row.bundle_digest !== "string" || typeof row.tool_slug !== "string" || !row.tool_slug ||
    !["pending", "approved", "denied", "stale"].includes(String(row.state)) ||
    typeof row.management_url !== "string") throw new ApiError("Invalid tool access request", 502);
  return row as unknown as PlatformToolRequest;
}
/** Declared access changed by a checked bundle (CAD-1565): which
 * connection slots appear/disappear and which capability slot
 * declarations differ between the installed and checked manifests. */
export interface AccessDiff {
  connections: { added: string[]; removed: string[] };
  capabilities: Record<string, { before: SlotDeclaration | null; after: SlotDeclaration | null }>;
}

export interface CompatibilityCheck {
  kind: "core" | "contract";
  required: string | number[];
  host?: string;
  contract?: string;
  supported?: number[];
  stage?: string | null;
  ok: boolean;
}
export interface CompatibilityReceipt {
  schema: number;
  ok: boolean | null;
  compatible?: boolean | null;
  status?: string;
  declared: boolean;
  core?: string | null;
  contracts?: Record<string, number[]>;
  unmet?: string | null;
  host?: { core: string; contracts: Record<string, number[]> } | null;
  checks: CompatibilityCheck[];
}
export interface UpgradeProposal {
  install_id: string; name: string; version: string; digest: string;
  expected_digest: string; expected_generation: string;
  structural_diff: { added: string[]; changed: string[]; removed: string[] };
  /** The provenance the daemon authorized for this proposal — required
   *  to equal the locator the caller pinned when one was sent. */
  catalogue?: CatalogueLocator | null;
  access_diff?: AccessDiff;
  compatibility?: CompatibilityReceipt; notes?: string[];
  secret_warnings: unknown[];
}
export interface ArtifactReceipt { id: string; step_id: string; digest: string; media_type: string; size: number }
export interface TextArtifact { id: string; digest: string; media_type: string; size: number; text: string }
export interface WorkspaceRun {
  id: string; install_id: string; context_id: string | null; state: string;
  snapshot_digest: string; approved_digest: string | null;
  snapshot: {
    workflow: { title: string; source_digest?: string; execution?: "agent" | "host"; steps: { id: string; kind: string; assignee: string; dependencies: string[]; instruction: string }[]; publication_slot?: string | null }; inputs: Record<string, string>; owner_pm: string;
    context?: { id: string; revision: number; digest: string } | null;
    publication?: { slot: string; binding: { id: string; revision: number; digest: string } | null };
    capabilities?: Record<string, { id: string; revision: number; digest: string }>;
    quotes?: Record<string, CapabilityQuote["quote"]>;
    source?: { receipt_id: string; post: { id: string; caption: string } } | null;
    assignments: Record<string, { alias: string; role: string; provider: string }>;
    input_origins?: Record<string, "app_default" | "context_default" | "run_override">;
  };
  steps: { step_id: string; task_id: string; state: string; message_id: string | null }[];
  failure?: { kind: "refused" | "uncertain"; reason: string; step_id: string };
  /** Run epochs in seconds (CAD-1123). */
  created?: number; updated?: number;
  /** The recorded execution approval, when there is one (CAD-1123 R4). */
  approval?: { by: string; at: number };
  artifacts: ArtifactReceipt[];
  reviews: { step_id: string; artifact_digest: string; reviewer: string; decision: string; rationale: string; asset_receipt_id?: string; asset_digest?: string }[];
}
export interface ImageReceipt {
  id: string; run_id: string; slot: string; digest: string; binding_digest: string;
  asset: { media_type: string; digest: string; size: number } | null;
  result: { schema: number; kind: string; provider: string; model: string; aspect_ratio: string; job_id: string; charge: { currency: string; scale: number; amount: string }; price_version: string; quoted_micros: number; repeated: boolean; asset_sha256: string; asset_media_type: string; source_receipt_id: string | null; source_post_id: string | null };
}
export interface ImageAsset {
  receipt_id: string; media_type: string; digest: string; size: number; base64: string;
}
export interface SourcePost {
  id: string; caption: string; permalink: string; published_at: string | null;
  published_at_unix: number | null; media_kind: string; preview_url: string | null;
}
export interface SourceReceipt {
  id: string; run_id: string; slot: string; digest: string; binding_digest: string;
  result: {
    schema: 1; kind: "social.source.posts"; provider: "agenticos_external";
    source_tool: "read_instagram_posts" | "scrapecreators.instagram.user.posts"; handle: string;
    profile_verified: boolean; empty_reason: string | null;
    posts: SourcePost[]; more_available: boolean;
    charge?: { currency: string; scale: number; amount: string } | null;
  };
}
export interface CapabilityQuote {
  slot: string;
  model?: string;
  binding_digest: string;
  quote_digest: string;
  quote: { schema: 1; currency: "USD"; unit_price_micros: number; units: number; total_price_micros: number; price_revision: string };
}
/** CAD-1177: a retained standalone tool invocation receipt. The `result`
 *  payload shape is tool-owned (e.g. `social.source.posts` for the
 *  Instagram read); the envelope fields are host-stamped. */
export interface ToolReceipt {
  id: string; request_id: string; install_id: string; alias: string; slot: string;
  binding_digest: string; input_digest: string; digest: string;
  input: Record<string, unknown>;
  result: { kind?: string; handle?: string; posts?: SourcePost[]; profile_verified?: boolean;
    empty_reason?: string | null; charge?: { currency: string; scale: number; amount: string } | null;
    [key: string]: unknown };
  asset?: { media_type: string; digest: string; size: number } | null;
  /** Host-stamped creation epoch seconds; the frame reads it as fetched_at. */
  created_at?: number;
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
  request_id: string;
  /** CAD-1171: required for an agent run; omitted for a host workflow, which has no owner PM. */
  owner_pm?: string;
  context_id?: string;
  source_receipt_id?: string; selected_post_id?: string;
}
/** CAD-1123 HP2: one operator gesture. Owner PM and worker roles come from the install team. */
export interface StartRun {
  install_id: string; workflow: string; inputs: Record<string, string>;
  request_id: string; expected_quotes: Record<string, CapabilityQuote["quote"]>; context_id?: string;
  source_receipt_id?: string; selected_post_id?: string;
}
/** The installation's default team (owner PM and one worker alias per workflow role). */
export interface InstallTeam { owner_pm: string; roles: Record<string, string>; revision: number }
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
/** CAD-1365: the original's byte bound and the parent fetch lifetime (the metadata timeout is unchanged). */
export const ORIGINAL_MEDIA_BYTES_MAX = 10 * 1024 * 1024;
const ORIGINAL_MEDIA_TIMEOUT_MS = 60_000;
const ORIGINAL_MIME = ["image/png", "image/jpeg", "image/webp"];

/** Read a response body, refusing (and cancelling) past the byte bound or a Content-Length mismatch. */
async function readBounded(response: Response): Promise<ArrayBuffer> {
  const invalid = () => new ApiError("The server returned an invalid original image", 502);
  const declared = response.headers.get("Content-Length");
  if (declared !== null && (!/^\d+$/.test(declared) || Number(declared) > ORIGINAL_MEDIA_BYTES_MAX)) throw invalid();
  const reader = response.body?.getReader();
  if (!reader) throw invalid();
  const chunks: Uint8Array[] = [];
  let total = 0;
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      total += value.byteLength;
      if (total > ORIGINAL_MEDIA_BYTES_MAX) { await reader.cancel().catch(() => {}); throw invalid(); }
      chunks.push(value);
    }
  } finally {
    reader.releaseLock();
  }
  if (declared !== null && Number(declared) !== total) throw invalid();
  const out = new Uint8Array(total);
  let at = 0;
  for (const chunk of chunks) { out.set(chunk, at); at += chunk.byteLength; }
  return out.buffer as ArrayBuffer;
}


function appResponseError(response: Response, value: unknown): ApiError {
  const body = value as { code?: unknown; error?: string | { code?: unknown; message?: unknown } } | null;
  const nested = typeof body?.error === "object" ? body.error : null;
  const code = typeof body?.code === "string" ? body.code : typeof nested?.code === "string" ? nested.code : undefined;
  const message = typeof body?.error === "string" ? body.error : typeof nested?.message === "string" ? nested.message : `${response.status} ${response.statusText}`;
  return new ApiError(message, response.status, code ? { code } : undefined);
}

async function request<T>(path: string, signal?: AbortSignal, body?: object, postEmpty = false): Promise<T> {
  return withPrivateOperation(async (operation) => {
    const read = body === undefined && !postEmpty;
    const response = await privateFetch(path, {
      method: read ? "GET" : "POST",
      credentials: "same-origin", cache: "no-store",
      headers: read ? sessionHeaders() : {
        "Content-Type": "application/json", "X-Cadence-Board": "1", ...sessionHeaders(),
      },
      ...(body === undefined ? {} : { body: JSON.stringify(body) }),
    }, operation);
    operation.assertCurrent();
    const value = await response.json().catch(() => null);
    operation.assertCurrent();
    if (!response.ok) throw appResponseError(response, value);
    if (value === null) throw new ApiError("The server returned an invalid app receipt", 502);
    return value as T;
  }, signal);
}

export type AppAgentSelection = {
  digest: string;
  revision: number;
  stale: boolean;
  agent: { id: string; name: string } | null;
};

function appAgentSelection(value: unknown, installId: string, stale: boolean): AppAgentSelection {
  const invalid = new ApiError("The server returned an invalid app agent selection", 502);
  if (typeof value !== "object" || value === null || !("install_id" in value) || value.install_id !== installId ||
    !("digest" in value) || typeof value.digest !== "string" || !/^sha256:[0-9a-f]{64}$/.test(value.digest) ||
    !("revision" in value) || typeof value.revision !== "number" || !Number.isSafeInteger(value.revision) || value.revision < 0 ||
    !("agent" in value)) throw invalid;
  const { digest, revision, agent } = value;
  if (agent === null) return { digest, revision, stale, agent };
  if (stale || revision === 0 || typeof agent !== "object" || !("profile_id" in agent) ||
    typeof agent.profile_id !== "string" || !agent.profile_id || !("profile_revision" in agent) ||
    typeof agent.profile_revision !== "number" || !Number.isSafeInteger(agent.profile_revision) || agent.profile_revision < 1 ||
    !("profile" in agent) || typeof agent.profile !== "object" || agent.profile === null ||
    !("id" in agent.profile) || agent.profile.id !== agent.profile_id ||
    !("name" in agent.profile) || typeof agent.profile.name !== "string" || !agent.profile.name.trim() ||
    !("revision" in agent.profile) || agent.profile.revision !== agent.profile_revision ||
    !("status" in agent.profile) || agent.profile.status !== "active") throw invalid;
  return { digest, revision, stale, agent: { id: agent.profile_id, name: agent.profile.name } };
}

/** One of the company's connected Instagram accounts or Facebook Pages (CAD-1290, CAD-1939). */
export interface PublishDestination { destination_id: string; label: string; toolkit: string }
export const workspaceApps = {
  appAgent: async (id: string, signal?: AbortSignal) => {
    const value = await request<unknown>(`${installation(id)}/agent`, signal);
    if (typeof value !== "object" || value === null || !("stale" in value) || typeof value.stale !== "boolean")
      throw new ApiError("The server returned an invalid app agent selection", 502);
    return appAgentSelection(value, id, value.stale);
  },
  setAppAgent: async (id: string, previous: AppAgentSelection, profileId: string | null, signal?: AbortSignal) => {
    const selected = appAgentSelection(await request<unknown>(`${installation(id)}/agent`, signal, {
      expected_digest: previous.digest, expected_revision: previous.revision, profile_id: profileId,
    }), id, false);
    if (selected.digest !== previous.digest || selected.revision !== previous.revision + 1 ||
      (selected.agent?.id ?? null) !== profileId)
      throw new ApiError("The app agent change could not be verified", 502);
    return selected;
  },
  activeAgentProfiles: async (signal?: AbortSignal) => {
    const value = await request<unknown>("/api/agent-profiles", signal);
    const invalid = new ApiError("The server returned an invalid agent roster", 502);
    if (typeof value !== "object" || value === null || !("profiles" in value) || !Array.isArray(value.profiles))
      throw invalid;
    const profiles = new Map<string, string>();
    for (const profile of value.profiles as unknown[]) {
      if (typeof profile !== "object" || profile === null || !("status" in profile) ||
        (profile.status !== "active" && profile.status !== "archived") ||
        !("id" in profile) || typeof profile.id !== "string" || !profile.id ||
        !("name" in profile) || typeof profile.name !== "string" || !profile.name.trim()) throw invalid;
      if (profile.status !== "active") continue;
      if (profiles.has(profile.id)) throw invalid;
      profiles.set(profile.id, profile.name);
    }
    return profiles;
  },
  /** CAD-1290: the host-composed AgenticOS connect link; `hosted:false` on a local board.
   *  `inlineConsent` is forwarded only when the board's own consent read proves the central
   *  capability exists, so an older daemon never receives a field it does not accept. */
  connectLink: (id: string, returnTo: string, signal?: AbortSignal, toolkit?: string, inlineConsent?: boolean) =>
    request<{ hosted: boolean; url?: string; unavailable?: boolean }>(`${installation(id)}/publishing/connect-link`, signal, { return_to: returnTo, ...(toolkit ? { toolkit } : {}), ...(inlineConsent ? { inline_consent: true } : {}) }),
  /** The owner-scoped publishing consent read. Its shape is the host's contract with the board:
   *  `{available, consents:[{destination_id, toolkit, state:"allowed"|"none"}]}`; an older board
   *  answers 404 and the caller treats that as unavailable, never as "no consent". */
  consentStatus: (id: string, signal?: AbortSignal) =>
    request<unknown>(`${installation(id)}/publishing/consent`, signal),
  /** Ask the board to record the owner's consent for the account this install publishes to. */
  consentAllow: (id: string, body: { destination_id: string }) =>
    request<unknown>(`${installation(id)}/publishing/consent`, undefined, body),
  destinations: (id: string, signal?: AbortSignal) =>
    request<{ unavailable: boolean; destinations: PublishDestination[] }>(`${installation(id)}/publishing/destinations`, signal),
  useDestination: (id: string, body: { destination_id: string; toolkit?: string; timezone?: string; request_id?: string; expected_revision?: number; context_id?: string }) =>
    request<{ binding: AppBinding }>(`${installation(id)}/publishing/use`, undefined, body),
  /** CAD-1939: retire one binding (one destination) at the revision the caller read. */
  revokeBinding: (id: string, bindingId: string, expectedRevision: number) =>
    request<{ binding: AppBinding }>(`${installation(id)}/bindings/${part(bindingId)}/revoke`, undefined, { expected_revision: expectedRevision }),
  platformToolRequests: async (id: string, digest: string, signal?: AbortSignal): Promise<PlatformToolRequest[] | null> => {
    let value: unknown;
    try {
      value = await request<unknown>(`${installation(id)}/platform-tool-requests`, signal);
    } catch (error) {
      // Older boards have no route; an installation-specific 404 remains an error.
      if (error instanceof ApiError && (error.status === 501 ||
        (error.status === 404 && /^(404(?: Not Found)?|not found)$/i.test(error.message)))) return null;
      throw error;
    }
    if (!value || typeof value !== "object" || !("requests" in value) || !Array.isArray(value.requests))
      throw new ApiError("Invalid tool access requests", 502);
    const rows = value.requests.map((row: unknown) => platformToolRequest(row, id));
    if (rows.some((row: PlatformToolRequest) => row.state !== "pending"))
      throw new ApiError("Invalid tool access requests", 502);
    if (rows.some((row: PlatformToolRequest) => row.bundle_digest !== digest))
      throw new ApiError("App changed while loading tool access requests", 409, { code: "installation_stale" });
    return rows;
  },
  decidePlatformToolRequest: async (target: PlatformToolRequest, decision: "approve" | "deny") => {
    const value = await request<unknown>(`${installation(target.install_id)}/platform-tool-requests/${part(target.id)}/decision`, undefined, { decision });
    if (!value || typeof value !== "object" || !("request" in value)) throw new ApiError("Invalid tool access decision", 502);
    const row = platformToolRequest(value.request, target.install_id);
    if (row.id !== target.id || row.slot !== target.slot ||
        row.bundle_digest !== target.bundle_digest || row.tool_slug !== target.tool_slug ||
        row.state !== (decision === "approve" ? "approved" : "denied"))
      throw new ApiError("Invalid tool access decision", 502);
    return row;
  },
  platformTools: async (id: string, slot: string, effect: CatalogEffect, signal?: AbortSignal) => {
    const query = new URLSearchParams({ slot });
    return parseCatalogTools(await request<unknown>(`${installation(id)}/platform-tools?${query}`, signal), effect);
  },
  bindPlatformTool: async (id: string, body: { slot: string; tool_slug: string; expected_digest: string; request_id: string; connection_id?: string }, effect: CatalogEffect) =>
    parseCatalogGrant(await request<unknown>(`${installation(id)}/platform-tools/bind`, undefined, body), {
      installId: id, digest: body.expected_digest, slot: body.slot, slug: body.tool_slug, effect,
      ...(body.connection_id === undefined ? {} : { connectionId: body.connection_id }),
    }),
  revokePlatformTool: async (id: string, bindingId: string, revision: number, expected: { digest: string; slot: string; slug: string; effect: CatalogEffect; connectionId?: string }) => {
    const receipt = parseCatalogGrant(await request<unknown>(`${installation(id)}/bindings/${part(bindingId)}/revoke`, undefined, { expected_revision: revision }), { installId: id, ...expected });
    if (receipt.id !== bindingId || receipt.state !== "revoked" || receipt.revision !== revision + 1)
      throw new ApiError("The tool revocation could not be verified.", 502);
    return receipt;
  },
  installations: (signal?: AbortSignal, shared = false) => shared
    ? coalesceInFlightRead(installationListReads, "operator", (sharedSignal) => request<Installation[]>("/api/app-installations", sharedSignal), signal)
    : request<Installation[]>("/api/app-installations", signal),
  detail: (id: string, signal?: AbortSignal) => request<Installation>(installation(id), signal),
  upgradeCheck: (id: string, body: UpgradeCheckBody, signal?: AbortSignal) =>
    request<UpgradeProposal>(`${installation(id)}/upgrade/check`, signal, body),
  upgrade: (id: string, body: UpgradeApplyBody, signal?: AbortSignal) =>
    request<CommittedInstallation>(`${installation(id)}/upgrade`, signal, body),
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
  updateContext: async (id: string, contextId: string, body: { expected_revision: number; label: string; input_defaults: Record<string, string> }) => (await request<{ context: AppContext }>(`${installation(id)}/contexts/${part(contextId)}/update`, undefined, body)).context,
  createBinding: async (id: string, body: BindingCreate) => (await request<{ binding: AppBinding }>(`${installation(id)}/bindings`, undefined, body)).binding,
  updateBinding: async (id: string, bindingId: string, body: { expected_revision: number; connection_id: string }) => (await request<{ binding: AppBinding }>(`${installation(id)}/bindings/${part(bindingId)}/update`, undefined, body)).binding,
  createRun: (body: CreateRun) => request<WorkspaceRun>("/api/app-runs", undefined, body),
  getRun: (id: string, signal?: AbortSignal) => request<WorkspaceRun>(run(id), signal),
  startRun: (body: StartRun) => request<WorkspaceRun>("/api/app-runs/start", undefined, body),
  team: async (id: string, signal?: AbortSignal) => (await request<{ team: InstallTeam | null }>(`${installation(id)}/team`, signal)).team,
  setTeam: async (id: string, body: { owner_pm: string; roles: Record<string, string>; expected_revision: number }) => (await request<{ team: InstallTeam }>(`${installation(id)}/team`, undefined, body)).team,
  approveRun: (id: string, digest: string) => request<WorkspaceRun>(`${run(id)}/approve`, undefined, { digest }),
  dispatchRun: (id: string) => request<WorkspaceRun>(`${run(id)}/dispatch`, undefined, {}),
  capabilityResults: async (id: string, signal?: AbortSignal) => (await request<{ results: SourceReceipt[] }>(`${run(id)}/capability-results`, signal)).results,
  imageResults: async (id: string, signal?: AbortSignal) => (await request<{ results: ImageReceipt[] }>(`${run(id)}/capability-results`, signal)).results,
  capabilityResult: (id: string, signal?: AbortSignal) => request<SourceReceipt>(`/api/app-capability-results/${part(id)}`, signal),
  capabilityAsset: (id: string, signal?: AbortSignal) => request<ImageAsset>(`/api/app-capability-results/${part(id)}/asset`, signal),
  bindingQuote: (id: string, slot: string, contextId?: string, signal?: AbortSignal, model?: string) => {
    const base = contextId ? `${installation(id)}/contexts/${part(contextId)}/bindings/${part(slot)}/quote` : `${installation(id)}/bindings/${part(slot)}/quote`;
    return request<CapabilityQuote>(model ? `${base}?model=${encodeURIComponent(model)}` : base, signal);
  },
  /** CAD-1177: invoke one declared screen tool through the host broker,
   *  bound to the live mount's action context. The frame supplies only
   *  alias/input/request_id; install/digest/slot/binding are server-side. */
  invokeTool: (body: { action_token: string; alias: string; input: Record<string, unknown>; request_id: string; generation_scope?: {operation:"caption"|"image";draft_id:string;revision:number} }) =>
    request<{ receipt?: ToolReceipt; generation_intent?: {request_id:string;operation:string;draft_id?:string;revision?:number;input_digest:string;state:"pending"|"uncertain";updated_at:number}; replayed: boolean }>("/api/app-screen-tools/invoke", undefined, toolActionBody(body)),
  /** CAD-1177: retire a mount's action handle on teardown/remount. The
   *  host sends the mount's action_token; the daemon revokes only a
   *  context the proven session owns. Best-effort — never blocks unmount. */
  revokeTool: (body: { action_token: string }) =>
    request<{ revoked: boolean }>("/api/app-screen-tools/revoke", undefined, body),
  /** The retained receipts for one installation (operator read). */
  toolResults: async (id: string, contextId?: string, signal?: AbortSignal) => (await request<{ results: ToolReceipt[] }>(`/api/app-screen-tools/results?install_id=${part(id)}${contextId?`&context_id=${part(contextId)}`:""}`, signal)).results,
  /** CAD-1365: `media.read` relay for a live `screen-media.v1` mount —
   *  the workspace catalog's bounded metadata page. The body is exactly
   *  `{action_token, input}`; the route refuses any scope/credential
   *  field and adds the proven session itself. */
  mediaRead: (body: { action_token: string; input: Record<string, unknown> }) =>
    request<Record<string, unknown>>(
      "/api/app-media/read", undefined, body),
  mediaChatAttach: (body: { action_token: string; asset_id: string; expected_revision: number; conversation_id: string | null }) =>
    request<{ asset_id: string; revision: number; host_command: {
      operation: "media.chat.attach"; conversation_id: string | null;
      args: { install_id: string; context_id: string; asset_id: string; revision: number; sha256: string; media_type: string; title: string; preview: string };
    } }>("/api/app-media/chat-attach", undefined, body),
  /** CAD-1365: `media.publish` relay — the frozen curation intent
   *  `{action_token, asset_id, expected_revision, request_id}` only. */
  mediaPublish: (body: { action_token: string; asset_id: string; expected_revision: number; request_id: string }) =>
    request<Record<string, unknown>>("/api/app-media/publish", undefined, body),
  /** CAD-1962: `media.import` relay — exactly `{action_token,
   *  receipt_id, request_id}`. The daemon copies the receipt's verified
   *  bytes into workspace custody and answers with the new draft asset. */
  mediaImport: (body: { action_token: string; receipt_id: string; request_id: string }) =>
    request<Record<string, unknown>>("/api/app-media/import", undefined, body),
  socialDraftAction: (operation: "create"|"list"|"show"|"update"|"discard"|"sources/show"|"sources/page"|"sources/get"|"sources/save"|"brands/list"|"brands/show"|"models/list"|"radar/show"|"radar/save"|"radar/scan"|"effect-stage"|"confirm"|"schedule"|"reschedule"|"cancel", body: Record<string, unknown> & { alias: string }) =>
    request<Record<string, unknown>>(`/api/app-social-drafts/${operation}`, undefined, toolActionBody(body)),
  /** SOC-32: the host-owned image upload door. `file` goes as multipart; the
   *  daemon re-proves the live mount/context/slot, normalises the image and
   *  returns its durable asset id plus a small display preview. */
  uploadSocialImage: async (body:{action_token:string;alias:string;file:File}):Promise<{asset_id:string;mime:string;width:number;height:number;size_bytes:number;digest:string}> => {
    const generation = privateSessionGeneration();
    const controller = new AbortController();
    const unsubscribe = onPrivateReadsPaused(() => controller.abort());
    try {
      return await withPrivateOperation(async (op) => {
        const form = new FormData();
        form.append("action_token", body.action_token);
        form.append("alias", body.alias);
        form.append("name", body.file.name);
        form.append("file", body.file, body.file.name);
        const response = await privateFetch("/api/app-social-drafts/image", { method: "POST", credentials: "same-origin", cache: "no-store", signal: controller.signal,
          headers: { "X-Cadence-Board": "1", ...sessionHeaders() }, body: form }, op);
        const value = await response.json().catch(() => null);
        op.assertCurrent();
        if (!response.ok) throw appResponseError(response, value);
        if (controller.signal.aborted || !privateReadsAllowed() || generation !== privateSessionGeneration()) throw new DOMException("The private session changed", "AbortError");
        const row = value as Record<string, unknown> | null;
        if (!row || typeof row.asset_id !== "string" || !["image/jpeg", "image/png"].includes(String(row.mime)) ||
            !Number.isSafeInteger(row.width) || !Number.isSafeInteger(row.height) || !Number.isSafeInteger(row.size_bytes) || typeof row.digest !== "string") {
          throw new ApiError("The server returned an invalid social image receipt", 502);
        }
        return { asset_id: row.asset_id, mime: String(row.mime), width: row.width as number, height: row.height as number, size_bytes: row.size_bytes as number, digest: row.digest };
      }, controller.signal);
    } finally { unsubscribe(); }
  },
  socialUploadedAsset: async (body:{action_token:string;alias:string;asset_id:string}):Promise<{bytes:ArrayBuffer;mime:string}> => {
    const generation = privateSessionGeneration();
    const controller = new AbortController();
    const unsubscribe = onPrivateReadsPaused(() => controller.abort());
    try {
      return await withPrivateOperation(async (op) => {
        const response = await privateFetch("/api/app-social-drafts/uploaded", { method: "POST", credentials: "same-origin", cache: "no-store", signal: controller.signal,
          headers: { "Content-Type": "application/json", "X-Cadence-Board": "1", ...sessionHeaders() }, body: JSON.stringify(toolActionBody({action_token: body.action_token, alias: body.alias, asset_id: body.asset_id})) }, op);
        if (!response.ok) throw appResponseError(response, await response.json().catch(() => null));
        const mime = response.headers.get("Content-Type") ?? "";
        if (!["image/jpeg", "image/png"].includes(mime)) throw new ApiError("The server returned an invalid social image", 502);
        const bytes = await response.arrayBuffer();
        op.assertCurrent();
        if (controller.signal.aborted || !privateReadsAllowed() || generation !== privateSessionGeneration()) throw new DOMException("The private session changed", "AbortError");
        return { bytes, mime };
      }, controller.signal);
    } finally { unsubscribe(); }
  },
  socialDraftAsset: async (body:{action_token:string;alias:string;draft_id:string}):Promise<{bytes:ArrayBuffer;mime:string}> => {
    const generation = privateSessionGeneration();
    const controller = new AbortController();
    const unsubscribe = onPrivateReadsPaused(() => controller.abort());
    try {
      return await withPrivateOperation(async (op) => {
        const response = await privateFetch("/api/app-social-drafts/asset", { method: "POST", credentials: "same-origin", cache: "no-store", signal: controller.signal,
          headers: { "Content-Type": "application/json", "X-Cadence-Board": "1", ...sessionHeaders() }, body: JSON.stringify(toolActionBody(body)) }, op);
        if (!response.ok) throw appResponseError(response, await response.json().catch(() => null));
        const mime = response.headers.get("Content-Type") ?? "";
        if (!["image/jpeg", "image/png"].includes(mime)) throw new ApiError("The server returned an invalid social image", 502);
        const bytes = await response.arrayBuffer();
        op.assertCurrent();
        if (controller.signal.aborted || !privateReadsAllowed() || generation !== privateSessionGeneration()) {
          throw new DOMException("The private session changed", "AbortError");
        }
        return { bytes, mime };
      }, controller.signal);
    } finally { unsubscribe(); }
  },
  /** CAD-1365: the native original bytes for `{action_token, asset_id}` only. The response is bounded
   *  (10 MiB, allowlisted image MIME) and the request lifetime is capped; the caller owns the bytes. */
  originalMedia: async (body: { action_token: string; asset_id: string }): Promise<{ bytes: ArrayBuffer; mime: string }> => {
    const generation = privateSessionGeneration();
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), ORIGINAL_MEDIA_TIMEOUT_MS);
    const unsubscribe = onPrivateReadsPaused(() => controller.abort());
    try {
      return await withPrivateOperation(async (op) => {
        const response = await privateFetch("/api/app-media/original", { method: "POST", credentials: "same-origin", cache: "no-store", signal: controller.signal,
          headers: { "Content-Type": "application/json", "X-Cadence-Board": "1", ...sessionHeaders() }, body: JSON.stringify(body) }, op);
        if (!response.ok) throw appResponseError(response, await response.json().catch(() => null));
        const mime = response.headers.get("Content-Type") ?? "";
        if (!ORIGINAL_MIME.includes(mime)) throw new ApiError("The server returned an invalid original image", 502);
        const bytes = await readBounded(response);
        op.assertCurrent();
        if (controller.signal.aborted || !privateReadsAllowed() || generation !== privateSessionGeneration()) {
          throw new DOMException("The private session changed", "AbortError");
        }
        return { bytes, mime };
      }, controller.signal);
    } finally { clearTimeout(timer); unsubscribe(); }
  },
  artifact: (id: string, signal?: AbortSignal) => request<TextArtifact>(`/api/app-run-artifacts/${part(id)}`, signal),
  stageEffect: async (id: string, body: { artifact_id: string; slot: string; request_id: string; title: string }) => (await request<{ effect: AppEffect }>(`${run(id)}/effects`, undefined, body)).effect,
  decideEffect: async (id: string, body: { digest: string; decision: "accept" | "decline" }) => (await request<{ effect: AppEffect }>(`${effect(id)}/decide`, undefined, body)).effect,
  resolveEffect: async (id: string, body: { digest: string; resolution: "close" | "acknowledge" }) => (await request<{ effect: AppEffect }>(`${effect(id)}/resolve`, undefined, body)).effect,
  outbox: (id: string, signal?: AbortSignal) => request<WorkspaceOutbox>(`/api/outbox?effect_id=${part(id)}`, signal),
};

// ---------- CAD-1129: the apps Explorer ----------

/** Fired after an install, remove or restore so the sidebar's app list
 * re-reads at once instead of waiting for the next navigation. */
export const APPS_CHANGED_EVENT = "cadence:apps-changed";

function replaceHostedRow<T extends { install_id: string }>(rows: T[] | null, change: Installation, project: (row: T) => T | null): T[] | null {
  const index = rows?.findIndex((row) => row.install_id === change.install_id) ?? -1;
  if (!rows || index < 0) return rows;
  const next = rows.slice();
  const row = project(next[index]);
  if (row === null) next.splice(index, 1);
  else next[index] = row;
  return next;
}

/** Apply only the verified lifecycle fields to a previously observed row. */
export function applyHostedLifecycleToList(rows: Installation[] | null, change: Installation): Installation[] | null {
  return replaceHostedRow(rows, change, (row) => {
    // A concurrent replacement is not a restore of the retained package.
    // Drop its stale launch metadata until the normal list read verifies it.
    if (typeof change.digest === "string" && change.digest !== row.digest) return null;
    return {
      ...row, state: change.state, executable: change.executable, removed_at: change.removed_at, restore_until: change.restore_until,
    };
  });
}

export function applyHostedLifecycleToHome(rows: HomeInstallation[] | null, change: Installation): HomeInstallation[] | null {
  return replaceHostedRow(rows, change, (row) => ({
    ...row,
    restore_until: change.restore_until,
    attention: change.state === "removed"
      ? { state: "removed", message: "Removed", action: "restore", count: 0 }
      : change.state === "disabled"
        ? { state: "off", message: "Disabled", action: "restore", count: 0 }
        : { state: "ok", message: null, action: null, count: 0 },
  }));
}

export function hostedLifecycleFromEvent(event: Event): Installation | undefined {
  return (event as CustomEvent<Installation | undefined>).detail;
}

export function publishHostedLifecycle(change: Installation): void {
  if (!privateReadsAllowed()) return;
  appsChanged(change);
}

export function notifyAppsChanged(): void {
  appsChanged();
}

function appsChanged(change?: Installation): void {
  dropAppCatalogSnapshot();
  installationsEpoch += 1;
  abortFlights(installationListReads);
  abortFlights(homeReads);
  abortFlights(appHomeReads);
  if (change) {
    const session = sessionKey();
    if (installationsSnapshot?.session === session) {
      installationsSnapshot.rows = applyHostedLifecycleToList(installationsSnapshot.rows, change)!;
    }
    if (appHomeSnapshot?.session === session && appHomeSnapshot.authority === "operator") {
      appHomeSnapshot.rows = applyHostedLifecycleToHome(appHomeSnapshot.rows, change)!;
    }
  } else dropInstallationsSnapshot();
  try {
    window.dispatchEvent(change ? new window.CustomEvent(APPS_CHANGED_EVENT, { detail: change }) : new Event(APPS_CHANGED_EVENT));
  } catch { /* no window in a test */ }
}

const CANONICAL_SEMVER = /^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$/;
const SAFE_CATALOG_ID = /^[A-Za-z0-9._-]{1,64}$/;

/** Numeric semver compare; `null` when either side is not plain `X.Y.Z`. */
export function compareSemverVersions(a: string, b: string): number | null {
  if (!CANONICAL_SEMVER.test(a) || !CANONICAL_SEMVER.test(b)) return null;
  const left = a.split(".").map(BigInt);
  const right = b.split(".").map(BigInt);
  for (let i = 0; i < 3; i += 1) {
    if (left[i] > 18446744073709551615n || right[i] > 18446744073709551615n) return null;
  }
  for (let i = 0; i < 3; i += 1) {
    if (left[i] !== right[i]) return left[i] < right[i] ? -1 : 1;
  }
  return 0;
}

export type CatalogUpdateHint = {
  catalogId: string;
  latestVersion: string;
  digest: string | null;
};

export function isSafeCatalogId(id: string | null | undefined): id is string {
  return typeof id === "string" && SAFE_CATALOG_ID.test(id) && !id.startsWith(".") && !id.includes("..");
}

export function catalogUpdateHref(catalogId: string, installId: string): string {
  return `/apps/catalog/${encodeURIComponent(catalogId)}?${new URLSearchParams({ installation: installId })}`;
}

/**
 * Derive actionable marketplace update hints keyed by `install_id`. Requires a
 * strictly newer SemVer (`compareSemverVersions === 1`) when the installed version
 * is known so every hint opens a real update action in the catalog panel.
 */
export function pendingCatalogUpdates(
  cards: readonly CatalogCard[] | null | undefined,
  installed?: readonly {
    install_id: string;
    version?: string | null;
    state?: string | null;
    attention?: { state: string };
  }[] | null,
): Map<string, CatalogUpdateHint> {
  const updates = new Map<string, CatalogUpdateHint>();
  if (!cards?.length) return updates;
  const byInstall = installed?.length
    ? new Map(installed.map((row) => [row.install_id, row]))
    : null;
  for (const card of cards) {
    if (
      card.update_available !== true ||
      card.retired === true ||
      card.state === "removed" ||
      card.state === "off" ||
      !isSafeCatalogId(card.id) ||
      typeof card.install_id !== "string" ||
      !card.install_id ||
      typeof card.version !== "string" ||
      compareSemverVersions(card.version, card.version) !== 0
    ) {
      continue;
    }
    const row = byInstall?.get(card.install_id);
    if (row) {
      const attState = row.attention?.state;
      if (
        row.state === "removed" ||
        row.state === "disabled" ||
        row.state === "off" ||
        attState === "removed" ||
        attState === "off"
      ) {
        continue;
      }
      if (typeof row.version === "string" && row.version.length > 0) {
        if (compareSemverVersions(card.version, row.version) !== 1) continue;
      }
    }
    updates.set(card.install_id, {
      catalogId: card.id,
      latestVersion: card.version,
      digest: typeof card.digest === "string" ? card.digest : null,
    });
  }
  return updates;
}


/** One catalog card. The built-in catalog is retired (CAD-1422): a
 *  daemon row is a tombstone (`retired`, `retired_reason`) for a
 *  catalog id a historical install or open request still names; the
 *  only live card is the operator's checked Git entry. */
export interface CatalogCard {
  id: string; source_kind: "builtin" | "git"; name: string; title: string;
  version?: string; tagline?: string; digest?: string; trust: "cadence" | "unreviewed";
  /** CAD-1422: the built-in source is retired — `retired_reason` says why. */
  retired?: boolean; retired_reason?: string;
  state?: "available" | "installed" | "off" | "requested" | "removed";
  install_id?: string; removed?: boolean; restorable?: boolean; requested_by_me?: boolean; request_count?: number;
  update_available?: boolean; featured?: boolean;
  /** Git check's resolved repository and commit. */
  source_url?: string; commit?: string; dir?: string;
  listing?: Listing;
  access?: { icon: string; title: string; sentence: string; chip?: unknown; note?: string }[];
  never?: string;
  about?: string; can?: string[]; screenshots?: { file: string; caption?: string }[];
  setup?: SetupStep[]; data?: ListingData; access_notes?: Record<string, string>;
  i18n?: Record<string, ListingDisplayLocale> | null;
  changes?: { version: string; notes: string[]; keeps?: string }[];
}

/** The Apps home's one row per live install (CAD-1129 H3). */
export interface HomeInstallation {
  install_id: string; name: string; title: string;
  /** Legacy project association, when the install has one — two
   *  same-name installs keep it visible so a reader can tell them
   *  apart (the old `/apps` list's `Project:` chip). */
  project?: string;
  tagline?: string; icon?: string;
  navigation?: ScreenNavigation | null;
  i18n?: AppDisplayI18n | null;
  /** The readable manifest record, when the row carries one: the
   *  installed version, the bundle's own listing copy and the recorded
   *  catalogue locator (null/absent on a path or Git install). */
  version?: string; listing?: Listing | null; catalogue?: CatalogueLocator | null;
  source_kind?: "private" | null;
  restore_until?: number | null;
  attention: {
    state: "ok" | "setup" | "update" | "off" | "attention" | "removed";
    message: string | null; action: string | null; count: number;
  };
}

/** The pinned apps as the board uses them: install ids in pin order, and
 * the same ids by most recently opened. */
export interface FavoritesPayload {
  favorites: string[]; workspace_default: string[]; recent: string[];
}

/** `app_favorites_get` / `app_favorites_put` as the daemon sends them. */
interface DaemonFavorites {
  owner?: string; is_default?: boolean;
  favorites?: { install_id: string; position?: number; opened_at?: number | null }[];
}

function favoritesFromDaemon(raw: DaemonFavorites): FavoritesPayload {
  const rows = [...(raw.favorites ?? [])].sort((a, b) => (a.position ?? 0) - (b.position ?? 0));
  const recent = rows.filter((r) => typeof r.opened_at === "number")
    .sort((a, b) => (b.opened_at as number) - (a.opened_at as number)).map((r) => r.install_id);
  return { favorites: rows.map((r) => r.install_id), workspace_default: [], recent };
}

/** `listing:` v1 (CAD-1129) — the OS-store card text. */
export interface SetupStep { slot?: string; connection?: string; label: string; help?: string; recommended?: string }
export interface ListingData { stores?: string[]; personal?: boolean }
export interface Listing {
  tagline?: string; icon?: string; category?: string; tags?: string[];
  publisher?: { name: string; url?: string };
  about?: string; can?: string[];
  screenshots?: { file: string; caption?: string }[];
  setup?: SetupStep[]; access_notes?: Record<string, string>;
  changes?: { version: string; notes: string[]; keeps?: string }[];
  data?: ListingData;
  i18n?: Record<string, ListingDisplayLocale> | null;
}

export interface InstallRequest {
  id: string; catalog_id: string; requested_by: string; at: number;
}

/** The remove sheet's `remove-preview` payload. */
export interface RemovePreview {
  install_id: string; name: string; title: string;
  personal_data: boolean; generation: string; digest: string;
  keeps?: { data?: string };
}

/** The `install-check` receipt: what an install would admit, before it
 * writes anything (CAD-1186/1194). `catalogue`/`access_diff`/
 * `compatibility` ride the same daemon reply the upgrade proposal reports. */
export interface InstallCheck {
  schema: number; name: string; version: string; digest: string;
  files: string[]; committed: false; notes: string[];
  secret_warnings: { rule: string; line: number; column: number; redacted: string; severity: string }[];
  catalogue?: CatalogueLocator | null;
  access_diff?: AccessDiff;
  compatibility?: CompatibilityReceipt;
}

/** The upload transport's body: a flat `{path: utf8}` bundle map, or
 * (CAD-1942) a catalogue locator alone — the board fetches and verifies
 * that release itself — plus the selected release's expected digest. */
export type FilesUploadBody = { expected_digest?: string } & (
  | { files: Record<string, string>; catalogue?: never }
  | { catalogue: CatalogueLocator; files?: never }
);

/** An upgrade body is a `files` map, a legacy `source`, or (CAD-1942) a
 * catalogue locator alone whose release the board fetches — the same
 * closed shape `workspace_upgrade_transport` admits. */
export type UpgradeBody = {
  expected_digest: string; expected_generation: string;
  catalogue?: CatalogueLocator; catalogue_resolution?: "manual";
} & ({ files: Record<string, string> } | { source: string } | { catalogue: CatalogueLocator });

/** A files-mode upgrade check pins the install's recorded digest and the
 * catalog generation it was read under; a checked apply adds the new
 * bundle digest and a retained request id. */
export type UpgradeCheckBody = UpgradeBody;
export type UpgradeApplyBody = UpgradeBody & {
  expected_new_digest: string;
  request_id: string;
  automatic?: true;
  observed_approval?: "approved";
};

export const appExplorer = {
  catalog: (signal?: AbortSignal) => request<{ catalog: CatalogCard[] }>("/api/app-catalog", signal),
  entry: (id: string, signal?: AbortSignal) => request<CatalogCard>(`/api/app-catalog/${part(id)}`, signal),
  home: (signal?: AbortSignal, authority?: "operator" | "member" | "viewer") => authority
    ? coalesceInFlightRead(homeReads, authority, (sharedSignal) => request<{ installations: HomeInstallation[] }>("/api/app-home", sharedSignal), signal)
    : request<{ installations: HomeInstallation[] }>("/api/app-home", signal),
  favorites: async (signal?: AbortSignal) => favoritesFromDaemon(await request<DaemonFavorites>("/api/app-favorites", signal)),
  requests: (signal?: AbortSignal) => request<{ requests: InstallRequest[] }>("/api/app-requests", signal),
  /** CAD-1194: the read-only install proposal. `source` is a path or a
   * Git URL (`builtin:<catalog id>` is the retired source form and is
   * refused); the returned `digest` is the pin every install below must
   * carry. */
  installCheck: (source: string) => request<InstallCheck>("/api/app-installations/check", undefined, { source }),
  /** Install a checked source (a Git URL) — the workspace install route,
   * pinned to the checked digest so changed bytes are refused. */
  installSource: (source: string, expectedDigest: string) =>
    request<Installation>("/api/app-installations", undefined, { source, expected_digest: expectedDigest }),
  /** The guarded install check: the board stages the posted bundle, or the
   * catalogue release it fetches itself, and the daemon reports what it
   * would admit without writing anything. The reply's `digest` must equal
   * the release's. */
  uploadCheck: (body: FilesUploadBody, signal?: AbortSignal) =>
    request<InstallCheck>("/api/app-installations/upload/check", signal, body),
  /** The guarded install: the same bundle or catalogue locator, pinned to
   * the checked `expected_digest`. The reply is the committed installation
   * record. */
  upload: (body: FilesUploadBody, signal?: AbortSignal) =>
    request<CommittedInstallation>("/api/app-installations/upload", signal, body),
  gitCheck: (url: string) => request<CatalogCard>("/api/app-catalog/git-check", undefined, { url }),
  putFavorites: async (installIds: string[]) => favoritesFromDaemon(await request<DaemonFavorites>("/api/app-favorites", undefined, { install_ids: installIds })),
  putFavoritesDefault: async (installIds: string[]) => favoritesFromDaemon(await request<DaemonFavorites>("/api/app-favorites/default", undefined, { install_ids: installIds })),
  opened: (installId: string) => request<unknown>("/api/app-favorites/opened", undefined, { install_id: installId }),
  requestInstall: (catalogId: string) => request<InstallRequest>("/api/app-catalog/request", undefined, { catalog_id: catalogId }),
  dismissRequest: (id: string) => request<unknown>("/api/app-requests/dismiss", undefined, { id }),
  updateCheck: (installId: string) => request<{ has_update: boolean; digest: string; access_change?: { state: string; added?: string[]; removed?: string[] } }>(`/api/app-installations/${part(installId)}/update-check`, undefined, {}),
  automaticUpdates: (installId: string, signal?: AbortSignal) =>
    request<{ install_id: string; automatic: boolean }>(`/api/app-installations/${part(installId)}/automatic-updates`, signal),
  setAutomaticUpdates: (installId: string, automatic: boolean) =>
    request<{ install_id: string; automatic: boolean }>(`/api/app-installations/${part(installId)}/automatic-updates`, undefined, { automatic }),
  removePreview: (installId: string) => request<RemovePreview>(`/api/app-installations/${part(installId)}/remove-preview`, undefined, {}),
  remove: (installId: string, body: { expected_generation: string; expected_digest: string; request_id: string }) => request<unknown>(`/api/app-installations/${part(installId)}/remove`, undefined, body),
  restore: (installId: string) => request<{ restored: boolean; digest: string }>(`/api/app-installations/${part(installId)}/restore`, undefined, {}),
  hostedRemove: (installId: string) => request<unknown>(`/api/app-installations/${part(installId)}/remove`, undefined, undefined, true),
  hostedRestore: (installId: string) => request<unknown>(`/api/app-installations/${part(installId)}/restore`, undefined, undefined, true),
  /** CAD-2215: exact-conversation member access. The read is validated
   * before it reaches a screen, so a malformed answer is an error, not an
   * empty grant list. `members` is the daemon-verified live roster (or
   * null when this session cannot prove it); `grant_reason` is "" only
   * when a grant may proceed. */
  memberAccess: async (installId: string, signal?: AbortSignal): Promise<MemberAccess> =>
    parseMemberAccess(await request<unknown>(`/api/app-installations/${part(installId)}/member-access`, signal)),
  /** One exact member and one exact conversation; the pinned digest is the
   * one the operator reviewed. */
  grantMemberAccess: async (
    installId: string,
    body: { member_sub: string; conversation_id: string; expected_digest: string },
  ): Promise<MemberAccessGrant> =>
    parseGrantReceipt(await request<unknown>(`/api/app-installations/${part(installId)}/member-access`, undefined, body)),
  /** Revoke exactly that member from exactly that conversation. */
  revokeMemberAccess: async (
    installId: string,
    body: { member_sub: string; conversation_id: string },
  ): Promise<RevokeReceipt> =>
    parseRevokeReceipt(await request<unknown>(`/api/app-installations/${part(installId)}/member-access/revoke`, undefined, body)),
};
// ---------- CAD-1137: per-session snapshot cache ----------

/**
 * The app page's read set in one in-memory snapshot, keyed by the route's
 * installation. A revisit paints it at once and the page revalidates behind
 * it (stale-while-revalidate); writes go through `workspaceApps`.
 *
 * The store is this tab's memory only — nothing is persisted — and it is
 * cleared when the session key changes, so a sign-out or a different
 * operator session can never see the previous session's payload. Entries
 * older than `SNAPSHOT_TTL_MS` are ignored rather than revived.
 */
export interface WorkspaceAppSnapshot {
  installation: Installation;
  contexts: AppContext[];
  bindings: AppBinding[];
  connections: Connection[];
  agents: Agent[];
  runs: WorkspaceRun[];
  effects: AppEffect[];
}

const SNAPSHOT_TTL_MS = 10 * 60 * 1000;
const snapshots = new Map<string, { at: number; session: string | null; data: WorkspaceAppSnapshot }>();

/** The last snapshot for `installId` from this operator session, or null. */
export function readWorkspaceAppSnapshot(installId: string): WorkspaceAppSnapshot | null {
  const entry = snapshots.get(installId);
  if (!entry) return null;
  if (entry.session !== sessionKey() || Date.now() - entry.at > SNAPSHOT_TTL_MS) {
    snapshots.delete(installId);
    return null;
  }
  return entry.data;
}

/** Record the snapshot a successful read produced. */
export function writeWorkspaceAppSnapshot(installId: string, data: WorkspaceAppSnapshot, session = sessionKey(), generation = privateSessionGeneration()): void {
  if (session !== sessionKey() || generation !== privateSessionGeneration() || !privateReadsAllowed()) return;
  snapshots.set(installId, { at: Date.now(), session, data });
}

type InFlightRead<T> = {
  installId: string;
  controller: AbortController;
  subscribers: number;
  settled: boolean;
  promise: Promise<T>;
};

const snapshotReads = new Map<string, InFlightRead<WorkspaceAppSnapshot>>();
const screenSnapshotReads = new Map<string, InFlightRead<WorkspaceAppSnapshot>>();
const scopeReads = new Map<string, InFlightRead<[Installation, AppContext[]]>>();
const appHomeReads = new Map<string, InFlightRead<HomeInstallation[]>>();
const appFavoritesReads = new Map<string, InFlightRead<FavoritesPayload>>();
const appCatalogReads = new Map<string, InFlightRead<CatalogCard[]>>();
const installationListReads = new Map<string, InFlightRead<Installation[]>>();
let installationsEpoch = 0;
const homeReads = new Map<string, InFlightRead<{ installations: HomeInstallation[] }>>();
let snapshotEpoch = 0;

function abortFlights<T>(reads: Map<string, InFlightRead<T>>): void {
  for (const flight of reads.values()) flight.controller.abort();
  reads.clear();
}

function abortInstallFlights<T>(reads: Map<string, InFlightRead<T>>, installId: string): void {
  for (const [key, flight] of reads) {
    if (flight.installId === installId) {
      if (reads.get(key) === flight) reads.delete(key);
      flight.controller.abort();
    }
  }
}

/** Coalesce concurrent identical in-flight reads without retaining settled past receipts. */
function coalesceInFlightRead<T>(
  reads: Map<string, InFlightRead<T>>,
  installId: string,
  loader: (signal: AbortSignal, requestSession: string | null, generation: number) => Promise<T>,
  signal?: AbortSignal,
): Promise<T> {
  if (signal?.aborted) return Promise.reject(new DOMException("The operation was aborted", "AbortError"));
  const requestSession = sessionKey();
  const generation = privateSessionGeneration();
  const epoch = snapshotEpoch;
  const key = JSON.stringify([installId, requestSession, generation]);
  let flight = reads.get(key);
  if (!flight) {
    const controller = new AbortController();
    const created: InFlightRead<T> = { installId, controller, subscribers: 0, settled: false, promise: Promise.resolve(null as never) };
    created.promise = (async () => {
      const value = await loader(controller.signal, requestSession, generation);
      if (controller.signal.aborted) throw new DOMException("The operation was aborted", "AbortError");
      if (epoch !== snapshotEpoch || requestSession !== sessionKey() || generation !== privateSessionGeneration() || !privateReadsAllowed()) {
        throw new ApiError("The session changed while loading this app", 401);
      }
      return value;
    })().finally(() => {
      created.settled = true;
      if (reads.get(key) === created) reads.delete(key);
    });
    flight = created;
    reads.set(key, flight);
  }
  const shared = flight;
  shared.subscribers += 1;
  return new Promise((resolve, reject) => {
    let released = false;
    const release = () => {
      if (released) return;
      released = true;
      signal?.removeEventListener("abort", abort);
      shared.subscribers -= 1;
      if (shared.subscribers === 0 && !shared.settled) {
        if (reads.get(key) === shared) reads.delete(key);
        shared.controller.abort();
      }
    };
    const abort = () => { release(); reject(new DOMException("The operation was aborted", "AbortError")); };
    signal?.addEventListener("abort", abort, { once: true });
    shared.promise.then(value => {
      if (released) return;
      release();
      if (epoch !== snapshotEpoch || shared.controller.signal.aborted) {
        reject(new DOMException("The private session changed", "AbortError"));
      } else {
        resolve(value);
      }
    }, error => { if (!released) { release(); reject(error); } });
  });
}

/** Forget one installation's snapshot (access refusal, removal). */
export function dropWorkspaceAppSnapshot(installId: string): void {
  snapshots.delete(installId);
  abortInstallFlights(snapshotReads, installId);
  abortInstallFlights(screenSnapshotReads, installId);
  abortInstallFlights(scopeReads, installId);
}

/** Concurrent `(detail, contexts)` scope reads share the in-flight request without retaining past receipts. */
export function readAppScopeFresh(installId: string, signal?: AbortSignal): Promise<[Installation, AppContext[]]> {
  return coalesceInFlightRead(
    scopeReads,
    installId,
    (sharedSignal) => Promise.all([
      workspaceApps.detail(installId, sharedSignal),
      workspaceApps.contexts(installId, sharedSignal),
    ]),
    signal,
  );
}

/**
 * Framed screen snapshot read: reuses the shell's verified `(installation, contexts)`
 * scope and fetches only `(bindings, runs, effects)` without blocking on `/api/agents`
 * or `/api/connections`.
 */
export function readWorkspaceScreenSnapshotFresh(
  installId: string,
  scope: { installation: Installation; contexts: AppContext[] },
  signal?: AbortSignal,
): Promise<WorkspaceAppSnapshot> {
  return coalesceInFlightRead(
    screenSnapshotReads,
    installId,
    async (sharedSignal, requestSession, generation) => {
      const [bindings, runs, effects] = await Promise.all([
        workspaceApps.bindings(installId, undefined, sharedSignal),
        workspaceApps.runs(installId, undefined, sharedSignal),
        workspaceApps.effects(installId, undefined, sharedSignal),
      ]);
      const existing = readWorkspaceAppSnapshot(installId);
      const data: WorkspaceAppSnapshot = {
        installation: scope.installation,
        contexts: scope.contexts,
        bindings,
        connections: existing?.connections ?? [],
        agents: existing?.agents ?? [],
        runs,
        effects,
      };
      writeWorkspaceAppSnapshot(installId, data, requestSession, generation);
      return data;
    },
    signal,
  );
}

/** The app page's reads, fired in parallel and stored as one snapshot. */
export function readWorkspaceAppSnapshotFresh(installId: string, signal?: AbortSignal): Promise<WorkspaceAppSnapshot> {
  return coalesceInFlightRead(
    snapshotReads,
    installId,
    async (sharedSignal, requestSession, generation) => {
      const [[installation, contexts], bindings, connections, agents, runs, effects] = await Promise.all([
        readAppScopeFresh(installId, sharedSignal),
        workspaceApps.bindings(installId, undefined, sharedSignal),
        workspaceApps.connections(sharedSignal),
        workspaceApps.agents(sharedSignal),
        workspaceApps.runs(installId, undefined, sharedSignal),
        workspaceApps.effects(installId, undefined, sharedSignal),
      ]);
      const data = { installation, contexts, bindings, connections, agents, runs, effects };
      writeWorkspaceAppSnapshot(installId, data, requestSession, generation);
      return data;
    },
    signal,
  );
}

/**
 * Warm only `(detail, contexts)` on sidebar hover/focus for screen-records apps
 * without triggering legacy polling endpoints; joins an in-flight scope read.
 */
const prefetchingScopes = new Map<string, Promise<unknown>>();
export function prefetchAppScope(installId: string): void {
  if (prefetchingScopes.has(installId)) return;
  const task = readAppScopeFresh(installId)
    .catch(() => null)
    .finally(() => { if (prefetchingScopes.get(installId) === task) prefetchingScopes.delete(installId); });
  prefetchingScopes.set(installId, task);
}

/**
 * Warm the cache for `installId` — the sidebar's app links call this on
 * hover/focus so opening the app paints before the click lands. Joins a
 * read already in flight for the same installation; failures are dropped.
 */
const prefetching = new Map<string, Promise<unknown>>();
export function prefetchWorkspaceApp(installId: string): void {
  if (prefetching.has(installId)) return;
  if (readWorkspaceAppSnapshot(installId) !== null) return;
  const task = readWorkspaceAppSnapshotFresh(installId)
    .catch(() => null)
    .finally(() => { if (prefetching.get(installId) === task) prefetching.delete(installId); });
  prefetching.set(installId, task);
}

/** The `/api/app-installations` list, held the same way: the Apps
 *  screen paints the last read at once and refetches behind it. */
const APP_CATALOG_TTL_MS = 5 * 60 * 1000;
let installationsSnapshot: { at: number; session: string | null; rows: Installation[] } | null = null;
let appHomeSnapshot: { at: number; session: string | null; authority: string; rows: HomeInstallation[] } | null = null;
let appFavoritesSnapshot: { at: number; session: string | null; data: FavoritesPayload } | null = null;
let appCatalogSnapshot: { at: number; session: string | null; cards: CatalogCard[] } | null = null;

export function readAppCatalogSnapshot(): CatalogCard[] | null {
  if (!appCatalogSnapshot || !privateReadsAllowed() || appCatalogSnapshot.session !== sessionKey() ||
      Date.now() - appCatalogSnapshot.at > APP_CATALOG_TTL_MS) {
    appCatalogSnapshot = null;
    return null;
  }
  return appCatalogSnapshot.cards;
}

/**
 * Operator-only TTL-cached catalog read: returns the cached snapshot without
 * hitting `/api/app-catalog` while fresh (5 min TTL), coalesces concurrent reads,
 * and drops cleanly on `notifyAppsChanged()`, session pause, or session reset.
 */
export function readAppCatalogCached(signal?: AbortSignal): Promise<CatalogCard[]> {
  const cached = readAppCatalogSnapshot();
  if (cached !== null) return Promise.resolve(cached);
  return coalesceInFlightRead(
    appCatalogReads,
    "__app_catalog__",
    async (sharedSignal, requestSession) => {
      const res = await appExplorer.catalog(sharedSignal);
      const cards = Array.isArray(res.catalog) ? res.catalog : [];
      appCatalogSnapshot = { at: Date.now(), session: requestSession, cards };
      return cards;
    },
    signal,
  );
}

export function dropAppCatalogSnapshot(): void {
  appCatalogSnapshot = null;
  abortFlights(appCatalogReads);
}

export function readInstallationsSnapshot(): Installation[] | null {
  if (!installationsSnapshot || !privateReadsAllowed() || installationsSnapshot.session !== sessionKey() ||
      Date.now() - installationsSnapshot.at > SNAPSHOT_TTL_MS) return null;
  return installationsSnapshot.rows;
}

/** Guard the installation read against session and mutation changes. */
export async function readInstallationsGuarded(signal?: AbortSignal, shared = false, storeSnapshot = true): Promise<Installation[]> {
  const requestSession = sessionKey();
  const generation = privateSessionGeneration();
  const epoch = snapshotEpoch;
  const listEpoch = installationsEpoch;
  const rows = await workspaceApps.installations(signal, shared);
  if (signal?.aborted) throw new DOMException("The operation was aborted", "AbortError");
  if (listEpoch !== installationsEpoch || epoch !== snapshotEpoch || requestSession !== sessionKey() || generation !== privateSessionGeneration() || !privateReadsAllowed()) {
    throw new ApiError("The session changed while loading apps", 401);
  }
  if (storeSnapshot && Array.isArray(rows)) installationsSnapshot = { at: Date.now(), session: requestSession, rows };
  return rows;
}

export function dropInstallationsSnapshot(): void {
  installationsSnapshot = null;
}

export function readAppHomeSnapshot(authority: string): HomeInstallation[] | null {
  if (!appHomeSnapshot || appHomeSnapshot.authority !== authority || !privateReadsAllowed() || appHomeSnapshot.session !== sessionKey() ||
      Date.now() - appHomeSnapshot.at > SNAPSHOT_TTL_MS) return null;
  return appHomeSnapshot.rows;
}

export function readAppHomeFresh(signal: AbortSignal | undefined, authority: "operator" | "member" | "viewer" | undefined): Promise<HomeInstallation[]> {
  return coalesceInFlightRead(
    appHomeReads,
    `__app_home__:${authority ?? "unknown"}`,
    async (sharedSignal, requestSession) => {
      const res = await appExplorer.home(sharedSignal, authority);
      const rows = res.installations;
      if (authority && Array.isArray(rows)) appHomeSnapshot = { at: Date.now(), session: requestSession, authority, rows };
      return rows;
    },
    signal,
  );
}

export function readAppFavoritesSnapshot(): FavoritesPayload | null {
  if (!appFavoritesSnapshot || !privateReadsAllowed() || appFavoritesSnapshot.session !== sessionKey() ||
      Date.now() - appFavoritesSnapshot.at > SNAPSHOT_TTL_MS) return null;
  return appFavoritesSnapshot.data;
}

export function readAppFavoritesFresh(signal?: AbortSignal): Promise<FavoritesPayload> {
  return coalesceInFlightRead(
    appFavoritesReads,
    "__app_favorites__",
    async (sharedSignal, requestSession) => {
      const data = await appExplorer.favorites(sharedSignal);
      if (data) appFavoritesSnapshot = { at: Date.now(), session: requestSession, data };
      return data;
    },
    signal,
  );
}

export function writeAppFavoritesSnapshot(data: FavoritesPayload): void {
  if (!privateReadsAllowed()) return;
  appFavoritesSnapshot = { at: Date.now(), session: sessionKey(), data };
}

onPrivateReadsPaused(() => {
  snapshotEpoch += 1;
  abortFlights(installationListReads);
  abortFlights(homeReads);
  abortFlights(snapshotReads);
  abortFlights(screenSnapshotReads);
  abortFlights(scopeReads);
  abortFlights(appHomeReads);
  abortFlights(appFavoritesReads);
  dropAppCatalogSnapshot();
  prefetching.clear();
  prefetchingScopes.clear();
});

onPrivateSessionReset(() => {
  abortFlights(installationListReads);
  abortFlights(homeReads);
  abortFlights(snapshotReads);
  abortFlights(screenSnapshotReads);
  abortFlights(scopeReads);
  abortFlights(appHomeReads);
  abortFlights(appFavoritesReads);
  dropAppCatalogSnapshot();
  snapshots.clear();
  prefetching.clear();
  prefetchingScopes.clear();
  dropInstallationsSnapshot();
  appHomeSnapshot = null;
  appFavoritesSnapshot = null;
});
