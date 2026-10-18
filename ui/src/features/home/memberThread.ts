import { api, ApiError, type MemberAssistHistory, type MemberAssistSend } from "../../lib/api";
import { type Resource, type ResourceState } from "../../lib/cache";
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
 *
 * Round-3 review repair — history truth travels INSIDE the member
 * thread payload (`MemberThreadState.history`), never in `Resource`'s
 * status/asOf and never in a side map. `Resource.write` unconditionally
 * stamps `status=ok`, clears `error` and refreshes `asOf`, so optimistic
 * add/settle/discard and cache-window merges would otherwise erase a
 * failed first read (empty data + ok ⇒ false "confirmed empty") or hide
 * a stale-refresh warning. The metadata is written ONLY by the loader
 * path below: a fetch result the Resource accepts (generation+epoch
 * still current) is the only thing that stamps `loaded:true` / clears
 * the retained read error; a dropped response, a fetch-start or the
 * finally tick cannot fabricate it. `memberWrite` carries the existing
 * observation + the live `Resource.error` through every local write so
 * a `write` clears neither. `Resource.reset()` drops the payload, so
 * the observation disappears with the rows — no side map, no listener,
 * no private-session hook.
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

// ---------------------------------------------------------------------
// Member-local history observation (round 3, v2 — payload-carried)
// ---------------------------------------------------------------------

/** The member chat's authoritative history-read truth. `loaded` flips
 *  true ONLY inside the `memberAssistHistory` fetcher — a value the
 *  Resource accepts on success — so a dropped response, a fetch start
 *  or an optimistic write can never stamp it. `readError` carries the
 *  last read failure text while `loaded` rows survive it. */
export interface MemberHistoryObservation {
  loaded: boolean;
  /** A genuine history read failed and its reason survived local writes. */
  readError?: string;
}

/** The member thread payload: shared `ThreadState` plus the member-only
 *  history observation. `Resource<MemberThreadState>` is the member
 *  store's type; the shared helpers still operate on the `ThreadState`
 *  fields (the extension is additive). */
export interface MemberThreadState extends ThreadState {
  history?: MemberHistoryObservation;
}

/** Derive the observation the component trusts. It reads the PAYLOAD's
 *  `history` metadata (only ever set by an accepted loader result or
 *  carried through a member local write) and the Resource's actual
 *  retained `error` — never `status`/`asOf`/`data !== null`, which a
 *  `write` stamps for free. Classification: `failed` when no read ever
 *  succeeded (`!history.loaded`), `stale` when a refresh failed after a
 *  successful one (`history.loaded` true, live `Resource.error` set). */
export interface MemberHistory {
  loaded: boolean;
  error?: { kind: "stale" | "failed"; text: string };
}

export function memberHistory(state: ResourceState<MemberThreadState>): MemberHistory {
  const obs = state.data?.history;
  const loaded = obs?.loaded === true;
  const errText = state.error ?? obs?.readError;
  if (errText) {
    return { loaded, error: { kind: loaded ? "stale" : "failed", text: errText } };
  }
  return { loaded };
}

/**
 * Wrap the member history fetcher so the ONLY `loaded:true` stamp is a
 * fetcher result the Resource accepts. `loadThread` resolves to a
 * `ThreadState`; we stamp `history:{loaded:true}` on it (a clean read —
 * `readError` left absent; a later failed refresh re-records error via
 * the Resource's own stale path into `Resource.error`). The stamp is on
 * the resolved value, applied by the Resource only when its generation/
 * epoch still match — never set before acceptance.
 *
 * One exception the shared loader cannot express for us: `loadThread`'s
 * 404 branch RESOLVES `{...held, exists:false, missing:true}` — the
 * Master's documented "not started" fallback (`thread.ts`). For the
 * member's bound conversation there is no "not started" — a 404 is a
 * failed/absent history READ, never an empty thread. The canonical
 * legitimate "no conversation yet" is a successful 200 page with
 * `entries[]`/`conversation:null` (handled inside `memberPage`), which
 * resolves normally. A resolved `missing:true` therefore throws, so the
 * Resource records failure/stale and retains prior read truth — missing
 * history stays UNKNOWN, never `loaded`.
 */
