import { api, ApiError, type MemberAssistHistory, type MemberAssistSend } from "../../lib/api";
import { type Resource } from "../../lib/cache";
import { cache } from "../../lib/resources";
import { usingPrivateOperation, type PrivateOperation } from "../../lib/privateSession";
import {
  addPending,
  asEntry,
  loadThread,
  newMessageId,
  settlePending,
  type PageReader,
  type ThreadState,
} from "./thread";

/**
 * CAD-1485: the member's own private-assistant chat. Cookie-facing twin
 * of the AOS relay — every call is a same-origin POST carrying only the
 * public `__Host-aos-board-session`; the board mints and re-verifies the
 * assistant envelope server-side. This store keys on the session's
 * verified scope (session id + member `sub`), so a member switch and a
 * re-login never read another session's rows and the draft/history
 * never reuses the Master `home` thread state.
 *
 * Reads are page reads (`history`), never a stream — the bound
 * conversation id is the daemon's stored binding, not a client field.
 * A `send` carries the client-chosen `message` id the daemon dedupes on,
 * so a retry is one message, not two. The actual `queued`/`attention`
 * receipt is read back to the row — an accepted-but-unlaunched send
 * shows its real cause, never a fabricated reply.
 */

/** The store/draft scope one member session owns. `sessionId` binds the
 *  rows to THIS live session, so a sign-out/sign-in as another member
 *  (or the same member re-verified) never inherits the previous chat. */
export function memberScope(sessionId: string | null, sub: string): string {
  return `${sessionId ?? ""}:${sub}`;
}

/** A history page is canonical assistant history or nothing — a missing
 *  or malformed `entries` list is an unknown surface, never an empty
 *  thread. The member's own entries carry `role:"member"`; the shared
 *  render treats the me-bubble as `role:"operator"`, so they are
 *  normalised at the boundary (stored role is never rewritten on the
 *  wire). An entry the parser cannot read is dropped by `asEntry`,
 *  matching the master reader's behaviour. */
function memberPage(page: MemberAssistHistory): {
  thread: unknown;
  entries: unknown[];
  more_before?: boolean;
} {
  if (page === null || typeof page !== "object" || Array.isArray(page)) {
    throw new ApiError("member history answered an unexpected shape", 0);
  }
  const raw = page.entries;
  if (!Array.isArray(raw)) {
    throw new ApiError("member history answered no usable entries", 0);
  }
  // The bound conversation is the canonical thread object the daemon
  // stamps on every history page. Its absence is a real state (the
  // binding has no conversation yet → legitimately empty thread); a
  // present-but-malformed one is an unusable page, never an empty chat.
  let conversation: unknown = null;
  if (page.conversation !== null && page.conversation !== undefined) {
    const c = page.conversation as Record<string, unknown>;
    const id = c.id;
    const alias = c.alias;
    if (
      typeof id !== "string" || id.length === 0 ||
      typeof alias !== "string" || alias.length === 0
    ) {
      throw new ApiError("member history answered an unusable conversation", 0);
    }
    conversation = c;
  }
  const entries = raw.map((e) => {
    if (e && typeof e === "object" && (e as Record<string, unknown>).role === "member") {
      return { ...(e as Record<string, unknown>), role: "operator" };
    }
    return e;
  });
  const renderable = entries.filter((e) => asEntry(e) !== null);
  // A nonempty page the parser cannot read is an unknown surface, never
  // a fresh empty chat — `asEntry` keeps ordinary metadata entries and
  // only drops what genuinely is not a thread entry.
  if (raw.length > 0 && renderable.length === 0) {
    throw new ApiError("member history answered no recognisable entries", 0);
  }
  return {
    thread: conversation,
    entries: renderable,
    more_before: page.more_before === true,
  };
}

/** The page reads the member thread needs: newest page, forward
 *  catch-up, and older pages — all through `memberAssistHistory`. */
