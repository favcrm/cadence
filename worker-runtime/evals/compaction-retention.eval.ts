/**
 * CAD-2725: opt-in live eval of what a compaction summary keeps and drops.
 *
 *   CADENCE_EVAL_LIVE=1 CADENCE_EVAL_MODEL_URL=<chat completions URL> \
 *   CADENCE_EVAL_MODEL_KEY=<key> CADENCE_EVAL_MODEL=<model id> \
 *   [CADENCE_EVAL_RUNS=5] bun run eval:compaction
 *
 * It sends the summary request the hosted runtime would send, twice per run:
 * "instruction" as shipped (CAD-2658) and "control" without the instruction.
 * It calls a paid model, so it never runs in `bun test` or CI; the scorer and
 * the captured request are covered offline by
 * `test/compaction-retention-eval.test.ts`.
 */
import { captureSummaryRequest, FACTS, LEAK_MARKERS, score, withoutInstruction, type Score } from "./compaction-retention/fixture.ts";

const REQUIRED = ["CADENCE_EVAL_LIVE", "CADENCE_EVAL_MODEL_URL", "CADENCE_EVAL_MODEL_KEY", "CADENCE_EVAL_MODEL"] as const;
const RUNS_DEFAULT = 5;
const RUNS_MAX = 20;
const TIMEOUT_MS = 300_000;

type Arm = "instruction" | "control";
type Run = { arm: Arm; model: string | null; usage: unknown; summary: string; score: Score };

function config() {
  const missing = REQUIRED.filter((name) => !process.env[name]);
  if (process.env.CADENCE_EVAL_LIVE && process.env.CADENCE_EVAL_LIVE !== "1") missing.push("CADENCE_EVAL_LIVE");
  const runs = Number(process.env.CADENCE_EVAL_RUNS ?? RUNS_DEFAULT);
  if (missing.length > 0 || !Number.isSafeInteger(runs) || runs < 1 || runs > RUNS_MAX) {
    console.error(
      "compaction eval refused: it calls a paid model and runs only when asked.\n" +
        (missing.length > 0 ? `Missing or invalid: ${[...new Set(missing)].join(", ")} (CADENCE_EVAL_LIVE must be 1).\n` : "") +
        (Number.isSafeInteger(runs) && runs >= 1 && runs <= RUNS_MAX ? "" : `CADENCE_EVAL_RUNS must be 1..${RUNS_MAX}.\n`) +
        "Nothing was sent.",
    );
    process.exit(2);
  }
  return {
    url: process.env.CADENCE_EVAL_MODEL_URL!,
    key: process.env.CADENCE_EVAL_MODEL_KEY!,
    model: process.env.CADENCE_EVAL_MODEL!,
    runs,
  };
}

function text(content: unknown): string {
  if (typeof content === "string") return content;
  if (Array.isArray(content))
    return content.flatMap((part: { type?: string; text?: string }) =>
      part.type === "text" && typeof part.text === "string" ? [part.text] : []).join("");
  return "";
}

/** The captured request as one non-streaming chat completion for the chosen model. */
function body(request: Record<string, unknown>, model: string) {
  const { stream: _stream, stream_options: _options, messages, ...rest } = request;
  return {
    ...rest,
    model,
    stream: false,
    messages: (messages as Array<{ role: string; content: unknown }>).map(({ role, content }) => ({ role, content })),
  };
}

async function complete(cfg: ReturnType<typeof config>, request: Record<string, unknown>) {
  const response = await fetch(cfg.url, {
    method: "POST",
    headers: { "content-type": "application/json", authorization: `Bearer ${cfg.key}` },
    body: JSON.stringify(body(request, cfg.model)),
    signal: AbortSignal.timeout(TIMEOUT_MS),
  });
  if (!response.ok) throw new Error(`model endpoint answered ${response.status}: ${(await response.text()).slice(0, 300)}`);
  const json = await response.json() as {
    model?: string; usage?: unknown;
    choices?: Array<{ message?: { content?: unknown }; finish_reason?: string }>;
  };
  const choice = json.choices?.[0];
  const summary = text(choice?.message?.content);
  if (!summary) throw new Error(`model returned no summary text (finish_reason ${choice?.finish_reason ?? "none"})`);
  return { model: json.model ?? null, usage: json.usage ?? null, summary };
}

function report(runs: readonly Run[], arm: Arm) {
  const mine = runs.filter((run) => run.arm === arm);
  const rate = (count: number) => Number((count / mine.length).toFixed(2));
  return {
    runs: mine.length,
    fact_retention: rate(mine.reduce((total, run) => total + run.score.kept.length, 0) / FACTS.length),
    facts: Object.fromEntries(FACTS.map((fact) => [fact.name, rate(mine.filter((run) => run.score.kept.includes(fact.name)).length)])),
    runs_with_any_leak: rate(mine.filter((run) => run.score.leaks.length > 0).length),
    leaks: Object.fromEntries(LEAK_MARKERS.map((marker) => [marker.name, rate(mine.filter((run) => run.score.leaks.includes(marker.name)).length)])),
  };
}

const cfg = config();
const instruction = await captureSummaryRequest();
const control = withoutInstruction(instruction);
const bytes = Buffer.byteLength(JSON.stringify(body(instruction, cfg.model)));
console.error(
  `compaction eval: ${cfg.runs} run(s) x 2 arms = ${cfg.runs * 2} requests to ${cfg.model}; ` +
    `about ${bytes} bytes (~${Math.ceil(bytes / 4)} input tokens) each.`,
);

const runs: Run[] = [];
for (let at = 0; at < cfg.runs; at++) {
  // Alternate the arms so drift in the endpoint hits both alike.
  for (const [arm, request] of [["instruction", instruction], ["control", control]] as const) {
    const result = await complete(cfg, request);
    runs.push({ arm, ...result, score: score(result.summary) });
    console.error(`  ${arm} ${at + 1}/${cfg.runs}: kept ${runs.at(-1)!.score.kept.length}/${FACTS.length}, leaks [${runs.at(-1)!.score.leaks.join(", ")}]`);
  }
}

const result = {
  eval: "CAD-2725 compaction retention",
  model_requested: cfg.model,
  models_reported: [...new Set(runs.map((run) => run.model))],
  runs_per_arm: cfg.runs,
  instruction: report(runs, "instruction"),
  control: report(runs, "control"),
  usage: runs.map((run) => ({ arm: run.arm, usage: run.usage })),
  summaries: runs.map((run) => ({ arm: run.arm, summary: run.summary })),
};
console.log(JSON.stringify(result, null, 2));
console.error(
  `\n           facts kept  runs with a leak\n` +
    `instruction   ${String(result.instruction.fact_retention).padEnd(10)}  ${result.instruction.runs_with_any_leak}\n` +
    `control       ${String(result.control.fact_retention).padEnd(10)}  ${result.control.runs_with_any_leak}`,
);
