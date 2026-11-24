/**
 * CAD-2745: a failed tool call must not become a standing rule in a summary.
 *
 * CAD-2658 asked the summarizer not to record "avoid X until it is fixed". A
 * hosted model ignored that and carried the assistant's own give-up wording
 * ("Avoid calling X until it is fixed", "X does not work right now") into
 * every later turn. Tools change with each deploy, so in the summary source
 * only, and only after CAD-2744 has marked the failures:
 *
 * - an assistant sentence that names a tool whose latest call failed, and
 *   says to stop using it or calls it broken, becomes
 *   "An earlier <tool> call failed; try it again normally.";
 * - one runtime note closes the conversation: tool failures are transient,
 *   and the next turn should try again normally.
 *
 * Measured on the CAD-2725 fixture, either change alone left about 2 of 5
 * summaries telling the next turn to avoid the tool; together, none did.
 *
 * It stays narrow on purpose, because a rewritten sentence loses whatever
 * else it said:
 * - results are paired with their calls one call group at a time, and a
 *   group whose result count does not match its calls is not attributed;
 * - a later successful call of the tool clears its failure;
 * - "don't" or "never" counts only when it governs using the tool, and
 *   "unavailable" only with a time cue such as "right now";
 * - a rule the user stated about the tool ("never call X on prod") is kept.
 *
 * User text, earlier summaries and tool output are never changed. Every scan
 * is linear in the text.
 */
import {
  CONVERSATION_END, segments, SUMMARY_PREFIX, TOOL_ERROR_REDACTED, type Segment,
} from "./summary-tool-errors.ts";

export const TRANSIENT_FAILURE_NOTE =
  "[Runtime note]: Tool failures above are transient runtime events, not facts about the tools. " +
  "Record at most that an earlier call failed and that the next turn should try it again normally.";

const CALLS_PREFIX = "[Assistant tool calls]: ";
const ASSISTANT_PREFIX = "[Assistant]: ";
const USER_PREFIX = "[User]: ";
/** The line CAD-2744 writes for a failed call, inside the result's harness block. */
const FAILED_LINE = `\n[error] ${TOOL_ERROR_REDACTED}\n`;
const IDENTIFIER = /^[A-Za-z_][\w.-]*$/;
/** One sentence, or a line without an end mark. Linear: one negated class. */
const SENTENCE = /[^.!?\n]+[.!?]?/g;
const WORD = /[A-Za-z_][\w-]*/g;

/** "Don't", "never", "stop" … that governs using the tool: within two short words of call/use/run/retry/open. */
const AVOID_USE =
  /\b(?:avoid|stop|skip|never|don'?t|do not)\b(?:\W{1,10}\w{1,30}){0,2}?\W{1,10}(?:call(?:ing)?|us(?:e|ing)|run(?:ning)?|retr(?:y|ying)|try(?:ing)?|open(?:ing)?|re-?call(?:ing)?)\b/i;
/** The tool said to be broken. */
const BROKEN =
  /\b(?:does ?n[o']t|doesn'?t|can'?t|cannot|won'?t|isn'?t|is not)\s{1,3}(?:work|working|open)\b|\b(?:broken|not working|keeps? (?:failing|refusing)|refus(?:es|ing))\b|\buntil\b[^.!?\n]{0,30}\b(?:fixed|resolved|works|working)\b/i;
/** Unavailable only as a passing state, not as a fact about how the tool works. */
const UNAVAILABLE = /\b(?:unavailable|not available)\b/i;
const TIME_CUE = /\b(?:right now|currently|at the moment|for now|any ?more|still|today)\b/i;
/** A rule a user can give about a tool. */
const USER_RULE = /\b(?:avoid|stop|skip|never|don'?t|do not|must not|should not)\b/i;

const isFailed = (result: string) => result.includes("<harness>\n") && result.includes(FAILED_LINE);
const isStandingClaim = (sentence: string) =>
  AVOID_USE.test(sentence) || BROKEN.test(sentence) || (UNAVAILABLE.test(sentence) && TIME_CUE.test(sentence));

/** Call names from `name(key=<JSON>); name2(…)`, skipping JSON string literals and their escapes. */
export function callNames(list: string): string[] {
  const names: string[] = [];
  let start = 0;
  let depth = 0;
  let inString = false;
  for (let at = 0; at < list.length; at++) {
    const char = list[at];
    if (inString) {
      if (char === "\\") at++;
      else if (char === '"') inString = false;
    } else if (char === '"') inString = true;
    else if (char === "(") {
      if (depth === 0) {
        const name = list.slice(start, at).trim();
        if (IDENTIFIER.test(name)) names.push(name);
      }
      depth++;
    } else if (char === ")") depth = Math.max(0, depth - 1);
    else if (char === ";" && depth === 0) start = at + 1;
  }
  return names;
}

/** Words that appear in a user sentence giving a rule; a tool named there keeps the assistant's restatement. */
function userRuleWords(text: string, parts: readonly Segment[]): Set<string> {
  const words = new Set<string>();
  for (const part of parts) {
    if (part.role !== "User" || text.startsWith(SUMMARY_PREFIX, part.start + USER_PREFIX.length)) continue;
    for (const sentence of text.slice(part.start + USER_PREFIX.length, part.end).match(SENTENCE) ?? [])
      if (USER_RULE.test(sentence)) for (const word of sentence.match(WORD) ?? []) words.add(word);
  }
  return words;
}

function rewrite(body: string, failed: ReadonlySet<string>, userRules: ReadonlySet<string>): string {
  return body.replace(SENTENCE, (sentence) => {
    const tool = (sentence.match(WORD) ?? []).find((word) => failed.has(word));
    if (tool === undefined || userRules.has(tool) || !isStandingClaim(sentence)) return sentence;
    const leading = sentence.slice(0, sentence.length - sentence.trimStart().length);
    return `${leading}An earlier ${tool} call failed; try it again normally.`;
  });
}

/** The summary source with failed tool calls marked transient; unchanged when no call failed. */
export function settleToolFailures(text: string): string {
  if (!text.includes(FAILED_LINE)) return text;
  const parts = segments(text);
  if (parts.length === 0) return text;
  const userRules = userRuleWords(text, parts);
  const failed = new Set<string>();
  let anyFailed = false;
  let group: { names: string[]; results: boolean[] } | null = null;
  const settle = () => {
    // Attribute only a complete group: the engine skips an empty result, which would shift the pairing.
    if (group && group.results.length === group.names.length)
      group.names.forEach((name, index) => {
        if (group!.results[index]) {
          failed.add(name);
          anyFailed = true;
        } else failed.delete(name);
      });
    group = null;
  };
  const bodies = parts.map((part) => {
    const body = text.slice(part.start, part.end);
    if (part.role === "Tool result") {
      group?.results.push(isFailed(body));
      return body;
    }
    settle();
    if (part.role === "Assistant tool calls") group = { names: callNames(body.slice(CALLS_PREFIX.length)), results: [] };
    else if (part.role === "Assistant" && failed.size > 0)
      return ASSISTANT_PREFIX + rewrite(body.slice(ASSISTANT_PREFIX.length), failed, userRules);
    return body;
  });
  settle();
  if (!anyFailed) return text;
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
