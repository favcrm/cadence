import Link from "../../ui/Link";
import { ResourceGate, StaleChip } from "../../ui/ResourceStatus";
import { checkpointDate, checkpointLabel, currentCheckpoints, scheduleLabel } from "./roadmap";
import { useMilestones } from "./useMilestones";

export default function ProjectMilestoneSummary({ project, issuesAsOf }: { project: string; issuesAsOf: number | null }) {
  const { state, retry } = useMilestones(project, issuesAsOf);
  const rows = state.data ?? [];
  const current = currentCheckpoints(rows);
  const needsDefinition = rows.filter((row) => !row.configured).length;
  return <section className="project-summary-card project-checkpoint-summary" aria-label="Delivery checkpoints">
    <div className="flex flex-wrap items-center gap-2 mb-3"><h2 className="text-section text-ink-100 font-semibold">Delivery checkpoints</h2><StaleChip state={state} /><Link className="lnk text-label ml-auto" href={`/projects/${encodeURIComponent(project)}/milestones`}>View roadmap ↗</Link></div>
    <ResourceGate state={state} loading="Reading milestones…" failed="Milestones could not be loaded" onRetry={retry} />
    {current.map((row) => <div key={row.id} className="flex flex-wrap gap-x-6 gap-y-2 py-2"><div className="min-w-0 flex-1"><span className="text-micro text-accent">{checkpointLabel(row)}</span><p className="text-secondary text-ink-200 mt-1 break-words">{row.title ?? row.id}</p>{row.exit && <p className="text-label text-ink-500 mt-1 break-words">{row.exit}</p>}</div><div className="text-label text-ink-400">{row.owner ?? "Unassigned"}<p className="mt-1">{checkpointDate(row.target_date) ? `Target ${checkpointDate(row.target_date)}` : "Not scheduled"}</p>{scheduleLabel(row) && <p className="text-warn mt-1">{scheduleLabel(row)}</p>}</div></div>)}
    {state.data && !current.length && <p className="text-label text-ink-500">{needsDefinition ? `${needsDefinition} checkpoints are referenced by work and need definitions.` : "No active or planned checkpoints."}</p>}
  </section>;
}
