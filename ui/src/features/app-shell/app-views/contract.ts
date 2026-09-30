/**
 * App view contract v1 (CAD-861, toward CAD-811): strict runtime
 * validation and text-only projection for a versioned, data-only view
 * descriptor a workspace app package may one day ship next to its
 * workflow bundle. This file mirrors
 * contracts/app-views/v1/app-view.schema.json — the two are the same
 * grammar in two notations; a change must land in both.
 *
 * A descriptor is *data*: it declares an optional `app` kind and a set
 * of named `views` (table / detail / form-preview). It cannot carry
 * code, markup, URLs, actor or scope claims, credentials, or effects —
 * those keys and value types are refused outright, and `parseAppView`
 * fails closed on the first violation.
 *
 * What this module trusts and what it never does:
 *  - Every string a package supplies is rendered as React text. The
 *    renderer in ./AppView.tsx never writes `dangerouslySetInnerHTML`,
 *    never evals, never imports, and builds no href — a descriptor
 *    cannot produce a script, a link target, or a DOM sink.
 *  - `fixtureRows` is the dev-preview's record source: bounded, text
 *    primitives only, keyed by the view's own declared field ids.
 *  - Identity fields a descriptor must never carry (the host owns
 *    them): `install_id`, `context_id`, `workspace`, `project`,
 *    `actor`, `by`, `secret`, `credential`, `token`, `sql`, `path`,
 *    `url`, `href`, `src`, `script`, `html`, `code`, `effect`, and the
 *    JS-escape duo `__proto__`/`constructor`/`prototype`.
 */

/* ------------------------------------------------------------------ */
/* Public types — the validated v1 shape.                              */
/* ------------------------------------------------------------------ */

export const APP_VIEW_CONTRACT = "app-views/v1" as const;
export type AppViewContract = typeof APP_VIEW_CONTRACT;

export type AppViewFormat = "text" | "number" | "date" | "datetime" | "enum" | "tags";
export type AppViewFieldKind = "scalar" | "list";

export interface AppViewField {
  /** Lowercase snake-ish identifier; must be unique inside its view
   *  and is the only string a column or fixture row may name. */
  id: string;
  /** Visible label — plain text only. */
  label: string;
  /** Rendering vocabulary. `enum` requires `values`; every other
   *  format forbids it. */
  format: AppViewFormat;
  kind: AppViewFieldKind;
  /** Allowlisted strings a `format: "enum"` value must equal. */
  values?: string[];
  /** Fields may point at a declared form-preview view in the same
   *  descriptor; the host later wires that to its own create surface. */
  createView?: string;
}

export interface AppViewColumn {
  /** Must equal a field `id` declared on the same view. */
  field: string;
  /** Optional label override — plain text. */
  label?: string;
}

export type AppViewKind = "table" | "detail" | "form";

export interface AppViewView {
  id: string;
  title: string;
  kind: AppViewKind;
  /** table/detail only: the declared fields rows may draw on. */
  fields?: AppViewField[];
  /** table only: which declared fields become columns, and in which
   *  order. Every column must name a declared field; an unknown field
   *  refuses. */
  columns?: AppViewColumn[];
  /** form only: the declared fields rendered as a disabled preview of
   *  the host's future create form. A form view declares no live
   *  mutation — submits stay the host's, and this increment wires none. */
  previewOf?: AppViewField[];
}

export interface AppViewDescriptor {
  contract: AppViewContract;
  /** The workspace-app kind this descriptor was reviewed with
   *  ("crm", "social-content"). It is provenance, not a routing or
   *  authority claim: a descriptor can never name an installation,
   *  context, workspace, project, or actor. */
  app: string;
  /** Human title for the whole descriptor — plain text. */
  title: string;
  /** Inert free-text summary. */
  summary?: string;
  /** Named views; `id` unique within the descriptor. */
  views: AppViewView[];
}

/* ------------------------------------------------------------------ */
/* Errors.                                                             */
/* ------------------------------------------------------------------ */

