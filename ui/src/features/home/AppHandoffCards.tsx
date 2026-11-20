import { useEffect, useId, useRef, useState } from "react";
import {
  appHandoffDecision,
  listAppHandoffs,
  type AppHandoffDecision,
  type AppHandoffMutation,
  type AppHandoffState,
} from "../../lib/appHandoffs";
import { useTranslate } from "../../lib/locale";
import SupplierBillReview from "./SupplierBillReview";

const summary: Record<AppHandoffState, string> = {
  pending: "Awaiting approval. The specialist has not started.",
  approved: "Approved. Run sends only the question shown here.",
  denied: "Declined. The specialist did not start.",
  running: "Submitted to the specialist. Check for the reply.",
  refused: "The handoff was refused. No specialist was started.",
  unconfirmed: "No confirmed result. Check again without resending.",
  unanswered: "The specialist stopped without an answer.",
  answered: "Confirmed specialist reply",
};

const stage: Record<AppHandoffState, number> = {
  pending: 0, approved: 1, running: 2, unconfirmed: 2,
  denied: 3, refused: 3, unanswered: 3, answered: 3,
};

// Only opaque action ids, never questions, recipients or answers. Cache in
// this tab as well as memory so a pending Run survives navigating or reloading.
const sent = new Set<string>();
function submitted(key: string): boolean {
  if (sent.has(key)) return true;
  try { return window.sessionStorage.getItem(key) === "1"; } catch { return false; }
}
function rememberSubmitted(key: string, value: boolean): void {
  if (value) sent.add(key); else sent.delete(key);
  try {
    if (value) window.sessionStorage.setItem(key, "1"); else window.sessionStorage.removeItem(key);
  } catch { /* the in-memory fence still covers this tab */ }
}

function HandoffCard({ conversation, row }: { conversation: string; row: AppHandoffDecision }) {
  const t = useTranslate();
  const id = useId();
  const status = useRef<HTMLParagraphElement>(null);
  const active = useRef<AbortController | null>(null);
  const [receipt, setReceipt] = useState<AppHandoffDecision | null>(null);
  const [busy, setBusy] = useState(false);
  const [failed, setFailed] = useState(false);
  const key = `cadence.appHandoff.submitted.${row.id}`;
  const [runUncertain, setRunUncertain] = useState(() => submitted(key));
  useEffect(() => () => active.current?.abort(), []);
  const decision = receipt && stage[receipt.state] >= stage[row.state] ? receipt : row;
  useEffect(() => {
    if (stage[decision.state] === 3) rememberSubmitted(key, false);
  }, [decision.state, key]);
  const state = runUncertain && (decision.state === "pending" || decision.state === "approved")
    ? "unconfirmed"
    : decision.state;

  const act = async (button: HTMLButtonElement, mutation?: AppHandoffMutation) => {
    if (active.current || (mutation === "run" && submitted(key))) return;
    // Fence before sending: navigation can abort the response, not the server.
    if (mutation === "run") {
      rememberSubmitted(key, true);
      setRunUncertain(true);
    }
    const abort = new AbortController();
    active.current = abort;
    setBusy(true);
    setFailed(false);
    try {
      const result = await appHandoffDecision(conversation, row.id, mutation, abort.signal);
      if (abort.signal.aborted) return;
      // The card must never accept a different question or recipient under the
      // already-reviewed id, even when the destination or locale changes.
      if (result.app !== row.app || result.recipient !== row.recipient || result.question !== row.question)
        throw new Error("The reviewed app handoff changed.");
      setReceipt(result);
    } catch {
      if (abort.signal.aborted) return;
      setFailed(true);
    } finally {
      active.current = null;
      if (!abort.signal.aborted) {
        setBusy(false);
        if (document.activeElement === button) requestAnimationFrame(() => status.current?.focus());
      }
    }
  };

  function action(label: string, mutation?: AppHandoffMutation) {
    return (
      <button type="button" className="btn btn-sm" aria-disabled={busy} aria-describedby={`${id}-question ${id}-status`}
        onClick={(event) => { void act(event.currentTarget, mutation); }}>
        {t(label)}
      </button>
    );
  }

  return (
    <article className="app-assistant-card min-w-0" aria-labelledby={`${id}-recipient`} data-app-handoff-state={state}>
      <header className="app-assistant-card-head">
        <strong id={`${id}-recipient`} className="text-label text-ink-100 [overflow-wrap:anywhere]">
          <bdi>{row.recipient}</bdi> · <bdi>{row.app}</bdi>
        </strong>
        <span className="text-micro text-ink-400">{t("Read-only handoff")}</span>
      </header>
      <div>
        <p className="text-micro text-ink-400">{t("Question to send")}</p>
        <p id={`${id}-question`} className="text-label text-ink-200 whitespace-pre-wrap [overflow-wrap:anywhere]" dir="auto">{row.question}</p>
      </div>
      <p ref={status} id={`${id}-status`} className="text-label text-ink-300" role="status" tabIndex={-1}>
        {busy ? t("Checking handoff…") : t(summary[state])}
      </p>
      {failed && <p className="text-label text-fail" role="alert">{t("The handoff status could not be confirmed.")}</p>}
      {decision.state === "answered" && (
        <p className="text-label text-ink-100 whitespace-pre-wrap [overflow-wrap:anywhere]" dir="auto">{decision.answer}</p>
      )}
      {decision.state === "answered" && <SupplierBillReview conversation={conversation} handoff={row.id} />}
      <div className="flex flex-wrap gap-2">
        {state === "pending" && action("Approve", "approve")}
        {(state === "pending" || state === "approved") && action("Deny", "deny")}
        {state === "approved" && action("Run handoff", "run")}
        {(state === "running" || state === "unconfirmed" || failed) && action("Check reply")}
      </div>
    </article>
  );
}

/** The caller keys this section by the server-proven private Home conversation. */
export default function AppHandoffCards({ conversation, revision }: { conversation: string; revision: number }) {
  const t = useTranslate();
  const [rows, setRows] = useState<AppHandoffDecision[]>([]);
  useEffect(() => {
    const abort = new AbortController();
    void listAppHandoffs(conversation, abort.signal).then((next) => {
      if (!abort.signal.aborted) setRows(next);
    }, () => {
      if (!abort.signal.aborted) setRows([]);
    });
    return () => abort.abort();
  }, [conversation, revision]);
  if (rows.length === 0) return null;
  return (
    <section className="app-assistant-activity" aria-label={t("Private app handoffs")}>
      {rows.map((row) => <HandoffCard key={row.id} conversation={conversation} row={row} />)}
    </section>
  );
}
