import { useCallback, useLayoutEffect, useRef, useSyncExternalStore } from "react";
import { api, ApiError } from "../../lib/api";
import { cache, resources } from "../../lib/resources";
import { useQuery } from "../../lib/useResource";
import type { Resource } from "../../lib/cache";
import type { MasterState, ThreadRef } from "../../lib/types";
import { EMPTY_THREAD, loadThread, type PageReader, type ThreadEntry, type ThreadState, type PendingMessage } from "../home/thread";

/**
 * CAD-1098: the UI's one view of per-app assistant conversations
 * (docs/design/CAD-1098-per-app-threads.md). Every wire shape lives
 * here and in the four `api.ts` call sites it wraps.
 *
 * Wire (the daemon's final shapes):
 * - `GET  /api/app-installations/<install>/conversations`
 *     -> `{ alias, install_id, general, conversations: [{ id, title, subject,
 *        context_id, is_general, archived, created, updated }] }`
 *     The list always carries the install's General conversation (the
 *     daemon ensures it). An unknown install is 404.
 * - `POST /api/app-installations/<install>/conversations`
 *     `{ context_id, subject?, general? }` -> `{ conversation, created }`
 *     (operator-only; idempotent per subject; none = always new; the daemon
 *     proves install, context and campaign).
 * - `GET  /api/threads/master?conversation=<id>` and the stream take the id.
 * - `POST /api/threads/master/messages` body `conversation: <id>` — a selector
 *   only; the daemon resolves install, subject and scope.
 * - A 404 on the list means the daemon predates conversations: the chat
 *   falls back to the home thread (`legacy`).
 */

export interface Conversation {
  id: string;
  title: string | null;
  subject: string | null;
  isGeneral: boolean;
}

export interface ConversationList {
  /** The daemon has no conversation routes — keep the single home thread. */
  legacy: boolean;
  conversations: Conversation[];
}

export const campaignSubject = (campaignId: string) => `campaign:${campaignId}`;

function parseConversation(raw: unknown): Conversation | null {
  if (!raw || typeof raw !== "object") return null;
  const v = raw as Record<string, unknown>;
  if (typeof v.id !== "string" || v.id === "" || v.archived === true) return null;
  return {
    id: v.id,
    title: typeof v.title === "string" && v.title !== "" ? v.title : null,
    subject: typeof v.subject === "string" ? v.subject : null,
    isGeneral: v.is_general === true,
  };
}

export function parseConversationList(raw: Record<string, unknown>): Conversation[] {
  const rows = Array.isArray(raw.conversations) ? raw.conversations : [];
  return rows.map(parseConversation).filter((c): c is Conversation => c !== null);
}

/** Picker label: General, a server title, the label the app's descriptor
 *  gives its subject kind (`campaign:<id>` is kind `campaign`), `Other` for a
 *  subject kind the descriptor does not declare, or a fresh conversation.
 *  The daemon's verified subject rules stay the authority; the descriptor
 *  only labels what the server returned. */
export function conversationLabel(
  c: Conversation,
  index: number,
  subjects: readonly { kind: string; label: string }[] = [],
): string {
  if (c.isGeneral) return "General";
  if (c.title) return c.title;
  if (c.subject !== null) {
    const kind = c.subject.split(":")[0];
    return subjects.find((s) => s.kind === kind)?.label ?? "Other";
  }
  return `Conversation ${index}`;
}

/** Composer commands that mean "start a fresh conversation" and are never sent. */
export function parseSlash(text: string): "new" | null {
  const t = text.trim().toLowerCase();
  return t === "/new" || t === "/clear" ? "new" : null;
}

// --- lists ---------------------------------------------------------------

// A daemon (or board) that predates conversations answers 501 or a bare
// 404 (no route); an unknown installation answers 404 naming it. Only the
// former is "legacy", and only for that install's list — never a tab-wide flag.
function predatesConversations(e: unknown): boolean {
  if (!(e instanceof ApiError)) return false;
  if (e.status === 501) return true;
  return e.status === 404 && !/Unknown app installation/i.test(e.message ?? "");
}

