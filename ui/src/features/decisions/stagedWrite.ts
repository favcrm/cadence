/**
 * CAD-2186: the owner's decisions from `GET /api/assist/actions`, read into
 * one shape the To do rail and the detail share.
 *
 * Staged kinds: `cadence_cli` (AOS-679, a hosted Cadence CLI write), `app_call` (a record write staged from the hosted
 * CLI or chat) and `board_create_issue` (the assistant proposing an issue).
 * A row that does not parse is not renderable: it is dropped, never shown as
 * a decision and never read as success. A `app_call` row without the new
 * summary fields (an API that predates them) keeps a safe fallback: its own
 * `subject` sentence and the raw call, with the summary marked incomplete.
 */

export type StagedStatus = "pending" | "approved" | "denied";
/** What the owner sees: a failed apply is `approved` with `result.ok === false`. */
/** `unconfirmed`: approved, but the row carries no `result.ok === true` to say it applied. */
export type StagedOutcome = "pending" | "applied" | "failed" | "denied" | "unconfirmed";

export interface StagedChange {
  field: string;
  label: string;
  kind: "changed" | "added" | "removed";
  /** `undefined`: the API did not say; `null`: the value is empty. */
  before?: string | null;
  after?: string | null;
}

export interface StagedOrigin {
  via: string;
  kind: string;
  label: string;
  actor?: string;
}

export interface StagedResult {
  ok: boolean;
  created?: boolean;
  code?: string;
  /** The board kept the row pending because the attempt did not settle. */
  retry?: boolean;
  record?: { id?: string; revision?: number };
  /** `board_create_issue`: the ticket it made. */
  ticket?: string;
}

export interface StagedWrite {
  id: string;
  kind: "app_call" | "create_issue" | "connected_effect" | "cadence_cli" | "custom_mcp_effect";
  status: StagedStatus;
  createdAt: number;
  decidedAt: number | null;
  title: string;
  /** App label ("CRM"); `null` when the API does not name one. */
  app: string | null;
  target: { kind: string; label: string; id: string | null } | null;
  changes: StagedChange[];
  unchangedCount: number;
  /** False when the summary may not cover everything the call carries. */
  changesComplete: boolean;
  reason: string | null;
  consent: [string, string][] | null;
  effects: string[];
  /** The exact stored call, shown only under Show raw. */
  raw: string;
  origin: StagedOrigin | null;
  result: StagedResult | null;
  /** Optional owner note recorded when denying a request. */
  decisionReason?: string | null;
  /** The record behind "Open record", when the API names a link. */
  recordLink: string | null;
  /** `cadence_cli` (AOS-679): a hosted Cadence CLI write staged for the owner. Display text is made visible (see `visible`). */
  cli?: { verb: string; summary: string | null; requestedBy: string | null };
  /** `connected_effect`: the draft or send a connected tool would make. */
  connected: {
    effect: "draft" | "send";
    slug: string;
    toolkit: string;
    account: string;
    /** AOS-647/CAD-2441: display-only fields; `args` (in `raw`) is what runs. */
    preview?: ConnectedPreview;
  } | null;
  /** `custom_mcp_effect`: an owner-approved remote Custom MCP tool call pinned to snapshot_revision. */
  customMcp?: {
    connectionId: string;
    connectionName: string;
    endpointHost: string;
    tool: string;
    snapshotRevision: number;
    preview?: ConnectedPreview;
  } | null;
}

/** A connected effect's display preview (AOS-647). Every value is untrusted text. */
export interface ConnectedPreview {
  title: string | null;
  toolkitLabel: string | null;
  /** The sending account the preview names (CAD-2463). */
  account: string | null;
  kind: "email" | "message" | "fields";
  fields: { role: string; label: string; value: string }[];
}

/** AOS-647 caps a preview at 12 fields of 2,000 characters; the board holds it to the same. */
const PREVIEW_FIELDS_MAX = 12;
const PREVIEW_VALUE_MAX = 2000;

