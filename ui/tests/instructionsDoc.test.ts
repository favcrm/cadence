/**
 * CAD-2091: how the Instructions document is saved. Hosted is fenced on the
 * profile revision; native overwrites. Each outcome is a different promise to
 * the person editing, so each is driven here against a fake board.
 */
import { ApiError } from "../src/lib/apiError";
import type { StoredAgentProfile } from "../src/lib/types";
import {
  checkHosted,
  checkNative,
  classifyFailure,
  isDirty,
  nativeChange,
  profileBody,
  saveHosted,
  saveNative,
  saveOverTheirs,
  savedText,
  sizeNote,
  textProblem,
  type DocDeps,
} from "../src/features/agents/instructionsDoc";

function check(condition: boolean, message: string): void {
  if (!condition) throw new Error(`instructionsDoc: ${message}`);
}

const profile = (over: Partial<StoredAgentProfile> = {}): StoredAgentProfile => ({
  id: "ap_1",
  name: "ads-agent",
  role: "Ads",
  responsibilities: "# Ads\n\nOld text.",
  skills: ["cadence"],
  status: "active",
  revision: 3,
  created_at: 1,
  updated_at: 2,
  created_by: "owner",
  catalogue: "ads",
  model: "inherit",
  ...over,
});

interface Calls {
  writes: { id: string; body: Record<string, unknown> }[];
  reads: number;
}
function deps(over: Partial<DocDeps> = {}): { deps: DocDeps; calls: Calls } {
  const calls: Calls = { writes: [], reads: 0 };
  return {
    calls,
    deps: {
      readProfile: async () => {
        calls.reads += 1;
        return profile();
      },
      writeProfile: async (id, body) => {
        calls.writes.push({ id, body: body as Record<string, unknown> });
        return profile({ revision: 4, responsibilities: String((body as { responsibilities: string }).responsibilities).trim() });
      },
      readNative: async () => "native text",
      writeNative: async () => ({ applies: "next_turn" }),
      ...over,
    },
  };
}