const listFamily = cache.family<string, ConversationList>("conversations", async (id) => {
  try {
    return { legacy: false, conversations: parseConversationList(await api.conversationList(id)) };
  } catch (e) {
    if (predatesConversations(e)) return { legacy: true, conversations: [] };
    throw e;
  }
});
export const conversationList = (installId: string): Resource<ConversationList> => listFamily(installId);

const inflight = new Map<string, Promise<Conversation>>();

/** Bumped by every explicit selection change (a picker choice, a new
 *  draft subject). A create that resolves after the operator chose
 *  another conversation must not yank the picker back to its result. */
let selectionEpoch = 0;
const selectionActionEpochs = new Map<string, number>();

function markSelectionAction(installId: string): void {
  selectionActionEpochs.set(installId, (selectionActionEpochs.get(installId) ?? 0) + 1);
}

/** Invalidate pending create auto-selection when the displayed frame visits a new owner. */
export function invalidateConversationSelection(): void {
  selectionEpoch += 1;
}

/** Create (or, for a subject, open) a conversation, then refresh the list.
 *  Concurrent calls for the same install, subject AND context share one
 *  request: the daemon's idempotent subject open is only the same request
 *  when the full creation binding matches. A differently bound request
 *  (another context) never borrows the first response — it obtains its
 *  own server validation instead of silently reusing a result minted for
 *  a different scope. The created conversation is selected only when the
 *  operator has not chosen another one while the request was in flight;
 *  the caller still receives the id so a create-on-first-send reaches its
 *  original destination. */
export function createConversation(
  installId: string,
  contextId: string,
  subject?: string,
  options: { shouldSelect?: () => boolean } = {},
): Promise<Conversation> {
  // The full captured creation binding is the identity: install, context
  // and subject. Context is part of it — the daemon proves the scope on
  // create, so two requests for the same subject under different contexts
  // are different requests, not one shared idempotent open.
  const key = `${installId}|${contextId}|${subject ?? ""}`;
  let request = subject ? inflight.get(key) : undefined;
  if (request === undefined) {
    const run = api
      .conversationCreate(installId, contextId, subject)
      .then((raw) => {
        const c = parseConversation(raw.conversation);
        if (!c) throw new Error("The assistant did not return a conversation.");
        return c;
      })
      .then(async (c) => {
        await conversationList(installId).refresh();
        return c;
      })
      .finally(() => {
        if (subject && inflight.get(key) === run) inflight.delete(key);
      });
    request = run;
    if (subject) inflight.set(key, run);
  }
  const epoch = selectionEpoch;
  return request.then((conversation) => {
    if (selectionEpoch === epoch && (options.shouldSelect?.() ?? true)) {
      selectConversation(installId, conversation.id);
    }
    return conversation;
  });
}

/** The campaign's conversation id (created idempotently); null when the
 *  daemon predates conversations, so the caller keeps the home thread. */
export async function openCampaignConversation(
  installId: string,
  contextId: string,
  campaignId: string,
): Promise<string | null> {
  try {
    return (await createConversation(installId, contextId, campaignSubject(campaignId))).id;
  } catch (e) {
    if (predatesConversations(e)) return null;
    throw e;
  }
}

/** Campaign page entry: switch the chat to this campaign's conversation
 *  if it exists; otherwise show it as new (unsaved). Opening a page never
 *  creates a conversation — the first send does (create-on-first-send). */
export async function autoSelectCampaignConversation(
  installId: string,
  campaignId: string,
): Promise<void> {
  const res = conversationList(installId);
  if (res.get().data === null) await res.refresh();
  const existing = res.get().data?.conversations.find((c) => c.subject === campaignSubject(campaignId));
  if (existing) return selectConversation(installId, existing.id);
  if (res.get().data?.legacy === false) setDraftSubject(installId, campaignSubject(campaignId));
}

// --- threads -------------------------------------------------------------

/** Page reads of one conversation's thread (CAD-1168: the pane pages
 *  earlier entries within the selected conversation through `before`). */
