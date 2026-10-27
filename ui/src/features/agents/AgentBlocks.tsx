/** Presentational agent blocks shared by the Agents list and the agent drawer. */

import { activityTimeMs, fmtTime } from "../../lib/fmt";
import { useTranslate } from "../../lib/locale";
import { agentStatus } from "./agentView";
import type { Agent, AgentTask } from "../../lib/types";

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

const TERMINAL_TASK_STATES = new Set([
  "verified",
  "done",
  "cancelled",
  "failed",
]);

export function activeTasks(a: Agent): AgentTask[] {
  return (a.tasks ?? []).filter(
    (task) => !TERMINAL_TASK_STATES.has(task.task_state),
  );
}

function taskLabel(task: AgentTask, t: (english: string) => string): string {
  const title = task.title ?? t("untitled task");
  const issue = task.issue ? `${task.issue} · ` : "";
  const job = task.job_title ?? task.job;
  const jobText = job ? ` · ${job}` : "";
  return `${issue}${title}${jobText}`;
}

export function WorkBlock({
  agent,
  onOpenIssue,
}: {
  agent: Agent;
  onOpenIssue?: (id: string) => void;
}) {
  const t = useTranslate();
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
        {blocked && <div className="text-fail">{t("blocked by quota")}</div>}
        {approval && <div className="text-warn">{t("Waiting for input")}</div>}
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
              <span className="truncate text-ink-200" title={taskLabel(task, t)}>
                {task.title ?? task.task}
              </span>
            </div>
            <div className="num text-micro text-ink-500 pl-3">
              {live ? t("running") : task.task_state}
              {task.job_title
                ? ` · ${task.job_title}`
                : task.job
                  ? ` · ${task.job}`
                  : ""}
              {task.job_state ? ` · ${t("stage")} ${task.job_state}` : ""}
            </div>
          </div>
        ))}
      </div>
    );
  }
  if (blocked) return <span className="text-fail">{t("blocked by quota")}</span>;
  if (approval) return <span className="text-warn">{t("Waiting for input")}</span>;
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
      <span className="text-info" title={summaries || ids || t("ad-hoc running")}>
        {summaries || (ids ? t("Working on a message") : t("Running without an issue"))}
      </span>
    );
  }
  if (agent.queued > 0)
    return (
      <span className="text-info">
        {t("{count} queued").replace("{count}", String(agent.queued))}
      </span>
    );
  if (agent.unknown > 0)
    return (
      <span className="text-fail">
        {t("{count} needs review").replace("{count}", String(agent.unknown))}
      </span>
    );
  if (agent.inbox) return <span className="text-ink-500">{t("mailbox")}</span>;
  if (agent.state === "idle")
    return <span className="text-ink-500">{t("idle · no active work")}</span>;
  if (agent.state === "stopped" && agent.state_label)
    return (
      <span className="text-ink-500">
        {t("{label} · resumable").replace("{label}", () => String(agent.state_label))}
      </span>
    );
  if (agent.state === "stopped")
    return <span className="text-ink-500">{t("stopped · no active work")}</span>;
  return <span className="text-ink-500">{agent.state || t("unknown")}</span>;
}

export function AgentState({ agent }: { agent: Agent }) {
  const t = useTranslate();
  const st = agentStatus(agent);
  return (
    <span className={`chip ${STATE_CHIP[st] ?? STATE_CHIP.stopped}`}>
      <i
        aria-hidden="true"
        className={`w-1.5 h-1.5 rounded-full ${STATE_DOT[st] ?? STATE_DOT.stopped}`}
      />
      {t(st)}
    </span>
  );
}

export function AgentQueue({ agent: a }: { agent: Agent }) {
  const t = useTranslate();
  return (
    <div className="text-label">
      <span className="num text-ink-400">
        {t("{running} running · {queued} queued")
          .replace("{running}", String(a.running))
          .replace("{queued}", String(a.queued))}
      </span>
      {a.unknown > 0 && (
        <div className="text-fail mt-1">
          {t("{count} outcome unknown").replace("{count}", String(a.unknown))}
        </div>
      )}
      {a.parked > 0 && (
        <div className="text-ink-500 mt-1">
          {t("{count} parked").replace("{count}", String(a.parked))}
        </div>
      )}
    </div>
  );
}

export function AgentActivity({ agent: a }: { agent: Agent }) {
  const t = useTranslate();
  const stamp = activityTimeMs(a.last_activity);
  return (
    <div className="text-label text-ink-400">
      <span className="num">
        {stamp ? fmtTime(new Date(stamp).toISOString()) : t("No activity recorded")}
      </span>
      {a.stalled ? (
        <div className="text-warn mt-1">
          {t("Stalled · {time}").replace("{time}", () => fmtSilence(a.silent_secs ?? 0))}
        </div>
      ) : (a.silent_secs ?? 0) >= 60 ? (
        <div className="text-micro text-ink-500 mt-1">
          {t("Silent · {time}").replace("{time}", () => fmtSilence(a.silent_secs!))}
        </div>
      ) : null}
    </div>
  );
}