/** One bounded, human-readable refusal. `path` is a dotted location
 *  inside the supplied value ("views.0.columns.2.field"). */
export class AppViewContractError extends Error {
  readonly path: string;
  constructor(path: string, message: string) {
    super(`${path}: ${message}`);
    this.name = "AppViewContractError";
    this.path = path;
  }
}

/* ------------------------------------------------------------------ */
/* Bounds and grammar. Keep them small — this is a review surface,     */
/* not a templating engine.                                            */
/* ------------------------------------------------------------------ */

const MAX_VIEWS = 16;
const MAX_FIELDS_PER_VIEW = 24;
const MAX_COLUMNS_PER_VIEW = 12;
const MAX_ENUM_VALUES = 24;
const MAX_ID_LENGTH = 64;
const MAX_LABEL_LENGTH = 80;
const MAX_TITLE_LENGTH = 120;
const MAX_SUMMARY_LENGTH = 280;
const MAX_ROWS = 64;
const MAX_ROW_TEXT = 512;
const MAX_LIST_ITEMS = 16;
const MAX_ROW_KEYS = 32;
/** Whole input, serialized: a bound on the bytes a caller can push
 *  through `parseAppView`/`fixtureRows` in one shot. */
const MAX_SERIALIZED_BYTES = 64 * 1024;

/** Identifier grammar shared by field ids, view ids and app names:
 *  lowercase, starts with a letter, then letters/digits/`-`/`_`. */
const IDENT = /^[a-z][a-z0-9_-]{0,63}$/;
const DATE = /^(\d{4})-(\d{2})-(\d{2})$/;
const DATETIME = /^(\d{4}-\d{2}-\d{2})T(\d{2}):(\d{2})(?::(\d{2})(?:\.\d+)?)?(Z|[+-](\d{2}):(\d{2}))$/;

