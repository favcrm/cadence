/** CAD-1817: the assistant's effective identity is a projection of one
 *  committed profile — native IDENTITY/SOUL files or hosted actor-private
 *  storage. The UI never
 *  derives a name from a display coincidence, never translates a custom
 *  name, and never treats an unreadable projection as "unnamed".
 *
 *  Two actors exist and stay separate: the private assistant (personal
 *  scope, owns its own profile) and the workspace coordinator (workspace
 *  scope, shares the workspace's identity). Native Master panes retain the
 *  coordinator; hosted panes use their conversation-bound actor projection.
 *  An alias or a display-name coincidence never chooses the profile. */

import { AVATAR_LOOKS, agentLook, type AvatarLook } from "../agents/AgentAvatar";
import { MASTER } from "../home/master";

export type AssistantScope = "personal" | "workspace";

/** The committed identity fields both projections carry. `revision` is an
 *  integer profile revision for a member and an opaque digest revision for
 *  the coordinator. */
export interface IdentityRecord {
  name: string | null;
  tone: string | null;
  look: AvatarLook;
  revision: number | string;
}

/** What the surface may claim about the actor: still reading, refused or
 *  malformed (explicitly unavailable), or the committed identity. */
export type IdentityRead =
  | { state: "loading" }
  | { state: "unavailable" }
  | { state: "ready"; identity: IdentityRecord };

export { AVATAR_LOOKS };
export type { AvatarLook };

/** The coordinator's alias stays `master`; only its display name is
 *  workspace-owned. */
export const WORKSPACE_ALIAS = MASTER;

export function isAvatarLook(value: unknown): value is AvatarLook {
  return typeof value === "string" && (AVATAR_LOOKS as readonly string[]).includes(value);
}

/** Trimmed non-empty text, `null` for an explicitly unset field, and
 *  `undefined` for a value that is not text at all. */
function nullableText(value: unknown): string | null | undefined {
  if (value === null) return null;
  if (typeof value !== "string") return undefined;
  const trimmed = value.trim();
  return trimmed === "" ? null : trimmed;
}

export class IdentityShapeError extends Error {}

/** Decode the exact wire identity. A malformed payload throws so the
 *  caller can show the actor as unavailable instead of silently renaming
 *  it to the fallback. */
export function decodeIdentity(raw: unknown, scope: AssistantScope): IdentityRecord {
  if (raw === null || typeof raw !== "object"
    || !("name" in raw) || !("tone" in raw) || !("look" in raw) || !("revision" in raw)) {
    throw new IdentityShapeError("identity fields are missing");
  }
  const name = nullableText(raw.name);
  if (name === undefined) throw new IdentityShapeError("name is not text");
  const tone = nullableText(raw.tone);
  if (tone === undefined) throw new IdentityShapeError("tone is not text");
  const look = raw.look;
  if (!isAvatarLook(look)) throw new IdentityShapeError("look is not in the catalog");
  const revision = raw.revision;
  if (typeof revision !== "number" && typeof revision !== "string") {
    throw new IdentityShapeError("revision is not a number or string");
  }
  if (scope === "personal") {
    if (typeof revision !== "number" || !Number.isInteger(revision) || revision < 0) {
      throw new IdentityShapeError("profile revision is not an integer");
    }
  } else if (typeof revision !== "string" || revision.trim() === "") {
    throw new IdentityShapeError("coordinator revision is not an opaque string");
  }
  return { name, tone, look, revision };
}

/** The signed-in member's own identity, read from their private state.
 *  A missing or malformed `identity` is unavailable — never "unset" and
 *  never another actor's name. */
export function readPersonalIdentity(state: unknown): IdentityRead {
  if (state === null || typeof state !== "object" || !("assistant" in state)) return { state: "unavailable" };
  const assistant = state.assistant;
  if (assistant === null || typeof assistant !== "object") return { state: "unavailable" };
  if (!("identity" in assistant)) return { state: "unavailable" };
  try {
    return { state: "ready", identity: decodeIdentity(assistant.identity, "personal") };
  } catch {
    return { state: "unavailable" };
  }
}

export type PersonalProfileAction = "update" | "complete" | "defer" | "resume";

export interface PersonalProfileMutation {
  expected_revision: number;
  action: PersonalProfileAction;
  name?: string;
  look?: AvatarLook;
}

export type PersonalProfileOutcome =
  | {
      ok: true;
      identity: IdentityRecord;
      bootstrap: string | null;
      conversation: string | null;
      alias: string;
    }
  | {
      ok: false;
      unsupported?: true;
      conflict?: true;
      error: string;
    };

/** Read `assistant.bootstrap_state` from `POST /api/member-assistant/{state,profile}`. */
export function readPersonalBootstrap(state: unknown): string | null {
  if (state === null || typeof state !== "object" || !("assistant" in state)) return null;
  const assistant = state.assistant;
  if (assistant === null || typeof assistant !== "object" || !("bootstrap_state" in assistant)) return null;
  return typeof assistant.bootstrap_state === "string" ? assistant.bootstrap_state : null;
}

