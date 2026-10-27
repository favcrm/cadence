import { useEffect, useRef, useState } from "react";
import { api, ApiError } from "../../../lib/api";
import { subscribeMasterChanged } from "../../../lib/masterChanged";
import { useTranslate } from "../../../lib/locale";
import Button from "../../../ui/Button";
import Link from "../../../ui/Link";
import { resourceHref, type ResourceRef } from "../assistantResourceNavigation";

interface Action {
  id: string;
  description: string;
  effect: string;
  confirmation: string;
  availability: string;
}
interface TagPreview {
  customer_label: string;
  before_tags: string[];
  after_tags: string[];
  expected_revision: number;
}
interface RecordPreview {
  record_id: string;
  operation: "record.create" | "record.update";
  before: Record<string, unknown> | null;
  after: Record<string, unknown>;
  expected_revision: number;
}
interface MediaQuote {
  schema: 1;
  currency: "USD";
  unit_price_micros: number;
  units: 1;
  total_price_micros: number;
  price_revision: string;
}
/** Frozen reference-edit card. `artifact_ref` is retained for approval
 *  equality and is never rendered as a link or as source facts. */
interface MediaRefinePreview {
  asset_id: string;
  revision: number;
  sha256: string;
  title: string;
  artifact_ref: string;
  instructions: string;
  workflow: "refine-image";
  slot: string;
  model: string;
  quote: MediaQuote;
  estimate_micros: number;
  currency: "USD";
  allow_once: true;
}
interface PermissionRequest {
  reason: string;
  scope: { install_id: string; context_id: string; action_id: string; resource_id: string };
  allow_always: boolean;
  preview: TagPreview | null;
  recordPreview: RecordPreview | null;
  mediaPreview: MediaRefinePreview | null;
  // Host-owned discriminator: "record" for a declared record change,
  // "media_refine" for one frozen library reference edit; absent or null
  // is legacy. Any other value is rejected at parse time.
  previewKind: "record" | "legacy" | "media_refine";
}
interface Operation {
  id: string;
  action_id: string;
  status: "pending_permission" | "running" | "succeeded" | "denied" | "failed" | "unknown";
  revision: number;
  summary: string;
  result: unknown;
  resource_refs: ResourceRef[];
  permission_request: PermissionRequest | null;
  error: string | null;
}
interface Permission {
  id: string;
  action_id: string;
  resource_id: string;
  effect: "allow" | "deny";
  revision: number;
  state: "active" | "revoked";
  scope_label: string;
  semantics_digest: string;
}

const statuses = new Set(["pending_permission", "running", "succeeded", "denied", "failed", "unknown"]);
/** Statuses that stay visible outside the completed-history disclosure:
 *  anything still actionable, in flight, failed or indeterminate. */
