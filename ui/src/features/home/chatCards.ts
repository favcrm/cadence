import { appViewRef, type AppViewRef } from "../canvas/appView";
import { placementOf, type Placement } from "../canvas/canvasLayout";
import { routeParams, withParams, type RouteParams } from "../workspace-apps/screen/routeParams";
import { recordViewId, type RecordViewRef } from "../canvas/recordView";
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
 *
 * CAD-2475 adds `view_ref` (agenticos-v2 AOS-654): a reference to an installed
 * app's screen, opened as a canvas app tab. Same strictness; never content.
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
const VIEWS = ["list", "thread", "table", "detail"] as const;
/** CAD-2543: a detail card is one record; its body and attachment caps (agenticos-v2 AOS-678). */
export const DETAIL_BODY_MAX = 20_000;
export const DETAIL_ATTACHMENTS_MAX = 20;
export const DETAIL_ATTACHMENT_NAME_MAX = 200;
/** Roles only a `detail` card may name. */
const DETAIL_ROLES = ["to", "cc", "body", "attachments"] as const;

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

const ROLE_NAMES = ["title", "subtitle", "snippet", "time", "actor", "status", "badge", "amount", "to", "cc", "body", "attachments"] as const;
type RoleName = (typeof ROLE_NAMES)[number];

/** agenticos-v2 AOS-647: optional layout roles, each naming a `fields[].key`. */
export interface RecordRoles {
  title?: string;
  subtitle?: string;
  snippet?: string;
  time?: string;
  actor?: string;
  status?: string;
  badge?: string;
  amount?: string;
  /** CAD-2543, `detail` view only. */
  to?: string;
  cc?: string;
  body?: string;
  attachments?: string;
  /** The row is the account's own side when `String(row[key])` is in `self`. */
  side?: { key: string; self: string[] };
  /** Raw badge/status value -> display text; null hides it. Display only. */
  labels?: Record<string, string | null>;
  /** A row is unread when its value at `key` (or any element) is in `values`. */
  unread?: { key: string; values: string[] };
}

