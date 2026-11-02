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
export type ThreadKind = "message" | "assistant_text" | "tool_call" | "tool_result" | "turn_result" | "artifact" | "hire_proposal";

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
  /** CAD-1485 round-3 (member surface only): set when the send's
   *  delivery is genuinely unknown — a lost response, a transport/5xx
   *  failure, or a malformed/mismatching 2xx receipt. The member chat
   *  renders "Delivery unconfirmed" (opt-in label) with the same
   *  manual same-id retry/discard affordances; absent, every existing
   *  surface keeps the Master "not sent" reading unchanged. */
  delivery?: "unconfirmed";
  /** Epoch ms when it was first sent. */
  at: number;
  /** CAD-574: the cited needs subjects the send carried — a retry must
   *  resend the same refs or the daemon's idempotency check conflicts. */
  refs?: { kind: string; id: string }[];
  /** CAD-1168: the retained-file ids the send carried. A retry resends
   *  exactly these — sending text without them under the same message
   *  id is the daemon's content-conflict refusal. */
  attachments?: { id: string }[];
  /** CAD-1168: the verified app binding the send carried. A retry
   *  resends to this original destination, never the route's current
   *  installation/context. */
  app?: { install_id: string; context_id?: string };
  /** CAD-1337: selected Home conversation is part of the retry envelope. */
  conversation?: string;
}

export interface ThreadState {
  /** False when the master has no thread yet (`thread: null`). */
  exists: boolean;
  /** Server-issued Home conversation id; absent for legacy/unusable pages. */
  conversation?: string;
  /** The thread endpoint answered 404: no such agent is registered. */
  missing?: boolean;
  /** Older entries exist on the server than the ones held; null when
   *  the daemon cannot say (it predates backward reads). */
  moreBefore?: boolean | null;
  /** CAD-1700: the newest visible assistant reply's `seq` in the COMPLETE
   *  thread (`thread_read`'s `reply_seq`), independent of the loaded
   *  window. Absent on a daemon that predates it; entries never advance
   *  it, and a page never lowers it. */
  replySeq?: number;
  /** CAD-1853: the highest `seq` the daemon has SCANNED for this thread.
   *  A projection filters another source's cloud entries out of a page, so
   *  the scanned cursor can be ahead of every held entry; resuming the
   *  catch-up from it is what keeps a fully filtered page from being read
   *  again forever. Absent on a daemon that predates the page cursor. */
  cursor?: number;
  /** CAD-1853: the lowest `seq` the daemon has SCANNED BACKWARDS for this
   *  thread (`scanned_before` of the oldest page read). The oldest HELD entry
   *  is not a continuation point once a projection can hide a whole page —
   *  it may not move at all — so "earlier" continues below this boundary
   *  instead. Absent on a daemon that predates it. */
  older?: number;
  /** A server code means cloud history is incomplete, even with no entries. */
  cloudWithheld?: string | null;
  /** Orders availability observations independently of entry sequence. */
  cloudRevision?: number;
  /** Restoration needs a fresh tail, not catch-up past the evicted rows. */
  cloudRefreshNeeded?: boolean;
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
    // The daemon sends epoch seconds (f64); the UI keeps an ISO string.
    created:
      typeof v.created === "string"
        ? v.created
        : typeof v.created === "number" && Number.isFinite(v.created)
          ? new Date(v.created * 1000).toISOString()
          : null,
  };
}

/** Only server provenance marks a cloud row, including routed local copies. */
export function isCloudEntry(entry: ThreadEntry): boolean {
  return entry.payload != null &&
    (Object.prototype.hasOwnProperty.call(entry.payload, "cloud_agent") ||
      entry.payload.source === "cloud_agent");
}

/** Absent page metadata is clean; a present malformed/unknown code is not. */
export function pageCloudWithheld(page: { cloud_withheld?: unknown }): string | null {
  if (!Object.prototype.hasOwnProperty.call(page, "cloud_withheld")) return null;
  return typeof page.cloud_withheld === "string" && page.cloud_withheld.trim() !== ""
    ? page.cloud_withheld
    : "unknown";
}

