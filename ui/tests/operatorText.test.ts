import { ACTION_MARK } from "../src/features/home/formModel";
import { FOLD_LINES, foldable, formReceipt, inlineTokens, lineCount, segments, type Inline } from "../src/features/home/operatorText";

function check(condition: boolean, message: string): void {
  if (!condition) throw new Error(`operatorText: ${message}`);
}
const show = (tokens: Inline[]): string => JSON.stringify(tokens);
const same = (text: string): void => {
  const tokens = inlineTokens(text);
  check(tokens.length === 1 && tokens[0].kind === "text" && tokens[0].text === text, `plain text stays as typed: ${text} -> ${show(tokens)}`);
};

// CAD-2362: inline Markdown in the operator's own bubble.
const mixed = inlineTokens("add `6220 膳食及應酬` and **book it** — see https://example.test/policy.");
check(
  show(mixed) ===
    show([
      { kind: "text", text: "add " },
      { kind: "code", text: "6220 膳食及應酬" },
      { kind: "text", text: " and " },
      { kind: "strong", text: "book it" },
      { kind: "text", text: " — see " },
      { kind: "link", text: "https://example.test/policy", href: "https://example.test/policy" },
      { kind: "text", text: "." },
    ]),
  `code, strong and a bare link, trailing period left as text: ${show(mixed)}`,
);
check(show(inlineTokens("read [the policy](https://example.test/p) first")) === show([
  { kind: "text", text: "read " },
  { kind: "link", text: "the policy", href: "https://example.test/p" },
  { kind: "text", text: " first" },
]), "a labelled https link");
check(show(inlineTokens("*soon* and _now_")) === show([
  { kind: "em", text: "soon" },
  { kind: "text", text: " and " },
  { kind: "em", text: "now" },
]), "emphasis with either marker");
// Nothing inside a code span is parsed.
check(show(inlineTokens("`**not bold** https://x.test`")) === show([{ kind: "code", text: "**not bold** https://x.test" }]), "a code span is opaque");

// Plain text, block syntax and markup stay exactly as typed.
for (const text of [
  "2*3*4 = 24",
  "snake_case_name and file_name_here",
  "# not a heading\n- not a list\n| not | a table |",
  "<b>not bold</b> <script>alert(1)</script>",
  "![alt](https://example.test/x.png)",
  "an unmatched `backtick and **bold",
  "javascript:alert(1) and [x](javascript:alert(1))",
  "** spaced ** and * spaced *",
  "line one\nline two",
  "",
]) {
  if (text === "") check(inlineTokens(text).length === 0, "an empty message has no tokens");
  else if (text.includes("[x](javascript")) {
    check(!inlineTokens(text).some((t) => t.kind === "link"), "only http(s) links are links");
  } else if (text.startsWith("![alt]")) {
    check(!inlineTokens(text).some((t) => t.kind !== "text" && t.kind !== "link"), "an image is never an image");
  } else same(text);
}
// Every token is text the renderer puts in a text node: no token carries markup.
check(inlineTokens("<img src=x onerror=alert(1)>").every((t) => t.kind === "text"), "HTML stays text");

// Segments: a single-line JSON paste (the observed case) becomes an indented block.
const paste =
  'QA test 4 (synthetic): call ask_form with exactly this form, unchanged: {"version":"1.0","createSurface":{"surfaceId":"qa","components":[{"id":"root","component":"Column","children":["q"]},{"id":"q","component":"Text","text":"QA closing check"}]}}';
const parts = segments(paste);
check(parts.length === 2 && parts[0].kind === "prose" && parts[1].kind === "code", `prose then code: ${JSON.stringify(parts)}`);
check(parts[0].text === "QA test 4 (synthetic): call ask_form with exactly this form, unchanged:", "the lead is the prose before the payload");
check(parts[1].kind === "code" && parts[1].lang === "json" && parts[1].text.split("\n").length > FOLD_LINES, "the payload is indented JSON");
check(foldable(parts), "a pasted payload folds even though it was one line");
// A short JSON value in a sentence, or an unbalanced brace, stays prose.
for (const text of ['set {"a":1} please', "a {broken json that runs on and on and on and on and on", "[1, 2] is a list"]) {
  const s = segments(text);
  check(s.length === 1 && s[0].kind === "prose" && s[0].text === text, `stays one prose segment: ${text}`);
}
// Fenced blocks are code, the text around them prose.
const fenced = segments("before\n```ts\nconst a = 1;\n```\nafter");
check(
  JSON.stringify(fenced) ===
    JSON.stringify([
      { kind: "prose", text: "before" },
      { kind: "code", text: "const a = 1;", lang: "ts" },
      { kind: "prose", text: "after" },
    ]),
  `fenced: ${JSON.stringify(fenced)}`,
);

// Folding.
check(!foldable(segments("one\ntwo\nthree")), "a short message does not fold");
check(!foldable(segments(Array.from({ length: FOLD_LINES * 2 }, (_, i) => `line ${i}`).join("\n"))), "twelve lines do not fold");
check(foldable(segments(Array.from({ length: FOLD_LINES * 2 + 1 }, (_, i) => `line ${i}`).join("\n"))), "thirteen lines fold");
check(foldable(segments("word ".repeat(260))), "one long wrapping line folds");
check(lineCount(segments("a\nb")) === 2, "line count follows newlines");

// The form receipt reads the host's label lines, only when the action parses.
const action = JSON.stringify({ version: "1.0", action: { name: "review.answer", surfaceId: "qa_review", context: { name: "QA Tester Six", day: ["fri"] } } });
const answer = `Form answer (qa_review):\nYour name: QA Tester Six\nReview day: Friday${ACTION_MARK}${action}`;
check(JSON.stringify(formReceipt(answer)) === JSON.stringify(["Your name: QA Tester Six", "Review day: Friday"]), "the receipt is the label lines");
for (const broken of ["Form answer (x):\nA: b", `Form answer (x):\nA: b${ACTION_MARK}{not json`, `Form answer (x):\nA: b${ACTION_MARK}${JSON.stringify({ nope: 1 })}`, `typed by hand${ACTION_MARK}${action}`]) {
  check(formReceipt(broken) === null, `no receipt when the card cannot show it: ${broken}`);
}

console.log("operatorText: ok");
