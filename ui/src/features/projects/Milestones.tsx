import { useState } from "react";
import type { ResourceState } from "../../lib/cache";
import type { IssueCard, MilestoneRow } from "../../lib/types";
import { ResourceGate, StaleChip } from "../../ui/ResourceStatus";
import { HealthBadge, HealthReasons, ProgressBar } from "./HealthBadge";
import { checkpointDate, checkpointLabel, checkpointWork, scheduleLabel } from "./roadmap";
import { useMilestones } from "./useMilestones";
import { healthView, progressView } from "./work";

export default function Milestones({ project, issues, onOpenIssue }: {
  project: string; issues: ResourceState<IssueCard[]>; onOpenIssue: (id: string) => void;
}) {
  const { state, retry } = useMilestones(project, issues.asOf);
  const [showClosed, setShowClosed] = useState(false);
  const rows = state.data ?? [];
  const closed = rows.filter((m) => m.status === "achieved" || m.status === "cancelled");
  const visible = rows.filter((m) => showClosed || !closed.includes(m));
  const undefinedCount = rows.filter((m) => !m.configured).length;
  const errors = [...new Set(rows.map((m) => m.config_error).filter(Boolean))];
  return <main className="px-4 lg:px-8 pt-5 pb-9 min-w-0" aria-label="milestones">
    <div className="flex flex-wrap items-center gap-3 mb-2">
      <h1 className="text-section font-semibold text-ink-100">Milestones</h1><StaleChip state={state} />
      {closed.length > 0 && <button className="lnk text-label ml-auto" aria-pressed={showClosed} onClick={() => setShowClosed(!showClosed)}>{showClosed ? "Hide" : "Show"} achieved & cancelled ({closed.length})</button>}
    </div>
    <p className="text-label text-ink-500 mb-5">Delivery checkpoints: what needs to be true, who owns it, and when it’s due.</p>
    <ResourceGate state={state} loading="Loading roadmap…" failed="Could not load milestones" onRetry={retry} />
    {errors.map((error) => <p key={error} className="card p-3 mb-3 text-label text-warn" role="note">{error}</p>)}
    {undefinedCount > 0 && <p className="roadmap-notice" role="note">{undefinedCount} {undefinedCount === 1 ? "checkpoint needs a definition" : "checkpoints need definitions"}. Tasks are linked, but the goal, owner, dates, and completion criteria haven’t been set.</p>}
    {state.data && visible.length === 0 && <p className="card px-4 py-5 text-secondary text-ink-400">{rows.length ? "No upcoming or active milestones." : "No delivery checkpoints yet."}</p>}
    <ol className="milestone-roadmap" aria-label="Delivery roadmap">
      {visible.map((milestone) => <MilestoneCheckpoint key={milestone.id} milestone={milestone} rows={rows} onOpenIssue={onOpenIssue} />)}
    </ol>
  </main>;
}

