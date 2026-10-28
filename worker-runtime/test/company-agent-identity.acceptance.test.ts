import { describe, expect, it } from "bun:test";
import { assistantIdentityText } from "../src/assistant-identity.ts";
import type { AssistantIdentityContext } from "../src/contract.ts";

/**
 * CAD-1907 acceptance check. Authored independently of the implementer.
 * Copy this file unedited. Do not weaken a refusal or replace the renderer.
 *
 * Proves the real `assistantIdentityText` renderer refuses a malformed
 * `company_agent` context and does not fall back to a workspace or personal
 * prompt. A valid context renders the job and nothing private.
 */

const MALFORMED = /committed assistant identity context is malformed/i;

function company(identity: Record<string, unknown>): AssistantIdentityContext {
  return { scope: "company_agent", identity } as AssistantIdentityContext;
}

describe("company agent identity refusal", () => {
  it("refuses a malformed company_agent and renders only a valid job", () => {
    const refuse = (identity: Record<string, unknown>) =>
      assistantIdentityText(company(identity));
    const base = {
      name: "Ledger",
      look: "bookkeeper",
      responsibilities: "Reconcile invoices.\nFlag discrepancies.",
      revision: "0",
    };

    expect(() => assistantIdentityText(company({}))).toThrow(MALFORMED);
    expect(() => refuse({ ...base, look: "" })).toThrow(MALFORMED);
    expect(() => refuse({ ...base, look: 7 })).toThrow(MALFORMED);
    expect(() => refuse({ ...base, revision: 0 })).toThrow(MALFORMED);
    expect(() => refuse({ ...base, revision: "01" })).toThrow(MALFORMED);
    expect(() => refuse({ ...base, revision: "" })).toThrow(MALFORMED);
    expect(() => refuse({ ...base, revision: "-1" })).toThrow(MALFORMED);
    expect(() => refuse({ ...base, responsibilities: "" })).toThrow(MALFORMED);
    expect(() => refuse({ ...base, responsibilities: "  \n\t  " })).toThrow(
      MALFORMED,
    );
    expect(() =>
      refuse({ ...base, responsibilities: `ok\u0000` }),
    ).toThrow(MALFORMED);
    expect(() => refuse({ ...base, responsibilities: "ok\r\nmore" })).toThrow(
      MALFORMED,
    );
    expect(() =>
      refuse({ ...base, responsibilities: "a".repeat(2001) }),
    ).toThrow(MALFORMED);
    expect(() => refuse({ ...base, responsibilities: 12 })).toThrow(MALFORMED);

    const text = assistantIdentityText(
      company({
        ...base,
        name: null,
        responsibilities: "  Reconcile invoices.\nFlag discrepancies.  ",
        user_name: "SYNTHETIC-PRIVATE-ADDRESS",
        preferences: "SYNTHETIC-PRIVATE-PREFERENCES",
      }),
    );
    expect(text).toContain("## Company agent identity (operator-managed)");
    expect(text).toContain("name: (unset)");
    expect(text).toContain("look: bookkeeper");
    expect(text).toContain("revision: 0");
    expect(text).toContain("Reconcile invoices.\nFlag discrepancies.");
    expect(text).not.toContain("SYNTHETIC-PRIVATE-ADDRESS");
    expect(text).not.toContain("SYNTHETIC-PRIVATE-PREFERENCES");
    expect(text).not.toContain("## Workspace assistant identity");
    expect(text).not.toContain("## Your private assistant profile");
  });
});
