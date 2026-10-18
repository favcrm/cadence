import type {
  HealthReason,
  HealthState,
  IssueCard,
  IssueHistoryEntry,
  StageMove,
  WorkBlock,
  WorkProgress,
  WorkStage,
} from "../../lib/types";

/**
 * The Projects screen's epic and milestone views (CAD-432) as plain data,
 * so the rules — which health reads how, which stage moves a viewer is
 * offered — are unit-tested in node (tests/projectsWork.test.ts) and the
 * components only lay them out.
 */

export type Tone = "ok" | "warn" | "fail" | "muted";

/** Chip classes per tone — theme tokens, so both themes follow. */
export const TONE_CHIP: Record<Tone, string> = {
  ok: "bg-ok/15 text-ok",
  warn: "bg-warn/10 text-warn",
  fail: "bg-fail/10 text-fail",
  muted: "bg-ink-800 text-ink-400",
};

/** Bar fill per tone. */
export const TONE_BAR: Record<Tone, string> = {
  ok: "bg-ok",
  warn: "bg-warn",
  fail: "bg-fail",
  muted: "bg-accent",
};

export interface HealthView {
  state: HealthState | "unknown";
  label: string;
  tone: Tone;
  /** `3 days in 'build' (limit 5)` — null when the entry time is unknown. */
  timing: string | null;
  /** Each reason with its next action, as the row lists them. */
  reasons: { detail: string; next: string; owner: string | null }[];
}

const HEALTH: Record<HealthState, { tone: Tone }> = {
  on_track: { tone: "ok" },
  at_risk: { tone: "warn" },
  stalled: { tone: "fail" },
};
type Translate = (english: string) => string;
const identity: Translate = (english) => english;

/** How a health block reads: label, tone, time in stage, reasons + next actions. */
export function healthView(
  health:
    | { state: string; reasons?: HealthReason[]; days_in_stage?: number | null; limit_days?: number }
    | null
    | undefined,
  stage?: string | null,
  t: Translate = identity,
): HealthView {
  const known = health && health.state in HEALTH ? HEALTH[health.state as HealthState] : null;
  const days = health?.days_in_stage;
  const timing =
    typeof days === "number" && stage
      ? `${days} ${t(days === 1 ? "day" : "days")} ${t("in")} ${stage}${
          typeof health?.limit_days === "number"
            ? ` ${t("(limit {limit})").replace("{limit}", String(health.limit_days))}`
            : ""
        }`
      : null;
  return {
    state: known ? (health!.state as HealthState) : "unknown",
    label: t(
      known && health?.state === "on_track"
        ? "on track"
        : known && health?.state === "at_risk"
          ? "at risk"
          : known
            ? "stalled"
            : "health unknown",
    ),
    tone: known?.tone ?? "muted",
    timing,
    reasons: (health?.reasons ?? []).map((r) => ({
      detail: r.detail,
      next: r.next,
      owner: r.owner ?? null,
    })),
  };
}

const RANK: Record<string, number> = { on_track: 0, at_risk: 1, stalled: 2 };

/** The worse of two health states (unknown counts as on track). */
export function worseHealth(a: string, b: string): string {
  return (RANK[b] ?? 0) > (RANK[a] ?? 0) ? b : a;
}

export interface ProgressView {
  done: number;
  total: number;
  /** Whole percent; null when nothing is weighted yet. */
  percent: number | null;
  label: string;
}

/** Size-weighted progress (S=1, M=3, L=8) as the bar shows it. */
export function progressView(p: WorkProgress | null | undefined, t: Translate = identity): ProgressView {
  const done = p?.done_weight ?? 0;
  const total = p?.total_weight ?? 0;
  const percent = total > 0 ? Math.round((done / total) * 100) : null;
  return {
    done,
    total,
    percent,
    label: total > 0 ? t("{done}/{total} weight").replace("{done}", String(done)).replace("{total}", String(total)) : t("no tasks yet"),
  };
}

