import { useState } from "react";
import { fmtTime } from "../../lib/fmt";
import { useQuery } from "../../lib/useResource";
import type { AppPendingSend, AppRun, AppRunOutput } from "../../lib/types";
import Link from "../../ui/Link";
import { ResourceGate } from "../../ui/ResourceStatus";
import type { HomeNeed } from "../home/needs";
import type { Viewer } from "../projects/work";
import {
  filterCounts,
  outputsOf,
  runFilter,
  runStages,
  runStatus,
  runTitle,
  type RunFilter,
} from "./appViewModel";

/** Posts — one card per run, with its stage row, filters and outputs. */
export default function PostsTab({
  runs,
  runsState,
  needs,
  items,
  pending,
  ready,
  viewer,
  onOpenIssue,
  onRetry,
}: {
  runs: AppRun[];
  runsState: ReturnType<typeof useQuery<AppRun[]>>;
  needs: HomeNeed[];
  items: AppRunOutput[];
  pending: AppPendingSend[];
  ready: boolean;
  viewer: Viewer;
  onOpenIssue: (id: string) => void;
  onRetry: () => void;
}) {
  const [filter, setFilter] = useState<RunFilter>("in_progress");
  const counts = filterCounts(runs, needs, items, pending);
  const shown = runs.filter((run) => {
    const mine = outputsOf(run, items, pending);
    return runFilter(run, needs, mine.items.length > 0) === filter;
  });
  return (
    <section className="min-w-0 space-y-2.5" aria-label="posts">
      {runs.length > 0 && <div className="flex flex-wrap items-center gap-1.5">
        {(
          [
            ["in_progress", "In progress"],
            ["needs_you", "Needs you"],
            ["published", "Published"],
          ] as [RunFilter, string][]
        ).map(([id, label]) => (
          <button
            key={id}
            type="button"
            aria-pressed={filter === id}
            onClick={() => setFilter(id)}
            className={`chip ${
              filter === id ? "bg-accent/20 text-accent" : "bg-ink-800 text-ink-400 hover:text-ink-200"
            }`}
          >
            {label}
            {counts[id] > 0 && <span className="num ml-1 opacity-70">{counts[id]}</span>}
          </button>
        ))}
        <span className="text-micro text-ink-500 ml-auto">
          {[
            counts.in_progress > 0 ? `${counts.in_progress} in progress` : null,
            counts.needs_you > 0 ? `${counts.needs_you} need${counts.needs_you === 1 ? "s" : ""} you` : null,
            counts.published > 0 ? `${counts.published} published` : null,
          ]
            .filter(Boolean)
            .join(" · ") || "Nothing yet"}
        </span>
      </div>}
      <ResourceGate
        state={runsState}
        loading="loading posts…"
        failed="could not load posts"
        onRetry={onRetry}
      />
      {runsState.data && runs.length === 0 && (
        <div className="card px-4 py-5 text-secondary text-ink-400">
          <h2 className="text-cardtitle font-medium text-ink-100">No posts yet</h2>
          <p className="mt-1">{!ready
            ? "Complete setup in Settings before starting a post."
            : viewer.readOnly && viewer.operator
              ? "New posts cannot be started on this read-only board."
            : !viewer.operator
              ? "Sign in as the operator to start a post."
              : "Start a post when you’re ready. You’ll review the plan before any work runs."}</p>
        </div>
      )}
      {runsState.data && runs.length > 0 && shown.length === 0 && (
        <div className="card px-4 py-5 text-secondary text-ink-400">
          {filter === "needs_you" ? "No posts need your attention." : filter === "published" ? "No published posts yet." : "No posts in progress."}
        </div>
      )}
      <ul className="space-y-2.5">
        {shown.map((run) => (
          <RunCard
            key={run.epic}
            run={run}
            needs={needs}
            mine={outputsOf(run, items, pending)}
            onOpenIssue={onOpenIssue}
          />
        ))}
      </ul>
    </section>
  );
}

const STAGE_CLS: Record<string, string> = {
  done: "bg-ok/15 text-ok",
  current: "bg-accent/20 text-accent font-semibold",
  waiting: "bg-ink-800 text-ink-500",
};

function RunCard({
  run,
  needs,
  mine,
  onOpenIssue,
}: {
  run: AppRun;
  needs: HomeNeed[];
  mine: { items: AppRunOutput[]; pending: AppPendingSend[] };
  onOpenIssue: (id: string) => void;
}) {
  const status = runStatus(run, needs, mine);
  const stages = runStages(run);
  return (
    <li className="card px-3.5 py-3 min-w-0" data-run={run.epic}>
      <div className="flex flex-wrap items-center gap-2 min-w-0">
        <button
          type="button"
          onClick={() => onOpenIssue(run.epic)}
          className="text-cardtitle font-medium text-ink-100 min-w-0 break-words flex-1 text-left hover:text-accent"
          title={`open ${run.epic}`}
        >
          {runTitle(run.title)}
        </button>
        <span className="flex flex-wrap items-center gap-1.5 shrink-0">
          <span className={`chip ${status.cls}`} data-state={status.text}>
            {status.text}
          </span>
          {status.action && (
            <Link href={status.action.href} className="lnk text-label">
              {status.action.label} →
            </Link>
          )}
        </span>
      </div>
      <div className="flex flex-wrap items-center gap-1 mt-2 min-w-0">
        {stages.map((s, i) => (
          <span key={`${s.label}-${i}`} className="flex items-center gap-1 min-w-0">
            {i > 0 && (
              <span className="text-ink-600" aria-hidden>
                →
              </span>
            )}
            <span className={`chip ${STAGE_CLS[s.tone]}`} data-stage={s.tone}>
              {s.tone === "done" ? "✓ " : ""}
              {s.label}
            </span>
          </span>
        ))}
      </div>
      {run.plan.proposed_at && (
        <div className="text-micro text-ink-500 mt-1.5 num">{fmtTime(run.plan.proposed_at)}</div>
      )}
    </li>
  );
}
