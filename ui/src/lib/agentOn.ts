import type { Agent } from "./types";

/**
 * Issue ids an agent is working.
 *
 * The local daemon always sends `on`; a board served by a different store may
 * omit it (the hosted tenant did, and the trackers crashed iterating it:
 * CAD-1833). A missing list means "no bound issues", never a page error.
 */
export function agentOn(agent: Pick<Agent, "on">): string[] {
  return Array.isArray(agent.on) ? agent.on : [];
}
