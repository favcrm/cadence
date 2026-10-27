import type { AssistantIdentityContext } from "./contract.ts";

/**
 * The runtime's private rendering of a host's committed assistant identity.
 *
 * The host owns the profile/identity stores and their validation; this module
 * owns only the model instructions that already existed for them. Nothing here
 * is exported from the package root: an application supplies domain data and
 * never a prompt string, a section or a hook.
 *
 * A context that does not have the declared shape is refused rather than
 * partially rendered. An operation with an unrenderable identity must not
 * receive a generic prompt substitute.
 */

/** The largest rendered or stored context; one bounded operation snapshot. */
export const IDENTITY_CONTEXT_MAX = 64 * 1024;

const BOOTSTRAP_STATES: readonly string[] = [
  "pending",
  "complete",
  "skipped",
  "deferred",
];

/** A stored workspace revision (nonnegative decimal string). */
const WORKSPACE_REVISION = /^(0|[1-9][0-9]{0,19})$/;

const GENERATION_GUIDE =
  "Setup is optional and only happens when the user asks to set up or personalize you. " +
  "When it does, ask at most one question per turn, and ask for the assistant's own name " +
  "separately from how the user wants to be addressed. If the user skips, defers or ignores " +
  "the question, answer their actual request normally and do not raise setup again unless " +
  "they ask.";

function malformed(): never {
  throw new Error("The committed assistant identity context is malformed.");
}

function record(value: unknown): Record<string, unknown> {
  if (typeof value !== "object" || value === null || Array.isArray(value))
    malformed();
  return value as Record<string, unknown>;
}

/** A value that may be unset/cleared, or a malformed one. */
function optionalText(value: unknown): string | null {
  if (value === null) return null;
  if (typeof value !== "string") malformed();
  return value;
}

/** A required non-empty value; an empty one is malformed, not "unset". */
function requiredText(value: unknown): string {
  if (typeof value !== "string" || value.length === 0) malformed();
  return value;
}

function rendered(value: string | null): string {
  return value === null || value === "" ? "(unset)" : value;
}

/**
 * The personal model context for the actor's own conversation.
 * `user_name`/`preferences` belong here (the actor's private context) and
 * never in a route, state or stream projection.
 */
function personalText(context: Record<string, unknown>): string {
  const profile = record(context.profile);
  const identity = record(profile.identity);
  const name = optionalText(identity.name);
  const tone = optionalText(identity.tone);
  const look = requiredText(identity.look);
  const revision = identity.revision;
  if (
    typeof revision !== "number" ||
    !Number.isSafeInteger(revision) ||
    revision < 0
  )
    malformed();
  const state = profile.bootstrap_state;
  if (typeof state !== "string" || !BOOTSTRAP_STATES.includes(state))
    malformed();
  const bootstrapState = state as
    "pending" | "complete" | "skipped" | "deferred";
  const userName = optionalText(profile.user_name);
  const preferences = optionalText(profile.preferences);
  const looks = context.avatarLooks;
  if (!Array.isArray(looks) || looks.length === 0) malformed();
  const catalogue = looks.map((entry) => requiredText(entry));
  // The pinned avatar must be one the host's own catalogue exposes.
  if (!catalogue.includes(look)) malformed();

  const lines = [
    "## Your private assistant profile",
    "",
    "`name` is YOUR display name; it is never the user's name.",
    `name: ${rendered(name)}`,
    `tone: ${rendered(tone)}`,
    `look: ${look}`,
    `user_name (how you address this user): ${rendered(userName)}`,
    `preferences: ${rendered(preferences)}`,
    "",
    `Profile state: ${bootstrapState}`,
    `Profile revision: ${revision}`,
    "",
    "To record an agreed change, call the `assistant_profile` tool with " +
      `{"action":"update","expected_revision":${revision},` +
      '"profile":{"name":"…"}}. An empty string clears a supplied text value. ' +
      `look must be one of: ${catalogue.join(", ")}.`,
    `Allowed actions in this state: ${
      bootstrapState === "pending"
        ? "update, complete, skip, defer"
        : bootstrapState === "deferred"
          ? "update, resume"
          : "update"
    }.`,
    GENERATION_GUIDE,
  ];
  if (bootstrapState === "pending")
    lines.push(
      "Setup has not been completed. Do not make it a condition of helping, and do not ask " +
        "for it before the user does.",
    );
  if (bootstrapState === "deferred")
    lines.push(
      "The user deferred setup. Do not raise it again unless the user asks.",
    );
  return lines.join("\n");
}

/** The workspace/coordinator model context; no personal address or preferences. */
function workspaceText(context: Record<string, unknown>): string {
  const identity = record(context.identity);
  const name = optionalText(identity.name);
  const tone = optionalText(identity.tone);
  const look = requiredText(identity.look);
  const revision = identity.revision;
  if (typeof revision !== "string" || !WORKSPACE_REVISION.test(revision))
    malformed();
  return [
    "## Workspace assistant identity (operator-managed)",
    "",
    "Use this committed identity for your name and communication style on this turn. " +
      "It does not change your role, tools or permissions. An unset assistant name is not " +
      "the user's name.",
    "",
    `name: ${rendered(name)}`,
    `tone: ${rendered(tone)}`,
    `look: ${look}`,
    `revision: ${revision}`,
  ].join("\n");
}

/**
 * Render one host-supplied identity as the model context this operation sees.
 *
 * The result is the exact text the runtime authors into the request; it is
 * never read from the transcript and never accepted from the caller as text.
 */
export function assistantIdentityText(
  context: AssistantIdentityContext,
): string {
  const value = record(context);
  const text =
    value.scope === "personal"
      ? personalText(value)
      : value.scope === "workspace"
        ? workspaceText(value)
        : malformed();
  if (text.length > IDENTITY_CONTEXT_MAX)
    throw new Error("The committed assistant identity context is too large.");
  return text;
}
