import { ApiError } from "../../lib/apiError";
import type { StoredAgentProfile } from "../../lib/types";
import { appliesText } from "./agentRoles";

/**
 * CAD-2091: the Instructions document and how it is saved.
 *
 * Two boards keep it differently, and the difference is the point of this file:
 *  - hosted (`POST /api/agent-profiles/<id>`) takes the WHOLE profile and the
 *    revision it was read at, and answers 409 to a stale one;
 *  - native (`POST /api/agents/<alias>/instructions`) takes the text alone and
 *    overwrites. It has no revision, so there is nothing to compare and this
 *    file does not pretend there is.
 * Every function here is pure or takes its calls as `DocDeps`, so a test can
 * drive each outcome without a board.
 */

/** Hosted counts UTF-16 units of the TRIMMED text (`AGENT_PROFILE_RESPONSIBILITIES_MAX`). */
export const HOSTED_MAX_UNITS = 8000;
/** Native counts UTF-8 bytes (`MAX_INSTRUCTIONS_BYTES`). */
export const NATIVE_MAX_BYTES = 32_000;

export type DocKind = "hosted" | "native";

export interface DocDeps {
  readProfile: (id: string) => Promise<StoredAgentProfile>;
  writeProfile: (id: string, body: object) => Promise<StoredAgentProfile>;
  /** The stored text, or null when the board does not serve it. */
  readNative: (alias: string) => Promise<string | null>;
  writeNative: (alias: string, text: string) => Promise<{ applies: string }>;
}

/** What a save, or a check after a dropped one, came to. */
export type DocOutcome =
  | { kind: "saved"; text: string; profile: StoredAgentProfile | null; applies: string | null }
  /** Someone else's revision is stored. `profile` is theirs, when it was read. */
  | { kind: "conflict"; profile: StoredAgentProfile | null }
  /** The answer never arrived: the write may or may not have happened. */
  | { kind: "unconfirmed" }
  /** A check found the write did not happen. */
  | { kind: "not_saved"; profile: StoredAgentProfile | null }
  /** The board said no, so nothing was written. */
  | { kind: "refused"; message: string };

const byteLength = (text: string) => new TextEncoder().encode(text).length;

// The hosted store refuses these before it trims; LF, CR and tab are allowed.
// oxlint-disable-next-line no-control-regex -- mirrors the board's own refusal
const CONTROL = /[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f-\u009f]/u;

/** Why this text cannot be saved, in words; null when it can. */
export function textProblem(kind: DocKind, text: string): string | null {
  if (kind === "native") {
    const bytes = byteLength(text);
    return bytes > NATIVE_MAX_BYTES ? `Too long: ${bytes.toLocaleString("en")} of ${NATIVE_MAX_BYTES.toLocaleString("en")} bytes.` : null;
  }
  const trimmed = text.trim();
  if (trimmed.length === 0) return "Write something to save. The board does not keep an empty document.";
  if (trimmed.length > HOSTED_MAX_UNITS) return `Too long: ${trimmed.length.toLocaleString("en")} of ${HOSTED_MAX_UNITS.toLocaleString("en")} characters.`;
  if (CONTROL.test(trimmed)) return "Contains a control character the board refuses.";
  return null;
}

/** A size line for the save bar; only near the limit, or over it. */
export function sizeNote(kind: DocKind, text: string): string | null {
  const [used, max, unit] =
    kind === "hosted" ? [text.trim().length, HOSTED_MAX_UNITS, "characters"] : [byteLength(text), NATIVE_MAX_BYTES, "bytes"];
  return used >= max * 0.9 ? `${used.toLocaleString("en")} of ${max.toLocaleString("en")} ${unit}` : null;
}

/** What the board would store for this draft. The hosted board trims. */
export function stored(kind: DocKind, draft: string): string {
  return kind === "hosted" ? draft.trim() : draft;
}

/** Whether saving this draft would change anything. */
export function isDirty(kind: DocKind, draft: string | null, served: string): boolean {
  return draft !== null && stored(kind, draft) !== served;
}

/** The hosted update body: exactly the keys the board accepts, from the profile
 *  as it was read. `model` is left out so the stored one stays. */
export function profileBody(from: StoredAgentProfile, text: string, expectedRevision: number): object {
  return {
    name: from.name,
    role: from.role,
    responsibilities: text,
    skills: from.skills,
    status: from.status,
    expectedRevision,
  };
}

