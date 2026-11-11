import {
  draftDirty,
  draftOf,
  hostedIfRev,
  nativeGrant,
  policyWrite,
  privateRefused,
  readPolicy,
} from "../src/features/projects/policyContract";

function equal(actual: unknown, expected: unknown, what: string): void {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) throw new Error(`${what}: expected ${e}, got ${a}`);
}

/* ---------------- native GET: recorded, absent, malformed --------------- */

const nativeRecorded = {
  project: "cadence",
  recorded: true,
  policy: { version: 3, visibility: "private", members: ["users/mia", "operator"] },
  private_available: true,
};
equal(
  readPolicy(nativeRecorded, false),
  {
    kind: "recorded",
    visibility: "private",
    grants: ["users/mia", "operator"],
    revision: 3,
    privateOffered: true,
  },
  "a native recorded policy decodes with its version and grants",
);

// A missing capability is unknown, not "private is allowed".
equal(
  readPolicy({ project: "cadence", recorded: true, policy: { version: 1, visibility: "workspace_visible" } }, false),
  {
    kind: "recorded",
    visibility: "workspace_visible",
    grants: [],
    revision: 1,
    privateOffered: null,
  },
  "a native policy without private_available reports the capability as unknown",
);
equal(
  privateRefused(
    readPolicy({ recorded: true, policy: { version: 1, visibility: "workspace_visible" } }, false),
    { visibility: "private", grants: [] },
    false,
  ),
  true,
  "an unknown native capability refuses a private write",
);

equal(
  readPolicy({ project: "cadence", recorded: false, policy: null, private_available: false }, false),
  { kind: "absent", privateOffered: false },
  "no native record is absent, not workspace-visible",
);
equal(
  privateRefused(
    readPolicy({ recorded: false, policy: null, private_available: true }, false),
    { visibility: "private", grants: [] },
    false,
  ),
  false,
  "a native board may offer private for an absent policy",
);
equal(
  privateRefused(
    readPolicy({ recorded: false, policy: null }, false),
    { visibility: "private", grants: [] },
    false,
  ),
  true,
  "an absent native policy with unknown capability refuses private",
);
equal(
  readPolicy({ recorded: false, policy: { version: 1, visibility: "workspace_visible" } }, false),
  { kind: "unreadable", reason: "recorded is false but a policy was sent" },
  "an inconsistent native answer is unreadable",
);
equal(
  readPolicy({ recorded: true, policy: { version: 1, visibility: "shared" } }, false).kind,
  "unreadable",
  "an unknown native visibility is unreadable, never a default",
);
equal(
  readPolicy({ recorded: true, policy: { version: 1, visibility: "workspace-visible" } }, false).kind,
  "unreadable",
  "a native response cannot use the hosted visibility spelling",
);

/* ---------------- hosted GET: state ok / not_stored --------------------- */

const hostedRecorded = {
  state: "ok",
  project: "pay",
  visibility: "workspace-visible",
  grants: ["usr_1", "usr_2"],
  revision: 7,
  provenance: "hosted-owner-attested",
  updated_by: "usr_1",
  updated_at: "2026-11-11T00:00:00Z",
};
equal(
  readPolicy(hostedRecorded, true),
  {
    kind: "recorded",
    visibility: "workspace_visible",
    grants: ["usr_1", "usr_2"],
    revision: 7,
    privateOffered: false,
  },
  "a hosted workspace-visible row decodes and offers no private transition",
);
equal(
  readPolicy({ ...hostedRecorded, visibility: "private" }, true).kind,
  "recorded",
  "a hosted private row decodes",
);
equal(
  readPolicy({ ...hostedRecorded, visibility: "workspace_visible" }, true).kind,
  "unreadable",
  "a hosted response cannot use the native visibility spelling",
);
equal(
  privateRefused(readPolicy(hostedRecorded, true), { visibility: "workspace_visible", grants: [] }, false),
  false,
  "a workspace-visible draft is not a private transition",
);