export function memberThreadReader(): PageReader {
  const limit = 200;
  return {
    after: (after, lim) =>
      api.memberAssistHistory({ after, limit: lim ?? limit }).then(memberPage),
    tail: (lim) =>
      api.memberAssistHistory({ tail: true, limit: lim ?? limit }).then(memberPage),
    before: (before, lim) =>
      api.memberAssistHistory({ before, limit: lim ?? limit }).then(memberPage),
  };
}

/**
 * One store per member session scope. `loadThread` keeps pending
 * sends across a refetch and reopens on the tail when too far behind —
 * identical semantics to the Master thread store, just keyed to the
 * member's scope and reading the member-assistant endpoints. The
 * privateSession epoch fences a stale store's late pages: a generation
 * that moved mid-read throws the session-changed abort before the
 * write lands.
 */
export function memberThreadStore(scope: string): Resource<ThreadState> {
  return cache.family<string, ThreadState>("memberThread", (key) =>
    loadThread(memberThreadReader(), cache.peek<ThreadState>(`memberThread:${key}`)?.get().data ?? null),
  )(scope);
}

/** Read the send receipt honestly. `state` names the stored state
 *  (`queued` while unlaunched); `duplicate` flags the idempotent
 *  retry; `attention` is the daemon's own unlaunched cause (the
 *  endpoint could not start). Never a fabricated reply. */
export interface MemberSendReceipt {
  /** `false` only when the send was refused (non-2xx) or the session changed. */
  ok: boolean;
  /** The accepted message id. */
  message?: string;
  /** The stored state — `queued` while the assistant could not start. */
  state?: string;
  duplicate?: boolean;
  /** The daemon's truthful cause when the turn could not launch. */
  attention?: string;
  error?: string;
}

/** Queue `text` to the member's own assistant — optimistic entry,
 *  reconciled by the client `message` id against the stored entry.
 *  The actual receipt is consumed: `queued`/`attention` surfaces the
 *  unlaunched cause on the pending row instead of a false "sent". */
export function sendToAssistant(
  scope: string,
  text: string,
  message = newMessageId(),
  operation?: PrivateOperation,
): Promise<MemberSendReceipt> {
  const body = text.trim();
  if (!body) return Promise.resolve({ ok: false, error: "Nothing to send." });
  const store = memberThreadStore(scope);
  return usingPrivateOperation(operation, async (op) => {
    try {
      op.assertCurrent();
      store.write((s) => addPending(s, message, body, Date.now()));
      const receipt: MemberAssistSend = await api.memberAssistSend(body, message);
      op.assertCurrent();
      // A 200 receipt is always an accepted send; `attention` names the
      // honest unlaunched cause, never a refusal (a refusal is the
      // non-2xx catch below).
      const receiptScope = receiptScopeOf(receipt);
      store.write((s) => settlePending(s, message, { ok: true }));
      return receiptScope;
    } catch (e: unknown) {
      const error = e instanceof Error ? (e.message || "The private session changed") : String(e);
      store.write((s) => settlePending(s, message, { ok: false, error }));
      return { ok: false, error };
    }
  }).catch((e: unknown) => {
    const error = e instanceof Error ? (e.message || "The private session changed") : String(e);
    store.write((s) => settlePending(s, message, { ok: false, error }));
    return { ok: false, error };
  });
}

/** The receipt's truthful outcome. A 200 that still could not launch
 *  the assistant is `ok` (the message is durably queued) but names its
 *  cause so the UI can show the honest "queued — assistant not
 *  started" state rather than a silent success. */
function receiptScopeOf(receipt: MemberAssistSend): MemberSendReceipt {
  // The daemon's own `attention` string is the ONLY evidence the send
  // could not launch — the stored state is `queued` for EVERY accepted
  // send (it drains once a real turn runs), so `queued` alone proves
  // nothing about the endpoint and must never be shown as a
  // not-started cause.
  const attention = typeof receipt.attention === "string" && receipt.attention.length > 0
    ? receipt.attention
    : undefined;
  return {
    ok: true,
    message: typeof receipt.message === "string" ? receipt.message : undefined,
    state: typeof receipt.state === "string" ? receipt.state : undefined,
    duplicate: receipt.duplicate === true,
    attention,
  };
}