/**
 * How a failed write reads. A refusal (4xx) means nothing was written. A 409 is
 * the board's revision fence. Anything else - no answer, a 5xx, a session that
 * changed under the request - leaves it unknown whether the write landed.
 */
export function classifyFailure(error: unknown): "conflict" | "unconfirmed" | "refused" {
  if (!(error instanceof ApiError)) return "unconfirmed";
  if (error.status === 409) return "conflict";
  if (error.status >= 500 || error.status === 0 || error.status === 408) return "unconfirmed";
  return "refused";
}

function failure(error: unknown, profile: StoredAgentProfile | null = null): DocOutcome {
  const kind = classifyFailure(error);
  if (kind === "conflict") return { kind, profile };
  if (kind === "unconfirmed") return { kind };
  return { kind, message: error instanceof Error ? error.message : String(error) };
}

/** Save to a hosted profile at the revision the draft started from. */
export async function saveHosted(deps: DocDeps, base: StoredAgentProfile, text: string): Promise<DocOutcome> {
  try {
    const profile = await deps.writeProfile(base.id, profileBody(base, text, base.revision));
    return { kind: "saved", text: profile.responsibilities, profile, applies: null };
  } catch (error) {
    return failure(error);
  }
}

/** "Save mine" after a conflict: take THEIR current profile, so their other
 *  edits survive, and put this text on top of it at their revision. */
export async function saveOverTheirs(deps: DocDeps, id: string, text: string): Promise<DocOutcome> {
  let theirs: StoredAgentProfile;
  try {
    theirs = await deps.readProfile(id);
  } catch {
    return { kind: "refused", message: "Could not read the current version, so nothing was saved." };
  }
  return saveHosted(deps, theirs, text);
}

/** Native has no revision: the text replaces whatever is stored. */
export async function saveNative(deps: DocDeps, alias: string, text: string): Promise<DocOutcome> {
  try {
    const out = await deps.writeNative(alias, text);
    return { kind: "saved", text, profile: null, applies: out.applies };
  } catch (error) {
    return failure(error);
  }
}

/** After a dropped save: read what is stored and say whether it is ours. */
export async function checkHosted(deps: DocDeps, base: StoredAgentProfile, text: string): Promise<DocOutcome> {
  let latest: StoredAgentProfile;
  try {
    latest = await deps.readProfile(base.id);
  } catch {
    return { kind: "unconfirmed" };
  }
  if (latest.revision > base.revision && latest.responsibilities === text.trim()) {
    return { kind: "saved", text: latest.responsibilities, profile: latest, applies: null };
  }
  if (latest.revision === base.revision) return { kind: "not_saved", profile: latest };
  return { kind: "conflict", profile: latest };
}

export async function checkNative(deps: DocDeps, alias: string, text: string): Promise<DocOutcome> {
  let latest: string | null;
  try {
    latest = await deps.readNative(alias);
  } catch {
    return { kind: "unconfirmed" };
  }
  if (latest === text) return { kind: "saved", text, profile: null, applies: null };
  return { kind: "not_saved", profile: null };
}

/** The toast after a save, saying when the agent sees the new text. A turn
 *  already running is never changed. `checked` is a save found by a check
 *  after the answer was lost, so the board never said when it applies. */
export function savedText(kind: DocKind, applies: string | null, checked = false): string {
  const lead = checked ? "It did save." : "Saved.";
  if (kind === "hosted") return `${lead} It applies when a session next loads this profile. A running turn isn’t changed.`;
  if (applies === null) return `${lead} The board did not say when it applies.`;
  const when = appliesText(applies);
  return applies === "next_turn" || applies === "next_launch"
    ? `${lead} ${when}. A turn already running keeps the text it started with.`
    : `${when}.`;
}

/** Native `instructions_updated` events carry who made the change. */
export function nativeChange(event: { kind: string; at: string; payload?: Record<string, unknown> }): { by: string | null } | null {
  if (event.kind !== "instructions_updated") return null;
  const by = event.payload?.by;
  return { by: typeof by === "string" && by !== "" ? by : null };
}

/** An estimate, not a count: the usual four characters to a token. */
export function estimateTokens(text: string): number {
  return Math.ceil(text.length / 4);
}
