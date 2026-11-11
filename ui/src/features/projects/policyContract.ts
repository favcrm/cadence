/**
 * CAD-2255: the two project-policy contracts, read and written as what they
 * are — never as one another.
 *
 * Native (`POST /api/projects/<key>/policy`) answers
 * `{project, recorded, policy: {version, visibility, members?} | null,
 * private_available}`. Hosted (`GET`/`PUT /api/projects/<key>/policy`) answers
 * `{state, project, visibility, grants, revision, provenance, updated_by,
 * updated_at}`; a project with no row answers `state: "not_stored"` — that is
 * *unknown* access policy, not workspace-visible, and the first write must pin
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
    }
  /** Native only: no policy has ever been recorded (REQ-007 keeps access). */
  | { kind: "absent"; privateOffered: boolean | null }
  /** Hosted only: the project has no stored policy row. */
  | { kind: "not_stored" }
  | { kind: "unreadable"; reason: string };

export interface PolicyDraft {
  visibility: RecordedVisibility;
  /** Wire grants: native `users/<handle>`/`operator`; hosted subject ids. */
  grants: string[];
}

export type PolicyWrite =
  | { contract: "native"; visibility: WireVisibility; members: string[] }
  | { contract: "hosted"; visibility: WireVisibility; grants: string[]; if_rev: string };

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

function grantsOf(value: unknown): string[] | null {
  if (!Array.isArray(value)) return null;
  const out: string[] = [];
  for (const entry of value) {
    if (typeof entry !== "string" || entry.trim() === "") return null;
    out.push(entry);
  }
  return out;
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
  if (body.state === "not_stored") {
    // The documented no-row shape, exactly. Anything else claiming "not
    // stored" while carrying values is not this shape.
    if (body.visibility !== null || body.revision !== null)
      return { kind: "unreadable", reason: "not_stored carried a visibility or revision" };
    const grants = body.grants === undefined ? [] : grantsOf(body.grants);
    if (grants === null || grants.length > 0)
      return { kind: "unreadable", reason: "not_stored carried grants" };
    return { kind: "not_stored" };
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
  // The hosted server refuses a NEW private row while isolation is unproven;
  // an existing private row stays manageable. There is no capability field to
  // read, so the record itself is the whole answer.
  return {
    kind: "recorded",
    visibility: shown,
    grants,
    revision,
    privateOffered: shown === "private",
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
  if (view.kind === "recorded") return { visibility: view.visibility, grants: view.grants };
  if (view.kind === "absent" || view.kind === "not_stored")
    return { visibility: "workspace_visible", grants: [] };
  return null;
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
  return view.kind !== "recorded" || view.visibility !== "private";
}

/**
 * Does the draft differ from the record? An absent or unrecorded policy is
 * always a write: recording workspace-visible is itself a change.
 */
export function draftDirty(view: PolicyView, draft: PolicyDraft | null): boolean {
  if (!draft) return false;
  if (view.kind === "absent" || view.kind === "not_stored") return true;
  if (view.kind !== "recorded") return false;
  return (
    draft.visibility !== view.visibility ||
    draft.grants.length !== view.grants.length ||
    draft.grants.some((grant, index) => grant !== view.grants[index])
  );
}
