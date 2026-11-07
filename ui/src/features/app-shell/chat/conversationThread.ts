import { useEffect } from "react";
import type { Resource } from "../../../lib/cache";
import { subscribeSse, type SseErrorState } from "../../../lib/sse";
import { resources } from "../../../lib/resources";
import type { MasterState } from "../../../lib/types";
import { privateSessionGeneration } from "../../../lib/privateSession";
import { EMPTY_THREAD, applyEarlier, fetchEarlier, reduceFrame, resumeSeq, threadItems, type PageReader, type ThreadPageLike, type ThreadState } from "../../home/thread";
import { applyPreviewFrame, setConversationPreview } from "./conversationPreview";

/**
 * The one conversation thread engine Home and every app pane share
 * (CAD-1700): the live entry stream with resume, and older-page paging.
 * Each surface keeps only what is genuinely its own (Home's dock and
 * plan cards; the pane's picker, directives and visit fences) and its
 * scroll-follow machine, which already runs on the shared
 * `scrollFollow` predicates.
 */

/**
 * Stream `url` into `store` while `enabled`. Resumes after the store's
 * newest seq (never a replay); on reconnect the subscription resumes with
 * `?after=` and the reducer dedupes by seq. The server's first `master`
 * snapshot on every connection makes the bar live before replayed entries;
 * TCP open alone cannot enable actions against a cached previous turn.
 * `preview` frames (CAD-2228) are the one exception: they are transient,
 * replace the in-flight text only, and never fold into `store`.
 */
/** A thread stream's link. `connecting` is the first attempt of this
 *  subscription: it is not live yet, but nothing has gone wrong either. */
export type StreamLink = "live" | "connecting" | SseErrorState;

/** How long a first attempt may stay silent before it counts as an outage: a
 *  stream that hangs must not read as healthy forever. */
const FIRST_CONNECT_GRACE_MS = 5000;

/**
 * Whether `snapshot` proves this connection speaks for `conversation`
 * (CAD-2228). The runtime's own `session.id` is the authority: the host's
 * `actor_identity` and the turn's `conversation` may disagree with it (or be
 * absent), and that is exactly the case that must never paint one
 * conversation's live text in another. A snapshot that binds no session still
 * keeps its master frame and its entries — previews just stay off — so older
 * and native servers keep their durable behaviour.
 */
function previewBound(snapshot: MasterState, conversation: string | null): boolean {
  if (conversation === null) return false;
  const session = snapshot.session;
  if (typeof session?.id !== "string" || session.id === "" || session.id !== conversation) return false;
  // A present turn must name the conversation it is generating for: a turn
  // with no `conversation` (or a malformed one) is unbound, not optional.
  if (snapshot.turn != null) {
    const boundTurn = snapshot.turn.conversation;
    if (typeof boundTurn !== "string" || boundTurn === "" || boundTurn !== conversation) return false;
  }
  // An own `actor_identity` property must agree. Absence is allowed — the
  // runtime-owned session (and turn, above) may prove the selection alone —
  // but a present null or malformed value is unavailable identity, not an
  // absent optional field, so it fails closed.
  if (Object.hasOwn(snapshot, "actor_identity")) {
    const identity = snapshot.actor_identity;
    if (identity === null || typeof identity !== "object") return false;
    const bound = identity.conversation_id;
    if (typeof bound !== "string" || bound === "" || bound !== conversation) return false;
  }
  return true;
}

