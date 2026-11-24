// CAD-2725: the offline half of the compaction retention eval. The live half
// (`evals/compaction-retention.eval.ts`) calls a paid model and never runs here.
import { describe, expect, it } from "bun:test";
import { SUMMARY_INSTRUCTION } from "../src/model.ts";
import { TOOL_ERROR_REDACTED } from "../src/summary-tool-errors.ts";
import {
  captureSummaryRequest, FACTS, LEAK_MARKERS, score, withoutInstruction,
} from "../evals/compaction-retention/fixture.ts";
import { isSummary } from "./support/hosted-session.ts";

const ALL_LEAKS = LEAK_MARKERS.map((marker) => marker.name);
const ALL_FACTS = FACTS.map((fact) => fact.name);

const GOOD = `## Goal
Launch Larkspur Tide Bakery's sourdough box page.

## Constraints & Preferences
- Launch date 2027-03-14; total budget HK$18,450, not to be exceeded.
- Odessa Venn approves every page. No emoji in any copy.
- The POS shows error E-4417 when it syncs the sourdough stock; the page must not go live until that is fixed.

## Key Decisions
- **Sourdough box first** (product LTB-4471), pastry box later: the supplier is signed.

## Next Steps
1. Open the launch canvas and lay out the sourdough box page. An earlier attempt to open the canvas failed; try again normally.`;

const LEAKY = `## Goal
Launch the Larkspur Tide page.

## Blocked
- canvas_open keeps refusing with canvas_slot_conflict_4c (Slot quartz-9 is held by another canvas).
- Loop guard stopped repeated canvas_open calls with board="orchid-ledger-7731".

## Next Steps
1. Avoid calling canvas_open; it does not work.`;

function userText(request: Record<string, unknown>): string {
  const content = (request.messages as Array<{ content: unknown }>)[1]!.content;
  return typeof content === "string" ? content
    : (content as Array<{ text?: string }>).map((part) => part.text ?? "").join("");
}

describe("compaction retention scorer", () => {
  it("keeps every fact and finds no leak in a clean summary", () => {
    expect(score(GOOD)).toEqual({ kept: ALL_FACTS, dropped: [], leaks: [] });
  });

  // Paraphrases count the same as the scripted wording, so the arm told to
  // drop codes is not credited for rewording the same standing claim.
  it.each([
    "canvas_open failed repeatedly.",
    "canvas_open failed every time with a conflict.",
    "Repeated attempts to open the canvas failed.",
    "Do not retry opening the canvas.",
    "Don't try to open the canvas again.",
    "The canvas tool is unavailable; skip it.",
    "### Blocked\n- canvas_open is blocked: another canvas holds it.",
    "### Blocked\n- The launch canvas cannot be opened.",
    "canvas_open keeps refusing.",
    "Avoid calling canvas_open until it is fixed.",
  ])("flags a standing failure claim: %p", (summary) => {
    expect(score(summary).leaks).toContain("standing failure claim");
  });

  it.each([
    "Do not work on the pastry box until the sourdough box ships.",
    "No emoji: always reject copy that contains emoji.",
    "Always err on the side of caution with the budget.",
    "An earlier attempt to open the canvas failed; try again normally.",
    "Next: open the launch canvas and lay out the sourdough box page.",
    "### Blocked\n- (none)",
  ])("does not flag the brief's own facts or the allowed wording: %p", (summary) => {
    expect(score(summary).leaks).toEqual([]);
  });

  it("names every leak and the dropped facts in a leaky summary", () => {
    const result = score(LEAKY);
    expect(result.leaks).toEqual(ALL_LEAKS);
    expect(result.kept).toEqual(["business name"]);
    expect(result.dropped).toHaveLength(FACTS.length - 1);
  });
});

describe("compaction retention request", () => {
  it("captures the runtime's real summary request, with and without the instruction", async () => {
    const instruction = await captureSummaryRequest();
    expect(isSummary(instruction)).toBe(true);
    const system = (request: Record<string, unknown>) =>
      JSON.stringify((request.messages as Array<{ content: unknown }>)[0]!.content);
    expect(system(instruction)).toContain(JSON.stringify(SUMMARY_INSTRUCTION).slice(1, -1));

    // The source holds every fact, so a summary that drops one did so
    // itself. CAD-2744: the tool's error code and text are removed before the
    // model sees them; the calls' arguments and the assistant's own "keeps
    // refusing" wording remain for the instruction to handle.
    expect(score(userText(instruction))).toEqual({
      kept: ALL_FACTS, dropped: [], leaks: ["argument value", "standing failure claim"],
    });
    expect(userText(instruction)).toContain(TOOL_ERROR_REDACTED);

    const control = withoutInstruction(instruction);
    expect(system(control)).not.toContain(SUMMARY_INSTRUCTION.slice(0, 40));
    expect(system(control)).toContain("context summarization assistant");
    expect(userText(control)).toBe(userText(instruction));
    expect({ ...control, messages: null }).toEqual({ ...instruction, messages: null });
  });
});

describe("compaction retention gate", () => {
  it("refuses without the opt-in and names what is missing", () => {
    const env = Object.fromEntries(
      Object.entries(process.env).filter(([name]) => !name.startsWith("CADENCE_EVAL_")),
    );
    const run = Bun.spawnSync(["bun", "evals/compaction-retention.eval.ts"], {
      cwd: `${import.meta.dir}/..`, env, stdout: "pipe", stderr: "pipe",
    });
    const stderr = run.stderr.toString();
    expect(run.exitCode).toBe(2);
    for (const name of ["CADENCE_EVAL_LIVE", "CADENCE_EVAL_MODEL_URL", "CADENCE_EVAL_MODEL_KEY", "CADENCE_EVAL_MODEL"])
      expect(stderr).toContain(name);
    expect(stderr).toContain("Nothing was sent.");
    expect(run.stdout.toString()).toBe("");
  });
});
