import type { IssueCard } from "../types";

const PRIORITY_CHIP: Record<string, string> = {
  P0: "bg-warn/10 text-warn",
  P1: "bg-ink-800 text-ink-200",
  P2: "bg-ink-800 text-ink-400",
  P3: "bg-ink-800 text-ink-500",
};

function prLabel(issue: IssueCard): string | null {
  const ref = issue.refs.find((r) => r.kind === "pr");
  if (!ref) return null;
  if (ref.label) return ref.label;
  const m = ref.url?.match(/\/pull\/(\d+)/) ?? ref.url?.match(/#(\d+)/);
  return m ? `PR #${m[1]}` : "PR";
}

interface Props {
  issue: IssueCard;
  parentTitle?: string;
  busyBy: string[];
  onOpen: (id: string) => void;
}

/** Why a card is not draggable — surfaced on hover. */
export function noDragReason(t: IssueCard): string | null {
  if (t.container) return "container — status rolls up from its children";
  if (t.status_source !== "file")
    return `status is derived from ${t.status_source} — set it there`;
  return null;
}

export default function Card({ issue, parentTitle, busyBy, onOpen }: Props) {
  const t = issue;
  const derived = t.status_source !== "file";
  const noDrag = noDragReason(t);
  const pr = prLabel(t);
  const artifacts = t.counts.artifacts + t.counts.refs;
  const meta: React.ReactNode[] = [];
  if (t.owner) {
    meta.push(
      <span key="o" className="inline-flex items-center gap-1.5 text-ink-400">
        {busyBy.length > 0 && <i className="w-1.5 h-1.5 rounded-full bg-info" />}
        {t.owner}
      </span>,
    );
  }
  if (pr) meta.push(<span key="pr">{pr}</span>);
  if (t.checks.total > 0) {
    meta.push(
      <span key="ck" title="checklist">
        {t.checks.done}/{t.checks.total} done
      </span>,
    );
  }
  if (t.counts.comments > 0) {
    meta.push(
      <span key="cm" title="comments">
        {t.counts.comments} cmt
      </span>,
    );
  }
  if (artifacts > 0) {
    meta.push(
      <span key="ar" title="artifacts">
        {artifacts} art
      </span>,
    );
  }

  return (
    <article
      tabIndex={0}
      role="button"
      aria-label={`${t.id} ${t.title}`}
      draggable={!noDrag}
      title={noDrag ?? undefined}
      onDragStart={(e) => {
        if (noDrag) return;
        e.dataTransfer.setData("text/plain", t.id);
        e.dataTransfer.effectAllowed = "move";
        e.currentTarget.classList.add("opacity-35");
      }}
      onDragEnd={(e) => e.currentTarget.classList.remove("opacity-35")}
      onClick={() => onOpen(t.id)}
      onKeyDown={(e) => {
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          onOpen(t.id);
        }
      }}
      className={`card tcard p-3 ${t.status === "done" ? "opacity-70" : ""} ${
        noDrag ? "cursor-default" : "cursor-grab active:cursor-grabbing"
      }`}
    >
      <div className="flex items-center gap-1.5 flex-wrap">
        <span className="num text-label text-ink-100 font-medium">{t.id}</span>
        <span className={`chip ${PRIORITY_CHIP[t.priority] ?? PRIORITY_CHIP.P2}`}>
          {t.priority}
        </span>
        {derived && (
          <span
            className="chip bg-info/10 text-info"
            title={`status derived from ${t.status_source}, not set by hand`}
          >
            {t.status_source === "job" ? "job" : "derived"}
          </span>
        )}
        {t.component && (
          <span className="chip bg-ink-800 text-ink-500">{t.component}</span>
        )}
        {t.blocked && (
          <span
            className="chip bg-warn/10 text-warn ml-auto"
            title={t.blocked_reason ?? undefined}
          >
            {t.blocked_reason ?? "blocked"}
          </span>
        )}
      </div>
      <h3 className="mt-2 text-secondary text-ink-200 leading-[1.45]">
        {t.title}
      </h3>
      {(t.agents?.length ?? 0) > 0 && (
        <div className="mt-1.5 flex flex-wrap gap-1.5">
          {t.agents!.map((a) => (
            <span
              key={`${a.alias}:${a.task}`}
              className="chip bg-ink-800 !py-[.1rem] text-ink-400"
              title={`${a.alias} · task ${a.task} (${a.task_state})${
                a.message ? ` · ${a.message}` : ""
              }`}
            >
              <i
                className={`w-1.5 h-1.5 rounded-full ${
                  a.state === "attention"
                    ? "bg-fail"
                    : a.message
                      ? "bg-info"
                      : "bg-ink-600"
                }`}
              />
              {a.alias}
              <span className="text-ink-600">{a.task_state}</span>
            </span>
          ))}
        </div>
      )}
      {t.parent && (
        <div className="mt-1.5 num text-micro text-ink-500">
          in {t.parent}
          {parentTitle ? ` · ${parentTitle}` : ""}
        </div>
      )}
      {meta.length > 0 && (
        <div className="mt-2.5 flex flex-wrap items-center gap-x-2.5 gap-y-1 num text-micro text-ink-500">
          {meta}
        </div>
      )}
      {t.blocked_by.length > 0 && (
        <div
          className={`mt-1.5 num text-micro ${
            t.blocked ? "text-warn/80" : "text-ink-500"
          }`}
        >
          {t.blocked ? "waits on" : "after"} {t.blocked_by.join(" · ")}
        </div>
      )}
    </article>
  );
}
