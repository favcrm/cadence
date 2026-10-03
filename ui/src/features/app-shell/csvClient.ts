import { ApiError } from "../../lib/api";
import { sessionHeaders } from "../../lib/sessionKey";
import { assertCleanBody, assertScope, listPath, type HostScope } from "./hostActions";

/**
 * Customer CSV preview/import client for the CRM Customers section
 * (CAD-865 wiring the CAD-779 host verbs into the board).
 *
 * The HTTP peer serves exactly two routes under the record collection —
 * `POST …/records/csv-preview` and `POST …/records/csv-import` — relayed
 * to the daemon's `app_record_csv_preview` / `app_record_csv_import`
 * verbs. Those record IDs are reserved: a record named `csv-preview` or
 * `csv-import` is unaddressable over HTTP by design, so no bulk POST can
 * ever masquerade as a single-record write.
 *
 * The install/context IDs come only from the URL-bound scope, never from
 * the CSV or a form field. Bodies serialize exactly the peer's grammar —
 * `{csv_text}` on preview; `{csv_text, preview_token, request_id,
 * decisions?}` on import — and refuse anything else client-side, before
 * fetch. The token binds the previewed bytes; the daemon replans the
 * same text at import and refuses a stale preview outright. Consent is
 * never inferred anywhere in this path: absent cells arrive `unknown`.
 */

/** Body keys the HTTP peer accepts per verb. Everything else is forged. */
const PREVIEW_KEYS = ["csv_text"] as const;
const IMPORT_KEYS = ["csv_text", "preview_token", "request_id", "decisions"] as const;
// CAD-1016: the operator's explicit confirm of the byte-bound previewed
// plan — mints the durable intent the assistant import redeems. The body
// carries the exact CSV + the reviewed decisions so the host can store
// the immutable plan; the agent never carries bytes. Strict keys — never
// credentials or scope.
const CONFIRM_KEYS = ["request_id", "preview_token", "decisions_digest", "csv_text", "decisions"] as const;

/** The daemon's bounds for this surface (CSV_TEXT_BYTES / CSV_ROWS_MAX /
 *  body caps): refuse early, before the wire. */
export const CSV_MAX_BYTES = 256 * 1024;
export const CSV_MAX_ROWS = 500;
export const CSV_MAX_COLUMNS = 16;

/** The columns the daemon's plan understands; anything else refuses. */
const CSV_COLUMNS = new Set([
  "record_id",
  "display_name",
  "email",
  "phone",
  "tags",
  "source",
  "consent_email",
  "consent_sms",
  "expected_revision",
]);

/** `sha256:` plus 64 lowercase hex — the preview token's stored shape. */
const TOKEN_PATTERN = /^sha256:[0-9a-f]{64}$/;

function identifier(id: string): boolean {
  return id.length >= 1 && id.length <= 128 && /^[A-Za-z0-9_-]+$/.test(id);
}

/** Per-row plan entry as the daemon reports it — `decision` drives the
 *  operator's import control per row. */
export type CsvDecision = "create" | "update" | "skip" | "needs_revision" | "error";

export interface CsvPlanRow {
  row: number;
  recordId: string;
  decision: CsvDecision;
  /** The CSV's own `expected_revision` cell, when parseable. */
  expectedRevision: number | null;
  /** The live record's current revision — the value an operator
   *  confirms to turn `needs_revision` into an `update`. */
  currentRevision: number | null;
  /** Planned profile as the server parsed it (null on error rows). */
  profile: Record<string, unknown> | null;
  /** Fixed refusal codes — never cell content. */
  errors: string[];
  reason: string | null;
  /** Present only when a new id collides with a known address. */
  duplicateOf: string | null;
}

export interface CsvPreview {
  previewToken: string;
  rowCount: number;
  summary: {
    create: number;
    update: number;
    skip: number;
    needsRevision: number;
    error: number;
  };
  rows: CsvPlanRow[];
}

/** One row's import outcome — durable per-row receipt from the server. */
export interface CsvImportRow {
  row: number;
  recordId: string;
  outcome: string;
  reason: string | null;
}

export interface CsvImportReceipt {
  requestId: string;
  previewToken: string;
  replayed: boolean;
  summary: { applied: number; skipped: number; failed: number };
  rows: CsvImportRow[];
}

/** The host-minted one-use confirm receipt the assistant import redeems
 *  (`csv-confirm` returns `confirm_token`/`state`/`request_id`). */
export interface CsvConfirmReceipt {
  confirmToken: string;
  state: string;
  requestId: string;
}

/** The operator's per-row import choice. `update` rows carry the
 *  revision they confirmed; `create`/`skip` never do. */
