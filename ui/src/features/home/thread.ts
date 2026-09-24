import type { SseEvent } from "../../lib/sse";

/**
 * The master's thread as the Home screen holds it (CAD-328): the stored
 * entries in `seq` order, plus the operator's messages still on their
 * way (optimistic). No React here — the merge, the stream reducer and
 * the optimistic reconcile are unit-tested in plain node
 * (tests/thread.test.ts).
 *
 * - Entries are keyed by `seq`: a page, a stream frame and a replay
 *   after a reconnect all merge by it, so an entry seen twice is kept
 *   once and entries arriving out of order still read in order.
 * - A pending message carries the client-chosen message id it was sent
 *   with (`POST /messages {"message"}`); the stored `operator` entry
 *   carries the same id, and its arrival retires the pending one.
 */

export type ThreadRole = "operator" | "agent" | "system";
export type ThreadKind = "message" | "assistant_text" | "tool_call" | "tool_result" | "turn_result";

/** One stored entry (`thread_read` / the stream's `entry` frame). */
export interface ThreadEntry {
  seq: number;
  role: ThreadRole | string;
  kind: ThreadKind | string;
  text: string;
  payload?: Record<string, unknown> | null;
  /** The queued message this entry belongs to. */
  message?: string | null;
  created?: string | null;
}

export interface PendingMessage {
  /** Client-chosen message id — a retry reuses it (one message, not two). */
  message: string;
  text: string;
  state: "sending" | "sent" | "failed";
  error?: string;
  /** Epoch ms when it was first sent. */
  at: number;
}

export interface ThreadState {
  /** False when the master has no thread yet (`thread: null`). */
  exists: boolean;
  /** The thread endpoint answered 404: no such agent is registered. */
  missing?: boolean;
  entries: ThreadEntry[];
  pending: PendingMessage[];
}

export const EMPTY_THREAD: ThreadState = { exists: false, entries: [], pending: [] };

/** A value from the wire as an entry, or null when it is not one. */
export function asEntry(value: unknown): ThreadEntry | null {
  if (!value || typeof value !== "object") return null;
  const v = value as Record<string, unknown>;
  const seq = typeof v.seq === "number" ? v.seq : Number(v.seq);
  if (!Number.isInteger(seq) || seq <= 0) return null;
  return {
    seq,
    role: typeof v.role === "string" ? v.role : "system",
    kind: typeof v.kind === "string" ? v.kind : "message",
    text: typeof v.text === "string" ? v.text : "",
    payload:
      v.payload && typeof v.payload === "object" ? (v.payload as Record<string, unknown>) : null,
    message: typeof v.message === "string" ? v.message : null,
    created: typeof v.created === "string" ? v.created : null,
  };
}

/** The highest stored `seq` — the resume cursor; null with none. */
export function lastSeq(state: ThreadState | null): number | null {
  const entries = state?.entries ?? [];
  return entries.length ? entries[entries.length - 1].seq : null;
}

/**
 * Fold entries into the state: dedupe by `seq`, keep `seq` order, and
 * retire every pending message whose stored operator entry arrived.
 * Returns the same object when nothing changed.
 */
export function mergeEntries(state: ThreadState | null, incoming: ThreadEntry[]): ThreadState {
  const base = state ?? EMPTY_THREAD;
  if (incoming.length === 0) return base;
  const bySeq = new Map<number, ThreadEntry>();
  for (const e of base.entries) bySeq.set(e.seq, e);
  let changed = false;
  for (const e of incoming) {
    if (!bySeq.has(e.seq)) changed = true;
    bySeq.set(e.seq, e);
  }
  const entries = changed ? [...bySeq.values()].sort((a, b) => a.seq - b.seq) : base.entries;
  const stored = new Set(
    entries.filter((e) => e.role === "operator" && e.message).map((e) => e.message as string),
  );
  const pending = base.pending.filter((p) => !stored.has(p.message));
  if (!changed && pending.length === base.pending.length && base.exists) return base;
  return { exists: true, missing: false, entries, pending };
}

/** `streamInto`'s reducer: an `entry` frame merges; anything else is ignored. */
export function reduceFrame(state: ThreadState | null, event: SseEvent): ThreadState {
  const base = state ?? EMPTY_THREAD;
  if (event.type !== "entry") return base;
  let parsed: unknown;
  try {
    parsed = JSON.parse(event.data);
  } catch {
    return base;
  }
  const entry = asEntry(parsed);
  return entry ? mergeEntries(base, [entry]) : base;
}

/** A page from `GET /api/threads/<alias>` folded into the state. */
export function mergePage(
  state: ThreadState | null,
  page: { thread?: unknown; entries?: unknown[] },
): ThreadState {
  const entries = (page.entries ?? []).map(asEntry).filter((e): e is ThreadEntry => e !== null);
  const merged = mergeEntries(state, entries);
  const found = merged.missing ? { ...merged, missing: false } : merged;
  if (found.exists || page.thread == null) return found;
  return { ...found, exists: true };
}

/** Add an optimistic message (or mark a retried one sending again). */
export function addPending(state: ThreadState | null, message: string, text: string, at: number): ThreadState {
  const base = state ?? EMPTY_THREAD;
  const rest = base.pending.filter((p) => p.message !== message);
  return { ...base, pending: [...rest, { message, text, state: "sending", at }] };
}