// The documented no-row shape. This is the live bug: it must never read as
// workspace-visible.
const hostedNoRow = {
  state: "not_stored",
  project: "pay",
  visibility: null,
  grants: [],
  revision: null,
  provenance: "unknown",
};
equal(readPolicy(hostedNoRow, true), { kind: "not_stored" }, "the hosted no-row shape is not_stored");
equal(hostedIfRev(readPolicy(hostedNoRow, true)), "", "the first hosted write pins if_rev as empty string");
equal(
  policyWrite(readPolicy(hostedNoRow, true), { visibility: "workspace_visible", grants: [] }, true),
  { contract: "hosted", visibility: "workspace-visible", grants: [], if_rev: "" },
  "the hosted first write keeps the compare-and-set token",
);
equal(
  privateRefused(readPolicy(hostedNoRow, true), { visibility: "private", grants: [] }, true),
  true,
  "a no-row hosted project cannot be turned private",
);
equal(
  readPolicy({ state: "not_stored", visibility: "workspace-visible", grants: [], revision: null }, true).kind,
  "unreadable",
  "not_stored carrying a visibility is unreadable",
);
equal(
  readPolicy({ ...hostedRecorded, state: "stale" }, true).kind,
  "unreadable",
  "an unrecognised hosted state is unreadable",
);
equal(
  readPolicy({ ...hostedRecorded, revision: null }, true).kind,
  "unreadable",
  "a hosted row without a revision cannot be written against",
);

/* ---------------- hosted PUT body pins the revision --------------------- */

const hostedView = readPolicy({ ...hostedRecorded, revision: 9 }, true);
equal(hostedIfRev(hostedView), "9", "the hosted revision is pinned as a string");
equal(
  policyWrite(hostedView, { visibility: "workspace_visible", grants: ["usr_2", "usr_3"] }, true),
  { contract: "hosted", visibility: "workspace-visible", grants: ["usr_2", "usr_3"], if_rev: "9" },
  "the hosted write is {visibility, grants, if_rev}",
);

// Native keeps its own body — never the hosted one.
const nativeView = readPolicy(nativeRecorded, false);
equal(
  policyWrite(nativeView, { visibility: "private", grants: ["users/mia"] }, false),
  { contract: "native", visibility: "private", members: ["users/mia"] },
  "the native write is {visibility, members}",
);
equal(
  policyWrite(readPolicy({ recorded: false, policy: null }, false), { visibility: "workspace_visible", grants: [] }, false),
  { contract: "native", visibility: "workspace-visible", members: [] },
  "recording a first native policy is a normal write",
);

// An unreadable read pins nothing: there is no write to invent.
equal(
  policyWrite({ kind: "unreadable", reason: "x" }, { visibility: "workspace_visible", grants: [] }, true),
  null,
  "an unreadable policy yields no write",
);

/* ---------------- drafts and native grant grammar ----------------------- */

equal(
  draftOf(readPolicy(hostedNoRow, true)),
  { visibility: "workspace_visible", grants: [] },
  "a no-row project starts from a workspace-visible proposal",
);
equal(
  draftDirty(readPolicy(nativeRecorded, false), { visibility: "private", grants: ["users/mia", "operator"] }),
  false,
  "an unchanged draft is clean",
);
equal(
  draftDirty(readPolicy(nativeRecorded, false), { visibility: "private", grants: ["users/mia"] }),
  true,
  "a removed grant is dirty",
);
equal(
  draftDirty(readPolicy({ recorded: false, policy: null }, false), { visibility: "workspace_visible", grants: [] }),
  true,
  "recording a first policy is always a change",
);
equal(nativeGrant("users/mia"), "users/mia", "a users/<handle> grant is accepted");
equal(nativeGrant("operator"), "operator", "the operator grant is accepted");
equal(nativeGrant("agents/dev"), null, "an agent spelling is refused");
equal(nativeGrant("users/"), null, "an empty handle is refused");

console.log("project policy contract checks passed");
