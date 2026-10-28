import { releaseChangesReach, routineRelease } from "../src/features/explorer/automaticUpdates";
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