export function useThreadStream(
  store: Resource<ThreadState>,
  url: string | null,
  enabled: boolean,
  onLink: (state: StreamLink) => void,
  onSnapshot?: () => void,
  graceMs: number = FIRST_CONNECT_GRACE_MS,
  previewConversation: string | null = null,
): void {
  useEffect(() => {
    if (!enabled || url === null) {
      // No subscription, no live text: a paused or switched surface shows
      // only what is stored.
      setConversationPreview(previewConversation, null);
      onLink("live");
      return;
    }
    onLink("connecting");
    // A fresh subscription starts with no live text. The server's preview
    // snapshot, sent after `master` and the committed entries, replaces it.
    // The key is closed over, not read from a ref: this subscription's frames
    // can only ever speak for the conversation it was opened for. A key that
    // resolves later reopens the stream, so the new one gets its own snapshot.
    setConversationPreview(previewConversation, null);
    // A preview paints only after THIS connection's `master` snapshot has
    // been accepted for THIS conversation. Reset on open, on error and at
    // subscription start; a buffered frame from a replaced connection, a
    // mismatched identity snapshot or a replaced conversation fails closed.
    let accepted = false;
    let active = true;
    // Until this subscription has been live or has failed, an open is still
    // its first attempt, not a reconnect.
    let settled = false;
    const session = privateSessionGeneration();
    const grace = setTimeout(() => {
      if (settled) return;
      settled = true;
      onLink("reconnecting");
    }, graceMs);
    const sub = subscribeSse({
      url,
      events: ["master", "entry", "cloud_withheld", "preview"],
      // Resume from what the daemon has SCANNED, not from the newest held
      // entry: a projection can hide rows past the last kept seq, and
      // resuming below them would re-request the same hidden page on every
      // (re)connect.
      lastEventId: String(resumeSeq(store.get().data) ?? 0),
      onOpen: () => {
        // A new connection's master must arrive before any preview paints.
        accepted = false;
        onLink(settled ? "reconnecting" : "connecting");
      },
      onEvent: (event) => {
        if (event.type === "preview") {
          // Ephemeral by contract: it replaces the in-flight text only and
          // never enters the thread store, its seq or its reply watermark.
          if (!accepted || session !== privateSessionGeneration()) return;
          applyPreviewFrame(previewConversation, event.data);
          return;
        }
        store.write((data) => {
          if (event.type === "master") {
            const snapshot = JSON.parse(event.data) as MasterState;
            // A snapshot with no working turn means any live text belonged
            // to a turn that is over. Reconnect snapshots are authoritative.
            if (snapshot.turn == null || snapshot.turn.state !== "working") {
              setConversationPreview(previewConversation, null);
            }
            // Preview admission is stricter than durable master handling: the
            // runtime-owned session must bind to the selected conversation and
            // every present turn/actor binding must agree. An unbound snapshot
            // keeps the master/entry path — older and native servers — but it
            // never enables previews.
            accepted = previewBound(snapshot, previewConversation);
            if (!accepted) setConversationPreview(previewConversation, null);
            if (Object.hasOwn(snapshot, "actor_identity")) {
              const conversation = snapshot.actor_identity?.conversation_id;
              if (typeof conversation !== "string" || conversation === "" || conversation !== data?.conversation) {
                // Fail closed for this connection's previews: the durable
                // mismatch already marks the link reconnecting.
                accepted = false;
                setConversationPreview(previewConversation, null);
                onLink("reconnecting");
                return data ?? EMPTY_THREAD;
              }
              // A connection snapshot can arrive after a newer identity GET.
              // Reconcile through the canonical reader instead of rolling back
              // its opaque revision with an older streamed profile.
              void resources.masterConversation(conversation).refresh();
              if (resources.masterThread.get().data?.conversation === conversation) {
                resources.masterState.write(() => snapshot);
              }
            } else {
              resources.masterState.write(() => snapshot);
            }
            onSnapshot?.();
            settled = true;
            clearTimeout(grace);
            onLink("live");
            return data ?? EMPTY_THREAD;
          }
          const next = reduceFrame(data, event, (entry) => {
            // The committed result ends the turn: the live text it replaces
            // must not outlive it, even if the server's clear frame is lost.
            if (entry.role === "agent" && entry.kind === "turn_result") {
              setConversationPreview(previewConversation, null);
            }
          });
          if (data?.cloudWithheld != null && next.cloudWithheld == null) {
            // A fresh tail can restore rows below the stream's resume cursor.
            // Defer until the store has published `next`.
            queueMicrotask(() => {
              if (active && store.get().data?.cloudRefreshNeeded) void store.invalidate();
            });
          }
          return next;
        });
      },
      onError: (state) => {
        settled = true;
        accepted = false;
        clearTimeout(grace);
        // The stream is down: the text is a snapshot of something that may
        // never have landed. Drop it rather than let it read as live.
        setConversationPreview(previewConversation, null);
        onLink(state);
        if (state === "stopped") void store.refresh();
      },
    });
    return () => {
      active = false;
      accepted = false;
      clearTimeout(grace);
      sub.close();
      setConversationPreview(previewConversation, null);
    };
    // Both callbacks are stable at the call sites; the stream must not
    // reopen when a parent re-renders. `previewConversation` IS a dependency:
    // a key change reopens the stream so a buffered frame of the old one can
    // never be attributed to the new conversation, and the new stream sends
    // its own preview snapshot.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [store, url, enabled, previewConversation]);
}

/** What one "Load earlier" press does. */
export type EarlierStep =
  /** Items already held are hidden by the window: widen it, no fetch. */
  | { kind: "widen" }
  /** Nothing older exists: the store is marked complete. */
  | { kind: "complete" }
  /** An older page is on its way; merge it with `land`. */
  | { kind: "fetch"; page: Promise<ThreadPageLike>; land: (older: ThreadPageLike) => void };

/**
 * One "Load earlier" step for `store` showing `limit` items: widen the
 * window over items already held, else fetch the page before the oldest
 * held entry. The caller owns its window size, loading flag and any
 * visit fencing; `land` merges the page into its own store regardless.
 */
export function earlierStep(store: Resource<ThreadState>, reader: PageReader, limit: number): EarlierStep | null {
  const data = store.get().data;
  const session = privateSessionGeneration();
  if (!data) return null;
  if (threadItems(data).length > limit) return { kind: "widen" };
  const page = fetchEarlier(reader, data);
  if (!page) {
    store.write((cur) => ({ ...(cur ?? data), moreBefore: false }));
    return { kind: "complete" };
  }
  return { kind: "fetch", page, land: (older) => {
    if (session !== privateSessionGeneration()) return;
    store.write((cur) => {
      if (!cur || cur.conversation !== data.conversation) return cur ?? EMPTY_THREAD;
      // Stream status newer than this read wins. Ordinary older history
      // still merges; a stale clean page cannot restore cloud access.
      if (cur.cloudRevision !== data.cloudRevision) {
        const scoped = { ...older };
        if (cur.cloudWithheld == null) delete scoped.cloud_withheld;
        else scoped.cloud_withheld = cur.cloudWithheld;
        return applyEarlier(cur, scoped);
      }
      return applyEarlier(cur, older);
    });
  } };
}
