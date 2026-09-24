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
  /** Older entries exist on the server than the ones held; null when
   *  the daemon cannot say (it predates backward reads). */
  moreBefore?: boolean | null;
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
  return { ...base, exists: true, missing: false, entries, pending };
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
const PLAN_TICKETS = /"tickets"\s*:\s*\[/;
const PLAN_BLOCK = /"plan"\s*:\s*\{/;
const PLAN_PROSE = new RegExp(`\\b[Pp]lan (${EPIC_ID.source})\\b`);

/**
 * The plan epic an entry points at, if any: a payload `epic`, an
 * `"epic": "X-1"` in a plan-shaped tool result, a `plan propose|show|approve …
 * X-1` command, or "plan X-1" in prose. The thread shows each plan's
 * card once, at its first mention.
 */
export function planRef(entry: ThreadEntry): string | null {
  const epic = entry.payload?.epic;
  if (typeof epic === "string" && new RegExp(`^${EPIC_ID.source}$`).test(epic)) return epic;
  if (entry.role === "operator") return null;
  const text = entry.text ?? "";
  // `"epic"` alone is not a plan (`epic stage` prints `{"epic","stage"}`):
  // only a plan-shaped payload — `plan propose`'s `tickets` list or
  // `plan show`'s `plan` block — counts.
  const planShaped = PLAN_TICKETS.test(text) || PLAN_BLOCK.test(text);
  return (
    (planShaped ? EPIC_JSON.exec(text)?.[1] : null) ??
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

/** One page of `GET /api/threads/<alias>`. */
export interface ThreadPageLike {
  thread?: unknown;
  entries?: unknown[];
  /** Backward reads only (CAD-328): older entries remain. Absent on a
   *  daemon that predates `tail`/`before` — it answered a forward page. */
  more_before?: boolean;
}

/** How the store reads pages: forward after a seq, the newest page, or
 *  the page below a seq. */
export interface PageReader {
  after: (after: number, limit: number) => Promise<ThreadPageLike>;
  tail: (limit: number) => Promise<ThreadPageLike>;
  before: (before: number, limit: number) => Promise<ThreadPageLike>;
}

/** Entries fetched per page — the newest page on open, each "earlier". */
export const PAGE = 200;
/** Forward catch-up pages before giving up and reopening on the tail. */
const CATCH_UP_PAGES = 2;

function notFound(e: unknown): boolean {
  return (e as { status?: number }).status === 404;
}

/**
 * Load the thread for display (CAD-328). With nothing held, read the
 * NEWEST page (`tail`) — never the whole history from the start; the
 * stream then resumes after its last seq. With entries held (a
 * refetch), catch up forward a couple of pages; a longer gap reopens on
 * the tail. Pending messages survive either way. A daemon that ignores
 * `tail` answers a forward page (no `more_before`): that page is kept
 * and `moreBefore` stays unknown. A 404 (no such agent) is the "master
 * not started" state, not a failure.
 */
export async function loadThread(
  read: PageReader,
  current: ThreadState | null,
  limit = PAGE,
): Promise<ThreadState> {
  const held = current ?? EMPTY_THREAD;
  try {
    let after = lastSeq(held);
    if (after !== null) {
      let state = held;
      for (let i = 0; i < CATCH_UP_PAGES; i++) {
        const page = await read.after(after, limit);
        state = mergePage(state, page);
        if ((page.entries ?? []).length < limit) return state;
        after = lastSeq(state) ?? after;
      }
      // Too far behind to replay: reopen on the newest page.
    }
    const page = await read.tail(limit);
    const fresh = mergePage({ ...EMPTY_THREAD, pending: held.pending }, page);
    return {
      ...fresh,
      moreBefore: typeof page.more_before === "boolean" ? page.more_before : null,
    };
  } catch (e) {
    if (notFound(e)) return { ...held, exists: false, missing: true };
    throw e;
  }
}

/** The page older than what `state` holds, or null when none is left. */
export function fetchEarlier(
  read: PageReader,
  state: ThreadState,
  limit = PAGE,
): Promise<ThreadPageLike> | null {
  const first = state.entries[0]?.seq;
  if (first === undefined || first <= 1) return null;
  return read.before(first, limit);
}

/**
 * Fold an older page into the CURRENT state (stream frames may have
 * landed while it was in flight): merge by seq, and record whether
 * anything older remains.
 */
export function applyEarlier(state: ThreadState | null, page: ThreadPageLike): ThreadState {
  const merged = mergePage(state, page);
  return { ...merged, moreBefore: page.more_before === true };
}

/** Items rendered at most, until the operator asks for earlier ones. */
export const WINDOW = 300;

/**
 * The last `limit` items and how many are hidden above them — the
 * thread view renders a bounded window however long the thread grows
 * (a live stream keeps appending).
 */
export function visibleWindow<T>(items: T[], limit = WINDOW): { shown: T[]; hidden: number } {
  const hidden = Math.max(0, items.length - limit);
  return { shown: hidden ? items.slice(hidden) : items, hidden };
}