export function conversationReader(conversation: string): PageReader {
  return {
    after: (after, limit) => api.thread("master", { after, limit, conversation }),
    tail: (limit) => api.thread("master", { tail: true, limit, conversation }),
    before: (before, limit) => api.thread("master", { before, limit, conversation }),
  };
}

const threadFamily = cache.family<string, ThreadState>("thread:conv", (id) =>
  loadThread(conversationReader(id), cache.peek<ThreadState>(`thread:conv:${id}`)?.get().data ?? null),
);
/** One store per conversation: its own entries, never another's. */
export const conversationThread = (conversation: string): Resource<ThreadState> => threadFamily(conversation);

export function conversationStreamUrl(conversation: string): string {
  return `/api/threads/master/stream?conversation=${encodeURIComponent(conversation)}`;
}

/** A never-loaded store for the pane while no conversation resolves. */
export const idleThread: Resource<ThreadState> = cache.resource<ThreadState>("thread:idle", async () => EMPTY_THREAD);

// --- per-app selection and collapse (client state only) ------------------

const listeners = new Set<() => void>();
const memory = new Map<string, string>();
const notify = () => listeners.forEach((l) => l());

function readKey(key: string): string | null {
  try {
    return globalThis.localStorage?.getItem(key) ?? memory.get(key) ?? null;
  } catch {
    return memory.get(key) ?? null;
  }
}
function writeKey(key: string, value: string) {
  memory.set(key, value);
  try {
    globalThis.localStorage?.setItem(key, value);
  } catch {
    /* storage may be blocked; the in-memory copy still serves this tab */
  }
}
const subscribe = (l: () => void) => {
  listeners.add(l);
  return () => void listeners.delete(l);
};

// An unsaved campaign conversation per install (client state only).
const drafts = new Map<string, string>();
export function setDraftSubject(installId: string, subject: string | null) {
  if (subject === null) drafts.delete(installId);
  else drafts.set(installId, subject);
  selectionEpoch += 1;
  markSelectionAction(installId);
  notify();
}
export function useDraftSubject(installId: string): string | null {
  return useSyncExternalStore(subscribe, () => drafts.get(installId) ?? null);
}

// --- the submitted-envelope outbox (CAD-1168) ----------------------------

/**
 * One immutable submitted envelope per original composer key, held from
 * the moment a send is handed off until its destination's own pending
 * store owns it. A create-on-first-send failure keeps the exact original
 * request here — message id, body, refs, file ids and captured
 * destination — so an explicit Retry resends that envelope and nothing
 * else, beside any later unsent draft. The shared `idleThread` is never
 * used for this: it is not scoped per installation/subject, so another
 * scope's request would leak into it.
 */
export interface OutboxEnvelope {
  message: string;
  text: string;
  refs?: ThreadRef[];
  attachments?: { id: string }[];
  app?: { install_id: string; context_id?: string };
  /** The unsaved subject this envelope was submitted under — a retry
   *  creates/opens THIS subject, never the route's current one. */
  subject?: string;
  at: number;
  state: "sending" | "failed";
  error?: string;
}

const outbox = new Map<string, OutboxEnvelope>();
const outboxListeners = new Set<() => void>();

function outboxNotify(): void {
  for (const listener of outboxListeners) listener();
}

/** Subscribe a pane to the outbox — module state, one entry per key. */
export function subscribeOutbox(listener: () => void): () => void {
  outboxListeners.add(listener);
  return () => {
    outboxListeners.delete(listener);
  };
}

/** The submitted envelope still awaiting its destination's pending row. */
export function outboxFor(key: string): OutboxEnvelope | null {
  return outbox.get(key) ?? null;
}

export function putOutbox(key: string, envelope: OutboxEnvelope): boolean {
  const held = outbox.get(key);
  // An unresolved submitted owner is never replaced implicitly: a new
  // ordinary submission is refused until the operator retries or
  // discards the original. A state update for the SAME message id (a
  // retry, or a failure recorded for the original) is not a replacement.
  if (held && held.message !== envelope.message) return false;
  outbox.set(key, envelope);
  outboxNotify();
  return true;
}

