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
  String.raw`^${AVOID_ACTION} ${TOOL}(?: (again|for now|until (?:it )?(?:is fixed|is resolved|works|fixed|resolved)))?$`,
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
/** A scoped rule protects a matching restatement, not every claim about that tool. */
function restrictionKey(match: RegExpExecArray): string {
  const condition = match[2]?.toLowerCase().replace(/^until (?:it )?(?:is )?/, "until ");
  return condition === undefined ? match[1]! : `${match[1]}\n${condition}`;
}

export function collectUserToolRestrictions(text: string, restricted: Set<string>): void {
  for (const match of text.matchAll(SENTENCE)) {
    const rule = AVOID.exec(match[0].trim().replace(/[.!]$/, "").replace(/\s+/g, " "));
    if (rule !== null) restricted.add(restrictionKey(rule));
  }
}

export function rewriteGiveUps(body: string, failed: ReadonlySet<string>, userRules: ReadonlySet<string>): string {
  return body.replace(SENTENCE, (sentence) => {
    const claim = sentence.trim().replace(/[.!]$/, "");
    const avoid = AVOID.exec(claim);
    const tool = avoid?.[1] ?? BROKEN.exec(claim)?.[1];
    if (tool === undefined || !failed.has(tool) || userRules.has(tool) ||
      (avoid !== null && userRules.has(restrictionKey(avoid)))) return sentence;
    const leading = sentence.slice(0, sentence.length - sentence.trimStart().length);
    return `${leading}An earlier ${tool} call failed; try it again normally.`;
  });
}
