import type { AppBindingDescriptor, AppViewBinding } from "./appBinding";
import type { AppViewDescriptor, AppViewFormat, AppViewView } from "./contract";

export const APP_ACTIONS_V2_CONTRACT = "app-actions/v2" as const;
const MAX_ACTIONS = 16;
const MAX_FIELDS = 64;
const MAX_ENUM_VALUES = 24;
const MAX_BYTES = 64 * 1024;
const MAX_NODES = 4096;
const IDENT = /^[a-z][a-z0-9_-]{0,63}$/;
const ACTION_ID = /^[a-z][a-z0-9_-]{0,63}(?:\.[a-z][a-z0-9_-]{0,63}){0,3}$/;
const FORMATS: readonly AppViewFormat[] = ["text", "number", "date", "datetime", "enum", "tags"];
const FORBIDDEN = new Set([
  "__proto__", "prototype", "constructor", "script", "scripts", "code", "html", "innerHTML",
  "css", "style", "javascript", "eval", "import", "module", "url", "uri", "href", "src",
  "link", "endpoint", "method", "route", "install_id", "installId", "context_id", "contextId",
  "workspace", "workspace_id", "project", "project_id", "project_link", "actor", "by", "role",
  "grant", "scope", "scopes", "capability", "capabilities", "caller", "secret", "secrets",
  "credential", "credentials", "token", "password", "sql", "query", "path", "file", "effect",
  "effects", "verified", "digest", "revision", "revision_pin", "approval_pin", "actor_request",
  "guard", "public", "unauthenticated", "skip_approval", "default", "pattern", "regex",
  "expression", "formula", "schema", "properties", "items", "send", "consent", "consent_email",
  "consent_sms", "record_id", "recordId", "expected_revision", "expectedRevision",
]);
const CUSTOMER_FIELDS = [
  { id: "display_name", type: "text", required: true, nullable: false },
  { id: "email", type: "text", required: false, nullable: true },
  { id: "phone", type: "text", required: false, nullable: true },
  { id: "source", type: "text", required: false, nullable: true },
  { id: "tags", type: "tags", required: false, nullable: false },
] as const;

export interface AppActionFieldV2 {
  id: string;
  label: string;
  type: AppViewFormat;
  required: boolean;
  nullable: boolean;
  values?: string[];
  maxLength?: number;
  maxItems?: number;
}

export interface AppActionV2 {
  id: string;
  title: string;
  operation: "record.create" | "record.update";
  record: string;
  form_view: string;
  input: { fields: AppActionFieldV2[] };
}

export interface AppActionDescriptorV2 {
  contract: typeof APP_ACTIONS_V2_CONTRACT;
  app: string;
  title: string;
  summary?: string;
  actions: AppActionV2[];
}

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
    if (FORBIDDEN.has(key)) fail(path, `forbidden descriptor key: ${key}`);
    if (descriptor.get || descriptor.set) fail(path, "accessors are not JSON data");
    scan(descriptor.value, `${path}.${key}`, budget, depth + 1);
  }
}

function object(value: unknown, allowed: readonly string[], path: string): Record<string, unknown> {
  if (!isObject(value)) fail(path, "expected an object");
  for (const key of Object.keys(value)) {
    if (!allowed.includes(key)) fail(path, `unknown key: ${key}`);
  }
  return value;
}

function text(value: unknown, path: string, max: number): string {
  if (typeof value !== "string" || value.length === 0 || value.length > max
      || /[\u0000-\u001f\u007f\u2028\u2029]/.test(value)) {
    return fail(path, `expected a bounded non-empty string (max ${max})`);
  }
  return value;
}

function identifier(value: unknown, path: string): string {
  const result = text(value, path, 64);
  if (!IDENT.test(result)) fail(path, "unsafe identifier");
  return result;
}

