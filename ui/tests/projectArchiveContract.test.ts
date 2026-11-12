import {
  ARCHIVE_CAPABILITY,
  readArchivedProjects,
  readLifecycle,
  receiptMatches,
  type ArchiveReceipt,
  type ArchiveView,
} from "../src/features/projects/archiveContract";

function assert(value: unknown, why: string): asserts value {
  if (!value) throw new Error(why);
}

function equal(actual: unknown, expected: unknown, what: string): void {
  if (JSON.stringify(actual) !== JSON.stringify(expected)) {
    throw new Error(`${what}: expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`);
  }
}

function receipt(over: Partial<Record<string, unknown>> = {}): Record<string, unknown> {
  return {
    archive_capability: ARCHIVE_CAPABILITY,
    project: "cadence",
    archived: false,
    revision: 12,
    archived_at: "2026-02-02T00:00:00Z",
    archived_by: "owner@example.test",
    changed_at: "2026-03-03T00:00:00Z",
    changed_by: "operator@example.test",
    blockers: [],
    ...over,
  };
}

/* ------------------------------ lifecycle ------------------------------ */

const active = readLifecycle(receipt(), "cadence");
if (active.kind !== "available") throw new Error("a shaped active receipt is available");
equal(active.receipt.archived, false, "the active receipt is not archived");
equal(active.receipt.revision, 12, "the receipt carries its fence");
equal(active.receipt.blockers, [], "no blockers is an empty list");

// The last-archive evidence survives a restore; only `archived` is current.
const restored = readLifecycle(
  receipt({
    archived: false,
    archived_at: "2026-02-02T00:00:00Z",
    archived_by: "owner@example.test",
    changed_at: "2026-03-03T00:00:00Z",
    changed_by: "operator@example.test",
  }),
  "cadence",
);
if (restored.kind !== "available") throw new Error("a restored receipt with archive evidence is available");
equal(restored.receipt.archived, false, "restored metadata does not make the project archived");
equal(restored.receipt.archived_at, "2026-02-02T00:00:00Z", "the last archive time is kept as evidence");

const archived = readLifecycle(receipt({ archived: true }), "cadence");
if (archived.kind !== "available") throw new Error("an archived receipt is available");
equal(archived.receipt.archived, true, "the archived receipt reports the archived state");

// revision 0 is the untouched project: active, every evidence field null.
const untouched = readLifecycle(
  receipt({
    revision: 0,
    archived: false,
    archived_at: null,
    archived_by: null,
    changed_at: null,
    changed_by: null,
  }),
  "cadence",
);
if (untouched.kind !== "available") throw new Error("an untouched revision-0 receipt is available");
equal(untouched.receipt.revision, 0, "the untouched receipt keeps revision 0");
equal(untouched.receipt.changed_at, null, "the untouched receipt has no change evidence");

// Receipts the server cannot emit are unreadable, never proved.
equal(
  readLifecycle(
    receipt({
      revision: 0,
      archived: true,
      archived_at: null,
      archived_by: null,
      changed_at: null,
      changed_by: null,
    }),
    "cadence",
  ).kind,
  "unreadable",
  "revision 0 cannot be archived",
);
equal(
  readLifecycle(receipt({ revision: 0 }), "cadence").kind,
  "unreadable",
  "revision 0 cannot carry evidence",
);
equal(
  readLifecycle(receipt({ archived_at: null }), "cadence").kind,
  "unreadable",
  "a transitioned receipt needs its last-archive time",
);
equal(
  readLifecycle(receipt({ archived_by: "" }), "cadence").kind,
  "unreadable",
  "a transitioned receipt needs a non-empty archive actor",
);
equal(
  readLifecycle(receipt({ changed_by: "   " }), "cadence").kind,
  "unreadable",
  "a transitioned receipt needs a non-empty change actor",
);
equal(
  readLifecycle(receipt({ changed_at: "yesterday" }), "cadence").kind,
  "unreadable",
  "a malformed change timestamp is unreadable",
);
equal(
  readLifecycle(receipt({ archived_at: "2026-02-02" }), "cadence").kind,
  "unreadable",
  "a date without a time and zone is unreadable",
);

// A receipt for another project is never rendered as this one's.
equal(
  readLifecycle(receipt({ project: "other" }), "cadence").kind,
  "unreadable",
  "a receipt naming another project is unreadable",
);
equal(
  readLifecycle(receipt({ project: "cadence", archive_capability: "cad2255.project-archive.v2" }), "cadence").kind,
  "unavailable",
  "an unrecognised capability is unavailable, never active",
);
equal(
  readLifecycle(receipt({ archive_capability: undefined }), "cadence").kind,
  "unknown",
  "a missing capability is unknown, never an offer to mutate access",
);
equal(
  readLifecycle({ archive_unavailable: true, status: 501 }, "cadence").kind,
  "unavailable",
  "an explicit 501 proves the capability absent",
);
equal(
  readLifecycle({ archive_unavailable: true, status: 404 }, "cadence").kind,
  "unknown",
  "an unanswered route (404) is unknown, not proof of unavailability",
);
equal(
  readLifecycle({ archive_unavailable: true }, "cadence").kind,
  "unknown",
  "a sentinel without a proved status is unknown",
);
equal(
  readLifecycle({ ...receipt(), revision: "13" }, "cadence").kind,
  "unreadable",
  "a string revision cannot fence a write",
);
equal(
  readLifecycle({ ...receipt(), revision: 1.5 }, "cadence").kind,
  "unreadable",
  "a fractional revision cannot fence a write",
);
equal(
  readLifecycle({ ...receipt(), archived: undefined }, "cadence").kind,
  "unreadable",
  "a receipt without the current archived flag is unreadable",
);
equal(
  readLifecycle({ ...receipt(), blockers: ["why", 3] }, "cadence").kind,
  "unreadable",
  "malformed blockers are unreadable",
);
equal(readLifecycle(null, "cadence").kind, "unreadable", "a non-object answer is unreadable");

