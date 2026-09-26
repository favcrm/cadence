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

// ---- Command results as fields (CAD-551 r2) ----

/** Wire keys → the label a command card shows. */
const KV_LABELS: Record<string, string> = {
  model: "model",
  thinkingLevel: "effort",
  sessionId: "session",
  contextUsage: "context",
  pendingMessageCount: "pending",
  isStreaming: "streaming",
  messageCount: "messages",
  was: "was",
};

/** The order familiar fields take on a card; the rest follow as sent. */
const KV_ORDER = [
  "model",
  "effort",
  "session",
  "context",
  "pending",
  "streaming",
  "messages",
  "was",
];

const shortJson = (v: unknown): string => {
  const s = JSON.stringify(v);
  return s.length > 90 ? `${s.slice(0, 87)}…` : s;
};

const num = (v: unknown): number | null =>
  typeof v === "number" && Number.isFinite(v) ? v : null;

const fmtNum = (n: number): string => n.toLocaleString("en-US");

/** One field's display value — objects fold to a compact read. */
function kvValue(v: unknown): string {
  if (v == null) return "—";
  if (typeof v !== "object") return String(v);
  if (Array.isArray(v)) return v.length ? v.map(kvValue).join(" · ") : "—";
  const o = v as Record<string, unknown>;
  if (typeof o.name === "string" && typeof o.id === "string") return `${o.name} (${o.id})`;
  const used = num(o.usedTokens ?? o.used_tokens ?? o.tokens);
  const max = num(o.maxTokens ?? o.max_tokens ?? o.contextWindow ?? o.window);
  if (used != null && max != null && max > 0) {
    return `${fmtNum(used)} / ${fmtNum(max)} (${Math.round((used / max) * 100)}%)`;
  }
  if (used != null) return fmtNum(used);
  const pct = num(o.percent ?? o.percentUsed ?? o.used_percent);
  if (pct != null) return `${Math.round(pct)}%`;
  return shortJson(v);
}

/**
 * A command result as `[label, value]` rows for the card's field list
 * (CAD-551 r2): objects list their fields — known wire names relabelled
 * (`sessionId` → `session`), familiar ones ordered first — and an array
 * (a `/models` list) lists one row per element keyed on id/name.
 * `[]` when the value has no fields to show — the card falls back to
 * raw JSON.
 */
export function kvRows(v: unknown): [string, string][] {
  const rows: [string, string][] = [];
  if (Array.isArray(v)) {
    for (const el of v) {
      if (el && typeof el === "object") {
        const o = el as Record<string, unknown>;
        const key = typeof o.id === "string" ? o.id : typeof o.name === "string" ? o.name : `${rows.length + 1}`;
        rows.push([key, kvValue(o)]);
      } else {
        rows.push([`${rows.length + 1}`, kvValue(el)]);
      }
    }
    return rows;
  }
  if (!v || typeof v !== "object") return rows;
  const obj = v as Record<string, unknown>;
  const byLabel = new Map<string, string>();
  const rest: string[] = [];
  for (const k of Object.keys(obj)) {
    const label = KV_LABELS[k];
    if (label === undefined) rest.push(k);
    else if (!byLabel.has(label)) byLabel.set(label, k);
  }
  const keys = [
    ...KV_ORDER.filter((l) => byLabel.has(l)).map((l) => byLabel.get(l) as string),
    ...rest,
  ];
  for (const k of keys) rows.push([KV_LABELS[k] ?? k, kvValue(obj[k])]);
  return rows;
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
    const c = row.message.created;
    const since =
      typeof c === "number" ? c : c ? Date.parse(c) / 1000 : null;
    return {
      kind: "working",
      message: row.message.id,
      summary: row.message.summary,
      since: since !== null && Number.isNaN(since) ? null : since,
      queued: row.queued,
    };
  }
  if (row && row.queued > 0) return { kind: "queued" };
  if (submitting) return { kind: "submitting" };
  return { kind: "idle" };
}

// ---- CAD-574: the model/effort dropdowns ----

import type { MasterModels } from "../../lib/types";

/** One row of the model dropdown: the id `master_command model` takes,
 *  the display label, and the catalog's cost tier. */
export interface ModelOption {
  id: string;
  label: string;
  /** `cost_tier` verbatim — Free / Low cost / Paid / unknown. */
  cost: string;
  /** Roles the model may serve (`allowed_for`); empty when unlisted. */
  roles: string[];
}

export interface MasterModelsView {
  models: ModelOption[];
  efforts: string[];
  /** The session's current model id / effort level, when it says. */
  model: string | null;
  effort: string | null;
}

/**
 * `GET /api/master/models` → the dropdowns' rows (CAD-574). Tolerant of
 * a partial payload — an entry without an id cannot be chosen and is
 * dropped, a missing cost tier renders `unknown`. Tested in
 * tests/masterModels.test.ts.
 */
export function masterModelsOptions(m: MasterModels | null | undefined): MasterModelsView {
  const str = (v: unknown): string | null =>
    typeof v === "string" && v.trim() !== "" ? v : null;
  const models: ModelOption[] = [];
  for (const e of m?.models ?? []) {
    if (!e || typeof e !== "object") continue;
    const id = str(e.id);
    if (!id) continue;
    models.push({
      id,
      label: str(e.label) ?? id,
      cost: str(e.cost_tier) ?? "unknown",
      roles: Array.isArray(e.allowed_for) ? e.allowed_for.filter((r): r is string => !!str(r)) : [],
    });
  }
  const efforts = (m?.efforts ?? []).filter((e): e is string => !!str(e));
  return {
    models,
    efforts,
    model: str(m?.current?.model),
    effort: str(m?.current?.effort),
  };
}

/**
 * A chip pick has landed once the session reports it. The reported id
 * may be the bare `model-2` while the list names `fake/model-2`
 * (provider/id) — the pick list is `master_models`' vocabulary, the
 * state read the provider's, so a model pick also lands when the
 * reported id is the pick's tail. Effort levels share one vocabulary.
 */
export function pickLanded(
  kind: "model" | "effort",
  pick: string,
  reported: string | null | undefined,
): boolean {
  if (!reported) return false;
  if (reported === pick) return true;
  return kind === "model" && pick.endsWith(`/${reported}`);
}