/** The row's `input.preview`, or undefined when it is absent or malformed (the args fallback stays). */
export function parsePreview(value: unknown): ConnectedPreview | undefined {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return undefined;
  const p = value as Record<string, unknown>;
  const kind = p.kind === "email" || p.kind === "message" || p.kind === "fields" ? p.kind : null;
  if (kind === null || !Array.isArray(p.fields)) return undefined;
  const fields: ConnectedPreview["fields"] = [];
  for (const f of p.fields.slice(0, PREVIEW_FIELDS_MAX)) {
    if (typeof f !== "object" || f === null) return undefined;
    const field = f as Record<string, unknown>;
    if (typeof field.label !== "string" || typeof field.value !== "string") return undefined;
    fields.push({
      role: typeof field.role === "string" ? field.role : "",
      label: field.label.slice(0, 80),
      value: field.value.slice(0, PREVIEW_VALUE_MAX),
    });
  }
  const text = (v: unknown) => (typeof v === "string" && v.trim() !== "" ? v.slice(0, 200) : null);
  return { title: text(p.title), toolkitLabel: text(p.toolkit_label), account: text(p.account), kind, fields };
}

const STATUSES: readonly string[] = ["pending", "approved", "denied"];

const isRecord = (v: unknown): v is Record<string, unknown> =>
  typeof v === "object" && v !== null && !Array.isArray(v);

const str = (v: unknown): string | null => (typeof v === "string" && v.trim() !== "" ? v : null);

/** A value shown as data: strings as they are, structures as JSON text. */
export function valueText(v: unknown): string | null | undefined {
  if (v === undefined) return undefined;
  if (v === null) return null;
  if (typeof v === "string") return v;
  if (typeof v === "number" || typeof v === "boolean") return String(v);
  try {
    return JSON.stringify(v);
  } catch {
    return undefined;
  }
}

function parseChange(v: unknown): StagedChange | null {
  if (!isRecord(v)) return null;
  // A name is data: any string, even a blank one, never a reason to drop the row.
  const field = typeof v.field === "string" ? v.field : null;
  const label = typeof v.label === "string" ? v.label : field;
  const kind = v.kind;
  if (field === null || label === null) return null;
  if (kind !== "changed" && kind !== "added" && kind !== "removed") return null;
  return { field, label, kind, before: valueText(v.before), after: valueText(v.after) };
}

function parseOrigin(v: unknown): StagedOrigin | null {
  if (!isRecord(v)) return null;
  const via = str(v.via);
  const kind = str(v.kind);
  const label = str(v.label);
  if (via === null || kind === null || label === null) return null;
  return { via, kind, label, ...(str(v.actor) ? { actor: str(v.actor)! } : {}) };
}

function parseResult(v: unknown): StagedResult | null | "invalid" {
  if (v === undefined || v === null) return null;
  if (!isRecord(v) || typeof v.ok !== "boolean") return "invalid";
  const record = isRecord(v.record)
    ? {
        ...(str(v.record.id) ? { id: str(v.record.id)! } : {}),
        ...(typeof v.record.revision === "number" ? { revision: v.record.revision } : {}),
      }
    : undefined;
  const ticket = isRecord(v.ticket) ? str(v.ticket.id) : null;
  return {
    ok: v.ok,
    ...(typeof v.created === "boolean" ? { created: v.created } : {}),
    ...(str(v.code) ? { code: str(v.code)! } : {}),
    ...(v.retry === true ? { retry: true } : {}),
    ...(record ? { record } : {}),
    ...(ticket ? { ticket } : {}),
  };
}

function parseConsent(v: unknown): [string, string][] | null {
  if (!isRecord(v)) return null;
  const entries = Object.entries(v).flatMap(([k, val]): [string, string][] => {
    const text = valueText(val);
    return typeof text === "string" ? [[k, text]] : [];
  });
  return entries.length > 0 ? entries : null;
}

