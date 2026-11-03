import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { useTranslate } from "../../lib/locale";
import { useResource } from "../../lib/useResource";
import type { ThreadRef } from "../../lib/types";
import Button from "../../ui/Button";
import SharedComposer from "../app-shell/chat/Composer";
import Conversation from "../app-shell/chat/Conversation";
import { AssistantActorProvider, type AssistantActorValue } from "../assistant/AssistantIdentity";
import { atTail, tailTop } from "../home/dock";
import {
  applyEarlier,
  discardPending,
  fetchEarlier,
  lastSeq,
  threadItems,
  visibleWindow,
  WINDOW,
} from "../home/thread";
import { useDockHeight } from "../home/useDockHeight";
import { threadReader } from "../../lib/resources";
import { subscribeThreadsChanged } from "../threads/threadsClient";
import { agentLook } from "./AgentAvatar";
import { agentThreadStore, sendToAgent } from "./agentThread";

/** How a chat page leaves the composer out: a plain reason in its place. */
export type ChatGate = { title: string; body: string; extra?: ReactNode };

/** One agent's page: Home's conversation and composer, its own details panel. */
export default function AgentChat({
  alias,
  heading,
  gate,
  notices,
  panel,
  block,
  onBack,
  onOpenIssue,
}: {
  alias: string;
  heading: ReactNode;
  /** Nothing can answer here, so no composer is offered. */
  gate: ChatGate | null;
  notices?: ReactNode;
  panel: ReactNode;
  block: string | null;
  onBack: () => void;
  onOpenIssue: (id: string) => void;
}) {
  const t = useTranslate();
  const [panelOpen, setPanelOpen] = useState<boolean>(() => window.matchMedia("(min-width: 1400px)").matches);
  const panelButton = useRef<HTMLButtonElement | null>(null);
  const closePanelIfOverlay = useCallback(() => {
    if (window.matchMedia("(min-width: 1400px)").matches) return false;
    setPanelOpen(false);
    panelButton.current?.focus();
    return true;
  }, []);
  // Esc closes the overlaid panel before it leaves the page.
  useEffect(() => {
    if (!panelOpen) return;
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape" && !event.defaultPrevented && closePanelIfOverlay()) event.preventDefault();
    };
    document.addEventListener("keydown", onKey, true);
    return () => document.removeEventListener("keydown", onKey, true);
  }, [panelOpen, closePanelIfOverlay]);

  return (
    <main className="ag-chat" data-panel={panelOpen ? "open" : "closed"}>
      <section className="ag-chat-main" aria-label={t("Conversation with {alias}").replace("{alias}", () => alias)}>
        <header className="ag-chat-head">
          <button type="button" className="ag-chat-back" aria-label={t("Back to Agents")} onClick={onBack}>
            ←
          </button>
          {heading}
          <button
            type="button"
            ref={panelButton}
            className="btn ag-chat-toggle"
            aria-expanded={panelOpen}
            onClick={() => setPanelOpen((open) => !open)}
          >
            {t("Details")}
          </button>
        </header>
        {notices}
        <AgentConversation alias={alias} gate={gate} block={block} onOpenIssue={onOpenIssue} />
      </section>
      {panelOpen && (
        <aside className="ag-chat-panel" aria-label={t("{alias} details").replace("{alias}", () => alias)}>
          <div className="ag-chat-panel-close">
            <Button
              variant="ghost"
              size="sm"
              aria-label={t("Close details")}
              onClick={() => {
                setPanelOpen(false);
                panelButton.current?.focus();
              }}
            >
              ✕
            </Button>
          </div>
          {panel}
        </aside>
      )}
    </main>
  );
}

