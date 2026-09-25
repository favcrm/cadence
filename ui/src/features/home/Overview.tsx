import { useWriteBlock } from "../auth/WriteGate";
import { exclusionLabel, issueCounts, statusBreakdown } from "../../lib/counts";
import type { ResourceState } from "../../lib/cache";
import type {
  IssueCard,
  MainCi,
  MonitorAlert,
  Monitoring,
  Overview,
  Project,
  ProjectContext,
} from "../../lib/types";
import { needSections } from "./needSections";
import { needLabel, shaCiLabel } from "../../lib/uxCopy";
import { StaleChip } from "../../ui/ResourceStatus";

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
  ci_unverified: "bg-warn/10 text-warn",
  silent_end: "bg-warn/10 text-warn",
  inbox_unread: "bg-warn/10 text-warn",
  inbox_stale: "bg-warn/10 text-warn",
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

function projectMatches(value: string | null | undefined, project: string): boolean {
  return project === "all" || value === project;
}

function isGlobalProject(value: string | null | undefined): boolean {
  return !value || value === "global" || value === "unknown";
}

/** Sections come from the server-resolved `audience` (CAD-253); with
 *  `decision`, "Needs your decision" always shows, empty or not. */
function NeedRows({ rows, decision }: { rows: Overview["needs_me"]; decision: boolean }) {
  const grouped = needSections(rows, decision);
  return (
    <div className="space-y-3">
      {grouped.map((group) => (
        <details key={group.key} open={group.key === "operator" || group.key === "dependency"}>
          <summary className="flex items-center gap-2 mb-1.5 cursor-pointer list-none">
            <span className="slabel">{group.label}</span>
            <span className="num text-micro text-ink-500">{group.rows.length}</span>
          </summary>
          <div className="space-y-1.5 mt-1.5">
            {group.rows.length === 0 && group.empty && (
              <div className="card px-3.5 py-2.5 text-label text-ink-500">{group.empty}</div>
            )}
            {group.rows.map((n, i) => (
              <div
                key={`${n.kind}-${n.title}-${i}`}
                className="card px-3.5 py-2.5 flex flex-wrap items-center gap-x-3 gap-y-1.5"
              >
                <span className={`chip ${KIND_CHIP[n.kind] ?? "bg-ink-800 text-ink-400"}`}>
                  {needLabel(n.kind)}
                </span>
                {/* One row per subject: the further causes ride along. */}
                {(n.causes ?? []).slice(1).map((c) => (
                  <span
                    key={c.cause}
                    className={`chip ${KIND_CHIP[c.cause] ?? "bg-ink-800 text-ink-400"}`}
                    title={c.title}
                  >
                    + {needLabel(c.cause)}
                  </span>
                ))}
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
                {n.audience === "operator" && n.audience_reason && (
                  <span className="text-micro text-fail shrink-0">{n.audience_reason}</span>
                )}
                <span className="chip bg-ink-800 text-ink-500 shrink-0">
                  {n.project || "global"}
                </span>
                <details className="basis-full sm:basis-auto sm:ml-auto min-w-0">
                  <summary className="cursor-pointer text-micro text-accent list-none hover:underline">
                    details
                  </summary>
                  <div className="mt-1.5 rounded border border-ink-700 bg-ink-900 px-2.5 py-2 text-micro text-ink-400">
                    <div className="text-ink-300">kind {n.kind} · observed {age(n.age)} ago · source {n.project || "global host"}</div>
                    {n.audience_reason && <div className="text-ink-300">for {n.audience} · {n.audience_reason}</div>}
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
  const block = useWriteBlock(readOnly);
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
            title={block ?? "acknowledge this durable monitor alert"}
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

/** A covered cancelled/missing SHA stays neutral — never the pass colour. */
function shaCiChip(state: string, covered: boolean): string {
  switch (state) {
    case "passed":
      return "bg-ok/15 text-ok";
    case "failed":
      return "bg-fail/10 text-fail";
    case "pending":
      return "bg-info/10 text-info";
    default:
      return covered ? "bg-ink-800 text-ink-400" : "bg-warn/10 text-warn";
  }
}

/** SHAs shown per repo; the rest are counted. */
const MAIN_CI_SHOWN = 8;

/// Default-branch CI per repo, newest SHA first — each SHA judged only
/// by its own `ci.yml` push run (CAD-267).
function MainCiView({ blocks }: { blocks: MainCi[] }) {
  return (
    <section>
      <div className="slabel mb-2">default-branch ci</div>
      <div className="card divide-y divide-ink-700/60">
        {blocks.map((b) => {
          const shas = b.shas ?? [];
          return (
            <div key={b.slug} className="px-4 py-3 space-y-1.5">
              <div className="flex flex-wrap items-baseline gap-x-2 text-micro">
                <span className="num text-label text-ink-200">{b.slug}</span>
                {b.branch && <span className="text-ink-400">{b.branch}</span>}
                <span className="text-ink-600">ci · {b.workflow ?? "ci.yml"} push runs</span>
                {b.order === "runs" && (
                  <span className="text-ink-500" title={b.log_error ?? undefined}>
                    ordered by run time — no local first-parent log
                  </span>
                )}
              </div>
              {b.error ? (
                <div className="text-label text-warn">
                  cannot read ci runs — {b.error}
                </div>
              ) : shas.length === 0 ? (
                <div className="text-label text-ink-500">no default-branch SHAs to classify</div>
              ) : (
                <ul className="space-y-0.5">
                  {shas.slice(0, MAIN_CI_SHOWN).map((s) => (
                    <li key={s.sha} className="flex flex-wrap items-center gap-x-2 text-micro">
                      <code className="num text-ink-400 w-16 shrink-0">{s.sha.slice(0, 7)}</code>
                      <span className={`chip !py-[.15rem] ${shaCiChip(s.state, !!s.covered_by)}`}>
                        {s.run_url ? (
                          <a href={s.run_url} target="_blank" rel="noreferrer" className="hover:underline">
                            {shaCiLabel(s)}
                          </a>
                        ) : (
                          shaCiLabel(s)
                        )}
                      </span>
                    </li>
                  ))}
                  {shas.length > MAIN_CI_SHOWN && (
                    <li className="text-micro text-ink-600">
                      {shas.length - MAIN_CI_SHOWN} older SHA{shas.length - MAIN_CI_SHOWN === 1 ? "" : "s"} not shown
                    </li>
                  )}
                </ul>
              )}
            </div>
          );
        })}
      </div>
    </section>
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
/// Context section renders in full. Only for a selected project.
function ProjectScope({
  project,
  projects,
  context,
  contextLoading,
  onOpenContext,
}: {
  project: string;
  projects: Project[];
  context: ProjectContext | null;
  contextLoading: boolean;
  onOpenContext: () => void;
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
            {docs.length - shown.length} more documents on Context
          </p>
        )}
        <button
          onClick={onOpenContext}
          className="lnk text-label"
          title="full context — manifest, excerpts and verified memory"
        >
          open context →
        </button>
      </div>
    </section>
  );
}

export default function OverviewView({
  state,
  issues,
  onRetry,
  project,
  projects,
  context,
  contextLoading,
  readOnly,
  onAck,
  onOpenContext,
}: {
  state: ResourceState<Overview>;
  /** The cards — project summary counts come from counts.ts, like the sidebar. */
  issues: ResourceState<IssueCard[]>;
  onRetry: () => void;
  project: string;
  projects: Project[];
  context: ProjectContext | null;
  contextLoading: boolean;
  readOnly: boolean;
  onAck: (monitor: string, seq: number) => void;
  onOpenContext: () => void;
}) {
  const data = state.data;
  if (!data) {
    return (
      <div className="px-4 lg:px-8 pt-4 pb-10 space-y-5 max-w-[68rem]">
        {/* "unavailable" only once a request actually failed with
            nothing ever loaded; before the first answer — or while a
            retry is in flight — this is a load, not a failure. */}
        {state.status === "failed" ? (
          <div className="text-label text-fail flex flex-wrap items-center gap-3" role="alert">
            <span>
              overview unavailable — the board server could not build the view
              {state.error ? ` (${state.error})` : ""}
            </span>
            <button
              onClick={onRetry}
              className="chip bg-fail/10 text-fail hover:bg-fail/20 transition-colors"
            >
              retry
            </button>
          </div>
        ) : (
          <div className="text-label text-ink-500" role="status">
            building the overview — daemon and GitHub probes can take several seconds
          </div>
        )}
        {project !== "all" && (
          <ProjectScope
            project={project}
            projects={projects}
            context={context}
            contextLoading={contextLoading}
            onOpenContext={onOpenContext}
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
  const scopedCi = (data.main_ci ?? []).filter((b) => projectMatches(b.project, project));
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
        <StaleChip state={state} />
      </header>
      {(data.github.state === "unavailable" ||
        !data.daemon.reachable ||
        (data.degraded ?? []).length > 0) && (
        <div className="card border-warn/40 px-4 py-3 text-secondary text-warn">
          {data.github.state === "unavailable" && (
            <div>github unavailable — PR and CI rows are missing</div>
          )}
          {!data.daemon.reachable && (
            <div>daemon unreachable — agent, approval and drift rows are missing</div>
          )}
          {(data.degraded ?? []).map((d, i) => (
            <div key={i}>
              degraded · {d.source}
              {d.subject ? ` ${d.subject}` : ""} — {d.detail}
            </div>
          ))}
        </div>
      )}

      {project !== "all" && (
        <ProjectScope
          project={project}
          projects={projects}
          context={context}
          contextLoading={contextLoading}
          onOpenContext={onOpenContext}
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
        <NeedRows rows={scopedNeeds} decision />
        {project !== "all" && globalNeeds.length > 0 && (
          <details className="mt-4">
            <summary className="cursor-pointer text-micro text-ink-400 hover:text-ink-200">
              Global or unassigned observations · {globalNeeds.length}
            </summary>
            <div className="mt-2"><NeedRows rows={globalNeeds} decision={false} /></div>
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

      {scopedCi.length > 0 && <MainCiView blocks={scopedCi} />}

      {scopedProjects.length > 0 && (
        <section>
          <div className="slabel mb-2">project summary</div>
          <div className="card divide-y divide-ink-700/60">
            {scopedProjects.map((p) => {
              // The same derivation as the sidebar and board header
              // (counts.ts over /api/issues), not the payload's
              // `open_by_status`, so the three numbers agree.
              const c = issues.data ? issueCounts(issues.data, p.key) : null;
              const excluded = c ? exclusionLabel(c) : "";
              return (
                <div key={p.key} className="px-4 py-2.5">
                <div className="flex items-baseline gap-3">
                  <span className="text-label font-semibold text-ink-100 w-28 shrink-0 truncate">
                    {p.key}
                  </span>
                  <span className="num text-label text-ink-400 min-w-0 flex-1">
                    {c === null
                      ? issues.status === "failed"
                        ? "issue counts unavailable"
                        : "counting…"
                      : `${c.open} open${c.open ? ` — ${statusBreakdown(c)}` : ""}`}
                    {excluded && (
                      <span className="text-micro text-ink-500"> · {excluded}</span>
                    )}
                  </span>
                  {p.oldest_review_age != null && (
                    <span className="num text-micro text-warn shrink-0">
                      oldest review {age(p.oldest_review_age)}
                    </span>
                  )}
                </div>
                {/* CAD-383: who holds each in-flight issue, and since when. */}
                {(p.claims ?? []).length > 0 && (
                  <ul className="mt-1 ml-[7.75rem] space-y-0.5">
                    {(p.claims ?? []).map((cl) => (
                      <li key={cl.issue} className="text-micro text-ink-400 truncate">
                        <span className="text-ink-200">{cl.issue}</span> {cl.status} ·{" "}
                        {cl.by ?? "?"}
                        {cl.owner && cl.owner !== cl.by ? ` (owner ${cl.owner})` : ""} ·{" "}
                        {cl.age_secs != null ? `claimed ${age(cl.age_secs)} ago` : "claim age unknown"}
                      </li>
                    ))}
                  </ul>
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