/** The stored call as text: pretty JSON when it is JSON, else exactly as stored. */
export function rawText(args: unknown): string {
  if (typeof args === "string") {
    try {
      return JSON.stringify(JSON.parse(args), null, 2);
    } catch {
      return args;
    }
  }
  try {
    return JSON.stringify(args, null, 2) ?? "";
  } catch {
    return "";
  }
}

function parseAppCall(base: Pick<StagedWrite, "id" | "status" | "createdAt" | "decidedAt" | "decisionReason">, row: Record<string, unknown>, result: StagedResult | null): StagedWrite | null {
  const input = isRecord(row.input) ? row.input : {};
  const origin = parseOrigin(row.origin);
  const title = str(row.title);
  if (title === null) {
    // An API that predates the summary: the approver sentence and the raw call.
    const subject = str(input.subject) ?? str(row.subject);
    if (subject === null) return null;
    return {
      ...base,
      kind: "app_call",
      title: subject,
      app: str(input.app),
      target: null,
      changes: [],
      unchangedCount: 0,
      changesComplete: false,
      reason: null,
      consent: null,
      effects: [],
      raw: rawText(input.args ?? input),
      origin,
      result,
      recordLink: null,
      connected: null,
      customMcp: null,
    };
  }
  if (!Array.isArray(row.changes)) return null;
  const changes = row.changes.map(parseChange);
  if (changes.some((c) => c === null)) return null;
  const target = isRecord(row.target) && str(row.target.label)
    ? { kind: str(row.target.kind) ?? "record", label: str(row.target.label)!, id: str(row.target.id) }
    : null;
  const effects = Array.isArray(row.effects) ? row.effects.filter((e): e is string => typeof e === "string" && e.trim() !== "") : [];
  const unchanged = typeof row.unchanged_count === "number" && row.unchanged_count > 0 ? Math.floor(row.unchanged_count) : 0;
  return {
    ...base,
    kind: "app_call",
    title,
    app: str(row.app) ?? str(input.app),
    target,
    changes: changes as StagedChange[],
    unchangedCount: unchanged,
    // Only an explicit `true` makes the summary complete.
    changesComplete: row.changes_complete === true,
    reason: str(row.reason),
    consent: parseConsent(row.consent),
    effects,
    raw: rawText(row.args ?? input.args ?? input),
    origin,
    result,
    recordLink: str(row.record_link),
    connected: null,
    customMcp: null,
  };
}

function parseCreateIssue(base: Pick<StagedWrite, "id" | "status" | "createdAt" | "decidedAt" | "decisionReason">, row: Record<string, unknown>, result: StagedResult | null): StagedWrite | null {
  const input = isRecord(row.input) ? row.input : null;
  const title = input ? str(input.title)?.trim() ?? null : null;
  if (input === null || title === null) return null;
  const field = (key: string, label: string): StagedChange[] => {
    const v = str(input[key]);
    return v === null ? [] : [{ field: key, label, kind: "added", after: v }];
  };
  return {
    ...base,
    kind: "create_issue",
    title,
    app: null,
    target: null,
    changes: [
      ...field("title", "Title"),
      ...field("project", "Project"),
      ...field("priority", "Priority"),
      ...field("body", "Description"),
    ],
    unchangedCount: 0,
    changesComplete: true,
    reason: null,
    consent: null,
    effects: [],
    raw: rawText(input),
    origin: parseOrigin(row.origin),
    result,
    recordLink: null,
    connected: null,
    customMcp: null,
  };
}

