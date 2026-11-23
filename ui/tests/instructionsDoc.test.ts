/**
 * CAD-2091: how the Instructions document is saved. Hosted is fenced on the
 * profile revision; native overwrites. Each outcome is a different promise to
 * the person editing, so each is driven here against a fake board.
 */
import { translateUiText } from "../src/lib/locale";
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
  contextOf,
  instructionsKind,
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

const id = (english: string) => english;

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
  check(classifyFailure(new ApiError("stale", 409), "hosted") === "conflict", "409 is the fence");
  check(classifyFailure(new ApiError("stale", 409), "native") === "refused", "a native 409 is no revision conflict: no use-theirs or save-mine");
  check(classifyFailure(new ApiError("x", 409, { code: "alias_taken" }), "hosted") === "refused", "a 409 that is not the revision fence is a refusal");
  check(classifyFailure(new ApiError("no", 403), "hosted") === "refused", "403 is a refusal");
  check(classifyFailure(new ApiError("no", 400), "hosted") === "refused", "400 is a refusal");
  check(classifyFailure(new ApiError("down", 503), "hosted") === "unconfirmed", "5xx may have committed");
  check(classifyFailure(new TypeError("Failed to fetch"), "hosted") === "unconfirmed", "a dropped connection is not confirmed");
  check(classifyFailure(new Error("Session changed during request"), "hosted") === "unconfirmed", "a changed session is not confirmed");

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
    check(out.kind === "conflict" && out.profile?.revision === 3, "stale revision reads as a conflict and carries their profile");
    check(calls.writes.length === 0 && calls.reads === 1, "a conflict reads theirs once and never overwrites");
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
  // Save mine: THEIR other fields survive, at THEIR revision, with MY text; a
  // revision newer than the one that raised the conflict gets another 409.
  {
    const theirs = profile({ revision: 7, name: "ads-agent-renamed", skills: ["cadence", "website-builder"], responsibilities: "theirs" });
    const { deps: d, calls } = deps({ readProfile: async () => theirs });
    const out = await saveHosted(d, theirs, "mine");
    check(out.kind === "saved", "save mine lands");
    const sent = calls.writes[0]!.body;
    check(sent.expectedRevision === 7 && sent.name === "ads-agent-renamed" && sent.responsibilities === "mine", "save mine rides their revision and fields");
    check(JSON.stringify(sent.skills) === JSON.stringify(["cadence", "website-builder"]), "their skills survive");
  }
  {
    let boardRevision = 8;
    const { deps: d, calls } = deps({
      readProfile: async () => profile({ revision: boardRevision }),
      writeProfile: async (_id, b) => {
        if ((b as { expectedRevision: number }).expectedRevision !== boardRevision) throw new ApiError("Reload the agent profile.", 409);
        throw new Error("unreachable in this probe");
      },
    });
    const out = await saveHosted(d, profile({ revision: 7 }), "mine");
    check(out.kind === "conflict" && out.profile?.revision === boardRevision, "a newer revision than the one seen gets another conflict, with the newer profile");
    check(calls.writes.length === 0, "and nothing is written over it");
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
  check(
    (await checkNative(deps({ readNative: async () => null }).deps, "ads", "x")).kind === "unconfirmed",
    "native check: a text the board does not serve tells nothing, so it stays unconfirmed",
  );

  // A check whose read answers with nothing usable is unconfirmed, not a crash.
  check(
    (await checkHosted(deps({ readProfile: async () => ({}) as StoredAgentProfile }).deps, profile(), "x")).kind === "unconfirmed",
    "a malformed check read settles as unconfirmed",
  );
  check((await checkNative(deps({ readNative: async () => "other" }).deps, "ads", "x")).kind === "not_saved", "native check: other text");
  {
    const out = await checkNative(deps({ readNative: async () => "other" }).deps, "ads", "x");
    check(out.kind === "not_saved" && out.text === "other", "native not_saved carries what it read");
  }

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
  check(/running turn/.test(savedText("hosted", null, false, id)), "hosted says a running turn is not changed");
  check(/next turn/.test(savedText("native", "next_turn", false, id)) && /already running/.test(savedText("native", "next_turn", false, id)), "native next turn");
  check(!/already running/.test(savedText("native", "not_delivered", false, id)) && /does not read it yet/.test(savedText("native", "not_delivered", false, id)), "not delivered is plain");
  check(/did save/.test(savedText("native", null, true, id)), "a checked save says so");

  // Who changed it, where the board records it.
  check(nativeChange({ kind: "instructions_updated", at: "x", payload: { by: "operator" } })?.by === "operator", "native event carries who");
  check(nativeChange({ kind: "instructions_updated", at: "x" })?.by === null, "no payload: not recorded");
  check(nativeChange({ kind: "turn_started", at: "x" }) === null, "other events are not edits");
  // Which way an agent is edited, if at all.
  const base = { hired: false, hostedBoard: false, served: "text" as string | null, master: false, profileBound: false };
  check(instructionsKind(base) === "native", "a runtime agent that serves its text is edited natively");
  check(instructionsKind({ ...base, master: true }) === null, "the master is read-only");
  check(instructionsKind({ ...base, profileBound: true }) === null, "an agent whose job comes from a settings profile is read-only");
  check(instructionsKind({ ...base, served: null }) === null, "nothing served, nothing to edit");
  check(instructionsKind({ ...base, hired: true, hostedBoard: true }) === "hosted", "a hosted profile is edited through its revision");
  check(instructionsKind({ ...base, hired: true }) === null, "a native settings profile has no edit route");
  // The Context drawer says "every session" only where that is true.
  check(contextOf("x", "native", null).heading === "Every session starts with", "native agent: every session");
  check(contextOf("x", "hosted", null).heading === "A session that loads this profile starts with", "hosted: only a session that loads the profile");
  const undelivered = contextOf("x", "native", "not_delivered");
  check(!undelivered.lines.some((l) => l.name === "Instructions") && !/Every session/.test(undelivered.heading), "not delivered: the instructions are not stated as loaded");
  check(contextOf("x".repeat(40), "native", "next_turn").lines[0]!.size.includes("(estimate)"), "the figure is labelled an estimate");
  // zh-TW: the History line has its own key; "By" stays "deleted by".
  const by = translateUiText("By {actor}", "zh-TW");
  check(by !== "By {actor}" && !by.includes("刪除") && by.includes("{actor}"), "History 'By' has its own zh-TW key");
  const changedBy = translateUiText("Changed by", "zh-TW");
  check(changedBy !== "Changed by" && !changedBy.includes("刪除"), "the History drawer label has its own zh-TW key");
  check(translateUiText("By", "zh-TW") === "刪除者", "the existing 'By' is left alone");
  check(translateUiText("About {n} tokens (estimate)", "zh-TW") !== "About {n} tokens (estimate)", "the estimate translates");
  console.log("instructionsDoc checks passed");
}
void main().catch((e: unknown) => {
  console.error(e);
  (globalThis as unknown as { process: { exit(n: number): never } }).process.exit(1);
});
