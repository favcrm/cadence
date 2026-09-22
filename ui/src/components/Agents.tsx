import { useEffect, useState } from "react";
import { api } from "../api";
import { fmtTime } from "../fmt";
import { provenanceDetail } from "../modelProvenance";
import { agentIsUnassigned, agentMatchesProject, issueProjectMap } from "../scope";
import { agentsEmptyCopy } from "../uxCopy";
import type {
  Agent,
  AgentDetail,
  AgentTask,
  AgentsPayload,
  IssueCard,
  UsageLimit,
} from "../types";

const STATE_CHIP: Record<string, string> = {
  busy: "bg-info/10 text-info",
  idle: "bg-ok/10 text-ok",
  stopped: "bg-ink-800 text-ink-400",
  attention: "bg-fail/10 text-fail",
  inbox: "bg-ink-800 text-ink-500",
  approval: "bg-warn/10 text-warn",
  queued: "bg-info/10 text-info",
  "quota blocked": "bg-fail/10 text-fail",
};

const STATE_DOT: Record<string, string> = {
  busy: "bg-info",
  idle: "bg-ok",
  stopped: "bg-ink-600",
  attention: "bg-fail",
  inbox: "bg-ink-700",
  approval: "bg-warn",
  queued: "bg-info",
  "quota blocked": "bg-fail",
};

/// Table order: fenced first, then working, idle, stopped, mailboxes.
function rank(a: Agent): number {
  if (a.fenced) return 0;
  if (a.inbox) return 4;
  if (a.running > 0 || a.queued > 0) return 1;
  if (a.state === "idle") return 2;
  return 3;
}

function stateLabel(a: Agent): string {
  if (a.fenced) return "attention";
  if (a.inbox) return "inbox";
  if (a.state === "blocked_by_quota" || a.quota?.state === "blocked") {
    return "quota blocked";
  }
  if (a.state === "waiting_input") return "approval";
  if (a.running > 0) return "busy";
  if (a.queued > 0) return "queued";
  return a.state;
}

/// Compact silence label for the badge — "45s", "34m", "1h 5m".
function fmtSilence(secs: number): string {
  if (secs < 60) return `${Math.floor(secs)}s`;
  if (secs < 3600) return `${Math.floor(secs / 60)}m`;
  return `${Math.floor(secs / 3600)}h ${Math.floor((secs % 3600) / 60)}m`;
}

/// A fence's recovery path — the daemon's own error text already names
/// `agent unfence` / `message reconcile`; render it verbatim as code.
function RecoveryBlock({ text }: { text: string }) {
  return (
    <pre className="mt-1.5 whitespace-pre-wrap rounded border border-fail/30 bg-fail/5 px-2.5 py-2 text-micro text-fail/90 font-mono">
      {text}
    </pre>
  );
}

const TERMINAL_TASK_STATES = new Set(["verified", "done", "cancelled", "failed"]);

function activeTasks(a: Agent): AgentTask[] {
  return (a.tasks ?? []).filter((task) => !TERMINAL_TASK_STATES.has(task.task_state));
}

function profileModel(a: Agent): { value: string; note: string; mismatch?: string; provenance?: string } {
  const reported = a.model_reported ?? a.model ?? null;
  const configured = a.model_configured ?? null;
  const provenanceText = provenanceDetail(a.model_selection, a.model_lookup_role);
  if (a.model_selection === null && (a.provider === "devin" || a.provider === "inbox")) {
    return {
      value: reported ?? "unsupported",
      note: "model selection unsupported",
      provenance: provenanceText || undefined,
    };
  }
  if (reported) {
    return {
      value: reported,
      note: "reported",
      mismatch:
        configured && configured !== reported ? `configured ${configured}` : undefined,
      provenance: provenanceText || undefined,
    };
  }
  if (configured) {
    return {
      value: configured,
      note: "configured",
      provenance: provenanceText || undefined,
    };
  }
  if (a.model_source === "provider default") {
    return {
      value: "provider default",
      note: "reported model unknown",
      provenance: provenanceText || undefined,
    };
  }
  return {
    value: "unknown",
    note: "no provider evidence",
    provenance: provenanceText || undefined,
  };
}

