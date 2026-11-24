/** Rewrite standalone assistant give-up claims, never arbitrary diagnostic prose. */
import { TOOL_ERROR_CODE_OMITTED, TOOL_ERROR_TEXT_OMITTED } from "./summary-tool-errors.ts";

export const TRANSIENT_FAILURE_NOTE =
  "[Runtime note]: Past tool failures are runtime events, not standing facts about the tools. " +
  "Keep user restrictions and scoped limits; otherwise the next turn should try again normally if needed.";

// Match the whole sentence. Arbitrary prefixes, complements and clauses may
// contain facts or valid restrictions; leave them for the runtime note.
const TOOL = String.raw`[\x60]?([A-Za-z_][\w.-]*)[\x60]?`;
const ACTION = "call(?:ing)?|us(?:e|ing)|run(?:ning)?|retr(?:y|ying)|open(?:ing)?|re-?call(?:ing)?";
const AVOID_ACTION = String.raw`(?:(?:(?:I|we|you) (?:will |should )?)?(?:avoid|stop|skip|never|don't|do not|not|must not|should not) (?:keep |try to )?(?:${ACTION})|avoid)`;
const AVOID = new RegExp(
  String.raw`^${AVOID_ACTION} ${TOOL}(?: again| for now| until (?:it )?(?:is fixed|is resolved|works|fixed|resolved))?$`,
  "i",
);
const TIME = "right now|currently|at the moment|for now|any ?more|still|today";
const OMITTED_ERROR = [TOOL_ERROR_TEXT_OMITTED, TOOL_ERROR_CODE_OMITTED]
  .map((marker) => marker.replace(/[()]/g, "\\$&")).join("|");
const BROKEN = new RegExp(
  String.raw`^${TOOL} (?:(?:does not work|doesn't work|won't work|is broken|is not working|isn't working|keeps failing|keeps refusing)(?: (?:${TIME}|though)| with (?:${OMITTED_ERROR})(?:: (?:${OMITTED_ERROR}))?|, avoid it)?|(?:is unavailable|is not available|isn't available) (?:${TIME}))$`,
  "i",
);
// Newlines can wrap a single restriction. Only terminal punctuation or the
// end of the message can bound a claim, not a visual line break.
const SENTENCE = /(?:[^.!?]|[.!?](?!\s|$))+[.!?]?/g;
const USER_RULE = new RegExp(String.raw`^${AVOID_ACTION} ${TOOL}(?=\s|[.!?,;:]|$)`, "i");

export function collectUserToolRestrictions(text: string, restricted: Set<string>): void {
  for (const match of text.matchAll(SENTENCE)) {
    const name = USER_RULE.exec(match[0].trim().replace(/[.!?]$/, ""))?.[1];
    if (name !== undefined) restricted.add(name);
  }
}

function claimTool(sentence: string): string | undefined {
  // Terminal punctuation is not part of an unquoted tool identifier.
  const claim = sentence.trim().replace(/[.!]$/, "");
  return AVOID.exec(claim)?.[1] ?? BROKEN.exec(claim)?.[1];
}

export function rewriteGiveUps(body: string, failed: ReadonlySet<string>, userRules: ReadonlySet<string>): string {
  return body.replace(SENTENCE, (sentence) => {
    const tool = claimTool(sentence);
    if (tool === undefined || !failed.has(tool) || userRules.has(tool)) return sentence;
    const leading = sentence.slice(0, sentence.length - sentence.trimStart().length);
    return `${leading}An earlier ${tool} call failed; try it again normally.`;
  });
}
