/**
 * CAD-2441 (card system phase 1, CAD-2437): what a finished turn read and
 * what it staged, derived only from the turn's own recorded tool calls and
 * results — never from the model's prose, so an answer cannot invent a
 * source or a write. No React, so the UI tests load it directly.
 */
import type { StagedWrite } from "../decisions/stagedWrite";
import type { ThreadEntry } from "./thread";

export type TurnSourceKind = "wiki" | "issue" | "attachment" | "artifact" | "web" | "app";

export interface TurnSource {
  kind: TurnSourceKind;
  /** Stable identity for deduplication. */
  key: string;
  /** What the chip names: a path, an id, a host or an app. */
  label: string;
  /** The board identity the chip opens (for an app record, the record id). */
  target: string;
  /** An app record's app key. */
  app?: string;
}

/** At most this many chips; the rest are "+N more". */
export const SOURCES_SHOWN = 8;

/**
 * A recorded call. Two forms exist: the v2 runtime records `name {json}`;
 * the native runtime records `store::threads::tool_summary`'s one line,
 * `name: <detail>`. The detail is the first of command, file_path, path,
 * pattern, url, query, description and prompt, or `{sorted keys}`
 * (CAD-2456). A summary's detail arrives as `summary`; a keys-only summary
 * carries none. Null when the text is neither form.
 */
export function parseToolCall(text: string): { name: string; args: Record<string, unknown>; summary?: string } | null {
  const trimmed = text.trim();
  const summary = /^([a-z][a-z0-9_]*): (.+)$/s.exec(trimmed);
  if (summary) {
    const detail = summary[2]!.trim();
    // `{a, b}` names keys only, never values.
    return { name: summary[1]!, args: {}, ...(detail.startsWith("{") ? {} : { summary: detail }) };
  }
  const space = trimmed.indexOf(" ");
  const name = space < 0 ? trimmed : trimmed.slice(0, space);
  if (!/^[a-z][a-z0-9_]*$/.test(name)) return null;
  if (space < 0) return { name, args: {} };
  try {
    const value: unknown = JSON.parse(trimmed.slice(space + 1));
    return value && typeof value === "object" && !Array.isArray(value)
      ? { name, args: value as Record<string, unknown> }
      : null;
  } catch {
    return null;
  }
}

/** A long opaque id cut for a chip: `art-4cff6ebb…`. */
export function shortId(id: string): string {
  const dash = id.indexOf("-");
  const head = dash > 0 && dash <= 8 ? dash + 1 : 0;
  return id.length > head + 10 ? `${id.slice(0, head + 8)}…` : id;
}

const str = (v: unknown): string | null => (typeof v === "string" && v.trim() !== "" ? v.trim() : null);

/** An app operation that reads one record by id. */
const APP_RECORD_READ = /^records\.(show|get|read)$/;

/** `app_call` args arrive as a JSON string or an object. */
function callArgs(value: unknown): Record<string, unknown> {
  if (value && typeof value === "object" && !Array.isArray(value)) return value as Record<string, unknown>;
  if (typeof value !== "string") return {};
  try {
    const parsed: unknown = JSON.parse(value);
    return parsed && typeof parsed === "object" && !Array.isArray(parsed) ? (parsed as Record<string, unknown>) : {};
  } catch {
    return {};
  }
}

function sourceOf(name: string, args: Record<string, unknown>, summary?: string): TurnSource | null {
  switch (name) {
    case "wiki_read": {
      // A summary's detail is the path (tool_summary's `path` field).
      const path = str(args.path) ?? (summary !== undefined ? str(summary) : null);
      return path ? { kind: "wiki", key: `wiki:${path}`, label: path, target: path } : null;
    }
    case "board_read_issue": {
      const id = str(args.id) ?? str(args.issue);
      return id ? { kind: "issue", key: `issue:${id}`, label: id, target: id } : null;
    }
    case "chat_attachment_read": {
      const id = str(args.id);
      return id ? { kind: "attachment", key: `attachment:${id}`, label: id, target: id } : null;
    }
    case "artifact_read":
    case "artifact_show": {
      // The chip names the artifact by its title when the turn holds it
      // (TurnRichCards); otherwise by this short id.
      const id = str(args.id) ?? str(args.artifact);
      return id ? { kind: "artifact", key: `artifact:${id}`, label: shortId(id), target: id } : null;
    }
    case "web_read": {
      // A summary's detail is the url (tool_summary's `url` field).
      const url = str(args.url) ?? (summary !== undefined ? str(summary) : null);
      if (!url) return null;
      let parsed: URL;
      try {
        parsed = new URL(url);
      } catch {
        return null;
      }
      if (parsed.protocol !== "https:" && parsed.protocol !== "http:") return null;
      return { kind: "web", key: `web:${parsed.href}`, label: parsed.host, target: parsed.href };
    }
    case "app_call": {
      // An app record, named by its board identity (app and record id).
      // Lists, searches and writes name no single record, so they are not
      // sources.
      const app = str(args.app);
      const operation = str(args.operation);
      if (!app || !operation || !APP_RECORD_READ.test(operation)) return null;
      const inner = callArgs(args.args);
      const record = str(inner.record_id) ?? str(inner.id);
      if (!record) return null;
      return { kind: "app", key: `app:${app}:${record}`, label: `${app} · ${record}`, target: record, app };
    }
    default:
      return null;
  }
}

