import type { Agent } from "../../lib/types";
import { agentIssueIds, type IssueIndex } from "../../lib/scope";

export const AGENT_FILTERS = [
  { value: "current", label: "Current" },
  { value: "all", label: "All" },
  { value: "holding", label: "Holding" },
  { value: "attention", label: "Needs attention" },
  { value: "working", label: "Working" },
  { value: "idle", label: "Idle" },
  { value: "stopped", label: "Stopped" },
  { value: "inbox", label: "Mailboxes" },
  { value: "other", label: "Other" },
] as const;
export type AgentFilter = (typeof AGENT_FILTERS)[number]["value"];

type AgentCategory = Exclude<AgentFilter, "all" | "current" | "holding">;

/** One classification drives counts, filtering and ordering. A mailbox's
 * queue is incoming mail, not evidence of a running worker. */
export function agentCategory(a: Agent): AgentCategory {
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

export function agentMatchesFilter(a: Agent, filter: AgentFilter): boolean {
  const category = agentCategory(a);
  if (filter === "all") return true;
  if (filter === "holding")
    return (
      agentHoldsWorkButDead(a) || lifecycleOf(a) === "idle-holding"
    );
  // A dead agent still bound to active work is current work, not history.
  if (filter === "current")
    return (
      agentHoldsWorkButDead(a) ||
      (category !== "stopped" && category !== "inbox")
    );
  return category === filter;
}

/** CAD-1320: the roster's critical signal — the endpoint is gone (dead,
 * fenced or stopped) while the agent still holds a task, an issue binding
 * or undelivered mail. Everything else is a live agent parked on work or
 * an ordinary row. */
export function agentHoldsWorkButDead(a: Agent): boolean {
  const live =
    !a.dead &&
    !a.fenced &&
    a.state !== "stopped" &&
    a.state !== "dead";
  const holds =
    (a.tasks?.length ?? 0) > 0 || a.on.length > 0 || a.queued > 0;
  return !live && holds;
}

export type AgentLifecycle =
  | "dead-holding"
  | "idle-holding"
  | "stale-inbox"
  | "active"
  | "normal";

/** The catalog's section bucket. Dead-holding outranks everything; a
 * mailbox with unread mail is a stale inbox; a running agent is working;
 * a live agent still bound to a claim is parked on it. */
export function lifecycleOf(a: Agent): AgentLifecycle {
  if (agentHoldsWorkButDead(a)) return "dead-holding";
  const category = agentCategory(a);
  if (category === "inbox") return a.queued > 0 ? "stale-inbox" : "normal";
  if (category === "working") return "active";
  if ((a.tasks?.length ?? 0) > 0 || a.on.length > 0) return "idle-holding";
  return "normal";
}

export type LifecycleTone = "fail" | "warn" | "info" | "ok" | "ink";

export const LIFECYCLE_SECTIONS: {
  key: AgentLifecycle;
  title: string;
  tone: LifecycleTone;
}[] = [
  { key: "dead-holding", title: "Dead · holding work", tone: "fail" },
  { key: "idle-holding", title: "Idle · holding claim", tone: "warn" },
  { key: "stale-inbox", title: "Stale inboxes", tone: "info" },
  { key: "active", title: "Working", tone: "ok" },
  { key: "normal", title: "Roster", tone: "ink" },
];

/** The card's bottom-left badge: lifecycle tone first, then the
 * category's own urgency for ordinary rows. */
export function lifecycleBadge(a: Agent): {
  label: string;
  tone: LifecycleTone;
} {
  const lifecycle = lifecycleOf(a);
  if (lifecycle === "dead-holding") {
    const state = a.fenced ? "fenced" : a.dead ? "dead" : a.state;
    return { label: `${state || "dead"} · holding`, tone: "fail" };
  }
  if (lifecycle === "idle-holding")
    return { label: `${agentStatus(a)} · holding`, tone: "warn" };
  if (lifecycle === "stale-inbox")
    return { label: "stale inbox", tone: "info" };
  const status = agentStatus(a);
  if (lifecycle === "active") return { label: status, tone: "ok" };
  if (["attention", "error", "quota blocked"].includes(status))
    return { label: status, tone: "fail" };
  if (status === "needs input") return { label: status, tone: "warn" };
  if (status === "busy" || status === "queued")
    return { label: status, tone: "info" };
  if (status === "idle") return { label: status, tone: "ok" };
  return { label: status, tone: "ink" };
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
  const order: Record<AgentCategory, number> = {
    attention: 1,
    working: 2,
    idle: 3,
    stopped: 4,
    other: 5,
    inbox: 6,
  };
  return order[agentCategory(a)];
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