const ACTIVE_STATUSES = new Set(["pending_permission", "running", "failed", "unknown"]);
const isActiveOperation = (operation: Operation): boolean => ACTIVE_STATUSES.has(operation.status);
function record(value: unknown): Record<string, unknown> | null {
  return value !== null && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : null;
}
function text(value: unknown, max = 500): string | null {
  return typeof value === "string" && value.length > 0 && value.length <= max ? value : null;
}
function parseAction(value: unknown): Action | null {
  const v = record(value);
  if (!v || !text(v.id, 64) || !text(v.description, 300)) return null;
  return {
    id: v.id as string,
    description: v.description as string,
    effect: text(v.effect, 80) ?? "",
    confirmation: text(v.confirmation, 80) ?? "",
    availability: text(v.availability, 120) ?? "",
  };
}
function parseRef(value: unknown): ResourceRef | null {
  const v = record(value);
  if (!v || !text(v.kind, 40) || !text(v.id, 128) || !text(v.label, 160)) return null;
  return { kind: v.kind as string, id: v.id as string, label: v.label as string };
}
function parseTagPreview(value: unknown): TagPreview | null {
  const preview = record(value);
  if (!preview || text(preview.customer_label, 160) === null ||
      !Number.isSafeInteger(preview.expected_revision) || (preview.expected_revision as number) < 1 ||
      !Array.isArray(preview.before_tags) || !Array.isArray(preview.after_tags) ||
      preview.before_tags.length > 16 || preview.after_tags.length > 16) return null;
  const validTags = (tags: unknown[]) => tags.every((tag) => typeof tag === "string" && tag.length > 0 && tag.length <= 40);
  if (!validTags(preview.before_tags) || !validTags(preview.after_tags)) return null;
  return {
    customer_label: preview.customer_label as string,
    before_tags: preview.before_tags as string[],
    after_tags: preview.after_tags as string[],
    expected_revision: preview.expected_revision as number,
  };
}
const RECORD_FIELDS = ["display_name", "email", "phone", "tags", "source"] as const;
// The typed preview of a declared record change. Only the host's known
// customer fields are kept; anything else in the payload is ignored, never
// rendered. Returns null unless the shape is exact.
function parseRecordProfile(value: unknown): Record<string, unknown> | null {
  const profile = record(value);
  if (!profile) return null;
  const out: Record<string, unknown> = {};
  for (const field of RECORD_FIELDS) {
    const raw = profile[field];
    if (raw === undefined || raw === null) continue;
    if (field === "tags") {
      if (!Array.isArray(raw) || raw.length > 16 || !raw.every((tag) => typeof tag === "string" && tag.length > 0 && [...tag].length <= 40)) return null;
      out.tags = raw;
    } else {
      if (text(raw, 320) === null) return null;
      out[field] = raw;
    }
  }
  return out;
}
// Plain text only (React escapes it). An absent or empty field shows "None".
function recordFieldText(profile: Record<string, unknown> | null, field: string): string {
  const value = profile?.[field];
  if (Array.isArray(value)) return value.length ? value.join(", ") : "None";
  return typeof value === "string" && value.length > 0 ? value : "None";
}
function parseRecordPreview(value: unknown): RecordPreview | null {
  const preview = record(value);
  if (!preview || text(preview.record_id, 64) === null ||
      (preview.operation !== "record.create" && preview.operation !== "record.update") ||
      !Number.isSafeInteger(preview.expected_revision) || (preview.expected_revision as number) < 1) return null;
  const after = parseRecordProfile(preview.after);
  if (after === null) return null;
  const before = preview.before === null ? null : parseRecordProfile(preview.before);
  if (preview.before !== null && before === null) return null;
  return {
    record_id: preview.record_id as string,
    operation: preview.operation,
    before,
    after,
    expected_revision: preview.expected_revision as number,
  };
}
const MEDIA_PREVIEW_KEYS = ["asset_id", "revision", "sha256", "title", "artifact_ref", "instructions", "workflow", "slot", "model", "quote", "estimate_micros", "currency", "allow_once"] as const;
const MEDIA_QUOTE_KEYS = ["schema", "currency", "unit_price_micros", "units", "total_price_micros", "price_revision"] as const;
function closedKeys(value: Record<string, unknown>, keys: readonly string[]): boolean {
  const seen = Object.keys(value);
  return seen.length === keys.length && keys.every((key) => seen.includes(key));
}
function usdMicros(value: unknown): number | null {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 1 && value <= 1_000_000_000 ? value : null;
}
function formatUsdMicros(micros: number): string {
  const whole = Math.floor(micros / 1_000_000);
  const fraction = String(micros % 1_000_000).padStart(6, "0");
  return `USD ${whole}.${fraction}`;
}
function parseMediaRefinePreview(value: unknown): MediaRefinePreview | null {
  const preview = record(value);
  const quote = record(preview?.quote);
  if (!preview || !quote || !closedKeys(preview, MEDIA_PREVIEW_KEYS) || !closedKeys(quote, MEDIA_QUOTE_KEYS)) return null;
  const assetId = text(preview.asset_id, 64);
  const sha = text(preview.sha256, 71);
  const title = text(preview.title, 200);
  const artifact = text(preview.artifact_ref, 256);
  const instructions = text(preview.instructions, 2000);
  const slot = text(preview.slot, 64);
  const model = text(preview.model, 80);
  const revision = preview.revision;
  const estimate = usdMicros(preview.estimate_micros);
  const unit = usdMicros(quote.unit_price_micros);
  const total = usdMicros(quote.total_price_micros);
  const priceRevision = text(quote.price_revision, 128);
  if (assetId === null || sha === null || sha.length !== 71 || !sha.startsWith("sha256:") ||
      title === null || artifact === null || instructions === null || slot === null || model === null ||
      preview.workflow !== "refine-image" || preview.currency !== "USD" || preview.allow_once !== true ||
      !Number.isSafeInteger(revision) || (revision as number) < 1 ||
      estimate === null || unit === null || total === null || priceRevision === null ||
      quote.schema !== 1 || quote.currency !== "USD" || quote.units !== 1 ||
      estimate !== total || unit !== total) return null;
  return {
    asset_id: assetId,
    revision: revision as number,
    sha256: sha,
    title,
    artifact_ref: artifact,
    instructions,
    workflow: "refine-image",
    slot,
    model,
    quote: { schema: 1, currency: "USD", unit_price_micros: unit, units: 1, total_price_micros: total, price_revision: priceRevision },
    estimate_micros: estimate,
    currency: "USD",
    allow_once: true,
  };
}
function parsePermissionRequest(value: unknown): PermissionRequest | null {
  const v = record(value);
  const scope = record(v?.scope);
  if (!v || !scope || typeof v.allow_always !== "boolean") return null;
  if (v.preview_kind !== undefined && v.preview_kind !== null && v.preview_kind !== "record" && v.preview_kind !== "media_refine") return null;
  const fields = [scope.install_id, scope.context_id, scope.action_id, scope.resource_id];
  if (!fields.every((field) => typeof field === "string" && field.length > 0 && field.length <= 128)) return null;
  const reason = text(v.reason, 300);
  if (reason === null) return null;
  const mediaPreview = v.preview_kind === "media_refine" ? parseMediaRefinePreview(v.preview) : null;
  if (v.preview_kind === "media_refine" && mediaPreview === null) return null;
  return {
    reason,
    scope: {
      install_id: scope.install_id as string,
      context_id: scope.context_id as string,
      action_id: scope.action_id as string,
      resource_id: scope.resource_id as string,
    },
    allow_always: v.allow_always,
    preview: v.preview_kind === "media_refine" ? null : parseTagPreview(v.preview),
    recordPreview: v.preview_kind === "media_refine" ? null : parseRecordPreview(v.preview),
    mediaPreview,
    previewKind: v.preview_kind === "record" ? "record" : v.preview_kind === "media_refine" ? "media_refine" : "legacy",
  };
}
function parseOperation(value: unknown): Operation | null {
  const v = record(value);
  if (!v || !text(v.id, 128) || !text(v.action_id, 64) || !statuses.has(String(v.status)) ||
      !Number.isSafeInteger(v.revision) || !text(v.summary, 500) || !Array.isArray(v.resource_refs)) return null;
  const refs = v.resource_refs.map(parseRef);
  if (refs.some((ref) => ref === null)) return null;
  const request = v.permission_request == null ? null : parsePermissionRequest(v.permission_request);
  if (v.permission_request != null && request === null) return null;
  return {
    id: v.id as string,
    action_id: v.action_id as string,
    status: v.status as Operation["status"],
    revision: v.revision as number,
    summary: v.summary as string,
    result: v.result,
    resource_refs: refs as ResourceRef[],
    permission_request: request,
    error: text(v.error, 500),
  };
}
function parsePermission(value: unknown): Permission | null {
  const v = record(value);
  if (!v || !text(v.id, 128) || !text(v.action_id, 64) || !text(v.resource_id, 128) ||
      (v.effect !== "allow" && v.effect !== "deny") || !Number.isSafeInteger(v.revision) ||
      (v.state !== "active" && v.state !== "revoked") || !text(v.scope_label, 200) || !text(v.semantics_digest, 128)) return null;
  return {
    id: v.id as string,
    action_id: v.action_id as string,
    resource_id: v.resource_id as string,
    effect: v.effect,
    revision: v.revision as number,
    state: v.state,
    scope_label: v.scope_label as string,
    semantics_digest: v.semantics_digest as string,
  };
}

