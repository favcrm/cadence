import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import type * as PartsModule from "../src/features/threads/ThreadChatParts";
import type * as PageModule from "../src/features/threads/ThreadsPage";
import type * as LinkModule from "../src/features/threads/IssueRefLink";
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
const { EntryRow, MessageRow } = require("../src/features/threads/ThreadsPage") as typeof PageModule;
const { OpenWorkItem } = require("../src/features/threads/IssueRefLink") as typeof LinkModule;
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
check(rendersMarkdown({ role: "system", kind: "message" }), "a system message is Markdown");
check(!rendersMarkdown({ role: "system", kind: "turn_result" }), "a system turn_result stays plain");
for (const kind of ["tool_call", "tool_result", "turn_result"]) {
  check(!rendersMarkdown({ role: "agent", kind }), `an agent ${kind} stays plain`);
}
check(!rendersMarkdown({ role: "operator", kind: "message" }), "an operator message is not the Markdown path");
const shell = "# comment\nline two\nline three";
const toolRow = renderToStaticMarkup(createElement(Turn, { who: "Agent", markdown: rendersMarkdown({ role: "agent", kind: "tool_result" }), children: shell }));
check(!toolRow.includes("<h1") && toolRow.includes("# comment\nline two"), "a tool_result keeps its lines and its # line as text");

// CAD-2387: the row wiring. EntryRow decides Markdown with rendersMarkdown, and
// issue ids open through OpenWorkItem only when the board knows them.
const projects = new Map([["CAD-2380", "cadence"]]);
const entryHtml = (role: string, kind: string, text: string, open: ((id: string) => void) | null = () => undefined): string =>
  renderToStaticMarkup(
    createElement(OpenWorkItem.Provider, { value: open }, createElement(EntryRow, { entry: { seq: 1, role, kind, text }, issueProjects: projects })),
  );
const sysResult = entryHtml("system", "turn_result", "# heading\nCAD-2380 done");
check(!sysResult.includes("<h1") && !sysResult.includes("answer-body") && sysResult.includes("# heading\nCAD-2380 done"), "EntryRow keeps a system turn_result plain");
const agentTool = entryHtml("agent", "tool_result", "# heading\nCAD-2380 done");
check(!agentTool.includes("<h1") && !agentTool.includes('class="lnk num"'), "EntryRow keeps an agent tool_result plain");
const agentMsg = entryHtml("agent", "message", "see CAD-2380 and ZZZ-9");
check(agentMsg.includes('<button class="lnk num">CAD-2380</button>'), "a known issue id in an agent message is a button");
check(!agentMsg.includes("<button class=\"lnk num\">ZZZ-9") && agentMsg.includes('<span class="num">ZZZ-9</span>'), "an unknown id stays plain");
const noOpener = entryHtml("agent", "message", "see CAD-2380", null);
check(!noOpener.includes("<button"), "without an opener an id stays plain");
const operatorMsg = entryHtml("operator", "message", "see CAD-2380");
check(!operatorMsg.includes("answer-body") && !operatorMsg.includes("<button"), "an operator message is unchanged");
const message = (kind: string, body: string) => ({
  id: "m1", seq: 1, conversation: "c", sender: { uid: "u", alias: "bot", kind, removed: false }, origin: null, body,
  state: "delivered", issue: null, task_id: null, created: 1, started: null, completed: null,
});
const rowHtml = (kind: string): string =>
  renderToStaticMarkup(
    createElement(OpenWorkItem.Provider, { value: () => undefined }, createElement(MessageRow, { message: message(kind, "see CAD-2380") as never, issueProjects: projects, writeBlock: null })),
  );
check(rowHtml("agent").includes('<button class="lnk num">CAD-2380</button>'), "MessageRow links a known id in an agent message");
check(!rowHtml("operator").includes("<button"), "MessageRow leaves an operator message alone");
const unknownRow = renderToStaticMarkup(
  createElement(OpenWorkItem.Provider, { value: () => undefined }, createElement(MessageRow, { message: message("unknown", "**x** CAD-2380") as never, issueProjects: projects, writeBlock: null })),
);
check(!unknownRow.includes("<strong>") && !unknownRow.includes("<button"), "MessageRow keeps a non-agent sender's text plain");
