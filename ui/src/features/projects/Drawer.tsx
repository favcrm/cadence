import { useEffect } from "react";
import Button from "../../ui/Button";
import { IconClose } from "../../ui/icons";
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
          <Button variant="ghost" size="sm" className="ml-auto" aria-label="Close" onClick={onClose} icon={<IconClose />} />
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
          <Button variant="primary" full href={href}>Open →</Button>
        ) : (
          <Button full disabled title="Project unknown">Open →</Button>
        )}
      </aside>
    </>
  );
}
