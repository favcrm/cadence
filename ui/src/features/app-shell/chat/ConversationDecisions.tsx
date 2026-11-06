import { useEffect, useRef, useState } from "react";
import { useTranslate } from "../../../lib/locale";
import StagedWriteDetail, { noticeText } from "../../decisions/StagedWriteDetail";
import { failureReason, isolate, outcomeOf } from "../../decisions/stagedWrite";
import { conversationDecisions, useDecisions } from "../../decisions/useDecisions";

/** The caller keys this component by the selected, server-proven conversation. */
export default function ConversationDecisions({
  conversation,
  revision,
  onPendingChange,
  onReview,
}: {
  conversation: string;
  revision: number;
  onPendingChange: (conversation: string, pending: boolean) => void;
  onReview: () => void;
}) {
  const t = useTranslate();
  const [store] = useState(() => conversationDecisions(conversation));
  const decisions = useDecisions(true, store);
  const [review, setReview] = useState<{ queue: string[]; at: number } | null>(null);
  const opener = useRef<HTMLButtonElement | null>(null);
  const activity = useRef<HTMLElement | null>(null);
  const previousRevision = useRef(revision);

  useEffect(() => {
    onPendingChange(conversation, decisions.pending.length > 0);
  }, [conversation, decisions.pending.length, onPendingChange]);

  useEffect(() => {
    if (previousRevision.current !== revision) store.backgroundReload();
    previousRevision.current = revision;
  }, [revision, store]);

  const open = (id: string, button: HTMLButtonElement) => {
    const queue = decisions.pending.map((row) => row.id);
    const at = queue.indexOf(id);
    if (at < 0) return;
    onReview();
    opener.current = button;
    store.clearNotice();
    setReview({ queue, at });
  };
  const close = () => {
    setReview(null);
    window.requestAnimationFrame(() => {
      if (opener.current?.isConnected) opener.current.focus();
      else activity.current?.focus();
    });
  };

  return (
    <>
      {(decisions.phase === "error" || decisions.phase === "unavailable" || decisions.notice !== null || decisions.pending.length > 0 || decisions.decided.length > 0) && (
        <section ref={activity} className="app-assistant-activity" aria-label={t("Decisions")} tabIndex={-1}>
          {decisions.phase === "error" && (
            <p className="text-label text-fail" role="alert">
              {t("The list couldn't be loaded. It will try again.")} {" "}
              <button type="button" className="lnk" onClick={() => void store.reloadDecisions()}>{t("Retry")}</button>
            </p>
          )}
          {decisions.phase === "unavailable" && (
            <p className="text-label text-fail" role="alert">{t("Decisions are unavailable.")}</p>
          )}
          {decisions.notice && review === null && <p className="text-label text-ink-300" role="status">{noticeText(decisions.notice, t)}</p>}
          {decisions.pending.map((write) => (
            <article className="app-assistant-card" key={write.id} data-chat-staged>
              <div className="app-assistant-card-head">
                <strong className="text-label text-ink-100"><bdi>{isolate(write.title)}</bdi></strong>
                <span className="text-micro text-ink-400">{t("Waiting")}</span>
              </div>
              {write.app && <p className="text-label text-ink-300"><bdi>{isolate(write.app)}</bdi></p>}
              <button type="button" className="btn btn-sm" aria-label={`${t("Review")} \u2068${isolate(write.title)}\u2069`} onClick={(event) => open(write.id, event.currentTarget)}>
                {t("Review")}
              </button>
            </article>
          ))}
          {decisions.decided.length > 0 && (
            <details className="app-assistant-card">
              <summary className="text-label text-ink-300">{t("Updates")}</summary>
              <ul>
                {decisions.decided.map((write) => {
                  const outcome = outcomeOf(write);
                  const label = outcome === "applied" ? t("Applied") : outcome === "denied" ? t("Denied") : outcome === "failed" ? t("Did not apply") : t("Result not confirmed");
                  return (
                    <li key={write.id} className="text-label text-ink-300">
                      <bdi>{isolate(write.title)}</bdi> · {label}{outcome === "failed" ? ` · ${t(failureReason(write))}` : ""}
                    </li>
                  );
                })}
              </ul>
            </details>
          )}
        </section>
      )}
      {review !== null && (
        <StagedWriteDetail
          decisions={decisions}
          store={store}
          queue={review.queue}
          at={review.at}
          readOnly={false}
          onAt={(at) => setReview((current) => current === null ? null : { ...current, at })}
          onClose={close}
        />
      )}
    </>
  );
}