function withCloudWithheld(state: ThreadState, code: string | null): ThreadState {
  const entries = code === null ? state.entries : state.entries.filter((e) => !isCloudEntry(e));
  return {
    ...state,
    cloudWithheld: code,
    cloudRevision: (state.cloudRevision ?? 0) + 1,
    cloudRefreshNeeded: code === null
      ? state.cloudRefreshNeeded || state.cloudWithheld != null
      : false,
    entries,
  };
}

/** The highest stored `seq` — the resume cursor; null with none. */
export function lastSeq(state: ThreadState | null): number | null {
  const entries = state?.entries ?? [];
  return entries.length ? entries[entries.length - 1].seq : null;
}

/** The forward resume cursor: every stored row at or below it has been
 *  scanned, including rows a projection filtered out. */
export function resumeSeq(state: ThreadState | null): number | null {
  const seq = Math.max(state?.cursor ?? 0, lastSeq(state) ?? 0);
  return seq > 0 ? seq : null;
}

/** A visible agent reply — the same rule as the daemon's complete
 *  watermark: agent `assistant_text`/`turn_result`, nonblank after the
 *  ECMAScript whitespace trim `String.prototype.trim` applies. */
function isReplyEntry(e: ThreadEntry): boolean {
  return (
    e.role === "agent" &&
    (e.kind === "assistant_text" || e.kind === "turn_result") &&
    e.text.trim() !== ""
  );
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
  // A live/folded visible reply is itself authoritative (CAD-1700): a
  // streamed answer advances the watermark even before a page carries
  // it. Non-reply and blank entries never do; a page can only raise it.
  const previousReply = base.replySeq ?? 0;
  let replySeq = previousReply;
  for (const e of incoming) {
    if (base.cloudWithheld != null && isCloudEntry(e)) continue;
    if (bySeq.get(e.seq) !== e) changed = true;
    bySeq.set(e.seq, e);
    if (isReplyEntry(e) && e.seq > replySeq) replySeq = e.seq;
  }
  const entries = changed ? [...bySeq.values()].sort((a, b) => a.seq - b.seq) : base.entries;
  const stored = new Set(
    entries.filter((e) => e.role === "operator" && e.message).map((e) => e.message as string),
  );
  const pending = base.pending.filter((p) => !stored.has(p.message));
  const replyAdvanced = replySeq > previousReply;
  if (!changed && pending.length === base.pending.length && base.exists && !replyAdvanced) {
    return base;
  }
  return {
    ...base,
    exists: true,
    missing: false,
    entries,
    pending,
    ...(replySeq > 0 ? { replySeq } : {}),
  };
}

/** Fold entries and independent cloud-availability frames from the stream. */
export function reduceFrame(state: ThreadState | null, event: SseEvent): ThreadState {
  const base = state ?? EMPTY_THREAD;
  if (event.type === "cloud_withheld") {
    let status: unknown;
    try {
      status = JSON.parse(event.data);
    } catch {
      return withCloudWithheld(base, "unknown");
    }
    const wire = status !== null && typeof status === "object"
      ? status as { cloud_withheld?: unknown }
      : { cloud_withheld: undefined };
    return withCloudWithheld(base, wire.cloud_withheld === null
      ? null
      : pageCloudWithheld({ cloud_withheld: wire.cloud_withheld }));
  }
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
  page: { thread?: unknown; entries?: unknown[]; reply_seq?: unknown; cloud_withheld?: unknown },
): ThreadState {
  const entries = (page.entries ?? []).map(asEntry).filter((e): e is ThreadEntry => e !== null);
  const merged = mergeEntries(withCloudWithheld(state ?? EMPTY_THREAD, pageCloudWithheld(page)), entries);
  // The page reports the COMPLETE thread's reply watermark, so keep the
  // maximum: a bounded, forward or older page (and a live frame that
  // carries only an entry) can never erase a reply it does not hold.
  const reply =
    typeof page.reply_seq === "number" &&
    Number.isSafeInteger(page.reply_seq) &&
    page.reply_seq >= 0
      ? page.reply_seq
      : undefined;
  const withReply =
    reply === undefined
      ? merged
      : { ...merged, replySeq: Math.max(merged.replySeq ?? 0, reply) };
  const found = withReply.missing ? { ...withReply, missing: false } : withReply;
  const wireThread = page.thread && typeof page.thread === "object"
    ? (page.thread as Record<string, unknown>)
    : null;
  const conversation =
    typeof wireThread?.id === "string" && wireThread.id.length > 0 ? wireThread.id : undefined;
  const exists = found.exists || page.thread != null;
  if (found.exists === exists && found.conversation === conversation) return found;
  return { ...found, exists, conversation };
}