function parseField(raw: unknown, path: string): AppActionFieldV2 {
  const field = object(raw, ["id", "label", "type", "required", "nullable", "values", "maxLength", "maxItems"], path);
  const id = identifier(field.id, `${path}.id`);
  const label = text(field.label, `${path}.label`, 80);
  const type = field.type === undefined ? "text" : field.type;
  if (typeof type !== "string" || !FORMATS.includes(type as AppViewFormat)) {
    fail(`${path}.type`, "unsupported action input type");
  }
  const required = field.required === undefined ? false : field.required;
  const nullable = field.nullable === undefined ? false : field.nullable;
  if (typeof required !== "boolean") fail(`${path}.required`, "expected a boolean");
  if (typeof nullable !== "boolean") fail(`${path}.nullable`, "expected a boolean");
  if (nullable && (required || type !== "text")) {
    fail(`${path}.nullable`, "nullable is only allowed on optional text fields");
  }
  let values: string[] | undefined;
  if (field.values !== undefined) {
    if (type !== "enum" || !Array.isArray(field.values) || field.values.length === 0 || field.values.length > MAX_ENUM_VALUES) {
      fail(`${path}.values`, "enum values are only allowed as a bounded non-empty enum list");
    }
    values = field.values.map((value, index) => text(value, `${path}.values.${index}`, 80));
  }
  if (type === "enum" && values === undefined) fail(`${path}.type`, "enum requires values");
  const maxLength = field.maxLength;
  if (maxLength !== undefined && (!Number.isSafeInteger(maxLength) || (maxLength as number) < 1 || (maxLength as number) > 4096
      || (type !== "text" && type !== "tags"))) {
    fail(`${path}.maxLength`, "maxLength is supported only on text/tags and must be 1–4096");
  }
  const maxItems = field.maxItems;
  if (maxItems !== undefined && (!Number.isSafeInteger(maxItems) || (maxItems as number) < 1 || (maxItems as number) > 64 || type !== "tags")) {
    fail(`${path}.maxItems`, "maxItems is supported only on tags and must be 1–64");
  }
  return {
    id,
    label,
    type: type as AppViewFormat,
    required,
    nullable,
    ...(values === undefined ? {} : { values }),
    ...(maxLength === undefined ? {} : { maxLength: maxLength as number }),
    ...(maxItems === undefined ? {} : { maxItems: maxItems as number }),
  };
}

function parseAction(raw: unknown, index: number): AppActionV2 {
  const path = `$.actions.${index}`;
  const action = object(raw, ["id", "title", "operation", "record", "form_view", "input"], path);
  const id = text(action.id, `${path}.id`, 259);
  if (!ACTION_ID.test(id)) fail(`${path}.id`, "unsafe dotted action id");
  const title = text(action.title, `${path}.title`, 120);
  if (action.operation !== "record.create" && action.operation !== "record.update") {
    fail(`${path}.operation`, "unsupported action operation");
  }
  const record = identifier(action.record, `${path}.record`);
  const form_view = identifier(action.form_view, `${path}.form_view`);
  const input = object(action.input, ["fields"], `${path}.input`);
  if (!Array.isArray(input.fields) || input.fields.length === 0 || input.fields.length > MAX_FIELDS) {
    fail(`${path}.input.fields`, `expected 1–${MAX_FIELDS} fields`);
  }
  const fields = input.fields.map((field, fieldIndex) => parseField(field, `${path}.input.fields.${fieldIndex}`));
  const ids = new Set<string>();
  for (const field of fields) {
    if (ids.has(field.id)) fail(`${path}.input.fields`, `duplicate field id: ${field.id}`);
    ids.add(field.id);
  }
  return { id, title, operation: action.operation, record, form_view, input: { fields } };
}

function previewShape(field: AppActionFieldV2): { format: AppViewFormat; kind: "scalar" | "list" } {
  return field.type === "tags" ? { format: "tags", kind: "list" } : { format: field.type, kind: "scalar" };
}

function customerBindingPair(views: AppViewDescriptor, binding: AppBindingDescriptor): {
  table: AppViewView;
  tableBinding: AppViewBinding;
  detail: AppViewView;
  detailBinding: AppViewBinding;
} {
  const details = views.views.filter((view) => view.kind === "detail"
    && binding.bindings.some((item) => item.view === view.id && item.source === "customers" && item.ops.includes("show")));
  const tables = views.views.filter((view) => view.kind === "table"
    && binding.bindings.some((item) => item.view === view.id && item.source === "customers" && item.ops.includes("list")));
  if (details.length !== 1 || tables.length !== 1) fail("$", "actions need one customers table and one customers detail");
  const detail = details[0];
  const table = tables[0];
  const detailBinding = binding.bindings.find((item) => item.view === detail.id)!;
  const tableBinding = binding.bindings.find((item) => item.view === table.id)!;
  if (detailBinding.source !== tableBinding.source
      || tableBinding.fields.filter((field) => field.key === "record_id" && field.format === "text").length !== 1) {
    fail("$", "customer table/detail source identity is incomplete");
  }
  return { table, tableBinding, detail, detailBinding };
}

