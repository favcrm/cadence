import { useCallback, useSyncExternalStore } from "react";
import { api, ApiError } from "../../lib/api";
import { cache, resources } from "../../lib/resources";
import { useQuery } from "../../lib/useResource";
import type { Resource } from "../../lib/cache";
import type { MasterState } from "../../lib/types";
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

/** Create (or, for a subject, open) a conversation, then refresh the list.
 *  Concurrent calls for the same install and subject share one request. */
export function createConversation(
  installId: string,
  contextId: string,
  subject?: string,
): Promise<Conversation> {
  const key = `${installId}|${subject ?? ""}`;
  const pending = inflight.get(key);
  if (pending && subject) return pending;
  const run = api
    .conversationCreate(installId, contextId, subject)
    .then((raw) => {
      const c = parseConversation(raw.conversation);
      if (!c) throw new Error("The assistant did not return a conversation.");
      return c;
    })
    .then(async (c) => {
      await conversationList(installId).refresh();
      selectConversation(installId, c.id);
      return c;
    })
    .finally(() => inflight.delete(key));
  if (subject) inflight.set(key, run);
  return run;
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

function conversationReader(conversation: string): PageReader {
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
  notify();
}
export function useDraftSubject(installId: string): string | null {
  return useSyncExternalStore(subscribe, () => drafts.get(installId) ?? null);
}

export function selectConversation(installId: string, id: string) {
  drafts.delete(installId);
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

export interface ActiveConversation {
  state: "loading" | "failed" | "legacy" | "ready";
  error: string | null;
  conversations: Conversation[];
  selected: Conversation | null;
  /** `campaign:<id>` while the chat shows a not-yet-created campaign conversation. */
  draftSubject: string | null;
  /** The thread store the chat shows; null until a conversation resolves. */
  store: Resource<ThreadState> | null;
  retry: () => void;
}

/** The install's conversations plus the one the chat is showing. */
export function useActiveConversation(installId: string): ActiveConversation {
  const res = conversationList(installId);
  const list = useQuery(res);
  const stored = useSelectedConversationId(installId);
  const draft = useDraftSubject(installId);
  const retry = useCallback(() => void res.refresh(), [res]);
  const data = list.data;
  if (data === null) {
    const failed = list.status === "failed";
    return { state: failed ? "failed" : "loading", error: failed ? (list.error ?? "request failed") : null, conversations: [], selected: null, draftSubject: null, store: null, retry };
  }
  if (data.legacy) {
    return { state: "legacy", error: null, conversations: [], selected: null, draftSubject: null, store: resources.masterThread, retry };
  }
  const draftOpen = draft !== null && !data.conversations.some((c) => c.subject === draft);
  const selected = draftOpen ? null : resolveSelected(data.conversations, stored);
  return {
    state: "ready",
    error: null,
    conversations: data.conversations,
    selected,
    draftSubject: draftOpen ? draft : null,
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
