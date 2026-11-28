/**
 * CAD-2255 / CAD-2884: the two project-policy contracts, read and written as
 * what they are — never as one another.
 *
 * Native (`POST /api/projects/<key>/policy`) answers
 * `{project, recorded, policy: {version, visibility, members?} | null,
 * private_available}`. Hosted (`GET`/`PUT /api/projects/<key>/policy`) answers
 * `{state, project, visibility, grants, members?, my_role?, capabilities?,
 * private_available?, revision, provenance, updated_by, updated_at}`; a
 * project with no row answers `state: "not_stored"` — that is *unknown*
 * access policy, not workspace-visible, and the first write must pin
 * `if_rev: ""` or it silently loses the compare-and-set.
 *
 * Every reader here is total and fail-closed: a missing, malformed or
 * unrecognised field is `unreadable`, never a default visibility, never
 * `private: true`, and never a fabricated success. Pure, so `tests/` can pin
 * both shapes without a DOM.
 */

import { objectField } from "../../lib/wire";

/** The recorded spelling shared by both stores. */
export type RecordedVisibility = "workspace_visible" | "private";
/** The wire spelling both writers accept. */
export type WireVisibility = "workspace-visible" | "private";

export type ProjectMemberRole = "owner" | "member" | "viewer";
export type EffectiveProjectRole = "workspace_owner" | ProjectMemberRole;

export interface ProjectMemberEntry {
  userId: string;
  role: ProjectMemberRole;
}

export interface ProjectPolicyCapabilities {
  can_manage: boolean;
  can_write: boolean;
  can_edit_details: boolean;
  can_archive: boolean;
}

export type PolicyView =
  | {
      kind: "recorded";
      /** Another vendor's store recorded the policy; the values below are it. */
      visibility: RecordedVisibility;
      grants: string[];
      /** Native policy version, or the hosted row revision. */
      revision: number | null;
      /**
       * Whether this viewer may record `private`. `false` means the build or
       * the server refuses a private transition; `null` is unknown and is
       * treated exactly like `false` by every consumer.
       */
      privateOffered: boolean | null;
      /** Hosted 3-role member list, when provided by AOS-814+. */
      members?: ProjectMemberEntry[];
      /** Caller's effective role on this project, when provided by AOS-814+. */
      myRole?: EffectiveProjectRole | null;
      /** Caller's project capability flags, when provided by AOS-814+. */
      capabilities?: ProjectPolicyCapabilities | null;
    }
  /** Native only: no policy has ever been recorded (REQ-007 keeps access). */
  | { kind: "absent"; privateOffered: boolean | null }
  /** Hosted only: the project has no stored policy row. */
  | {
      kind: "not_stored";
      privateOffered?: boolean | null;
      myRole?: EffectiveProjectRole | null;
      capabilities?: ProjectPolicyCapabilities | null;
    }
  | { kind: "unreadable"; reason: string };

export interface PolicyDraft {
  visibility: RecordedVisibility;
  /** Wire grants: native `users/<handle>`/`operator`; hosted subject ids. */
  grants: string[];
  /** Hosted explicit 3-role member list (`owner`, `member`, `viewer`). */
  members?: ProjectMemberEntry[];
}

export type PolicyWrite =
  | { contract: "native"; visibility: WireVisibility; members: string[] }
  | {
      contract: "hosted";
      visibility: WireVisibility;
      grants: string[];
      members?: ProjectMemberEntry[];
      if_rev: string;
    };

type Record_ = Record<string, unknown>;

function visibility(value: unknown): RecordedVisibility | null {
  return value === "workspace_visible" || value === "private" ? value : null;
}

/**
 * Hosted's wire spelling, kept separate from native's: the hosted store
 * returns `workspace-visible`, every native writer reads `workspace_visible`.
 * A spelling from the other board is not this board's — it is not mapped for
 * you, so a mis-shaped hosted row is unreadable instead of guessed.
 */
function hostedVisibility(value: unknown): RecordedVisibility | null {
  return value === "workspace-visible" ? "workspace_visible" : value === "private" ? "private" : null;
}

