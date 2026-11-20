import { useEffect, useId, useRef, useState } from "react";
import { appHandoffReview, type SupplierBillDraftReview } from "../../lib/appHandoffs";
import { useTranslate } from "../../lib/locale";
import { homeDecisions, useDecisions } from "../decisions/useDecisions";

export default function SupplierBillReview({ conversation, handoff }: { conversation: string; handoff: string }) {
  const t = useTranslate();
  const statusId = useId();
  const status = useRef<HTMLParagraphElement>(null);
  const active = useRef<AbortController | null>(null);
  const [review, setReview] = useState<SupplierBillDraftReview | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [loadFailed, setLoadFailed] = useState(false);
  const [loadAttempt, setLoadAttempt] = useState(0);
  const staged = useDecisions(review?.actionId != null);
  useEffect(() => {
    const abort = new AbortController();
    void appHandoffReview(conversation, handoff, false, abort.signal).then((value) => {
      if (!abort.signal.aborted) setReview(value);
    }, () => {
      if (!abort.signal.aborted) setLoadFailed(true);
    });
    return () => { abort.abort(); active.current?.abort(); };
  }, [conversation, handoff, loadAttempt]);
  if (!review) {
    return loadFailed ? (
      <div className="text-label">
        <p role="alert" className="text-fail">{t("Could not load the draft proposal.")}</p>
        <button type="button" className="btn btn-sm" onClick={() => {
          setLoadFailed(false);
          setLoadAttempt((attempt) => attempt + 1);
        }}>{t("Retry")}</button>
      </div>
    ) : null;
  }
  const actionId = review.actionId;

  const send = async (opener: HTMLButtonElement) => {
    if (active.current) return;
    const abort = new AbortController();
    active.current = abort;
    setBusy(true);
    setError("");
    try {
      const receipt = await appHandoffReview(conversation, handoff, true, abort.signal);
      if (!receipt?.actionId) throw new Error(t("Could not confirm the review request."));
      if (abort.signal.aborted) return;
      if (document.activeElement === opener) requestAnimationFrame(() => status.current?.focus());
      setReview(receipt);
      void homeDecisions.reloadDecisions();
    } catch (cause) {
      if (!abort.signal.aborted) setError(cause instanceof Error ? cause.message : t("Could not confirm the review request."));
    } finally {
      active.current = null;
      if (!abort.signal.aborted) setBusy(false);
    }
  };

  const amount = (minor: number) => {
    const places = review.minorDigits;
    const digits = String(minor).padStart(places + 1, "0");
    return places === 0 ? digits : `${digits.slice(0, -places)}.${digits.slice(-places)}`;
  };
  return (
    <div className="border-t border-ink-700/70 pt-2 text-label text-ink-200 grid gap-2 min-w-0">
      <strong>{t("Supplier bill draft proposal")}</strong>
      <p className="text-ink-400">{t("Source: unverified text from the question above. Account codes are left blank.")}</p>
      <blockquote className="whitespace-pre-wrap break-words" dir="auto">{review.sourceExcerpt}</blockquote>
      <dl className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-3 gap-y-1">
        <dt>{t("Supplier")}</dt><dd className="break-words"><bdi>{review.supplier}</bdi> · <bdi>{review.contactId}</bdi></dd>
        <dt>{t("Supplier reference")}</dt><dd><bdi>{review.reference}</bdi></dd>
        <dt>{t("Bill date")}</dt><dd>{review.issueDate}</dd>
        <dt>{t("Due date")}</dt><dd>{review.dueDate}</dd>
        <dt>{t("Total")}</dt><dd dir="ltr">{review.currency} {amount(review.totalMinor)}</dd>
      </dl>
      <ul className="grid gap-1" aria-label={t("Bill lines")}>
        {review.lines.map((line, index) => (
          <li className="flex justify-between gap-3" key={index}>
            <bdi className="min-w-0 break-words">{line.description}</bdi>
            <bdi className="shrink-0">{amount(line.amountMinor)}</bdi>
          </li>
        ))}
      </ul>
      <p ref={status} id={statusId} role="status" tabIndex={-1}>
        {review.actionId ? t("Sent to owner review. This did not save the bill.")
          : busy ? t("Sending to owner review…") : t("A separate owner approval saves a draft only.")}
      </p>
      {error && <p role="alert" className="text-fail">{error}</p>}
      <div>
        {actionId ? (
          staged.pending.some((action) => action.id === actionId) && (
            <button type="button" className="btn btn-sm" onClick={(event) => homeDecisions.requestReview(actionId, event.currentTarget)}>{t("Open owner review")}</button>
          )
        ) : (
          <button type="button" className="btn btn-sm" aria-disabled={busy} aria-describedby={statusId}
            onClick={(event) => { void send(event.currentTarget); }}>{t("Send to owner review")}</button>
        )}
      </div>
    </div>
  );
}
