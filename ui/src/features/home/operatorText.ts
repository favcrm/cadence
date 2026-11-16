/**
 * CAD-2362: the pure model of an operator's own message — the inline
 * Markdown it may carry, where a pasted payload starts, when the bubble
 * folds, and the one-line receipt a form answer reads as. No React, so the
 * UI tests load it directly; `ThreadView.tsx` renders it.
 *
 * Operator text is plain text first: anything this module does not
 * recognise stays exactly as typed, and nothing here ever yields markup.
 * Block syntax (headings, lists, tables, images, raw HTML) is not parsed, so
 * a leading `#` or `-` keeps its place in the message.
 */
import { ACTION_MARK } from "./formModel";

export type Inline =
  | { kind: "text"; text: string }
  | { kind: "code"; text: string }
  | { kind: "strong"; text: string }
  | { kind: "em"; text: string }
  | { kind: "link"; text: string; href: string };

/** A code span: one pair of backticks on one line, nothing parsed inside. */
const CODE = /`([^`\n]+)`/g;

/**
 * The other inline marks, in priority order: a `[label](https://…)` link, a
 * bare `http(s)://` URL (trailing punctuation stays text), `**strong**`, and
 * `*em*` / `_em_`. Emphasis needs a non-space just inside each marker and no
 * letter or digit just outside it, so `2*3*4` and `snake_case_name` stay as
 * typed.
 */
const RICH =
  /\[([^\]\n]+)\]\((https?:\/\/[^\s)]+)\)|(https?:\/\/[^\s<>()]*[^\s<>().,;:!?'"’」』）])|\*\*(?=\S)([^*\n]*?\S)\*\*|(?<![\p{L}\p{N}*\\])\*(?=[^\s*])([^*\n]*?[^\s*])\*(?![\p{L}\p{N}*])|(?<![\p{L}\p{N}_\\])_(?=[^\s_])([^_\n]*?[^\s_])_(?![\p{L}\p{N}_])/gu;

function push(out: Inline[], token: Inline): void {
  const last = out[out.length - 1];
  if (token.kind === "text" && last?.kind === "text") last.text += token.text;
  else if (token.kind !== "text" || token.text !== "") out.push(token);
}

function rich(text: string, out: Inline[]): void {
  let at = 0;
  for (const m of text.matchAll(RICH)) {
    push(out, { kind: "text", text: text.slice(at, m.index) });
    if (m[1] !== undefined) push(out, { kind: "link", text: m[1], href: m[2] });
    else if (m[3] !== undefined) push(out, { kind: "link", text: m[3], href: m[3] });
    else if (m[4] !== undefined) push(out, { kind: "strong", text: m[4] });
    else push(out, { kind: "em", text: m[5] ?? m[6] });
    at = m.index + m[0].length;
  }
  push(out, { kind: "text", text: text.slice(at) });
}

/** The operator's text as inline tokens. Concatenating every token's
 *  source would give the text back; an unmatched marker stays literal. */
export function inlineTokens(text: string): Inline[] {
  const out: Inline[] = [];
  let at = 0;
  for (const m of text.matchAll(CODE)) {
    rich(text.slice(at, m.index), out);
    push(out, { kind: "code", text: m[1] });
    at = m.index + m[0].length;
  }
  rich(text.slice(at), out);
  return out;
}

export type Segment = { kind: "prose"; text: string } | { kind: "code"; text: string; lang: string | null };

const FENCE = /^```([^\n`]*)\n([\s\S]*?)\n?```[ \t]*$/gm;
/** Past this the paste is too large to scan for a JSON tail; it stays prose. */
const SCAN_LIMIT = 200_000;
/** A JSON tail shorter than this is a value in a sentence, not a paste. */
const MIN_JSON = 40;

function jsonTail(text: string): Segment[] | null {
  if (text.length > SCAN_LIMIT) return null;
  for (let i = text.search(/[{[]/); i >= 0 && i < text.length; ) {
    const tail = text.slice(i).trim();
    if (tail.length >= MIN_JSON) {
      try {
        const value: unknown = JSON.parse(tail);
        if (value && typeof value === "object") {
          const lead = text.slice(0, i).trimEnd();
          const code: Segment = { kind: "code", text: JSON.stringify(value, null, 2), lang: "json" };
          return lead ? [{ kind: "prose", text: lead }, code] : [code];
        }
      } catch {
        /* not JSON from here: try the next opening bracket */
      }
    }
    const next = text.slice(i + 1).search(/[{[]/);
    i = next < 0 ? -1 : i + 1 + next;
  }
  return null;
}

/**
 * The message as prose and code: each fenced block is code; with no fence, a
 * trailing JSON object or array (a pasted payload, even on one line) is code,
 * shown indented. Anything else is one prose segment.
 */
export function segments(text: string): Segment[] {
  const fenced: Segment[] = [];
  let at = 0;
  for (const m of text.matchAll(FENCE)) {
    const lead = text.slice(at, m.index).replace(/\n$/, "");
    if (lead.trim()) fenced.push({ kind: "prose", text: lead });
    fenced.push({ kind: "code", text: m[2], lang: m[1].trim() || null });
    at = m.index + m[0].length;
  }
  if (fenced.length > 0) {
    const rest = text.slice(at).replace(/^\n/, "");
    if (rest.trim()) fenced.push({ kind: "prose", text: rest });
    return fenced;
  }
  return jsonTail(text) ?? [{ kind: "prose", text }];
}

/** Lines a folded message keeps visible. */
export const FOLD_LINES = 6;
/** Characters past which a message with long wrapped lines folds. */
const FOLD_CHARS = 1200;

/** Rendered lines, counting each segment's own newlines. */
export function lineCount(parts: Segment[]): number {
  return parts.reduce((n, s) => n + s.text.split("\n").length, 0);
}

/** Whether the bubble folds: more than twice the visible lines, a code block
 *  taller than the fold, or a long message whose lines wrap. */
export function foldable(parts: Segment[]): boolean {
  if (lineCount(parts) > FOLD_LINES * 2) return true;
  if (parts.some((s) => s.kind === "code" && s.text.split("\n").length > FOLD_LINES)) return true;
  return parts.reduce((n, s) => n + s.text.length, 0) > FOLD_CHARS;
}

/**
 * A form answer as its one-line receipt: the `label: value` lines the host
 * wrote above the A2UI action (AgentForm). Null when the action is absent or
 * unreadable — the same answers the form card cannot show — so the bubble
 * keeps the full text instead.
 */
export function formReceipt(text: string): string[] | null {
  const at = text.lastIndexOf(ACTION_MARK);
  if (at < 0) return null;
  try {
    const parsed = JSON.parse(text.slice(at + ACTION_MARK.length)) as { action?: unknown } | null;
    if (!parsed || typeof parsed.action !== "object" || parsed.action === null) return null;
  } catch {
    return null;
  }
  const lines = text.slice(0, at).split("\n");
  if (!/^Form answer \(.*\):$/.test(lines[0] ?? "")) return null;
  return lines.slice(1).filter((l) => l.trim() !== "");
}
