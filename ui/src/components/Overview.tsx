import type {
  MonitorAlert,
  Monitoring,
  Overview,
  Project,
  ProjectContext,
} from "../types";
import { needGroupKey, needLabel } from "../uxCopy";

const KIND_CHIP: Record<string, string> = {
  merge: "bg-ok/15 text-ok",
  intake: "bg-info/10 text-info",
  approval: "bg-warn/10 text-warn",
  fenced: "bg-fail/10 text-fail",
  stalled: "bg-warn/10 text-warn",
  drift: "bg-accent/10 text-accent",
  pr_no_verdict: "bg-info/10 text-info",
  review_no_pr: "bg-ink-800 text-ink-300",
  blocked_ready: "bg-accent/10 text-accent",
  ci_red: "bg-fail/10 text-fail",
  silent_end: "bg-warn/10 text-warn",
  inbox_unread: "bg-warn/10 text-warn",
  tracker_behind: "bg-ink-800 text-ink-400",
};

function age(secs: number): string {
  const s = Math.max(0, Math.floor(secs));
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${Math.floor(s / 60)}m`;
  if (s < 86400) return `${Math.floor(s / 3600)}h`;
  return `${Math.floor(s / 86400)}d`;
}

function when(epoch: number | null | undefined): string {
  if (epoch == null) return "never";
  return new Date(epoch * 1000).toLocaleString();
}

const MONITOR_STATE_CHIP: Record<string, string> = {
  active: "bg-ok/15 text-ok",
  degraded: "bg-fail/10 text-fail",
  stale: "bg-warn/10 text-warn",
  stopped: "bg-ink-800 text-ink-400",
  unavailable: "bg-warn/10 text-warn",
  off: "bg-ink-800 text-ink-400",
};

const NEED_GROUPS = [
  { key: "decision", label: "Needs your decision" },
  { key: "team", label: "Team handling" },
  { key: "dependency", label: "Waiting on dependency" },
  { key: "info", label: "Information" },
] as const;

function needGroup(kind: string): (typeof NEED_GROUPS)[number] {
  const key = needGroupKey(kind);
  return NEED_GROUPS.find((group) => group.key === key) ?? NEED_GROUPS[1];
}

function projectMatches(value: string | null | undefined, project: string): boolean {
  return project === "all" || value === project;
}

function isGlobalProject(value: string | null | undefined): boolean {
  return !value || value === "global" || value === "unknown";
}

function NeedRows({ rows }: { rows: Overview["needs_me"] }) {
  const grouped = NEED_GROUPS.map((group) => ({
    ...group,
    rows: rows.filter((row) => needGroup(row.kind).key === group.key),
  })).filter((group) => group.rows.length > 0);
  return (
    <div className="space-y-3">
      {grouped.map((group) => (
        <details key={group.key} open={group.key === "decision" || group.key === "dependency"}>
          <summary className="flex items-center gap-2 mb-1.5 cursor-pointer list-none">
            <span className="slabel">{group.label}</span>
            <span className="num text-micro text-ink-500">{group.rows.length}</span>
          </summary>
          <div className="space-y-1.5 mt-1.5">
            {group.rows.map((n, i) => (
              <div
                key={`${n.kind}-${n.title}-${i}`}
                className="card px-3.5 py-2.5 flex flex-wrap items-center gap-x-3 gap-y-1.5"
              >
                <span className={`chip ${KIND_CHIP[n.kind] ?? "bg-ink-800 text-ink-400"}`}>
                  {needLabel(n.kind)}
                </span>
                <span className="num text-label text-ink-500 w-9 shrink-0">
                  {age(n.age)}
                </span>
                <span className="min-w-0 flex-1 basis-[12rem] text-label text-ink-200">
                  {n.link ? (
                    <a href={n.link} target="_blank" rel="noreferrer" className="hover:text-accent">
                      {n.title}
                    </a>
                  ) : (
                    n.title
                  )}
                </span>
                <span className="chip bg-ink-800 text-ink-500 shrink-0">
                  {n.project || "global"}
                </span>
                <details className="basis-full sm:basis-auto sm:ml-auto min-w-0">
                  <summary className="cursor-pointer text-micro text-accent list-none hover:underline">
                    details
                  </summary>
                  <div className="mt-1.5 rounded border border-ink-700 bg-ink-900 px-2.5 py-2 text-micro text-ink-400">
                    <div className="text-ink-300">kind {n.kind} · observed {age(n.age)} ago · source {n.project || "global host"}</div>
                    <code className="num block mt-1 whitespace-pre-wrap break-words">{n.command}</code>
                  </div>
                </details>
              </div>
            ))}
          </div>
        </details>
      ))}
    </div>
  );
}

function MonitoringView({
  data,
  project,
  readOnly,
  onAck,
}: {
  data: Monitoring;
  project: string;
  readOnly: boolean;
  onAck: (monitor: string, seq: number) => void;
}) {
  const monitors = data.monitors.filter((monitor) => projectMatches(monitor.project, project));
  const globalMonitors = data.monitors.filter((monitor) => isGlobalProject(monitor.project));
  const alerts = data.alerts.filter((alert) => projectMatches(alert.project, project));
  const globalAlerts = data.alerts.filter((alert) => isGlobalProject(alert.project));
  return (
    <section>
      <div className="slabel mb-2">coordinator</div>
      <div className="card px-4 py-3.5 space-y-3">
        <div className="flex flex-wrap items-center gap-2">
          <span
            className={`chip ${MONITOR_STATE_CHIP[data.state] ?? "bg-ink-800 text-ink-400"}`}
          >
            monitor {data.state}
          </span>
          <span className="text-label text-ink-400">
            last successful scan: {when(data.last_success_at)}
          </span>
          <span className="text-micro text-ink-500">
            next reconciliation: {when(data.next_check_at)}
          </span>
        </div>

        <div className="text-micro text-ink-500">
          local UI visibility only — external push delivery is unconfigured
        </div>

        {!data.available && (
          <div className="text-label text-warn">
            monitor RPC unavailable; persistent monitor health cannot be confirmed
          </div>
        )}
        {data.errors.map((e, i) => (
          <div key={`${e.monitor ?? "monitor"}-${i}`} className="text-label text-fail">
            {e.monitor ? `${e.monitor}: ` : ""}{e.error}
          </div>
        ))}

        {monitors.length > 0 && (
          <div className="space-y-1 border-t border-ink-700/60 pt-2">
            {monitors.map((monitor) => (
              <div
                key={monitor.id}
                className="flex flex-wrap items-center gap-x-2 gap-y-1 text-micro"
              >
                <span
                  className={`chip !py-[.15rem] ${MONITOR_STATE_CHIP[monitor.monitoring] ?? "bg-ink-800 text-ink-400"}`}
                >
                  {monitor.monitoring}
                </span>
                <span className="num text-ink-300">{monitor.id}</span>
                <span className="text-ink-500">owner {monitor.owner}</span>
                <span className="text-ink-600">
                  dispatch {monitor.auto_dispatch_enabled ? "automatic opt-in" : monitor.dispatch_enabled ? "manual" : "off"}
                </span>
                <span className="text-ink-500">
                  scan {when(monitor.last_success_at)} · last check {when(monitor.last_check_at)}
                </span>
                <span className="text-ink-600">
                  heartbeat {when(monitor.heartbeat_at)} · coverage {monitor.coverage.join(", ") || "none"}
                </span>
                {monitor.error && <span className="text-fail">{monitor.error}</span>}
              </div>
            ))}
          </div>
        )}

        {monitors.length === 0 && data.available && (
          <div className="text-label text-ink-500">
            {project === "all"
              ? "no monitor registration is active; autonomous reconciliation is stopped"
              : `no monitor registration is attached to ${project}`}
          </div>
        )}

        {alerts.length > 0 && (
          <div className="space-y-2 border-t border-ink-700/60 pt-3">
            <div className="flex items-center justify-between">
              <span className="slabel">actionable alerts</span>
              <span className="num text-micro text-ink-500">
                {alerts.filter((alert) => alert.state === "open").length} open · {alerts.length} recorded
              </span>
            </div>
            {alerts.map((alert) => (
              <MonitorAlertView
                key={`${alert.monitor}-${alert.seq}`}
                alert={alert}
                readOnly={readOnly}
                onAck={onAck}
              />
            ))}
          </div>
        )}

        {project !== "all" && globalAlerts.length > 0 && (
          <details className="border-t border-ink-700/60 pt-3">
            <summary className="cursor-pointer text-micro text-ink-400 hover:text-ink-200">
              Global or unassigned alerts · {globalAlerts.length}
            </summary>
            <div className="mt-2 space-y-2">
              {globalAlerts.map((alert) => (
                <MonitorAlertView key={`${alert.monitor}-${alert.seq}`} alert={alert} readOnly={readOnly} onAck={onAck} />
              ))}
            </div>
          </details>
        )}
        {project !== "all" && globalMonitors.length > 0 && (
          <details className="border-t border-ink-700/60 pt-3">
            <summary className="cursor-pointer text-micro text-ink-400 hover:text-ink-200">
              Global or unassigned monitors · {globalMonitors.length}
            </summary>
            <div className="mt-2 space-y-1 text-micro text-ink-500">
              {globalMonitors.map((monitor) => (
                <div key={monitor.id} className="flex flex-wrap gap-2">
                  <span className="num text-ink-300">{monitor.id}</span>
                  <span>{monitor.monitoring}</span>
                  <span>owner {monitor.owner}</span>
                  <span>coverage {monitor.coverage.join(", ") || "none"}</span>
                </div>
              ))}
            </div>
          </details>
        )}
      </div>
    </section>
  );
}

function MonitorAlertView({
  alert,
  readOnly,
  onAck,
}: {
  alert: MonitorAlert;
  readOnly: boolean;
  onAck: (monitor: string, seq: number) => void;
}) {
  const open = alert.state === "open";
  const stateLabel = alert.state === "resolved" ? "resolved" : open ? "open" : "acknowledged";
  return (
    <div className="rounded border border-ink-700/70 bg-ink-875 px-3 py-2.5 space-y-1.5">
      <div className="flex flex-wrap items-center gap-2">
        <span className={`chip !py-[.15rem] ${open ? "bg-warn/10 text-warn" : "bg-ink-800 text-ink-500"}`}>
          {stateLabel}
        </span>
        <span className="chip !py-[.15rem] bg-ink-800 text-ink-300">{alert.kind}</span>
        <span className="num text-micro text-ink-500">{age(alert.age_secs)}</span>
        <span className="num text-micro text-ink-500">
          {alert.project} · {alert.monitor} · {alert.task ?? "task unknown"}
        </span>
        {open && (
          <button
            className="chip ml-auto bg-accent/10 text-accent hover:bg-accent/20 disabled:opacity-50"
            disabled={readOnly}
            title={readOnly ? "board is read-only" : "acknowledge this durable monitor alert"}
            onClick={() => onAck(alert.monitor, alert.seq)}
          >
            ack
          </button>
        )}
      </div>
      <div className="text-label text-ink-200">{alert.next_action}</div>
      <div className="text-micro text-ink-500">
        next owner <span className="text-ink-300">{alert.next_owner}</span> · {alert.authority}
      </div>
      <div className="text-micro text-ink-600">
        evidence event {alert.event_seq} · {alert.fingerprint}
      </div>
    </div>
  );
}

const DOC_STATE_CHIP: Record<string, string> = {
  ready: "bg-ok/15 text-ok",
  current: "bg-ok/15 text-ok",
  stale: "bg-warn/10 text-warn",
  dirty: "bg-warn/10 text-warn",
  uncompared: "bg-warn/10 text-warn",
};

/// Compact reading of the project's declared scope plus its tracked
/// document manifest — the same `/api/projects/:key/context` payload the
/// Plan tab renders in full. Only for a selected project.
function ProjectScope({
  project,
  projects,
  context,
  contextLoading,
  onOpenPlan,
}: {
  project: string;
  projects: Project[];
  context: ProjectContext | null;
  contextLoading: boolean;
  onOpenPlan: () => void;
}) {
  const meta = projects.find((p) => p.key === project);
  const docs = context?.documents.filter((d) => d.selected) ?? [];
  const shown = docs.slice(0, 6);
  return (
    <section>
      <div className="slabel mb-2">project context</div>
      <div className="card px-4 py-3.5 space-y-3">
        {meta && (
          <div className="flex flex-wrap items-center gap-x-3 gap-y-1.5 text-micro">
            {meta.repos.map((repo, i) => (
              <span key={i} className="num text-ink-300">
                {repo.remote ?? repo.path}
              </span>
            ))}
            {meta.components.map((component) => (
              <span
                key={component}
                className="chip bg-ink-800 text-ink-400 !py-[.15rem]"
              >
                {component}
              </span>
            ))}
            {meta.default_owner && (
              <span className="text-ink-500">
                default owner {meta.default_owner}
              </span>
            )}
          </div>
        )}
        {contextLoading && !context && (
          <p className="text-label text-ink-500">
            reading the project's tracked manifest…
          </p>
        )}
        {docs.length > 0 && (
          <div className="divide-y divide-ink-700/60">
            {shown.map((doc) => (
              <div
                key={doc.id}
                className="py-1.5 flex flex-wrap items-baseline gap-x-2 gap-y-0.5"
              >
                <span className="text-label text-ink-200">{doc.title}</span>
                <span className="num text-micro text-ink-500">{doc.path}</span>
                {doc.required && <span className="kicker">required</span>}
                {!DOC_STATE_CHIP[doc.state] && (
                  <span className="chip bg-fail/10 text-fail !py-[.15rem]">
                    {doc.state}
                  </span>
                )}
              </div>
            ))}
          </div>
        )}
        {context && docs.length > shown.length && (
          <p className="text-micro text-ink-500">
            {docs.length - shown.length} more documents on Plan
          </p>
        )}
        <button
          onClick={onOpenPlan}
          className="lnk text-label"
          title="full context — manifest, excerpts and verified memory"
        >
          open plan →
        </button>
      </div>
    </section>
  );
}

export default function OverviewView({
  data,
  loading,
  project,
  projects,
  context,
  contextLoading,
  readOnly,
  onAck,
  onOpenPlan,
}: {
  data: Overview | null;
  loading: boolean;
  project: string;
  projects: Project[];
  context: ProjectContext | null;
  contextLoading: boolean;
  readOnly: boolean;
  onAck: (monitor: string, seq: number) => void;
  onOpenPlan: () => void;
}) {
  if (!data) {
    return (
      <div className="px-4 lg:px-8 pt-4 pb-10 space-y-5 max-w-[68rem]">
        <div className="text-label text-ink-500">
          {loading
            ? "building the overview — daemon and GitHub probes can take several seconds"
            : "overview unavailable — the board server could not build the view"}
        </div>
        {project !== "all" && (
          <ProjectScope
            project={project}
            projects={projects}
            context={context}
            contextLoading={contextLoading}
            onOpenPlan={onOpenPlan}
          />
        )}
      </div>
    );
  }
  const drift = data.drift;
  const scopedNeeds = data.needs_me.filter((need) => projectMatches(need.project, project));
  const globalNeeds = data.needs_me.filter((need) => isGlobalProject(need.project));
  const scopedProjects = data.projects.filter((item) => projectMatches(item.key, project));
  const otherProjectCount = project === "all" ? 0 : data.projects.filter((item) => item.key !== project).length;
  const scopedDrift = project === "all" || !drift.project || drift.project === project;
  return (
    <div className="px-4 lg:px-8 pt-4 pb-10 space-y-5 max-w-[68rem]">
      <header className="flex flex-wrap items-end gap-3">
        <div>
          <div className="slabel">workspace</div>
          <h1 className="text-section font-semibold text-ink-100 mt-1">
            {project === "all" ? "All projects" : project}
          </h1>
        </div>
        <span className="kicker">
          exact project ownership · unresolved work stays visible
        </span>
      </header>
      {(data.github.state === "unavailable" || !data.daemon.reachable) && (
        <div className="card border-warn/40 px-4 py-3 text-secondary text-warn">
          {data.github.state === "unavailable" && (
            <div>github unavailable — PR and CI rows are missing</div>
          )}
          {!data.daemon.reachable && (
            <div>daemon unreachable — agent, approval and drift rows are missing</div>
          )}
        </div>
      )}

      {project !== "all" && (
        <ProjectScope
          project={project}
          projects={projects}
          context={context}
          contextLoading={contextLoading}
          onOpenPlan={onOpenPlan}
        />
      )}

      {data.monitoring && (
        <MonitoringView
          data={data.monitoring}
          project={project}
          readOnly={readOnly}
          onAck={onAck}
        />
      )}

      <section>
        <div className="slabel mb-2">needs me · {project === "all" ? "all projects" : project}</div>
        {scopedNeeds.length === 0 ? (
          <div className="card px-4 py-6 text-center text-label text-ink-500">
            nothing waiting on a human for this project
          </div>
        ) : (
          <NeedRows rows={scopedNeeds} />
        )}
        {project !== "all" && globalNeeds.length > 0 && (
          <details className="mt-4">
            <summary className="cursor-pointer text-micro text-ink-400 hover:text-ink-200">
              Global or unassigned observations · {globalNeeds.length}
            </summary>
            <div className="mt-2"><NeedRows rows={globalNeeds} /></div>
          </details>
        )}
      </section>

      <section>
        <div className="slabel mb-2">deploy drift</div>
        <div className="card px-4 py-3.5">
          {scopedDrift && drift.known ? (
            drift.count === 0 ? (
              <div className="text-label text-ink-300">
                <span className="text-ok">up to date</span> — {drift.project} is
                running the latest on {drift.ref}
              </div>
            ) : (
              <div>
                <div className="text-label text-ink-200">
                  <span className="text-warn font-semibold">
                    {drift.count} commit{drift.count === 1 ? "" : "s"}
                  </span>{" "}
                  merged on {drift.ref} past build{" "}
                  <code className="num text-ink-400">
                    {drift.build_commit?.slice(0, 10)}
                  </code>{" "}
                  ({drift.project})
                </div>
                <ul className="mt-2 space-y-0.5">
                  {(drift.commits ?? []).map((c, i) => (
                    <li key={i} className="text-micro text-ink-400 truncate">
                      {c.subject}
                      {c.pr ? <span className="text-ink-500"> #{c.pr}</span> : null}
                    </li>
                  ))}
                </ul>
                {drift.held && (
                  <div className="text-micro text-ink-500 mt-2">{drift.held}</div>
                )}
              </div>
            )
          ) : scopedDrift ? (
            <div className="text-label text-ink-500">
              {drift.reason ?? "cannot tell"}
            </div>
          ) : (
            <div className="text-label text-ink-500">
              deployment snapshot belongs to <span className="text-ink-200">{drift.project ?? "global host"}</span>; no drift is attributed to {project}
            </div>
          )}
        </div>
      </section>

      {scopedProjects.length > 0 && (
        <section>
          <div className="slabel mb-2">project summary</div>
          <div className="card divide-y divide-ink-700/60">
            {scopedProjects.map((p) => {
              const counts = Object.entries(p.open_by_status)
                .sort(([a], [b]) => a.localeCompare(b))
                .map(([k, v]) => `${k}:${v}`)
                .join("  ");
              return (
                <div key={p.key} className="px-4 py-2.5 flex items-baseline gap-3">
                  <span className="text-label font-semibold text-ink-100 w-28 shrink-0 truncate">
                    {p.key}
                  </span>
                  <span className="num text-label text-ink-400 min-w-0 flex-1">
                    {counts || "no open issues"}
                  </span>
                  {p.oldest_review_age != null && (
                    <span className="num text-micro text-warn shrink-0">
                      oldest review {age(p.oldest_review_age)}
                    </span>
                  )}
                </div>
              );
            })}
          </div>
          {otherProjectCount > 0 && (
            <p className="text-micro text-ink-500 mt-2">
              {otherProjectCount} other project summaries remain in All projects.
            </p>
          )}
        </section>
      )}
    </div>
  );
}
