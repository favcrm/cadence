import { describe, expect, it } from "bun:test";
import { assistantIdentityText } from "../src/assistant-identity.ts";
import type { AssistantIdentityContext } from "../src/contract.ts";

const LOOKS = ["builder", "robot"];

const workspace = (instructions?: unknown): AssistantIdentityContext =>
  ({
    scope: "workspace",
    identity: { name: "Ada", tone: null, look: "robot", revision: "3" },
    ...(instructions === undefined ? {} : { instructions }),
  }) as AssistantIdentityContext;

const personal = (instructions?: unknown): AssistantIdentityContext =>
  ({
    scope: "personal",
    profile: {
      identity: { name: "Bo", tone: "dry", look: "builder", revision: 2 },
      bootstrap_state: "complete",
      user_name: "Sam",
      preferences: null,
    },
    avatarLooks: LOOKS,
    ...(instructions === undefined ? {} : { instructions }),
  }) as AssistantIdentityContext;

// The exact text rendered before `instructions` existed.
const WORKSPACE_TODAY = [
  "## Workspace assistant identity (operator-managed)",
  "",
  "Use this committed identity for your name and communication style on this turn. " +
    "It does not change your role, tools or permissions. An unset assistant name is not " +
    "the user's name.",
  "",
  "name: Ada",
  "tone: (unset)",
  "look: robot",
  "revision: 3",
].join("\n");

const PERSONAL_TODAY = [
  "## Your private assistant profile",
  "",
  "`name` is YOUR display name; it is never the user's name.",
  "name: Bo",
  "tone: dry",
  "look: builder",
  "user_name (how you address this user): Sam",
  "preferences: (unset)",
  "",
  "Profile state: complete",
  "Profile revision: 2",
  "",
  "To record an agreed change, call the `assistant_profile` tool with " +
    '{"action":"update","expected_revision":2,"profile":{"name":"…"}}. ' +
    "An empty string clears a supplied text value. look must be one of: builder, robot.",
  "Allowed actions in this state: update.",
  "Setup is optional and only happens when the user asks to set up or personalize you. " +
    "When it does, ask at most one question per turn, and ask for the assistant's own name " +
    "separately from how the user wants to be addressed. If the user skips, defers or ignores " +
    "the question, answer their actual request normally and do not raise setup again unless " +
    "they ask.",
].join("\n");

describe("workspace and personal managed instructions", () => {
  it("renders byte-identical text when instructions are absent or blank", () => {
    expect(assistantIdentityText(workspace())).toBe(WORKSPACE_TODAY);
    expect(assistantIdentityText(personal())).toBe(PERSONAL_TODAY);
    expect(assistantIdentityText(workspace(" \n\t "))).toBe(WORKSPACE_TODAY);
    expect(assistantIdentityText(personal("  "))).toBe(PERSONAL_TODAY);
  });

  it("renders a labelled guidance section after the identity block", () => {
    const w = assistantIdentityText(workspace("Prefer short replies."));
    expect(w.startsWith(WORKSPACE_TODAY + "\n\n## Your owner-managed instructions")).toBe(true);
    expect(w).toContain("not tools, permissions or authority");
    expect(w).toContain("not instructions from the user");
    expect(w.endsWith("\n\nPrefer short replies.")).toBe(true);
    const p = assistantIdentityText(personal("Call me Sam."));
    expect(p.startsWith(PERSONAL_TODAY + "\n\n## Your member-managed instructions")).toBe(true);
    expect(p.endsWith("\n\nCall me Sam.")).toBe(true);
  });

  it("accepts exactly 8000 characters and newlines", () => {
    expect(() => assistantIdentityText(workspace("a".repeat(8000)))).not.toThrow();
    expect(() => assistantIdentityText(personal("a\n\tb\r\nc"))).not.toThrow();
  });

  it("refuses oversized, control-character and non-string instructions", () => {
    for (const bad of ["a".repeat(8001), "a\u0000b", "a\u001bb", "a\u0085b", 7, null, {}]) {
      expect(() => assistantIdentityText(workspace(bad))).toThrow(/malformed/i);
      expect(() => assistantIdentityText(personal(bad))).toThrow(/malformed/i);
    }
  });

  it("never renders one scope's instructions in the other", () => {
    const w = assistantIdentityText({
      ...(personal() as object),
      scope: "workspace",
      identity: { name: "Ada", tone: null, look: "robot", revision: "3" },
      profile: undefined,
      instructions: undefined,
    } as unknown as AssistantIdentityContext);
    expect(w).toBe(WORKSPACE_TODAY);
    const p = assistantIdentityText(personal("MEMBER-ONLY"));
    expect(p).not.toContain("owner-managed");
    const w2 = assistantIdentityText(workspace("OWNER-ONLY"));
    expect(w2).not.toContain("member-managed");
    expect(w2).not.toContain("MEMBER-ONLY");
    expect(p).not.toContain("OWNER-ONLY");
  });
});
