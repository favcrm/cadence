import { describe, expect, it } from "bun:test";
import { assistantIdentityText } from "../src/assistant-identity.ts";
import type { AssistantIdentityContext } from "../src/contract.ts";

/** The established eleven-slug catalogue, in its established order. */
const AVATAR_LOOKS = [
  "builder",
  "guard",
  "bookkeeper",
  "content",
  "designer",
  "reception",
  "operations",
  "support",
  "tester",
  "robot",
  "custom",
] as const;

type PersonalContext = Extract<AssistantIdentityContext, { scope: "personal" }>;
type WorkspaceContext = Extract<
  AssistantIdentityContext,
  { scope: "workspace" }
>;

const personal = (
  overrides: {
    name?: string | null;
    tone?: string | null;
    look?: string;
    revision?: number;
    bootstrapState?: "pending" | "complete" | "skipped" | "deferred";
    userName?: string | null;
    preferences?: string | null;
    avatarLooks?: readonly string[];
  } = {},
): PersonalContext => ({
  scope: "personal",
  profile: {
    identity: {
      name: overrides.name ?? null,
      tone: overrides.tone ?? null,
      look: overrides.look ?? "builder",
      revision: overrides.revision ?? 0,
    },
    bootstrap_state: overrides.bootstrapState ?? "pending",
    user_name: overrides.userName ?? null,
    preferences: overrides.preferences ?? "concise",
  },
  avatarLooks: overrides.avatarLooks ?? AVATAR_LOOKS,
});

const workspace = (
  overrides: {
    name?: string | null;
    tone?: string | null;
    look?: string;
    revision?: string;
  } = {},
): WorkspaceContext => ({
  scope: "workspace",
  identity: {
    name: overrides.name ?? null,
    tone: overrides.tone ?? null,
    look: overrides.look ?? "robot",
    revision: overrides.revision ?? "0",
  },
});

describe("assistant identity context rendering", () => {
  it("renders the workspace identity without the actor's private fields", () => {
    const privateProfile = personal({
      userName: "SYNTHETIC-PRIVATE-ADDRESS",
      preferences: "SYNTHETIC-PRIVATE-PREFERENCES",
    }).profile;
    const text = assistantIdentityText({
      ...workspace(),
      profile: privateProfile,
    });
    expect(text).not.toContain("SYNTHETIC-PRIVATE-ADDRESS");
    expect(text).not.toContain("SYNTHETIC-PRIVATE-PREFERENCES");
  });

  it("refuses a context that does not have the declared shape", () => {
    const malformed = (value: unknown) =>
      assistantIdentityText(value as AssistantIdentityContext);

    expect(() => malformed({ scope: "team" })).toThrow(
      /identity context is malformed/i,
    );
    expect(() => malformed({ scope: "personal" })).toThrow(/malformed/i);
    expect(() =>
      malformed({
        scope: "personal",
        profile: {
          identity: {
            name: null,
            tone: null,
            look: "builder",
            revision: -1,
          },
          bootstrap_state: "pending",
          user_name: null,
          preferences: null,
        },
        avatarLooks: AVATAR_LOOKS,
      }),
    ).toThrow(/malformed/i);
    expect(() => malformed(personal({ revision: 1.5 }))).toThrow(/malformed/i);
    expect(() =>
      malformed(personal({ bootstrapState: "unknown" as never })),
    ).toThrow(/malformed/i);
    expect(() => malformed(personal({ name: 7 as never }))).toThrow(
      /malformed/i,
    );
    // The pinned look must be one the host's own catalogue exposes.
    expect(() => malformed(personal({ look: "dragon" }))).toThrow(/malformed/i);
    expect(() => malformed(personal({ avatarLooks: [] }))).toThrow(
      /malformed/i,
    );
    // A workspace revision is a nonnegative decimal string, not a number.
    expect(() =>
      malformed({
        scope: "workspace",
        identity: { name: null, tone: null, look: "robot", revision: 0 },
      }),
    ).toThrow(/malformed/i);
    expect(() => malformed(workspace({ revision: "01" }))).toThrow(
      /malformed/i,
    );
    expect(() => malformed(null)).toThrow(/malformed/i);
  });

  it("refuses a context whose rendered instructions are oversized", () => {
    expect(() =>
      assistantIdentityText(personal({ name: "a".repeat(70_000) })),
    ).toThrow(/identity context is too large/i);
  });
});
