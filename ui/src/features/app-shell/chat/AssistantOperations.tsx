import { useEffect, useRef, useState } from "react";
import { api, ApiError } from "../../../lib/api";
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
interface PermissionRequest {
  reason: string;
  scope: { install_id: string; context_id: string; action_id: string; resource_id: string };
  allow_always: boolean;
  preview: TagPreview | null;
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
function parsePermissionRequest(value: unknown): PermissionRequest | null {
  const v = record(value);
  const scope = record(v?.scope);
  if (!v || !scope || typeof v.allow_always !== "boolean") return null;
  const fields = [scope.install_id, scope.context_id, scope.action_id, scope.resource_id];
  if (!fields.every((field) => typeof field === "string" && field.length > 0 && field.length <= 128)) return null;
  const reason = text(v.reason, 300);
  if (reason === null) return null;
  return {
    reason,
    scope: {
      install_id: scope.install_id as string,
      context_id: scope.context_id as string,
      action_id: scope.action_id as string,
      resource_id: scope.resource_id as string,
    },
    allow_always: v.allow_always,
    preview: parseTagPreview(v.preview),
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

function AssistantOperationCard({ operation, installId, contextId, busy, decide }: {
  operation: Operation;
  installId: string;
  contextId: string;
  busy: boolean;
  decide: (operation: Operation, decision: "allow_once" | "allow_always" | "deny") => void;
}) {
  const request = operation.permission_request;
  const requestMatches = request !== null && request.scope.install_id === installId &&
    request.scope.context_id === contextId && request.scope.action_id === operation.action_id &&
    (!request.allow_always || operation.action_id === "customer.tags.update");
  const requiresTagPreview = operation.action_id === "customer.tags.update";
  const previewReady = !requiresTagPreview || request?.preview !== null && request?.preview !== undefined;
  const title = operation.action_id.replaceAll(".", " · ");
  const resultRows = resultValues(operation.result);
  return (
    <article className="app-assistant-card" data-operation-status={operation.status}>
      <div className="app-assistant-card-head">
        <strong className="text-label text-ink-100">{title}</strong>
        <span className="text-micro text-ink-400">{operation.status.replaceAll("_", " ")}</span>
      </div>
      <p className="text-label text-ink-300">{operation.summary}</p>
      {operation.status === "pending_permission" && request === null && <p className="text-micro text-fail" role="alert">The permission request is unavailable or invalid. No decision is available.</p>}
      {operation.status === "pending_permission" && request && (
        <div className="app-assistant-permission" role="group" aria-label="Permission request">
          <p className="text-label text-warn">{request.reason}</p>
          {!requestMatches ? <p className="text-micro text-fail" role="alert">The permission scope does not match this installation and action; no decision is available.</p> : <>
            <p className="text-micro text-ink-400">Only this action on {request.scope.resource_id} in this installation.</p>
            {requiresTagPreview && request.preview === null && <p className="text-micro text-fail" role="alert">The required current tag-change preview is missing or invalid. No permission decision is available.</p>}
            {requiresTagPreview && request.preview !== null && (
              <div className="app-assistant-tag-preview" aria-label="Customer tag change preview">
                <strong className="text-label text-ink-100">{request.preview.customer_label}</strong>
                <dl>
                  <div><dt>Before</dt><dd>{request.preview.before_tags.length ? request.preview.before_tags.join(", ") : "No tags"}</dd></div>
                  <div><dt>After</dt><dd>{request.preview.after_tags.length ? request.preview.after_tags.join(", ") : "No tags"}</dd></div>
                  <div><dt>Record revision</dt><dd>{request.preview.expected_revision}</dd></div>
                </dl>
              </div>
            )}
            <div className="app-assistant-controls">
              <Button size="sm" variant="primary" disabled={busy || !previewReady} onClick={() => decide(operation, "allow_once")}>Allow once</Button>
              {request.allow_always && <Button size="sm" disabled={busy || !previewReady} onClick={() => decide(operation, "allow_always")}>Always allow for this customer</Button>}
              <Button size="sm" variant="danger" disabled={busy || !previewReady} onClick={() => decide(operation, "deny")}>Deny this request</Button>
            </div>
          </>}
        </div>
      )}
      {operation.status === "running" && <p className="text-micro text-info" role="status">In progress</p>}
      {operation.status === "denied" && <p className="text-micro text-ink-400">No standing permission was created.</p>}
      {operation.status === "failed" && operation.error && <p className="text-micro text-fail" role="alert">{operation.error}</p>}
      {resultRows.length > 0 && (
        <dl className="app-assistant-result">
          {resultRows.map(([key, value]) => <div key={key}><dt>{key}</dt><dd>{value}</dd></div>)}
        </dl>
      )}
      {operation.resource_refs.length > 0 && (
        <ul className="app-assistant-resources" aria-label="Result resources">
          {operation.resource_refs.map((ref, index) => {
            const href = resourceHref(installId, contextId, ref);
            return <li key={`${ref.kind}:${ref.id}:${index}`}>{href ? <Link className="lnk text-label" href={href}>{ref.label}</Link> : <span className="text-label text-ink-300">{ref.label}</span>}</li>;
          })}
        </ul>
      )}
    </article>
  );
}

interface AssistantData {
  scopeKey: string;
  actions: Action[];
  operations: Operation[];
  permissions: Permission[];
  error: string | null;
}
function emptyData(scopeKey: string): AssistantData {
  return { scopeKey, actions: [], operations: [], permissions: [], error: null };
}

/** Routine refresh cadence, counted from the end of the previous read. */
const POLL_MS = 8_000;

/** The one reader of one installation/context, as seen by the component. */
interface Poller {
  alive: () => boolean;
  /** A decision, revoke or block was confirmed: reads begun before it are dropped, and one read follows it. */
  written: () => Promise<void>;
  refresh: () => Promise<void>;
}

export default function AssistantOperations({ installId, contextId, canDecide }: {
  installId: string;
  contextId: string;
  canDecide: boolean;
}) {
  const scopeKey = JSON.stringify([installId, contextId]);
  const poller = useRef<Poller | null>(null);
  const [data, setData] = useState<AssistantData>(() => emptyData(scopeKey));
  const [busyState, setBusyState] = useState<{ scopeKey: string; id: string | null }>({ scopeKey, id: null });
  const [expanded, setExpanded] = useState(false);
  const visible = data.scopeKey === scopeKey ? data : emptyData(scopeKey);
  const busy = busyState.scopeKey === scopeKey ? busyState.id : null;

  useEffect(() => {
    // One effect run owns one scope. A scope change or unmount ends it, so
    // its in-flight read can never render into the new scope.
    let alive = true;
    let inFlight: Promise<void> | null = null;
    // Set when the in-flight read may predate a write, or was dropped by one.
    let requeue = false;
    // Bumped by every confirmed write; a read that began earlier is stale.
    let epoch = 0;
    let timer: number | null = null;
    const stopTimer = () => {
      if (timer !== null) window.clearTimeout(timer);
      timer = null;
    };

    const readOnce = async (): Promise<void> => {
      const startedAt = epoch;
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
          actions: actionResponse.actions.map(parseAction).filter((v): v is Action => v !== null),
          operations: operationResponse.operations.map((row) => parseOperation(record(row)?.operation ?? row)).filter((v): v is Operation => v !== null),
          permissions: permissionResponse.permissions.map((row) => parsePermission(record(row)?.permission ?? row)).filter((v): v is Permission => v !== null),
          error: null,
        });
      } catch (e) {
        if (!alive) return;
        if (epoch !== startedAt) { requeue = true; return; }
        if (e instanceof ApiError && e.status === 404) {
          setData(emptyData(scopeKey));
        } else {
          setData((previous) => ({
            ...(previous.scopeKey === scopeKey ? previous : emptyData(scopeKey)),
            error: "Assistant activity could not be loaded. Please retry.",
          }));
        }
      }
    };

    const schedule = () => {
      stopTimer();
      // Hidden tabs do not poll; the visibility listener resumes the reads.
      if (document.hidden) return;
      timer = window.setTimeout(() => {
        timer = null;
        if (alive && !document.hidden) void poll();
      }, POLL_MS);
    };
    const run = async (): Promise<void> => {
      do {
        requeue = false;
        await readOnce();
      } while (requeue && alive);
      // Synchronous from here on: a request made after this starts a new read.
      inFlight = null;
      if (alive) schedule();
    };
    const start = (): Promise<void> => {
      stopTimer();
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
    const onVisibility = () => {
      if (!alive) return;
      if (document.hidden) stopTimer();
      else void poll();
    };

    const handle: Poller = { alive: () => alive, written, refresh: poll };
    poller.current = handle;
    document.addEventListener("visibilitychange", onVisibility);
    void poll();
    return () => {
      alive = false;
      stopTimer();
      document.removeEventListener("visibilitychange", onVisibility);
      if (poller.current === handle) poller.current = null;
    };
  }, [installId, contextId, scopeKey]);

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

  if (!visible.actions.length && !visible.operations.length && !visible.permissions.length && !visible.error) return null;
  return (
    <section className="app-assistant-activity" aria-label="Assistant activity">
      <div className="app-assistant-section-head">
        <h2 className="text-label text-ink-100">Assistant activity</h2>
        <button type="button" className="lnk text-micro" aria-expanded={expanded} onClick={() => setExpanded((v) => !v)}>
          {expanded ? "Hide permissions and actions" : "Actions & permissions"}
        </button>
      </div>
      {visible.error && <p className="text-micro text-fail" role="alert">{visible.error} <button className="lnk" type="button" onClick={() => void poller.current?.refresh()}>Retry</button></p>}
      {visible.operations.map((operation) => <AssistantOperationCard key={operation.id} operation={operation} installId={installId} contextId={contextId} busy={busy === operation.id || !canDecide} decide={decide} />)}
      {expanded && (
        <div className="app-assistant-settings">
          <h3 className="text-micro text-ink-300">Action discovery</h3>
          {visible.actions.length ? <ul>{visible.actions.map((action) => {
            const availability = availabilityLabel(action.availability);
            return <li key={action.id} data-availability={availability.available ? "available" : "unavailable"}>
              <strong>{availability.label}: {action.id}</strong>
              {availability.available ? <><span>{action.description}</span><small>{action.effect} · {action.confirmation}</small></> : availability.detail && <small>{availability.detail}</small>}
            </li>;
          })}</ul> : <p className="text-micro text-ink-500">No actions are available in this context.</p>}
          <h3 className="text-micro text-ink-300">Permission settings</h3>
          {visible.permissions.length ? <ul>{visible.permissions.map((permission) => (
            <li key={permission.id} data-permission-state={permission.state}>
              <span><strong>{permission.effect === "deny" ? "Blocked" : "Allowed"}: {permission.action_id}</strong><small>{permission.scope_label} · customer/resource {permission.resource_id}</small></span>
              {canDecide && permission.state === "active" && <div className="app-assistant-controls">
                {permission.effect === "allow" && <Button size="sm" variant="danger" disabled={busy !== null} onClick={() => void block(permission)}>Block this action</Button>}
                <Button size="sm" disabled={busy !== null} onClick={() => void revoke(permission)}>{permission.effect === "deny" ? "Unblock" : "Revoke"}</Button>
              </div>}
            </li>
          ))}</ul> : <p className="text-micro text-ink-500">No standing permissions.</p>}
          {!canDecide && <p className="text-micro text-ink-500">Permission decisions are available to the signed-in operator.</p>}
        </div>
      )}
    </section>
  );
}
