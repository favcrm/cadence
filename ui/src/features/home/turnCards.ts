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
  /** The board identity the chip opens. */
  target: string;
}

/** At most this many chips; the rest are "+N more". */
export const SOURCES_SHOWN = 8;

/** A recorded call as `name {json}`. Null when it is not one. */
export function parseToolCall(text: string): { name: string; args: Record<string, unknown> } | null {
  const trimmed = text.trim();
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

const str = (v: unknown): string | null => (typeof v === "string" && v.trim() !== "" ? v.trim() : null);

/** An app operation that reads records, not one that changes them. */
const APP_READ = /^records\.(show|get|read|list|search|query|aggregate|find)$/;

function sourceOf(name: string, args: Record<string, unknown>): TurnSource | null {
  switch (name) {
    case "wiki_read": {
      const path = str(args.path);
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
      const id = str(args.id) ?? str(args.artifact);
      return id ? { kind: "artifact", key: `artifact:${id}`, label: id, target: id } : null;
    }
    case "web_read": {
      const url = str(args.url);
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
      const app = str(args.app);
      const operation = str(args.operation);
      if (!app || !operation || !APP_READ.test(operation)) return null;
      return { kind: "app", key: `app:${app}`, label: app, target: app };
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
    const source = sourceOf(call.name, call.args);
    if (!source || seen.has(source.key)) continue;
    seen.add(source.key);
    out.push(source);
  }
  return out;
}

const norm = (s: string): string => s.toLowerCase().replace(/-/g, "");

/**
 * The staged writes this turn made: a decision whose id appears in one of the
 * turn's recorded tool results. Only rows the surface already holds can
 * match, so a write the viewer cannot see never shows.
 */
export function stagedInTurn(entries: readonly ThreadEntry[], rows: readonly StagedWrite[]): StagedWrite[] {
  const results = entries
    .filter((entry) => entry.kind === "tool_result")
    .map((entry) => norm(`${entry.text ?? ""} ${entry.payload ? JSON.stringify(entry.payload) : ""}`))
    .join("\n");
  if (results.trim() === "") return [];
  return rows.filter((row) => row.id.length >= 8 && results.includes(norm(row.id)));
}

export interface StagedField {
  label: string;
  value: string;
  /** Empty or not provided: shown as flagged. */
  missing: boolean;
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
    ? preview.fields.map((field) => ({ label: field.label, value: cut(field.value), missing: field.value.trim() === "" }))
    : write.changes.map((change) => {
        const value = change.after ?? "";
        return { label: change.label, value: cut(value), missing: change.after == null || value.trim() === "" };
      });
  return { fields: all.slice(0, STAGED_FIELDS_SHOWN), more: Math.max(0, all.length - STAGED_FIELDS_SHOWN) };
}
