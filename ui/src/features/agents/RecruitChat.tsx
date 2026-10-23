import { useCallback, useEffect, useMemo, useState } from "react";
import { api } from "../../lib/api";
import { cache } from "../../lib/resources";
import { useTranslate } from "../../lib/locale";
import { useResource } from "../../lib/useResource";
import Conversation from "../app-shell/chat/Conversation";
import SharedComposer from "../app-shell/chat/Composer";
import { addPending, loadThread, newMessageId, settlePending, threadItems, type ThreadState } from "../home/thread";
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
  onOpenIssue,
}: {
  hireBlock: string | null;
  onBack: () => void;
  onOpenIssue: (id: string) => void;
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
      {conversation && !hireBlock && <Chat key={conversation} conversation={conversation} onOpenIssue={onOpenIssue} />}
    </main>
  );
}

function Chat({ conversation, onOpenIssue }: { conversation: string; onOpenIssue: (id: string) => void }) {
  const t = useTranslate();
  const store = recruitThread(`${conversation}:`);
  const thread = useResource(store);
  useEffect(() => {
    void store.revalidate();
    const timer = setInterval(() => void store.refresh(), 3000);
    return () => clearInterval(timer);
  }, [store]);
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
  return (
    <div className="rc-chat" data-chat-panel>
      <Conversation
        mode={{ kind: "home" }}
        items={items}
        readOnly={false}
        liveAfter={Number.MAX_SAFE_INTEGER}
        onOpenIssue={onOpenIssue}
        onRetry={onRetry}
        onDiscard={onDiscard}
      />
      <SharedComposer
        density="full"
        storeKey={`recruit:${conversation}`}
        block={null}
        onSend={({ body }) => send(body)}
        refocusAfterSend
        attach={false}
        className="card p-2.5 relative"
        placeholder={t("Describe who you need…")}
        ariaLabel={t("Message to Master")}
        formProps={{ "data-composer": "" }}
      />
    </div>
  );
}
