import { automaticUpgradeClaim, installConsentAllowsAutomatic, mayApplyAutomatically, recordedPackageSource, releaseChangesReach, routineRelease } from "../src/features/explorer/automaticUpdates";
import type { AccessDiff } from "../src/features/workspace-apps/workspaceApps";

function equal(actual: unknown, expected: unknown, what: string): void {
  if (actual !== expected) throw new Error(`${what}: expected ${expected}, got ${actual}`);
}

const quiet: AccessDiff = {
  connections: { added: [], removed: [] },
  capabilities: {},
};

const routine = {
  access_diff: quiet,
  secret_warnings: [],
  compatibility: { ok: true, compatible: true },
};

equal(routineRelease(routine), true, "an unchanged release is routine");
equal(routineRelease({ ...routine, access_diff: undefined }), false, "a missing access diff is not routine");
equal(
  routineRelease({
    ...routine,
    access_diff: { ...quiet, connections: { added: ["mail"], removed: [] } },
  }),
  false,
  "a new connection changes reach",
);
equal(
  routineRelease({
    ...routine,
    access_diff: {
      ...quiet,
      capabilities: { files: { before: null, after: null } },
    },
  }),
  false,
  "a capability change is not routine",
);
equal(routineRelease({ ...routine, secret_warnings: [{}] }), false, "a secret warning is not routine");
equal(
  routineRelease({ ...routine, compatibility: { ...routine.compatibility, compatible: false } }),
  false,
  "an incompatible release is not routine",
);
equal(releaseChangesReach(quiet), false, "empty access is not a reach change");
equal(releaseChangesReach(undefined), true, "unreadable access is a reach change");
equal(recordedPackageSource({ kind: "git", url: " https://example.com/app.git " }), "https://example.com/app.git", "a git install records its url");
equal(recordedPackageSource({ kind: "builtin", url: "builtin:crm" }), "", "a retired built-in source is not an upstream");
equal(recordedPackageSource(undefined), "", "a missing source is not an upstream");
equal(
  mayApplyAutomatically({
    checkedSource: "https://example.com/app.git",
    recordedSource: "https://example.com/app.git",
    receipt: routine,
  }),
  true,
  "a routine receipt of the recorded source may apply on its own",
);
equal(
  mayApplyAutomatically({
    checkedSource: "/tmp/other",
    recordedSource: "https://example.com/app.git",
    receipt: routine,
  }),
  false,
  "a typed source does not apply on its own",
);
equal(
  mayApplyAutomatically({
    checkedSource: "https://example.com/app.git",
    recordedSource: "",
    receipt: routine,
  }),
  false,
  "an install with no recorded source does not apply on its own",
);

equal(installConsentAllowsAutomatic({ approved: true, approval: { state: "approved" } }), true, "an approved install may receive a routine release");
equal(installConsentAllowsAutomatic({ approved: false, approval: { state: "revoked" } }), false, "a revoked install does not apply on its own");
equal(installConsentAllowsAutomatic({ approved: null, approval: { state: "pending" } }), false, "an unapproved install does not apply on its own");
equal(installConsentAllowsAutomatic({ approved: true, removed: 1, approval: { state: "approved" } }), false, "a removed install does not apply on its own");
equal(installConsentAllowsAutomatic({ approved: true, state: "disabled", approval: { state: "approved" } }), false, "a disabled install does not apply on its own");
equal(JSON.stringify(automaticUpgradeClaim()), JSON.stringify({ automatic: true, observed_approval: "approved" }), "an automatic apply carries the approval it observed");