export function MilestoneCheckpoint({ milestone: m, rows, onOpenIssue }: {
  milestone: MilestoneRow; rows: MilestoneRow[]; onOpenIssue: (id: string) => void;
}) {
  const health = healthView(m.health);
  const schedule = scheduleLabel(m);
  const tasks = m.tasks ?? m.issues;
  const pending = (m.depends_on ?? []).filter((id) => rows.find((row) => row.id === id)?.status !== "achieved");
  const finished = m.status === "achieved" || m.status === "cancelled";
  return <li className="milestone-checkpoint" data-milestone={m.id} data-checkpoint-status={m.status ?? "undefined"}>
    <div className="checkpoint-heading">
      <div className="min-w-0">
        <div className="flex flex-wrap items-center gap-2 mb-2"><span className="num text-micro text-ink-500">{m.id}</span><span className={`chip ${m.status === "active" ? "bg-accent/10 text-accent" : m.status === "achieved" ? "bg-ok/10 text-ok" : "bg-ink-800 text-ink-400"}`}>{checkpointLabel(m)}</span>{schedule && <span className="text-micro text-warn">{schedule}</span>}</div>
        <h2 className="text-cardtitle font-medium text-ink-100 break-words">{m.title ?? `Checkpoint ${m.id}`}</h2>
        {m.description && <p className="text-label text-ink-400 mt-1.5 max-w-[75ch] break-words">{m.description}</p>}
      </div>
      <dl className="checkpoint-metadata">
        <div><dt>Owner</dt><dd>{m.owner ?? "Unassigned"}</dd></div>
        {checkpointDate(m.start_date) && <div><dt>Start</dt><dd><time dateTime={m.start_date!}>{checkpointDate(m.start_date)}</time></dd></div>}
        <div><dt>Target</dt><dd>{checkpointDate(m.target_date) ? <time dateTime={m.target_date!}>{checkpointDate(m.target_date)}</time> : "Not scheduled"}</dd></div>
        {checkpointDate(m.completed_date) && <div><dt>Achieved</dt><dd><time dateTime={m.completed_date!}>{checkpointDate(m.completed_date)}</time></dd></div>}
      </dl>
    </div>
    {m.exit && <div className="checkpoint-criteria"><span className="slabel">Completion criteria</span><p className="text-secondary text-ink-200 mt-1 break-words max-w-[85ch]">{m.exit}</p></div>}
    {pending.length > 0 && !finished && <p className="text-label text-warn mt-3">Depends on {pending.join(", ")} · {pending.length === 1 ? "checkpoint is" : "checkpoints are"} still open</p>}
    <div className="checkpoint-work"><span className="text-label text-ink-400">{checkpointWork(m)}</span>{!finished && health.state !== "on_track" && <HealthBadge view={health} />}{m.epics.length > 0 && <span className="text-micro text-ink-500">{m.epics.length} contributing {m.epics.length === 1 ? "epic" : "epics"}</span>}</div>
    <details className="checkpoint-details">
      <summary>Work & evidence</summary>
      <div className="checkpoint-detail-grid">
        <section className="min-w-0"><h3 className="slabel mb-3">Work progress · weighted by task size</h3><ProgressBar view={progressView(m.progress)} tone={health.tone} /><p className="text-micro text-ink-500 mt-2 mb-3">Task completion supports the checkpoint. Achievement is recorded separately.</p><HealthReasons view={health} />
          {m.epics.length > 0 && <div className="mt-4"><h3 className="slabel mb-2">Contributing epics</h3><ul className="checkpoint-links">{m.epics.map((e) => <li key={e.id}><button onClick={() => onOpenIssue(e.id)}><span className="num text-micro text-accent">{e.id}</span><span>{e.title}</span>{e.task_count !== undefined && <small>{e.task_count} tasks</small>}</button></li>)}</ul></div>}
          {tasks.length > 0 && <div className="mt-4"><h3 className="slabel mb-2">{m.tasks ? "Scoped tasks" : "Issues outside epics"}</h3><ul className="checkpoint-links">{tasks.map((task) => <li key={task.id}><button onClick={() => onOpenIssue(task.id)}><span className="num text-micro text-accent">{task.id}</span><span>{task.title}</span><small className={task.blocked ? "text-warn" : ""}>{task.blocked ? "Blocked" : task.status}</small></button></li>)}</ul></div>}
        </section>
        <section className="min-w-0"><h3 className="slabel mb-2">Completion evidence</h3>{m.evidence?.length ? <ul className="space-y-2 text-label text-ink-300 break-words">{m.evidence.map((entry, i) => <li key={i}>{/^https?:\/\//i.test(entry) ? <a className="lnk break-all" href={entry} target="_blank" rel="noopener noreferrer">{entry} ↗</a> : entry}</li>)}</ul> : <p className="text-label text-ink-500">No evidence recorded.</p>}
          {!!m.depends_on?.length && <div className="mt-5"><h3 className="slabel mb-2">Dependencies</h3><ul className="space-y-2 text-label text-ink-400">{m.depends_on.map((id) => { const row = rows.find((r) => r.id === id); return <li key={id}><span className="num">{id}</span>{row?.title ? ` · ${row.title}` : ""} · {row ? checkpointLabel(row) : "Unknown checkpoint"}</li>; })}</ul></div>}
        </section>
      </div>
    </details>
  </li>;
}