/** A connected tool's staged draft or send. Any malformed part drops the row: it is never approvable. */
function parseConnectedEffect(base: Pick<StagedWrite, "id" | "status" | "createdAt" | "decidedAt" | "decisionReason">, row: Record<string, unknown>, result: StagedResult | null): StagedWrite | null {
  const input = isRecord(row.input) ? row.input : null;
  if (input === null) return null;
  const slug = str(input.slug);
  const toolkit = str(input.toolkit);
  const account = str(input.account);
  const effect = input.effect;
  if (slug === null || toolkit === null || account === null || str(input.connection_id) === null) return null;
  if (effect !== "draft" && effect !== "send") return null;
  if (typeof input.args !== "string") return null;
  let args: unknown;
  try {
    args = JSON.parse(input.args);
  } catch {
    return null;
  }
  if (!isRecord(args)) return null;
  const preview = parsePreview(input.preview);
  return {
    ...base,
    kind: "connected_effect",
    title: slug,
    app: `${account} (${toolkit})`,
    target: null,
    // Every key and value the call carries, as text: nothing is summarised away.
    changes: Object.entries(args).map(([field, v]) => ({ field, label: field, kind: "added", after: valueText(v) })),
    unchangedCount: 0,
    changesComplete: true,
    reason: null,
    consent: null,
    effects: [],
    raw: rawText(input.args),
    origin: parseOrigin(row.origin),
    result,
    recordLink: null,
    connected: { effect, slug, toolkit, account, ...(preview ? { preview } : {}) },
    customMcp: null,
  };
}

/** CAD-2556: a staged Custom MCP tool call pinned to a snapshot revision. */
function parseCustomMcpEffect(
  base: Pick<StagedWrite, "id" | "status" | "createdAt" | "decidedAt" | "decisionReason">,
  row: Record<string, unknown>,
  result: StagedResult | null,
): StagedWrite | null {
  const input = isRecord(row.input) ? row.input : null;
  if (input === null) return null;
  const connectionId = str(input.connection_id);
  const connectionName = str(input.connection_name);
  const endpointHost = str(input.endpoint_host);
  const tool = str(input.tool);
  const snapshotRevision = input.snapshot_revision;
  if (
    connectionId === null ||
    connectionName === null ||
    endpointHost === null ||
    tool === null ||
    typeof snapshotRevision !== "number" ||
    !Number.isInteger(snapshotRevision) ||
    snapshotRevision < 1
  ) {
    return null;
  }
  if (typeof input.args !== "string") return null;
  let args: unknown;
  try {
    args = JSON.parse(input.args);
  } catch {
    return null;
  }
  if (!isRecord(args)) return null;
  const preview = parsePreview(input.preview);
  const origin = parseOrigin(row.origin);
  return {
    ...base,
    kind: "custom_mcp_effect",
    title: visible(tool),
    app: visible(`${connectionName} (${endpointHost})`),
    target: null,
    changes: Object.entries(args).map(([field, v]) => {
      const rendered = valueText(v);
      return {
        field: visible(field),
        label: visible(field),
        kind: "added" as const,
        after: typeof rendered === "string" ? visible(rendered) : rendered,
      };
    }),
    unchangedCount: 0,
    changesComplete: true,
    reason: null,
    consent: null,
    effects: [],
    raw: visible(rawText(input.args)),
    origin: origin === null ? null : { ...origin, label: visible(origin.label), via: visible(origin.via) },
    result: result?.code === undefined ? result : { ...result, code: visible(result.code) },
    recordLink: null,
    connected: null,
    customMcp: {
      connectionId,
      connectionName: visible(connectionName),
      endpointHost: visible(endpointHost),
      tool: visible(tool),
      snapshotRevision,
      ...(preview ? { preview } : {}),
    },
  };
}

/**
 * Bidi controls, zero-width and other invisible or control characters, shown
 * as `[U+202E]` so a staged value cannot read differently from what it is.
 * Newlines and tabs stay; display only, never applied to what runs.
 */
const HIDDEN = /[\u0000-\u0008\u000B\u000C\u000E-\u001F\u007F-\u009F\u00AD\u061C\u115F\u1160\u17B4\u17B5\u180B-\u180F\u200B-\u200F\u2028-\u202E\u2060-\u206F\u2800\u3164\uFEFF\uFFA0\uFFF9-\uFFFB\u{1D173}-\u{1D17A}\u{E0000}-\u{E0FFF}]|\u034F|[\uFE00-\uFE0F]/gu;
export const visible = (text: string): string =>
  text.replace(HIDDEN, (c) => `[U+${c.codePointAt(0)!.toString(16).toUpperCase().padStart(4, "0")}]`);