/** The POST answered: sent (awaiting its stored entry) or failed. */
export function settlePending(
  state: ThreadState | null,
  message: string,
  outcome: { ok: true } | { ok: false; error: string },
): ThreadState {
  const base = state ?? EMPTY_THREAD;
  if (!base.pending.some((p) => p.message === message)) return base;
  const error = "error" in outcome ? outcome.error : undefined;
  return {
    ...base,
    pending: base.pending.map((p): PendingMessage =>
      p.message !== message
        ? p
        : error === undefined
          ? { ...p, state: "sent", error: undefined }
          : { ...p, state: "failed", error },
    ),
  };
}

/** Drop a failed optimistic message the operator gave up on. */
export function discardPending(state: ThreadState | null, message: string): ThreadState {
  const base = state ?? EMPTY_THREAD;
  return { ...base, pending: base.pending.filter((p) => p.message !== message) };
}

/** What the thread view renders, in order. */
export type ThreadItem =
  | { type: "operator"; key: string; entry: ThreadEntry }
  | { type: "system"; key: string; entry: ThreadEntry }
  | { type: "commentary"; key: string; entry: ThreadEntry }
  | { type: "tools"; key: string; entries: ThreadEntry[] }
  | { type: "answer"; key: string; entry: ThreadEntry }
  | { type: "pending"; key: string; pending: PendingMessage };

const TOOL_KINDS = new Set(["tool_call", "tool_result"]);

/**
 * Entries by kind: the operator's message, `assistant_text` as
 * commentary, runs of `tool_call`/`tool_result` as one collapsed group,
 * `turn_result` as the answer; other messages (the daemon's, a peer's)
 * as a system line. Pending messages come last.
 */
export function threadItems(state: ThreadState | null): ThreadItem[] {
  const items: ThreadItem[] = [];
  for (const entry of state?.entries ?? []) {
    const key = `e${entry.seq}`;
    if (TOOL_KINDS.has(entry.kind)) {
      const last = items[items.length - 1];
      if (last?.type === "tools") last.entries.push(entry);
      else items.push({ type: "tools", key, entries: [entry] });
    } else if (entry.kind === "assistant_text") {
      items.push({ type: "commentary", key, entry });
    } else if (entry.kind === "turn_result") {
      items.push({ type: "answer", key, entry });
    } else if (entry.role === "operator") {
      items.push({ type: "operator", key, entry });
    } else {
      items.push({ type: "system", key, entry });
    }
  }
  for (const pending of state?.pending ?? []) {
    items.push({ type: "pending", key: `p${pending.message}`, pending });
  }
  return items;
}

const EPIC_ID = /[A-Z][A-Z0-9]{0,9}-\d+/;
const EPIC_JSON = new RegExp(`"epic"\\s*:\\s*"(${EPIC_ID.source})"`);
const PLAN_CMD = new RegExp(`\\bplan (?:propose[ds]?|show|approve|reject)\\b[^\\n]*?\\b(${EPIC_ID.source})\\b`);
const PLAN_PROSE = new RegExp(`\\b[Pp]lan (${EPIC_ID.source})\\b`);

/**
 * The plan epic an entry points at, if any: a payload `epic`, an
 * `"epic": "X-1"` in a tool result, a `plan propose|show|approve …
 * X-1` command, or "plan X-1" in prose. The thread shows each plan's
 * card once, at its first mention.
 */
export function planRef(entry: ThreadEntry): string | null {
  const epic = entry.payload?.epic;
  if (typeof epic === "string" && new RegExp(`^${EPIC_ID.source}$`).test(epic)) return epic;
  if (entry.role === "operator") return null;
  const text = entry.text ?? "";
  return (
    EPIC_JSON.exec(text)?.[1] ??
    (TOOL_KINDS.has(entry.kind) ? PLAN_CMD.exec(text)?.[1] : null) ??
    PLAN_PROSE.exec(text)?.[1] ??
    null
  );
}

/** item key → epic, for the first mention of each plan. */
export function planAnchors(items: ThreadItem[]): Map<string, string> {
  const seen = new Set<string>();
  const anchors = new Map<string, string>();
  for (const item of items) {
    const entries =
      item.type === "tools" ? item.entries : item.type === "pending" ? [] : [item.entry];
    for (const entry of entries) {
      const epic = planRef(entry);
      if (epic && !seen.has(epic)) {
        seen.add(epic);
        anchors.set(item.key, epic);
        break;
      }
    }
  }
  return anchors;
}

/** A fresh client message id. */
export function newMessageId(rand: () => number = Math.random, now: () => number = Date.now): string {
  return `ui-${now().toString(36)}-${Math.floor(rand() * 36 ** 6).toString(36)}`;
}

/** One page fetch: `(after, limit)` → the endpoint's page. */
export type PageFetcher = (
  after: number,
  limit: number,
) => Promise<{ thread?: unknown; entries?: unknown[] }>;

/**
 * Load everything after what `current` already holds, page by page
 * (at most `maxPages`), keeping its pending messages. A 404 (no such
 * agent) is not a failure: it is the "master not started" state.
 */
export async function loadThread(
  fetchPage: PageFetcher,
  current: ThreadState | null,
  limit = 500,
  maxPages = 20,
): Promise<ThreadState> {
  let state: ThreadState = current ?? EMPTY_THREAD;
  let after = lastSeq(state) ?? 0;
  try {
    for (let i = 0; i < maxPages; i++) {
      const page = await fetchPage(after, limit);
      state = mergePage(state, page);
      const next = lastSeq(state) ?? 0;
      if ((page.entries ?? []).length < limit || next <= after) break;
      after = next;
    }
  } catch (e) {
    if ((e as { status?: number }).status === 404) {
      return { ...state, exists: false, missing: true };
    }
    throw e;
  }
  return state;
}
