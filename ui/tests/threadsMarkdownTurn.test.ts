import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import type * as PartsModule from "../src/features/threads/ThreadChatParts";
import type * as BarModule from "../src/features/app-shell/chat/TurnBar";
import { LocaleTextProvider } from "../src/lib/localeText";
import { rendersMarkdown } from "../src/features/threads/threadsView";
import { translateUiText } from "../src/lib/locale";
declare function require(name: string): any;

// The Node seam the other component tests use: no CSS, no icon bundles.
const loader = require("module"), originalRequire = loader.prototype.require;
loader.prototype.require = function (this: unknown, id: string) {
  if (id.endsWith(".css")) return {};
  if (id === "@hugeicons/core-free-icons") return new Proxy({}, { get: () => ({}) });
  if (id === "@hugeicons/react") return { HugeiconsIcon: () => null };
  return originalRequire.apply(this, arguments);
};
const { Turn } = require("../src/features/threads/ThreadChatParts") as typeof PartsModule;
const { TurnBar } = require("../src/features/app-shell/chat/TurnBar") as typeof BarModule;
loader.prototype.require = originalRequire;

function check(condition: boolean, message: string): void {
  if (!condition) throw new Error(`threadsMarkdownTurn: ${message}`);
}

// CAD-2380: a Threads agent or system turn renders through Md and `.answer`.
const body = "**done** and `code`\n\n<script>alert(1)</script> <b>raw</b>";
const agent = renderToStaticMarkup(createElement(Turn, { who: "Agent", markdown: true, children: body }));
check(agent.includes("<strong>done</strong>"), "bold renders as Markdown");
check(agent.includes("<code") && agent.includes("code</code>"), "inline code renders as Markdown");
check(agent.includes('class="answer"><div class="answer-body issue-reader"'), "the body nests as Home does, so the descendant .answer rules match");
check(!agent.includes("<script") && !agent.includes("<b>"), "raw HTML never becomes an element");
check(agent.includes("&lt;script&gt;") || agent.includes("&lt;b&gt;"), "raw HTML stays as text");
const sha = "0123456789abcdef0123456789abcdef01234567";
const idTurn = renderToStaticMarkup(createElement(Turn, { who: "Agent", markdown: true, children: `built ${sha} ok` }));
check(idTurn.includes('data-id-token="true"'), "a 40-hex id renders as an id token");

// Without `markdown` the body stays plain text; an operator turn keeps OperatorText.
const plain = renderToStaticMarkup(createElement(Turn, { who: "Someone", children: "**not bold**" }));
check(plain.includes("**not bold**") && !plain.includes("<strong>"), "a non-markdown turn stays plain");
const operator = renderToStaticMarkup(createElement(Turn, { who: "You", me: true, markdown: true, children: "**bold**" }));
check(!operator.includes("answer-body"), "an operator turn keeps OperatorText, not the answer surface");

// The failure detail goes through t(): known text translates, server text stays.
const bar = (reason: string): string =>
  renderToStaticMarkup(
    createElement(
      LocaleTextProvider,
      { value: (s: string) => translateUiText(s, "zh-TW") },
      createElement(TurnBar, { state: { kind: "failed", reason, message: null }, reconnecting: false, actions: { onCancel: () => undefined, onReview: () => undefined } }),
    ),
  );
check(bar("Assistant could not finish this turn.").includes("助理未能完成此回合。"), "the fallback detail translates");
check(bar("upstream 503: provider exploded").includes("upstream 503: provider exploded"), "unknown server text shows as is");
check(translateUiText("not in the table", "zh-TW") === "not in the table", "t() returns an unknown key unchanged");
const billing = translateUiText("Assistant could not run: billing is not set up for this company. The company owner needs to set up billing.", "zh-TW");
check(billing.includes("計費") && !billing.includes("帳單"), "the billing sentence says 計費, not 帳單");

// Only agent prose and system notes are Markdown; tool rows stay plain.
check(rendersMarkdown({ role: "agent", kind: "message" }) && rendersMarkdown({ role: "agent", kind: "assistant_text" }), "agent prose is Markdown");
check(rendersMarkdown({ role: "system", kind: "message" }), "a system note is Markdown");
for (const kind of ["tool_call", "tool_result", "turn_result"]) {
  check(!rendersMarkdown({ role: "agent", kind }), `an agent ${kind} stays plain`);
}
check(!rendersMarkdown({ role: "operator", kind: "message" }), "an operator message is not the Markdown path");
const shell = "# comment\nline two\nline three";
const toolRow = renderToStaticMarkup(createElement(Turn, { who: "Agent", markdown: rendersMarkdown({ role: "agent", kind: "tool_result" }), children: shell }));
check(!toolRow.includes("<h1") && toolRow.includes("# comment\nline two"), "a tool_result keeps its lines and its # line as text");
