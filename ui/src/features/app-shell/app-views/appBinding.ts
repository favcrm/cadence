import type { AppViewDescriptor, AppViewFormat, AppViewView } from "./contract";

export const APP_BINDING_CONTRACT = "app-bindings/v1" as const;

export interface AppViewFieldBinding {
  field: string;
  key: string;
  format: AppViewFormat;
}

export interface AppViewBinding {
  view: string;
  source: "customers" | "caption-runs";
  ops: ("list" | "show")[];
  fields: AppViewFieldBinding[];
}

export interface AppBindingDescriptor {
  contract: typeof APP_BINDING_CONTRACT;
  app: string;
  title: string;
  bindings: AppViewBinding[];
}

const MAX_BYTES = 16 * 1024;
const MAX_NODES = 1024;
const IDENT = /^[a-z][a-z0-9_-]{0,63}$/;
const KEY_PATH = /^[a-z0-9_-]+(?:\.[a-z0-9_-]+)*$/;
const FORMATS: readonly AppViewFormat[] = ["text", "number", "date", "datetime", "enum", "tags"];
const RUN_STATES = ["awaiting_approval", "approved", "running", "succeeded", "failed", "cancelled"];
const CONSENT = ["granted", "denied", "unknown"];
const FORBIDDEN = new Set([
  "__proto__", "prototype", "constructor", "script", "scripts", "code", "html", "innerHTML",
  "css", "style", "javascript", "eval", "import", "module", "url", "uri", "href", "src",
  "link", "action", "endpoint", "install_id", "installId", "context_id", "contextId", "workspace",
  "workspace_id", "project", "project_id", "project_link", "actor", "by", "role", "grant", "scope",
  "scopes", "capability", "capabilities", "secret", "secrets", "credential", "credentials", "token",
  "password", "sql", "query", "path", "file", "effect", "effects", "verified", "digest", "revision",
  "method", "call", "rpc", "tool", "command", "exec", "args", "arguments", "binding", "connection",
  "account", "slot", "request", "request_id", "fetch", "body", "params", "where", "order_by", "limit",
  "cursor", "write", "update", "delete", "send", "approve", "dispatch", "run_id", "artifact",
  "artifacts", "snapshot", "record", "store", "migration",
]);

function fail(path: string, message: string): never {
  throw new Error(`${path}: ${message}`);
}

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value)
    && (Object.getPrototypeOf(value) === Object.prototype || Object.getPrototypeOf(value) === null);
}

function scan(value: unknown, path: string, budget: { nodes: number }, depth = 0): void {
  if (depth > 24) fail(path, "input is nested too deeply");
  if (++budget.nodes > MAX_NODES) fail(path, "input has too many nodes");
  if (value === null || typeof value === "boolean" || typeof value === "string") return;
  if (typeof value === "number" && Number.isFinite(value)) return;
  if (Array.isArray(value)) {
    value.forEach((child, index) => scan(child, `${path}.${index}`, budget, depth + 1));
    return;
  }
  if (!isObject(value)) fail(path, "expected plain JSON data");
  for (const [key, descriptor] of Object.entries(Object.getOwnPropertyDescriptors(value))) {
    if (FORBIDDEN.has(key)) fail(path, `forbidden binding key: ${key}`);
    if (descriptor.get || descriptor.set) fail(path, "accessors are not JSON data");
    scan(descriptor.value, `${path}.${key}`, budget, depth + 1);
  }
}

function bounded(raw: unknown): void {
  scan(raw, "$", { nodes: 0 });
  if (new TextEncoder().encode(JSON.stringify(raw)).byteLength > MAX_BYTES) {
    fail("$", `input exceeds ${MAX_BYTES} bytes`);
  }
}

function text(value: unknown, path: string, max: number): string {
  if (typeof value !== "string" || value.length === 0 || value.length > max
      || /[\u0000-\u001f\u007f\u2028\u2029]/.test(value)) {
    return fail(path, `expected a bounded non-empty string (max ${max})`);
  }
  return value;
}

function ident(value: unknown, path: string): string {
  const valueText = text(value, path, 64);
  if (!IDENT.test(valueText)) fail(path, "unsafe identifier");
  return valueText;
}

function objectWithKeys(value: unknown, allowed: readonly string[], path: string): Record<string, unknown> {
  if (!isObject(value)) fail(path, "expected an object");
  for (const key of Object.keys(value)) {
    if (!allowed.includes(key)) fail(path, `unknown key: ${key}`);
  }
  return value;
}

const SOURCE_KEYS: Record<string, Record<string, { format: AppViewFormat; kind: "scalar" | "list"; values?: readonly string[] }>> = {
  customers: {
    record_id: { format: "text", kind: "scalar" },
    display_name: { format: "text", kind: "scalar" },
    email: { format: "text", kind: "scalar" },
    phone: { format: "text", kind: "scalar" },
    tags: { format: "tags", kind: "list" },
    source: { format: "text", kind: "scalar" },
    "consent.email": { format: "enum", kind: "scalar", values: CONSENT },
    "consent.sms": { format: "enum", kind: "scalar", values: CONSENT },
  },
  "caption-runs": {
    id: { format: "text", kind: "scalar" },
    state: { format: "enum", kind: "scalar", values: RUN_STATES },
    context_id: { format: "text", kind: "scalar" },
    snapshot_digest: { format: "text", kind: "scalar" },
    "snapshot.workflow.title": { format: "text", kind: "scalar" },
    "snapshot.inputs.subject": { format: "text", kind: "scalar" },
    "snapshot.context.id": { format: "text", kind: "scalar" },
  },
};