/**
 * A staged Cadence CLI write (AOS-679). `changes` is the API's full list, one
 * `added` entry per staged argument and per `set` pair; only an explicit
 * `changes_complete: true` makes it reviewable. Otherwise no summary and no
 * partial list is kept, and Approve stays off (`approvable`). A malformed row
 * (no verb, or a change that does not parse) is dropped.
 */
function parseCadenceCli(base: Pick<StagedWrite, "id" | "status" | "createdAt" | "decidedAt" | "decisionReason">, row: Record<string, unknown>, result: StagedResult | null): StagedWrite | null {
  const input = isRecord(row.input) ? row.input : {};
  const verb = str(row.verb) ?? str(input.verb);
  if (verb === null) return null;
  const complete = row.changes_complete === true;
  let changes: StagedChange[] = [];
  if (complete) {
    if (!Array.isArray(row.changes)) return null;
    const parsed = row.changes.map(parseChange);
    if (parsed.some((c) => c === null)) return null;
    changes = (parsed as StagedChange[]).map((c) => ({
      ...c,
      field: visible(c.field),
      label: visible(c.label),
      ...(typeof c.before === "string" ? { before: visible(c.before) } : {}),
      ...(typeof c.after === "string" ? { after: visible(c.after) } : {}),
    }));
  }
  const origin = parseOrigin(row.origin);
  const summary = complete ? str(row.summary) : null;
  const requestedBy = str(row.requested_by);
  return {
    ...base,
    kind: "cadence_cli",
    title: visible(str(row.title) ?? `Cadence: ${verb}`),
    app: null,
    target: null,
    changes,
    unchangedCount: 0,
    changesComplete: complete,
    reason: null,
    consent: null,
    effects: [],
    raw: visible(rawText(typeof row.args === "string" ? row.args : (input.args ?? ""))),
    origin: origin === null ? null : { ...origin, label: visible(origin.label), via: visible(origin.via) },
    result: result?.code === undefined ? result : { ...result, code: visible(result.code) },
    recordLink: null,
    cli: { verb: visible(verb), summary: summary === null ? null : visible(summary), requestedBy: requestedBy === null ? null : visible(requestedBy) },
    connected: null,
    customMcp: null,
  };
}

/** Approve is offered only for a row whose whole request can be read. Deny is always safe. */
export const approvable = (write: StagedWrite): boolean => write.kind !== "cadence_cli" || write.changesComplete;

/** One list row → a decision, or null when it is not one we can show. */
export function parseStagedRow(row: unknown): StagedWrite | null {
  if (!isRecord(row)) return null;
  const id = str(row.id);
  const status = row.status;
  if (id === null || typeof status !== "string" || !STATUSES.includes(status)) return null;
  if (typeof row.createdAt !== "number" || !Number.isFinite(row.createdAt)) return null;
  const result = parseResult(row.result);
  if (result === "invalid") return null;
  const base = {
    id,
    status: status as StagedStatus,
    createdAt: row.createdAt,
    decidedAt: typeof row.decidedAt === "number" && Number.isFinite(row.decidedAt) ? row.decidedAt : null,
    decisionReason: str(row.decision_reason),
  };
  if (row.tool === "app_call") return parseAppCall(base, row, result);
  if (row.tool === "board_create_issue") return parseCreateIssue(base, row, result);
  if (row.tool === "connected_effect") return parseConnectedEffect(base, row, result);
  if (row.tool === "custom_mcp_effect") return parseCustomMcpEffect(base, row, result);
  if (row.tool === "cadence_cli") return parseCadenceCli(base, row, result);
  return null;
}

export function parseStagedList(body: unknown): StagedWrite[] | null {
  if (!isRecord(body) || !Array.isArray(body.actions)) return null;
  return body.actions.map(parseStagedRow).filter((r): r is StagedWrite => r !== null);
}

/** The provider may have acted: not a failure, not a success, and not "nothing ran", whatever the row's status. */
export const isUncertain = (write: StagedWrite): boolean => write.connected !== null && write.result?.ok === false && (write.result.code === "uncertain" || write.result.retry === true);

