import { useEffect, useRef, useState } from "react";
import { useTranslate } from "../../../lib/locale";
import StagedWriteDetail, { noticeText } from "../../decisions/StagedWriteDetail";
import { failureReason, isolate, outcomeOf, sourceLine, type StagedWrite } from "../../decisions/stagedWrite";
import { api } from "../../../lib/api";
import { customMcpErrorText } from "../../settings/customMcpView";
import InlineToolDecision, { isInlineTool, type InlineEntry, type InlineVerb } from "./InlineToolDecision";
import { conversationDecisions, useDecisions, type DecisionsStore } from "../../decisions/useDecisions";

/** The caller keys this component by the selected, server-proven conversation. */
export default function ConversationDecisions({
  conversation,
  revision,
  onPendingChange,
  onReview,
  store: shared,
  canAlwaysAllow = false,
}: {
  conversation: string;
  revision: number | string;
  onPendingChange?: (conversation: string, pending: boolean) => void;
  onReview?: () => void;
  /** CAD-2364: the pane's store for this conversation, so its answers' chips read the same list. */
  store?: DecisionsStore;
  /** Signed-in members see Always allow on custom MCP cards; `agenticos-v2` authoritatively enforces `403 owner_required` on shared servers and non-MCP actions. */
  canAlwaysAllow?: boolean;
}) {
  const t = useTranslate();
  const [store] = useState(() => shared ?? conversationDecisions(conversation));
  const decisions = useDecisions(true, store);
  const [review, setReview] = useState<{ queue: string[]; at: number } | null>(null);
  const [inline, setInline] = useState<Record<string, InlineEntry>>({});
  const patchInline = (id: string, next: InlineEntry) => setInline((cur) => ({ ...cur, [id]: next }));

  const decideInline = async (write: StagedWrite, verb: InlineVerb) => {
    patchInline(write.id, { write, busy: true, note: undefined });
    const result = await store.decide(write.id, verb === "reject" ? "deny" : "approve");
    if (result !== "decided") {
      const note = result === "access" ? t("Only the workspace owner can decide this request.")
        : result === "elsewhere" ? t("Already decided elsewhere. See Feed.")
        : t("{title}: the result could not be confirmed. See Feed.").replace("{title}", () => write.title);
      patchInline(write.id, { write, busy: false, note });
      return;
    }
    if (verb === "reject") return patchInline(write.id, { write, busy: false, done: "Rejected" });
    if (verb === "approve" || write.customMcp == null) return patchInline(write.id, { write, busy: false, done: "Approved" });
    try {
      await api.customMcpUpdateTools(write.customMcp.connectionId, [{ tool_name: write.customMcp.tool, mode: "read" }]);
      patchInline(write.id, { write, busy: false, done: "Always allowed" });
    } catch (e: unknown) {
      // The call stays approved once; only the standing permission was refused.
      patchInline(write.id, { write, busy: false, done: "Approved", note: t(customMcpErrorText(e)) });
    }
  };
  const inlineCard = (entry: InlineEntry) => (
    <InlineToolDecision key={entry.write.id} entry={entry} canAlwaysAllow={canAlwaysAllow} t={t} onDecide={(verb) => void decideInline(entry.write, verb)} />
  );
  const opener = useRef<HTMLElement | null>(null);
  const activity = useRef<HTMLElement | null>(null);
  const historySummary = useRef<HTMLElement | null>(null);
  const previousRevision = useRef(revision);

  useEffect(() => {
    onPendingChange?.(conversation, decisions.pending.length > 0);
  }, [conversation, decisions.pending.length, onPendingChange]);

  useEffect(() => {
    if (previousRevision.current !== revision) store.backgroundReload();
    previousRevision.current = revision;
  }, [revision, store]);

  const open = (id: string, button: HTMLElement) => {
    const queue = decisions.pending.map((row) => row.id);
    const at = queue.indexOf(id);
    if (at < 0) return;
    onReview?.();
    opener.current = button;
    store.clearNotice();
    setReview({ queue, at });
  };
  // CAD-2364: a chip in an answer opens the same review card.
  const openRef = useRef(open);
  openRef.current = open;
  useEffect(() => store.onReviewRequest((id, button) => openRef.current(id, button)), [store]);
  const close = () => {
    setReview(null);
    window.requestAnimationFrame(() => {
      if (opener.current?.isConnected) opener.current.focus();
      else (activity.current ?? historySummary.current)?.focus();
    });
  };

  return (
    <>
      {decisions.notice && review === null && <p className="m-0 text-label text-ink-300" role="status">{noticeText(decisions.notice, t)}</p>}
      {(decisions.phase === "error" || decisions.phase === "unavailable" || decisions.pending.length > 0 || Object.keys(inline).length > 0) && (
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
          {decisions.pending.map((write) => {
            if (isInlineTool(write)) return inlineCard(inline[write.id] ?? { write, busy: false });
            const source = sourceLine(write, t);
            return (
              <article className="app-assistant-card" key={write.id} data-chat-staged>
                <div className="app-assistant-card-head">
                  <strong className="text-label text-ink-100"><bdi>{isolate(write.title)}</bdi></strong>
                  <span className="text-micro text-ink-400">{t("Waiting")}</span>
                </div>
                {source && <p className="text-label text-ink-300"><bdi>{isolate(source)}</bdi></p>}
                <button type="button" className="btn btn-sm" aria-label={`${t("Review")} \u2068${isolate(write.title)}\u2069`} onClick={(event) => open(write.id, event.currentTarget)}>
                  {t("Review")}
                </button>
              </article>
            );
          })}
          {Object.values(inline)
            .filter((entry) => !decisions.pending.some((row) => row.id === entry.write.id))
            .map(inlineCard)}
        </section>
      )}
      {decisions.decided.length > 0 && (
        <details className="app-decision-history">
          <summary ref={historySummary} className="text-label text-ink-300">{t("Updates")} ({decisions.decided.length})</summary>
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
