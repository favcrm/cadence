import {
  missingRequired,
  readNudgeDismissed,
  statusChip,
  writeNudgeDismissed,
  type SetupCheck,
  type SetupReport,
} from "../src/features/setup/checks";

function equal(actual: unknown, expected: unknown, what: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${what}: expected ${e}, got ${a}`);
}

function check(check: string, status: SetupCheck["status"], group: SetupCheck["group"], detail = ""): SetupCheck {
  return { check, status, detail, fix: status === "ok" ? null : `fix ${check}`, group };
}

function report(checks: SetupCheck[]): SetupReport {
  return { checks, checked_at: 0, detect_only: true, ran_now: true, age_ms: 0, recheck_in_ms: 5000 };
}

const ready = [
  check("state_dir", "ok", "environment"),
  check("tracker", "ok", "environment"),
  check("skill", "missing", "environment"),
  check("daemon", "ok", "environment"),
  check("ui", "ok", "environment"),
  check("login", "unknown", "environment"),
  check("claude", "ok", "provider"),
  check("codex", "missing", "provider", "codex 0.1; not signed in"),
  check("pi", "missing", "provider", "not on PATH"),
  check("master", "ok", "master"),
];
equal(missingRequired(report(ready)), [], "optional checks (skill, login, pi) never block");

const fresh = ready.map((c) =>
  ["daemon", "master", "claude"].includes(c.check) ? { ...c, status: "missing" as const, fix: `fix ${c.check}` } : c,
);
equal(missingRequired(report(fresh)), ["daemon", "master", "master CLI"], "required and a master CLI");
equal(missingRequired(report([])), ["state_dir", "tracker", "daemon", "master", "master CLI"], "no checks → all missing");
const unknownDaemon = ready.map((c) =>
  c.check === "daemon" ? { ...c, status: "unknown" as const, fix: "cadence daemon start" } : c,
);
equal(missingRequired(report(unknownDaemon)), ["daemon"], "unknown is not ready");
const piOnly = ready.map((c) =>
  c.check === "claude" ? { ...c, status: "missing" as const } : c.check === "pi" ? { ...c, status: "ok" as const } : c,
);
equal(missingRequired(report(piOnly)), ["master CLI"], "pi alone is no master in the MVP");

// A build without `master start`: master is missing with no fix — nothing
// to run, so it never holds the Home link open.
const noVerb = ready.map((c) => (c.check === "master" ? { ...c, status: "missing" as const, fix: null } : c));
equal(missingRequired(report(noVerb)), [], "master with no fix is not actionable");
// The verb exists: master missing with `master start` stays required.
const withVerb = ready.map((c) =>
  c.check === "master" ? { ...c, status: "missing" as const, fix: "cadence master start" } : c,
);
equal(missingRequired(report(withVerb)), ["master"], "master with a fix is required");

equal(statusChip(ready[6]).label, "signed in", "provider ok");
equal(statusChip(ready[7]).label, "sign in", "provider installed, signed out");
equal(statusChip(ready[8]).label, "not installed", "provider absent");
equal(statusChip(ready[2]).label, "missing", "environment missing");
equal(statusChip(check("tracker", "failed", "environment")).tone, "fail", "failed");

const store = new Map<string, string>();
const storage = {
  getItem: (k: string) => store.get(k) ?? null,
  setItem: (k: string, v: string) => void store.set(k, v),
};
equal(readNudgeDismissed(storage), false, "not dismissed");
writeNudgeDismissed(storage);
equal(readNudgeDismissed(storage), true, "dismissed sticks");
const blocked = {
  getItem: (): string | null => {
    throw new Error("blocked");
  },
  setItem: () => {
    throw new Error("blocked");
  },
};
equal(readNudgeDismissed(blocked), false, "blocked storage reads as not dismissed");
writeNudgeDismissed(blocked); // must not throw
equal(readNudgeDismissed(undefined), false, "no storage");

console.log("setup checks passed");