function isProjectMemberRole(value: unknown): value is ProjectMemberRole {
  return value === "owner" || value === "member" || value === "viewer";
}

function isEffectiveRole(value: unknown): value is EffectiveProjectRole {
  return value === "workspace_owner" || isProjectMemberRole(value);
}

function grantsOf(value: unknown): string[] | null {
  if (!Array.isArray(value)) return null;
  const out: string[] = [];
  for (const entry of value) {
    if (typeof entry !== "string" || entry.trim() === "") return null;
    out.push(entry);
  }
  return out;
}

function membersOf(value: unknown): ProjectMemberEntry[] | undefined | null {
  if (value === undefined) return undefined;
  if (!Array.isArray(value)) return null;
  const seen = new Set<string>();
  const out: ProjectMemberEntry[] = [];
  for (const entry of value) {
    const obj = objectField(entry);
    if (!obj || typeof obj.userId !== "string") return null;
    const trimmed = obj.userId.trim();
    if (trimmed === "" || seen.has(trimmed)) return null;
    if (!isProjectMemberRole(obj.role)) return null;
    seen.add(trimmed);
    out.push({ userId: trimmed, role: obj.role });
  }
  return out;
}

function myRoleOf(value: unknown): EffectiveProjectRole | null | undefined | "invalid" {
  if (value === undefined) return undefined;
  if (value === null) return null;
  return isEffectiveRole(value) ? value : "invalid";
}

function capabilitiesOf(
  value: unknown,
): ProjectPolicyCapabilities | null | undefined | "invalid" {
  if (value === undefined) return undefined;
  if (value === null) return null;
  const obj = objectField(value);
  if (
    !obj ||
    typeof obj.can_manage !== "boolean" ||
    typeof obj.can_write !== "boolean" ||
    typeof obj.can_edit_details !== "boolean" ||
    typeof obj.can_archive !== "boolean"
  )
    return "invalid";
  return {
    can_manage: obj.can_manage,
    can_write: obj.can_write,
    can_edit_details: obj.can_edit_details,
    can_archive: obj.can_archive,
  };
}

/**
 * Read either contract. `hosted` is the board's own claim from `/api/meta`;
 * when it is unknown (`null`) neither contract is guessed and the answer is
 * `unreadable` — the caller shows an error instead of a policy.
 */
export function readPolicy(raw: unknown, hosted: boolean | null): PolicyView {
  const body = objectField(raw);
  if (!body) return { kind: "unreadable", reason: "the policy answer is not an object" };
  if (hosted === null) return { kind: "unreadable", reason: "the board did not say whether it is hosted" };
  return hosted ? readHosted(body) : readNative(body);
}

function readNative(body: Record_): PolicyView {
  if (typeof body.recorded !== "boolean")
    return { kind: "unreadable", reason: "the policy answer has no recorded flag" };
  // A native board states this capability explicitly. Anything but `true` is
  // not an offer to record private; a missing value is unknown, not enabled.
  const offered =
    typeof body.private_available === "boolean" ? body.private_available : null;
  if (!body.recorded) {
    if (body.policy !== null && body.policy !== undefined)
      return { kind: "unreadable", reason: "recorded is false but a policy was sent" };
    return { kind: "absent", privateOffered: offered };
  }
  const policy = objectField(body.policy);
  if (!policy) return { kind: "unreadable", reason: "recorded is true but no policy was sent" };
  const shown = visibility(policy.visibility);
  if (shown === null) return { kind: "unreadable", reason: "the policy visibility is not recognised" };
  const version =
    typeof policy.version === "number" && Number.isSafeInteger(policy.version) && policy.version >= 0
      ? policy.version
      : null;
  if (version === null) return { kind: "unreadable", reason: "the policy version is not a number" };
  const grants = policy.members === undefined ? [] : grantsOf(policy.members);
  if (grants === null) return { kind: "unreadable", reason: "the policy grants are malformed" };
  return { kind: "recorded", visibility: shown, grants, revision: version, privateOffered: offered };
}