export interface CsvRowDecision {
  row: number;
  action: "create" | "update" | "skip";
  expectedRevision?: number;
}

const DECISIONS = new Set<CsvDecision>(["create", "update", "skip", "needs_revision", "error"]);

function asDecision(value: unknown): CsvDecision {
  if (typeof value === "string" && (DECISIONS as Set<string>).has(value)) {
    return value as CsvDecision;
  }
  throw new ApiError("The server returned an invalid CSV receipt", 502);
}

function asPlanRow(value: unknown): CsvPlanRow {
  const row = value as Record<string, unknown> | null;
  if (
    !row ||
    typeof row.row !== "number" ||
    typeof row.record_id !== "string" ||
    row.decision === undefined
  ) {
    throw new ApiError("The server returned an invalid CSV receipt", 502);
  }
  return {
    row: row.row,
    recordId: row.record_id,
    decision: asDecision(row.decision),
    expectedRevision: typeof row.expected_revision === "number" ? row.expected_revision : null,
    currentRevision: typeof row.current_revision === "number" ? row.current_revision : null,
    profile:
      row.profile !== null && typeof row.profile === "object"
        ? (row.profile as Record<string, unknown>)
        : null,
    errors: Array.isArray(row.errors) ? row.errors.filter((e): e is string => typeof e === "string") : [],
    reason: typeof row.reason === "string" ? row.reason : null,
    duplicateOf: typeof row.duplicate_of === "string" ? row.duplicate_of : null,
  };
}

function countField(summary: Record<string, unknown>, key: string): number {
  const value = summary[key];
  if (typeof value !== "number" || value < 0 || !Number.isInteger(value)) {
    throw new ApiError("The server returned an invalid CSV receipt", 502);
  }
  return value;
}

/**
 * Client-side shape checks that refuse before the wire what the daemon
 * refuses anyway — the server remains the authority; these only keep
 * obviously malformed text and envelopes off the wire.
 */
export function checkCsvText(csvText: string): void {
  if (csvText.trim() === "") {
    throw new ApiError("the CSV is empty — pick a file or paste rows first", 400);
  }
  if (new TextEncoder().encode(csvText).length > CSV_MAX_BYTES) {
    throw new ApiError("the CSV exceeds its 256 KiB bound", 400);
  }
  const header = csvText.split("\n", 1)[0]?.replace(/\r$/, "") ?? "";
  const columns = header.split(",").map((name) => name.trim().replace(/^"|"$/g, ""));
  if (columns.length > CSV_MAX_COLUMNS) {
    throw new ApiError("the CSV has more columns than the importer accepts", 400);
  }
  for (const name of columns) {
    if (!CSV_COLUMNS.has(name)) {
      throw new ApiError(
        "the CSV header names a column the importer does not know — allowed: record_id, display_name, email, phone, tags, source, consent_email, consent_sms, expected_revision",
        400,
      );
    }
  }
  if (!columns.includes("display_name")) {
    throw new ApiError("the CSV requires a display_name column", 400);
  }
}

async function post<T>(path: string, body: Record<string, unknown>): Promise<T> {
  const response = await fetch(path, {
    method: "POST",
    credentials: "same-origin",
    cache: "no-store",
    headers: {
      "Content-Type": "application/json",
      "X-Cadence-Board": "1",
      ...sessionHeaders(),
    },
    body: JSON.stringify(body),
  });
  const value = await response.json().catch(() => null);
  if (!response.ok) {
    throw new ApiError(value?.error ?? `${response.status} ${response.statusText}`, response.status);
  }
  if (value === null) throw new ApiError("The server returned an invalid CSV receipt", 502);
  return value as T;
}