function profileEffort(a: Agent): { value: string; note: string } {
  if (a.effort_applicable === false || a.effort_source === "not_applicable") {
    return { value: "n/a", note: "provider does not report effort" };
  }
  const effective = a.effort_reported ?? null;
  if (effective) return { value: effective, note: "confirmed" };
  if (a.effort) return { value: a.effort, note: "configured" };
  return { value: "unknown", note: "no provider evidence" };
}

function usageFor(a: Agent): UsageLimit | null {
  return a.quota ?? a.usage_limit ?? null;
}

function usageText(a: Agent): { value: string; note: string; tone: string } {
  const quota = usageFor(a);
  if (!quota) {
    return {
      value: "unavailable",
      note: "provider quota telemetry is not integrated",
      tone: "text-ink-500",
    };
  }
  const state = quota.state ?? "unknown";
  if (state === "error") {
    return { value: "error", note: quota.message ?? quota.reason ?? "quota query failed", tone: "text-fail" };
  }
  if (state === "stale") {
    return {
      value: "stale",
      note: [
        quota.observed_at ? `last update ${fmtTime(quota.observed_at)}` : "last update unknown",
        quota.window,
        quota.source ? `source ${quota.source}` : null,
      ]
        .filter(Boolean)
        .join(" · "),
      tone: "text-warn",
    };
  }
  if (state === "blocked") {
    return { value: "blocked", note: quota.reason ?? "provider reported a quota block", tone: "text-fail" };
  }
  if (state !== "available") {
    return { value: "unknown", note: quota.reason ?? "allowance not reported", tone: "text-ink-500" };
  }
  // Null means unavailable; zero is a real value and must remain visible.
  const unit = quota.unit ? ` ${quota.unit}` : "";
  const remaining = quota.remaining != null ? `${quota.remaining}${unit} remaining` : null;
  const used = quota.used != null ? `${quota.used}${unit} used` : null;
  const limit = quota.limit != null ? `${quota.limit}${unit} limit` : null;
  const usedAgainstLimit =
    quota.used != null && quota.limit != null
      ? `${quota.used}/${quota.limit}${unit} used`
      : null;
  const percent = quota.used_percent != null ? `${quota.used_percent}% used` : null;
  const value = remaining ?? usedAgainstLimit ?? used ?? percent ?? limit ?? "available";
  const window = quota.window ? `${quota.window}` : null;
  const reset = quota.reset_at ? `reset ${fmtTime(quota.reset_at)}` : null;
  const pool = quota.pool
    ? `shared${quota.pool.label ? ` · ${quota.pool.label}` : ""}${
        quota.pool.count != null ? ` · ${quota.pool.count} agents` : ""
      }`
    : null;
  const poolAgents = quota.pool?.agents?.length
    ? `agents ${quota.pool.agents.join(", ")}`
    : null;
  const lastUpdate = quota.updated_at ?? quota.observed_at;
  return {
    value,
    note: [
      window,
      reset,
      pool,
      poolAgents,
      lastUpdate ? `last update ${fmtTime(lastUpdate)}` : null,
      quota.source ? `source ${quota.source}` : null,
    ]
      .filter(Boolean)
      .join(" · ") || "reported",
    tone: "text-ink-300",
  };
}

function taskLabel(task: AgentTask): string {
  const title = task.title ?? "untitled task";
  const issue = task.issue ? `${task.issue} · ` : "";
  const job = task.job_title ?? task.job;
  const jobText = job ? ` · ${job}` : "";
  return `${issue}${title}${jobText}`;
}