/** Add an optimistic message (or mark a retried one sending again). */
export function addPending(
  state: ThreadState | null,
  message: string,
  text: string,
  at: number,
  refs?: { kind: string; id: string }[],
  attachments?: { id: string }[],
  app?: { install_id: string; context_id?: string },
  conversation?: string,
): ThreadState {
  const base = state ?? EMPTY_THREAD;
  const rest = base.pending.filter((p) => p.message !== message);
  return { ...base, pending: [...rest, { message, text, state: "sending", at, refs, attachments, app, conversation }] };
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
  | { type: "thinking"; key: string; entry: ThreadEntry }
  | { type: "commentary"; key: string; entry: ThreadEntry }
  | { type: "tools"; key: string; entries: ThreadEntry[] }
  | { type: "answer"; key: string; entry: ThreadEntry }
  | { type: "artifact"; key: string; entry: ThreadEntry }
  | { type: "hire"; key: string; entry: ThreadEntry; reason: string | null }
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
  // The daemon stores an artifact's card inside the put RPC, before the put's
  // own `tool_result`. A card met inside a tools run waits until the run ends
  // so it cannot split the call from its result (CAD-1728).
  let held: ThreadItem[] = [];
  const flush = () => {
    items.push(...held);
    held = [];
  };
  for (const entry of state?.entries ?? []) {
    const key = `e${entry.seq}`;
    if (entry.kind === "artifact" && items[items.length - 1]?.type === "tools") {
      held.push({ type: "artifact", key, entry });
      continue;
    }
    if (!TOOL_KINDS.has(entry.kind)) flush();
    if (TOOL_KINDS.has(entry.kind)) {
      const last = items[items.length - 1];
      if (last?.type === "tools") last.entries.push(entry);
      else items.push({ type: "tools", key, entries: [entry] });
    } else if (entry.kind === "thinking") {
      items.push({ type: "thinking", key, entry });
    } else if (entry.kind === "assistant_text") {
      items.push({ type: "commentary", key, entry });
    } else if (entry.kind === "turn_result") {
      items.push({ type: "answer", key, entry });
    } else if (entry.kind === "hire_proposal") {
      // CAD-1752: Master's inert proposal. Its reasoning is the turn's own
      // commentary just before the card; that line moves into the card
      // instead of showing twice.
      const last = items[items.length - 1];
      const lead =
        last?.type === "commentary" && last.entry.message === entry.message ? items.pop() : null;
      const reason = lead?.type === "commentary" ? lead.entry.text.trim() || null : null;
      items.push({ type: "hire", key, entry, reason });
    } else if (entry.kind === "artifact") {
      // CAD-1726: a card for a canvas artifact — metadata, never content.
      items.push({ type: "artifact", key, entry });
    } else if (entry.role === "operator") {
      items.push({ type: "operator", key, entry });
    } else {
      items.push({ type: "system", key, entry });
    }
  }
  flush();
  for (const pending of state?.pending ?? []) {
    items.push({ type: "pending", key: `p${pending.message}`, pending });
  }
  return items;
}

/** Item types that belong to an assistant turn. */
const TURN_MEMBERS = new Set(["thinking", "commentary", "tools", "answer", "artifact", "hire"]);

/** One render run: an assistant turn (its flat items), or one item that is
 *  not part of a turn. The items themselves stay flat — `threadItems` is
 *  still the one contract for the canvas feed, the working state and the
 *  window; a turn is only how a contiguous run of them paints. */
