import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { api } from "../../lib/api";
import { useResource } from "../../lib/useResource";
import { useTranslate } from "../../lib/locale";
import type { ThreadRef } from "../../lib/types";
import SharedComposer from "../app-shell/chat/Composer";
import Conversation from "../app-shell/chat/Conversation";
import { tailTop } from "./dock";
import { memberHistory, memberThreadStore, memberWrite, sendToAssistant } from "./memberThread";
import { initialFollow, motionBehavior, onSend, type Follow } from "./scrollFollow";
import {
  applyEarlier,
  fetchEarlier,
  lastSeq,
  threadItems,
  visibleWindow,
  WINDOW,
} from "./thread";
import { memberThreadReader } from "./memberThread";
import {
  AssistantAboutButton,
  AssistantActorProvider,
  AssistantFace,
  type AssistantActorValue,
} from "../assistant/AssistantIdentity";
import {
  displayName,
  messageLabel,
  messagePlaceholder,
  readPersonalIdentity,
  scopeDescriptor,
  type AvatarLook,
  type IdentityRead,
} from "../assistant/identity";

/**
 * CAD-1485: the member's own private-assistant chat, rendered inside the
 * existing Home screen in place of the Master workspace. The public
 * session cookie is the only credential; the board mints and verifies
 * the assistant envelope server-side, so this component holds no member
 * authority, no envelope and no signed SSO fragment — just the `sub`
 * and session the verified session already carries.
 *
 * Unlike the Master thread there is no operator stream: the bound
 * conversation id is the daemon's stored binding, read through
 * `memberAssistHistory` page reads (tail on open, `after` to catch up,
 * `before` for older). A gentle poll keeps a live turn's replies
 * flowing; send still lands an optimistic row reconciled by `message`
 * id. The store keys on the session-scoped `scope`, so a member switch
 * and a re-login never cross, and the Master draft is never reused.
 * `key={scope}` on this component remounts it on any scope change, so
 * no local state, follow ref or scroll position leaks across.
 */
/** The private surface's one state read: binding alias, conversation,
 *  bootstrap phase and the committed identity projection. */
interface MemberSurface {
  ok: boolean;
  bootstrap: string | null;
  conversation: string | null;
  alias: string;
  identity: IdentityRead;
}

const LOADING_IDENTITY: IdentityRead = { state: "loading" };