function WorkBlock({
  agent,
  onOpenIssue,
}: {
  agent: Agent;
  onOpenIssue?: (id: string) => void;
}) {
  const tasks = activeTasks(agent);
  const runningTaskIds = new Set(
    (agent.running_messages ?? []).map((message) => message.task).filter(Boolean),
  );
  const work = tasks.map((task) => ({
    task,
    live: runningTaskIds.has(task.task),
  }));
  const blocked = agent.state === "blocked_by_quota" || agent.quota?.state === "blocked";
  const approval = agent.state === "waiting_input";
  if (work.length > 0) {
    return (
      <div className="space-y-1.5 min-w-0">
        {blocked && <div className="text-fail">blocked by quota</div>}
        {approval && <div className="text-warn">waiting for approval</div>}
        {work.map(({ task, live }) => (
          <div key={task.task} className="min-w-0">
            <div className="flex items-center gap-1.5 min-w-0">
              <i className={`w-1.5 h-1.5 rounded-full shrink-0 ${live ? "bg-info" : "bg-ink-600"}`} />
              {task.issue && onOpenIssue ? (
                <button
                  className="lnk num shrink-0"
                  onClick={(event) => {
                    event.stopPropagation();
                    onOpenIssue(task.issue);
                  }}
                >
                  {task.issue}
                </button>
              ) : null}
              <span className="truncate text-ink-200" title={taskLabel(task)}>
                {task.title ?? task.task}
              </span>
            </div>
            <div className="num text-micro text-ink-500 pl-3">
              {live ? "running" : task.task_state}
              {task.job_title ? ` · ${task.job_title}` : task.job ? ` · ${task.job}` : ""}
              {task.job_state ? ` · stage ${task.job_state}` : ""}
            </div>
          </div>
        ))}
      </div>
    );
  }
  if (blocked) return <span className="text-fail">blocked by quota</span>;
  if (approval) return <span className="text-warn">waiting for approval</span>;
  if (agent.running > 0) {
    const summaries = (agent.running_messages ?? [])
      .map((message) => message.summary)
      .filter((summary): summary is string => Boolean(summary))
      .join(" · ");
    const ids = (agent.running_messages ?? [])
      .map((message) => message.id)
      .filter(Boolean)
      .join(" · ");
    return (
      <span className="text-info" title={summaries || ids || "ad-hoc running"}>
        ad-hoc running{summaries ? ` · ${summaries}` : ids ? ` · ${ids}` : ""}
      </span>
    );
  }
  if (agent.queued > 0) return <span className="text-info">{agent.queued} queued</span>;
  if (agent.unknown > 0) return <span className="text-fail">{agent.unknown} needs review</span>;
  if (agent.inbox) return <span className="text-ink-500">mailbox</span>;
  if (agent.state === "idle") return <span className="text-ink-500">idle · no active work</span>;
  if (agent.state === "stopped") return <span className="text-ink-500">stopped · no active work</span>;
  return <span className="text-ink-500">{agent.state || "unknown"}</span>;
}

function ProfileBlock({ agent }: { agent: Agent }) {
  const model = profileModel(agent);
  const effort = profileEffort(agent);
  const usage = usageText(agent);
  return (
    <div className="space-y-1 min-w-[9rem]">
      <div className="num text-ink-200 truncate" title={`${model.value} · ${model.note}`}>
        {model.value}
      </div>
      <div className="num text-micro text-ink-500">
        model {model.note}
        {model.mismatch ? ` · ${model.mismatch}` : ""}
      </div>
      {model.provenance && (
        <div className="num text-micro text-ink-500">{model.provenance}</div>
      )}
      <div className="num text-ink-300">effort {effort.value}</div>
      <div className="num text-micro text-ink-500">{effort.note}</div>
      <div className={`num text-ink-300 ${usage.tone}`}>{usage.value}</div>
      <div className="num text-micro text-ink-500" title={usage.note}>
        {usage.note}
      </div>
    </div>
  );
}

