import { ApiError } from "../../lib/apiError";
import type { AvatarLook, StoredAgentProfile } from "../../lib/types";
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

/** CAD-2354: `workspace` is the company Master's text (owners), `personal` is
 *  the member's own assistant's (that member only). Both are fenced by a
 *  revision like a hosted profile, but live on their own routes. */
export type DocKind = "hosted" | "native" | "workspace" | "personal";
export type OwnKind = "workspace" | "personal";

export const isOwnKind = (kind: DocKind | null): kind is OwnKind => kind === "workspace" || kind === "personal";

/** The Master's or the member's instructions as one board read them. The
 *  workspace revision is a decimal string and the personal one a number; both
 *  stay as served and are only ever compared for equality. `identity` is the
 *  Master's current name, tone and look, which its write sends back whole. */
export interface OwnDoc {
  text: string;
  revision: string | number;
  identity?: { name: string | null; tone: string | null; look: AvatarLook };
}

/** The calls behind those two kinds. A board without the routes answers
 *  404/405/501, which `ownUnsupported` recognises. */
export interface OwnDeps {
  read: (kind: OwnKind) => Promise<OwnDoc>;
  /** Writes `text` over `base` and answers the committed document. */
  write: (kind: OwnKind, base: OwnDoc, text: string) => Promise<OwnDoc>;
}

/** An older board: the routes are not there, so nothing is editable. */
export function ownUnsupported(error: unknown): boolean {
  return error instanceof ApiError && (error.status === 404 || error.status === 405 || error.status === 501);
}

export interface DocDeps {
  readProfile: (id: string) => Promise<StoredAgentProfile>;
  writeProfile: (id: string, body: object) => Promise<StoredAgentProfile>;
  /** The stored text, or null when the board does not serve it. */
  readNative: (alias: string) => Promise<string | null>;
  writeNative: (alias: string, text: string) => Promise<{ applies: string }>;
}

/** What a save, or a check after a dropped one, came to. */
export type DocOutcome =
  | { kind: "saved"; text: string; profile: StoredAgentProfile | null; applies: string | null; own?: OwnDoc }
  /** Someone else's revision is stored. `profile` is theirs, when it was read. */
  | { kind: "conflict"; profile: StoredAgentProfile | null; own?: OwnDoc | null }
  /** The answer never arrived: the write may or may not have happened. */
  | { kind: "unconfirmed" }
  /** A check found the write did not happen. */
  /** `text` is what a native check read, so a different one can be shown. */
  | { kind: "not_saved"; profile: StoredAgentProfile | null; text?: string | null; own?: OwnDoc }
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
  // Whitespace-only clears the Master's or the member's text; a profile cannot be empty.
  if (trimmed.length === 0 && !isOwnKind(kind)) return "Write something to save. The board does not keep an empty document.";
  if (trimmed.length > HOSTED_MAX_UNITS) return `Too long: ${trimmed.length.toLocaleString("en")} of ${HOSTED_MAX_UNITS.toLocaleString("en")} characters.`;
  if (CONTROL.test(trimmed)) return "Contains a control character the board refuses.";
  return null;
}

/** A size line for the save bar; only near the limit, or over it. */
export function sizeNote(kind: DocKind, text: string): string | null {
  const [used, max, unit] =
    kind !== "native" ? [text.trim().length, HOSTED_MAX_UNITS, "characters"] : [byteLength(text), NATIVE_MAX_BYTES, "bytes"];
  return used >= max * 0.9 ? `${used.toLocaleString("en")} of ${max.toLocaleString("en")} ${unit}` : null;
}

/** What the board would store for this draft. The hosted board trims. */
export function stored(kind: DocKind, draft: string): string {
  return kind !== "native" ? draft.trim() : draft;
}

/** Whether saving this draft would change anything. */
export function isDirty(kind: DocKind, draft: string | null, served: string): boolean {
  return draft !== null && stored(kind, draft) !== served;
}

/** The hosted update body: exactly the keys the board accepts, from the profile
 *  as it was read. `model` is left out so the stored one stays. */
