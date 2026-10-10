import Link from "../../ui/Link";
import { ResourceGate, StaleChip } from "../../ui/ResourceStatus";
import { checkpointDate, checkpointLabel, currentCheckpoints, scheduleLabel } from "./roadmap";
import { useMilestones } from "./useMilestones";
import { useLocale } from "../../lib/locale";

export default function ProjectMilestoneSummary({ project, issuesAsOf }: { project: string; issuesAsOf: number | null }) {
  const { state, retry } = useMilestones(project, issuesAsOf);
  const { locale, t, formatNumber } = useLocale();
  const rows = state.data ?? [];
  const current = currentCheckpoints(rows);
  const needsDefinition = rows.filter((row) => !row.configured).length;
  return <section className="project-summary-card project-checkpoint-summary" aria-label={t("Delivery checkpoints")}>
    <div className="flex flex-wrap items-center gap-2 mb-3"><h2 className="text-section text-ink-100 font-semibold">{t("Delivery checkpoints")}</h2><StaleChip state={state} /><Link className="lnk text-label ml-auto" href={`/projects/${encodeURIComponent(project)}/milestones`}>{t("View roadmap")} ↗</Link></div>
    <ResourceGate state={state} loading={t("Reading milestones…")} failed={t("Milestones could not be loaded")} onRetry={retry} />
    {current.map((row) => {
      const date = checkpointDate(row.target_date, locale);
      const schedule = scheduleLabel(row, t, formatNumber);
      return <div key={row.id} className="flex flex-wrap gap-x-6 gap-y-2 py-2"><div className="min-w-0 flex-1"><span className="text-micro text-accent">{checkpointLabel(row, t)}</span><p className="text-secondary text-ink-200 mt-1 break-words">{row.title ?? row.id}</p>{row.exit && <p className="text-label text-ink-500 mt-1 break-words">{row.exit}</p>}</div><div className="text-label text-ink-400">{row.owner ?? t("Unassigned")}<p className="mt-1">{date ? `${t("Target")} ${date}` : t("Not scheduled")}</p>{schedule && <p className="text-warn mt-1">{schedule}</p>}</div></div>;
    })}
    {state.data && !current.length && <p className="text-label text-ink-500">{needsDefinition ? t("{count} checkpoints are referenced by work and need definitions.").replace("{count}", formatNumber(needsDefinition)) : t("No active or planned checkpoints.")}</p>}
  </section>;
}
