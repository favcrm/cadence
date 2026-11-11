import { invalidatedBy } from "../src/lib/cache";
import { cliCommand } from "../src/features/projects/policyView";
import { draftDirty, readPolicy, type PolicyView } from "../src/features/projects/policyContract";

function equal(actual: unknown, expected: unknown, what: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${what}: expected ${e}, got ${a}`);
}

// CAD-1950: the Settings page prints the CLI command for the state on screen.
// The two must not drift, because `--grant` replaces and omitting it clears.
equal(
  cliCommand("pay", { visibility: "workspace_visible", grants: [] }),
  "cadence issue project policy pay --visibility workspace-visible",
  "visible with no grants",
);
equal(
  cliCommand("pay", { visibility: "private", grants: ["users/alex"] }),
  "cadence issue project policy pay --visibility private --grant users/alex",
  "private with one grant",
);
equal(
  cliCommand("pay", {
    visibility: "workspace_visible",
    grants: ["users/sarah", "operator"],
  }),
  "cadence issue project policy pay --visibility workspace-visible --grant users/sarah --grant operator",
  "a visible project still spells out the grants it would keep",
);

// A project with no recorded policy is always a change: recording
// workspace-visible is a write, and without this the page could never create
// the policy it invites the operator to create.
const visibleView: PolicyView = {
  kind: "recorded",
  visibility: "workspace_visible",
  grants: [],
  revision: 1,
  privateOffered: false,
};
const visible = { visibility: "workspace_visible" as const, grants: [] };
equal(
  draftDirty(readPolicy({ recorded: false, policy: null }, false), visible),
  true,
  "no policy is always dirty",
);
equal(draftDirty(visibleView, visible), false, "an unchanged record is clean");
equal(
  draftDirty(visibleView, { visibility: "private", grants: [] }),
  true,
  "a visibility change is dirty",
);
equal(
  draftDirty(
    { kind: "recorded", visibility: "private", grants: ["users/alex"], revision: 2, privateOffered: true },
    { visibility: "private", grants: ["users/alex", "users/sarah"] },
  ),
  true,
  "an added grant is dirty",
);
equal(
  draftDirty(
    { kind: "recorded", visibility: "private", grants: ["users/alex"], revision: 2, privateOffered: true },
    { visibility: "private", grants: ["users/sarah"] },
  ),
  true,
  "a replaced grant is dirty",
);
equal(draftDirty({ kind: "unreadable", reason: "x" }, visible), false, "nothing loaded is not a change");
equal(draftDirty({ kind: "absent", privateOffered: null }, visible), true, "an absent native record is a write");

// The server names `projectPolicy` in an `issues` event. `invalidatedBy`
// drops any name missing from the client allowlist, which would leave the
// Settings page showing a stale policy and able to save over a newer record.
equal(
  invalidatedBy(JSON.stringify({ resources: ["issues", "projectPolicy"] })),
  ["issues", "projectPolicy"],
  "the policy read is not dropped by the client allowlist",
);

console.log("project policy view checks passed");
