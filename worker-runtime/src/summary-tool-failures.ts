/** Rewrite standalone assistant give-up claims, never arbitrary diagnostic prose. */
import { TOOL_ERROR_CODE_OMITTED, TOOL_ERROR_TEXT_OMITTED } from "./summary-tool-errors.ts";
import { proseClauses } from "./summary-prose.ts";

export const TRANSIENT_FAILURE_NOTE =
  "[Runtime note]: Past tool failures are runtime events, not standing facts about the tools. " +
  "Keep user restrictions and scoped limits; otherwise the next turn should try again normally if needed.";

// Match the whole sentence. Arbitrary prefixes, complements and clauses may
// contain facts or valid restrictions; leave them for the runtime note.
const TOOL = String.raw`[\x60]?(?<tool>[A-Za-z_][\w.-]*)[\x60]?`;
const ACTION = "call(?:ing)?|us(?:e|ing)|run(?:ning)?|retr(?:y|ying)|open(?:ing)?|re-?call(?:ing)?";
const AVOID_ACTION = String.raw`(?:(?:(?:I|we|you) (?:will |should )?)?(?:avoid|stop|skip|never|don't|do not|not|must not|should not) (?:(?<repeat>keep |continue (?:to )?)|try to )?(?<action>${ACTION})|avoid)`;
const LIMIT = "again|for now|until (?:it )?(?:is fixed|is resolved|works|fixed|resolved)";
const AVOID_CLAIM = String.raw`${AVOID_ACTION} ${TOOL}(?: (?<condition>${LIMIT}))?`;
const AVOID = new RegExp(`^${AVOID_CLAIM}$`, "i");
// This continuation only repeats the preceding directive about the same tool.
// Do not split assistant prose at arbitrary semicolons.
const GIVE_UP = new RegExp(
  String.raw`^${AVOID_CLAIM}(?:[ \t]*;[ \t]*(?<continuation>(?:stop and )?avoid (?:${ACTION}) it(?: (?:${LIMIT}))?))?$`, "i",
);
const TIME = "right now|currently|at the moment|for now|any ?more|still|today";
const OMITTED_ERROR = [TOOL_ERROR_TEXT_OMITTED, TOOL_ERROR_CODE_OMITTED]
  .map((marker) => marker.replace(/[()]/g, "\\$&")).join("|");
const BROKEN = new RegExp(
  String.raw`^${TOOL} (?:(?:does not work|doesn't work|won't work|is broken|is not working|isn't working|keeps failing|keeps refusing)(?: (?:${TIME}|though)| with (?:${OMITTED_ERROR})(?:: (?:${OMITTED_ERROR}))?|, avoid it)?|(?:is unavailable|is not available|isn't available) (?:${TIME}))$`,
  "i",
);
function restrictionScope(groups: Record<string, string>): string {
  const condition = groups.condition?.toLowerCase().replace(/^until (?:it )?(?:is )?/, "until ");
  const scope = condition ?? (/^(?:retr|re-?call)/i.test(groups.action ?? "") ? "again" : "");
  return groups.repeat ? `repeated:${scope}` : scope;
}

export function collectUserToolRestrictions(text: string, restricted: Map<string, Set<string>>): void {
  for (const [start, end] of proseClauses(text, true)) {
    const claim = text.slice(start, end).trim().replace(/[.!]$/, "").replace(/\s+/g, " ");
    const rule = AVOID.exec(claim)?.groups;
    if (!rule) continue;
    const tool = rule.tool!;
    let scopes = restricted.get(tool);
    if (!scopes) restricted.set(tool, scopes = new Set());
    scopes.add(restrictionScope(rule));
  }
}

export function rewriteGiveUps(body: string, failed: ReadonlySet<string>, userRules: ReadonlyMap<string, ReadonlySet<string>>): string {
  let out = "";
  let from = 0;
  for (const [start, end] of proseClauses(body, false)) {
    const sentence = body.slice(start, end);
    const leading = sentence.length - sentence.trimStart().length;
    const trimmed = sentence.trim();
    const claim = trimmed.replace(/[.!]$/, "");
    const avoid = GIVE_UP.exec(claim)?.groups;
    const tool = avoid?.tool ?? BROKEN.exec(claim)?.groups?.tool;
    if (tool === undefined || !failed.has(tool)) continue;
    const scopes = userRules.get(tool);
    if (avoid && avoid.continuation === undefined && scopes?.has(restrictionScope(avoid))) continue;
    out += body.slice(from, start + leading);
    out += `An earlier ${tool} call failed`;
    if (!scopes) out += "; try it again normally.";
    // A failure-derived "for now" or "until fixed" does not relax a user's
    // unqualified no-repeat rule. Keep the stronger limit explicit.
    else if (avoid && scopes.has("again")) out += `. The user said not to call ${tool} again.`;
    else out += ".";
    from = start + leading + trimmed.length;
  }
  return from === 0 ? body : out + body.slice(from);
}