function AgentDrawer({
  alias,
  onClose,
  onOpenIssue,
}: {
  alias: string;
  onClose: () => void;
  onOpenIssue: (id: string) => void;
}) {
  const [detail, setDetail] = useState<AgentDetail | null>(null);
  const [err, setErr] = useState<string | null>(null);

  useEffect(() => {
    setDetail(null);
    setErr(null);
    let live = true;
    api
      .agent(alias)
      .then((d) => live && setDetail(d))
      .catch((e) => live && setErr(String(e.message ?? e)));
    return () => {
      live = false;
    };
  }, [alias]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [onClose]);

  const a = detail?.agent;
  const caps = (a?.capabilities ?? {}) as Record<string, unknown>;
  const capList = Object.entries(caps).filter(([, v]) => v === true || typeof v === "string");

  return (
    <>
      <div className="fixed inset-0 bg-ink-950/70 z-20" onClick={onClose} />
      <aside
        className="drawer fixed top-0 right-0 h-full w-full sm:w-[34rem] bg-ink-875 border-l border-ink-700 z-30 flex flex-col"
        aria-label="Agent detail"
      >
        <header className="px-5 pt-4 pb-4 border-b border-ink-700 flex items-start gap-3 shrink-0">
          <div className="min-w-0 flex-1">
            <div className="num text-label text-ink-500">
              {alias}
              {a ? ` · ${a.provider} · ${a.endpoint_kind} · ${a.state}` : ""}
            </div>
            <h2 className="text-drawer font-semibold text-ink-100 leading-tight mt-1">
              {a?.role ?? "agent"}
            </h2>
          </div>
          <button
            aria-label="Close"
            onClick={onClose}
            className="closebtn ml-auto shrink-0 w-8 h-8 grid place-items-center rounded border border-ink-600 text-ink-300 bg-ink-850"
          >
            <svg
              width="12"
              height="12"
              viewBox="0 0 12 12"
              stroke="currentColor"
              strokeWidth="1.5"
              fill="none"
              style={{ pointerEvents: "none" }}
            >
              <path d="M2 2l8 8M10 2l-8 8" />
            </svg>
          </button>
        </header>

        <div className="flex-1 overflow-y-auto px-5 py-5 space-y-6">
          {err && <p className="text-secondary text-fail">{err}</p>}
          {detail && a && (
            <>
              {detail.fenced && detail.recovery && (
                <section>
                  <div className="flex items-baseline gap-2 mb-2">
                    <h3 className="text-cardtitle font-semibold text-fail">
                      Fenced
                    </h3>
                    <span className="kicker">recovery is an operator command</span>
                  </div>
                  <RecoveryBlock text={detail.recovery} />
                </section>
              )}

              <section>
                <div className="flex items-baseline gap-2 mb-2">
                  <h3 className="text-cardtitle font-semibold text-ink-100">
                    Identity
                  </h3>
                </div>
                <dl className="grid grid-cols-[7rem_1fr] gap-y-1.5 text-label">
                  {(
                    [
                      ["thread", a.thread_id],
                      ["session", a.session_id],
                      ["endpoint", a.endpoint],
                      ["pid", a.pid],
                      ["model", a.model],
                      ["generation", a.generation],
                      ["cwd", a.cwd],
                      ["sandbox", a.sandbox],
                    ] as [string, unknown][]
                  )
                    .filter(([, v]) => v !== null && v !== undefined && v !== "")
                    .map(([k, v]) => (
                      <div key={k} className="contents">
                        <dt className="slabel">{k}</dt>
                        <dd className="num text-ink-300 truncate" title={String(v)}>
                          {String(v)}
                        </dd>
                      </div>
                    ))}
                  {detail.resume && (
                    <div className="contents">
                      <dt className="slabel">resume</dt>
                      <dd>
                        <code className="num text-micro text-accent bg-accent/10 rounded px-1.5 py-0.5">
                          {detail.resume}
                        </code>
                      </dd>
                    </div>
                  )}
                  {Object.entries(a.params ?? {}).map(([k, v]) => (
                    <div key={k} className="contents">
                      <dt className="slabel" title={`param ${k}`}>
                        {k}
                      </dt>
                      <dd
                        className="num text-ink-300 truncate"
                        title={typeof v === "string" ? v : JSON.stringify(v)}
                      >
                        {typeof v === "string" ? v : JSON.stringify(v)}
                      </dd>
                    </div>
                  ))}
                </dl>
              </section>

              {(detail.tasks?.length ?? 0) > 0 && (
                <section>
                  <div className="flex items-baseline gap-2 mb-2">
                    <h3 className="text-cardtitle font-semibold text-ink-100">
                      Tasks
                    </h3>
                    <span className="kicker">assigned in flight</span>
                  </div>
                  <ul className="border border-ink-700 rounded-lg divide-y divide-ink-700/80 bg-ink-850">
                    {detail.tasks!.map((t) => (
                      <li
                        key={t}
                        className="flex items-center gap-2.5 px-3 py-2.5"
                      >
                        <span className="num text-label text-ink-200">{t}</span>
                      </li>
                    ))}
                  </ul>
                </section>
              )}

              {(detail.on?.length ?? 0) > 0 && (
                <section>
                  <div className="flex items-baseline gap-2 mb-2">
                    <h3 className="text-cardtitle font-semibold text-ink-100">
                      Issues
                    </h3>
                    <span className="kicker">bound through the job</span>
                  </div>
                  <div className="flex flex-wrap gap-1.5">
                    {detail.on!.map((id) => (
                      <button
                        key={id}
                        className="lnk"
                        onClick={() => onOpenIssue(id)}
                      >
                        {id}
                      </button>
                    ))}
                  </div>
                </section>
              )}

              {(detail.running.length > 0 || detail.queued > 0) && (
                <section>
                  <div className="flex items-baseline gap-2 mb-2">
                    <h3 className="text-cardtitle font-semibold text-ink-100">
                      Messages
                    </h3>
                    <span className="kicker">
                      {detail.running.length} running · {detail.queued} queued ·{" "}
                      {detail.unknown} unknown
                    </span>
                  </div>
                  <ul className="border border-ink-700 rounded-lg divide-y divide-ink-700/80 bg-ink-850">
                    {detail.running.map((m) => (
                      <li
                        key={m.id}
                        className="flex items-center gap-2.5 px-3 py-2.5"
                      >
                        <i className="w-1.5 h-1.5 rounded-full bg-info" />
                        <span className="num text-label text-ink-200">{m.id}</span>
                        {m.turn_id && (
                          <span
                            className="num text-micro text-ink-500"
                            title="turn id"
                          >
                            {m.turn_id}
                          </span>
                        )}
                        {m.task && (
                          <span className="num text-micro text-ink-500">
                            task {m.task}
                          </span>
                        )}
                        {m.summary && (
                          <span className="num text-micro text-ink-400 truncate" title={m.summary}>
                            {m.summary}
                          </span>
                        )}
                        {m.created && (
                          <span className="num text-micro text-ink-500 ml-auto">
                            {fmtTime(m.created)}
                          </span>
                        )}
                      </li>
                    ))}
                  </ul>
                </section>
              )}

              {capList.length > 0 && (
                <section>
                  <div className="flex items-baseline gap-2 mb-2">
                    <h3 className="text-cardtitle font-semibold text-ink-100">
                      Capabilities
                    </h3>
                    <span className="kicker">from the registry</span>
                  </div>
                  <div className="flex flex-wrap gap-1.5">
                    {capList.map(([k, v]) => (
                      <span
                        key={k}
                        className="chip bg-ink-800 text-ink-300"
                        title={typeof v === "string" ? v : k}
                      >
                        {typeof v === "string" ? `${k}: ${v}` : k}
                      </span>
                    ))}
                  </div>
                </section>
              )}

              <section>
                <div className="flex items-baseline gap-2 mb-2">
                  <h3 className="text-cardtitle font-semibold text-ink-100">
                    Events
                  </h3>
                  <span className="kicker">last {detail.events.length}</span>
                </div>
                {detail.events.length ? (
                  <ul className="border border-ink-700 rounded-lg divide-y divide-ink-700/80 bg-ink-850">
                    {detail.events.map((e) => (
                      <li
                        key={e.seq}
                        className="flex items-baseline gap-2.5 px-3 py-2"
                      >
                        <span className="num text-micro text-ink-500 w-8 shrink-0">
                          #{e.seq}
                        </span>
                        <span className="num text-label text-ink-200">
                          {e.kind}
                        </span>
                        {(e.task_id || e.job_id) && (
                          <span className="num text-micro text-ink-500">
                            {e.task_id ?? e.job_id}
                          </span>
                        )}
                        <span className="num text-micro text-ink-500 ml-auto shrink-0">
                          {fmtTime(e.at)}
                        </span>
                      </li>
                    ))}
                  </ul>
                ) : (
                  <p className="text-secondary text-ink-500">No events yet.</p>
                )}
              </section>
            </>
          )}
        </div>
      </aside>
    </>
  );
}

