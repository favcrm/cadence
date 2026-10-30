import { invalidatedBy } from "../src/lib/cache";
import { cliCommand, policyDirty } from "../src/features/projects/policyView";

function equal(actual: unknown, expected: unknown, what: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${what}: expected ${e}, got ${a}`);
}

// CAD-1950: the Settings page prints the CLI command for the state on screen.
// The two must not drift, because `--grant` replaces and omitting it clears.
equal(
  cliCommand("pay", { visibility: "workspace_visible", members: [] }),
  "cadence issue project policy pay --visibility workspace-visible",
  "visible with no grants",
);
equal(
  cliCommand("pay", { visibility: "private", members: ["users/alex"] }),
  "cadence issue project policy pay --visibility private --grant users/alex",
  "private with one grant",
);
equal(
  cliCommand("pay", {
    visibility: "workspace_visible",
    members: ["users/sarah", "operator"],
  }),
  "cadence issue project policy pay --visibility workspace-visible --grant users/sarah --grant operator",
  "a visible project still spells out the grants it would keep",
);

// A project with no recorded policy is always a change: recording
// workspace-visible is a write, and without this the page could never create
// the policy it invites the operator to create.
const visible = { visibility: "workspace_visible" as const, members: [] };
equal(policyDirty(false, visible, visible), true, "no policy is always dirty");
equal(policyDirty(true, visible, visible), false, "an unchanged record is clean");
equal(
  policyDirty(true, visible, { visibility: "private", members: [] }),
  true,
  "a visibility change is dirty",
);
equal(
  policyDirty(true, { visibility: "private", members: ["users/alex"] }, {
    visibility: "private",
    members: ["users/alex", "users/sarah"],
  }),
  true,
  "an added grant is dirty",
);
equal(
  policyDirty(true, { visibility: "private", members: ["users/alex"] }, {
    visibility: "private",
    members: ["users/sarah"],
  }),
  true,
  "a replaced grant is dirty",
);
equal(policyDirty(true, null, null), false, "nothing loaded is not a change");

// The server names `projectPolicy` in an `issues` event. `invalidatedBy`
// drops any name missing from the client allowlist, which would leave the
// Settings page showing a stale policy and able to save over a newer record.
equal(
  invalidatedBy(JSON.stringify({ resources: ["issues", "projectPolicy"] })),
  ["issues", "projectPolicy"],
  "the policy read is not dropped by the client allowlist",
);

console.log("project policy view checks passed");
