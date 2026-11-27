/**
 * Shared `@mention` parsing, `/note` silent detection, handle slugging, and
 * composer autocomplete helpers (CAD-2711). Keeps email addresses untouched.
 */

export interface MentionCandidate {
  id: string;
  /** Handle inserted after `@` (lowercase slug or agent alias). */
  handle: string;
  /** Human-readable label shown in the picker row. */
  label: string;
  kind: "human" | "agent";
  subtitle?: string;
  /** True when the recipient is a member of the current conversation. */
  active: boolean;
  /** Explains why picking/pinging this recipient will not notify or run them. */
  unavailableReason?: string;
}

export interface MentionOutcome {
  kind: "notified" | "queued" | "silent" | "blocked_acl" | "blocked_dm" | "blocked_chain" | string;
  handle: string;
  label: string;
  reason?: string;
}

export interface ActiveMention {
  /** Index of `@` in the text. */
  start: number;
  /** Caret offset (end of typed handle query). */
  end: number;
  /** Text typed after `@` up to the caret. */
  query: string;
}

/** Canonical slug matching Rust `store::mentions::slugify_name` (`"Mei Lin"` -> `"mei-lin"`). */
export function slugifyHandle(name: string): string {
  let out = "";
  let pendingDash = false;
  for (const ch of name) {
    if (/[A-Za-z0-9]/.test(ch)) {
      if (pendingDash && out.length > 0) out += "-";
      out += ch.toLowerCase();
      pendingDash = false;
    } else if (out.length > 0) {
      pendingDash = true;
    }
  }
  return out;
}

/** True when `body` starts (after leading whitespace) with `/note` followed by whitespace or EOS. */
export function startsWithNote(body: string): boolean {
  const trimmed = body.trimStart();
  if (trimmed.length < 5) return false;
  if (trimmed.slice(0, 5).toLowerCase() !== "/note") return false;
  return trimmed.length === 5 || /\s/.test(trimmed[5]!);
}

function isBoundaryBeforeAt(ch: string | undefined): boolean {
  if (ch === undefined) return true;
  return !/[A-Za-z0-9_.]/.test(ch);
}

function isHandleChar(ch: string): boolean {
  return /[A-Za-z0-9_-]/.test(ch);
}

const MENTION_TOKEN = /@([A-Za-z0-9][A-Za-z0-9_-]{0,63})(?![A-Za-z0-9_-])/g;
const LEADING_COMMAND_TOKEN = /^(?:\/skill:[a-z0-9][a-z0-9-]{0,31}|\/[a-z][a-z0-9-]{0,31})(?=\s|$)/;
const LEADING_NOTE_TOKEN = /^\/note(?=\s|$)/i;

export interface MentionSpan {
  start: number;
  end: number;
  raw: string;
  handle: string;
}

/**
 * Find boundary-safe `@handle` spans inside prose, skipping email-style
 * `name@domain` occurrences where `@` immediately follows a word char or `.`.
 */
export function findMentionSpans(text: string): MentionSpan[] {
  const out: MentionSpan[] = [];
  for (const m of text.matchAll(MENTION_TOKEN)) {
    const start = m.index ?? 0;
    const prev = start > 0 ? text[start - 1] : undefined;
    if (!isBoundaryBeforeAt(prev)) continue;
    const handle = m[1] ?? "";
    if (!handle) continue;
    out.push({
      start,
      end: start + m[0].length,
      raw: m[0],
      handle,
    });
  }
  return out;
}

export type ComposerHighlightKind = "text" | "command" | "mention-agent" | "mention-human";

export interface ComposerHighlightSpan {
  kind: ComposerHighlightKind;
  text: string;
}

/**
 * Partition composer draft text into highlight spans for inline backdrop
 * rendering. Concatenating every span's `text` always reproduces `text` verbatim.
 */