export default function Agents({
  payload,
  issues,
  project,
  onOpenIssue,
  loading = false,
  error = null,
}: {
  payload: AgentsPayload | null;
  issues: IssueCard[];
  project: string;
  onOpenIssue: (id: string) => void;
  loading?: boolean;
  error?: string | null;
}) {
  const [open, setOpen] = useState<string | null>(null);
  const issueProjects = issueProjectMap(issues);
  const allAgents = payload?.agents ?? [];
  const agents = allAgents
    .filter((agent) => agentMatchesProject(agent, project, issueProjects))
    .slice()
    .sort((a, b) => rank(a) - rank(b) || a.alias.localeCompare(b.alias));
  const globalAgents = allAgents.filter((agent) => agentIsUnassigned(agent, issueProjects));
  const otherProjectAgents = project === "all"
    ? []
    : allAgents.filter((agent) => !agentMatchesProject(agent, project, issueProjects) && !agentIsUnassigned(agent, issueProjects));
  const totals = agents.reduce(
    (sum, agent) => ({
      running: sum.running + agent.running,
      queued: sum.queued + agent.queued,
      fenced: sum.fenced + (agent.fenced ? 1 : 0),
      parked: sum.parked + agent.parked,
      inboxes: sum.inboxes + (agent.inbox ? 1 : 0),
    }),
    { running: 0, queued: 0, fenced: 0, parked: 0, inboxes: 0 },
  );
  // A refresh error is an observation about the new request. It must not
  // erase the last successful rows already held in `payload`.
  const showRows = payload !== null || (!loading && !error);
  const emptyCopy = agentsEmptyCopy(project);

  return (
    <main className="px-4 lg:px-8 pt-6 pb-9 max-w-[106rem] w-full">
      <div
        className="flex flex-wrap items-end gap-x-3 gap-y-3 mb-4 reveal"
        style={{ animationDelay: "40ms" }}
      >
        <h1 className="text-section font-semibold text-ink-100 leading-tight">
          Agents
        </h1>
        <span className="kicker">
          {agents.length} in {project === "all" ? "all projects" : project} · {totals?.fenced ?? 0} fenced ·{" "}
          {totals?.queued ?? 0} queued
        </span>
      </div>

      {project !== "all" && (globalAgents.length > 0 || otherProjectAgents.length > 0) && (
        <div className="card mb-4 px-4 py-3 text-label text-ink-400 border-ink-700">
          <span className="text-ink-200">Scope is exact issue/job ownership.</span>{" "}
          {globalAgents.length > 0 && `${globalAgents.length} global or unassigned observation${globalAgents.length === 1 ? " remains" : "s remain"}. `}
          {otherProjectAgents.length > 0 && `${otherProjectAgents.length} other-project agent${otherProjectAgents.length === 1 ? " is" : "s are"} available in All projects.`}
        </div>
      )}

      {payload?.daemon === "unreachable" && (
        <div className="card mb-4 px-4 py-3 text-secondary text-warn border-warn/40">
          daemon unreachable — the table below is the last known state.
        </div>
      )}

      {loading && (
        <div className="card mb-4 px-4 py-5 text-secondary text-ink-400" role="status">
          loading agent observations…
        </div>
      )}
      {error && (
        <div className="card mb-4 px-4 py-5 text-secondary text-fail border-fail/40" role="alert">
          could not load agent observations — {error}
        </div>
      )}

      {showRows && project !== "all" && globalAgents.length > 0 && (
        <details className="mb-4">
          <summary className="cursor-pointer text-micro text-ink-400 hover:text-ink-200">
            Global or unassigned agents · {globalAgents.length}
          </summary>
          <p className="text-micro text-ink-500 mt-2">
            These agents have no exact issue binding in the current payload and are not assigned to {project}.
          </p>
          <div className="flex flex-wrap gap-1.5 mt-2">
            {globalAgents.map((agent) => (
              <button key={agent.alias} className="chip bg-ink-800 text-ink-300 hover:text-accent" onClick={() => setOpen(agent.alias)}>
                {agent.alias} · {stateLabel(agent)}
              </button>
            ))}
          </div>
        </details>
      )}

      {/* Phone: stacked agent cards — the table's columns don't fit 390px. */}
      {showRows && <div className="sm:hidden space-y-2.5 reveal" style={{ animationDelay: "80ms" }}>
        {agents.map((a) => {
          const st = stateLabel(a);
          return (
            <article
              key={a.alias}
              role="button"
              tabIndex={0}
              onClick={() => setOpen(a.alias)}
              onKeyDown={(e) => {
                if (e.key === "Enter" || e.key === " ") {
                  e.preventDefault();
                  setOpen(a.alias);
                }
              }}
              className={`card w-full text-left p-3.5 space-y-2 cursor-pointer ${
                a.fenced ? "border-fail/40" : ""
              }`}
            >
              <div className="flex items-center gap-2">
                <i className={`w-1.5 h-1.5 rounded-full ${STATE_DOT[st] ?? STATE_DOT.stopped}`} />
                <span className="num text-label text-ink-100 font-medium">
                  {a.alias}
                </span>
                <span className={`chip ml-auto ${STATE_CHIP[st] ?? STATE_CHIP.stopped}`}>
                  {st}
                </span>
              </div>
              {a.fenced && <RecoveryBlock text={a.recovery ?? "fenced"} />}
              <div className="grid grid-cols-[5.4rem_1fr] gap-y-1 text-label">
                <span className="slabel">provider</span>
                <span className="num text-ink-300">
                  {a.provider}/{a.endpoint_kind}
                </span>
                <span className="slabel">group</span>
                <span className="num text-ink-300">
                  {a.group_root ? "root" : a.group}
                </span>
                <span className="slabel">role</span>
                <span className="num text-ink-300">
                  {a.role ?? "unknown"}
                  {a.team_role ? ` · team ${a.team_role}` : ""}
                </span>
                <span className="slabel">model</span>
                <div><ProfileBlock agent={a} /></div>
                <span className="slabel">current work</span>
                <div className="min-w-0"><WorkBlock agent={a} onOpenIssue={onOpenIssue} /></div>
                {a.on.length > 0 && (
                  <>
                    <span className="slabel">on issue</span>
                    <span className="flex flex-wrap gap-1.5">
                      {a.on.map((id) => (
                        <span
                          key={id}
                          role="link"
                          className="lnk"
                          onClick={(e) => {
                            e.stopPropagation();
                            onOpenIssue(id);
                          }}
                        >
                          {id}
                        </span>
                      ))}
                    </span>
                  </>
                )}
                <span className="slabel">running</span>
                <span className="num text-ink-300">
                  {a.running}
                  {a.message?.id ? ` · ${a.message.id}` : ""}
                </span>
                <span className="slabel">queued</span>
                <span className={`num ${a.unknown > 0 ? "text-fail" : "text-ink-300"}`}>
                  {a.queued}
                  {a.unknown > 0 ? ` +${a.unknown} unk` : ""}
                </span>
                <span className="slabel">activity</span>
                <span className="num text-ink-300">
                  {a.last_activity ? fmtTime(a.last_activity) : "—"}
                  {a.stalled
                    ? ` · stalled ${fmtSilence(a.silent_secs ?? 0)}`
                    : (a.silent_secs ?? 0) >= 60
                      ? ` · silent ${fmtSilence(a.silent_secs!)}`
                      : ""}
                </span>
                {(a.fenced || a.resume) && (
                  <>
                    <span className="slabel">recovery</span>
                    <span>
                      {a.fenced ? (
                        <span className="chip bg-fail/10 text-fail">reconcile</span>
                      ) : (
                        <code className="num text-micro text-ink-400" title={a.resume_hint ?? "resume command"}>
                          {a.resume}
                        </code>
                      )}
                    </span>
                  </>
                )}
              </div>
            </article>
          );
        })}
        {agents.length === 0 && (
          <div className="card p-8 text-center text-ink-500">
            {emptyCopy}
          </div>
        )}
      </div>}

      {showRows && <div className="hidden sm:block card overflow-hidden reveal" style={{ animationDelay: "80ms" }}>
        <div className="overflow-x-auto">
        <table className="w-full min-w-[64rem] text-label">
          <thead>
            <tr className="border-b border-ink-700 text-left">
              <th className="slabel font-normal px-4 py-2.5">agent</th>
              <th className="slabel font-normal px-3 py-2.5">provider</th>
              <th className="slabel font-normal px-3 py-2.5">state</th>
              <th className="slabel font-normal px-3 py-2.5">group</th>
              <th className="slabel font-normal px-3 py-2.5">model / effort / usage</th>
              <th className="slabel font-normal px-3 py-2.5">current work</th>
              <th className="slabel font-normal px-3 py-2.5">queue</th>
              <th className="slabel font-normal px-3 py-2.5">last activity</th>
              <th className="slabel font-normal px-3 py-2.5">recovery</th>
            </tr>
          </thead>
          <tbody className="divide-y divide-ink-700/70">
            {agents.map((a) => {
              const st = stateLabel(a);
              return (
                <tr
                  key={a.alias}
                  onClick={() => setOpen(a.alias)}
                  className={`cursor-pointer hover:bg-ink-850 transition-colors ${
                    a.fenced ? "bg-fail/[.04]" : ""
                  }`}
                >
                  <td className="px-4 py-2.5 align-top">
                    <span className="num text-ink-100">{a.alias}</span>
                    <div className="num text-micro text-ink-500 truncate" title={a.role ?? "role unknown"}>
                      {a.role ?? "role unknown"}
                    </div>
                    {a.fenced && <RecoveryBlock text={a.recovery ?? "fenced"} />}
                  </td>
                  <td className="px-3 py-2.5 align-top">
                    <span className="num text-ink-400">
                      {a.provider}/{a.endpoint_kind}
                    </span>
                  </td>
                  <td className="px-3 py-2.5 align-top">
                    <span className={`chip ${STATE_CHIP[st] ?? STATE_CHIP.stopped}`}>
                      <i className={`w-1.5 h-1.5 rounded-full ${STATE_DOT[st] ?? STATE_DOT.stopped}`} />
                      {st}
                    </span>
                  </td>
                  <td className="px-3 py-2.5 align-top">
                    <span className="num text-ink-400">
                      {a.group_root ? "root" : a.group}
                    </span>
                  </td>
                  <td className="px-3 py-2.5 align-top">
                    <ProfileBlock agent={a} />
                  </td>
                  <td className="px-3 py-2.5 align-top">
                    <WorkBlock agent={a} onOpenIssue={onOpenIssue} />
                  </td>
                  <td className="px-3 py-2.5 align-top">
                    <span className={`num ${a.unknown > 0 ? "text-fail" : "text-ink-200"}`}>
                      {a.running} running · {a.queued} queued
                    </span>
                    {a.unknown > 0 && <div className="num text-micro text-fail mt-0.5">{a.unknown} unknown</div>}
                  </td>
                  <td className="px-3 py-2.5 align-top">
                    <span className="num text-ink-400">
                      {a.last_activity ? fmtTime(a.last_activity) : "—"}
                    </span>
                    {a.stalled ? (
                      <div>
                        <span className="chip bg-warn/10 text-warn mt-1">
                          stalled {fmtSilence(a.silent_secs ?? 0)}
                        </span>
                      </div>
                    ) : (a.silent_secs ?? 0) >= 60 ? (
                      <div className="num text-micro text-ink-500 mt-0.5">
                        silent {fmtSilence(a.silent_secs!)}
                      </div>
                    ) : null}
                  </td>
                  <td className="px-3 py-2.5 align-top">
                    {a.fenced ? (
                      <span className="chip bg-fail/10 text-fail">
                        reconcile
                      </span>
                    ) : a.resume ? (
                      <code
                        className="num text-micro text-ink-400"
                        title={a.resume_hint ?? "resume command"}
                      >
                        {a.resume}
                      </code>
                    ) : (
                      <span className="num text-ink-600">—</span>
                    )}
                  </td>
                </tr>
              );
            })}
            {agents.length === 0 && (
              <tr>
                <td colSpan={9} className="px-4 py-8 text-center text-ink-500">
                  {emptyCopy}
                </td>
              </tr>
            )}
          </tbody>
        </table>
        </div>
      </div>}

      <footer className="mt-8 pt-4 border-t border-ink-700 text-label text-ink-500 num">
        daemon agent_list + agent_show · binding via tasks × jobs.issue_id ·
        fenced agents need `cadence agent unfence` / `message reconcile`.
      </footer>

      {open && (
        <AgentDrawer
          alias={open}
          onClose={() => setOpen(null)}
          onOpenIssue={(id) => {
            setOpen(null);
            onOpenIssue(id);
          }}
        />
      )}
    </main>
  );
}
