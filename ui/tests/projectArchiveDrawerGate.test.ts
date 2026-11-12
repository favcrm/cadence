import {
  ARCHIVED_WRITE_BLOCK,
  UNVERIFIED_WRITE_BLOCK,
  drawerArchiveGate,
  issuePageWriteBlock,
} from "../src/features/projects/drawerArchiveGate";
import type { ResourceState } from "../src/lib/cache";

function equal(actual: unknown, expected: unknown, what: string): void {
  if (actual !== expected) {
    throw new Error(`${what}: expected ${String(expected)}, got ${String(actual)}`);
  }
}

function receipt(project: string, over: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    archive_capability: "cad2255.project-archive.v1",
    project,
    archived: false,
    revision: 4,
    archived_at: "2026-01-01T00:00:00Z",
    archived_by: "owner@example.test",
    changed_at: "2026-01-02T00:00:00Z",
    changed_by: "operator@example.test",
    blockers: [],
    ...over,
  };
}

function state(over: Partial<ResourceState<unknown>>): ResourceState<unknown> {
  return { data: null, status: "ok", error: null, asOf: Date.now(), inFlight: false, ...over };
}

/* ------------------------------ drawer gate ------------------------------ */

// A cached active receipt is not proof while the read is being revalidated:
// another tab may have archived the project since that answer.
equal(
  drawerArchiveGate({
    project: "cadence",
    state: state({ data: receipt("cadence"), status: "ok", inFlight: true }),
  }).writeBlock,
  UNVERIFIED_WRITE_BLOCK,
  "a cached active receipt during revalidation is blocked",
);
equal(
  drawerArchiveGate({
    project: "cadence",
    state: state({ data: receipt("cadence"), status: "loading", inFlight: true }),
  }).writeBlock,
  UNVERIFIED_WRITE_BLOCK,
  "a cached receipt behind a load is blocked",
);
equal(
  drawerArchiveGate({
    project: "cadence",
    state: state({ data: receipt("cadence", { archived: true }), status: "ok", inFlight: true }),
  }).writeBlock,
  UNVERIFIED_WRITE_BLOCK,
  "even a cached archived receipt is unverified while revalidating",
);
equal(
  drawerArchiveGate({ project: "cadence", state: state({ data: null, status: "loading", inFlight: true }) }).writeBlock,
  UNVERIFIED_WRITE_BLOCK,
  "a missing lifecycle read is blocked",
);

// Only a current, matching ACTIVE answer unlocks.
equal(
  drawerArchiveGate({ project: "cadence", state: state({ data: receipt("cadence") }) }).writeBlock,
  null,
  "a current matching active receipt unlocks",
);
equal(
  drawerArchiveGate({
    project: "cadence",
    state: state({ data: receipt("cadence", { archived: true }) }),
  }).writeBlock,
  ARCHIVED_WRITE_BLOCK,
  "a current archived receipt blocks with its reason",
);
equal(
  drawerArchiveGate({ project: "cadence", state: state({ data: receipt("other") }) }).writeBlock,
  UNVERIFIED_WRITE_BLOCK,
  "a receipt for another project does not unlock the item",
);
equal(
  drawerArchiveGate({ project: "cadence", state: state({ data: receipt("cadence"), status: "stale", error: "offline" }) })
    .writeBlock,
  UNVERIFIED_WRITE_BLOCK,
  "a stale read does not unlock the item",
);
equal(
  drawerArchiveGate({ project: "cadence", state: state({ data: receipt("cadence"), status: "failed", error: "down" }) })
    .writeBlock,
  UNVERIFIED_WRITE_BLOCK,
  "a failed read does not unlock the item",
);
equal(
  drawerArchiveGate({ project: "cadence", state: state({ data: { archive_capability: "v2" } }) }).writeBlock,
  UNVERIFIED_WRITE_BLOCK,
  "an unrecognised capability does not unlock the item",
);
equal(
  drawerArchiveGate({ project: null, state: state({ data: receipt("cadence") }) }).writeBlock,
  "This issue's project could not be determined, so its work stays read-only.",
  "a null item project never borrows another project's receipt",
);

/* ---------------------------- direct issue page --------------------------- */

equal(
  issuePageWriteBlock({ routeProject: "cadence", detailProject: null, state: null }),
  "This issue's project has not loaded, so its work stays read-only.",
  "an unloaded issue project fails closed",
);
equal(
  issuePageWriteBlock({ routeProject: "cadence", detailProject: "other", state: state({ data: receipt("cadence") }) }),
  "This issue belongs to a different project than this page requested, so its work stays read-only.",
  "a loaded issue from another project fails closed even with an active route receipt",
);
equal(
  issuePageWriteBlock({
    routeProject: "cadence",
    detailProject: "cadence",
    state: state({ data: receipt("cadence") }),
  }),
  null,
  "a matching current active issue project unlocks",
);
equal(
  issuePageWriteBlock({
    routeProject: "cadence",
    detailProject: "cadence",
    state: state({ data: receipt("cadence", { archived: true }) }),
  }),
  ARCHIVED_WRITE_BLOCK,
  "an archived loaded issue project blocks",
);
equal(
  issuePageWriteBlock({
    routeProject: "cadence",
    detailProject: "cadence",
    state: state({ data: receipt("cadence"), inFlight: true }),
  }),
  UNVERIFIED_WRITE_BLOCK,
  "a revalidating issue project blocks",
);
equal(
  issuePageWriteBlock({
    routeProject: "cadence",
    detailProject: "cadence",
    state: state({ data: receipt("other") }),
  }),
  UNVERIFIED_WRITE_BLOCK,
  "an issue receipt for another project blocks",
);

console.log("project archive write-gate checks passed");