/**
 * The sources a turn read, in the order it first read them, one chip per
 * source. Only recorded `tool_call` entries count.
 */
export function turnSources(entries: readonly ThreadEntry[]): TurnSource[] {
  const seen = new Set<string>();
  const out: TurnSource[] = [];
  for (const entry of entries) {
    if (entry.kind !== "tool_call") continue;
    const call = parseToolCall(entry.text ?? "");
    if (!call) continue;
    const source = sourceOf(call.name, call.args, call.summary);
    if (!source || seen.has(source.key)) continue;
    seen.add(source.key);
    out.push(source);
  }
  return out;
}

const norm = (s: string): string => s.toLowerCase().replace(/-/g, "");

/**
 * The decision id a staging result names: `{"staged": true, "actionId": …}`
 * (or `action_id`), from the result's JSON text or payload. A result that
 * only lists, reads or mentions an id names none (CAD-2450).
 */
export function stagedActionId(entry: ThreadEntry): string | null {
  const candidates: unknown[] = [entry.payload];
  try {
    candidates.push(JSON.parse((entry.text ?? "").trim()));
  } catch {
    // Not JSON: not a staging result.
  }
  for (const value of candidates) {
    if (!value || typeof value !== "object" || Array.isArray(value)) continue;
    const result = value as Record<string, unknown>;
    if (result.staged !== true) continue;
    const id = str(result.actionId) ?? str(result.action_id);
    if (id) return norm(id);
  }
  return null;
}

/**
 * The staged writes this turn made: a decision named by one of the turn's
 * staging results. Only rows the surface already holds can match, so a write
 * the viewer cannot see never shows, and a later turn that lists or reads the
 * decision does not repeat its card.
 */
export function stagedInTurn(entries: readonly ThreadEntry[], rows: readonly StagedWrite[]): StagedWrite[] {
  const ids = new Set(entries.filter((entry) => entry.kind === "tool_result").map(stagedActionId).filter((id): id is string => id !== null));
  if (ids.size === 0) return [];
  return rows.filter((row) => ids.has(norm(row.id)));
}

export interface StagedField {
  label: string;
  value: string;
  /** Empty or not provided: shown as flagged. */
  missing: boolean;
  /** The write removes this field; `value` is what it removes. */
  removed: boolean;
}

/** At most this many fields on the card; the review card has the rest. */
export const STAGED_FIELDS_SHOWN = 6;
/** Each value is cut to this many characters on the card. */
export const STAGED_VALUE_CHARS = 280;

const cut = (s: string): string => (s.length > STAGED_VALUE_CHARS ? `${s.slice(0, STAGED_VALUE_CHARS - 1)}…` : s);

/**
 * The key fields the staged card shows: a connected effect's `preview` when
 * the row carries one (display only — what runs is the stored call), else
 * the row's own changes. Every value is plain text.
 */
export function stagedFields(write: StagedWrite): { fields: StagedField[]; more: number } {
  const preview = write.connected?.preview;
  const all: StagedField[] = preview
    ? preview.fields.map((field) => ({ label: field.label, value: cut(field.value), missing: field.value.trim() === "", removed: false }))
    : write.changes.map((change) => {
        if (change.kind === "removed") return { label: change.label, value: cut(change.before ?? ""), missing: false, removed: true };
        const value = change.after ?? "";
        return { label: change.label, value: cut(value), missing: change.after == null || value.trim() === "", removed: false };
      });
  return { fields: all.slice(0, STAGED_FIELDS_SHOWN), more: Math.max(0, all.length - STAGED_FIELDS_SHOWN) };
}
