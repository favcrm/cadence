import { useEffect, useRef, useState } from "react";
import { api } from "../../lib/api";
import { fmtTime } from "../../lib/fmt";
import { provenanceDetail } from "./modelProvenance";
import type { ResourceState } from "../../lib/cache";
import {
  agentIsUnassigned,
  agentIssueIds,
  agentMatchesProject,
  issueIndex,
  type IssueIndex,
} from "../../lib/scope";
import { agentsEmptyCopy } from "../../lib/uxCopy";
import { ResourceGate, StaleChip } from "../../ui/ResourceStatus";
import { IconClose, IconSearch } from "../../ui/icons";
import Button from "../../ui/Button";
import {
  AGENT_FILTERS,
  agentCategory,
  agentMatchesSearch,
  agentOrder,
  agentStatus,
  type AgentFilter,
} from "./agentView";
import "./agents.css";
import type {
  Agent,
  AgentDetail,
  AgentTask,
  AgentsPayload,
  IssueCard,
  UsageLimit,
} from "../../lib/types";

const STATE_CHIP: Record<string, string> = {
  busy: "bg-info/10 text-info",
  idle: "bg-ok/10 text-ok",
  stopped: "bg-ink-800 text-ink-400",
  attention: "bg-fail/10 text-fail",
  inbox: "bg-ink-800 text-ink-500",
  "needs input": "bg-warn/10 text-warn",
  queued: "bg-info/10 text-info",
  "quota blocked": "bg-fail/10 text-fail",
  error: "bg-fail/10 text-fail",
};

const STATE_DOT: Record<string, string> = {
  busy: "bg-info",
  idle: "bg-ok",
  stopped: "bg-ink-600",
  attention: "bg-fail",
  inbox: "bg-ink-700",
  "needs input": "bg-warn",
  queued: "bg-info",
  "quota blocked": "bg-fail",
  error: "bg-fail",
};

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

const TERMINAL_TASK_STATES = new Set([
  "verified",
  "done",
  "cancelled",
  "failed",
]);

function activeTasks(a: Agent): AgentTask[] {
  return (a.tasks ?? []).filter(
    (task) => !TERMINAL_TASK_STATES.has(task.task_state),
  );
}

