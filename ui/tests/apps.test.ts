import {
  appApprovalChip,
  appHref,
  approvalPending,
  approveBlock,
  doctorFindings,
  runHref,
  sourceLabel,
  unboundSlots,
} from "../src/features/apps/apps";
import type { AppRow } from "../src/lib/types";

function equal(actual: unknown, expected: unknown, what: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${what}: expected ${e}, got ${a}`);
}

const row: AppRow = {
  project: "cadence",
  name: "studio",
  title: "Studio",
  version: "0.1.0",
  approval: "approved",
  connections: [{ slot: "publish", bound: "local" }],
  workflows: ["do-check"],
};

// Routes: detail under /apps, run into the project's Workflows `?run=`.
equal(appHref("cadence", "studio"), "/apps/cadence/studio", "app detail href");
equal(
  runHref("cadence", "studio/do-check"),
  "/projects/cadence/workflows?run=studio%2Fdo-check",
  "run href encodes the qualified name",
);
equal(
  new URLSearchParams("run=studio%2Fdo-check").get("run"),
  "studio/do-check",
  "run param decodes to the qualified name",
);

// Approval chips — the four states a row reports.
equal(appApprovalChip(row).text, "approved", "approved chip");
equal(appApprovalChip({ ...row, approval: "changed" }).text, "changed since approval", "changed chip");
equal(appApprovalChip({ ...row, approval: "unapproved" }).text, "unapproved", "unapproved chip");
equal(appApprovalChip({ ...row, approval: "unknown" }).text, "approval unknown", "unknown chip");
equal(appApprovalChip({ ...row, error: "broken" }).text, "broken", "error row chip");

// Approve is offered only while approval is pending and only to the operator.
equal(approvalPending(row), false, "approved is not pending");
equal(approvalPending({ ...row, approval: "changed" }), true, "changed is pending");
equal(approvalPending({ ...row, approval: "unapproved" }), true, "unapproved is pending");
equal(approvalPending({ ...row, error: "symlink" }), false, "a broken row has no approve");
const operator = { readOnly: false, operator: true };
equal(approveBlock({ ...row, approval: "unapproved" }, operator), null, "operator may approve");
equal(approveBlock({ ...row, approval: "unapproved" }, { readOnly: true, operator: true }) !== null, true, "read-only blocked");
equal(approveBlock({ ...row, approval: "unapproved" }, { readOnly: false, operator: false }) !== null, true, "unproven blocked");
equal(approveBlock(row, operator), null, "approved row: no button either way");

// Slots: unbound are the ones doctor flags.
equal(unboundSlots(row), [], "bound slot is not unbound");
equal(
  unboundSlots({ ...row, connections: [{ slot: "publish", bound: null }] }),
  ["publish"],
  "null bound is unbound",
);

// Source: a git install names url + pinned sha; a path install names the path.
equal(
  sourceLabel({ ...row, source: { kind: "git", url: "https://x/y", sha: "0123456789abcdef" } }),
  "git https://x/y @ 0123456789ab",
  "git source",
);
equal(sourceLabel({ ...row, source: { kind: "path", path: "apps/studio" } }), "path apps/studio", "path source");
equal(sourceLabel({ ...row, source: null }), null, "no source");

// Doctor findings flatten to labeled lines.
equal(doctorFindings(null), [], "no doctor row");
equal(
  doctorFindings({
    project: "cadence",
    app: "studio",
    slots_ok: [{ slot: "publish", connection: "local" }],
    unbound: ["logs"],
    unknown_connection: [{ slot: "cdn", connection: "gone" }],
    stray_bindings: ["extra"],
  }).map((f) => f.text),
  [
    "publish → local",
    "logs — unbound",
    "cdn → gone — unknown connection",
    "extra — bound but not declared",
  ],
  "findings",
);

console.log("apps checks passed");
