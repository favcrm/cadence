import { useEffect, useId, useRef } from "react";
import Button from "../../ui/Button";
import { IconClose } from "../../ui/icons";
import type { ResourceState } from "../../lib/cache";
import type { IssueDetail } from "../../lib/types";
import { useTranslate } from "../../lib/locale";
import { laneState, peekSummary } from "../issues/model";

interface Props {
  id: string;
  detail: IssueDetail | null;
  readState?: ResourceState<IssueDetail> | null;
  onRetry?: () => void;
  /** Full issue page. Null when the project is unknown. */
  href: string | null;
  onClose: () => void;
}

const STATUS: Record<string, string> = {
  doing: "bg-info/15 text-info",
  review: "bg-warn/15 text-warn",
  done: "bg-ok/15 text-ok",
};

/** A brief, modal preview. The full issue page owns triage and history. */
export default function Drawer({ id, detail, readState, onRetry, href, onClose }: Props) {
  const t = useTranslate();
  const dialogRef = useRef<HTMLDialogElement>(null);
  const titleId = useId();

  useEffect(() => {
    const dialog = dialogRef.current;
    const trigger = document.activeElement;
    const overflow = document.body.style.overflow;
    dialog?.showModal();
    document.body.style.overflow = "hidden";
    return () => {
      dialog?.close();
      document.body.style.overflow = overflow;
      if (trigger instanceof HTMLElement && trigger.isConnected) trigger.focus();
    };
  }, []);

  const agents = detail?.agents ?? [];
  const lane = agents.length === 0 ? t("No lane yet") : agents.map((a) => a.alias).join(", ");
  const accept = !detail || detail.checks.total === 0 ? "none" : `${detail.checks.total} checks`;
  const failed = !detail && readState?.status === "failed";

  return (
    <dialog
      ref={dialogRef}
      className="issue-peek bg-ink-875 border-l border-ink-700 text-ink-200"
      aria-labelledby={titleId}
      data-peek={id}
      onCancel={(event) => { event.preventDefault(); onClose(); }}
      onClick={(event) => {
        if (event.target !== event.currentTarget) return;
        const rect = event.currentTarget.getBoundingClientRect();
        if (event.clientX < rect.left || event.clientX > rect.right || event.clientY < rect.top || event.clientY > rect.bottom) onClose();
      }}
    >
      <div className="flex items-center gap-2">
        <div className="kicker">{t("Quick peek")}</div>
        <Button variant="ghost" size="sm" className="ml-auto issue-peek-close" aria-label={t("Close preview")} onClick={onClose} icon={<IconClose />} />
      </div>
      <div className="min-w-0">
        <div className="kicker">
          <span className="num">{id}</span>
          {detail ? ` · ${detail.project}` : ""}
        </div>
        <h2 id={titleId} className="text-cardtitle font-semibold text-ink-100 mt-1">{detail?.title ?? t("Issue preview")}</h2>
      </div>
      {detail ? (
        <>
          <div className="flex flex-wrap gap-1.5">
            <span className={`chip ${STATUS[detail.status] ?? "bg-ink-800 text-ink-300"}`}>{detail.status}</span>
            <span className="chip bg-ink-800 text-ink-400">{detail.priority}</span>
            {(detail.parent || detail.component) && <span className="chip bg-ink-800 text-ink-400">{detail.parent ?? detail.component}</span>}
          </div>
          <p className="text-secondary text-ink-400 m-0">{peekSummary(detail.body)}</p>
          <dl className="grid grid-cols-[92px_minmax(0,1fr)] gap-x-2 gap-y-1.5">
            <dt className="slabel">{t("Lane")}</dt>
            <dd className="text-secondary text-ink-200 min-w-0 truncate" title={lane}>{lane}{agents.length > 0 ? ` · ${laneState(agents)}` : ""}</dd>
            <dt className="slabel">{t("Accept")}</dt>
            <dd className="text-secondary text-ink-200">{accept}</dd>
            <dt className="slabel">{t("Owner")}</dt>
            <dd className="text-secondary text-ink-200">{detail.owner ?? t("unassigned")}</dd>
          </dl>
          {readState?.inFlight && (
            <p className="text-label text-ink-400 m-0" role="status">{t("Refreshing preview; showing saved details.")}</p>
          )}
          {readState?.status === "stale" && !readState.inFlight && (
            <div className="flex items-center gap-2 flex-wrap" role="status">
              <span className="text-label text-warn">{t("Showing saved details; the preview may be out of date.")}</span>
              {onRetry && <Button size="sm" onClick={onRetry}>{t("Retry")}</Button>}
            </div>
          )}
        </>
      ) : failed ? (
        <div className="text-label text-fail" role="alert">
          <p className="m-0">{t("Could not load this preview")}{readState.error ? `: ${readState.error}` : "."}</p>
          {onRetry && <Button className="mt-3" onClick={onRetry}>{t("Retry")}</Button>}
        </div>
      ) : (
        <p className="text-secondary text-ink-400 m-0" role="status">{t("Loading issue…")}</p>
      )}
      <div className="mt-auto pt-4">
        {href ? <Button variant="primary" full href={href}>{t("Open full issue")}</Button> : <Button full disabled title={t("Project unknown")}>{t("Open full issue")}</Button>}
      </div>
    </dialog>
  );
}