export function profileBody(
  from: StoredAgentProfile,
  text: string,
  expectedRevision: number,
  skills = from.skills,
  enabledTools?: string[],
): object {
  return {
    name: from.name,
    role: from.role,
    responsibilities: text,
    skills,
    status: from.status,
    expectedRevision,
    // AOS-679: sent only when changed; absent keeps the stored opt-in groups.
    ...(enabledTools === undefined ? {} : { enabled_tools: enabledTools }),
  };
}

/**
 * How a failed write reads. A refusal (4xx) means nothing was written. A 409 is
 * the revision fence, which only a hosted profile has: on native it is just a
 * refusal, so no use-theirs or save-mine is offered. Anything else - no answer,
 * a 5xx, a session that changed under the request - leaves it unknown whether
 * the write landed.
 */
export function classifyFailure(error: unknown, kind: DocKind): "conflict" | "unconfirmed" | "refused" {
  if (!(error instanceof ApiError)) return "unconfirmed";
  if (error.status === 409) {
    return kind !== "native" && (error.code === undefined || error.code === "revision_conflict") ? "conflict" : "refused";
  }
  if (error.status >= 500 || error.status === 0 || error.status === 408) return "unconfirmed";
  return "refused";
}

function failure(error: unknown, kind: DocKind): DocOutcome {
  const out = classifyFailure(error, kind);
  if (out === "conflict") return { kind: out, profile: null };
  if (out === "unconfirmed") return { kind: out };
  return { kind: out, message: error instanceof Error ? error.message : String(error) };
}

/** Save to a hosted profile at the revision `base` was read at. On a 409 the
 *  board's current profile is read and returned, so "Save mine" writes at THAT
 *  revision: a newer one gets another 409 instead of being overwritten. */
export async function saveHosted(deps: DocDeps, base: StoredAgentProfile, text: string): Promise<DocOutcome> {
  try {
    const profile = await deps.writeProfile(base.id, profileBody(base, text, base.revision));
    return { kind: "saved", text: profile.responsibilities, profile, applies: null };
  } catch (error) {
    const out = failure(error, "hosted");
    if (out.kind !== "conflict") return out;
    try {
      return { kind: "conflict", profile: await deps.readProfile(base.id) };
    } catch {
      return out;
    }
  }
}

/** Save the Master's or the member's text at the revision `base` was read at.
 *  On a 409 the board's current document is read and returned, so "Save mine"
 *  writes at THAT revision and a newer one is refused again, not overwritten. */
export async function saveOwn(deps: OwnDeps, kind: OwnKind, base: OwnDoc, text: string): Promise<DocOutcome> {
  try {
    const own = await deps.write(kind, base, text);
    return { kind: "saved", text: own.text, profile: null, applies: null, own };
  } catch (error) {
    const out = failure(error, kind);
    if (out.kind !== "conflict") return out;
    try {
      return { kind: "conflict", profile: null, own: await deps.read(kind) };
    } catch {
      return out;
    }
  }
}

/** After a dropped save: read what is stored and say whether it is ours. A
 *  revision different from the one we wrote at, holding our text, is our save. */
export async function checkOwn(deps: OwnDeps, kind: OwnKind, base: OwnDoc, text: string): Promise<DocOutcome> {
  try {
    const latest = await deps.read(kind);
    if (latest.revision === undefined || typeof latest.text !== "string") throw new Error("unusable read");
    if (String(latest.revision) === String(base.revision)) return { kind: "not_saved", profile: null, own: latest };
    if (latest.text === text.trim()) return { kind: "saved", text: latest.text, profile: null, applies: null, own: latest };
    return { kind: "conflict", profile: null, own: latest };
  } catch {
    return { kind: "unconfirmed" };
  }
}

/** Native has no revision: the text replaces whatever is stored. */
export async function saveNative(deps: DocDeps, alias: string, text: string): Promise<DocOutcome> {
  try {
    const out = await deps.writeNative(alias, text);
    return { kind: "saved", text, profile: null, applies: out.applies };
  } catch (error) {
    return failure(error, "native");
  }
}