function readHosted(body: Record_): PolicyView {
  if (body.private_available !== undefined && typeof body.private_available !== "boolean")
    return { kind: "unreadable", reason: "private_available must be a boolean" };
  const privateFlag =
    typeof body.private_available === "boolean" ? body.private_available : undefined;
  const myRole = myRoleOf(body.my_role);
  if (myRole === "invalid")
    return { kind: "unreadable", reason: "my_role is not a valid project role" };
  const capabilities = capabilitiesOf(body.capabilities);
  if (capabilities === "invalid")
    return { kind: "unreadable", reason: "capabilities is malformed" };

  if (body.state === "not_stored") {
    // The documented no-row shape, plus optional AOS-814 metadata fields.
    if (body.visibility !== null || body.revision !== null)
      return { kind: "unreadable", reason: "not_stored carried a visibility or revision" };
    const grants = body.grants === undefined ? [] : grantsOf(body.grants);
    if (grants === null || grants.length > 0)
      return { kind: "unreadable", reason: "not_stored carried grants" };
    const members = membersOf(body.members);
    if (members === null || (members !== undefined && members.length > 0))
      return { kind: "unreadable", reason: "not_stored carried members" };
    return {
      kind: "not_stored",
      ...(privateFlag !== undefined ? { privateOffered: privateFlag } : {}),
      ...(myRole !== undefined ? { myRole } : {}),
      ...(capabilities !== undefined ? { capabilities } : {}),
    };
  }
  if (body.state !== "ok") return { kind: "unreadable", reason: "the policy state is not recognised" };
  const shown = hostedVisibility(body.visibility);
  if (shown === null) return { kind: "unreadable", reason: "the policy visibility is not recognised" };
  const revision =
    typeof body.revision === "number" && Number.isSafeInteger(body.revision) && body.revision >= 1
      ? body.revision
      : null;
  if (revision === null) return { kind: "unreadable", reason: "the policy revision is not a positive number" };
  const grants = grantsOf(body.grants);
  if (grants === null) return { kind: "unreadable", reason: "the policy grants are malformed" };
  const members = membersOf(body.members);
  if (members === null)
    return { kind: "unreadable", reason: "the policy members are malformed" };

  // AOS-814 sends explicit `private_available: true`; legacy hosted servers omit
  // it and only permit managing an already-private row.
  const privateOffered = privateFlag !== undefined ? privateFlag : shown === "private";
  return {
    kind: "recorded",
    visibility: shown,
    grants,
    revision,
    privateOffered,
    ...(members !== undefined ? { members } : {}),
    ...(myRole !== undefined ? { myRole } : {}),
    ...(capabilities !== undefined ? { capabilities } : {}),
  };
}

/**
 * The draft the page starts from. A recorded policy is its own baseline. A
 * project with no record starts from an explicit *proposal* of
 * workspace-visible — the only value that adds no restriction — and the page
 * says the record is absent beside it. An unreadable answer has no draft: a
 * proposal built on an unproved read would be a guess.
 */
export function draftOf(view: PolicyView): PolicyDraft | null {
  if (view.kind === "recorded") {
    return {
      visibility: view.visibility,
      grants: view.grants,
      ...(view.members !== undefined
        ? { members: view.members.map((m) => ({ ...m })) }
        : {}),
    };
  }
  if (view.kind === "absent" || view.kind === "not_stored") {
    return {
      visibility: "workspace_visible",
      grants: [],
      ...(view.kind === "not_stored" && view.privateOffered === true ? { members: [] } : {}),
    };
  }
  return null;
}

/** Effective team member list with roles for UI rendering. */
export function effectiveMembers(draft: PolicyDraft): ProjectMemberEntry[] {
  if (draft.members !== undefined) return draft.members;
  return draft.grants.map((userId) => ({ userId, role: "member" as const }));
}

export function policyCapabilities(view: PolicyView): ProjectPolicyCapabilities | null {
  if (view.kind === "recorded" || view.kind === "not_stored") return view.capabilities ?? null;
  return null;
}