function profileModel(a: Agent): {
  value: string;
  note: string;
  mismatch?: string;
  provenance?: string;
} {
  const reported = a.model_reported ?? a.model ?? null;
  const configured = a.model_configured ?? null;
  const provenanceText = provenanceDetail(
    a.model_selection,
    a.model_lookup_role,
  );
  if (
    a.model_selection === null &&
    (a.provider === "devin" || a.provider === "inbox")
  ) {
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
        configured && configured !== reported
          ? `configured ${configured}`
          : undefined,
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
    return {
      value: "error",
      note: quota.message ?? quota.reason ?? "quota query failed",
      tone: "text-fail",
    };
  }
  if (state === "stale") {
    return {
      value: "stale",
      note: [
        quota.observed_at
          ? `last update ${fmtTime(quota.observed_at)}`
          : "last update unknown",
        quota.window,
        quota.source ? `source ${quota.source}` : null,
      ]
        .filter(Boolean)
        .join(" · "),
      tone: "text-warn",
    };
  }
  if (state === "blocked") {
    return {
      value: "blocked",
      note: quota.reason ?? "provider reported a quota block",
      tone: "text-fail",
    };
  }
  if (state !== "available") {
    return {
      value: "unknown",
      note: quota.reason ?? "allowance not reported",
      tone: "text-ink-500",
    };
  }
  // Null means unavailable; zero is a real value and must remain visible.
  const unit = quota.unit ? ` ${quota.unit}` : "";
  const remaining =
    quota.remaining != null ? `${quota.remaining}${unit} remaining` : null;
  const used = quota.used != null ? `${quota.used}${unit} used` : null;
  const limit = quota.limit != null ? `${quota.limit}${unit} limit` : null;
  const usedAgainstLimit =
    quota.used != null && quota.limit != null
      ? `${quota.used}/${quota.limit}${unit} used`
      : null;
  const percent =
    quota.used_percent != null ? `${quota.used_percent}% used` : null;
  const value =
    remaining ?? usedAgainstLimit ?? used ?? percent ?? limit ?? "available";
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
    note:
      [
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
    (agent.running_messages ?? [])
      .map((message) => message.task)
      .filter(Boolean),
  );
  const work = tasks.map((task) => ({
    task,
    live: runningTaskIds.has(task.task),
  }));
  const blocked =
    agent.state === "blocked_by_quota" ||
    agent.quota?.state === "blocked" ||
    agent.usage_limit?.state === "blocked";
  const approval = agent.state === "waiting_input";
  if (work.length > 0) {
    return (
      <div className="space-y-1.5 min-w-0">
        {blocked && <div className="text-fail">blocked by quota</div>}
        {approval && <div className="text-warn">Waiting for input</div>}
        {work.map(({ task, live }) => (
          <div key={task.task} className="min-w-0">
            <div className="flex items-center gap-1.5 min-w-0">
              <i
                className={`w-1.5 h-1.5 rounded-full shrink-0 ${live ? "bg-info" : "bg-ink-600"}`}
              />
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
              {task.job_title
                ? ` · ${task.job_title}`
                : task.job
                  ? ` · ${task.job}`
                  : ""}
              {task.job_state ? ` · stage ${task.job_state}` : ""}
            </div>
          </div>
        ))}
      </div>
    );
  }
  if (blocked) return <span className="text-fail">blocked by quota</span>;
  if (approval) return <span className="text-warn">Waiting for input</span>;
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
        {summaries || ids || "Running without an issue"}
      </span>
    );
  }
  if (agent.queued > 0)
    return <span className="text-info">{agent.queued} queued</span>;
  if (agent.unknown > 0)
    return <span className="text-fail">{agent.unknown} needs review</span>;
  if (agent.inbox) return <span className="text-ink-500">mailbox</span>;
  if (agent.state === "idle")
    return <span className="text-ink-500">idle · no active work</span>;
  if (agent.state === "stopped" && agent.state_label)
    return (
      <span className="text-ink-500">{agent.state_label} · resumable</span>
    );
  if (agent.state === "stopped")
    return <span className="text-ink-500">stopped · no active work</span>;
  return <span className="text-ink-500">{agent.state || "unknown"}</span>;
}

/** Exact assigned/owned issue links not already shown with current work. */
function RelatedIssues({
  agent,
  index,
  onOpenIssue,
}: {
  agent: Agent;
  index: IssueIndex;
  onOpenIssue: (id: string) => void;
}) {
  const viaTask = new Set(activeTasks(agent).map((task) => task.issue));
  const ids = agentIssueIds(agent, index).filter((id) => !viaTask.has(id));
  if (ids.length === 0) return null;
  return (
    <div className="num text-micro text-ink-500 mt-1 flex flex-wrap items-center gap-x-1.5">
      Issues
      {ids.map((id) => (
        <button
          key={id}
          className="lnk"
          onClick={(event) => {
            event.stopPropagation();
            onOpenIssue(id);
          }}
        >
          {id}
        </button>
      ))}
    </div>
  );
}