export type ThreadRun =
  | { turn: true; key: string; items: ThreadItem[]; message: string | null }
  | { turn: false; key: string; item: ThreadItem };

/**
 * Group the flat render list into turns. Reasoning, commentary, tool runs,
 * the answer and its artifact cards are one turn while they stay contiguous.
 * A terminal `answer` closes the turn: a further reasoning/commentary/tool
 * item starts the next one, while artifact cards may still follow it. A run
 * of artifact cards with no agent text is not a turn (no head, no trace).
 */
export function threadRuns(items: ThreadItem[]): ThreadRun[] {
  const runs: ThreadRun[] = [];
  let run: ThreadItem[] = [];
  let runMessage: string | null = null;
  const flush = () => {
    if (run.length === 0) return;
    if (run.some((item) => item.type !== "artifact")) {
      runs.push({ turn: true, key: run[0].key, items: run, message: runMessage });
    } else {
      for (const item of run) runs.push({ turn: false, key: item.key, item });
    }
    run = [];
    runMessage = null;
  };
  for (const item of items) {
    if (!TURN_MEMBERS.has(item.type)) {
      flush();
      runs.push({ turn: false, key: item.key, item });
      continue;
    }
    const message = itemMessage(item);
    // Never join two operator turns just because the intervening row is a
    // system note; the message id is the turn's identity when both sides
    // carry one.
    if (run.length > 0 && message !== null && runMessage !== null && message !== runMessage) {
      flush();
    }
    if (item.type !== "artifact" && run.some((i) => i.type === "answer")) flush();
    run.push(item);
    if (message !== null) runMessage = message;
  }
  flush();
  return runs;
}

/** The queued message id an item's entries carry — the turn it belongs to. */
export function itemMessage(item: ThreadItem): string | null {
  if (item.type === "pending") return item.pending.message;
  if (item.type === "tools") return item.entries.find((e) => e.message)?.message ?? null;
  return item.entry.message ?? null;
}

/** React key for a user row. The optimistic pending row and the stored
 *  operator entry share one message id; one key keeps that row (and the
 *  typing block under it) from unmounting and rising in again when the
 *  send is acknowledged. */
export function rowPaintKey(item: ThreadItem): string {
  const message = itemMessage(item);
  if ((item.type === "operator" || item.type === "pending") && message) return `user:${message}`;
  return item.key;
}

const OPEN_TURN = new Set(["thinking", "commentary", "tools", "hire"]);

/** The send named by the working id: the user row whose message matches
 *  it. Null when the daemon reports an id no user row carries — callers
 *  then fall back to the user row before the run. */
export function activeSendId(items: ThreadItem[], runningMessage: string | null): string | null {
  if (runningMessage === null || runningMessage === "") return null;
  for (let i = items.length - 1; i >= 0; i--) {
    const item = items[i];
    if ((item.type === "operator" || item.type === "pending") && itemMessage(item) === runningMessage) {
      return runningMessage;
    }
  }
  return null;
}

/** The send a run belongs to: the nearest user row before it, skipping
 *  messages still queued behind the running turn. A queued row can sit
 *  between the working send and its first assistant rows. */
export function sendBefore(items: ThreadItem[], runItems: ThreadItem[], queued: ReadonlySet<string>, skip: string | null = null): string | null {
  const idx = items.indexOf(runItems[0]);
  if (idx < 0) return null;
  for (let i = idx - 1; i >= 0; i--) {
    const item = items[i];
    if (item.type !== "operator" && item.type !== "pending") continue;
    if (item.type === "pending" && item.pending.state === "failed") continue;
    const id = itemMessage(item);
    if (id !== null && (queued.has(id) || id === skip)) continue;
    return id;
  }
  return null;
}