export function policyMyRole(view: PolicyView): EffectiveProjectRole | null {
  if (view.kind === "recorded" || view.kind === "not_stored") return view.myRole ?? null;
  return null;
}

export function isProjectViewer(view: PolicyView | null): boolean {
  if (!view) return false;
  const role = policyMyRole(view);
  if (role === "viewer") return true;
  const caps = policyCapabilities(view);
  return caps !== null && caps.can_write === false;
}

export function canManageProjectPolicy(view: PolicyView | null): boolean {
  if (!view) return false;
  const caps = policyCapabilities(view);
  if (caps !== null) return caps.can_manage;
  const role = policyMyRole(view);
  return role === "workspace_owner" || role === "owner";
}

/** The recorded `revision` a hosted compare-and-set pins; `""` for no row. */
export function hostedIfRev(view: PolicyView): string | null {
  if (view.kind === "not_stored") return "";
  if (view.kind === "recorded") return String(view.revision ?? "");
  return null;
}

/** Wire spelling for a recorded visibility. */
export function wireVisibility(value: RecordedVisibility): WireVisibility {
  return value === "private" ? "private" : "workspace-visible";
}

/** The body one writer accepts, or null when the read cannot pin a write. */
export function policyWrite(
  view: PolicyView,
  draft: PolicyDraft,
  hosted: boolean,
): PolicyWrite | null {
  if (view.kind === "unreadable") return null;
  const visibility = wireVisibility(draft.visibility);
  if (!hosted) return { contract: "native", visibility, members: draft.grants };
  const ifRev = hostedIfRev(view);
  if (ifRev === null) return null;
  if (draft.members !== undefined) {
    const members = draft.members.map((m) => ({ userId: m.userId, role: m.role }));
    return {
      contract: "hosted",
      visibility,
      grants: draft.visibility === "private" ? members.map((m) => m.userId) : [],
      members,
      if_rev: ifRev,
    };
  }
  return { contract: "hosted", visibility, grants: draft.grants, if_rev: ifRev };
}

/** Native grant spelling the tracker's policy grammar accepts. */
export function nativeGrant(value: string): string | null {
  const trimmed = value.trim();
  if (trimmed === "operator") return trimmed;
  return /^users\/[A-Za-z0-9._:@-]{1,128}$/.test(trimmed) ? trimmed : null;
}

/**
 * Whether the draft would record a private policy this board refuses. An
 * unknown capability is a refusal: never send a write that might widen access.
 */
export function privateRefused(
  view: PolicyView,
  draft: PolicyDraft | null,
  hosted: boolean,
): boolean {
  if (!draft || draft.visibility !== "private") return false;
  if (!hosted) {
    return (view.kind !== "recorded" && view.kind !== "absent") || view.privateOffered !== true;
  }
  if (view.kind === "recorded") {
    return view.privateOffered !== true && view.visibility !== "private";
  }
  if (view.kind === "not_stored") {
    return view.privateOffered !== true;
  }
  return true;
}

function membersEqual(
  left: readonly ProjectMemberEntry[],
  right: readonly ProjectMemberEntry[],
): boolean {
  return (
    left.length === right.length &&
    left.every((entry, idx) => {
      const other = right[idx];
      return other !== undefined && entry.userId === other.userId && entry.role === other.role;
    })
  );
}

/**
 * Does the draft differ from the record? An absent or unrecorded policy is
 * always a write: recording workspace-visible is itself a change.
 */
export function draftDirty(view: PolicyView, draft: PolicyDraft | null): boolean {
  if (!draft) return false;
  if (view.kind === "absent" || view.kind === "not_stored") return true;
  if (view.kind !== "recorded") return false;
  if (draft.visibility !== view.visibility) return true;
  if (draft.members !== undefined || view.members !== undefined) {
    const baseMembers =
      view.members ?? view.grants.map((userId) => ({ userId, role: "member" as const }));
    return !membersEqual(effectiveMembers(draft), baseMembers);
  }
  return (
    draft.grants.length !== view.grants.length ||
    draft.grants.some((grant, index) => grant !== view.grants[index])
  );
}