export function splitComposerHighlights(
  text: string,
  options?: {
    candidates?: readonly MentionCandidate[];
    highlightCommand?: boolean | "note";
  },
): ComposerHighlightSpan[] {
  if (!text) return [];
  const candidateKindByHandle = new Map<string, "agent" | "human">();
  for (const candidate of options?.candidates ?? []) {
    if (!candidate.active) continue;
    const key = candidate.handle.trim().toLowerCase();
    if (key && !candidateKindByHandle.has(key)) candidateKindByHandle.set(key, candidate.kind);
  }

  const out: ComposerHighlightSpan[] = [];
  let cursor = 0;

  if (options?.highlightCommand) {
    const pattern = options.highlightCommand === "note" ? LEADING_NOTE_TOKEN : LEADING_COMMAND_TOKEN;
    const cmd = pattern.exec(text);
    if (cmd?.[0]) {
      out.push({ kind: "command", text: cmd[0] });
      cursor = cmd[0].length;
    }
  }

  if (candidateKindByHandle.size > 0) {
    for (const span of findMentionSpans(text)) {
      if (span.start < cursor) continue;
      const targetKind = candidateKindByHandle.get(span.handle.toLowerCase());
      if (!targetKind) continue;
      if (span.start > cursor) {
        out.push({ kind: "text", text: text.slice(cursor, span.start) });
      }
      out.push({
        kind: targetKind === "agent" ? "mention-agent" : "mention-human",
        text: span.raw,
      });
      cursor = span.end;
    }
  }

  if (cursor < text.length) {
    out.push({ kind: "text", text: text.slice(cursor) });
  }
  return out;
}

/**
 * Find an active `@handle` token ending at `caret`, ignoring email-style
 * `name@domain` addresses where `@` follows a letter, digit, `_`, or `.`.
 */
export function findActiveMention(text: string, caret: number): ActiveMention | null {
  const clamped = Math.max(0, Math.min(caret, text.length));
  let pos = clamped;
  while (pos > 0 && isHandleChar(text[pos - 1]!)) {
    pos -= 1;
    if (clamped - pos > 64) return null;
  }
  if (pos === 0 || text[pos - 1] !== "@") return null;
  const atIndex = pos - 1;
  const prev = atIndex > 0 ? text[atIndex - 1] : undefined;
  if (!isBoundaryBeforeAt(prev)) return null;
  return {
    start: atIndex,
    end: clamped,
    query: text.slice(atIndex + 1, clamped),
  };
}

/** Filter and rank candidates: active channel members first, then unavailable hints. */
export function filterMentionCandidates(
  candidates: readonly MentionCandidate[],
  query: string,
): MentionCandidate[] {
  const q = query.trim().toLowerCase();
  const matched = candidates.filter((c) => {
    if (!q) return true;
    return (
      c.handle.toLowerCase().includes(q) ||
      c.label.toLowerCase().includes(q) ||
      (c.subtitle?.toLowerCase().includes(q) ?? false)
    );
  });
  return matched.slice().sort((a, b) => {
    if (a.active !== b.active) return a.active ? -1 : 1;
    const ah = a.handle.toLowerCase();
    const bh = b.handle.toLowerCase();
    const aStarts = q !== "" && ah.startsWith(q);
    const bStarts = q !== "" && bh.startsWith(q);
    if (aStarts !== bStarts) return aStarts ? -1 : 1;
    return a.label.localeCompare(b.label);
  });
}

/** Replace an active `@query` span with `@handle ` and return the updated draft and caret. */
export function applyMention(
  text: string,
  mention: ActiveMention,
  candidate: Pick<MentionCandidate, "handle">,
): { text: string; caret: number } {
  const before = text.slice(0, mention.start);
  const after = text.slice(mention.end);
  const needsSpace = after.length === 0 || !/^\s/.test(after[0]!);
  const inserted = `@${candidate.handle}${needsSpace ? " " : ""}`;
  return {
    text: `${before}${inserted}${after}`,
    caret: before.length + inserted.length,
  };
}
