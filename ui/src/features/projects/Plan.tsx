import type { ResourceState } from "../../lib/cache";
import { useTranslate } from "../../lib/locale";
import type { IssueCard } from "../../lib/types";
import Epics from "./Epics";
import Milestones from "./Milestones";
import type { Viewer } from "./work";

export default function Plan({
  project,
  issues,
  viewer,
  milestonesEnabled = true,
  onOpenIssue,
  onRetry,
}: {
  project: string;
  issues: ResourceState<IssueCard[]>;
  viewer: Viewer;
  milestonesEnabled?: boolean;
  onOpenIssue: (id: string) => void;
  onRetry: () => void;
}) {
  const t = useTranslate();
  return (
    <main className="px-4 lg:px-8 pt-5 pb-9 min-w-0" aria-label={t("Plan")}>
      <div className="mb-4">
        <h1 className="text-section font-semibold text-ink-100 m-0">{t("Plan")}</h1>
        <p className="text-label text-ink-500 mt-1 mb-0">
          {t("Time-bound milestones and stage-gated initiatives in one roadmap.")}
        </p>
      </div>
      <div className="plan-legend">
        {milestonesEnabled && (
          <span>
            <strong>{t("Milestones (When):")}</strong>{" "}
            {t("Target dates, completion criteria, and evidence.")}
          </span>
        )}
        <span>
          <strong>{t("Initiatives (What):")}</strong>{" "}
          {t("Multi-task workstreams advancing through stage gates.")}
        </span>
      </div>
      {milestonesEnabled && (
        <Milestones
          embedded
          project={project}
          issues={issues}
          onOpenIssue={onOpenIssue}
        />
      )}
      <Epics
        embedded
        project={project}
        issues={issues}
        viewer={viewer}
        onOpenIssue={onOpenIssue}
        onRetry={onRetry}
      />
    </main>
  );
}