async function main() {
  // The update body carries exactly the keys the board accepts, no model, and
  // the revision the draft STARTED from.
  const body = profileBody(profile(), "new", 3) as Record<string, unknown>;
  check(
    JSON.stringify(Object.keys(body).sort()) === JSON.stringify(["expectedRevision", "name", "responsibilities", "role", "skills", "status"]),
    "hosted body keys",
  );
  check(body.expectedRevision === 3 && !("model" in body) && !("id" in body), "revision from the draft's base, model left to the board");

  // Outcome classification.
  check(classifyFailure(new ApiError("stale", 409)) === "conflict", "409 is the fence");
  check(classifyFailure(new ApiError("no", 403)) === "refused", "403 is a refusal");
  check(classifyFailure(new ApiError("no", 400)) === "refused", "400 is a refusal");
  check(classifyFailure(new ApiError("down", 503)) === "unconfirmed", "5xx may have committed");
  check(classifyFailure(new TypeError("Failed to fetch")) === "unconfirmed", "a dropped connection is not confirmed");
  check(classifyFailure(new Error("Session changed during request")) === "unconfirmed", "a changed session is not confirmed");

  // Save: success replaces the draft with what the board stored (it trims).
  {
    const { deps: d, calls } = deps();
    const out = await saveHosted(d, profile(), "  mine \n\n");
    check(out.kind === "saved" && out.text === "mine", "the stored, trimmed text comes back");
    check(calls.writes.length === 1 && calls.writes[0]!.body.expectedRevision === 3, "one write at the draft's revision");
  }
  // A stale revision is a conflict and writes nothing more.
  {
    const { deps: d, calls } = deps({
      writeProfile: async () => {
        throw new ApiError("Reload the agent profile.", 409);
      },
    });
    const out = await saveHosted(d, profile(), "mine");
    check(out.kind === "conflict", "stale revision reads as a conflict");
    check(calls.writes.length === 0 && calls.reads === 0, "a conflict neither retries nor overwrites");
  }
  // A dropped connection is never success.
  {
    const { deps: d } = deps({
      writeProfile: async () => {
        throw new TypeError("Failed to fetch");
      },
    });
    const out = await saveHosted(d, profile(), "mine");
    check(out.kind === "unconfirmed", "a dropped save is not confirmed");
  }
  // Save mine: THEIR other fields survive, at THEIR revision, with MY text.
  {
    const theirs = profile({ revision: 7, name: "ads-agent-renamed", skills: ["cadence", "website-builder"], responsibilities: "theirs" });
    const { deps: d, calls } = deps({ readProfile: async () => theirs });
    const out = await saveOverTheirs(d, "ap_1", "mine");
    check(out.kind === "saved", "save mine lands");
    const sent = calls.writes[0]!.body;
    check(sent.expectedRevision === 7 && sent.name === "ads-agent-renamed" && sent.responsibilities === "mine", "save mine rides their revision and fields");
    check(JSON.stringify(sent.skills) === JSON.stringify(["cadence", "website-builder"]), "their skills survive");
  }
  {
    const { deps: d, calls } = deps({
      readProfile: async () => {
        throw new TypeError("offline");
      },
    });
    const out = await saveOverTheirs(d, "ap_1", "mine");
    check(out.kind === "refused" && calls.writes.length === 0, "an unreadable current version writes nothing");
  }

  // Check after a dropped save.
  {
    const base = profile();
    const said = async (latest: StoredAgentProfile) => checkHosted(deps({ readProfile: async () => latest }).deps, base, "  mine ");
    check((await said(profile({ revision: 4, responsibilities: "mine" }))).kind === "saved", "newer revision holding my trimmed text did save");
    check((await said(profile())).kind === "not_saved", "same revision did not save");
    check((await said(profile({ revision: 4, responsibilities: "someone else" }))).kind === "conflict", "newer revision with other text is theirs");
    const unreachable = await checkHosted(
      deps({
        readProfile: async () => {
          throw new TypeError("offline");
        },
      }).deps,
      base,
      "mine",
    );
    check(unreachable.kind === "unconfirmed", "an unreachable check stays unconfirmed");
  }
  check((await checkNative(deps().deps, "ads", "native text")).kind === "saved", "native check: same text did save");
  check((await checkNative(deps().deps, "ads", "other")).kind === "not_saved", "native check: different text did not");

  // Native overwrites and says when it applies.
  {
    const out = await saveNative(deps().deps, "ads", "text");
    check(out.kind === "saved" && out.applies === "next_turn", "native save reports when it applies");
    const dropped = await saveNative(
      deps({
        writeNative: async () => {
          throw new TypeError("offline");
        },
      }).deps,
      "ads",
      "text",
    );
    check(dropped.kind === "unconfirmed", "native dropped save is not confirmed");
  }

  // Limits and dirtiness. Hosted trims and refuses empty; native counts bytes.
  check(textProblem("hosted", "   ") !== null, "hosted refuses empty");
  check(textProblem("hosted", "x".repeat(8001)) !== null && textProblem("hosted", "x".repeat(8000)) === null, "hosted caps at 8,000");
  check(textProblem("hosted", "a\u0007b") !== null && textProblem("hosted", "a\tb\r\n") === null, "hosted control characters");
  check(textProblem("native", "é".repeat(16_001)) !== null && textProblem("native", "é".repeat(16_000)) === null, "native counts bytes");
  check(sizeNote("hosted", "x".repeat(100)) === null && sizeNote("hosted", "x".repeat(7300)) !== null, "size shows near the limit only");
  check(!isDirty("hosted", "same\n\n", "same") && isDirty("native", "same\n\n", "same"), "hosted trims, native does not");
  check(!isDirty("native", null, "same"), "no draft is not a change");

  // When it applies: a running turn is never changed.
  check(/running turn/.test(savedText("hosted", null)), "hosted says a running turn is not changed");
  check(/next turn/.test(savedText("native", "next_turn")) && /already running/.test(savedText("native", "next_turn")), "native next turn");
  check(!/already running/.test(savedText("native", "not_delivered")) && /does not read it yet/.test(savedText("native", "not_delivered")), "not delivered is plain");
  check(/did save/.test(savedText("native", null, true)), "a checked save says so");

  // Who changed it, where the board records it.
  check(nativeChange({ kind: "instructions_updated", at: "x", payload: { by: "operator" } })?.by === "operator", "native event carries who");
  check(nativeChange({ kind: "instructions_updated", at: "x" })?.by === null, "no payload: not recorded");
  check(nativeChange({ kind: "turn_started", at: "x" }) === null, "other events are not edits");
  console.log("instructionsDoc checks passed");
}
void main().catch((e: unknown) => {
  console.error(e);
  (globalThis as unknown as { process: { exit(n: number): never } }).process.exit(1);
});