function availabilityLabel(value: string): { label: string; available: boolean; detail: string | null } {
  const state = value.trim().toLowerCase().replaceAll("-", "_");
  if (state === "available") return { label: "Available", available: true, detail: null };
  if (state === "requires_context" || state === "context_required") return { label: "Select a context", available: false, detail: null };
  if (state.includes("setup") || state.includes("connect")) return { label: "Setup required", available: false, detail: value.slice(0, 120) };
  if (state.includes("unavailable") || state === "disabled") return { label: "Unavailable", available: false, detail: value.slice(0, 120) };
  return { label: "Availability unknown", available: false, detail: null };
}

function resultValues(value: unknown, depth = 0): Array<[string, string]> {
  if (depth > 2) return [];
  const v = record(value);
  if (!v) return [];
  const rows: Array<[string, string]> = [];
  for (const [key, entry] of Object.entries(v).slice(0, 8)) {
    if (/token|secret|credential|authorization|html|url|email|phone|address|identity|consent|input|payload/i.test(key)) continue;
    if (typeof entry === "string" || typeof entry === "number" || typeof entry === "boolean") {
      rows.push([key.replaceAll("_", " "), String(entry).slice(0, 240)]);
    } else if (Array.isArray(entry) && entry.length <= 8 && entry.every((item) => ["string", "number", "boolean"].includes(typeof item))) {
      rows.push([key.replaceAll("_", " "), entry.map(String).join(", ").slice(0, 240)]);
    }
  }
  return rows;
}

