/** CAD-1817: the assistant's effective identity is a projection of one
 *  committed profile — the member's private IDENTITY/SOUL files or the
 *  workspace coordinator's protected IDENTITY/SOUL files. The UI never
 *  derives a name from a display coincidence, never translates a custom
 *  name, and never treats an unreadable projection as "unnamed".
 *
 *  Two actors exist and stay separate: the private assistant (personal
 *  scope, owns its own profile) and the workspace coordinator (workspace
 *  scope, shares the workspace's identity). A Master-backed pane is
 *  labelled from the coordinator only; the owner's private profile is
 *  never borrowed for it. */

import type { MasterState } from "../../lib/types";
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
  if (raw === null || typeof raw !== "object") throw new IdentityShapeError("identity is not an object");
  const row = raw as Record<string, unknown>;
  const name = nullableText(row.name);
  if (name === undefined) throw new IdentityShapeError("name is not text");
  const tone = nullableText(row.tone);
  if (tone === undefined) throw new IdentityShapeError("tone is not text");
  const look = row.look;
  if (!isAvatarLook(look)) throw new IdentityShapeError("look is not in the catalog");
  const revision = row.revision;
  if (scope === "personal") {
    if (typeof revision !== "number" || !Number.isInteger(revision) || revision < 0) {
      throw new IdentityShapeError("profile revision is not an integer");
    }
  } else if (typeof revision !== "string" || revision.trim() === "") {
    throw new IdentityShapeError("coordinator revision is not an opaque string");
  }
  return { name, tone, look, revision: revision as number | string };
}

/** The signed-in member's own identity, read from their private state.
 *  A missing or malformed `identity` is unavailable — never "unset" and
 *  never another actor's name. */
export function readPersonalIdentity(state: unknown): IdentityRead {
  if (state === null || typeof state !== "object") return { state: "unavailable" };
  const assistant = (state as { assistant?: unknown }).assistant;
  if (assistant === null || typeof assistant !== "object") return { state: "unavailable" };
  if (!("identity" in assistant)) return { state: "unavailable" };
  try {
    return { state: "ready", identity: decodeIdentity(assistant.identity, "personal") };
  } catch {
    return { state: "unavailable" };
  }
}

/** The workspace identity, read from the coordinator's own state. */
export function readWorkspaceIdentity(master: MasterState | null | undefined): IdentityRead {
  if (master === null || master === undefined) return { state: "unavailable" };
  try {
    return { state: "ready", identity: decodeIdentity(master.identity, "workspace") };
  } catch {
    return { state: "unavailable" };
  }
}

/** The visible name: the committed name verbatim (never translated), or
 *  the localized generic role label only while the name is unset. */
export function displayName(read: IdentityRead, scope: AssistantScope, t: (text: string) => string): string {
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
export function scopeDescriptor(scope: AssistantScope, t: (text: string) => string): string {
  return scope === "workspace"
    ? t("Workspace assistant · shared with this workspace")
    : t("Personal assistant · private to you");
}

/** Composer placeholder naming the actual actor. */
export function messagePlaceholder(read: IdentityRead, scope: AssistantScope, t: (text: string) => string): string {
  if (read.state === "ready" && read.identity.name !== null) return `${t("Message")} ${read.identity.name}…`;
  return scope === "workspace" ? t("Message the workspace assistant…") : t("Message your assistant…");
}

/** Composer accessible label naming the actual actor. */
export function messageLabel(read: IdentityRead, scope: AssistantScope, t: (text: string) => string): string {
  return `${t("Message")} ${displayName(read, scope, t)}`;
}
