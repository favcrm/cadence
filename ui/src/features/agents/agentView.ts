import type { Agent } from "../../lib/types";
import { agentIssueIds, type IssueIndex } from "../../lib/scope";

export const AGENT_FILTERS = [
  { value: "all", label: "All" },
  { value: "attention", label: "Needs attention" },
  { value: "working", label: "Working" },
  { value: "idle", label: "Idle" },
  { value: "stopped", label: "Stopped" },
  { value: "inbox", label: "Mailboxes" },
  { value: "other", label: "Other" },
] as const;
export type AgentFilter = (typeof AGENT_FILTERS)[number]["value"];

/** One classification drives counts, filtering and ordering. A mailbox's
 * queue is incoming mail, not evidence of a running worker. */
export function agentCategory(a: Agent): Exclude<AgentFilter, "all"> {
  if (
    a.fenced ||
    a.unknown > 0 ||
    a.stalled ||
    ["attention", "error", "waiting_input", "blocked_by_quota"].includes(
      a.state,
    ) ||
    a.quota?.state === "blocked" ||
    a.usage_limit?.state === "blocked"
  )
    return "attention";
  if (a.inbox || a.provider === "inbox") return "inbox";
  if (
    a.running > 0 ||
    a.queued > 0 ||
    ["busy", "running", "starting"].includes(a.state)
  )
    return "working";
  if (a.state === "idle" || a.state === "stopped") return a.state;
  return "other";
}

export function agentStatus(a: Agent): string {
  if (a.fenced) return "attention";
  if (a.inbox || a.provider === "inbox") return "inbox";
  if (
    a.state === "blocked_by_quota" ||
    a.quota?.state === "blocked" ||
    a.usage_limit?.state === "blocked"
  )
    return "quota blocked";
  if (a.state === "waiting_input") return "needs input";
  if (a.running > 0) return "busy";
  if (a.queued > 0) return "queued";
  return a.state || "unknown";
}

export function agentOrder(a: Agent): number {
  if (a.fenced) return 0;
  return { attention: 1, working: 2, idle: 3, stopped: 4, other: 5, inbox: 6 }[
    agentCategory(a)
  ];
}

/** Search only observation and exact issue bindings; never infer projects
 * from an alias or a message's free text. */
export function agentMatchesSearch(
  a: Agent,
  query: string,
  index: IssueIndex,
): boolean {
  const terms = query.toLowerCase().trim().split(/\s+/).filter(Boolean);
  const text = [
    a.alias,
    a.role,
    a.team_role,
    a.provider,
    a.group,
    a.model_reported,
    a.model_configured,
    a.model,
    ...agentIssueIds(a, index),
    ...(a.tasks ?? []).flatMap((t) => [t.title, t.job_title]),
    ...(a.running_messages ?? []).map((m) => m.summary),
    a.message?.summary,
  ]
    .filter(Boolean)
    .join(" ")
    .toLowerCase();
  return terms.every((term) => text.includes(term));
}