/** The assistant item that opens the run still in flight for `message`.
 *  Null when that send has no assistant rows yet, or the run already has
 *  an answer. Hosted transcripts often omit the message id on agent rows,
 *  so the run is the contiguous assistant items after the user row. A
 *  working id that matches no user row still adopts that open run when
 *  every tagged row in it names that id — otherwise the real turn paints
 *  as finished and a second assistant head is appended under it.
 *
 *  The run is adopted only when no row in it belongs to another send: a
 *  queued or interleaved turn must not be marked busy, and must not have
 *  the placeholder of the send actually running suppressed. A queued user
 *  row can sit between the running send and its first assistant rows, so
 *  an empty matching run falls through to the tail run. */
export function inFlightTurnKey(items: ThreadItem[], message: string | null): string | null {
  if (message === null || message === "") return null;
  // A failed pending row stays visible for Retry/Discard but is not a live
  // send: it neither bounds nor names the running turn.
  const isUser = (item: ThreadItem) =>
    (item.type === "operator" || item.type === "pending")
    && !(item.type === "pending" && item.pending.state === "failed");
  // `present` distinguishes "no assistant rows yet" (fall through) from a
  // run that answered or names another send (refuse, never fall through).
  const isTurn = (item: ThreadItem) =>
    OPEN_TURN.has(item.type) || item.type === "answer" || item.type === "artifact";
  const runAfter = (start: number): { key: string | null; present: boolean } => {
    let first: string | null = null;
    let present = false;
    for (let i = start; i < items.length; i++) {
      const item = items[i];
      if (isUser(item)) break;
      // A system or other non-turn row is transparent: it neither proves a
      // run exists nor carries a send's provenance.
      if (!isTurn(item)) continue;
      present = true;
      if (item.type === "answer") return { key: null, present };
      const tagged = itemMessage(item);
      if (tagged !== null && tagged !== message) return { key: null, present };
      if (first === null && OPEN_TURN.has(item.type)) first = item.key;
    }
    return { key: first, present };
  };
  const idx = items.findIndex((item) => isUser(item) && itemMessage(item) === message);
  if (idx >= 0) {
    const run = runAfter(idx + 1);
    if (run.present) return run.key;
  }
  let lastUser = -1;
  for (let i = 0; i < items.length; i++) {
    if (isUser(items[i])) lastUser = i;
  }
  if (lastUser < 0) return null;
  return runAfter(lastUser + 1).key;
}

// ---- CAD-551: steps, refusals, and the working row's "what now" ----

/** A tool call that runs `cadence artifact put` (not text that merely names
 *  it), with or without global flags (`cadence --org x artifact put …`). Reads
 *  at most the first words, so its cost does not grow with the text. */
export function isArtifactPutCall(text: string): boolean {
  const words = text.slice(0, 1000).replace(/^\s*(?:bash:\s*)?/i, "").split(/\s+/, 24);
  if (!/^(?:\S*\/)?cadence$/i.test(words[0] ?? "")) return false;
  let flagValue = false;
  for (let i = 1; i < words.length; i++) {
    const w = words[i].toLowerCase();
    if (w === "artifact" && words[i + 1]?.toLowerCase() === "put") return true;
    if (w.startsWith("-")) flagValue = !w.includes("=");
    else if (flagValue) flagValue = false;
    else return false;
  }
  return false;
}

/** One tool call and the result that answered it (when it has). */
export interface ToolStep {
  /** The provider's `tool_use_id` when it sent one — the pair key. */
  id?: string;
  call: ThreadEntry;
  result?: ThreadEntry;
  /** The provider refused the call (a permission denial), not failed it. */
  refused: boolean;
  /** The call's result was an error — including a refusal. */
  error: boolean;
  /** One-line call summary, already redacted by the daemon. */
  summary: string;
  /** A result landed for the call. */
  done: boolean;
}

/** First line of a summary, trimmed — entries are one-liners already,
 * but a provider detail can smuggle newlines onto a row. */
export function stepSummary(text: string | null | undefined): string {
  const line = (text ?? "").split("\n", 1)[0].trim();
  return line.length > 120 ? `${line.slice(0, 117)}…` : line;
}

