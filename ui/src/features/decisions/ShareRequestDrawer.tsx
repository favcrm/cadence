import { useEffect, useState, type ReactNode } from "react";
import DrawerShell from "../app-shell/shared/DrawerShell";
import { READ_ONLY_COPY } from "../home/AnswerForm";
import { useLocaleTag, useTranslate } from "../../lib/locale";
import { optionsLine } from "../canvas/viewParams";
import { alreadyText, SHARE_NOTE_MAX } from "../home/shareRequest";
import { ageText, isolate, type StagedWrite } from "./stagedWrite";
import type { Attempt } from "./useDecisions";

/**
 * AOS-711 (CAD-2705): an owner's drawer for a member's request for an
 * owner-only report. Who, what (with the view's options), the member's reason,
 * and what sharing does: a read-only PDF, no access to the books. Share report
 * and Decline (with an optional note) go through the share-request routes
 * (`store.decideShare`), never the generic approve/deny. Text from a person is
 * isolated and shown as typed.
 */
const B = ({ children }: { children: ReactNode }) => <bdi>{typeof children === "string" ? isolate(children) : children}</bdi>;

export default function ShareRequestDrawer({
  write,
  attempt,
  busy,
  lost,
  uncertain,
  elsewhere,
  block,
  at,
  total,
  onShare,
  onDecline,
  onCheck,
  onContinue,
  onSkip,
  onClose,
}: {
  write: StagedWrite;
  attempt: Attempt | undefined;
  /** A decision is in flight (this one or another). */
  busy: boolean;
  lost: boolean;
  uncertain: boolean;
  /** Decided by someone else (the list says so, or the server answered 409). */
  elsewhere: boolean;
  block: string | null;
  at: number;
  total: number;
  onShare: (clicks: number) => void;
  onDecline: (note: string, clicks: number) => void;
  onCheck: () => void;
  onContinue: () => void;
  onSkip: () => void;
  onClose: () => void;
}) {
  const t = useTranslate();
  const locale = useLocaleTag();
  const share = write.share!;
  const [declining, setDeclining] = useState(false);
  const [note, setNote] = useState("");
  // A new request is a new subject.
  useEffect(() => {
    setDeclining(false);
    setNote("");
  }, [write.id]);
  const pending = write.status === "pending";
  const off = block !== null || busy || lost || uncertain || !pending || elsewhere;
  const sharing = attempt?.phase === "busy" && attempt.verb === "approve";
  const declined = attempt?.phase === "busy" && attempt.verb === "deny";
  const options = optionsLine(share.params, locale, t);
  const warn = (text: string, role: "alert" | "status" = "alert") => <p className="issue-peek-note warn m-0" role={role}>{text}</p>;

  const state =
    lost ? <p className="issue-peek-note err m-0" role="alert">{t("Only the workspace owner can decide this request.")}</p>
    : attempt?.phase === "elsewhere" && attempt.winner ? warn(alreadyText(attempt.winner, t))
    : elsewhere ? warn(write.decidedBy
      ? t(write.status === "approved" ? "{name} already shared this." : "{name} already declined this.").replace("{name}", () => write.decidedBy ?? "")
      : t("This was already decided elsewhere. The list has been refreshed."))
    : uncertain ? warn(t("We could not confirm the result. It may already have gone through, so do not decide again; check first."))
    : attempt?.phase === "snapshot" ? warn(t("The report couldn't be prepared. It is still waiting, so you can try again later."))
    : block !== null ? warn(t(READ_ONLY_COPY), "status")
    : null;

  const foot = (
    <>
      <div className="crm-drawer-actions">
        {elsewhere ? (
          <button type="button" className="btn btn-sm btn-primary" onClick={onContinue}>{t("Continue")}</button>
        ) : uncertain ? (
          <button type="button" className="btn btn-sm btn-primary" aria-disabled={busy || undefined} onClick={onCheck}>{t("Check again")}</button>
        ) : declining ? (
          <>
            <button type="button" className="btn btn-sm btn-primary" data-share-decline aria-disabled={off || undefined} aria-busy={declined || undefined} onClick={(e) => onDecline(note, e.detail)}>
              {declined ? <><span className="btn-spin" aria-hidden="true" />{t("Declining…")}</> : t("Decline request")}
            </button>
            <button type="button" className="btn btn-sm" aria-disabled={busy || undefined} onClick={() => { if (!busy) { setDeclining(false); setNote(""); } }}>{t("Back")}</button>
          </>
        ) : (
          <>
            <button type="button" className="btn btn-sm btn-primary" data-share-approve aria-disabled={off || undefined} aria-busy={sharing || undefined} title={block !== null ? t(READ_ONLY_COPY) : undefined} onClick={(e) => onShare(e.detail)}>
              {sharing ? <><span className="btn-spin" aria-hidden="true" />{t("Preparing the report…")}</> : t("Share report")}
            </button>
            <button type="button" className="btn btn-sm" data-share-decline-open aria-disabled={off || undefined} onClick={() => { if (!off) setDeclining(true); }}>{t("Decline…")}</button>
          </>
        )}
        <button type="button" className="btn btn-sm btn-ghost" aria-disabled={busy || undefined} onClick={onSkip}>{t("Not now")}</button>
      </div>
      <span className="crm-drawer-note" aria-live="polite">{t("{n} of {m}").replace("{n}", String(at + 1)).replace("{m}", String(total))}</span>
    </>
  );

  return (
    <DrawerShell
      kind="staged"
      modal
      trapFocus
      scrim="board"
      size="md"
      preventClose={busy}
      label={t("Decision")}
      title={<B>{declining ? t("Decline {name}'s request").replace("{name}", () => share.requester) : t("Share a report with {name}?").replace("{name}", () => share.requester)}</B>}
      subtitle={<span>{t("Asked by")} <B>{share.requester}</B> · {ageText(write.createdAt, t)}</span>}
      foot={foot}
      onClose={onClose}
    >
      <div className="wi-panel dx-panel" data-share-drawer>
        {state}
        {declining ? (
          <div className="dx-deny-box">
            <label className="rv-h" htmlFor="share-decline-note">{t("Note to {name} (optional)").replace("{name}", () => share.requester)}</label>
            <textarea
              id="share-decline-note"
              value={note}
              onChange={(e) => setNote(e.target.value)}
              maxLength={SHARE_NOTE_MAX}
              rows={3}
              placeholder={t("Why are you declining? (optional)")}
            />
            <p className="rv-note m-0">{t("{name} sees that you declined, with your note. Nothing else changes.").replace("{name}", () => share.requester)}</p>
          </div>
        ) : (
          <>
            <dl className="dx-kv-list">
              <div className="dx-kv-line"><dt>{t("Who")}</dt><dd><B>{share.requester}</B> · {t("member")}</dd></div>
              <div className="dx-kv-line">
                <dt>{t("What")}</dt>
                <dd><B>{`${share.appLabel} · ${share.viewLabel}`}</B>{options === "" ? null : <><br /><span data-share-options>{options}</span></>}</dd>
              </div>
              {share.reason === undefined ? null : <div className="dx-kv-line"><dt>{t("Reason")}</dt><dd><blockquote className="dx-quote m-0"><B>{share.reason}</B></blockquote></dd></div>}
            </dl>
            <p className="rv-note m-0" data-share-safe>{t("Shares a read-only PDF of this report. {name} gets no access to the books.").replace("{name}", () => share.requester)}</p>
            <p className="rv-note m-0">{t("The PDF is generated at the moment you approve and won't update afterwards.")}</p>
          </>
        )}
      </div>
    </DrawerShell>
  );
}