export function outcomeOf(write: StagedWrite): StagedOutcome {
  if (write.status === "pending") return "pending";
  if (isUncertain(write)) return "unconfirmed";
  if (write.status === "denied") return "denied";
  if (write.result?.ok === false) return "failed";
  // Success is only ever what the row says: ok === true (or the ticket a create made).
  if (write.result?.ok === true || (write.kind === "create_issue" && write.result?.ticket)) return "applied";
  return "unconfirmed";
}

/** Plain reasons for the closed set of apply codes; anything else says so. */
const REASONS: Readonly<Record<string, string>> = {
  record_not_found: "the record no longer exists",
  revision_conflict: "the record changed after it was staged",
  record_exists: "a record with that id already exists",
  record_too_large: "the record would be too large",
  validation_failed: "the app rejected a value",
  invalid: "the app rejected the request",
  owner_required: "only the workspace owner can run this",
  screen_access_denied: "the app's connection lacks permission",
  operation_not_declared: "the app does not offer this operation",
  app_unavailable: "the app could not be reached",
  storage_unavailable: "the record store could not be reached",
  refused: "the app refused it",
};

const CONNECTED_REASONS: Readonly<Record<string, string>> = {
  expired: "it was staged more than 7 days ago and has expired",
  owner_required: "only the workspace owner can run this",
  invalid: "the request was not valid",
  unavailable: "the tool is no longer published or priced",
  not_allowlisted: "the tool is not on the allowed list",
  toolkit_disabled: "this kind of connection is turned off",
  foreign_connection: "the connection belongs to someone else",
  connection_inactive: "the connection is no longer active",
  connection_access_denied: "the connection does not allow this",
  insufficient_funds: "there are not enough funds to run it",
  invalid_provider_response: "the provider's reply could not be understood",
  upstream_error: "the provider reported an error",
  uncertain: "the provider may have acted, so do not assume it did not",
};

const CUSTOM_MCP_REASONS: Readonly<Record<string, string>> = {
  expired: "it was staged more than 7 days ago and has expired",
  owner_required: "only the workspace owner can run this",
  invalid: "the request was not valid",
  snapshot_drift: "the Custom MCP tool definition changed since it was staged",
  stale_revision: "the Custom MCP tool definition changed since it was staged",
  drift_review_required: "the Custom MCP server has pending changes that must be reviewed first",
  auth_expired: "authorization for this Custom MCP server expired — reconnect in Settings → Connections",
  tool_disabled: "this Custom MCP tool is disabled",
  connection_access_denied: "this Custom MCP server policy does not allow this action",
  ssrf_blocked: "the remote endpoint host is blocked",
  ssrf_redirect_blocked: "the remote server attempted a blocked redirect",
  mcp_upstream_error: "the remote Custom MCP server reported an error",
  upstream_error: "the remote Custom MCP server reported an error",
  timeout: "the remote Custom MCP server timed out",
};

export function failureReason(write: StagedWrite): string {
  const code = write.result?.code;
  if (write.cli) return `refused (${code ?? "unknown"})`;
  if (write.customMcp != null) return (code && CUSTOM_MCP_REASONS[code]) || `refused (${code ?? "unknown"})`;
  if (write.connected !== null) return (code && CONNECTED_REASONS[code]) || `refused (${code ?? "unknown"})`;
  return (code && REASONS[code]) || "the reason is not recognised";
}

/** A failure reason in the viewer's language; an unlisted connected code keeps its code, as data. */
export function reasonText(reason: string, t: (english: string) => string): string {
  const code = /^refused \((.*)\)$/s.exec(reason)?.[1];
  return code === undefined ? t(reason) : t("refused ({code})").replace("{code}", () => isolate(code));
}

/** The row's own heading: a connected effect leads with its verb. */
export function titleOf(write: StagedWrite, t: (english: string) => string): string {
  if (write.customMcp != null) return `${t("Run tool")} · ${write.title}`;
  return write.connected === null ? write.title : `${t(write.connected.effect === "send" ? "Send" : "Draft")} · ${write.title}`;
}