/** Drop the outbox entry for `key` only when it is still `message`'s — a
 *  newer envelope is never removed by an older settlement. */
export function removeOutbox(key: string, message: string): void {
  const held = outbox.get(key);
  if (!held || held.message !== message) return;
  outbox.delete(key);
  outboxNotify();
}

/** The pane's typed drafts and attachment queues now live in
 *  `chat/composerStore.ts`, keyed by the same `install|conversation`
 *  identity the composer receives as `storeKey` — one lifetime rule for
 *  text and files, and an in-flight upload survives the remount. */

export function selectConversation(installId: string, id: string) {
  drafts.delete(installId);
  selectionEpoch += 1;
  markSelectionAction(installId);
  writeKey(`chat-conv:${installId}`, id);
  notify();
}

export function setCollapsed(installId: string, collapsed: boolean) {
  writeKey(`chat-collapsed:${installId}`, collapsed ? "1" : "0");
  notify();
}

/** The conversation to show: the stored pick if it still exists, else General. */
export function resolveSelected(list: Conversation[], stored: string | null): Conversation | null {
  return list.find((c) => c.id === stored) ?? list.find((c) => c.isGeneral) ?? list[0] ?? null;
}

export function useSelectedConversationId(installId: string): string | null {
  return useSyncExternalStore(subscribe, () => readKey(`chat-conv:${installId}`));
}

export function useChatCollapsed(installId: string): [boolean, (next: boolean) => void] {
  const raw = useSyncExternalStore(subscribe, () => readKey(`chat-collapsed:${installId}`));
  const set = useCallback((next: boolean) => setCollapsed(installId, next), [installId]);
  return [raw === "1", set];
}

export interface ConversationLinkRequest {
  /** A new value for every visit to a different query target, including A→B→A. */
  visit: number;
  /** Parsed URL selector only; the installation's server list proves membership. */
  target: string | null;
  /** Malformed, empty or duplicate query values fail without selecting a thread. */
  error: string | null;
  /** Optional context selector paired with a permalink; never an authority claim. */
  contextTarget?: string | null;
  contextError?: string | null;
}

export interface ConversationContextProof {
  /** The shell is still reading its verified installation/context receipt. */
  status: "pending" | "ready" | "failed";
  /** The current context from the shell's verified binding, not from the URL. */
  contextId: string | null;
  error: string | null;
}

export interface ActiveConversation {
  state: "loading" | "failed" | "legacy" | "ready";
  error: string | null;
  conversations: Conversation[];
  selected: Conversation | null;
  /** `campaign:<id>` while the chat shows a not-yet-created campaign conversation. */
  draftSubject: string | null;
  /** An explicit host permalink is waiting for or failed server-list validation. */
  requestPending: boolean;
  requestError: string | null;
  /** The thread store the chat shows; null until a conversation resolves. */
  store: Resource<ThreadState> | null;
  retry: () => void;
}

