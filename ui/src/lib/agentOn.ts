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

/**
 * The row a stream event carries into the cache. A delete is `{ op, id }` with
 * no row at all, so only an upsert is normalized; passing the tombstone through
 * keeps `patchRows` free to remove it.
 */
export function agentRowForEvent(
  op: "upsert" | "delete",
  row: Agent | undefined,
): Agent | undefined {
  return op === "upsert" ? normalizeAgent(row as Agent) : row;
}

export function normalizeAgentsPayload(payload: AgentsPayload): AgentsPayload {
  // A missing roster is a malformed response, not an empty workspace: throwing
  // keeps the resource's last-good data instead of publishing "no agents".
  if (!Array.isArray(payload.agents)) throw new Error("agents payload is not a roster");
  return { ...payload, agents: payload.agents.map(normalizeAgent) };
}