/**
 * Fold a `tools` item's entries into call/result pairs. `tool_use_id`
 * pairs a result with its call; a result carrying NO id (an old daemon)
 * answers the earliest open call, positionally. A result whose id names
 * no call in the window — the call was paged out — stands alone at the
 * end rather than closing an unrelated open call.
 */
export function toolSteps(entries: ThreadEntry[]): ToolStep[] {
  const steps: ToolStep[] = [];
  const byId = new Map<string, ToolStep>();
  const open: ToolStep[] = [];
  const lone: ToolStep[] = [];
  for (const e of entries) {
    if (e.kind === "tool_call") {
      const id = typeof e.payload?.tool_use_id === "string" ? e.payload.tool_use_id : undefined;
      const step: ToolStep = {
        id,
        call: e,
        refused: false,
        error: false,
        summary: stepSummary(e.text),
        done: false,
      };
      steps.push(step);
      if (id) byId.set(id, step);
      open.push(step);
      continue;
    }
    if (e.kind !== "tool_result") continue;
    const id = typeof e.payload?.tool_use_id === "string" ? e.payload.tool_use_id : undefined;
    let step = (id && byId.get(id)) || undefined;
    if (step?.done) step = undefined;
    if (!step && !id) step = open.find((s) => !s.done);
    const refused = e.payload?.refused === true;
    const error = e.payload?.is_error === true;
    if (!step) {
      lone.push({
        call: e,
        result: e,
        refused,
        error,
        summary: stepSummary(e.text),
        done: true,
      });
      continue;
    }
    step.result = e;
    step.done = true;
    step.refused = refused;
    step.error = error;
  }
  return lone.length ? [...steps, ...lone] : steps;
}

/**
 * The tool call still awaiting its result at the thread's tail — the
 * working row's live "step" line. Results pair with calls in order, so
 * the tail is either an open `tool_call` (returned) or something that
 * ends the tool run — `tool_result`, text, a new message — and there is
 * no step in flight (null).
 */
export function openStep(entries: ThreadEntry[]): ThreadEntry | null {
  let i = entries.length - 1;
  while (i >= 0 && TOOL_KINDS.has(entries[i].kind)) i--;
  const tail = entries.slice(i + 1);
  if (!tail.length) return null;
  const steps = toolSteps(tail);
  const open = steps.filter((s) => !s.done && s.call.kind === "tool_call");
  return open.length ? open[open.length - 1].call : null;
}

const EPIC_ID = /[A-Z][A-Z0-9]{0,9}-\d+/;
const EPIC_JSON = new RegExp(`"epic"\\s*:\\s*"(${EPIC_ID.source})"`);
const PLAN_VERB = /\bplan (?:propose[ds]?|show|approve|reject)\b/;
const EPIC_WORD = new RegExp(`\\b(${EPIC_ID.source})\\b`, "g");

/** The epic a `plan propose|show|approve|reject … X-1` command names: on each
 *  line, the first epic id after the first plan verb. One verb search and one
 *  id search per line, so the cost is linear in the text. */