/** The install's conversations plus the one the chat is showing. */
export function useActiveConversation(
  installId: string,
  request: ConversationLinkRequest | null = null,
  contextProof: ConversationContextProof = { status: "ready", contextId: null, error: null },
): ActiveConversation {
  const res = conversationList(installId);
  const list = useQuery(res);
  const stored = useSelectedConversationId(installId);
  const draft = useDraftSubject(installId);
  const selectionAction = useSyncExternalStore(
    subscribe,
    () => selectionActionEpochs.get(installId) ?? 0,
  );
  const retry = useCallback(() => void res.refresh(), [res]);
  const data = list.data;
  const consumedVisit = useRef<number | null>(null);
  const recordedLinkVisit = useRef<number | null>(null);
  const appliedLinkAction = useRef<{ visit: number; action: number } | null>(null);
  const linkedContextRef = useRef<{ installId: string; target: string; contextId: string; action: number } | null>(null);
  const contextTarget = request?.contextTarget ?? null;
  const contextError = request?.contextError ?? null;
  const contextMismatch =
    contextTarget !== null &&
    contextProof.status === "ready" &&
    contextProof.contextId !== contextTarget;
  const verifiedContextError =
    contextError ??
    (contextTarget === null
      ? null
      : contextProof.status === "failed"
        ? contextProof.error ?? "The linked context could not be verified — nothing was opened."
        : contextProof.error ??
          (contextMismatch
            ? "The linked context does not match this installation's verified current context — nothing was opened."
            : null));
  const contextPending = contextTarget !== null && contextProof.status === "pending";
  const contextMatches = contextTarget === null ||
    (contextProof.status === "ready" && contextProof.contextId === contextTarget && verifiedContextError === null);
  const requestedRows =
    request?.error === null && request.target !== null && data !== null && !data.legacy
      ? data.conversations.filter((conversation) => conversation.id === request.target)
      : [];
  const requested = requestedRows.length === 1 ? requestedRows[0] : null;
  const requestConsumed = request !== null && consumedVisit.current === request.visit;
  useLayoutEffect(() => {
    if (request === null || recordedLinkVisit.current === request.visit) return;
    recordedLinkVisit.current = request.visit;
    if (request.target !== null && contextTarget !== null) {
      linkedContextRef.current = {
        installId,
        target: request.target,
        contextId: contextTarget,
        action: selectionAction,
      };
    } else {
      linkedContextRef.current = null;
    }
  }, [contextTarget, installId, request, selectionAction]);
  // A consumed context-scoped request keeps owning its selector until an
  // explicit selection action or URL removal. Pending, failed and mismatched
  // receipts therefore remain closed under the request's visible error.
  const requestManuallyChanged =
    request !== null &&
    requestConsumed &&
    appliedLinkAction.current?.visit === request.visit &&
    appliedLinkAction.current.action !== selectionAction;
  const linkedContext = linkedContextRef.current;
  const retiredLinkStillSelected =
    request === null &&
    linkedContext !== null &&
    linkedContext.installId === installId &&
    linkedContext.action === selectionAction &&
    stored === linkedContext.target &&
    draft === null;
  const retiredContextPending = retiredLinkStillSelected && contextProof.status === "pending";
  const retiredContextError = retiredLinkStillSelected && contextProof.status === "failed"
    ? contextProof.error ?? "The conversation's original context could not be verified — chat remains closed."
    : null;
  const retiredContextChanged = retiredLinkStillSelected && contextProof.status === "ready" &&
    contextProof.contextId !== linkedContext?.contextId;
  const requestOwnsSelection =
    request !== null &&
    (!requestConsumed ||
      (!requestManuallyChanged && stored === request.target && draft === null));
  const ignoreExpiredLinkedPick = retiredContextChanged;

  useLayoutEffect(() => {
    if (!ignoreExpiredLinkedPick || data === null || data.legacy || draft !== null) return;
    const fallback = resolveSelected(data.conversations, null);
    if (fallback !== null && stored !== fallback.id) selectConversation(installId, fallback.id);
  }, [data, draft, ignoreExpiredLinkedPick, installId, stored]);

  useLayoutEffect(() => {
    if (
      request === null ||
      request.error !== null ||
      verifiedContextError !== null ||
      contextPending ||
      !contextMatches ||
      request.target === null ||
      requestConsumed ||
      data === null ||
      data.legacy ||
      requestedRows.length !== 1
    ) {
      return;
    }
    // Mark before notifying subscribers so the next render resolves from
    // the ordinary stored selection; picker/New/campaign choices then win.
    consumedVisit.current = request.visit;
    selectConversation(installId, request.target);
    appliedLinkAction.current = {
      visit: request.visit,
      action: selectionActionEpochs.get(installId) ?? 0,
    };
    if (contextTarget !== null) {
      linkedContextRef.current = {
        installId,
        target: request.target,
        contextId: contextTarget,
        action: selectionActionEpochs.get(installId) ?? 0,
      };
    }
  }, [
    contextMatches,
    contextPending,
    data,
    installId,
    request,
    requestConsumed,
    requestManuallyChanged,
    requestedRows.length,
    selectionAction,
    contextTarget,
    verifiedContextError,
  ]);

  if (data === null) {
    const failed = list.status === "failed";
    return {
      state: failed ? "failed" : "loading",
      error: failed ? (list.error ?? "request failed") : null,
      conversations: [],
      selected: null,
      draftSubject: null,
      requestPending:
        (request !== null && request.error === null && verifiedContextError === null && !failed) || retiredContextPending,
      requestError: request?.error ?? verifiedContextError ?? retiredContextError,
      store: null,
      retry,
    };
  }
  if (data.legacy) {
    return {
      state: "legacy",
      error: null,
      conversations: [],
      selected: null,
      draftSubject: null,
      requestPending: false,
      requestError: requestOwnsSelection
        ? request?.error ?? verifiedContextError ?? "This installation does not support linked conversations — nothing was opened."
        : retiredContextError,
      // An explicit conversation request must never fall back to Home's thread.
      store: request === null && !retiredLinkStillSelected ? resources.masterThread : null,
      retry,
    };
  }

  if (request === null && retiredLinkStillSelected && (retiredContextPending || retiredContextError !== null)) {
    return {
      state: "ready",
      error: null,
      conversations: data.conversations,
      selected: null,
      draftSubject: null,
      requestPending: retiredContextPending,
      requestError: retiredContextError,
      store: null,
      retry,
    };
  }

  if (requestOwnsSelection) {
    let requestError = request?.error ?? verifiedContextError;
    if (requestError === null && contextPending) {
      return {
        state: "ready",
        error: null,
        conversations: data.conversations,
        selected: null,
        draftSubject: null,
        requestPending: true,
        requestError: null,
        store: null,
        retry,
      };
    }
    if (requestError === null && requestedRows.length > 1) {
      requestError = "This installation returned duplicate rows for the linked conversation — nothing was opened.";
    } else if (requestError === null && requestedRows.length === 0) {
      requestError = "The linked conversation is not available in this installation — nothing was opened.";
    }
    return {
      state: "ready",
      error: null,
      conversations: data.conversations,
      selected: requestError === null ? requested : null,
      draftSubject: null,
      requestPending: false,
      requestError,
      store: requestError === null && requested ? conversationThread(requested.id) : null,
      retry,
    };
  }

  // Keep an explicitly opened unsaved-subject frame stable through the
  // create request's own list refresh. The refreshed list may contain the
  // new subject before createConversation performs its guarded selection;
  // resolving General in that interval would invalidate that same visit
  // and strand its source draft. Explicit selection clears this draft.
  const draftOpen = draft !== null;
  const selected = draftOpen
    ? null
    : resolveSelected(data.conversations, ignoreExpiredLinkedPick ? null : stored);
  return {
    state: "ready",
    error: null,
    conversations: data.conversations,
    selected,
    draftSubject: draftOpen ? draft : null,
    requestPending: false,
    requestError: null,
    store: draftOpen ? idleThread : selected ? conversationThread(selected.id) : null,
    retry,
  };
}

// --- queued notice -------------------------------------------------------

export const QUEUED_NOTICE = "Assistant is finishing another task — your message is queued";

/** An operator message in this conversation has no turn result yet — the
 *  panel keeps polling the master's state so a reload still shows the queue. */
export function hasUnansweredOperator(entries: ThreadEntry[]): boolean {
  const answered = new Set(entries.filter((e) => e.kind === "turn_result").map((e) => e.message));
  return entries.some((e) => e.role === "operator" && e.kind === "message" && !answered.has(e.message));
}

/** True when this conversation is waiting on a reply while the master's
 *  running message belongs to a different conversation. */
export function isQueuedBehindOther(
  turn: MasterState["turn"] | undefined,
  entries: ThreadEntry[],
  pending: PendingMessage[],
): boolean {
  if (!turn || turn.state !== "working") return false;
  if (entries.some((e) => e.message === turn.message)) return false;
  const answered = new Set(entries.filter((e) => e.kind === "turn_result").map((e) => e.message));
  const waiting = entries.some((e) => e.role === "operator" && e.kind === "message" && !answered.has(e.message));
  return waiting || pending.some((p) => p.state !== "failed");
}