type Say = (english: string) => string;
const same: Say = (s) => s;

/** `Atlas (agent)`, `Priya Nair (person)`; null when no origin is recorded (the assistant, in Home). */
export function askedBy(write: StagedWrite, t: Say = same): string | null {
  if (write.cli) return write.cli.requestedBy ?? (write.origin?.label ?? null);
  const o = write.origin;
  if (o === null) return null;
  return `${o.label} (${o.kind === "agent" || o.kind === "person" ? t(o.kind) : o.kind})`;
}

/** The rail's second line: `CRM · Atlas (agent)`. */
export function sourceLine(write: StagedWrite, t: Say = same): string {
  return [write.app, askedBy(write, t)].filter((p): p is string => !!p).join(" · ");
}

/** `just now`, `4 min`, `2 h`, `3 d` since `fromMs`, in the viewer's language. */
export function ageText(fromMs: number, t: Say = same, nowMs = Date.now()): string {
  const s = Math.max(0, Math.floor((nowMs - fromMs) / 1000));
  if (s < 60) return t("just now");
  if (s < 3600) return t("{n} min").replace("{n}", String(Math.floor(s / 60)));
  if (s < 86400) return t("{n} h").replace("{n}", String(Math.floor(s / 3600)));
  return t("{n} d").replace("{n}", String(Math.floor(s / 86400)));
}

/** The keys the call itself sent (`profile` in the stored args), or null when they cannot be read. */
export function sentFields(write: StagedWrite): Set<string> | null {
  if (write.kind !== "app_call") return null;
  try {
    const call: unknown = JSON.parse(write.raw);
    const profile = isRecord(call) ? call.profile : null;
    return isRecord(profile) ? new Set(Object.keys(profile)) : null;
  } catch {
    return null;
  }
}

/** Isolate initiators and terminators inside shown text could close the isolate around it. */
export const isolate = (text: string): string => text.replace(/[\u2066-\u2069]/g, "");

export type StagedDomain = "marketing" | "design" | "business" | "operations" | "engineering" | "workspace";

function matchDomainText(text: string): StagedDomain | null {
  const s = text.toLowerCase();
  if (/\b(marketing|campaign|newsletter|mailchimp|resend|promo|seo|social|ad|ads|audience)\b/.test(s)) return "marketing";
  if (/\b(design|figma|brand|creative|mockup|asset|palette|typography|ui|ux)\b/.test(s)) return "design";
  if (/\b(crm|customer|contact|deal|lead|quote|invoice|billing|stripe|shopify|sales|account|revenue)\b/.test(s)) return "business";
  if (/\b(ops|operation|operations|incident|vendor|inventory|support|runbook|compliance|procurement|schedule)\b/.test(s)) return "operations";
  if (/\b(git|github|repo|pull request|deploy|release|ci|api|worker|code|engineering)\b/.test(s)) return "engineering";
  return null;
}

/** Classify a staged write by its domain surface for consistent cross-domain presentation. */
export function stagedDomain(write: StagedWrite): StagedDomain {
  if (write.kind === "create_issue") {
    const project = write.changes.find((c) => c.field === "project")?.after ?? "";
    return (
      matchDomainText(project) ??
      matchDomainText([write.title, ...write.changes.map((c) => `${c.field} ${c.after ?? ""}`)].join(" ")) ??
      "workspace"
    );
  }
  return matchDomainText([write.app ?? "", write.target?.kind ?? ""].join(" ")) ?? matchDomainText(write.title) ?? "workspace";
}

const DOMAIN_LABELS: Readonly<Record<StagedDomain, string>> = {
  marketing: "Marketing",
  design: "Design",
  business: "Business",
  operations: "Operations",
  engineering: "Engineering",
  workspace: "Workspace",
};

export const stagedDomainLabel = (domain: StagedDomain): string => DOMAIN_LABELS[domain];