/* --------------------------- receipt matching -------------------------- */

const success = (over: Partial<ArchiveReceipt> = {}): ArchiveView => {
  const view = readLifecycle(receipt({ archived: true, revision: 13, ...over }), "cadence");
  if (view.kind !== "available") throw new Error("the fixtures prove a receipt");
  return view;
};

assert(
  receiptMatches(success(), { project: "cadence", archived: true, expectedRevision: 12 }),
  "a matching receipt at the exact next revision proves the archive",
);
assert(
  !receiptMatches(success({ revision: 14 }), { project: "cadence", archived: true, expectedRevision: 12 }),
  "a jumped revision may belong to another actor and is not this success",
);
assert(
  !receiptMatches(success(), { project: "cadence", archived: true, expectedRevision: Number.MAX_SAFE_INTEGER }),
  "an expected revision at the safe-integer ceiling cannot be incremented",
);
assert(
  !receiptMatches(success({ revision: 12 }), { project: "cadence", archived: true, expectedRevision: 12 }),
  "a receipt at the pinned revision is stale, not a success",
);
assert(
  !receiptMatches(success({ revision: 11 }), { project: "cadence", archived: true, expectedRevision: 12 }),
  "a receipt below the pinned revision is stale, not a success",
);
assert(
  !receiptMatches(success(), { project: "cadence", archived: false, expectedRevision: 12 }),
  "a receipt for the other state does not prove this transition",
);
assert(
  !receiptMatches(success(), { project: "other", archived: true, expectedRevision: 12 }),
  "a receipt for another project does not prove this transition",
);
assert(
  !receiptMatches(
    { kind: "unavailable", reason: "x" },
    { project: "cadence", archived: true, expectedRevision: 12 },
  ),
  "an unavailable read never matches a transition",
);
assert(
  !receiptMatches(
    { kind: "unknown", reason: "x" },
    { project: "cadence", archived: true, expectedRevision: 12 },
  ),
  "an unknown read never matches a transition",
);
assert(
  !receiptMatches(
    { kind: "unreadable", reason: "x" },
    { project: "cadence", archived: true, expectedRevision: 12 },
  ),
  "an unreadable read never matches a transition",
);

/* ------------------------- archived-project discovery ------------------------- */

const row = {
  key: "legacy-app",
  prefix: "LEG",
  components: ["web"],
  repos: [{ path: "legacy" }],
  issues: 42,
  archived: true,
};

const listed = readArchivedProjects({ archive_supported: true, projects: [row] });
if (listed.kind !== "available") throw new Error("an explicit capability with archived rows is available");
equal(listed.projects.length, 1, "the archived row is listed");
equal(listed.projects[0]!.key, "legacy-app", "the row keeps its key");
equal(listed.projects[0]!.archived, true, "the row stays marked archived");

equal(
  readArchivedProjects({ archive_supported: true, projects: [] }),
  { kind: "available", projects: [] },
  "an empty proved list is an honest empty list",
);

// The old server ignores `?archived=1` and answers the ordinary list.
equal(
  readArchivedProjects({ projects: [row] }).kind,
  "unavailable",
  "a server without the capability is unavailable, never an empty archived list",
);
equal(
  readArchivedProjects({ archive_supported: false, projects: [row] }).kind,
  "unavailable",
  "an explicit false capability is unavailable",
);
equal(
  readArchivedProjects({ archive_unavailable: true }).kind,
  "unavailable",
  "an unserved route sentinel is unavailable",
);
equal(
  readArchivedProjects({ archive_supported: true, projects: [{ ...row, archived: false }] }).kind,
  "unreadable",
  "an active row in the archived list is unreadable, never shown as archived",
);
equal(
  readArchivedProjects({ archive_supported: true, projects: [{ ...row, repos: "legacy" }] }).kind,
  "unreadable",
  "malformed repos are unreadable",
);
equal(
  readArchivedProjects({ archive_supported: true, projects: [{ ...row, issues: undefined }] }).kind,
  "unreadable",
  "a row with no issue count is unreadable",
);
equal(readArchivedProjects(null).kind, "unreadable", "a non-object discovery answer is unreadable");

console.log("project archive contract checks passed");