function ProfileBlock({ agent }: { agent: Agent }) {
  const model = profileModel(agent);
  const effort = profileEffort(agent);
  const usage = usageText(agent);
  return (
    <div className="space-y-1 min-w-0 agents-profile">
      <div
        className="num text-ink-200 truncate"
        title={`${model.value} · ${model.note}`}
      >
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
  observed,
  onClose,
  onOpenIssue,
}: {
  alias: string;
  observed?: Agent;
  onClose: () => void;
  onOpenIssue: (id: string) => void;
}) {
  const [detail, setDetail] = useState<AgentDetail | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [retry, setRetry] = useState(0);
  const dialogRef = useRef<HTMLDialogElement>(null);

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
  }, [alias, retry]);

  useEffect(() => {
    const dialog = dialogRef.current;
    const trigger = document.activeElement;
    const overflow = document.body.style.overflow;
    dialog?.showModal();
    document.body.style.overflow = "hidden";
    return () => {
      dialog?.close();
      document.body.style.overflow = overflow;
      if (trigger instanceof HTMLElement && trigger.isConnected)
        trigger.focus();
    };
  }, []);

  const a = detail?.agent;
  // A collection observation keeps evidence available during loading/failure.
  // Detail carries provider evidence, but not selection provenance or grouping.
  const profile: Agent | undefined = a
    ? {
        group: "",
        parked: 0,
        ...observed,
        ...a,
        tasks: observed?.tasks,
        running: detail.running.length,
        queued: detail.queued,
        unknown: detail.unknown,
        fenced: detail.fenced,
        on: detail.on ?? observed?.on ?? [],
      }
    : observed;
  const fenced = detail?.fenced ?? observed?.fenced;
  const recovery = detail?.recovery ?? observed?.recovery;
  const resume = detail?.resume ?? observed?.resume;
  const caps = (a?.capabilities ?? {}) as Record<string, unknown>;
  const capList = Object.entries(caps).filter(
    ([, v]) => v === true || typeof v === "string",
  );

  return (
    <dialog
      ref={dialogRef}
      className="agents-drawer bg-ink-875 border-l border-ink-700 text-ink-200"
      aria-labelledby="agent-detail-title"
      onCancel={(event) => {
        event.preventDefault();
        onClose();
      }}
      onClick={(event) => {
        if (event.target !== event.currentTarget) return;
        const rect = event.currentTarget.getBoundingClientRect();
        if (
          event.clientX < rect.left ||
          event.clientX > rect.right ||
          event.clientY < rect.top ||
          event.clientY > rect.bottom
        )
          onClose();
      }}
    >
      <header className="px-5 pt-4 pb-4 border-b border-ink-700 flex items-start gap-3 shrink-0">
        <div className="min-w-0 flex-1">
          <h2
            id="agent-detail-title"
            className="text-drawer font-semibold text-ink-100 leading-tight break-words"
          >
            {alias}
          </h2>
          <p className="text-label text-ink-400 mt-1">
            {a?.role ?? observed?.role ?? "Agent"} ·{" "}
            {a?.provider ?? observed?.provider ?? "Loading provider"}
            {profile && ` / ${profile.endpoint_kind}`}
          </p>
          {observed && (
            <p className="text-micro text-ink-500 mt-1">
              Group: {observed.group_root ? "root" : observed.group}
              {observed.team_role ? ` · Team role: ${observed.team_role}` : ""}
            </p>
          )}
          {profile && (
            <div className="mt-2">
              <AgentState agent={profile} />
            </div>
          )}
        </div>
        <Button
          icon={<IconClose />}
          aria-label="Close agent details"
          className="agents-close"
          onClick={onClose}
        />
      </header>

      <div className="flex-1 overflow-y-auto px-5 py-5 space-y-6">
        {!detail && !err && (
          <p role="status" className="text-label text-ink-500">
            Loading agent details…
          </p>
        )}
        {err && (
          <div className="space-y-2">
            <p role="alert" className="text-label text-fail">
              Could not load agent details: {err}
            </p>
            <Button onClick={() => setRetry((r) => r + 1)}>
              Retry details
            </Button>
          </div>
        )}
        {fenced && (
          <section>
            <h3 className="text-cardtitle font-semibold text-fail">
              Recovery required
            </h3>
            <p className="text-label text-ink-400 mt-1">
              An operator must inspect the outcome before reconciling.
            </p>
            <RecoveryBlock
              text={
                recovery ??
                "Recovery guidance unavailable. Refresh agent details before taking action."
              }
            />
          </section>
        )}
        {observed && (
          <section>
            <h3 className="text-cardtitle font-semibold text-ink-100 mb-2">
              Current work
            </h3>
            <WorkBlock agent={observed} onOpenIssue={onOpenIssue} />
            <AgentQueue agent={observed} />
            <div className="mt-2">
              <AgentActivity agent={observed} />
            </div>
          </section>
        )}
        {profile && (
          <section>
            <h3 className="text-cardtitle font-semibold text-ink-100 mb-2">
              Model, effort & usage
            </h3>
            <ProfileBlock agent={profile} />
          </section>
        )}
        {resume && (
          <section>
            <h3 className="text-cardtitle font-semibold text-ink-100 mb-2">
              Resume command
            </h3>
            <p className="text-label text-ink-400 mb-2">
              {observed?.resume_hint ??
                "For use in a terminal by the operator."}
            </p>
            <pre className="agents-command num text-label text-accent bg-accent/10 rounded p-3">
              {resume}
            </pre>
          </section>
        )}
        {detail && a && (
          <>
            <section>
              <div className="flex items-baseline gap-2 mb-2">
                <h3 className="text-cardtitle font-semibold text-ink-100">
                  Identity
                </h3>
              </div>
              <dl className="grid grid-cols-[7rem_minmax(0,1fr)] gap-y-1.5 text-label">
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
                      <dd
                        className="num text-ink-300 break-words"
                        title={String(v)}
                      >
                        {String(v)}
                      </dd>
                    </div>
                  ))}
                {Object.entries(a.params ?? {}).map(([k, v]) => (
                  <div key={k} className="contents">
                    <dt className="slabel" title={`param ${k}`}>
                      {k}
                    </dt>
                    <dd
                      className="num text-ink-300 break-words"
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
                      <span className="num text-label text-ink-200 break-words">
                        {t}
                      </span>
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
                      <span className="num text-label text-ink-200 break-words">
                        {m.id}
                      </span>
                      {m.task && (
                        <span className="num text-micro text-ink-500">
                          task {m.task}
                        </span>
                      )}
                      {m.summary && (
                        <span
                          className="num text-micro text-ink-400 truncate"
                          title={m.summary}
                        >
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
                      className="flex flex-wrap items-baseline gap-2.5 px-3 py-2"
                    >
                      <span className="num text-micro text-ink-500 w-8 shrink-0">
                        #{e.seq}
                      </span>
                      <span className="num text-label text-ink-200 break-words">
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
    </dialog>
  );
}

function AgentState({ agent }: { agent: Agent }) {
  const st = agentStatus(agent);
  return (
    <span className={`chip ${STATE_CHIP[st] ?? STATE_CHIP.stopped}`}>
      <i
        aria-hidden="true"
        className={`w-1.5 h-1.5 rounded-full ${STATE_DOT[st] ?? STATE_DOT.stopped}`}
      />
      {st}
    </span>
  );
}

function AgentQueue({ agent: a }: { agent: Agent }) {
  return (
    <div className="text-label">
      <span className="num text-ink-400">
        {a.running} running · {a.queued} queued
      </span>
      {a.unknown > 0 && (
        <div className="text-fail mt-1">{a.unknown} outcome unknown</div>
      )}
      {a.parked > 0 && (
        <div className="text-ink-500 mt-1">{a.parked} parked</div>
      )}
    </div>
  );
}

function AgentActivity({ agent: a }: { agent: Agent }) {
  return (
    <div className="text-label text-ink-400">
      <span className="num">
        {a.last_activity ? fmtTime(a.last_activity) : "No activity recorded"}
      </span>
      {a.stalled ? (
        <div className="text-warn mt-1">
          Stalled · {fmtSilence(a.silent_secs ?? 0)}
        </div>
      ) : (a.silent_secs ?? 0) >= 60 ? (
        <div className="text-micro text-ink-500 mt-1">
          Silent · {fmtSilence(a.silent_secs!)}
        </div>
      ) : null}
    </div>
  );
}

function AgentIdentity({
  agent: a,
  onOpen,
}: {
  agent: Agent;
  onOpen: () => void;
}) {
  return (
    <div className="min-w-0">
      <button
        type="button"
        className="agent-name"
        onClick={onOpen}
        aria-label={`Open agent ${a.alias}`}
      >
        {a.alias}
      </button>
      <p className="text-micro text-ink-500 mt-1 break-words">
        {a.role ?? "Agent"} · {a.provider} · {a.group_root ? "root" : a.group}
      </p>
    </div>
  );
}

export default function Agents({
  state,
  issues: issuesState,
  project,
  open,
  onOpenAgent: setOpen,
  onOpenIssue,
  onRetry,
  onRetryAssignments,
}: {
  state: ResourceState<AgentsPayload>;
  issues: ResourceState<IssueCard[]>;
  project: string;
  open: string | null;
  onOpenAgent: (alias: string | null) => void;
  onOpenIssue: (id: string) => void;
  onRetry: () => void;
  onRetryAssignments: () => void;
}) {
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState<AgentFilter>("all");
  const payload = state.data;
  const scopeUnavailable = project !== "all" && issuesState.data === null;
  const issueProjects = issueIndex(issuesState.data ?? []);
  const allAgents = payload?.agents ?? [];
  const scoped = allAgents.filter((a) =>
    agentMatchesProject(a, project, issueProjects),
  );
  const counts = Object.fromEntries(
    AGENT_FILTERS.map(({ value }) => [
      value,
      value === "all"
        ? scoped.length
        : scoped.filter((a) => agentCategory(a) === value).length,
    ]),
  ) as Record<AgentFilter, number>;
  const agents = scoped
    .filter(
      (a) =>
        (filter === "all" || agentCategory(a) === filter) &&
        agentMatchesSearch(a, query, issueProjects),
    )
    .sort(
      (a, b) => agentOrder(a) - agentOrder(b) || a.alias.localeCompare(b.alias),
    );
  const globalAgents = allAgents.filter((a) =>
    agentIsUnassigned(a, issueProjects),
  );
  const otherProjectAgents =
    project === "all"
      ? []
      : allAgents.filter(
          (a) =>
            !agentMatchesProject(a, project, issueProjects) &&
            !agentIsUnassigned(a, issueProjects),
        );
  const filtered = filter !== "all" || query.trim() !== "";
  const reset = () => {
    setQuery("");
    setFilter("all");
  };
  const emptyCopy =
    scoped.length === 0
      ? agentsEmptyCopy(project)
      : "No agents match these filters.";
  const agent = allAgents.find((a) => a.alias === open);
  const summary = !payload
    ? state.status === "failed"
      ? "Agents unavailable"
      : "Loading agents…"
    : scopeUnavailable
      ? issuesState.status === "failed"
        ? "Assignments unavailable"
        : "Loading assignments…"
      : `${scoped.length} ${project === "all" ? "across all projects" : `in ${project}`}`;

  return (
    <main className="agents-page px-4 lg:px-8 pt-6 pb-9 w-full">
      <div className="flex flex-wrap items-center gap-3 mb-2">
        <h1 className="text-section font-semibold text-ink-100">Agents</h1>
        <span className="text-label text-ink-500">{summary}</span>
        <StaleChip state={state} />
      </div>
      <p className="text-label text-ink-400 mb-5">
        See current work, waiting agents, and recent activity.
      </p>

      {project !== "all" &&
        issuesState.data !== null &&
        (globalAgents.length > 0 || otherProjectAgents.length > 0) && (
          <p className="text-label text-ink-500 mb-4">
            Agents working on or owning active issues in {project}.{" "}
            {otherProjectAgents.length > 0 &&
              `${otherProjectAgents.length} in other projects are visible in All projects.`}
          </p>
        )}
      {payload?.daemon === "unreachable" && (
        <div
          className="card mb-4 px-4 py-3 text-label text-warn border-warn/40"
          role="status"
        >
          Daemon unavailable. Showing the last known agent state.
        </div>
      )}
      <ResourceGate
        state={state}
        loading="Loading agents…"
        failed="Could not load agents"
        onRetry={onRetry}
      />
      {payload && scopeUnavailable && (
        <div className="text-label text-warn mb-4 space-y-2">
          <p role={issuesState.status === "failed" ? "alert" : "status"}>
            {issuesState.status === "failed"
              ? "Project issue assignments could not be loaded. Select All projects to see every agent."
              : "Loading project issue assignments…"}
          </p>
          {issuesState.status === "failed" && (
            <Button onClick={onRetryAssignments}>Retry assignments</Button>
          )}
        </div>
      )}
      {project !== "all" && issuesState.status === "stale" && (
        <p role="status" className="text-label text-warn mb-4">
          Project issue assignments may be out of date.
        </p>
      )}

      {payload && !scopeUnavailable && (
        <>
          <div className="agents-tools mb-4">
            <div
              className="agents-filters"
              role="group"
              aria-label="Filter agents by status"
            >
              {AGENT_FILTERS.filter(
                (f) =>
                  f.value !== "other" || counts.other > 0 || filter === "other",
              ).map(({ value, label }) => (
                <button
                  key={value}
                  type="button"
                  className="agents-filter"
                  aria-pressed={filter === value}
                  onClick={() => setFilter(value)}
                >
                  {label}
                  <span className="num">{counts[value]}</span>
                </button>
              ))}
            </div>
            <label className="agents-search" htmlFor="agents-search">
              <IconSearch />
              <span className="sr-only">Search agents</span>
              <input
                id="agents-search"
                type="search"
                value={query}
                onChange={(e) => setQuery(e.target.value)}
                placeholder="Search agents, work, or issues"
              />
            </label>
          </div>
          {filtered && (
            <div className="flex items-center gap-3 mb-3 text-label text-ink-500">
              <span role="status">
                Showing {agents.length} of {scoped.length} agents
              </span>
              <Button variant="ghost" size="sm" onClick={reset}>
                Clear filters
              </Button>
            </div>
          )}
          {agents.length === 0 ? (
            <div className="card py-10 px-5 text-center text-label text-ink-500">
              {emptyCopy}
            </div>
          ) : (
            <>
              <div className="agents-cards space-y-3">
                {agents.map((a) => (
                  <article key={a.alias} className="card p-4">
                    <div className="flex items-start justify-between gap-3">
                      <AgentIdentity
                        agent={a}
                        onOpen={() => setOpen(a.alias)}
                      />
                      <AgentState agent={a} />
                    </div>
                    {a.fenced && (
                      <p className="text-label text-fail mt-2">
                        Recovery required · Open agent for guidance
                      </p>
                    )}
                    <div className="mt-3">
                      <WorkBlock agent={a} onOpenIssue={onOpenIssue} />
                      <RelatedIssues
                        agent={a}
                        index={issueProjects}
                        onOpenIssue={onOpenIssue}
                      />
                    </div>
                    <div className="agents-card-footer">
                      <AgentQueue agent={a} />
                      <AgentActivity agent={a} />
                    </div>
                  </article>
                ))}
              </div>
              <div className="agents-table card overflow-hidden">
                <table className="w-full text-label">
                  <caption className="sr-only">
                    Agents in {project === "all" ? "all projects" : project}
                  </caption>
                  <thead>
                    <tr>
                      {[
                        "Agent",
                        "Status",
                        "Current work",
                        "Queue",
                        "Last activity",
                      ].map((h) => (
                        <th
                          key={h}
                          scope="col"
                          className="slabel font-normal text-left px-4 py-3"
                        >
                          {h}
                        </th>
                      ))}
                    </tr>
                  </thead>
                  <tbody className="divide-y divide-ink-700/70">
                    {agents.map((a) => (
                      <tr key={a.alias}>
                        <td>
                          <AgentIdentity
                            agent={a}
                            onOpen={() => setOpen(a.alias)}
                          />
                        </td>
                        <td>
                          <AgentState agent={a} />
                          {a.fenced && (
                            <p className="text-micro text-fail mt-2">
                              Recovery required
                            </p>
                          )}
                        </td>
                        <td>
                          <WorkBlock agent={a} onOpenIssue={onOpenIssue} />
                          <RelatedIssues
                            agent={a}
                            index={issueProjects}
                            onOpenIssue={onOpenIssue}
                          />
                        </td>
                        <td>
                          <AgentQueue agent={a} />
                        </td>
                        <td>
                          <AgentActivity agent={a} />
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            </>
          )}
          {project !== "all" &&
            issuesState.data !== null &&
            globalAgents.length > 0 && (
              <details className="mt-5 agents-unassigned">
                <summary className="text-label text-ink-500 cursor-pointer">
                  Global or unassigned agents · {globalAgents.length}
                </summary>
                <p className="text-label text-ink-500 my-2">
                  These agents have no issue assignment to a project.
                </p>
                <div className="flex flex-wrap gap-2">
                  {globalAgents.map((a) => (
                    <Button
                      key={a.alias}
                      size="sm"
                      onClick={() => setOpen(a.alias)}
                    >
                      {a.alias}
                    </Button>
                  ))}
                </div>
              </details>
            )}
        </>
      )}
      {open && (
        <AgentDrawer
          key={open}
          alias={open}
          observed={agent}
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