function calendarDate(value: string): boolean {
  const match = DATE.exec(value);
  if (!match) return false;
  const [, y, m, d] = match;
  const year = Number(y), month = Number(m), day = Number(d);
  if (year < 1 || month < 1 || month > 12 || day < 1) return false;
  const leap = year % 4 === 0 && (year % 100 !== 0 || year % 400 === 0);
  const days = [31, leap ? 29 : 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
  return day <= days[month - 1];
}

function calendarDateTime(value: string): boolean {
  const match = DATETIME.exec(value);
  if (!match || !calendarDate(match[1])) return false;
  return Number(match[2]) < 24 && Number(match[3]) < 60
    && (match[4] === undefined || Number(match[4]) < 60)
    && (match[5] === "Z" || (Number(match[6]) < 24 && Number(match[7]) < 60));
}
const CONTROL = /[\u0000-\u001f\u007f\u2028\u2029]/;
const ENUM_FORMATS: readonly AppViewFormat[] = ["enum"];
const FORMATS: readonly AppViewFormat[] = ["text", "number", "date", "datetime", "enum", "tags"];
const KINDS: readonly AppViewFieldKind[] = ["scalar", "list"];
const VIEW_KINDS: readonly AppViewKind[] = ["table", "detail", "form"];

/**
 * Keys a descriptor may never carry — recursively, at every level.
 * They name executable surfaces, URL/navigation escapes, scope and
 * actor identity, storage internals, or authority the host alone
 * owns. A data-only contract has no legitimate use for any of them.
 */
export const FORBIDDEN_DESCRIPTOR_KEYS = [
  "__proto__",
  "prototype",
  "constructor",
  "script",
  "scripts",
  "code",
  "html",
  "innerHTML",
  "css",
  "style",
  "javascript",
  "eval",
  "import",
  "module",
  "url",
  "uri",
  "href",
  "src",
  "link",
  "action",
  "endpoint",
  "install_id",
  "installId",
  "context_id",
  "contextId",
  "workspace",
  "workspace_id",
  "project",
  "project_id",
  "project_link",
  "actor",
  "by",
  "role",
  "grant",
  "scope",
  "scopes",
  "capability",
  "capabilities",
  "secret",
  "secrets",
  "credential",
  "credentials",
  "token",
  "password",
  "sql",
  "query",
  "path",
  "file",
  "effect",
  "effects",
  "verified",
  "digest",
  "revision",
] as const;

const FORBIDDEN = new Set<string>(FORBIDDEN_DESCRIPTOR_KEYS);

/* ------------------------------------------------------------------ */
/* Small checked readers.                                              */
/* ------------------------------------------------------------------ */

type Obj = Record<string, unknown>;

function isObj(v: unknown): v is Obj {
  return typeof v === "object" && v !== null && !Array.isArray(v)
    && (Object.getPrototypeOf(v) === Object.prototype || Object.getPrototypeOf(v) === null);
}

function fail(path: string, message: string): never {
  throw new AppViewContractError(path, message);
}

/** Deep-scan `value` for forbidden keys and oversized structures.
 *  Runs before shape validation so a hostile payload cannot push the
 *  shape checks through a huge or nested shell. Arrays and objects
 *  both count toward one shared node budget. */
function scanUnsafe(value: unknown, path: string, budget: { nodes: number }, depth = 0): void {
  if (depth > 24) fail(path, "input is nested too deeply");
  if (++budget.nodes > 4096) fail(path, "input has too many nodes");
  if (value === null || typeof value === "boolean" || typeof value === "string") return;
  if (typeof value === "number" && Number.isFinite(value)) return;
  if (Array.isArray(value)) {
    for (let i = 0; i < value.length; i++) scanUnsafe(value[i], `${path}.${i}`, budget, depth + 1);
    return;
  }
  if (!isObj(value)) fail(path, "expected plain JSON data");
  for (const [key, property] of Object.entries(Object.getOwnPropertyDescriptors(value))) {
    if (FORBIDDEN.has(key)) fail(path, `forbidden descriptor key: ${key}`);
    if (property.get || property.set) fail(path, "accessors are not JSON data");
    scanUnsafe(property.value, `${path}.${key}`, budget, depth + 1);
  }
}

function boundedJson(raw: unknown, path: string): void {
  // Scan before serialization: cycles, executable values and custom object
  // prototypes must refuse without running toJSON or overflowing the stack.
  scanUnsafe(raw, path, { nodes: 0 });
  const bytes = new TextEncoder().encode(JSON.stringify(raw)).byteLength;
  if (bytes > MAX_SERIALIZED_BYTES) fail(path, `input exceeds ${MAX_SERIALIZED_BYTES} bytes`);
}

function text(v: unknown, path: string, max: number): string {
  if (typeof v !== "string" || v.length === 0) fail(path, "expected a non-empty string");
  if (v.length > max) fail(path, `string is longer than ${max} characters`);
  if (CONTROL.test(v)) fail(path, "control characters are not allowed");
  return v;
}

function ident(v: unknown, path: string): string {
  const s = text(v, path, MAX_ID_LENGTH);
  if (!IDENT.test(s)) fail(path, `unsafe identifier: ${JSON.stringify(s)}`);
  return s;
}

function stringList(v: unknown, path: string, maxItems: number, maxLen: number): string[] {
  if (!Array.isArray(v)) fail(path, "expected an array of strings");
  if (v.length > maxItems) fail(path, `more than ${maxItems} entries`);
  const out: string[] = [];
  for (let i = 0; i < v.length; i++) out.push(text(v[i], `${path}.${i}`, maxLen));
  return out;
}

function oneOf<T extends string>(v: unknown, allowed: readonly T[], path: string): T {
  if (typeof v !== "string" || !allowed.includes(v as T)) {
    fail(path, `expected one of ${allowed.join(", ")}`);
  }
  return v as T;
}

/* ------------------------------------------------------------------ */
/* Shape validation.                                                   */
/* ------------------------------------------------------------------ */

function parseField(raw: unknown, path: string): AppViewField {
  if (!isObj(raw)) fail(path, "field must be an object");
  const allowed = ["id", "label", "format", "kind", "values", "createView"];
  for (const key of Object.keys(raw)) {
    if (!allowed.includes(key)) fail(path, `unknown field key: ${key}`);
  }
  const id = ident(raw.id, `${path}.id`);
  const label = text(raw.label, `${path}.label`, MAX_LABEL_LENGTH);
  const format = oneOf(raw.format ?? "text", FORMATS, `${path}.format`);
  const kind = oneOf(raw.kind ?? "scalar", KINDS, `${path}.kind`);
  let values: string[] | undefined;
  if (raw.values !== undefined) {
    if (!ENUM_FORMATS.includes(format)) {
      fail(`${path}.values`, `values is only allowed on format "enum"`);
    }
    values = stringList(raw.values, `${path}.values`, MAX_ENUM_VALUES, MAX_LABEL_LENGTH);
    if (values.length === 0) fail(`${path}.values`, "enum needs at least one value");
  }
  if (format === "enum" && values === undefined) {
    fail(`${path}.format`, 'format "enum" requires a values list');
  }
  if (format === "tags" && kind !== "list") {
    fail(`${path}.kind`, 'format "tags" must be kind "list"');
  }
  if (kind === "list" && format !== "text" && format !== "tags") {
    fail(`${path}.kind`, 'v1 lists support only text or tags');
  }
  let createView: string | undefined;
  if (raw.createView !== undefined) {
    createView = ident(raw.createView, `${path}.createView`);
  }
  return createView === undefined
    ? { id, label, format, kind, ...(values ? { values } : {}) }
    : { id, label, format, kind, ...(values ? { values } : {}), createView };
}

function parseView(raw: unknown, path: string): AppViewView {
  if (!isObj(raw)) fail(path, "view must be an object");
  const allowed = ["id", "title", "kind", "fields", "columns", "previewOf"];
  for (const key of Object.keys(raw)) {
    if (!allowed.includes(key)) fail(path, `unknown view key: ${key}`);
  }
  const id = ident(raw.id, `${path}.id`);
  const title = text(raw.title, `${path}.title`, MAX_TITLE_LENGTH);
  const kind = oneOf(raw.kind, VIEW_KINDS, `${path}.kind`);

  if (kind === "form") {
    if (raw.fields !== undefined || raw.columns !== undefined) {
      fail(path, 'a "form" view carries previewOf, not fields/columns');
    }
    if (!Array.isArray(raw.previewOf) || raw.previewOf.length === 0) {
      fail(`${path}.previewOf`, 'a "form" view needs at least one preview field');
    }
    if (raw.previewOf.length > MAX_FIELDS_PER_VIEW) {
      fail(`${path}.previewOf`, `more than ${MAX_FIELDS_PER_VIEW} fields`);
    }
    const previewOf = raw.previewOf.map((f, i) => parseField(f, `${path}.previewOf.${i}`));
    const seen = new Set<string>();
    for (const f of previewOf) {
      if (seen.has(f.id)) fail(`${path}.previewOf`, `duplicate field id: ${f.id}`);
      seen.add(f.id);
    }
    return { id, title, kind, previewOf };
  }

  if (raw.previewOf !== undefined) {
    fail(path, `a "${kind}" view cannot carry previewOf`);
  }
  if (!Array.isArray(raw.fields) || raw.fields.length === 0) {
    fail(`${path}.fields`, `a "${kind}" view needs at least one declared field`);
  }
  if (raw.fields.length > MAX_FIELDS_PER_VIEW) {
    fail(`${path}.fields`, `more than ${MAX_FIELDS_PER_VIEW} fields`);
  }
  const fields = raw.fields.map((f, i) => parseField(f, `${path}.fields.${i}`));
  const seen = new Set<string>();
  for (const f of fields) {
    if (seen.has(f.id)) fail(`${path}.fields`, `duplicate field id: ${f.id}`);
    seen.add(f.id);
  }

  let columns: AppViewColumn[] | undefined;
  if (kind === "table") {
    if (!Array.isArray(raw.columns) || raw.columns.length === 0) {
      fail(`${path}.columns`, 'a "table" view needs at least one column');
    }
    if (raw.columns.length > MAX_COLUMNS_PER_VIEW) {
      fail(`${path}.columns`, `more than ${MAX_COLUMNS_PER_VIEW} columns`);
    }
    const declared = new Set(fields.map((f) => f.id));
    const used = new Set<string>();
    columns = raw.columns.map((c, i) => {
      const cp = `${path}.columns.${i}`;
      if (!isObj(c)) fail(cp, "column must be an object");
      for (const key of Object.keys(c)) {
        if (key !== "field" && key !== "label") fail(cp, `unknown column key: ${key}`);
      }
      const field = ident(c.field, `${cp}.field`);
      if (!declared.has(field)) {
        fail(`${cp}.field`, `column names undeclared field: ${field}`);
      }
      if (used.has(field)) fail(`${cp}.field`, `duplicate column field: ${field}`);
      used.add(field);
      const label = c.label === undefined ? undefined : text(c.label, `${cp}.label`, MAX_LABEL_LENGTH);
      return label === undefined ? { field } : { field, label };
    });
  } else if (raw.columns !== undefined) {
    fail(path, `a "${kind}" view cannot carry columns`);
  }
  return { id, title, kind, fields, ...(columns ? { columns } : {}) };
}

/**
 * Validate `raw` as an app-views/v1 descriptor. Throws
 * `AppViewContractError` on the first violation — unknown keys,
 * forbidden keys anywhere in the tree, bad identifier grammar,
 * oversized strings/arrays, undeclared column or fixture references,
 * wrong or missing contract tag. On success returns a fresh,
 * structurally-shared copy typed as `AppViewDescriptor`.
 */
export function parseAppView(raw: unknown): AppViewDescriptor {
  boundedJson(raw, "$");

  if (!isObj(raw)) fail("$", "descriptor must be an object");
  for (const key of Object.keys(raw)) {
    if (!["contract", "app", "title", "summary", "views"].includes(key)) {
      fail("$", `unknown descriptor key: ${key}`);
    }
  }
  if (raw.contract !== APP_VIEW_CONTRACT) {
    fail("$.contract", `expected ${JSON.stringify(APP_VIEW_CONTRACT)}`);
  }
  const app = ident(raw.app, "$.app");
  const title = text(raw.title, "$.title", MAX_TITLE_LENGTH);
  const summary =
    raw.summary === undefined ? undefined : text(raw.summary, "$.summary", MAX_SUMMARY_LENGTH);
  if (!Array.isArray(raw.views) || raw.views.length === 0) {
    fail("$.views", "descriptor needs at least one view");
  }
  if (raw.views.length > MAX_VIEWS) fail("$.views", `more than ${MAX_VIEWS} views`);
  const views = raw.views.map((v, i) => parseView(v, `$.views.${i}`));
  const viewIds = new Set<string>();
  for (const v of views) {
    if (viewIds.has(v.id)) fail("$.views", `duplicate view id: ${v.id}`);
    viewIds.add(v.id);
  }
  // Cross-references: fields may name a declared form view, and only a
  // form view — anything else would dangle at render time.
  const formIds = new Set(views.filter((v) => v.kind === "form").map((v) => v.id));
  const checkCreateView = (fields: AppViewField[], path: string) => {
    for (const f of fields) {
      if (f.createView !== undefined && !formIds.has(f.createView)) {
        fail(path, `createView names no declared form view: ${f.createView}`);
      }
    }
  };
  for (let i = 0; i < views.length; i++) {
    const v = views[i];
    if (v.fields) checkCreateView(v.fields, `$.views.${i}.fields`);
    if (v.previewOf) checkCreateView(v.previewOf, `$.views.${i}.previewOf`);
  }
  return { contract: APP_VIEW_CONTRACT, app, title, ...(summary ? { summary } : {}), views };
}

/** Report the first validation error without throwing. */
export function describeAppView(raw: unknown): { ok: true; descriptor: AppViewDescriptor } | { ok: false; error: string } {
  try {
    return { ok: true, descriptor: parseAppView(raw) };
  } catch (e) {
    return { ok: false, error: e instanceof Error ? e.message : String(e) };
  }
}

/* ------------------------------------------------------------------ */
/* Fixture rows: the dev preview's synthetic record source.            */
/*                                                                     */
/* A view's `fixtureRows` supply is NOT part of the descriptor —      */
/* packages never ship rows. The preview caller hands rows in         */
/* separately so a descriptor can't smuggle data past validation as   */
/* content, and this function still checks them against the view's    */
/* declared fields: unknown keys refuse, values stay bounded text     */
/* primitives, enum/list formats are enforced.                        */
/* ------------------------------------------------------------------ */

export type AppViewCell = string | number | string[];
export type AppViewRow = Record<string, AppViewCell>;

function cell(raw: unknown, field: AppViewField, path: string): AppViewCell {
  if (field.kind === "list") {
    const items = stringList(raw, path, MAX_LIST_ITEMS, MAX_ROW_TEXT);
    return items;
  }
  if (field.format === "number") {
    if (typeof raw === "number") {
      if (!Number.isFinite(raw)) fail(path, "number is not finite");
      return raw;
    }
    const s = text(raw, path, MAX_ROW_TEXT);
    if (s.trim() === "" || !Number.isFinite(Number(s))) {
      fail(path, "expected a number");
    }
    return s;
  }
  const s = text(raw, path, MAX_ROW_TEXT);
  if (field.format === "date" && !calendarDate(s)) fail(path, "expected a valid YYYY-MM-DD calendar date");
  if (field.format === "datetime" && !calendarDateTime(s)) {
    fail(path, "expected a valid ISO calendar datetime with an explicit timezone");
  }
  if (field.format === "enum" && !field.values!.includes(s)) {
    fail(path, `enum value outside the declared values: ${JSON.stringify(s)}`);
  }
  return s;
}

/**
 * Validate fixture rows for one already-validated view. Rows are
 * plain objects keyed by declared field id; missing cells render as
 * "—". Unknown keys, unbounded strings, wrong shapes and undeclared
 * enum values all refuse — a fixture is still untrusted input even
 * though only the host's own examples ship today.
 */
export function fixtureRows(view: AppViewView, raw: unknown): AppViewRow[] {
  boundedJson(raw, "rows");
  const declared = new Map((view.fields ?? []).map((f) => [f.id, f]));
  if (declared.size === 0) fail("rows", `view ${JSON.stringify(view.id)} declares no fields`);
  if (!Array.isArray(raw)) fail("rows", "expected an array of row objects");
  if (raw.length > MAX_ROWS) fail("rows", `more than ${MAX_ROWS} rows`);
  return raw.map((row, i) => {
    const path = `rows.${i}`;
    if (!isObj(row)) fail(path, "row must be an object");
    if (Object.keys(row).length > MAX_ROW_KEYS) fail(path, `more than ${MAX_ROW_KEYS} cells`);
    const out: AppViewRow = {};
    for (const [key, value] of Object.entries(row)) {
      const field = declared.get(key);
      if (!field) fail(`${path}.${key}`, `row names undeclared field: ${key}`);
      out[key] = cell(value, field, `${path}.${key}`);
    }
    return out;
  });
}

/* ------------------------------------------------------------------ */
/* Display projection: descriptor cell → bounded text for React.      */
/* ------------------------------------------------------------------ */

/** One line of display text for a cell, or "—" when absent. Numbers
 *  stay plain decimal; enums render their declared label; lists join
 *  with ", ". Output length is bounded by the row bound at parse
 *  time; nothing here is HTML or a URL — React renders it as text. */
export function cellText(value: AppViewCell | undefined): string {
  if (value === undefined || value === "") return "—";
  if (Array.isArray(value)) return value.length === 0 ? "—" : value.join(", ");
  if (typeof value === "number") return String(value);
  return value;
}
