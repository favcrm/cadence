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
  const [state, setState] = useState<{ ok: boolean; bootstrap?: string | null; conversation?: string | null } | null>(null);
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

  // The assistant's surface state once — the bound conversation and
  // whether BOOTSTRAP onboarding is still pending (the member's first
  // turns run it; the store shows them as ordinary chat). Fenced on
  // scope: a stale state answer after a scope change is dropped.
  useEffect(() => {
    let live = true;
    api.memberAssistState()
      .then((s) => {
        if (live) setState({ ok: true, bootstrap: s.assistant.bootstrap_state ?? null, conversation: s.conversation_id });
      })
      .catch(() => {
        if (live) setState({ ok: false });
      });
    return () => {
      live = false;
    };
  }, [scope]);

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

  const { shown, hidden } = useMemo(() => visibleWindow(items, limit), [items, limit]);
  // A confirmed-empty greeting only when a real history read succeeded
  // and the last read was clean: a failed first read or a stale refresh
  // is unknown history, never "empty" — the greeting waits for an
  // actual successful refresh, and a pending send/discard cannot
  // produce it.
  const empty = loaded && history.error === undefined && items.length === 0;

  return (
    <main className="home-workspace px-4 lg:px-8 pt-5 pb-3 w-full min-w-0 flex-1 min-h-0 grid gap-5 grid-rows-[minmax(0,1fr)]">
      <section className="min-w-0 flex flex-col min-h-0 gap-4" aria-label={t("assistant thread")}>
        <div className="relative flex-1 min-h-0 flex flex-col" data-chat-panel>
          <div className="master-heading flex items-center gap-2 flex-wrap pb-2.5 border-b border-ink-700/70">
            <span className="text-accent text-section" aria-hidden>✳</span>
            <h1 className="text-section font-semibold text-ink-100">{t("Assistant")}</h1>
            {state?.ok === false && (
              <span className="chip bg-warn/10 text-warn">{t("unavailable")}</span>
            )}
            {state?.bootstrap === "pending" && (
              <span className="text-micro text-ink-500">{t("first-time setup")}</span>
            )}
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
              placeholder={t("Message your assistant…")}
              ariaLabel={t("Message to your assistant")}
              formProps={{ "data-composer": "" }}
            />
          </div>
        </div>
      </section>
    </main>
  );
}