/** Actions the operator may allow always, each with the label that states
 *  the grant's real scope: tags per customer, the paid Social actions for the
 *  whole installation (CAD-1852), and library image refine while the granting
 *  owner stays signed in on this board (CAD-1868). */
const STANDING_LABELS: Readonly<Record<string, string>> = {
  "customer.tags.update": "Always allow for this customer",
  "social.posts.fetch": "Always allow — any post in this installation",
  "social.draft.create": "Always allow — any post in this installation",
  "media.refine.prepare": "Always allow image edits while I'm signed in",
};

function AssistantOperationCard({ operation, installId, contextId, busy, decide, appPath }: {
  operation: Operation;
  installId: string;
  contextId: string;
  busy: boolean;
  decide: (operation: Operation, decision: "allow_once" | "allow_always" | "deny") => void;
  /** The verified canonical `/apps/<key>` path for resource links; null means no link. */
  appPath: string | null;
}) {
  const t = useTranslate();
  const request = operation.permission_request;
  const requestMatches = request !== null && request.scope.install_id === installId &&
    request.scope.context_id === contextId && request.scope.action_id === operation.action_id &&
    (!request.allow_always || Object.hasOwn(STANDING_LABELS, operation.action_id));
  const requiresTagPreview = operation.action_id === "customer.tags.update";
  // A declared record change is decided only from its authoritative typed
  // preview, which must name this request's record. Legacy tag preview and
  // Social actions keep their existing gate.
  const recordPreview = request?.recordPreview ?? null;
  const recordPreviewReady = recordPreview !== null && request !== null && recordPreview.record_id === request.scope.resource_id;
  // Only a host-declared record request needs a valid record preview. Any
  // other request keeps its legacy gate (exact tag preview or none).
  const recordKind = request?.previewKind === "record";
  const mediaKind = request?.previewKind === "media_refine";
  const mediaPreview = request?.mediaPreview ?? null;
  const mediaPreviewReady = mediaKind && mediaPreview !== null && request !== null &&
    mediaPreview.asset_id === request.scope.resource_id && mediaPreview.allow_once === true &&
    operation.action_id === "media.refine.prepare";
  const recordPreviewMissing = recordKind && !recordPreviewReady;
  const previewReady = requiresTagPreview
    ? request?.preview !== null && request?.preview !== undefined
    : mediaKind ? mediaPreviewReady : !recordKind || recordPreviewReady;
  const title = operation.action_id.replaceAll(".", " · ");
  const resultRows = resultValues(operation.result);
  return (
    <article className="app-assistant-card" data-operation-status={operation.status}>
      <div className="app-assistant-card-head">
        <strong className="text-label text-ink-100">{title}</strong>
        <span className="text-micro text-ink-400">{operation.status.replaceAll("_", " ")}</span>
      </div>
      <p className="text-label text-ink-300">{operation.summary}</p>
      {operation.status === "pending_permission" && request === null && <p className="text-micro text-fail" role="alert">{t("The permission request is unavailable or invalid. No decision is available.")}</p>}
      {operation.status === "pending_permission" && request && (
        <div className="app-assistant-permission" role="group" aria-label={t("Permission request")}>
          <p className="text-label text-warn">{request.reason}</p>
          {!requestMatches ? <p className="text-micro text-fail" role="alert">{t("The permission scope does not match this installation and action; no decision is available.")}</p> : <>
            <p className="text-micro text-ink-400">{t("Only this action on")} {request.scope.resource_id} {t("in this installation.")}</p>
            {requiresTagPreview && request.preview === null && <p className="text-micro text-fail" role="alert">{t("The required current tag-change preview is missing or invalid. No permission decision is available.")}</p>}
            {recordPreviewMissing && recordPreview === null && <p className="text-micro text-fail" role="alert">{t("The required record change preview is missing or invalid. No permission decision is available.")}</p>}
            {recordPreview !== null && (
              <div className="app-assistant-tag-preview" aria-label={t("Customer record change preview")}>
                <strong className="text-label text-ink-100">{recordPreview.record_id}</strong>
                {!recordPreviewReady && <p className="text-micro text-fail" role="alert">{t("This preview does not match the requested record. No permission decision is available.")}</p>}
                <dl>
                  {RECORD_FIELDS.filter((field) => recordPreview.before !== null && recordPreview.before[field] !== undefined || recordPreview.after[field] !== undefined).map((field) => (
                    <div key={field}>
                      <dt>{field.replaceAll("_", " ")}</dt>
                      <dd>{recordFieldText(recordPreview.before, field)} → {recordFieldText(recordPreview.after, field)}</dd>
                    </div>
                  ))}
                  <div><dt>{t("Record revision")}</dt><dd>{recordPreview.expected_revision}</dd></div>
                </dl>
              </div>
            )}
            {mediaKind && mediaPreview === null && <p className="text-micro text-fail" role="alert">{t("The required image refinement preview is missing or invalid. No permission decision is available.")}</p>}
            {mediaPreview !== null && (
              <div className="app-assistant-tag-preview" aria-label={t("Library image refinement preview")}>
                <strong className="text-label text-ink-100">{mediaPreview.title}</strong>
                {!mediaPreviewReady && <p className="text-micro text-fail" role="alert">{t("This preview does not match the requested library image. No permission decision is available.")}</p>}
                <dl>
                  <div><dt>{t("Source")}</dt><dd>{mediaPreview.asset_id}</dd></div>
                  <div><dt>{t("Revision")}</dt><dd>{mediaPreview.revision}</dd></div>
                  <div><dt>{t("Original")}</dt><dd>{mediaPreview.sha256}</dd></div>
                  <div><dt>{t("Instructions")}</dt><dd>{mediaPreview.instructions}</dd></div>
                  <div><dt>{t("Model")}</dt><dd>{mediaPreview.model}</dd></div>
                  <div><dt>{t("Estimated price")}</dt><dd>{t("about {price}").replace("{price}", formatUsdMicros(mediaPreview.quote.total_price_micros))}</dd></div>
                </dl>
              </div>
            )}
            {requiresTagPreview && request.preview !== null && (
              <div className="app-assistant-tag-preview" aria-label={t("Customer tag change preview")}>
                <strong className="text-label text-ink-100">{request.preview.customer_label}</strong>
                <dl>
                  <div><dt>{t("Before")}</dt><dd>{request.preview.before_tags.length ? request.preview.before_tags.join(", ") : t("No tags")}</dd></div>
                  <div><dt>{t("After")}</dt><dd>{request.preview.after_tags.length ? request.preview.after_tags.join(", ") : t("No tags")}</dd></div>
                  <div><dt>{t("Record revision")}</dt><dd>{request.preview.expected_revision}</dd></div>
                </dl>
              </div>
            )}
            <div className="app-assistant-controls">
              <Button size="sm" variant="primary" disabled={busy || !previewReady} onClick={() => decide(operation, "allow_once")}>{t("Allow once")}</Button>
              {request.allow_always && Object.hasOwn(STANDING_LABELS, operation.action_id) && <Button size="sm" disabled={busy || !previewReady} onClick={() => decide(operation, "allow_always")}>{t(STANDING_LABELS[operation.action_id])}</Button>}
              <Button size="sm" variant="danger" disabled={busy || !previewReady} onClick={() => decide(operation, "deny")}>{t("Deny this request")}</Button>
            </div>
          </>}
        </div>
      )}
      {operation.status === "running" && <p className="text-micro text-info" role="status">{t("In progress")}</p>}
      {operation.status === "denied" && <p className="text-micro text-ink-400">{t("No standing permission was created.")}</p>}
      {operation.status === "failed" && operation.error && <p className="text-micro text-fail" role="alert">{operation.error}</p>}
      {resultRows.length > 0 && (
        <dl className="app-assistant-result">
          {resultRows.map(([key, value]) => <div key={key}><dt>{key}</dt><dd>{value}</dd></div>)}
        </dl>
      )}
      {operation.resource_refs.length > 0 && (
        <ul className="app-assistant-resources" aria-label={t("Result resources")}>
          {operation.resource_refs.map((ref, index) => {
            const href = resourceHref(appPath, installId, contextId, ref);
            return <li key={`${ref.kind}:${ref.id}:${index}`}>{href ? <Link className="lnk text-label" href={href}>{ref.label}</Link> : <span className="text-label text-ink-300">{ref.label}</span>}</li>;
          })}
        </ul>
      )}
    </article>
  );
}