export const csvActions = {
  /** `POST …/records/csv-preview` — read-only plan; writes nothing.
   *  The token in the receipt binds these exact bytes for import. */
  async preview(scope: HostScope, csvText: string): Promise<CsvPreview> {
    assertScope(scope);
    checkCsvText(csvText);
    const body: Record<string, unknown> = { csv_text: csvText };
    assertCleanBody(body, PREVIEW_KEYS);
    const value = await post<Record<string, unknown>>(`${listPath(scope)}/csv-preview`, body);
    const summary = (value.summary ?? {}) as Record<string, unknown>;
    if (typeof value.preview_token !== "string" || !Array.isArray(value.rows)) {
      throw new ApiError("The server returned an invalid CSV receipt", 502);
    }
    return {
      previewToken: value.preview_token,
      rowCount: typeof value.row_count === "number" ? value.row_count : value.rows.length,
      summary: {
        create: countField(summary, "create"),
        update: countField(summary, "update"),
        skip: countField(summary, "skip"),
        needsRevision: countField(summary, "needs_revision"),
        error: countField(summary, "error"),
      },
      rows: (value.rows as unknown[]).map(asPlanRow),
    };
  },

  /** `POST …/records/csv-import` — commits the previewed plan. The
   *  request id reserves a pending receipt before any row mutates; a
   *  retry with the same id replays the stored outcome. Decisions, when
   *  given, must address known rows with their plan-consistent actions. */
  async import(
    scope: HostScope,
    csvText: string,
    previewToken: string,
    requestId: string,
    decisions?: CsvRowDecision[],
  ): Promise<CsvImportReceipt> {
    assertScope(scope);
    checkCsvText(csvText);
    if (!TOKEN_PATTERN.test(previewToken)) {
      throw new ApiError("the CSV preview token is missing or malformed — preview again", 400);
    }
    if (!identifier(requestId)) {
      throw new ApiError("the import request id is out of bounds", 400);
    }
    const body: Record<string, unknown> = {
      csv_text: csvText,
      preview_token: previewToken,
      request_id: requestId,
    };
    if (decisions !== undefined) {
      if (decisions.length === 0 || decisions.length > CSV_MAX_ROWS) {
        throw new ApiError("the CSV decisions are out of bounds", 400);
      }
      const seen = new Set<number>();
      body.decisions = decisions.map((item) => {
        if (!Number.isInteger(item.row) || item.row < 1 || seen.has(item.row)) {
          throw new ApiError("a CSV decision targets an unknown or repeated row", 400);
        }
        seen.add(item.row);
        if (!["create", "update", "skip"].includes(item.action)) {
          throw new ApiError("a CSV decision names an unknown action", 400);
        }
        const entry: Record<string, unknown> = { row: item.row, action: item.action };
        if (item.expectedRevision !== undefined) {
          if (!Number.isInteger(item.expectedRevision) || item.expectedRevision < 1) {
            throw new ApiError("an update decision needs a positive expected revision", 400);
          }
          entry.expected_revision = item.expectedRevision;
        }
        return entry;
      });
    }
    assertCleanBody(body, IMPORT_KEYS);
    const value = await post<Record<string, unknown>>(`${listPath(scope)}/csv-import`, body);
    const summary = (value.summary ?? {}) as Record<string, unknown>;
    const rows = Array.isArray(value.rows) ? value.rows : null;
    if (typeof value.request_id !== "string" || rows === null) {
      throw new ApiError("The server returned an invalid CSV receipt", 502);
    }
    return {
      requestId: value.request_id,
      previewToken: typeof value.preview_token === "string" ? value.preview_token : "",
      replayed: value.replayed === true,
      summary: {
        applied: countField(summary, "applied"),
        skipped: countField(summary, "skipped"),
        failed: countField(summary, "failed"),
      },
      rows: rows.map((raw) => {
        const row = raw as Record<string, unknown>;
        return {
          row: typeof row.row === "number" ? row.row : -1,
          recordId: typeof row.record_id === "string" ? row.record_id : "",
          outcome: typeof row.outcome === "string" ? row.outcome : "unknown",
          reason: typeof row.reason === "string" ? row.reason : null,
        };
      }),
    };
  },

  /** `POST …/records/csv-confirm` — the operator's explicit, host-side
   *  confirm of the exact previewed plan. Mints the durable intent: the
   *  host stores `csv_text` + the reviewed `decisions` bound to the byte
   *  token + request id + decisions digest, and returns the one-use
   *  `confirm_token` nonce the agent's scoped turn redeems via
   *  csv-assistant-import. The bytes never ride the text-only chat — the
   *  agent resolves the plan by request id + nonce. `decisions` is the
   *  wire `[{row, action, expected_revision?}]` the daemon re-digests. */
  async confirm(
    scope: HostScope,
    csvText: string,
    previewToken: string,
    requestId: string,
    decisionsDigest: string,
    decisions?: { row: number; action: string; expected_revision?: number }[],
  ): Promise<CsvConfirmReceipt> {
    assertScope(scope);
    checkCsvText(csvText);
    if (!TOKEN_PATTERN.test(previewToken)) {
      throw new ApiError("the CSV preview token is missing or malformed — preview again", 400);
    }
    if (!identifier(requestId)) {
      throw new ApiError("the import request id is out of bounds", 400);
    }
    if (!TOKEN_PATTERN.test(decisionsDigest)) {
      throw new ApiError("the CSV decisions digest is missing or malformed", 400);
    }
    const body: Record<string, unknown> = {
      request_id: requestId,
      preview_token: previewToken,
      decisions_digest: decisionsDigest,
      csv_text: csvText,
    };
    if (decisions !== undefined) {
      if (decisions.length === 0 || decisions.length > CSV_MAX_ROWS) {
        throw new ApiError("the CSV decisions are out of bounds", 400);
      }
      const seen = new Set<number>();
      body.decisions = decisions.map((item) => {
        if (!Number.isInteger(item.row) || item.row < 1 || seen.has(item.row)) {
          throw new ApiError("a CSV decision targets an unknown or repeated row", 400);
        }
        seen.add(item.row);
        if (!["create", "update", "skip"].includes(item.action)) {
          throw new ApiError("a CSV decision names an unknown action", 400);
        }
        const entry: Record<string, unknown> = { row: item.row, action: item.action };
        if (item.expected_revision !== undefined) {
          if (!Number.isInteger(item.expected_revision) || item.expected_revision < 1) {
            throw new ApiError("a CSV decision carries a malformed expected revision", 400);
          }
          entry.expected_revision = item.expected_revision;
        }
        return entry;
      });
    }
    assertCleanBody(body, CONFIRM_KEYS);
    const value = await post<Record<string, unknown>>(`${listPath(scope)}/csv-confirm`, body);
    if (typeof value.confirm_token !== "string" || typeof value.request_id !== "string") {
      throw new ApiError("The server returned an invalid CSV confirm receipt", 502);
    }
    return {
      confirmToken: value.confirm_token,
      state: typeof value.state === "string" ? value.state : "unknown",
      requestId: value.request_id,
    };
  },
};

