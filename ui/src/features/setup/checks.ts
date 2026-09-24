/**
 * The setup wizard's rules over `GET /api/setup` — pure, so they are
 * unit-tested in plain node (tests/setupChecks.test.ts).
 *
 * The endpoint runs `cadence setup`'s checks detect only and returns each
 * as `{check, status, detail, fix, group}`. This module decides which
 * step shows a check, how its status reads, and whether setup is finished
 * enough that Home stops linking here.
 */

export type SetupStatus = "ok" | "created" | "started" | "missing" | "failed" | "unknown";
export type SetupGroup = "environment" | "provider" | "master";

export interface SetupCheck {
  check: string;
  status: SetupStatus;
  detail: string;
  /** A copy-paste command, null when there is nothing to run. */
  fix: string | null;
  group: SetupGroup;
}

export interface SetupReport {
  checks: SetupCheck[];
  /** Epoch ms the checks ran (the board reuses one run for a minute). */
  checked_at: number;
  detect_only: boolean;
  /** This request ran the checks (false: answered from the last run). */
  ran_now: boolean;
  /** Age of the run when answered. */
  age_ms: number;
  /** How long until a re-check runs the probes again (0: now). */
  recheck_in_ms: number;
}

/** Checks Home insists on when this build can fix them. */
export const REQUIRED_CHECKS = ["state_dir", "tracker", "daemon", "master"] as const;

/** CLIs that can be the master in the MVP — one of them must be ready. */
export const MASTER_CLIS = ["claude", "codex"] as const;

const LABELS: Record<string, string> = {
  state_dir: "State directory",
  tracker: "Tracker",
  skill: "Cadence skill",
  daemon: "Daemon",
  ui: "Board",
  login: "Operator login link",
  master: "Master agent",
  claude: "Claude Code",
  codex: "Codex",
  "cursor-agent": "Cursor agent",
  devin: "Devin",
  pi: "Pi",
};

export function checkLabel(name: string): string {
  return LABELS[name] ?? name;
}

export function isReady(c: SetupCheck): boolean {
  return c.status === "ok" || c.status === "created" || c.status === "started";
}

export type Tone = "ok" | "warn" | "fail" | "muted";

/** The chip a check shows: a short word and its colour. */
export function statusChip(c: SetupCheck): { label: string; tone: Tone } {
  if (isReady(c)) return { label: c.group === "provider" ? "signed in" : "ready", tone: "ok" };
  if (c.status === "failed") return { label: "failed", tone: "fail" };
  if (c.status === "unknown") return { label: "unknown", tone: "muted" };
  if (c.group === "provider") {
    return c.detail.includes("not on PATH")
      ? { label: "not installed", tone: "muted" }
      : { label: "sign in", tone: "warn" };
  }
  return { label: "missing", tone: "warn" };
}

export function inGroup(report: SetupReport | null, group: SetupGroup): SetupCheck[] {
  return report ? report.checks.filter((c) => c.group === group) : [];
}

/**
 * What still blocks a first run: each required check that is not ready
 * and that this build can act on, plus "a master CLI" when neither
 * Claude nor Codex is signed in. A check with no fix (`master` in a
 * build without `master start`) has no command to run, so it can never
 * hold the Home link open.
 */
export function missingRequired(report: SetupReport): string[] {
  const by = new Map(report.checks.map((c) => [c.check, c]));
  const out: string[] = REQUIRED_CHECKS.filter((name) => {
    const c = by.get(name);
    return !c || (!isReady(c) && c.fix !== null);
  });
  const cliReady = MASTER_CLIS.some((name) => {
    const c = by.get(name);
    return c !== undefined && isReady(c);
  });
  if (!cliReady) out.push("master CLI");
  return out;
}

// ---------- the Home link, dismissible per browser ----------

const NUDGE_KEY = "cadence-setup-nudge-dismissed";

type Get = Pick<Storage, "getItem">;
type Put = Pick<Storage, "setItem">;

export function readNudgeDismissed(storage: Get | undefined): boolean {
  try {
    return storage?.getItem(NUDGE_KEY) === "1";
  } catch {
    return false;
  }
}

/** Storage can throw (private windows): then the dismissal lasts the page. */
export function writeNudgeDismissed(storage: Put | undefined): void {
  try {
    storage?.setItem(NUDGE_KEY, "1");
  } catch {
    // Unstorable — the caller's state still hides the link for now.
  }
}

export function browserStorage(): Storage | undefined {
  try {
    return typeof window === "undefined" ? undefined : window.localStorage;
  } catch {
    return undefined;
  }
}