function AgentConversation({
  alias,
  gate,
  block,
  onOpenIssue,
}: {
  alias: string;
  gate: ChatGate | null;
  block: string | null;
  onOpenIssue: (id: string) => void;
}) {
  const t = useTranslate();
  const store = agentThreadStore(alias);
  const thread = useResource(store);
  const [limit, setLimit] = useState(WINDOW);
  const [loadingEarlier, setLoadingEarlier] = useState(false);
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const dockRef = useRef<HTMLDivElement | null>(null);
  const liveAfter = useRef<number | null>(null);
  const onTail = useRef(true);
  const items = useMemo(() => threadItems(thread.data), [thread.data]);
  const loaded = thread.asOf !== null && thread.asOf !== undefined;
  if (liveAfter.current === null && loaded) liveAfter.current = lastSeq(thread.data) ?? 0;

  // Opening an agent is opening its chat: on a wide screen the caret lands in
  // the composer. A phone keeps its keyboard down until asked.
  useEffect(() => {
    if (!window.matchMedia("(min-width: 901px)").matches) return;
    dockRef.current?.querySelector<HTMLTextAreaElement>("textarea")?.focus({ preventScroll: true });
  }, []);

  // No stream names this thread's rows, so a gentle poll carries a live reply;
  // a Threads frame refreshes it sooner. A gated page has nothing to wait for.
  useEffect(() => {
    if (gate) return;
    const refresh = () => void store.refresh();
    refresh();
    const timer = setInterval(refresh, 6000);
    const unsubscribe = subscribeThreadsChanged(refresh);
    return () => {
      clearInterval(timer);
      unsubscribe();
    };
  }, [store, gate]);

  const toTail = useCallback(() => {
    const el = scrollRef.current;
    if (el) el.scrollTop = tailTop(el.scrollHeight, el.clientHeight);
  }, []);
  useDockHeight(dockRef, scrollRef, (tail) => {
    if (tail) toTail();
  });
  useEffect(() => {
    if (onTail.current) toTail();
  }, [items, toTail]);

  const onEarlier = useCallback(() => {
    const data = store.get().data;
    if (!data) return;
    if (threadItems(data).length > limit) {
      setLimit((l) => l + WINDOW);
      return;
    }
    const page = fetchEarlier(threadReader(alias), data);
    if (!page) {
      store.write((s) => ({ ...(s ?? data), moreBefore: false }));
      return;
    }
    setLoadingEarlier(true);
    void page
      .then((older) => {
        store.write((s) => applyEarlier(s, older));
        setLimit((l) => l + WINDOW);
      })
      .catch(() => undefined)
      .finally(() => setLoadingEarlier(false));
  }, [alias, store, limit]);

  const actor = useMemo<AssistantActorValue>(
    () => ({
      scope: "workspace",
      alias,
      read: { state: "ready", identity: { name: alias, tone: null, look: agentLook(alias), revision: 0 } },
      personal: null,
      workspace: null,
    }),
    [alias],
  );
  const { shown, hidden } = useMemo(() => visibleWindow(items, limit), [items, limit]);
  const failed = thread.error !== null && thread.error !== undefined;

  return (
    <AssistantActorProvider actor={actor}>
      <div className="relative flex-1 min-h-0 flex flex-col" data-chat-panel>
        {failed && (
          <div className="card px-3.5 py-3 mt-3 text-label text-warn break-words" role="alert">
            {t("The thread could not be read —")} {thread.error}{" "}
            <button className="lnk" onClick={() => void store.refresh()}>
              {t("Retry")}
            </button>
          </div>
        )}
        {!gate && !loaded && !failed && (
          <p className="text-label text-ink-500 pt-3">{t("Reading the thread…")}</p>
        )}
        <div
          className="chat-scroll min-h-0 flex-1 overflow-y-auto pt-3"
          ref={scrollRef}
          data-chat-scroll
          onScroll={(event) => {
            const el = event.currentTarget;
            onTail.current = atTail(el.scrollTop, el.scrollHeight, el.clientHeight);
          }}
        >
          {loaded && !failed && items.length === 0 && !gate && (
            <div className="chat-empty">
              <p className="text-label text-ink-400">
                {t("No messages with {alias} yet.").replace("{alias}", () => alias)}
              </p>
            </div>
          )}
          {hidden > 0 || thread.data?.moreBefore === true ? (
            <div className="flex justify-center">
              <button className="lnk text-label disabled:opacity-50" disabled={loadingEarlier} onClick={onEarlier}>
                {loadingEarlier ? t("Loading…") : t("Load earlier messages")}
              </button>
            </div>
          ) : null}
          {!gate && (
            <Conversation
              mode={{ kind: "home" }}
              items={shown}
              legacyTranscript={!items.some((item) => item.type === "answer")}
              readOnly={block !== null}
              liveAfter={liveAfter.current ?? 0}
              onOpenIssue={onOpenIssue}
              onRetry={(message: string, text: string, refs?: ThreadRef[], attachments?: { id: string }[]) => {
                void sendToAgent(alias, text, message, refs, attachments);
              }}
              onDiscard={(message: string) => store.write((s) => discardPending(s, message))}
            />
          )}
        </div>
        <div className="chatdock" ref={dockRef} data-chat-dock>
          {gate ? (
            <div className="card p-3.5 ag-chat-gate" role="status">
              <strong>{gate.title}</strong>
              <p>{gate.body}</p>
              {gate.extra}
            </div>
          ) : (
            <SharedComposer
              density="full"
              storeKey={`threads:${alias}`}
              block={block}
              onSend={({ body, refs, attachments }) => {
                onTail.current = true;
                void sendToAgent(alias, body, undefined, refs, attachments);
              }}
              refocusAfterSend
              attach
              className="card p-2.5 relative"
              placeholder={t("Message {alias}…").replace("{alias}", () => alias)}
              ariaLabel={t("Message to {alias}").replace("{alias}", () => alias)}
              formProps={{ "data-composer": "" }}
            />
          )}
        </div>
      </div>
    </AssistantActorProvider>
  );
}
