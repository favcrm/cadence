import type { Agent } from "../../lib/types";
import { lifecycleOf } from "../agents/agentView";
import type { ConversationKind, ConversationRow, DeliveryState, Participant } from "./threadsClient";

/**
 * Pure view logic for the Threads page (CAD-818 / CAD-819 / CAD-820 /
 * CAD-821). No React, no fetching: the page wires these to the real
 * endpoints. Nothing here decides authority; the daemon does.
 */

export type ConversationFilter = "all" | "agents" | "operator" | "groups";

/** `?c=<id>` names the open conversation; `?kind=`, `?project=`, `?agent=`
 *  and `?system=1` are the list filters. Unknown values fall back to the
 *  default, never to an error. */
export interface ThreadsUrlState {
  conversation: string | null;
  kind: ConversationFilter;
  project: string | null;
  agent: string | null;
  includeSystem: boolean;
}

export function readThreadsUrl(search: string): ThreadsUrlState {
  const q = new URLSearchParams(search);
  const kind = q.get("kind");
  return {
    conversation: nonEmpty(q.get("c")),
    kind: kind === "agents" || kind === "operator" || kind === "groups" ? kind : "all",
    project: nonEmpty(q.get("project")),
    agent: nonEmpty(q.get("agent")),
    includeSystem: q.get("system") === "1",
  };
}

function nonEmpty(v: string | null): string | null {
  return v === null || v.trim() === "" ? null : v;
}

/** The search string for a state, dropping defaults so a link stays short. */
export function threadsUrlSearch(state: Partial<ThreadsUrlState>): string {
  const q = new URLSearchParams();
  if (state.conversation) q.set("c", state.conversation);
  if (state.kind && state.kind !== "all") q.set("kind", state.kind);
  if (state.project) q.set("project", state.project);
  if (state.agent) q.set("agent", state.agent);
  if (state.includeSystem) q.set("system", "1");
  const s = q.toString();
  return s ? `?${s}` : "";
}

/** The agent alias of the operator DM: its live non-operator participant. */
export function operatorDmAlias(row: ConversationRow): string | null {
  if (row.kind !== "operator") return null;
  const agent = row.participants.find((p) => p.kind === "agent" && !p.removed && p.alias);
  return agent?.alias ?? null;
}

/** The conversation's kind as the list and the timeline label it. */
export function kindFilterMatches(filter: ConversationFilter, kind: ConversationKind): boolean {
  if (filter === "all") return true;
  if (filter === "groups") return kind === "group";
  if (filter === "operator") return kind === "operator";
  return kind === "pair";
}

/** A participant's display name. A removed agent keeps its alias with
 *  "(removed)"; a legacy row with no recorded sender is "Unknown sender". */
export function participantLabel(
  p: Participant | null | undefined,
  t: (english: string) => string = (english) => english,
): string {
  if (!p) return t("Unknown sender");
  if (p.kind === "unknown" || (p.uid === null && p.kind !== "operator" && p.kind !== "daemon")) {
    return t("Unknown sender");
  }
  if (p.kind === "operator") return t("You");
  if (p.kind === "daemon") return t("System");
  const name = p.alias ?? t("agent");
  return p.removed ? `${name} ${t("(removed)")}` : name;
}

/** The conversation title: the participants, not an id. */
export function conversationTitle(
  row: ConversationRow,
  t: (english: string) => string = (english) => english,
): string {
  const names = row.participants.map((p) => participantLabel(p, t));
  return names.length > 0 ? names.join(" ↔ ") : t("Conversation");
}

/** Delivery is a stage, never a claim. Queued is not delivered or seen. */
const DELIVERY_LABEL: Record<DeliveryState, string> = {
  pending: "Pending",
  queued: "Queued",
  available: "Available",
  refused: "Refused",
  claimed: "Claimed",
  running: "Running",
  completed: "Completed",
  failed: "Failed",
  unknown: "Unknown",
  interrupted: "Interrupted",
};

export function deliveryLabel(
  state: DeliveryState | string,
  t: (english: string) => string = (english) => english,
): string {
  return t(DELIVERY_LABEL[state as DeliveryState] ?? "Unknown");
}

/** The composer's honest send expectation for a DM recipient, from the
 *  roster's lifecycle. Queuing never confirms receipt; a stopped, dead,
 *  fenced or quota-blocked recipient is never implied reachable, and a
 *  mailbox is a notice sink with no session. A live running or idle
 *  session is the only "can deliver" claim the observation supports. */
export function dmExpectation(a: Agent | undefined): string {
  if (!a)
    return "Messages queue for this agent's next turn. Sending does not change its assignment.";
  const lifecycle = lifecycleOf(a);
  if (lifecycle === "dead-holding")
    return "This agent is not running but still holds work — the message queues; it is not received.";
  if (a.dead || a.fenced)
    return "This agent is not available — the message queues; it is not received.";
  if (a.inbox || a.provider === "inbox")
    return "This recipient is a mailbox — a post is recorded, never delivered to a session.";
  if (a.state === "stopped")
    return "This agent is stopped — the message queues; it is not received.";
  if (
    a.state === "blocked_by_quota" ||
    a.quota?.state === "blocked" ||
    a.usage_limit?.state === "blocked"
  )
    return "This agent's provider allowance is blocked — the message queues; it is not received.";
  if (lifecycle === "active")
    return "Will queue for the next turn. Sending does not interrupt the current task.";
  if (a.state === "idle")
    return "This session is open — the message can start its next turn.";
  if (lifecycle === "idle-holding")
    return "This agent is parked on a claim — the message queues; it is not received.";
  return "Messages queue for this agent's next turn. Sending does not change its assignment.";
}
