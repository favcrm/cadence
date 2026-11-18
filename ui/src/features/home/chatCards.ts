import type { ThreadEntry } from "./thread";

/**
 * CAD-2443: the host-written `connect_ref` and `record_ref` chat card entries
 * (agenticos-v2 AOS-647; schemas in `packages/contracts/src/chat-cards.ts`).
 * Pure parsing and layout — no React — unit-tested against the copied
 * fixtures (tests/fixtures/chat-cards).
 *
 * The narrowing is strict, like the contract's `strictObject`: an entry that
 * does not match exactly yields null and the caller shows the entry's `text`
 * as a system line, never a half-rendered card. Every value in a record card
 * is plain text (React escapes it; there is no Markdown and no link), capped.
 * A card never carries a URL: the Connect button uses the board's own flow.
 */

export const RECORD_MAX_ITEMS = 50;
export const RECORD_MAX_FIELDS = 40;
/** Display caps: a cell's characters, tags shown, columns of a list row. */
export const CELL_MAX_CHARS = 300;
export const TAGS_MAX = 8;
export const LIST_COLUMNS_MAX = 4;

const TOOLKIT = /^[a-z_][a-z0-9_-]{0,63}$/;
const TOOL = /^[a-z][a-z0-9_]{1,63}$/;
const FIELD_KEY = /^[A-Za-z][A-Za-z0-9_]{0,63}$/;
const FIELD_TYPES = ["string", "number", "integer", "boolean", "string_array"] as const;
const VIEWS = ["list", "thread", "table"] as const;

export type RecordFieldType = (typeof FIELD_TYPES)[number];
export type RecordView = (typeof VIEWS)[number];
type Cell = string | number | boolean | string[] | null;

export interface ConnectCardModel {
  toolkit: string;
  label: string;
}

export interface RecordField {
  key: string;
  label: string;
  type: RecordFieldType;
  untrusted: boolean;
}

export interface RecordCardModel {
  toolkit: string;
  tool: string;
  view: RecordView;
  fields: RecordField[];
  items: Record<string, Cell>[];
  truncated: boolean;
}

const isObject = (v: unknown): v is Record<string, unknown> =>
  v !== null && typeof v === "object" && !Array.isArray(v);

/** Only the listed keys, nothing else (the contract is a strict object). */
const exactKeys = (v: Record<string, unknown>, keys: string[]): boolean => {
  const own = Object.keys(v);
  return own.length === keys.length && keys.every((k) => Object.prototype.hasOwnProperty.call(v, k));
};

const isCell = (v: unknown): v is Cell =>
  v === null ||
  typeof v === "string" ||
  typeof v === "boolean" ||
  (typeof v === "number" && Number.isFinite(v)) ||
  (Array.isArray(v) && v.every((s) => typeof s === "string"));

/** The entry's payload when it is a `v: 1` object of the given kind. */
function payloadOf(entry: ThreadEntry, kind: string): Record<string, unknown> | null {
  if (entry.kind !== kind || entry.role !== "agent") return null;
  const p = entry.payload;
  return isObject(p) && p.v === 1 ? p : null;
}

export function parseConnectRef(entry: ThreadEntry): ConnectCardModel | null {
  const p = payloadOf(entry, "connect_ref");
  if (!p || !exactKeys(p, ["v", "toolkit", "toolkit_label"])) return null;
  const { toolkit, toolkit_label: label } = p;
  if (typeof toolkit !== "string" || !TOOLKIT.test(toolkit)) return null;
  if (typeof label !== "string" || label.trim() === "" || label.length > 200) return null;
  return { toolkit, label: label.trim() };
}

function parseField(v: unknown): RecordField | null {
  if (!isObject(v) || !exactKeys(v, ["key", "label", "type", "untrusted"])) return null;
  const { key, label, type, untrusted } = v;
  if (typeof key !== "string" || !FIELD_KEY.test(key)) return null;
  if (typeof label !== "string" || label.length > 100) return null;
  if (!(FIELD_TYPES as readonly unknown[]).includes(type)) return null;
  if (typeof untrusted !== "boolean") return null;
  return { key, label: label || key, type: type as RecordFieldType, untrusted };
}

