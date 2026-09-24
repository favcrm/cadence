import {
  masterNeedsUnmet,
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
  check("master_login", "ok", "master"),
];
equal(missingRequired(report(ready)), [], "optional checks (skill, login, pi) never block");

const fresh = ready.map((c) =>
  ["daemon", "master", "claude"].includes(c.check) ? { ...c, status: "missing" as const, fix: `fix ${c.check}` } : c,
);
equal(missingRequired(report(fresh)), ["daemon", "master", "master CLI"], "required and a master CLI");
equal(
  missingRequired(report([])),
  ["state_dir", "tracker", "daemon", "master", "master_login", "master CLI"],
  "no checks → all missing",
);
// The master's own login (CAD-439) is required: without it the master
// cannot authenticate — `--copy-login` resolves to the same `ok`.
const noLogin = ready.map((c) =>
  c.check === "master_login" ? { ...c, status: "missing" as const, fix: "CLAUDE_CONFIG_DIR=… claude auth login" } : c,
);
equal(missingRequired(report(noLogin)), ["master_login"], "the master's own login is required");
// A host that cannot confine the master: `master_login` reports ok (the
// master uses the operator's login) and the offer carries the
// `--unconfined` command with its warning — nothing master-side blocks.
const unconfined = report(
  ready.map((c) =>
    c.check === "master_login"
      ? {
          ...c,
          status: "ok" as const,
          detail:
            "this host cannot confine the master — `master start --unconfined` runs it on " +
            "your own Claude login, with no filesystem sandbox: it can read and write your files",
        }
      : c,
  ),
);
unconfined.master = {
  providers: [
    {
      bin: "claude",
      ready: true,
      start: "cadence master start --unconfined --provider claude",
      warning:
        "the master runs UNCONFINED: no filesystem sandbox on this host, so it can read and " +
        "write your files (ssh keys, forge logins, every repo) — its Bash allowlist is the only limit",
    },
  ],
};
equal(missingRequired(unconfined), [], "unconfined: master_login ok, the offer carries the command");
const offer = unconfined.master!.providers[0];
equal(offer.start!.includes("--unconfined"), true, "the offer names --unconfined");
equal(offer.warning!.includes("UNCONFINED"), true, "the offer carries the risk warning");
equal(masterNeedsUnmet(unconfined), false, "a ready-to-offer master is not prerequisite-blocked");
// While `master` waits on `tracker` the offer card hides — the step
// shows one command for starting the master, not two (N4).
const trackerMissing = report(
  ready.map((c) =>
    c.check === "tracker"
      ? { ...c, status: "missing" as const, fix: "cadence issue init" }
      : c.check === "master"
        ? { ...c, status: "missing" as const, fix: "cadence master start", detail: "needs `tracker` first" }
        : c,
  ),
);
trackerMissing.master = { providers: [{ bin: "claude", ready: true, start: null }] };
equal(masterNeedsUnmet(trackerMissing), true, "needs-blocked master hides the offer card");
equal(masterNeedsUnmet(report(ready)), false, "a ready master is not blocked");
// Once the offer carries the start command the check's own fix is
// absorbed by it — `master` still blocks Home until the command runs.
const offerFix = report(
  ready.map((c) => (c.check === "master" ? { ...c, status: "missing" as const, fix: null } : c)),
);
offerFix.master = { providers: [{ bin: "claude", ready: true, start: "cadence master start --provider claude" }] };
equal(missingRequired(offerFix), ["master"], "the offer's command keeps master required");
equal(masterNeedsUnmet(offerFix), false, "a plain missing master (files absent) does not hide the card");
// Codex signed in but not master-capable (the board's offer list is the
// authority — a refused provider does not satisfy "a master CLI").
const codexOnly = ready.map((c) =>
  c.check === "claude"
    ? { ...c, status: "missing" as const, fix: "claude auth login" }
    : c.check === "codex"
      ? { ...c, status: "ok" as const, fix: null }
      : c,
);
equal(missingRequired(report(codexOnly)), ["master CLI"], "codex is not a master provider");
// The board's offer list drives it: a payload offering codex accepts it.
const codexOffered = report(codexOnly);
codexOffered.master = { providers: [
  { bin: "claude", ready: false, start: null },
  { bin: "codex", ready: true, start: "cadence master start --provider codex" },
] };
equal(missingRequired(codexOffered), [], "the payload's provider list is the authority");
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
