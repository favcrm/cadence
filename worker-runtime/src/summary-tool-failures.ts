/**
 * CAD-2745: a failed tool call must not become a standing rule in a summary.
 *
 * CAD-2658 asked the summarizer not to record "avoid X until it is fixed". A
 * hosted model ignored that and carried the assistant's own give-up wording
 * ("Avoid calling X until it is fixed", "X does not work right now") into
 * every later turn. In the summary source only, after CAD-2744 has redacted
 * tool errors, replace standalone give-up sentences about a tool whose latest
 * attributable call failed. Add one runtime note for failures, even when they
 * can't be matched to a specific call.
 *
 * Only rewrite complete claims, not sentences with arguments, scope, or other
 * facts. A group with missing results is never attributed, a later successful
 * call clears the failure, and a user-stated tool restriction is preserved.
 * User text, earlier summaries and tool output are never changed.
 */
import {
  CONVERSATION_END, segments, SUMMARY_PREFIX, TOOL_ERROR_CODE_OMITTED,
  TOOL_ERROR_REDACTED, TOOL_ERROR_TEXT_OMITTED, type Segment,
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
/** Split at line breaks and terminal punctuation, not dots within identifiers. */
const SENTENCE = /(?:[^.!?\n]|[.!?](?!\s|$))+[.!?]?/g;
const WORD = /[A-Za-z_](?:[\w-]|\.(?=[\w.-]))*/g;

// Match the whole sentence. Arbitrary prefixes, complements and clauses may
// contain facts or valid restrictions; leave them for the runtime note.
const TOOL = String.raw`[\x60]?([A-Za-z_][\w.-]*)[\x60]?`;
const ACTION = "call(?:ing)?|us(?:e|ing)|run(?:ning)?|retr(?:y|ying)|open(?:ing)?|re-?call(?:ing)?";
const AVOID = new RegExp(
  String.raw`^(?:(?:(?:I|we) (?:will |should )?)?(?:avoid|stop|skip|never|don't|do not|not) (?:keep |try to )?(?:${ACTION})|avoid) ${TOOL}(?: again| for now| until (?:it )?(?:is fixed|is resolved|works|fixed|resolved))?$`,
  "i",
);
const TIME = "right now|currently|at the moment|for now|any ?more|still|today";
const OMITTED_ERROR = [TOOL_ERROR_TEXT_OMITTED, TOOL_ERROR_CODE_OMITTED]
  .map((marker) => marker.replace(/[()]/g, "\\$&")).join("|");
const BROKEN = new RegExp(
  String.raw`^${TOOL} (?:(?:does not work|doesn't work|won't work|is broken|is not working|isn't working|keeps failing|keeps refusing)(?: (?:${TIME}|though)| with (?:${OMITTED_ERROR})(?:: (?:${OMITTED_ERROR}))?|, avoid it)?|(?:is unavailable|is not available|isn't available) (?:${TIME}))$`,
  "i",
);
/** A rule a user can give about a tool. */
const USER_RULE = /\b(?:avoid|stop|skip|never|don'?t|do not|must not|should not)\b/i;

const isFailed = (result: string) => result.includes("<harness>\n") && result.includes(FAILED_LINE);
function claimTool(sentence: string): string | undefined {
  // Terminal punctuation is not part of an unquoted tool identifier.
  const claim = sentence.trim().replace(/[.!]$/, "");
  return AVOID.exec(claim)?.[1] ?? BROKEN.exec(claim)?.[1];
}

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
    const tool = claimTool(sentence);
    if (tool === undefined || !failed.has(tool) || userRules.has(tool)) return sentence;
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
    if (group && group.results.length === group.names.length) {
      const { names, results } = group;
      names.forEach((name, index) => {
        if (results[index]) failed.add(name);
        else failed.delete(name);
      });
    }
    group = null;
  };
  const bodies = parts.map((part) => {
    const body = text.slice(part.start, part.end);
    if (part.role === "Tool result") {
      const failure = isFailed(body);
      if (failure) anyFailed = true;
      group?.results.push(failure);
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