function memberLoadThread(current: MemberThreadState | null): Promise<MemberThreadState> {
  return loadThread(memberThreadReader(), current).then(
    (state): MemberThreadState => {
      if (state.missing === true) {
        throw new ApiError(
          "member history answered 404 — the bound assistant surface is not available",
          0,
        );
      }
      return { ...state, history: { loaded: true } };
    },
  );
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
export function memberThreadStore(scope: string): Resource<MemberThreadState> {
  return cache.family<string, MemberThreadState>("memberThread", (key) =>
    memberLoadThread(cache.peek<MemberThreadState>(`memberThread:${key}`)?.get().data ?? null),
  )(scope);
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
 * Every local mutation to a member store — optimistic add/settle,
 * discard, cache-window widening, earlier-page merge — goes through
 * this helper. `Resource.write` would stamp `ok`/`error=null`/`asOf`
 * and thereby erase the read truth; instead we capture the current
 * observation AND the live `Resource.error` BEFORE the write and fold
 * them into the locally written payload, so a failed/stale read and its
 * reason survive a pending send/discard/window change. A genuine
 * accepted refresh still overwrites `history` to `loaded:true` and the
 * Resource clears `error` on success — never fabricated.
 *
 * One asymmetry `Resource.write` cannot express (round 5): the
 * Resource queues a trailing read when a superseded request RESOLVES
 * but drops a superseded REJECTION without scheduling one (both drop
 * on the same generation check; only the success branch sets
 * `trailing`). With no successful read yet a pre-load write would
 * leave the surface on "Reading the thread…" forever — the dropped
 * failure records nothing and nothing else re-reads — so this helper
 * calls `invalidate()`: cacheCore.ts:190 marks the flight `trailing`
 * and the finally at 163-168 runs exactly one fresh read (or starts
 * one at once when nothing is flying). `refresh()` would only join
 * the superseded flight and schedule nothing. After a successful read
 * the Resource's own trailing path and the poll keep recovering, so
 * the extra call stops being needed.
 */
export function memberWrite(
  store: Resource<MemberThreadState>,
  fn: (data: MemberThreadState | null) => MemberThreadState,
): void {
  const before = store.get();
  const obs: MemberHistoryObservation | undefined = before.data?.history;
  const err = before.error;
  // Preserve the observation and, when a real read failed, its text in
  // `readError` — the Resource will clear `error`, so the payload keeps
  // it for `memberHistory` to classify. A clean/clean-loaded write keeps
  // `history` as-is (or absent); no successful read is invented.
  const carried: MemberHistoryObservation | undefined = err
    ? { loaded: obs?.loaded === true, readError: err }
    : obs;
  store.write((cur) => {
    const next = fn(cur);
    return carried === undefined ? next : { ...next, history: carried };
  });
  // A local write bumps the Resource generation, so a history request
  // already in flight is superseded either way — resolve or reject.
  // Until one read has genuinely landed, no write may leave the store
  // dependent on that superseded flight: `invalidate()` marks the
  // flight `trailing` (cacheCore.ts:190), so the Resource's finally
  // runs exactly one fresh read whether the dropped request resolved
  // or rejected — `refresh()` would only join it and schedule
  // nothing. With no read in flight `invalidate()` falls through to
  // `refresh()` and starts one. After a successful read the poll and
  // the Resource's own trailing path keep recovering, so the extra
  // call stops being needed.
  if (before.data?.history?.loaded !== true) {
    void store.invalidate();
  }
}

// ---------------------------------------------------------------------
// Send / delivery truth (round 3, v2)
// ---------------------------------------------------------------------

/** Read the send receipt honestly. `state` names the stored state
 *  (`queued` while unlaunched); `duplicate` flags the idempotent
 *  retry; `attention` is the daemon's own unlaunched cause (the
 *  endpoint could not start). Never a fabricated reply. */
export interface MemberSendReceipt {
  /** `true` only when the daemon answered a usable receipt bound to the
   *  message id this send carried AND a nonempty stored state. */
  ok: boolean;
  /** `true` when delivery is genuinely UNKNOWN — a lost response, a
   *  transport failure, a 5xx, a status-0 (no usable server answer), or
   *  a malformed/mismatching 2xx. The row keeps the same message id/text
   *  and "Delivery unconfirmed" with the manual same-id retry; it is
   *  never shown "not sent". */
  unconfirmed?: boolean;
  /** The accepted message id. */
  message?: string;
  /** The stored state — `queued` while the assistant could not start. */
  state?: string;
  duplicate?: boolean;
  /** The daemon's truthful cause when the turn could not launch. */
  attention?: string;
  error?: string;
}

/**
 * Whether a thrown send error is a PROVEN pre-effect refusal — the
 * daemon definitely never committed the message — versus an unknown
 * outcome.
 *
 * Only `ApiError` with a real 4xx counts: on the member-assist send
 * route every refusal (strict body/field/scope rejection at the edge,
 * or a daemon `Rejected` mapped to 403) happens BEFORE the
 * `send_as`/`enqueue_steered_for_with_conversation` commit, so a 4xx
 * proves THAT attempt was refused — the prior same-id attempt's
 * outcome it cannot establish.
 *
 * Everything else is UNCONFIRMED, never "not sent": `ApiError`
 * `status===0` means no usable server answer — the request may already
 * have reached the daemon and committed (connection loss after commit
 * is exactly the lost-response case); a 5xx, a transport `TypeError`, a
 * decode failure, a `DOMException` session-change or any other thrown
 * value likewise cannot prove the send never committed. v1 wrongly
 * treated status-0 as proven refusal; corrected here.
 */
function provenRefusal(e: unknown): e is ApiError {
  return e instanceof ApiError && e.status >= 400 && e.status < 500;
}

/**
 * Settle the pending row for an UNKNOWN/unconfirmed outcome: run the
 * shared `settlePending` (unchanged signature/impl — marks `failed`
 * with the error), then annotate ONLY this member row's `delivery`
 * marker. This keeps the member-specific opt-in off the shared
 * reducer's return shape, so Master callers see the identical payload
 * as before (no `delivery` key added/cleared on shared output).
 */
function settleMemberPending(
  state: MemberThreadState | null,
  message: string,
  outcome: { ok: true } | { ok: false; error: string; delivery?: "unconfirmed" },
): MemberThreadState {
  const settled = settlePending(state, message, outcome) as MemberThreadState;
  const delivery = "delivery" in outcome ? outcome.delivery : undefined;
  return {
    ...settled,
    pending: settled.pending.map((p) =>
      p.message === message ? { ...p, delivery } : p,
    ),
  };
}

/** Queue `text` to the member's own assistant — optimistic entry,
 *  reconciled by the client `message` id against the stored entry.
 *  The actual receipt is consumed: `queued`/`attention` surfaces the
 *  unlaunched cause on the pending row instead of a false "sent".
 *
 *  Delivery truth: `ok` requires a usable receipt matching the
 *  requested `message` id plus a NONEMPTY stored state. Every other
 *  outcome is honest about what is known: a proven pre-effect 4xx
 *  refusal settles `failed` ("not sent"); a lost response, transport
 *  error, 5xx, status-0 (no usable answer), or a malformed/mismatching
 *  2xx settles `failed` with `delivery:"unconfirmed"` so the row reads
 *  "Delivery unconfirmed" — same id, same text, manual same-id retry —
 *  never "not sent" and never an auto-replay/new-id retry. The retry
 *  carries the earlier same-id unconfirmed observation forward while
 *  in flight and after a retry-only failure: a 4xx proves ONLY that
 *  the retry was refused, so it can never downgrade a prior
 *  unconfirmed attempt to "not sent". */
export function sendToAssistant(
  scope: string,
  text: string,
  message = newMessageId(),
  operation?: PrivateOperation,
): Promise<MemberSendReceipt> {
  const body = text.trim();
  if (!body) return Promise.resolve({ ok: false, error: "Nothing to send." });
  const store = memberThreadStore(scope);
  // The same-message pending row THIS scope already holds may carry an
  // earlier attempt's `delivery` observation (member-local; the shared
  // `addPending` would drop it when it replaces the row). Capture it
  // now — only ever `"unconfirmed"` today — so the retry keeps the
  // prior attempt's unknown outcome while it flies and the refusal
  // branch below can distinguish "first-ever 4xx" (not sent) from
  // "retry of an already-unconfirmed send" (still unconfirmed: a 4xx
  // proves only THIS attempt was refused, not the earlier one).
  const previousDelivery = store
    .get()
    .data?.pending.find((p) => p.message === message)?.delivery;
  const hadUnconfirmed = previousDelivery === "unconfirmed";
  /** Settle the pending row from an outcome `e`: proven refusal or unknown. */
  const settleOutcome = (e: unknown): MemberSendReceipt => {
    const text = e instanceof Error ? (e.message || "The private session changed") : String(e);
    if (provenRefusal(e) && !hadUnconfirmed) {
      memberWrite(store, (s) => settleMemberPending(s, message, { ok: false, error: text }));
      return { ok: false, error: text };
    }
    memberWrite(store, (s) =>
      settleMemberPending(s, message, { ok: false, error: text, delivery: "unconfirmed" }),
    );
    return { ok: false, unconfirmed: true, error: text };
  };
  return usingPrivateOperation(operation, async (op) => {
    try {
      op.assertCurrent();
      memberWrite(store, (s) => {
        const added = addPending(s, message, body, Date.now()) as MemberThreadState;
        return previousDelivery === undefined
          ? added
          : {
              ...added,
              pending: added.pending.map((p) =>
                p.message === message ? { ...p, delivery: previousDelivery } : p,
              ),
            };
      });
      const receipt: MemberAssistSend = await api.memberAssistSend(body, message);
      op.assertCurrent();
      const receiptScope = receiptScopeOf(receipt, message);
      if (!receiptScope.ok) {
        // A 2xx that cannot bind to THIS send — missing/mismatched
        // message id or an empty/absent stored state — proves nothing
        // either way: unconfirmed, never accepted, never "not sent".
        memberWrite(store, (s) =>
          settleMemberPending(s, message, { ok: false, error: "delivery could not be confirmed", delivery: "unconfirmed" }),
        );
        return { ok: false, unconfirmed: true, error: "delivery could not be confirmed" };
      }
      memberWrite(store, (s) => settleMemberPending(s, message, { ok: true }));
      return receiptScope;
    } catch (e: unknown) {
      return settleOutcome(e);
    }
  }).catch((e: unknown) => settleOutcome(e));
}

/** The receipt's truthful outcome, bound to THIS send. `ok` requires a
 *  usable receipt: the daemon's `message` must equal the id we sent AND
 *  `state` must be a NONEMPTY string — the actual stored state, which
 *  the daemon stamps from the committed row (`enqueue_steered_for_…`,
 *  or `stored.state` on the native-steering path). An empty/absent
 *  state cannot prove the send committed. A missing/mismatching
 *  receipt establishes neither acceptance nor refusal. `attention` is
 *  the ONLY evidence the send could not launch; the stored state is
 *  `queued` for EVERY accepted send (it drains once a real turn runs),
 *  so `queued` alone proves nothing about the endpoint and is never
 *  shown as a not-started cause. */
function receiptScopeOf(receipt: MemberAssistSend, expectedMessage: string): MemberSendReceipt {
  const attention =
    typeof receipt.attention === "string" && receipt.attention.length > 0
      ? receipt.attention
      : undefined;
  const message = typeof receipt.message === "string" ? receipt.message : undefined;
  const state =
    typeof receipt.state === "string" && receipt.state.length > 0 ? receipt.state : undefined;
  if (message === undefined || message !== expectedMessage || state === undefined) {
    return { ok: false, unconfirmed: true, error: "the send receipt was not usable" };
  }
  return {
    ok: true,
    message,
    state,
    duplicate: receipt.duplicate === true,
    attention,
  };
}
