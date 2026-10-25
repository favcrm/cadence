import type { Agent, AgentsPayload } from "./types";

/**
 * Issue ids an agent is working.
 *
 * The local daemon always sends `on`; a board served by a different store may
 * omit it (the hosted tenant did, and the tracker crashed iterating it:
 * CAD-1833). A missing list means "no bound issues", never a page error.
 */
export function agentOn(agent: Pick<Agent, "on">): string[] {
  return Array.isArray(agent.on) ? agent.on : [];
}

const count = (value: unknown): number =>
  typeof value === "number" && Number.isFinite(value) ? value : 0;

/**
 * The full row shape the UI contracts on, from a server that may send less.
 * Board totals add these fields directly, so a missing counter is a NaN on
 * screen unless it becomes zero here.
 */
export function normalizeAgent(agent: Agent): Agent {
  return {
    ...agent,
    on: agentOn(agent),
    running: count(agent.running),
    queued: count(agent.queued),
    unknown: count(agent.unknown),
    parked: count(agent.parked),
    fenced: agent.fenced === true,
    inbox: agent.inbox ?? null,
  };
}

export function normalizeAgentsPayload(payload: AgentsPayload): AgentsPayload {
  return { ...payload, agents: (payload.agents ?? []).map(normalizeAgent) };
}