/** Parse the bounded companion contract, then prove every view/field/type
 *  reference against the descriptor from the same installed receipt. */
export function parseAppBinding(raw: unknown, descriptor: AppViewDescriptor): AppBindingDescriptor {
  bounded(raw);
  const root = objectWithKeys(raw, ["contract", "app", "title", "bindings"], "$");
  if (root.contract !== APP_BINDING_CONTRACT) fail("$.contract", `expected ${APP_BINDING_CONTRACT}`);
  const app = ident(root.app, "$.app");
  const title = text(root.title, "$.title", 120);
  if (app !== descriptor.app) fail("$.app", "binding and descriptor app identities differ");
  if (!Array.isArray(root.bindings) || root.bindings.length === 0 || root.bindings.length > 16) {
    fail("$.bindings", "expected 1–16 view bindings");
  }
  const views = new Set<string>();
  const bindings = root.bindings.map((rawBinding, index): AppViewBinding => {
    const path = `$.bindings.${index}`;
    const item = objectWithKeys(rawBinding, ["view", "source", "ops", "fields"], path);
    const viewId = ident(item.view, `${path}.view`);
    if (views.has(viewId)) fail(`${path}.view`, `duplicate binding for ${viewId}`);
    views.add(viewId);
    const view = descriptor.views.find((candidate) => candidate.id === viewId);
    if (!view || view.kind === "form") fail(`${path}.view`, "binding must name an installed table or detail view");
    if (item.source !== "customers" && item.source !== "caption-runs") fail(`${path}.source`, "unsupported source");
    const source = item.source;
    const expectedOp = view.kind === "table" ? "list" : "show";
    const rawOps = item.ops;
    if (!Array.isArray(rawOps) || rawOps.length === 0 || rawOps.length > 2) {
      fail(`${path}.ops`, "expected one or two read ops");
    }
    const ops = rawOps.map((op, opIndex) => {
      if (op !== expectedOp) fail(`${path}.ops.${opIndex}`, `a ${view.kind} view admits only ${expectedOp}`);
      if (rawOps.indexOf(op) !== opIndex) fail(`${path}.ops.${opIndex}`, `duplicate op: ${op}`);
      return op as "list" | "show";
    });
    if (!Array.isArray(item.fields) || item.fields.length === 0 || item.fields.length > 24) {
      fail(`${path}.fields`, "expected 1–24 mapped fields");
    }
    const declared = new Map((view.fields ?? []).map((field) => [field.id, field]));
    const seenFields = new Set<string>();
    const fields = item.fields.map((rawField, fieldIndex): AppViewFieldBinding => {
      const fieldPath = `${path}.fields.${fieldIndex}`;
      const mapping = objectWithKeys(rawField, ["field", "key", "format"], fieldPath);
      const fieldId = ident(mapping.field, `${fieldPath}.field`);
      if (seenFields.has(fieldId)) fail(`${fieldPath}.field`, `duplicate field mapping: ${fieldId}`);
      seenFields.add(fieldId);
      const key = text(mapping.key, `${fieldPath}.key`, 64);
      if (!KEY_PATH.test(key)) fail(`${fieldPath}.key`, "unsafe source key path");
      const produced = SOURCE_KEYS[source][key];
      if (!produced) fail(`${fieldPath}.key`, `source ${source} has no projection key ${key}`);
      if (typeof mapping.format !== "string" || !FORMATS.includes(mapping.format as AppViewFormat)) {
        fail(`${fieldPath}.format`, "unsupported display format");
      }
      const format = mapping.format as AppViewFormat;
      const descriptorField = declared.get(fieldId);
      if (!descriptorField) fail(`${fieldPath}.field`, `undeclared descriptor field: ${fieldId}`);
      if (descriptorField.format !== format || produced.format !== format || descriptorField.kind !== produced.kind) {
        fail(fieldPath, "binding format/kind does not match the descriptor and source projection");
      }
      if (produced.values && JSON.stringify(descriptorField.values ?? []) !== JSON.stringify(produced.values)) {
        fail(fieldPath, "descriptor enum values differ from the source domain");
      }
      return { field: fieldId, key, format };
    });
    return { view: viewId, source, ops, fields };
  });
  return { contract: APP_BINDING_CONTRACT, app, title, bindings };
}

export function identitySourceKey(source: AppViewBinding["source"]): "record_id" | "id" {
  return source === "customers" ? "record_id" : "id";
}

export function isSafeRecordId(value: unknown): value is string {
  return typeof value === "string" && /^[A-Za-z0-9_-]{1,128}$/.test(value);
}

export function viewBindingFor(descriptor: AppViewDescriptor, binding: AppBindingDescriptor, view: AppViewView): AppViewBinding | null {
  if (!descriptor.views.some((candidate) => candidate.id === view.id && candidate.kind === view.kind)) return null;
  return binding.bindings.find((candidate) => candidate.view === view.id) ?? null;
}
