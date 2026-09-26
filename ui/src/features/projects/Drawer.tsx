import { useEffect } from "react";
import { IconClose } from "../../ui/icons";
import Link from "../../ui/Link";
import type { IssueDetail } from "../../lib/types";
import { laneState, peekSummary } from "../issues/model";

interface Props {
  id: string;
  detail: IssueDetail | null;
  /** Full issue page. Null when the project is unknown. */
  href: string | null;
  onClose: () => void;
}

const STATUS: Record<string, string> = {
  doing: "bg-info/15 text-info",
  review: "bg-warn/15 text-warn",
  done: "bg-ok/15 text-ok",
};

/**
 * Quick peek. The issue page holds triage, kick off, and the timeline.
 * This panel is the summary plus a link that opens that page.
 */
export default function Drawer({ id, detail, href, onClose }: Props) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [onClose]);

  const agents = detail?.agents ?? [];
  const lane = agents.length === 0 ? "No lane yet" : agents.map((a) => a.alias).join(", ");
  const accept = !detail || detail.checks.total === 0 ? "none" : `${detail.checks.total} checks`;

  return (
    <>
      <div className="fixed inset-0 bg-scrim z-20" onClick={onClose} />
      <aside
        className="drawer fixed top-0 right-0 h-full w-full sm:w-[22.5rem] bg-ink-875 border-l border-ink-700 z-30 flex flex-col gap-3.5 px-4 pt-4 pb-6"
        aria-label="Quick peek"
        data-peek={id}
      >
        <div className="flex items-center gap-2">
          <div className="kicker">Quick peek</div>
          <button
            aria-label="Close"
            onClick={onClose}
            className="closebtn ml-auto shrink-0 w-8 h-8 grid place-items-center rounded border border-ink-600 text-ink-300 bg-ink-850"
          >
            <IconClose style={{ pointerEvents: "none" }} />
          </button>
        </div>
        <div className="min-w-0">
          <div className="kicker">
            <span className="num">{id}</span>
            {detail ? ` · ${detail.project}` : ""}
          </div>
          <h2 className="text-cardtitle font-semibold text-ink-100 mt-1">{detail?.title ?? "…"}</h2>
        </div>
        {detail && (
          <div className="flex flex-wrap gap-1.5">
            <span className={`chip ${STATUS[detail.status] ?? "bg-ink-800 text-ink-300"}`}>{detail.status}</span>
            <span className="chip bg-ink-800 text-ink-400">{detail.priority}</span>
            {(detail.parent || detail.component) && (
              <span className="chip bg-ink-800 text-ink-400">{detail.parent ?? detail.component}</span>
            )}
          </div>
        )}
        <p className="text-secondary text-ink-400 m-0">{detail ? peekSummary(detail.body) : "Loading…"}</p>
        <dl className="grid grid-cols-[92px_minmax(0,1fr)] gap-x-2 gap-y-1.5">
          <dt className="slabel">Lane</dt>
          <dd className="text-secondary text-ink-200 min-w-0 truncate" title={lane}>
            {lane}
            {agents.length > 0 ? ` · ${laneState(agents)}` : ""}
          </dd>
          <dt className="slabel">Accept</dt>
          <dd className="text-secondary text-ink-200">{accept}</dd>
          <dt className="slabel">Owner</dt>
          <dd className="text-secondary text-ink-200">{detail?.owner ?? "unassigned"}</dd>
        </dl>
        {href ? (
          <Link href={href} className="h-8 px-3 inline-flex items-center justify-center rounded bg-accent text-on-accent text-label font-medium w-fit">
            Open →
          </Link>
        ) : (
          <button type="button" className="h-8 px-3 rounded border border-ink-600 text-label text-ink-400 opacity-50 w-fit" disabled title="Project unknown">
            Open →
          </button>
        )}
      </aside>
    </>
  );
}
