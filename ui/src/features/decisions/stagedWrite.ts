/**
 * CAD-2186: the owner's decisions from `GET /api/assist/actions`, read into
 * one shape the To do rail and the detail share.
 *
 * Two staged kinds exist: `app_call` (a record write staged from the hosted
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
  record?: { id?: string; revision?: number };
  /** `board_create_issue`: the ticket it made. */
  ticket?: string;
}

export interface StagedWrite {
  id: string;
  kind: "app_call" | "create_issue";
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
  /** The record behind "Open record", when the API names a link. */
  recordLink: string | null;
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
  const field = str(v.field);
  const label = str(v.label) ?? field;
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

function parseAppCall(base: Pick<StagedWrite, "id" | "status" | "createdAt" | "decidedAt">, row: Record<string, unknown>, result: StagedResult | null): StagedWrite | null {
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
  };
}

function parseCreateIssue(base: Pick<StagedWrite, "id" | "status" | "createdAt" | "decidedAt">, row: Record<string, unknown>, result: StagedResult | null): StagedWrite | null {
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
  };
}

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
  };
  if (row.tool === "app_call") return parseAppCall(base, row, result);
  if (row.tool === "board_create_issue") return parseCreateIssue(base, row, result);
  return null;
}

export function parseStagedList(body: unknown): StagedWrite[] | null {
  if (!isRecord(body) || !Array.isArray(body.actions)) return null;
  return body.actions.map(parseStagedRow).filter((r): r is StagedWrite => r !== null);
}

export function outcomeOf(write: StagedWrite): StagedOutcome {
  if (write.status === "pending") return "pending";
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

export function failureReason(write: StagedWrite): string {
  const code = write.result?.code;
  return (code && REASONS[code]) || "the reason is not recognised";
}

type Say = (english: string) => string;
const same: Say = (s) => s;

/** `Atlas (agent)`, `Priya Nair (person)`; null when no origin is recorded (the assistant, in Home). */
export function askedBy(write: StagedWrite, t: Say = same): string | null {
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