export function parseRecordRef(entry: ThreadEntry): RecordCardModel | null {
  const p = payloadOf(entry, "record_ref");
  if (!p || !exactKeys(p, ["v", "toolkit", "tool", "items", "fields", "view", "truncated"])) return null;
  const { toolkit, tool, items, fields, view, truncated } = p;
  if (typeof toolkit !== "string" || !TOOLKIT.test(toolkit)) return null;
  if (typeof tool !== "string" || !TOOL.test(tool)) return null;
  if (!(VIEWS as readonly unknown[]).includes(view)) return null;
  if (typeof truncated !== "boolean") return null;
  if (!Array.isArray(fields) || fields.length > RECORD_MAX_FIELDS) return null;
  if (!Array.isArray(items) || items.length > RECORD_MAX_ITEMS) return null;
  const parsedFields: RecordField[] = [];
  for (const f of fields) {
    const field = parseField(f);
    if (!field) return null;
    parsedFields.push(field);
  }
  const parsedItems: Record<string, Cell>[] = [];
  for (const item of items) {
    if (!isObject(item)) return null;
    for (const v of Object.values(item)) if (!isCell(v)) return null;
    parsedItems.push(item as Record<string, Cell>);
  }
  return {
    toolkit,
    tool,
    view: view as RecordView,
    fields: parsedFields,
    items: parsedItems,
    truncated,
  };
}

/** Plain text for display: control characters out, capped with an ellipsis. */
export function plainText(value: string, max = CELL_MAX_CHARS): string {
  const clean = value.replace(/[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f]/g, "").trim();
  return clean.length > max ? `${clean.slice(0, max - 1)}…` : clean;
}

export interface CellLabels {
  yes: string;
  no: string;
}

/** One value as plain text; "" when absent or null. */
export function cellText(value: Cell | undefined, labels: CellLabels = { yes: "Yes", no: "No" }): string {
  if (value === undefined || value === null) return "";
  if (typeof value === "boolean") return value ? labels.yes : labels.no;
  if (typeof value === "number") return String(value);
  if (Array.isArray(value)) {
    const shown = value.slice(0, TAGS_MAX).map((s) => plainText(s, 60)).filter(Boolean);
    const more = value.length - TAGS_MAX;
    return shown.join(", ") + (more > 0 ? ` +${more}` : "");
  }
  return plainText(value);
}

export interface LabeledValue {
  label: string;
  value: string;
}

export type RecordLayout =
  /** Compact rows: each row is the first columns of one item. */
  | { view: "list"; rows: LabeledValue[][] }
  /** Message-style rows. `outbound` is true when the board's side sent it. */
  | { view: "thread"; messages: { sender: string; time: string; text: string; outbound: boolean }[] }
  /** Key/value lines of one item. */
  | { view: "table"; pairs: LabeledValue[] };

const SENDER_KEYS = ["sender", "from", "author", "direction"];
const TIME_KEYS = ["sent_at", "created_at", "created", "time", "timestamp", "date"];
const TEXT_KEYS = ["text", "body", "message", "content"];
const OUT = new Set(["out", "outbound", "sent", "agent", "me"]);

const firstKey = (fields: RecordField[], names: string[]): string | null =>
  names.find((n) => fields.some((f) => f.key === n)) ?? null;

/**
 * The model laid out for its view, or null when a view cannot be laid out
 * (a thread with no text field, an empty table): the caller falls back to
 * the entry's text.
 */
export function recordLayout(model: RecordCardModel, labels?: CellLabels): RecordLayout | null {
  const text = (item: Record<string, Cell>, key: string | null) =>
    key === null ? "" : cellText(item[key], labels);
  const pairs = (item: Record<string, Cell>, fields: RecordField[]): LabeledValue[] =>
    fields
      .map((f) => ({ label: plainText(f.label, 100), value: text(item, f.key) }))
      .filter((p) => p.value !== "");
  if (model.view === "thread") {
    const textKey = firstKey(model.fields, TEXT_KEYS);
    if (!textKey) return null;
    const senderKey = firstKey(model.fields, SENDER_KEYS);
    const timeKey = firstKey(model.fields, TIME_KEYS);
    return {
      view: "thread",
      messages: model.items.map((item) => {
        const sender = text(item, senderKey);
        return {
          sender,
          time: text(item, timeKey),
          text: text(item, textKey),
          outbound: OUT.has(sender.toLowerCase()),
        };
      }),
    };
  }
  if (model.view === "table") {
    const rows = model.items[0] ? pairs(model.items[0], model.fields) : [];
    return rows.length ? { view: "table", pairs: rows } : null;
  }
  const columns = model.fields.slice(0, LIST_COLUMNS_MAX);
  return { view: "list", rows: model.items.map((item) => pairs(item, columns)) };
}
