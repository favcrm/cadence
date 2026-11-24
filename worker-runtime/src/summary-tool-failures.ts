/**
 * CAD-2745: a failed tool call must not become a standing rule in a summary.
 *
 * CAD-2658 asked the summarizer not to record "avoid X until it is fixed". A
 * hosted model ignored that and carried the assistant's own give-up wording
 * ("Avoid calling X until it is fixed", "X does not work right now") into
 * every later turn. Tools change with each deploy, so in the summary source
 * only, and only after CAD-2744 has marked the failures:
 *
 * - an assistant sentence that names a tool whose call failed earlier in the
 *   source, and calls it broken or says to avoid it, becomes
 *   "An earlier <tool> call failed; try it again normally.";
 * - one runtime note closes the conversation: tool failures are transient,
 *   and the next turn should try again normally.
 *
 * Measured on the CAD-2725 fixture, either change alone left about 2 of 5
 * summaries telling the next turn to avoid the tool; together, none did.
 * User text, earlier summaries and tool output are not changed here.
 */
import { CONVERSATION_END, segments, type Segment } from "./summary-tool-errors.ts";

export const TRANSIENT_FAILURE_NOTE =
  "[Runtime note]: Tool failures above are transient runtime events, not facts about the tools. " +
  "Record at most that an earlier call failed and that the next turn should try it again normally.";

const CALLS_PREFIX = "[Assistant tool calls]: ";
const ASSISTANT_PREFIX = "[Assistant]: ";
/** A call as the engine writes it: `name(key=…)`, calls joined by "; ". */
const CALL_NAME = /(?:^|; )([A-Za-z_][\w.-]*)\(/g;
/** A failed result: the engine's harness block holds an `[error]` diagnostic (plain substring checks: linear). */
const failedResult = (body: string) => body.includes("<harness>\n") && body.includes("\n[error] ");
/** One sentence, or a line without an end mark. Linear: one negated class. */
const SENTENCE = /[^.!?\n]+[.!?]?/g;
/** Wording that calls a tool broken or says to avoid it. Each part is bounded. */
const STANDING_CLAIM = new RegExp(
  [
    String.raw`\b(?:avoid|stop|skip|never|don'?t|do not)\b`,
    String.raw`\b(?:does ?n[o']t|do ?n[o']t|can'?t|cannot|won'?t|isn'?t|is not)\s+(?:work|working|available|open)`,
    String.raw`\b(?:broken|unavailable|not working|keeps? (?:failing|refusing)|refus(?:es|ing))\b`,
    String.raw`\buntil\b[^.!?\n]{0,30}\b(?:fixed|resolved|works)\b`,
  ].join("|"),
  "i",
);

function escape(text: string): string {
  return text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

function rewrite(body: string, failed: ReadonlySet<string>): string {
  const names = [...failed].map((name) => ({ name, pattern: new RegExp(String.raw`(?<![\w.-])${escape(name)}(?![\w-])`) }));
  return body.replace(SENTENCE, (sentence) => {
    const tool = names.find(({ pattern }) => pattern.test(sentence));
    if (!tool || !STANDING_CLAIM.test(sentence)) return sentence;
    const leading = sentence.slice(0, sentence.length - sentence.trimStart().length);
    return `${leading}An earlier ${tool.name} call failed; try it again normally.`;
  });
}

/** The summary source with failed tool calls marked transient; unchanged when no call failed. */
export function settleToolFailures(text: string): string {
  if (!text.includes("<harness>")) return text;
  const parts = segments(text);
  const pending: string[] = [];
  const failed = new Set<string>();
  const bodies = parts.map((part: Segment) => {
    const body = text.slice(part.start, part.end);
    if (part.role === "Assistant tool calls") {
      for (const match of body.slice(CALLS_PREFIX.length).matchAll(CALL_NAME)) pending.push(match[1]!);
    } else if (part.role === "Tool result") {
      const name = pending.shift();
      if (name !== undefined && failedResult(body)) failed.add(name);
    } else if (part.role === "Assistant" && failed.size > 0) {
      return ASSISTANT_PREFIX + rewrite(body.slice(ASSISTANT_PREFIX.length), failed);
    }
    return body;
  });
  if (failed.size === 0) return text;
  let out = text.slice(0, parts[0]!.start);
  parts.forEach((part, index) => {
    out += bodies[index];
    const next = parts[index + 1];
    out += text.slice(part.end, next === undefined ? part.end : next.start);
  });
  out += text.slice(parts.at(-1)!.end);
  const end = out.lastIndexOf(CONVERSATION_END);
  return end < 0 ? out : `${out.slice(0, end)}\n\n${TRANSIENT_FAILURE_NOTE}${out.slice(end)}`;
}
