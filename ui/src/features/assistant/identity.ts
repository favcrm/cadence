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
