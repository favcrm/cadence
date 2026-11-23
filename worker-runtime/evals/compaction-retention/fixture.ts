/**
 * CAD-2725: what a compaction summary keeps and what it drops.
 *
 * A fully synthetic conversation: a brief of facts a later turn needs, then a
 * long tail of one tool refusing the same call. The real hosted session
 * compacts it, and the summary request it would send is captured unchanged.
 * Nothing here is a real business, customer or transcript.
 */
import { BACKGROUND_CONTEXT } from "@earendil-works/chord/context";
import { SUMMARY_INSTRUCTION } from "../../src/model.ts";
import {
  answer, chunk, cleanups, isSummary, session, type SessionOptions,
} from "../../test/support/hosted-session.ts";

/** A fact the summary must keep; any one variant counts (case-insensitive). */
export type Fact = { name: string; variants: readonly string[] };

export const FACTS: readonly Fact[] = [
  { name: "business name", variants: ["Larkspur Tide"] },
  { name: "launch date", variants: ["2027-03-14", "14 March 2027", "March 14, 2027", "14 Mar 2027", "Mar 14, 2027"] },
  { name: "budget", variants: ["18,450", "18450", "18.45k", "18.45 k"] },
  { name: "contact", variants: ["Odessa Venn"] },
  { name: "product code", variants: ["LTB-4471"] },
  { name: "decision", variants: ["sourdough box first", "sourdough box before", "start with the sourdough box", "lead with the sourdough box"] },
  { name: "preference", variants: ["emoji"] },
];

const TOOL = "canvas_open";
const REFUSAL_CODE = "canvas_slot_conflict_4c";
const REFUSAL_TEXT = "Slot quartz-9 is held by another canvas";
const ARGUMENTS = { board: "orchid-ledger-7731", layout: "split-heron" } as const;

/** A sentence that names the failing tool: its name, or the canvas it opens. */
const TOOL_MENTION = /canvas/i;

/**
 * Wording that turns a failure into a standing fact or an instruction. It
 * counts only inside a sentence that names the tool, so the brief's own
 * facts ("do not work on the pastry box", "always reject copy with emoji")
 * are not leaks, and the form the instruction allows ("an earlier attempt to
 * open the canvas failed; try again normally") is not one either.
 */
const STANDING_CLAIM = new RegExp(
  [
    // A failure said to repeat: "keeps refusing", "failed repeatedly", "repeated attempts ... failed".
    String.raw`\b(keeps?|kept|repeatedly|consistently|always|persistent(ly)?|continu(es|ally|ously))\b\W+(\w+\W+){0,3}(fail|refus|error|reject)`,
    String.raw`\b(fail|refus|error|reject)\w*\W+(\w+\W+){0,3}(repeatedly|consistently|every time|each time|again and again|multiple times|several times|\d+ times)`,
    String.raw`\b(repeated|multiple|several|all|every)\b\W+(\w+\W+){0,6}(fail|refus|error|reject)`,
    // A failure said to be the tool's state.
    String.raw`(does ?n[o']t|do ?n[o']t|can ?n[o']t|cannot|won'?t|will not)\W+(work|open|be opened|be used)`,
    String.raw`\b(broken|not working|unusable|unavailable|blocked|refuses|refusing|is down)\b`,
    // An instruction not to use it.
    String.raw`\b(avoid|stop|skip|do not|don'?t|never)\b\W+(\w+\W+){0,3}(us|call|retry|try|open|attempt)`,
    String.raw`\buntil\b\W+(\w+\W+){0,3}(fixed|resolved|works)`,
  ].join("|"),
  "i",
);

/** The summary's sentences: split on line breaks and sentence ends. */
function sentences(text: string): string[] {
  return text.split(/\n+|(?<=[.;!?])\s+/).filter((sentence) => sentence.trim() !== "");
}

/** Text from the failing calls that must not become a lasting fact. */
export const LEAK_MARKERS: readonly { name: string; test: (summary: string) => boolean }[] = [
  { name: "refusal code", test: (summary) => /canvas_slot_conflict/i.test(summary) },
  { name: "refusal text", test: (summary) => /quartz-9|held by another canvas|slot conflict|loop guard/i.test(summary) },
  { name: "argument value", test: (summary) => /orchid-ledger|split-heron/i.test(summary) },
  {
    name: "standing failure claim",
    test: (summary) => sentences(summary).some((sentence) => TOOL_MENTION.test(sentence) && STANDING_CLAIM.test(sentence)),
  },
];

/** The user turns, in order. The facts live only here. */
const BRIEF =
  "Brief for the launch. The business is Larkspur Tide Bakery. We launch on 2027-03-14. " +
  "The total launch budget is HK$18,450 and must not be exceeded. Odessa Venn approves every page. " +
  "The product code for the sourdough box is LTB-4471. Decision: launch the sourdough box first and " +
  "the pastry box later, because the sourdough supplier is already signed. Preference: no emoji in any copy.";