function planCommandEpic(text: string): string | null {
  for (const line of text.split("\n")) {
    const verb = PLAN_VERB.exec(line);
    if (!verb) continue;
    EPIC_WORD.lastIndex = verb.index + verb[0].length;
    const epic = EPIC_WORD.exec(line);
    if (epic) return epic[1];
  }
  return null;
}
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
    (TOOL_KINDS.has(entry.kind) ? planCommandEpic(text) : null) ??
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
  /** CAD-1700: the complete thread's newest visible assistant reply seq
   *  (0 when none), independent of the page cursor. Absent on a daemon
   *  that predates it. */
  reply_seq?: number;
  /** CAD-1853: the last `seq` this page scanned, past every row the
   *  projection filtered out. Absent on a daemon that predates it. */
  cursor?: number;
  /** CAD-1853: backward reads only — the LOWEST `seq` this page scanned, the
   *  `before` continuation past every row the projection filtered out. */
  scanned_before?: number;
  /** CAD-1853: forward reads only — the scan filled the page, so raw rows may
   *  remain beyond it. `false` is exhaustion; absent on a daemon that
   *  predates it. */
  more_after?: boolean;
  cloud_withheld?: string;
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
  let state = held;
  try {
    const resume = held.cloudRefreshNeeded ? null : resumeSeq(held);
    if (resume !== null) {
      let after: number = resume;
      for (let i = 0; i < CATCH_UP_PAGES; i++) {
        const page = await read.after(after, limit);
        const visible = (page.entries ?? []).length;
        state = mergePage(state, page);
        if (state.cloudRefreshNeeded) break;
        // CAD-1853: resume past every row the daemon scanned, not just the
        // ones it kept. A page of another source's cloud entries is filtered
        // out of the projection, shows nothing, and still carries the cursor
        // beyond them; ignoring it would read the same hidden page forever.
        const next: number =
          typeof page.cursor === "number" &&
          Number.isSafeInteger(page.cursor) &&
          page.cursor > after
            ? page.cursor
            : (lastSeq(state) ?? after);
        if (next > after) state = { ...state, cursor: next };
        // Exhaustion is a property of the RAW scan, not of the kept rows: a
        // projection can hide every row of a FULL page, so a small visible
        // count no longer proves the thread's end. Prefer the daemon's own
        // raw-page answer; a daemon that predates it keeps the visible-count
        // rule.
        const exhausted =
          typeof page.more_after === "boolean"
            ? !page.more_after
            : visible > 0 && visible < limit;
        if (exhausted) return state;
        if (next <= after) return state;
        after = next;
      }
      // Too far behind to replay: reopen on the newest page.
    }
    const page = await read.tail(limit);
    const fresh = mergePage(
      state.cloudRefreshNeeded || state.cloudWithheld != null
        ? state
        : { ...EMPTY_THREAD, pending: held.pending, replySeq: state.replySeq },
      page,
    );
    const scanned = Math.max(
      typeof page.cursor === "number" && Number.isSafeInteger(page.cursor)
        ? page.cursor
        : 0,
      resumeSeq(fresh) ?? 0,
    );
    // The tail is a BACKWARD read too. Its lowest scanned seq is where
    // "earlier" continues, even when the projection kept none of the page —
    // without it a fully filtered tail leaves older history unreachable,
    // because there is no held entry to page below.
    const older =
      typeof page.scanned_before === "number" &&
      Number.isSafeInteger(page.scanned_before) &&
      page.scanned_before > 0
        ? page.scanned_before
        : null;
    return {
      ...fresh,
      cloudRefreshNeeded: false,
      ...(scanned > 0 ? { cursor: scanned } : {}),
      ...(older !== null ? { older } : {}),
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
  // Continue below the last BACKWARD SCAN, not below the oldest held entry:
  // a projection can hide a whole page, so the oldest held seq does not move
  // and paging from it would re-read the same hidden page forever. The
  // daemon's `scanned_before` is that boundary; without it (an older daemon)
  // the first held entry is all we have.
  const first = state.entries[0]?.seq;
  const before =
    first === undefined
      ? state.older
      : state.older === undefined
        ? first
        : Math.min(first, state.older);
  if (before === undefined || before <= 1) return null;
  return read.before(before, limit);
}

/**
 * Fold an older page into the CURRENT state (stream frames may have
 * landed while it was in flight): merge by seq, and record whether
 * anything older remains.
 */
export function applyEarlier(state: ThreadState | null, page: ThreadPageLike): ThreadState {
  const merged = mergePage(state, page);
  // The backward boundary only moves DOWN as pages fold in; keeping the
  // smallest boundary read so far is what makes repeated "earlier" presses
  // cross hidden pages instead of asking for the same one again.
  const scanned =
    typeof page.scanned_before === "number" &&
    Number.isSafeInteger(page.scanned_before) &&
    page.scanned_before > 0
      ? page.scanned_before
      : undefined;
  const older =
    scanned === undefined
      ? state?.older
      : state?.older === undefined
        ? scanned
        : Math.min(state.older, scanned);
  return {
    ...merged,
    moreBefore: page.more_before === true,
    ...(older !== undefined ? { older } : {}),
  };
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