/** A fresh per-import request id inside the daemon's identifier grammar. */
export function newImportRequestId(): string {
  const bytes = new Uint8Array(12);
  crypto.getRandomValues(bytes);
  return `csv-${Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("")}`;
}

/** Strict positive-integer parse: the whole string must be one or
 *  more decimal digits — no signs, whitespace, exponents, decimals or
 *  any leading/trailing character — matching the daemon's
 *  `as_u64`/`>0` check which never trims. `undefined` when malformed. */
export function parseRevision(text: string | undefined): number | undefined {
  if (text === undefined || !/^[0-9]+$/.test(text)) return undefined;
  const value = Number(text);
  if (!Number.isSafeInteger(value) || value < 1) return undefined;
  return value;
}

/** The decision list the import verb accepts. Error rows are omitted
 *  entirely — the daemon refuses a decision that targets an error row,
 *  so omitting (never an explicit skip) is the only safe send. `apply`
 *  keeps the plan's action; `skip` drops a row the plan said it could
 *  apply. */
export type CsvRowChoice = "apply" | "skip";

export function csvPlanChoice(row: CsvPlanRow): CsvRowChoice {
  return row.decision === "error" || row.decision === "skip" ? "skip" : "apply";
}

export function buildCsvDecisions(
  rows: CsvPlanRow[],
  choices: Map<number, CsvRowChoice>,
  revisions: Map<number, string>,
): CsvRowDecision[] {
  const decisions: CsvRowDecision[] = [];
  for (const row of rows) {
    // An error row is never decided — the daemon refuses it outright.
    if (row.decision === "error") continue;
    const choice = choices.get(row.row) ?? csvPlanChoice(row);
    if (choice === "skip") {
      decisions.push({ row: row.row, action: "skip" });
      continue;
    }
    switch (row.decision) {
      case "create":
        decisions.push({ row: row.row, action: "create" });
        break;
      case "update":
        decisions.push({
          row: row.row,
          action: "update",
          expectedRevision: row.expectedRevision ?? undefined,
        });
        break;
      case "needs_revision": {
        const revision = parseRevision(revisions.get(row.row));
        if (revision === undefined) continue; // readiness keeps it off the wire
        decisions.push({ row: row.row, action: "update", expectedRevision: revision });
        break;
      }
      default:
        break;
    }
  }
  return decisions;
}

/** How many rows will actually write — the apply-chosen create,
 *  update and revision-confirmed rows. Plan skips, operator skips and
 *  error rows are never counted as approved writes. */
export function csvApplyCount(
  rows: CsvPlanRow[],
  choices: Map<number, CsvRowChoice>,
): number {
  return rows.filter((row) => {
    if (row.decision === "error" || row.decision === "skip") return false;
    return (choices.get(row.row) ?? csvPlanChoice(row)) === "apply";
  }).length;
}

/** Whether the current choices can be sent: every applied
 *  `needs_revision` row needs a strictly positive integer revision. */
export function csvDecisionsReady(
  rows: CsvPlanRow[],
  choices: Map<number, CsvRowChoice>,
  revisions: Map<number, string>,
): boolean {
  return rows.every((row) => {
    if (row.decision !== "needs_revision") return true;
    if ((choices.get(row.row) ?? "apply") !== "apply") return true;
    return parseRevision(revisions.get(row.row)) !== undefined;
  });
}