const ASK = "Open the launch canvas and lay out the sourdough box page.";
const RETRY = "Try opening the canvas once more.";
const LAST = "The canvas was fixed in today's deploy. Continue with the launch page.";

const CALLS_PER_ROUND = 4;
const ROUNDS = 3;
const GIVE_UP = [
  `${TOOL} keeps refusing with ${REFUSAL_CODE}: ${REFUSAL_TEXT}. I'll stop retrying for now.`,
  `Still refusing: ${REFUSAL_CODE}. ${TOOL} does not work right now.`,
  `${TOOL} failed again with the same refusal. Avoid calling ${TOOL} until it is fixed.`,
];

function canvasCall(id: number) {
  return new Response(
    chunk({ role: "assistant", content: "" }) +
      chunk({ tool_calls: [{ index: 0, id: `canvas-${id}`, type: "function",
        function: { name: TOOL, arguments: JSON.stringify(ARGUMENTS) } }] }) +
      chunk({}, "tool_calls") + "data: [DONE]\n\n",
    { headers: { "content-type": "text/event-stream" } },
  );
}

type Message = { role?: string; content?: unknown };

/** Messages after the last user message that are tool results. */
function trailingToolResults(messages: readonly Message[]): number {
  let count = 0;
  for (let at = messages.length - 1; at >= 0 && messages[at]!.role !== "user"; at--)
    if (messages[at]!.role === "tool") count++;
  return count;
}

/** Run the conversation and return the summary request the runtime would send. */
export async function captureSummaryRequest(): Promise<Record<string, unknown>> {
  let captured: Record<string, unknown> | undefined;
  let calls = 0;
  let round = 0;
  const options: SessionOptions = {
    keepRecentTokens: 1,
    tools: [{
      name: "canvas", tools: [{
        name: TOOL, description: "Open a launch canvas on a board.",
        parameters: {
          type: "object",
          properties: { board: { type: "string" }, layout: { type: "string" } },
          required: ["board", "layout"], additionalProperties: false,
        },
        replay: "safe",
        async execute() { throw new Error(`${REFUSAL_CODE}: ${REFUSAL_TEXT}`); },
      }],
    }],
  };
  const run = await session((requests) => {
    const input = requests.at(-1)!.input;
    if (isSummary(input)) {
      captured = structuredClone(input);
      return answer("Summary.");
    }
    const messages = input.messages as Message[];
    const last = messages.at(-1);
    if (last?.role === "tool") {
      if (trailingToolResults(messages) < CALLS_PER_ROUND) return canvasCall(++calls);
      return answer(GIVE_UP[round++ % GIVE_UP.length]!);
    }
    const text = JSON.stringify(last?.content ?? "");
    if (text.includes("canvas") && !text.includes("fixed")) return canvasCall(++calls);
    return answer("Noted.");
  }, options);
  try {
    for (const prompt of [BRIEF, ASK, ...Array.from({ length: ROUNDS - 1 }, () => RETRY)])
      await (await run.submit(prompt)).wait(BACKGROUND_CONTEXT);
    // Force a compaction before the next generation, as the blocking threshold would.
    options.reserveTokens = 1_000_000;
    await (await run.submit(LAST)).wait(BACKGROUND_CONTEXT);
  } finally {
    for (const cleanup of cleanups.splice(0).reverse()) await cleanup();
  }
  if (captured === undefined) throw new Error("The session did not compact.");
  if (calls !== CALLS_PER_ROUND * ROUNDS) throw new Error(`Expected ${CALLS_PER_ROUND * ROUNDS} failing calls, saw ${calls}.`);
  return captured;
}

/** The same request without the CAD-2658 instruction: the control arm. */
export function withoutInstruction(request: Record<string, unknown>): Record<string, unknown> {
  const messages = structuredClone(request.messages) as Message[];
  const system = messages[0]!;
  if (typeof system.content === "string") {
    system.content = system.content.replace(`\n\n${SUMMARY_INSTRUCTION}`, "");
  } else if (Array.isArray(system.content)) {
    system.content = system.content.filter(
      (part: { type?: string; text?: string }) => !(part.type === "text" && part.text === SUMMARY_INSTRUCTION),
    );
  }
  if (JSON.stringify(system.content).includes(SUMMARY_INSTRUCTION.slice(0, 40)))
    throw new Error("The control arm still carries the instruction.");
  return { ...request, messages };
}

export type Score = {
  kept: readonly string[];
  dropped: readonly string[];
  leaks: readonly string[];
};

/** Which facts a summary kept and which failure details it carried forward. */
export function score(summary: string): Score {
  const lower = summary.toLowerCase();
  const kept: string[] = [];
  const dropped: string[] = [];
  for (const fact of FACTS)
    (fact.variants.some((variant) => lower.includes(variant.toLowerCase())) ? kept : dropped).push(fact.name);
  const leaks = LEAK_MARKERS.filter((marker) => marker.test(summary)).map((marker) => marker.name);
  return { kept, dropped, leaks };
}