/** After a dropped save: read what is stored and say whether it is ours. */
export async function checkHosted(deps: DocDeps, base: StoredAgentProfile, text: string): Promise<DocOutcome> {
  // A read that answers with something unusable tells nothing: unconfirmed.
  try {
    const latest = await deps.readProfile(base.id);
    if (typeof latest.revision !== "number" || typeof latest.responsibilities !== "string") throw new Error("unusable read");
    if (latest.revision > base.revision && latest.responsibilities === text.trim()) {
      return { kind: "saved", text: latest.responsibilities, profile: latest, applies: null };
    }
    if (latest.revision === base.revision) return { kind: "not_saved", profile: latest };
    return { kind: "conflict", profile: latest };
  } catch {
    return { kind: "unconfirmed" };
  }
}

export async function checkNative(deps: DocDeps, alias: string, text: string): Promise<DocOutcome> {
  let latest: string | null;
  try {
    latest = await deps.readNative(alias);
  } catch {
    return { kind: "unconfirmed" };
  }
  // Not served is not "different": nothing was learned, so it stays unconfirmed.
  if (latest === null) return { kind: "unconfirmed" };
  if (latest === text) return { kind: "saved", text, profile: null, applies: null };
  return { kind: "not_saved", profile: null, text: latest };
}

/** The toast after a save, saying when the agent sees the new text. A turn
 *  already running is never changed. `checked` is a save found by a check
 *  after the answer was lost, so the board never said when it applies. Each
 *  sentence goes through `t` on its own. */
export function savedText(kind: DocKind, applies: string | null, checked: boolean, t: (english: string) => string): string {
  const lead = t(checked ? "It did save." : "Saved.");
  if (kind === "hosted") return `${lead} ${t("It applies when a session next loads this profile.")} ${t("A running turn isn’t changed.")}`;
  // The board does not say whether its runtime renders these yet, so no promise of when.
  if (isOwnKind(kind)) return `${lead} ${t("A running turn isn’t changed.")}`;
  if (applies === null) return `${lead} ${t("The board did not say when it applies.")}`;
  const when = t(appliesText(applies));
  return applies === "next_turn" || applies === "next_launch"
    ? `${lead} ${when}. ${t("A turn already running keeps the text it started with.")}`
    : `${when}.`;
}

/** Which way, if any, this agent's instructions are edited. Hosted edits the
 *  profile; native overwrites a runtime's text, except for the master and an
 *  agent whose job comes from a settings profile. */
export function instructionsKind(a: {
  hired: boolean;
  hostedBoard: boolean;
  served: string | null;
  master: boolean;
  profileBound: boolean;
  /** CAD-2354: the hosted page this is, when it is the Master's (owner) or the
   *  member's own assistant's. */
  own?: OwnKind | null;
}): DocKind | null {
  if (a.own) return a.hostedBoard ? a.own : null;
  if (a.hired) return a.hostedBoard ? "hosted" : null;
  return a.served !== null && !a.master && !a.profileBound ? "native" : null;
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

/** A line of the Context drawer: what loads, and an estimate where one is honest. */
export interface ContextLine {
  name: string;
  /** English with `{n}` standing for `count`, so it can be translated. */
  size: string;
  count?: number;
}

/** What loads, said only where it is true. Only the document is measured, and
 *  only as an estimate; the rest is named and not counted. Hosted conversations
 *  are not bound to a profile, and an agent whose last save was not delivered
 *  does not read the text, so neither is told "every session starts with" it. */
export function contextOf(text: string, kind: DocKind | null, applies: string | null): { heading: string; lines: ContextLine[] } {
  const rest: ContextLine[] = [
    { name: "Platform and workspace rules", size: "Not counted" },
    { name: "Skill names and descriptions", size: "Not counted" },
    { name: "Tool definitions", size: "Not counted" },
  ];
  if (isOwnKind(kind)) {
    // Built-in text is never shown, so it is named and not counted.
    return {
      heading: "Made up of",
      lines: [
        { name: "Built-in instructions", size: "Not counted" },
        { name: kind === "workspace" ? "Owner-managed instructions" : "Your instructions", size: "About {n} tokens (estimate)", count: estimateTokens(text) },
        ...rest,
      ],
    };
  }
  if (kind === "native" && applies === "not_delivered") return { heading: "Loaded at session start", lines: rest };
  const own: ContextLine = { name: "Instructions", size: "About {n} tokens (estimate)", count: estimateTokens(text) };
  return {
    heading: kind === "hosted" ? "A session that loads this profile starts with" : "Every session starts with",
    lines: [own, ...rest],
  };
}