/** Execute and validate one fenced personal-profile mutation. Malformed 200
 *  payloads fail closed; 404/501 degrade cleanly when a board predates AOS-570;
 *  409 marks revision/state conflicts so callers refresh committed state. */
export async function mutatePersonalProfile(
  send: (req: PersonalProfileMutation) => Promise<unknown>,
  req: PersonalProfileMutation,
): Promise<PersonalProfileOutcome> {
  try {
    const raw = await send(req);
    if (raw === null || typeof raw !== "object" || !("applies" in raw) || raw.applies !== "next_turn") {
      return { ok: false, error: "The saved assistant identity response was not recognized." };
    }
    const parsed = readPersonalIdentity(raw);
    if (parsed.state !== "ready") {
      return { ok: false, error: "The saved assistant identity response was not recognized." };
    }
    const assistant = "assistant" in raw && raw.assistant !== null && typeof raw.assistant === "object"
      ? (raw.assistant as Record<string, unknown>)
      : null;
    const conversation = "conversation_id" in raw && typeof raw.conversation_id === "string"
      ? raw.conversation_id
      : null;
    const alias = assistant !== null && typeof assistant.alias === "string" && assistant.alias.trim() !== ""
      ? assistant.alias
      : "assistant";
    return {
      ok: true,
      identity: parsed.identity,
      bootstrap: readPersonalBootstrap(raw),
      conversation,
      alias,
    };
  } catch (error) {
    const status = error !== null && typeof error === "object" && "status" in error && typeof error.status === "number"
      ? error.status
      : null;
    if (status === 404 || status === 501) {
      return {
        ok: false,
        unsupported: true,
        error: "Direct assistant setup isn’t available on this board yet. Say hello below to name your assistant.",
      };
    }
    const message = error instanceof Error ? error.message : String(error);
    if (status === 409) {
      return { ok: false, conflict: true, error: message };
    }
    return { ok: false, error: message };
  }
}

/** The workspace identity, read from the coordinator's own state. */
export function readWorkspaceIdentity(master: unknown): IdentityRead {
  if (master === null || typeof master !== "object" || !("identity" in master)) return { state: "unavailable" };
  try {
    return { state: "ready", identity: decodeIdentity(master.identity, "workspace") };
  } catch {
    return { state: "unavailable" };
  }
}

export interface ConversationIdentityRead {
  scope: AssistantScope | null;
  read: IdentityRead;
}

/** A selected hosted conversation must not borrow Home's identity. Only an
 *  absent projection retains the native coordinator contract; malformed or
 *  foreign projections are explicitly unavailable. */
export function readConversationIdentity(
  master: unknown,
  conversation: string | null,
): ConversationIdentityRead {
  const unavailable: ConversationIdentityRead = { scope: null, read: { state: "unavailable" } };
  if (master === null || typeof master !== "object") return unavailable;
  if (!("actor_identity" in master)) {
    return { scope: "workspace", read: readWorkspaceIdentity(master) };
  }
  const projection = master.actor_identity;
  if (projection === null || typeof projection !== "object"
    || !("conversation_id" in projection) || !("scope" in projection) || !("identity" in projection)) return unavailable;
  if (conversation === null || conversation === "" || projection.conversation_id !== conversation) return unavailable;
  const scope = projection.scope;
  if (scope !== "personal" && scope !== "workspace") return unavailable;
  try {
    return { scope, read: { state: "ready", identity: decodeIdentity(projection.identity, scope) } };
  } catch {
    return { scope, read: { state: "unavailable" } };
  }
}

/** The visible name: the committed name verbatim (never translated), or
 *  the localized generic role label only while the name is unset. */
export function displayName(read: IdentityRead, scope: AssistantScope | null, t: (text: string) => string): string {
  if (read.state === "ready" && read.identity.name !== null) return read.identity.name;
  return scope === "workspace" ? t("Workspace assistant") : t("Assistant");
}

/** The avatar look: a pinned catalog look wins, then the stable alias
 *  assignment (the coordinator's robot). Display names never choose a
 *  face, so renaming never rerolls it. */
export function identityLook(read: IdentityRead, alias: string): AvatarLook {
  if (read.state === "ready") return agentLook(alias, { look: read.identity.look });
  return agentLook(alias);
}

/** The explicit ownership descriptor shown beside the actor's name. */
export function scopeDescriptor(scope: AssistantScope | null, t: (text: string) => string): string {
  if (scope === null) return "";
  return scope === "workspace"
    ? t("Workspace coordinator · workspace-owned")
    : t("Personal assistant · private to you");
}

/** Composer placeholder naming the actual actor. */
export function messagePlaceholder(read: IdentityRead, scope: AssistantScope | null, t: (text: string) => string): string {
  if (read.state === "ready" && read.identity.name !== null) return `${t("Message")} ${read.identity.name}…`;
  if (scope === null) return `${t("Message")} ${t("Assistant")}…`;
  return scope === "workspace" ? t("Message the workspace assistant…") : t("Message your assistant…");
}

/** Composer accessible label naming the actual actor. */
export function messageLabel(read: IdentityRead, scope: AssistantScope | null, t: (text: string) => string): string {
  return `${t("Message")} ${displayName(read, scope, t)}`;
}