interface AssistantData {
  scopeKey: string;
  connectionRevision: number | null;
  actions: Action[];
  operations: Operation[];
  permissions: Permission[];
  error: string | null;
  /** The app's assistant descriptor is not consented yet: a calm, permanent-until-approved state. */
  consent: boolean;
}
function emptyData(scopeKey: string): AssistantData {
  return { scopeKey, connectionRevision: null, actions: [], operations: [], permissions: [], error: null, consent: false };
}

/** How a failed read settles. `none`: the app declares no assistant actions
 *  (a valid, permanent state, so no panel). `consent`: the descriptor awaits
 *  approval (a calm notice). `error`: a real failure, the error with Retry. */
export type ReadOutcome = "none" | "consent" | "error";
export function readFailure(e: unknown): ReadOutcome {
  if (e instanceof ApiError && e.status === 404) return "none";
  if (e instanceof ApiError && e.status === 409 && /not consented/i.test(e.message)) return "consent";
  return "error";
}

/** The one reader of one installation/context, as seen by the component. */
interface Poller {
  alive: () => boolean;
  /** A decision, revoke or block was confirmed: reads begun before it are dropped, and one read follows it. */
  written: () => Promise<void>;
  refresh: () => Promise<void>;
}

export default function AssistantOperations({ installId, contextId, canDecide, appPath, connectionRevision, onPendingChange }: {
  installId: string;
  contextId: string;
  canDecide: boolean;
  /** The verified canonical `/apps/<key>` path; resource links emit it. Null means no links. */
  appPath: string | null;
  /** The transcript snapshot this scope must reconcile before actions become live. */
  connectionRevision: number;
  onPendingChange: (pending: boolean, reconciledRevision: number | null) => void;
}) {
  const t = useTranslate();
  const scopeKey = JSON.stringify([installId, contextId]);
  const contextReady = installId !== "" && contextId !== "";
  const poller = useRef<Poller | null>(null);
  const connection = useRef(connectionRevision);
  connection.current = connectionRevision;
  const [data, setData] = useState<AssistantData>(() => emptyData(scopeKey));
  const [busyState, setBusyState] = useState<{ scopeKey: string; id: string | null }>({ scopeKey, id: null });
  const [expanded, setExpanded] = useState(false);
  const visible = data.scopeKey === scopeKey ? data : emptyData(scopeKey);
  const busy = busyState.scopeKey === scopeKey ? busyState.id : null;
  // Current activity stays inline; completed succeeded/denied receipts sit
  // behind an explicit count-labelled history disclosure, all reachable.
  const activeOperations = visible.operations.filter(isActiveOperation);
  const completedOperations = visible.operations.filter((operation) => !isActiveOperation(operation));
  const pendingDecision = activeOperations.some((operation) => operation.status === "pending_permission");
  const reconciledRevision = contextReady ? visible.connectionRevision : connectionRevision;
  useEffect(() => {
    onPendingChange(pendingDecision, reconciledRevision);
  }, [pendingDecision, reconciledRevision, onPendingChange]);

  useEffect(() => {
    // An unresolved context owns no poller and sends no assistant requests.
    if (!contextReady) {
      setData(emptyData(scopeKey));
      return;
    }
    // One effect run owns one scope. A scope change or unmount ends it, so
    // its in-flight read can never render into the new scope.
    let alive = true;
    let inFlight: Promise<void> | null = null;
    // Set when the in-flight read may predate a write, or was dropped by one.
    let requeue = false;
    // Bumped by every confirmed write; a read that began earlier is stale.
    let epoch = 0;

    const readOnce = async (): Promise<void> => {
      const startedAt = epoch;
      const startedConnection = connection.current;
      try {
        // Wait for every sibling to settle: a failed read must not release the batch while others are in flight.
        const [actions, operations, permissions] = await Promise.allSettled([
          api.assistantActions(installId, contextId),
          api.assistantOperations(installId, contextId),
          api.assistantPermissions(installId, contextId),
        ]);
        if (!alive) return;
        if (epoch !== startedAt) { requeue = true; return; }
        if (actions.status === "rejected") throw actions.reason;
        if (operations.status === "rejected") throw operations.reason;
        if (permissions.status === "rejected") throw permissions.reason;
        const actionResponse = actions.value;
        const operationResponse = operations.value;
        const permissionResponse = permissions.value;
        if (!Array.isArray(actionResponse.actions) || !Array.isArray(operationResponse.operations) || !Array.isArray(permissionResponse.permissions)) {
          throw new Error("The assistant service returned an invalid response.");
        }
        setData({
          scopeKey,
          connectionRevision: startedConnection,
          actions: actionResponse.actions.map(parseAction).filter((v): v is Action => v !== null),
          operations: operationResponse.operations.map((row) => parseOperation(record(row)?.operation ?? row)).filter((v): v is Operation => v !== null),
          permissions: permissionResponse.permissions.map((row) => parsePermission(record(row)?.permission ?? row)).filter((v): v is Permission => v !== null),
          error: null,
          consent: false,
        });
      } catch (e) {
        if (!alive) return;
        if (epoch !== startedAt) { requeue = true; return; }
        const outcome = readFailure(e);
        if (outcome === "none") {
          setData({ ...emptyData(scopeKey), connectionRevision: startedConnection });
        } else if (outcome === "consent") {
          setData({ ...emptyData(scopeKey), connectionRevision: startedConnection, consent: true });
        } else {
          setData((previous) => ({
            ...(previous.scopeKey === scopeKey ? previous : emptyData(scopeKey)),
            error: "Assistant activity could not be loaded. Please retry.",
          }));
        }
      }
    };

    const run = async (): Promise<void> => {
      do {
        requeue = false;
        await readOnce();
      } while (requeue && alive);
      // Synchronous from here on: a request made after this starts a new read.
      inFlight = null;
    };
    const start = (): Promise<void> => {
      const pending = run();
      inFlight = pending;
      return pending;
    };
    // Routine reads join the one in flight: at most one batch per scope.
    const poll = (): Promise<void> => inFlight ?? start();
    const written = (): Promise<void> => {
      epoch += 1;
      if (inFlight) {
        requeue = true;
        return inFlight;
      }
      return start();
    };
    // Visibility resumes and `master` frames re-read; a hidden tab reads when shown.
    // A frame invalidates reads begun before it (epoch + requeue), so a
    // pre-change batch can never swallow the notification.
    const resumeRead = () => {
      if (alive && !document.hidden) void written();
    };

    const handle: Poller = { alive: () => alive, written, refresh: poll };
    // No context yet: nothing to ask for (an empty context ID is a 400).
    if (contextId === "") return;
    poller.current = handle;
    document.addEventListener("visibilitychange", resumeRead);
    const unsubscribeMaster = subscribeMasterChanged(resumeRead);
    return () => {
      alive = false;
      document.removeEventListener("visibilitychange", resumeRead);
      unsubscribeMaster();
      if (poller.current === handle) poller.current = null;
    };
  }, [installId, contextId, scopeKey, contextReady]);
  useEffect(() => {
    // Reconcile a new transcript snapshot through the existing write fence.
    // Keep the scope's handle alive so in-flight decisions still settle.
    void poller.current?.written();
  }, [scopeKey, connectionRevision]);

  const runWrite = async (busyId: string, write: () => Promise<unknown>, failure: string) => {
    const handle = poller.current;
    if (!handle) return;
    const expectedScope = scopeKey;
    setBusyState({ scopeKey: expectedScope, id: busyId });
    try {
      await write();
      // The write is confirmed: the list must be read again after it, not from an older read.
      if (handle.alive()) await handle.written();
    } catch {
      if (handle.alive()) setData((previous) => ({ ...(previous.scopeKey === expectedScope ? previous : emptyData(expectedScope)), error: failure }));
    } finally {
      if (handle.alive()) setBusyState({ scopeKey: expectedScope, id: null });
    }
  };
  const decide = (operation: Operation, decision: "allow_once" | "allow_always" | "deny") =>
    runWrite(operation.id, () => api.assistantDecision(installId, operation.id, { decision, expected_revision: operation.revision }),
      "The permission decision could not be saved. Reload and try again.");
  const revoke = (permission: Permission) =>
    runWrite(permission.id, () => api.assistantRevoke(installId, permission.id, { expected_revision: permission.revision }),
      "The permission could not be revoked. Reload and try again.");
  const block = (permission: Permission) =>
    runWrite(`block:${permission.id}`, () => api.assistantBlock(installId, contextId, { action_id: permission.action_id, resource_id: permission.resource_id }),
      "The action could not be blocked. Reload and try again.");

  if (!visible.actions.length && !visible.operations.length && !visible.permissions.length && !visible.error && !visible.consent) return null;
  return (
    <section className="app-assistant-activity" aria-label={t("Assistant activity")}>
      <div className="app-assistant-section-head">
        <h2 className="text-label text-ink-100">{t("Assistant activity")}</h2>
        <button type="button" className="lnk text-micro" aria-expanded={expanded} onClick={() => setExpanded((v) => !v)}>
          {expanded ? t("Hide permissions and actions") : t("Actions & permissions")}
        </button>
      </div>
      {visible.consent && <p className="text-micro text-ink-400" role="status">{t("Approve the app to enable assistant actions.")}</p>}
      {visible.error && <p className="text-micro text-fail" role="alert">{visible.error} <button className="lnk" type="button" onClick={() => void poller.current?.refresh()}>{t("Retry")}</button></p>}
      {activeOperations.map((operation) => <AssistantOperationCard key={operation.id} operation={operation} installId={installId} contextId={contextId} appPath={appPath} busy={busy === operation.id || !canDecide} decide={decide} />)}
      {completedOperations.length > 0 && (
        <details className="app-assistant-history">
          <summary className="text-micro">{t("Completed activity")} ({completedOperations.length})</summary>
          <div className="app-assistant-history-list">
            {completedOperations.map((operation) => <AssistantOperationCard key={operation.id} operation={operation} installId={installId} contextId={contextId} appPath={appPath} busy={busy === operation.id || !canDecide} decide={decide} />)}
          </div>
        </details>
      )}
      {expanded && (
        <div className="app-assistant-settings">
          <h3 className="text-micro text-ink-300">{t("Action discovery")}</h3>
          {visible.actions.length ? <ul>{visible.actions.map((action) => {
            const availability = availabilityLabel(action.availability);
            return <li key={action.id} data-availability={availability.available ? "available" : "unavailable"}>
              <strong>{t(availability.label)}: {action.id}</strong>
              {availability.available ? <><span>{action.description}</span><small>{action.effect} · {action.confirmation}</small></> : availability.detail && <small>{availability.detail}</small>}
            </li>;
          })}</ul> : <p className="text-micro text-ink-500">{t("No actions are available in this context.")}</p>}
          <h3 className="text-micro text-ink-300">{t("Permission settings")}</h3>
          {visible.permissions.length ? <ul>{visible.permissions.map((permission) => (
            <li key={permission.id} data-permission-state={permission.state}>
              <span><strong>{permission.effect === "deny" ? t("Blocked") : t("Allowed")}: {permission.action_id}</strong><small>{permission.scope_label} · {t("customer/resource")} {permission.resource_id}</small></span>
              {canDecide && permission.state === "active" && <div className="app-assistant-controls">
                {permission.effect === "allow" && <Button size="sm" variant="danger" disabled={busy !== null} onClick={() => void block(permission)}>{t("Block this action")}</Button>}
                <Button size="sm" disabled={busy !== null} onClick={() => void revoke(permission)}>{permission.effect === "deny" ? t("Unblock") : t("Revoke")}</Button>
              </div>}
            </li>
          ))}</ul> : <p className="text-micro text-ink-500">{t("No standing permissions.")}</p>}
          {!canDecide && <p className="text-micro text-ink-500">{t("Permission decisions are available to the signed-in operator.")}</p>}
        </div>
      )}
    </section>
  );
}