export interface RecordCardModel {
  toolkit: string;
  tool: string;
  view: RecordView;
  fields: RecordField[];
  roles: RecordRoles | null;
  items: Record<string, Cell>[];
  truncated: boolean;
  /** CAD-2458 header data (agenticos-v2 #2252), each optional. */
  toolLabel: string | null;
  toolkitLabel: string | null;
  account: string | null;
  total: number | null;
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

/** Strict like the contract's `recordCardRolesSchema`; every key must be a field. */
function parseRoles(v: unknown, fieldKeys: string[]): RecordRoles | null {
  if (!isObject(v)) return null;
  const roles: RecordRoles = {};
  for (const [name, value] of Object.entries(v)) {
    if (name === "side") {
      if (!isObject(value) || !exactKeys(value, ["key", "self"])) return null;
      const { key, self } = value;
      if (typeof key !== "string" || !fieldKeys.includes(key)) return null;
      if (!Array.isArray(self) || self.length < 1 || self.length > 10) return null;
      if (!self.every((s) => typeof s === "string" && s.length >= 1 && s.length <= 100)) return null;
      roles.side = { key, self: self as string[] };
    } else if (name === "labels") {
      if (!isObject(value)) return null;
      const entries = Object.entries(value);
      if (entries.length > 40) return null;
      const map: Record<string, string | null> = Object.create(null);
      for (const [raw, shown] of entries) {
        if (raw.length < 1 || raw.length > 100) return null;
        if (shown !== null && (typeof shown !== "string" || shown.length > 100)) return null;
        map[raw] = shown;
      }
      roles.labels = map;
    } else if (name === "unread") {
      if (!isObject(value) || !exactKeys(value, ["key", "values"])) return null;
      const { key, values } = value;
      if (typeof key !== "string" || !fieldKeys.includes(key)) return null;
      if (!Array.isArray(values) || values.length < 1 || values.length > 10) return null;
      if (!values.every((s) => typeof s === "string" && s.length >= 1 && s.length <= 100)) return null;
      roles.unread = { key, values: values as string[] };
    } else if ((ROLE_NAMES as readonly string[]).includes(name)) {
      if (typeof value !== "string" || !FIELD_KEY.test(value) || !fieldKeys.includes(value)) return null;
      roles[name as RoleName] = value;
    } else return null;
  }
  return roles;
}

export function parseRecordRef(entry: ThreadEntry): RecordCardModel | null {
  const p = payloadOf(entry, "record_ref");
  if (!p) return null;
  const hasRoles = Object.prototype.hasOwnProperty.call(p, "roles");
  const base = ["v", "toolkit", "tool", "items", "fields", "view", "truncated"];
  const optional = ["roles", "tool_label", "toolkit_label", "account", "total"];
  const present = optional.filter((k) => Object.prototype.hasOwnProperty.call(p, k));
  if (!exactKeys(p, [...base, ...present])) return null;
  const header = (key: string): string | null | undefined => {
    if (!present.includes(key)) return null;
    const v = p[key];
    return typeof v === "string" && v.trim() !== "" && v.length <= 200 ? plainText(v, 200) : undefined;
  };
  const rawToolLabel = header("tool_label");
  const toolkitLabel = header("toolkit_label");
  const rawAccount = header("account");
  if (rawToolLabel === undefined || toolkitLabel === undefined || rawAccount === undefined) return null;
  const toolLabel = stripToolkitPrefix(rawToolLabel, toolkitLabel);
  const account = rawAccount !== null && rawAccount.trim().toLowerCase() === toolkitLabel?.trim().toLowerCase() ? null : rawAccount;
  if (present.includes("total") && !(Number.isInteger(p.total) && (p.total as number) >= 0)) return null;
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
  const roles = hasRoles ? parseRoles(p.roles, parsedFields.map((f) => f.key)) : null;
  if (hasRoles && !roles) return null;
  if (view === "detail" ? !validDetail(roles, parsedItems) : roles && DETAIL_ROLES.some((r) => roles[r] !== undefined)) return null;
  return {
    toolkit,
    tool,
    view: view as RecordView,
    fields: parsedFields,
    roles,
    items: parsedItems,
    truncated,
    toolLabel,
    toolkitLabel,
    account,
    total: present.includes("total") ? (p.total as number) : null,
  };
}

/** `detail`: one item, `title` and `body` roles, a text body within its cap, at most 20 attachment names (agenticos-v2 AOS-678). */
function validDetail(roles: RecordRoles | null, items: Record<string, Cell>[]): boolean {
  if (!roles || roles.title === undefined || roles.body === undefined || items.length !== 1) return false;
  const body = items[0][roles.body];
  if (body !== undefined && body !== null && (typeof body !== "string" || body.length > DETAIL_BODY_MAX)) return false;
  const names = roles.attachments === undefined ? null : items[0][roles.attachments];
  if (names === null || names === undefined) return true;
  return Array.isArray(names) && names.length <= DETAIL_ATTACHMENTS_MAX && names.every((n) => n.length <= DETAIL_ATTACHMENT_NAME_MAX);
}

/** "Gmail: fetch emails" under toolkit "Gmail" reads "Fetch emails"; unchanged when there is no such prefix. */
export function stripToolkitPrefix(tool: string | null, toolkit: string | null): string | null {
  if (tool === null || toolkit === null || toolkit.trim() === "") return tool;
  const t = tool.trim();
  const k = toolkit.trim();
  if (!t.toLowerCase().startsWith(k.toLowerCase())) return tool;
  const rest = t.slice(k.length);
  const m = /^(?::\s*|\s+)(\S[\s\S]*)$/.exec(rest);
  if (!m) return tool;
  const r = m[1].trim();
  return r === "" ? tool : r[0].toUpperCase() + r.slice(1);
}

/** Plain text for display: control characters out, capped with an ellipsis. */
export function plainText(value: string, max = CELL_MAX_CHARS): string {
  const clean = value.replace(/[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f]/g, "").trim();
  return clean.length > max ? `${clean.slice(0, max - 1)}…` : clean;
}

/** A value placed inside a composer prompt: one line, capped. */
export const inPrompt = (value: string): string => plainText(value.replace(/\s+/g, " "), 120);

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

/** A role-mapped time: the text as given, and the instant when it parses. */
export interface TimeValue {
  text: string;
  instant: number | null;
}

/** A badge/status value to show; `muted` when the tool's `labels` map has no entry for it. */
export interface RoleChip {
  text: string;
  muted: boolean;
}

export interface RoleRow {
  /** The title as given, and its "Name <address>" split (name = title when it has no address). */
  title: string;
  name: string;
  address: string;
  subtitle: string;
  snippet: string;
  time: TimeValue | null;
  /** Status then badge values that display as text. */
  chips: RoleChip[];
  /** A mapped label displayed "★". */
  star: boolean;
  /** The `unread` role matched this row. */
  unread: boolean;
  amount: string;
}

export interface RoleMessage {
  /** The actor's name; "" when absent or when it is the host's `api` marker. */
  actor: string;
  time: TimeValue | null;
  /** Local calendar day of the time (`YYYY-MM-DD`); null when the time does not parse. */
  day: string | null;
  text: string;
  own: boolean;
  /** The actor value was `api`: sent by the assistant after approval. */
  approved: boolean;
}

/** CAD-2543: one record (an email) laid out for the compact card and the reader tab. Plain text throughout. */
export interface RecordDetail {
  /** The subject. */
  title: string;
  /** The sender's name and address (`from`), either may be empty. */
  name: string;
  address: string;
  to: string;
  cc: string;
  time: TimeValue | null;
  /** Plain text with its line breaks, capped. */
  body: string;
  truncated: boolean;
  attachments: string[];
  star: boolean;
  unread: boolean;
  /** Mapped labels that show as text. */
  chips: RoleChip[];
  /** "Gmail", and the connected account when there is one. */
  source: string;
  account: string | null;
}

/** "Name <address>" split into its two parts; any other text is a name alone. */
export function splitAddress(value: string): { name: string; address: string } {
  const m = /^(.*?)\s*<([^<>\s/]{1,200})>$/.exec(value);
  if (!m) return { name: value, address: "" };
  const name = m[1].replace(/^["']+|["']+$/g, "").trim();
  return name ? { name, address: m[2] } : { name: m[2], address: "" };
}

/** The local calendar day of an instant, for day separators. */
export function dayKey(instant: number): string {
  const d = new Date(instant);
  const two = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${two(d.getMonth() + 1)}-${two(d.getDate())}`;
}

export type RecordLayout =
  /** Compact rows: each row is the first columns of one item. */
  | { view: "list"; rows: LabeledValue[][] }
  /** Rows laid out from `payload.roles`. */
  | { view: "role-list"; rows: RoleRow[] }
  /** Bubbles laid out from `payload.roles`. */
  | { view: "role-thread"; messages: RoleMessage[]; contact: string }
  /** One record: a compact card, and the reader tab. */
  | { view: "detail"; detail: RecordDetail }
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

const ISO = /^\d{4}-\d{2}-\d{2}(?:[T ]\d{2}:\d{2}(?::\d{2}(?:\.\d+)?)?(?:Z|[+-]\d{2}:?\d{2})?)?$/;

/** An ISO string or an epoch (seconds, or milliseconds from 1e11) as an instant; else null. */
export function parseInstant(value: Cell | undefined): number | null {
  let ms: number | null = null;
  if (typeof value === "number") ms = value;
  else if (typeof value === "string") {
    const v = value.trim();
    if (/^\d{9,13}$/.test(v)) ms = Number(v);
    else if (ISO.test(v)) {
      const parsed = Date.parse(v.replace(" ", "T"));
      return Number.isFinite(parsed) ? parsed : null;
    }
  }
  if (ms === null || !Number.isFinite(ms) || ms <= 0) return null;
  return ms < 1e11 ? ms * 1000 : ms;
}

/** Display values of a status or badge cell through `roles.labels`: null hides, "★" is the star, unmapped shows muted. */
function chipValues(value: Cell | undefined, roles: RecordRoles, labels?: CellLabels): { chips: RoleChip[]; star: boolean } {
  const raws = Array.isArray(value) ? value.slice(0, TAGS_MAX) : [cellText(value, labels)];
  const chips: RoleChip[] = [];
  let star = false;
  for (const raw of raws) {
    if (raw === "") continue;
    const mapped = roles.labels && Object.hasOwn(roles.labels, raw) ? roles.labels[raw] : undefined;
    if (mapped === null) continue;
    const text = plainText(mapped === undefined ? raw : mapped, 60);
    if (text === "") continue;
    if (mapped !== undefined && text === "★") star = true;
    else chips.push({ text, muted: mapped === undefined });
  }
  return { chips, star };
}

const isUnread = (item: Record<string, Cell>, unread: RecordRoles["unread"]): boolean => {
  if (!unread) return false;
  const v = item[unread.key];
  const seen = Array.isArray(v) ? v : v === undefined || v === null ? [] : [String(v)];
  return seen.some((s) => unread.values.includes(s));
};

const hasAnyRole = (roles: RecordRoles, names: RoleName[]): boolean => names.some((n) => roles[n] !== undefined);

/** The roles layout of a list or thread; null when the roles cannot lay it out. */
function roleLayout(model: RecordCardModel, roles: RecordRoles, labels?: CellLabels): RecordLayout | null {
  const text = (item: Record<string, Cell>, key: string | undefined) =>
    key === undefined ? "" : cellText(item[key], labels);
  const time = (item: Record<string, Cell>): TimeValue | null => {
    if (roles.time === undefined) return null;
    const t = text(item, roles.time);
    return t ? { text: t, instant: parseInstant(item[roles.time]) } : null;
  };
  if (model.view === "detail") {
    const item = model.items[0];
    if (!item || roles.title === undefined || roles.body === undefined) return null;
    // Labels come from the status/badge roles, else from the field the unread rule reads.
    const labelKeys = [roles.status, roles.badge].filter((k): k is string => k !== undefined);
    if (labelKeys.length === 0 && roles.unread) labelKeys.push(roles.unread.key);
    const shown = labelKeys.map((k) => chipValues(item[k], roles, labels));
    const names = roles.attachments === undefined ? null : item[roles.attachments];
    const from = splitAddress(text(item, roles.actor));
    const body = item[roles.body];
    return {
      view: "detail",
      detail: {
        title: text(item, roles.title),
        ...from,
        to: text(item, roles.to),
        cc: text(item, roles.cc),
        time: time(item),
        body: typeof body === "string" ? plainText(body, DETAIL_BODY_MAX) : "",
        truncated: model.truncated,
        attachments: Array.isArray(names) ? names.map((n) => plainText(n, DETAIL_ATTACHMENT_NAME_MAX)).filter(Boolean) : [],
        star: shown.some((v) => v.star),
        unread: isUnread(item, roles.unread),
        chips: shown.flatMap((v) => v.chips),
        source: model.toolkitLabel ?? model.toolkit,
        account: model.account,
      },
    };
  }
  if (model.view === "thread") {
    if (!hasAnyRole(roles, ["snippet", "title", "subtitle"])) return null;
    const side = roles.side;
    const messages = model.items
      .map((item) => {
        const own = side !== undefined && item[side.key] != null && side.self.includes(String(item[side.key]));
        const rawActor = text(item, roles.actor);
        const approved = rawActor === "api";
        const when = time(item);
        return {
          actor: approved ? "" : splitAddress(rawActor).name,
          time: when,
          day: when?.instant != null ? dayKey(when.instant) : null,
          text: text(item, roles.snippet) || text(item, roles.title) || text(item, roles.subtitle),
          own,
          approved,
        };
      })
      .filter((m) => m.text !== "");
    const contact = messages.find((m) => !m.own && m.actor !== "")?.actor ?? "";
    return messages.length ? { view: "role-thread", messages, contact } : null;
  }
  if (model.view === "list") {
    if (!hasAnyRole(roles, ["title", "subtitle", "snippet", "time", "status", "badge", "amount"])) return null;
    const rows = model.items
      .map((item): RoleRow => {
        const title = text(item, roles.title);
        const shown = [roles.status, roles.badge].map((k) => (k ? chipValues(item[k], roles, labels) : { chips: [], star: false }));
        return {
          title,
          ...splitAddress(title),
          subtitle: text(item, roles.subtitle),
          snippet: text(item, roles.snippet),
          time: time(item),
          chips: shown.flatMap((v) => v.chips),
          star: shown.some((v) => v.star),
          unread: isUnread(item, roles.unread),
          amount: text(item, roles.amount),
        };
      })
      .filter((r) => r.title || r.subtitle || r.snippet || r.time || r.chips.length || r.star || r.amount);
    return rows.length ? { view: "role-list", rows } : null;
  }
  return null;
}

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
  if (model.roles) {
    const withRoles = roleLayout(model, model.roles, labels);
    if (withRoles) return withRoles;
  }
  if (model.view === "detail") return null;
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

/** The canvas reader tab for a stored `detail` card, from the entry alone (no request). Null when it is not one. */
export function recordCardView(entry: ThreadEntry): RecordViewRef | null {
  const model = parseRecordRef(entry);
  const layout = model?.view === "detail" ? recordLayout(model) : null;
  if (layout?.view !== "detail") return null;
  const { detail } = layout;
  return { key: `e${entry.seq}`, id: recordViewId(`e${entry.seq}`), title: detail.title || detail.source, detail };
}

// ---- view_ref (CAD-2475; agenticos-v2 AOS-654 `viewRefPayloadSchema`) ----

const VIEW_APP = /^[A-Za-z0-9][A-Za-z0-9_.-]{0,63}$/;
const VIEW_ID = /^[A-Za-z0-9_-]{1,128}$/;
const VIEW_TAG = /^[A-Za-z0-9_-]{1,64}$/;
const VIEW_SECTION = /^[a-z0-9][a-z0-9-]{0,31}$/;
const VIEW_DECLARED = /^[a-z0-9_-]{1,32}$/;
const NO_CONTROLS = /^[^\u0000-\u001f\u007f-\u009f]*$/;

export interface ViewCardModel {
  app: string;
  appLabel: string;
  installId: string;
  screenTag: string;
  section: string;
  recordId: string | null;
  title: string;
  /** CAD-2555 — a declared app view: its id, label and params (grammar only; the host checked the declaration). */
  view?: string;
  viewLabel?: string;
  params?: RouteParams;
  /** CAD-2635 — where Nova asks for the view; a value the board does not know is dropped, never refused. */
  placement?: Placement;
}

const plainField = (v: unknown, max: number): v is string =>
  typeof v === "string" && v.length >= 1 && v.length <= max && NO_CONTROLS.test(v);

/** Strict like the contract: unknown keys, bad grammar or a missing key yield null (the text line shows). */
export function parseViewRef(entry: ThreadEntry): ViewCardModel | null {
  const p = payloadOf(entry, "view_ref");
  if (!p) return null;
  const hasRecord = Object.prototype.hasOwnProperty.call(p, "record_id");
  const optional = ["view", "view_label", "params", "placement"].filter((key) => Object.prototype.hasOwnProperty.call(p, key));
  if (!exactKeys(p, ["v", "app", "app_label", "install_id", "screen_tag", "section", "title", ...(hasRecord ? ["record_id"] : []), ...optional])) return null;
  const { app, app_label, install_id, screen_tag, section, record_id, title, view, view_label, params, placement } = p;
  if (typeof app !== "string" || !VIEW_APP.test(app)) return null;
  if (!plainField(app_label, 60) || !plainField(title, 120)) return null;
  if (typeof install_id !== "string" || !VIEW_ID.test(install_id)) return null;
  if (typeof screen_tag !== "string" || !VIEW_TAG.test(screen_tag)) return null;
  if (typeof section !== "string" || !VIEW_SECTION.test(section)) return null;
  if (hasRecord && record_id !== null && (typeof record_id !== "string" || !VIEW_ID.test(record_id))) return null;
  if (optional.includes("view") && (typeof view !== "string" || !VIEW_DECLARED.test(view))) return null;
  if (optional.includes("view_label") && !plainField(view_label, 60)) return null;
  if (optional.includes("params") && !routeParams(params)) return null;
  return {
    app, appLabel: app_label, installId: install_id, screenTag: screen_tag, section, recordId: typeof record_id === "string" ? record_id : null, title,
    ...(optional.includes("view") ? { view: view as string } : {}),
    ...(optional.includes("view_label") ? { viewLabel: view_label as string } : {}),
    ...(optional.includes("params") ? { params: params as RouteParams } : {}),
    ...(placementOf(placement) ? { placement: placementOf(placement) } : {}),
  };
}

/** Where a card opens its tab: the app pane's own context when the card is for that install, else the install's default. */
export interface ViewScope {
  installId: string | null;
  contextId: string;
  /** The app pane's own page: moves it to a route through the full-page app's route path. Absent on Home. */
  showHere?: (route: AppViewRef["route"]) => void;
}
/** A card for the installation whose page is on screen moves that page; it opens no canvas tab. */
export function viewHere(model: ViewCardModel, scope: ViewScope): boolean {
  return scope.showHere !== undefined && scope.installId === model.installId;
}
export const HOME_VIEW_SCOPE: ViewScope = { installId: null, contextId: "default" };

/** The canvas reference of a card, or null when the frame would refuse it (grammar the tab is stricter about). */
export function viewCardAppView(model: ViewCardModel, scope: ViewScope = HOME_VIEW_SCOPE): AppViewRef | null {
  const contextId = scope.installId === model.installId ? scope.contextId : "default";
  const ref = appViewRef({
    installId: model.installId,
    tag: model.screenTag,
    contextId,
    route: { section: model.section, view: "list", record_id: model.recordId, segment_id: null, ...withParams(model.params) },
    title: model.title,
    appLabel: model.appLabel,
    ...(model.viewLabel ? { viewLabel: model.viewLabel } : {}),
    // A placeholder: the tab builds the real link from the installation it reads.
    fromCard: true,
    href: `/apps/${model.app}`,
  });
  return ref;
}