/** Who may move stages here. */
export interface Viewer {
  readOnly: boolean;
  /** Passed the board's operator proof (`/api/meta` → `operator`).
   *  CAD-1193: `null` means the proof could not answer — access is
   *  still being checked. Unknown is never shown as signed out and
   *  grants nothing: every consumer reads `=== true` as the only yes. */
  operator: boolean | null;
  /** CAD-1193: when `operator` is `null`, `access` says which unknown
   *  it is — `"checking"` while the first probe is still in flight,
   *  `"unavailable"` once a completed probe failed or the server
   *  could not answer. Absent/`null` reads as the unknown the
   *  `operator` value already carries; it never grants anything and
   *  never marks a signed-out or non-operator viewer. */
  access?: "checking" | "unavailable" | null;
  /** CAD-1193: a bounded retry of the access check itself — wired to
   *  the real metadata refresh, never a reload or a sign-in. */
  onRetryAccess?: () => void;
  /** CAD-1537: a proven non-operator — a signed-in `member` session or a
   *  viewer of a `--read-only` board — who may load an installed app's
   *  receipts and mount its screen read-only. Never set while `operator`
   *  is `null`; the daemon decides what each read-only mount may call. */
  screenReader?: boolean;
}

/**
 * The stage moves this viewer is offered. The server lists the legal
 * moves (`work.stage.moves`, the writer's own `check_move`); on top:
 *
 * - only forward one step or back — anything else is dropped, whatever
 *   the payload says;
 * - an operator-only move is shown only to the operator;
 * - the board relays every move over its own connection, which the
 *   daemon attributes to the operator, so the board route demands the
 *   operator for every move: a viewer who is not the proven operator
 *   (an agent, an unprovable caller) and a read-only board are offered
 *   none. Agents move stages with `cadence issue epic stage`.
 */
export function visibleMoves(stage: WorkStage | null | undefined, viewer: Viewer): StageMove[] {
  if (!stage?.moves || viewer.readOnly || !viewer.operator) return [];
  const at = stage.stages.indexOf(stage.id);
  return stage.moves.filter((m) => {
    const to = stage.stages.indexOf(m.to);
    if (to === -1 || to === at) return false;
    if (m.forward) return at !== -1 && to === at + 1;
    return at === -1 || to < at;
  });
}

/** Why no move is offered — the line shown in place of the buttons. */
export function noMoveReason(stage: WorkStage | null | undefined, viewer: Viewer, t: Translate = identity): string | null {
  if (!stage) return null;
  if (!stage.moves) return t("This board's server does not list stage moves.");
  if (stage.moves.length === 0) {
    return t(stage.source === "plan" ? "The plan decides this stage until it is approved." : "No stage move is open.");
  }
  if (viewer.readOnly) return t("The board is read-only.");
  if (viewer.operator === null) {
    return t(
      viewer.access === "unavailable"
        ? "The access check could not confirm this session — stage moves stay off until it answers."
        : "Checking whether this session may move stages…",
    );
  }
  if (!viewer.operator) {
    return t("Stage moves on the board are the operator's. Agents move a stage with `cadence issue epic stage`.");
  }
  return null;
}

/** The server's cap on a stage note, in UTF-8 bytes (`claim::NOTE_MAX`). */
export const NOTE_MAX_BYTES = 500;

/** A note's size as the server counts it — bytes, not characters. */
export function noteBytes(note: string): number {
  return new TextEncoder().encode(note.trim()).length;
}

/** One stage-history row, newest first. */
export interface StageEvent {
  at: string;
  by: string;
  what: string;
  note: string | null;
}

/** The stage moves (and plan decisions, which set the first stages) in a history. */
export function stageEvents(history: IssueHistoryEntry[]): StageEvent[] {
  return history
    .filter((e) => e.kind === "stage" || e.kind === "plan")
    .map((e) => ({
      at: e.at,
      by: e.by,
      what: e.kind === "stage" && e.from && e.to ? `${e.from} → ${e.to}` : e.summary,
      note: e.note ?? null,
    }));
}

/** A project's epics (effective type `epic`), in board order. */
export function epicsOf(cards: IssueCard[], project: string): (IssueCard & { work: WorkBlock })[] {
  return cards.filter(
    (c): c is IssueCard & { work: WorkBlock } => c.project === project && c.work?.type === "epic",
  );
}

/** An epic's children, in board order. */
export function childrenOf(cards: IssueCard[], epic: string): IssueCard[] {
  return cards.filter((c) => c.parent === epic);
}

/** `build` → `2/5 build`: the stage's place in the project's list. */
export function stageLabel(stage: WorkStage | null | undefined, t: Translate = identity): string {
  if (!stage) return t("no stage");
  const i = stage.stages.indexOf(stage.id);
  return i === -1 ? stage.id : `${stage.id} ${i + 1}/${stage.stages.length}`;
}