function validateAdapter(descriptor: AppActionDescriptorV2, views: AppViewDescriptor, binding: AppBindingDescriptor): void {
  if (descriptor.app !== "crm" || descriptor.app !== views.app || descriptor.app !== binding.app) {
    fail("$.app", "the first live adapter requires a paired crm action/view/binding receipt");
  }
  if (descriptor.actions.length !== 2) fail("$.actions", "the CRM adapter requires exactly customer.create and customer.update");
  const expected = new Map<string, AppActionV2["operation"]>([
    ["customer.create", "record.create"],
    ["customer.update", "record.update"],
  ]);
  const actions = new Set<string>();
  let pair: ReturnType<typeof customerBindingPair> | null = null;
  for (const action of descriptor.actions) {
    const requiredOperation = expected.get(action.id);
    if (!requiredOperation || action.operation !== requiredOperation || action.record !== "customer") {
      fail("$.actions", `unsupported CRM action or record: ${action.id}`);
    }
    if (actions.has(action.id)) fail("$.actions", "duplicate action id");
    actions.add(action.id);
    if (action.input.fields.length !== CUSTOMER_FIELDS.length
        || action.input.fields.some((field, index) => {
          const expectedField = CUSTOMER_FIELDS[index];
          return field.id !== expectedField.id || field.type !== expectedField.type
            || field.required !== expectedField.required || field.nullable !== expectedField.nullable
            || field.values !== undefined;
        })) {
      fail(`$.actions.${action.id}.input.fields`, "customer fields do not match the closed host adapter");
    }
    const form = views.views.find((view) => view.id === action.form_view);
    if (!form || form.kind !== "form" || !form.previewOf || form.previewOf.length !== action.input.fields.length) {
      fail(`$.actions.${action.id}.form_view`, "action must select a matching form preview");
    }
    action.input.fields.forEach((field, index) => {
      const preview = form.previewOf![index];
      const shape = previewShape(field);
      if (preview.id !== field.id || preview.label !== field.label || preview.format !== shape.format
          || preview.kind !== shape.kind || JSON.stringify(preview.values ?? []) !== JSON.stringify(field.values ?? [])) {
        fail(`$.actions.${action.id}.input.fields.${index}`, "action field does not match its preview field");
      }
    });
    if (action.id === "customer.update") pair = customerBindingPair(views, binding);
  }
  if (!actions.has("customer.create") || !actions.has("customer.update")) fail("$.actions", "both CRM customer actions are required");
  pair ??= customerBindingPair(views, binding);
  for (const id of ["display_name", "email", "phone", "source", "tags"]) {
    if (pair.detailBinding.fields.filter((field) => field.key === id).length !== 1) {
      fail("$", `customer detail must project ${id} exactly once`);
    }
  }
}

/** Parse the receipt's action companion and cross-validate it against the
 *  view and binding in that same installation receipt. The server remains
 *  authoritative and repeats this proof from bundle bytes on every write. */
export function parseAppActionV2(
  raw: unknown,
  views: AppViewDescriptor,
  binding: AppBindingDescriptor,
): AppActionDescriptorV2 {
  scan(raw, "$", { nodes: 0 });
  if (new TextEncoder().encode(JSON.stringify(raw)).byteLength > MAX_BYTES) fail("$", `input exceeds ${MAX_BYTES} bytes`);
  const root = object(raw, ["contract", "app", "title", "summary", "actions"], "$");
  if (root.contract !== APP_ACTIONS_V2_CONTRACT) fail("$.contract", `expected ${APP_ACTIONS_V2_CONTRACT}`);
  const app = identifier(root.app, "$.app");
  const title = text(root.title, "$.title", 120);
  const summary = root.summary === undefined ? undefined : text(root.summary, "$.summary", 280);
  if (!Array.isArray(root.actions) || root.actions.length === 0 || root.actions.length > MAX_ACTIONS) {
    fail("$.actions", `expected 1–${MAX_ACTIONS} actions`);
  }
  const actions = root.actions.map((action, index) => parseAction(action, index));
  const ids = new Set<string>();
  for (const action of actions) {
    if (ids.has(action.id)) fail("$.actions", `duplicate action id: ${action.id}`);
    ids.add(action.id);
  }
  const descriptor: AppActionDescriptorV2 = {
    contract: APP_ACTIONS_V2_CONTRACT,
    app,
    title,
    ...(summary === undefined ? {} : { summary }),
    actions,
  };
  validateAdapter(descriptor, views, binding);
  return descriptor;
}
