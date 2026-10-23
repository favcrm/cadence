import { useCallback, useEffect, useMemo, useState, type ReactNode } from "react";
import { api } from "../../lib/api";
import { cache } from "../../lib/resources";
import { useTranslate } from "../../lib/locale";
import { useResource } from "../../lib/useResource";
import Button from "../../ui/Button";
import AgentAvatar from "./AgentAvatar";
import HireProposalCard from "./HireProposalCard";
import { addPending, loadThread, newMessageId, settlePending, threadItems, type ThreadItem, type ThreadState } from "../home/thread";
import { useThreadStream } from "../app-shell/chat/conversationThread";
import Md from "../../ui/Md";
import { HireGate } from "./RecruitCards";

/** The master's Recruit conversation store (CAD-1772), one per conversation id. */
const recruitThread = cache.family<string, ThreadState>("recruitThread", (key) => {
  const [conversation] = key.split(":");
  return loadThread(
    {
      after: (after, limit) => api.thread("master", { after, limit, conversation }),
      tail: (limit) => api.thread("master", { tail: true, limit, conversation }),
      before: (before, limit) => api.thread("master", { before, limit, conversation }),
    },
    cache.peek<ThreadState>(`recruitThread:${key}`)?.get().data ?? null,
  );
});

/** Ask Master to recruit: a chat on the master's dedicated Recruit
 *  conversation. Master answers with a role card (`HireProposalCard`,
 *  rendered by the shared thread renderer); the operator still presses Hire. */
export default function RecruitChat({
  hireBlock,
  onBack,
}: {
  hireBlock: string | null;
  onBack: () => void;
}) {
  const t = useTranslate();
  const [conversation, setConversation] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    if (hireBlock) return;
    let live = true;
    api
      .recruitConversation()
      .then((r) => live && setConversation(r.conversation.id))
      .catch((e: unknown) => live && setError(e instanceof Error ? e.message : String(e)));
    return () => {
      live = false;
    };
  }, [hireBlock]);

  return (
    <main className="agents-page px-4 lg:px-8 pt-6 pb-9 w-full">
      <button type="button" className="rc-back" onClick={onBack}>
        ← {t("Agents")}
      </button>
      <div className="rc-head">
        <div>
          <h1>{t("Ask Master to recruit")}</h1>
          <p>{t("Say what you need. Master picks a role; you still press Hire.")}</p>
        </div>
      </div>
      <HireGate reason={hireBlock} />
      {error && (
        <div className="card px-3.5 py-3 text-label text-fail break-words" role="alert">
          {error}
        </div>
      )}
      {conversation && !hireBlock && <Chat key={conversation} conversation={conversation} />}
    </main>
  );
}

function Chat({ conversation }: { conversation: string }) {
  const t = useTranslate();
  const store = recruitThread(`${conversation}:`);
  const thread = useResource(store);
  useEffect(() => {
    void store.revalidate();
  }, [store]);
  // Live entries over the thread stream, as Home does - no polling.
  useThreadStream(
    store,
    `/api/threads/master/stream?conversation=${encodeURIComponent(conversation)}`,
    thread.data !== null,
    () => {},
  );
  const items = useMemo(() => threadItems(thread.data), [thread.data]);
  const send = useCallback(
    (text: string, message = newMessageId()) => {
      store.write((s) => addPending(s, message, text, Date.now(), undefined, undefined, undefined, conversation));
      api
        .threadSend("master", text, message, undefined, undefined, conversation)
        .then(() => store.write((s) => settlePending(s, message, { ok: true })))
        .catch((e: unknown) =>
          store.write((s) => settlePending(s, message, { ok: false, error: e instanceof Error ? e.message : String(e) })),
        );
    },
    [store, conversation],
  );
  const onRetry = useCallback((message: string, text: string) => send(text, message), [send]);
  const onDiscard = useCallback(
    (message: string) =>
      store.write((s) => ({ ...(s ?? { exists: true, entries: [], pending: [] }), pending: (s?.pending ?? []).filter((p) => p.message !== message) })),
    [store],
  );
  const [draft, setDraft] = useState("");
  const submit = (e: React.FormEvent) => {
    e.preventDefault();
    const text = draft.trim();
    if (!text) return;
    setDraft("");
    send(text);
  };
  const bubble = (item: ThreadItem, index: number) => {
    switch (item.type) {
      case "operator":
        return <Me key={item.key} text={item.entry.text} />;
      case "pending":
        return (
          <Me key={item.key} text={item.pending.text}>
            {item.pending.state === "failed" && (
              <span className="text-micro text-fail">
                {item.pending.error}{" "}
                <button type="button" className="lnk" onClick={() => onRetry(item.pending.message, item.pending.text)}>
                  {t("Retry")}
                </button>{" "}
                <button type="button" className="lnk" onClick={() => onDiscard(item.pending.message)}>
                  {t("Discard")}
                </button>
              </span>
            )}
          </Me>
        );
      case "answer":
      case "commentary": {
        // Master's reasoning and its proposal card share one bubble (mockup).
        const next = items[index + 1];
        return (
          <Master key={item.key}>
            <Md text={item.entry.text} />
            {next?.type === "hire" && !next.reason && <HireProposalCard entry={next.entry} reason={null} />}
          </Master>
        );
      }
      case "hire":
        if (items[index - 1] && ["answer", "commentary"].includes(items[index - 1].type) && !item.reason) return null;
        return (
          <Master key={item.key}>
            <HireProposalCard entry={item.entry} reason={item.reason} />
          </Master>
        );
      default:
        return null;
    }
  };
  return (
    <div className="rc-chat chat" data-chat-panel>
      <div role="log" aria-live="polite" aria-label={t("messages")} className="chat">
        {items.map(bubble)}
      </div>
      <form className="compose" onSubmit={submit} data-composer="">
        <input
          className="field"
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          placeholder={t("Describe who you need…")}
          aria-label={t("Message to Master")}
        />
        <Button type="submit" size="sm" disabled={!draft.trim()}>
          {t("Send")}
        </Button>
      </form>
    </div>
  );
}

function Me({ text, children }: { text: string; children?: ReactNode }) {
  return (
    <div className="msg me">
      <div className="bub">
        {text}
        {children}
      </div>
    </div>
  );
}

function Master({ children }: { children: ReactNode }) {
  return (
    <div className="msg">
      <AgentAvatar slug="robot" size={32} still />
      <div className="bub">{children}</div>
    </div>
  );
}
