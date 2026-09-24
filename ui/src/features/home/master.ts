import type { AgentsPayload } from "../../lib/types";

/** The master's alias — `cadence master start` registers it (CAD-339). */
export const MASTER = "master";
export const START_COMMAND = "cadence master start";

export type MasterStatus =
  /** Agents not loaded yet. */
  | { kind: "unknown" }
  /** The daemon is down — nothing can be sent. */
  | { kind: "offline" }
  /** No agent named `master` is registered. */
  | { kind: "absent" }
  /** Registered but stopped (or its process is gone). */
  | { kind: "stopped"; label: string }
  | { kind: "running"; state: string };

/**
 * Where the master stands, from the agents payload. `threadMissing` is
 * the thread endpoint's 404 — the daemon knows no such agent — which
 * wins over a stale agents list.
 */
export function masterStatus(agents: AgentsPayload | null, threadMissing: boolean): MasterStatus {
  if (threadMissing) return { kind: "absent" };
  if (!agents) return { kind: "unknown" };
  if (agents.daemon === "unreachable") return { kind: "offline" };
  const row = agents.agents.find((a) => a.alias === MASTER);
  if (!row) return { kind: "absent" };
  if (row.state === "stopped" || row.dead) {
    return { kind: "stopped", label: row.state_label ?? (row.dead ? "its process is gone" : "stopped") };
  }
  return { kind: "running", state: row.state };
}

/** Why the composer is disabled, or null when it may send. `block` is
 *  why this board cannot write at all (read-only, not signed in). */
export function composerBlock(block: string | null, status: MasterStatus): string | null {
  if (block) return `${block} Messages cannot be sent until then.`;
  switch (status.kind) {
    case "unknown":
      return "Checking whether the master is running…";
    case "offline":
      return "The daemon is not reachable — messages send once it is back.";
    case "absent":
      return `The master is not started — run \`${START_COMMAND}\` first.`;
    case "stopped":
      return `The master is stopped (${status.label}) — run \`${START_COMMAND}\` to bring it back.`;
    case "running":
      return null;
  }
}
