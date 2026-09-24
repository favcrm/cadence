import { useEffect, useRef } from "react";
import type { ResourceState } from "../../lib/cache";
import { resources } from "../../lib/resources";
import type { IssueCard } from "../../lib/types";
import { useQuery } from "../../lib/useResource";
import { ResourceGate, StaleChip } from "../../ui/ResourceStatus";
import { HealthBadge, HealthReasons, ProgressBar } from "./HealthBadge";
import { healthView, progressView } from "./work";

/**
 * Projects → a project → Milestones (CAD-432): each milestone's
 * size-weighted progress over its work items and the worst health of its
 * epics, with the reasons and next actions (`cadence milestone ls`).
 * Re-read whenever the tracker's issues change.
 */
export default function Milestones({
  project,
  issues,
  onOpenIssue,
}: {
  project: string;
  issues: ResourceState<IssueCard[]>;
  onOpenIssue: (id: string) => void;
}) {
  const resource = resources.milestones(project);
  const state = useQuery(resource);
  // The issues store is the stream's: a tracker change refetches it, and
  // then the roll-up — which the stream does not carry — is re-read too.
  const seen = useRef(issues.asOf);
  useEffect(() => {
    if (seen.current === issues.asOf) return;
    seen.current = issues.asOf;
    void resource.invalidate();
  }, [issues.asOf, resource]);
  const rows = state.data ?? [];

  return (
    <main className="px-4 lg:px-8 pt-4 pb-9 min-w-0" aria-label="milestones">
      <div className="flex flex-wrap items-center gap-2 mb-3">
        <h1 className="text-section font-semibold text-ink-100">Milestones</h1>
        <StaleChip state={state} />
      </div>
      <ResourceGate
        state={state}
        loading="loading milestones…"
        failed="could not load milestones"
        onRetry={() => void resource.refresh()}
      />
      {state.status === "empty" && (
        <div className="card px-4 py-5 text-secondary text-ink-400">
          No milestones in {project}. List them in the project's <span className="num">PROJECT.md</span>, or set{" "}
          <span className="num">milestone=</span> on an issue.
        </div>
      )}
      <ul className="grid gap-2.5 lg:grid-cols-2">
        {rows.map((m) => {
          const health = healthView(m.health);
          const progress = progressView(m.progress);
          return (
            <li key={m.id} className="card px-3.5 py-3 min-w-0 space-y-2.5" data-milestone={m.id}>
              <div className="flex items-start gap-2 min-w-0">
                <div className="min-w-0 flex-1">
                  <div className="flex flex-wrap items-center gap-1.5">
                    <span className="num text-label text-info">{m.id}</span>
                    <HealthBadge view={health} />
                    {!m.configured && (
                      <span className="chip bg-ink-800 text-ink-500" title="named by an issue, not in PROJECT.md">
                        unlisted
                      </span>
                    )}
                  </div>
                  {m.title && <div className="text-cardtitle font-medium text-ink-100 mt-1 break-words">{m.title}</div>}
                  {m.exit && <p className="text-label text-ink-400 mt-0.5 break-words">Exit: {m.exit}</p>}
                </div>
              </div>
              <ProgressBar view={progress} tone={health.tone} />
              <HealthReasons view={health} />
              {m.epics.length > 0 && (
                <ul className="divide-y divide-ink-700 border-t border-ink-700" aria-label={`${m.id} epics`}>
                  {m.epics.map((e) => {
                    const eh = healthView({ state: e.health });
                    return (
                      <li key={e.id} className="pt-1.5 pb-1 flex items-center gap-2 min-w-0">
                        <button className="lnk num text-label shrink-0" onClick={() => onOpenIssue(e.id)}>
                          {e.id}
                        </button>
                        <span className="text-secondary text-ink-200 min-w-0 flex-1 truncate">{e.title}</span>
                        {e.stage && <span className="chip bg-accent/10 text-accent shrink-0">{e.stage}</span>}
                        <HealthBadge view={eh} />
                        <span className="num text-micro text-ink-500 shrink-0 w-9 text-right">
                          {typeof e.progress === "number" ? `${Math.round(e.progress * 100)}%` : "—"}
                        </span>
                      </li>
                    );
                  })}
                </ul>
              )}
              {m.issues.length > 0 && (
                <p className="text-micro text-ink-500">
                  {m.issues.length} {m.issues.length === 1 ? "issue" : "issues"} outside epics
                </p>
              )}
            </li>
          );
        })}
      </ul>
    </main>
  );
}
