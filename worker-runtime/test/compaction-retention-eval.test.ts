// CAD-2725: the offline half of the compaction retention eval. The live half
// (`evals/compaction-retention.eval.ts`) calls a paid model and never runs here.
import { describe, expect, it } from "bun:test";
import { SUMMARY_INSTRUCTION } from "../src/model.ts";
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

    // The source holds every fact and every failure detail, so a summary
    // that drops a fact or keeps a leak did so itself.
    expect(score(userText(instruction))).toEqual({ kept: ALL_FACTS, dropped: [], leaks: ALL_LEAKS });

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
