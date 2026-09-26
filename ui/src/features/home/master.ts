import type { Agent, AgentsPayload, MasterState } from "../../lib/types";

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

// ---- CAD-551: the composer's `/` verbs and the working row ----

/** One `/` verb (CAD-551). `help` is composer-local — it never crosses
 * the board; the rest post to `/api/master/command`, whose own
 * allowlist is the daemon's set. `arg` is the autocomplete hint's
 * placeholder, `kind` splits reads from mutations for styling. */
export interface SlashCommand {
  name: string;
  arg?: string;
  blurb: string;
  kind: "read" | "set" | "act";
}

export const SLASH_COMMANDS: SlashCommand[] = [
  { name: "help", blurb: "What the / commands do", kind: "read" },
  { name: "state", blurb: "Session state — model, effort, session id", kind: "read" },
  { name: "stats", blurb: "Context usage — how much of the window is in use", kind: "read" },
  { name: "models", blurb: "Models the provider offers", kind: "read" },
  { name: "model", arg: "provider/id", blurb: "Set the model (bare: the provider's list)", kind: "set" },
  { name: "levels", blurb: "Thinking levels the provider offers", kind: "read" },
  { name: "effort", arg: "level", blurb: "Set the thinking level (bare: levels + current)", kind: "set" },
  { name: "compact", blurb: "Compact the session context now", kind: "act" },
  { name: "new", blurb: "Restart the provider session — fresh context", kind: "act" },
  { name: "stop", blurb: "Interrupt the running turn", kind: "act" },
];

/** `/name` or `/name arg…` — null when the draft is not a command. */
export function parseSlash(draft: string): { name: string; arg: string } | null {
  if (!draft.startsWith("/")) return null;
  const body = draft.slice(1);
  const split = body.indexOf(" ");
  const name = (split < 0 ? body : body.slice(0, split)).toLowerCase();
  if (!/^[a-z]+$/.test(name)) return null;
  return { name, arg: split < 0 ? "" : body.slice(split + 1).trim() };
}

/** Verbs whose name starts with `prefix` — the autocomplete rows. */
export function slashMatches(prefix: string): SlashCommand[] {
  return SLASH_COMMANDS.filter((c) => c.name.startsWith(prefix));
}

/**
 * Where the master's turn stands (CAD-551): `master_state`'s live turn
 * wins; the agents row covers a daemon that cannot answer (501) or a
 * refresh gap — `running` over `queued`, never both shown at once.
 * `submitting` is the composer's own POST still in flight (no stored
 * entry yet), `queued` the "behind a turn" wait.
 */
export type TurnState =
  | { kind: "idle" }
  | { kind: "submitting" }
  | { kind: "queued"; summary?: string | null; since?: number | null }
  | {
      kind: "working";
      message: string;
      summary?: string | null;
      since?: number | null;
      queued?: number;
      compacting?: boolean;
    };

export function turnState(
  master: MasterState | null | undefined,
  row: Agent | undefined,
  submitting: boolean,
): TurnState {
  if (master?.turn) {
    const t = master.turn;
    if (t.state === "working") {
      return {
        kind: "working",
        message: t.message,
        summary: t.summary,
        since: t.since,
        queued: master.queued ?? row?.queued,
        compacting: master.session?.compacting === true,
      };
    }
    return { kind: "queued", summary: t.summary, since: t.since };
  }
  // Fallbacks: the agents row's counters (works on a daemon without
  // master_state) then the composer's own in-flight send.
  if (row && row.running > 0 && row.message) {
    return {
      kind: "working",
      message: row.message.id,
      summary: row.message.summary,
      since: row.message.created ? Date.parse(row.message.created) / 1000 : null,
      queued: row.queued,
    };
  }
  if (row && row.queued > 0) return { kind: "queued" };
  if (submitting) return { kind: "submitting" };
  return { kind: "idle" };
}