export default function MemberChat({
  scope,
  readOnly,
  onOpenIssue,
}: {
  /** The session-scoped store/draft key — `${sessionId}:${sub}`. */
  scope: string;
  readOnly: boolean;
  onOpenIssue: (id: string) => void;
}) {
  const t = useTranslate();
  const store = memberThreadStore(scope);
  const thread = useResource(store);
  // The member scope is never named in stream-resource invalidations
  // (`memberThread` is not in RESOURCE_NAMES) and `useResource` only
  // subscribes — so nothing else starts the first `loadThread`. This
  // mount revalidate is the same stale-while-revalidate entry the
  // Master thread's `useQuery` uses; without it the store stays
  // `loading` forever and no history truth can ever record.
  useEffect(() => {
    void store.revalidate();
  }, [store]);
  const [surface, setSurface] = useState<MemberSurface | null>(null);
  const [pendingLook, setPendingLook] = useState<AvatarLook | null>(null);
  const alive = useRef(true);
  const identityGate = useRef<{ inflight: boolean; again: boolean; timer: number | null }>({
    inflight: false,
    again: false,
    timer: null,
  });
  const loadSurfaceRef = useRef<() => void>(() => {});
  const [limit, setLimit] = useState(WINDOW);
  const [loadingEarlier, setLoadingEarlier] = useState(false);
  const [sendNote, setSendNote] = useState<string | null>(null);
  const follow = useRef<Follow>(initialFollow());
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const liveAfter = useRef<number>(0);

  const items = useMemo(() => threadItems(thread.data), [thread.data]);
  // History truth is carried in the member payload (round 3): a
  // `Resource.write` stamps ok/error=null/asOf on every optimistic
  // mutation, so `data !== null`/`status` can never prove a read
  // succeeded. `memberHistory` reads the payload's loader-stamped
  // `history` metadata plus the Resource's own retained `error` — a
  // genuine accepted refresh is the only thing that flips `loaded`.
  const history = memberHistory(thread);
  const loaded = history.loaded;
  const moreBefore = thread.data?.moreBefore === true;

  // The assistant's surface state — the bound conversation, whether
  // BOOTSTRAP onboarding is still pending, the stable alias and the
  // committed identity. One read at a time: a scheduled refresh waits
  // behind an in-flight read instead of racing it, and this component is
  // keyed by scope, so an answer from another session/user cannot land.
  const loadSurface = useCallback(() => {
    const gate = identityGate.current;
    if (gate.inflight) {
      gate.again = true;
      return;
    }
    gate.inflight = true;
    api
      .memberAssistState()
      .then((next) => {
        if (!alive.current) return;
        const identity = readPersonalIdentity(next);
        setSurface({
          ok: true,
          bootstrap: next.assistant.bootstrap_state ?? null,
          conversation: next.conversation_id,
          alias: next.assistant.alias,
          identity,
        });
        // The picker's request clears only when the committed identity
        // actually carries the chosen look.
        setPendingLook((requested) =>
          requested !== null && identity.state === "ready" && identity.identity.look === requested
            ? null
            : requested,
        );
      })
      .catch(() => {
        if (!alive.current) return;
        setSurface((prev) =>
          prev === null
            ? { ok: false, bootstrap: null, conversation: null, alias: "", identity: { state: "unavailable" } }
            : { ...prev, ok: false, identity: { state: "unavailable" } },
        );
      })
      .finally(() => {
        gate.inflight = false;
        if (gate.again && alive.current) {
          gate.again = false;
          loadSurfaceRef.current();
        }
      });
  }, []);
  loadSurfaceRef.current = loadSurface;

  const scheduleSurface = useCallback((delay = 0) => {
    const gate = identityGate.current;
    if (gate.timer !== null) window.clearTimeout(gate.timer);
    gate.timer = window.setTimeout(() => {
      gate.timer = null;
      loadSurfaceRef.current();
    }, delay);
  }, []);

  useEffect(() => {
    alive.current = true;
    scheduleSurface(0);
    return () => {
      alive.current = false;
      const gate = identityGate.current;
      if (gate.timer !== null) window.clearTimeout(gate.timer);
      gate.timer = null;
      gate.again = false;
    };
  }, [scheduleSurface]);

  // Accepted history activity — a reply or completed turn landed — is
  // when a committed profile change can become visible. Refresh then,
  // coalesced behind the single-flight read; no second interval starts.
  const identitySeq = lastSeq(thread.data);
  const seenIdentitySeq = useRef<number | null | undefined>(undefined);
  useEffect(() => {
    if (!loaded) return;
    if (seenIdentitySeq.current === undefined) {
      seenIdentitySeq.current = identitySeq;
      return;
    }
    if (identitySeq !== null && identitySeq !== seenIdentitySeq.current) {
      seenIdentitySeq.current = identitySeq;
      scheduleSurface(600);
    }
  }, [loaded, identitySeq, scheduleSurface]);

  // Poll the bound conversation for new entries — the member surface has
  // no operator stream, so a live turn's reply lands through `loadThread`'s
  // forward `after` page (the fetch the store's refresh runs). Slower than
  // the Master stream but truthful; pending sends ride the same store.
  useEffect(() => {
    if (!loaded) return;
    const timer = setInterval(() => void store.refresh(), 6000);
    return () => clearInterval(timer);
  }, [loaded, store]);

  // The seq watermark for the enter animation, captured once loaded.
  useEffect(() => {
    if (loaded && liveAfter.current === 0) {
      liveAfter.current = lastSeq(thread.data) ?? 0;
    }
  }, [loaded, thread.data]);

  const [glideTick, setGlide] = useState(0);
  const scrollPanel = useCallback((kind: "glide" | "jump") => {
    const el = scrollRef.current;
    if (!el) return;
    el.scrollTo({ top: tailTop(el.scrollHeight, el.clientHeight), behavior: motionBehavior(false, kind) });
  }, []);
  const engage = useCallback(() => {
    const next = onSend(follow.current);
    follow.current = next.state;
    setGlide((n) => n + 1);
  }, []);
  useEffect(() => {
    if (glideTick === 0) return;
    scrollPanel("glide");
  }, [glideTick, scrollPanel]);

  // Mirrors the Master thread's earlier-history handler: reveal
  // already-held hidden rows first by widening the bounded window; only
  // then fetch and merge an older page (widening again so the merged
  // rows actually render). An impossible page (the earliest seq is 1)
  // clears `moreBefore` so the button stops offering unreachable history.
  const onEarlier = useCallback(() => {
    const data = store.get().data;
    if (!data) return;
    const hidden = Math.max(0, threadItems(data).length - limit);
    if (hidden > 0) {
      setLimit((l) => l + WINDOW);
      return;
    }
    const page = fetchEarlier(memberThreadReader(), data);
    if (!page) {
      memberWrite(store, (s) => ({ ...(s ?? data), moreBefore: false }));
      return;
    }
    setLoadingEarlier(true);
    void page
      .then((older) => {
        memberWrite(store, (s) => applyEarlier(s, older));
        setLimit((l) => l + WINDOW);
      })
      .catch(() => undefined)
      .finally(() => setLoadingEarlier(false));
  }, [store, limit]);

  const onRetry = useCallback(
    (message: string, text: string, _refs?: ThreadRef[], _attachments?: { id: string }[]) => {
      void sendToAssistant(scope, text, message).then(() => engage());
    },
    [scope, engage],
  );
  const onDiscard = useCallback(
    (message: string) => memberWrite(store, (s) => ({ ...(s ?? { exists: true, entries: [], pending: [] }), pending: (s?.pending ?? []).filter((p) => p.message !== message) })),
    [store],
  );

  const identity = surface?.identity ?? LOADING_IDENTITY;
  // CAD-1817: an avatar pick is an ordinary explicit chat request through
  // the existing private send. It never writes profile state from the
  // browser; the displayed look changes only after an authorized state
  // refresh reads the committed `profile.look`.
  const requestLook = useCallback(
    async (look: AvatarLook) => {
      if (identity.state === "ready" && identity.identity.look === look) return { ok: true };
      try {
        const receipt = await sendToAssistant(scope, `Use the ${look} avatar for yourself.`);
        if (!receipt.ok) return { ok: false, error: receipt.error };
        setPendingLook(look);
        engage();
        scheduleSurface(4000);
        return { ok: true };
      } catch (error) {
        return { ok: false, error: error instanceof Error ? error.message : String(error) };
      }
    },
    [identity, scope, engage, scheduleSurface],
  );

  const actor = useMemo<AssistantActorValue>(
    () => ({
      scope: "personal",
      alias: surface?.alias ?? "",
      read: identity,
      // A read-only board cannot send the chat request, so it offers no
      // editor rather than a control whose only outcome is a refusal.
      personal: readOnly ? null : { requestLook, pendingLook },
      workspace: null,
    }),
    [surface?.alias, identity, requestLook, pendingLook, readOnly],
  );

  const { shown, hidden } = useMemo(() => visibleWindow(items, limit), [items, limit]);
  // A confirmed-empty greeting only when a real history read succeeded
  // and the last read was clean: a failed first read or a stale refresh
  // is unknown history, never "empty" — the greeting waits for an
  // actual successful refresh, and a pending send/discard cannot
  // produce it.
  const empty = loaded && history.error === undefined && items.length === 0;

  return (
    <AssistantActorProvider actor={actor}>
    <main className="home-workspace px-4 lg:px-8 pt-5 pb-3 w-full min-w-0 flex-1 min-h-0 grid gap-5 grid-rows-[minmax(0,1fr)]">
      <section className="min-w-0 flex flex-col min-h-0 gap-4" aria-label={t("assistant thread")}>
        <div className="relative flex-1 min-h-0 flex flex-col" data-chat-panel>
          <div
            className="master-heading assistant-heading pb-2.5 border-b border-ink-700/70"
            data-assistant-identity="personal"
            data-identity-state={identity.state}
          >
            <AssistantFace size={30} />
            <div className="assistant-heading-text">
              <div className="assistant-heading-name">
                <h1 className="text-section font-semibold text-ink-100" data-assistant-name>{displayName(identity, "personal", t)}</h1>
                {surface?.ok === false && (
                  <span className="chip bg-warn/10 text-warn">{t("unavailable")}</span>
                )}
                {surface?.bootstrap === "pending" && (
                  <span className="text-micro text-ink-500">{t("first-time setup")}</span>
                )}
              </div>
              <p className="assistant-scope">{scopeDescriptor("personal", t)}</p>
            </div>
            <div className="assistant-heading-actions">
              <AssistantAboutButton />
            </div>
          </div>

          {history.error?.kind === "failed" && (
            <div className="card px-3.5 py-3 mt-3 text-label text-fail break-words" role="alert">
              {t("The thread could not be read —")} {history.error.text}{" "}
              <button className="lnk" onClick={() => void store.refresh()}>{t("Retry")}</button>
            </div>
          )}
          {history.error?.kind === "stale" && (
            <div className="card px-3.5 py-3 mt-3 text-label text-warn break-words" role="alert">
              {t("The latest refresh failed — showing the last history it read.")} {history.error.text}{" "}
              <button className="lnk" onClick={() => void store.refresh()}>{t("Retry")}</button>
            </div>
          )}
          {!loaded && history.error?.kind !== "failed" && (
            <p className="text-label text-ink-500 pt-3">{t("Reading the thread…")}</p>
          )}

          <div className="chat-scroll min-h-0 flex-1 overflow-y-auto pt-3" ref={scrollRef} data-chat-scroll>
            {empty && (
              <div className="chat-empty">
                <p className="text-label text-ink-400">
                  {t("This is your private assistant — say hello to begin.")}
                </p>
              </div>
            )}
            {hidden > 0 || moreBefore ? (
              <div className="flex justify-center">
                <button className="lnk text-label disabled:opacity-50" disabled={loadingEarlier} onClick={onEarlier}>
                  {loadingEarlier ? t("Loading…") : t("Load earlier messages")}
                </button>
              </div>
            ) : null}
            <Conversation
              mode={{ kind: "home" }}
              items={shown}
              legacyTranscript={!items.some((item) => item.type === "answer")}
              readOnly={readOnly}
              liveAfter={liveAfter.current}
              onOpenIssue={onOpenIssue}
              onRetry={onRetry}
              onDiscard={onDiscard}
            />
          </div>

          {sendNote !== null && (
            <div className="text-micro text-ink-500 px-1 pb-1" role="status">{sendNote}</div>
          )}
          <div className="chatdock" data-chat-dock>
            <SharedComposer
              density="full"
              storeKey={`member-assist:${scope}`}
              block={readOnly ? t("The board is read-only.") : null}
              onSend={({ body }) => {
                void sendToAssistant(scope, body).then((receipt) => {
                  // The actual receipt decides the note: a queued-but-
                  // unlaunched send names its cause, never a fake reply.
                  setSendNote(
                    receipt.ok && receipt.attention
                      ? `${t("Sent")} — ${receipt.attention}`
                      : null,
                  );
                  engage();
                });
              }}
              refocusAfterSend
              attach={false}
              className="card p-2.5 relative"
              placeholder={messagePlaceholder(identity, "personal", t)}
              ariaLabel={messageLabel(identity, "personal", t)}
              formProps={{ "data-composer": "" }}
            />
          </div>
        </div>
      </section>
    </main>
    </AssistantActorProvider>
  );
}
